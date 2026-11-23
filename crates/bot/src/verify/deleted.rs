//! The verification channel was deleted: make it again and deal with whoever deleted it.

use super::{adopt, CHANNEL_LOCK};
use crate::{
    app::{now_secs, App},
    events::send_log,
};
use anyhow::Result;
use brim_logging::{Log, LogKind};
use std::{collections::BTreeMap, sync::Mutex};
use twilight_model::{
    channel::Channel,
    guild::audit_log::{AuditLogEntry, AuditLogEventType as A},
    id::{
        marker::{ChannelMarker, GuildMarker},
        Id,
    },
};

/// New verification channels per server in [`REMAKE_WINDOW`]; the next delete is left alone.
const REMAKE_CAP: usize = 3;
const REMAKE_WINDOW: i64 = 600;
type Seen<K, V> = Mutex<BTreeMap<K, V>>;
/// Per guild: when the bot made a verification channel again.
static REMADE: Seen<u64, Vec<i64>> = Mutex::new(BTreeMap::new());
/// Deleted verification channels, kept for the audit entry that names who deleted them.
static DELETED: Seen<u64, Deleted> = Mutex::new(BTreeMap::new());

struct Deleted {
    name: String,
    remade: Remade,
    at: i64,
}

/// Whether a deleted verification channel was made again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Remade {
    Pending,
    Made(Id<ChannelMarker>),
    Not,
}
/// (guild, user) -> when they deleted the verification channel, kept a day.
static DELETERS: Seen<(u64, u64), i64> = Mutex::new(BTreeMap::new());

/// Counts a remake in `times` unless [`REMAKE_CAP`] were made in the window.
pub(crate) fn remake_allowed(times: &mut Vec<i64>, now: i64) -> bool {
    times.retain(|t| now - t < REMAKE_WINDOW);
    let ok = times.len() < REMAKE_CAP;
    if ok {
        times.push(now);
    }
    ok
}

#[derive(Debug, PartialEq, Eq)]
enum Deleter {
    Ignore,
    DmOwner,
    Quarantine,
    /// The level could not be read: the channel is made again, nobody is punished.
    Unread,
}

/// What happens to whoever deleted the verification channel. Trusted (Owner, Extra Owner,
/// Trusted Admin) get one warning; the Discord server owner cannot be quarantined. `trusted` is
/// `None` when the owner or the level could not be read.
fn deleted_by(
    is_me: bool,
    trusted: Option<bool>,
    server_owner: bool,
    deleted_before: bool,
) -> Deleter {
    match trusted {
        _ if is_me => Deleter::Ignore,
        None => Deleter::Unread,
        Some(t) if server_owner || (t && !deleted_before) => Deleter::DmOwner,
        Some(_) => Deleter::Quarantine,
    }
}

/// The verification channel was deleted: make it again where it was (at most [`REMAKE_CAP`] in
/// 10 minutes), and remember it for [`on_audit`], which learns who deleted it.
pub async fn on_channel_delete(app: &App, c: &Channel) -> Result<()> {
    let Some(gid) = c.guild_id else {
        return Ok(());
    };
    let cfg = app.cfg(gid.get()).await?;
    if !cfg.verification.enabled || cfg.verification.channel != Some(c.id) {
        return Ok(());
    }
    let name = c.name.clone().unwrap_or_else(|| "verification".into());
    let now = now_secs();
    // Noted before the remake, so an audit entry that comes in meanwhile still finds it.
    let note = |remade| {
        let mut deleted = DELETED.lock().unwrap();
        deleted.retain(|_, d: &mut Deleted| now - d.at < REMAKE_WINDOW);
        deleted.insert(
            c.id.get(),
            Deleted {
                name: name.clone(),
                remade,
                at: now,
            },
        );
    };
    note(Remade::Pending);
    let _one_at_a_time = CHANNEL_LOCK.lock().await;
    let allowed = remake_allowed(REMADE.lock().unwrap().entry(gid.get()).or_default(), now);
    let remade = if allowed {
        match remake(app, gid, c, &name).await {
            Ok(id) => Remade::Made(id),
            Err(e) => {
                let why = crate::http_err::why(&e);
                let vars = [("channel.name", name.as_str()), ("error", &why)];
                alert(app, gid, deleted_log("not_remade", &vars)).await;
                Remade::Not
            }
        }
    } else {
        let vars = [
            ("channel.name", name.as_str()),
            ("count", &REMAKE_CAP.to_string()),
        ];
        alert(app, gid, deleted_log("deleted_again", &vars)).await;
        Remade::Not
    };
    note(remade);
    Ok(())
}

/// The log of entry `log.anti_nuke.verification_channel_<kind>`.
pub(crate) fn deleted_log(kind: &str, vars: &[(&str, &str)]) -> Log {
    brim_logging::overlay::security_log(
        "Verification Channel Deleted",
        &format!("log.anti_nuke.verification_channel_{kind}"),
        vars,
    )
}

/// The anti-nuke log and a DM to the owner with the log's text.
async fn alert(app: &App, gid: Id<GuildMarker>, log: Log) {
    let text = log.lines_text();
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "verification channel log failed");
    }
    crate::reply::dm_owner(app, gid, "dm.security", &text).await;
}

/// A new channel with the deleted one's name, category, position and overwrites, the quarantine
/// role's set to the verification shape (the old one may have been broken). Once it exists it
/// counts as made; a failed save or panel post is reported on its own.
async fn remake(
    app: &App,
    gid: Id<GuildMarker>,
    c: &Channel,
    name: &str,
) -> Result<Id<ChannelMarker>> {
    let mut overwrites = c.permission_overwrites.clone().unwrap_or_default();
    if let Some(q) = app.cfg(gid.get()).await?.antinuke.quarantine_role {
        overwrites.retain(|o| o.id != q.cast());
        overwrites.push(super::quarantine_shape(q));
    }
    let mut req = app
        .http
        .create_guild_channel(gid, name)
        .permission_overwrites(&overwrites);
    if let Some(p) = c.parent_id {
        req = req.parent_id(p);
    }
    if let Some(pos) = c.position {
        req = req.position(pos.max(0) as u64);
    }
    let ch = req.await?.model().await?;
    if let Err(e) = adopt(app, gid, ch.id).await {
        let why = crate::http_err::why(&e);
        let channel = format!("<#{}>", ch.id);
        let vars = [("channel", channel.as_str()), ("error", &why)];
        alert(app, gid, deleted_log("not_set_up", &vars)).await;
    }
    Ok(ch.id)
}

/// The audit entry of a verification channel delete names who did it: tell the owner, and
/// quarantine them per [`deleted_by`].
pub async fn on_audit(app: &App, e: &AuditLogEntry) -> Result<()> {
    let (A::ChannelDelete, Some(gid), Some(actor), Some(target)) =
        (e.action_type, e.guild_id, e.user_id, e.target_id)
    else {
        return Ok(());
    };
    let peek = || {
        DELETED
            .lock()
            .unwrap()
            .get(&target.get())
            .map(|d| (d.name.clone(), d.remade))
    };
    let mut found = peek();
    // Wait while the gateway delete has not come in yet, or the new channel is being made.
    let unseen =
        found.is_none() && app.cfg(gid.get()).await?.verification.channel == Some(target.cast());
    if unseen || matches!(found, Some((_, Remade::Pending))) {
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            found = peek();
            if found.as_ref().is_some_and(|f| f.1 != Remade::Pending) {
                break;
            }
        }
    }
    let Some((name, remade)) = found else {
        return Ok(());
    };
    DELETED.lock().unwrap().remove(&target.get());
    // An error is never read as "not trusted" before a quarantine: unread punishes nobody.
    let owner = match app.guild_info(gid).await {
        Ok(i) => Some(i.owner),
        Err(e) => {
            tracing::warn!(?e, %gid, "verification channel delete: owner lookup failed");
            None
        }
    };
    let trusted = match owner {
        Some(o) => match app.level(gid, o, actor).await {
            Ok(level) => Some(level.immune()),
            Err(e) => {
                tracing::warn!(?e, %gid, %actor, "verification channel delete: level lookup failed");
                None
            }
        },
        None => None,
    };
    // The bot owner: a Discord owner paused by a rescue claim is not told it was them.
    let is_owner = owner.is_some() && app.bot_owner(gid).await.ok() == Some(actor);
    let now = now_secs();
    let before = {
        let mut d = DELETERS.lock().unwrap();
        d.retain(|_, at| now - *at < 86_400);
        d.insert((gid.get(), actor.get()), now).is_some()
    };
    let made = match remade {
        Remade::Made(id) => format!("Brim made a new one: <#{id}>."),
        Remade::Pending => "Brim is making a new one.".into(),
        Remade::Not => "Brim did not make a new one.".into(),
    };
    let deleted = format!("The verification channel #{name} was deleted by <@{actor}>. {made}");
    let by = format!("<@{actor}>");
    let vars = [
        ("channel.name", name.as_str()),
        ("by", by.as_str()),
        ("made", made.as_str()),
    ];
    match deleted_by(actor == app.me, trusted, is_owner, before) {
        Deleter::Ignore => {}
        Deleter::Unread => alert(app, gid, deleted_log("level_unread", &vars)).await,
        Deleter::DmOwner if is_owner => {
            alert(app, gid, deleted_log("deleted_by_owner", &vars)).await
        }
        Deleter::DmOwner => alert(app, gid, deleted_log("deleted_by_trusted", &vars)).await,
        Deleter::Quarantine => {
            let why = "Deleted the verification channel";
            match crate::antinuke::quarantine_actor(app, gid, actor, why).await {
                // The quarantine has its own anti-nuke log.
                Ok(done) => {
                    let what = if done.applied == crate::antinuke::Applied::Kicked {
                        "was kicked (the bot could not be quarantined)"
                    } else {
                        "was quarantined"
                    };
                    let text = format!("{deleted} <@{actor}> {what}.");
                    crate::reply::dm_owner(app, gid, "dm.security", &text).await
                }
                Err(e) => {
                    let why = crate::http_err::why(&e);
                    let vars = [vars[0], vars[1], vars[2], ("error", why.as_str())];
                    alert(app, gid, deleted_log("not_quarantined", &vars)).await
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn at_most_three_new_channels_in_ten_minutes() {
        let mut times = vec![];
        assert!(remake_allowed(&mut times, 0));
        assert!(remake_allowed(&mut times, 100));
        assert!(remake_allowed(&mut times, 200));
        assert!(!remake_allowed(&mut times, 300));
        assert!(!remake_allowed(&mut times, 599));
        // The first one is 10 minutes old: room for one more.
        assert!(remake_allowed(&mut times, 600));
        assert!(!remake_allowed(&mut times, 601));
    }

    #[test]
    fn who_deleted_the_channel_decides_the_quarantine() {
        use Deleter::*;
        // (is me, trusted, server owner, deleted before)
        assert_eq!(deleted_by(true, Some(false), false, false), Ignore);
        assert_eq!(deleted_by(true, None, false, false), Ignore);
        assert_eq!(deleted_by(false, Some(false), false, false), Quarantine);
        assert_eq!(deleted_by(false, Some(true), false, false), DmOwner);
        assert_eq!(deleted_by(false, Some(true), false, true), Quarantine);
        assert_eq!(deleted_by(false, Some(true), true, false), DmOwner);
        assert_eq!(deleted_by(false, Some(true), true, true), DmOwner);
        // A failed lookup punishes nobody, also for a repeat deleter.
        assert_eq!(deleted_by(false, None, false, false), Unread);
        assert_eq!(deleted_by(false, None, false, true), Unread);
    }
}

//! Protected channels: the log channels, and channels a feature registers (the public ticket
//! channel). Deleted while in use, one is made again ([`remake`]) and who deleted it is logged
//! ([`report`]). Nobody is punished here: anti-nuke counts the delete like any other, and its
//! revert skips a channel that has a copy ([`copy_of`]), so it does not make a second one.
//! Messages deleted by hand in a log channel are reported ([`on_audit`]); they cannot come back.
//! Log channels are also locked ([`lock`]): only Brim has Manage Messages and Manage Channels.

use crate::{
    app::{now_secs, App},
    events::send_log,
    http_err,
};
use anyhow::Result;
use brim_logging::{overlay::security_log, Log, LogKind};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use twilight_http::request::AuditLogReason;
use twilight_model::{
    channel::Channel,
    guild::{
        audit_log::{AuditLogEntry, AuditLogEventType as A},
        Permissions,
    },
    http::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
    id::{
        marker::{ChannelMarker, GuildMarker, UserMarker},
        Id,
    },
};

/// Per (guild, kind of channel): when the bot made one again.
static REMADE: Mutex<BTreeMap<(u64, &'static str), Vec<i64>>> = Mutex::new(BTreeMap::new());

/// The outcome of a remake, for the log.
pub struct Remade {
    pub id: Option<Id<ChannelMarker>>,
    pub text: String,
}

/// Runs `make` unless 3 channels of this kind were made in the last 10 minutes.
pub async fn remake(
    gid: Id<GuildMarker>,
    what: &'static str,
    make: impl std::future::Future<Output = Result<Option<Id<ChannelMarker>>>>,
) -> Remade {
    let allowed = crate::verify::deleted::remake_allowed(
        REMADE.lock().unwrap().entry((gid.get(), what)).or_default(),
        now_secs(),
    );
    if !allowed {
        let text =
            "It was deleted again after 3 new ones in 10 minutes, so Brim stopped making new ones.";
        return Remade {
            id: None,
            text: text.into(),
        };
    }
    match make.await {
        Ok(Some(id)) => Remade {
            id: Some(id),
            text: format!("Brim made a new one: <#{id}>."),
        },
        Ok(None) => Remade {
            id: None,
            text: "Brim did not make a new one.".into(),
        },
        Err(e) => Remade {
            id: None,
            text: format!("Brim could not make a new one: {}", http_err::why(&e)),
        },
    }
}

/// Logs who deleted `c` (from the audit log; the bot's own deletes are skipped) to the anti-nuke
/// log, and with `dm` to the owner too.
pub async fn report(app: &App, c: &Channel, what: &str, made: &Remade, dm: bool) -> Result<()> {
    let Some(gid) = c.guild_id else { return Ok(()) };
    // The audit entry can come a moment after the gateway delete.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let who = who(app, gid, &[A::ChannelDelete], c.id, false).await;
    if who == Some(app.me) {
        return Ok(());
    }
    let by = who.map_or("someone Brim could not see in the audit log".into(), |u| {
        format!("<@{u}>")
    });
    let name = c.name.as_deref().unwrap_or("unknown");
    let vars = [
        ("channel_kind", what),
        ("channel.name", name),
        ("by", by.as_str()),
        ("made", made.text.as_str()),
    ];
    let log = security_log(
        &format!("{} Deleted", title(what)),
        "log.anti_nuke.protected_channel_deleted",
        &vars,
    );
    alert(app, gid, log, dm).await
}

/// Who did the newest `kinds` entry on `target`. 100 entries, so a nuke's other deletes do not
/// push it out.
async fn who(
    app: &App,
    gid: Id<GuildMarker>,
    kinds: &[A],
    target: Id<ChannelMarker>,
    skip_me: bool,
) -> Option<Id<UserMarker>> {
    for kind in kinds {
        let Ok(r) = app.http.audit_log(gid).action_type(*kind).limit(100).await else {
            continue;
        };
        let hit = r.model().await.ok().and_then(|l| {
            l.entries
                .into_iter()
                .find(|e| {
                    e.target_id.map(|i| i.get()) == Some(target.get())
                        && !(skip_me && e.user_id == Some(app.me))
                })
                .and_then(|e| e.user_id)
        });
        if hit.is_some() {
            return hit;
        }
    }
    None
}

/// Puts the lock back on a log channel (`security::lock_overwrites`): only Brim may delete
/// messages or the channel there, whoever changed it (Owner, Extra Owner and Trusted Admin
/// included). Returns whether anything changed.
pub async fn lock(app: &App, gid: Id<GuildMarker>, c: &Channel) -> Result<bool> {
    // One at a time, from the live channel: each PUT fires a ChannelUpdate with an older payload.
    let _one = LOCKING.lock().await;
    let live = app.http.channel(c.id).await?.model().await?;
    let img = crate::backup::channel_img(live);
    let fix = brim_antinuke::security::lock_overwrites(&img, gid.get(), app.me.get());
    for o in &fix {
        let ow = PermissionOverwrite {
            id: Id::new(o.target),
            kind: if o.is_role {
                PermissionOverwriteType::Role
            } else {
                PermissionOverwriteType::Member
            },
            allow: Some(Permissions::from_bits_retain(o.allow)),
            deny: Some(Permissions::from_bits_retain(o.deny)),
        };
        app.http
            .update_channel_permission(c.id, &ow)
            .reason("Log channel: only Brim may delete messages here")
            .await?;
    }
    Ok(!fix.is_empty())
}

static LOCKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Per channel: when the owner was last sent a DM about its lock (one per 10 minutes, the
/// anti-nuke log gets every one), so flipping it in a loop does not flood the owner.
static LOCK_DMS: Mutex<BTreeMap<u64, i64>> = Mutex::new(BTreeMap::new());

fn dm_due(channel: u64, now: i64) -> bool {
    let mut m = LOCK_DMS.lock().unwrap();
    m.retain(|_, at| now - *at < 600);
    m.insert(channel, now).is_none()
}

/// A log channel changed: lock it again and log who unlocked it (the audit entry comes a moment
/// later, so that part runs on its own).
pub async fn on_channel_update(app: &Arc<App>, c: &Channel) -> Result<()> {
    let Some(gid) = c.guild_id else { return Ok(()) };
    if !app.cfg(gid.get()).await?.log_channels().contains(&c.id) || !lock(app, gid, c).await? {
        return Ok(());
    }
    let (app, id, name) = (app.clone(), c.id, c.name.clone().unwrap_or_default());
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let kinds = [
            A::ChannelOverwriteCreate,
            A::ChannelOverwriteUpdate,
            A::ChannelOverwriteDelete,
        ];
        // Skips the bot's own entries: its relock is newer than the change it undid.
        let by = match who(&app, gid, &kinds, id, true).await {
            Some(u) => format!("<@{u}>"),
            None => "someone Brim could not see in the audit log".into(),
        };
        let log = security_log(
            "Log Channel Lock Changed",
            "log.anti_nuke.log_channel_lock_changed",
            &[("by", &by), ("channel.name", &name)],
        );
        let dm = dm_due(id.get(), now_secs());
        if let Err(e) = alert(&app, gid, log, dm).await {
            tracing::warn!(?e, "log channel lock log failed");
        }
    });
    Ok(())
}

/// After a restore or revert made channels again: the backup's overwrites may predate the lock.
pub async fn lock_guild(app: &App, gid: Id<GuildMarker>) {
    let channels = match app.http.guild_channels(gid).await {
        Ok(r) => r.models().await.unwrap_or_default(),
        Err(e) => return tracing::warn!(?e, "reading channels to lock the log channels failed"),
    };
    if let Err(e) = lock_all(app, gid, &channels).await {
        tracing::warn!(?e, "locking the log channels failed");
    }
}

/// At start: locks every log channel that is not, so servers from before the lock get it.
pub async fn lock_all(app: &App, gid: Id<GuildMarker>, channels: &[Channel]) -> Result<()> {
    let logs = app.cfg(gid.get()).await?.log_channels();
    for c in channels.iter().filter(|c| logs.contains(&c.id)) {
        match lock(app, gid, c).await {
            Ok(true) => {
                let name = c.name.as_deref().unwrap_or("unknown");
                let log = security_log(
                    "Log Channel Locked",
                    "log.anti_nuke.log_channel_locked",
                    &[("channel.name", name)],
                );
                alert(app, gid, log, false).await?;
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(?e, channel = %c.id, "locking a log channel failed"),
        }
    }
    Ok(())
}

fn title(what: &str) -> String {
    what.split(' ')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The anti-nuke log, and with `dm` its text to the owner.
async fn alert(app: &App, gid: Id<GuildMarker>, log: Log, dm: bool) -> Result<()> {
    if dm {
        crate::reply::dm_owner(app, gid, "dm.security", &log.lines_text()).await;
    }
    send_log(app, gid.get(), LogKind::AntiNuke, log).await
}

/// A log channel was deleted: a copy (name, category, position, topic and overwrites) takes its
/// place in the config. `None` when `c` was no log channel.
pub async fn log_channel_deleted(app: &App, c: &Channel) -> Result<Option<Remade>> {
    let Some(gid) = c.guild_id else {
        return Ok(None);
    };
    if !app.cfg(gid.get()).await?.log_channels().contains(&c.id) {
        return Ok(None);
    }
    let mut differ = vec![];
    let mut made = remake(gid, "log channel", copy(app, gid, c, &mut differ)).await;
    if made.id.is_some() && !differ.is_empty() {
        // The backup may undo a tightening made after it, so staff see what changed.
        made.text.push_str(&format!(
            " Its overwrites came from the newest backup and differ from the deleted channel's for {}: check them.",
            differ.join(", ")
        ));
    }
    Ok(Some(made))
}

/// Per old log channel id: the copy that took its place, and when. The anti-nuke revert reads it
/// so it does not make a second channel ([`copy_of`]).
/// A copy still being made is 0. In memory: after a restart the revert makes its own.
static COPIES: Mutex<BTreeMap<u64, (u64, i64)>> = Mutex::new(BTreeMap::new());

/// The copy made in place of the deleted channel `old` in the last hour, if any; waits up to
/// 10 s for one still being made.
pub async fn copy_of(old: u64) -> Option<u64> {
    for _ in 0..20 {
        let now = now_secs();
        let got = {
            let mut m = COPIES.lock().unwrap();
            m.retain(|_, (_, at)| now - *at < 3600);
            m.get(&old).map(|(new, _)| *new)
        };
        match got {
            Some(0) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            other => return other,
        }
    }
    None
}

/// The overwrites for the copy: the newest backup's for this channel (a nuker may have added a
/// grant before deleting it), else the deleted channel's without member overwrites; roles that
/// no longer exist are left out (Discord refuses them), and the lock goes on top.
async fn copy_overwrites(
    app: &App,
    gid: Id<GuildMarker>,
    c: &Channel,
    differ: &mut Vec<String>,
) -> Result<Vec<twilight_model::channel::permission_overwrite::PermissionOverwrite>> {
    let saved: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT ch FROM backups, jsonb_array_elements(image->'channels') ch
         WHERE guild_id = $1 AND ch->>'id' = $2 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(gid.get() as i64)
    .bind(c.id.get().to_string())
    .fetch_optional(&app.pool)
    .await?;
    let mut img = crate::backup::channel_img(c.clone());
    match saved.and_then(|v| serde_json::from_value::<brim_antinuke::ChannelImg>(v).ok()) {
        Some(b) => {
            *differ = differing(&b.overwrites, &img.overwrites);
            img.overwrites = b.overwrites
        }
        None => img.overwrites.retain(|o| o.is_role),
    }
    let roles: Vec<u64> = app
        .http
        .roles(gid)
        .await?
        .models()
        .await?
        .iter()
        .map(|r| r.id.get())
        .collect();
    img.overwrites
        .retain(|o| (o.is_role && roles.contains(&o.target)) || o.target == app.me.get());
    for o in brim_antinuke::security::lock_overwrites(&img, gid.get(), app.me.get()) {
        img.overwrites
            .retain(|x| (x.target, x.is_role) != (o.target, o.is_role));
        img.overwrites.push(o);
    }
    Ok(crate::backup::overwrites(&img.overwrites))
}

/// Who has a different overwrite (or only one) in the backup than on the deleted channel.
fn differing(
    backup: &[brim_antinuke::OverwriteImg],
    deleted: &[brim_antinuke::OverwriteImg],
) -> Vec<String> {
    let mut out: Vec<String> = (backup.iter().filter(|o| !deleted.contains(o)))
        .chain(deleted.iter().filter(|o| !backup.contains(o)))
        .map(|o| match o.is_role {
            true => format!("<@&{}>", o.target),
            false => format!("<@{}>", o.target),
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

async fn copy(
    app: &App,
    gid: Id<GuildMarker>,
    c: &Channel,
    differ: &mut Vec<String>,
) -> Result<Option<Id<ChannelMarker>>> {
    COPIES.lock().unwrap().insert(c.id.get(), (0, now_secs()));
    let made = make_copy(app, gid, c, differ).await;
    let mut m = COPIES.lock().unwrap();
    match &made {
        Ok(Some(new)) => m.insert(c.id.get(), (new.get(), now_secs())),
        _ => m.remove(&c.id.get()),
    };
    made
}

async fn make_copy(
    app: &App,
    gid: Id<GuildMarker>,
    c: &Channel,
    differ: &mut Vec<String>,
) -> Result<Option<Id<ChannelMarker>>> {
    let name = c.name.as_deref().unwrap_or("logs");
    let overwrites = copy_overwrites(app, gid, c, differ).await?;
    let create = |parent: bool| {
        let mut req = app
            .http
            .create_guild_channel(gid, name)
            .kind(c.kind)
            .permission_overwrites(&overwrites);
        if let Some(p) = c.parent_id.filter(|_| parent) {
            req = req.parent_id(p);
        }
        if let Some(pos) = c.position {
            req = req.position(pos.max(0) as u64);
        }
        if let Some(t) = c.topic.as_deref() {
            req = req.topic(t);
        }
        req
    };
    // The category can be gone too (a nuke): then at the top, and the revert moves it back.
    let new = match create(true).await {
        Ok(r) => r,
        Err(_) if c.parent_id.is_some() => create(false).await?,
        Err(e) => return Err(e.into()),
    }
    .model()
    .await?
    .id;
    let why = match crate::relink::swap_ids(app, gid, c.id.get(), new.get()).await {
        Ok(0) => {
            "the config no longer names the old channel (a restore made it again first)".into()
        }
        Ok(_) => return Ok(Some(new)),
        Err(e) => http_err::why(&e),
    };
    // Not counted as made: the old routes are forgotten, so logs reach the main channel.
    if let Err(e) = app.http.delete_channel(new).await {
        tracing::warn!(?e, "deleting an unused log channel copy failed");
    }
    anyhow::bail!("a new one was made but not used: {why}")
}

/// Messages deleted by hand (by anyone but Brim) in a log channel: the owner and the
/// anti-nuke log hear of it. Discord adds later deletes by the same person in the same channel
/// to the first entry without a new event, so only the first is reported.
pub async fn on_audit(app: &App, e: &AuditLogEntry) -> Result<()> {
    let (Some(gid), Some(actor)) = (e.guild_id, e.user_id) else {
        return Ok(());
    };
    let channel = match e.action_type {
        A::MessageDelete => e.options.as_ref().and_then(|o| o.channel_id),
        A::MessageBulkDelete => e.target_id.map(|t| t.cast()),
        _ => None,
    };
    let Some(ch) = channel else { return Ok(()) };
    if actor == app.me || !app.cfg(gid.get()).await?.log_channels().contains(&ch) {
        return Ok(());
    }
    let n = e
        .options
        .as_ref()
        .and_then(|o| o.count.clone())
        .unwrap_or_else(|| "1".into());
    let (by, channel) = (format!("<@{actor}>"), format!("<#{ch}>"));
    let log = security_log(
        "Log Messages Deleted",
        "log.anti_nuke.log_messages_deleted",
        &[("by", &by), ("count", &n), ("channel", &channel)],
    );
    alert(app, gid, log, true).await
}

#[cfg(test)]
mod tests {
    use twilight_model::id::Id;

    #[test]
    fn log_channels_include_appeals_and_the_ticket_log() {
        let mut c = crate::app::GuildCfg::default();
        c.logging.fallback = Some(Id::new(1));
        c.logging
            .routes
            .insert(brim_logging::LogKind::Report, Id::new(2));
        c.appeals.channel = Some(Id::new(3));
        c.tickets.log_channel = Some(Id::new(4));
        let all: Vec<u64> = c.log_channels().iter().map(|i| i.get()).collect();
        assert_eq!(all, [1, 2, 3, 4]);
    }

    #[test]
    fn backup_overwrites_that_differ_are_named() {
        let o = |target, deny| brim_antinuke::OverwriteImg {
            target,
            is_role: true,
            allow: 0,
            deny,
        };
        // Role 2 lost View after the backup; role 3 is only on the deleted channel.
        let backup = [o(1, 0), o(2, 0)];
        let deleted = [o(1, 0), o(2, 1024), o(3, 1024)];
        assert_eq!(super::differing(&backup, &deleted), ["<@&2>", "<@&3>"]);
    }

    #[test]
    fn titles_are_capitalised() {
        assert_eq!(super::title("log channel"), "Log Channel");
        assert_eq!(
            super::title("public ticket channel"),
            "Public Ticket Channel"
        );
    }
}

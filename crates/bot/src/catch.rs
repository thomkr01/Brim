//! Golden channel (honeypot): hacked accounts spam every channel, so whoever writes in it is
//! punished through the normal mod path (case, mod log, appeal link) with the bot as moderator.
use anyhow::Result;
use brim_core::{permits::Level, sections::CatchAction};
use brim_logging::{embed::RED, Log, LogKind};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use twilight_model::{
    channel::{message::MessageType, Channel},
    gateway::payload::incoming::MessageCreate,
    http::channel_position::Position,
    id::{
        marker::{ChannelMarker, GuildMarker, UserMarker},
        Id,
    },
};

use crate::{
    app::App,
    commands::{apply_dm, ModKind},
    events::send_log,
    prefix::Flags,
};

const REASON: &str = "Wrote in the golden channel";

/// What a member writes. A pin, boost or join notice is a system message by whoever caused it,
/// so a moderator who pins a message there must not be punished.
const WRITES: [MessageType; 5] = [
    MessageType::Regular,
    MessageType::Reply,
    MessageType::ChatInputCommand,
    MessageType::ContextMenuCommand,
    MessageType::ThreadCreated,
];

/// How long one catch blocks the next for the same member: a spam bot sends several messages
/// at once, and each must not start its own ban, DM and case.
const HOLD: Duration = Duration::from_secs(30);

/// (server, member) caught in the last [`HOLD`] (one bot process per database, as for reports).
static CAUGHT: LazyLock<Mutex<HashMap<(u64, u64), Instant>>> = LazyLock::new(Default::default);

/// True for the first catch of a member within [`HOLD`]; later ones only delete their message.
fn first_catch(key: (u64, u64), now: Instant) -> bool {
    let mut held = CAUGHT.lock().unwrap_or_else(|e| e.into_inner());
    held.retain(|_, at| now.duration_since(*at) < HOLD);
    match held.contains_key(&key) {
        true => false,
        false => held.insert(key, now).is_none(),
    }
}

/// The member who wrote `m`: for a slash command used here, the member who ran it, not the app
/// that answered. None for bots, webhooks and Brim's own answers.
fn writer(m: &MessageCreate, me: Id<UserMarker>) -> Option<Id<UserMarker>> {
    match &m.interaction_metadata {
        Some(_) if m.author.id == me => None,
        Some(i) => Some(i.user.id),
        None if m.author.bot || m.webhook_id.is_some() => None,
        None => Some(m.author.id),
    }
}

/// True when the message was in the golden channel by someone it punishes; nothing else should run.
/// Log mode punishes nobody, so automod still runs.
pub async fn on_message(app: &App, m: &MessageCreate) -> Result<bool> {
    let Some(gid) = m.guild_id else {
        return Ok(false);
    };
    let cfg = app.cfg(gid.get()).await?;
    let c = &cfg.catch;
    let Some(catch) = c.channel.filter(|_| c.enabled) else {
        return Ok(false);
    };
    if !WRITES.contains(&m.kind) {
        return Ok(false);
    }
    let Some(uid) = writer(m, app.me) else {
        return Ok(false);
    };
    // Threads of the golden channel catch too.
    if catch != m.channel_id
        && !app
            .channel_scope(gid, m.channel_id)
            .await
            .is_ok_and(|(parent, _)| parent == catch)
    {
        return Ok(false);
    }
    // A slash command's message carries the app's member, not the roles of who ran it.
    let roles = m
        .member
        .as_ref()
        .filter(|_| m.interaction_metadata.is_none());
    let roles = roles.map_or(&[][..], |mem| &mem.roles);
    if c.users.contains(&uid) || roles.iter().any(|r| c.roles.contains(r)) {
        return Ok(false);
    }
    // Only Owner, Extra Owners and Trusted Admins: a hacked mod with a permit is caught too.
    if app.is_immune(gid, uid).await? {
        return Ok(false);
    }
    let owner = app.guild_info(gid).await?.owner;
    let (kind, soft) = match c.action {
        CatchAction::Ban => (ModKind::Ban, false),
        CatchAction::Softban => (ModKind::Ban, true),
        CatchAction::Kick => (ModKind::Kick, false),
        CatchAction::Timeout => (ModKind::Timeout, false),
        CatchAction::Log => {
            let log = test_log(uid.get());
            if let Err(e) = send_log(app, gid.get(), LogKind::AutoMod, log).await {
                tracing::warn!(?e, "golden channel test log failed");
            }
            return Ok(false);
        }
    };
    crate::http_err::best_effort(
        app.http.delete_message(m.channel_id, m.id).await,
        "delete caught message",
    );
    if !first_catch((gid.get(), uid.get()), Instant::now()) {
        return Ok(true);
    }
    let flags = Flags {
        dm: c.dm,
        soft,
        compact: true,
        full_purge: true,
        ..Default::default()
    };
    // The bot acts with owner rights, so the usual permit and hierarchy checks still refuse
    // owners and members above the bot's role; that refusal is logged, never silent.
    let done = apply_dm(
        app,
        gid,
        owner,
        (app.me, &Level::Owner),
        kind,
        uid,
        Some(REASON),
        None,
        0,
        flags,
        None,
    )
    .await;
    if !done.as_ref().is_ok_and(|a| a.case.is_some()) {
        // Not caught: the member's next message must try again.
        CAUGHT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(gid.get(), uid.get()));
    }
    let text = match done {
        Ok(a) if a.case.is_none() => a.text,
        Ok(a) => {
            if let Some(case_no) = a.case {
                crate::proof::verified(app, gid, case_no, &crate::proof::copy_of(m)).await;
            }
            if matches!(kind, ModKind::Kick | ModKind::Timeout) {
                purge_seen(app, gid.get(), uid.get()).await;
            }
            count(app, gid.get(), c.action).await;
            return Ok(true);
        }
        Err(e) => format!("Failed: {e}"),
    };
    let log = failed_log(uid.get(), &text);
    // The member was caught: a failed log must not hand the message on as uncaught.
    if let Err(e) = send_log(app, gid.get(), LogKind::AutoMod, log).await {
        tracing::warn!(?e, "golden channel failure log failed");
    }
    Ok(true)
}

/// Deletes the member's recent messages in other channels, as a ban's purge does.
/// shortcut: only the messages Auto Mod tracked (its last 50 per member, Auto Mod on); a full
/// search of every channel if servers report spam left behind.
async fn purge_seen(app: &App, gid: u64, uid: u64) {
    let seen = app
        .heat
        .lock()
        .unwrap()
        .get_mut(&(gid, uid))
        .map(|(_, h)| h.take_msgs());
    crate::reply::purge_ids(app, &seen.unwrap_or_default()).await;
}

/// One row per catch, for the caught counter (no member id: the case holds who).
async fn count(app: &App, gid: u64, action: CatchAction) {
    let done = sqlx::query("INSERT INTO catch_hits (guild_id, action) VALUES ($1, $2)")
        .bind(gid as i64)
        .bind(action.name())
        .execute(&app.pool)
        .await;
    if let Err(e) = done {
        tracing::warn!(?e, "catch count failed");
    }
}

pub(crate) fn test_log(uid: u64) -> Log {
    // The entry keeps its old key, so a server's edited version still applies after the rename.
    Log::new("Golden channel (test mode)", RED)
        .entry("log.auto_mod.catch_channel_test_mode")
        .user("Member", uid)
        .line("Action", "None, test mode only logs")
        .var("member", &brim_logging::embed::user(uid))
}

pub(crate) fn failed_log(uid: u64, error: &str) -> Log {
    Log::new("Golden channel action failed", RED)
        .entry("log.auto_mod.catch_channel_action_failed")
        .user("Member", uid)
        .line("Error", error)
        .var("member", &brim_logging::embed::user(uid))
        .var("error", error)
}

/// The title of the default warning (entry `reply.catch.warning`).
const NOTICE: &str = "Golden channel";
/// The default title before the rename (2026-11-25): a warning posted then still counts, so a
/// save does not post a second one.
const OLD_NOTICE: &str = "Catch channel";

/// What writing in the golden channel does, for the warning's `{punishment}`.
fn punishment(a: CatchAction) -> &'static str {
    match a {
        CatchAction::Ban | CatchAction::Softban => "get you automatically banned",
        CatchAction::Kick => "get you automatically kicked",
        CatchAction::Timeout => "get you automatically timed out",
        CatchAction::Log => "be logged for the staff team",
    }
}

/// Posts the warning in the golden channel once: skipped when the bot's warning is already there.
pub async fn post_notice(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let cfg = app.cfg(gid.get()).await?;
    let c = &cfg.catch;
    let Some(ch) = c.channel.filter(|_| c.enabled) else {
        return Ok(());
    };
    // Deleted in Discord: Overview shows "The golden channel was deleted" with a fix, so each save
    // does not log a failed post (test server, 2026-11-18).
    let live = match app.http.channel(ch).await {
        Err(e) if crate::http_err::gone(&e) => return Ok(()),
        r => r?.model().await?,
    };
    // A stored id from before the save-time checks may name another server's channel.
    anyhow::ensure!(
        live.guild_id == Some(gid),
        "not posting the golden channel warning in {ch}: that channel is not in this server"
    );
    let recent = app
        .http
        .channel_messages(ch)
        .limit(50)
        .await?
        .models()
        .await?;
    let vars = [("punishment", punishment(c.action))];
    let said = crate::reply::say(app, gid, "reply.catch.warning", &vars).await;
    // The default's title, or the edited version's title or text: a warning posted before an
    // edit still counts, so a save does not post a second one.
    let title = said.embeds.first().and_then(|e| e.title.as_deref());
    let mine = |m: &twilight_model::channel::Message| {
        let titled = |e: &twilight_model::channel::message::Embed| {
            matches!(e.title.as_deref(), Some(NOTICE | OLD_NOTICE))
                || (title.is_some() && e.title.as_deref() == title)
        };
        let same_text = !said.text.is_empty() && m.content == said.text;
        m.author.id == app.me && (m.embeds.iter().any(titled) || same_text)
    };
    if recent.iter().any(mine) {
        return Ok(());
    }
    let mut post = app.http.create_message(ch).embeds(&said.embeds);
    if !said.text.is_empty() {
        post = post.content(&said.text);
    }
    post.allowed_mentions(Some(&Default::default())).await?;
    Ok(())
}

/// Moves the golden channel back into place when someone drags it: the bottom of its Information
/// category when that is set and still there, else the top of the server.
pub async fn keep_top(app: &App, ch: &Channel) -> Result<()> {
    let Some(gid) = ch.guild_id else {
        return Ok(());
    };
    let cfg = app.cfg(gid.get()).await?;
    if !cfg.catch.keep_top || cfg.catch.channel != Some(ch.id) {
        return Ok(());
    }
    // The cache may predate the save that set the category (a late notify): read it fresh before
    // moving the channel out of Information to the top.
    let category = match cfg.catch.category {
        Some(c) => Some(c),
        None => saved_category(app, gid).await?,
    };
    let pos = match category {
        Some(cat) => {
            let all = app.http.guild_channels(gid).await?.models().await?;
            // The list is newer than the event (PITFALLS: an event can be older than a write).
            let me = all.iter().find(|c| c.id == ch.id).unwrap_or(ch);
            match all.iter().any(|c| c.id == cat) {
                true => bottom_of(&all, me, cat),
                false => top(me),
            }
        }
        None => top(ch),
    };
    if let Some(pos) = pos {
        app.http.update_guild_channel_positions(gid, &[pos]).await?;
    }
    Ok(())
}

/// The golden channel's category in the newest saved config, past the cache.
async fn saved_category(app: &App, gid: Id<GuildMarker>) -> Result<Option<Id<ChannelMarker>>> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT config->'catch'->>'category' FROM guild_config WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?
    .flatten();
    Ok(id.and_then(|i| i.parse().ok()).and_then(Id::new_checked))
}

/// Uncategorized channels sit above every category; None when it is there already.
fn top(ch: &Channel) -> Option<Position> {
    let moved = ch.position.unwrap_or(0) != 0 || ch.parent_id.is_some();
    moved.then(|| Position {
        parent_id: Some(None),
        ..Position::from((ch.id, 0))
    })
}

/// The move to the bottom of category `cat`; None when it is there already, so the move's own
/// update event stops here.
fn bottom_of(all: &[Channel], ch: &Channel, cat: Id<ChannelMarker>) -> Option<Position> {
    let spot = |c: &Channel| {
        (
            c.id.get(),
            c.parent_id.map(Id::get),
            i64::from(c.position.unwrap_or(0)),
        )
    };
    let others: Vec<_> = all.iter().map(spot).collect();
    let pos = brim_core::sections::bottom_position(&others, spot(ch), cat.get())?;
    Some(Position {
        parent_id: Some(Some(cat)),
        ..Position::from((ch.id, pos))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_default_is_the_old_text() {
        let vars = [("punishment", punishment(CatchAction::Softban))];
        let m = brim_logging::botmsg::render("reply.catch.warning", None, &vars);
        assert_eq!(m["embeds"][0]["title"], NOTICE);
        assert_eq!(m["embeds"][0]["color"], RED);
        assert_eq!(m["embeds"][0]["description"], "This channel is to catch bots which automatically spam each channel with scams before they are able to post to the rest of the server.\n\n**Sending a message in this channel will get you automatically banned.**");
        assert_eq!(punishment(CatchAction::Log), "be logged for the staff team");
    }
}

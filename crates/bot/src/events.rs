//! Gateway event dispatch: logging first, then commands, automod and anti-nuke.
use anyhow::Result;
use brim_logging::{embed::RED, Log, LogConfig, LogKind, LogMessage};
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    time::Instant,
};
use twilight_gateway::Event;
use twilight_model::{
    channel::{
        message::{
            component::{ActionRow, Button, ButtonStyle, Component},
            AllowedMentions, Embed,
        },
        Channel,
    },
    guild::Permissions,
    http::attachment::Attachment,
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, RoleMarker},
        Id,
    },
};

use crate::{
    antinuke, app::App, automod, commands, confirm, metrics, prefix, reports, slash, tickets,
    verify,
};

fn guild_of(ev: &Event) -> Option<u64> {
    let g = match ev {
        Event::MessageCreate(m) => m.guild_id,
        Event::MessageUpdate(m) => m.0.guild_id,
        Event::MessageDelete(m) => m.guild_id,
        Event::MessageDeleteBulk(m) => m.guild_id,
        Event::MemberAdd(m) => Some(m.guild_id),
        Event::MemberRemove(m) => Some(m.guild_id),
        Event::GuildAuditLogEntryCreate(e) => e.0.guild_id,
        Event::AutoModerationActionExecution(e) => Some(e.guild_id),
        Event::VoiceStateUpdate(v) => v.0.guild_id,
        Event::MessagePollVoteAdd(v) => v.guild_id,
        Event::MessagePollVoteRemove(v) => v.guild_id,
        Event::GuildScheduledEventCreate(e) => Some(e.guild_id),
        Event::GuildScheduledEventUpdate(e) => Some(e.guild_id),
        Event::GuildScheduledEventDelete(e) => Some(e.guild_id),
        Event::GuildScheduledEventUserAdd(e) => Some(e.guild_id),
        Event::GuildScheduledEventUserRemove(e) => Some(e.guild_id),
        Event::GuildUpdate(g) => Some(g.id),
        Event::ThreadCreate(t) => t.guild_id,
        Event::ThreadUpdate(t) => t.guild_id,
        Event::ThreadDelete(t) => Some(t.guild_id),
        Event::ThreadListSync(s) => Some(s.guild_id),
        _ => None,
    };
    g.map(|g| g.get())
}

/// Channel and role updates drop the cached answer sooner.
const PUBLIC_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Whether a member can view a channel: base permissions, then the channel's (allow, deny)
/// overwrites in order (@everyone first, then the role's).
fn sees(base: Permissions, overwrites: &[(Permissions, Permissions)]) -> bool {
    base.contains(Permissions::ADMINISTRATOR)
        || overwrites
            .iter()
            .fold(base, |p, (allow, deny)| (p - *deny) | *allow)
            .contains(Permissions::VIEW_CHANNEL)
}

/// Logs carry deleted messages and staff actions, so they never go where @everyone, or a main
/// role every member gets, can read them. Err when the channel is not in `gid`: a stored id from
/// before the save-time checks must not post one server's logs into another.
async fn public(app: &App, gid: Id<GuildMarker>, channel: Id<ChannelMarker>) -> Result<bool> {
    let cached = app.log_public.lock().unwrap().get(&(gid, channel)).copied();
    if let Some((p, _)) = cached.filter(|(_, at)| at.elapsed() < PUBLIC_TTL) {
        return Ok(p);
    }
    let c = app.http.channel(channel).await?.model().await?;
    anyhow::ensure!(
        c.guild_id == Some(gid),
        "not logging to {channel}: that channel is not in this server, pick a log channel of this server"
    );
    let main = match c.guild_id {
        Some(gid) => app.cfg(gid.get()).await?.lockdown.main_roles.clone(),
        None => vec![],
    };
    public_with(app, c, &main).await
}

/// [`public`] for a fetched channel with the main roles given; it reads no config, so a caller
/// inside `App::cfg` can use it.
pub(crate) async fn public_with(
    app: &App,
    c: twilight_model::channel::Channel,
    main: &[Id<RoleMarker>],
) -> Result<bool> {
    let channel = c.id;
    let p = match c.guild_id {
        Some(gid) => {
            // By the permissions a lock saved: the channel is public again once it ends.
            let info = crate::lockdown::unlocked_info(app, gid).await?;
            let perms = |r: u64| {
                Permissions::from_bits_truncate(
                    info.role_perms.get(&Id::new(r)).copied().unwrap_or(0),
                )
            };
            let ows = c.permission_overwrites.unwrap_or_default();
            let ow = |r: u64| {
                ows.iter()
                    .find(|o| o.id.get() == r)
                    .map_or((Permissions::empty(), Permissions::empty()), |o| {
                        (o.allow, o.deny)
                    })
            };
            let g = gid.get();
            sees(perms(g), &[ow(g)])
                || main
                    .iter()
                    .any(|r| sees(perms(g) | perms(r.get()), &[ow(g), ow(r.get())]))
        }
        None => false,
    };
    if let Some(gid) = c.guild_id {
        app.log_public
            .lock()
            .unwrap()
            .insert((gid, channel), (p, Instant::now()));
    }
    Ok(p)
}

/// A held-back log is not only in the container log: the owner hears once a day per channel.
async fn warn_public(app: &App, channel: Id<ChannelMarker>) {
    static LAST: std::sync::Mutex<std::collections::BTreeMap<u64, i64>> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());
    if !crate::reply::alert_due(
        &mut LAST.lock().unwrap(),
        channel.get(),
        crate::app::now_secs(),
        86_400,
    ) {
        return;
    }
    let gid = match app.http.channel(channel).await {
        Ok(r) => r.model().await.ok().and_then(|c| c.guild_id),
        Err(_) => None,
    };
    if let Some(gid) = gid {
        let text = format!("Brim did not post a log in <#{channel}>: every member can read that channel. Make it private, or press **Make private** under the security score on the dashboard.");
        crate::reply::dm_owner(app, gid, "dm.owner_alert", &text).await;
    }
}

/// A link button row under a log, or nothing.
fn link_row(link: Option<&(&str, String)>) -> Vec<Component> {
    link.map(|(label, url)| {
        Component::ActionRow(ActionRow {
            id: None,
            components: vec![Component::Button(brim_logging::emoji::LINK.on(Button {
                id: None,
                custom_id: None,
                disabled: false,
                emoji: None,
                label: Some((*label).into()),
                style: ButtonStyle::Link,
                url: Some(url.clone()),
                sku_id: None,
            }))],
        })
    })
    .into_iter()
    .collect()
}

/// The Bot Messages key of a log type: `log.<serde name>`.
fn log_key(kind: LogKind) -> String {
    let name = serde_json::to_value(kind).ok();
    format!(
        "log.{}",
        name.as_ref().and_then(|v| v.as_str()).unwrap_or("")
    )
}

/// What a log tells the Bot Messages entries beyond its title: the entry it belongs to when the
/// title does not name one, and the values an edited version of that entry may fill (see
/// `Log::entry`, `Log::var`).
#[derive(Default)]
pub(crate) struct Meta {
    entry: Option<String>,
    vars: Vec<(String, String)>,
}

impl Meta {
    fn of(log: &Log) -> Self {
        let (entry, vars) = log.meta();
        Self {
            entry: entry.map(Into::into),
            vars: vars.to_vec(),
        }
    }
}

/// The log with the server's saved frame laid over it (Bot Messages page); its own entry and
/// values reach an entry of facts. `key` is the log's group (`log.<group>`): the entry of its type (found by its title) wins, else the group's own entry,
/// which is also where an edit from before the per-type entries sits.
pub(crate) async fn framed_log(app: &App, gid: Id<GuildMarker>, key: &str, log: Log) -> Embed {
    let meta = Meta::of(&log);
    framed_with(app, gid, key, &log.build(), &meta).await
}

#[cfg(test)]
async fn framed(app: &App, gid: Id<GuildMarker>, key: &str, embed: &Embed) -> Embed {
    framed_with(app, gid, key, embed, &Meta::default()).await
}

async fn framed_with(
    app: &App,
    gid: Id<GuildMarker>,
    key: &str,
    embed: &Embed,
    meta: &Meta,
) -> Embed {
    if brim_logging::botmsg::find(key).is_none_or(|d| d.locked) {
        return embed.clone();
    }
    let typed = brim_logging::botmsg::log_type_key(key, embed.title.as_deref().unwrap_or(""));
    let own = meta.entry.as_deref();
    // A log with its own entry that shares a title with an entry of facts (two "Rule Not
    // Approved" logs) must not take that other entry's edit: it says something else.
    let typed = typed
        .filter(|t| own.is_none() || !brim_logging::botmsg::find(t).is_some_and(|d| d.details()));
    // A security log in an ordinary group (NSFW, webhook spam) never takes that group's frame,
    // which any Admin may edit without the security rules.
    let secure = |k: &str| brim_logging::botmsg::find(k).is_some_and(|d| d.security);
    let group =
        (!own.into_iter().chain(typed.as_deref()).any(secure) || secure(key)).then_some(key);
    for k in own.into_iter().chain(typed.as_deref()).chain(group) {
        // A copy without {text} is never sent (`render` falls back), so the next entry is tried.
        if let Some(stored) = crate::reply::stored_msg(app, gid, k)
            .await
            .filter(|s| brim_logging::botmsg::missing_text(k, s).is_none())
        {
            // An entry that names the log's facts (`{case}`, `{reason}` ...) is the whole log;
            // a frame only wraps the log's own text.
            let detailed = brim_logging::botmsg::find(k).is_some_and(|d| d.details());
            return if detailed {
                over(k, &stored, embed, &meta.vars, true)
            } else {
                frame_over(k, &stored, embed)
            };
        }
    }
    embed.clone()
}

/// Lays a saved log frame over `embed`: title and text from the frame (filled with the log's own),
/// colour, footer and pictures when the frame sets them, its fields added. Over Discord's 6000
/// characters the log is sent as built, so an edit never loses a log.
pub(crate) fn frame_over(key: &str, stored: &serde_json::Value, embed: &Embed) -> Embed {
    over(key, stored, embed, &[], false)
}

use brim_logging::overlay::over;

/// Logged text is user-controlled, so it is sent with mentions disabled.
#[allow(clippy::too_many_arguments)]
async fn send_with(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
    (key, meta): (&str, &Meta),
    pings: &str,
    embed: &Embed,
    rows: &[Component],
    mentions: &AllowedMentions,
    files: &[Attachment],
) -> Result<Id<MessageMarker>> {
    let embed = &framed_with(app, gid, key, embed, meta).await;
    if public(app, gid, channel).await? {
        warn_public(app, channel).await;
        anyhow::bail!(
            "not logging to {channel}: @everyone can read it, pick a private log channel"
        );
    }
    Ok(app
        .http
        .create_message(channel)
        .content(pings)
        .embeds(std::slice::from_ref(embed))
        .components(rows)
        .allowed_mentions(Some(mentions))
        .attachments(files)
        .await?
        .model()
        .await?
        .id)
}

/// Sends a log a purge held back, when the purge failed before its own entry.
pub(crate) async fn send_held(app: &App, m: &LogMessage) {
    if let Err(e) = send_entry(app, m).await {
        tracing::warn!(?e, channel = %m.channel_id, "log send failed");
    }
}

/// How long a log actor's roles are reused, so a burst of logs reads each member once.
const ROLES_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the ignore lists hide this log: one of its actors is an ignored user or holds an
/// ignored role. A failed read sends the log (fail open).
async fn actor_ignored(app: &App, m: &LogMessage) -> bool {
    if m.actors.is_empty() || !m.kind.ignorable() {
        return false;
    }
    let Ok(cfg) = app.cfg(m.guild.get()).await else {
        return false;
    };
    let (users, ignored) = (&cfg.logging.ignored_users, &cfg.logging.ignored_roles);
    if m.actors.iter().any(|a| users.contains(a)) {
        return true;
    }
    if ignored.is_empty() {
        return false;
    }
    for &actor in &m.actors {
        let key = (m.guild, actor);
        let cached = app.log_roles.lock().unwrap().get(&key).cloned();
        let roles = match cached.filter(|(_, at)| at.elapsed() < ROLES_TTL) {
            Some((r, _)) => r,
            None => {
                // A member who left has no roles to read: their log is sent.
                let Ok(member) = crate::http_err::member(&app.http, m.guild, actor).await else {
                    continue;
                };
                let roles = member.map(|m| m.roles).unwrap_or_default();
                let mut c = app.log_roles.lock().unwrap();
                if c.len() >= 1000 {
                    c.retain(|_, (_, at)| at.elapsed() < ROLES_TTL);
                }
                c.insert(key, (roles.clone(), Instant::now()));
                roles
            }
        };
        if roles.iter().any(|r| ignored.contains(r)) {
            return true;
        }
    }
    false
}

async fn send_entry(app: &App, m: &LogMessage) -> Result<()> {
    if actor_ignored(app, m).await {
        return Ok(());
    }
    let mentions = AllowedMentions::default();
    let rows = link_row(m.link.as_ref());
    let meta = Meta {
        entry: None,
        vars: m.vars.clone(),
    };
    let id = send_with(
        app,
        m.guild,
        m.channel_id,
        (&log_key(m.kind), &meta),
        "",
        &m.embed,
        &rows,
        &mentions,
        &[],
    )
    .await?;
    if let Some((name, text)) = &m.file {
        let file = Attachment::from_bytes(name.clone(), text.as_bytes().to_vec(), 0);
        reply_file(app, m.channel_id, id, file).await?;
    }
    Ok(())
}

/// Discord shows attachments above the embed, so the file is its own message below it, replying to it.
async fn reply_file(
    app: &App,
    channel: Id<ChannelMarker>,
    to: Id<MessageMarker>,
    file: Attachment,
) -> Result<Id<MessageMarker>> {
    Ok(app
        .http
        .create_message(channel)
        .reply(to)
        .fail_if_not_exists(false)
        .allowed_mentions(Some(&AllowedMentions::default()))
        .attachments(&[file])
        .await?
        .model()
        .await?
        .id)
}

/// A ticket log in a private channel, with an optional file below it. It ends with the date and
/// time it happened.
pub(crate) async fn send_private(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
    log: Log,
    file: Option<Attachment>,
) -> Result<(Id<MessageMarker>, Option<Id<MessageMarker>>)> {
    let log = log.time();
    let id = send_with(
        app,
        gid,
        channel,
        ("log.ticket", &Meta::of(&log)),
        "",
        &log.build(),
        &[],
        &AllowedMentions::default(),
        &[],
    )
    .await?;
    let file = match file {
        Some(f) => Some(reply_file(app, channel, id, f).await?),
        None => None,
    };
    Ok((id, file))
}

/// Every log sent here ends with the date and time it happened.
pub async fn send_log(app: &App, guild: u64, kind: LogKind, embed: Log) -> Result<()> {
    send_log_files(app, guild, kind, embed, &[], &[], &[])
        .await
        .map(drop)
}

/// Like [`send_log`] with text files (name, text) below the embed, as a bulk delete log has, and
/// button rows under it.
pub async fn send_log_files(
    app: &App,
    guild: u64,
    kind: LogKind,
    embed: Log,
    files: &[(String, String)],
    attach: &[Attachment],
    rows: &[Component],
) -> Result<Option<(Id<ChannelMarker>, Id<MessageMarker>)>> {
    let Some(ch) = app.cfg(guild).await?.logging.target(kind, None) else {
        return Ok(None);
    };
    let mentions = AllowedMentions::default();
    let embed = embed.time();
    let meta = Meta::of(&embed);
    let id = send_with(
        app,
        Id::new(guild),
        ch,
        (&log_key(kind), &meta),
        "",
        &embed.build(),
        rows,
        &mentions,
        attach,
    )
    .await?;
    for (name, text) in files {
        let file = Attachment::from_bytes(name.clone(), text.as_bytes().to_vec(), 0);
        reply_file(app, ch, id, file).await?;
    }
    Ok(Some((ch, id)))
}

/// Like [`send_log`] but pings the given roles (panic mode warns staff roles).
pub async fn send_log_pinging(
    app: &App,
    guild: u64,
    kind: LogKind,
    embed: Log,
    roles: &[Id<RoleMarker>],
) -> Result<()> {
    let Some(ch) = app.cfg(guild).await?.logging.target(kind, None) else {
        return Ok(());
    };
    let pings: String = roles.iter().map(|r| format!("<@&{r}> ")).collect();
    let mentions = AllowedMentions {
        roles: roles.to_vec(),
        ..Default::default()
    };
    let embed = embed.time();
    let meta = Meta::of(&embed);
    send_with(
        app,
        Id::new(guild),
        ch,
        (&log_key(kind), &meta),
        pings.trim_end(),
        &embed.build(),
        &[],
        &mentions,
        &[],
    )
    .await
    .map(drop)
}

/// Like [`send_log`] with a link button under the embed.
pub async fn send_log_link(
    app: &App,
    guild: u64,
    kind: LogKind,
    embed: Log,
    link: (&str, String),
) -> Result<()> {
    send_log_rows(app, guild, kind, embed, &link_row(Some(&link))).await
}

/// Like [`send_log`] with button rows under the embed.
pub async fn send_log_rows(
    app: &App,
    guild: u64,
    kind: LogKind,
    embed: Log,
    rows: &[Component],
) -> Result<()> {
    let Some(ch) = app.cfg(guild).await?.logging.target(kind, None) else {
        return Ok(());
    };
    let mentions = AllowedMentions::default();
    let embed = embed.time();
    let meta = Meta::of(&embed);
    send_with(
        app,
        Id::new(guild),
        ch,
        (&log_key(kind), &meta),
        "",
        &embed.build(),
        rows,
        &mentions,
        &[],
    )
    .await
    .map(drop)
}

/// A log of one `event`, routed as the logger's own are (custom type, event route, kind route, general
/// channel) and held back by the ignore lists for `actor`. Nothing when it has no channel.
pub(crate) async fn send_event_log(
    app: &App,
    guild: Id<GuildMarker>,
    kind: LogKind,
    event: brim_logging::LogEvent,
    actor: Id<twilight_model::id::marker::UserMarker>,
    embed: Log,
) -> Result<()> {
    let Some(channel_id) = app
        .cfg(guild.get())
        .await?
        .logging
        .target(kind, Some(event))
    else {
        return Ok(());
    };
    let embed = embed.time();
    let m = LogMessage {
        guild,
        channel_id,
        kind,
        vars: embed.meta().1.to_vec(),
        embed: embed.build(),
        link: None,
        file: None,
        actors: vec![actor],
    };
    send_entry(app, &m).await
}

/// A deleted log channel leaves the config, so its logs reach the main log channel, which says so.
async fn forget_log_channel(app: &App, gid: Id<GuildMarker>, c: &Channel) -> Result<()> {
    if !app.cfg(gid.get()).await?.logging.clone().forget(c.id) {
        return Ok(());
    }
    let saved = crate::settings::save(app, gid, app.me.get(), &["logging"], |doc| {
        let mut log: LogConfig =
            serde_json::from_value(doc["logging"].clone()).map_err(|e| e.to_string())?;
        log.forget(c.id);
        // Merged in, so keys LogConfig does not model (the dashboard's config_changes) survive.
        let new = serde_json::to_value(log).map_err(|e| e.to_string())?;
        if let (Some(old), serde_json::Value::Object(new)) = (doc["logging"].as_object_mut(), new) {
            old.extend(new);
        }
        Ok(())
    })
    .await?;
    saved.map_err(anyhow::Error::msg)?;
    let name = c.name.as_deref().unwrap_or("unknown");
    let embed = Log::new("Log Channel Deleted", RED)
        .line("Channel", &format!("#{name} (`{}`)", c.id))
        .text("Its logs now go to the main log channel. Pick a new channel on the dashboard.");
    send_log(app, gid.get(), LogKind::Server, embed).await
}

/// Sends held delete logs once their wait is over. `handle` only flushes when an event arrives,
/// so a quiet guild would otherwise never log its deletes.
pub async fn flush_logs(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        tick.tick().await;
        let due = app.logger.lock().unwrap().flush(Instant::now());
        for m in due {
            if let Err(e) = send_entry(&app, &m).await {
                tracing::warn!(?e, channel = %m.channel_id, "log send failed");
            }
        }
    }
}

/// What the raw gateway frame carries that twilight's model drops.
#[derive(Default)]
pub struct Extra {
    /// Discord's "likely spammer" user flag on a member join.
    pub spammer: bool,
}

/// A handler or log task that shutdown waits for. Counted down on drop, so one that panics does
/// not stay "in flight" for good.
struct InFlight;

impl InFlight {
    fn new() -> Self {
        metrics::IN_FLIGHT.fetch_add(1, Relaxed);
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        metrics::IN_FLIGHT.fetch_sub(1, Relaxed);
    }
}

pub async fn handle(app: Arc<App>, ev: Event, extra: Extra) {
    metrics::EVENTS.fetch_add(1, Relaxed);
    let _done = InFlight::new();
    // Every log line from the handler carries the server id, so a failed Discord call can be traced to it.
    let span = tracing::info_span!("event", guild = ev.guild_id().map(|g| g.get()));
    if let Err(e) = tracing::Instrument::instrument(run(&app, &ev, extra), span).await {
        tracing::warn!(?e, kind = ?ev.kind(), guild = ev.guild_id().map(|g| g.get()), "event handler failed");
        metrics::EVENT_ERRORS.fetch_add(1, Relaxed);
    }
}

/// Last step before exit: lets running handlers finish (strikes and locks are written as they
/// happen, so nothing else is pending), sends the held logs and closes the pool.
pub async fn shutdown(app: &App) {
    for _ in 0..50 {
        if metrics::IN_FLIGHT.load(Relaxed) == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let held = app
        .logger
        .lock()
        .unwrap()
        .flush(Instant::now() + std::time::Duration::from_secs(3600));
    for m in held {
        if let Err(e) = send_entry(app, &m).await {
            tracing::warn!(?e, "log send failed during shutdown");
        }
    }
    crate::welcome::flush_audit(app).await;
    app.pool.close().await;
}

async fn run(app: &Arc<App>, ev: &Event, extra: Extra) -> Result<()> {
    // Before any await: each event runs in its own task, so an await here would let a later voice
    // update of the same member (or one after a GuildCreate) take the logger lock first.
    match ev {
        Event::GuildCreate(g) => {
            crate::welcome::seed_voice(app, g);
            if let twilight_model::gateway::payload::incoming::GuildCreate::Available(g) = &**g {
                for c in &g.channels {
                    app.note_category(c.id.get(), c.parent_id.map(|p| p.get()), true);
                }
            }
        }
        // Before any await too: the delete of a channel just moved must see the move.
        Event::ChannelCreate(c) => {
            app.note_category(c.id.get(), c.parent_id.map(|p| p.get()), true)
        }
        Event::ChannelUpdate(c) => {
            app.note_category(c.id.get(), c.parent_id.map(|p| p.get()), false)
        }
        Event::MessageDelete(m) => crate::qr::deleted((m.id.get(), 0)),
        Event::MessageDeleteBulk(m) => m
            .ids
            .iter()
            .for_each(|id| crate::qr::deleted((id.get(), 0))),
        Event::ChannelDelete(c) => crate::qr::deleted((0, c.id.get())),
        Event::ThreadDelete(t) => crate::qr::deleted((0, t.id.get())),
        Event::MemberUpdate(m) => {
            let (g, u) = (m.guild_id.get(), m.user.id.get());
            app.forget_level(g, u);
            // A raid trigger fetches this member's roles again: the join's are stale.
            if let Some(d) = app.raids.lock().unwrap().get_mut(&g) {
                d.set_roles(u, None);
            }
        }
        Event::MemberRemove(m) => {
            app.forget_level(m.guild_id.get(), m.user.id.get());
            // A rejoin is a new membership: it must not skip the verification gate.
            crate::verify::given::forget_member(m.guild_id, m.user.id);
        }
        _ => {}
    }
    // A voice member's channel before this event, read as the update is recorded.
    let voice = match ev {
        Event::VoiceStateUpdate(v) => v.0.guild_id.map(|g| {
            let old = app
                .logger
                .lock()
                .unwrap()
                .voice_update(g, v.0.user_id, v.0.channel_id);
            (v.0.user_id, (old, v.0.channel_id))
        }),
        _ => None,
    };
    let voice_old = voice.and_then(|v| v.1 .0);
    if let Some(guild) = guild_of(ev) {
        // A failed config read skips only the logs, never the protection below.
        match app.cfg(guild).await {
            Ok(cfg) => {
                let mut msgs = {
                    let mut logger = app.logger.lock().unwrap();
                    match voice {
                        Some((user, moved)) => logger.handle_voice(
                            Id::new(guild),
                            &cfg.logging,
                            user,
                            moved,
                            Instant::now(),
                        ),
                        None => logger.handle(Id::new(guild), &cfg.logging, ev, Instant::now()),
                    }
                };
                if let Event::MessageDeleteBulk(m) = ev {
                    let mod_log = cfg.logging.target(LogKind::ModCommand, None);
                    let ids: Vec<u64> = m.ids.iter().map(|i| i.get()).collect();
                    crate::purge::on_bulk(&ids, &mut msgs, mod_log);
                }
                // Log posts wait on the log channel's rate limit, so they must not hold back
                // raid detection, anti-nuke or automod.
                // One task per event, as the events themselves: no order between events either
                // way. One sender for every server would make one raid's log channel delay the
                // logs of all servers.
                if !msgs.is_empty() {
                    let (app, done) = (app.clone(), InFlight::new());
                    let send = async move {
                        let _done = done;
                        for m in msgs {
                            if let Err(e) = send_entry(&app, &m).await {
                                tracing::warn!(?e, channel = %m.channel_id, "log send failed");
                            }
                        }
                    };
                    tokio::spawn(tracing::Instrument::in_current_span(send));
                }
            }
            Err(e) => tracing::warn!(?e, "config read failed, the logs of this event are skipped"),
        }
    }
    match ev {
        // Webhook and app posts count as bots but still reach automod (never commands).
        Event::MessageCreate(m)
            if !m.author.bot || m.webhook_id.is_some() || m.interaction_metadata.is_some() =>
        {
            if crate::catch::on_message(app, m).await? {
                return Ok(());
            }
            if let (false, None) = (m.author.bot, m.guild_id) {
                return tickets::on_dm(app, m).await;
            }
            if let (false, Some(gid)) = (m.author.bot, m.guild_id) {
                // A failed reply or lookup here must not skip automod below.
                let cmd = async {
                    let cfg = app.cfg(gid.get()).await?;
                    let rest = m.content.strip_prefix(&cfg.misc.prefix);
                    let rest = rest.filter(|r| !r.is_empty());
                    // The server's own settings for the command: a server alias becomes its
                    // command, and a command that is off, narrowed or cooling down stops here.
                    let gate = crate::cmdcfg::prefix(app, m, gid, &cfg, rest).await?;
                    let Some((rest, settings)) = gate else {
                        return Ok(());
                    };
                    let rest = rest.as_deref();
                    // Reply helpers find the command's frame by this word (Bot messages page).
                    let word = rest.and_then(|r| r.split_whitespace().next()).unwrap_or("");
                    let run = async {
                        if tickets::on_message(app, m, gid, rest).await? {
                            // Relayed or a ticket command; automod still sees it below.
                        } else if let Some(rest) = rest {
                            // An ignored command still goes through automod below.
                            if !commands::ignored(app, m, gid, rest).await?
                                && !crate::general::try_run(app, m, rest).await?
                                && !crate::settings::try_run(app, m, rest).await?
                            {
                                if let Some(parsed) = prefix::parse(rest) {
                                    commands::run(app, m, parsed).await?;
                                }
                            }
                        }
                        anyhow::Ok(())
                    };
                    let framed = crate::reply::WORD.scope((word.to_lowercase(), gid), run);
                    let r = crate::cmdcfg::RUN.scope(settings, framed).await;
                    crate::cmdcfg::done(app, m, settings).await;
                    r
                };
                if let Err(e) = cmd.await {
                    tracing::warn!(?e, "ticket relay or command failed; automod still runs");
                }
            }
            automod::on_message(app, m).await?;
            // After automod: a message it removed, or a member it punished, gets no answer.
            if let Some(gid) = m.guild_id {
                if let Err(e) = crate::msgrules::on_message(app, m, gid).await {
                    tracing::warn!(?e, "message rules: counting or a trigger failed");
                }
            }
        }
        // Another bot's post only moves stickies down ("Also after other bots").
        Event::MessageCreate(m) if m.guild_id.is_some() => {
            if let Err(e) = crate::msgrules::on_message(app, m, m.guild_id.unwrap()).await {
                tracing::warn!(?e, "message rules: a sticky wait failed");
            }
        }
        // A webhook or app can change its text or files without edited_timestamp (a deferred fill,
        // a button update): new text or files are filtered (unseen files scanned and QR-read),
        // the same ones never twice.
        Event::MessageUpdate(m) if app_post(app, &m.0) => {
            if automod::new_text(&m.0) {
                automod::on_edit(app, &m.0).await?;
            } else if !m.embeds.is_empty() {
                scan_preview(app, &m.0).await?;
            }
        }
        // Discord also sends updates for link previews; only real edits are checked again.
        Event::MessageUpdate(m) if m.edited_timestamp.is_some() && !m.author.bot => {
            automod::on_edit(app, &m.0).await?;
        }
        // A link preview or GIF arrived after the post: only the image scan looks at it.
        Event::MessageUpdate(m)
            if m.edited_timestamp.is_none() && !m.embeds.is_empty() && !m.author.bot =>
        {
            scan_preview(app, &m.0).await?;
        }
        Event::RoleCreate(r) => app.invalidate_guild(r.guild_id),
        Event::RoleUpdate(r) => {
            app.invalidate_object(r.guild_id, r.role.id.get());
            app.log_public.lock().unwrap().clear();
        }
        Event::RoleDelete(r) => app.invalidate_guild(r.guild_id),
        Event::ChannelCreate(c) => {
            app.note_parent(&c.0);
            c.guild_id.into_iter().for_each(|g| app.invalidate_guild(g));
            if let Err(e) = crate::setup::enforce_quarantine(app, c).await {
                tracing::warn!(?e, channel = %c.id, "quarantine overwrite failed");
            }
        }
        Event::ChannelUpdate(c) => {
            if let Some(g) = c.guild_id {
                app.log_public.lock().unwrap().remove(&(g, c.id));
            }
            c.guild_id
                .into_iter()
                .for_each(|g| app.invalidate_object(g, c.id.get()));
            if let Err(e) = crate::setup::enforce_quarantine(app, c).await {
                tracing::warn!(?e, channel = %c.id, "quarantine overwrite failed");
            }
            // A slowmode staff change in Discord's own UI while an auto slowmode starts wins.
            if let Some(g) = c.guild_id {
                if let Err(e) = crate::slowmode::channel_updated(app, g.get(), c.id.get()).await {
                    tracing::warn!(?e, channel = %c.id, "checking the auto slowmode failed");
                }
            }
            if let Err(e) = crate::protected::on_channel_update(app, c).await {
                tracing::warn!(?e, channel = %c.id, "locking a log channel failed");
            }
            crate::catch::keep_top(app, c).await?;
        }
        Event::ChannelDelete(c) => {
            app.note_parent(&c.0);
            // A server channel always has its server; the rows below all belong to one.
            if let Some(g) = c.guild_id {
                if let Err(e) = crate::msghooks::channel_deleted(app, g, c.id.get()).await {
                    tracing::warn!(?e, "forgetting a deleted channel's message webhook failed");
                }
                if let Err(e) = crate::feeds::channel_deleted(app, c.id.get()).await {
                    tracing::warn!(?e, "marking a deleted channel's feeds failed");
                }
                if let Err(e) =
                    crate::msgrules::channel_deleted(app, g, c.id.get(), c.name.as_deref()).await
                {
                    tracing::warn!(?e, "pausing a deleted channel's message schedules failed");
                }
                // Logged, not `?`: the log and verification channel remakes below must still run.
                if let Err(e) = tickets::on_channel_delete(app, g, c.id).await {
                    tracing::warn!(?e, "closing a deleted ticket channel's ticket failed");
                }
            }
            if let Err(e) = verify::deleted::on_channel_delete(app, c).await {
                tracing::warn!(?e, "remaking the verification channel failed");
            }
            if let Some(g) = c.guild_id {
                app.invalidate_guild(g);
                let remade = crate::protected::log_channel_deleted(app, &c.0)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(?e, "remaking a log channel failed");
                        None
                    });
                if remade.as_ref().is_none_or(|r| r.id.is_none()) {
                    // Logged, not `?`: the report below must still run.
                    if let Err(e) = forget_log_channel(app, g, &c.0).await {
                        tracing::warn!(?e, "forgetting a deleted log channel failed");
                    }
                }
                // Last: it waits for the audit entry that names who deleted it.
                if let Some(r) = remade {
                    if let Err(e) =
                        crate::protected::report(app, &c.0, "log channel", &r, true).await
                    {
                        tracing::warn!(?e, "logging who deleted a log channel failed");
                    }
                }
                // Nobody deletes the public ticket channel while it is on (owner decision 11-15).
                if tickets::public::is_public(app, g, c.id).await {
                    let what = "public ticket channel";
                    let r =
                        crate::protected::remake(g, what, tickets::public::ensure(app, g)).await;
                    if let Err(e) = crate::protected::report(app, &c.0, what, &r, false).await {
                        tracing::warn!(?e, "logging who deleted the public ticket channel failed");
                    }
                }
            }
        }
        Event::GuildUpdate(g) => {
            app.invalidate_guild(g.id);
            crate::welcome::save_icon(app, g.id, g.icon.as_ref());
            crate::owner::owner_seen(app, g.id, g.owner_id).await?;
            if let Err(e) = app.sync_unpermittable(g.id).await {
                tracing::warn!(?e, "reading the roles Discord hands out failed");
            }
        }
        Event::GuildCreate(g) => {
            crate::welcome::on_guild_create(app, g).await?;
            if let twilight_model::gateway::payload::incoming::GuildCreate::Available(g) = &**g {
                if let Err(e) = crate::staff_hoist::once(app, g.id, &g.roles).await {
                    tracing::warn!(?e, "the one-time staff role pass failed");
                }
            }
        }
        Event::AutoModerationRuleCreate(r) => {
            crate::native::rule_changed(r.0.guild_id.get(), r.0.id.get());
            crate::native::rule_seen(&r.0);
        }
        Event::AutoModerationRuleUpdate(r) => {
            crate::native::rule_changed(r.0.guild_id.get(), r.0.id.get());
            crate::native::rule_seen(&r.0);
            crate::automod::on_rule_change(app, &r.0, false).await
        }
        Event::AutoModerationRuleDelete(r) => {
            crate::automod::on_rule_change(app, &r.0, true).await;
            crate::native::rule_deleted(app, &r.0).await?
        }
        Event::GuildDelete(g) => crate::welcome::on_guild_delete(app, g).await?,
        Event::Ready(r) => crate::welcome::on_ready(app, r).await?,
        Event::MemberUpdate(m) if m.user.id == app.me => app.invalidate_guild(m.guild_id),
        Event::MemberUpdate(m) => {
            if let Err(e) = antinuke::enforce_permits(app, m).await {
                tracing::warn!(?e, "enforcing permits failed");
            }
            if let Err(e) = crate::permit_sync::on_member_update(app, m).await {
                tracing::warn!(?e, "taking unlisted permit roles failed");
            }
            if let Err(e) = crate::timeout_roles::on_member_update(app, m).await {
                tracing::warn!(?e, "removing main roles on timeout failed");
            }
            if let Err(e) = crate::timeout_end::on_member_update(app, m).await {
                tracing::warn!(?e, "queueing the timeout end log failed");
            }
            if let Err(e) = crate::profile_log::on_member_update(app, m).await {
                tracing::warn!(?e, "the avatar and name change log failed");
            }
            if let Err(e) = verify::on_member_update(app, m).await {
                tracing::warn!(?e, "pausing the verification time limit failed");
            }
            if let Err(e) = crate::sticky::on_member_update(app, m).await {
                tracing::warn!(?e, "saving sticky roles failed");
            }
            if !crate::joingate::on_member_update(app, m).await? {
                let (user, nick) = (&m.user, m.nick.as_deref());
                let r = crate::dehoist::check(app, m.guild_id, user, nick, &m.roles).await;
                if let Err(e) = r {
                    tracing::warn!(?e, "auto-dehoist failed");
                }
            }
        }
        Event::MemberAdd(m) => {
            crate::permit_sync::forget_left(m.guild_id, m.user.id);
            if let Err(e) = crate::profile_log::on_join(app, m.guild_id, &m.user).await {
                tracing::warn!(?e, "storing a joiner's name and avatar failed");
            }
            crate::joingate::on_join(app, m, extra.spammer).await?
        }
        Event::MemberRemove(m) => {
            if let Err(e) =
                crate::permit_sync::on_leave(app, m.guild_id, m.user.id, m.user.bot).await
            {
                tracing::warn!(?e, "removing a leaver from the permits failed");
            }
            if let Err(e) = crate::profile_log::on_leave(app, m.guild_id, m.user.id).await {
                tracing::warn!(?e, "removing a leaver's stored name and avatar failed");
            }
            crate::sticky::on_leave(app, m.guild_id, m.user.id).await?
        }
        Event::BanAdd(b) => {
            crate::invite_cmds::revoke_links(app, b.guild_id, b.user.id).await;
            crate::sticky::on_ban(app, b.guild_id, b.user.id).await?
        }
        Event::InteractionCreate(i) => {
            if !verify::handle(app, &i.0).await?
                && !tickets::handle(app, &i.0).await?
                && !confirm::handle_confirm(app, &i.0).await?
                && !reports::handle(app, &i.0).await?
                && !crate::appeals::handle(app, &i.0).await?
                && !crate::settings::handle(app, &i.0).await?
                && !crate::whitelist::handle(app, &i.0).await?
                && !crate::raiders::handle(app, &i.0).await?
                && !crate::nsfw::handle(app, &i.0).await?
                && !crate::msgmenu::handle(app, &i.0).await?
                && !crate::undo::handle(app, &i.0).await?
            {
                slash::handle(app, &i.0).await?
            }
        }
        // Anti-nuke first: the verification check can wait for the gateway delete.
        Event::GuildAuditLogEntryCreate(e) => {
            // Before anything slow: a grant handled in parallel must not use the old gate.
            verify::given::audit_changed(app, &e.0);
            // The catch-up after a reconnect may have handled this entry already.
            let g = e.0.guild_id.map(|g| g.get());
            let inflight = g.and_then(|g| crate::welcome::saw_audit(app, g, e.0.id.get()));
            let new = g.is_none() || inflight.is_some();
            let (nuke, rule) = if new {
                let nuke = antinuke::on_audit(app, &e.0).await;
                (nuke, crate::native::on_rule_audit(app, &e.0).await)
            } else {
                (Ok(()), Ok(()))
            };
            if let (true, Some(g)) = (new, g) {
                crate::welcome::handled_audit(app, g, &e.0).await;
            }
            drop(inflight);
            let allow = crate::raiders::on_unban(app, &e.0).await;
            if let Err(err) = verify::deleted::on_audit(app, &e.0).await {
                tracing::warn!(?err, "verification channel delete check failed");
            }
            if let Err(err) = crate::protected::on_audit(app, &e.0).await {
                tracing::warn!(?err, "reporting deleted log messages failed");
            }
            if let Err(err) = verify::given::on_audit(app, &e.0).await {
                tracing::warn!(
                    ?err,
                    "taking back a verification role from another bot failed"
                );
            }
            if let Err(err) = crate::msghooks::on_audit(app, &e.0).await {
                tracing::warn!(?err, "recording a deleted Message Builder webhook failed");
            }
            // Stats are a side effect: their failure never stops protection.
            if let Err(err) = crate::modstats::on_audit(app, &e.0).await {
                tracing::warn!(?err, "recording a mod action failed");
            }
            nuke.and(allow).and(rule)?
        }
        // Voice raid first: the anti-nuke recheck can wait out its fetch gap.
        Event::VoiceStateUpdate(v) => {
            if let Err(e) = crate::voice_raid::on_voice(app, &v.0, voice_old).await {
                tracing::warn!(?e, "voice raid check failed");
            }
            crate::voice_nuke::on_voice(app, &v.0, voice_old).await
        }
        Event::AutoModerationActionExecution(e) => crate::native::on_native_hit(app, e).await?,
        Event::ReactionAdd(r) => crate::activity::on_reaction(app, r).await?,
        Event::ThreadCreate(t) => crate::activity::on_thread(app, &t.0).await?,
        Event::ThreadUpdate(t) => crate::activity::on_thread_update(app, &t.0).await?,
        Event::ThreadListSync(s) => s
            .threads
            .iter()
            .filter_map(|t| Some((t.id, t.parent_id?)))
            .for_each(|(t, p)| app.note_thread(t, p)),
        _ => {}
    }
    Ok(())
}

/// A webhook's or another app's message (not Brim's own).
fn app_post(app: &App, m: &twilight_model::channel::Message) -> bool {
    m.author.id != app.me && automod::app_message(m)
}

/// Only the image scan reads a preview update.
async fn scan_preview(app: &App, m: &twilight_model::channel::Message) -> Result<()> {
    if let Some(gid) = m.guild_id {
        let am = &app.cfg(gid.get()).await?.automod;
        if am.enabled && am.filters.nsfw_images.enabled {
            app.nsfw.enqueue(m, !automod::files_can_be_new(m), false);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ignore lists check every actor (author and deleter); a failed read and a security log are sent.
    #[tokio::test]
    async fn ignore_lists_hide_only_ignorable_logs_of_any_ignored_actor() {
        let Some(t) = crate::test_support::T::new(640).await else {
            return;
        };
        let g = t.g(0);
        let cfg = serde_json::json!({"logging": {"ignored_roles": ["77"], "ignored_users": ["8"]}});
        t.server_config(0, t.owner(), cfg).await;
        t.set(|m| {
            m.members.insert((g.get(), 5), vec![77]);
            m.members.insert((g.get(), 6), vec![78]);
        });
        let log = |kind, actor: u64| LogMessage {
            guild: g,
            channel_id: Id::new(1),
            kind,
            embed: Log::new("Role Update", 0).build(),
            link: None,
            file: None,
            actors: vec![Id::new(actor)],
            vars: vec![],
        };
        assert!(actor_ignored(&t.app, &log(LogKind::Role, 5)).await);
        assert!(!actor_ignored(&t.app, &log(LogKind::Role, 6)).await);
        assert!(!actor_ignored(&t.app, &log(LogKind::AntiNuke, 5)).await);
        // A delete by an ignored moderator (second actor) is hidden too.
        let mut deleted = log(LogKind::Message, 6);
        deleted.actors.push(Id::new(8));
        assert!(actor_ignored(&t.app, &deleted).await);
        t.app.log_roles.lock().unwrap().clear();
        t.set(|m| m.fail_member = true);
        assert!(
            !actor_ignored(&t.app, &log(LogKind::Role, 5)).await,
            "fail open"
        );
        t.cleanup().await;
    }

    /// Every log frame on the Bot Messages page names a real log type, and a saved frame keeps
    /// the log's own parts unless it sets them.
    #[test]
    fn log_frames_lay_over_the_built_log() {
        for (kind, _) in brim_logging::botmsg::LOGS {
            if *kind != "ticket" {
                let k: LogKind = serde_json::from_value(serde_json::json!(kind)).unwrap();
                assert_eq!(log_key(k), format!("log.{kind}"));
            }
        }
        let built = Log::new("Message Deleted", 5)
            .line("Channel", "#a")
            .field("F", "v")
            .footer("code")
            .build();
        let stored = serde_json::json!({"embeds": [{"title": "🗑️ {title}", "description": "{text}\nSee the rules."}]});
        let e = frame_over("log.message", &stored, &built);
        assert_eq!(e.title.as_deref(), Some("🗑️ Message Deleted"));
        assert!(e
            .description
            .as_deref()
            .unwrap()
            .ends_with("#a\nSee the rules."));
        assert_eq!((e.color, e.fields.len()), (Some(5), 1));
        assert_eq!(e.footer.unwrap().text, "code");
        let big = serde_json::json!({"embeds": [{"description": "{text}", "footer": {"text": "x".repeat(2048)}, "fields": [{"name": "n", "value": "y".repeat(1024)}, {"name": "n", "value": "y".repeat(1024)}, {"name": "n", "value": "y".repeat(1024)}, {"name": "n", "value": "y".repeat(1024)}]}]});
        assert_eq!(
            frame_over("log.message", &big, &built),
            built,
            "over 6000 sends as built"
        );
        let long = Log::new("T", 5).text(&"x".repeat(4090)).build();
        let more = serde_json::json!({"embeds": [{"description": "More words here.\n{text}"}]});
        assert_eq!(
            frame_over("log.message", &more, &long),
            long,
            "a cut log sends as built"
        );
        // An edited version without {text} (saved before the save check, or in a second embed)
        // must never drop the log's own lines: the log is sent as built.
        for no_text in [
            serde_json::json!({"embeds": [{"title": "{title}", "description": "Something happened."}]}),
            serde_json::json!({"embeds": [{"description": "Hi"}, {"description": "{text}"}]}),
            serde_json::json!({"embeds": [{"description": "x", "image": {"url": "https://a/{text}"}}]}),
        ] {
            let e = frame_over("log.message", &no_text, &built);
            assert!(
                e.description.as_deref().unwrap().contains("#a"),
                "{no_text}"
            );
        }
        // A type frame lays over the log like its group's frame does.
        let e = frame_over("log.message.message_deleted", &stored, &built);
        assert_eq!(e.title.as_deref(), Some("🗑️ Message Deleted"));
        for k in ["anti_nuke", "panic", "join_raid"] {
            let d = brim_logging::botmsg::find(&format!("log.{k}")).unwrap();
            // Editable, but only with its title and details as plain text (plan section 6).
            assert!(d.security && d.required == ["title", "text"], "{k}");
        }
    }

    #[test]
    fn sees_follows_base_and_overwrites() {
        let (view, none) = (Permissions::VIEW_CHANNEL, Permissions::empty());
        assert!(sees(view, &[]));
        assert!(!sees(view, &[(none, view)]));
        assert!(!sees(none, &[]));
        assert!(sees(none, &[(view, none)]));
        assert!(sees(Permissions::ADMINISTRATOR, &[(none, view)]));
        // Hidden from @everyone but opened for a main role.
        assert!(sees(view, &[(none, view), (view, none)]));
    }

    /// A log channel id that names another server's channel is refused at send time, even when
    /// that channel is private and was already checked for its own server (security review-17).
    #[tokio::test]
    async fn a_log_never_goes_to_another_servers_channel() {
        let Some(t) = crate::test_support::T::new(992).await else {
            return;
        };
        const CH: u64 = 4242;
        let cfg = serde_json::json!({"logging": {"routes": {"mod_command": CH.to_string()}}});
        t.server_config(0, t.owner(), cfg.clone()).await;
        t.server_config(1, t.owner(), cfg).await;
        let (a, b) = (t.g(0), t.g(1));
        t.set(|m| {
            m.text_channels.insert(CH, (b.get(), false));
        });
        let log = || Log::new("Test", RED).text("x");
        // Its own server first, so the cache holds "private" for the channel.
        send_log(&t.app, b.get(), LogKind::ModCommand, log())
            .await
            .unwrap();
        let e = send_log(&t.app, a.get(), LogKind::ModCommand, log())
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("not in this server"), "{e:#}");
        assert_eq!(t.calls(&format!("POST /channels/{CH}/messages")), 1);
        t.cleanup().await;
    }

    /// The edit of a log type wins; a type without one takes its group's edit (an edit from before
    /// the per-type entries); a locked group and a title with no entry stay as built.
    #[tokio::test]
    async fn a_log_type_edit_beats_its_group_edit() {
        let Some(t) = crate::test_support::T::new(993).await else {
            return;
        };
        let g = t.g(0);
        let put = |key: &'static str, title: &'static str| {
            let pool = t.app.pool.clone();
            async move {
                sqlx::query("insert into bot_messages (guild_id, key, message, updated_by) values ($1, $2, $3, 1)")
                    .bind(g.get() as i64)
                    .bind(key)
                    .bind(serde_json::json!({"embeds": [{"title": title, "description": "{text}"}]}))
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        put("log.message", "Group edit").await;
        let built = |t: &str| Log::new(t, 5).line("Channel", "#a").build();
        let title = |e: Embed| e.title.unwrap();
        assert_eq!(
            title(framed(&t.app, g, "log.message", &built("Message Deleted")).await),
            "Group edit"
        );
        put("log.message.message_deleted", "Type edit").await;
        assert_eq!(
            title(framed(&t.app, g, "log.message", &built("Message Deleted")).await),
            "Type edit"
        );
        assert_eq!(
            title(framed(&t.app, g, "log.message", &built("Message Edited")).await),
            "Group edit"
        );
        assert_eq!(
            title(framed(&t.app, g, "log.anti_nuke", &built("Anti Nuke")).await),
            "Anti Nuke"
        );
        // A security log in an ordinary group never takes the group's frame.
        put("log.auto_mod", "Group edit").await;
        assert_eq!(
            title(framed(&t.app, g, "log.auto_mod", &built("NSFW Image Found")).await),
            "NSFW Image Found"
        );
        assert_eq!(
            title(framed(&t.app, g, "log.auto_mod", &built("Warn Action")).await),
            "Group edit"
        );
        t.cleanup().await;
    }
}

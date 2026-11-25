//! Prefix mod commands: permit check -> action -> case (+ undo data). ban/kick/timeout/warn.

use anyhow::Result;
use brim_antinuke::Action as NukeAction;
use brim_automod::lockdown::{kind_from, LockKind};
use brim_core::{
    cases, jobs, permits,
    sections::{AntinukeCfg, MiscCfg},
    warn,
};
use brim_logging::{
    embed::{self, duration, ARROW, ORANGE, RED},
    Log, LogKind,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use twilight_http::{request::AuditLogReason, Client};
use twilight_model::channel::message::Component;
use twilight_model::http::attachment::Attachment;
use twilight_model::{
    channel::{message::AllowedMentions, ChannelType},
    gateway::payload::incoming::MessageCreate,
    id::{
        marker::{GuildMarker, UserMarker},
        Id,
    },
    util::Timestamp,
};

use crate::{
    antinuke,
    app::{now_secs, App},
    appeals, backup,
    confirm::{ask_confirm, Action, Pending},
    hierarchy::top_position,
    lockdown, modcmds,
    prefix::{Flags, Parsed},
    reply::{self, Applied},
    slash::BackupCmd,
    targets,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ModKind {
    Ban,
    Kick,
    Timeout,
    Warn,
}

impl ModKind {
    /// Doubles as the permit name, the rate-limit action and the case kind.
    pub fn name(self) -> &'static str {
        match self {
            ModKind::Ban => "ban",
            ModKind::Kick => "kick",
            ModKind::Timeout => "timeout",
            ModKind::Warn => "warn",
        }
    }
}

/// The target outranks the bot when its top role is not below the bot's (the guild owner always does).
fn above_bot(target_top: Option<i64>, bot_top: i64, target_is_owner: bool) -> bool {
    target_is_owner || target_top.is_some_and(|t| t >= bot_top)
}

pub(crate) async fn target_above_bot(
    app: &App,
    gid: Id<GuildMarker>,
    target: Id<UserMarker>,
) -> Result<bool> {
    let info = app.guild_info(gid).await?;
    if target == info.owner {
        return Ok(true);
    }
    let top = crate::http_err::member(&app.http, gid, target)
        .await?
        .map(|m| top_position(&info.roles, &m.roles));
    Ok(above_bot(top, info.bot_top, false))
}

/// Refuses while panic mode locks mod commands; on a rate-limit breach the actor is quarantined.
pub(crate) async fn gated(
    app: &App,
    gid: Id<GuildMarker>,
    actor_id: Id<UserMarker>,
    actor: &permits::Level,
    action: &str,
) -> Result<Option<String>> {
    if app.mod_commands_locked(gid.get()) && !actor.immune() {
        return Ok(Some("Mod commands are locked during panic mode".into()));
    }
    let cfg = app.cfg(gid.get()).await?;
    let fallback = nuke_fallback(&cfg.antinuke, action);
    let own = permits::command_limit(actor, action, None);
    let Some(limit) = own.or(permits::command_limit(actor, action, fallback)) else {
        return Ok(None);
    };
    let action = limit_key(own.is_some(), action);
    let ok = app.rate_limiter.lock().unwrap().allow(
        gid.get() as i64,
        actor_id.get() as i64,
        action,
        limit,
        std::time::Instant::now(),
    );
    // Kept for a restart (N3); a lost row only makes the limit forget one use.
    let kept =
        sqlx::query("INSERT INTO command_uses (guild_id, actor_id, action) VALUES ($1, $2, $3)")
            .bind(gid.get() as i64)
            .bind(actor_id.get() as i64)
            .bind(action)
            .execute(&app.pool)
            .await;
    if let Err(e) = kept {
        tracing::warn!(?e, "saving a command use failed");
    }
    if ok {
        return Ok(None);
    }
    antinuke::quarantine_actor(app, gid, actor_id, "command rate limit exceeded").await?;
    // What the bot already did for them (bans, kicks) is undone, as for a nuke.
    if let Err(e) = antinuke::revert_breach(app, gid, actor_id, &cfg.backups).await {
        tracing::warn!(?e, "reverting a command rate limit breach failed");
    }
    Ok(Some(format!(
        "Command rate limit hit for `{action}`: nothing done, you have been quarantined"
    )))
}

/// The rate limiter key of a command. Anti-nuke counts bans and kicks on one card, so under its
/// fallback limit they share one key too; a permit's own limit stays per action.
fn limit_key(own: bool, action: &str) -> &str {
    match (own, action) {
        (false, "ban" | "kick") => "ban+kick",
        _ => action,
    }
}

/// Anti-nuke's limit for the same action, for a permit without its own. Anti-nuke trips when
/// a count reaches its limit and the permit limit when one goes past it, so one less.
fn nuke_fallback(an: &AntinukeCfg, action: &str) -> Option<permits::Limit> {
    let card = match action {
        "ban" => NukeAction::Ban,
        "kick" => NukeAction::Kick,
        "timeout" => NukeAction::Timeout,
        _ => return None,
    };
    let (l, _) = an.limit(card).filter(|_| an.enabled)?;
    Some(permits::Limit {
        per_min: l.per_minute.saturating_sub(1),
        per_hour: l.per_hour.saturating_sub(1),
    })
}

/// Readable reason for a refused permit check.
/// `level`: the target's, so a protected target is named immune only when they are.
pub(crate) fn deny_text(
    perm: &str,
    target: u64,
    level: &permits::Level,
    d: &permits::Denied,
) -> String {
    let why = match d {
        permits::Denied::NoPermit => format!("you lack the {perm} permit"),
        permits::Denied::TargetProtected if level.immune() => "they are immune".into(),
        permits::Denied::TargetProtected => "they hold a permit".into(),
        permits::Denied::TargetAboveBot => "their top role is above the bot".into(),
        permits::Denied::SelfTarget => "you cannot target yourself".into(),
    };
    format!("Cannot {perm} <@{target}>: {why}")
}

/// Permit check -> hierarchy -> rate limit -> DM -> action -> case. Err only for infrastructure
/// failures. `points` only matters for warns.
#[allow(clippy::too_many_arguments)]
pub async fn apply(
    app: &App,
    gid: Id<GuildMarker>,
    owner: Id<UserMarker>,
    actor: (Id<UserMarker>, &permits::Level),
    kind: ModKind,
    target: Id<UserMarker>,
    reason: Option<&str>,
    duration_s: Option<i64>,
    points: i32,
) -> Result<Applied> {
    apply_dm(
        app,
        gid,
        owner,
        actor,
        kind,
        target,
        reason,
        duration_s,
        points,
        Flags::default(),
        None,
    )
    .await
}

/// [`apply`] with the prefix flags: `dm` tells the target even when the server's DM toggle is off,
/// `soft` turns a ban into a softban, `no_avatar` / `voice` limit a kick, `compact` shortens the reply.
#[allow(clippy::too_many_arguments)]
pub async fn apply_dm(
    app: &App,
    gid: Id<GuildMarker>,
    owner: Id<UserMarker>,
    (actor_id, actor): (Id<UserMarker>, &permits::Level),
    kind: ModKind,
    target: Id<UserMarker>,
    reason: Option<&str>,
    duration_s: Option<i64>,
    points: i32,
    flags: Flags,
    proof: Option<&str>,
) -> Result<Applied> {
    let perm = kind.name();
    let t = target.get();
    let refused = |text: String| Applied {
        target: Some(t),
        ..text.into()
    };
    let target_level = app.level(gid, owner, target).await?;
    let above = kind != ModKind::Warn && target_above_bot(app, gid, target).await?;
    if let Err(d) = permits::check(
        actor,
        perm,
        actor_id.get() as i64,
        &target_level,
        target.get() as i64,
        above,
    ) {
        return Ok(refused(deny_text(perm, t, &target_level, &d)));
    }
    if let Some(msg) = gated(app, gid, actor_id, actor, perm).await? {
        return Ok(refused(msg));
    }
    if kind == ModKind::Kick {
        if let Some(why) = kick_filtered(app, gid, target, flags).await? {
            return Ok(refused(format!("Skipped <@{t}>: {why}")));
        }
    }
    let cfg = app.cfg(gid.get()).await?;
    let soft = flags.soft && kind == ModKind::Ban;
    let name = if soft { "softban" } else { perm };
    let dm_kind = match kind {
        ModKind::Ban if duration_s.is_some() && !soft => "tempban",
        _ => name,
    };
    let appeal = appeals::note(&cfg.appeals, &cfg.misc.ban_appeal_message, gid)
        .filter(|_| matches!(kind, ModKind::Ban | ModKind::Timeout) && !soft);
    let timeout_s = duration_s.unwrap_or(cfg.misc.timeout_secs() as i64);
    // A timeout always lasts: the server default when no length was given.
    let duration_s = if kind == ModKind::Timeout {
        Some(timeout_s)
    } else {
        duration_s
    };
    // A softban of someone not in the server would lift their ban.
    if soft
        && crate::http_err::member(&app.http, gid, target)
            .await?
            .is_none()
    {
        return Ok(refused(format!(
            "Skipped <@{t}>: they are not in the server, so a softban would unban them. Use ban."
        )));
    }
    let notice = reply::Notice {
        kind: dm_kind,
        reason,
        duration_s,
        moderator: (!cfg.misc.hide_staff_in_dms).then_some(actor_id),
        appeal,
    };
    // A hackban targets ids that left the server: a DM per raid id would only cost DM_WAIT each.
    let dm = (reply::wants_dm(flags.dm, &cfg.misc) && !flags.hack)
        .then(|| reply::tell(app, gid, target, notice));
    let audit = audit_reason(reason, actor_id);
    // A temp ban's unban job, finished again when the ban fails: it must not lift a later ban.
    let mut unban_job = None;
    let act = async {
        anyhow::Ok(match kind {
            ModKind::Ban if soft => {
                // A softban exists to purge messages, so purge a day when the server default is none.
                app.http
                    .create_ban(gid, target)
                    .delete_message_seconds(cfg.misc.purge_secs().max(86_400))
                    .reason(&audit)
                    .await?;
                // The ban stands: an outage on the unban is retried by a job, never a lasting ban.
                let retry = match app.http.delete_ban(gid, target).reason(&audit).await {
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!(?e, "softban unban failed, queued again");
                        let g = gid.get() as i64;
                        Some(
                            jobs::enqueue(&app.pool, g, "unban", json!({"user":t}), now_secs())
                                .await?,
                        )
                    }
                };
                crate::scheduler::forget_jobs(app, gid, t, &["unban"], retry).await;
                (None, 0)
            }
            ModKind::Ban => {
                // Queued first: a restart right after the ban must not make it permanent. If the ban
                // fails, the job is finished below.
                if let Some(d) = duration_s {
                    let (g, at) = (gid.get() as i64, now_secs() + d);
                    unban_job =
                        Some(jobs::enqueue(&app.pool, g, "unban", json!({"user":t}), at).await?);
                }
                app.http
                    .create_ban(gid, target)
                    .delete_message_seconds(cfg.misc.purge_secs())
                    .reason(&audit)
                    .await?;
                crate::scheduler::forget_jobs(app, gid, t, crate::scheduler::BANNED, unban_job)
                    .await;
                (Some(json!({"op":"unban","user":t})), 0)
            }
            ModKind::Kick => {
                app.http
                    .remove_guild_member(gid, target)
                    .reason(&audit)
                    .await?;
                (None, 0)
            }
            ModKind::Timeout => {
                timeout_until(app, gid, target, now_secs() + timeout_s, &audit).await?;
                (Some(json!({"op":"untimeout","user":t})), 0)
            }
            ModKind::Warn => (None, points),
        })
    };
    // The DM goes first: a kicked or banned member can't be reached afterwards.
    let (told, acted) = reply::dm_and_act(reply::dm_first(dm_kind), dm, act).await;
    // Only a refusal (4xx): after an outage the ban may stand, and its unban must still run.
    if let (Err(e), Some(id)) = (&acted, unban_job) {
        if crate::http_err::refused(e) {
            jobs::finish(&app.pool, id).await?;
        }
    }
    let (inverse, points) = match acted {
        Ok(a) => a,
        // An outage is an infrastructure failure, so a job retries it instead of giving up.
        Err(e) if !crate::http_err::refused(&e) => return Err(e),
        // Keep the DM outcome: the member may already have been told.
        Err(e) => {
            tracing::warn!(?e, "mod command failed");
            let why = crate::http_err::why(&e);
            let text = format!("Failed to {perm} <@{t}>: {why}");
            return Ok(Applied {
                dm: told,
                ..refused(text)
            });
        }
    };
    let case_no = cases::create(
        &app.pool,
        cases::NewCase {
            guild_id: gid.get() as i64,
            kind: if soft { "softban" } else { perm },
            user_id: t as i64,
            mod_id: actor_id.get() as i64,
            reason,
            points,
            duration_s,
            proof,
            inverse,
            feature: if actor_id == app.me { "brim" } else { "staff" },
        },
    )
    .await?;
    let log = mod_log_embed(
        (case_no, name),
        (actor_id.get(), target.get()),
        duration_s,
        reason,
    );
    let kept = send_case_log(app, gid, (case_no, name), log, &[], proof).await;
    let mut text = reply::case_text(
        cfg.misc.compact_replies || flags.compact,
        (case_no.into(), name, t),
        reason,
        actor_id.get(),
    );
    if let Some(k) = kept {
        text += format!("\n{k}").as_str();
    }
    if kind == ModKind::Warn {
        // The warn is saved: a failed auto action is told, not returned as a failed warn (the
        // moderator would warn again and the points would double).
        match apply_warn_actions(app, &cfg.misc, gid, target, points).await {
            Ok(Some((extra, _))) => text += format!("\n{extra}").as_str(),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(?e, "warn auto action failed");
                let why = crate::http_err::why(&e);
                text += format!("\nWarn saved, but the auto action failed: {why}").as_str();
            }
        }
    }
    Ok(Applied {
        target: Some(t),
        case: Some(case_no),
        text,
        dm: told,
        posted: false,
    })
}

/// Why `?na` / `?vc` leave this member out of a kick, if they do.
async fn kick_filtered(
    app: &App,
    gid: Id<GuildMarker>,
    target: Id<UserMarker>,
    flags: Flags,
) -> Result<Option<&'static str>> {
    if flags.no_avatar {
        match crate::http_err::member(&app.http, gid, target).await? {
            None => return Ok(Some("they are not in the server")),
            Some(m) if m.avatar.is_some() || m.user.avatar.is_some() => {
                return Ok(Some(
                    "they have an avatar (`?na` kicks only members without one)",
                ))
            }
            Some(_) => {}
        }
    }
    if flags.voice {
        // Discord answers 404 when the member is in no voice channel; other errors skip them too,
        // since a kick that may not match the filter is worse than one left out.
        let in_voice = app.http.user_voice_state(gid, target).await.is_ok();
        if !in_voice {
            return Ok(Some(
                "they are not in a voice channel (`?vc` kicks only members in one)",
            ));
        }
    }
    Ok(None)
}

/// The bot stays silent for quarantined members and in a voice channel's chat (purge excepted).
pub async fn ignored(
    app: &App,
    msg: &MessageCreate,
    gid: Id<GuildMarker>,
    rest: &str,
) -> Result<bool> {
    let held: Option<(i64,)> =
        sqlx::query_as("SELECT 1::bigint FROM quarantines WHERE guild_id=$1 AND user_id=$2")
            .bind(gid.get() as i64)
            .bind(msg.author.id.get() as i64)
            .fetch_optional(&app.pool)
            .await?;
    if held.is_some() {
        return Ok(true);
    }
    if crate::prefix::parse(rest).is_some_and(|p| p.command == "purge") {
        return Ok(false);
    }
    // Cached: the channel fetch is only for a channel the cache does not know yet.
    let ch = msg.channel_id;
    if let Ok(info) = app.guild_info(gid).await {
        if info.channels.contains(&ch) {
            return Ok(info.voice.contains(&ch));
        }
    }
    if app.thread_parents.lock().unwrap().contains_key(&ch) {
        return Ok(false);
    }
    let Ok(r) = app.http.channel(ch).await else {
        return Ok(false);
    };
    let Ok(c) = r.model().await else {
        return Ok(false);
    };
    Ok(matches!(
        c.kind,
        ChannelType::GuildVoice | ChannelType::GuildStageVoice
    ))
}

/// Names one mod command looks up; each is a member search.
const MAX_NAMES: usize = 10;

/// Every command name [`run`] answers; the invoking message is deleted for these when `delete_invocation` is on.
pub(crate) const KNOWN: &[&str] = &[
    "ban",
    "softban",
    "kick",
    "warn",
    "timeout",
    "verifysetup",
    "lock",
    "unlock",
    "unban",
    "hackban",
    "untimeout",
    "unwarn",
    "quarantine",
    "unquarantine",
    "purge",
    "slowmode",
    "cases",
    "case",
    "verify",
    "sanitize",
    "note",
    "notes",
    "editc",
    "editreason",
    "closecase",
    "caseoff",
    "opencase",
    "caseon",
    "clearwarns",
    "warns",
    "modcases",
];

/// Prefix entry: resolves names, then runs [`apply`] per target and posts each reply.
pub async fn run(app: &App, msg: &MessageCreate, mut p: Parsed) -> Result<()> {
    let Some(gid) = msg.guild_id else {
        return Ok(());
    };
    p.flags.soft |= p.command == "softban";
    p.flags.hack |= p.command == "hackban";
    if KNOWN.contains(&p.command.as_str()) {
        let cfg = app.cfg(gid.get()).await?;
        if cfg.misc.delete_invocation {
            let owner = app.guild_info(gid).await?.owner;
            if app.level(gid, owner, msg.author.id).await? != permits::Level::None {
                let r = app.http.delete_message(msg.channel_id, msg.id).await;
                crate::http_err::best_effort(r, "delete command message");
            }
        }
        if p.too_long {
            let owner = app.guild_info(gid).await?.owner;
            if app.level(gid, owner, msg.author.id).await? == permits::Level::None {
                return Ok(());
            }
            return wrong_use(app, msg, gid, brim_core::duration::TOO_LONG).await;
        }
    }
    let kind = match p.command.as_str() {
        "ban" | "softban" | "hackban" => ModKind::Ban,
        "kick" => ModKind::Kick,
        "warn" => ModKind::Warn,
        "timeout" => ModKind::Timeout,
        "verifysetup" => {
            if !app.is_immune(gid, msg.author.id).await? {
                return Ok(());
            }
            let said = crate::verify::setup(app, gid).await?;
            return say_said(app, msg, &said).await;
        }
        "lock" | "unlock" => {
            let owner = app.guild_info(gid).await?.owner;
            let actor = app.level(gid, owner, msg.author.id).await?;
            if p.kind.as_deref() == Some("update") {
                let text = p.input.clone().unwrap_or_else(|| p.names.join(", "));
                let said =
                    lockdown::update(app, gid, msg.channel_id, (msg.author.id, &actor), &text)
                        .await?;
                return say_said(app, msg, &said).await;
            }
            // `lock role`, `lock server`, `lock channel`...: one-off type instead of the dashboard default.
            // Channel mentions lock (or unlock) those channels instead of this one.
            let channels = match p.channels.as_slice() {
                [] => vec![msg.channel_id.get()],
                c => c.to_vec(),
            };
            let name = p.kind.as_ref().or(p.names.first()).map(String::as_str);
            // `-blind` turns a server lock (or the default) into a blind one.
            let name = match (p.flags.blind, name) {
                (true, None | Some("server" | "sc")) => Some("blind"),
                (_, n) => n,
            };
            let name = name.or((!p.channels.is_empty()).then_some("c"));
            let mut kinds = vec![];
            for c in &channels {
                match name.map(|n| kind_from(n, Some(*c))) {
                    Some(Some(k)) if !kinds.contains(&k) => kinds.push(k),
                    Some(Some(_)) => {}
                    Some(None) => return wrong_use(app, msg, gid, lockdown::LOCK_TYPES).await,
                    None => {}
                }
            }
            let unlock = p.command == "unlock";
            if name.is_none() {
                if !unlock {
                    // A bare `lock` lists the active locks instead of locking.
                    let e = lockdown::overview(app, gid).await?;
                    return say_embeds(app, msg, "", &[e]).await;
                }
                // A bare `unlock` in a locked channel lifts that channel only.
                let here = LockKind::Channel(msg.channel_id.get());
                if lockdown::active(app, gid).await?.contains(&here) {
                    kinds.push(here);
                }
            }
            let kinds: Vec<Option<LockKind>> = match kinds.is_empty() {
                true => vec![None],
                false => kinds.into_iter().map(Some).collect(),
            };
            let compact = cfg_compact(app, gid, p.flags).await?;
            // Server-wide locks take a while: show a loading reply, then edit in the result.
            let pending = match kinds.as_slice() {
                [k] if !compact && !matches!(k, Some(LockKind::Channel(_))) => Some(
                    send_reply(
                        app,
                        msg,
                        "",
                        &[lockdown::loading(app, gid, k.as_ref(), unlock).await],
                        true,
                    )
                    .await?,
                ),
                _ => None,
            };
            let mut out = vec![];
            for kind in kinds {
                let actor = (msg.author.id, &actor);
                out.push(lockdown::command(app, gid, actor, unlock, kind, p.duration_s).await?);
            }
            if compact {
                let text: Vec<String> = out.into_iter().map(|r| r.text).collect();
                return say(app, msg, &text.join("\n")).await;
            }
            // shortcut: Discord shows at most 10 embeds, so more than 10 channels show the first 10.
            let embeds: Vec<_> = out.into_iter().take(10).map(|r| r.embed).collect();
            return match pending {
                Some(m) => {
                    let framed = reply::command_reply(app, true, "", &embeds).await;
                    let embeds = framed.map_or(embeds, |f| f.1);
                    app.http
                        .update_message(msg.channel_id, m.id)
                        .embeds(Some(&embeds))
                        .await?;
                    Ok(())
                }
                None => say_embeds(app, msg, "", &embeds).await,
            };
        }
        "unban" | "untimeout" | "unwarn" | "quarantine" | "unquarantine" | "purge" | "slowmode"
        | "cases" | "case" | "verify" | "sanitize" | "note" | "notes" | "editc" | "editreason"
        | "proof" | "closecase" | "caseoff" | "opencase" | "caseon" | "clearwarns" | "warns"
        | "modcases" => return modcmds::run_prefix(app, msg, p).await,
        "backups" => return backup_prefix(app, msg, gid, &p).await,
        _ => return Ok(()),
    };
    let owner = app.guild_info(gid).await?.owner;
    let cfg = app.cfg(gid.get()).await?;
    // Before any name lookup, so a member without the permit cannot make the bot search for and
    // answer hundreds of names.
    let level = app.level(gid, owner, msg.author.id).await?;
    if !level.has(kind.name()) {
        if cfg.misc.ignore_no_permit && level == permits::Level::None {
            return Ok(());
        }
        let vars = [("permit", kind.name())];
        return say_key(app, msg, gid, "reply.error.no_permit", &vars).await;
    }
    if p.names.len() > MAX_NAMES {
        let text = format!("At most {MAX_NAMES} names per command. Use mentions or ids for more.");
        return wrong_use(app, msg, gid, &text).await;
    }
    let mut targets: Vec<Id<UserMarker>> = p.targets.iter().map(|t| Id::new(*t)).collect();
    let mut missed = vec![];
    for n in &p.names {
        match targets::resolve(app, gid, n).await? {
            Ok(id) => targets.push(id),
            Err(e) => missed.push(e),
        }
    }
    if !missed.is_empty() {
        say(app, msg, &missed.join("\n")).await?;
    }
    // A run with no target would report "All members were warned!" (live 2026-11-13). Failed
    // names were answered above; a role mention is not a target.
    if targets.is_empty() {
        if p.names.is_empty() {
            wrong_use(app, msg, gid, "Name a member: a mention, an id or a name.").await?;
        }
        return Ok(());
    }
    let unsure = p.unsure();
    let job = Pending::new(
        gid,
        msg.author.id,
        Action::Mod {
            kind,
            targets,
            reason: p.reason,
            duration_s: p.duration_s,
            flags: p.flags,
            proof: None,
            unsure,
            reply: crate::confirm::reply_of(msg),
        },
    );
    if job.needs_confirm(cfg.misc.confirm) {
        return ask_confirm(app, msg, job).await;
    }
    let done = job.run(app, owner, cfg.misc.warn_base_points).await?;
    let t = reply::ModTexts::load(app, gid).await;
    let (text, embeds) = job.reply(&t, &done, cfg.misc.compact_replies, cfg.misc.timeout_secs());
    say_embeds(app, msg, &text, &embeds).await
}

async fn cfg_compact(app: &App, gid: Id<GuildMarker>, flags: Flags) -> Result<bool> {
    Ok(flags.compact || app.cfg(gid.get()).await?.misc.compact_replies)
}

/// Discord's audit log caps reasons at 512 characters.
pub(crate) fn audit_reason(reason: Option<&str>, actor: Id<UserMarker>) -> String {
    embed::clip(
        &format!(
            "{} | Moderator: {actor}",
            reason.unwrap_or("No reason provided")
        ),
        500,
    )
}

/// Mod log: no title, Case / Type / Moderator columns, then Target, Duration and Reason, with the
/// case's undo buttons.
pub(crate) async fn mod_log(
    app: &App,
    gid: Id<GuildMarker>,
    (case_no, name): (i32, &str),
    (actor, target): (Id<UserMarker>, Id<UserMarker>),
    duration_s: Option<i64>,
    reason: Option<&str>,
) {
    let log = mod_log_embed(
        (case_no, name),
        (actor.get(), target.get()),
        duration_s,
        reason,
    );
    send_mod_log_files(app, gid, log, &[], &crate::undo::rows(case_no, name)).await
}

async fn send_mod_log_files(
    app: &App,
    gid: Id<GuildMarker>,
    log: Log,
    files: &[(String, String)],
    rows: &[Component],
) {
    let kind = LogKind::ModCommand;
    let r = crate::events::send_log_files(app, gid.get(), kind, log, files, &[], rows);
    if let Err(e) = r.await {
        tracing::warn!(?e, "mod log failed");
    }
}

/// Largest proof file copied onto the mod log (Discord's upload limit for a bot without boosts).
const PROOF_MAX: usize = 25 << 20;

/// The case's mod log, with its proof file on the same message. Discord's file links stop working
/// after about a day, so the case keeps that message's link instead. A proof that could not be
/// kept that way gives the line that tells the moderator.
pub(crate) async fn send_case_log(
    app: &App,
    gid: Id<GuildMarker>,
    (case_no, kind): (i32, &str),
    log: Log,
    files: &[(String, String)],
    proof: Option<&str>,
) -> Option<String> {
    let rows = crate::undo::rows(case_no, kind);
    let Some(url) = proof else {
        send_mod_log_files(app, gid, log, files, &rows).await;
        return None;
    };
    let copy = match download_proof(app, url).await {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!(?e, "copying the proof file failed");
            None
        }
    };
    let mut attach: Vec<Attachment> = copy.into_iter().collect();
    let kind = LogKind::ModCommand;
    let mut sent =
        crate::events::send_log_files(app, gid.get(), kind, log.clone(), files, &attach, &rows)
            .await;
    if let (Err(e), false) = (&sent, attach.is_empty()) {
        // Discord refused the file (too large for the server, or a hiccup): the case still gets
        // its mod log, without the copy.
        tracing::warn!(?e, "mod log with the proof file failed, posting it without");
        attach.clear();
        sent = crate::events::send_log_files(app, gid.get(), kind, log, files, &[], &rows).await;
    }
    const SHORT: &str =
        "so the case keeps only Discord's file link, which stops working after about a day";
    match sent {
        Ok(Some((ch, msg))) if !attach.is_empty() => {
            let link = format!("https://discord.com/channels/{gid}/{ch}/{msg}");
            // The proof row `cases::create` made from the file's link now names its kept copy.
            let saved = sqlx::query(
                "UPDATE case_proofs SET kind = 'image', body = $3
                 WHERE guild_id = $1 AND case_no = $2 AND body = $4 AND NOT verified",
            )
            .bind(gid.get() as i64)
            .bind(case_no)
            .bind(&link)
            .bind(url)
            .execute(&app.pool)
            .await;
            match saved {
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(?e, "saving the proof link failed");
                    Some(format!("The proof is on the mod log, but saving its link to the case failed, {SHORT}."))
                }
            }
        }
        Ok(Some(_)) => Some(format!(
            "The proof file could not be copied to the mod log, {SHORT}."
        )),
        Ok(None) => Some(format!(
            "No Mod Logs channel is set, {SHORT}. Set one to keep proof files."
        )),
        Err(e) => {
            tracing::warn!(?e, "mod log failed");
            Some(format!("The mod log could not be posted, {SHORT}."))
        }
    }
}

/// The `proof:` file, fetched while its fresh link still works. Only Discord's own file hosts.
async fn download_proof(app: &App, url: &str) -> Result<Attachment> {
    let host = reqwest::Url::parse(url)?.host_str().map(str::to_owned);
    anyhow::ensure!(
        matches!(
            host.as_deref(),
            Some("cdn.discordapp.com" | "media.discordapp.net")
        ),
        "proof is not a Discord file link"
    );
    // The shared client has a timeout; a bare `reqwest::get` could hang the reply.
    let r = app.api.client().get(url).send().await?.error_for_status()?;
    let host = r.url().host_str().map(str::to_owned);
    // A redirect must not take the download off Discord's hosts.
    anyhow::ensure!(
        matches!(
            host.as_deref(),
            Some("cdn.discordapp.com" | "media.discordapp.net")
        ),
        "the proof download left Discord's file hosts"
    );
    let name = r
        .url()
        .path_segments()
        .and_then(|mut p| p.next_back())
        .filter(|n| !n.is_empty())
        .unwrap_or("proof")
        .to_string();
    let bytes = brim_automod::sites::read_capped(r, PROOF_MAX).await?;
    Ok(Attachment::from_bytes(name, bytes, 0))
}

pub(crate) fn mod_log_embed(
    (case_no, name): (i32, &str),
    (actor, target): (u64, u64),
    duration_s: Option<i64>,
    reason: Option<&str>,
) -> Log {
    let moderator = embed::user(actor);
    // A case without a member (target 0: a purge of a channel, a slowmode) shows `None`.
    let member = match target {
        0 => "`None`".to_string(),
        t => embed::user(t),
    };
    let mut log = Log::new("", case_color(name))
        .inline("Case:", &format!("`{case_no}`"))
        .inline("Type:", &format!("`{}`", case_title(name)))
        .inline("Moderator:", &moderator)
        .field("Target:", &format!("{ARROW} {member}"));
    let mut shown = String::new();
    if let Some(d) = duration_s {
        shown = format!("`{}`", duration(d));
        log = log.field("Duration:", &shown);
    }
    let reason = reason.unwrap_or("No reason provided");
    // The values an edited version of the entry uses (`log.mod_command.case`); a purge adds its own.
    log.field("Reason:", reason)
        .entry("log.mod_command.case")
        .var("case", &case_no.to_string())
        .var("kind", &case_title(name))
        .var("member", &member)
        .var("moderator", &moderator)
        .var("duration", &shown)
        .var("reason", reason)
        .var("channel", "")
        .var("deleted", "")
        .var("too_old", "")
        .var("filter", "")
}

fn case_title(kind: &str) -> String {
    let mut c = kind.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn case_color(kind: &str) -> u32 {
    match kind {
        "ban" | "softban" => RED,
        _ => ORANGE,
    }
}

pub(crate) async fn apply_warn_actions(
    app: &App,
    misc: &MiscCfg,
    g: Id<GuildMarker>,
    u: Id<UserMarker>,
    added: i32,
) -> Result<Option<(String, bool)>> {
    if !misc.auto_warn_actions {
        return Ok(None);
    }
    let (gid, uid) = (g.get() as i64, u.get() as i64);
    let after = cases::warn_points(&app.pool, gid, uid).await?;
    let Some(t) = warn::crossed(&misc.warn_actions, after - added, after) else {
        return Ok(warn::next(&misc.warn_actions, after).map(|n| {
            (
                format!("`{}` points away from `{:?}`", n.points - after, n.action),
                false,
            )
        }));
    };
    let why = format!("Reached {} warn points", t.points);
    let (kind, secs) = match t.action {
        warn::Action::Ban => ("ban", None),
        warn::Action::Kick => ("kick", None),
        warn::Action::Timeout { secs: 0 } => ("timeout", Some(misc.timeout_secs() as i64)),
        warn::Action::Timeout { secs } => ("timeout", Some(secs)),
    };
    let notice = reply::Notice {
        kind,
        reason: Some(&why),
        duration_s: secs,
        moderator: (!misc.hide_staff_in_dms).then_some(app.me.cast()),
        appeal: None,
    };
    let dm = reply::wants_dm(false, misc).then(|| reply::tell(app, g, u, notice));
    let act = async {
        match secs {
            Some(s) => timeout(&app.http, g, u, s, &why).await?,
            None if kind == "ban" => drop(app.http.create_ban(g, u).reason(&why).await?),
            None => drop(app.http.remove_guild_member(g, u).reason(&why).await?),
        }
        anyhow::Ok(())
    };
    // Same rule as a typed punishment: tell the member first.
    reply::dm_and_act(true, dm, act).await.1?;
    if kind == "ban" {
        crate::scheduler::forget_jobs(app, g, u.get(), crate::scheduler::BANNED, None).await;
    }
    let case_no = cases::create(
        &app.pool,
        cases::NewCase {
            guild_id: gid,
            kind,
            user_id: uid,
            mod_id: app.me.get() as i64,
            reason: Some(&why),
            points: 0,
            duration_s: secs,
            proof: None,
            inverse: None,
            feature: "warn_points",
        },
    )
    .await?;
    mod_log(
        app,
        g,
        (case_no, kind),
        (app.me.cast(), u),
        secs,
        Some(&why),
    )
    .await;
    // The bot clears the warns once the punishment is applied.
    cases::reset_warns(&app.pool, gid, uid, app.me.get() as i64).await?;
    Ok(Some((
        format!("<@{uid}> reached {} warn points: {:?}", t.points, t.action),
        kind == "timeout",
    )))
}

/// Discord rejects timeouts longer than 28 days.
const MAX_TIMEOUT_S: i64 = 28 * 86_400;

/// Seconds to apply now, and whether a renewal must follow.
fn timeout_chunk(remaining: i64) -> (i64, bool) {
    (remaining.min(MAX_TIMEOUT_S), remaining > MAX_TIMEOUT_S)
}

/// Single capped timeout for callers that cannot renew (warn thresholds, automod). Keeps a longer
/// running timeout; a failed read times out anyway.
pub async fn timeout(
    http: &Client,
    g: Id<GuildMarker>,
    u: Id<UserMarker>,
    secs: i64,
    reason: &str,
) -> Result<()> {
    let until = now_secs() + timeout_chunk(secs).0;
    let running = crate::http_err::member(http, g, u).await.ok().flatten();
    if running
        .and_then(|m| m.communication_disabled_until)
        .is_some_and(|t| t.as_secs() >= until)
    {
        return Ok(());
    }
    http.update_guild_member(g, u)
        .communication_disabled_until(Some(Timestamp::from_secs(until)?))
        .reason(reason)
        .await?;
    Ok(())
}

/// Applies the next 28-day chunk of a timeout ending at `until` and queues a renewal when more remains.
pub async fn timeout_until(
    app: &App,
    g: Id<GuildMarker>,
    u: Id<UserMarker>,
    until: i64,
    reason: &str,
) -> Result<()> {
    let now = now_secs();
    let (chunk, more) = timeout_chunk(until - now);
    if chunk <= 0 {
        return Ok(());
    }
    app.http
        .update_guild_member(g, u)
        .communication_disabled_until(Some(Timestamp::from_secs(now + chunk)?))
        .reason(reason)
        .await?;
    let mut job = None;
    if more {
        // Renewed a day early so scheduler lag cannot open a gap.
        let p = json!({"user":u.get(),"until_secs":until});
        let at = now + chunk - 86_400;
        job = Some(jobs::enqueue(&app.pool, g.get() as i64, "timeout_renew", p, at).await?);
    }
    // After the PATCH: a failed one keeps the old timeout's renewal. One chain per member.
    crate::scheduler::forget_jobs(app, g, u.get(), &["timeout_renew"], job).await;
    Ok(())
}

pub(crate) async fn say(app: &App, msg: &MessageCreate, text: &str) -> Result<()> {
    say_embeds(app, msg, text, &[]).await
}

pub(crate) async fn say_embeds(
    app: &App,
    msg: &MessageCreate,
    text: &str,
    embeds: &[twilight_model::channel::message::Embed],
) -> Result<()> {
    send_reply(app, msg, text, embeds, true).await.map(drop)
}

/// Sends a short reply (see [`reply::say`]): an edited version as it is, the default through the
/// command's frame.
pub(crate) async fn say_said(app: &App, msg: &MessageCreate, said: &reply::Said) -> Result<()> {
    send_reply(
        app,
        msg,
        &said.text,
        &said.embeds,
        !said.edited && !said.security,
    )
    .await
    .map(drop)
}

/// Sends the short reply entry `key` with `vars` filled.
pub(crate) async fn say_key(
    app: &App,
    msg: &MessageCreate,
    gid: Id<GuildMarker>,
    key: &str,
    vars: &[(&str, &str)],
) -> Result<()> {
    say_said(app, msg, &reply::say(app, gid, key, vars).await).await
}

/// Sends `text` (what was wrong with the command as typed) in the wrong use entry.
pub(crate) async fn wrong_use(
    app: &App,
    msg: &MessageCreate,
    gid: Id<GuildMarker>,
    text: &str,
) -> Result<()> {
    say_key(app, msg, gid, "reply.error.wrong_use", &[("text", text)]).await
}

/// Sends a reply (deleted later if the server wants that) and returns it.
async fn send_reply(
    app: &App,
    msg: &MessageCreate,
    text: &str,
    embeds: &[twilight_model::channel::message::Embed],
    frame: bool,
) -> Result<twilight_model::channel::Message> {
    let framed = if frame {
        reply::command_reply(app, true, text, embeds).await
    } else {
        None
    };
    let (text, embeds) = framed.as_ref().map_or((text, embeds), |f| (&f.0, &f.1));
    let mut post = app.http.create_message(msg.channel_id).content(text);
    if let Some(to) = crate::cmdcfg::reply_to(msg.channel_id) {
        post = post.reply(to).fail_if_not_exists(false);
    }
    let sent = post
        .embeds(embeds)
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await?
        .model()
        .await?;
    if let Some(gid) = msg.guild_id {
        // The command's own "delete the answer" setting wins over the server's.
        let own = crate::cmdcfg::delete_secs(msg.channel_id);
        if let Some(secs) = own.or(app.cfg(gid.get()).await?.misc.delete_replies_secs) {
            reply::delete_later(app, gid, msg.channel_id, sent.id, secs).await?;
        }
    }
    Ok(sent)
}

/// `t!backups ID|take|load ID|delete ID|clear` and its aliases.
async fn backup_prefix(
    app: &App,
    msg: &MessageCreate,
    gid: Id<GuildMarker>,
    p: &Parsed,
) -> Result<()> {
    let owner = app.guild_info(gid).await?.owner;
    let actor = app.level(gid, owner, msg.author.id).await?;
    if actor == permits::Level::None {
        return Ok(());
    }
    let mut args = msg.content.split_whitespace().skip(1);
    // `bload dh0mfu`-style aliases carry the subcommand in `p.kind`.
    let sub = p
        .kind
        .clone()
        .or_else(|| args.next().map(str::to_lowercase));
    let id = args.next().map(str::to_owned);
    let cmd = match (sub.as_deref().unwrap_or("list"), id) {
        ("take" | "create", _) => BackupCmd::Create,
        ("list", _) => BackupCmd::List,
        ("clear", _) => BackupCmd::Clear,
        ("load", Some(id)) => BackupCmd::Load {
            id,
            confirm: msg.content.contains("?confirm"),
        },
        ("delete", Some(id)) => BackupCmd::Delete { id },
        (key, None) if key != "load" && key != "delete" => BackupCmd::Info { id: key.into() },
        _ => {
            let line = [(
                "text",
                "`backups [ID | take | load ID | delete ID | clear]`",
            )];
            return say_key(app, msg, gid, "reply.error.usage", &line).await;
        }
    };
    let said = backup_cmd(app, gid, (msg.author.id, &actor), cmd).await?;
    say_said(app, msg, &said).await
}

/// `/backups create|list|load|delete|purge` and `t!backups ID|take|load|delete|clear`. `load`
/// previews unless `confirm` is set.
pub async fn backup_cmd(
    app: &App,
    gid: Id<GuildMarker>,
    (actor_id, actor): (Id<UserMarker>, &permits::Level),
    cmd: BackupCmd,
) -> Result<reply::Said> {
    if !actor.has("backups") {
        return Ok(String::from("Cannot manage backups: you lack the backups permit").into());
    }
    if let Some(msg) = gated(app, gid, actor_id, actor, "backups").await? {
        return Ok(msg.into());
    }
    // The refusals that guard anti-nuke's restore source are security texts, never framed by
    // `cmd.backups` (a Trusted Admin may edit that frame).
    let no = |key: &'static str| reply::say(app, gid, key, &[]);
    // Loading deletes whatever the backup lacks, and clearing or deleting (ten deletes are a clear)
    // removes anti-nuke's restore source: owner level plus a fresh 2FA, as the dashboard asks
    // (S-h3 2). The load preview only reads.
    if matches!(
        cmd,
        BackupCmd::Clear | BackupCmd::Delete { .. } | BackupCmd::Load { confirm: true, .. }
    ) {
        if !matches!(actor, permits::Level::Owner | permits::Level::ExtraOwner) {
            return Ok(no("reply.backups.owners_load").await);
        }
        if !crate::settings::fresh_2fa(app, actor_id.get(), gid.get()).await? {
            return Ok(crate::settings::needs_2fa(app, gid).await);
        }
    }
    // A nuke under way should not lose its restore source to a hijacked owner account.
    if matches!(cmd, BackupCmd::Clear | BackupCmd::Delete { .. })
        && backup::locked_or_panic(app, gid).await?
    {
        return Ok(no("reply.backups.locked").await);
    }
    // `backup::load` refuses this too (for its other callers); here it is the security entry.
    if matches!(cmd, BackupCmd::Load { confirm: true, .. })
        && backup::locked_or_panic(app, gid).await?
    {
        return Ok(no("reply.backups.locked_load").await);
    }
    // The newest backup is anti-nuke's restore source (owner decision 11-16): owner level, no 2FA.
    if matches!(cmd, BackupCmd::Create)
        && !matches!(actor, permits::Level::Owner | permits::Level::ExtraOwner)
    {
        return Ok(no("reply.backups.owners_create").await);
    }
    let text: String = match cmd {
        BackupCmd::Create => match backup::save(app, gid, Some(actor_id.get())).await {
            Ok(code) => format!("Saved backup `{code}`"),
            Err(e) => e.to_string(),
        },
        BackupCmd::List => {
            let rows = backup::list(app, gid).await?;
            if rows.is_empty() {
                "No backups yet".into()
            } else {
                rows.iter()
                    .map(|(code, at, r, c, by)| {
                        let by = by.map_or("Brim".into(), |u| format!("<@{u}>"));
                        format!("`{code}` {by} <t:{at}:R>: {r} roles, {c} channels")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        BackupCmd::Info { id } => backup::info(app, gid, &id).await?,
        BackupCmd::Load { id, confirm } => {
            backup::load(app, gid, &id, confirm, actor_id.get()).await?
        }
        BackupCmd::Delete { id } => match backup::delete(app, gid, Some(&id)).await? {
            true => format!("Deleted backup `{id}`"),
            false => format!("No backup `{id}`"),
        },
        BackupCmd::Clear => {
            backup::delete(app, gid, None).await?;
            "Deleted all backups".into()
        }
    };
    Ok(text.into())
}

/// `/revert`: undo what `target` did in the last `since_s` seconds.
pub async fn revert_cmd(
    app: &App,
    gid: Id<GuildMarker>,
    (actor_id, actor): (Id<UserMarker>, &permits::Level),
    target: Id<UserMarker>,
    since_s: i64,
    owners: bool,
) -> Result<reply::Said> {
    if !actor.has("revert") {
        return Ok(String::from("Cannot revert: you lack the revert permit").into());
    }
    // Owner, Extra Owner and TA actions are undone only when an Owner or Extra Owner asks for it.
    // So are Brim's own (automatic bans; its quarantines are never lifted here).
    let owner = app.guild_info(gid).await?.owner;
    if target == app.me || app.level(gid, owner, target).await?.immune() {
        let may = matches!(actor, permits::Level::Owner | permits::Level::ExtraOwner);
        // Security texts: never framed by `cmd.revert` (see `reply::Said::security`).
        let who = [("target", format!("<@{target}>"))];
        let who: Vec<(&str, &str)> = who.iter().map(|(k, v)| (*k, v.as_str())).collect();
        if !may {
            return Ok(reply::say(app, gid, "reply.revert.owners_only", &who).await);
        }
        if !owners {
            return Ok(reply::say(app, gid, "reply.revert.include_owners", &who).await);
        }
    }
    if let Some(msg) = gated(app, gid, actor_id, actor, "revert").await? {
        return Ok(msg.into());
    }
    let r = antinuke::revert_actor(app, gid, target, now_secs() - since_s, None).await?;
    let reinvite = antinuke::capped(r.reinvite.iter().map(|u| format!("<@{u}>")));
    Ok(format!(
        "Reverted {}/{} actions by <@{target}> and recreated {} deleted. Re-invite: {}",
        r.ops - r.failed.len(),
        r.ops,
        r.recreated,
        if reinvite.is_empty() {
            "none".into()
        } else {
            reinvite.join(" ")
        }
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::{above_bot, timeout_chunk, Flags, Id, ModKind, MAX_TIMEOUT_S};
    use crate::confirm::Action;
    use crate::modcmds::ModCmd;

    #[test]
    fn a_permit_without_limits_takes_the_anti_nuke_limit() {
        let on =
            |v: serde_json::Value| -> super::AntinukeCfg { serde_json::from_value(v).unwrap() };
        let an = on(
            serde_json::json!({"enabled": true, "limits": {"Ban": {"per_minute": 3, "per_hour": 9}}}),
        );
        let l = super::nuke_fallback(&an, "kick").unwrap();
        assert_eq!((l.per_min, l.per_hour), (2, 8), "kicks share the ban card");
        assert!(super::nuke_fallback(&an, "purge").is_none());
        assert_eq!(
            super::limit_key(false, "kick"),
            super::limit_key(false, "ban")
        );
        assert_eq!(
            super::limit_key(true, "kick"),
            "kick",
            "a permit's own limit"
        );
        assert!(
            super::nuke_fallback(&an, "timeout").is_none(),
            "opt-in card not set"
        );
        assert!(
            super::nuke_fallback(&on(serde_json::json!({})), "ban").is_none(),
            "anti-nuke off"
        );
    }

    #[test]
    fn pending_action_survives_the_database_round_trip() {
        let flags = Flags {
            dm: true,
            soft: true,
            compact: false,
            ..Default::default()
        };
        let a = Action::Mod {
            kind: ModKind::Ban,
            targets: vec![Id::new(42)],
            reason: Some("spam".into()),
            duration_s: Some(60),
            flags,
            proof: Some("https://x/y.png".into()),
            unsure: true,
            reply: None,
        };
        let Action::Mod {
            kind,
            targets,
            proof,
            ..
        } = serde_json::from_value(serde_json::to_value(&a).unwrap()).unwrap()
        else {
            panic!("variant changed")
        };
        assert_eq!((kind, targets), (ModKind::Ban, vec![Id::new(42)]));
        assert_eq!(proof.as_deref(), Some("https://x/y.png"));
        let purge = ModCmd::Purge {
            count: 5,
            user: None,
            from: Some(crate::modcmds::Anchor::Until(9)),
            filters: vec![],
            own: vec![],
            before: None,
        };
        let e = Action::Ext {
            cmd: purge.clone(),
            channel: Id::new(7),
            reason: None,
            proof: None,
            users: vec![5],
            unsure: true,
            dm: false,
            reply: None,
        };
        let Action::Ext { cmd, channel, .. } =
            serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap()
        else {
            panic!("variant changed")
        };
        assert_eq!((cmd, channel), (purge, Id::new(7)));
    }

    #[test]
    fn timeout_chunking() {
        assert_eq!(timeout_chunk(600), (600, false));
        assert_eq!(timeout_chunk(MAX_TIMEOUT_S), (MAX_TIMEOUT_S, false));
        assert_eq!(timeout_chunk(MAX_TIMEOUT_S + 1), (MAX_TIMEOUT_S, true));
    }

    #[test]
    fn hierarchy() {
        assert!(above_bot(Some(5), 5, false)); // equal is refused
        assert!(above_bot(Some(9), 5, false));
        assert!(!above_bot(Some(4), 5, false));
        assert!(!above_bot(None, 5, false)); // not in the server
        assert!(above_bot(None, 5, true)); // owner
    }
}

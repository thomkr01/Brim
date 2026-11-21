//! Audit-log driven anti-nuke: count watched actions per executor, quarantine and revert on a breach,
//! and enforce the quarantine hold.
use anyhow::{Context, Result};
use brim_antinuke::{
    gained_dangerous, has_dangerous, hold_violation, nuke_plan, plan_revert, Action, Inverse,
    LogEntry, NukeCase, NukeStep, Verdict,
};
use brim_automod::heat::{Breakdown, HeatCfg, HEAT_DECAY};
use brim_core::{
    cases::{self, NewCase},
    jobs, permits,
    sections::BackupsCfg,
};
use brim_logging::{
    embed::{user as mention, ALERT, NO, YES},
    overlay::security_log,
    Log, LogKind,
};
use serde_json::json;
use sqlx::{
    types::chrono::{DateTime, Utc},
    PgPool,
};
use std::{
    collections::HashMap,
    future::Future,
    time::{Duration, Instant},
};
use twilight_http::request::{AuditLogReason, Method, RequestBuilder};
use twilight_model::{
    gateway::payload::incoming::MemberUpdate,
    guild::{
        audit_log::{AuditLogChange, AuditLogEntry, AuditLogEventType as A},
        Permissions,
    },
    http::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
    id::{
        marker::{GuildMarker, RoleMarker, UserMarker},
        Id,
    },
};

use crate::{
    app::{now_secs, App, GuildCfg},
    backup,
    events::send_log,
};

fn watched(e: &AuditLogEntry) -> Option<Action> {
    let changed = |f: fn(&AuditLogChange) -> bool| e.changes.iter().any(f);
    Some(match e.action_type {
        A::ChannelCreate => Action::ChannelCreate,
        A::ChannelDelete => Action::ChannelDelete,
        A::RoleCreate => Action::RoleCreate,
        A::RoleDelete => Action::RoleDelete,
        A::MemberBanAdd => Action::Ban,
        A::MemberKick => Action::Kick,
        A::WebhookCreate => Action::WebhookCreate,
        A::WebhookDelete => Action::WebhookDelete,
        A::MemberPrune => Action::Prune,
        A::BotAdd => Action::BotAdd,
        A::MemberUpdate
            if changed(|c| {
                matches!(
                    c,
                    AuditLogChange::CommunicationDisabledUntil { new: Some(_), .. }
                )
            }) =>
        {
            Action::Timeout
        }
        A::MemberRoleUpdate if changed(|c| matches!(c, AuditLogChange::RoleRemoved { .. })) => {
            Action::RoleRemove
        }
        A::ChannelUpdate => Action::ChannelUpdate,
        A::ThreadDelete => Action::ThreadDelete,
        A::RoleUpdate if changed(|c| matches!(c, AuditLogChange::Name { .. })) => {
            Action::RoleUpdate
        }
        A::EmojiDelete => Action::EmojiDelete,
        A::StickerDelete => Action::StickerDelete,
        A::AutoModerationRuleDelete => Action::AutoModRuleDelete,
        A::AutoModerationRuleUpdate => Action::AutoModRuleUpdate,
        A::MemberBanRemove => Action::Unban,
        A::IntegrationCreate => Action::IntegrationCreate,
        A::IntegrationDelete => Action::IntegrationDelete,
        A::EmojiCreate => Action::EmojiCreate,
        A::StickerCreate => Action::StickerCreate,
        A::GuildScheduledEventCreate => Action::ScheduledEventCreate,
        A::GuildUpdate
            if changed(|c| {
                matches!(
                    c,
                    AuditLogChange::Name { .. } | AuditLogChange::IconHash { .. }
                )
            }) =>
        {
            Action::GuildUpdate
        }
        // SOUNDBOARD_SOUND_DELETE: twilight 0.18 has no variant. Matched by number, so a later
        // twilight that names it keeps matching.
        t if u16::from(t) == 132 => Action::SoundboardDelete,
        _ => return None,
    })
}

/// What undoes the entry. A server update that changed both name and icon gives two.
fn inverse(action: Action, e: &AuditLogEntry) -> Vec<Inverse> {
    if action == Action::GuildUpdate {
        return e
            .changes
            .iter()
            .filter_map(|c| match c {
                AuditLogChange::Name {
                    old: Some(name),
                    new,
                } => Some(Inverse::GuildName {
                    name: name.clone(),
                    from: new.clone(),
                }),
                AuditLogChange::IconHash { old, new } => Some(Inverse::GuildIcon {
                    hash: old.map(|h| h.to_string()),
                    from: Some(new.map(|h| h.to_string())),
                }),
                _ => None,
            })
            .collect();
    }
    let Some(id) = e.target_id.map(|t| t.get()) else {
        return vec![];
    };
    if action == Action::RoleRemove {
        let roles: Vec<u64> = e
            .changes
            .iter()
            .filter_map(|c| match c {
                AuditLogChange::RoleRemoved { new, .. } => Some(new),
                _ => None,
            })
            .flatten()
            .map(|r| r.id.get())
            .collect();
        return vec![Inverse::AddRoles { user: id, roles }];
    }
    vec![match action {
        Action::ChannelCreate => Inverse::DeleteChannel { id },
        Action::RoleCreate => Inverse::DeleteRole { id },
        Action::WebhookCreate => Inverse::DeleteWebhook { id },
        Action::Ban => Inverse::Unban { user: id },
        Action::Kick => Inverse::Reinvite { user: id },
        Action::IntegrationCreate => Inverse::DeleteIntegration { id },
        Action::EmojiCreate => Inverse::DeleteEmoji { id },
        Action::StickerCreate => Inverse::DeleteSticker { id },
        Action::ScheduledEventCreate => Inverse::DeleteScheduledEvent { id },
        _ => return vec![],
    }]
}

/// A live audit entry: counted now, whatever the host clock says of its snowflake.
pub async fn on_audit(app: &App, e: &AuditLogEntry) -> Result<()> {
    on_audit_as(app, e, false).await
}

/// An entry the catch-up read after a gap: counted at its own time.
pub(crate) async fn on_replayed(app: &App, e: &AuditLogEntry) -> Result<()> {
    on_audit_as(app, e, true).await
}

/// A replayed entry anti-nuke watches that is past the hour window: logged, not counted.
pub(crate) fn too_old(e: &AuditLogEntry) -> bool {
    watched(e).is_some() && replay_age(e.id.get(), true) >= HOUR
}

const HOUR: Duration = Duration::from_secs(3600);

async fn on_audit_as(app: &App, e: &AuditLogEntry, replayed: bool) -> Result<()> {
    let (Some(gid), Some(actor)) = (e.guild_id, e.user_id) else {
        return Ok(());
    };
    if actor == app.me {
        return Ok(());
    }
    // A read error must not skip the guards and the count: the entry is marked handled after.
    let (cfg, cfg_err) = match app.cfg_or_last(gid.get()).await {
        Ok(c) => (c, None),
        Err((c, e)) => (c, Some(e)),
    };
    // Owner decision 2026-11-13 22:49 and 23:35: with anti-nuke off the guards below still undo what
    // only permits may do (Discord rights give no power), but nobody is punished or counted.
    let punish = cfg.antinuke.enabled;
    // Each check runs whatever an earlier one returned: an error in one (a REST or DB hiccup)
    // must not skip the lock set-back or the count, which would work as "exempt" (D79).
    // The first error is returned at the end.
    let mut failed = Ok(());
    let mut keep = |r: Result<()>| {
        if let (Err(e), true) = (r, failed.is_ok()) {
            failed = Err(e);
        }
    };
    if matches!(
        e.action_type,
        A::MemberRoleUpdate
            | A::RoleUpdate
            | A::ChannelOverwriteCreate
            | A::ChannelOverwriteUpdate
            | A::GuildUpdate
    ) {
        // Runs with Quarantine Hold off too: it then only keeps unverified joiners held.
        if e.action_type == A::MemberRoleUpdate {
            keep(hold(app, e, actor, punish).await);
        }
        // A role rename still counts below.
        keep(guard_perms(app, &cfg, e, actor, punish).await);
        if e.action_type == A::MemberRoleUpdate {
            keep(guard_permit_roles(app, e, actor, punish).await);
            keep(guard_permit_removal(app, e, actor).await);
        }
    }
    if matches!(
        e.action_type,
        A::ChannelOverwriteCreate | A::ChannelOverwriteUpdate | A::ChannelOverwriteDelete
    ) {
        keep(hold_overwrite(app, e, actor, punish).await);
    }
    if matches!(
        e.action_type,
        A::RoleUpdate | A::ChannelOverwriteUpdate | A::ChannelOverwriteDelete
    ) {
        keep(guard_lock(app, &cfg, e, actor, punish).await);
    }
    match e.action_type {
        A::MemberUpdate | A::MemberKick | A::MemberBanAdd if cfg.antinuke.quarantine_hold => {
            keep(hold(app, e, actor, punish).await)
        }
        A::RoleCreate | A::BotAdd => keep(guard_perms(app, &cfg, e, actor, punish).await),
        _ => {}
    }
    // A replay past 5 minutes: the bot may have been added again since by someone who may.
    if e.action_type == A::BotAdd && replay_age(e.id.get(), replayed) < Duration::from_secs(300) {
        keep(guard_app_add(app, e, actor).await);
    }
    // A replayed entry past 5 minutes: the victim may have rejoined or been unbanned since.
    if (matches!(e.action_type, A::MemberKick | A::MemberBanAdd)
        || matches!(watched(e), Some(Action::Timeout)))
        && replay_age(e.id.get(), replayed) < Duration::from_secs(300)
    {
        keep(guard_rank(app, &cfg, e, actor, punish).await);
    }
    if let Some(err) = cfg_err {
        // The defaults have anti-nuke off: the action is saved uncounted, so `/revert` and the
        // Revert Center can still undo it.
        log_uncounted(app, gid, actor, e, replayed).await;
        return failed.and(Err(err.context("the settings could not be read")));
    }
    if !punish {
        return failed;
    }
    let counted = count_audit(app, &cfg, gid, actor, e, replayed).await;
    counted.and(failed)
}

/// A watched action seen while the server's settings could not be read: saved with `counted`
/// 0 (nobody is punished on unknown settings), and staff told at most once in 10 minutes.
async fn log_uncounted(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    e: &AuditLogEntry,
    replayed: bool,
) {
    let Some(action) = watched(e) else {
        return;
    };
    let ago = replay_age(e.id.get(), replayed);
    let target = e.target_id.map(|t| t.get());
    let inv = inverse(action, e);
    save_actions(
        app,
        gid,
        actor,
        action,
        target,
        &inv,
        (0, ago),
        Some(e.id.get()),
    )
    .await;
    if !first_in(("cfg_unread", gid.get(), 0), Some(Duration::from_secs(600))) {
        return;
    }
    let alert = format!("{NO} **This server's settings could not be read**");
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.settings_unread",
        &[("alert", &alert)],
    );
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "settings failure log failed");
    }
}

/// The counting half of `on_audit`: voice, then the watched cards.
async fn count_audit(
    app: &App,
    cfg: &GuildCfg,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    e: &AuditLogEntry,
    replayed: bool,
) -> Result<()> {
    if matches!(e.action_type, A::MemberDisconnect | A::MemberMove) {
        return crate::voice_nuke::note(app, cfg, gid, e, replayed).await;
    }
    let Some(action) = watched(e) else {
        return Ok(());
    };
    if cfg.antinuke.limit(action).is_none() {
        return Ok(());
    }
    // An entry read after downtime counts at its own time; past the hour window it cannot trip,
    // but is still logged (counted 0) for /revert and the Revert Center.
    let ago = replay_age(e.id.get(), replayed);
    // Ticket bots open and close channels all day; the gateway event names the category first.
    // Only a bot is exempt, and not for a channel just moved in: a nuker moves channels into the
    // category and deletes them. Panic's Whitelisted Categories are panic-only.
    let tickets = &cfg.antinuke.ticket_categories;
    if let (Action::ChannelCreate | Action::ChannelDelete, Some(t)) = (action, e.target_id) {
        if tickets.is_empty() {
            app.channel_parents.lock().unwrap().remove(&t.get());
        } else if (if ago.is_zero() {
            parent_of(app, t.get()).await
        } else {
            replayed_parent(app, gid, t.get()).await
        })
        .is_some_and(|p| tickets.iter().any(|c| c.get() == p))
            && !app.moved_in(t.get(), tickets)
            && is_bot(app, actor).await
        {
            return Ok(());
        }
    }
    // A webhook on the webhook whitelist is exempt whatever its creator may do. A failed lookup
    // counts the action, so a Discord error cannot hide a nuke.
    let listed = app.whitelisted(gid, actor, action.whitelist_flag()).await;
    let exempt = listed.unwrap_or_else(|e| {
        tracing::warn!(?e, "whitelist lookup failed, counting the action");
        false
    }) || (action == Action::WebhookCreate
        && e.target_id.is_some_and(|t| {
            cfg.whitelist
                .spam
                .webhooks
                .iter()
                .any(|w| w.get() == t.get())
        }));
    let inv = inverse(action, e);
    if let (true, Some(t), Action::ChannelCreate | Action::RoleCreate) =
        (exempt, e.target_id, action)
    {
        backup::track_created(app, gid, action, t.get()).await?;
    }
    count_at(
        app,
        cfg,
        gid,
        actor,
        action,
        e.target_id.map(|t| t.get()),
        &inv,
        exempt,
        1,
        ago,
        Some(e.id.get()),
    )
    .await
}

/// The `action_log` rows of one action: one per inverse (a name and icon change are two),
/// `counted` on the first only, which puts the window back after a restart (`seed_windows`).
#[allow(clippy::too_many_arguments)]
async fn save_actions(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    action: Action,
    target: Option<u64>,
    inv: &[Inverse],
    (counted, ago): (i32, Duration),
    audit: Option<u64>,
) {
    let rows: Vec<Option<&Inverse>> = if inv.is_empty() {
        vec![None]
    } else {
        inv.iter().map(Some).collect()
    };
    for (k, i) in rows.into_iter().enumerate() {
        let logged = sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse, counted, created_at, audit_id) VALUES ($1,$2,$3,$4,$5,$6, now() - make_interval(secs => $7), $8)")
            .bind(gid.get() as i64)
            .bind(actor.get() as i64)
            .bind(format!("{action:?}"))
            .bind(target.map(|t| t as i64))
            .bind(i.map(|i| json!(i)))
            .bind(if k == 0 { counted } else { 0 })
            .bind(ago.as_secs_f64())
            .bind(audit.map(|a| a as i64))
            .execute(&app.pool)
            .await;
        // A slow database must not let the action go uncounted: only its revert row (and its
        // count after a restart) is lost.
        if let Err(e) = logged {
            tracing::warn!(?e, %gid, %actor, ?action, "saving an anti-nuke action failed");
        }
    }
}

/// How long ago a replayed audit entry was made, by its snowflake. A live one is zero:
/// a host clock that runs fast must never make live entries look old.
pub(crate) fn replay_age(id: u64, replayed: bool) -> Duration {
    let ms = crate::automod::now_ms() - crate::voice_nuke::created_ms(id);
    if replayed {
        Duration::from_millis(ms.max(0) as u64)
    } else {
        Duration::ZERO
    }
}

/// The category of a channel made or deleted while the bot was down: no gateway event noted it.
/// A channel that still exists says so itself; a deleted one is looked up in the latest backup.
/// Not found (a ticket made and closed in the gap) counts the action.
async fn replayed_parent(app: &App, gid: Id<GuildMarker>, channel: u64) -> Option<u64> {
    if let Ok(r) = app.http.channel(Id::new(channel)).await {
        if let Ok(c) = r.model().await {
            return c.parent_id.map(|p| p.get());
        }
    }
    let image = backup::latest(app, gid).await.ok().flatten()?;
    image.channels.into_iter().find(|c| c.id == channel)?.parent
}

/// Logs, heats and counts `n` actions of one audit entry, and contains the actor at the limit.
/// The voice path counts how much a merged entry grew (`voice_nuke`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn count(
    app: &App,
    cfg: &GuildCfg,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    action: Action,
    target: Option<u64>,
    inv: &[Inverse],
    exempt: bool,
    n: u32,
) -> Result<()> {
    count_at(
        app,
        cfg,
        gid,
        actor,
        action,
        target,
        inv,
        exempt,
        n,
        Duration::ZERO,
        None,
    )
    .await
}

/// As `count`, for actions made `ago` (an audit entry read after downtime): logged and
/// windowed at their own time, and no heat, which would have decayed by now.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn count_at(
    app: &App,
    cfg: &GuildCfg,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    action: Action,
    target: Option<u64>,
    inv: &[Inverse],
    exempt: bool,
    n: u32,
    ago: Duration,
    audit: Option<u64>,
) -> Result<()> {
    let Some((_, heat)) = cfg.antinuke.limit(action) else {
        return Ok(());
    };
    let mut hot = false;
    if !exempt {
        let counted = if ago < HOUR { n as i32 } else { 0 };
        save_actions(app, gid, actor, action, target, inv, (counted, ago), audit).await;
        hot = heat > 0
            && ago.is_zero()
            && cfg.panic.heat_algorithm
            && add_heat(
                app,
                gid,
                actor,
                heat.saturating_mul(n),
                cfg.panic.heat_decay,
            );
        // A failed whitelist lookup counts the action, but an immune actor must not start
        // heat panic; an unread level still counts.
        if hot && matches!(app.is_immune(gid, actor).await, Ok(true)) {
            hot = false;
        }
        if hot {
            crate::panic::note_heat(app, gid, actor).await;
        }
    }
    let verdict = {
        let mut an = app.antinuke.lock().unwrap();
        an.limits = cfg.antinuke.watched();
        an.on_actions_at(
            gid.get(),
            actor.get(),
            action,
            exempt,
            Instant::now(),
            ago,
            n,
        )
    };
    if let Some(why) = breach(&verdict, hot, action) {
        quarantine_and_revert(app, gid, actor, &why).await?;
    }
    Ok(())
}

/// Why the actor is contained: a reached limit, or 100% anti-nuke heat spread over several
/// cards that each stayed under their limit.
fn breach(verdict: &Verdict, hot: bool, action: Action) -> Option<String> {
    match verdict {
        Verdict::Quarantine(c) => Some(format!(
            "{action:?} limit reached ({}/min, {}/hour){}",
            c.minute,
            c.hour,
            if action.cannot_undo() {
                "; cannot be undone in Discord"
            } else {
                ""
            }
        )),
        _ if hot => Some("anti-nuke heat reached 100%".into()),
        _ => None,
    }
}

/// Startup: the anti-nuke windows, permit command limits and heat panic from before a restart
/// (N3), read from `action_log`, `command_uses` and `heat_panic`. Runs before the gateway.
pub(crate) async fn seed_windows(app: &App) -> Result<()> {
    type Row = (i64, i64, String, i32, f64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT guild_id, actor_id, action, counted, EXTRACT(EPOCH FROM now() - created_at)::float8
         FROM action_log WHERE created_at > now() - interval '1 hour' AND counted > 0
         ORDER BY created_at, id",
    )
    .fetch_all(&app.pool)
    .await?;
    let now = Instant::now();
    {
        let mut an = app.antinuke.lock().unwrap();
        // Only rows `count` wrote name an action; cases and the bot's own rows do not parse.
        for (g, a, action, n, ago) in rows {
            if let Ok(action) = serde_json::from_value::<Action>(json!(action)) {
                let ago = Duration::from_secs_f64(ago.max(0.0));
                an.seed((g as u64, a as u64, action), ago, n as u32, now);
            }
        }
    }
    // Rows from before 4005 have no guild: skipped, as the server they count in is unknown.
    let uses: Vec<(i64, i64, String, f64)> = sqlx::query_as(
        "SELECT guild_id, actor_id, action, EXTRACT(EPOCH FROM now() - at)::float8 FROM command_uses
         WHERE at > now() - interval '1 hour' AND guild_id IS NOT NULL ORDER BY at",
    )
    .fetch_all(&app.pool)
    .await?;
    {
        let mut rl = app.rate_limiter.lock().unwrap();
        for (guild, actor, action, ago) in uses {
            if let Some(at) = now.checked_sub(Duration::from_secs_f64(ago.max(0.0))) {
                rl.seed(guild, actor, &action, at);
            }
        }
    }
    let heat: Vec<(i64, Vec<i64>, i64)> =
        sqlx::query_as("SELECT guild_id, raiders, until FROM heat_panic WHERE until > $1")
            .bind(now_secs())
            .fetch_all(&app.pool)
            .await?;
    let mut m = app.heat_raiders.lock().unwrap();
    for (g, raiders, until) in heat {
        m.insert(
            g as u64,
            (raiders.into_iter().map(|r| r as u64).collect(), until),
        );
    }
    Ok(())
}

/// The category of a created or deleted channel, as its gateway event noted it. Both events run
/// as their own tasks, so the audit entry can come first: a miss waits up to 0.5 s for the other.
async fn parent_of(app: &App, channel: u64) -> Option<u64> {
    for _ in 0..5 {
        if let Some(p) = app.channel_parents.lock().unwrap().remove(&channel) {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// Adds a staff action's heat %; true when the member just reached 100.
fn add_heat(app: &App, gid: Id<GuildMarker>, actor: Id<UserMarker>, heat: u32, decay: f64) -> bool {
    let cfg = HeatCfg {
        decay_per_sec: decay.clamp(HEAT_DECAY.0, HEAT_DECAY.1),
        ..Default::default()
    };
    let b = Breakdown {
        parts: vec![("nuke", f64::from(heat))],
    };
    let mut map = app.nuke_heat.lock().unwrap();
    let now = now_secs() * 1000;
    if map.len() > 10_000 {
        map.retain(|_, s| s.decayed(now, &cfg) >= 1.0); // under 1% heat is spent
        if map.len() > 10_000 {
            crate::caps::drop_biggest(&mut map, |k| k.0);
        }
    }
    let s = map.entry((gid.get(), actor.get())).or_default();
    let hot = s.apply(&b, now, &cfg);
    if hot {
        s.value = 0.0;
    }
    hot
}

/// How long the BotAdd guard waits before reading a bot's own role a second time.
#[cfg(not(test))]
const BOT_ROLE_RETRY: Duration = Duration::from_secs(3);
#[cfg(test)]
const BOT_ROLE_RETRY: Duration = Duration::from_millis(10);

/// The dangerous grant guard (roles, members, bots, channel overwrites) and the vanity guard.
/// Undoes the change where the bot can, then quarantines the actor if `punish`.
async fn guard_perms(
    app: &App,
    cfg: &GuildCfg,
    e: &AuditLogEntry,
    actor: Id<UserMarker>,
    punish: bool,
) -> Result<()> {
    let (Some(gid), Some(target)) = (e.guild_id, e.target_id) else {
        return Ok(());
    };
    let an = &cfg.antinuke;
    // Owner 2026-11-15: dangerous permissions are given only by an Owner, Extra Owner or Trusted
    // Admin, whatever the toggles and whitelist flags say; never by a bot.
    let grant = matches!(
        e.action_type,
        A::RoleCreate
            | A::RoleUpdate
            | A::MemberRoleUpdate
            | A::BotAdd
            | A::ChannelOverwriteCreate
            | A::ChannelOverwriteUpdate
    );
    if !(grant || an.vanity_guard) {
        return Ok(());
    }
    // The quarantine role itself: power given to it reaches every quarantined member, staff too,
    // so only an Owner or Extra Owner gives it any (owner decision 2026-11-18). A Trusted Admin's change
    // is undone, never punished.
    let q_role = an.quarantine_role.is_some_and(|q| match e.action_type {
        A::RoleUpdate => target.cast() == q,
        A::ChannelOverwriteCreate | A::ChannelOverwriteUpdate => e
            .options
            .as_ref()
            .is_some_and(|o| o.kind.as_deref() == Some("0") && o.id.is_some_and(|i| i.cast() == q)),
        _ => false,
    });
    let trusted = std::sync::atomic::AtomicBool::new(false);
    // Asked only once the entry is a guarded change. None: exempt; Some(true): unread, so the
    // change is undone but nobody punished (owner decision).
    let spared = || async {
        let exempt = if q_role {
            match app.guild_info(gid).await {
                Ok(info) => app.level(gid, info.owner, actor).await.map(|l| match l {
                    permits::Level::Owner | permits::Level::ExtraOwner => true,
                    permits::Level::TrustedAdmin => {
                        trusted.store(true, std::sync::atomic::Ordering::Relaxed);
                        false
                    }
                    _ => false,
                }),
                Err(e) => Err(e),
            }
        } else if grant {
            app.is_immune(gid, actor).await
        } else {
            app.whitelisted(gid, actor, "antinuke").await
        };
        match exempt {
            Ok(true) => None,
            Ok(false) => Some(trusted.load(std::sync::atomic::Ordering::Relaxed)),
            Err(e) => {
                tracing::warn!(
                    ?e,
                    "anti-nuke exemption lookup failed, undoing without punishment"
                );
                Some(true)
            }
        }
    };
    let gained = |old: Option<Permissions>, new: Option<Permissions>| {
        let bits = |p: Option<Permissions>| p.map_or(0, |p| p.bits());
        gained_dangerous(bits(old), bits(new)).then(|| old.unwrap_or(Permissions::empty()))
    };
    // The undo; a failed one is logged and the actor is still quarantined.
    let mut undo: Result<()> = Ok(());
    // A later change already replaced the entry's value: setting back would undo that one too.
    let mut skipped = false;
    let mut unsure;
    let mut keep_bits = None;
    let why = match e.action_type {
        A::RoleCreate => {
            if !e.changes.iter().any(|c| matches!(c, AuditLogChange::Permissions { new, .. } if gained(None, *new).is_some())) {
                return Ok(());
            }
            unsure = match spared().await {
                None => return Ok(()),
                Some(u) => u,
            };
            // A bot's own role is judged on its BotAdd entry, by who added the bot: this entry
            // may name the bot. Unread, the role is emptied but nobody punished.
            match async { anyhow::Ok(app.http.roles(gid).await?.models().await?) }.await {
                Ok(roles) => {
                    let bot_own = |r: &twilight_model::guild::Role| {
                        r.id == target.cast() && r.tags.as_ref().is_some_and(|t| t.bot_id.is_some())
                    };
                    if roles.iter().any(bot_own) {
                        return Ok(());
                    }
                    // Only the dangerous bits it still holds go: a catch-up has no age limit, and a
                    // later change by someone else must stay.
                    // Deleted since: nothing to undo, and nothing "changed again" to log.
                    let Some(now) = roles
                        .iter()
                        .find(|r| r.id == target.cast())
                        .map(|r| r.permissions.bits())
                    else {
                        return Ok(());
                    };
                    keep_bits = Some(now & !brim_antinuke::DANGEROUS_PERMS);
                    skipped = !has_dangerous(now);
                }
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "reading the new role failed, emptying it without punishment"
                    );
                    unsure = true;
                }
            }
            if !skipped {
                // Unread, every permission goes.
                let rest = Permissions::from_bits_retain(keep_bits.unwrap_or(0));
                undo = app
                    .http
                    .update_role(gid, target.cast())
                    .permissions(rest)
                    .await
                    .map(drop)
                    .map_err(Into::into);
            }
            "created a role with dangerous permissions"
        }
        A::RoleUpdate => {
            let Some((old, new)) = e.changes.iter().find_map(|c| match c {
                AuditLogChange::Permissions { old, new } => gained(*old, *new).map(|o| (o, *new)),
                _ => None,
            }) else {
                return Ok(());
            };
            // Mention @everyone alone on a staff permit role (not a main role) is allowed, as on
            // the security score. Unread staff roles count as none.
            let added = new.map_or(0, |n| n.bits()) & !old.bits();
            if added & brim_antinuke::STAFF_DANGEROUS == 0 {
                let staff = brim_core::permits::all_staff_roles(&app.pool, gid.get() as i64).await;
                let main = app
                    .cfg(gid.get())
                    .await
                    .map(|c| c.lockdown.main_roles.clone());
                if staff.is_ok_and(|s| s.contains(&(target.get() as i64)))
                    && main.is_ok_and(|m| !m.contains(&target.cast()))
                {
                    return Ok(());
                }
            }
            unsure = match spared().await {
                None => return Ok(()),
                Some(u) => u,
            };
            undo = async {
                let roles = app.http.roles(gid).await?.models().await?;
                let now = roles
                    .iter()
                    .find(|r| r.id == target.cast())
                    .map(|r| r.permissions);
                if now != new {
                    skipped = true;
                    return Ok(());
                }
                app.http
                    .update_role(gid, target.cast())
                    .permissions(old)
                    .await?;
                Ok(())
            }
            .await;
            "gave a role dangerous permissions"
        }
        A::MemberRoleUpdate => {
            let given: Vec<Id<RoleMarker>> = e
                .changes
                .iter()
                .filter_map(|c| match c {
                    AuditLogChange::RoleAdded { new, .. } => Some(new),
                    _ => None,
                })
                .flatten()
                .map(|r| r.id)
                .collect();
            if given.is_empty() {
                return Ok(());
            }
            // Only Discord gives a managed role, and it cannot be taken off (a bot's own role is
            // judged on its BotAdd entry). Fails closed: unread, every given role is taken back.
            let (added, unread) = match dangerous_given(app, gid, &given).await {
                Ok(added) => (added, false),
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "reading the given roles failed, taking them back without punishment"
                    );
                    (given, true)
                }
            };
            if added.is_empty() {
                return Ok(());
            }
            unsure = match spared().await {
                None => return Ok(()),
                Some(u) => u || unread,
            };
            // Each role on its own: unread, the list may hold a managed role Discord refuses, and
            // that must not keep the dangerous ones after it.
            for r in added {
                if let Err(err) = app
                    .http
                    .remove_guild_member_role(gid, target.cast(), r)
                    .await
                {
                    undo = Err(err.into());
                }
            }
            "gave a member a role with dangerous permissions"
        }
        A::BotAdd => {
            // The bot's own role, made with the bot; its RoleCreate entry is skipped above.
            // Unread, staff are told to check the bot: an error must not pass as "no dangerous role".
            // Read again once after a while: the role may not be listed yet when the entry comes.
            let mut found = bot_role(app, gid, target.cast()).await;
            if !matches!(found, Ok(Some(_))) {
                tokio::time::sleep(BOT_ROLE_RETRY).await;
                found = bot_role(app, gid, target.cast()).await;
            }
            let found = match found {
                Ok(Some(r)) => r,
                // Still not listed after the second read: unchecked, as for an error.
                res => {
                    tracing::warn!(?res, "reading a new bot's role failed");
                    if spared().await.is_none() {
                        return Ok(());
                    }
                    let action =
                        "not checked: the bot's role could not be read. Check its permissions";
                    return undone_log(app, gid, actor, Some(target.cast()), "added a bot", action)
                        .await;
                }
            };
            let (role, perms) = found;
            if !has_dangerous(perms) {
                return Ok(());
            }
            unsure = match spared().await {
                None => return Ok(()),
                Some(u) => u,
            };
            undo = app
                .http
                .update_role(gid, role)
                .permissions(Permissions::empty())
                .reason("Anti-nuke: only an Owner, Extra Owner or Trusted Admin adds a bot with dangerous permissions")
                .await
                .map(drop)
                .map_err(Into::into);
            "added a bot with dangerous permissions"
        }
        A::ChannelOverwriteCreate | A::ChannelOverwriteUpdate => {
            // Any role or member (owner 2026-11-15); "1" is a member overwrite, "0" a role one.
            let Some((id, kind)) = e.options.as_ref().and_then(|o| match o.kind.as_deref() {
                Some("0") => Some((o.id?, PermissionOverwriteType::Role)),
                Some("1") => Some((o.id?, PermissionOverwriteType::Member)),
                _ => None,
            }) else {
                return Ok(());
            };
            let Some((old, new)) = e.changes.iter().find_map(|c| match c {
                AuditLogChange::Allow { old, new } => gained(*old, *new).map(|o| (o, *new)),
                _ => None,
            }) else {
                return Ok(());
            };
            unsure = match spared().await {
                None => return Ok(()),
                Some(u) => u,
            };
            let channel = target.cast();
            undo = async {
                let now = app
                    .http
                    .channel(channel)
                    .await?
                    .model()
                    .await?
                    .permission_overwrites
                    .unwrap_or_default()
                    .into_iter()
                    .find(|o| {
                        use twilight_model::channel::permission_overwrite::PermissionOverwriteType as K;
                        let member = matches!(kind, PermissionOverwriteType::Member);
                        o.id == id && (o.kind == K::Member) == member
                    });
                if now.as_ref().map(|o| o.allow) != new {
                    skipped = true;
                    return Ok(());
                }
                if e.action_type == A::ChannelOverwriteCreate {
                    let del = app.http.delete_channel_permission(channel);
                    match kind {
                        PermissionOverwriteType::Member => del.member(id.cast()).await?,
                        _ => del.role(id.cast()).await?,
                    };
                    return Ok(());
                }
                // Keep the overwrite's current denies; only the allow change is undone.
                let deny = now.map(|o| o.deny);
                let o = PermissionOverwrite {
                    id,
                    kind,
                    allow: Some(old),
                    deny,
                };
                app.http.update_channel_permission(channel, &o).await?;
                Ok(())
            }
            .await;
            "gave a role or member dangerous channel permissions"
        }
        A::GuildUpdate if an.vanity_guard => {
            let Some(old) = e.changes.iter().find_map(|c| match c {
                AuditLogChange::VanityUrlCode { old, .. } => Some(old.clone()),
                _ => None,
            }) else {
                return Ok(());
            };
            unsure = match spared().await {
                None => return Ok(()),
                Some(u) => u,
            };
            // twilight has no vanity route, so this is a raw PATCH (needs Manage Server).
            undo = match old {
                Some(code) => {
                    async {
                        let req =
                            RequestBuilder::raw(Method::Patch, format!("guilds/{gid}/vanity-url"))
                                .json(&json!({ "code": code }))
                                .build()?;
                        app.http.request::<()>(req).await?;
                        anyhow::Ok(())
                    }
                    .await
                }
                None => Err(anyhow::anyhow!("the server had no vanity URL before")),
            };
            "changed the vanity URL"
        }
        _ => return Ok(()),
    };
    let failed = undo.err().map(|err| {
        tracing::warn!(?err, why, "undoing a guarded change failed");
        format!("NOT undone: {err}. Check it and set it back by hand")
    });
    let member = match e.action_type {
        A::MemberRoleUpdate | A::BotAdd => Some(target.cast()),
        A::ChannelOverwriteCreate | A::ChannelOverwriteUpdate => e
            .options
            .as_ref()
            .filter(|o| o.kind.as_deref() == Some("1"))
            .and_then(|o| o.id)
            .map(|id| id.cast()),
        _ => None,
    };
    let quiet = unsure || !punish;
    // A reaction-role or sticky bot gives the role again at once: log each loop once (as permit roles).
    let mut repeats = String::new();
    if quiet && failed.is_none() && e.action_type == A::MemberRoleUpdate {
        let mut set: Vec<i64> = e
            .changes
            .iter()
            .filter_map(|c| match c {
                AuditLogChange::RoleAdded { new, .. } => Some(new),
                _ => None,
            })
            .flatten()
            .map(|r| r.id.get() as i64)
            .collect();
        set.sort();
        let key = (gid.get(), actor.get(), set);
        match take_back_log_due(&mut TAKE_BACKS.lock().unwrap(), key, Instant::now()) {
            None => return Ok(()),
            Some(Some((n, since))) => {
                let at = crate::app::now_secs() - since.elapsed().as_secs() as i64;
                repeats =
                    format!(" Taken back {n} more times since <t:{at}:R>, not logged one by one.");
            }
            Some(None) => {}
        }
    }
    let skip = "not set back: the permissions were changed again since, so that later change stays. Check them";
    let action = match (&failed, skipped) {
        (Some(f), _) => Some(f.as_str()),
        (None, true) => Some(skip),
        (None, false) => None,
    };
    if let Some(action) = action {
        let logged = undone_log(app, gid, actor, member, why, action).await;
        if quiet {
            return logged;
        }
        if let Err(err) = logged {
            tracing::warn!(?err, "anti-nuke log failed");
        }
    } else if unsure {
        let action = if trusted.load(std::sync::atomic::Ordering::Relaxed) {
            format!("undone: only an Owner or Extra Owner gives the quarantine role permissions. A Trusted Admin is never punished.{repeats}")
        } else {
            format!(
                "undone where it can be; not quarantined: their permits could not be read.{repeats}"
            )
        };
        return undone_log(app, gid, actor, member, why, &action).await;
    } else if !punish {
        return undone_log(app, gid, actor, member, why, &format!("{OFF}.{repeats}")).await;
    }
    quarantine_and_revert(app, gid, actor, why).await
}

/// A kick, ban or timeout done outside Brim to someone the actor does not outrank (owner decision
/// 2026-11-15): a ban is lifted, a timeout removed, a kicked or banned victim gets a one-use
/// invite by DM, and the actor is quarantined like any breach; with anti-nuke off or an immune
/// actor, only undone and logged. The guild owner outranks all. A leaver still loses their permit
/// (`permit_sync::on_leave`).
async fn guard_rank(
    app: &App,
    cfg: &GuildCfg,
    e: &AuditLogEntry,
    actor: Id<UserMarker>,
    punish: bool,
) -> Result<()> {
    let (Some(gid), Some(target)) = (e.guild_id, e.target_id) else {
        return Ok(());
    };
    let target: Id<UserMarker> = target.cast();
    // Any unread level: undone and logged as if the victim held the highest level and the
    // actor none, and nobody is punished (owner decision 11-17: no punishment retry).
    let owner = app
        .guild_info(gid)
        .await
        .map(|i| i.owner)
        .inspect_err(|e| tracing::warn!(?e, "reading the owner for the rank guard failed"))
        .ok();
    // The bot owner skips the guard; a Discord owner paused by a rescue claim does not.
    let bot_owner = match owner {
        Some(_) => app.bot_owner(gid).await.ok(),
        None => None,
    };
    if bot_owner == Some(actor) || actor == target {
        return Ok(());
    }
    let timeout = e.action_type == A::MemberUpdate;
    let at = crate::voice_nuke::created_ms(e.id.get());
    let mut unread = owner.is_none();
    let victim = match (owner, crate::permit_sync::left_level(gid, target, at)) {
        (_, Some(l)) if !timeout => Ok(l),
        (None, _) => Ok(permits::Level::Owner),
        // A kick or ban whose MemberRemove did not run yet: still listed by id. `false`: only the
        // id is known, and the member is gone.
        (Some(owner), _) if !timeout => {
            let (g, o, u) = (gid.get() as i64, owner.get() as i64, target.get() as i64);
            permits::resolve(&app.pool, g, o, u, false).await
        }
        (Some(owner), _) => app.level(gid, owner, target).await,
    };
    let victim = victim.unwrap_or_else(|e| {
        tracing::warn!(
            ?e,
            "reading the victim's level failed, undoing without punishment"
        );
        unread = true;
        permits::Level::Owner
    });
    if victim == permits::Level::None {
        return Ok(());
    }
    // An actor who left since is judged by the level they had.
    let mine = match (owner, crate::permit_sync::left_level(gid, actor, at)) {
        (_, Some(l)) => l,
        (None, None) => permits::Level::None,
        (Some(owner), None) => app.level(gid, owner, actor).await.unwrap_or_else(|e| {
            tracing::warn!(
                ?e,
                "reading the actor's level failed, undoing without punishment"
            );
            unread = true;
            permits::Level::None
        }),
    };
    if mine.outranks(&victim) {
        return Ok(());
    }
    const REASON: &str = "Anti-nuke: only a higher level acts against a permit holder";
    let (what, undone) = match e.action_type {
        A::MemberBanAdd => {
            let r = app.http.delete_ban(gid, target).reason(REASON).await;
            ("banned", Some(("ban lifted", r.map(drop))))
        }
        A::MemberUpdate => {
            let r = app
                .http
                .update_guild_member(gid, target)
                .communication_disabled_until(None)
                .reason(REASON)
                .await;
            ("timed out", Some(("timeout removed", r.map(drop))))
        }
        _ => ("kicked", None),
    };
    let mut done = vec![];
    let mut undo_failed = false;
    match undone {
        Some((ok, Ok(()))) => done.push(ok.to_string()),
        Some((ok, Err(err))) => {
            tracing::warn!(
                ?err,
                "undoing a kick, ban or timeout of a higher level failed"
            );
            undo_failed = true;
            done.push(format!("failed, not {ok}"));
        }
        None => {}
    }
    if !timeout && undo_failed {
        done.push("no invite: the ban stays".into());
    } else if !timeout {
        done.push(reinvite(app, cfg, gid, target, what).await.into());
    }
    let why = format!(
        "{what} <@{target}>, who holds the same or a higher level; only a higher level may ({})",
        done.join("; ")
    );
    let immune = mine.immune();
    if punish && !immune && !unread {
        return quarantine_and_revert(app, gid, actor, &why).await;
    }
    let tail = match (unread, immune, undo_failed) {
        (true, ..) => "not punished: a level could not be read",
        (_, true, false) => "undone, the actor is immune",
        (_, true, true) => "not undone, the actor is immune",
        (_, false, _) => "Anti-nuke is off, so nobody was punished",
    };
    let name = crate::reply::md(&crate::verify::guild_name(app, gid).await);
    crate::reply::dm_owner(
        app,
        gid,
        "dm.security",
        &format!("**{name}**: <@{actor}> {why}: {tail}."),
    )
    .await;
    let head = format!("{what} a member of the same or a higher level");
    let action = if immune || unread {
        format!("{}; {tail}", done.join("; "))
    } else {
        format!("{}. {OFF}", done.join("; "))
    };
    undone_log(app, gid, actor, Some(target), &head, &action).await
}

/// (guild, user) -> when Brim last made them an invite back.
static INVITED: std::sync::LazyLock<std::sync::Mutex<HashMap<(u64, u64), Instant>>> =
    std::sync::LazyLock::new(Default::default);

/// Best effort: a one-use invite, valid 24 hours, DMed to a member kicked or banned against the
/// rank rule, at most one per (guild, user) in 10 minutes (in memory). Made on the verification
/// channel, else the system channel; none for a bot. Returns the line for the log.
async fn reinvite(
    app: &App,
    cfg: &GuildCfg,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    what: &str,
) -> &'static str {
    let bot = async { anyhow::Ok(app.http.user(user).await?.model().await?.bot) };
    match bot.await {
        Ok(false) => {}
        Ok(true) => return "no invite: a bot",
        Err(e) => {
            tracing::warn!(?e, "reading a kicked or banned user failed");
            return "no invite: Brim could not read the user";
        }
    }
    // A stored id from before the save-time checks may name another server's channel.
    let ours = match cfg.verification.channel {
        Some(c) => match app.guild_info(gid).await {
            Ok(info) => info.channels.contains(&c).then_some(c),
            Err(e) => {
                tracing::warn!(?e, "reading the server's channels for an invite failed");
                return "no invite: Brim could not read the server's channels";
            }
        },
        None => None,
    };
    let channel = match ours {
        Some(c) => Some(c),
        None => match app.http.guild(gid).await {
            Ok(r) => r.model().await.ok().and_then(|g| g.system_channel_id),
            Err(e) => {
                tracing::warn!(?e, "reading the system channel for an invite failed");
                None
            }
        },
    };
    let Some(ch) = channel else {
        return "no invite: no verification or system channel";
    };
    {
        let mut all = INVITED.lock().unwrap();
        all.retain(|_, at| at.elapsed() < TAKE_BACK_LOG_EVERY);
        if all.contains_key(&(gid.get(), user.get())) {
            return "no invite: one was sent in the last 10 minutes";
        }
        all.insert((gid.get(), user.get()), Instant::now());
    }
    let made = async {
        let req = app
            .http
            .create_invite(ch)
            .max_age(86_400)
            .max_uses(1)
            .unique(true);
        let bytes = req
            .reason("Anti-nuke: invite back after a kick or ban against the rank rule")
            .await?
            .bytes()
            .await?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        anyhow::Ok(v["code"].as_str().map(String::from))
    };
    let code = match made.await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(?e, channel = ch.get(), "creating an invite failed");
            None
        }
    };
    let Some(code) = code else {
        INVITED.lock().unwrap().remove(&(gid.get(), user.get()));
        return "no invite: Brim could not create one";
    };
    let name = crate::reply::md(&crate::verify::guild_name(app, gid).await);
    let text = format!("You were {what} from **{name}** by someone who may not act against your level, so Brim set it aside. Join again with this invite (one use, valid 24 hours): https://discord.gg/{code}\nLeaving took your permit, Extra Owner or Trusted Admin: ask the owner to add you again.");
    if crate::reply::dm_msg(
        app,
        Some(gid),
        user,
        "dm.invite_back",
        &[("text", &text)],
        &[],
    )
    .await
    {
        "invite DM sent"
    } else {
        "invite DM failed (DMs closed?)"
    }
}

/// The roles of `given` that are not managed and carry dangerous permissions (a lock's saved ones
/// count). A failed lock read falls back to the live roles. Mention @everyone alone on a staff
/// permit role does not count: Auto Setup gives it on purpose, and the permit guard handles who
/// may hold that role.
async fn dangerous_given(
    app: &App,
    gid: Id<GuildMarker>,
    given: &[Id<RoleMarker>],
) -> Result<Vec<Id<RoleMarker>>> {
    let staff = brim_core::permits::all_staff_roles(&app.pool, gid.get() as i64).await?;
    let main = app.cfg(gid.get()).await?.lockdown.main_roles.clone();
    // As the security score: a main role (every member has it) never gets the staff exception.
    let mask = |r: Id<RoleMarker>| {
        if staff.contains(&(r.get() as i64)) && !main.contains(&r) {
            brim_antinuke::STAFF_DANGEROUS
        } else {
            brim_antinuke::DANGEROUS_PERMS
        }
    };
    let pick = |managed: &dyn Fn(Id<RoleMarker>) -> bool,
                perms: &dyn Fn(Id<RoleMarker>) -> Option<u64>| {
        given
            .iter()
            .copied()
            .filter(|r| !managed(*r) && perms(*r).is_some_and(|p| p & mask(*r) != 0))
            .collect()
    };
    match crate::lockdown::unlocked_info(app, gid).await {
        Ok(info) => Ok(pick(&|r| info.managed.contains(&r), &|r| {
            info.role_perms.get(&r).copied()
        })),
        Err(err) => {
            tracing::warn!(
                ?err,
                "reading the saved lock permissions failed, using the live roles"
            );
            let roles = app.http.roles(gid).await?.models().await?;
            let find = |r: Id<RoleMarker>| roles.iter().find(|x| x.id == r);
            Ok(pick(&|r| find(r).is_some_and(|x| x.managed), &|r| {
                find(r).map(|x| x.permissions.bits())
            }))
        }
    }
}

/// Giving a member a role that carries a permit hands out a permit, and with it any whitelist:
/// undone and the actor quarantined, unless an Owner, Extra Owner or Trusted Admin did it.
/// Once the guild's permits belong to user ids, a permit role is taken back from anyone not listed
/// in its permit, whoever gave it, and nobody is punished: a role alone gives no permit (owner decision 2026-11-15).
async fn guard_permit_roles(
    app: &App,
    e: &AuditLogEntry,
    actor: Id<UserMarker>,
    punish: bool,
) -> Result<()> {
    let (Some(gid), Some(target)) = (e.guild_id, e.target_id) else {
        return Ok(());
    };
    let added = role_ids(e, true);
    // A dangerous role is `guard_perms`'s: taken back and logged once, there.
    let ids: Vec<Id<RoleMarker>> = added.iter().map(|r| Id::new(*r as u64)).collect();
    let theirs = dangerous_given(app, gid, &ids).await.unwrap_or_default();
    let added: Vec<i64> = added
        .into_iter()
        .filter(|r| !theirs.contains(&Id::new(*r as u64)))
        .collect();
    // ... but `guard_perms` spares a Trusted Admin, so their dangerous grants are checked below.
    let dangerous: Vec<i64> = theirs.iter().map(|r| r.get() as i64).collect();
    if added.is_empty() && dangerous.is_empty() {
        return Ok(());
    }
    let g = gid.get() as i64;
    let mut roles = permit_roles_in(&app.pool, g, &added).await?;
    let listed = !roles.is_empty() && permits::members_migrated(&app.pool, g).await?;
    if listed {
        let owner = app.guild_info(gid).await?.owner.get() as i64;
        let own = permits::member_roles(&app.pool, g, target.get() as i64, owner).await?;
        roles.retain(|r| !own.contains(&r.0));
    }
    if roles.is_empty() && (listed || dangerous.is_empty()) {
        return Ok(());
    }
    // Unread, the role is taken back but nobody punished (owner decision).
    let level = if listed {
        Ok(permits::Level::None)
    } else {
        match app.guild_info(gid).await {
            Ok(info) => app.level(gid, info.owner, actor).await,
            Err(e) => Err(e),
        }
    };
    // A Trusted Admin is never punished, but only an Owner or Extra Owner hands out a permit
    // with the anti-nuke or quarantine whitelist or View Mod Stats (as the dashboard's keep_exemptions).
    let mut trusted = false;
    let unsure = match level {
        Ok(permits::Level::Owner | permits::Level::ExtraOwner) => return Ok(()),
        Ok(permits::Level::TrustedAdmin) => {
            roles.extend(
                permit_roles_in(&app.pool, g, &dangerous)
                    .await
                    .unwrap_or_default(),
            );
            // Unread: taken back all the same; a Trusted Admin is never punished.
            let exempt = owner_only_roles(&app.pool, g, &roles)
                .await
                .unwrap_or_else(|_| roles.iter().map(|r| r.0).collect());
            roles.retain(|r| exempt.contains(&r.0));
            if roles.is_empty() {
                return Ok(());
            }
            trusted = true;
            false
        }
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(
                ?e,
                "immunity lookup failed, taking the permit role back without punishment"
            );
            true
        }
    };
    // Only dangerous roles came: those are `guard_perms`'s for everyone but a Trusted Admin.
    if roles.is_empty() {
        return Ok(());
    }
    if listed && fight_stopped(app, gid, actor, target.cast()).await {
        return Ok(());
    }
    let rule = if listed {
        "Brim gives a permit's roles only to the members listed in the permit"
    } else if trusted {
        "only an Owner or Extra Owner gives roles of a permit with the anti-nuke or quarantine whitelist or View Mod Stats"
    } else {
        "only an Owner, Extra Owner or Trusted Admin gives permit roles"
    };
    // A failed undo must not spare the actor.
    let mut failed = vec![];
    for &(r, _) in &roles {
        let undo = app
            .http
            .remove_guild_member_role(gid, target.cast(), Id::new(r as u64))
            .reason(if listed {
                "Anti-nuke: Brim gives a permit's roles only to the permit's members"
            } else if trusted {
                "Anti-nuke: roles of a permit with the anti-nuke or quarantine whitelist are given only by an Owner or Extra Owner"
            } else {
                "Anti-nuke: a permit role is given only by an Owner, Extra Owner or Trusted Admin"
            })
            .await;
        if let Err(err) = undo {
            tracing::warn!(?err, "taking a permit role back failed");
            failed.push(r);
        }
    }
    let names: Vec<String> = roles.iter().map(|r| format!("<@&{}>", r.0)).collect();
    let mut set: Vec<i64> = roles.iter().map(|r| r.0).collect();
    set.sort();
    let key = (gid.get(), actor.get(), set);
    // Only weak permits (a link whitelist, say) are taken back too, but punish nobody: they hold
    // no moderation, dashboard or anti-nuke rights (owner decision, 11-13; bots get no exception).
    let weak = !roles.iter().any(|r| r.1);
    // A migrated guild's owner hears of a fight only from `fight_stopped`: one DM, not two.
    if !weak
        && !listed
        && !trusted
        && owner_dm_due(&mut OWNER_DMS.lock().unwrap(), (gid.get(), actor.get()))
    {
        let name = crate::reply::md(&crate::verify::guild_name(app, gid).await);
        let fix = format!("If <@{actor}> should give these roles, make them an Extra Owner or Trusted Admin; else keep the permit on a staff role that only staff hand out. A bot (a reaction-role or level bot, say) cannot give permit roles: move the permit off the roles it hands out.");
        let text = format!(
            "**{name}**: <@{actor}> keeps giving out roles with a staff permit (last: {}). Brim took them back {OWNER_DM_AFTER} times: {rule}. {fix}",
            names.join(", ")
        );
        crate::reply::dm_owner(app, gid, "dm.security", &text).await;
    }
    if unsure {
        let what = "gave a member a role that carries a permit";
        return unsure_log(app, gid, actor, Some(target.cast()), what).await;
    }
    if listed || !punish || weak || trusted {
        let why = if listed {
            "The role alone gives no permit"
        } else if trusted {
            "A Trusted Admin is never punished"
        } else if !punish {
            "Anti-nuke is off"
        } else {
            "The permits are weak"
        };
        let Some(repeats) = take_back_log_due(&mut TAKE_BACKS.lock().unwrap(), key, Instant::now())
        else {
            return Ok(());
        };
        let action = format!(
            "{}: {rule}. {why}, so nobody was punished",
            outcome("taken back", &failed, roles.len())
        );
        let repeats = repeats.map_or(String::new(), |(n, since)| {
            repeats_text("taken back", n, since)
        });
        let log = security_log(
            "Anti Nuke",
            "log.anti_nuke.permit_role_taken_back",
            &[
                ("by", &mention(actor.get())),
                ("member", &mention(target.get())),
                ("alert", "**Gave a member a role that carries a permit**"),
                ("roles", &names.join(", ")),
                ("action", &action),
                ("repeats", &repeats),
            ],
        );
        return send_log(app, gid.get(), LogKind::AntiNuke, log).await;
    }
    quarantine_and_revert(
        app,
        gid,
        actor,
        "gave a member a role that carries a permit",
    )
    .await
}

/// A member listed in a permit keeps its roles (owner decision 2026-11-15): a role of it that anyone but
/// Brim removes is given back. Not while a quarantine holds the member's roles.
async fn guard_permit_removal(app: &App, e: &AuditLogEntry, actor: Id<UserMarker>) -> Result<()> {
    let (Some(gid), Some(target)) = (e.guild_id, e.target_id) else {
        return Ok(());
    };
    let removed = role_ids(e, false);
    let g = gid.get() as i64;
    if removed.is_empty() || !permits::members_migrated(&app.pool, g).await? {
        return Ok(());
    }
    let owner = app.guild_info(gid).await?.owner.get() as i64;
    let own = permits::member_roles(&app.pool, g, target.get() as i64, owner).await?;
    let back: Vec<i64> = removed.into_iter().filter(|r| own.contains(r)).collect();
    if back.is_empty()
        || quarantined(app, gid, target.cast()).await?
        || fight_stopped(app, gid, actor, target.cast()).await
    {
        return Ok(());
    }
    let mut failed = vec![];
    for &r in &back {
        let give = app
            .http
            .add_guild_member_role(gid, target.cast(), Id::new(r as u64))
            .reason(crate::permit_sync::REASON)
            .await;
        if let Err(err) = give {
            tracing::warn!(?err, "giving a permit role back failed");
            failed.push(r);
        }
    }
    let mut set = back.clone();
    set.sort();
    let key = (gid.get(), actor.get(), set);
    let Some(repeats) = take_back_log_due(&mut GIVE_BACKS.lock().unwrap(), key, Instant::now())
    else {
        return Ok(());
    };
    let permit: String = sqlx::query_scalar(
        "SELECT p.name FROM permit_members m JOIN permits p ON p.id = m.permit_id
         WHERE m.guild_id = $1 AND m.user_id = $2",
    )
    .bind(g)
    .bind(target.get() as i64)
    .fetch_optional(&app.pool)
    .await?
    .unwrap_or_default();
    let names: Vec<String> = back.iter().map(|r| format!("<@&{r}>")).collect();
    let action = format!(
        "{}: Brim gives a permit's roles to its listed members. Nobody was punished",
        outcome("given back", &failed, back.len())
    );
    let repeats = repeats.map_or(String::new(), |(n, since)| {
        repeats_text("given back", n, since)
    });
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.permit_role_given_back",
        &[
            ("by", &mention(actor.get())),
            ("member", &mention(target.get())),
            (
                "alert",
                "**Removed a permit role from a member listed in the permit**",
            ),
            ("permit", &permit),
            ("roles", &names.join(", ")),
            ("action", &action),
            ("repeats", &repeats),
        ],
    );
    send_log(app, gid.get(), LogKind::AntiNuke, log).await
}

/// The role ids an entry added (`added`) or removed.
fn role_ids(e: &AuditLogEntry, added: bool) -> Vec<i64> {
    e.changes
        .iter()
        .filter_map(|c| match c {
            AuditLogChange::RoleAdded { new, .. } if added => Some(new),
            AuditLogChange::RoleRemoved { new, .. } if !added => Some(new),
            _ => None,
        })
        .flatten()
        .map(|r| r.id.get() as i64)
        .collect()
}

/// What a take-back or give-back of `all` roles did: `done`, or which roles failed.
fn outcome(done: &str, failed: &[i64], all: usize) -> String {
    let f: Vec<String> = failed.iter().map(|r| format!("<@&{r}>")).collect();
    match failed.len() {
        0 => done.into(),
        n if n == all => format!("failed, not {done}"),
        _ => format!("{done}, except {} (failed)", f.join(", ")),
    }
}

/// After this many take-backs plus give-backs on one (guild, actor, member) in
/// [`TAKE_BACK_LOG_EVERY`], Brim reverts them only once per [`FIGHT_SLOW`] for the rest of
/// that window: another bot or user fights it, and each revert feeds the loop. It never stops: a
/// stop would leave the role where the fighter put it.
const FIGHT_MAX: u32 = 5;
const FIGHT_SLOW: Duration = Duration::from_secs(60);
/// Per (guild, actor, member): when the window started, how many reverts it counted, and when
/// the last one was done.
type Fights = HashMap<(u64, u64, u64), (Instant, u32, Instant)>;
static FIGHTS: std::sync::LazyLock<std::sync::Mutex<Fights>> =
    std::sync::LazyLock::new(Default::default);

/// Counts one revert of (guild, actor, member) in its window: the count, and whether to revert
/// now. In memory: a restart starts over.
fn fight_count(m: &mut Fights, key: (u64, u64, u64), now: Instant) -> (u32, bool) {
    m.retain(|_, (at, _, _)| now.duration_since(*at) < TAKE_BACK_LOG_EVERY);
    if m.len() > 10_000 {
        crate::caps::drop_biggest(m, |k| k.0);
    }
    let (_, n, last) = m.entry(key).or_insert((now, 0, now));
    *n = n.saturating_add(1);
    let due = *n <= FIGHT_MAX || now.duration_since(*last) >= FIGHT_SLOW;
    if due {
        *last = now;
    }
    (*n, due)
}

/// Whether Brim skips this revert of `actor`'s permit role change on `target` (past
/// [`FIGHT_MAX`], within [`FIGHT_SLOW`] of the last); the first time past it sends one log and
/// one owner DM.
async fn fight_stopped(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    target: Id<UserMarker>,
) -> bool {
    let key = (gid.get(), actor.get(), target.get());
    let (n, due) = fight_count(&mut FIGHTS.lock().unwrap(), key, Instant::now());
    if n == FIGHT_MAX + 1 {
        let what = format!("<@{actor}> changed the permit roles of <@{target}> {FIGHT_MAX} times in 10 minutes, and Brim set them back each time. Another bot or user keeps changing this member's permit roles and should be stopped; for 10 minutes Brim sets them back at most once a minute.");
        let log = security_log(
            "Anti Nuke",
            "log.anti_nuke.permit_roles_slowed",
            &[
                ("by", &mention(actor.get())),
                ("member", &mention(target.get())),
                ("alert", "**Slowed down reverting a member's permit roles**"),
                ("action", &what),
            ],
        );
        if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
            tracing::warn!(?e, "permit fight log failed");
        }
        let name = crate::reply::md(&crate::verify::guild_name(app, gid).await);
        crate::reply::dm_owner(app, gid, "dm.security", &format!("**{name}**: {what}")).await;
    }
    !due
}

/// Whether the member update path, which knows no actor, skips taking `target`'s permit roles in
/// `guild`: a fight on them is past [`FIGHT_MAX`] and one was reverted in the last [`FIGHT_SLOW`].
/// Else a revert now counts as that minute's one.
pub(crate) fn permit_fight_stopped(guild: u64, target: u64) -> bool {
    fight_held(&mut FIGHTS.lock().unwrap(), guild, target, Instant::now())
}

fn fight_held(m: &mut Fights, guild: u64, target: u64, now: Instant) -> bool {
    let mut lasts: Vec<&mut Instant> = m
        .iter_mut()
        .filter(|(k, (at, n, _))| {
            k.0 == guild
                && k.2 == target
                && *n > FIGHT_MAX
                && now.duration_since(*at) < TAKE_BACK_LOG_EVERY
        })
        .map(|(_, f)| &mut f.2)
        .collect();
    if lasts.iter().any(|l| now.duration_since(**l) < FIGHT_SLOW) {
        return true;
    }
    for l in &mut lasts {
        **l = now;
    }
    false
}

/// The log's "Repeats" line: `n` take-backs (or give-backs) since `since`, not logged one by one.
fn repeats_text(what: &str, n: u32, since: Instant) -> String {
    let at = crate::app::now_secs() - since.elapsed().as_secs() as i64;
    format!("{what} {n} times since <t:{at}:R>, not logged one by one")
}

/// After this many take-backs by one (guild, actor) with a staff permit, the owner gets one DM
/// (owner decision: staff permits only).
const OWNER_DM_AFTER: u32 = 5;
/// Take-backs per (guild, actor), and the pairs whose owner was DMed already.
#[derive(Default)]
struct OwnerDms {
    counts: HashMap<(u64, u64), u32>,
    // shortcut: grows by one per DM sent and is never pruned; DMs are rare, prune if that changes.
    sent: std::collections::HashSet<(u64, u64)>,
}
static OWNER_DMS: std::sync::LazyLock<std::sync::Mutex<OwnerDms>> =
    std::sync::LazyLock::new(Default::default);

/// Counts a take-back; true once per (guild, actor), at the [`OWNER_DM_AFTER`]th. The size cap
/// clears only the counts, so it never sends again. In memory: a restart may DM again.
fn owner_dm_due(m: &mut OwnerDms, key: (u64, u64)) -> bool {
    if m.sent.contains(&key) {
        return false;
    }
    if m.counts.len() > 10_000 {
        crate::caps::drop_biggest(&mut m.counts, |k| k.0);
    }
    let n = m.counts.entry(key).or_insert(0);
    *n = n.saturating_add(1);
    if *n < OWNER_DM_AFTER {
        return false;
    }
    m.counts.remove(&key);
    m.sent.insert(key)
}

/// How often a take-back of one (guild, actor, role set) is logged; repeats in between are counted.
const TAKE_BACK_LOG_EVERY: Duration = Duration::from_secs(600);

/// Per (guild, actor, sorted role set): when its take-back was last logged, and how many came since.
type TakeBacks = HashMap<(u64, u64, Vec<i64>), (Instant, u32)>;
static TAKE_BACKS: std::sync::LazyLock<std::sync::Mutex<TakeBacks>> =
    std::sync::LazyLock::new(Default::default);
/// The same throttle for permit roles given back to a listed member (a role-stripping bot loops).
static GIVE_BACKS: std::sync::LazyLock<std::sync::Mutex<TakeBacks>> =
    std::sync::LazyLock::new(Default::default);

/// Whether this take-back is logged: `None` counts it as a repeat; `Some` logs it, with the
/// repeats since the last log (and that log's time) when there were any.
fn take_back_log_due(
    m: &mut TakeBacks,
    key: (u64, u64, Vec<i64>),
    now: Instant,
) -> Option<Option<(u32, Instant)>> {
    let fresh = |at: &Instant| now.duration_since(*at) < TAKE_BACK_LOG_EVERY;
    // Expired entries without repeats hold nothing to tell.
    m.retain(|_, (at, n)| *n > 0 || fresh(at));
    if m.len() > 10_000 {
        crate::caps::drop_biggest(m, |k| k.0);
    }
    match m.get_mut(&key) {
        Some((at, n)) if fresh(at) => {
            *n += 1;
            None
        }
        Some((at, n)) => {
            let was = (*n, *at);
            (*at, *n) = (now, 0);
            Some(Some(was))
        }
        None => {
            m.insert(key, (now, 0));
            Some(None)
        }
    }
}

/// Which of `roles` carry a permit or a level in the guild, and whether one of their permits is
/// guarded (any perm or flag outside `permits::WEAK_PERMS`, `WEAK_WHITELIST`). A level role
/// (Trusted Admin, Owner) is always guarded.
async fn permit_roles_in(pool: &PgPool, guild: i64, roles: &[i64]) -> Result<Vec<(i64, bool)>> {
    Ok(sqlx::query_as(
        "SELECT role_id, bool_or(guarded) FROM (
           SELECT r.role_id, NOT (p.perms <@ $3 AND p.whitelist <@ $4) AS guarded FROM permit_roles r
           JOIN permits p ON p.id = r.permit_id WHERE r.guild_id = $1 AND r.role_id = ANY($2)
           UNION ALL SELECT role_id, true FROM level_roles WHERE guild_id = $1 AND role_id = ANY($2)
         ) x GROUP BY role_id",
    )
    .bind(guild)
    .bind(roles)
    .bind(permits::WEAK_PERMS)
    .bind(permits::WEAK_WHITELIST)
    .fetch_all(pool)
    .await?)
}

/// The roles among `roles` whose permit has a flag only an Owner or Extra Owner may hand out: the
/// anti-nuke or quarantine whitelist (`all`, `antinuke`, `quarantine`, `wl_*`) or View Mod Stats.
async fn owner_only_roles(pool: &PgPool, guild: i64, roles: &[(i64, bool)]) -> Result<Vec<i64>> {
    let ids: Vec<i64> = roles.iter().map(|r| r.0).collect();
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT r.role_id FROM permit_roles r JOIN permits p ON p.id = r.permit_id
         WHERE r.guild_id = $1 AND r.role_id = ANY($2)
           AND (p.whitelist && ARRAY['all', 'antinuke', 'quarantine']
                OR EXISTS (SELECT 1 FROM unnest(p.whitelist) w WHERE left(w, 3) = 'wl_')
                OR 'mod_stats' = ANY(p.perms))",
    )
    .bind(guild)
    .bind(&ids)
    .fetch_all(pool)
    .await?)
}

/// A change to something an active lock holds, with the value the lock left there.
#[derive(Debug, PartialEq)]
enum Locked {
    /// A stripped role's permissions changed: `to` is its saved value minus what its locks take.
    Role { id: u64, to: Permissions },
    /// A locked overwrite changed or was deleted: `allow`/`deny` are what the lock wrote.
    Overwrite {
        channel: u64,
        role: u64,
        allow: Permissions,
        deny: Permissions,
    },
}

/// What `e` changed of the active locks' targets, if anything. The target comes from the lock
/// snapshots, never from the entry: two quick edits would otherwise set back to the first edit.
fn lock_target(e: &AuditLogEntry, locks: &[brim_automod::lockdown::Snapshot]) -> Option<Locked> {
    use brim_automod::lockdown::{DANGEROUS, SILENCE};
    let target = e.target_id?.get();
    match e.action_type {
        A::RoleUpdate => {
            let old = e.changes.iter().find_map(|c| match c {
                AuditLogChange::Permissions { old, .. } => Some(old.map_or(0, |o| o.bits())),
                _ => None,
            })?;
            // Every lock that holds the role: the lowest saved value, minus all they take.
            let (saved, takes) = locks
                .iter()
                .filter_map(|l| {
                    let (_, saved) = l.role_perms.iter().find(|(r, _)| *r == target)?;
                    Some((*saved, l.takes()))
                })
                .reduce(|a, b| (a.0 & b.0, a.1 | b.1))?;
            // A set-back never adds a dangerous bit the role lacked before this change: a channel
            // lock takes none, and the saved value may hold some staff removed since.
            Some(Locked::Role {
                id: target,
                to: Permissions::from_bits_retain(saved & !takes & !(DANGEROUS & !old)),
            })
        }
        A::ChannelOverwriteUpdate | A::ChannelOverwriteDelete => {
            let role = e.options.as_ref()?.id?.get();
            // As `lockdown::exec` wrote it: the overwrite before the lock, with SILENCE denied.
            let (allow, deny) = locks
                .iter()
                .filter(|l| l.channel_denies.contains(&(target, role)))
                .map(|l| {
                    l.prior
                        .iter()
                        .find(|p| (p.0, p.1) == (target, role))
                        .map_or((0, SILENCE), |p| (p.2 & !SILENCE, p.3 | SILENCE))
                })
                .reduce(|a, b| (a.0 & b.0, a.1 | b.1))?;
            // As for a role: never allow a dangerous bit the entry's old allow lacked.
            let old_allow = e
                .changes
                .iter()
                .find_map(|c| match c {
                    AuditLogChange::Allow { old, .. } => Some(old.map_or(0, |o| o.bits())),
                    _ => None,
                })
                .unwrap_or(0);
            let allow = allow & !(DANGEROUS & !old_allow);
            Some(Locked::Overwrite {
                channel: target,
                role,
                allow: Permissions::from_bits_retain(allow),
                deny: Permissions::from_bits_retain(deny),
            })
        }
        _ => None,
    }
}

/// During a lock, a change to a locked role or overwrite is set back (unless an Owner, Extra
/// Owner or Trusted Admin made it), logged, and counted on the Channel or Role Updates card
/// if `punish` (anti-nuke on).
async fn guard_lock(
    app: &App,
    cfg: &GuildCfg,
    e: &AuditLogEntry,
    actor: Id<UserMarker>,
    punish: bool,
) -> Result<()> {
    let Some(gid) = e.guild_id else {
        return Ok(());
    };
    let locks = crate::lockdown::active_locks(app, gid.get()).await?;
    let Some(locked) = lock_target(e, &locks) else {
        return Ok(());
    };
    // Unknown is set back as for anyone, but not counted: undo, not punish.
    let lookup = app.is_immune(gid, actor).await;
    let unsure = lookup.is_err();
    let immune = lookup.unwrap_or(false);
    let set_back: Result<()> = async {
        match &locked {
            _ if immune => {}
            Locked::Role { id, to } => {
                app.http
                    .update_role(gid, Id::new(*id))
                    .permissions(*to)
                    .await?;
            }
            Locked::Overwrite {
                channel,
                role,
                allow,
                deny,
            } => {
                let o = PermissionOverwrite {
                    id: Id::new(*role),
                    kind: PermissionOverwriteType::Role,
                    allow: Some(*allow),
                    deny: Some(*deny),
                };
                app.http
                    .update_channel_permission(Id::new(*channel), &o)
                    .await?;
            }
        }
        Ok(())
    }
    .await;
    let (what, action) = match &locked {
        Locked::Role { id, .. } => (format!("<@&{id}>"), Action::RoleUpdate),
        Locked::Overwrite { channel, role, .. } => {
            (format!("<#{channel}> (<@&{role}>)"), Action::ChannelUpdate)
        }
    };
    let result = match (&set_back, immune) {
        (_, true) => "left as is (Owner, Extra Owner or Trusted Admin)".to_string(),
        (Ok(()), _) if unsure => {
            "set back; not counted: their permits could not be read".to_string()
        }
        (Ok(()), _) => "set back".to_string(),
        (Err(err), _) => format!("could not be set back: {}", crate::http_err::why(err)),
    };
    let alert = format!("{NO} **Changed a locked target during a lockdown**");
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.locked_target_changed",
        &[
            ("by", &mention(actor.get())),
            ("alert", &alert),
            ("target", &what),
            ("result", &result),
        ],
    );
    if let Err(err) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?err, "lock change log failed");
    }
    // A rename is already counted as a Role Update by `watched`.
    if !punish || immune || unsure || watched(e) == Some(action) {
        return Ok(());
    }
    let exempt = app
        .whitelisted(gid, actor, "antinuke")
        .await
        .unwrap_or(false);
    let target = e.target_id.map(|t| t.get());
    count(app, cfg, gid, actor, action, target, &[], exempt, 1).await
}

/// Roles of `member` that carry dangerous permissions and sit below the bot, so it can take them.
fn dangerous_below_bot(
    member: &[Id<RoleMarker>],
    perms: &HashMap<Id<RoleMarker>, u64>,
    positions: &HashMap<Id<RoleMarker>, i64>,
    bot_top: i64,
) -> Vec<Id<RoleMarker>> {
    member
        .iter()
        .copied()
        .filter(|r| perms.get(r).is_some_and(|p| has_dangerous(*p)))
        .filter(|r| positions.get(r).is_some_and(|p| *p < bot_top))
        .collect()
}

/// Misc "enforce permits": a member with no permit loses the roles that give them dangerous permissions.
pub async fn enforce_permits(app: &App, m: &MemberUpdate) -> Result<()> {
    let gid = m.guild_id;
    if m.user.bot || !app.cfg(gid.get()).await?.misc.enforce_permits {
        return Ok(());
    }
    let info = crate::lockdown::unlocked_info(app, gid).await?;
    let (managed, bad): (Vec<_>, Vec<_>) =
        dangerous_below_bot(&m.roles, &info.role_perms, &info.roles, info.bot_top)
            .into_iter()
            .partition(|r| info.managed.contains(r));
    // Discord refuses to take a managed (bot or integration) role off: staff are told once.
    for r in managed
        .iter()
        .filter(|r| first_in(("managed", gid.get(), r.get()), None))
    {
        let alert = format!("{NO} **A managed role holds dangerous permissions**");
        let log = security_log(
            "Anti Nuke",
            "log.anti_nuke.managed_role_dangerous",
            &[("alert", &alert), ("role", &format!("<@&{r}>"))],
        );
        if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
            tracing::warn!(?e, "managed role log failed");
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    // Until the first permit sync listed the role holders by id, nobody holds a permit: taking
    // their roles now would lose the staff for good (the migration reads who holds them).
    if !permits::members_migrated(&app.pool, gid.get() as i64).await? {
        return Ok(());
    }
    let level = permits::resolve(
        &app.pool,
        gid.get() as i64,
        info.owner.get() as i64,
        m.user.id.get() as i64,
        m.user.bot,
    )
    .await?;
    if level != permits::Level::None {
        return Ok(());
    }
    // One refused role must not leave the others on.
    let mut failed = vec![];
    for &r in &bad {
        if let Err(e) = app.http.remove_guild_member_role(gid, m.user.id, r).await {
            tracing::warn!(?e, "removing a dangerous role failed");
            failed.push(r.get() as i64);
        }
    }
    let names: Vec<String> = bad.iter().map(|r| format!("<@&{r}>")).collect();
    let head = match failed.is_empty() {
        true => format!("{YES} **Dangerous roles removed**"),
        false => format!("{NO} **Dangerous roles not all removed**"),
    };
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.dangerous_roles_removed",
        &[
            ("member", &mention(m.user.id.get())),
            ("alert", &head),
            ("roles", &names.join(", ")),
            ("result", &outcome("removed", &failed, bad.len())),
        ],
    );
    send_log(app, gid.get(), LogKind::AntiNuke, log).await
}

/// Which quarantine a `quarantines` row holds (`on_join`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// Anti-nuke, staff, the Raider List, a rate limit.
    Real,
    /// Verification's quarantine on join: verifying lifts it.
    Join,
    /// Verification's time limit: a join quarantine only when the member has no roles to save,
    /// as before the flag existed (the owner has not decided whether verifying lifts it).
    Deadline,
    /// Puts an existing quarantine back on (a rejoin, sticky roles): the row keeps its saved
    /// roles, and `on_join` says which kind it is.
    Again(bool),
}

/// Strips the member's roles (saving them) so their permissions are gone. Bots go through
/// [`contain`].
pub(crate) async fn quarantine_as(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    reason: Option<&str>,
    kind: Kind,
) -> Result<()> {
    // Containment comes first: an unreadable config quarantines with the defaults (no
    // Quarantine role known, so every role that can be taken goes).
    let read = app.cfg(gid.get()).await;
    let cfg_ok = read.is_ok();
    let cfg = read.unwrap_or_else(|e| {
        tracing::warn!(
            ?e,
            "reading the config for a quarantine failed, using the defaults"
        );
        Default::default()
    });
    // Without the config the Quarantine role is unknown: a member with a row keeps the role that
    // holds them, and loses only dangerous ones. A failed row read contains as usual.
    if !cfg_ok && quarantined(app, gid, user).await.unwrap_or(false) {
        return strip_dangerous(app, gid, user).await;
    }
    let member = app.http.guild_member(gid, user).await?.model().await?;
    // Read before the row is written, so a failed read leaves no quarantine row behind.
    let (q, managed) = quarantine_parts(app, gid, &cfg).await?;
    if q.is_none() && cfg.antinuke.quarantine_role.is_some() {
        quarantine_role_missing(app, gid).await;
    }
    let roles = quarantine_roles(q, &member.roles, &managed);
    let saved: Vec<i64> = member.roles.iter().map(|r| r.get() as i64).collect();
    let (on_join, released) = row_flags(kind, cfg_ok, other_roles(&member.roles, q, &managed));
    // Saved before the roles go, so a crash between the two never loses them. A second
    // quarantine keeps the first saved list, unless a new real one finds the member holding
    // other roles again in the same membership (released by hand): then the old list is stale.
    let (g, u) = (gid.get() as i64, user.get() as i64);
    let joined = member
        .joined_at
        .and_then(|t| DateTime::from_timestamp_micros(t.as_micros()));
    let row = (
        saved.as_slice(),
        on_join,
        released.then_some(joined).flatten(),
    );
    let old = save_row(&app.pool, (g, u), row).await?;
    let update = app.http.update_guild_member(gid, user).roles(&roles);
    let update = match reason {
        Some(r) => update.reason(r),
        None => update,
    };
    if let Err(e) = update.await {
        // Role too low or permission missing: they are not quarantined, so the row goes back.
        restore_row(&app.pool, (g, u), old).await?;
        return Err(e.into());
    }
    // A join quarantine is only "not verified yet": no punishment, links stay.
    if kind != Kind::Join {
        crate::invite_cmds::revoke_links_later(app, gid, user);
    }
    // The member is quarantined: a missing revert row must not report them as free.
    if let Err(e) = record_quarantine(app, gid, user).await {
        tracing::warn!(?e, "logging the quarantine for the revert center failed");
    }
    Ok(())
}

/// [`quarantine_as`] without the config, for a member with a row: a released, re-armed member may
/// hold staff roles again, so every role with dangerous permissions goes. Ok only while they look
/// held (at most one role besides managed ones: the Quarantine role) and keep nothing dangerous.
async fn strip_dangerous(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> Result<()> {
    let info = match crate::lockdown::unlocked_info(app, gid).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(?e, "reading the locks failed, judging live permissions");
            app.guild_info(gid).await?
        }
    };
    let member = app.http.guild_member(gid, user).await?.model().await?;
    let bad = dangerous_below_bot(&member.roles, &info.role_perms, &info.roles, info.bot_top);
    let bad: Vec<_> = bad
        .into_iter()
        .filter(|r| !info.managed.contains(r))
        .collect();
    if !bad.is_empty() {
        let keep: Vec<_> = member
            .roles
            .iter()
            .filter(|r| !bad.contains(r))
            .copied()
            .collect();
        app.http.update_guild_member(gid, user).roles(&keep).await?;
    }
    let left = member
        .roles
        .iter()
        .filter(|r| !bad.contains(r) && !info.managed.contains(r))
        .count();
    // A dangerous role the bot cannot take (above it, or managed) still leaves them armed.
    let armed = member
        .roles
        .iter()
        .any(|r| !bad.contains(r) && info.role_perms.get(r).is_some_and(|p| has_dangerous(*p)));
    anyhow::ensure!(
        left <= 1 && !armed,
        "the settings could not be read, so only roles with dangerous permissions were taken"
    );
    Ok(())
}

/// A `quarantines` row: saved roles, on_join, created_at.
type Row = (Vec<i64>, bool, DateTime<Utc>);

/// Which row version [`save_row`] wrote (`xmin`, new on every write), and the row it replaced
/// (None after an insert).
#[derive(Debug, PartialEq)]
struct Saved {
    inserted: bool,
    version: i64,
    old: Option<Row>,
}

/// Writes the quarantine row. A real quarantine wins over a join one. `released`: the member
/// holds other roles again and joined at this time; when that was before the row's quarantine,
/// the saved list is stale, so today's roles are saved and the quarantine starts again.
async fn save_row(
    pool: &PgPool,
    (g, u): (i64, i64),
    (saved, on_join, released): (&[i64], bool, Option<DateTime<Utc>>),
) -> Result<Saved> {
    type Out = (
        bool,
        i64,
        Option<Vec<i64>>,
        Option<bool>,
        Option<DateTime<Utc>>,
    );
    let r: Out = sqlx::query_as(
        "WITH old AS (SELECT saved_roles, on_join, created_at FROM quarantines
                      WHERE guild_id = $1 AND user_id = $2)
         INSERT INTO quarantines AS q (guild_id, user_id, saved_roles, on_join) VALUES ($1, $2, $3, $4)
         ON CONFLICT (guild_id, user_id) DO UPDATE SET
             saved_roles = CASE WHEN $5 < q.created_at THEN EXCLUDED.saved_roles ELSE q.saved_roles END,
             created_at = CASE WHEN $5 < q.created_at THEN now() ELSE q.created_at END,
             on_join = q.on_join AND EXCLUDED.on_join
         RETURNING q.xmax = 0, q.xmin::text::bigint,
                   (SELECT saved_roles FROM old), (SELECT on_join FROM old), (SELECT created_at FROM old)",
    )
    .bind(g)
    .bind(u)
    .bind(saved)
    .bind(on_join)
    .bind(released)
    .fetch_one(pool)
    .await?;
    let old = match (r.2, r.3, r.4) {
        (Some(s), Some(j), Some(at)) if !r.0 => Some((s, j, at)),
        _ => None,
    };
    Ok(Saved {
        inserted: r.0,
        version: r.1,
        old,
    })
}

/// Undoes [`save_row`] only while the row is still the version it wrote, so a quarantine written
/// meanwhile (even an identical one) is never deleted or downgraded.
async fn restore_row(pool: &PgPool, (g, u): (i64, i64), s: Saved) -> Result<()> {
    let q = match (s.inserted, s.old) {
        (true, _) => sqlx::query(
            "DELETE FROM quarantines WHERE guild_id = $1 AND user_id = $2 AND xmin::text::bigint = $3",
        )
        .bind(g)
        .bind(u),
        (false, Some(old)) => sqlx::query(
            "UPDATE quarantines SET saved_roles = $1, on_join = $2, created_at = $3
             WHERE guild_id = $4 AND user_id = $5 AND xmin::text::bigint = $6",
        )
        .bind(old.0)
        .bind(old.1)
        .bind(old.2)
        .bind(g)
        .bind(u),
        (false, None) => return Ok(()),
    };
    q.bind(s.version).execute(pool).await?;
    Ok(())
}

/// The quarantine role while it still exists in Discord, and the managed roles, read live.
async fn quarantine_parts(
    app: &App,
    gid: Id<GuildMarker>,
    cfg: &GuildCfg,
) -> Result<(Option<Id<RoleMarker>>, Vec<Id<RoleMarker>>)> {
    let live = app.http.roles(gid).await?.models().await?;
    let q = cfg
        .antinuke
        .quarantine_role
        .filter(|q| live.iter().any(|r| r.id == *q));
    let managed = live
        .into_iter()
        .filter(|r| r.managed)
        .map(|r| r.id)
        .collect();
    Ok((q, managed))
}

/// True the first time `key` comes up, then again only once `every` has passed (never with None).
fn first_in(key: (&'static str, u64, u64), every: Option<Duration>) -> bool {
    type Seen = std::sync::Mutex<HashMap<(&'static str, u64, u64), Instant>>;
    static SEEN: std::sync::LazyLock<Seen> = std::sync::LazyLock::new(Default::default);
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    // shortcut: past the cap, keys older than an hour go (a once-only log may repeat), fine
    // while no window is longer than an hour.
    if seen.len() > 10_000 {
        seen.retain(|_, at| now.duration_since(*at) < HOUR);
    }
    match seen.get(&key) {
        Some(at) if every.is_none_or(|e| now.duration_since(*at) < e) => false,
        _ => {
            seen.insert(key, now);
            true
        }
    }
}

/// A deleted Quarantine role: members are still stripped (to their managed roles), and staff
/// learn why they no longer get the role (at most every 10 minutes, a raid quarantines many).
async fn quarantine_role_missing(app: &App, gid: Id<GuildMarker>) {
    tracing::warn!(guild = gid.get(), "quarantine role missing");
    if !first_in(("q_missing", gid.get(), 0), Some(Duration::from_secs(600))) {
        return;
    }
    let alert = format!("{NO} **Quarantine role missing**");
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.quarantine_role_missing",
        &[("alert", &alert)],
    );
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "quarantine role missing log failed");
    }
}

/// The role list a quarantined member keeps: the quarantine role, plus the managed (bot or
/// integration) roles they hold, as Discord refuses a list that drops one.
fn quarantine_roles(
    quarantine: Option<Id<RoleMarker>>,
    member: &[Id<RoleMarker>],
    managed: &[Id<RoleMarker>],
) -> Vec<Id<RoleMarker>> {
    quarantine
        .into_iter()
        .chain(member.iter().copied().filter(|r| managed.contains(r)))
        .collect()
}

/// The member holds only what [`quarantine_roles`] leaves: they are still held in Discord.
fn held(
    member: &[Id<RoleMarker>],
    quarantine: Option<Id<RoleMarker>>,
    managed: &[Id<RoleMarker>],
) -> bool {
    quarantine.is_none_or(|q| member.contains(&q)) && !other_roles(member, quarantine, managed)
}

/// The row's (on_join, released) for a quarantine of `kind`; `other`: [`other_roles`].
/// Without the config the Quarantine role is unknown and counts as "other", so neither may
/// change the row: on_join true keeps the row's value (`save_row` ANDs it), and the saved roles
/// stay.
fn row_flags(kind: Kind, cfg_ok: bool, other: bool) -> (bool, bool) {
    let on_join = match kind {
        Kind::Real => false,
        Kind::Join => true,
        Kind::Deadline => !cfg_ok || !other,
        Kind::Again(j) => j,
    };
    (on_join, cfg_ok && kind == Kind::Real && other)
}

/// The member holds a role [`quarantine_roles`] would take.
fn other_roles(
    member: &[Id<RoleMarker>],
    quarantine: Option<Id<RoleMarker>>,
    managed: &[Id<RoleMarker>],
) -> bool {
    member
        .iter()
        .any(|r| Some(*r) != quarantine && !managed.contains(r))
}

/// Logs the bot's quarantine so the revert center can undo it. Written after the role change
/// succeeded, so a failed quarantine never leaves an unquarantine to run.
async fn record_quarantine(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> Result<()> {
    sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse) VALUES ($1,$2,$3,$4,$5)")
        .bind(gid.get() as i64)
        .bind(app.me.get() as i64)
        .bind("quarantine")
        .bind(user.get() as i64)
        .bind(json!(Inverse::Unquarantine { user: user.get() }))
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// Quarantine without a revert (a command-rate-limit breach, a deleted verification channel).
/// A bot also loses its managed role's permissions, as in a nuke.
pub(crate) async fn quarantine_actor(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    why: &str,
) -> Result<Contained> {
    let done = contain(app, gid, actor, None, Kind::Real).await?;
    if done.applied == Applied::Quarantined {
        crate::reply::queue_quarantine_dm(app, gid, actor).await;
    }
    // Best effort: the quarantine worked, so the log must go out.
    let case = match open_case(app, gid, actor, why).await {
        Ok(c) => format!("`{c}`"),
        Err(e) => {
            tracing::warn!(?e, "opening a quarantine case failed");
            "not saved".into()
        }
    };
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.quarantined",
        &[
            ("case", &case),
            ("by", &mention(actor.get())),
            ("alert", &action_text(done)),
            ("reason", why),
        ],
    );
    send_log(app, gid.get(), LogKind::AntiNuke, log).await?;
    Ok(done)
}

/// After a command rate limit breach: undoes what the actor did in the last hour, also through
/// the bot (their cases' bans and kicks), and tells staff.
pub(crate) async fn revert_breach(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    backups: &BackupsCfg,
) -> Result<()> {
    let sum = revert_actor(app, gid, actor, now_secs() - 3600, Some(backups)).await?;
    if sum.ops == 0 && sum.reinvite.is_empty() {
        return Ok(());
    }
    let reinvite = capped(sum.reinvite.iter().map(|u| format!("<@{u}>")));
    let alert = format!("{YES} **Reverted their last hour**");
    let reinvite = if reinvite.is_empty() {
        "None".into()
    } else {
        reinvite.join(" ")
    };
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.rate_limit_reverted",
        &[
            ("by", &mention(actor.get())),
            ("alert", &alert),
            (
                "reverted",
                &format!("`{}/{}`", sum.ops - sum.failed.len(), sum.ops),
            ),
            ("reinvite", &reinvite),
        ],
    );
    send_log(app, gid.get(), LogKind::AntiNuke, log).await
}

/// A `quarantines` row exists: anti-nuke, staff or verification's join quarantine holds them.
pub(crate) async fn quarantined(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
) -> Result<bool> {
    Ok(quarantine_kind(app, gid, user).await?.is_some())
}

/// The row's `on_join` (verification's join quarantine), or None without a row.
pub(crate) async fn quarantine_kind(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
) -> Result<Option<bool>> {
    Ok(
        sqlx::query_scalar("SELECT on_join FROM quarantines WHERE guild_id = $1 AND user_id = $2")
            .bind(gid.get() as i64)
            .bind(user.get() as i64)
            .fetch_optional(&app.pool)
            .await?,
    )
}

/// What [`requarantine_rejoin`] found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rejoin {
    Free,
    /// Quarantined again (anti-nuke, staff, the Raider List).
    Held,
    /// Back in verification's join quarantine: still has to verify.
    Verifying,
}

/// Leaving does not end a quarantine: a member who rejoins while their quarantine row exists is
/// contained again.
pub(crate) async fn requarantine_rejoin(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
) -> Result<Rejoin> {
    let Some(on_join) = quarantine_kind(app, gid, user).await? else {
        return Ok(Rejoin::Free);
    };
    // With verification off nobody can verify out of a join quarantine: it ends (as in N1).
    if on_join && !app.cfg(gid.get()).await?.verification.enabled {
        sqlx::query("DELETE FROM quarantines WHERE guild_id = $1 AND user_id = $2 AND on_join")
            .bind(gid.get() as i64)
            .bind(user.get() as i64)
            .execute(&app.pool)
            .await?;
        return Ok(Rejoin::Free);
    }
    // Before the roles go: an autorole bot can act at once.
    note_rejoin(gid, user);
    let done = contain(app, gid, user, None, Kind::Again(on_join)).await?;
    // Not verified yet is not an anti-nuke matter.
    if on_join {
        return Ok(Rejoin::Verifying);
    }
    if done.applied == Applied::Quarantined {
        crate::reply::queue_quarantine_dm(app, gid, user).await;
    }
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.quarantined_rejoined",
        &[
            ("member", &mention(user.get())),
            ("alert", &action_text(done)),
        ],
    );
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "rejoin quarantine log failed");
    }
    Ok(Rejoin::Held)
}

async fn open_case(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    why: &str,
) -> Result<i32> {
    cases::create(
        &app.pool,
        NewCase {
            guild_id: gid.get() as i64,
            kind: "quarantine",
            user_id: actor.get() as i64,
            mod_id: app.me.get() as i64,
            reason: Some(why),
            points: 0,
            duration_s: None,
            proof: None,
            inverse: Some(json!({"op": "unquarantine", "user": actor.get()})),
        },
    )
    .await
}

/// Result of undoing an actor's logged actions.
#[derive(Default)]
pub(crate) struct RevertSummary {
    pub ops: usize,
    pub done: Vec<Inverse>,
    /// Steps that ran and failed; they stay unreverted for the next run or `/revert`.
    pub failed: Vec<Inverse>,
    pub reinvite: Vec<u64>,
    /// Changes made to recreate deleted channels and roles from the backups.
    pub recreated: usize,
    /// Roles and channels recreated from an older backup without their dangerous permissions.
    pub stripped: usize,
    /// Deleted channels and roles that did not come back (no backup holds them, or it failed).
    pub missing: usize,
}

/// Undoes the actor's unreverted logged actions since `since_secs` (unix seconds) and marks them reverted.
pub(crate) async fn revert_actor(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    since_secs: i64,
    filters: Option<&BackupsCfg>,
) -> Result<RevertSummary> {
    let since = since_secs;
    // Only rows read here can be marked reverted at the end: the actor's newer actions are not undone yet.
    let rows: Vec<(i64, serde_json::Value, i64)> = sqlx::query_as(
        "SELECT id, inverse, EXTRACT(EPOCH FROM created_at)::BIGINT FROM action_log
         WHERE guild_id=$1 AND actor_id=$2 AND NOT reverted AND inverse IS NOT NULL AND created_at >= to_timestamp($3)
         ORDER BY id DESC",
    )
    .bind(gid.get() as i64)
    .bind(actor.get() as i64)
    .bind(since)
    .fetch_all(&app.pool)
    .await?;
    let unmarked = unmarked_rows(gid);
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| !unmarked.contains(&r.0))
        .collect();
    // A row whose inverse does not parse can never be undone, so it counts as handled.
    let read: Vec<(i64, Option<Inverse>)> = rows
        .iter()
        .map(|(id, inv, _)| (*id, serde_json::from_value(inv.clone()).ok()))
        .collect();
    // Newest first by id: `at` is whole seconds, and two renames in one second must put the
    // older name back last.
    let log: Vec<LogEntry> = rows
        .into_iter()
        .filter_map(|(_, inv, at)| {
            Some(LogEntry {
                actor: actor.get(),
                at,
                inverse: serde_json::from_value(inv).ok()?,
                reverted: false,
            })
        })
        .collect();
    let mut plan = plan_revert(&log, actor.get(), since);
    // The bot's own quarantines and cleared bot roles stay: reverting them would free a nuker.
    if actor == app.me {
        plan.ops.retain(|op| !bot_contain_op(op));
    }
    // The actor's oldest name and icon rows in the window, reverted or not: a late rerun that read
    // only newer renames still ends on the name before the first one.
    let firsts: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT DISTINCT ON (inverse->>'op') inverse FROM action_log
         WHERE guild_id=$1 AND actor_id=$2 AND inverse->>'op' IN ('guild_name','guild_icon')
           AND created_at >= to_timestamp($3)
         ORDER BY inverse->>'op', id",
    )
    .bind(gid.get() as i64)
    .bind(actor.get() as i64)
    .bind(since)
    .fetch_all(&app.pool)
    .await?;
    let firsts: Vec<Inverse> = firsts
        .into_iter()
        .filter_map(|(v,)| serde_json::from_value(v).ok())
        .collect();
    brim_antinuke::settle_guild(&mut plan.ops, &firsts);
    if let Some(f) = filters {
        plan.ops.retain(|op| match op {
            Inverse::DeleteRole { .. } => f.restore && f.delete_roles,
            Inverse::DeleteChannel { .. } => f.restore && f.delete_channels,
            Inverse::DeleteWebhook { .. } => f.restore && f.delete_webhooks,
            _ => true,
        });
    }
    // Deleted channels and roles have no logged inverse; they come back from the latest backup.
    // Read before any inverse runs: a failed read after them would leave them all unmarked.
    let gone: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT id, target_id FROM action_log WHERE guild_id=$1 AND actor_id=$2 AND NOT reverted
         AND action IN ('ChannelDelete','RoleDelete') AND target_id IS NOT NULL AND created_at >= to_timestamp($3)",
    )
    .bind(gid.get() as i64)
    .bind(actor.get() as i64)
    .bind(since)
    .fetch_all(&app.pool)
    .await?;
    let gone: Vec<(i64, i64)> = gone
        .into_iter()
        .filter(|g| !unmarked.contains(&g.0))
        .collect();
    let mut failed = vec![];
    for op in &plan.ops {
        // Already undone (a revert cut off by a restart runs again) or the target is gone.
        match run_inverse(app, gid, op).await {
            Err(e) if !crate::http_err::gone_any(&e) => {
                tracing::warn!(?e, ?op, "revert step failed");
                failed.push(op.clone());
            }
            _ => {}
        }
    }
    let targets: std::collections::HashSet<u64> = gone.iter().map(|g| g.1 as u64).collect();
    let back = if targets.is_empty() {
        Default::default()
    } else {
        backup::recreate(app, gid, &targets, filters)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(?e, "recreating deleted objects failed");
                Default::default()
            })
    };
    // Failed, filtered out or not recreated stays unreverted, for the next run or `/revert`.
    let ids: Vec<i64> = read
        .iter()
        .filter(|(_, inv)| inv.as_ref().is_none_or(|i| undone(i, &plan.ops, &failed)))
        .map(|r| r.0)
        .chain(
            gone.iter()
                .filter(|g| back.back.contains(&(g.1 as u64)))
                .map(|g| g.0),
        )
        .collect();
    // The work is done: a failed mark must not fail the run, or its retry would recreate the
    // deleted objects again. The rows are kept in memory and marked with the next run.
    let all: Vec<i64> = ids.iter().chain(&unmarked).copied().collect();
    let marked = sqlx::query("UPDATE action_log SET reverted = true WHERE id = ANY($1)")
        .bind(&all)
        .execute(&app.pool)
        .await;
    note_unmarked(gid, &all, marked.is_err());
    if let Err(e) = marked {
        tracing::warn!(?e, "marking reverted rows failed; kept in memory");
    }
    let ops = plan.ops.len();
    // Only the steps that ran: the nuke case and its report must not count a failed one as undone.
    plan.ops.retain(|op| !failed.contains(op));
    Ok(RevertSummary {
        ops,
        done: plan.ops,
        failed,
        reinvite: plan.reinvite,
        recreated: back.changes,
        stripped: back.stripped,
        missing: targets.iter().filter(|t| !back.back.contains(t)).count(),
    })
}

/// (guild, action_log id) reverted but not marked in the database (the UPDATE failed).
/// shortcut: in memory, so a restart before the next successful mark forgets them; persist if
/// that ever matters.
static UNMARKED: std::sync::LazyLock<std::sync::Mutex<HashMap<(u64, i64), Instant>>> =
    std::sync::LazyLock::new(Default::default);

fn unmarked_rows(gid: Id<GuildMarker>) -> Vec<i64> {
    let mut all = UNMARKED.lock().unwrap_or_else(|p| p.into_inner());
    all.retain(|_, at| at.elapsed() < Duration::from_secs(86_400));
    all.keys()
        .filter(|k| k.0 == gid.get())
        .map(|k| k.1)
        .collect()
}

/// Keeps `ids` as unmarked when `failed`, or forgets them once they are marked.
fn note_unmarked(gid: Id<GuildMarker>, ids: &[i64], failed: bool) {
    let mut all = UNMARKED.lock().unwrap_or_else(|p| p.into_inner());
    for &id in ids {
        if failed {
            all.insert((gid.get(), id), Instant::now());
        } else {
            all.remove(&(gid.get(), id));
        }
    }
}

/// Undoes a containment the bot made (its quarantine, a nuking bot's cleared role): never run by
/// a revert of the bot's actions, here or in the dashboard.
pub(crate) fn bot_contain_op(op: &Inverse) -> bool {
    matches!(op, Inverse::Unquarantine { .. } | Inverse::RolePerms { .. })
}

/// A read row's inverse was carried out: it is in the plan and did not fail. Name and icon rows
/// went into one op of their kind (`settle_guild`), so they follow that op. Kicks only report a
/// re-invite, which the caller sends.
fn undone(inv: &Inverse, ops: &[Inverse], failed: &[Inverse]) -> bool {
    let same = |op: &Inverse| {
        op == inv
            || matches!(
                (op, inv),
                (Inverse::GuildName { .. }, Inverse::GuildName { .. })
                    | (Inverse::GuildIcon { .. }, Inverse::GuildIcon { .. })
            )
    };
    matches!(inv, Inverse::Reinvite { .. }) || (ops.iter().any(same) && !failed.iter().any(same))
}

/// How a nuker was dealt with, for the report and the nuke case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// Auto quarantine is off.
    Off,
    /// The quarantine failed; the reason in words.
    Failed(&'static str),
    Quarantined,
    /// A bot that could not be quarantined.
    Kicked,
}

/// What [`contain`] did; `bot_role` is set for a bot with its own managed role: true when that
/// role's permissions were cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Contained {
    pub applied: Applied,
    pub bot_role: Option<bool>,
}

impl Contained {
    /// Quarantined or kicked: the nuker can do no more.
    fn stopped(&self) -> bool {
        matches!(self.applied, Applied::Quarantined | Applied::Kicked)
    }
}

const BOT_ROLE_REASON: &str = "Anti-nuke: this bot nuked the server";

/// Quarantines a nuker. A bot first loses its managed role's permissions (the role cannot be
/// taken off it) and is kicked only when that or the quarantine fails.
pub(crate) async fn contain(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    reason: Option<&str>,
    kind: Kind,
) -> Result<Contained> {
    contain_with(app, gid, actor, reason, kind, true).await
}

/// [`contain`]; `hide` false leaves [`hide_channels`] to the caller, so a nuke's revert does not
/// wait for one overwrite write per channel (`revert_and_report` hides after the revert).
async fn contain_with(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    reason: Option<&str>,
    kind: Kind,
    hide: bool,
) -> Result<Contained> {
    let member = app.http.guild_member(gid, actor).await?.model().await?;
    if !member.user.bot {
        quarantine_as(app, gid, actor, reason, kind).await?;
        if hide {
            hide_channels_best_effort(app, gid, actor, &member.roles, kind).await;
        }
        return Ok(Contained {
            applied: Applied::Quarantined,
            bot_role: None,
        });
    }
    let role = bot_role(app, gid, actor).await;
    let found = role.as_ref().ok().copied().flatten();
    let done = contain_bot(
        role,
        |(id, _)| async move {
            app.http
                .update_role(gid, id)
                .permissions(Permissions::empty())
                .reason(BOT_ROLE_REASON)
                .await
                .map(drop)
                .map_err(Into::into)
        },
        || quarantine_as(app, gid, actor, reason, kind),
        || async move {
            app.http
                .remove_guild_member(gid, actor)
                .reason(BOT_ROLE_REASON)
                .await
                .map(drop)
                .map_err(Into::into)
        },
    )
    .await?;
    if hide && done.applied == Applied::Quarantined {
        hide_channels_best_effort(app, gid, actor, &member.roles, kind).await;
    }
    // Logged only once the bot stays: a kick deletes its managed role, leaving nothing to restore.
    if let (Applied::Quarantined, Some(true), Some((id, perms))) =
        (done.applied, done.bot_role, found)
    {
        // The bot is already contained; a missing revert row must not report it as free.
        if let Err(e) = record_bot_role(app, gid, id, perms).await {
            tracing::warn!(?e, "logging the cleared bot role failed");
        }
    }
    Ok(done)
}

/// D14, kept apart from [`contain`]'s role steps: a real quarantine also takes the channel access
/// the member has through their own overwrites or a managed role's (booster, bot role), which the
/// quarantine role cannot override. A failure is only logged: the member is already quarantined.
async fn hide_channels_best_effort(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    roles: &[Id<RoleMarker>],
    kind: Kind,
) {
    if matches!(kind, Kind::Join | Kind::Deadline | Kind::Again(true)) {
        return;
    }
    if let Err(e) = hide_channels(app, gid, user, roles).await {
        tracing::warn!(?e, "taking a quarantined member's channel access failed");
        let alert = format!("{NO} **Could not take all channel access from a quarantined member**");
        let log = security_log(
            "Anti Nuke",
            "log.anti_nuke.channel_access_not_taken",
            &[
                ("member", &mention(user.get())),
                ("alert", &alert),
                ("error", &format!("{e:#}")),
            ],
        );
        if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
            tracing::warn!(?e, "channel access failure log failed");
        }
    }
}

async fn hide_channels(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    roles: &[Id<RoleMarker>],
) -> Result<()> {
    let info = app.guild_info(gid).await?;
    let managed: Vec<u64> = roles
        .iter()
        .filter(|r| info.managed.contains(r))
        .map(|r| r.get())
        .collect();
    let mut failed = vec![];
    for c in app.http.guild_channels(gid).await?.models().await? {
        let ows = c.permission_overwrites.unwrap_or_default();
        let Some((bits, own)) = channel_grant(&ows, user.get(), &managed) else {
            continue;
        };
        // One refused channel (deleted mid-nuke, hidden from the bot) must not leave the rest (M2).
        if let Err(e) = deny_member(app, gid, user, c.id, bits, own).await {
            tracing::warn!(
                ?e,
                channel = c.id.get(),
                "hiding a channel from a quarantined member failed"
            );
            failed.push(format!("<#{}>", c.id));
        }
    }
    if failed.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{} channels could not be hidden: {}",
        failed.len(),
        capped(failed.into_iter()).join(", ")
    )
}

/// Puts a member overwrite denying `bits` (and allowing nothing) over a quarantined member in
/// `channel`, after saving their own overwrite `own` for unquarantine. The first save wins: a
/// later call sees the deny an earlier one put there.
async fn deny_member(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    channel: Id<twilight_model::id::marker::ChannelMarker>,
    bits: Permissions,
    own: Option<(Permissions, Permissions)>,
) -> Result<()> {
    let saved = sqlx::query(
        "INSERT INTO quarantine_overwrites (guild_id, user_id, channel_id, had, allow, deny)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
    )
    .bind(gid.get() as i64)
    .bind(user.get() as i64)
    .bind(channel.get() as i64)
    .bind(own.is_some())
    .bind(own.map_or(0, |o| o.0.bits()) as i64)
    .bind(own.map_or(0, |o| o.1.bits()) as i64)
    .execute(&app.pool)
    .await;
    // Containment comes first: the deny goes on even when the save failed.
    let o = PermissionOverwrite {
        id: user.cast(),
        kind: PermissionOverwriteType::Member,
        allow: Some(Permissions::empty()),
        deny: Some(own.map_or(Permissions::empty(), |o| o.1) | bits),
    };
    let put = app.http.update_channel_permission(channel, &o).await;
    if let Err(e) = &saved {
        tracing::warn!(
            ?e,
            channel = channel.get(),
            "saving a quarantined member's overwrite failed"
        );
        let had = match own {
            Some((allow, deny)) => format!(
                "Their own overwrite here was allow `{allow:?}`, deny `{deny:?}`. Set it back by hand on release."
            ),
            None => "They had no own overwrite here. Delete the bot's deny for them by hand on release.".into(),
        };
        let alert = format!("{NO} **A quarantined member's channel overwrite was not saved**");
        let log = security_log(
            "Anti Nuke",
            "log.anti_nuke.overwrite_not_saved",
            &[
                ("member", &mention(user.get())),
                ("alert", &alert),
                ("channel", &format!("<#{channel}>")),
                (
                    "effect",
                    &format!("It will not come back on release. {had}"),
                ),
            ],
        );
        if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
            tracing::warn!(?e, "overwrite save failure log failed");
        }
    }
    put?;
    Ok(())
}

/// What a channel grants `user` through their own overwrite or one of `managed` roles' (None:
/// nothing), and their own overwrite's (allow, deny).
fn channel_grant(
    ows: &[twilight_model::channel::permission_overwrite::PermissionOverwrite],
    user: u64,
    managed: &[u64],
) -> Option<(Permissions, Option<(Permissions, Permissions)>)> {
    use twilight_model::channel::permission_overwrite::PermissionOverwriteType as K;
    let own = ows
        .iter()
        .find(|o| o.kind == K::Member && o.id.get() == user)
        .map(|o| (o.allow, o.deny));
    let bits = ows
        .iter()
        .filter(|o| o.kind == K::Role && managed.contains(&o.id.get()))
        .fold(own.map_or(Permissions::empty(), |o| o.0), |b, o| {
            b | o.allow
        });
    (!bits.is_empty()).then_some((bits, own))
}

/// Puts back the channel overwrites [`hide_channels`] replaced, on unquarantine. A failed one is
/// kept for the next release.
async fn unhide_channels(app: &App, gid: Id<GuildMarker>, user: u64) -> Result<()> {
    let rows: Vec<(i64, bool, i64, i64)> = sqlx::query_as(
        "SELECT channel_id, had, allow, deny FROM quarantine_overwrites WHERE guild_id = $1 AND user_id = $2",
    )
    .bind(gid.get() as i64)
    .bind(user as i64)
    .fetch_all(&app.pool)
    .await?;
    for (channel, had, allow, deny) in rows {
        let ch = Id::new(channel as u64);
        let put = if had {
            let o = PermissionOverwrite {
                id: Id::new(user),
                kind: PermissionOverwriteType::Member,
                allow: Some(Permissions::from_bits_retain(allow as u64)),
                deny: Some(Permissions::from_bits_retain(deny as u64)),
            };
            app.http.update_channel_permission(ch, &o).await.map(drop)
        } else {
            app.http
                .delete_channel_permission(ch)
                .member(Id::new(user))
                .await
                .map(drop)
        };
        if let Err(e) = put.map_err(anyhow::Error::from) {
            if !crate::http_err::gone_any(&e) {
                tracing::warn!(
                    ?e,
                    channel,
                    "restoring a quarantined member's overwrite failed"
                );
                continue;
            }
        }
        sqlx::query(
            "DELETE FROM quarantine_overwrites WHERE guild_id = $1 AND user_id = $2 AND channel_id = $3",
        )
        .bind(gid.get() as i64)
        .bind(user as i64)
        .bind(channel)
        .execute(&app.pool)
        .await?;
    }
    Ok(())
}

/// A bot loses its managed role's permissions, then is quarantined. A failed step, or a failed
/// lookup of its role, ends in a kick.
async fn contain_bot<R, C, Q, K>(
    role: Result<Option<R>>,
    clear: impl FnOnce(R) -> C,
    quarantine: impl FnOnce() -> Q,
    kick: impl FnOnce() -> K,
) -> Result<Contained>
where
    C: Future<Output = Result<()>>,
    Q: Future<Output = Result<()>>,
    K: Future<Output = Result<()>>,
{
    let mut bot_role = None;
    let steps: Result<()> = async {
        if let Some(r) = role? {
            bot_role = Some(false);
            clear(r).await?;
            bot_role = Some(true);
        }
        quarantine().await
    }
    .await;
    let applied = match steps {
        Ok(()) => Applied::Quarantined,
        Err(e) => {
            tracing::warn!(?e, "containing a bot failed, kicking it");
            kick().await?;
            Applied::Kicked
        }
    };
    Ok(Contained { applied, bot_role })
}

/// The bot's own managed role (Discord tags it with the bot's id) and its permissions.
async fn bot_role(
    app: &App,
    gid: Id<GuildMarker>,
    bot: Id<UserMarker>,
) -> Result<Option<(Id<RoleMarker>, u64)>> {
    let roles = app.http.roles(gid).await?.models().await?;
    Ok(roles
        .into_iter()
        .find(|r| r.tags.as_ref().and_then(|t| t.bot_id) == Some(bot))
        .map(|r| (r.id, r.permissions.bits())))
}

/// Logs the bot role's old permissions, so the revert center can put them back.
async fn record_bot_role(
    app: &App,
    gid: Id<GuildMarker>,
    role: Id<RoleMarker>,
    perms: u64,
) -> Result<()> {
    sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse) VALUES ($1,$2,$3,$4,$5)")
        .bind(gid.get() as i64)
        .bind(app.me.get() as i64)
        .bind("bot_role_cleared")
        .bind(role.get() as i64)
        .bind(json!(Inverse::RolePerms { id: role.get(), perms }))
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// A restart mid-revert would leave the nuke half undone: the `nuke_revert` guard job runs it again
/// after a restart, and is dropped once this run is over.
async fn quarantine_and_revert(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    why: &str,
) -> Result<()> {
    let key = (gid.get(), actor.get());
    let Some(token) = claim_nuke_run(app, key) else {
        // Every event past the limit lands here while the first run is busy; its action is
        // logged, and the running revert picks it up before it ends.
        return Ok(());
    };
    let _claim = RunClaim(app, key, token);
    async {
        let since = now_secs() - 3600;
        let payload = json!({"actor": actor.get(), "why": why, "since": since});
        // One guard job per actor: a run that fails (an unread level) keeps it, and the next run
        // takes it over instead of queueing another, with a fresh due time and its lease dropped
        // (a failed heavy job stays leased for 15 minutes). No run of it is mid-way: a job runs
        // only under this claim (`resume_nuke`). A failed write runs without a guard: the
        // containment must not wait for the database.
        let pending: Option<i64> = sqlx::query_scalar(
            "UPDATE scheduled_jobs SET run_at = now() + interval '120 seconds', locked_until = NULL
             WHERE id = (SELECT id FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'nuke_revert'
                         AND payload->>'actor' = $2 ORDER BY id LIMIT 1)
             RETURNING id",
        )
        .bind(gid.get() as i64)
        .bind(actor.get().to_string())
        .fetch_optional(&app.pool)
        .await
        .inspect_err(|e| tracing::warn!(?e, "reading the nuke_revert guard failed"))
        .ok()
        .flatten();
        let guard = match pending {
            Some(id) => Some(id),
            None => jobs::enqueue(
                &app.pool,
                gid.get() as i64,
                "nuke_revert",
                payload,
                now_secs() + 120,
            )
            .await
            .inspect_err(|e| tracing::warn!(?e, "queueing the nuke_revert guard failed"))
            .ok(),
        };
        // An actor an earlier run already quarantined only has late audit entries left: revert
        // them without a second case and owner alert. One the last run could not stop is
        // contained again on each run, but reported once (no case and report per action).
        let full = !still_contained(app, gid, actor).await;
        let report = full && !reported_free(gid, actor);
        let r = nuke_run(app, gid, actor, why, since, (full, report)).await;
        // A failed run keeps its guard, so the job runs it again (M3).
        if let (Ok(()), Some(guard)) = (&r, guard) {
            jobs::finish(&app.pool, guard).await?;
        }
        r
    }
    .await
}

/// Frees a nuke-run claim when the run ends, also when it panics (or the actor stays claimed and
/// its guard job is skipped for good). Only its own claim: `rerun` may have freed it already and
/// a newer run claimed the actor meanwhile.
struct RunClaim<'a>(&'a App, (u64, u64), u64);

impl Drop for RunClaim<'_> {
    fn drop(&mut self) {
        let mut runs = self.0.nuke_runs.lock().unwrap_or_else(|p| p.into_inner());
        release(&mut runs, self.1, self.2);
    }
}

/// The `nuke_revert` guard job after a restart: the cut-off run again, under the same claim, so a
/// live run started meanwhile is not doubled.
pub(crate) async fn resume_nuke(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    why: &str,
    since: i64,
    was_reported: bool,
) -> Result<()> {
    let key = (gid.get(), actor.get());
    let Some(token) = claim_nuke_run(app, key) else {
        return Ok(());
    };
    let _claim = RunClaim(app, key, token);
    // A retry does only what is left: it tries to contain whenever the actor is free (the
    // Contain step asks their level), and reports (and counts towards panic) only when no run
    // reported this nuke yet.
    let contain = !still_contained(app, gid, actor).await;
    let report = !was_reported && !reported(gid, actor);
    nuke_run(app, gid, actor, why, since, (contain, report)).await
}

/// True while a quarantine and revert for this actor runs in this process.
pub(crate) fn nuke_running(app: &App, gid: u64, actor: u64) -> bool {
    app.nuke_runs.lock().unwrap().contains_key(&(gid, actor))
}

/// Quarantined by row and still held in Discord. A row alone is not enough: roles given back by
/// hand (a Quarantine Whitelist user, Quarantine Hold off) leave it behind, and a re-armed nuker
/// must be contained again. A failed read counts as not contained: a second case and alert are
/// better than a nuker left free.
async fn still_contained(app: &App, gid: Id<GuildMarker>, actor: Id<UserMarker>) -> bool {
    let live = async {
        if !quarantined(app, gid, actor).await? {
            return Ok(false);
        }
        // Gone: the row decides, as before.
        let Some(member) = crate::http_err::member(&app.http, gid, actor).await? else {
            return Ok(true);
        };
        let cfg = app.cfg(gid.get()).await?;
        let (q, managed) = quarantine_parts(app, gid, &cfg).await?;
        // A bot's own role cannot be taken off, only cleared.
        let cleared =
            !member.user.bot || bot_role(app, gid, actor).await?.is_none_or(|(_, p)| p == 0);
        anyhow::Ok(held(&member.roles, q, &managed) && cleared)
    };
    live.await.unwrap_or_else(|e| {
        tracing::warn!(?e, "checking a quarantined nuker failed, containing again");
        false
    })
}

/// One claimed run: the full contain, revert, report and panic, or only the revert, then again
/// for every action that came in meanwhile.
async fn nuke_run(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    why: &str,
    since: i64,
    (contain, report): (bool, bool),
) -> Result<()> {
    // Asked on every run, live and retry. Immune: the action counted only because a
    // whitelist lookup failed, so nothing is undone, reported or counted towards panic.
    let level = app.is_immune(gid, actor).await;
    if let Ok(true) = level {
        return immune_log(app, gid, actor).await;
    }
    let mut level = Some(level);
    // An unread level fails the run at the end (the guard job asks again), but the reverts of
    // actions that came in meanwhile still run now.
    let mut unread = None;
    let mut first = contain || report;
    loop {
        let r = if first {
            let level = level.take().unwrap_or(Ok(false));
            match revert_and_report(app, gid, actor, why, since, (contain, report), level).await {
                Err(e) if e.downcast_ref::<LevelUnread>().is_some() => {
                    unread = Some(e);
                    Ok(())
                }
                r => r,
            }
        } else {
            let backups = app.cfg_or_last(gid.get()).await;
            let backups = backups.unwrap_or_else(|(c, _)| c).backups.clone();
            revert_actor(app, gid, actor, since, Some(&backups))
                .await
                .map(|s| {
                    tracing::info!(
                        ops = s.ops,
                        recreated = s.recreated,
                        "reverted the nuker's later actions"
                    )
                })
        };
        if r.is_err() || !nuke_rerun(app, (gid.get(), actor.get())) {
            return r.and(unread.map_or(Ok(()), Err));
        }
        first = false;
    }
}

/// The error a nuke run ends with when the actor's level could not be read: it keeps the guard
/// job, whose retry asks the level again.
#[derive(Debug)]
struct LevelUnread;

impl std::fmt::Display for LevelUnread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the nuker's level could not be read, the containment is tried again")
    }
}

/// Why an unread actor was not contained, in the report and the case line.
const UNREAD: &str = "their level could not be read, so nobody was punished";

/// A nuke run stopped because the actor is immune. At most one log per actor in 10 minutes.
async fn immune_log(app: &App, gid: Id<GuildMarker>, actor: Id<UserMarker>) -> Result<()> {
    if !first_in(
        ("immune_nuke", gid.get(), actor.get()),
        Some(Duration::from_secs(600)),
    ) {
        return Ok(());
    }
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.immune_permits_unread",
        &[
            ("by", &mention(actor.get())),
            (
                "alert",
                "**A limit was reached only because their permits could not be read**",
            ),
        ],
    );
    send_log(app, gid.get(), LogKind::AntiNuke, log).await
}

/// True when no quarantine and revert runs for this (guild, actor) yet; otherwise asks the running
/// one to revert once more.
fn claim_nuke_run(app: &App, key: (u64, u64)) -> Option<u64> {
    claim_run(&mut app.nuke_runs.lock().unwrap(), key)
}

/// True when actions came in during the run (and clears that); false ends the run and frees the
/// claim in the same step, so a later action starts a new run instead of being missed.
fn nuke_rerun(app: &App, key: (u64, u64)) -> bool {
    rerun(&mut app.nuke_runs.lock().unwrap(), key)
}

/// The claim's token (one per run), or None while another run holds the actor.
fn claim_run(runs: &mut NukeRuns, key: (u64, u64)) -> Option<u64> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    match runs.get_mut(&key) {
        Some((_, again)) => {
            *again = true;
            None
        }
        None => {
            let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            runs.insert(key, (token, false));
            Some(token)
        }
    }
}

fn rerun(runs: &mut NukeRuns, key: (u64, u64)) -> bool {
    match runs.get_mut(&key) {
        Some((_, again)) if *again => {
            *again = false;
            true
        }
        _ => {
            runs.remove(&key);
            false
        }
    }
}

/// Frees the claim only while it is still the one `token` took.
fn release(runs: &mut NukeRuns, key: (u64, u64), token: u64) {
    if runs.get(&key).is_some_and(|r| r.0 == token) {
        runs.remove(&key);
    }
}

/// (guild, actor) -> (the running run's token, actions came in meanwhile).
pub(crate) type NukeRuns = HashMap<(u64, u64), (u64, bool)>;

pub(crate) async fn revert_and_report(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    why: &str,
    since: i64,
    (contain, report_it): (bool, bool),
    level: Result<bool>,
) -> Result<()> {
    // Containment comes first: an unreadable config runs the last one loaded, else the defaults
    // (auto quarantine on).
    let (cfg, cfg_err) = match app.cfg_or_last(gid.get()).await {
        Ok(c) => (c, None),
        Err((c, e)) => (c, Some(e)),
    };
    // Not contained here only because an earlier run already did it.
    let mut done = Contained {
        applied: if contain || !cfg.antinuke.auto_quarantine {
            Applied::Off
        } else {
            Applied::Quarantined
        },
        bot_role: None,
    };
    let mut sum = None;
    let mut failed = None;
    let mut unread = None;
    let mut level = Some(level);
    for step in nuke_plan(cfg.antinuke.auto_quarantine) {
        match step {
            NukeStep::Contain if !contain => {}
            NukeStep::Report | NukeStep::Panic if !report_it => {}
            // `level` is the run's own read (`nuke_run`, live and every retry): an unread level
            // reverts but never contains, and the error keeps the guard job, so the
            // retry asks again. A failed quarantine (bot role too low) must not stop the revert.
            NukeStep::Contain => {
                done = match level.take().unwrap_or(Ok(false)) {
                    Ok(true) => Ok(Contained {
                        applied: Applied::Failed("they are immune, so nobody was punished"),
                        bot_role: None,
                    }),
                    Ok(false) => contain_with(app, gid, actor, None, Kind::Real, false).await,
                    Err(e) => {
                        tracing::warn!(?e, "reading a nuker's level failed, not containing");
                        unread = Some(e);
                        Ok(Contained {
                            applied: Applied::Failed(UNREAD),
                            bot_role: None,
                        })
                    }
                }
                .unwrap_or_else(|e| {
                    tracing::warn!(?e, "quarantine failed");
                    let why = if crate::http_err::gone_any(&e) {
                        "they are no longer on the server"
                    } else if crate::http_err::forbidden_any(&e) {
                        "missing permissions: check that the bot's role is on top"
                    } else {
                        "see the bot's logs"
                    };
                    Contained {
                        applied: Applied::Failed(why),
                        bot_role: None,
                    }
                });
                if done.applied == Applied::Quarantined {
                    crate::reply::queue_quarantine_dm(app, gid, actor).await;
                }
                // A run that does not report (an earlier one did) and stops the actor: staff read
                // "not quarantined" before, so they are told, and the next nuke is reported.
                if !report_it && done.stopped() {
                    mark_stopped(gid, actor);
                    let case = match open_case(app, gid, actor, why).await {
                        Ok(c) => format!("`{c}`"),
                        Err(e) => {
                            tracing::warn!(?e, "opening a quarantine case failed");
                            "not saved".into()
                        }
                    };
                    let log = security_log(
                        "Anti Nuke",
                        "log.anti_nuke.contained_by_retry",
                        &[
                            ("case", &case),
                            ("by", &mention(actor.get())),
                            ("alert", &action_text(done)),
                        ],
                    );
                    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
                        tracing::warn!(?e, "late containment log failed");
                    }
                }
            }
            NukeStep::Revert => {
                let r = revert_actor(app, gid, actor, since, Some(&cfg.backups)).await;
                // The overwrite part of the containment, after the channels are back.
                if done.applied == Applied::Quarantined {
                    let m = async {
                        anyhow::Ok(app.http.guild_member(gid, actor).await?.model().await?)
                    };
                    match m.await {
                        Ok(m) => {
                            hide_channels_best_effort(app, gid, actor, &m.roles, Kind::Real).await
                        }
                        Err(e) => tracing::warn!(?e, "reading a nuker to hide channels failed"),
                    }
                }
                // A failed revert still reports and counts towards panic; the error is returned
                // at the end, so the guard job runs the revert again (M3).
                sum = Some(r.unwrap_or_else(|e| {
                    tracing::warn!(?e, "nuke revert failed");
                    failed = Some(e);
                    RevertSummary::default()
                }));
            }
            NukeStep::Report => {
                let sum = sum.as_ref().context("the revert runs before the report")?;
                // A failed report must not keep panic mode from starting.
                if let Err(e) = report(app, gid, actor, why, done, sum).await {
                    tracing::warn!(?e, "anti-nuke report failed");
                }
                // Right after every report, so no retry reports twice.
                mark_reported(app, gid, actor, done.stopped()).await;
                if let Some(e) = &failed {
                    let alert = format!("{NO} **The revert of this nuke failed**");
                    let log = security_log(
                        "Anti Nuke",
                        "log.anti_nuke.revert_failed",
                        &[
                            ("by", &mention(actor.get())),
                            ("alert", &alert),
                            ("reason", &crate::http_err::why(e)),
                        ],
                    );
                    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
                        tracing::warn!(?e, "revert failure log failed");
                    }
                }
                if let Some(e) = &cfg_err {
                    let alert =
                        format!("{NO} **This server's settings could not be read during a nuke**");
                    let log = security_log(
                        "Anti Nuke",
                        "log.anti_nuke.settings_unread_in_nuke",
                        &[
                            ("by", &mention(actor.get())),
                            ("alert", &alert),
                            ("error", &format!("{e:#}")),
                        ],
                    );
                    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
                        tracing::warn!(?e, "config failure log failed");
                    }
                }
            }
            NukeStep::Panic => crate::panic::note(app, gid, actor, false).await?,
        }
    }
    // The config error is logged above; the run itself is done, so the guard job may go.
    if let Some(e) = failed {
        return Err(e.context("the nuke revert failed"));
    }
    unread.map_or(Ok(()), |e| Err(e.context(LevelUnread)))
}

/// (guild, actor) whose nuke was already reported: its guard job's retry does not report again
/// (no second case, incident count, owner DM or panic count).
/// The value: when, and whether that report's run stopped the actor.
type Reports = HashMap<(u64, u64), (Instant, bool)>;
static REPORTED: std::sync::LazyLock<std::sync::Mutex<Reports>> =
    std::sync::LazyLock::new(Default::default);

/// Marks the report done, in memory and on the guard job (which survives a restart). Best effort:
/// a lost mark only means a second report.
async fn mark_reported(app: &App, gid: Id<GuildMarker>, actor: Id<UserMarker>, stopped: bool) {
    {
        let mut all = REPORTED.lock().unwrap_or_else(|p| p.into_inner());
        all.retain(|_, (at, _)| at.elapsed() < HOUR);
        all.insert((gid.get(), actor.get()), (Instant::now(), stopped));
    }
    let r = sqlx::query(
        "UPDATE scheduled_jobs SET payload = payload || '{\"reported\": true}'
         WHERE guild_id = $1 AND kind = 'nuke_revert' AND payload->>'actor' = $2",
    )
    .bind(gid.get() as i64)
    .bind(actor.get().to_string())
    .execute(&app.pool)
    .await;
    if let Err(e) = r {
        tracing::warn!(?e, "marking the nuke as reported failed");
    }
}

fn reported(gid: Id<GuildMarker>, actor: Id<UserMarker>) -> bool {
    let all = REPORTED.lock().unwrap_or_else(|p| p.into_inner());
    all.get(&(gid.get(), actor.get()))
        .is_some_and(|(at, _)| at.elapsed() < HOUR)
}

/// A later run stopped the actor after a report that said it did not.
fn mark_stopped(gid: Id<GuildMarker>, actor: Id<UserMarker>) {
    let mut all = REPORTED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((_, stopped)) = all.get_mut(&(gid.get(), actor.get())) {
        *stopped = true;
    }
}

/// Reported in the last hour by a run that did not stop the actor. A nuker staff released and
/// who nukes again is reported again: their last report said stopped.
fn reported_free(gid: Id<GuildMarker>, actor: Id<UserMarker>) -> bool {
    let all = REPORTED.lock().unwrap_or_else(|p| p.into_inner());
    all.get(&(gid.get(), actor.get()))
        .is_some_and(|(at, stopped)| !stopped && at.elapsed() < HOUR)
}

async fn report(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    why: &str,
    done: Contained,
    sum: &RevertSummary,
) -> Result<()> {
    // Best effort: a database error must not keep the log and the owner DM from going out.
    // Nobody was quarantined on an unread level, so there is no quarantine case.
    let case = match done.applied {
        Applied::Failed(UNREAD) => Ok(None),
        _ => open_case(app, gid, actor, why).await.map(Some),
    };
    let (nuke, saved) = record_nuke(app, gid, actor, &sum.done, done.stopped()).await;
    if let Ok(Some(case)) = case {
        crate::raiders::nuked(app, gid, actor, (case, &nuke, sum.ops), done.stopped()).await;
    }
    let unsaved: Vec<String> = [case.as_ref().err(), saved.as_ref().err()]
        .into_iter()
        .flatten()
        .map(|e| format!("{e:#}"))
        .collect();
    let case = case.ok().flatten();
    let mut log = report_log((case, &nuke), actor.get(), done, why, sum);
    if !unsaved.is_empty() {
        tracing::warn!(?unsaved, "saving the nuke case or record failed");
        log = log.fact(
            "Not saved",
            "not_saved",
            "the case or the nuke record could not be saved; see the bot's logs",
        );
    }
    // A nuker who hid the log channel from the bot must not keep the owner from hearing of it.
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "anti-nuke log failed");
    }
    dm_report(app, gid, (case, &nuke), actor.get(), done, sum).await;
    Ok(())
}

/// The case part of a nuke log and DM: the case, or why there is none.
fn case_line(case: Option<i32>, nuke: &str, done: Contained) -> String {
    match (case, done.applied) {
        (Some(c), _) => format!("`{c}` (nuke `{nuke}`)"),
        (None, Applied::Failed(UNREAD)) => format!("none, nobody was quarantined (nuke `{nuke}`)"),
        (None, _) => format!("not saved (nuke `{nuke}`)"),
    }
}

/// DMs the owner [`nuke_dm`]. Best effort: a failed DM never fails the report.
async fn dm_report(
    app: &App,
    gid: Id<GuildMarker>,
    ids: (Option<i32>, &str),
    actor: u64,
    done: Contained,
    sum: &RevertSummary,
) {
    static LAST: std::sync::Mutex<std::collections::BTreeMap<u64, i64>> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());
    // One DM per server per 60 s, apart from the alert limit: a new nuke case starts only after 600 s quiet (NUKE_GAP), so each case's first report always reaches the owner; later actors' reports are in the log.
    if !crate::reply::alert_due(&mut LAST.lock().unwrap(), gid.get(), now_secs(), 60) {
        return;
    }
    // Takes the alert slot (its result unused): the owner already hears of the attack here.
    crate::reply::alert_due(
        &mut crate::reply::ALERTED.lock().unwrap(),
        gid.get(),
        now_secs(),
        600,
    );
    let owner = match app.bot_owner(gid).await {
        Ok(o) => o,
        Err(e) => return tracing::warn!(?e, "nuke report DM: guild lookup failed"),
    };
    let logs = app
        .cfg(gid.get())
        .await
        .ok()
        .and_then(|c| c.logging.target(LogKind::AntiNuke, None))
        .map(|ch| format!("https://discord.com/channels/{gid}/{ch}"));
    let text = nuke_dm(ids, actor, done, sum, logs.as_deref());
    crate::reply::dm_msg(
        app,
        Some(gid),
        owner,
        "dm.security",
        &[("text", &text)],
        &[],
    )
    .await;
}

/// Most failed steps or re-invites a report names; the rest are counted.
const LIST_CAP: usize = 10;

/// The first [`LIST_CAP`] items, then "and N more".
pub(crate) fn capped(items: impl ExactSizeIterator<Item = String>) -> Vec<String> {
    let more = items.len().saturating_sub(LIST_CAP);
    let mut out: Vec<String> = items.take(LIST_CAP).collect();
    if more > 0 {
        out.push(format!("and {more} more"));
    }
    out
}

/// The failed revert steps, one per line, for the log and the owner's DM.
fn failed_list(failed: &[Inverse]) -> String {
    capped(failed.iter().map(undo_text))
        .iter()
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A revert step in words. No server text (a nuker's server name) goes in.
fn undo_text(op: &Inverse) -> String {
    use brim_logging::embed::{channel, role, user};
    match op {
        Inverse::Unban { user: u } => format!("Unban {}", user(*u)),
        Inverse::DeleteChannel { id } => format!("Delete the new channel {}", channel(*id)),
        Inverse::DeleteRole { id } => format!("Delete the new role {}", role(*id)),
        Inverse::DeleteWebhook { id } => format!("Delete the new webhook `{id}`"),
        Inverse::RestoreRoles { user: u, roles } => {
            format!("Give {} back `{}` roles", user(*u), roles.len())
        }
        Inverse::AddRoles { user: u, roles } => {
            format!(
                "Give {} back `{}` removed roles (not staff or dangerous ones, nor to a quarantined member)",
                user(*u),
                roles.len()
            )
        }
        Inverse::Unquarantine { user: u } => format!("Release {} from quarantine", user(*u)),
        Inverse::Ban { user: u, .. } => format!("Ban {} again", user(*u)),
        Inverse::Timeout { user: u, .. } => format!("Time out {} again", user(*u)),
        Inverse::Reinvite { user: u } => format!("Re-invite {}", user(*u)),
        Inverse::RolePerms { id, .. } => format!("Give {} its permissions back", role(*id)),
        Inverse::DeleteIntegration { id } => format!("Remove the new app `{id}`"),
        Inverse::DeleteEmoji { id } => format!("Delete the new emoji `{id}`"),
        Inverse::DeleteSticker { id } => format!("Delete the new sticker `{id}`"),
        Inverse::DeleteScheduledEvent { id } => format!("Delete the new event `{id}`"),
        Inverse::GuildName { .. } => "Put the server name back".into(),
        Inverse::GuildIcon { .. } => "Put the server icon back".into(),
    }
}

/// The owner's DM after a nuke report: who, what was done to them, what was undone, what failed
/// and what cannot be undone. Empty sections are left out; under 2000 characters.
pub(crate) fn nuke_dm(
    (case, nuke): (Option<i32>, &str),
    actor: u64,
    done: Contained,
    sum: &RevertSummary,
    logs: Option<&str>,
) -> String {
    use brim_logging::emoji::PANIC;
    let mut s = format!(
        "{PANIC} **A nuke attempt was stopped** on your server.\n**Responsible:** {}\n{}\n**Undone:** {} of {} steps",
        brim_logging::embed::user(actor),
        action_text(done),
        sum.ops - sum.failed.len(),
        sum.ops
    );
    if sum.recreated > 0 {
        s.push_str(&format!("\n**Recreated from backup:** {}", sum.recreated));
    }
    if !sum.failed.is_empty() {
        s.push_str(&format!(
            "\n**Could not undo** (use `/revert` to try again):\n{}",
            failed_list(&sum.failed)
        ));
    }
    let mut never = vec![];
    if !sum.reinvite.is_empty() {
        let who = capped(sum.reinvite.iter().map(|u| format!("<@{u}>")));
        never.push(format!("Kicked, re-invite them: {}", who.join(" ")));
    }
    if sum.missing > 0 {
        never.push(format!(
            "`{}` deleted channels or roles did not come back: no backup holds them",
            sum.missing
        ));
    }
    if sum.stripped > 0 {
        never.push(format!(
            "`{}` recreated roles or channels came from an older backup without dangerous permissions: give them back by hand if they should have them",
            sum.stripped
        ));
    }
    if !never.is_empty() {
        s.push_str(&format!(
            "\n**Cannot be undone:**\n- {}",
            never.join("\n- ")
        ));
    }
    s.push_str(&format!("\nCase {}", case_line(case, nuke, done)));
    if let Some(l) = logs {
        s.push_str(&format!("\nDetails: {l}"));
    }
    s
}

/// What happened to the nuker, as the log's action line.
fn action_text(done: Contained) -> String {
    match (done.applied, done.bot_role) {
        (Applied::Quarantined, Some(true)) => format!("{YES} **Quarantined**, bot role cleared"),
        (Applied::Quarantined, _) => format!("{YES} **Quarantined**"),
        (Applied::Kicked, _) => format!("{YES} **Kicked** (the bot could not be quarantined)"),
        (Applied::Failed(why), _) => format!("{NO} **NOT quarantined** ({why})"),
        (Applied::Off, _) => format!("{NO} **Not quarantined** (auto quarantine is off)"),
    }
}

/// The log after a nuke is stopped and reverted.
pub(crate) fn report_log(
    (case, nuke): (Option<i32>, &str),
    actor: u64,
    done: Contained,
    why: &str,
    sum: &RevertSummary,
) -> Log {
    let action = action_text(done);
    let reinvite = if sum.reinvite.is_empty() {
        "None".into()
    } else {
        capped(sum.reinvite.iter().map(|u| format!("<@{u}>"))).join(" ")
    };
    let mark = |b: bool| if b { YES } else { NO }.to_string();
    let given = |b: bool, s: String| if b { s } else { String::new() };
    Log::new("Anti Nuke Report:", ALERT)
        .entry("log.anti_nuke.report")
        .fact("CASE", "case", &case_line(case, nuke, done))
        .user_fact("Responsible", "by", actor)
        .fact_text("alert", &action)
        .fact("Reason", "reason", why)
        .section("More Details:")
        .fact("Action Applied", "applied", &mark(done.stopped()))
        .fact("Bot Role Cleansing", "bot_role", &done.bot_role.map_or(String::new(), mark))
        .section("Revert Report:")
        .fact(
            "Reverted",
            "reverted",
            &format!("`{}/{}`", sum.ops - sum.failed.len(), sum.ops),
        )
        .fact("Recreated", "recreated", &format!("`{}`", sum.recreated))
        .fact("Kicked, re-invite", "reinvite", &reinvite)
        .fact(
            "Could not undo",
            "not_undone",
            &given(!sum.failed.is_empty(), format!("\n{}", failed_list(&sum.failed))),
        )
        .fact(
            "Not recreated",
            "not_recreated",
            &given(sum.missing > 0, format!(
                "`{}` deleted channels or roles did not come back: no backup holds them. Turn on Backups so anti-nuke can recreate them.",
                sum.missing
            )),
        )
        .fact(
            "Older backup",
            "older_backup",
            &given(sum.stripped > 0, format!(
                "`{}` recreated roles or channels came from an older backup and got no dangerous permissions (role permissions or channel overwrite allows). Give them back by hand if they should have them.",
                sum.stripped
            )),
        )
        // Set by the caller when the case could not be saved.
        .var("not_saved", "")
}

/// A quiet gap this long starts a new nuke case instead of extending the last one.
pub(crate) const NUKE_GAP: Duration = Duration::from_secs(600);

/// Adds the quarantined actor and their reverted ops to the guild's open nuke case and saves it.
async fn record_nuke(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    ops: &[Inverse],
    contained: bool,
) -> (String, Result<()>) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
        ^ gid.get();
    let case = {
        let mut open = app.nukes.lock().unwrap();
        let entry = open
            .entry(gid.get())
            .or_insert_with(|| (NukeCase::new(seed), Instant::now()));
        if entry.1.elapsed() > NUKE_GAP {
            *entry = (NukeCase::new(seed), Instant::now());
        }
        entry.1 = Instant::now();
        entry.0.add(actor.get(), ops.iter().cloned(), contained);
        entry.0.clone()
    };
    let saved = sqlx::query(
        "INSERT INTO nuke_incidents (guild_id, case_id, data) VALUES ($1,$2,$3)
         ON CONFLICT (guild_id, case_id) DO UPDATE SET data = $3",
    )
    .bind(gid.get() as i64)
    .bind(&case.id)
    .bind(sqlx::types::Json(&case))
    .execute(&app.pool)
    .await;
    // The id stays: the next upsert inside NUKE_GAP saves the case under it.
    (case.id, saved.map(drop).map_err(Into::into))
}

const REVERT_REASON: &str = "Anti-nuke revert";

/// The removed roles a revert gives back: none to a member who is quarantined now, and never a
/// role with dangerous permissions or a permit; those are left to staff, as the step says.
async fn safe_to_give_back(
    app: &App,
    guild: i64,
    user: u64,
    roles: &[u64],
    risky: &[Id<RoleMarker>],
    permit: &[(i64, bool)],
) -> Result<Vec<u64>> {
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM quarantines WHERE guild_id = $1 AND user_id = $2)",
    )
    .bind(guild)
    .bind(user as i64)
    .fetch_one(&app.pool)
    .await?;
    if held {
        return Ok(vec![]);
    }
    Ok(roles
        .iter()
        .copied()
        .filter(|r| !risky.contains(&Id::new(*r)) && !permit.iter().any(|p| p.0 == *r as i64))
        .collect())
}

pub(crate) async fn run_inverse(app: &App, gid: Id<GuildMarker>, op: &Inverse) -> Result<()> {
    match op {
        Inverse::Unban { user } => {
            app.http.delete_ban(gid, Id::new(*user)).await?;
            crate::scheduler::forget_jobs(app, gid, *user, &["unban"], None).await;
        }
        Inverse::DeleteChannel { id } => {
            app.http.delete_channel(Id::new(*id)).await?;
        }
        Inverse::DeleteRole { id } => {
            app.http.delete_role(gid, Id::new(*id)).await?;
        }
        Inverse::DeleteWebhook { id } => {
            app.http.delete_webhook(Id::new(*id)).await?;
        }
        Inverse::RestoreRoles { user, roles } => {
            let roles: Vec<Id<RoleMarker>> = roles.iter().map(|r| Id::new(*r)).collect();
            app.http
                .update_guild_member(gid, Id::new(*user))
                .roles(&roles)
                .await?;
        }
        Inverse::AddRoles { user, roles } => {
            let g = gid.get() as i64;
            let ids: Vec<Id<RoleMarker>> = roles.iter().map(|r| Id::new(*r)).collect();
            let wide: Vec<i64> = roles.iter().map(|r| *r as i64).collect();
            let risky = dangerous_given(app, gid, &ids).await?;
            let permit = permit_roles_in(&app.pool, g, &wide).await?;
            let mut last = None;
            for r in safe_to_give_back(app, g, *user, roles, &risky, &permit).await? {
                // One deleted role must not keep the others from coming back.
                if let Err(e) = app
                    .http
                    .add_guild_member_role(gid, Id::new(*user), Id::new(r))
                    .reason(REVERT_REASON)
                    .await
                {
                    tracing::warn!(?e, r, "giving a removed role back failed");
                    last = Some(e);
                }
            }
            if let Some(e) = last {
                return Err(e.into());
            }
        }
        Inverse::Unquarantine { user } => {
            let saved: Option<(Vec<i64>,)> = sqlx::query_as(
                "SELECT saved_roles FROM quarantines WHERE guild_id=$1 AND user_id=$2",
            )
            .bind(gid.get() as i64)
            .bind(*user as i64)
            .fetch_optional(&app.pool)
            .await?;
            if let Some((saved,)) = saved {
                let roles: Vec<Id<RoleMarker>> = saved.iter().map(|r| Id::new(*r as u64)).collect();
                app.http
                    .update_guild_member(gid, Id::new(*user))
                    .roles(&roles)
                    .await?;
                sqlx::query("DELETE FROM quarantines WHERE guild_id=$1 AND user_id=$2")
                    .bind(gid.get() as i64)
                    .bind(*user as i64)
                    .execute(&app.pool)
                    .await?;
                // The saved roles can predate a permit change: the sync puts the permit roles right.
                let sync =
                    jobs::enqueue_once(&app.pool, gid.get() as i64, "permit_sync", now_secs());
                if let Err(e) = sync.await {
                    tracing::warn!(?e, "queueing the permit sync failed");
                }
            }
            // Released: their own channel access comes back too.
            if let Err(e) = unhide_channels(app, gid, *user).await {
                tracing::warn!(?e, "restoring a released member's overwrites failed");
            }
        }
        // A temp ban comes back with its old end; one that would have ended by now is not redone.
        Inverse::Ban { user, until } => {
            if until.is_some_and(|t| t <= now_secs()) {
                return Ok(());
            }
            let mut job = None;
            if let Some(t) = until {
                let p = serde_json::json!({ "user": user });
                job = Some(jobs::enqueue(&app.pool, gid.get() as i64, "unban", p, *t).await?);
            }
            // Queued first so a crash never leaves a temp ban without its end; a failed ban drops
            // the job again, or it would lift a later permanent ban (R-h3 3, as ops::run_inverse).
            if let Err(e) = app.http.create_ban(gid, Id::new(*user)).await {
                if let Some(id) = job {
                    sqlx::query("DELETE FROM scheduled_jobs WHERE id = $1")
                        .bind(id)
                        .execute(&app.pool)
                        .await?;
                }
                return Err(e.into());
            }
            crate::scheduler::forget_jobs(app, gid, *user, crate::scheduler::BANNED, job).await;
        }
        Inverse::Timeout { user, until } => {
            crate::commands::timeout_until(app, gid, Id::new(*user), *until, "Restored timeout")
                .await?;
        }
        Inverse::RolePerms { id, perms } => {
            app.http
                .update_role(gid, Id::new(*id))
                .permissions(Permissions::from_bits_retain(*perms))
                .await?;
        }
        Inverse::DeleteIntegration { id } => remove_app(app, gid, *id).await?,
        Inverse::DeleteEmoji { id } => {
            app.http
                .delete_emoji(gid, Id::new(*id))
                .reason(REVERT_REASON)
                .await?;
        }
        Inverse::DeleteSticker { id } => {
            app.http
                .delete_guild_sticker(gid, Id::new(*id))
                .reason(REVERT_REASON)
                .await?;
        }
        Inverse::DeleteScheduledEvent { id } => {
            app.http
                .delete_guild_scheduled_event(gid, Id::new(*id))
                .await?; // twilight takes no audit reason here
        }
        // Only when the live name differs. `from` (a row as stored, not settled by
        // `revert_actor`): only while the server still has the name the entry set.
        Inverse::GuildName { name, from } => {
            let live = app.http.guild(gid).await?.model().await?.name;
            if live == *name || !brim_antinuke::still_set(from, &live) {
                return Ok(());
            }
            app.http
                .update_guild(gid)
                .name(name)
                .reason(REVERT_REASON)
                .await?;
        }
        Inverse::GuildIcon { hash, from } => {
            let live = app
                .http
                .guild(gid)
                .await?
                .model()
                .await?
                .icon
                .map(|h| h.to_string());
            if live == *hash || !brim_antinuke::still_set(from, &live) {
                return Ok(());
            }
            let icon = match hash {
                Some(h) => Some(
                    brim_automod::discord_api::stored_icon(
                        &app.pool,
                        app.api.client(),
                        gid.get(),
                        h,
                    )
                    .await?,
                ),
                None => None,
            };
            app.http
                .update_guild(gid)
                .icon(icon.as_deref())
                .reason(REVERT_REASON)
                .await?;
        }
        // Discord cannot undo a kick, so this is reported for re-invite, not executed.
        Inverse::Reinvite { .. } => {}
    }
    Ok(())
}

/// The role ids an entry adds.
fn roles_added(e: &AuditLogEntry) -> impl Iterator<Item = Id<RoleMarker>> + '_ {
    e.changes
        .iter()
        .filter_map(|c| match c {
            AuditLogChange::RoleAdded { new, .. } => Some(new),
            _ => None,
        })
        .flatten()
        .map(|r| r.id)
}

/// Someone changed a quarantined member's roles: quarantine them too (if `punish`) and undo the
/// change.
async fn hold(app: &App, e: &AuditLogEntry, actor: Id<UserMarker>, punish: bool) -> Result<()> {
    let (Some(gid), Some(target)) = (e.guild_id, e.target_id) else {
        return Ok(());
    };
    let target = target.cast::<UserMarker>();
    let Some(on_join) = quarantine_kind(app, gid, target).await? else {
        return Ok(());
    };
    let cfg = app.cfg(gid.get()).await?;
    let (whitelisted, unsure) = hold_whitelisted(app, &cfg, gid, actor, target, on_join).await;
    if !hold_violation(actor.get(), target.get(), true, whitelisted) {
        return Ok(());
    }
    // A join quarantine keeps the joiner held, but an autorole bot or staff touching them is
    // normal work, not a nuke (owner decision, 11-11).
    if on_join {
        if e.action_type == A::MemberRoleUpdate {
            // Q2 (owner decision 11-14): a verify permit verifies by hand, as at the button.
            let v = &cfg.verification;
            if v.enabled
                && roles_added(e).any(|r| v.roles.contains(&r))
                && crate::verify::given::may_verify(app, gid, actor).await
            {
                // A failed grant falls back to the undo: the member stays held, not half verified.
                // The guard reads the quarantine again: a real one since the read above holds.
                match crate::verify::self_grant(app, gid, target, v).await {
                    Ok(()) => return Ok(()),
                    Err(err) => tracing::warn!(?err, "verifying by hand failed, undoing"),
                }
            }
            undo_role_change(app, &cfg, gid, target).await;
            if !punish {
                let names: Vec<String> = roles_added(e).map(|r| format!("<@&{r}>")).collect();
                let log = security_log(
                    "Anti Nuke",
                    "log.anti_nuke.unverified_roles_taken",
                    &[
                        ("by", &mention(actor.get())),
                        ("member", &mention(target.get())),
                        (
                            "alert",
                            "**Gave roles to a member who has not verified yet**",
                        ),
                        ("roles", &names.join(", ")),
                    ],
                );
                return send_log(app, gid.get(), LogKind::AntiNuke, log).await;
            }
        }
        return Ok(());
    }
    // Switched off, nobody is punished for touching a quarantined member (owner decision, 11-12).
    if !cfg.antinuke.quarantine_hold {
        return Ok(());
    }
    // A kick, ban or other member change cannot be undone: with anti-nuke off there is nothing to
    // do, and an "undone" log would be false.
    if !punish && e.action_type != A::MemberRoleUpdate {
        return Ok(());
    }
    if unsure {
        if e.action_type == A::MemberRoleUpdate {
            undo_role_change(app, &cfg, gid, target).await;
        }
        return unsure_log(
            app,
            gid,
            actor,
            Some(target),
            "touched a quarantined member",
        )
        .await;
    }
    if e.action_type != A::MemberRoleUpdate {
        let what = "touched a quarantined member";
        return quarantine_and_revert(app, gid, actor, what).await;
    }
    // An autorole bot right after a rejoin's quarantine: undone, not punished (as 11-11).
    // Any number of bots and changes in the grace, but only plain roles: a dangerous or permit
    // role is punished as before.
    if rejoined_recently(gid, target)
        && is_bot(app, actor).await
        && plain_roles_added(app, gid, e).await
    {
        undo_role_change(app, &cfg, gid, target).await;
        return Ok(());
    }
    if !punish {
        undo_role_change(app, &cfg, gid, target).await;
        let what = "changed the roles of a quarantined member";
        return undone_log(app, gid, actor, Some(target), what, OFF).await;
    }
    // A failed undo must not spare the actor, nor wait for its retry.
    let undone = back_to_quarantine(app, &cfg, gid, target).await;
    let r =
        quarantine_and_revert(app, gid, actor, "changed the roles of a quarantined member").await;
    if let Err(err) = undone {
        tracing::warn!(?err, "undoing a quarantined member's role change failed");
        undo_role_change(app, &cfg, gid, target).await;
    }
    r
}

/// A member overwrite that gives a quarantined member channel access is the hold's matter too
///: undone, and the actor quarantined as for a role change. A join quarantine only undoes.
/// Lifting the quarantine's own deny (clearing its bits or deleting the overwrite) is a grant too.
pub(crate) async fn hold_overwrite(
    app: &App,
    e: &AuditLogEntry,
    actor: Id<UserMarker>,
    punish: bool,
) -> Result<()> {
    let (Some(gid), Some(channel), Some(o)) = (e.guild_id, e.target_id, e.options.as_ref()) else {
        return Ok(());
    };
    // A create has no old values and a delete no new ones: both read as empty.
    let bits = |allow: bool| {
        e.changes.iter().find_map(|c| match c {
            AuditLogChange::Allow { old, new } if allow => Some((*old, *new)),
            AuditLogChange::Deny { old, new } if !allow => Some((*old, *new)),
            _ => None,
        })
    };
    let empty = Permissions::empty();
    let allow = bits(true);
    let (old_allow, new_allow) = allow.unwrap_or((None, None));
    let (old_deny, new_deny) = bits(false).map_or((empty, empty), |(o, n)| {
        (o.unwrap_or(empty), n.unwrap_or(empty))
    });
    let gained = new_allow.unwrap_or(empty) - old_allow.unwrap_or(empty);
    // Removing a deny grants as much as adding an allow (a delete removes all of it).
    let lost = old_deny - new_deny;
    if gained.is_empty() && lost.is_empty() {
        return Ok(());
    }
    let user = match (o.id, o.kind.as_deref()) {
        (Some(user), Some("1")) => user.cast::<UserMarker>(),
        (Some(role), Some("0")) => {
            return hold_role_overwrite(app, gid, channel.cast(), role.cast(), gained | lost).await
        }
        _ => return Ok(()),
    };
    // A member's lost deny matters only where the quarantine put it (`quarantine_overwrites`).
    if gained.is_empty() && !hid_channel(app, gid, user, channel.get()).await? {
        return Ok(());
    }
    let Some(on_join) = quarantine_kind(app, gid, user).await? else {
        return Ok(());
    };
    let cfg = app.cfg(gid.get()).await?;
    let (whitelisted, unsure) = hold_whitelisted(app, &cfg, gid, actor, user, on_join).await;
    if !hold_violation(actor.get(), user.get(), true, whitelisted) {
        return Ok(());
    }
    let undo = async {
        let live = app.http.channel(channel.cast()).await?.model().await?;
        let now = live
            .permission_overwrites
            .unwrap_or_default()
            .into_iter()
            .find(|o| o.id.get() == user.get());
        // The allow as it was, and the lost deny back on top of what is there now.
        let o = PermissionOverwrite {
            id: user.cast(),
            kind: PermissionOverwriteType::Member,
            allow: Some(match allow {
                Some((old, _)) => old.unwrap_or(empty),
                None => now.as_ref().map_or(empty, |o| o.allow),
            }),
            deny: Some(now.map_or(empty, |o| o.deny) | lost),
        };
        app.http
            .update_channel_permission(channel.cast(), &o)
            .await?;
        anyhow::Ok(())
    };
    // A failed undo must not spare the actor, and staff are told.
    let failed = undo.await.err();
    if let Some(err) = &failed {
        tracing::warn!(?err, "undoing a quarantined member's overwrite failed");
        let alert =
            format!("{NO} **Could not take back channel access given to a quarantined member**");
        let log = security_log(
            "Anti Nuke",
            "log.anti_nuke.overwrite_not_taken_back",
            &[
                ("by", &mention(actor.get())),
                ("member", &mention(user.get())),
                ("alert", &alert),
                ("channel", &format!("<#{channel}>")),
                ("error", &crate::http_err::why(err)),
            ],
        );
        if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
            tracing::warn!(?e, "failed undo log failed");
        }
    }
    if on_join || !cfg.antinuke.quarantine_hold {
        return Ok(());
    }
    let what = "gave a quarantined member channel access";
    // After a failed undo, an "undone" log would be false; the failure log above says it.
    if (unsure || !punish) && failed.is_some() {
        return Ok(());
    }
    if unsure {
        return unsure_log(app, gid, actor, Some(user), what).await;
    }
    if !punish {
        return undone_log(app, gid, actor, Some(user), what, OFF).await;
    }
    quarantine_and_revert(app, gid, actor, what).await
}

/// Whether the hold spares `actor` (quarantine whitelist), and whether that could not be read.
/// Unread, the hold undoes but punishes nobody: a lookup error must not quarantine staff (#97),
/// nor leave the quarantined member what they were given (owner decision: undo, not punish).
async fn hold_whitelisted(
    app: &App,
    cfg: &GuildCfg,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    target: Id<UserMarker>,
    on_join: bool,
) -> (bool, bool) {
    // A Trusted Admin is not exempt from a quarantine (owner decision 11-17, option A): only an Owner or
    // Extra Owner gives a quarantined member roles back. A join quarantine (an unverified joiner)
    // still lets a Trusted Admin through, as do a permit's `quarantine` flag and the quarantine
    // whitelist.
    // Below Extra Owner, the flag and the whitelist spare the actor only for a member who held no
    // staff roles: staff go free only through an Owner or Extra Owner, as `unquarantine` asks.
    let listed = async {
        let level = app.cached_level(gid, actor, None).await?;
        if matches!(level, permits::Level::Owner | permits::Level::ExtraOwner) {
            return anyhow::Ok(true);
        }
        if !(app.quarantine_listed(cfg, gid, actor).await? || hold_exempt(&level, on_join)) {
            return Ok(false);
        }
        let saved: Option<Vec<i64>> = sqlx::query_scalar(
            "SELECT saved_roles FROM quarantines WHERE guild_id = $1 AND user_id = $2",
        )
        .bind(gid.get() as i64)
        .bind(target.get() as i64)
        .fetch_optional(&app.pool)
        .await?;
        let owner = app.guild_info(gid).await?.owner;
        Ok(
            !held_staff(app, gid, owner, target, &saved.unwrap_or_default())
                .await?
                .1,
        )
    };
    match listed.await {
        Ok(w) => (w, false),
        Err(e) => {
            tracing::warn!(
                ?e,
                "quarantine whitelist lookup failed, undoing without punishment"
            );
            (false, true)
        }
    }
}

/// The level a quarantined member's saved `roles` give back, and whether that makes them staff: a
/// permit, or a role with dangerous permissions (judged as before a role lock stripped them).
pub(crate) async fn held_staff(
    app: &App,
    gid: Id<GuildMarker>,
    owner: Id<UserMarker>,
    target: Id<UserMarker>,
    roles: &[i64],
) -> Result<(permits::Level, bool)> {
    let member = crate::http_err::member(&app.http, gid, target).await?;
    let held = permits::resolve(
        &app.pool,
        gid.get() as i64,
        owner.get() as i64,
        target.get() as i64,
        member.is_some_and(|m| m.user.bot),
    )
    .await?;
    let perms = crate::lockdown::unlocked_role_perms(app, gid).await?;
    let dangerous = roles.iter().any(|r| {
        Id::new_checked(*r as u64)
            .and_then(|r| perms.get(&r))
            .is_some_and(|p| p & brim_antinuke::DANGEROUS_PERMS != 0)
    });
    // `resolve` gives a quarantined member no permit at all, so the member's own rows are read here.
    let permit: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM permit_members WHERE guild_id = $1 AND user_id = $2)",
    )
    .bind(gid.get() as i64)
    .bind(target.get() as i64)
    .fetch_one(&app.pool)
    .await?;
    let staff = held != permits::Level::None || permit || dangerous;
    Ok((held, staff))
}

/// The level half of the quarantine hold's exemption: Owner, Extra Owner, or a permit with the
/// `quarantine` whitelist flag. A Trusted Admin only for a join quarantine.
fn hold_exempt(level: &permits::Level, on_join: bool) -> bool {
    (on_join || !matches!(level, permits::Level::TrustedAdmin)) && level.whitelisted("quarantine")
}

/// A guarded change undone without punishment, because the actor's permits could not be read.
async fn unsure_log(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    target: Option<Id<UserMarker>>,
    what: &str,
) -> Result<()> {
    let why = "undone where it can be; not quarantined: their permits could not be read";
    undone_log(app, gid, actor, target, what, why).await
}

/// The Action line of a guarded change undone with anti-nuke off.
const OFF: &str = "undone where it can be. Anti-nuke is off, so nobody was punished";

/// A guarded change undone without punishment, `action` saying why.
async fn undone_log(
    app: &App,
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    target: Option<Id<UserMarker>>,
    what: &str,
    action: &str,
) -> Result<()> {
    let member = target.map_or(String::new(), |t| mention(t.get()));
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.change_undone",
        &[
            ("by", &mention(actor.get())),
            ("member", &member),
            ("alert", &format!("**{what}**")),
            ("action", action),
        ],
    );
    send_log(app, gid.get(), LogKind::AntiNuke, log).await
}

/// Owner decision 2026-11-15: an app added by anyone but an Owner, Extra Owner or Trusted Admin is
/// removed and logged; nobody is quarantined for one add (the app-add limit still counts it).
async fn guard_app_add(app: &App, e: &AuditLogEntry, actor: Id<UserMarker>) -> Result<()> {
    let (Some(gid), Some(bot)) = (e.guild_id, e.target_id) else {
        return Ok(());
    };
    if bot.cast() == app.me {
        return Ok(());
    }
    // Unread: removed anyway, as for any change only those levels may make.
    if app.is_immune(gid, actor).await.unwrap_or(false) {
        return Ok(());
    }
    let action = match app
        .http
        .remove_guild_member(gid, bot.cast())
        .reason("Anti-nuke: only an Owner, Extra Owner or Trusted Admin may add apps")
        .await
    {
        // Gone already: the join gate's bot filter may have kicked it first.
        Ok(_) => "removed: only an Owner, Extra Owner or Trusted Admin may add apps".to_string(),
        Err(e) if crate::http_err::gone(&e) => {
            "removed: only an Owner, Extra Owner or Trusted Admin may add apps".to_string()
        }
        Err(e) => format!("could not remove it ({e}). Remove it in Server Settings > Integrations"),
    };
    undone_log(app, gid, actor, Some(bot.cast()), "Added an app", &action).await
}

/// Removes the app behind integration `id`; see [`brim_antinuke::app_removal`].
async fn remove_app(app: &App, gid: Id<GuildMarker>, id: u64) -> Result<()> {
    let list = app.http.guild_integrations(gid).await?.models().await?;
    let list = list.into_iter().map(|i| {
        let bot = i.application.and_then(|a| a.bot).map(|b| b.id.get());
        (i.id.get(), bot)
    });
    match brim_antinuke::app_removal(id, list) {
        brim_antinuke::AppRemoval::Gone => Ok(()),
        // Never itself: an IntegrationCreate in the attack window may be Brim coming back.
        brim_antinuke::AppRemoval::KickBot(bot) if bot == app.me.get() => Ok(()),
        brim_antinuke::AppRemoval::KickBot(bot) => {
            match app
                .http
                .remove_guild_member(gid, Id::new(bot))
                .reason(REVERT_REASON)
                .await
            {
                Err(e) if !crate::http_err::gone(&e) => Err(e.into()),
                _ => Ok(()),
            }
        }
        brim_antinuke::AppRemoval::Manual => anyhow::bail!(
            "this app has no bot user, so only a person can remove it in Server Settings > Integrations"
        ),
    }
}

/// Whether the quarantine put a deny on `user`'s overwrite in `channel` (`hide_channels`).
async fn hid_channel(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    channel: u64,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM quarantine_overwrites WHERE guild_id = $1 AND user_id = $2 AND channel_id = $3)",
    )
    .bind(gid.get() as i64)
    .bind(user.get() as i64)
    .bind(channel as i64)
    .fetch_one(&app.pool)
    .await?)
}

/// A managed role's overwrite (booster, a bot's role) that now grants more in a channel: the
/// members it holds who are really quarantined get a member deny over those bits, as at contain
///. Nobody is punished: staff set up booster channels as normal work.
async fn hold_role_overwrite(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Id<twilight_model::id::marker::ChannelMarker>,
    role: Id<RoleMarker>,
    gained: Permissions,
) -> Result<()> {
    if !app.guild_info(gid).await?.managed.contains(&role) {
        return Ok(());
    }
    // A quarantined member keeps their managed roles, and the saved list holds them.
    let held: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM quarantines WHERE guild_id = $1 AND NOT on_join AND $2 = ANY(saved_roles)",
    )
    .bind(gid.get() as i64)
    .bind(role.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    if held.is_empty() {
        return Ok(());
    }
    let ows = app
        .http
        .channel(channel)
        .await?
        .model()
        .await?
        .permission_overwrites
        .unwrap_or_default();
    for u in held {
        let user = Id::new(u as u64);
        let own = ows
            .iter()
            .find(|o| o.id.get() == user.get())
            .map(|o| (o.allow, o.deny));
        if let Err(e) = deny_member(app, gid, user, channel, gained, own).await {
            tracing::warn!(
                ?e,
                user = u,
                "denying a quarantined member a role's new access failed"
            );
        }
    }
    Ok(())
}

/// Sets a quarantined member's roles back to what [`quarantine_roles`] leaves.
async fn back_to_quarantine(
    app: &App,
    cfg: &GuildCfg,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
) -> Result<()> {
    let member = app.http.guild_member(gid, user).await?.model().await?;
    let (q, managed) = quarantine_parts(app, gid, cfg).await?;
    let roles = quarantine_roles(q, &member.roles, &managed);
    app.http
        .update_guild_member(gid, user)
        .roles(&roles)
        .await?;
    Ok(())
}

/// Undoes a role change on a held member without punishing anyone. Discord failing is tried once
/// more after 2 s, then told to staff in the anti-nuke log.
async fn undo_role_change(app: &App, cfg: &GuildCfg, gid: Id<GuildMarker>, user: Id<UserMarker>) {
    let Err(e) = back_to_quarantine(app, cfg, gid, user).await else {
        return;
    };
    tracing::warn!(
        ?e,
        "undoing a quarantined member's role change failed, trying again"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let Err(e) = back_to_quarantine(app, cfg, gid, user).await else {
        return;
    };
    tracing::warn!(
        ?e,
        "undoing a quarantined member's role change failed twice"
    );
    let alert = format!("{NO} **Could not take back roles given to a quarantined member**");
    let log = security_log(
        "Anti Nuke",
        "log.anti_nuke.roles_not_taken_back",
        &[
            ("member", &mention(user.get())),
            ("alert", &alert),
            ("error", &crate::http_err::why(&e)),
        ],
    );
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "failed undo log failed");
    }
}

/// How long after a rejoin's quarantine a bot's role change counts as an autorole.
const REJOIN_GRACE: Duration = Duration::from_secs(60);

type Rejoins = std::sync::Mutex<HashMap<(u64, u64), Instant>>;
static REJOINED: std::sync::LazyLock<Rejoins> = std::sync::LazyLock::new(Default::default);

/// Notes a quarantine put back on a rejoin. In memory: after a restart the grace is gone and the
/// bot is quarantined as before, which fails safe.
fn note_rejoin(gid: Id<GuildMarker>, user: Id<UserMarker>) {
    let mut all = REJOINED.lock().unwrap_or_else(|p| p.into_inner());
    all.retain(|_, at| at.elapsed() < REJOIN_GRACE);
    all.insert((gid.get(), user.get()), Instant::now());
}

fn rejoined_recently(gid: Id<GuildMarker>, user: Id<UserMarker>) -> bool {
    let all = REJOINED.lock().unwrap_or_else(|p| p.into_inner());
    all.get(&(gid.get(), user.get()))
        .is_some_and(|at| at.elapsed() < REJOIN_GRACE)
}

/// The entry adds only roles an autorole gives: no dangerous permissions (as held before a role
/// lock, read live) and no permit. A failed lookup or an unknown role counts as not plain.
async fn plain_roles_added(app: &App, gid: Id<GuildMarker>, e: &AuditLogEntry) -> bool {
    let added: Vec<Id<RoleMarker>> = e
        .changes
        .iter()
        .filter_map(|c| match c {
            AuditLogChange::RoleAdded { new, .. } => Some(new),
            _ => None,
        })
        .flatten()
        .map(|r| r.id)
        .collect();
    let check = async {
        // Read live: a role created a moment ago is not in the cached guild info.
        let live = app.http.roles(gid).await?.models().await?;
        let live = live.iter().map(|r| (r.id, r.permissions.bits())).collect();
        let perms = crate::lockdown::unlocked(app, gid, live).await?;
        let permit = permits::permit_roles(&app.pool, gid.get() as i64).await?;
        anyhow::Ok(added.iter().all(|r| {
            !permit.contains(&(r.get() as i64))
                && perms
                    .get(r)
                    .is_some_and(|p| p & brim_antinuke::DANGEROUS_PERMS == 0)
        }))
    };
    check.await.unwrap_or(false)
}

/// A bot account; a failed lookup counts as no bot, so the hold punishes as before.
pub(crate) async fn is_bot(app: &App, user: Id<UserMarker>) -> bool {
    async { anyhow::Ok(app.http.user(user).await?.model().await?.bot) }
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nuker_left_free_is_reported_once_and_a_stopped_one_again() {
        let (gid, actor) = (Id::new(9_123_001), Id::new(9_123_002));
        let mark = |stopped| {
            REPORTED
                .lock()
                .unwrap()
                .insert((gid.get(), actor.get()), (Instant::now(), stopped))
        };
        assert!(!reported_free(gid, actor));
        mark(false);
        assert!(
            reported_free(gid, actor),
            "a failed containment is not reported again"
        );
        mark(true);
        assert!(
            !reported_free(gid, actor),
            "released and nuking again: a new report"
        );
        assert!(reported(gid, actor));
    }

    /// R1: past the limit a fight is still reverted, once a minute per (guild, actor, member);
    /// the member update path shares that minute.
    #[test]
    fn a_stopped_fight_still_reverts_once_a_minute() {
        let mut m = Fights::new();
        let (k, t0) = ((1, 2, 3), Instant::now());
        for i in 1..=FIGHT_MAX {
            assert_eq!(fight_count(&mut m, k, t0), (i, true));
        }
        let at = |s| t0 + Duration::from_secs(s);
        assert_eq!(fight_count(&mut m, k, at(1)), (FIGHT_MAX + 1, false));
        assert!(fight_held(&mut m, 1, 3, at(30)));
        assert!(!fight_held(&mut m, 1, 4, at(30)), "another member");
        assert!(!fight_held(&mut m, 1, 3, at(61)), "a minute on: reverted");
        assert!(fight_held(&mut m, 1, 3, at(62)));
        assert!(!fight_count(&mut m, k, at(100)).1);
        assert!(fight_count(&mut m, k, at(122)).1);
        assert!(!fight_count(&mut m, k, at(123)).1);
    }

    #[test]
    fn nuke_dm_caps_lists_and_leaves_empty_sections_out() {
        let quarantined = Contained {
            applied: Applied::Quarantined,
            bot_role: None,
        };
        let mut sum = RevertSummary {
            ops: 3,
            done: vec![],
            failed: vec![],
            reinvite: vec![],
            recreated: 0,
            stripped: 0,
            missing: 0,
        };
        let calm = nuke_dm((Some(7), "abc123"), 42, quarantined, &sum, None);
        assert!(calm.contains("**Undone:** 3 of 3 steps"), "{calm}");
        assert!(calm.contains("Quarantined") && calm.contains("<@42>"));
        assert!(calm.contains("Case `7` (nuke `abc123`)"));
        for gone in [
            "Recreated",
            "Could not undo",
            "Cannot be undone",
            "Details:",
        ] {
            assert!(!calm.contains(gone), "{gone} in {calm}");
        }
        // The worst case: long ids everywhere, many failed steps and kicks.
        let big = u64::MAX;
        sum.ops = 200;
        sum.failed = (0..100)
            .map(|_| Inverse::RestoreRoles {
                user: big,
                roles: vec![1; 300],
            })
            .collect();
        sum.reinvite = vec![big; 100];
        (sum.missing, sum.stripped, sum.recreated) = (50, 50, 999);
        let logs = format!("https://discord.com/channels/{big}/{big}");
        let full = nuke_dm(
            (Some(i32::MAX), "abc123"),
            big,
            quarantined,
            &sum,
            Some(&logs),
        );
        assert!(full.contains("**Undone:** 100 of 200 steps"));
        assert!(full.contains("**Recreated from backup:** 999"));
        assert_eq!(full.matches("Give <@").count(), LIST_CAP, "{full}");
        assert!(full.contains("- and 90 more"));
        assert!(full.contains("re-invite them:") && full.contains(" and 90 more\n"));
        assert!(full.contains("`50` deleted") && full.contains("`50` recreated roles"));
        assert!(full.ends_with(&format!("Details: {logs}")));
        assert!(
            full.chars().count() < 2000,
            "{} chars",
            full.chars().count()
        );
        // The log lists the same failed steps.
        let log = report_log((Some(7), "abc123"), 42, quarantined, "test", &sum).build();
        let text = log.description.unwrap_or_default();
        assert!(text.contains(&failed_list(&sum.failed)), "{text}");
    }

    #[test]
    fn one_nuke_run_per_actor_and_late_actions_rerun_it() {
        let mut runs = HashMap::new();
        let k = (1, 2);
        let first = claim_run(&mut runs, k).unwrap();
        assert!(claim_run(&mut runs, k).is_none()); // a second event while it runs
        assert!(claim_run(&mut runs, (1, 3)).is_some()); // another actor runs on its own
        assert!(rerun(&mut runs, k)); // that event is reverted once more
        assert!(!rerun(&mut runs, k)); // nothing new: the claim is freed
        let second = claim_run(&mut runs, k).unwrap(); // so a later action starts a new run
                                                       // The first run frees its claim after `jobs::finish`: the second run's claim stays.
        release(&mut runs, k, first);
        assert!(claim_run(&mut runs, k).is_none());
        release(&mut runs, k, second);
        assert!(claim_run(&mut runs, k).is_some());
    }

    #[test]
    fn quarantine_keeps_only_the_quarantine_role_and_managed_roles() {
        let id = |n| Id::<RoleMarker>::new(n);
        let got = quarantine_roles(Some(id(9)), &[id(1), id(2), id(3)], &[id(2), id(7)]);
        assert_eq!(got, vec![id(9), id(2)]);
        assert!(quarantine_roles(None, &[id(1)], &[]).is_empty());
    }

    #[test]
    fn held_means_only_what_quarantine_leaves() {
        let id = |n| Id::<RoleMarker>::new(n);
        let (q, managed) = (Some(id(9)), [id(2)]);
        assert!(held(&[id(9), id(2)], q, &managed));
        assert!(held(&[id(9)], q, &managed));
        assert!(
            !held(&[id(9), id(1)], q, &managed),
            "a role given back by hand"
        );
        assert!(
            !held(&[id(2)], q, &managed),
            "the quarantine role taken off"
        );
        assert!(!held(&[], q, &managed));
        // No (or a deleted) quarantine role: stripped to managed roles is held.
        assert!(held(&[id(2)], None, &managed));
        assert!(!held(&[id(1)], None, &managed));
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn quarantine_rows_keep_their_kind_and_saved_roles() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let key = (6_200_000_000 + std::process::id() as i64, 42);
        let row = |u: i64| {
            let pool = &pool;
            async move {
                sqlx::query_as::<_, (Vec<i64>, bool)>(
                    "SELECT saved_roles, on_join FROM quarantines WHERE guild_id = $1 AND user_id = $2",
                )
                .bind(key.0)
                .bind(u)
                .fetch_optional(pool)
                .await
                .unwrap()
            }
        };
        let (long_ago, later) = (
            DateTime::from_timestamp(1, 0),
            DateTime::from_timestamp(4e9 as i64, 0),
        );
        // A join quarantine; a real one on top of it wins and keeps the saved list.
        let first = save_row(&pool, key, (&[], true, None)).await.unwrap();
        assert!(first.inserted && first.old.is_none());
        let real = save_row(&pool, key, (&[9], false, None)).await.unwrap();
        assert_eq!(row(42).await, Some((vec![], false)));
        // A failed role change puts the join quarantine back.
        restore_row(&pool, key, real).await.unwrap();
        assert_eq!(row(42).await, Some((vec![], true)));
        // A join quarantine never turns a real one back into a join one.
        save_row(&pool, key, (&[], false, None)).await.unwrap();
        save_row(&pool, key, (&[], true, None)).await.unwrap();
        assert_eq!(row(42).await, Some((vec![], false)));
        // Holding other roles in a membership from after the quarantine (a rejoin with
        // autoroles): the saved list stays.
        save_row(&pool, key, (&[7], false, later)).await.unwrap();
        assert_eq!(row(42).await, Some((vec![], false)));
        // Released by hand in the same membership, then quarantined again: today's roles.
        let again = save_row(&pool, key, (&[7, 8], false, long_ago))
            .await
            .unwrap();
        assert_eq!(row(42).await, Some((vec![7, 8], false)));
        // A quarantine written meanwhile is not undone by a failed one.
        sqlx::query("UPDATE quarantines SET saved_roles = '{5}' WHERE guild_id = $1")
            .bind(key.0)
            .execute(&pool)
            .await
            .unwrap();
        restore_row(&pool, key, again).await.unwrap();
        assert_eq!(row(42).await, Some((vec![5], false)));
        // A failed first quarantine leaves no row, unless another wrote it meanwhile.
        let k2 = (key.0, 43);
        let made = save_row(&pool, k2, (&[1], false, None)).await.unwrap();
        restore_row(&pool, k2, made).await.unwrap();
        assert_eq!(row(43).await, None);
        let made = save_row(&pool, k2, (&[1], true, None)).await.unwrap();
        save_row(&pool, k2, (&[1], false, None)).await.unwrap();
        restore_row(&pool, k2, made).await.unwrap();
        assert_eq!(
            row(43).await,
            Some((vec![1], false)),
            "a real one is never deleted"
        );
        // An identical write meanwhile is a new version too: the failed one leaves it.
        let first = save_row(&pool, k2, (&[1], false, None)).await.unwrap();
        save_row(&pool, k2, (&[1], false, None)).await.unwrap();
        restore_row(&pool, k2, first).await.unwrap();
        assert_eq!(row(43).await, Some((vec![1], false)));
        sqlx::query("DELETE FROM quarantines WHERE guild_id = $1")
            .bind(key.0)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Runs the bot steps with fake calls; `fail` names the call that errors.
    async fn bot_run(role: Result<Option<u8>>, fail: &str) -> (Contained, Vec<&'static str>) {
        let calls = std::cell::RefCell::new(vec![]);
        let call = |name: &'static str| {
            calls.borrow_mut().push(name);
            if name == fail {
                anyhow::bail!("{name} failed")
            }
            Ok(())
        };
        let done = contain_bot(
            role,
            |_| std::future::ready(call("clear")),
            || std::future::ready(call("quarantine")),
            || std::future::ready(call("kick")),
        )
        .await
        .unwrap();
        (done, calls.into_inner())
    }

    #[tokio::test]
    async fn a_bot_is_kicked_only_when_clearing_or_quarantine_fails() {
        let c = |applied, bot_role| Contained { applied, bot_role };
        assert_eq!(
            bot_run(Ok(Some(1)), "").await,
            (
                c(Applied::Quarantined, Some(true)),
                vec!["clear", "quarantine"]
            )
        );
        assert_eq!(
            bot_run(Ok(None), "").await,
            (c(Applied::Quarantined, None), vec!["quarantine"])
        );
        assert_eq!(
            bot_run(Ok(Some(1)), "clear").await,
            (c(Applied::Kicked, Some(false)), vec!["clear", "kick"])
        );
        assert_eq!(
            bot_run(Ok(Some(1)), "quarantine").await,
            (
                c(Applied::Kicked, Some(true)),
                vec!["clear", "quarantine", "kick"]
            )
        );
        // The bot's role could not be read: it may still hold it, so it is kicked.
        assert_eq!(
            bot_run(Err(anyhow::anyhow!("rate limited")), "").await,
            (c(Applied::Kicked, None), vec!["kick"])
        );
    }

    #[test]
    fn only_dangerous_roles_below_the_bot_are_taken() {
        let id = |n| Id::<RoleMarker>::new(n);
        let perms = HashMap::from([(id(1), 1 << 3), (id(2), 1 << 10), (id(3), 1 << 3)]);
        let positions = HashMap::from([(id(1), 2), (id(2), 3), (id(3), 9)]);
        let got = dangerous_below_bot(&[id(1), id(2), id(3), id(4)], &perms, &positions, 5);
        assert_eq!(got, vec![id(1)]);
    }

    /// The dashboard's Rule coverage table reads `Action::undo`; it must say what `inverse` does.
    #[test]
    fn undo_matches_inverse() {
        let e: AuditLogEntry = serde_json::from_value(json!({
            "action_type": 1, "changes": [{"key": "name", "old_value": "a", "new_value": "b"}],
            "id": "1", "user_id": "2", "target_id": "3"
        }))
        .unwrap();
        for a in Action::ALL {
            let undone = inverse(a, &e)
                .iter()
                .any(|i| !matches!(i, Inverse::Reinvite { .. }));
            let want = a.undo() == brim_antinuke::Undo::Yes;
            assert_eq!(undone, want, "{a:?}");
        }
    }

    /// security review (option A): only an Owner or Extra Owner, or a `quarantine` permit, may give a
    /// quarantined member roles back; a Trusted Admin only for a join quarantine.
    #[test]
    fn a_trusted_admin_is_held_by_a_quarantine() {
        use permits::Level;
        let permit = |w: &str| Level::Custom {
            rank: 1,
            perms: vec![],
            whitelist: vec![w.into()],
            limits: Default::default(),
        };
        for (l, quarantine, join) in [
            (Level::Owner, true, true),
            (Level::ExtraOwner, true, true),
            (Level::TrustedAdmin, false, true),
            (permit("quarantine"), true, true),
            (permit("automod"), false, false),
            (Level::None, false, false),
        ] {
            assert_eq!(hold_exempt(&l, false), quarantine, "{l:?}");
            assert_eq!(hold_exempt(&l, true), join, "{l:?}");
        }
    }

    #[test]
    fn timeouts_and_role_renames_are_watched_other_updates_not() {
        let entry = |t: u16, changes: serde_json::Value| -> AuditLogEntry {
            serde_json::from_value(json!({
                "action_type": t, "changes": changes, "id": "1", "user_id": "2", "target_id": "3"
            }))
            .unwrap()
        };
        let timeout = json!([{"key": "communication_disabled_until", "new_value": "2030-01-01T00:00:00+00:00"}]);
        let lifted = json!([{"key": "communication_disabled_until", "old_value": "2030-01-01T00:00:00+00:00"}]);
        let rename = json!([{"key": "name", "old_value": "a", "new_value": "b"}]);
        assert_eq!(watched(&entry(24, timeout)), Some(Action::Timeout));
        assert_eq!(watched(&entry(24, lifted)), None);
        assert_eq!(watched(&entry(31, rename)), Some(Action::RoleUpdate));
        assert_eq!(watched(&entry(31, json!([]))), None);
        assert_eq!(watched(&entry(23, json!([]))), Some(Action::Unban));
        // A role taken from a member counts (heat only) and is given back; a role given does not.
        let taken = entry(
            25,
            json!([{"key": "$remove", "new_value": [{"id": "9", "name": "r"}]}]),
        );
        assert_eq!(watched(&taken), Some(Action::RoleRemove));
        assert_eq!(
            inverse(Action::RoleRemove, &taken),
            [Inverse::AddRoles {
                user: 3,
                roles: vec![9]
            }]
        );
        let given = json!([{"key": "$add", "new_value": [{"id": "9", "name": "r"}]}]);
        assert_eq!(watched(&entry(25, given)), None);
    }

    #[test]
    fn new_triggers_are_watched_and_undone() {
        let entry = |t: u16, changes: serde_json::Value| -> AuditLogEntry {
            serde_json::from_value(json!({
                "action_type": t, "changes": changes, "id": "1", "user_id": "2", "target_id": "3"
            }))
            .unwrap()
        };
        for (t, a, inv) in [
            (
                80,
                Action::IntegrationCreate,
                vec![Inverse::DeleteIntegration { id: 3 }],
            ),
            (82, Action::IntegrationDelete, vec![]),
            (
                60,
                Action::EmojiCreate,
                vec![Inverse::DeleteEmoji { id: 3 }],
            ),
            (
                90,
                Action::StickerCreate,
                vec![Inverse::DeleteSticker { id: 3 }],
            ),
            (
                100,
                Action::ScheduledEventCreate,
                vec![Inverse::DeleteScheduledEvent { id: 3 }],
            ),
            (132, Action::SoundboardDelete, vec![]),
            (112, Action::ThreadDelete, vec![]),
        ] {
            let e = entry(t, json!([]));
            assert_eq!(watched(&e), Some(a), "{t}");
            assert_eq!(inverse(a, &e), inv, "{t}");
        }
        // Soundboard create and update are not watched. Voice moves and disconnects are counted
        // only by `voice_nuke`, so a merged entry is never counted by two paths.
        for t in [130, 26, 27] {
            assert_eq!(watched(&entry(t, json!([]))), None, "{t}");
        }
        let hash = "0123456789abcdef0123456789abcdef";
        let both = entry(
            1,
            json!([{"key": "name", "old_value": "Home", "new_value": "Nuked"},
                   {"key": "icon_hash", "old_value": hash, "new_value": format!("a_{hash}")}]),
        );
        assert_eq!(watched(&both), Some(Action::GuildUpdate));
        assert_eq!(
            inverse(Action::GuildUpdate, &both),
            vec![
                Inverse::GuildName {
                    name: "Home".into(),
                    from: Some("Nuked".into()),
                },
                Inverse::GuildIcon {
                    hash: Some(hash.into()),
                    from: Some(Some(format!("a_{hash}"))),
                }
            ]
        );
        // An icon added where there was none is removed again.
        let added = entry(1, json!([{"key": "icon_hash", "new_value": hash}]));
        assert_eq!(
            inverse(Action::GuildUpdate, &added),
            vec![Inverse::GuildIcon {
                hash: None,
                from: Some(Some(hash.into())),
            }]
        );
        // Other server settings (here the vanity URL) are not this card.
        let vanity = entry(
            1,
            json!([{"key": "vanity_url_code", "old_value": "a", "new_value": "b"}]),
        );
        assert_eq!(watched(&vanity), None);
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. A row the revert did not read
    /// (here one without an inverse, as an action logged while it ran) stays unreverted.
    #[tokio::test]
    async fn revert_marks_only_the_rows_it_read() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let app = App::new(
            twilight_http::Client::new(String::new()),
            twilight_http::Client::new(String::new()),
            pool,
            brim_automod::discord_api::Api::new(""),
            Id::new(1),
        );
        let gid = Id::new(6_400_000_000 + std::process::id() as u64);
        let actor = Id::new(42);
        for (action, inv) in [
            ("Kick", Some(json!(Inverse::Reinvite { user: 7 }))),
            ("ChannelUpdate", None),
        ] {
            sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse) VALUES ($1,$2,$3,7,$4)")
                .bind(gid.get() as i64)
                .bind(actor.get() as i64)
                .bind(action)
                .bind(inv)
                .execute(&app.pool)
                .await
                .unwrap();
        }
        let sum = revert_actor(&app, gid, actor, now_secs() - 60, None)
            .await
            .unwrap();
        assert_eq!(sum.reinvite, vec![7]);
        let left: Vec<(String,)> = sqlx::query_as(
            "SELECT action FROM action_log WHERE guild_id=$1 AND NOT reverted ORDER BY id",
        )
        .bind(gid.get() as i64)
        .fetch_all(&app.pool)
        .await
        .unwrap();
        assert_eq!(left, vec![("ChannelUpdate".to_string(),)]);
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. Reverting the bot (`/revert
    /// actor:@Brim`) never lifts its quarantines or gives a nuking bot its role back.
    #[tokio::test]
    async fn reverting_the_bot_keeps_its_containments() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let app = App::new(
            twilight_http::Client::new(String::new()),
            twilight_http::Client::new(String::new()),
            pool,
            brim_automod::discord_api::Api::new(""),
            Id::new(1),
        );
        let gid = Id::new(6_460_000_000 + std::process::id() as u64);
        for (action, inv) in [
            ("quarantine", json!(Inverse::Unquarantine { user: 7 })),
            (
                "bot_role_cleared",
                json!(Inverse::RolePerms { id: 8, perms: 8 }),
            ),
            ("Kick", json!(Inverse::Reinvite { user: 9 })),
        ] {
            sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse) VALUES ($1,1,$2,7,$3)")
                .bind(gid.get() as i64)
                .bind(action)
                .bind(inv)
                .execute(&app.pool)
                .await
                .unwrap();
        }
        let sum = revert_actor(&app, gid, app.me, now_secs() - 60, None)
            .await
            .unwrap();
        assert_eq!((sum.ops, sum.failed.len(), sum.reinvite), (0, 0, vec![9]));
        let left: Vec<(String,)> = sqlx::query_as(
            "SELECT action FROM action_log WHERE guild_id=$1 AND NOT reverted ORDER BY id",
        )
        .bind(gid.get() as i64)
        .fetch_all(&app.pool)
        .await
        .unwrap();
        let left: Vec<_> = left.into_iter().map(|r| r.0).collect();
        assert_eq!(left, ["quarantine", "bot_role_cleared"]);
        sqlx::query("DELETE FROM action_log WHERE guild_id=$1")
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await
            .unwrap();
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. A restart keeps the anti-nuke
    /// window, the permit command limit and heat panic (N3).
    #[tokio::test]
    async fn windows_survive_a_restart() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let app = App::new(
            twilight_http::Client::new(String::new()),
            twilight_http::Client::new(String::new()),
            pool,
            brim_automod::discord_api::Api::new(""),
            Id::new(1),
        );
        let g = 6_480_000_000 + std::process::id() as i64;
        // A merged entry counted twice, a second inverse row of one entry, and a case row.
        for (action, n) in [("Ban", 2), ("Ban", 0), ("ban", 1)] {
            sqlx::query(
                "INSERT INTO action_log (guild_id, actor_id, action, counted) VALUES ($1,42,$2,$3)",
            )
            .bind(g)
            .bind(action)
            .bind(n)
            .execute(&app.pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO command_uses (guild_id, actor_id, action) VALUES ($1, $1, 'ban')")
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
        // A row from before 4005 has no guild: skipped, not counted anywhere.
        sqlx::query("INSERT INTO command_uses (actor_id, action) VALUES ($1, 'kick')")
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO heat_panic (guild_id, raiders, until) VALUES ($1, '{42}', $2)")
            .bind(g)
            .bind(now_secs() + 60)
            .execute(&app.pool)
            .await
            .unwrap();
        seed_windows(&app).await.unwrap();
        let limit = brim_antinuke::Limit {
            per_minute: 3,
            per_hour: 10,
        };
        let v = {
            let mut an = app.antinuke.lock().unwrap();
            an.limits = HashMap::from([(Action::Ban, limit)]);
            an.on_action(g as u64, 42, Action::Kick, false, Instant::now())
        };
        assert!(
            matches!(v, Verdict::Quarantine(c) if c.minute == 3),
            "{v:?}"
        );
        let one = permits::Limit {
            per_min: 1,
            per_hour: 5,
        };
        let allowed = app
            .rate_limiter
            .lock()
            .unwrap()
            .allow(g, g, "ban", one, Instant::now());
        assert!(!allowed, "the use before the restart counts");
        {
            let mut rl = app.rate_limiter.lock().unwrap();
            let now = Instant::now();
            assert!(rl.allow(g + 1, g, "ban", one, now), "another server's use");
            assert!(
                rl.allow(g, g, "kick", one, now),
                "an old row without a guild"
            );
        }
        assert!(crate::panic::is_raider(
            &app,
            Id::new(g as u64),
            Id::new(42)
        ));
        for t in ["action_log", "heat_panic"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {t} WHERE guild_id = $1"
            )))
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
        }
        sqlx::query("DELETE FROM command_uses WHERE actor_id = $1")
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn only_roles_with_a_permit_are_guarded() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let g = 6_490_000_000 + std::process::id() as i64;
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO permits (guild_id, name, rank, perms) VALUES ($1, 'Mods', 1, '{ban}') RETURNING id",
        )
        .bind(g)
        .fetch_one(&pool)
        .await
        .unwrap();
        let (weak,): (i64,) = sqlx::query_as(
            "INSERT INTO permits (guild_id, name, rank, whitelist) VALUES ($1, 'Links', 1, '{links}') RETURNING id",
        )
        .bind(g)
        .fetch_one(&pool)
        .await
        .unwrap();
        // The D16 attack: one narrow anti-nuke whitelist is enough to guard the role.
        let (narrow,): (i64,) = sqlx::query_as(
            "INSERT INTO permits (guild_id, name, rank, whitelist) VALUES ($1, 'Deleters', 1, '{wl_channel_delete}') RETURNING id",
        )
        .bind(g)
        .fetch_one(&pool)
        .await
        .unwrap();
        for (p, r) in [(id, 7), (weak, 8), (weak, 7), (narrow, 9)] {
            sqlx::query(
                "INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1, $2, $3)",
            )
            .bind(p)
            .bind(g)
            .bind(r)
            .execute(&pool)
            .await
            .unwrap();
        }
        // A level role (Trusted Admin, Owner) is always guarded.
        sqlx::query(
            "INSERT INTO level_roles (guild_id, level, role_id) VALUES ($1, 'trusted_admin', 6)",
        )
        .bind(g)
        .execute(&pool)
        .await
        .unwrap();
        let mut got = permit_roles_in(&pool, g, &[5, 6, 7, 8, 9]).await.unwrap();
        got.sort();
        // 7 is guarded through its ban permit; 8 carries only a link whitelist.
        assert_eq!(got, vec![(6, true), (7, true), (8, false), (9, true)]);
        // Only 9's permit has an anti-nuke whitelist: a Trusted Admin may not hand that one out.
        assert_eq!(owner_only_roles(&pool, g, &got).await.unwrap(), vec![9]);
        assert!(permit_roles_in(&pool, g + 1, &[7])
            .await
            .unwrap()
            .is_empty());
        sqlx::query("DELETE FROM permits WHERE guild_id = $1")
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM level_roles WHERE guild_id = $1")
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Review 11-14 (G): one take-back log per (guild, actor, role set) per 10 minutes, the next
    /// one counting the repeats.
    #[test]
    fn take_back_logs_are_throttled_with_a_count() {
        let mut m = TakeBacks::new();
        let t0 = Instant::now();
        let key = || (1, 2, vec![3]);
        assert_eq!(take_back_log_due(&mut m, key(), t0), Some(None));
        assert_eq!(take_back_log_due(&mut m, key(), t0), None);
        assert_eq!(take_back_log_due(&mut m, (1, 2, vec![4]), t0), Some(None));
        assert_eq!(
            take_back_log_due(&mut m, key(), t0 + Duration::from_secs(1)),
            None
        );
        let later = t0 + TAKE_BACK_LOG_EVERY;
        assert_eq!(take_back_log_due(&mut m, key(), later), Some(Some((2, t0))));
        assert_eq!(take_back_log_due(&mut m, key(), later), None);
    }

    /// Q6: the owner is DMed once, at the fifth take-back by one (guild, actor), whatever roles.
    #[test]
    fn the_owner_dm_comes_once_after_five_take_backs() {
        let mut m = OwnerDms::default();
        let due: Vec<bool> = (0..7).map(|_| owner_dm_due(&mut m, (1, 2))).collect();
        assert_eq!(due, [false, false, false, false, true, false, false]);
        assert!(!owner_dm_due(&mut m, (1, 3)), "another actor counts alone");
        // The size cap clears the counts, never the sent ones.
        m.counts.extend((0..10_001).map(|i| ((9, i), 1)));
        assert!(!owner_dm_due(&mut m, (9, 0)));
        assert!((0..5).all(|_| !owner_dm_due(&mut m, (1, 2))), "never again");
    }

    #[test]
    fn only_a_replayed_entry_has_an_age() {
        // An entry from 2015: live, it still counts now (a fast host clock changes nothing).
        assert_eq!(replay_age(1 << 22, false), Duration::ZERO);
        assert!(replay_age(1 << 22, true) > HOUR);
    }

    #[test]
    fn an_unread_config_never_changes_the_quarantine_row() {
        assert_eq!(row_flags(Kind::Real, true, true), (false, true));
        assert_eq!(row_flags(Kind::Real, false, true), (false, false));
        assert_eq!(row_flags(Kind::Deadline, true, true), (false, false));
        assert_eq!(row_flags(Kind::Deadline, false, true), (true, false));
        assert_eq!(row_flags(Kind::Again(true), false, true), (true, false));
    }

    #[test]
    fn changes_to_locked_targets_are_found() {
        use brim_automod::lockdown::SILENCE;
        // A Role lock saved role 5 with 2048 | 8 (Send, Administrator) and took Administrator;
        // a channel lock on 9 saved its overwrite of role 5 as allow 64, deny 1024.
        let lock: brim_automod::lockdown::Snapshot = serde_json::from_value(json!({
            "kind": "Role", "role_perms": [[5, 2056]], "channel_denies": [[9, 5]],
            "join_lock": false, "invites_paused": false, "prior": [[9, 5, 64 | 2048, 1024]]
        }))
        .unwrap();
        let entry = |t: u16, target: &str, changes: serde_json::Value| -> AuditLogEntry {
            serde_json::from_value(json!({
                "action_type": t, "changes": changes, "id": "1", "user_id": "2",
                "target_id": target, "options": {"id": "5", "type": "0"}
            }))
            .unwrap()
        };
        // M1: two quick edits each set back to the lock's value, never to the first edit.
        for (old, new) in [("2048", "2056"), ("2056", "2064")] {
            let perms = json!([{"key": "permissions", "old_value": old, "new_value": new}]);
            assert_eq!(
                lock_target(&entry(31, "5", perms), std::slice::from_ref(&lock)),
                Some(Locked::Role {
                    id: 5,
                    to: Permissions::from_bits_retain(2056 & !lock.takes())
                })
            );
        }
        let perms = json!([{"key": "permissions", "old_value": "0", "new_value": "2048"}]);
        assert_eq!(
            lock_target(&entry(31, "6", perms), std::slice::from_ref(&lock)),
            None
        );
        // A channel-only lock saved Administrator (8) and takes none; staff removed it, then
        // someone changed the role: the set-back must not give Administrator back.
        let server: brim_automod::lockdown::Snapshot = serde_json::from_value(json!({
            "kind": "Server", "role_perms": [[5, 2056]], "channel_denies": [],
            "join_lock": false, "invites_paused": false
        }))
        .unwrap();
        let perms = json!([{"key": "permissions", "old_value": "0", "new_value": "2048"}]);
        assert_eq!(
            lock_target(&entry(31, "5", perms), std::slice::from_ref(&server)),
            Some(Locked::Role {
                id: 5,
                to: Permissions::empty()
            })
        );
        let want = Some(Locked::Overwrite {
            channel: 9,
            role: 5,
            allow: Permissions::from_bits_retain((64 | 2048) & !SILENCE),
            deny: Permissions::from_bits_retain(1024 | SILENCE),
        });
        for (old, new) in [("3072", "64"), ("64", "0")] {
            let deny = json!([{"key": "deny", "old_value": old, "new_value": new}]);
            assert_eq!(
                lock_target(&entry(14, "9", deny), std::slice::from_ref(&lock)),
                want
            );
        }
        let deny = json!([{"key": "deny", "old_value": "2048", "new_value": "0"}]);
        assert_eq!(
            lock_target(&entry(15, "9", deny.clone()), std::slice::from_ref(&lock)),
            want,
            "a deleted overwrite is written again"
        );
        assert_eq!(
            lock_target(&entry(14, "8", deny.clone()), std::slice::from_ref(&lock)),
            None
        );
        assert_eq!(lock_target(&entry(14, "9", deny), &[]), None, "no lock");
        // The lock saved allow Manage Channels (16); staff took it away since. A set-back must
        // not give it back, unless the entry's old allow still had it.
        let mut managed = lock.clone();
        managed.prior = vec![(9, 5, 16 | 65536, 1024)];
        let allow_of = |old: &str| match lock_target(
            &entry(
                14,
                "9",
                json!([{"key": "allow", "old_value": old, "new_value": "8256"}]),
            ),
            std::slice::from_ref(&managed),
        ) {
            Some(Locked::Overwrite { allow, .. }) => allow.bits(),
            other => panic!("{other:?}"),
        };
        assert_eq!(allow_of("65536"), 65536 & !SILENCE);
        assert_eq!(allow_of("65552"), 65552 & !SILENCE);
    }

    #[test]
    fn channel_access_of_a_quarantined_member_is_found() {
        use twilight_model::channel::permission_overwrite::{
            PermissionOverwrite as Ow, PermissionOverwriteType as K,
        };
        let ow = |id: u64, kind, allow: u64, deny: u64| Ow {
            allow: Permissions::from_bits_retain(allow),
            deny: Permissions::from_bits_retain(deny),
            id: Id::new(id),
            kind,
        };
        let p = Permissions::from_bits_retain;
        // Their own allow, a managed role's allow, and an unrelated role's.
        let ows = [
            ow(7, K::Member, 1024, 2),
            ow(3, K::Role, 2048, 0),
            ow(4, K::Role, 8, 0),
        ];
        assert_eq!(
            channel_grant(&ows, 7, &[3]),
            Some((p(1024 | 2048), Some((p(1024), p(2)))))
        );
        assert_eq!(channel_grant(&ows[2..], 7, &[3]), None);
        assert_eq!(channel_grant(&ows[1..], 7, &[3]), Some((p(2048), None)));
        // A member overwrite that only denies grants nothing.
        assert_eq!(channel_grant(&[ow(7, K::Member, 0, 1024)], 7, &[]), None);
    }

    #[test]
    fn full_heat_contains_like_a_limit() {
        let c = brim_antinuke::Counts { minute: 3, hour: 3 };
        let allow = Verdict::Allow(c);
        assert_eq!(breach(&allow, false, Action::Ban), None);
        assert_eq!(
            breach(&allow, true, Action::Ban).as_deref(),
            Some("anti-nuke heat reached 100%")
        );
        assert!(breach(&Verdict::Quarantine(c), true, Action::Ban)
            .unwrap()
            .starts_with("Ban limit reached"));
    }

    #[test]
    fn only_carried_out_ops_count_as_undone() {
        let ban = Inverse::Unban { user: 7 };
        let chan = Inverse::DeleteChannel { id: 5 };
        let name = |n: &str| Inverse::GuildName {
            name: n.into(),
            from: Some("x".into()),
        };
        let settled = Inverse::GuildName {
            name: "orig".into(),
            from: None,
        };
        let ops = [ban.clone(), settled.clone()];
        assert!(undone(&ban, &ops, &[]));
        assert!(!undone(&ban, &ops, std::slice::from_ref(&ban)), "failed");
        assert!(!undone(&chan, &ops, &[]), "filtered out");
        assert!(
            undone(&name("a"), &ops, &[]),
            "merged into the settled rename"
        );
        assert!(!undone(&name("a"), &ops, &[settled]));
        assert!(undone(&Inverse::Reinvite { user: 7 }, &[], &[]));
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. A filtered op and a deleted
    /// channel that no backup can bring back stay unreverted, for a later `/revert`.
    #[tokio::test]
    async fn revert_leaves_what_it_did_not_undo() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let app = App::new(
            twilight_http::Client::new(String::new()),
            twilight_http::Client::new(String::new()),
            pool,
            brim_automod::discord_api::Api::new(""),
            Id::new(1),
        );
        let gid = Id::new(6_450_000_000 + std::process::id() as u64);
        let actor = Id::new(42);
        for (action, inv) in [
            (
                "ChannelCreate",
                Some(json!(Inverse::DeleteChannel { id: 9 })),
            ),
            ("ChannelDelete", None),
            ("Kick", Some(json!(Inverse::Reinvite { user: 7 }))),
        ] {
            sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse) VALUES ($1,$2,$3,9,$4)")
                .bind(gid.get() as i64)
                .bind(actor.get() as i64)
                .bind(action)
                .bind(inv)
                .execute(&app.pool)
                .await
                .unwrap();
        }
        let off = BackupsCfg {
            restore: false,
            ..Default::default()
        };
        let sum = revert_actor(&app, gid, actor, now_secs() - 60, Some(&off))
            .await
            .unwrap();
        assert_eq!((sum.ops, sum.failed.len(), sum.recreated), (0, 0, 0));
        let left: Vec<(String,)> = sqlx::query_as(
            "SELECT action FROM action_log WHERE guild_id=$1 AND NOT reverted ORDER BY id",
        )
        .bind(gid.get() as i64)
        .fetch_all(&app.pool)
        .await
        .unwrap();
        let left: Vec<_> = left.into_iter().map(|r| r.0).collect();
        assert_eq!(left, ["ChannelCreate", "ChannelDelete"]);
        sqlx::query("DELETE FROM action_log WHERE guild_id=$1")
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await
            .unwrap();
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn nuke_case_grows_and_is_saved() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let app = App::new(
            twilight_http::Client::new(String::new()),
            twilight_http::Client::new(String::new()),
            pool,
            brim_automod::discord_api::Api::new(""),
            Id::new(1),
        );
        let gid = Id::new(6_000_000_000 + std::process::id() as u64);
        let first = record_nuke(
            &app,
            gid,
            Id::new(10),
            &[Inverse::DeleteChannel { id: 5 }],
            true,
        )
        .await;
        first.1.unwrap();
        let first = first.0;
        let second =
            record_nuke(&app, gid, Id::new(11), &[Inverse::Unban { user: 7 }], false).await;
        second.1.unwrap();
        let second = second.0;
        assert_eq!(first, second);
        let (data,): (serde_json::Value,) =
            sqlx::query_as("SELECT data FROM nuke_incidents WHERE guild_id=$1 AND case_id=$2")
                .bind(gid.get() as i64)
                .bind(&first)
                .fetch_one(&app.pool)
                .await
                .unwrap();
        assert_eq!(data["quarantined"], serde_json::json!([10]));
        assert_eq!(data["failed"], serde_json::json!([11]));
        assert_eq!(data["reverted"].as_array().unwrap().len(), 2);
        sqlx::query("DELETE FROM nuke_incidents WHERE guild_id=$1")
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await
            .unwrap();
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn quarantine_record_inverse_roundtrips() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let app = App::new(
            twilight_http::Client::new(String::new()),
            twilight_http::Client::new(String::new()),
            pool,
            brim_automod::discord_api::Api::new(""),
            Id::new(1),
        );
        let gid = Id::new(6_100_000_000 + std::process::id() as u64);
        record_quarantine(&app, gid, Id::new(42)).await.unwrap();
        let (actor, inv): (i64, serde_json::Value) = sqlx::query_as(
            "SELECT actor_id, inverse FROM action_log WHERE guild_id=$1 AND action='quarantine'",
        )
        .bind(gid.get() as i64)
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert_eq!(actor, 1);
        assert_eq!(
            serde_json::from_value::<Inverse>(inv).unwrap(),
            Inverse::Unquarantine { user: 42 }
        );
        sqlx::query("DELETE FROM action_log WHERE guild_id=$1")
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await
            .unwrap();
    }
}

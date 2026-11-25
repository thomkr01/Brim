//! Panic mode (spec 4.2): runs the antinuke crate's plans when several accounts get quarantined at once.
use anyhow::{Context, Result};
use brim_antinuke::{panic_end_plan, panic_plan, PanicStep};
use brim_automod::lockdown::{self as lockdown_bits, LockKind};
use brim_core::{jobs, permits::Level, sections::PanicCfg};
use brim_logging::emoji::{PANIC, QUARANTINED};
use brim_logging::{
    embed::{self, ARROW, DONE, NO, YES},
    Log, LogKind,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use twilight_model::{
    channel::permission_overwrite::PermissionOverwriteType as ModelOverwriteType,
    guild::Permissions,
    http::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
    id::{
        marker::{ChannelMarker, GuildMarker, UserMarker},
        Id,
    },
};

use crate::{
    app::{App, PanicRun},
    backup,
    events::{send_log, send_log_pinging},
    lockdown,
};

/// True while heat panic is on for the guild and `user` is one of its raiders.
pub fn is_raider(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> bool {
    let mut m = app.heat_raiders.lock().unwrap();
    match m.get(&gid.get()) {
        Some((_, until)) if *until <= crate::app::now_secs() => {
            m.remove(&gid.get());
            false
        }
        Some((set, _)) => set.contains(&user.get()),
        None => false,
    }
}

/// Counts `user` towards panic mode (a quarantined nuker, or with `heat` an automod heat trip)
/// and starts it when enough distinct accounts hit the window. No-op while panic mode runs.
pub async fn note(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>, heat: bool) -> Result<()> {
    let cfg = app.cfg(gid.get()).await?.panic.clone();
    let (executors, tracker) = if heat {
        (
            if cfg.heat_algorithm {
                cfg.heat_executors
            } else {
                0
            },
            &app.panic_heat,
        )
    } else {
        (cfg.executors, &app.panic)
    };
    if executors == 0 || app.panic_runs.lock().unwrap().contains_key(&gid.get()) {
        return Ok(());
    }
    let (tripped, mut raiders) = {
        let mut t = tracker.lock().unwrap();
        t.window = Duration::from_secs(cfg.window_secs);
        t.executors = executors;
        let now = Instant::now();
        let raiders = t.involved(gid.get(), now);
        (t.record(gid.get(), user.get(), now), raiders)
    };
    if !tripped {
        return Ok(());
    }
    if !heat {
        return start(app, gid, &cfg, None).await;
    }
    // Heat panic: every message from these accounts is deleted and timed out until it ends.
    raiders.insert(user.get());
    let mut until = crate::app::now_secs() + cfg.duration_secs as i64;
    // A second wave during heat panic joins the first: those raiders stay held until their end.
    if let Some((old, end)) = app.heat_raiders.lock().unwrap().get(&gid.get()) {
        if *end > crate::app::now_secs() {
            raiders.extend(old);
            until = until.max(*end);
        }
    }
    let log = heat_log(raiders.len(), until - crate::app::now_secs());
    let ids: Vec<i64> = raiders.iter().map(|r| *r as i64).collect();
    app.heat_raiders
        .lock()
        .unwrap()
        .insert(gid.get(), (raiders, until));
    // Kept for a restart (N3, `seed_windows`); a lost row ends heat panic early on one.
    let kept = sqlx::query(
        "INSERT INTO heat_panic (guild_id, raiders, until) VALUES ($1, $2, $3)
         ON CONFLICT (guild_id) DO UPDATE SET raiders = $2, until = $3",
    )
    .bind(gid.get() as i64)
    .bind(&ids)
    .bind(until)
    .execute(&app.pool)
    .await;
    if let Err(e) = kept {
        tracing::warn!(?e, "saving heat panic failed");
    }
    // Heat panic is on already: a failed log must not fail the caller's punishment.
    if let Err(e) = send_log_pinging(app, gid.get(), LogKind::Panic, log, &cfg.warned_roles).await {
        tracing::warn!(?e, "heat panic log failed");
    }
    Ok(())
}

/// [`note`] for a heat trip. Its failure is only logged: the caller's punishment still runs.
pub(crate) async fn note_heat(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) {
    if let Err(e) = note(app, gid, user, true).await {
        tracing::warn!(?e, "counting towards heat panic failed");
    }
}

async fn put_role(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
    role: u64,
    allow: Permissions,
    deny: Permissions,
) -> Result<()> {
    let o = PermissionOverwrite {
        id: Id::new(role),
        kind: PermissionOverwriteType::Role,
        allow: Some(allow),
        deny: Some(deny),
    };
    crate::verify::given::mark_own(gid, channel.get());
    app.http.update_channel_permission(channel, &o).await?;
    Ok(())
}

/// The bits panic's server lock will take from each main role (@everyone when none are set),
/// read before the lock. A role an earlier lock already stripped has nothing left to take. Never a
/// bit the role lock takes too (Mention @everyone): a category allow would beat that lock.
fn taken(
    roles: &[twilight_model::guild::Role],
    main: &[u64],
    gid: u64,
    voice: bool,
) -> Vec<(u64, u64, bool)> {
    let mask = (lockdown_bits::SILENCE | if voice { lockdown_bits::VOICE } else { 0 })
        & !lockdown_bits::DANGEROUS;
    roles
        .iter()
        .map(|r| (r.id.get(), r.permissions.bits() & mask, r.managed))
        .filter(|(id, bits, _)| {
            *bits != 0 && (main.contains(id) || (main.is_empty() && *id == gid))
        })
        .collect()
}

/// The bits to allow for `role` on a category so it can do there what the server lock took
/// (`taken`) and nothing more: never a bit the category already allows or denies the role, nor
/// one `others_deny` (what other overwrites deny there) holds. A role allow beats every role deny,
/// so giving such a bit would let a muted member write; the category stays locked for it instead.
fn give_back(taken: u64, role_ow: (u64, u64), others_deny: u64) -> u64 {
    taken & !role_ow.0 & !role_ow.1 & !others_deny
}

/// One exemption to make: the overwrite `role` has on `category` now (`saved`, for [`unexempt`]),
/// and the one to put.
struct Exemption {
    category: Id<ChannelMarker>,
    role: u64,
    allow: Permissions,
    deny: Permissions,
    saved: Value,
}

/// What a whitelisted category needs so each main role can do there what panic's server lock
/// takes. Changes nothing; [`start`] saves each one in the `panic_end` job before it puts it.
async fn exemptions(
    app: &App,
    gid: Id<GuildMarker>,
    category: Id<ChannelMarker>,
    taken: &[(u64, u64, bool)],
    quarantine_role: Option<u64>,
) -> Result<Vec<Exemption>> {
    let ch = app.http.channel(category).await?.model().await?;
    anyhow::ensure!(ch.guild_id == Some(gid), "category is not in this server");
    let ows: Vec<_> = ch
        .permission_overwrites
        .unwrap_or_default()
        .into_iter()
        .filter(|o| o.kind == ModelOverwriteType::Role)
        .collect();
    let mut out = vec![];
    for &(role, bits, managed) in taken {
        let old = ows.iter().find(|o| o.id.get() == role);
        // Quarantine keeps managed roles, so their allow would meet the Quarantine deny.
        let quarantine_role = quarantine_role.filter(|_| !managed);
        let (allow, deny) = old.map_or((0, 0), |o| (o.allow.bits(), o.deny.bits()));
        // @everyone's allow comes before every role deny, so only a role's allow needs them.
        // Quarantined members lose their other roles, so the Quarantine role's deny never meets
        // a main role's allow.
        let others_deny = if role == gid.get() {
            0
        } else {
            ows.iter()
                .filter(|o| o.id.get() != role && Some(o.id.get()) != quarantine_role)
                .fold(0, |d, o| d | o.deny.bits())
        };
        let add = give_back(bits, (allow, deny), others_deny);
        if add == 0 {
            if give_back(bits, (allow, deny), 0) != 0 {
                tracing::info!(
                    category = category.get(),
                    role,
                    "panic: category stays locked for a main role, another role denies there"
                );
            }
            continue;
        }
        out.push(Exemption {
            category,
            role,
            allow: Permissions::from_bits_retain(allow | add),
            deny: Permissions::from_bits_retain(deny),
            saved: json!({
                "channel": category.get(), "role": role, "allow": allow, "deny": deny, "had": old.is_some()
            }),
        });
    }
    Ok(out)
}

/// `channels` with panic's category exemptions undone as [`unexempt`] would: the overwrite each
/// main role had on a whitelisted category before (or none), from the pending `panic_end` job.
/// shortcut: only the category itself; a child channel synced to it is read live.
pub(crate) async fn unexempted(
    app: &App,
    gid: Id<GuildMarker>,
    channels: &mut [twilight_model::channel::Channel],
) -> Result<()> {
    let ends: Vec<Value> = sqlx::query_scalar(
        "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'panic_end'",
    )
    .bind(gid.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    for saved in ends
        .iter()
        .flat_map(|p| p["exempt"].as_array().into_iter().flatten())
    {
        let Some(c) = saved["channel"]
            .as_u64()
            .and_then(|id| channels.iter_mut().find(|c| c.id.get() == id))
        else {
            continue;
        };
        let role = exempt_role(gid, saved);
        let all = c.permission_overwrites.get_or_insert_with(Vec::new);
        all.retain(|o| o.id.get() != role);
        if saved["had"].as_bool().unwrap_or(false) {
            let bits = |k: &str| Permissions::from_bits_retain(saved[k].as_u64().unwrap_or(0));
            all.push(
                twilight_model::channel::permission_overwrite::PermissionOverwrite {
                    id: Id::new(role),
                    kind: ModelOverwriteType::Role,
                    allow: bits("allow"),
                    deny: bits("deny"),
                },
            );
        }
    }
    Ok(())
}

/// The role a saved exemption is for; jobs from before roles were saved only exempted @everyone.
fn exempt_role(gid: Id<GuildMarker>, saved: &Value) -> u64 {
    saved["role"].as_u64().unwrap_or(gid.get())
}

async fn unexempt(app: &App, gid: Id<GuildMarker>, saved: &Value) -> Result<()> {
    let channel = Id::new(
        saved["channel"]
            .as_u64()
            .context("saved overwrite has no channel")?,
    );
    let role = exempt_role(gid, saved);
    if saved["had"].as_bool().unwrap_or(false) {
        let bits = |k: &str| Permissions::from_bits_retain(saved[k].as_u64().unwrap_or(0));
        put_role(app, gid, channel, role, bits("allow"), bits("deny")).await
    } else {
        crate::verify::given::mark_own(gid, channel.get());
        app.http
            .delete_channel_permission(channel)
            .role(Id::new(role))
            .await?;
        Ok(())
    }
}

/// Marks panic's start as done however `start` returns, so [`relock`] works for the rest of it.
struct Started<'a>(&'a App, Id<GuildMarker>, u64);

impl Drop for Started<'_> {
    fn drop(&mut self) {
        // Only this start's run: an early end and a new start may have replaced it.
        if let Some(run) = self.0.panic_runs.lock().unwrap().get_mut(&self.1.get()) {
            if run.starting == self.2 {
                run.starting = 0;
            }
        }
    }
}

/// While `start` still runs, its `panic_end` job stays at least this far ahead, so a panic shorter than
/// the category calls cannot end in the middle of the start and leave its lockdown without an end job.
const START_MARGIN_SECS: i64 = 60;

/// Rewrites the pending `panic_end` job's payload in place (one statement, so it is never missing),
/// and moves it to at least [`START_MARGIN_SECS`] from now. A dashboard end (`ended_by`) keeps its
/// name, its time and its timed end (`ends_at`, for [`keep_timed_end`]): `start` handles an early
/// end itself (`ends_later`).
async fn end_payload(app: &App, gid: Id<GuildMarker>, payload: Value) -> Result<()> {
    let n = sqlx::query(
        "UPDATE scheduled_jobs SET
           payload = $2 || CASE WHEN payload->'ended_by' IS NULL THEN '{}'::jsonb
                           ELSE jsonb_strip_nulls(jsonb_build_object('ended_by', payload->'ended_by', 'ends_at', payload->'ends_at')) END,
           run_at = CASE WHEN payload->'ended_by' IS NULL
                         THEN greatest(run_at, now() + make_interval(secs => $3)) ELSE run_at END
         WHERE guild_id = $1 AND kind = 'panic_end'",
    )
    .bind(gid.get() as i64)
    .bind(payload)
    .bind(START_MARGIN_SECS as f64)
    .execute(&app.pool)
    .await?
    .rows_affected();
    anyhow::ensure!(n == 1, "panic_end job missing");
    Ok(())
}

/// `by`: who started it from the dashboard (0 when the job did not say); None for a trip.
pub async fn start(app: &App, gid: Id<GuildMarker>, cfg: &PanicCfg, by: Option<u64>) -> Result<()> {
    // The check and the insert are one step: two trips at once must not start it twice, or the
    // second saves the already exempted overwrites and the categories stay open after panic ends.
    static STARTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let token = STARTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let run = PanicRun {
        lock_mod_commands: cfg.lock_mod_commands,
        starting: token,
    };
    if app
        .panic_runs
        .lock()
        .unwrap()
        .insert(gid.get(), run)
        .is_some()
    {
        return Ok(());
    }
    let started = Started(app, gid, token);
    // An end that failed waits for its retry with its locks still on: a new start would replace its
    // job, and with it the claim on those locks, which then never lift. That panic runs on.
    let pending: Result<bool, _> = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'panic_end')",
    )
    .bind(gid.get() as i64)
    .fetch_one(&app.pool)
    .await;
    match pending {
        Ok(false) => {}
        Ok(true) => return Ok(()),
        Err(e) => {
            app.panic_runs.lock().unwrap().remove(&gid.get());
            return Err(e.into());
        }
    }
    let ends = crate::app::now_secs() + cfg.duration_secs as i64;
    // The dashboard reads the mod command lock from this job, so it exists before the category calls.
    // No unlock until the end: nothing is locked before the full payload is saved.
    let payload = |exempted: &[Value], unlock: bool| json!({"lock_mod_commands": cfg.lock_mod_commands, "unlock": unlock, "exempt": exempted, "locked": false, "locked_server": false});
    if let Err(e) = jobs::replace(
        &app.pool,
        gid.get() as i64,
        "panic_end",
        payload(&[], false),
        ends.max(crate::app::now_secs() + START_MARGIN_SECS),
    )
    .await
    {
        app.panic_runs.lock().unwrap().remove(&gid.get());
        return Err(e);
    }
    let mut exempted = vec![];
    let mut saved = Ok(());
    // Only a lock panic makes takes anything a category needs back.
    let categories = if cfg.lock_on_trigger {
        cfg.whitelisted_categories.as_slice()
    } else {
        &[]
    };
    let taken = if categories.is_empty() {
        vec![]
    } else {
        match lock_targets(app, gid).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    ?e,
                    "panic: reading roles for the category exemptions failed"
                );
                vec![]
            }
        }
    };
    let quarantine_role = match app.cfg(gid.get()).await {
        Ok(c) => c.antinuke.quarantine_role.map(|r| r.get()),
        Err(_) => None,
    };
    'categories: for &c in categories {
        let todo = match exemptions(app, gid, c, &taken, quarantine_role).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(?e, channel = c.get(), "panic: reading a category failed");
                continue;
            }
        };
        for one in todo {
            // Saved before the change, so a crash or a lost answer still leaves a job that puts
            // the overwrite back; putting back one that never changed does no harm.
            exempted.push(one.saved.clone());
            saved = end_payload(app, gid, payload(&exempted, false)).await;
            if saved.is_err() {
                break 'categories;
            }
            if let Err(e) = put_role(app, gid, one.category, one.role, one.allow, one.deny).await {
                tracing::warn!(?e, channel = c.get(), "panic: exempting category failed");
            }
        }
    }
    if saved.is_ok() {
        saved = end_payload(app, gid, payload(&exempted, cfg.unlock_when_ending)).await;
    }
    if let Err(e) = saved {
        // Without its end job panic mode would never end: mod commands stay locked and the
        // categories open. Undo the start so the next trip can try again, and drop the job, or the
        // dashboard and a restart would still read mod commands as locked.
        for one in &exempted {
            if let Err(e) = unexempt(app, gid, one).await {
                tracing::warn!(?e, "panic: putting a category back failed");
            }
        }
        let drop_job =
            sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'panic_end'")
                .bind(gid.get() as i64)
                .execute(&app.pool)
                .await;
        if let Err(e) = drop_job {
            tracing::warn!(?e, "panic: removing the panic_end job failed");
        }
        app.panic_runs.lock().unwrap().remove(&gid.get());
        return Err(e);
    }
    crate::reply::alert_owner(app, gid, "Panic mode started", LogKind::Panic).await;
    let mut owned = vec![];
    for step in panic_plan(cfg.lock_on_trigger, cfg.lock_mod_commands) {
        match step {
            // A failed step is logged, so the report below is still sent.
            // Panic's end lifts only a lock this call saved, also one an error left half applied;
            // a lock from before panic, or one staff make after a failed lock, stays.
            // Two locks, so one staff already made (say a server lock) leaves the other to panic.
            // The role lock takes dangerous perms, the server lock takes writing from main roles.
            PanicStep::LockServer => {
                for (kind, key) in [(LockKind::Role, ROLE_LOCK), (LockKind::Server, SERVER_LOCK)] {
                    let saved = take_lock(app, gid, kind.clone(), key).await;
                    let wait = if key == ROLE_LOCK {
                        ROLE_WAITS
                    } else {
                        SERVER_WAITS
                    };
                    if !saved && covered(app, gid, &kind).await {
                        own_lock(app, gid, wait, true).await;
                    }
                    if saved {
                        let made = created(app, gid, &kind).await;
                        owned.push((kind, made));
                    } else if key == SERVER_LOCK {
                        // A lock staff made first covers writing, so the categories close with it.
                        drop_exemptions(app, gid, &exempted).await;
                    }
                }
            }
            PanicStep::PingWarnedRoles => {
                let log = triggered_log();
                if let Err(e) =
                    send_log_pinging(app, gid.get(), LogKind::Panic, log, &cfg.warned_roles).await
                {
                    tracing::warn!(?e, "panic: pinging the warned roles failed");
                }
            }
            // Mod commands are gated by `panic_runs`, and the involved accounts were quarantined
            // by the verdicts that tripped panic mode.
            PanicStep::LockModCommands | PanicStep::QuarantineInvolved => {}
            PanicStep::LoadLatestBackup => {
                // The locks are on: a failed config read must not skip the end and the log.
                let backup = match app.cfg(gid.get()).await {
                    Err(e) => Backup::Failed(e.to_string()),
                    Ok(c) if !(c.backups.enabled && c.backups.restore && cfg.load_backup) => {
                        Backup::Disabled
                    }
                    Ok(_) if !may_restore(app, gid, by).await => Backup::NotAllowed(by),
                    Ok(_) => match backup::restore_latest(app, gid).await {
                        Ok(Some(sum)) => Backup::Loaded(sum),
                        Ok(None) => Backup::None,
                        Err(e) => Backup::Failed(e.to_string()),
                    },
                };
                let case: Option<(String, Value)> = sqlx::query_as(
                    "SELECT case_id, data FROM nuke_incidents WHERE guild_id = $1 ORDER BY created_at DESC LIMIT 1",
                )
                .bind(gid.get() as i64)
                .fetch_optional(&app.pool)
                .await
                .unwrap_or_default();
                let sent = send_log(
                    app,
                    gid.get(),
                    LogKind::Panic,
                    report(case.as_ref(), &backup),
                );
                if let Err(e) = sent.await {
                    tracing::warn!(?e, "panic report log failed");
                }
            }
            PanicStep::UnlockServer => {}
        }
    }
    // Staff can end panic from the dashboard while this ran: its end then read flags this
    // call had not set yet, or never saw these locks. Whatever the end missed is lifted here.
    if !ends_later(app, gid).await {
        // Only the lock this start made, by its age: the end may have lifted it and staff made a
        // new one of the same kind since.
        for (kind, made) in &owned {
            if made.is_none() || created(app, gid, kind).await != *made {
                continue;
            }
            if let Err(e) = lockdown::unlock_now(app, gid, Some(kind)).await {
                tracing::warn!(?e, "panic: lifting a lock after an early end failed");
            }
        }
        drop_exemptions(app, gid, &exempted).await;
        return Ok(());
    }
    // A lock from before panic may have been lifted while this ran, when `relock` still waited.
    // Start is done here, so a lock lifted from now on reaches `relock` and nothing is missed.
    drop(started);
    catch_up_waits(app, gid).await;
    let log = lockdown_log();
    // Panic mode runs: a failed log must not end the nuke run that started it.
    if let Err(e) = send_log(app, gid.get(), LogKind::Panic, log).await {
        tracing::warn!(?e, "panic lockdown log failed");
    }
    Ok(())
}

pub(crate) fn heat_log(raiders: usize, secs: i64) -> Log {
    brim_logging::overlay::security_log(
        "Heat Panic Mode Triggered!",
        "log.panic.heat_panic",
        &[
            ("count", &raiders.to_string()),
            ("duration", &embed::duration(secs)),
        ],
    )
}

pub(crate) fn triggered_log() -> Log {
    let alert = format!("{PANIC} **An abnormal behaviour has been detected!**");
    brim_logging::overlay::security_log(
        "Panic Mode Triggered! [Anti Nuke]",
        "log.panic.triggered",
        &[("alert", &alert)],
    )
}

pub(crate) fn lockdown_log() -> Log {
    let alert = format!("{YES} **The server has been locked down.**");
    brim_logging::overlay::security_log(
        "Panic Mode Lockdown",
        "log.panic.lockdown",
        &[("alert", &alert)],
    )
}

/// What panic mode's backup step did.
pub(crate) enum Backup {
    /// Backups or "Restore after a nuke" are off.
    Disabled,
    None,
    Loaded(backup::Summary),
    Failed(String),
    /// Started by hand by someone below the level of a direct load (the user, 0 when unknown).
    NotAllowed(Option<u64>),
}

/// A trip restores as the owner set it up. Panic mode started by hand loads a backup only for
/// the owner or an Extra Owner, the level `t!backups load` and the dashboard load need; it is
/// read when the job runs, and an unread level does not load.
async fn may_restore(app: &App, gid: Id<GuildMarker>, by: Option<u64>) -> bool {
    let Some(by) = by else { return true };
    let Some(user) = Id::new_checked(by) else {
        return false;
    };
    let level = async {
        let owner = app.guild_info(gid).await?.owner;
        app.level(gid, owner, user).await
    };
    match level.await {
        Ok(l) => matches!(l, Level::Owner | Level::ExtraOwner),
        Err(e) => {
            tracing::warn!(
                ?e,
                "panic: reading the starter's level failed, not loading a backup"
            );
            false
        }
    }
}

/// The green "Panic Mode Report:" embed, sent once per panic: the latest nuke case, who was and
/// was not quarantined, and what the backup step did.
pub(crate) fn report(case: Option<&(String, Value)>, b: &Backup) -> Log {
    let mut quarantined = vec![];
    if let Some((id, data)) = case {
        let ids = |k: &str| -> Vec<u64> {
            data[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .collect()
        };
        let (who, failed) = (ids("quarantined"), ids("failed"));
        quarantined.push(format!("{ARROW} **CASE:** `{id}`"));
        quarantined.push(format!(
            "{QUARANTINED} **Members Quarantined:** {} {YES} | {} {NO}",
            who.len(),
            failed.len()
        ));
        for (u, mark) in who
            .iter()
            .map(|u| (u, QUARANTINED))
            .chain(failed.iter().map(|u| (u, NO)))
        {
            let frags = data["fragments"][u.to_string()].as_u64().unwrap_or(0);
            quarantined.push(format!(
                "{ARROW} {} [{frags} fragments] {mark}",
                embed::user(*u)
            ));
        }
    }
    let (result, error, failed, sum) = match b {
        Backup::Disabled => (
            format!("{NO} **Backup System is disabled!**"),
            "",
            None,
            None,
        ),
        Backup::None => (
            format!("{NO} **There is no backup to load.**"),
            "",
            None,
            None,
        ),
        Backup::NotAllowed(by) => {
            let who = by.filter(|b| *b > 0).map_or("Someone".into(), embed::user);
            (format!(
                "{NO} **The recent backup was not loaded:** {who} started panic mode by hand, and only the owner or an Extra Owner can load a backup."
            ), "", None, None)
        }
        Backup::Failed(e) => (
            format!("{NO} **Backup System failed to load the recent backup!**"),
            e.as_str(),
            None,
            None,
        ),
        Backup::Loaded(sum) => match sum.failures() {
            None => (
                format!("{YES} **Backup System loaded the recent backup successfully!**"),
                "",
                None,
                Some(sum),
            ),
            Some(f) => (
                format!("{NO} **Backup System loaded the recent backup, but some steps failed!**"),
                "",
                Some(f),
                Some(sum),
            ),
        },
    };
    let col = |c: backup::Counts| {
        format!(
            "Deleted: `{}`\nRecovered: `{}`\nEdited: `{}`",
            c.deleted, c.created, c.updated
        )
    };
    let log = Log::new("Panic Mode Report:", DONE)
        .entry("log.panic.report")
        .fact_text("quarantined", &quarantined.join("\n"))
        .section("Backup System Report:")
        .fact_text("backup_result", &result)
        .fact("Error", "error", error)
        .fact("Failed", "failed_steps", failed.as_deref().unwrap_or(""));
    // Only a loaded backup has counts; the other outcomes leave the fields out.
    let cols = sum.map(|sum| {
        [
            col(sum.roles),
            col(sum.channels),
            col(sum.categories),
            format!("Deleted: `{}`", sum.webhooks_deleted),
            // Restores never touch Brim's own settings.
            format!(
                "{ARROW} Server: {}\n{ARROW} Brim: {NO}{}",
                if sum.settings { YES } else { NO },
                if sum.icon_lost {
                    format!("\n{ARROW} Server icon: {NO} no copy kept")
                } else {
                    String::new()
                }
            ),
        ]
    });
    let names = [
        ("Roles", "backup_roles"),
        ("Channels", "backup_channels"),
        ("Categories", "backup_categories"),
        ("Webhooks", "backup_webhooks"),
        ("Settings", "backup_settings"),
    ];
    let mut log = log;
    for (i, (name, var)) in names.into_iter().enumerate() {
        let value = cols.as_ref().map_or("", |c| c[i].as_str());
        if !value.is_empty() {
            log = log.inline(name, value);
        }
        log = log.var(var, value);
    }
    log
}

/// `panic_end` payload key: panic's own role lock.
pub(crate) const ROLE_LOCK: &str = "locked";
/// `panic_end` payload key: panic's own server (writing) lock.
pub(crate) const SERVER_LOCK: &str = "locked_server";

/// The main roles (or @everyone) with the bits panic's server lock would take from them now, and
/// whether the role is managed.
async fn lock_targets(app: &App, gid: Id<GuildMarker>) -> Result<Vec<(u64, u64, bool)>> {
    let cfg = app.cfg(gid.get()).await?;
    let main: Vec<u64> = cfg.lockdown.main_roles.iter().map(|r| r.get()).collect();
    let roles = app.http.roles(gid).await?.models().await?;
    Ok(taken(&roles, &main, gid.get(), cfg.lockdown_prefs.voice))
}

/// `panic_end` payload keys: a lock from before panic held this part when panic started, so
/// panic takes it once that lock is lifted ([`relock`]).
const ROLE_WAITS: &str = "waits_role";
const SERVER_WAITS: &str = "waits_server";

/// While panic runs with its lock on: which parts it waits to take ([`ROLE_WAITS`],
/// [`SERVER_WAITS`]). `None` when panic does not run, does not lock, or the job cannot be read.
pub(crate) async fn held(app: &App, gid: Id<GuildMarker>) -> Option<(bool, bool)> {
    if !app.panic_runs.lock().unwrap().contains_key(&gid.get()) {
        return None;
    }
    if !app.cfg(gid.get()).await.ok()?.panic.lock_on_trigger {
        return None;
    }
    let p: Value = sqlx::query_scalar(
        "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'panic_end'",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await
    .ok()??;
    let flag = |k: &str| p[k].as_bool().unwrap_or(false);
    Some((flag(ROLE_WAITS), flag(SERVER_WAITS)))
}

/// After an unlock during panic: takes a part panic waits for (a lock from before panic held it)
/// once an unlock lifted the lock that held it, say a timed one whose timer ran. Once only: a
/// lock of panic's own that staff lift stays lifted. Only logged on failure.
pub(crate) async fn relock(
    app: &App,
    gid: Id<GuildMarker>,
    waits: (bool, bool),
    lifted: &[LockKind],
) {
    if !app
        .panic_runs
        .lock()
        .unwrap()
        .get(&gid.get())
        .is_some_and(|r| r.starting == 0)
    {
        return;
    }
    for (kind, key, wait, waiting) in [
        (LockKind::Role, ROLE_LOCK, ROLE_WAITS, waits.0),
        (LockKind::Server, SERVER_LOCK, SERVER_WAITS, waits.1),
    ] {
        if !waiting || !lifted.iter().any(|l| l.overlaps(&kind)) {
            continue;
        }
        take_waited(app, gid, kind, key, wait).await;
    }
}

/// Takes a part panic waits for. Marked once saved, inside the guild's lock mutex, so a second
/// take at the same time sees the mark. A refused or failed lock keeps waiting, unless panic's own lock holds
/// the part already (a second relock at the same time). A lock that an end of panic missed
/// meanwhile is lifted again at once. Only logged on failure.
async fn take_waited(
    app: &App,
    gid: Id<GuildMarker>,
    kind: LockKind,
    key: &'static str,
    wait: &'static str,
) {
    own_lock(app, gid, wait, false).await;
    let mut saved = false;
    let r = lockdown::lock_marked(app, gid, kind.clone(), None, &mut saved, Some(key)).await;
    if let Err(e) = r {
        tracing::warn!(?e, key, "panic: locking again failed");
    }
    if !saved {
        if !owns(app, gid, key).await {
            own_lock(app, gid, wait, true).await;
        }
        return;
    }
    let made = created(app, gid, &kind).await;
    let ended =
        !app.panic_runs.lock().unwrap().contains_key(&gid.get()) || !ends_later(app, gid).await;
    if ended && made.is_some() && created(app, gid, &kind).await == made {
        if let Err(e) = lockdown::unlock_now(app, gid, Some(&kind)).await {
            tracing::warn!(?e, "panic: lifting a lock after the end failed");
        }
    }
}

/// Whether the `panic_end` job says panic holds the lock under `key`. A failed read says yes, so
/// nothing waits twice.
async fn owns(app: &App, gid: Id<GuildMarker>, key: &str) -> bool {
    let p: Result<Option<Value>, _> = sqlx::query_scalar(
        "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'panic_end'",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await;
    match p {
        Ok(Some(p)) => p[key].as_bool().unwrap_or(false),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// When the active lock of exactly `kind` was saved, to know it again later; `None` on none.
async fn created(app: &App, gid: Id<GuildMarker>, kind: &LockKind) -> Option<f64> {
    brim_automod::store::lock_created(&app.pool, gid.get(), kind)
        .await
        .ok()
        .flatten()
}

/// Takes each part panic waits for that no lock holds any more, at the end of `start`.
pub(crate) async fn catch_up_waits(app: &App, gid: Id<GuildMarker>) {
    let Some(waits) = held(app, gid).await else {
        return;
    };
    for (kind, key, wait, waiting) in [
        (LockKind::Role, ROLE_LOCK, ROLE_WAITS, waits.0),
        (LockKind::Server, SERVER_LOCK, SERVER_WAITS, waits.1),
    ] {
        if !waiting || covered(app, gid, &kind).await {
            continue;
        }
        take_waited(app, gid, kind, key, wait).await;
    }
}

/// Whether an active lock already holds `kind`'s part (so panic's own lock of it was refused).
async fn covered(app: &App, gid: Id<GuildMarker>, kind: &LockKind) -> bool {
    lockdown::active(app, gid)
        .await
        .is_ok_and(|a| a.iter().any(|k| k.overlaps(kind)))
}

/// One of panic's locks, with its owner flag marked before and after (see [`start`]).
async fn take_lock(app: &App, gid: Id<GuildMarker>, kind: LockKind, key: &'static str) -> bool {
    own_lock(app, gid, key, true).await;
    let mut saved = false;
    let r = lockdown::lock_marked(app, gid, kind, None, &mut saved, Some(key)).await;
    if let Err(e) = r {
        tracing::warn!(?e, key, "panic: locking the server failed");
    }
    if !saved {
        own_lock(app, gid, key, false).await;
    }
    saved
}

/// Whether the `panic_end` job is still waiting: neither gone (an early end ran) nor taken by the
/// scheduler (an early end runs now). A failed read counts as waiting, so nothing is lifted.
pub(crate) async fn ends_later(app: &App, gid: Id<GuildMarker>) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'panic_end'
                       AND (locked_until IS NULL OR locked_until < now()))",
    )
    .bind(gid.get() as i64)
    .fetch_one(&app.pool)
    .await
    .unwrap_or(true)
}

/// Puts the categories back at once when panic's server lock was not made, and empties the job's
/// list. Only logged on failure; the end puts back what is still listed.
async fn drop_exemptions(app: &App, gid: Id<GuildMarker>, exempted: &[Value]) {
    if exempted.is_empty() {
        return;
    }
    let mut all = true;
    for one in exempted {
        if let Err(e) = unexempt(app, gid, one).await {
            tracing::warn!(?e, "panic: putting a category back failed");
            all = false;
        }
    }
    if !all {
        return;
    }
    let r = sqlx::query(
        "UPDATE scheduled_jobs SET payload = payload || jsonb_build_object('exempt', '[]'::jsonb)
         WHERE guild_id = $1 AND kind = 'panic_end'",
    )
    .bind(gid.get() as i64)
    .execute(&app.pool)
    .await;
    if let Err(e) = r {
        tracing::warn!(?e, "panic: emptying the exemptions failed");
    }
}

/// Records in the `panic_end` job whether the lock under `key` ([`ROLE_LOCK`], [`SERVER_LOCK`]) is
/// panic's own, so its end lifts only that one. Only logged on failure.
pub(crate) async fn own_lock(app: &App, gid: Id<GuildMarker>, key: &str, locked: bool) {
    let r = sqlx::query(
        "UPDATE scheduled_jobs SET payload = payload || jsonb_build_object($2::text, $3::bool)
         WHERE guild_id = $1 AND kind = 'panic_end'",
    )
    .bind(gid.get() as i64)
    .bind(key)
    .bind(locked)
    .execute(&app.pool)
    .await;
    if let Err(e) = r {
        tracing::warn!(?e, "panic: saving whose lock it is failed");
    }
}

/// Ends panic mode for a guild; runs from the `panic_end` job, whose payload says what to put back.
pub async fn end(app: &App, gid: Id<GuildMarker>, payload: &Value) -> Result<()> {
    app.panic_runs.lock().unwrap().remove(&gid.get());
    // The job as it is now: a staff unlock after the scheduler read it cleared panic's claim.
    let now: Option<Value> = sqlx::query_scalar(
        "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'panic_end'",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let payload = now.as_ref().unwrap_or(payload);
    for saved in payload["exempt"].as_array().into_iter().flatten() {
        if let Err(e) = unexempt(app, gid, saved).await {
            tracing::warn!(?e, "panic: restoring category overwrite failed");
        }
    }
    // Jobs from before `locked` existed unlock the role lock as they did, and had no server lock.
    let role = payload[ROLE_LOCK].as_bool().unwrap_or(true);
    let server = payload[SERVER_LOCK].as_bool().unwrap_or(false);
    for step in panic_end_plan((role || server) && payload["unlock"].as_bool().unwrap_or(false)) {
        if step == PanicStep::UnlockServer {
            // Both are tried, so a failed role unlock does not leave everyone muted.
            let r = match role {
                true => lockdown::unlock(app, gid, Some(&LockKind::Role))
                    .await
                    .map(drop),
                false => Ok(()),
            };
            if server {
                lockdown::unlock(app, gid, Some(&LockKind::Server)).await?;
            }
            r?;
        }
    }
    // Panic has ended: a failed log must not retry the end, which writes old overwrites again.
    if let Err(e) = send_log(app, gid.get(), LogKind::Panic, end_log(payload)).await {
        tracing::warn!(?e, "panic end log failed");
    }
    Ok(())
}

/// Puts back the timed end of an early end the scheduler refused (its asker lost the level): a
/// new `panic_end` job at the time the dashboard moved it from, with the current payload, so panic
/// mode still ends and keeps what it must restore. The refused row is finished by the caller; a retry
/// after a put-back that was not finished adds no second one.
pub async fn keep_timed_end(app: &App, id: i64) -> Result<()> {
    // shortcut: a job moved by a dashboard from before `ends_at` gets one hour; the field is always
    // set since, so this only covers an update that lands while panic mode runs.
    sqlx::query(
        "INSERT INTO scheduled_jobs (guild_id, kind, payload, run_at)
         SELECT guild_id, kind, payload - 'ended_by' - 'ends_at',
                COALESCE(to_timestamp((payload->>'ends_at')::float8), now() + interval '1 hour')
         FROM scheduled_jobs j WHERE id = $1
           AND NOT EXISTS (SELECT 1 FROM scheduled_jobs o
                           WHERE o.guild_id = j.guild_id AND o.kind = 'panic_end' AND o.id <> $1)",
    )
    .bind(id)
    .execute(&app.pool)
    .await?;
    Ok(())
}

/// Names who ended panic early from the dashboard; a timed end has nobody.
pub(crate) fn end_log(payload: &Value) -> Log {
    let by = payload["ended_by"]
        .as_str()
        .and_then(|u| u.parse::<u64>().ok())
        .map_or(String::new(), embed::user);
    let alert = format!("{YES} **Panic mode has ended.**");
    brim_logging::overlay::security_log(
        "Panic Mode Ended",
        "log.panic.ended",
        &[("alert", &alert), ("by", &by)],
    )
}

/// Re-arms panic mode and join raids after a restart: the pending `panic_end` and `raid_end` jobs
/// say which guilds are still in them.
pub async fn restore(app: &App) -> Result<()> {
    for (guild, payload) in jobs::pending(&app.pool, "raid_end").await? {
        if let Some(until) = payload["until_ms"].as_i64() {
            let started = payload["started_ms"].as_i64().unwrap_or(until);
            let d = brim_automod::join::RaidDetector::restored(
                started,
                until,
                payload["code"].as_str().unwrap_or_default().into(),
            );
            app.raids.lock().unwrap().insert(guild as u64, d);
        }
    }
    for (guild, payload) in jobs::pending(&app.pool, "panic_end").await? {
        app.panic_runs.lock().unwrap().insert(
            guild as u64,
            PanicRun {
                lock_mod_commands: payload["lock_mod_commands"].as_bool().unwrap_or(false),
                starting: 0,
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_early_end_names_who_ended_it() {
        let by = format!("{:?}", end_log(&json!({"ended_by": "42"})).build());
        assert!(by.contains("<@42>"), "{by}");
        let timed = format!("{:?}", end_log(&json!({})).build());
        assert!(!timed.contains("Ended by"), "{timed}");
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. The per-category re-save changes the
    /// one pending job in place, and refuses when there is none (start then undoes itself).
    #[tokio::test]
    async fn end_payload_rewrites_the_pending_job_in_place() {
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
        let gid = Id::new(9_890_000_000 + std::process::id() as u64);
        let g = gid.get() as i64;
        assert!(end_payload(&app, gid, json!({})).await.is_err());
        jobs::replace(
            &app.pool,
            g,
            "panic_end",
            json!({"exempt": []}),
            crate::app::now_secs() + 3600,
        )
        .await
        .unwrap();
        let one = json!({"unlock": false, "exempt": [{"channel": 5, "had": false}]});
        end_payload(&app, gid, one.clone()).await.unwrap();
        assert_eq!(
            jobs::pending(&app.pool, "panic_end")
                .await
                .unwrap()
                .into_iter()
                .filter(|(id, _)| *id == g)
                .map(|(_, p)| p)
                .collect::<Vec<_>>(),
            [one]
        );
        // A job already due (a panic shorter than the start) moves out of the scheduler's reach.
        sqlx::query("UPDATE scheduled_jobs SET run_at = now() WHERE guild_id = $1")
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
        end_payload(&app, gid, json!({})).await.unwrap();
        let ahead: f64 = sqlx::query_scalar(
            "SELECT extract(epoch from run_at - now())::float8 FROM scheduled_jobs WHERE guild_id = $1",
        )
        .bind(g)
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert!(ahead > 55.0, "{ahead}");
        // L6: a dashboard end keeps its name and stays due.
        sqlx::query(
            "UPDATE scheduled_jobs SET run_at = now(), payload = payload || '{\"ended_by\": \"42\"}' WHERE guild_id = $1",
        )
        .bind(g)
        .execute(&app.pool)
        .await
        .unwrap();
        end_payload(&app, gid, json!({"unlock": true}))
            .await
            .unwrap();
        let (p, ahead): (Value, f64) = sqlx::query_as(
            "SELECT payload, extract(epoch from run_at - now())::float8 FROM scheduled_jobs WHERE guild_id = $1",
        )
        .bind(g)
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert_eq!(p, json!({"unlock": true, "ended_by": "42"}));
        assert!(ahead <= 0.0, "{ahead}");
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1")
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
    }

    #[test]
    fn the_lock_takes_writing_from_main_roles_and_a_category_gets_only_that_back() {
        let role = |id: u64, bits: u64| -> twilight_model::guild::Role {
            serde_json::from_value(json!({"id": id.to_string(), "name": "r", "color": 0, "colors": {"primary_color": 0},
                "hoist": false, "managed": false, "mentionable": false, "permissions": bits.to_string(), "position": 0, "flags": 0}))
            .unwrap()
        };
        let (send, view, react) = (1u64 << 11, 1u64 << 10, 1u64 << 6);
        let roles = [
            role(9, view | send | react | (1 << 17)),
            role(5, view | send),
            role(6, view),
        ];
        // No main roles: @everyone (the guild id) is the one locked.
        assert_eq!(taken(&roles, &[], 9, false), [(9, send | react, false)]);
        // Main roles set: only those, and a role with nothing to lose is skipped.
        assert_eq!(taken(&roles, &[5, 6], 9, false), [(5, send, false)]);
        // Already allowed, denied on the role or denied to @everyone: never given.
        assert_eq!(give_back(send | react, (0, 0), 0), send | react);
        assert_eq!(give_back(send | react, (0, react), 0), send);
        assert_eq!(give_back(send | react, (send, 0), react), 0);
    }

    fn text(l: Log) -> String {
        serde_json::to_string(&l.build()).unwrap()
    }

    /// security review: panic started by hand loads a backup only for an Owner or Extra Owner, as a
    /// direct load; a trip loads as set up.
    #[tokio::test]
    async fn a_trusted_admins_panic_loads_no_backup() {
        let Some(t) = crate::test_support::T::new(214).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({})).await;
        let ta = t.base * 10 + 1;
        t.exec("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $1 * 10 + 1)")
            .await;
        t.set(|m| {
            m.members.insert((t.g(0).get(), ta), vec![]);
        });
        let g = t.g(0);
        assert!(may_restore(&t.app, g, None).await, "a trip");
        assert!(may_restore(&t.app, g, Some(t.owner())).await);
        assert!(!may_restore(&t.app, g, Some(ta)).await, "a Trusted Admin");
        assert!(
            !may_restore(&t.app, g, Some(0)).await,
            "an old job without `by`"
        );
        let r = text(report(None, &Backup::NotAllowed(Some(ta))));
        assert!(
            r.contains("only the owner or an Extra Owner can load a backup"),
            "{r}"
        );
        t.cleanup().await;
    }

    #[test]
    fn report_counts_both_sides_and_names_every_backup_outcome() {
        let case = (
            "9g5udu".to_string(),
            json!({"quarantined": [11], "failed": [22], "fragments": {"11": 8, "22": 1}}),
        );
        let t = text(report(Some(&case), &Backup::Disabled));
        assert!(t.contains("**CASE:** `9g5udu`"), "{t}");
        assert!(
            t.contains(&format!("**Members Quarantined:** 1 {YES} | 1 {NO}")),
            "{t}"
        );
        assert!(
            t.contains("[8 fragments]") && t.contains("[1 fragments]"),
            "{t}"
        );
        assert!(t.contains("Backup System is disabled!"), "{t}");
        assert!(text(report(Some(&case), &Backup::None)).contains("There is no backup to load."));
        let failed = text(report(None, &Backup::Failed("boom".into())));
        assert!(failed.contains("failed to load") && failed.contains("boom"));
        assert!(!failed.contains("CASE"));
        let loaded = text(report(
            Some(&case),
            &Backup::Loaded(backup::Summary::default()),
        ));
        assert!(
            loaded.contains("loaded the recent backup successfully"),
            "{loaded}"
        );
        let partly = text(report(
            None,
            &Backup::Loaded(backup::Summary {
                failed: 3,
                first_error: Some("role: Missing Permissions".into()),
                ..Default::default()
            }),
        ));
        assert!(
            partly.contains("some steps failed")
                && partly.contains("3 steps failed, first: role: Missing Permissions"),
            "{partly}"
        );
        // An older case saved before the failed list existed reads as 0 failed.
        let old = ("abc123".to_string(), json!({"quarantined": [11]}));
        assert!(text(report(Some(&old), &Backup::None)).contains(&format!("1 {YES} | 0 {NO}")));
    }
}

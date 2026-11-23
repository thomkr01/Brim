//! Polls `scheduled_jobs` and runs what is due; failures are logged, never fatal.
use anyhow::{Context, Result};
use brim_automod::lockdown::{kind_from, missing, LockKind};
use brim_core::{
    jobs::{self, Job},
    permits::Level,
};
use brim_logging::{
    embed::{DONE, NO, RED, YES},
    Log, LogKind,
};
use serde_json::Value;
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    time::Duration,
};
use twilight_model::id::{
    marker::{GuildMarker, UserMarker},
    Id,
};

use crate::{
    app::{now_secs, App},
    backup, commands,
    events::{send_log, send_log_link},
    http_err,
    joingate::RAID_REPORT,
    lockdown, metrics, panic, verify,
};

fn user(payload: &Value) -> Result<Id<UserMarker>> {
    Ok(Id::new(
        payload["user"]
            .as_u64()
            .context("job payload has no user")?,
    ))
}

/// What a ban ends: an older temp ban's unban, and a timeout's renewal (which would retry hourly).
pub(crate) const BANNED: &[&str] = &["unban", "timeout_renew"];

/// Drops the pending `kinds` jobs an earlier temp ban, long timeout or raid punishment of `user`
/// queued, other than `keep`, so they cannot lift, stretch or redo what was done since. Only
/// logged on failure: the action itself is already done.
pub(crate) async fn forget_jobs(
    app: &App,
    gid: Id<GuildMarker>,
    user: u64,
    kinds: &[&str],
    keep: Option<i64>,
) {
    if let Err(e) = jobs::cancel_user(&app.pool, gid.get() as i64, kinds, user, keep).await {
        tracing::warn!(?e, ?kinds, user, "dropping an earlier pending job failed");
    }
}

async fn dispatch(app: &App, job: &Job) -> Result<()> {
    // The dashboard's Raider List decisions name no server: the job fans out to each one.
    if job.guild_id == 0 && job.kind == crate::datareq::KIND {
        return crate::datareq::run_job(app, &job.payload).await;
    }
    if job.guild_id == 0 {
        return crate::raiders::fan_out(app, job).await;
    }
    let gid = Id::new(job.guild_id as u64);
    if let Some(why) = refused(app, gid, job).await? {
        if job.kind == "panic_end" {
            panic::keep_timed_end(app, job.id).await?;
        }
        dropped_log(app, gid, job, why).await;
        return Ok(());
    }
    match job.kind.as_str() {
        "unban" => {
            let u = user(&job.payload)?;
            let retry = job.payload["verify_retry"] == true;
            if !retry || crate::verify::still_timeout_ban(app, gid, u).await? {
                app.http.delete_ban(gid, u).await?;
                if retry {
                    crate::verify::retry_after_ban(app, gid, u).await;
                }
            }
        }
        "lock" => {
            let p = &job.payload;
            let channel = p["channel"]
                .as_u64()
                .or_else(|| p["channel"].as_str()?.parse().ok());
            let kind = kind_from(p["kind"].as_str().unwrap_or_default(), channel)
                .context("job payload has no valid lock kind")?;
            lockdown::lock_for(app, gid, kind, p["duration_s"].as_i64()).await?;
        }
        // Timed unlocks name their lock; older jobs and the dashboard's lift everything.
        "unlock" => {
            let which = serde_json::from_value(job.payload["lock"].clone()).ok();
            lockdown::unlock(app, gid, which.as_ref()).await?;
        }
        "timeout_renew" => {
            let until = job.payload["until_secs"]
                .as_i64()
                .context("job payload has no until_secs")?;
            let r =
                commands::timeout_until(app, gid, user(&job.payload)?, until, "Timeout renewal");
            match r.await {
                // Not in the server: Discord keeps a timeout for a rejoin, so try hourly until it ends.
                Err(e) if http_err::code(&e) == Some(10007) && until > now_secs() => {
                    let p = job.payload.clone();
                    jobs::enqueue(
                        &app.pool,
                        job.guild_id,
                        "timeout_renew",
                        p,
                        now_secs() + 3600,
                    )
                    .await?;
                }
                r => r?,
            }
        }
        "panic_end" => panic::end(app, gid, &job.payload).await?,
        "panic_start" => {
            if !app.panic_runs.lock().unwrap().contains_key(&gid.get()) {
                let cfg = app.cfg(gid.get()).await?.panic.clone();
                // Started by hand: the backup step checks who did (0: a job from before `by`).
                let by = id_of(&job.payload, "by").unwrap_or(0);
                panic::start(app, gid, &cfg, Some(by)).await?;
            }
        }
        "note_log" => {
            let n = serde_json::from_value(job.payload.clone()).context("bad note_log payload")?;
            crate::modview::send_note_log(app, gid.get(), &n).await?;
        }
        "case_log" => {
            let c = serde_json::from_value(job.payload.clone()).context("bad case_log payload")?;
            crate::proof::send_case_log(app, gid.get(), &c).await?;
        }
        "case_delete_log" => {
            let c = serde_json::from_value(job.payload.clone())
                .context("bad case_delete_log payload")?;
            crate::proof::send_case_delete_log(app, gid.get(), &c).await?;
        }
        "old_cases_log" => {
            let count = job.payload["count"]
                .as_u64()
                .context("job payload has no count")?;
            let days = job.payload["days"]
                .as_i64()
                .context("job payload has no days")?;
            crate::proof::send_old_cases_log(app, gid.get(), count, days).await?;
        }
        "timeout_end" => crate::timeout_end::run(app, gid, &job.payload).await?,
        "lock_update" => {
            let text = job.payload["text"]
                .as_str()
                .context("job payload has no text")?;
            let by = need(&job.payload, "by")?;
            crate::lockdown::dashboard_update(app, gid, by, text).await?;
        }
        "backup_create" => {
            let id = backup::save(app, gid, job.payload["by"].as_u64()).await?;
            send_log(app, gid.get(), LogKind::Server, created_log(&id)).await?;
        }
        "backup_load" => {
            let id = job.payload["id"]
                .as_i64()
                .context("job payload has no id")?;
            // The gate above refuses a load that names no Owner or Extra Owner.
            let by = need(&job.payload, "by")?;
            let text = backup::load(app, gid, &id.to_string(), true, by).await?;
            // `load` answers a refusal (locked, panic, no such backup) as text.
            let log = loaded_log(id, &text);
            send_log(app, gid.get(), LogKind::Server, log).await?;
        }
        "appeal_accept" => crate::appeals::accept_job(app, gid, &job.payload).await?,
        "owner_check" => crate::owner::owner_check_job(app, gid).await?,
        "rescue_expired" => crate::owner::rescue_expired_job(app, gid).await?,
        "permit_sync" => crate::permit_sync::sync(app, gid).await?,
        "keyword_rule_sync" => crate::automod::rule_sync_job(app, gid.get()).await,
        "delete_message" => crate::reply::run_delete(app, &job.payload).await?,
        "confirm_expire" => crate::confirm::run_expire(app, gid, &job.payload).await?,
        "raid_punish" => crate::joingate::punish_job(app, gid, &job.payload).await?,
        crate::reply::QUARANTINE_DM => {
            crate::reply::quarantine_dm_job(app, gid, &job.payload).await?
        }
        crate::reply::QUARANTINE_DM_COOLDOWN => {}
        crate::verify::given::CATCH_UP => {
            crate::verify::given::catch_up(app, gid, Some(&job.payload)).await?
        }
        crate::joingate::MISSED_REJOINS => {
            crate::joingate::requarantine_missed(app, gid, &[], false, Some(&job.payload)).await?
        }
        "verify_deadline" => verify::deadline(app, gid, user(&job.payload)?).await?,
        "verify_sweep" => verify::sweep(app, gid).await?,
        "verify_blocked" => {
            let list = job.payload["list"].as_str().unwrap_or("VPN");
            verify::web_blocked(app, gid, user(&job.payload)?, list).await?
        }
        "verify_grant" => {
            let same_ip: Vec<u64> = job.payload["same_ip"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str()?.parse().ok())
                .collect();
            verify::web_grant(app, gid, user(&job.payload)?, &same_ip).await?
        }
        "raid_end" => {
            let caught: i64 = sqlx::query_scalar(
                "select count(*) from raid_log where guild_id = $1 and raid_ms = $2",
            )
            .bind(gid.get() as i64)
            .bind(job.payload["started_ms"].as_i64().unwrap_or(0))
            .fetch_one(&app.pool)
            .await?;
            let code = job.payload["code"].as_str().unwrap_or_default();
            let url = crate::invites::raid_url(code).filter(|_| caught > 0);
            let log = crate::joingate::raid_report_log(caught, url.is_some().then_some(code));
            raid_report(app, gid, log, url).await?
        }
        "voice_raid_end" => crate::voice_raid::end(app, gid, &job.payload).await?,
        "nuke_revert" => {
            let p = &job.payload;
            let actor = Id::new(p["actor"].as_u64().context("job payload has no actor")?);
            let why = p["why"].as_str().unwrap_or_default();
            let since = p["since"].as_i64().context("job payload has no since")?;
            let reported = p["reported"].as_bool().unwrap_or(false);
            crate::antinuke::resume_nuke(app, gid, actor, why, since, reported).await?
        }
        "restore_main_roles" => crate::timeout_roles::restore(app, gid, &job.payload).await?,
        crate::sticky::KIND => crate::sticky::run_job(app, gid, &job.payload).await?,
        "slowmode_off" => crate::slowmode::off(app, gid, &job.payload).await?,
        "raider_check" => crate::raiders::check_job(app, gid, &job.payload).await?,
        "raider_scan" => crate::raiders::scan_job(app, gid).await?,
        "raider_lift" => crate::raiders::lift_job(app, gid, &job.payload).await?,
        brim_core::giveaways::END_JOB => crate::giveaways::end_job(app, gid, &job.payload).await?,
        brim_core::giveaways::REFRESH_JOB => crate::giveaways::refresh_job(app, gid).await?,
        brim_core::giveaways::REPOST_JOB => {
            crate::giveaways::repost_job(app, gid, &job.payload).await?
        }
        brim_core::giveaways::ANNOUNCE_JOB => {
            crate::giveaways::announce_job(app, gid, &job.payload).await?
        }
        brim_core::giveaways::ACTION_JOB => {
            crate::giveaways::action_job(app, gid, &job.payload).await?
        }
        brim_core::giveaways::DM_JOB => crate::giveaways::dm_job(app, gid, &job.payload).await?,
        "ticket_close_check" => crate::tickets::close_check(app, gid, &job.payload).await?,
        other => tracing::warn!(kind = other, "unknown job kind"),
    }
    Ok(())
}

/// Jobs a person queues on the dashboard. Each runs only while that person may still do it: a job
/// whose asker lost the right while it waited is dropped and logged (security review, owner decision
/// 2026-11-17). The checks are the dashboard's at the time it queued the job.
const RECHECKED: [&str; 7] = [
    "lock",
    "unlock",
    "lock_update",
    "panic_start",
    "panic_end",
    "backup_create",
    "backup_load",
];

/// Why a [`RECHECKED`] job must not run, or None. An error (Discord did not answer) fails the job,
/// so it is tried again.
async fn refused(app: &App, gid: Id<GuildMarker>, job: &Job) -> Result<Option<&'static str>> {
    let kind = job.kind.as_str();
    if !RECHECKED.contains(&kind) {
        return Ok(None);
    }
    // An early panic end names who ended it; the timed end names nobody.
    let key = if kind == "panic_end" {
        "ended_by"
    } else {
        "by"
    };
    let Some(by) = id_of(&job.payload, key).and_then(Id::new_checked) else {
        // The bot's own lock timers name no person, and always name their lock.
        let timer = kind == "panic_end" || (kind == "unlock" && !job.payload["lock"].is_null());
        return Ok((!timer).then_some("It names no person who asked for it."));
    };
    let owner = app.guild_info(gid).await?.owner;
    let level = app.level(gid, owner, by).await?;
    let mods_locked = app.mod_commands_locked(gid.get());
    let ok = match kind {
        "backup_create" | "backup_load" => matches!(level, Level::Owner | Level::ExtraOwner),
        "panic_start" | "panic_end" => matches!(
            level,
            Level::Owner | Level::ExtraOwner | Level::TrustedAdmin
        ),
        "lock_update" => may_lock(&level, "lock_updates", &[], mods_locked),
        "lock" => {
            let p = &job.payload;
            let channel = id_of(p, "channel");
            let Some(k) = kind_from(p["kind"].as_str().unwrap_or_default(), channel) else {
                return Ok(Some("It names no valid lock."));
            };
            may_lock(&level, "lockdown", &[k], mods_locked)
        }
        // As the dashboard asks: the permit of every lock it lifts.
        _ => {
            let which: Option<LockKind> = serde_json::from_value(job.payload["lock"].clone()).ok();
            let lifts: Vec<LockKind> = brim_automod::store::locks(&app.pool, gid.get())
                .await?
                .into_iter()
                .filter_map(|s| s.kind)
                .filter(|a| which.as_ref().is_none_or(|k| k.overlaps(a)))
                .chain(which.clone())
                .collect();
            may_lock(&level, "lockdown", &lifts, mods_locked)
        }
    };
    Ok((!ok).then_some(match kind {
        "panic_end" => "The person who ended panic mode early no longer has the level for it. Panic mode runs until its timed end.",
        _ => "The person who asked for it no longer has the level or permit for it.",
    }))
}

/// The dashboard's lock bar: TA and up always; below that the Dashboard Edit permit perm, `perm`,
/// the lock permit perm of each kind, and no panic mode lock of mod commands.
fn may_lock(level: &Level, perm: &str, kinds: &[LockKind], mods_locked: bool) -> bool {
    match level {
        Level::Owner | Level::ExtraOwner | Level::TrustedAdmin => true,
        Level::Custom { perms, .. } => {
            perms.iter().any(|p| p == "dashboard_edit")
                && level.has(perm)
                && !mods_locked
                && kinds.iter().all(|k| missing(|p| level.has(p), k).is_none())
        }
        Level::None => false,
    }
}

pub(crate) fn created_log(id: impl std::fmt::Display) -> Log {
    Log::new("Backup Created", DONE)
        .line("Id", &format!("`{id}`"))
        .var("backup", &id.to_string())
}

/// "Backup Loaded", or "Backup Not Loaded" with `load`'s refusal.
pub(crate) fn loaded_log(id: i64, text: &str) -> Log {
    let (title, color, mark, key) = match text.starts_with("Restored") {
        true => ("Backup Loaded", DONE, YES, "log.server.backup_loaded"),
        false => ("Backup Not Loaded", RED, NO, "log.server.backup_not_loaded"),
    };
    Log::new(title, color)
        .entry(key)
        .line("Id", &format!("`{id}`"))
        .text(&format!("{mark} **{text}**"))
        .var("backup", &id.to_string())
        .var("result", text)
}

pub(crate) fn dropped(kind: &str, by: Option<u64>, why: &str) -> Log {
    let mut log = Log::new("Scheduled Action Dropped", RED).line("Action", &format!("`{kind}`"));
    let mut asked = String::new();
    if let Some(by) = by {
        asked = format!("<@{by}>");
        log = log.line("Asked by", &asked);
    }
    log.text(why)
        .var("job", kind)
        .var("by", &asked)
        .var("reason", why)
}

async fn dropped_log(app: &App, gid: Id<GuildMarker>, job: &Job, why: &str) {
    tracing::warn!(id = job.id, kind = %job.kind, why, "dropped a queued job");
    let log = dropped(&job.kind, id_of(&job.payload, "by"), why);
    if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
        tracing::warn!(?e, id = job.id, "could not log the dropped job");
    }
}

/// Posts a raid report, with the Full Raid Report button when there is a page to open.
pub(crate) async fn raid_report(
    app: &App,
    gid: Id<GuildMarker>,
    log: Log,
    url: Option<String>,
) -> Result<()> {
    match url {
        Some(url) => {
            send_log_link(app, gid.get(), LogKind::JoinRaid, log, (RAID_REPORT, url)).await
        }
        None => send_log(app, gid.get(), LogKind::JoinRaid, log).await,
    }
}

const ONE_TARGET: [&str; 7] = [
    "unban",
    "raid_punish",
    "timeout_renew",
    "delete_message",
    "slowmode_off",
    "raider_check",
    "raider_lift",
];

/// Runs one claim round for one lane's kinds; failures are logged, never fatal.
async fn round(app: &Arc<App>, lane: jobs::Lane) {
    match jobs::claim_due(&app.pool, 50, lane).await {
        Ok(due) => {
            for job in due {
                // A guard whose run is still alive here: that run deletes the job when it ends,
                // and the lease brings it back only if this process dies first.
                if job.kind == "nuke_revert"
                    && job.payload["actor"]
                        .as_u64()
                        .is_some_and(|a| crate::antinuke::nuke_running(app, job.guild_id as u64, a))
                {
                    continue;
                }
                // Startup catch-ups page members or the audit log for minutes: they run beside the
                // worker, so they never hold back panic_start or nuke_revert. Each takes its own
                // per-guild lock. The row is done only on Ok; on an error it stays leased, so the
                // lease, the attempt limit and the give-up log apply as for any job.
                // Urgent jobs each get a task too, so one server's long nuke revert does not hold
                // another server's panic start.
                if SPAWNED.contains(&job.kind.as_str()) || lane == jobs::Lane::Urgent {
                    let app = app.clone();
                    tokio::spawn(async move {
                        match dispatch(&app, &job).await {
                            Ok(()) => {
                                metrics::JOBS_DONE.fetch_add(1, Relaxed);
                                if let Err(e) = jobs::finish(&app.pool, job.id).await {
                                    tracing::warn!(?e, id = job.id, "finishing job failed");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(?e, id = job.id, kind = %job.kind, "job failed");
                                metrics::JOBS_FAILED.fetch_add(1, Relaxed);
                                if job.attempts >= jobs::MAX_ATTEMPTS {
                                    give_up_log(&app, &job, &e).await;
                                    if let Err(e) = jobs::finish(&app.pool, job.id).await {
                                        tracing::warn!(?e, id = job.id, "finishing job failed");
                                    }
                                }
                            }
                        }
                    });
                    continue;
                }
                let done = match dispatch(app, &job).await {
                    Ok(()) => {
                        metrics::JOBS_DONE.fetch_add(1, Relaxed);
                        true
                    }
                    // One-target jobs whose member left, ban was lifted by hand or message is gone:
                    // nothing left to do, and retrying would only fail again.
                    Err(e) if ONE_TARGET.contains(&job.kind.as_str()) && http_err::gone_any(&e) => {
                        tracing::info!(id = job.id, kind = %job.kind, "job target is gone");
                        true
                    }
                    Err(e) => {
                        tracing::warn!(?e, id = job.id, kind = %job.kind, "job failed");
                        metrics::JOBS_FAILED.fetch_add(1, Relaxed);
                        // Otherwise the lease expires and it runs again.
                        let last = job.attempts >= jobs::MAX_ATTEMPTS;
                        if last {
                            give_up_log(app, &job, &e).await;
                            if job.kind == "raider_lift" {
                                crate::raiders::lift_gave_up(app, &job, &e).await;
                            }
                            if job.kind == crate::datareq::KIND {
                                crate::datareq::gave_up(app, &job).await;
                            }
                        }
                        last
                    }
                };
                if done {
                    if let Err(e) = jobs::finish(&app.pool, job.id).await {
                        tracing::warn!(?e, id = job.id, "finishing job failed");
                    }
                }
            }
        }
        Err(e) => tracing::warn!(?e, "claiming jobs failed"),
    }
}

const SPAWNED: [&str; 4] = [
    // A DM can wait up to 3 s: 100 winner DMs must not hold back unbans and unlocks.
    brim_core::giveaways::DM_JOB,
    crate::joingate::MISSED_REJOINS,
    crate::verify::given::CATCH_UP,
    "permit_sync",
];

/// A dropped job (a temp ban that never ends, a lock that stays) must be visible on Discord, not
/// only in the container log.
async fn give_up_log(app: &App, job: &Job, e: &anyhow::Error) {
    let log = give_up(&job.kind, e, job.attempts);
    if let Err(e) = send_log(app, job.guild_id as u64, LogKind::Server, log).await {
        tracing::warn!(?e, id = job.id, "could not log the failed job");
    }
}

pub(crate) fn give_up(kind: &str, e: &anyhow::Error, tries: impl std::fmt::Display) -> Log {
    let error = crate::http_err::why(e);
    Log::new("Scheduled Action Failed", RED)
        .line("Action", &format!("`{kind}`"))
        .line("Error", &error)
        .text(&format!("Gave up after {tries} tries, do it by hand."))
        .var("job", kind)
        .var("error", &error)
        .var("tries", &tries.to_string())
}

pub async fn run(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    let mut n: u32 = 0;
    loop {
        tick.tick().await;
        round(&app, jobs::Lane::Light).await;
        // About once an hour (720 ticks of 5 s), and once soon after start.
        // Own task: its deletes must not hold up due jobs.
        if n % 720 == 12 {
            let app = app.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::msghooks::sweep(&app).await {
                    tracing::warn!(?e, "removing unused message webhooks failed");
                }
                if let Err(e) = brim_core::persondata::sweep_kept(&app.pool).await {
                    tracing::warn!(?e, "finishing done data delete requests failed");
                }
            });
        }
        n = n.wrapping_add(1);
        if let Err(e) = jobs::expire_cases(&app.pool).await {
            tracing::warn!(?e, "case expiry failed");
        }
    }
}

/// The restore worker: backup loads, member scans and catch-ups run here, on their own task and
/// HTTP client, so a long restore never holds up the timers in [`run`].
pub async fn run_restore(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        round(&app, jobs::Lane::Heavy).await;
    }
}

/// The protection worker: panic starts and nuke reverts, so no server's backup load or member
/// scan delays another server's protection.
pub async fn run_urgent(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        round(&app, jobs::Lane::Urgent).await;
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;

/// An id in a job payload, as a number or a string.
pub(crate) fn id_of(p: &Value, k: &str) -> Option<u64> {
    p[k].as_u64().or_else(|| p[k].as_str()?.parse().ok())
}

/// [`id_of`], or an error naming the missing key.
pub(crate) fn need(p: &Value, k: &str) -> Result<u64> {
    id_of(p, k).with_context(|| format!("job payload has no {k}"))
}

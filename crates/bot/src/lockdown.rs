//! Runs automod lockdown plans against Discord and restores them from the stored snapshot.
use anyhow::Result;
use brim_automod::{
    discord_api::MAX_INVITE_PAUSE_MS,
    lockdown::{
        missing, plan, unlock_ops, JoinLock, LockKind, Op, Plan, RoleInfo, Snapshot, SILENCE,
    },
    store,
};
use twilight_model::{
    guild::Permissions,
    http::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
    id::{
        marker::{ChannelMarker, GuildMarker, UserMarker},
        Id,
    },
};

use crate::{
    app::{now_secs, App},
    events::send_log,
};
use brim_logging::{
    embed::{self, ALERT, ARROW, BLUE, DONE, GREEN, NO, RED, YES},
    emoji::{ANNOUNCE, LOCK, MODERATOR, RUNNING, UNLOCK},
    Log, LogKind,
};
use std::time::{Duration, Instant};
use twilight_model::channel::message::Embed;

/// Runs one op; returns the channel it created, if any.
async fn exec(app: &App, gid: Id<GuildMarker>, op: &Op, snap: &Snapshot) -> Result<Option<u64>> {
    match op {
        Op::SetRolePerms { role, perms } => {
            crate::verify::given::mark_own(gid, *role);
            let perms = Permissions::from_bits_retain(*perms);
            app.http
                .update_role(gid, Id::new(*role))
                .permissions(perms)
                .await?;
        }
        Op::DenyChannelSend { channel, role } => {
            crate::verify::given::mark_own(gid, *channel);
            // Other bits of an existing overwrite stay; unlock puts the old one back.
            let (allow, deny) = snap
                .prior
                .iter()
                .find(|p| p.0 == *channel && p.1 == *role)
                .map_or((0, 0), |p| (p.2, p.3));
            let o = PermissionOverwrite {
                id: Id::new(*role),
                kind: PermissionOverwriteType::Role,
                allow: Some(Permissions::from_bits_retain(allow & !SILENCE)),
                deny: Some(Permissions::from_bits_retain(deny | SILENCE)),
            };
            app.http
                .update_channel_permission(Id::new(*channel), &o)
                .await?;
        }
        Op::CreateLockedChannel => {
            // @everyone may read it but not write, so members see why the server went quiet.
            let everyone = twilight_model::channel::permission_overwrite::PermissionOverwrite {
                id: gid.cast(),
                kind: twilight_model::channel::permission_overwrite::PermissionOverwriteType::Role,
                allow: Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY,
                deny: Permissions::SEND_MESSAGES,
            };
            let ch = app
                .http
                .create_guild_channel(gid, "server-locked")
                .permission_overwrites(&[everyone])
                .await?
                .model()
                .await?;
            app.http
                .create_message(ch.id)
                .embeds(&[locked_embed(app, gid, &[]).await])
                .await?;
            return Ok(Some(ch.id.get()));
        }
        // No-op: the saved snapshot records the join lock and `join_lock` reads it from there.
        Op::EnableJoinLock(_) => {}
        Op::PauseInvites => {
            let now = now_secs() * 1000;
            app.api
                .pause_invites(gid.get(), Some((now, now + MAX_INVITE_PAUSE_MS)))
                .await?;
        }
    }
    Ok(None)
}

/// Locks the guild unless an active lock already covers the same part (its snapshot would save
/// locked perms).
pub async fn lock(app: &App, gid: Id<GuildMarker>, kind: LockKind) -> Result<bool> {
    lock_for(app, gid, kind, None).await
}

/// The longest timed lock: 30 days.
const MAX_LOCK_SECS: i64 = 30 * 86_400;

/// [`lock`] that lifts itself after `secs`. The timer is queued before any lock step runs, so a
/// lock that a restart cut off and [`resume`] finished still ends on time.
pub async fn lock_for(
    app: &App,
    gid: Id<GuildMarker>,
    kind: LockKind,
    secs: Option<i64>,
) -> Result<bool> {
    lock_saving(app, gid, kind, secs, &mut false).await
}

/// [`lock_for`] that sets `saved` once this call saved its snapshot: an error after that leaves a
/// (half applied) lock of this call's, an error before it leaves none.
pub async fn lock_saving(
    app: &App,
    gid: Id<GuildMarker>,
    kind: LockKind,
    secs: Option<i64>,
    saved: &mut bool,
) -> Result<bool> {
    lock_marked(app, gid, kind, secs, saved, None).await
}

/// [`lock_saving`] that, with `panic_key`, marks the lock as panic's own (`panic::own_lock`) right
/// after its snapshot is saved, while no other lock or unlock of the guild can run: an unlock or a
/// second lock then always sees the mark.
pub async fn lock_marked(
    app: &App,
    gid: Id<GuildMarker>,
    kind: LockKind,
    secs: Option<i64>,
    saved: &mut bool,
    panic_key: Option<&str>,
) -> Result<bool> {
    // A typed or dashboard duration has no upper bound; a huge one would overflow the end time.
    let secs = secs.map(|s| s.min(MAX_LOCK_SECS));
    if lock_thread(app, gid, &kind, true).await? {
        // A thread lock keeps no snapshot, but its timer still lifts it. An older timer would lift
        // this newer lock, so it goes first.
        forget_timers(app, gid, &kind).await;
        if let Some(d) = secs {
            let job = serde_json::json!({ "lock": kind });
            brim_core::jobs::enqueue(&app.pool, gid.get() as i64, "unlock", job, now_secs() + d)
                .await?;
        }
        return Ok(true);
    }
    // Held until the lock is applied: a second lock waits, then sees this one as active.
    let _one = exclusive(app, gid).await;
    let active: Vec<LockKind> = store::locks(&app.pool, gid.get())
        .await?
        .into_iter()
        .filter_map(|s| s.kind)
        .collect();
    // A combined lock takes the parts no active lock has; its timer names what it took.
    let Some(kind) = kind.rest(&active) else {
        return Ok(false);
    };
    let plan = plan_now(app, gid, &kind).await?;
    // Saved first so a failure halfway can still be unlocked exactly, or finished after a restart.
    let r = store::save_lock(&app.pool, gid.get(), &plan.snapshot).await;
    forget_locks(gid.get());
    r?;
    *saved = true;
    if let Some(key) = panic_key {
        crate::panic::own_lock(app, gid, key, true).await;
    }
    if let Some(d) = secs {
        let job = serde_json::json!({ "lock": kind });
        brim_core::jobs::enqueue(&app.pool, gid.get() as i64, "unlock", job, now_secs() + d)
            .await?;
    }
    apply(app, gid, plan).await?;
    announce(app, gid, state_log(false)).await;
    Ok(true)
}

/// Lock and unlock of one guild run one at a time. The dashboard only queues jobs and this bot
/// runs one process, so an in-process lock covers every writer of `lock_snapshots`.
async fn exclusive(app: &App, gid: Id<GuildMarker>) -> tokio::sync::OwnedMutexGuard<()> {
    let m = app
        .lock_ops
        .lock()
        .unwrap()
        .entry(gid.get())
        .or_default()
        .clone();
    m.lock_owned().await
}

/// The lock plan for the guild as it is now.
async fn plan_now(app: &App, gid: Id<GuildMarker>, kind: &LockKind) -> Result<Plan> {
    let cfg = app.cfg(gid.get()).await?;
    let roles = crate::hierarchy::ranked(gid, app.http.roles(gid).await?.models().await?);
    let mine = app
        .http
        .guild_member(gid, app.me)
        .await?
        .model()
        .await?
        .roles;
    let bot_position = roles
        .iter()
        .filter(|r| mine.contains(&r.id))
        .map(|r| r.position as i32)
        .max()
        .unwrap_or(0);
    let info: Vec<RoleInfo> = roles
        .iter()
        .map(|r| RoleInfo {
            id: r.id.get(),
            perms: r.permissions.bits(),
            position: r.position as i32,
            managed: r.managed,
        })
        .collect();
    let main_roles: Vec<u64> = cfg.lockdown.main_roles.iter().map(|r| r.get()).collect();
    let mut plan = plan(
        kind,
        gid.get(),
        &main_roles,
        &info,
        bot_position,
        cfg.lockdown_prefs.voice,
    );
    if let LockKind::Channel(c) = *kind {
        let ch = app.http.channel(Id::new(c)).await?.model().await?;
        plan.snapshot.prior = ch
            .permission_overwrites
            .unwrap_or_default()
            .iter()
            .filter(|o| plan.snapshot.channel_denies.contains(&(c, o.id.get())))
            .map(|o| (c, o.id.get(), o.allow.bits(), o.deny.bits()))
            .collect();
    }
    if !cfg.lockdown_prefs.pause_invites {
        plan.ops.retain(|o| *o != Op::PauseInvites);
        plan.snapshot.invites_paused = false;
    }
    // So the unlock lifts only the pause the lock set. A failed read must not stop the lock.
    if plan.snapshot.invites_paused {
        let read = tokio::time::timeout(Duration::from_secs(2), app.api.pauses(gid.get())).await;
        match read {
            Ok(Ok(p)) => plan.snapshot.prior_pauses = p,
            Ok(Err(e)) => tracing::warn!(?e, "reading the invite pause before a lock failed"),
            Err(_) => tracing::warn!("reading the invite pause before a lock took over 2 s"),
        }
    }
    Ok(plan)
}

/// Runs the ops of a saved lock, then marks it done. Every op is safe to run twice.
async fn apply(app: &App, gid: Id<GuildMarker>, mut plan: Plan) -> Result<()> {
    for op in &plan.ops {
        if *op == Op::CreateLockedChannel && plan.snapshot.locked_channel.is_some() {
            continue;
        }
        if let Some(created) = exec(app, gid, op, &plan.snapshot).await? {
            plan.snapshot.locked_channel = Some(created);
            let r = store::save_lock(&app.pool, gid.get(), &plan.snapshot).await;
            forget_locks(gid.get());
            r?;
        }
    }
    Ok(store::lock_done(&app.pool, gid.get(), &plan.snapshot).await?)
}

/// Finishes the locks and unlocks a restart cut off. A lock keeps its saved snapshot (the live
/// perms may already be half locked) and runs its ops again.
pub async fn resume(app: &App) -> Result<()> {
    for (guild, snap, unlocking) in store::interrupted(&app.pool).await? {
        let gid = Id::new(guild);
        // `interrupted` flipped cut-off unlocks back to locked.
        forget_locks(guild);
        let r = async {
            match (unlocking, &snap.kind) {
                (true, kind) => unlock(app, gid, kind.as_ref()).await.map(drop),
                (false, Some(k)) => {
                    let ops = plan_now(app, gid, k).await?.ops;
                    apply(
                        app,
                        gid,
                        Plan {
                            ops,
                            snapshot: snap.clone(),
                        },
                    )
                    .await
                }
                (false, None) => Ok(store::lock_done(&app.pool, guild, &snap).await?),
            }
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(?e, guild, unlocking, "resuming an interrupted lock failed");
        }
    }
    Ok(())
}

/// Threads cannot have permission overwrites, so a channel lock there locks the thread itself.
/// Returns false when `kind` is not a thread's channel lock.
async fn lock_thread(
    app: &App,
    gid: Id<GuildMarker>,
    kind: &LockKind,
    locked: bool,
) -> Result<bool> {
    let LockKind::Channel(c) = *kind else {
        return Ok(false);
    };
    let c = Id::new(c);
    let ch = app.http.channel(c).await?.model().await?;
    // A typed `<#id>` can point anywhere; never touch a channel of another server.
    anyhow::ensure!(
        ch.guild_id == Some(gid),
        "<#{c}> is not a channel of this server"
    );
    if !ch.kind.is_thread() {
        return Ok(false);
    }
    app.http.update_thread(c).locked(locked).await?;
    Ok(true)
}

/// Kinds of the guild's active locks, oldest first.
pub async fn active(app: &App, gid: Id<GuildMarker>) -> Result<Vec<LockKind>> {
    Ok(store::locks(&app.pool, gid.get())
        .await?
        .into_iter()
        .filter_map(|s| s.kind)
        .collect())
}

/// Guild info whose role permissions include what an active lock saved: a Role lock strips
/// them, and a role it stripped is dangerous again once the lock ends. For every check that judges
/// a role dangerous; a failed lock read fails the check.
pub async fn unlocked_info(
    app: &App,
    gid: Id<GuildMarker>,
) -> Result<std::sync::Arc<crate::hierarchy::GuildInfo>> {
    let info = app.guild_info(gid).await?;
    Ok(with_saved(info, &active_locks(app, gid.get()).await?))
}

/// The active locks of each guild, kept in memory: dangerous-role checks run per join and per
/// audit entry, and a raid is when locks are on. Every lock write in this module forgets the
/// guild (`forget_locks`); a read that raced a forget is not kept (generation).
type LockCache = (
    u64,
    std::collections::BTreeMap<u64, std::sync::Arc<Vec<Snapshot>>>,
);
static LOCKS: std::sync::Mutex<LockCache> =
    std::sync::Mutex::new((0, std::collections::BTreeMap::new()));

pub(crate) async fn active_locks(app: &App, guild: u64) -> Result<std::sync::Arc<Vec<Snapshot>>> {
    let generation = {
        let c = LOCKS.lock().unwrap();
        if let Some(l) = c.1.get(&guild) {
            return Ok(l.clone());
        }
        c.0
    };
    let locks = std::sync::Arc::new(store::locks(&app.pool, guild).await?);
    let mut c = LOCKS.lock().unwrap();
    if c.0 == generation {
        c.1.insert(guild, locks.clone());
    }
    Ok(locks)
}

/// After any write to a guild's `lock_snapshots`.
pub(crate) fn forget_locks(guild: u64) {
    let mut c = LOCKS.lock().unwrap();
    c.0 += 1;
    c.1.remove(&guild);
}

fn with_saved(
    info: std::sync::Arc<crate::hierarchy::GuildInfo>,
    locks: &[Snapshot],
) -> std::sync::Arc<crate::hierarchy::GuildInfo> {
    let saved: Vec<&(u64, u64)> = locks.iter().flat_map(|l| &l.role_perms).collect();
    if saved.is_empty() {
        return info;
    }
    let mut i = (*info).clone();
    for (r, p) in saved {
        if let Some(live) = i.role_perms.get_mut(&Id::new(*r)) {
            *live |= p;
        }
    }
    std::sync::Arc::new(i)
}

/// Role permissions with what the active locks saved put back ([`unlocked_info`]).
pub async fn unlocked_role_perms(
    app: &App,
    gid: Id<GuildMarker>,
) -> Result<std::collections::HashMap<Id<twilight_model::id::marker::RoleMarker>, u64>> {
    Ok(unlocked_info(app, gid).await?.role_perms.clone())
}

/// `perms` (live role permissions) with what the active locks saved added, as [`unlocked_info`]
/// does: a role a lock stripped counts with its saved bits.
pub async fn unlocked(
    app: &App,
    gid: Id<GuildMarker>,
    mut perms: std::collections::HashMap<Id<twilight_model::id::marker::RoleMarker>, u64>,
) -> Result<std::collections::HashMap<Id<twilight_model::id::marker::RoleMarker>, u64>> {
    for snap in active_locks(app, gid.get()).await?.iter() {
        for &(r, p) in &snap.role_perms {
            if let Some(live) = Id::new_checked(r).and_then(|r| perms.get_mut(&r)) {
                *live |= p;
            }
        }
    }
    Ok(perms)
}

/// `channels` with the overwrites the active channel locks changed put back as they were (an
/// overwrite a lock created is dropped), oldest lock first, like [`unlocked`] for roles.
pub async fn unlocked_channels(
    app: &App,
    gid: Id<GuildMarker>,
    channels: &mut [twilight_model::channel::Channel],
) -> Result<()> {
    let mut done = std::collections::HashSet::new();
    for snap in active_locks(app, gid.get()).await?.iter() {
        for &(channel, role) in &snap.channel_denies {
            if !done.insert((channel, role)) {
                continue;
            }
            let Some(c) = channels.iter_mut().find(|c| c.id.get() == channel) else {
                continue;
            };
            let all = c.permission_overwrites.get_or_insert_with(Vec::new);
            all.retain(|o| o.id.get() != role);
            if let Some(&(_, _, allow, deny)) =
                snap.prior.iter().find(|p| p.0 == channel && p.1 == role)
            {
                all.push(twilight_model::channel::permission_overwrite::PermissionOverwrite {
                    id: Id::new(role),
                    kind: twilight_model::channel::permission_overwrite::PermissionOverwriteType::Role,
                    allow: Permissions::from_bits_retain(allow),
                    deny: Permissions::from_bits_retain(deny),
                });
            }
        }
    }
    Ok(())
}

/// A startup check that waits out a raid or lock: `kind` is one job per guild.
pub struct Deferred {
    pub kind: &'static str,
    /// When the first wait began (unix secs), from the job or the pending one.
    pub first: i64,
    /// Failed runs so far; the check stops retrying after [`MAX_TRIES`].
    pub tries: i64,
}

/// Longest a [`Deferred`] check waits before it runs anyway.
const MAX_WAIT_SECS: i64 = 1800;
/// Failed runs before a [`Deferred`] check gives up until the next GuildCreate.
pub const MAX_TRIES: i64 = 5;

impl Deferred {
    /// `first` and `tries` from the job's payload, else from a pending job (a GuildCreate must
    /// not restart the cap or the count).
    pub async fn new(
        app: &App,
        gid: Id<GuildMarker>,
        kind: &'static str,
        job: Option<&serde_json::Value>,
    ) -> Self {
        let payload = match job {
            Some(p) => Some(p.clone()),
            None => sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2 LIMIT 1",
            )
            .bind(gid.get() as i64)
            .bind(kind)
            .fetch_optional(&app.pool)
            .await
            .ok()
            .flatten(),
        };
        let field = |k: &str| payload.as_ref().and_then(|p| p[k].as_i64());
        Deferred {
            kind,
            first: field("first").unwrap_or_else(now_secs),
            tries: field("tries").unwrap_or(0),
        }
    }

    /// True while a raid or lock is on and the 30 minutes are not up. A failed check runs.
    pub async fn waits(&self, app: &App, gid: Id<GuildMarker>) -> bool {
        match raid_or_lock(app, gid).await {
            Ok(on) => on && now_secs() - self.first < MAX_WAIT_SECS,
            Err(e) => {
                tracing::warn!(?e, "raid or lock check failed, running anyway");
                false
            }
        }
    }

    /// Runs the check again in 5 minutes, keeping `first`. `failed`: a failed run, counted; past
    /// [`MAX_TRIES`] staff hear of it and nothing is queued (the next GuildCreate runs it again).
    pub async fn later(&self, app: &App, gid: Id<GuildMarker>, failed: bool) -> Result<()> {
        let tries = self.tries + i64::from(failed);
        if tries >= MAX_TRIES {
            tracing::warn!(
                guild = gid.get(),
                kind = self.kind,
                tries,
                "startup check gave up"
            );
            let log = startup_failed_log(self.kind, tries);
            if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
                tracing::warn!(?e, "startup check give-up log failed");
            }
            // The claimed row goes too, so the scheduler does not retry it either.
            sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2")
                .bind(gid.get() as i64)
                .bind(self.kind)
                .execute(&app.pool)
                .await?;
            return Ok(());
        }
        let payload = serde_json::json!({ "first": self.first, "tries": tries });
        brim_core::jobs::replace(
            &app.pool,
            gid.get() as i64,
            self.kind,
            payload,
            now_secs() + 300,
        )
        .await
    }
}

/// The Lockdown Updates log of a lock (`lift` false) or unlock.
pub(crate) fn state_log(lift: bool) -> Log {
    if lift {
        Log::new("Server Unlocked", DONE)
            .entry("log.mod_command.server_unlocked")
            .text(&format!("{YES} **The server has been unlocked.**"))
    } else {
        Log::new("Server Locked", ALERT)
            .entry("log.mod_command.server_locked")
            .text(&format!("{YES} **The server has been locked down.**"))
    }
}

pub(crate) fn incomplete_log(failed: impl std::fmt::Display) -> Log {
    Log::new("Unlock Incomplete", ALERT)
        .text(&format!("`{failed}` permission changes could not be undone. Check the bot's role and permissions, then fix them by hand."))
        .var("tries", &failed.to_string())
}

pub(crate) fn startup_failed_log(kind: &str, tries: i64) -> Log {
    Log::new("Scheduled Action Failed", RED)
        .entry("log.server.startup_check_failed")
        .line("Action", &format!("`{kind}`"))
        .text(&format!(
            "Gave up after {tries} failed tries. It runs again when Brim reconnects; check the bot log."
        ))
        .var("job", kind)
        .var("tries", &tries.to_string())
}

/// A join raid is running or a lock is on: member edits and new tickets wait.
pub async fn raid_or_lock(app: &App, gid: Id<GuildMarker>) -> Result<bool> {
    let now = crate::automod::now_ms();
    let raid = app
        .raids
        .lock()
        .unwrap()
        .get(&gid.get())
        .is_some_and(|r| r.active(now));
    Ok(raid || !active(app, gid).await?.is_empty())
}

/// Join lock of the guild's stored snapshot, if it has one.
pub async fn join_lock(app: &App, gid: Id<GuildMarker>) -> Result<Option<JoinLock>> {
    load_join_lock(&app.pool, gid.get()).await
}

async fn load_join_lock(pool: &sqlx::PgPool, guild: u64) -> Result<Option<JoinLock>> {
    let locks = store::locks(pool, guild).await?;
    Ok(locks
        .iter()
        .find_map(|s| s.kind.as_ref().and_then(join_lock_of)))
}

fn join_lock_of(kind: &LockKind) -> Option<JoinLock> {
    kind.covers().2
}

async fn announce(app: &App, gid: Id<GuildMarker>, log: Log) {
    let on = app
        .cfg(gid.get())
        .await
        .is_ok_and(|c| c.lockdown_prefs.updates);
    if on {
        if let Err(e) = send_log(app, gid.get(), LogKind::ModCommand, log).await {
            tracing::warn!(?e, "lockdown update log failed");
        }
    }
}

/// A lifted lock takes its end with it: its timed unlock job, and panic mode's unlock when panic
/// made it. Otherwise either would lift a later lock of the same part. Only logged on failure.
async fn forget_timers(app: &App, gid: Id<GuildMarker>, kind: &LockKind) {
    let timer = sqlx::query(
        "DELETE FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'unlock' AND payload->'lock' = $2",
    )
    .bind(gid.get() as i64)
    .bind(sqlx::types::Json(kind))
    .execute(&app.pool)
    .await;
    if let Err(e) = timer {
        tracing::warn!(?e, "dropping the timed unlock failed");
    }
    if kind.overlaps(&LockKind::Role) {
        crate::panic::own_lock(app, gid, crate::panic::ROLE_LOCK, false).await;
    }
    if kind.overlaps(&LockKind::Server) {
        crate::panic::own_lock(app, gid, crate::panic::SERVER_LOCK, false).await;
    }
}

/// A role a restore made again (old id -> new id) under an active lock comes with the backup's
/// permissions, so it would be the one main role still writing. Locks it as the lock locked the
/// old one and moves the snapshot's entry to the new id, so the unlock gives it back. Only logged.
pub async fn relink_roles(
    app: &App,
    gid: Id<GuildMarker>,
    map: &std::collections::HashMap<u64, u64>,
) {
    if let Err(e) = relink_roles_now(app, gid, map).await {
        tracing::warn!(?e, "locking recreated roles failed");
    }
}

async fn relink_roles_now(
    app: &App,
    gid: Id<GuildMarker>,
    map: &std::collections::HashMap<u64, u64>,
) -> Result<()> {
    let _one = exclusive(app, gid).await;
    let locks = store::locks(&app.pool, gid.get()).await?;
    if !locks
        .iter()
        .any(|s| s.role_perms.iter().any(|(r, _)| map.contains_key(r)))
    {
        return Ok(());
    }
    let live: Vec<(u64, u64)> = app
        .http
        .roles(gid)
        .await?
        .models()
        .await?
        .iter()
        .map(|r| (r.id.get(), r.permissions.bits()))
        .collect();
    // Every lock's entry moves to the new id first, so a failed PATCH or a crash still leaves an
    // unlock that knows the role; then one PATCH per role takes what all its locks take.
    let mut strip: std::collections::HashMap<u64, (u64, u64)> = Default::default();
    for mut snap in locks {
        let takes = snap.takes();
        let mut moved = false;
        for entry in snap.role_perms.iter_mut() {
            let Some(&new) = map.get(&entry.0) else {
                continue;
            };
            let Some(&(_, now)) = live.iter().find(|(r, _)| *r == new) else {
                continue;
            };
            *entry = (new, now);
            strip.entry(new).or_insert((now, 0)).1 |= takes;
            moved = true;
        }
        if moved {
            // Not `save_lock`: the lock keeps its state and its age, which tells it apart.
            let r = store::update_lock(&app.pool, gid.get(), &snap).await;
            forget_locks(gid.get());
            r?;
        }
    }
    for (role, (now, takes)) in strip {
        let op = Op::SetRolePerms {
            role,
            perms: now & !takes,
        };
        exec(app, gid, &op, &Snapshot::default()).await?;
    }
    Ok(())
}

/// Lifts the active locks that overlap `which`, or all of them; returns how many were lifted.
/// A lock lifted during panic that covered a part panic does not hold is taken again by panic.
pub async fn unlock(app: &App, gid: Id<GuildMarker>, which: Option<&LockKind>) -> Result<usize> {
    let before = crate::panic::held(app, gid).await;
    let (n, lifted) = unlock_now(app, gid, which).await?;
    if let Some(waiting) = before {
        crate::panic::relock(app, gid, waiting, &lifted).await;
    }
    Ok(n)
}

/// [`unlock`] without panic taking anything back: how many locks it lifted, and their kinds.
pub(crate) async fn unlock_now(
    app: &App,
    gid: Id<GuildMarker>,
    which: Option<&LockKind>,
) -> Result<(usize, Vec<LockKind>)> {
    if let Some(k) = which {
        if lock_thread(app, gid, k, false).await? {
            forget_timers(app, gid, k).await;
            return Ok((1, vec![k.clone()]));
        }
    }
    let _one = exclusive(app, gid).await;
    let (mut lifted, mut failed) = (0, 0);
    let mut kinds = vec![];
    for snap in store::locks(&app.pool, gid.get()).await? {
        let hit = match (which, &snap.kind) {
            (Some(w), Some(k)) => w.overlaps(k),
            (Some(_), None) => false,
            (None, _) => true,
        };
        if !hit {
            continue;
        }
        // Read before the snapshot is taken, so a failed read leaves the lock to retry.
        let current: Vec<(u64, u64)> = app
            .http
            .roles(gid)
            .await?
            .models()
            .await?
            .iter()
            .map(|r| (r.id.get(), r.permissions.bits()))
            .collect();
        let took = store::take_lock(&app.pool, gid.get(), &snap).await;
        forget_locks(gid.get());
        if !took? {
            continue;
        }
        // The lock is claimed now, so one failed step must not leave the rest locked. A deleted
        // channel or role has nothing to restore.
        let mut step = |r: Result<()>| match r {
            Err(e) if !crate::http_err::gone_any(&e) => {
                tracing::warn!(?e, "unlock step failed");
                failed += 1;
            }
            _ => {}
        };
        for op in unlock_ops(&snap, &current) {
            step(exec(app, gid, &op, &snap).await.map(drop));
        }
        if snap.invites_paused {
            let now = now_secs() * 1000;
            step(app.api.set_pauses(gid.get(), now, snap.prior_pauses).await);
        }
        for &(channel, role) in &snap.channel_denies {
            crate::verify::given::mark_own(gid, channel);
            let r = match snap.prior.iter().find(|p| p.0 == channel && p.1 == role) {
                Some(&(_, _, allow, deny)) => {
                    let o = PermissionOverwrite {
                        id: Id::new(role),
                        kind: PermissionOverwriteType::Role,
                        allow: Some(Permissions::from_bits_retain(allow)),
                        deny: Some(Permissions::from_bits_retain(deny)),
                    };
                    app.http
                        .update_channel_permission(Id::new(channel), &o)
                        .await
                }
                None => {
                    app.http
                        .delete_channel_permission(Id::new(channel))
                        .role(Id::new(role))
                        .await
                }
            };
            step(r.map(drop).map_err(Into::into));
        }
        if let Some(c) = snap.locked_channel {
            crate::http_err::best_effort(
                app.http.delete_channel(Id::new(c)).await,
                "delete lock channel",
            );
        }
        let r = store::drop_lock(&app.pool, gid.get(), &snap).await;
        forget_locks(gid.get());
        r?;
        if let Some(k) = &snap.kind {
            forget_timers(app, gid, k).await;
        }
        kinds.extend(snap.kind.clone());
        lifted += 1;
    }
    if failed > 0 {
        let log = incomplete_log(failed);
        if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
            tracing::warn!(?e, "could not log the incomplete unlock");
        }
    }
    if lifted > 0 {
        if active(app, gid).await?.is_empty() {
            sqlx::query("DELETE FROM lock_updates WHERE guild_id=$1")
                .bind(gid.get() as i64)
                .execute(&app.pool)
                .await?;
        }
        announce(app, gid, state_log(true)).await;
    }
    Ok((lifted, kinds))
}

/// `lock MESSAGE ?t update`: adds the message as UPDATE n to the blind lock's notice, or, without a
/// blind lock, posts the last five updates in `channel`.
pub async fn update(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
    (actor_id, actor): (Id<UserMarker>, &brim_core::permits::Level),
    text: &str,
) -> Result<crate::reply::Said> {
    let said = |key: &'static str| crate::reply::say(app, gid, key, &[]);
    if !actor.has("lock_updates") {
        return Ok(said("reply.lock.update_no_permit").await);
    }
    if let Some(why) = crate::commands::gated(app, gid, actor_id, actor, "lock_updates").await? {
        return Ok(crate::reply::Said {
            text: why,
            embeds: vec![],
            edited: false,
        });
    }
    let locks = store::locks(&app.pool, gid.get()).await?;
    if locks.is_empty() {
        return Ok(said("reply.lock.nothing_locked").await);
    }
    let g = gid.get() as i64;
    sqlx::query("INSERT INTO lock_updates (guild_id, author_id, message) VALUES ($1,$2,$3)")
        .bind(g)
        .bind(actor_id.get() as i64)
        .bind(embed::clip(text, 500))
        .execute(&app.pool)
        .await?;
    if let Some(ch) = locks.iter().find_map(|s| s.locked_channel) {
        refresh_notice(app, gid, Id::new(ch)).await?;
        return Ok(said("reply.lock.update_added").await);
    }
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT message, EXTRACT(EPOCH FROM created_at)::bigint FROM lock_updates
         WHERE guild_id=$1 ORDER BY id DESC LIMIT 5",
    )
    .bind(g)
    .fetch_all(&app.pool)
    .await?;
    let list: Vec<String> = rows
        .iter()
        .rev()
        .map(|(m, at)| format!("{ARROW} <t:{at}:R> {m}"))
        .collect();
    let notice = crate::reply::bot_embed_for(
        app,
        gid,
        "reply.lock.updates_notice",
        &[("text", &list.join("\n"))],
    )
    .await;
    let r = app.http.create_message(channel).embeds(&[notice]).await;
    crate::http_err::best_effort(r, "post lock update");
    Ok(said("reply.lock.update_posted").await)
}

/// Rewrites the blind lock's notice in `ch` with every stored update as UPDATE n.
async fn refresh_notice(app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>) -> Result<()> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT author_id, message FROM lock_updates WHERE guild_id=$1 ORDER BY id")
            .bind(gid.get() as i64)
            .fetch_all(&app.pool)
            .await?;
    let e = locked_embed(app, gid, &rows).await;
    // The channel only holds the notice: nobody else may write there.
    let notice = app
        .http
        .channel_messages(ch)
        .after(Id::new(1))
        .limit(1)
        .await?
        .models()
        .await?;
    match notice.first() {
        Some(m) => {
            app.http
                .update_message(ch, m.id)
                .embeds(Some(std::slice::from_ref(&e)))
                .await?;
        }
        None => {
            app.http
                .create_message(ch)
                .embeds(std::slice::from_ref(&e))
                .await?;
        }
    }
    Ok(())
}

/// Dashboard "Send an update" during a blind lock (job `lock_update`): stores it and refreshes the notice.
pub async fn dashboard_update(app: &App, gid: Id<GuildMarker>, by: u64, text: &str) -> Result<()> {
    let locks = store::locks(&app.pool, gid.get()).await?;
    let Some(ch) = locks.iter().find_map(|s| s.locked_channel) else {
        return Ok(());
    };
    sqlx::query("INSERT INTO lock_updates (guild_id, author_id, message) VALUES ($1,$2,$3)")
        .bind(gid.get() as i64)
        .bind(by as i64)
        .bind(embed::clip(text, 500))
        .execute(&app.pool)
        .await?;
    refresh_notice(app, gid, Id::new(ch)).await
}

/// The red notice in `#server-locked`, with every update (author id, text) numbered, newest last.
/// Only the last six fit, so the numbers keep counting past the dropped ones.
pub async fn locked_embed(app: &App, gid: Id<GuildMarker>, updates: &[(i64, String)]) -> Embed {
    crate::reply::bot_embed_for(
        app,
        gid,
        "reply.lock.locked_notice",
        &[("text", &updates_text(updates))],
    )
    .await
}

/// The `{text}` of the lock notice: the last six updates, numbered, each after a blank line.
fn updates_text(updates: &[(i64, String)]) -> String {
    let skip = updates.len().saturating_sub(6);
    updates
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(n, (author, text))| {
            format!("\n📌 **UPDATE {}** (<@{author}>):\n{ARROW} {text}", n + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn lock_name(k: &LockKind) -> String {
    match k {
        LockKind::Server => "channels".into(),
        LockKind::Blind => "channels (blind)".into(),
        LockKind::Channel(c) => format!("<#{c}>"),
        LockKind::Role => "roles".into(),
        LockKind::Join(j) => format!("joins ({j:?})").to_lowercase(),
        LockKind::All => "everything".into(),
        LockKind::Mixed {
            channels,
            roles,
            joins,
        } => [
            (*channels, "channels"),
            (*roles, "roles"),
            (*joins, "joins"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(" + "),
    }
}

pub const LOCK_TYPES: &str =
    "Lock type: server (sc), blind, role (r), channel (c), kick (k), ban (b) or all (s)";

/// A lock command's answer: `text` for compact replies, `embed` otherwise.
pub struct Reply {
    pub text: String,
    pub embed: Embed,
    /// The Bot Messages entry of the embed and its values.
    pub(crate) entry: Entry,
}

/// An answer's entry key and the values an edited version of it fills.
pub(crate) type Entry = (&'static str, Vec<(&'static str, String)>);

/// The server's edited version of answer entry `key` in place of the built `embed`, when it has one.
async fn own(app: &App, gid: Id<GuildMarker>, (key, vars): &Entry, embed: Embed) -> Embed {
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let said = crate::reply::say(app, gid, key, &vars).await;
    match said.edited {
        true => said.embeds.into_iter().next().unwrap_or(embed),
        false => embed,
    }
}

impl Reply {
    async fn own(mut self, app: &App, gid: Id<GuildMarker>) -> Self {
        self.embed = own(app, gid, &self.entry, self.embed).await;
        self
    }
}

pub(crate) fn fail(lift: bool, text: String) -> Reply {
    let (title, key) = if lift {
        ("Unlock Result:", "reply.lock.unlock_result")
    } else {
        ("Lock Result:", "reply.lock.lock_result")
    };
    let embed = Log::new(title, BLUE)
        .text(&format!("{ARROW} {text}"))
        .build();
    let entry = (key, vec![("result", text.clone())]);
    Reply { text, embed, entry }
}

/// [`fail`] with the sentence of the short reply entry `key`. An edited version with an embed
/// replaces the result embed; one with only text goes into the usual result embed.
async fn fail_key(
    app: &App,
    gid: Id<GuildMarker>,
    lift: bool,
    key: &str,
    vars: &[(&str, &str)],
) -> Reply {
    let said = crate::reply::say(app, gid, key, vars).await;
    let r = fail(lift, said.text);
    match said.edited {
        true => Reply {
            embed: said.embeds.into_iter().next().unwrap_or(r.embed),
            ..r
        },
        false => r.own(app, gid).await,
    }
}

/// Shown while a server-wide lock or unlock runs; replaced by the result.
pub async fn loading(
    app: &App,
    gid: Id<GuildMarker>,
    kind: Option<&LockKind>,
    lift: bool,
) -> Embed {
    let (embed, entry) = loading_embed(kind, lift);
    own(app, gid, &entry, embed).await
}

pub(crate) fn loading_embed(kind: Option<&LockKind>, lift: bool) -> (Embed, Entry) {
    let name = kind.map_or("all".into(), type_name);
    let (title, verb, key) = if lift {
        ("Brim Unlocking:", "Disabling", "reply.lock.unlocking")
    } else {
        ("Brim Lockdown:", "Enabling", "reply.lock.locking")
    };
    let embed = Log::new(title, BLUE)
        .text(&format!("{RUNNING} {verb} `{name}` lockdown!"))
        .build();
    (embed, (key, vec![("lock_type", name)]))
}

/// `channels (blind)` -> `Channels (blind)`, as the Type line shows it.
fn type_name(k: &LockKind) -> String {
    let n = lock_name(k);
    let mut c = n.chars();
    c.next()
        .map_or(n.clone(), |f| f.to_uppercase().chain(c).collect())
}

/// The finished lock or unlock, with how long it took.
pub(crate) fn finished(
    kind: Option<&LockKind>,
    name: String,
    lift: bool,
    took: Duration,
    actor: Id<UserMarker>,
) -> Reply {
    if let Some(LockKind::Channel(c)) = kind {
        let (color, icon, done, body, key) = if lift {
            (
                GREEN,
                UNLOCK,
                "unlocked",
                "Everyone now has access to this channel.",
                "reply.lock.channel_unlocked",
            )
        } else {
            (
                RED,
                LOCK,
                "locked",
                "You will gain access again once the lockdown is lifted.",
                "reply.lock.channel_locked",
            )
        };
        let embed = Log::new("", color)
            .text(&format!("{icon} **<#{c}> has been {done}!**"))
            .text(body)
            .build();
        return Reply {
            text: format!("<#{c}> has been {done}"),
            embed,
            entry: (key, vec![("channel", format!("<#{c}>"))]),
        };
    }
    let ms = took.as_secs_f64() * 1000.0;
    let (title, icon, line, ok, verb, key) = if lift {
        (
            "Brim Unlocking:",
            UNLOCK,
            "has been lifted",
            "Unlocking",
            "unlocking",
            "reply.lock.unlocked",
        )
    } else {
        (
            "Brim Lockdown:",
            LOCK,
            "has been successfully enforced",
            "Lockdown",
            "locking down",
            "reply.lock.locked",
        )
    };
    let embed = Log::new(title, GREEN)
        .text(&format!(
            "{icon} `{name}` lockdown mode {line} in `{ms:.0} ms`."
        ))
        .text("")
        .text(&format!("{YES} **{ok} successful!**"))
        .text(&format!(
            "{ARROW} *Brim has finished {verb} in `{ms:.2} ms`.*"
        ))
        .text(&format!("{MODERATOR} **Staff:** <@{actor}> `{actor}`"))
        .line("Type", &name)
        .build();
    let entry = vec![
        ("lock_type", name.clone()),
        ("took", format!("{ms:.0}")),
        ("took_exact", format!("{ms:.2}")),
        ("moderator", format!("<@{actor}> `{actor}`")),
    ];
    Reply {
        text: format!("{ok} successful: {name} ({ms:.0} ms)"),
        embed,
        entry: (key, entry),
    }
}

/// A bare prefix `lock`: the active locks, or how to add one.
pub async fn overview(app: &App, gid: Id<GuildMarker>) -> Result<Embed> {
    let active = active(app, gid).await?;
    let prefix = app.cfg(gid.get()).await?.misc.prefix.clone();
    let (embed, entry) = overview_embed(&active, &prefix);
    Ok(own(app, gid, &entry, embed).await)
}

pub(crate) fn overview_embed(active: &[LockKind], prefix: &str) -> (Embed, Entry) {
    let mut log = Log::new("Active lockdown modes:", RED);
    if active.is_empty() {
        log = log
            .text(&format!(
                "{NO} There are no active lockdown modes currently!"
            ))
            .text("")
            .text(&format!("{ANNOUNCE} **Add a lockdown:**"))
            .text(&format!(
                "You can add a lockdown by doing `{prefix}lock ?t [TYPE]`"
            ));
    }
    let locks: Vec<String> = active
        .iter()
        .map(|k| format!("{LOCK} {}", type_name(k)))
        .collect();
    for l in &locks {
        log = log.text(l);
    }
    let entry = match active.is_empty() {
        true => (
            "reply.lock.none_active",
            vec![("prefix", prefix.to_string())],
        ),
        false => ("reply.lock.active", vec![("locks", locks.join("\n"))]),
    };
    (log.build(), entry)
}

/// Lock or unlock the server; shared by the prefix and slash commands. `kind` overrides the dashboard's lock type.
pub async fn command(
    app: &App,
    gid: Id<GuildMarker>,
    (actor_id, actor): (Id<UserMarker>, &brim_core::permits::Level),
    lift: bool,
    kind: Option<LockKind>,
    duration_s: Option<i64>,
) -> Result<Reply> {
    if !actor.has("lockdown") {
        let key = if lift {
            "reply.lock.no_permit_unlock"
        } else {
            "reply.lock.no_permit_lock"
        };
        return Ok(fail_key(app, gid, lift, key, &[]).await);
    }
    if let Some(why) = crate::commands::gated(app, gid, actor_id, actor, "lockdown").await? {
        return Ok(fail(lift, why).own(app, gid).await);
    }
    let start = Instant::now();
    if lift {
        // Lifting a lock needs the same permit as setting it, for every lock `unlock` lifts: a
        // named kind lifts each active lock that overlaps it, also a combined one.
        let lifts: Vec<LockKind> = active(app, gid)
            .await?
            .into_iter()
            .filter(|a| kind.as_ref().is_none_or(|k| k.overlaps(a)))
            .chain(kind.clone())
            .collect();
        if let Some(p) = lifts.iter().find_map(|k| missing(|p| actor.has(p), k)) {
            return Ok(fail_key(
                app,
                gid,
                lift,
                "reply.lock.unlock_needs_permit",
                &[("permit", p)],
            )
            .await);
        }
        // Named before the locks are gone, for the Type line.
        let name = match &kind {
            Some(k) => type_name(k),
            None => active(app, gid)
                .await?
                .iter()
                .map(type_name)
                .collect::<Vec<_>>()
                .join(" + "),
        };
        return Ok(match unlock(app, gid, kind.as_ref()).await? {
            0 => fail_key(app, gid, lift, "reply.lock.nothing_to_unlock", &[]).await,
            _ => {
                finished(kind.as_ref(), name, lift, start.elapsed(), actor_id)
                    .own(app, gid)
                    .await
            }
        });
    }
    let kind = match kind {
        Some(k) => k,
        None => app.cfg(gid.get()).await?.lockdown_prefs.kind(),
    };
    let name = lock_name(&kind);
    if let Some(p) = missing(|p| actor.has(p), &kind) {
        let vars = [("lock", name.as_str()), ("permit", p)];
        return Ok(fail_key(app, gid, lift, "reply.lock.lock_needs_permit", &vars).await);
    }
    // The reply names what the lock takes: a combined lock skips the parts already locked.
    let took = kind
        .rest(&active(app, gid).await?)
        .unwrap_or_else(|| kind.clone());
    Ok(if lock_for(app, gid, kind.clone(), duration_s).await? {
        finished(
            Some(&took),
            type_name(&took),
            lift,
            start.elapsed(),
            actor_id,
        )
        .own(app, gid)
        .await
    } else {
        fail_key(
            app,
            gid,
            lift,
            "reply.lock.already_covered",
            &[("lock", name.as_str())],
        )
        .await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No DB needed, so CI catches a twilight upgrade that drops the vendored patch: a role read
    /// from Discord keeps bits twilight does not know (1<<48, Set Voice Channel Status).
    #[test]
    fn twilight_keeps_unknown_permission_bits() {
        let bits = (1u64 << 48) | (1 << 47) | 8;
        let role: twilight_model::guild::Role = serde_json::from_value(serde_json::json!({
            "id": "5", "name": "r", "color": 0, "colors": {"primary_color": 0}, "hoist": false,
            "managed": false, "mentionable": false, "permissions": bits.to_string(),
            "position": 1, "flags": 0
        }))
        .unwrap();
        assert_eq!(role.permissions.bits(), bits);
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. Lifting a lock drops its timer
    /// (not another lock's) and tells panic mode the role lock is no longer its own.
    #[tokio::test]
    async fn a_lifted_lock_takes_its_timers_with_it() {
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
        let gid = Id::new(6_470_000_000 + std::process::id() as u64);
        let g = gid.get() as i64;
        let later = now_secs() + 600;
        for kind in [LockKind::Role, LockKind::Channel(5)] {
            let job = serde_json::json!({ "lock": kind });
            brim_core::jobs::enqueue(&app.pool, g, "unlock", job, later)
                .await
                .unwrap();
        }
        let end = serde_json::json!({ "unlock": true, "locked": false });
        brim_core::jobs::enqueue(&app.pool, g, "panic_end", end, later)
            .await
            .unwrap();
        let jobs = || async {
            let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
                "SELECT kind, payload FROM scheduled_jobs WHERE guild_id = $1 ORDER BY id",
            )
            .bind(g)
            .fetch_all(&app.pool)
            .await
            .unwrap();
            rows
        };
        crate::panic::own_lock(&app, gid, crate::panic::ROLE_LOCK, true).await;
        assert_eq!(jobs().await[2].1["locked"], true);
        forget_timers(&app, gid, &LockKind::Role).await;
        let left = jobs().await;
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].1["lock"], serde_json::json!(LockKind::Channel(5)));
        assert_eq!(left[1].1["locked"], false);
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1")
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
    }

    #[test]
    fn lock_replies_read_like_the_panels() {
        assert_eq!(type_name(&LockKind::Server), "Channels");
        let r = finished(
            Some(&LockKind::Channel(5)),
            String::new(),
            true,
            Duration::ZERO,
            Id::new(1),
        );
        assert_eq!(r.text, "<#5> has been unlocked");
        let r = finished(
            Some(&LockKind::Role),
            "Roles".into(),
            false,
            Duration::from_micros(390_110),
            Id::new(1),
        );
        assert_eq!(r.text, "Lockdown successful: Roles (390 ms)");
        assert!(r.embed.description.unwrap().contains("`390.11 ms`"));
    }

    #[test]
    fn lock_notice_numbers_updates_past_the_dropped_ones() {
        let notice = |u: &[(i64, String)]| {
            crate::reply::bot_embed(
                "reply.lock.locked_notice",
                None,
                &[("text", &updates_text(u))],
            )
        };
        let e = notice(&[]);
        assert_eq!(e.color, Some(RED));
        assert!(!e.description.unwrap().contains("UPDATE"));
        let ups: Vec<(i64, String)> = (1..=8).map(|n| (7, format!("note {n}"))).collect();
        let d = notice(&ups).description.unwrap();
        assert!(
            !d.contains("UPDATE 2**")
                && d.contains("**UPDATE 3** (<@7>):")
                && d.contains("UPDATE 8")
        );
        assert!(d.contains("note 8"));
        // Byte-equal to the notice the code built before it became an entry.
        let two = notice(&ups[..2]).description.unwrap();
        let (up, ar) = ("📢", "▸");
        let old = format!(
            "{up} Staff have locked this server down.\n{ar} You cannot see or talk in the channels until the lockdown ends.\n{ar} **Hang tight while staff sort everything out.**\n\n📌 **UPDATE 1** (<@7>):\n{ar} note 1\n\n📌 **UPDATE 2** (<@7>):\n{ar} note 2"
        );
        assert_eq!(two.replace('\u{fe0f}', ""), old.replace('\u{fe0f}', ""));
    }

    #[test]
    fn a_stripped_role_is_judged_by_its_saved_permissions() {
        let info = std::sync::Arc::new(crate::hierarchy::GuildInfo {
            owner: Id::new(1),
            roles: Default::default(),
            role_perms: [(Id::new(5), 1 << 10), (Id::new(6), 0)].into(),
            managed: Default::default(),
            unpermittable: Default::default(),
            parents: Default::default(),
            channels: Default::default(),
            nsfw: Default::default(),
            voice: Default::default(),
            bot_top: 0,
            fetched: Instant::now(),
        });
        let lock: Snapshot = serde_json::from_value(serde_json::json!({
            "kind": null, "role_perms": [[5, 8], [9, 8]], "channel_denies": [],
            "join_lock": false, "invites_paused": false
        }))
        .unwrap();
        let got = with_saved(info.clone(), &[lock]);
        assert_eq!(got.role_perms[&Id::new(5)], (1 << 10) | 8);
        assert_eq!(got.role_perms[&Id::new(6)], 0);
        assert!(!got.role_perms.contains_key(&Id::new(9)), "deleted since");
        assert!(std::sync::Arc::ptr_eq(
            &with_saved(info.clone(), &[]),
            &info
        ));
    }

    #[test]
    fn join_lock_maps_from_kind() {
        assert_eq!(
            join_lock_of(&LockKind::Join(JoinLock::Kick)),
            Some(JoinLock::Kick)
        );
        assert_eq!(join_lock_of(&LockKind::All), Some(JoinLock::Ban));
        assert_eq!(join_lock_of(&LockKind::Server), None);
        assert_eq!(join_lock_of(&LockKind::Channel(1)), None);
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn join_lock_reads_saved_snapshot() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let gid = 4_000_000_000_000_000_000 + std::process::id() as u64;
        let p = plan(&LockKind::Join(JoinLock::Kick), gid, &[], &[], 0, false);
        store::save_lock(&pool, gid, &p.snapshot).await.unwrap();
        assert_eq!(
            load_join_lock(&pool, gid).await.unwrap(),
            Some(JoinLock::Kick)
        );
        store::take_lock(&pool, gid, &p.snapshot).await.unwrap();
        assert_eq!(load_join_lock(&pool, gid).await.unwrap(), None);
        store::drop_lock(&pool, gid, &p.snapshot).await.unwrap();
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. The active locks are read once and
    /// kept, until a lock write forgets them.
    #[tokio::test]
    async fn active_locks_are_kept_until_a_lock_write() {
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
        let gid = 4_200_000_000_000_000_000 + std::process::id() as u64;
        assert!(active_locks(&app, gid).await.unwrap().is_empty());
        let snap: Snapshot = serde_json::from_value(serde_json::json!({
            "kind": "Role", "role_perms": [[5, 8]], "channel_denies": [],
            "join_lock": false, "invites_paused": false
        }))
        .unwrap();
        store::save_lock(&app.pool, gid, &snap).await.unwrap();
        assert!(active_locks(&app, gid).await.unwrap().is_empty(), "kept");
        forget_locks(gid);
        assert_eq!(active_locks(&app, gid).await.unwrap().len(), 1);
        store::take_lock(&app.pool, gid, &snap).await.unwrap();
        store::drop_lock(&app.pool, gid, &snap).await.unwrap();
    }
}

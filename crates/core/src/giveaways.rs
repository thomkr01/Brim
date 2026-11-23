//! Giveaways: the stored rows, the entry rules and the limits that the bot and the dashboard share.
//! The bot posts the message, takes the entries and draws; the dashboard makes and changes rows.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{
    types::chrono::{DateTime, Utc},
    PgPool,
};
use twilight_model::id::{marker::RoleMarker, Id};

/// Running giveaways per server.
pub const MAX_RUNNING: i64 = 25;
/// Winners per giveaway.
pub const WINNERS: (i64, i64) = (1, 100);
/// Shortest and longest duration in seconds (the scheduler tick sets the low end).
pub const DURATION: (i64, i64) = (60, 90 * 86_400);
/// Roles per list (required, blocked, bypass, extra entries).
pub const MAX_ROLES: usize = 10;
/// Entries a role with extra entries gives.
pub const EXTRA: (i64, i64) = (1, 50);
/// Longest prize text (an embed title holds 256 characters).
pub const MAX_PRIZE: usize = 200;
/// Longest button label (Discord's limit is 80).
pub const MAX_LABEL: usize = 80;
/// Hours a winner has to claim the prize (default 48).
pub const CLAIM_HOURS: (i64, i64) = (1, 720);
/// A reroll of a giveaway that ended longer ago than this asks to confirm first.
pub const OLD_REROLL_DAYS: i64 = 7;
/// Winners the public winners post names; above it, the post shows the count only.
pub const PING_CAP: usize = 25;

/// A role that gives more than one entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extra {
    pub role: Id<RoleMarker>,
    pub entries: u16,
}

/// Who may enter one giveaway. Role ids are strings in the JSON, as in every config section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Req {
    /// The member needs one of these roles (empty: no role needed).
    pub required: Vec<Id<RoleMarker>>,
    /// Any of these roles: no entry, also with a bypass role.
    pub blocked: Vec<Id<RoleMarker>>,
    /// Skips the account age, the time in the server and the required roles. Never moderation.
    pub bypass: Vec<Id<RoleMarker>>,
    pub extra: Vec<Extra>,
    pub min_age_days: u32,
    pub min_member_hours: u32,
    /// Members a join raid caught in their current stay cannot enter.
    pub block_raid_joiners: bool,
}

impl Req {
    /// The first rule this value breaks, as a sentence for the person who set it.
    pub fn problem(&self) -> Option<String> {
        let lists = [
            ("required roles", self.required.len()),
            ("blocked roles", self.blocked.len()),
            ("bypass roles", self.bypass.len()),
            ("extra entry roles", self.extra.len()),
        ];
        if let Some((name, n)) = lists.iter().find(|(_, n)| *n > MAX_ROLES) {
            return Some(format!(
                "The list of {name} has {n} roles. The limit is {MAX_ROLES}."
            ));
        }
        if let Some(e) = self
            .extra
            .iter()
            .find(|e| !(EXTRA.0..=EXTRA.1).contains(&(e.entries as i64)))
        {
            return Some(format!(
                "A role gives {} entries. Use {} to {}.",
                e.entries, EXTRA.0, EXTRA.1
            ));
        }
        if self.min_age_days > 3650 {
            return Some("The minimum account age is longer than 10 years.".into());
        }
        if self.min_member_hours > 87_600 {
            return Some("The minimum time in the server is longer than 10 years.".into());
        }
        None
    }
}

/// Why a member cannot enter. The order of [`refusal`] is the order of the checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Bot,
    Host,
    Sponsor,
    BlockedRole,
    Quarantined,
    Flagged,
    RaidJoiner,
    Unverified,
    AccountAge(u32),
    MemberTime(u32),
    MissingRole,
}

impl Refusal {
    /// The reason a member reads in the private answer and in the removal DM.
    pub fn text(&self) -> String {
        match self {
            Self::Bot => "bots cannot enter.".into(),
            Self::Host => "the host cannot enter their own giveaway.".into(),
            Self::Sponsor => "the sponsor cannot enter their own giveaway.".into(),
            Self::BlockedRole => "you have a role that cannot enter this giveaway.".into(),
            Self::Quarantined => "you are in quarantine.".into(),
            Self::Flagged => "Brim's raid protection flagged your account.".into(),
            Self::RaidJoiner => "you joined during a raid.".into(),
            Self::Unverified => "you are not verified yet.".into(),
            Self::AccountAge(d) => format!("your Discord account must be at least {d} days old."),
            Self::MemberTime(h) => format!("you must be in this server for at least {h} hours."),
            Self::MissingRole => "you do not have a role this giveaway needs.".into(),
        }
    }
}

/// What is known about a member when they enter or when they are drawn.
pub struct Facts<'a> {
    pub user: u64,
    pub roles: &'a [Id<RoleMarker>],
    pub bot: bool,
    pub quarantined: bool,
    /// On the Raider List (not let back in here) or acted on by the join checks in this stay.
    pub flagged: bool,
    pub raid_joiner: bool,
    pub unverified: bool,
    /// Join time of the current stay, in ms.
    pub joined_ms: Option<i64>,
    pub now_ms: i64,
}

const DAY_MS: i64 = 86_400_000;

/// The first rule that keeps `f` out. Moderation first (blocked roles, quarantine, flags), then
/// the bypass, which skips only the account age, the time in the server and the required roles.
pub fn refusal(req: &Req, host: Option<i64>, sponsor: Option<i64>, f: &Facts) -> Option<Refusal> {
    let has = |list: &[Id<RoleMarker>]| f.roles.iter().any(|r| list.contains(r));
    let me = Some(f.user as i64);
    let checks = [
        (f.bot, Refusal::Bot),
        (host == me, Refusal::Host),
        (sponsor == me, Refusal::Sponsor),
        (has(&req.blocked), Refusal::BlockedRole),
        (f.quarantined, Refusal::Quarantined),
        (f.flagged, Refusal::Flagged),
        (req.block_raid_joiners && f.raid_joiner, Refusal::RaidJoiner),
        (f.unverified, Refusal::Unverified),
    ];
    if let Some((_, r)) = checks.into_iter().find(|(hit, _)| *hit) {
        return Some(r);
    }
    if has(&req.bypass) {
        return None;
    }
    let created = crate::giveaways::created_ms(f.user);
    if req.min_age_days > 0 && f.now_ms - created < req.min_age_days as i64 * DAY_MS {
        return Some(Refusal::AccountAge(req.min_age_days));
    }
    let hours = req.min_member_hours as i64 * 3_600_000;
    if req.min_member_hours > 0 && f.joined_ms.is_none_or(|j| f.now_ms - j < hours) {
        return Some(Refusal::MemberTime(req.min_member_hours));
    }
    if !req.required.is_empty() && !has(&req.required) {
        return Some(Refusal::MissingRole);
    }
    None
}

/// When a Discord account was made, from its id.
pub fn created_ms(user: u64) -> i64 {
    brim_automod::join::account_created_ms(user)
}

/// The member's entries: 1, or the most entries one of their extra entry roles gives, with that
/// role. Counted at entry and again when their roles change.
pub fn weight(req: &Req, roles: &[Id<RoleMarker>]) -> (u16, Option<Id<RoleMarker>>) {
    req.extra
        .iter()
        .filter(|e| roles.contains(&e.role) && e.entries > 1)
        .max_by_key(|e| e.entries)
        .map_or((1, None), |e| (e.entries, Some(e.role)))
}

/// One stored giveaway.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Giveaway {
    pub id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub message_id: Option<i64>,
    pub prize: String,
    pub host_id: Option<i64>,
    pub sponsor_id: Option<i64>,
    pub created_by: i64,
    pub winners: i16,
    pub status: String,
    pub ends_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub paused_at: Option<DateTime<Utc>>,
    pub req: sqlx::types::Json<Req>,
    pub button_label: String,
    pub button_emoji: String,
    pub winner_ids: Vec<i64>,
    pub past_winners: Vec<i64>,
    pub claim_hours: i32,
    pub needs_repost: bool,
    pub issue: Option<String>,
    pub shown_entries: i32,
    pub draws: i32,
    pub created_at: DateTime<Utc>,
}

impl Giveaway {
    pub fn running(&self) -> bool {
        self.status == "running"
    }
}

/// A giveaway to make.
#[derive(Clone, Debug, Deserialize)]
pub struct New {
    pub guild_id: i64,
    pub channel_id: i64,
    pub prize: String,
    pub host_id: Option<i64>,
    pub sponsor_id: Option<i64>,
    pub created_by: i64,
    pub winners: i64,
    pub duration_secs: i64,
    pub req: Req,
    pub button_label: String,
    pub button_emoji: String,
    pub claim_hours: i64,
    /// Queue the first post in the same transaction (the dashboard); the command posts at once.
    pub post_job: bool,
}

/// The first value of `n` outside its limit, as a sentence for the person who typed it.
pub fn new_problem(n: &New) -> Option<String> {
    let prize = n.prize.trim();
    if prize.is_empty() {
        return Some("The prize is empty.".into());
    }
    if prize.chars().count() > MAX_PRIZE {
        return Some(format!("The prize is longer than {MAX_PRIZE} characters."));
    }
    if !(WINNERS.0..=WINNERS.1).contains(&n.winners) {
        return Some(format!(
            "A giveaway has {} to {} winners.",
            WINNERS.0, WINNERS.1
        ));
    }
    if !(DURATION.0..=DURATION.1).contains(&n.duration_secs) {
        return Some("A giveaway runs from 1 minute to 90 days.".into());
    }
    if n.button_label.chars().count() > MAX_LABEL {
        return Some(format!(
            "The button label is longer than {MAX_LABEL} characters."
        ));
    }
    if !(CLAIM_HOURS.0..=CLAIM_HOURS.1).contains(&n.claim_hours) {
        return Some(format!(
            "The claim time is {} to {} hours.",
            CLAIM_HOURS.0, CLAIM_HOURS.1
        ));
    }
    n.req.problem()
}

const COLS: &str =
    "id, guild_id, channel_id, message_id, prize, host_id, sponsor_id, created_by, winners,
    status, ends_at, ended_at, paused_at, req, button_label, button_emoji, winner_ids, past_winners,
    claim_hours, needs_repost, issue, shown_entries, draws, created_at";

/// Saves `n` as a running giveaway and queues its end, or says which limit it breaks. The count of
/// running giveaways and the insert are one transaction under a per-server lock.
pub async fn create(pool: &PgPool, n: &New, max_running: i64) -> Result<Result<Giveaway, String>> {
    if let Some(p) = new_problem(n) {
        return Ok(Err(p));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('giveaways:' || $1::text, 0))")
        .bind(n.guild_id)
        .execute(&mut *tx)
        .await?;
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM giveaways WHERE guild_id = $1 AND status = 'running'",
    )
    .bind(n.guild_id)
    .fetch_one(&mut *tx)
    .await?;
    let cap = max_running.clamp(1, MAX_RUNNING);
    if running >= cap {
        return Ok(Err(format!(
            "This server has {running} running giveaways. The limit is {cap}."
        )));
    }
    let g: Giveaway = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO giveaways (guild_id, channel_id, prize, host_id, sponsor_id, created_by,
           winners, ends_at, req, button_label, button_emoji, claim_hours, needs_repost)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(secs => $8), $9, $10, $11, $12, $13)
         RETURNING {COLS}"
    )))
    .bind(n.guild_id)
    .bind(n.channel_id)
    .bind(n.prize.trim())
    .bind(n.host_id)
    .bind(n.sponsor_id)
    .bind(n.created_by)
    .bind(n.winners as i16)
    .bind(n.duration_secs as f64)
    .bind(sqlx::types::Json(&n.req))
    .bind(n.button_label.trim())
    .bind(n.button_emoji.trim())
    .bind(n.claim_hours as i32)
    .bind(n.post_job)
    .fetch_one(&mut *tx)
    .await?;
    if n.post_job {
        let payload = serde_json::json!({"id": g.id, "first": true});
        crate::jobs::enqueue(
            &mut *tx,
            n.guild_id,
            REPOST_JOB,
            payload,
            Utc::now().timestamp(),
        )
        .await?;
    }
    crate::jobs::enqueue(
        &mut *tx,
        n.guild_id,
        END_JOB,
        serde_json::json!({"id": g.id}),
        g.ends_at.timestamp(),
    )
    .await?;
    tx.commit().await?;
    Ok(Ok(g))
}

/// The job that draws the winners at the end time.
pub const END_JOB: &str = "giveaway_end";
/// The job that writes the entry count into the posted messages, at most once a minute.
pub const REFRESH_JOB: &str = "giveaway_refresh";
/// The job that posts the message again (deleted by hand, or after a backup restore).
pub const REPOST_JOB: &str = "giveaway_repost";
/// Job that DMs one winner.
pub const DM_JOB: &str = "giveaway_dm";
/// Job that writes the drawn message and posts the winners.
pub const ANNOUNCE_JOB: &str = "giveaway_announce";
/// Job that runs a dashboard action that needs Discord: cancel, reroll or redraw.
pub const ACTION_JOB: &str = "giveaway_action";

pub async fn get(pool: &PgPool, guild: i64, id: i64) -> Result<Option<Giveaway>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM giveaways WHERE id = $1 AND guild_id = $2"
    )))
    .bind(id)
    .bind(guild)
    .fetch_optional(pool)
    .await?)
}

/// The giveaway whose message is `message`.
pub async fn by_message(pool: &PgPool, guild: i64, message: i64) -> Result<Option<Giveaway>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM giveaways WHERE message_id = $1 AND guild_id = $2"
    )))
    .bind(message)
    .bind(guild)
    .fetch_optional(pool)
    .await?)
}

/// Running giveaways first (soonest end first), then the newest ended or cancelled ones.
pub async fn list(pool: &PgPool, guild: i64, ended: i64) -> Result<Vec<Giveaway>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "(SELECT {COLS} FROM giveaways WHERE guild_id = $1 AND status = 'running' ORDER BY ends_at)
         UNION ALL
         (SELECT {COLS} FROM giveaways WHERE guild_id = $1 AND status <> 'running'
          ORDER BY ended_at DESC NULLS LAST, id DESC LIMIT $2)"
    )))
    .bind(guild)
    .bind(ended)
    .fetch_all(pool)
    .await?)
}

/// What an Enter press did.
#[derive(Debug, PartialEq, Eq)]
pub enum Entered {
    New,
    /// Already in, with this many entries.
    Already(i16),
    /// Not running (ended, cancelled, gone) or paused.
    Closed,
}

/// Adds one entry. The primary key keeps a member in once, also under two fast presses, and the
/// insert runs only while the giveaway is running and not paused.
pub async fn enter(pool: &PgPool, guild: i64, id: i64, user: i64, weight: u16) -> Result<Entered> {
    let new: Option<i64> = sqlx::query_scalar(
        "INSERT INTO giveaway_entries (giveaway_id, guild_id, user_id, weight)
         SELECT $1, $2, $3, $4 WHERE EXISTS (SELECT 1 FROM giveaways
           WHERE id = $1 AND guild_id = $2 AND status = 'running' AND paused_at IS NULL)
         ON CONFLICT DO NOTHING RETURNING user_id",
    )
    .bind(id)
    .bind(guild)
    .bind(user)
    .bind(weight as i16)
    .fetch_optional(pool)
    .await?;
    if new.is_some() {
        return Ok(Entered::New);
    }
    let had: Option<i16> = sqlx::query_scalar(
        "SELECT weight FROM giveaway_entries WHERE giveaway_id = $1 AND guild_id = $2 AND user_id = $3",
    )
    .bind(id)
    .bind(guild)
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(had.map_or(Entered::Closed, Entered::Already))
}

pub async fn entry_count(pool: &PgPool, id: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM giveaway_entries WHERE giveaway_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

/// Up to `limit` entrants in draw order, without `skip`, past winners and quarantined members.
/// The database draws: each entry is one equal chance (an entrant with weight `w` sorts by
/// `-ln(u) / w`, a weighted sample without repeats), so a draw of 100,000 entries reads `limit`
/// rows, not all of them. Ask again with the tried ones in `skip` for more.
pub async fn candidates(pool: &PgPool, g: &Giveaway, skip: &[i64], limit: i64) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT e.user_id FROM giveaway_entries e
         WHERE e.giveaway_id = $1 AND e.guild_id = $2
           AND NOT (e.user_id = ANY($3)) AND NOT (e.user_id = ANY($4))
           AND NOT EXISTS (SELECT 1 FROM quarantines q WHERE q.guild_id = $2 AND q.user_id = e.user_id)
         ORDER BY -ln(1.0 - random()) / GREATEST(e.weight, 1)
         LIMIT $5",
    )
    .bind(g.id)
    .bind(g.guild_id)
    .bind(&g.past_winners)
    .bind(skip)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// The running giveaways of the server that `user` has an entry in, with their entry weight.
pub async fn entered_running(pool: &PgPool, guild: i64, user: i64) -> Result<Vec<(Giveaway, i16)>> {
    let ids: Vec<(i64, i16)> = sqlx::query_as(
        "SELECT e.giveaway_id, e.weight FROM giveaway_entries e JOIN giveaways g ON g.id = e.giveaway_id
         WHERE e.guild_id = $1 AND e.user_id = $2 AND g.status = 'running'",
    )
    .bind(guild)
    .bind(user)
    .fetch_all(pool)
    .await?;
    let mut out = vec![];
    for (id, w) in ids {
        if let Some(g) = get(pool, guild, id).await? {
            out.push((g, w));
        }
    }
    Ok(out)
}

/// Removes one entry; true when there was one.
pub async fn remove_entry(pool: &PgPool, guild: i64, id: i64, user: i64) -> Result<bool> {
    Ok(sqlx::query(
        "DELETE FROM giveaway_entries WHERE giveaway_id = $1 AND guild_id = $2 AND user_id = $3",
    )
    .bind(id)
    .bind(guild)
    .bind(user)
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

pub async fn set_weight(pool: &PgPool, guild: i64, id: i64, user: i64, weight: u16) -> Result<()> {
    sqlx::query(
        "UPDATE giveaway_entries SET weight = $4 WHERE giveaway_id = $1 AND guild_id = $2 AND user_id = $3",
    )
    .bind(id)
    .bind(guild)
    .bind(user)
    .bind(weight as i16)
    .execute(pool)
    .await?;
    Ok(())
}

/// Takes the draw of a running giveaway: status `ended` with its end time. A run that stopped
/// before it saved its winners (`draws` still 0) may take it again; only one end job exists per
/// giveaway, so two runs never draw at once. None when there is nothing to draw.
pub async fn claim_end(pool: &PgPool, guild: i64, id: i64) -> Result<Option<Giveaway>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE giveaways SET status = 'ended', ended_at = COALESCE(ended_at, now()), paused_at = NULL
         WHERE id = $1 AND guild_id = $2 AND (status = 'running' OR (status = 'ended' AND draws = 0))
         RETURNING {COLS}"
    )))
    .bind(id)
    .bind(guild)
    .fetch_optional(pool)
    .await?)
}

/// Saves a draw: the winners now, and that they won once. `seen` is the `draws` count the draw
/// started from: when another draw saved first, nothing changes and the result is false. In the
/// same transaction it queues the announcement (`key`: the message to post) and one DM job per
/// new winner, so a restart after the save still announces, and nothing is sent twice.
pub async fn save_draw(
    pool: &PgPool,
    guild: i64,
    id: i64,
    seen: i32,
    winners: &[i64],
    new: &[i64],
    key: &str,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let saved = sqlx::query(
        "UPDATE giveaways SET winner_ids = $4, past_winners = past_winners || $5, draws = draws + 1
         WHERE id = $1 AND guild_id = $2 AND draws = $3 AND status = 'ended'",
    )
    .bind(id)
    .bind(guild)
    .bind(seen)
    .bind(winners)
    .bind(new)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !saved {
        return Ok(false);
    }
    let (now, draws) = (Utc::now().timestamp(), seen + 1);
    let payload = serde_json::json!({ "id": id, "draws": draws, "key": key, "won": new });
    crate::jobs::enqueue(&mut *tx, guild, ANNOUNCE_JOB, payload, now).await?;
    for u in new {
        let payload = serde_json::json!({ "id": id, "draws": draws, "user": u });
        crate::jobs::enqueue(&mut *tx, guild, DM_JOB, payload, now).await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// Cancels a running giveaway and drops its end job; false when it was not running.
pub async fn cancel(pool: &PgPool, guild: i64, id: i64) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let n = sqlx::query(
        "UPDATE giveaways SET status = 'cancelled', ended_at = now(), paused_at = NULL
         WHERE id = $1 AND guild_id = $2 AND status = 'running'",
    )
    .bind(id)
    .bind(guild)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // Not while a draw runs (status is `ended` then): a failed draw needs its job to retry.
    if n == 1 {
        drop_jobs(&mut tx, guild, id).await?;
    }
    tx.commit().await?;
    Ok(n == 1)
}

async fn drop_jobs(tx: &mut sqlx::PgConnection, guild: i64, id: i64) -> Result<()> {
    sqlx::query(
        "DELETE FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3",
    )
    .bind(guild)
    .bind(END_JOB)
    .bind(id.to_string())
    .execute(tx)
    .await?;
    Ok(())
}

/// What a request to end now did.
#[derive(Debug, PartialEq, Eq)]
pub enum EndNow {
    /// The end job runs within seconds.
    Queued,
    /// The end job runs already.
    Running,
    NotRunning,
}

/// Moves the end job of a running giveaway to now. Only one end job exists per giveaway: a job
/// the scheduler holds is left alone, and a lost job is made again.
pub async fn end_now(pool: &PgPool, guild: i64, id: i64) -> Result<EndNow> {
    let mut tx = pool.begin().await?;
    let running: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM giveaways WHERE id = $1 AND guild_id = $2 AND status = 'running' FOR UPDATE",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?;
    if running.is_none() {
        return Ok(EndNow::NotRunning);
    }
    let jobs: Vec<bool> = sqlx::query_scalar(
        "SELECT locked_until IS NOT NULL AND locked_until > now() FROM scheduled_jobs
         WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3",
    )
    .bind(guild)
    .bind(END_JOB)
    .bind(id.to_string())
    .fetch_all(&mut *tx)
    .await?;
    if jobs.iter().any(|&leased| leased) {
        return Ok(EndNow::Running);
    }
    if jobs.is_empty() {
        let job = serde_json::json!({"id": id});
        crate::jobs::enqueue(&mut *tx, guild, END_JOB, job, 0).await?;
    } else {
        sqlx::query(
            "UPDATE scheduled_jobs SET run_at = now() WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3",
        )
        .bind(guild)
        .bind(END_JOB)
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE giveaways SET ends_at = LEAST(ends_at, now()) WHERE id = $1 AND guild_id = $2",
    )
    .bind(id)
    .bind(guild)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(EndNow::Queued)
}

/// Moves the end of a running giveaway by `secs` (negative: earlier) and its end job with it. The
/// new end is at least a minute from now and at most 90 days after the start. Returns the new end,
/// or None when the giveaway is not running.
pub async fn shift(pool: &PgPool, guild: i64, id: i64, secs: i64) -> Result<Option<DateTime<Utc>>> {
    let mut tx = pool.begin().await?;
    let ends: Option<DateTime<Utc>> = sqlx::query_scalar(
        "UPDATE giveaways SET ends_at = LEAST(created_at + make_interval(secs => $4),
             GREATEST(now() + interval '1 minute', ends_at + make_interval(secs => $3)))
         WHERE id = $1 AND guild_id = $2 AND status = 'running' RETURNING ends_at",
    )
    .bind(id)
    .bind(guild)
    .bind(secs as f64)
    .bind(DURATION.1 as f64)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(ends) = ends else {
        return Ok(None);
    };
    move_end_job(&mut tx, guild, id, ends).await?;
    tx.commit().await?;
    Ok(Some(ends))
}

/// Puts the unleased end job of giveaway `id` at `ends`, or queues one when it is lost.
async fn move_end_job(
    tx: &mut sqlx::PgConnection,
    guild: i64,
    id: i64,
    ends: DateTime<Utc>,
) -> Result<()> {
    let moved = sqlx::query(
        "UPDATE scheduled_jobs SET run_at = $4 WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3",
    )
    .bind(guild)
    .bind(END_JOB)
    .bind(id.to_string())
    .bind(ends)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if moved == 0 {
        let job = serde_json::json!({"id": id});
        crate::jobs::enqueue(&mut *tx, guild, END_JOB, job, ends.timestamp()).await?;
    }
    Ok(())
}

/// Changes the prize, winner count and rules of a running giveaway. The new rules apply to the
/// entries already in: the caller removes the entries that fail them.
pub async fn edit(
    pool: &PgPool,
    guild: i64,
    id: i64,
    prize: &str,
    winners: i64,
    req: &Req,
) -> Result<Result<bool, String>> {
    let prize = prize.trim();
    if prize.is_empty() || prize.chars().count() > MAX_PRIZE {
        return Ok(Err(format!(
            "The prize must have 1 to {MAX_PRIZE} characters."
        )));
    }
    if !(WINNERS.0..=WINNERS.1).contains(&winners) {
        return Ok(Err(format!(
            "A giveaway has {} to {} winners.",
            WINNERS.0, WINNERS.1
        )));
    }
    if let Some(p) = req.problem() {
        return Ok(Err(p));
    }
    Ok(Ok(sqlx::query(
        "UPDATE giveaways SET prize = $3, winners = $4, req = $5, shown_entries = -1
         WHERE id = $1 AND guild_id = $2 AND status = 'running'",
    )
    .bind(id)
    .bind(guild)
    .bind(prize)
    .bind(winners as i16)
    .bind(sqlx::types::Json(req))
    .execute(pool)
    .await?
    .rows_affected()
        == 1))
}

/// Marks a running giveaway to be posted (again), in `channel` when given, and queues the post.
/// `first`: a new giveaway (the bot logs it as started). False when it is not running.
pub async fn queue_post(
    pool: &PgPool,
    guild: i64,
    id: i64,
    channel: Option<i64>,
    first: bool,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let n = sqlx::query(
        "UPDATE giveaways SET channel_id = COALESCE($3, channel_id), message_id = NULL,
           needs_repost = true, issue = NULL
         WHERE id = $1 AND guild_id = $2 AND status = 'running'",
    )
    .bind(id)
    .bind(guild)
    .bind(channel)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 1 {
        let payload = serde_json::json!({ "id": id, "first": first });
        crate::jobs::enqueue(&mut *tx, guild, REPOST_JOB, payload, Utc::now().timestamp()).await?;
    }
    tx.commit().await?;
    Ok(n == 1)
}

/// The entry count of each of `ids` (only those with entries).
pub async fn entry_counts(pool: &PgPool, guild: i64, ids: &[i64]) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT giveaway_id, count(*) FROM giveaway_entries
         WHERE guild_id = $1 AND giveaway_id = ANY($2) GROUP BY giveaway_id",
    )
    .bind(guild)
    .bind(ids)
    .fetch_all(pool)
    .await?)
}

/// Entries shown per page on the dashboard.
pub const ENTRIES_PAGE: i64 = 50;

/// One page of a giveaway's entrants (user, weight), by user id.
pub async fn entries_page(
    pool: &PgPool,
    guild: i64,
    id: i64,
    page: i64,
) -> Result<Vec<(i64, i16)>> {
    Ok(sqlx::query_as(
        "SELECT user_id, weight FROM giveaway_entries WHERE giveaway_id = $1 AND guild_id = $2
         ORDER BY user_id LIMIT $3 OFFSET $4",
    )
    .bind(id)
    .bind(guild)
    .bind(ENTRIES_PAGE)
    .bind(page.clamp(0, 100_000) * ENTRIES_PAGE)
    .fetch_all(pool)
    .await?)
}

/// Saves where the message is now and clears the re-post mark and the issue.
pub async fn set_posted(
    pool: &PgPool,
    guild: i64,
    id: i64,
    channel: i64,
    message: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE giveaways SET channel_id = $2, message_id = $3, needs_repost = false, issue = NULL,
           shown_entries = -1 WHERE id = $1 AND guild_id = $4",
    )
    .bind(id)
    .bind(channel)
    .bind(message)
    .bind(guild)
    .execute(pool)
    .await?;
    Ok(())
}

/// Notes a problem staff must see on the dashboard (None clears it).
pub async fn set_issue(pool: &PgPool, guild: i64, id: i64, issue: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE giveaways SET issue = $2 WHERE id = $1 AND guild_id = $3")
        .bind(id)
        .bind(issue)
        .bind(guild)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: u64) -> Id<RoleMarker> {
        Id::new(id)
    }

    fn facts(roles: &[Id<RoleMarker>]) -> Facts<'_> {
        Facts {
            // An account made long ago.
            user: 1 << 22,
            roles,
            bot: false,
            quarantined: false,
            flagged: false,
            raid_joiner: false,
            unverified: false,
            joined_ms: Some(0),
            now_ms: 4_000_000_000_000,
        }
    }

    /// Moderation comes first and a bypass role never skips it; the bypass skips only age, time in
    /// the server and the required roles.
    #[test]
    fn checks_run_in_order_and_bypass_skips_only_the_soft_rules() {
        let (r5, r7, r76) = ([r(5)], [r(7)], [r(7), r(6)]);
        let req = Req {
            required: vec![r(5)],
            blocked: vec![r(6)],
            bypass: vec![r(7)],
            min_age_days: 7,
            min_member_hours: 24,
            ..Req::default()
        };
        assert_eq!(
            refusal(&req, None, None, &facts(&r5)),
            None,
            "an old member with the role"
        );
        assert_eq!(
            refusal(&req, None, None, &facts(&[])),
            Some(Refusal::MissingRole)
        );
        assert_eq!(refusal(&req, None, None, &facts(&r7)), None, "bypass");
        assert_eq!(
            refusal(&req, None, None, &facts(&r76)),
            Some(Refusal::BlockedRole),
            "a blocked role wins over a bypass role"
        );
        let mut q = facts(&r7);
        q.quarantined = true;
        assert_eq!(refusal(&req, None, None, &q), Some(Refusal::Quarantined));
        let mut f = facts(&r7);
        f.flagged = true;
        assert_eq!(refusal(&req, None, None, &f), Some(Refusal::Flagged));
        let host = (1i64 << 22, facts(&r5));
        assert_eq!(
            refusal(&req, Some(host.0), None, &host.1),
            Some(Refusal::Host)
        );
        assert_eq!(
            refusal(&req, None, Some(host.0), &host.1),
            Some(Refusal::Sponsor)
        );
        let mut young = facts(&r5);
        young.user = ((young.now_ms - 1_420_070_400_000 - DAY_MS) as u64) << 22;
        assert_eq!(
            refusal(&req, None, None, &young),
            Some(Refusal::AccountAge(7))
        );
        let mut new = facts(&r5);
        new.joined_ms = Some(new.now_ms - 3_600_000);
        assert_eq!(
            refusal(&req, None, None, &new),
            Some(Refusal::MemberTime(24))
        );
        let mut raid = facts(&r7);
        raid.raid_joiner = true;
        assert_eq!(refusal(&req, None, None, &raid), None, "the option is off");
        let on = Req {
            block_raid_joiners: true,
            ..req.clone()
        };
        assert_eq!(refusal(&on, None, None, &raid), Some(Refusal::RaidJoiner));
    }

    #[test]
    fn weight_is_the_best_extra_role() {
        let req = Req {
            extra: vec![
                Extra {
                    role: r(1),
                    entries: 3,
                },
                Extra {
                    role: r(2),
                    entries: 5,
                },
            ],
            ..Req::default()
        };
        assert_eq!(weight(&req, &[]), (1, None));
        assert_eq!(weight(&req, &[r(1)]), (3, Some(r(1))));
        assert_eq!(weight(&req, &[r(1), r(2)]), (5, Some(r(2))));
    }

    #[test]
    fn limits_are_checked_at_creation() {
        let n = New {
            guild_id: 1,
            channel_id: 2,
            prize: "Nitro".into(),
            host_id: None,
            sponsor_id: None,
            created_by: 3,
            winners: 1,
            duration_secs: 3600,
            req: Req::default(),
            button_label: String::new(),
            button_emoji: String::new(),
            claim_hours: 48,
            post_job: false,
        };
        assert_eq!(new_problem(&n), None);
        let bad = [
            New {
                winners: 101,
                ..n.clone()
            },
            New {
                duration_secs: 59,
                ..n.clone()
            },
            New {
                duration_secs: 91 * 86_400,
                ..n.clone()
            },
            New {
                prize: " ".into(),
                ..n.clone()
            },
            New {
                req: Req {
                    blocked: (1..=11).map(r).collect(),
                    ..Req::default()
                },
                ..n.clone()
            },
            New {
                req: Req {
                    extra: vec![Extra {
                        role: r(1),
                        entries: 51,
                    }],
                    ..Req::default()
                },
                ..n.clone()
            },
        ];
        for b in bad {
            assert!(new_problem(&b).is_some(), "{b:?}");
        }
    }

    async fn db(offset: i64) -> Option<(PgPool, i64)> {
        let url = crate::db::test_url().ok()?;
        let pool = crate::db::connect(&url).await.unwrap();
        let gid = 7_100_000_000 + std::process::id() as i64 * 10 + offset;
        sqlx::query("DELETE FROM giveaways WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
        Some((pool, gid))
    }

    fn new(gid: i64) -> New {
        New {
            guild_id: gid,
            channel_id: 2,
            prize: "Nitro".into(),
            host_id: Some(9),
            sponsor_id: None,
            created_by: 9,
            winners: 1,
            duration_secs: 3600,
            req: Req::default(),
            button_label: String::new(),
            button_emoji: String::new(),
            claim_hours: 48,
            post_job: false,
        }
    }

    /// One entry per member, also when pressed twice; a draw is taken once; a reroll that lost
    /// the race to another saves nothing; a cancel drops the end job.
    #[tokio::test]
    async fn entries_and_the_draw_happen_once() {
        let Some((pool, gid)) = db(1).await else {
            return;
        };
        let g = create(&pool, &new(gid), 25).await.unwrap().unwrap();
        let jobs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'giveaway_end'",
        )
        .bind(gid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(jobs, 1);
        let (a, b) = tokio::join!(enter(&pool, gid, g.id, 5, 2), enter(&pool, gid, g.id, 5, 2));
        let mut both = [a.unwrap(), b.unwrap()];
        both.sort_by_key(|e| matches!(e, Entered::New));
        assert_eq!(both, [Entered::Already(2), Entered::New]);
        assert_eq!(
            enter(&pool, 1, g.id, 6, 1).await.unwrap(),
            Entered::Closed,
            "other server"
        );
        assert_eq!(entry_count(&pool, g.id).await.unwrap(), 1);

        assert_eq!(end_now(&pool, gid, g.id).await.unwrap(), EndNow::Queued);
        let claimed = claim_end(&pool, gid, g.id).await.unwrap().unwrap();
        assert!(
            claim_end(&pool, 1, g.id).await.unwrap().is_none(),
            "other server"
        );
        // A run that died before it saved can take it again; after the save nobody can.
        assert!(claim_end(&pool, gid, g.id).await.unwrap().is_some());
        assert!(save_draw(
            &pool,
            gid,
            g.id,
            claimed.draws,
            &[5],
            &[5],
            "giveaway.winners"
        )
        .await
        .unwrap());
        assert!(claim_end(&pool, gid, g.id).await.unwrap().is_none());
        let queued: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM scheduled_jobs WHERE guild_id = $1 AND kind IN ($2, $3) ORDER BY kind",
        )
        .bind(gid)
        .bind(ANNOUNCE_JOB)
        .bind(DM_JOB)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            queued,
            [ANNOUNCE_JOB, DM_JOB],
            "the save queues the post and the DM"
        );
        assert!(!save_draw(
            &pool,
            gid,
            g.id,
            claimed.draws,
            &[6],
            &[6],
            "giveaway.winners"
        )
        .await
        .unwrap());
        assert_eq!(
            enter(&pool, gid, g.id, 7, 1).await.unwrap(),
            Entered::Closed
        );
        let after = get(&pool, gid, g.id).await.unwrap().unwrap();
        assert_eq!(
            (after.winner_ids.clone(), after.past_winners.clone()),
            (vec![5], vec![5])
        );
        assert!(
            candidates(&pool, &after, &[], 10).await.unwrap().is_empty(),
            "a past winner is out"
        );

        let c = create(&pool, &new(gid), 25).await.unwrap().unwrap();
        assert!(shift(&pool, gid, c.id, -7200).await.unwrap().is_some());
        assert!(cancel(&pool, gid, c.id).await.unwrap());
        assert!(!cancel(&pool, gid, c.id).await.unwrap());
        let jobs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND payload->>'id' = $2",
        )
        .bind(gid)
        .bind(c.id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(jobs, 0);
        sqlx::query("DELETE FROM giveaways WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// The running cap holds under parallel creates.
    #[tokio::test]
    async fn the_running_cap_holds_under_parallel_creates() {
        let Some((pool, gid)) = db(2).await else {
            return;
        };
        let runs: Vec<_> = (0..5)
            .map(|_| {
                let (pool, n) = (pool.clone(), new(gid));
                tokio::spawn(async move { create(&pool, &n, 3).await.unwrap() })
            })
            .collect();
        let mut made = vec![];
        for r in runs {
            made.push(r.await.unwrap());
        }
        assert_eq!(made.iter().filter(|r| r.is_ok()).count(), 3);
        sqlx::query("DELETE FROM giveaways WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// 100,000 entries: parallel presses (each member twice) give one entry each, the count and
    /// a 100 winner draw stay fast, and the draw follows the weights. Times go to stderr.
    #[tokio::test]
    async fn a_giveaway_with_100000_entries() {
        let Some((pool, gid)) = db(3).await else {
            return;
        };
        let g = create(&pool, &new(gid), 25).await.unwrap().unwrap();
        let t = std::time::Instant::now();
        sqlx::query(
            "INSERT INTO giveaway_entries (giveaway_id, guild_id, user_id, weight)
             SELECT $1, $2, u, CASE WHEN u % 10 = 0 THEN 5 ELSE 1 END FROM generate_series(1, 100000) u",
        )
        .bind(g.id)
        .bind(gid)
        .execute(&pool)
        .await
        .unwrap();
        eprintln!("insert 100000 entries: {:?}", t.elapsed());

        // 5,000 presses at once: 2,500 new members, each pressing twice.
        let t = std::time::Instant::now();
        let presses: Vec<_> = (0..5000)
            .map(|n| {
                let (pool, id) = (pool.clone(), g.id);
                tokio::spawn(
                    async move { enter(&pool, gid, id, 200_000 + n / 2, 1).await.unwrap() },
                )
            })
            .collect();
        let mut new_ones = 0;
        for p in presses {
            if p.await.unwrap() == Entered::New {
                new_ones += 1;
            }
        }
        let took = t.elapsed();
        eprintln!(
            "5000 parallel presses: {took:?} ({:?} each on average)",
            took / 5000
        );
        assert_eq!(new_ones, 2500);

        let t = std::time::Instant::now();
        assert_eq!(entry_count(&pool, g.id).await.unwrap(), 102_500);
        eprintln!("entry count: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let picked = candidates(&pool, &g, &[], 200).await.unwrap();
        eprintln!("draw 200 candidates of 102500: {:?}", t.elapsed());
        let mut unique = picked.clone();
        unique.sort();
        unique.dedup();
        assert_eq!((picked.len(), unique.len()), (200, 200));
        let more = candidates(&pool, &g, &picked, 200).await.unwrap();
        assert!(more.iter().all(|u| !picked.contains(u)));

        // A tenth of the members have 5 entries: 50,000 of the 142,500 chances, so about 70 of 200.
        let heavy = picked
            .iter()
            .filter(|&&u| u <= 100_000 && u % 10 == 0)
            .count();
        assert!((45..=95).contains(&heavy), "{heavy} of 200 had 5 entries");

        sqlx::query("DELETE FROM giveaways WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
    }
}

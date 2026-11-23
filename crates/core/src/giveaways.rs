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
/// Hours a winner keeps the winner role (default 7 days).
pub const ROLE_HOURS: (u32, u32) = (1, 8760);
/// Hours until the winner thread closes (default 3 days).
pub const THREAD_HOURS: (u32, u32) = (1, 720);
/// Length of an entry code, in characters.
pub const CODE_LEN: (usize, usize) = (6, 50);
/// Wrong entry codes in a row before the member must wait.
pub const CODE_TRIES: i16 = 3;
/// Minutes a member waits after too many wrong entry codes.
pub const CODE_WAIT_MINS: i64 = 10;

/// The extras of one giveaway: a winner role for a time, a private thread for the winners, and
/// entry by reaction instead of the button.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extras {
    /// Given to each winner and taken back after `role_hours`.
    pub winner_role: Option<Id<RoleMarker>>,
    pub role_hours: u32,
    /// A private thread for the winners, the host and the sponsor; it closes after `thread_hours`.
    pub thread: bool,
    pub thread_hours: u32,
    /// Members enter with a reaction (the button emoji) instead of the button.
    pub reaction: bool,
}

impl Default for Extras {
    fn default() -> Self {
        Self {
            winner_role: None,
            role_hours: 168,
            thread: false,
            thread_hours: 72,
            reaction: false,
        }
    }
}

impl Extras {
    /// The first rule this value breaks, as a sentence for the person who set it.
    pub fn problem(&self) -> Option<String> {
        if self.winner_role.is_some() && !(ROLE_HOURS.0..=ROLE_HOURS.1).contains(&self.role_hours) {
            return Some(format!(
                "Winners keep the role {} to {} hours.",
                ROLE_HOURS.0, ROLE_HOURS.1
            ));
        }
        if self.thread && !(THREAD_HOURS.0..=THREAD_HOURS.1).contains(&self.thread_hours) {
            return Some(format!(
                "The winner thread closes after {} to {} hours.",
                THREAD_HOURS.0, THREAD_HOURS.1
            ));
        }
        None
    }
}

/// What an entry code is compared as: trimmed, lower case.
fn code_text(code: &str) -> String {
    code.trim().to_lowercase()
}

/// Why `code` cannot be the entry code of a giveaway (None: it can). `reaction`: reaction entry
/// is on, and a reaction cannot carry a code.
pub fn code_problem(code: &str, reaction: bool) -> Option<String> {
    let n = code_text(code).chars().count();
    if reaction {
        return Some("An entry code needs the Enter button: turn reaction entry off.".into());
    }
    (!(CODE_LEN.0..=CODE_LEN.1).contains(&n)).then(|| {
        format!(
            "The entry code has {} to {} characters.",
            CODE_LEN.0, CODE_LEN.1
        )
    })
}

/// The stored form of the entry code of giveaway `id`: SHA-256, salted with the id.
pub fn code_hash(id: i64, code: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(format!("brim giveaway code\0{id}\0{}", code_text(code)));
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compares two hashes in a time that does not depend on where they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

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
    TimedOut,
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
            Self::TimedOut => "you are timed out.".into(),
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
    /// In a Discord timeout now.
    pub timed_out: bool,
    pub raid_joiner: bool,
    pub unverified: bool,
    /// Join time of the current stay, in ms.
    pub joined_ms: Option<i64>,
    pub now_ms: i64,
}

const DAY_MS: i64 = 86_400_000;

/// The first rule that keeps `f` out. Moderation first (blocked roles, quarantine, flags, timeout), then
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
        (f.timed_out, Refusal::TimedOut),
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
    pub extras: sqlx::types::Json<Extras>,
    pub thread_id: Option<i64>,
    /// An entry code is set (the hash itself is never read into this row).
    pub has_code: bool,
}

impl Giveaway {
    pub fn running(&self) -> bool {
        self.status == "running"
    }
}

/// A giveaway to make.
#[derive(Clone, Deserialize)]
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
    pub extras: Extras,
    /// The entry code members type after they press Enter; None: no code.
    pub code: Option<String>,
    /// Queue the first post in the same transaction (the dashboard); the command posts at once.
    pub post_job: bool,
}

/// Never shows the entry code, so a `?new` in a log line cannot write it.
impl std::fmt::Debug for New {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("New")
            .field("guild_id", &self.guild_id)
            .field("prize", &self.prize)
            .field("winners", &self.winners)
            .field("duration_secs", &self.duration_secs)
            .field("req", &self.req)
            .field("extras", &self.extras)
            .field("code", &self.code.as_ref().map(|_| "<hidden>"))
            .finish_non_exhaustive()
    }
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
    if let Some(p) = n
        .code
        .as_deref()
        .and_then(|c| code_problem(c, n.extras.reaction))
    {
        return Some(p);
    }
    n.extras.problem().or_else(|| n.req.problem())
}

const COLS: &str =
    "id, guild_id, channel_id, message_id, prize, host_id, sponsor_id, created_by, winners,
    status, ends_at, ended_at, paused_at, req, button_label, button_emoji, winner_ids, past_winners,
    claim_hours, needs_repost, issue, shown_entries, draws, created_at, extras, thread_id,
    (code_hash IS NOT NULL) AS has_code";

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
           winners, ends_at, req, button_label, button_emoji, claim_hours, needs_repost, extras)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(secs => $8), $9, $10, $11, $12, $13, $14)
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
    .bind(sqlx::types::Json(&n.extras))
    .fetch_one(&mut *tx)
    .await?;
    let mut g = g;
    if let Some(code) = &n.code {
        sqlx::query("UPDATE giveaways SET code_hash = $2 WHERE id = $1")
            .bind(g.id)
            .bind(code_hash(g.id, code))
            .execute(&mut *tx)
            .await?;
        g.has_code = true;
    }
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
/// Job that takes the winner role back from one member.
pub const ROLE_OFF_JOB: &str = "giveaway_role_off";
/// Job that puts new winners into the winner thread (made at the first draw) and takes replaced
/// winners out.
pub const THREAD_JOB: &str = "giveaway_thread";
/// Job that closes (archives and locks) the winner thread.
pub const THREAD_CLOSE_JOB: &str = "giveaway_thread_close";

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

/// What a typed entry code did.
#[derive(Debug, PartialEq, Eq)]
pub enum CodeTry {
    Right,
    /// Wrong; this many tries are left before the wait.
    Wrong(i16),
    /// Too many wrong tries: wait this many minutes (rounded up).
    Wait(i64),
}

/// Checks `typed` against the entry code of giveaway `id` for `user`. The try is counted before
/// the compare, in one statement, so parallel tries cannot pass the limit: the third wrong try
/// starts a 10 minute wait, and during the wait no try is compared at all.
pub async fn try_code(
    pool: &PgPool,
    guild: i64,
    id: i64,
    user: i64,
    typed: &str,
) -> Result<CodeTry> {
    let n: Option<i16> = sqlx::query_scalar(
        "INSERT INTO giveaway_code_tries AS t (giveaway_id, guild_id, user_id, tries, locked_until)
         VALUES ($1, $2, $3, 1, NULL)
         ON CONFLICT (giveaway_id, user_id) DO UPDATE SET
           tries = CASE WHEN t.locked_until IS NULL THEN t.tries + 1 ELSE 1 END,
           locked_until = CASE WHEN t.locked_until IS NULL AND t.tries + 1 >= $4
             THEN now() + make_interval(mins => $5) END
         WHERE t.locked_until IS NULL OR t.locked_until <= now()
         RETURNING tries",
    )
    .bind(id)
    .bind(guild)
    .bind(user)
    .bind(CODE_TRIES)
    .bind(CODE_WAIT_MINS as i32)
    .fetch_optional(pool)
    .await?;
    let Some(n) = n else {
        return Ok(CodeTry::Wait(code_wait(pool, id, user).await?.unwrap_or(1)));
    };
    let stored: Option<String> =
        sqlx::query_scalar("SELECT code_hash FROM giveaways WHERE id = $1 AND guild_id = $2")
            .bind(id)
            .bind(guild)
            .fetch_optional(pool)
            .await?
            .flatten();
    // A giveaway without a code takes no code, so nothing is right.
    if stored.is_some_and(|h| same(&h, &code_hash(id, typed))) {
        sqlx::query("DELETE FROM giveaway_code_tries WHERE giveaway_id = $1 AND user_id = $2")
            .bind(id)
            .bind(user)
            .execute(pool)
            .await?;
        return Ok(CodeTry::Right);
    }
    Ok(match n >= CODE_TRIES {
        true => CodeTry::Wait(CODE_WAIT_MINS),
        false => CodeTry::Wrong(CODE_TRIES - n),
    })
}

/// Minutes `user` still waits before the next entry code try (rounded up); None: no wait.
pub async fn code_wait(pool: &PgPool, id: i64, user: i64) -> Result<Option<i64>> {
    let secs: Option<f64> = sqlx::query_scalar(
        "SELECT extract(epoch FROM locked_until - now())::float8 FROM giveaway_code_tries
         WHERE giveaway_id = $1 AND user_id = $2 AND locked_until > now()",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(secs.map(|s| ((s / 60.0).ceil() as i64).max(1)))
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
         WHERE id = $1 AND guild_id = $2
           AND ((status = 'running' AND paused_at IS NULL) OR (status = 'ended' AND draws = 0))
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
/// new winner, so a restart after the save still announces, and nothing is sent twice. The claim
/// places follow the winners ([`sync_claims`]); `auto`: a redraw because a claim time ran out.
#[allow(clippy::too_many_arguments)]
pub async fn save_draw(
    pool: &PgPool,
    guild: i64,
    id: i64,
    seen: i32,
    winners: &[i64],
    new: &[i64],
    key: &str,
    auto: bool,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let old: Option<(Vec<i64>, sqlx::types::Json<Extras>)> = sqlx::query_as(
        "SELECT winner_ids, extras FROM giveaways WHERE id = $1 AND guild_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((old, extras)) = old else {
        return Ok(false);
    };
    // Winners this draw replaced: they leave the winner thread and lose the winner role now.
    let gone: Vec<i64> = old.into_iter().filter(|u| !winners.contains(u)).collect();
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
    sync_claims(&mut tx, guild, id, seen == 0, winners, new, auto).await?;
    let (now, draws) = (Utc::now().timestamp(), seen + 1);
    let payload =
        serde_json::json!({ "id": id, "draws": draws, "key": key, "won": new, "gone": gone });
    crate::jobs::enqueue(&mut *tx, guild, ANNOUNCE_JOB, payload, now).await?;
    if extras.thread && (!new.is_empty() || !gone.is_empty()) {
        let payload = serde_json::json!({ "id": id, "won": new, "gone": gone });
        crate::jobs::enqueue(&mut *tx, guild, THREAD_JOB, payload, now).await?;
    }
    if let Some(role) = extras.winner_role {
        for u in &gone {
            let payload = serde_json::json!({ "id": id, "user": u, "role": role.get() as i64 });
            crate::jobs::enqueue(&mut *tx, guild, ROLE_OFF_JOB, payload, now).await?;
        }
    }
    for u in new {
        let payload = serde_json::json!({ "id": id, "draws": draws, "user": u });
        crate::jobs::enqueue(&mut *tx, guild, DM_JOB, payload, now).await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// The job that redraws one place when its claim time ran out.
pub const CLAIM_JOB: &str = "giveaway_claim";
/// Hours a redrawn or rerolled winner has to claim.
pub const REDRAW_HOURS: i32 = 24;
/// Redraws of one place because nobody claimed it; then staff are told.
pub const MAX_REDRAWS: i16 = 3;

/// One winner place and its claim.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Claim {
    pub giveaway_id: i64,
    pub place: i16,
    pub user_id: i64,
    pub deadline: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub redraws: i16,
}

/// Gives each new winner a place: a place a replaced winner left keeps its redraw count, other
/// winners keep theirs. The first draw gives the giveaway's claim time, a later draw 24 hours.
/// Queues one claim job per new place at its deadline.
async fn sync_claims(
    tx: &mut sqlx::PgConnection,
    guild: i64,
    id: i64,
    first: bool,
    winners: &[i64],
    new: &[i64],
    auto: bool,
) -> Result<()> {
    let mut free: Vec<(i16, i16)> = sqlx::query_as(
        "DELETE FROM giveaway_claims WHERE giveaway_id = $1 AND NOT (user_id = ANY($2))
         RETURNING place, redraws",
    )
    .bind(id)
    .bind(winners)
    .fetch_all(&mut *tx)
    .await?;
    let held: Vec<(i16, i64)> =
        sqlx::query_as("SELECT place, user_id FROM giveaway_claims WHERE giveaway_id = $1")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
    free.sort();
    let mut taken: Vec<i16> = held.iter().map(|h| h.0).collect();
    taken.extend(free.iter().map(|f| f.0));
    let mut next = 1i16;
    let hours: i32 = match first {
        true => {
            sqlx::query_scalar("SELECT claim_hours FROM giveaways WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?
        }
        false => REDRAW_HOURS,
    };
    for &u in new.iter().filter(|u| !held.iter().any(|h| h.1 == **u)) {
        let (place, redraws) = match free.is_empty() {
            false => free.remove(0),
            true => {
                while taken.contains(&next) {
                    next += 1;
                }
                taken.push(next);
                (next, 0)
            }
        };
        let deadline: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO giveaway_claims (giveaway_id, guild_id, place, user_id, deadline, redraws)
             VALUES ($1, $2, $3, $4, now() + make_interval(hours => $5), $6) RETURNING deadline",
        )
        .bind(id)
        .bind(guild)
        .bind(place)
        .bind(u)
        .bind(hours)
        .bind(redraws + auto as i16)
        .fetch_one(&mut *tx)
        .await?;
        let job = serde_json::json!({"id": id, "place": place, "user": u});
        crate::jobs::enqueue(&mut *tx, guild, CLAIM_JOB, job, deadline.timestamp()).await?;
    }
    Ok(())
}

/// What a Claim press did.
#[derive(Debug, PartialEq, Eq)]
pub enum Claimed {
    /// Claimed now; the server of the giveaway.
    Done(i64),
    Already,
    /// The claim time is over.
    Late,
    NotWinner,
}

/// Claims the prize of giveaway `id` for `user`, once and only in time. `guild`: the server the
/// press came from (None in a DM).
pub async fn claim(pool: &PgPool, id: i64, user: i64, guild: Option<i64>) -> Result<Claimed> {
    let done: Option<i64> = sqlx::query_scalar(
        "UPDATE giveaway_claims SET claimed_at = now()
         WHERE giveaway_id = $1 AND user_id = $2 AND claimed_at IS NULL AND deadline > now()
           AND ($3::bigint IS NULL OR guild_id = $3)
         RETURNING guild_id",
    )
    .bind(id)
    .bind(user)
    .bind(guild)
    .fetch_optional(pool)
    .await?;
    if let Some(g) = done {
        return Ok(Claimed::Done(g));
    }
    let row: Option<bool> = sqlx::query_scalar(
        "SELECT claimed_at IS NOT NULL FROM giveaway_claims WHERE giveaway_id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(true) => Claimed::Already,
        Some(false) => Claimed::Late,
        None => Claimed::NotWinner,
    })
}

/// The redraw count of place `place` when `user` still holds it unclaimed and its time is over.
/// It also closes the claim at once, so a press during the redraw cannot claim a place that is
/// being drawn again.
pub async fn claim_due(
    pool: &PgPool,
    guild: i64,
    id: i64,
    place: i16,
    user: i64,
) -> Result<Option<i16>> {
    Ok(sqlx::query_scalar(
        "UPDATE giveaway_claims SET deadline = LEAST(deadline, now())
         WHERE giveaway_id = $1 AND guild_id = $2 AND place = $3 AND user_id = $4
           AND claimed_at IS NULL AND deadline <= now() + interval '10 seconds'
         RETURNING redraws",
    )
    .bind(id)
    .bind(guild)
    .bind(place)
    .bind(user)
    .fetch_optional(pool)
    .await?)
}

/// Every place of giveaway `id` whose claim time is over and that may be drawn again (fewer than
/// [`MAX_REDRAWS`]): their winners, so one redraw replaces them all. Closes their claims.
pub async fn claims_due(pool: &PgPool, guild: i64, id: i64) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "UPDATE giveaway_claims SET deadline = LEAST(deadline, now())
         WHERE giveaway_id = $1 AND guild_id = $2 AND claimed_at IS NULL
           AND deadline <= now() + interval '10 seconds' AND redraws < $3
         RETURNING user_id",
    )
    .bind(id)
    .bind(guild)
    .bind(MAX_REDRAWS)
    .fetch_all(pool)
    .await?)
}

/// The claim places of `ids`, by giveaway and place.
pub async fn claims(pool: &PgPool, guild: i64, ids: &[i64]) -> Result<Vec<Claim>> {
    Ok(sqlx::query_as(
        "SELECT giveaway_id, place, user_id, deadline, claimed_at, redraws FROM giveaway_claims
         WHERE guild_id = $1 AND giveaway_id = ANY($2) ORDER BY giveaway_id, place",
    )
    .bind(guild)
    .bind(ids)
    .fetch_all(pool)
    .await?)
}

/// Staff set the claim time by hand: a running giveaway gives it to its winners; on an ended
/// one every winner who has not claimed yet has `hours` from now. False when it was cancelled
/// or is not found.
pub async fn set_claim_time(
    pool: &PgPool,
    guild: i64,
    id: i64,
    hours: i64,
) -> Result<Result<bool, String>> {
    if !(CLAIM_HOURS.0..=CLAIM_HOURS.1).contains(&hours) {
        return Ok(Err(format!(
            "The claim time is {} to {} hours.",
            CLAIM_HOURS.0, CLAIM_HOURS.1
        )));
    }
    let mut tx = pool.begin().await?;
    let n = sqlx::query(
        "UPDATE giveaways SET claim_hours = $3
         WHERE id = $1 AND guild_id = $2 AND status IN ('running', 'ended')",
    )
    .bind(id)
    .bind(guild)
    .bind(hours as i32)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(Ok(false));
    }
    let moved: Vec<(i16, i64, DateTime<Utc>)> = sqlx::query_as(
        "UPDATE giveaway_claims SET deadline = now() + make_interval(hours => $3)
         WHERE giveaway_id = $1 AND guild_id = $2 AND claimed_at IS NULL
         RETURNING place, user_id, deadline",
    )
    .bind(id)
    .bind(guild)
    .bind(hours as i32)
    .fetch_all(&mut *tx)
    .await?;
    // The new deadlines get new jobs; free old ones go, and a held one finds its deadline not
    // reached and does nothing.
    sqlx::query(
        "DELETE FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3
           AND (locked_until IS NULL OR locked_until < now())",
    )
    .bind(guild)
    .bind(CLAIM_JOB)
    .bind(id.to_string())
    .execute(&mut *tx)
    .await?;
    for (place, user, deadline) in moved {
        let job = serde_json::json!({"id": id, "place": place, "user": user});
        crate::jobs::enqueue(&mut *tx, guild, CLAIM_JOB, job, deadline.timestamp()).await?;
    }
    tx.commit().await?;
    Ok(Ok(true))
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
    /// Paused during an attack: it can end once the attack is over.
    Paused,
}

/// Moves the end job of a running giveaway to now. Only one end job exists per giveaway: a job
/// the scheduler holds is left alone, and a lost job is made again.
pub async fn end_now(pool: &PgPool, guild: i64, id: i64) -> Result<EndNow> {
    let mut tx = pool.begin().await?;
    let paused: Option<bool> = sqlx::query_scalar(
        "SELECT paused_at IS NOT NULL FROM giveaways
         WHERE id = $1 AND guild_id = $2 AND status = 'running' FOR UPDATE",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?;
    match paused {
        None => return Ok(EndNow::NotRunning),
        Some(true) => return Ok(EndNow::Paused),
        Some(false) => {}
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

/// Puts the unleased end job of giveaway `id` at `ends`, or queues one when there is none. A job
/// the scheduler holds now is left alone: it is deleted when it ends, so a move of it would be lost
/// ([`end_due`] makes that run wait).
async fn move_end_job(
    tx: &mut sqlx::PgConnection,
    guild: i64,
    id: i64,
    ends: DateTime<Utc>,
) -> Result<()> {
    let moved = sqlx::query(
        "UPDATE scheduled_jobs SET run_at = $4 WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3
           AND (locked_until IS NULL OR locked_until < now())",
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

/// Whether end job `job` of giveaway `id` may draw now. A paused giveaway, or one whose end moved
/// later after the job was taken, waits: it keeps one pending end job (made here when no other
/// exists; a paused one checks again in an hour, and [`resume`] moves it). True when the giveaway
/// is not running, so [`claim_end`] decides.
pub async fn end_due(pool: &PgPool, guild: i64, id: i64, job: i64) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, DateTime<Utc>, bool)> = sqlx::query_as(
        "SELECT status, ends_at, paused_at IS NOT NULL FROM giveaways
         WHERE id = $1 AND guild_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((status, ends, paused)) = row else {
        return Ok(true);
    };
    // A few seconds early is the scheduler tick, not a moved end.
    if status != "running" || (!paused && ends <= Utc::now() + chrono::Duration::seconds(10)) {
        return Ok(true);
    }
    let other: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scheduled_jobs
           WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3 AND id <> $4)",
    )
    .bind(guild)
    .bind(END_JOB)
    .bind(id.to_string())
    .bind(job)
    .fetch_one(&mut *tx)
    .await?;
    if !other {
        let at = match paused {
            true => Utc::now().timestamp() + 3600,
            false => ends.timestamp(),
        };
        crate::jobs::enqueue(&mut *tx, guild, END_JOB, serde_json::json!({"id": id}), at).await?;
    }
    tx.commit().await?;
    Ok(false)
}

/// Pauses the running giveaways of the server that are not paused yet: a raid, a lockdown, panic
/// mode or an anti-nuke case started. Entries are refused and no draw runs until [`resume`].
/// Returns how many it paused.
pub async fn pause(pool: &PgPool, guild: i64) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE giveaways SET paused_at = now()
         WHERE guild_id = $1 AND status = 'running' AND paused_at IS NULL",
    )
    .bind(guild)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Ends the pause of the server's giveaways: each one ends later by the time it was paused, and
/// its end job moves with it. The message shows the new end at the next refresh. Returns the ids.
pub async fn resume(pool: &PgPool, guild: i64) -> Result<Vec<i64>> {
    let mut tx = pool.begin().await?;
    let moved: Vec<(i64, DateTime<Utc>)> = sqlx::query_as(
        "UPDATE giveaways SET ends_at = ends_at + (now() - paused_at), paused_at = NULL,
           shown_entries = -1
         WHERE guild_id = $1 AND status = 'running' AND paused_at IS NOT NULL
         RETURNING id, ends_at",
    )
    .bind(guild)
    .fetch_all(&mut *tx)
    .await?;
    for (id, ends) in &moved {
        move_end_job(&mut tx, guild, *id, *ends).await?;
    }
    tx.commit().await?;
    Ok(moved.into_iter().map(|(id, _)| id).collect())
}

/// Per server with running giveaways: (server, all of them paused, any of them paused). One read
/// for the whole bot, so the pause check costs one query per round, not one per server.
pub async fn pause_states(pool: &PgPool) -> Result<Vec<(i64, bool, bool)>> {
    Ok(sqlx::query_as(
        "SELECT guild_id, bool_and(paused_at IS NOT NULL), bool_or(paused_at IS NOT NULL)
         FROM giveaways WHERE status = 'running' GROUP BY guild_id",
    )
    .fetch_all(pool)
    .await?)
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
    extras: &Extras,
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
    if let Some(p) = req.problem().or_else(|| extras.problem()) {
        return Ok(Err(p));
    }
    // Reaction entry stays as the giveaway was posted: the message has its button or its reaction.
    Ok(Ok(sqlx::query(
        "UPDATE giveaways SET prize = $3, winners = $4, req = $5, shown_entries = -1,
           extras = $6 || jsonb_build_object('reaction', COALESCE(extras->'reaction', 'false'))
         WHERE id = $1 AND guild_id = $2 AND status = 'running'",
    )
    .bind(id)
    .bind(guild)
    .bind(prize)
    .bind(winners as i16)
    .bind(sqlx::types::Json(req))
    .bind(sqlx::types::Json(extras))
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

/// A role that a restore made again: the rules and the winner role of the running giveaways name
/// the new id.
pub async fn relink_roles(pool: &PgPool, guild: i64, old: i64, new: i64) -> Result<u64> {
    sqlx::query(
        "UPDATE giveaways SET extras = jsonb_set(extras, '{winner_role}', to_jsonb($3::text))
         WHERE guild_id = $1 AND status = 'running' AND extras->>'winner_role' = $2::text",
    )
    .bind(guild)
    .bind(old.to_string())
    .bind(new.to_string())
    .execute(pool)
    .await?;
    Ok(sqlx::query(
        "UPDATE giveaways SET req = req
           || jsonb_build_object('required', COALESCE((SELECT jsonb_agg(CASE WHEN x = to_jsonb($2::text)
                THEN to_jsonb($3::text) ELSE x END) FROM jsonb_array_elements(req->'required') x), '[]'))
           || jsonb_build_object('blocked', COALESCE((SELECT jsonb_agg(CASE WHEN x = to_jsonb($2::text)
                THEN to_jsonb($3::text) ELSE x END) FROM jsonb_array_elements(req->'blocked') x), '[]'))
           || jsonb_build_object('bypass', COALESCE((SELECT jsonb_agg(CASE WHEN x = to_jsonb($2::text)
                THEN to_jsonb($3::text) ELSE x END) FROM jsonb_array_elements(req->'bypass') x), '[]'))
           || jsonb_build_object('extra', COALESCE((SELECT jsonb_agg(CASE WHEN x->>'role' = $2::text
                THEN jsonb_set(x, '{role}', to_jsonb($3::text)) ELSE x END)
                FROM jsonb_array_elements(req->'extra') x), '[]'))
         WHERE guild_id = $1 AND status = 'running' AND strpos(req::text, '\"' || $2::text || '\"') > 0",
    )
    .bind(guild)
    .bind(old.to_string())
    .bind(new.to_string())
    .execute(pool)
    .await?
    .rows_affected())
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
            timed_out: false,
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
        let mut t = facts(&r7);
        t.timed_out = true;
        assert_eq!(
            refusal(&req, None, None, &t),
            Some(Refusal::TimedOut),
            "a bypass role never skips a timeout"
        );
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
            extras: Default::default(),
            code: None,
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
            extras: Default::default(),
            code: None,
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
            "giveaway.winners",
            false
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
            "giveaway.winners",
            false
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

    async fn end_jobs(pool: &PgPool, gid: i64, id: i64) -> Vec<(i64, i64)> {
        sqlx::query_as(
            "SELECT id, extract(epoch FROM run_at)::bigint FROM scheduled_jobs
             WHERE guild_id = $1 AND kind = $2 AND payload->>'id' = $3 ORDER BY id",
        )
        .bind(gid)
        .bind(END_JOB)
        .bind(id.to_string())
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn clean(pool: &PgPool, gid: i64) {
        for t in ["giveaways", "scheduled_jobs"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {t} WHERE guild_id = $1"
            )))
            .bind(gid)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// A pause refuses entries and every draw; the resume moves the end and its job later by the
    /// paused time. An end job that the scheduler took during the pause waits and keeps one job.
    #[tokio::test]
    async fn a_pause_holds_entries_and_the_draw_and_moves_the_end() {
        let Some((pool, gid)) = db(4).await else {
            return;
        };
        let g = create(&pool, &new(gid), 25).await.unwrap().unwrap();
        let job = end_jobs(&pool, gid, g.id).await[0].0;
        assert_eq!(pause(&pool, gid).await.unwrap(), 1);
        assert_eq!(pause(&pool, gid).await.unwrap(), 0, "paused once");
        assert_eq!(
            enter(&pool, gid, g.id, 5, 1).await.unwrap(),
            Entered::Closed
        );
        assert_eq!(end_now(&pool, gid, g.id).await.unwrap(), EndNow::Paused);
        assert!(
            !end_due(&pool, gid, g.id, job).await.unwrap(),
            "no draw while paused"
        );
        // The run that waits is deleted when it ends: one new job takes its place.
        assert_eq!(end_jobs(&pool, gid, g.id).await.len(), 2);
        sqlx::query("DELETE FROM scheduled_jobs WHERE id = $1")
            .bind(job)
            .execute(&pool)
            .await
            .unwrap();
        let job = end_jobs(&pool, gid, g.id).await[0].0;
        assert!(!end_due(&pool, gid, g.id, -1).await.unwrap());
        assert_eq!(end_jobs(&pool, gid, g.id).await.len(), 1, "no second job");
        assert!(claim_end(&pool, gid, g.id).await.unwrap().is_none());
        let states = pause_states(&pool).await.unwrap();
        assert_eq!(states.iter().find(|r| r.0 == gid), Some(&(gid, true, true)));

        // The scheduler holds the job while the pause ends: the resume queues a new one.
        sqlx::query(
            "UPDATE scheduled_jobs SET locked_until = now() + interval '1 minute' WHERE id = $1",
        )
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE giveaways SET paused_at = now() - interval '10 minutes' WHERE id = $1")
            .bind(g.id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(resume(&pool, gid).await.unwrap(), vec![g.id]);
        assert!(resume(&pool, gid).await.unwrap().is_empty());
        let after = get(&pool, gid, g.id).await.unwrap().unwrap();
        let moved = (after.ends_at - g.ends_at).num_seconds();
        assert!(
            (595..=610).contains(&moved),
            "moved by the pause: {moved} s"
        );
        let jobs = end_jobs(&pool, gid, g.id).await;
        assert_eq!(jobs.len(), 2, "the held job and a new one");
        assert!(
            (jobs[1].1 - after.ends_at.timestamp()).abs() <= 1,
            "at the new end"
        );
        // The held run wakes: the end is later now, so it waits and leaves the new job.
        assert!(!end_due(&pool, gid, g.id, job).await.unwrap());
        assert_eq!(end_jobs(&pool, gid, g.id).await.len(), 2);
        sqlx::query("DELETE FROM scheduled_jobs WHERE id = $1")
            .bind(job)
            .execute(&pool)
            .await
            .unwrap();
        let last = end_jobs(&pool, gid, g.id).await[0].0;
        assert_eq!(enter(&pool, gid, g.id, 5, 1).await.unwrap(), Entered::New);
        assert_eq!(end_now(&pool, gid, g.id).await.unwrap(), EndNow::Queued);
        assert!(end_due(&pool, gid, g.id, last).await.unwrap());
        assert!(claim_end(&pool, gid, g.id).await.unwrap().is_some());
        clean(&pool, gid).await;
    }

    /// Each winner gets a place with the claim time; a claim works once and only for the winner;
    /// a redraw of one place keeps the others, gives the new winner 24 hours and counts the
    /// automatic redraws; staff can move the claim time of an ended giveaway.
    #[tokio::test]
    async fn claims_follow_the_winners() {
        let Some((pool, gid)) = db(7).await else {
            return;
        };
        let mut n = new(gid);
        n.claim_hours = 5;
        let g = create(&pool, &n, 25).await.unwrap().unwrap();
        let c = claim_end(&pool, gid, g.id).await.unwrap().unwrap();
        assert!(save_draw(
            &pool,
            gid,
            g.id,
            c.draws,
            &[5, 6],
            &[5, 6],
            "giveaway.winners",
            false
        )
        .await
        .unwrap());
        let rows = claims(&pool, gid, &[g.id]).await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|c| (c.place, c.user_id))
                .collect::<Vec<_>>(),
            [(1, 5), (2, 6)]
        );
        let hours = (rows[0].deadline - Utc::now()).num_minutes();
        assert!(
            (295..=300).contains(&hours),
            "the giveaway's claim time: {hours} min"
        );
        assert_eq!(
            claim(&pool, g.id, 7, None).await.unwrap(),
            Claimed::NotWinner
        );
        assert_eq!(
            claim(&pool, g.id, 5, Some(gid)).await.unwrap(),
            Claimed::Done(gid)
        );
        assert_eq!(
            claim(&pool, g.id, 5, Some(gid)).await.unwrap(),
            Claimed::Already
        );
        assert_eq!(
            claim_due(&pool, gid, g.id, 2, 6).await.unwrap(),
            None,
            "not over yet"
        );

        sqlx::query("UPDATE giveaway_claims SET deadline = now() - interval '1 minute' WHERE user_id = 6 AND giveaway_id = $1")
            .bind(g.id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(claim(&pool, g.id, 6, None).await.unwrap(), Claimed::Late);
        assert_eq!(claim_due(&pool, gid, g.id, 2, 6).await.unwrap(), Some(0));
        assert_eq!(
            claim_due(&pool, 1, g.id, 2, 6).await.unwrap(),
            None,
            "other server"
        );
        let g2 = get(&pool, gid, g.id).await.unwrap().unwrap();
        assert!(save_draw(
            &pool,
            gid,
            g.id,
            g2.draws,
            &[5, 8],
            &[8],
            "giveaway.reroll",
            true
        )
        .await
        .unwrap());
        let rows = claims(&pool, gid, &[g.id]).await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|c| (c.place, c.user_id, c.redraws, c.claimed_at.is_some()))
                .collect::<Vec<_>>(),
            [(1, 5, 0, true), (2, 8, 1, false)]
        );
        let h = (rows[1].deadline - Utc::now()).num_hours();
        assert!((23..=24).contains(&h), "a redrawn winner has 24 hours");

        assert!(set_claim_time(&pool, gid, g.id, 0).await.unwrap().is_err());
        assert_eq!(set_claim_time(&pool, gid, g.id, 2).await.unwrap(), Ok(true));
        let rows = claims(&pool, gid, &[g.id]).await.unwrap();
        assert!((rows[1].deadline - Utc::now()).num_minutes() <= 120);
        assert!(
            rows[0].claimed_at.is_some(),
            "a claimed place stays claimed"
        );
        let jobs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
        )
        .bind(gid)
        .bind(CLAIM_JOB)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            jobs, 1,
            "old jobs go when the claim time moves; one unclaimed place"
        );

        // Fewer winners than places, then a reroll of all: each place number once.
        let g3 = get(&pool, gid, g.id).await.unwrap().unwrap();
        assert!(save_draw(
            &pool,
            gid,
            g.id,
            g3.draws,
            &[5],
            &[],
            "giveaway.reroll",
            false
        )
        .await
        .unwrap());
        let g4 = get(&pool, gid, g.id).await.unwrap().unwrap();
        assert!(save_draw(
            &pool,
            gid,
            g.id,
            g4.draws,
            &[10, 11, 12],
            &[10, 11, 12],
            "giveaway.reroll",
            false
        )
        .await
        .unwrap());
        let mut places: Vec<i16> = claims(&pool, gid, &[g.id])
            .await
            .unwrap()
            .iter()
            .map(|c| c.place)
            .collect();
        places.sort();
        assert_eq!(places, [1, 2, 3]);
        sqlx::query("UPDATE giveaway_claims SET deadline = now() WHERE giveaway_id = $1")
            .bind(g.id)
            .execute(&pool)
            .await
            .unwrap();
        let mut due = claims_due(&pool, gid, g.id).await.unwrap();
        due.sort();
        assert_eq!(
            due,
            [10, 11, 12],
            "one redraw takes every place that ran out"
        );
        clean(&pool, gid).await;
    }

    /// A remade role takes the place of the old one in every role list of a running giveaway.
    #[tokio::test]
    async fn a_remade_role_moves_into_the_rules() {
        let Some((pool, gid)) = db(6).await else {
            return;
        };
        let mut n = new(gid);
        n.req = Req {
            required: vec![r(11)],
            blocked: vec![r(11), r(12)],
            bypass: vec![r(13)],
            extra: vec![Extra {
                role: r(11),
                entries: 3,
            }],
            ..Req::default()
        };
        let g = create(&pool, &n, 25).await.unwrap().unwrap();
        assert_eq!(relink_roles(&pool, gid, 11, 21).await.unwrap(), 1);
        assert_eq!(relink_roles(&pool, gid, 99, 98).await.unwrap(), 0);
        let req = get(&pool, gid, g.id).await.unwrap().unwrap().req.0;
        assert_eq!(req.required, vec![r(21)]);
        assert_eq!(req.blocked, vec![r(21), r(12)]);
        assert_eq!(req.bypass, vec![r(13)]);
        assert_eq!(
            req.extra,
            vec![Extra {
                role: r(21),
                entries: 3
            }]
        );
        clean(&pool, gid).await;
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

    #[test]
    fn extras_and_codes_have_limits() {
        assert_eq!(Extras::default().problem(), None);
        let role = Extras {
            winner_role: Some(r(4)),
            role_hours: 0,
            ..Extras::default()
        };
        assert!(role.problem().is_some(), "a winner role needs a time");
        let thread = Extras {
            thread: true,
            thread_hours: 721,
            ..Extras::default()
        };
        assert!(thread.problem().is_some());
        assert_eq!(code_problem(" Pizza Party ", false), None);
        assert!(code_problem("abcde", false).is_some(), "too short");
        assert!(code_problem(&"a".repeat(51), false).is_some(), "too long");
        assert!(
            code_problem("pizza", true).is_some(),
            "a reaction cannot carry a code"
        );
        // Trimmed and in any case; the id is the salt.
        assert_eq!(code_hash(5, " PIZZA "), code_hash(5, "pizza"));
        assert_ne!(code_hash(5, "pizza"), code_hash(6, "pizza"));
        assert_eq!(code_hash(5, "pizza").len(), 64);
        assert!(same("ab", "ab") && !same("ab", "ac") && !same("ab", "abc"));
    }

    /// Three wrong codes give a 10 minute wait in which even the right code does nothing; a right
    /// code before that enters and clears the count. Parallel tries cannot pass the limit.
    #[tokio::test]
    async fn entry_codes_allow_three_wrong_tries_then_a_wait() {
        let Some((pool, gid)) = db(9).await else {
            return;
        };
        let n = New {
            code: Some("Pizza Party".into()),
            ..new(gid)
        };
        let g = create(&pool, &n, 25).await.unwrap().unwrap();
        assert!(g.has_code);
        let stored: String = sqlx::query_scalar("SELECT code_hash FROM giveaways WHERE id = $1")
            .bind(g.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!stored.contains("pizza"), "only the hash is kept");
        let t = |u: i64, c: &'static str| try_code(&pool, gid, g.id, u, c);
        assert_eq!(t(1, "nope").await.unwrap(), CodeTry::Wrong(2));
        assert_eq!(t(1, " pizza party ").await.unwrap(), CodeTry::Right);
        assert_eq!(
            t(1, "nope").await.unwrap(),
            CodeTry::Wrong(2),
            "a right code clears the count"
        );
        assert_eq!(t(2, "a").await.unwrap(), CodeTry::Wrong(2));
        assert_eq!(t(2, "b").await.unwrap(), CodeTry::Wrong(1));
        assert_eq!(t(2, "c").await.unwrap(), CodeTry::Wait(CODE_WAIT_MINS));
        assert_eq!(
            t(2, "pizza party").await.unwrap(),
            CodeTry::Wait(10),
            "no compare in the wait"
        );
        assert_eq!(code_wait(&pool, g.id, 2).await.unwrap(), Some(10));
        assert_eq!(code_wait(&pool, g.id, 1).await.unwrap(), None);
        sqlx::query("UPDATE giveaway_code_tries SET locked_until = now() WHERE user_id = 2")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            t(2, "pizza party").await.unwrap(),
            CodeTry::Right,
            "the wait is over"
        );

        // 20 tries at once by one member: at most 3 are compared.
        let runs: Vec<_> = (0..20)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move { try_code(&pool, gid, g.id, 3, "wrong").await.unwrap() })
            })
            .collect();
        let mut compared = 0;
        for r in runs {
            if !matches!(r.await.unwrap(), CodeTry::Wait(_)) {
                compared += 1;
            }
        }
        assert_eq!(compared, 2, "the third try starts the wait");
        clean(&pool, gid).await;
    }

    /// A redraw that replaces a winner queues the winner role removal for them, and the
    /// announcement lists them, so they leave the winner thread.
    #[tokio::test]
    async fn a_replaced_winner_loses_the_role() {
        let Some((pool, gid)) = db(8).await else {
            return;
        };
        let n = New {
            winners: 2,
            extras: Extras {
                winner_role: Some(r(44)),
                ..Extras::default()
            },
            ..new(gid)
        };
        let g = create(&pool, &n, 25).await.unwrap().unwrap();
        claim_end(&pool, gid, g.id).await.unwrap().unwrap();
        assert!(save_draw(
            &pool,
            gid,
            g.id,
            0,
            &[5, 6],
            &[5, 6],
            "giveaway.winners",
            false
        )
        .await
        .unwrap());
        assert!(
            save_draw(&pool, gid, g.id, 1, &[5, 7], &[7], "giveaway.reroll", false)
                .await
                .unwrap()
        );
        let off: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
        )
        .bind(gid)
        .bind(ROLE_OFF_JOB)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(off.len(), 1);
        assert_eq!(
            (off[0]["user"].as_i64(), off[0]["role"].as_i64()),
            (Some(6), Some(44))
        );
        let gone: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT payload->'gone' FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2
             ORDER BY id",
        )
        .bind(gid)
        .bind(ANNOUNCE_JOB)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(gone, [serde_json::json!([]), serde_json::json!([6])]);

        // A remade winner role moves into the extras; an edit keeps reaction entry as posted.
        let n = New {
            extras: Extras {
                winner_role: Some(r(11)),
                reaction: true,
                ..Extras::default()
            },
            ..new(gid)
        };
        let h = create(&pool, &n, 25).await.unwrap().unwrap();
        relink_roles(&pool, gid, 11, 21).await.unwrap();
        let x = get(&pool, gid, h.id).await.unwrap().unwrap().extras.0;
        assert_eq!(x.winner_role, Some(r(21)));
        let off = Extras {
            reaction: false,
            thread: true,
            ..x
        };
        assert_eq!(
            edit(&pool, gid, h.id, "Nitro", 1, &Req::default(), &off)
                .await
                .unwrap(),
            Ok(true)
        );
        let x = get(&pool, gid, h.id).await.unwrap().unwrap().extras.0;
        assert!(x.reaction && x.thread, "{x:?}");
        clean(&pool, gid).await;
    }
}

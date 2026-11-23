//! Giveaway templates and plans. A template keeps the settings of a giveaway under a name. A plan
//! starts a giveaway from such settings on a schedule written like a message schedule (`msgsched`:
//! a date, a repeat, several times), and may wait until its last giveaway has ended. Both are rows
//! of `giveaway_plans`; a plan has a `spec`, a template has none. A deleted template or plan never
//! changes a giveaway that already runs: a giveaway keeps its own copy of the settings.
use crate::{
    giveaways::{self, Extras, New, Req},
    msgsched,
    sections::GiveawaysCfg,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{
    types::chrono::{DateTime, Utc},
    types::Json,
    PgPool,
};

/// Templates and plans per server.
pub const MAX_TEMPLATES: i64 = 25;
pub const MAX_PLANS: i64 = 10;
pub const MAX_NAME: usize = 60;
/// A run later than this (Brim was off) is skipped, not started.
pub const LATE_SECS: i64 = 3600;
/// The last run of a plan (a one-time start) is started up to this late.
pub const LAST_GRACE_SECS: i64 = 6 * 3600;
/// A run that waits for the plan's giveaway to end looks again after this long.
pub const RETRY_SECS: i64 = 60;
/// One stored template or plan.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Plan {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub channel_id: Option<i64>,
    pub prize: String,
    pub host_id: Option<i64>,
    pub sponsor_id: Option<i64>,
    pub winners: i16,
    pub duration_secs: i32,
    pub claim_hours: Option<i32>,
    pub req: Json<Req>,
    pub extras: Json<Extras>,
    pub spec: Option<Value>,
    pub next_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub wait_for_end: bool,
    pub held: bool,
    pub issue: Option<String>,
    pub last_run: Option<DateTime<Utc>>,
    pub created_by: i64,
}

const COLS: &str = "id, guild_id, name, channel_id, prize, host_id, sponsor_id, winners, duration_secs,
    claim_hours, req, extras, spec, next_at, enabled, wait_for_end, held, issue, last_run, created_by";

/// What a save sends. `spec` set: a plan; `None`: a template.
#[derive(Clone, Debug, Deserialize)]
pub struct Save {
    pub name: String,
    pub channel_id: Option<i64>,
    pub prize: String,
    pub host_id: Option<i64>,
    pub sponsor_id: Option<i64>,
    pub winners: i64,
    pub duration_secs: i64,
    pub claim_hours: Option<i64>,
    #[serde(default)]
    pub req: Req,
    #[serde(default)]
    pub extras: Extras,
    pub spec: Option<Value>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub wait_for_end: bool,
}

/// The settings of a giveaway, by reference.
struct Fields<'a> {
    prize: &'a str,
    host: Option<i64>,
    sponsor: Option<i64>,
    winners: i64,
    secs: i64,
    claim: Option<i64>,
    req: &'a Req,
    extras: &'a Extras,
}

/// The giveaway these settings make. Never carries an entry code.
fn new_of(guild: i64, by: i64, channel: i64, f: Fields, cfg: &GiveawaysCfg, post_job: bool) -> New {
    New {
        guild_id: guild,
        channel_id: channel,
        prize: f.prize.into(),
        host_id: f.host,
        sponsor_id: f.sponsor,
        created_by: by,
        winners: f.winners,
        duration_secs: f.secs,
        req: f.req.clone(),
        button_label: cfg.button_label.clone(),
        button_emoji: cfg.button_emoji.clone(),
        claim_hours: f.claim.unwrap_or(i64::from(cfg.claim_hours)),
        extras: f.extras.clone(),
        code: None,
        post_job,
    }
}

impl Save {
    fn problem(&self, guild: i64, by: i64) -> Option<String> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME {
            return Some(format!("The name is 1 to {MAX_NAME} characters."));
        }
        if self.spec.is_some() && self.channel_id.is_none() {
            return Some("Pick a channel.".into());
        }
        let f = Fields {
            prize: &self.prize,
            host: self.host_id,
            sponsor: self.sponsor_id,
            winners: self.winners,
            secs: self.duration_secs,
            claim: self.claim_hours,
            req: &self.req,
            extras: &self.extras,
        };
        let n = new_of(
            guild,
            by,
            self.channel_id.unwrap_or(0),
            f,
            &GiveawaysCfg::default(),
            false,
        );
        giveaways::new_problem(&n)
    }
}

/// Saves a template or plan (`id`: change that one), or says what is wrong. Counts and the name
/// check run under a per-server lock, so two saves cannot both pass. Saving a plan sets its next
/// run and clears its issue and any waiting run; `by` is who runs it from now on.
pub async fn save(
    pool: &PgPool,
    guild: i64,
    id: Option<i64>,
    s: &Save,
    by: i64,
    now: DateTime<Utc>,
) -> Result<Result<Plan, String>> {
    if let Some(p) = s.problem(guild, by) {
        return Ok(Err(p));
    }
    let (spec, next) = match &s.spec {
        None => (None, None),
        Some(v) => {
            let parsed = match msgsched::parse(v, now) {
                Ok(p) => p,
                Err(e) => return Ok(Err(e)),
            };
            if msgsched::messages(&parsed).is_some() {
                return Ok(Err(
                    "A giveaway cannot start after a number of messages.".into()
                ));
            }
            if msgsched::too_close(&parsed, now) {
                return Ok(Err(format!(
                    "Runs must be at least {} minutes apart.",
                    msgsched::MIN_MINUTES
                )));
            }
            let next = msgsched::next(&parsed, now);
            if s.enabled && next.is_none() {
                return Ok(Err("This schedule has no run ahead.".into()));
            }
            (Some(msgsched::to_json(&parsed)), next.filter(|_| s.enabled))
        }
    };
    let plan = spec.is_some();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('giveaway_plans:' || $1::text, 0))")
        .bind(guild)
        .execute(&mut *tx)
        .await?;
    let name = s.name.trim();
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM giveaway_plans WHERE guild_id = $1 AND name = $2
           AND (spec IS NULL) = $3 AND id IS DISTINCT FROM $4)",
    )
    .bind(guild)
    .bind(name)
    .bind(!plan)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if taken {
        return Ok(Err(format!("A {} named “{name}” exists.", kind(plan))));
    }
    let cap = if plan { MAX_PLANS } else { MAX_TEMPLATES };
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM giveaway_plans WHERE guild_id = $1 AND (spec IS NULL) = $2
           AND id IS DISTINCT FROM $3",
    )
    .bind(guild)
    .bind(!plan)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if n >= cap {
        return Ok(Err(format!("A server has at most {cap} {}s.", kind(plan))));
    }
    let sql = if id.is_some() {
        "UPDATE giveaway_plans SET name = $2, channel_id = $3, prize = $4, host_id = $5,
           sponsor_id = $6, winners = $7, duration_secs = $8, claim_hours = $9, req = $10,
           extras = $11, spec = $12, next_at = $13, enabled = $14, wait_for_end = $15,
           held = false, issue = NULL, created_by = $16
         WHERE id = $17 AND guild_id = $1 AND (spec IS NULL) = $18 RETURNING "
    } else {
        "INSERT INTO giveaway_plans (guild_id, name, channel_id, prize, host_id, sponsor_id, winners,
           duration_secs, claim_hours, req, extras, spec, next_at, enabled, wait_for_end, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) RETURNING "
    };
    let q = sqlx::query_as::<_, Plan>(sqlx::AssertSqlSafe(format!("{sql}{COLS}")))
        .bind(guild)
        .bind(name)
        .bind(s.channel_id)
        .bind(s.prize.trim())
        .bind(s.host_id)
        .bind(s.sponsor_id)
        .bind(s.winners as i16)
        .bind(s.duration_secs as i32)
        .bind(s.claim_hours.map(|h| h as i32))
        .bind(Json(&s.req))
        .bind(Json(&s.extras))
        .bind(spec)
        .bind(next)
        .bind(plan && s.enabled)
        .bind(plan && s.wait_for_end)
        .bind(by);
    // `$17` and `$18` only in the update: the kind of an existing row never changes.
    let row = match id {
        Some(id) => q.bind(id).bind(!plan).fetch_optional(&mut *tx).await?,
        None => Some(q.fetch_one(&mut *tx).await?),
    };
    tx.commit().await?;
    Ok(row.ok_or_else(|| "That is gone.".to_string()))
}

fn kind(plan: bool) -> &'static str {
    if plan {
        "plan"
    } else {
        "template"
    }
}

/// The templates and plans of a server, templates first.
pub async fn list(pool: &PgPool, guild: i64) -> Result<Vec<Plan>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM giveaway_plans WHERE guild_id = $1 ORDER BY (spec IS NOT NULL), name LIMIT 100"
    )))
    .bind(guild)
    .fetch_all(pool)
    .await?)
}

/// Deletes a template or plan. Giveaways it started keep running.
pub async fn delete(pool: &PgPool, guild: i64, id: i64) -> Result<bool> {
    Ok(
        sqlx::query("DELETE FROM giveaway_plans WHERE id = $1 AND guild_id = $2")
            .bind(id)
            .bind(guild)
            .execute(pool)
            .await?
            .rows_affected()
            == 1,
    )
}

/// A channel that a restore made again.
pub async fn relink_channel(pool: &PgPool, guild: i64, old: i64, new: i64) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE giveaway_plans SET channel_id = $3 WHERE guild_id = $1 AND channel_id = $2",
    )
    .bind(guild)
    .bind(old)
    .bind(new)
    .execute(pool)
    .await?
    .rows_affected())
}

/// A deleted channel: its plans stop, with the reason on the row.
pub async fn channel_gone(pool: &PgPool, guild: i64, channel: i64, why: &str) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE giveaway_plans SET enabled = false, next_at = NULL, held = false, issue = $3
         WHERE guild_id = $1 AND channel_id = $2 AND enabled",
    )
    .bind(guild)
    .bind(channel)
    .bind(why)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Stops a plan whose saver lost their rights, if `by` still saved it last.
pub async fn pause(pool: &PgPool, guild: i64, id: i64, by: i64, why: &str) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE giveaway_plans SET enabled = false, next_at = NULL, held = false, issue = $4
         WHERE id = $1 AND guild_id = $2 AND created_by = $3 AND enabled",
    )
    .bind(id)
    .bind(guild)
    .bind(by)
    .bind(why)
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

/// A plan whose run failed (an unreadable row or a database error): looks again in a minute, with
/// the reason on it, so one bad row never blocks the plans behind it.
pub async fn delay(pool: &PgPool, id: i64, why: &str) -> Result<()> {
    sqlx::query(
        "UPDATE giveaway_plans SET next_at = now() + make_interval(secs => $2), issue = $3
         WHERE id = $1 AND enabled",
    )
    .bind(id)
    .bind(RETRY_SECS as f64)
    .bind(why)
    .execute(pool)
    .await?;
    Ok(())
}

/// Servers with a plan due in the next 30 seconds (the bot's pause round asks which are attacked).
pub async fn soon_guilds(pool: &PgPool) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT guild_id FROM giveaway_plans
         WHERE enabled AND next_at <= now() + interval '30 seconds'",
    )
    .fetch_all(pool)
    .await?)
}

/// A server under attack starts no giveaway: its plans due soon move a minute on, with the
/// reason, and the round that follows asks again. A plan never counts as late for this wait, and
/// it starts once, when the attack is over.
pub async fn postpone(pool: &PgPool, guild: i64) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE giveaway_plans SET next_at = now() + make_interval(secs => $2), issue = $3
         WHERE guild_id = $1 AND enabled AND next_at <= now() + interval '30 seconds'",
    )
    .bind(guild)
    .bind(RETRY_SECS as f64)
    .bind("Waits while the server is under attack, and starts when it is over.")
    .execute(pool)
    .await?
    .rows_affected())
}

/// The `limit` oldest due plans as (id, server, saver); the runner checks each saver, then calls
/// [`run`].
pub async fn due(pool: &PgPool, limit: i64) -> Result<Vec<(i64, i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT id, guild_id, created_by FROM giveaway_plans
         WHERE enabled AND next_at <= now() ORDER BY next_at LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// What a run did.
#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// Started this giveaway.
    Started(i64),
    /// Did not start; the reason is on the plan.
    Skipped(String),
    /// Waits for the plan's running giveaway to end.
    Held,
}

/// Runs one due plan, or `None` when another runner took it or it is not due. The next run is
/// stored in the same transaction that makes the giveaway, so a plan never starts twice (a restart,
/// a late job or a second runner see the moved time) and never loses a run to a crash. The limit
/// of running giveaways counts as for any start; a run over it is skipped with the reason.
/// `cfg`: the server's giveaway settings; `None` when giveaways are off. `up_since`: when the
/// runner started. A run is late by the time Brim was down (`up_since` minus its time), never by
/// the time it waited for a busy runner.
pub async fn run(
    pool: &PgPool,
    id: i64,
    by: i64,
    cfg: Option<&GiveawaysCfg>,
    up_since: DateTime<Utc>,
) -> Result<Option<Ran>> {
    let mut tx = pool.begin().await?;
    let Some(p): Option<Plan> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM giveaway_plans
         WHERE id = $1 AND enabled AND next_at <= now() AND created_by = $2 FOR UPDATE SKIP LOCKED"
    )))
    .bind(id)
    .bind(by)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    // The next run is after the run being made, even when this clock is behind the database's.
    let now = Utc::now().max(p.next_at.unwrap_or_else(Utc::now));
    let parsed = p.spec.as_ref().map(|v| msgsched::parse(v, now));
    let next = parsed
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .and_then(|s| msgsched::next(s, now));
    let late = p.next_at.map_or(0, |a| (up_since - a).num_seconds());
    let (ran, next_at, held) = 'run: {
        if let Some(Err(e)) = &parsed {
            break 'run (
                Ran::Skipped(format!("This schedule cannot run: {e}")),
                None,
                false,
            );
        }
        let Some(cfg) = cfg else {
            break 'run (
                Ran::Skipped("Skipped a run: Giveaways are off.".into()),
                next,
                false,
            );
        };
        let limit = if next.is_none() {
            LAST_GRACE_SECS
        } else {
            LATE_SECS
        };
        if !p.held && late > limit {
            let why = "Skipped a run: Brim was not running at the time.".to_string();
            break 'run (Ran::Skipped(why), next, false);
        }
        if p.wait_for_end {
            let open: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM giveaways WHERE guild_id = $1 AND plan_id = $2 AND status = 'running')",
            )
            .bind(p.guild_id)
            .bind(p.id)
            .fetch_one(&mut *tx)
            .await?;
            if open {
                let again = now + chrono::Duration::seconds(RETRY_SECS);
                break 'run (Ran::Held, Some(again), true);
            }
        }
        let f = Fields {
            prize: &p.prize,
            host: p.host_id,
            sponsor: p.sponsor_id,
            winners: i64::from(p.winners),
            secs: i64::from(p.duration_secs),
            claim: p.claim_hours.map(i64::from),
            req: &p.req,
            extras: &p.extras,
        };
        let n = new_of(
            p.guild_id,
            p.created_by,
            p.channel_id.unwrap_or(0),
            f,
            cfg,
            true,
        );
        match giveaways::create_in(&mut tx, &n, i64::from(cfg.max_running), Some(p.id)).await? {
            Ok(g) => (Ran::Started(g.id), next, false),
            Err(why) => (Ran::Skipped(format!("Skipped a run: {why}")), next, false),
        }
    };
    let (issue, ran_now) = match &ran {
        Ran::Skipped(why) => (Some(why.as_str()), false),
        Ran::Held => (None, false),
        Ran::Started(_) => (None, true),
    };
    // A plan with no run left (a one-time start, or a schedule that cannot run) switches itself off.
    let on = next_at.is_some();
    sqlx::query(
        "UPDATE giveaway_plans SET next_at = $2, enabled = $3, held = $4, issue = $5,
           last_run = CASE WHEN $6 THEN now() ELSE last_run END WHERE id = $1",
    )
    .bind(p.id)
    .bind(next_at)
    .bind(on)
    .bind(held)
    .bind(issue)
    .bind(ran_now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(ran))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use twilight_model::id::Id;

    async fn db(offset: i64) -> Option<(PgPool, i64)> {
        let url = crate::db::test_url().ok()?;
        let pool = crate::db::connect(&url).await.unwrap();
        let gid = 7_200_000_000 + std::process::id() as i64 * 10 + offset;
        for q in [
            "DELETE FROM giveaway_plans WHERE guild_id = $1",
            "DELETE FROM giveaways WHERE guild_id = $1",
            "DELETE FROM scheduled_jobs WHERE guild_id = $1",
        ] {
            sqlx::query(q).bind(gid).execute(&pool).await.unwrap();
        }
        Some((pool, gid))
    }

    fn hourly() -> Option<Value> {
        Some(json!({"tz": "UTC", "lines": [{"kind": "every", "minutes": 60}]}))
    }

    fn save_of(name: &str, spec: Option<Value>) -> Save {
        Save {
            name: name.into(),
            channel_id: spec.is_some().then_some(2),
            prize: "Nitro".into(),
            host_id: None,
            sponsor_id: None,
            winners: 1,
            duration_secs: 3600,
            claim_hours: None,
            req: Req::default(),
            extras: Extras::default(),
            enabled: spec.is_some(),
            spec,
            wait_for_end: false,
        }
    }

    fn cfg(max: u32) -> GiveawaysCfg {
        GiveawaysCfg {
            max_running: max,
            ..GiveawaysCfg::default()
        }
    }

    async fn due_now(pool: &PgPool, id: i64) {
        sqlx::query(
            "UPDATE giveaway_plans SET next_at = now() - interval '1 minute' WHERE id = $1",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn made(pool: &PgPool, plan: i64) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM giveaways WHERE plan_id = $1")
            .bind(plan)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A name, the count per kind and the schedule are checked; a template and a plan may share a name.
    #[tokio::test]
    async fn saves_check_the_name_the_count_and_the_schedule() {
        let Some((pool, g)) = db(1).await else { return };
        let now = Utc::now();
        let pool = &pool;
        let ok = |s: Save| async move { save(pool, g, None, &s, 9, now).await.unwrap() };
        assert!(ok(save_of("Weekly", None)).await.is_ok());
        assert!(ok(save_of("Weekly", None))
            .await
            .unwrap_err()
            .contains("exists"));
        assert!(
            ok(save_of("Weekly", hourly())).await.is_ok(),
            "a plan may share a template's name"
        );
        assert!(ok(save_of(" ", None)).await.is_err());
        let mut no_channel = save_of("x", hourly());
        no_channel.channel_id = None;
        assert!(ok(no_channel).await.is_err());
        let by_messages = Some(json!({"tz": "UTC", "lines": [{"kind": "messages", "n": 20}]}));
        assert!(ok(save_of("m", by_messages))
            .await
            .unwrap_err()
            .contains("messages"));
        let fast = Some(json!({"tz": "UTC", "lines": [{"kind": "every", "minutes": 5}]}));
        assert!(
            ok(save_of("f", fast)).await.is_err(),
            "runs 15 minutes apart at least"
        );
        let past = Some(
            json!({"tz": "UTC", "lines": [{"kind": "once", "date": "2020-01-01", "time": "10:00"}]}),
        );
        assert!(ok(save_of("p", past))
            .await
            .unwrap_err()
            .contains("no run ahead"));
        let mut long = save_of("l", None);
        long.duration_secs = 5;
        assert!(
            ok(long).await.is_err(),
            "the giveaway limits hold for a template too"
        );
        for i in 0..MAX_PLANS - 1 {
            assert!(ok(save_of(&format!("p{i}"), hourly())).await.is_ok());
        }
        assert!(ok(save_of("one more", hourly()))
            .await
            .unwrap_err()
            .contains("at most"));
    }

    /// Four runners at once make one giveaway; the next run is stored with it; the giveaway has no
    /// entry code and knows its plan.
    #[tokio::test]
    async fn a_due_plan_starts_one_giveaway_once() {
        let Some((pool, g)) = db(2).await else { return };
        let p = save(&pool, g, None, &save_of("Hourly", hourly()), 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            run(&pool, p.id, 9, Some(&cfg(25)), Utc::now())
                .await
                .unwrap(),
            None,
            "not due yet"
        );
        due_now(&pool, p.id).await;
        let c = cfg(25);
        let (a, b, c2, d) = tokio::join!(
            run(&pool, p.id, 9, Some(&c), Utc::now()),
            run(&pool, p.id, 9, Some(&c), Utc::now()),
            run(&pool, p.id, 9, Some(&c), Utc::now()),
            run(&pool, p.id, 9, Some(&c), Utc::now())
        );
        let started = [a, b, c2, d]
            .into_iter()
            .map(|r| r.unwrap())
            .filter(|r| matches!(r, Some(Ran::Started(_))))
            .count();
        assert_eq!(started, 1, "one run claims the plan");
        assert_eq!(made(&pool, p.id).await, 1);
        let (has_code, by, post): (bool, i64, i64) = sqlx::query_as(
            "SELECT code_hash IS NOT NULL, created_by,
               (SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'giveaway_repost')
             FROM giveaways WHERE plan_id = $2",
        )
        .bind(g)
        .bind(p.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!has_code);
        assert_eq!(by, 9);
        assert_eq!(post, 1, "the post is queued");
        let after = list(&pool, g)
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.id == p.id)
            .unwrap();
        assert!(after.next_at.unwrap() > Utc::now(), "the next run is ahead");
        assert!(after.last_run.is_some() && after.issue.is_none());
        // Another saver's run does not count: the plan runs with the rights of who saved it last.
        due_now(&pool, p.id).await;
        assert_eq!(
            run(&pool, p.id, 10, Some(&c), Utc::now()).await.unwrap(),
            None
        );
    }

    /// The limit of running giveaways holds for a plan: the run is skipped with the reason and the
    /// plan goes on.
    #[tokio::test]
    async fn a_plan_respects_the_limit_of_running_giveaways() {
        let Some((pool, g)) = db(3).await else { return };
        let p = save(&pool, g, None, &save_of("Hourly", hourly()), 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        let c = cfg(1);
        due_now(&pool, p.id).await;
        assert!(matches!(
            run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap(),
            Some(Ran::Started(_))
        ));
        due_now(&pool, p.id).await;
        let r = run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap();
        assert!(
            matches!(&r, Some(Ran::Skipped(w)) if w.contains("limit")),
            "{r:?}"
        );
        assert_eq!(made(&pool, p.id).await, 1);
        let after = list(&pool, g).await.unwrap().remove(0);
        assert!(after.enabled && after.issue.is_some() && after.next_at.unwrap() > Utc::now());
    }

    /// "Start after the existing giveaway ends": the run waits, looks again after a minute, and
    /// starts once the giveaway has ended.
    #[tokio::test]
    async fn a_plan_can_wait_for_its_giveaway_to_end() {
        let Some((pool, g)) = db(4).await else { return };
        let mut s = save_of("Hourly", hourly());
        s.wait_for_end = true;
        let p = save(&pool, g, None, &s, 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        let c = cfg(25);
        due_now(&pool, p.id).await;
        assert!(matches!(
            run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap(),
            Some(Ran::Started(_))
        ));
        due_now(&pool, p.id).await;
        assert_eq!(
            run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap(),
            Some(Ran::Held)
        );
        let held = list(&pool, g).await.unwrap().remove(0);
        assert!(held.held && held.enabled);
        assert!(held.next_at.unwrap() < Utc::now() + chrono::Duration::seconds(RETRY_SECS + 5));
        // Still running: it waits again, and a late look is not skipped.
        sqlx::query("UPDATE giveaway_plans SET next_at = now() - interval '5 hours' WHERE id = $1")
            .bind(p.id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap(),
            Some(Ran::Held)
        );
        assert_eq!(made(&pool, p.id).await, 1);
        sqlx::query("UPDATE giveaways SET status = 'ended' WHERE plan_id = $1")
            .bind(p.id)
            .execute(&pool)
            .await
            .unwrap();
        due_now(&pool, p.id).await;
        assert!(matches!(
            run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap(),
            Some(Ran::Started(_))
        ));
        assert_eq!(made(&pool, p.id).await, 2);
        assert!(!list(&pool, g).await.unwrap().remove(0).held);
    }

    /// A run that Brim missed by more than an hour is skipped; the last run of a plan has 6 hours
    /// and then switches the plan off; giveaways turned off skip the run with the reason.
    #[tokio::test]
    async fn late_runs_are_skipped_and_a_one_time_plan_switches_itself_off() {
        let Some((pool, g)) = db(5).await else { return };
        let c = cfg(25);
        let p = save(&pool, g, None, &save_of("Hourly", hourly()), 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE giveaway_plans SET next_at = now() - interval '3 hours' WHERE id = $1")
            .bind(p.id)
            .execute(&pool)
            .await
            .unwrap();
        let r = run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap();
        assert!(
            matches!(&r, Some(Ran::Skipped(w)) if w.contains("not running")),
            "{r:?}"
        );
        assert_eq!(made(&pool, p.id).await, 0);
        due_now(&pool, p.id).await;
        let r = run(&pool, p.id, 9, None, Utc::now()).await.unwrap();
        assert!(
            matches!(&r, Some(Ran::Skipped(w)) if w.contains("off")),
            "{r:?}"
        );
        // A one-time plan: a date ahead, then forced due 3 hours late.
        let date = (Utc::now() + chrono::Duration::days(2))
            .format("%Y-%m-%d")
            .to_string();
        let once =
            Some(json!({"tz": "UTC", "lines": [{"kind": "once", "date": date, "time": "10:00"}]}));
        let o = save(&pool, g, None, &save_of("Once", once), 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        // The time has come: the date is past and no other run is left.
        sqlx::query("UPDATE giveaway_plans SET next_at = now() - interval '3 hours', spec = jsonb_set(spec, '{lines,0,date}', '\"2020-01-01\"') WHERE id = $1")
            .bind(o.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            run(&pool, o.id, 9, Some(&c), Utc::now()).await.unwrap(),
            Some(Ran::Started(_))
        ));
        let done = list(&pool, g)
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.id == o.id)
            .unwrap();
        assert!(!done.enabled && done.next_at.is_none(), "no run left");
        assert!(due(&pool, 100).await.unwrap().iter().all(|d| d.0 != o.id));
    }

    /// A restore that makes a role or channel again moves it into templates and plans; a deleted
    /// channel stops the plans in it; a delete of a plan leaves its giveaways.
    #[tokio::test]
    async fn restores_deletes_and_lost_rights_reach_plans() {
        let Some((pool, g)) = db(6).await else { return };
        let mut s = save_of("Hourly", hourly());
        s.req.required = vec![Id::new(5)];
        s.extras.winner_role = Some(Id::new(5));
        let p = save(&pool, g, None, &s, 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        let mut t = save_of("Tpl", None);
        t.req.blocked = vec![Id::new(5)];
        let t = save(&pool, g, None, &t, 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        giveaways::relink_roles(&pool, g, 5, 6).await.unwrap();
        relink_channel(&pool, g, 2, 3).await.unwrap();
        let all = list(&pool, g).await.unwrap();
        let (a, b) = (
            all.iter().find(|x| x.id == p.id).unwrap(),
            all.iter().find(|x| x.id == t.id).unwrap(),
        );
        assert_eq!(a.req.required, vec![Id::new(6)]);
        assert_eq!(a.extras.winner_role, Some(Id::new(6)));
        assert_eq!(a.channel_id, Some(3));
        assert_eq!(b.req.blocked, vec![Id::new(6)]);
        assert_eq!(channel_gone(&pool, g, 3, "gone").await.unwrap(), 1);
        let a = list(&pool, g)
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.id == p.id)
            .unwrap();
        assert!(!a.enabled && a.issue.as_deref() == Some("gone"));
        // Lost rights: only the saver of record is paused.
        let again = save(&pool, g, Some(p.id), &s, 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        assert!(
            again.enabled && again.issue.is_none(),
            "saving turns it on again"
        );
        assert!(!pause(&pool, g, p.id, 10, "x").await.unwrap());
        assert!(pause(&pool, g, p.id, 9, "x").await.unwrap());
        due_now(&pool, p.id).await;
        assert!(due(&pool, 100).await.unwrap().iter().all(|d| d.0 != p.id));
        assert!(delete(&pool, g, p.id).await.unwrap());
        assert!(!delete(&pool, g, p.id).await.unwrap());
    }

    /// A busy runner never makes a plan late: only the time Brim was down counts.
    #[tokio::test]
    async fn a_plan_the_runner_was_late_to_is_not_skipped() {
        let Some((pool, g)) = db(7).await else { return };
        let c = cfg(25);
        let p = save(&pool, g, None, &save_of("Busy", hourly()), 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE giveaway_plans SET next_at = now() - interval '5 hours' WHERE id = $1")
            .bind(p.id)
            .execute(&pool)
            .await
            .unwrap();
        // The runner started 5 hours ago and took until now to reach this plan: it starts.
        let up = Utc::now() - chrono::Duration::hours(6);
        assert!(matches!(
            run(&pool, p.id, 9, Some(&c), up).await.unwrap(),
            Some(Ran::Started(_))
        ));
        // After a down time of 5 hours it is skipped.
        sqlx::query("UPDATE giveaway_plans SET next_at = now() - interval '5 hours' WHERE id = $1")
            .bind(p.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            run(&pool, p.id, 9, Some(&c), Utc::now()).await.unwrap(),
            Some(Ran::Skipped(_))
        ));
        sqlx::query("DELETE FROM giveaway_plans WHERE guild_id = $1")
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM giveaways WHERE guild_id = $1")
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// A server under attack: its plans move on and keep their place; the others are not touched.
    #[tokio::test]
    async fn an_attacked_server_postpones_its_due_plans() {
        let Some((pool, g)) = db(8).await else { return };
        let p = save(&pool, g, None, &save_of("Soon", hourly()), 9, Utc::now())
            .await
            .unwrap()
            .unwrap();
        due_now(&pool, p.id).await;
        assert!(soon_guilds(&pool).await.unwrap().contains(&g));
        assert_eq!(postpone(&pool, g).await.unwrap(), 1);
        assert!(!soon_guilds(&pool).await.unwrap().contains(&g));
        assert!(due(&pool, 100).await.unwrap().iter().all(|d| d.0 != p.id));
        let after = list(&pool, g).await.unwrap().remove(0);
        assert!(after.enabled && after.issue.unwrap().starts_with("Waits while"));
        assert_eq!(postpone(&pool, g).await.unwrap(), 0);
        sqlx::query("DELETE FROM giveaway_plans WHERE guild_id = $1")
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Many plans due in the same minute across many servers: every one starts once, none is
    /// skipped as late, and 8 lanes side by side never start one twice. `PLAN_LOAD` sets the number
    /// of plans (default 400; the load numbers use 10000).
    #[tokio::test]
    async fn many_due_plans_all_start_once() {
        let Some((pool, _)) = db(9).await else { return };
        let n: i64 = std::env::var("PLAN_LOAD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400);
        let base = 7_300_000_000 + std::process::id() as i64 * 100_000;
        let all = "guild_id >= $1 AND guild_id < $1 + 100000";
        let clean = || async {
            for t in ["giveaway_plans", "giveaways", "scheduled_jobs"] {
                sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {t} WHERE {all}")))
                    .bind(base)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        clean().await;
        // 10 plans per server, all due now.
        sqlx::query(
            "INSERT INTO giveaway_plans (guild_id, name, channel_id, prize, winners, duration_secs,
               spec, next_at, enabled, created_by)
             SELECT $1 + i / 10, 'P' || i, 2, 'Nitro', 1, 3600,
               '{\"tz\": \"UTC\", \"lines\": [{\"kind\": \"every\", \"minutes\": 60}]}',
               now() - interval '1 minute', true, 9
             FROM generate_series(0, $2 - 1) i",
        )
        .bind(base)
        .bind(n)
        .execute(&pool)
        .await
        .unwrap();
        let t = std::time::Instant::now();
        let c = std::sync::Arc::new(cfg(25));
        let mut lanes = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let (pool, c) = (pool.clone(), c.clone());
            lanes.spawn(async move {
                let mut started = 0;
                loop {
                    // Only these plans, picked in the query: `due` takes the oldest of all servers,
                    // so another test's older due plans fill its 200 and this lane saw none.
                    let due: Vec<(i64, i64, i64)> = sqlx::query_as(
                        "SELECT id, guild_id, created_by FROM giveaway_plans
                         WHERE enabled AND next_at <= now()
                           AND guild_id >= $1 AND guild_id < $1 + 100000
                         ORDER BY next_at LIMIT 200",
                    )
                    .bind(base)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
                    if due.is_empty() {
                        return started;
                    }
                    for (id, _, by) in due {
                        if let Some(Ran::Started(_)) =
                            run(&pool, id, by, Some(&c), Utc::now()).await.unwrap()
                        {
                            started += 1;
                        }
                    }
                }
            });
        }
        let mut started = 0;
        while let Some(r) = lanes.join_next().await {
            started += r.unwrap();
        }
        eprintln!(
            "{n} due plans started in {:.1} s",
            t.elapsed().as_secs_f64()
        );
        assert_eq!(started, n);
        let twice: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM (SELECT plan_id FROM giveaways WHERE plan_id IS NOT NULL
               AND guild_id >= $1 AND guild_id < $1 + 100000 GROUP BY plan_id HAVING count(*) > 1) x",
        )
        .bind(base)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(twice, 0);
        clean().await;
    }
}

//! The "Timeout Ended" log. Discord sends no event when a timeout runs out, so a timed-out member
//! gets one `timeout_end` job (kept up to date from member updates) that reads the member again
//! at the end. A timeout lifted early drops the job: that has its own member update log.
use anyhow::{Context, Result};
use brim_core::jobs;
use brim_logging::{
    embed::{Log, GREEN},
    LogEvent, LogKind,
};
use serde_json::{json, Value};
use twilight_model::{
    gateway::payload::incoming::MemberUpdate,
    id::{marker::GuildMarker, Id},
};

use crate::app::{now_secs, App};

const KIND: &str = "timeout_end";
/// A log later than this after the end says it was sent late (the bot was offline or busy).
const LATE_SECS: i64 = 120;

/// Keeps one job per timed-out member at the end of the timeout; nothing when the server sends
/// this log nowhere. `since` stays the time the timeout was first seen across extensions.
pub async fn on_member_update(app: &App, m: &MemberUpdate) -> Result<()> {
    let cfg = app.cfg(m.guild_id.get()).await?;
    let routed = cfg
        .logging
        .target(LogKind::Member, Some(LogEvent::MemberTimeoutEnd));
    if routed.is_none() {
        return Ok(());
    }
    let (g, u, now) = (m.guild_id.get() as i64, m.user.id.get(), now_secs());
    let until = m.communication_disabled_until.map(|t| t.as_secs());
    let Some(until) = until.filter(|&t| t > now) else {
        jobs::cancel_user(&app.pool, g, &[KIND], u, None).await?;
        return Ok(());
    };
    let old = jobs::of_user(&app.pool, g, KIND, u).await?;
    if old.as_ref().is_some_and(|(_, at, _)| *at == until) {
        return Ok(());
    }
    let since = old.and_then(|(_, _, p)| p["since"].as_i64()).unwrap_or(now);
    queue(app, g, u, until, since).await
}

/// One job for the member: the new one first, then the older ones go.
async fn queue(app: &App, g: i64, user: u64, until: i64, since: i64) -> Result<()> {
    let p = json!({"user": user, "until_secs": until, "since": since});
    let id = jobs::enqueue(&app.pool, g, KIND, p, until).await?;
    jobs::cancel_user(&app.pool, g, &[KIND], user, Some(id)).await?;
    Ok(())
}

/// The job: logs the end when the member is no longer timed out. A member who left, or whose
/// timeout was made longer in the meantime (a missed update), gets no log now.
pub async fn run(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let user = p["user"].as_u64().context("job payload has no user")?;
    let until = p["until_secs"]
        .as_i64()
        .context("job payload has no until_secs")?;
    let since = p["since"].as_i64().unwrap_or(until);
    let Some(member) = crate::http_err::member(&app.http, gid, Id::new(user)).await? else {
        return Ok(());
    };
    let now = now_secs();
    let still = member
        .communication_disabled_until
        .map(|t| t.as_secs())
        .filter(|&t| t > now);
    if let Some(t) = still {
        return queue(app, gid.get() as i64, user, t, since).await;
    }
    let case: Option<i32> = sqlx::query_scalar(
        // A case made up to 5 minutes before the timeout was first seen is its case.
        "SELECT case_no FROM cases WHERE guild_id = $1 AND user_id = $2 AND kind = 'timeout'
           AND created_at >= to_timestamp($3::float8) - interval '5 minutes'
         ORDER BY case_no DESC LIMIT 1",
    )
    .bind(gid.get() as i64)
    .bind(user as i64)
    .bind(since as f64)
    .fetch_optional(&app.pool)
    .await?;
    crate::events::send_event_log(
        app,
        gid,
        LogKind::Member,
        LogEvent::MemberTimeoutEnd,
        Id::new(user),
        log(user, since, until, case, now),
    )
    .await
}

pub(crate) fn log(user: u64, since: i64, until: i64, case: Option<i32>, now: i64) -> Log {
    let late = if now - until > LATE_SECS {
        "Brim was not running at the end of the timeout"
    } else {
        ""
    };
    let mut l = Log::new("Timeout Ended", GREEN)
        .fact("Member", "member", &brim_logging::embed::user(user))
        .fact("Timed Out", "started", &format!("<t:{since}:F>"))
        .fact("Ended", "ended", &format!("<t:{until}:F>"))
        .var("case", &case.map(|c| c.to_string()).unwrap_or_default());
    if let Some(c) = case {
        l = l.line("Case", &format!("`{c}`"));
    }
    l.fact("Logged Late", "late", late)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::T;
    use twilight_model::util::Timestamp;

    /// A member gets one job at the end of the timeout; an extension moves it and keeps `since`, a
    /// lifted timeout drops it, and a server with no channel for the log queues nothing.
    #[tokio::test]
    async fn a_timeout_has_one_end_job_that_follows_its_changes() {
        let Some(t) = T::new(700).await else { return };
        t.server_config(
            0,
            t.owner(),
            serde_json::json!({"logging": {"fallback": "55"}}),
        )
        .await;
        t.server_config(1, t.owner(), serde_json::json!({})).await;
        let (user, now) = (t.base + 9, now_secs());
        let jobs = |g: u64| {
            let pool = t.app.pool.clone();
            let g = t.g(g).get() as i64;
            async move {
                sqlx::query_as::<_, (i64, Value)>(
                    "SELECT extract(epoch from run_at)::bigint, payload FROM scheduled_jobs
                     WHERE guild_id = $1 AND kind = 'timeout_end'",
                )
                .bind(g)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        let up = |g: u64, until: Option<i64>| -> MemberUpdate {
            let until = until.map(|u| Timestamp::from_secs(u).unwrap().iso_8601().to_string());
            serde_json::from_value(serde_json::json!({
                "guild_id": t.g(g).to_string(), "roles": [], "deaf": false, "mute": false,
                "joined_at": "2026-01-01T00:00:00.000000+00:00", "pending": false, "avatar": null,
                "communication_disabled_until": until, "nick": null, "premium_since": null,
                "user": {"id": user.to_string(), "username": "u", "discriminator": "0", "avatar": null},
            }))
            .unwrap()
        };
        let end = now + 3600;
        on_member_update(&t.app, &up(0, Some(end))).await.unwrap();
        on_member_update(&t.app, &up(0, Some(end))).await.unwrap();
        let [(at, p)] = &jobs(0).await[..] else {
            panic!("one job")
        };
        assert_eq!(
            (*at, p["user"].as_u64(), p["until_secs"].as_i64()),
            (end, Some(user), Some(end))
        );
        let since = p["since"].as_i64().unwrap();
        on_member_update(&t.app, &up(0, Some(end + 7200)))
            .await
            .unwrap();
        let [(at, p)] = &jobs(0).await[..] else {
            panic!("one job after an extension")
        };
        assert_eq!((*at, p["since"].as_i64()), (end + 7200, Some(since)));
        on_member_update(&t.app, &up(1, Some(end))).await.unwrap();
        assert!(jobs(1).await.is_empty(), "no channel for the log: no job");
        on_member_update(&t.app, &up(0, None)).await.unwrap();
        assert!(jobs(0).await.is_empty(), "lifted: the job goes");
        t.cleanup().await;
    }

    fn text(l: Log) -> String {
        serde_json::to_string(&l.build()).unwrap()
    }

    #[test]
    fn the_log_names_the_times_and_says_when_it_is_late() {
        let t = text(log(5, 100, 200, Some(7), 210));
        assert!(t.contains("Timeout Ended") && t.contains("<t:100:F>") && t.contains("<t:200:F>"));
        assert!(t.contains("`7`") && !t.contains("Logged Late"));
        let t = text(log(5, 100, 200, None, 200 + LATE_SECS + 1));
        assert!(t.contains("Logged Late") && !t.contains("Case"));
    }
}

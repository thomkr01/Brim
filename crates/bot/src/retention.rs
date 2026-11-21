//! Each server's own retention limits (`retention` section, security review, owner decision 2026-11-17).
//! Run by the hourly sweep in `welcome::purge_left`. Open reports, appeals and tickets are never
//! deleted, and the newest case and ticket of a server stay so their numbers are never given again.
use anyhow::Result;
use brim_core::{
    jobs,
    sections::{RetentionCfg, RETENTION_MIN_DAYS},
};
use serde_json::Value;
use sqlx::PgPool;

/// The DELETE for one limit: $1 the server, $2 the days.
fn delete_for(field: &str) -> Option<&'static str> {
    Some(match field {
        // A case with a timed action still running (a temp ban, a decaying warn) stays until it ends.
        "cases_days" => {
            "DELETE FROM cases WHERE guild_id = $1 AND created_at < now() - make_interval(days => $2)
               AND NOT COALESCE(active AND expires_at > now(), false)
               AND case_no < (SELECT max(case_no) FROM cases WHERE guild_id = $1)"
        }
        // Only the copies of members' messages; what staff added stays with its case. Proofs and
        // edit history of a deleted case go with it (ON DELETE CASCADE).
        "proofs_days" => {
            "DELETE FROM case_proofs WHERE guild_id = $1 AND verified
               AND added_at < now() - make_interval(days => $2)"
        }
        // Edit history goes with the note (ON DELETE CASCADE).
        "notes_days" => {
            "DELETE FROM notes WHERE guild_id = $1 AND created_at < now() - make_interval(days => $2)"
        }
        "reports_days" => {
            "DELETE FROM reports WHERE guild_id = $1 AND status <> 'open'
               AND created_at < now() - make_interval(days => $2)"
        }
        "appeals_days" => {
            "DELETE FROM appeals WHERE guild_id = $1 AND status <> 'open'
               AND created_at < now() - make_interval(days => $2)"
        }
        // Transcript rows go with the ticket (ON DELETE CASCADE); the file in Discord stays.
        "tickets_days" => {
            "DELETE FROM tickets WHERE guild_id = $1 AND closed_at < now() - make_interval(days => $2)
               AND number < (SELECT max(number) FROM tickets WHERE guild_id = $1)"
        }
        // The current version always stays, so the bot and the dashboard keep their config.
        "config_history_days" => {
            "DELETE FROM guild_config WHERE guild_id = $1 AND created_at < now() - make_interval(days => $2)
               AND version < (SELECT max(version) FROM guild_config WHERE guild_id = $1)"
        }
        _ => return None,
    })
}

/// A server with no `retention` section (or no config at all) gets the defaults. `only`: one server
/// (tests), else every server.
pub async fn sweep(pool: &PgPool, only: Option<i64>) -> Result<()> {
    // A member's last seen name and avatar (`profile_log`), 90 days after the member was last
    // seen: the same in every server, with no setting (owner decision 2026-11-20). The daily member list
    // read keeps a present member's row fresh, so this takes the rows of servers that turned the
    // logs off and of leaves nothing else caught.
    let old = sqlx::query(
        "DELETE FROM profile_snapshot WHERE seen_at < now() - interval '90 days'
           AND ($1::bigint IS NULL OR guild_id = $1)",
    )
    .bind(only)
    .execute(pool)
    .await;
    if let Err(e) = old {
        tracing::warn!(?e, "pruning old stored names and avatars failed");
    }
    let rows: Vec<(i64, Option<Value>)> = sqlx::query_as(
        "SELECT o.guild_id, c.config->'retention' FROM guild_owners o
         LEFT JOIN LATERAL (SELECT config FROM guild_config WHERE guild_id = o.guild_id
                            ORDER BY version DESC LIMIT 1) c ON true
         WHERE $1::bigint IS NULL OR o.guild_id = $1",
    )
    .bind(only)
    .fetch_all(pool)
    .await?;
    for (guild, v) in rows {
        // The dashboard refuses a bad section, so this only meets old or hand-made rows.
        let cfg: RetentionCfg = match v.map(serde_json::from_value) {
            None => RetentionCfg::default(),
            Some(Ok(c)) => c,
            // Fails toward keeping: an unreadable section deletes nothing.
            Some(Err(e)) => {
                tracing::warn!(%e, guild, "bad retention section, nothing deleted");
                continue;
            }
        };
        for (field, days) in cfg.days() {
            let Some(sql) = delete_for(field).filter(|_| days > 0) else {
                continue;
            };
            let days = days.clamp(RETENTION_MIN_DAYS, 3650) as i32;
            match sqlx::query(sql).bind(guild).bind(days).execute(pool).await {
                // One summary line per run, as a job so a restart never loses it. Best effort.
                Ok(r) if field == "cases_days" && r.rows_affected() > 0 => {
                    let p = serde_json::json!({"count": r.rows_affected(), "days": days});
                    let at = crate::app::now_secs();
                    if let Err(e) = jobs::enqueue(pool, guild, "old_cases_log", p, at).await {
                        tracing::warn!(?e, guild, "queueing the retention log failed");
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(?e, guild, field, "retention sweep failed"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_limit_has_a_delete() {
        for (f, _) in RetentionCfg::default().days() {
            assert!(delete_for(f).is_some(), "{f}");
        }
    }

    /// Old closed rows go, open ones and the newest numbered rows stay; 0 keeps.
    #[tokio::test]
    async fn sweep_follows_each_servers_limits() {
        let Some(t) = crate::test_support::T::new(107).await else {
            return;
        };
        let (g, pool) = (t.g(0).get() as i64, &t.app.pool);
        let old = "now() - interval '400 days'";
        for q in [
            "DELETE FROM cases WHERE guild_id = $1",
            "DELETE FROM reports WHERE guild_id = $1",
            "DELETE FROM tickets WHERE guild_id = $1",
            "DELETE FROM notes WHERE guild_id = $1",
            "DELETE FROM guild_owners WHERE guild_id = $1",
            "DELETE FROM guild_config WHERE guild_id = $1",
            "DELETE FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'old_cases_log'",
            "DELETE FROM profile_snapshot WHERE guild_id = $1",
        ] {
            sqlx::query(q).bind(g).execute(pool).await.unwrap();
        }
        t.server_config(
            0,
            t.owner(),
            json!({"retention": {"cases_days": 30, "notes_days": 0}}),
        )
        .await;
        for (no, at) in [(1, old), (2, old), (3, "now()")] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, created_at) VALUES ($1, {no}, 'warn', 5, 6, {at})"
            )))
            .bind(g)
            .execute(pool)
            .await
            .unwrap();
        }
        // The only case left is old but the newest: it stays, so case 2 is never given again.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, created_at, active, expires_at)
             VALUES ($1, 4, 'ban', 5, 6, {old}, true, now() + interval '1 day')"
        )))
        .bind(g)
        .execute(pool)
        .await
        .unwrap();
        for (msg, status, at) in [
            (4, "open", old),
            (5, "dismissed", old),
            (6, "dismissed", "now()"),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, message_id, content, status, created_at)
                 VALUES ($1, 1, 2, 3, {msg}, 'x', '{status}', {at})"
            )))
            .bind(g)
            .execute(pool)
            .await
            .unwrap();
        }
        for (no, closed) in [(1, Some(old)), (2, None), (3, Some(old))] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO tickets (guild_id, number, user_id, closed_at) VALUES ($1, {no}, 5, {})",
                closed.unwrap_or("NULL")
            )))
            .bind(g)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO notes (guild_id, user_id, author_id, text, created_at) VALUES ($1, 5, 6, 'x', {old})"
        )))
        .bind(g)
        .execute(pool)
        .await
        .unwrap();
        // A stored name: 100 days old goes (always 90 days), 80 days old and a new one stay.
        sqlx::query(
            "INSERT INTO profile_snapshot (guild_id, user_id, username, seen_at) VALUES
             ($1, 1, 'a', now() - interval '100 days'), ($1, 2, 'b', now() - interval '80 days'), ($1, 3, 'c', now())",
        )
        .bind(g)
        .execute(pool)
        .await
        .unwrap();
        sweep(pool, Some(g)).await.unwrap();
        let ids = |q: &'static str| async move {
            sqlx::query_scalar::<_, i32>(q)
                .bind(g)
                .fetch_all(pool)
                .await
                .unwrap()
        };
        // Case 1 and 2 are old; 3 is new, 4 still has a running ban.
        assert_eq!(
            ids("SELECT case_no FROM cases WHERE guild_id = $1 ORDER BY 1").await,
            [3, 4]
        );
        // One summary job for the run, with the number of cases it removed.
        let jobs: Vec<Value> = sqlx::query_scalar(
            "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'old_cases_log'",
        )
        .bind(g)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(jobs, [json!({"count": 2, "days": 30})]);
        sweep(pool, Some(g)).await.unwrap();
        assert_eq!(
            ids("SELECT count(*)::int FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'old_cases_log'").await,
            [1],
            "a run that removes nothing logs nothing"
        );
        assert_eq!(
            ids("SELECT count(*)::int FROM reports WHERE guild_id = $1").await,
            [2],
            "the old dismissed report goes (365-day default), the open one stays"
        );
        assert_eq!(
            ids("SELECT number FROM tickets WHERE guild_id = $1 ORDER BY 1").await,
            [2, 3],
            "old closed ticket 1 goes; open ticket 2 and the newest ticket 3 stay"
        );
        assert_eq!(
            ids("SELECT count(*)::int FROM notes WHERE guild_id = $1").await,
            [1],
            "notes_days 0 keeps"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT user_id FROM profile_snapshot WHERE guild_id = $1 ORDER BY 1"
            )
            .bind(g)
            .fetch_all(pool)
            .await
            .unwrap(),
            [2, 3],
            "a stored name of a member not seen for 90 days goes"
        );
        sqlx::query("DELETE FROM profile_snapshot WHERE guild_id = $1")
            .bind(g)
            .execute(pool)
            .await
            .unwrap();
    }
}

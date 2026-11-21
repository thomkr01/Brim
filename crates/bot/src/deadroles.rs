//! Permit role rows whose Discord role is gone. A row stays for `DEAD_ROLE_DAYS` after the bot
//! first sees its role missing, because an anti-nuke restore makes the role again with a new id and
//! `relink` needs the old row to point it there. After that the row is dropped. Every reader also
//! skips missing roles at once (`tickets::live_roles`), so the days only cost a row.
use crate::app::App;
use anyhow::Result;

/// Days a permit role may stay missing before its row is dropped.
pub const DEAD_ROLE_DAYS: i32 = 7;

/// One pass over every server with permit roles; run hourly.
pub async fn sweep(app: &App) -> Result<()> {
    let guilds: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT guild_id FROM permit_roles")
        .fetch_all(&app.pool)
        .await?;
    for g in guilds {
        let Some(gid) = twilight_model::id::Id::new_checked(g as u64) else {
            continue;
        };
        // A server the bot cannot read (left, Discord down) is left alone.
        let Ok(info) = app.guild_info(gid).await else {
            continue;
        };
        let live: Vec<i64> = info.roles.keys().map(|r| r.get() as i64).collect();
        // One server's database error must not stop the pass for the others.
        if let Err(e) = mark(&app.pool, g, &live).await {
            tracing::warn!(?e, guild = g, "marking missing permit roles failed");
        }
    }
    Ok(())
}

/// Starts the clock on roles missing from `live`, stops it on roles back in it, and drops rows
/// missing for `DEAD_ROLE_DAYS`. An empty `live` (every server has @everyone) is a bad read: nothing happens.
async fn mark(pool: &sqlx::PgPool, guild: i64, live: &[i64]) -> Result<()> {
    if live.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE permit_roles SET missing_since = NULL
         WHERE guild_id = $1 AND role_id = ANY($2) AND missing_since IS NOT NULL",
    )
    .bind(guild)
    .bind(live)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE permit_roles SET missing_since = now()
         WHERE guild_id = $1 AND NOT role_id = ANY($2) AND missing_since IS NULL",
    )
    .bind(guild)
    .bind(live)
    .execute(pool)
    .await?;
    sqlx::query(
        "DELETE FROM permit_roles WHERE guild_id = $1
           AND missing_since < now() - make_interval(days => $2)",
    )
    .bind(guild)
    .bind(DEAD_ROLE_DAYS)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_role_missing_for_seven_days_loses_its_row_and_a_returned_one_keeps_it() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let g = 8_100_000_000 + std::process::id() as i64;
        let (pid,): (i64,) = sqlx::query_as(
            "INSERT INTO permits (guild_id, name, rank, perms) VALUES ($1,'t',3,'{tickets}') RETURNING id",
        )
        .bind(g)
        .fetch_one(&pool)
        .await
        .unwrap();
        // 1 live, 2 just went missing, 3 missing for 8 days, 4 missing before but back now.
        sqlx::query("INSERT INTO permit_roles (permit_id, guild_id, role_id, missing_since) VALUES ($1,$2,1,NULL),($1,$2,2,NULL),($1,$2,3,now() - interval '8 days'),($1,$2,4,now() - interval '8 days')")
            .bind(pid)
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
        mark(&pool, g, &[1, 4]).await.unwrap();
        let rows: Vec<(i64, bool)> = sqlx::query_as(
            "SELECT role_id, missing_since IS NOT NULL FROM permit_roles WHERE guild_id = $1 ORDER BY 1",
        )
        .bind(g)
        .fetch_all(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM permits WHERE guild_id = $1")
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(rows, [(1, false), (2, true), (4, false)]);
    }
}

//! Stats: counts of what Brim and staff do, per server and UTC day (migration 4100).
//! The counts (`stat_daily`, `stat_daily_all`) are kept for good and hold no member data
//! (owner decision 2026-11-25). Only the detail rows (`stat_events`) are pruned, which never
//! changes a count. Staff and Brim actions in `mod_actions` count through its trigger; every
//! other event calls [`record`].
use sqlx::{PgConnection, PgPool};

/// Days the detail rows are kept (owner decision 2026-11-25).
pub const EVENT_DAYS: i32 = 90;

/// Adds one event and its count in one statement. `kind` and `feature` are short lower-case
/// names (`raid_caught`, `joingate`); `n` is how many (accounts caught, messages purged).
pub async fn record(
    c: &mut PgConnection,
    guild: i64,
    kind: &str,
    feature: &str,
    n: i64,
    case_no: Option<i32>,
) -> sqlx::Result<()> {
    sqlx::query("SELECT stat_add($1, $2, $3, $4, $5)")
        .bind(guild)
        .bind(kind)
        .bind(feature)
        .bind(n)
        .bind(case_no)
        .execute(c)
        .await
        .map(drop)
}

/// Counts Brim joining a server once: a second GuildCreate for the same join (a reconnect within
/// minutes) adds nothing.
pub async fn server_joined(c: &mut PgConnection, guild: i64) -> sqlx::Result<()> {
    sqlx::query(
        "SELECT stat_add($1, 'server_join', 'bot', 1, NULL) WHERE NOT EXISTS (
           SELECT 1 FROM stat_events WHERE guild_id = $1 AND kind = 'server_join'
             AND at > now() - interval '10 minutes')",
    )
    .bind(guild)
    .execute(c)
    .await
    .map(drop)
}

/// Deletes detail rows older than [`EVENT_DAYS`], in batches. The counts stay as they are.
pub async fn prune(pool: &PgPool) -> sqlx::Result<()> {
    while sqlx::query(
        "DELETE FROM stat_events WHERE id IN (SELECT id FROM stat_events
           WHERE at < now() - make_interval(days => $1) LIMIT 10000)",
    )
    .bind(EVENT_DAYS)
    .execute(pool)
    .await?
    .rows_affected()
        > 0
    {}
    Ok(())
}

/// Hourly: adds every finished UTC day not yet added from `stat_daily` into `stat_daily_all`, and
/// sets today's `servers_total`. A day is finished one hour after it ends, so an event written in
/// a transaction that started before midnight is in it. Each day is added once (`stat_folded`).
pub async fn fold(pool: &PgPool) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    // One fold at a time: a second one waits and then finds nothing left to add.
    // fetch_one: a missing row is an error, never a fold that silently stops.
    sqlx::query("SELECT day FROM stat_folded FOR UPDATE")
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query(
        "WITH r AS (SELECT day AS last, ((now() - interval '1 hour') AT TIME ZONE 'UTC')::date - 1 AS upto
                    FROM stat_folded),
              add AS (INSERT INTO stat_daily_all (day, kind, feature, n, servers)
                      SELECT d.day, d.kind, d.feature, sum(d.n), count(*) FROM stat_daily d, r
                      WHERE d.day > r.last AND d.day <= r.upto GROUP BY d.day, d.kind, d.feature
                      ON CONFLICT (day, kind, feature) DO UPDATE
                        SET n = stat_daily_all.n + EXCLUDED.n,
                            servers = stat_daily_all.servers + EXCLUDED.servers)
         UPDATE stat_folded SET day = r.upto FROM r WHERE r.upto > r.last",
    )
    .execute(&mut *tx)
    .await?;
    // The servers Brim is in now: known owners, without the ones marked as left.
    sqlx::query(
        "INSERT INTO stat_daily_all (day, kind, feature, n, servers)
         SELECT (now() AT TIME ZONE 'UTC')::date, 'servers_total', 'bot', count(*), count(*)
         FROM guild_owners o WHERE NOT EXISTS (SELECT 1 FROM guild_left l WHERE l.guild_id = o.guild_id)
         ON CONFLICT (day, kind, feature) DO UPDATE SET n = EXCLUDED.n, servers = EXCLUDED.servers",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

#[cfg(test)]
#[path = "stats/tests.rs"]
mod tests;

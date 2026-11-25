use super::*;
use crate::cases::{self, ModAction};

async fn pool() -> Option<PgPool> {
    let url = crate::db::test_url().ok()?;
    Some(crate::db::connect(&url).await.unwrap())
}

async fn daily(pool: &PgPool, g: i64, kind: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT coalesce(sum(n), 0)::bigint FROM stat_daily WHERE guild_id = $1 AND kind = $2",
    )
    .bind(g)
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn events(pool: &PgPool, g: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM stat_events WHERE guild_id = $1")
        .bind(g)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The owner rule: pruning old detail rows never changes a count.
#[tokio::test]
async fn pruning_old_events_keeps_the_counts() {
    let Some(pool) = pool().await else { return };
    let g = 5_400_000_000 + std::process::id() as i64;
    crate::db::purge_guild(&pool, g).await.unwrap();
    let mut c = pool.acquire().await.unwrap();
    record(&mut c, g, "raid_caught", "raid", 40, None)
        .await
        .unwrap();
    record(&mut c, g, "raid_caught", "raid", 2, None)
        .await
        .unwrap();
    record(&mut c, g, "lockdown", "staff", 1, Some(7))
        .await
        .unwrap();
    // The first two are old now: past the 90 days.
    sqlx::query(
        "UPDATE stat_events SET at = now() - interval '91 days' WHERE guild_id = $1 AND kind = 'raid_caught'",
    )
    .bind(g)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(daily(&pool, g, "raid_caught").await, 42);
    prune(&pool).await.unwrap();
    assert_eq!(events(&pool, g).await, 1, "only the new event stays");
    assert_eq!(daily(&pool, g, "raid_caught").await, 42, "the count stays");
    assert_eq!(daily(&pool, g, "lockdown").await, 1);
    let all = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT coalesce(sum(n), 0)::bigint FROM stat_daily_all
             WHERE day = (now() AT TIME ZONE 'UTC')::date AND kind = 'raid_caught'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = all().await;
    crate::db::purge_guild(&pool, g).await.unwrap();
    assert_eq!(
        daily(&pool, g, "raid_caught").await,
        0,
        "the server purge deletes its rows"
    );
    assert_eq!(
        all().await,
        before + 42,
        "today's counts go to the owner totals first"
    );
}

/// Every staff and Brim action counts once, also a replayed audit entry; the dashboard's rows
/// (no feature) count under their source.
#[tokio::test]
async fn staff_actions_count_once_under_their_feature() {
    let Some(pool) = pool().await else { return };
    let g = 5_401_000_000 + std::process::id() as i64;
    crate::db::purge_guild(&pool, g).await.unwrap();
    let act = |feature, audit_id| ModAction {
        guild_id: g,
        mod_id: 8,
        kind: "ban",
        source: "discord",
        target_id: Some(7),
        case_no: None,
        audit_id,
        ai_client: None,
        feature,
    };
    let mut c = pool.acquire().await.unwrap();
    cases::record(&mut c, act(Some("discord_app"), Some(55)))
        .await
        .unwrap();
    cases::record(&mut c, act(Some("discord_app"), Some(55)))
        .await
        .unwrap();
    cases::record(&mut c, act(None, None)).await.unwrap();
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT feature, n FROM stat_daily WHERE guild_id = $1 AND kind = 'ban' ORDER BY feature",
    )
    .bind(g)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![("discord".into(), 1), ("discord_app".into(), 1)]);
    // A bad name is not counted, but the action itself is still recorded.
    let bad = ModAction {
        kind: "Bad-Kind",
        ..act(Some("staff"), None)
    };
    cases::record(&mut c, bad).await.unwrap();
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mod_actions WHERE guild_id = $1 AND kind = 'Bad-Kind'",
    )
    .bind(g)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((kept, events(&pool, g).await), (1, 2));
    // A case counts under the feature that made it.
    let case = cases::NewCase {
        guild_id: g,
        kind: "timeout",
        user_id: 7,
        mod_id: 9,
        reason: None,
        points: 0,
        duration_s: Some(60),
        proof: None,
        inverse: None,
        feature: "automod",
    };
    let no = cases::create(&pool, case).await.unwrap();
    let (f, case_no): (String, Option<i32>) = sqlx::query_as(
        "SELECT feature, case_no FROM stat_events WHERE guild_id = $1 AND kind = 'timeout'",
    )
    .bind(g)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((f.as_str(), case_no), ("automod", Some(no)));
    // Deleting the case (a retention limit) leaves the count.
    sqlx::query("DELETE FROM cases WHERE guild_id = $1")
        .bind(g)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(daily(&pool, g, "timeout").await, 1);
    crate::db::purge_guild(&pool, g).await.unwrap();
}

/// A bad name is refused by the table, so a typo can not start a new kind by accident.
#[tokio::test]
async fn names_are_checked() {
    let Some(pool) = pool().await else { return };
    let g = 5_402_000_000 + std::process::id() as i64;
    let mut c = pool.acquire().await.unwrap();
    assert!(record(&mut c, g, "Raid Caught", "raid", 1, None)
        .await
        .is_err());
    assert!(record(&mut c, g, "raid", "", 1, None).await.is_err());
    assert_eq!(events(&pool, g).await, 0);
}

/// A leave counts once per mark, a join once per GuildCreate burst.
#[tokio::test]
async fn server_joins_and_leaves_count_once() {
    let Some(pool) = pool().await else { return };
    let g = 5_403_000_000 + std::process::id() as i64;
    crate::db::purge_guild(&pool, g).await.unwrap();
    for _ in 0..2 {
        sqlx::query("INSERT INTO guild_left (guild_id) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(g)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(daily(&pool, g, "server_leave").await, 1);
    let mut c = pool.acquire().await.unwrap();
    server_joined(&mut c, g).await.unwrap();
    server_joined(&mut c, g).await.unwrap();
    assert_eq!(daily(&pool, g, "server_join").await, 1);
    crate::db::purge_guild(&pool, g).await.unwrap();
}

/// Load: many writes to one count at the same time (a raid of 1,000) and to many servers at the
/// same time all land; no count is lost to a race.
#[tokio::test]
async fn parallel_writes_lose_no_count() {
    let Some(pool) = pool().await else { return };
    // Far above the ids of other tests (5 to 9 billion plus the pid): a range of 2,001 servers
    // next to them would purge their rows while they run.
    let base = 20_000_000_000 + (std::process::id() as i64 % 100_000) * 10_000;
    let one = base;
    let servers = 2_000;
    for g in [one].into_iter().chain(base + 1..=base + servers) {
        crate::db::purge_guild(&pool, g).await.unwrap();
    }
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..1_000 {
        let pool = pool.clone();
        tasks.spawn(async move {
            let mut c = pool.acquire().await.unwrap();
            record(&mut c, one, "raid_caught", "raid", 1, None)
                .await
                .unwrap();
            let g = base + 1 + (i % servers);
            record(&mut c, g, "ban", "staff", 1, None).await.unwrap();
            record(
                &mut c,
                base + 1 + ((i + 1_000) % servers),
                "ban",
                "staff",
                1,
                None,
            )
            .await
            .unwrap();
        });
    }
    while let Some(r) = tasks.join_next().await {
        r.unwrap();
    }
    assert_eq!(daily(&pool, one, "raid_caught").await, 1_000);
    let (bans, with): (i64, i64) = sqlx::query_as(
        "SELECT sum(n)::bigint, count(DISTINCT guild_id) FROM stat_daily
         WHERE guild_id BETWEEN $1 AND $2 AND kind = 'ban'",
    )
    .bind(base + 1)
    .bind(base + servers)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((bans, with), (2_000, servers));
    for g in [one].into_iter().chain(base + 1..=base + servers) {
        crate::db::purge_guild(&pool, g).await.unwrap();
    }
}

/// The owner totals: each finished day is added once, the totals outlive the server's own rows
/// (a server that left stays in them), and today waits.
#[tokio::test]
async fn fold_adds_each_finished_day_once() {
    let Some(pool) = pool().await else { return };
    let g = 5_405_000_000 + std::process::id() as i64;
    crate::db::purge_guild(&pool, g).await.unwrap();
    // A day no other test writes: in the year 2000, one per process.
    let day: chrono::NaiveDate = sqlx::query_scalar("SELECT date '2000-01-01' + ($1 % 300)::int")
        .bind(std::process::id() as i64)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM stat_daily_all WHERE day = $1")
        .bind(day)
        .execute(&pool)
        .await
        .unwrap();
    for (guild, n) in [(g, 3_i64), (g + 1, 4)] {
        sqlx::query("INSERT INTO stat_daily (guild_id, day, kind, feature, n) VALUES ($1, $2, 'kick', 'staff', $3)")
            .bind(guild)
            .bind(day)
            .bind(n)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE stat_folded SET day = $1 - 1")
        .bind(day)
        .execute(&pool)
        .await
        .unwrap();
    let mut c = pool.acquire().await.unwrap();
    record(&mut c, g, "kick", "staff", 1, None).await.unwrap();
    fold(&pool).await.unwrap();
    fold(&pool).await.unwrap();
    let all = || async {
        sqlx::query_as::<_, (i64, i32)>(
            "SELECT n, servers FROM stat_daily_all WHERE day = $1 AND kind = 'kick' AND feature = 'staff'",
        )
        .bind(day)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(all().await, (7, 2), "added once, by two servers");
    let today: Option<i64> = sqlx::query_scalar(
        "SELECT n FROM stat_daily_all WHERE day = (now() AT TIME ZONE 'UTC')::date AND kind = 'servers_total'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(today.is_some(), "today's server count is set");
    // Servers ever: a known server is counted, and stays counted after its purge.
    sqlx::query(
        "INSERT INTO guild_owners (guild_id, owner_id) VALUES ($1, 1) ON CONFLICT DO NOTHING",
    )
    .bind(g)
    .execute(&pool)
    .await
    .unwrap();
    let ever = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT n FROM stat_daily_all WHERE day = (now() AT TIME ZONE 'UTC')::date AND kind = 'servers_ever'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    fold(&pool).await.unwrap();
    let counted = ever().await;
    let known: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stat_servers_ever WHERE key = sha256($1::text::bytea)",
    )
    .bind(g)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(known, 1);
    for guild in [g, g + 1] {
        crate::db::purge_guild(&pool, guild).await.unwrap();
    }
    assert_eq!(all().await, (7, 2), "the totals keep a purged server");
    fold(&pool).await.unwrap();
    assert!(ever().await >= counted, "servers ever never goes down");
    let still: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stat_servers_ever WHERE key = sha256($1::text::bytea)",
    )
    .bind(g)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(still, 1, "the purge keeps the server in servers ever");
    let active: Option<i64> = sqlx::query_scalar(
        "SELECT n FROM stat_daily_all WHERE day = (now() AT TIME ZONE 'UTC')::date AND kind = 'servers_active_30d'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(active.is_some());
}

/// The golden channel's caught counter counts in the stats too.
#[tokio::test]
async fn a_golden_channel_catch_counts() {
    let Some(pool) = pool().await else { return };
    let g = 5_406_000_000 + std::process::id() as i64;
    crate::db::purge_guild(&pool, g).await.unwrap();
    sqlx::query(
        "INSERT INTO catch_hits (guild_id, action) VALUES ($1, 'Softban'), ($1, 'Softban')",
    )
    .bind(g)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(daily(&pool, g, "catch").await, 2);
    crate::db::purge_guild(&pool, g).await.unwrap();
}

//! SQL-path tests against a real Postgres. Skipped unless DATABASE_URL is set.
use brim_core::{
    cases::{self, NewCase},
    db,
    permits::{self, Level},
};
use serde_json::{json, Value};
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let url = brim_core::db::test_url().ok()?;
    Some(db::connect(&url).await.expect("connect + migrate"))
}

/// Unique per test process and per test (via `offset`).
fn gid(offset: i64) -> i64 {
    7_000_000_000 + std::process::id() as i64 + offset
}

async fn cleanup(p: &PgPool, g: i64) {
    for t in [
        "cases",
        "action_log",
        "mod_actions",
        "extra_owners",
        "trusted_admins",
        "permit_roles",
        "permits",
        "quarantines",
        "unpermittable_roles",
        "permit_members",
        "permit_members_migrated",
    ] {
        let sql = format!("DELETE FROM {t} WHERE guild_id = $1");
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(g)
            .execute(p)
            .await
            .unwrap();
    }
}

fn case(
    g: i64,
    kind: &'static str,
    user: i64,
    points: i32,
    inverse: Option<Value>,
) -> NewCase<'static> {
    NewCase {
        guild_id: g,
        kind,
        user_id: user,
        mod_id: 1,
        reason: None,
        points,
        duration_s: None,
        proof: None,
        inverse,
    }
}

#[tokio::test]
async fn case_numbers_are_per_guild() {
    let Some(p) = pool().await else { return };
    let g = gid(0);
    cleanup(&p, g).await;
    assert_eq!(
        cases::create(&p, case(g, "ban", 2, 0, None)).await.unwrap(),
        1
    );
    assert_eq!(
        cases::create(&p, case(g, "kick", 2, 0, None))
            .await
            .unwrap(),
        2
    );
    cleanup(&p, g).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_case_numbers_are_distinct() {
    let Some(p) = pool().await else { return };
    let g = gid(1);
    cleanup(&p, g).await;
    let (a, b, c, d) = tokio::join!(
        cases::create(&p, case(g, "ban", 2, 0, None)),
        cases::create(&p, case(g, "ban", 3, 0, None)),
        cases::create(&p, case(g, "ban", 4, 0, None)),
        cases::create(&p, case(g, "ban", 5, 0, None)),
    );
    let mut nums = vec![a.unwrap(), b.unwrap(), c.unwrap(), d.unwrap()];
    nums.sort();
    assert_eq!(nums, vec![1, 2, 3, 4]);
    cleanup(&p, g).await;
}

#[tokio::test]
async fn create_writes_action_log_with_inverse() {
    let Some(p) = pool().await else { return };
    let g = gid(2);
    cleanup(&p, g).await;
    let inv = json!({"op": "unban", "user": 42});
    let n = cases::create(&p, case(g, "ban", 42, 0, Some(inv.clone())))
        .await
        .unwrap();
    let row: (String, Option<i64>, Option<Value>, Option<i32>, i64, bool) = sqlx::query_as(
        "SELECT action, target_id, inverse, case_no, actor_id, reverted FROM action_log WHERE guild_id = $1",
    )
    .bind(g)
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!(
        row,
        ("ban".to_owned(), Some(42), Some(inv), Some(n), 1, false)
    );
    cleanup(&p, g).await;
}

#[tokio::test]
async fn warn_points_sum_and_reset() {
    let Some(p) = pool().await else { return };
    let g = gid(3);
    cleanup(&p, g).await;
    cases::create(&p, case(g, "warn", 9, 2, None))
        .await
        .unwrap();
    cases::create(&p, case(g, "warn", 9, 3, None))
        .await
        .unwrap();
    cases::create(&p, case(g, "kick", 9, 9, None))
        .await
        .unwrap(); // not a warn
    cases::create(&p, case(g, "warn", 8, 7, None))
        .await
        .unwrap(); // other user
    assert_eq!(cases::warn_points(&p, g, 9).await.unwrap(), 5);

    cases::reset_warns(&p, g, 9, 1).await.unwrap();
    assert_eq!(cases::warn_points(&p, g, 9).await.unwrap(), 0);
    assert_eq!(cases::warn_points(&p, g, 8).await.unwrap(), 7);
    cleanup(&p, g).await;
}

#[tokio::test]
async fn expired_warns_not_counted() {
    let Some(p) = pool().await else { return };
    let g = gid(4);
    cleanup(&p, g).await;
    sqlx::query(
        "INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, points, expires_at)
         VALUES ($1, 100, 'warn', 9, 1, 4, now() - interval '1 day')",
    )
    .bind(g)
    .execute(&p)
    .await
    .unwrap();
    assert_eq!(cases::warn_points(&p, g, 9).await.unwrap(), 0);
    cases::create(&p, case(g, "warn", 9, 2, None))
        .await
        .unwrap();
    assert_eq!(cases::warn_points(&p, g, 9).await.unwrap(), 2);
    cleanup(&p, g).await;
}

async fn permit(p: &PgPool, g: i64, name: &str, rank: i32, perms: &[&str], role: i64) {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO permits (guild_id, name, rank, perms) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(g)
    .bind(name)
    .bind(rank)
    .bind(perms)
    .fetch_one(p)
    .await
    .unwrap();
    sqlx::query("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(g)
        .bind(role)
        .execute(p)
        .await
        .unwrap();
}

/// Lists `user` in the guild's permit `name` (a permit belongs to user ids).
async fn member(p: &PgPool, g: i64, name: &str, user: i64) {
    sqlx::query(
        "INSERT INTO permit_members (permit_id, guild_id, user_id)
         SELECT id, guild_id, $3 FROM permits WHERE guild_id = $1 AND name = $2",
    )
    .bind(g)
    .bind(name)
    .bind(user)
    .execute(p)
    .await
    .unwrap();
}

#[tokio::test]
async fn permits_resolve_levels() {
    let Some(p) = pool().await else { return };
    let g = gid(5);
    cleanup(&p, g).await;
    let owner = 11;
    sqlx::query("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, 12)")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, 13)")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    permit(&p, g, "mod", 5, &["ban", "kick"], 100).await;
    permit(&p, g, "low", 1, &["kick"], 201).await;
    permit(&p, g, "high", 4, &["ban"], 202).await;
    for (name, user) in [("mod", 12), ("mod", 14), ("high", 15)] {
        member(&p, g, name, user).await;
    }

    assert_eq!(
        permits::resolve(&p, g, owner, owner, false).await.unwrap(),
        Level::Owner
    );
    assert_eq!(
        permits::resolve(&p, g, owner, 12, false).await.unwrap(),
        Level::ExtraOwner
    );
    assert_eq!(
        permits::resolve(&p, g, owner, 13, false).await.unwrap(),
        Level::TrustedAdmin
    );
    // Q4: a bot listed as EO or TA gets neither, only the permit it is listed in.
    assert_eq!(
        permits::resolve(&p, g, owner, 13, true).await.unwrap(),
        Level::None
    );
    let Level::Custom { rank, .. } = permits::resolve(&p, g, owner, 12, true).await.unwrap() else {
        panic!("expected the bot permit");
    };
    assert_eq!(rank, 5);

    let Level::Custom { rank, perms, .. } =
        permits::resolve(&p, g, owner, 14, false).await.unwrap()
    else {
        panic!("expected Custom");
    };
    assert_eq!(
        (rank, perms),
        (5, vec!["ban".to_owned(), "kick".to_owned()])
    );

    // The permit a user is listed in counts (one permit per person), and the dashboard reads
    // the same one.
    let Level::Custom { rank, perms, .. } =
        permits::resolve(&p, g, owner, 15, false).await.unwrap()
    else {
        panic!("expected Custom");
    };
    assert_eq!((rank, perms), (4, vec!["ban".to_owned()]));
    let top = permits::top_permit(&p, g, 15).await.unwrap().unwrap();
    assert_eq!(top.1, vec!["ban".to_owned()]);

    // Owner decision 2026-11-20: a role alone gives nothing. The permit roles 100, 201, 202, 203 are
    // not looked at: a user listed in no permit has none, wherever their roles come from.
    assert_eq!(
        permits::resolve(&p, g, owner, 16, false).await.unwrap(),
        Level::None
    );
    // Not even before the guild's migration ran (a guild with no `permit_members_migrated` row).
    let migrated: bool = permits::members_migrated(&p, g).await.unwrap();
    assert!(!migrated, "the point of the check");
    cleanup(&p, g).await;
}

/// Q5: a permit on a role Discord hands out (linked, subscription, Onboarding) is reported once.
#[tokio::test]
async fn unpermittable_roles_carry_no_permit() {
    let Some(p) = pool().await else { return };
    let g = gid(60);
    cleanup(&p, g).await;
    permit(&p, g, "high", 4, &["ban"], 301).await;
    permit(&p, g, "low", 1, &["kick"], 302).await;
    permits::set_unpermittable(&p, g, permits::FROM_ONBOARDING, &[301, 303])
        .await
        .unwrap();
    permits::set_unpermittable(&p, g, permits::FROM_TAGS, &[301])
        .await
        .unwrap();
    assert_eq!(
        permits::unreported_unpermittable(&p, g).await.unwrap(),
        [301]
    );
    assert!(
        permits::unreported_unpermittable(&p, g)
            .await
            .unwrap()
            .is_empty(),
        "once"
    );
    permits::set_unpermittable(&p, g, permits::FROM_ONBOARDING, &[])
        .await
        .unwrap();
    permits::set_unpermittable(&p, g, permits::FROM_TAGS, &[])
        .await
        .unwrap();
    cleanup(&p, g).await;
}

#[tokio::test]
async fn unwhitelisted_staff_lists_admins_and_moderators_without_the_automod_flag() {
    let Some(p) = pool().await else { return };
    let g = gid(12);
    cleanup(&p, g).await;
    // Nobody listed in a permit is no finding; only the missing flag is.
    for (name, wl) in [
        ("Admins", vec!["all"]),
        ("Moderators", vec!["reports"]),
        ("Helpers", vec![]),
    ] {
        sqlx::query("INSERT INTO permits (guild_id, name, whitelist) VALUES ($1, $2, $3)")
            .bind(g)
            .bind(name)
            .bind(wl)
            .execute(&p)
            .await
            .unwrap();
    }
    assert_eq!(
        permits::unwhitelisted_staff(&p, g).await.unwrap(),
        ["Moderators"]
    );
    cleanup(&p, g).await;
}

#[tokio::test]
async fn mod_roles_leave_out_admin_roles() {
    let Some(p) = pool().await else { return };
    let g = gid(13);
    cleanup(&p, g).await;
    permit(&p, g, "Admins", 3, &[], 100).await;
    permit(&p, g, "Moderators", 2, &[], 100).await;
    permit(&p, g, "Helpers", 1, &[], 102).await;
    sqlx::query(
        "INSERT INTO permit_roles (permit_id, guild_id, role_id)
         SELECT id, guild_id, 101 FROM permits WHERE guild_id = $1 AND name = 'Moderators'",
    )
    .bind(g)
    .execute(&p)
    .await
    .unwrap();
    assert_eq!(permits::mod_roles(&p, g).await.unwrap(), [101]);
    cleanup(&p, g).await;
}

/// Stores a little in several guild tables, plus a ticket user whose last server is `g`.
async fn seed(p: &PgPool, g: i64, user: i64) {
    cases::create(p, case(g, "ban", user, 0, None))
        .await
        .unwrap(); // cases + action_log
    for sql in [
        "INSERT INTO guild_config (guild_id, version) VALUES ($1, 1)",
        "INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, 1)",
        "INSERT INTO welcomed_guilds (guild_id) VALUES ($1)",
        "INSERT INTO member_roles (guild_id, user_id, roles, joined_at) VALUES ($1, 1, '{2}', now())",
    ] {
        sqlx::query(sql).bind(g).execute(p).await.unwrap();
    }
    sqlx::query("INSERT INTO ticket_users (user_id, last_guild) VALUES ($1, $2) ON CONFLICT (user_id) DO UPDATE SET last_guild = $2")
        .bind(user).bind(g).execute(p).await.unwrap();
}

/// Rows left for `g` in the seeded tables, and whether the ticket user still points at it.
async fn left(p: &PgPool, g: i64, user: i64) -> (i64, bool) {
    let n: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM cases WHERE guild_id = $1) + (SELECT count(*) FROM action_log WHERE guild_id = $1)
              + (SELECT count(*) FROM guild_config WHERE guild_id = $1) + (SELECT count(*) FROM extra_owners WHERE guild_id = $1)
              + (SELECT count(*) FROM welcomed_guilds WHERE guild_id = $1) + (SELECT count(*) FROM guild_left WHERE guild_id = $1)
              + (SELECT count(*) FROM member_roles WHERE guild_id = $1)
              + (SELECT count(*) FROM mod_actions WHERE guild_id = $1)",
    )
    .bind(g).fetch_one(p).await.unwrap();
    let points: bool = sqlx::query_scalar(
        "SELECT exists(SELECT 1 FROM ticket_users WHERE user_id = $1 AND last_guild = $2)",
    )
    .bind(user)
    .bind(g)
    .fetch_one(p)
    .await
    .unwrap();
    (n, points)
}

#[tokio::test]
async fn cases_and_audit_entries_count_once_for_mod_stats() {
    let Some(p) = pool().await else { return };
    let g = gid(30);
    cleanup(&p, g).await;
    let no = cases::create(&p, case(g, "warn", 42, 1, None))
        .await
        .unwrap();
    for _ in 0..2 {
        // A gateway event replayed after a reconnect carries the same audit log id.
        cases::record(
            &mut p.acquire().await.unwrap(),
            cases::ModAction {
                guild_id: g,
                mod_id: 9,
                kind: "ban",
                source: "discord",
                target_id: Some(43),
                case_no: None,
                audit_id: Some(77),
                ai_client: None,
            },
        )
        .await
        .unwrap();
    }
    type Row = (i64, String, String, Option<i64>, Option<i32>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT mod_id, kind, source, target_id, case_no FROM mod_actions WHERE guild_id = $1 ORDER BY id",
    )
    .bind(g)
    .fetch_all(&p)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "warn".into(), "bot".into(), Some(42), Some(no)),
            (9, "ban".into(), "discord".into(), Some(43), None),
        ]
    );
    // Deleting the case leaves the history.
    sqlx::query("DELETE FROM cases WHERE guild_id = $1")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM mod_actions WHERE guild_id = $1")
        .bind(g)
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(n, 2);
    cleanup(&p, g).await;
}

#[tokio::test]
async fn purge_deletes_one_guild_only() {
    let Some(p) = pool().await else { return };
    let (a, b) = (gid(20), gid(21));
    for g in [a, b] {
        db::purge_guild(&p, g).await.unwrap();
        seed(&p, g, g).await;
    }
    db::purge_guild(&p, a).await.unwrap();
    assert_eq!(left(&p, a, a).await, (0, false));
    assert_eq!(left(&p, b, b).await, (7, true));
    db::purge_guild(&p, b).await.unwrap();
}

#[tokio::test]
async fn left_guilds_marks_only_guilds_gone_over_30_days_due() {
    let Some(p) = pool().await else { return };
    let (old, new) = (gid(22), gid(23));
    for (g, days) in [(old, 31), (new, 29)] {
        db::purge_guild(&p, g).await.unwrap();
        seed(&p, g, g).await;
        sqlx::query("INSERT INTO guild_left (guild_id, left_at) VALUES ($1, now() - make_interval(days => $2))")
            .bind(g).bind(days).execute(&p).await.unwrap();
    }
    let ids = db::left_guilds(&p, 30).await.unwrap();
    assert!(ids.contains(&(old, true)) && ids.contains(&(new, false)));
    db::purge_guild(&p, old).await.unwrap();
    assert_eq!(left(&p, old, old).await, (0, false));
    assert_eq!(left(&p, new, new).await, (8, true));
    db::purge_guild(&p, new).await.unwrap();
}

#[tokio::test]
async fn invites_one_credit_per_member_and_per_guild() {
    use brim_core::invites::{self, How, Join, Stats};
    let Some(p) = pool().await else { return };
    let (a, b) = (gid(70), gid(71));
    for g in [a, b] {
        db::purge_guild(&p, g).await.unwrap();
    }
    let j = |g, user, ms, inviter: Option<i64>, how| Join {
        guild_id: g,
        user_id: user,
        joined_at_ms: ms,
        code: Some("abc"),
        inviter_id: inviter,
        how,
        reason: None,
    };
    // Ten parallel copies of one join give one journal row and one credit.
    let futs = (0..10).map(|_| {
        let p = p.clone();
        async move { invites::record_joins(&p, &[j(a, 10, 1_000, Some(1), How::Single)]).await }
    });
    for r in futures_join(futs).await {
        r.unwrap();
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM invite_joins WHERE guild_id = $1")
        .bind(a)
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Leave, then rejoin through another inviter: still one credit, to the first inviter.
    invites::record_leave(&p, a, 10).await.unwrap();
    assert_eq!(
        invites::stats(&p, a, 1).await.unwrap(),
        Stats {
            total: 1,
            present: 0,
            left: 1
        }
    );
    invites::record_joins(&p, &[j(a, 10, 2_000, Some(2), How::Single)])
        .await
        .unwrap();
    invites::record_joins(&p, &[j(a, 10, 2_000, Some(2), How::Single)])
        .await
        .unwrap();
    let m = invites::who(&p, a, 10).await.unwrap().unwrap();
    assert_eq!(
        (m.inviter_id, m.joins, m.present, m.first_joined_ms),
        (Some(1), 2, true, 1_000)
    );
    assert_eq!(invites::stats(&p, a, 2).await.unwrap().total, 0);
    // A first join that is unknown is never credited by a later known one.
    invites::record_joins(&p, &[j(a, 11, 1_000, None, How::Unknown)])
        .await
        .unwrap();
    invites::record_joins(&p, &[j(a, 11, 3_000, Some(1), How::Single)])
        .await
        .unwrap();
    assert!(!invites::who(&p, a, 11).await.unwrap().unwrap().creditable);
    // Self-invites and bots give no credit; group matches do.
    invites::record_joins(
        &p,
        &[
            j(a, 1, 1_000, Some(1), How::Single),
            j(a, 12, 1_000, Some(3), How::Bot),
            j(a, 13, 1_000, Some(3), How::Group),
        ],
    )
    .await
    .unwrap();
    // The same user in another guild is separate.
    invites::record_joins(&p, &[j(b, 10, 1_000, Some(3), How::Single)])
        .await
        .unwrap();
    let top = invites::top(&p, a, 10, 0).await.unwrap();
    assert_eq!(
        top.iter().map(|t| (t.0, t.1.total)).collect::<Vec<_>>(),
        vec![(1, 1), (3, 1)]
    );
    assert_eq!(invites::inviters(&p, a).await.unwrap(), 2);
    assert_eq!(invites::stats(&p, b, 3).await.unwrap().total, 1);
    assert_eq!(invites::summary(&p, a).await.unwrap(), (Some(1_000), 1));
    // Links Brim made: owner kept, gone ones dropped from the live list.
    invites::save_link(&p, a, "x1", 5, 9, 0, None)
        .await
        .unwrap();
    invites::save_link(&p, a, "x2", 5, 9, 0, Some(1_000))
        .await
        .unwrap();
    invites::save_link(&p, a, "x3", 5, 9, 0, None)
        .await
        .unwrap();
    let owners = invites::link_owners(&p, a, &["x1".into(), "zz".into()])
        .await
        .unwrap();
    assert_eq!(owners.get("x1"), Some(&5));
    assert!(invites::link_owners(&p, b, &["x1".into()])
        .await
        .unwrap()
        .is_empty());
    invites::links_gone(&p, a, Some("x3"), &[], 0)
        .await
        .unwrap();
    assert_eq!(
        invites::live_links(&p, a, 5).await.unwrap(),
        vec![("x1".to_string(), None)]
    );
    // A list asked for before a link was made does not mark it gone.
    invites::links_gone(&p, a, None, &["zz".into()], 0)
        .await
        .unwrap();
    assert_eq!(invites::live_links(&p, a, 5).await.unwrap().len(), 1);
    assert_eq!(invites::recent_links(&p, a).await.unwrap(), 3);
    let later = 4_102_444_800_000; // 2100
    invites::links_gone(&p, a, None, &["zz".into()], later)
        .await
        .unwrap();
    assert!(invites::live_links(&p, a, 5).await.unwrap().is_empty());
    assert!(invites::last_link_ms(&p, a, 5).await.unwrap().is_some());
    // Guild purge takes all three tables, for this guild only.
    db::purge_guild(&p, a).await.unwrap();
    for t in ["invite_joins", "invite_members", "invite_links"] {
        let sql = format!("SELECT count(*) FROM {t} WHERE guild_id = $1");
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(a)
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(n, 0, "{t}");
    }
    assert_eq!(invites::stats(&p, b, 3).await.unwrap().total, 1);
    db::purge_guild(&p, b).await.unwrap();
}

/// Runs the futures at once on this runtime.
async fn futures_join<F: std::future::Future + Send + 'static>(
    f: impl Iterator<Item = F>,
) -> Vec<F::Output>
where
    F::Output: Send + 'static,
{
    let hs: Vec<_> = f.map(tokio::spawn).collect();
    let mut out = vec![];
    for h in hs {
        out.push(h.await.unwrap());
    }
    out
}

/// Owner decision 2026-11-15: once a guild is migrated, a permit belongs to user ids and a role carries none.
#[tokio::test]
async fn permits_follow_user_ids_after_migration() {
    let Some(p) = pool().await else { return };
    let g = gid(61);
    cleanup(&p, g).await;
    permit(&p, g, "high", 4, &["ban"], 401).await;
    permit(&p, g, "low", 1, &["kick"], 402).await;
    permits::set_unpermittable(&p, g, permits::FROM_TAGS, &[402])
        .await
        .unwrap();
    assert!(!permits::members_migrated(&p, g).await.unwrap());
    // Before: the role path. 21 holds both roles, 22 only the unpermittable one, 23 none.
    let holders = vec![(21, vec![401, 402]), (22, vec![402]), (23, vec![])];
    let mut c = p.acquire().await.unwrap();
    let moved = permits::migrate_members(&mut c, g, &holders).await.unwrap();
    assert_eq!(moved, vec![(21, "high".to_owned())]);
    assert!(permits::members_migrated(&p, g).await.unwrap());
    assert!(
        permits::migrate_members(&mut c, g, &holders)
            .await
            .unwrap()
            .is_empty(),
        "once"
    );
    // After: the role alone gives nothing, the id does.
    let rank = |l: Level| match l {
        Level::Custom { rank, .. } => rank,
        _ => -1,
    };
    assert_eq!(
        rank(permits::resolve(&p, g, 1, 21, false).await.unwrap()),
        4
    );
    assert_eq!(
        permits::resolve(&p, g, 1, 24, false).await.unwrap(),
        Level::None
    );
    assert_eq!(permits::member_roles(&p, g, 21, 0).await.unwrap(), [401]);
    let all = permits::members(&p, g).await.unwrap();
    assert_eq!(all, vec![(21, "high".to_owned(), vec![401])]);
    // A second permit for the same person is refused (one permit per person).
    let low: i64 =
        sqlx::query_scalar("SELECT id FROM permits WHERE guild_id = $1 AND name = 'low'")
            .bind(g)
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(sqlx::query(
        "INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, 21)"
    )
    .bind(low)
    .bind(g)
    .execute(&p)
    .await
    .is_err());
    // Leaving takes the permit, EO and TA.
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, 21)")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    let held = permits::forget_member(&mut c, g, 21).await.unwrap();
    assert_eq!(held, ["Trusted Admin", "permit high"]);
    assert_eq!(
        permits::resolve(&p, g, 1, 21, false).await.unwrap(),
        Level::None
    );
    drop(c);
    cleanup(&p, g).await;
}

/// A quarantined user holds no permit on either path (the quarantine strips the roles that once
/// carried it), and the migration reports only the people it listed.
#[tokio::test]
async fn quarantine_drops_the_permit_and_migration_reports_inserts_only() {
    let Some(p) = pool().await else { return };
    let (g, old) = (gid(62), gid(63));
    cleanup(&p, g).await;
    cleanup(&p, old).await;
    permit(&p, g, "high", 4, &["ban"], 501).await;
    permit(&p, old, "high", 4, &["ban"], 502).await;
    // 31 is listed already: the migration skips it and must not report it.
    sqlx::query(
        "INSERT INTO permit_members (permit_id, guild_id, user_id)
         SELECT id, guild_id, 31 FROM permits WHERE guild_id = $1",
    )
    .bind(g)
    .execute(&p)
    .await
    .unwrap();
    let mut c = p.acquire().await.unwrap();
    let holders = vec![(31, vec![501]), (32, vec![501])];
    let moved = permits::migrate_members(&mut c, g, &holders).await.unwrap();
    assert_eq!(moved, vec![(32, "high".to_owned())]);
    drop(c);
    // Listed: a permit; quarantined: none. The old guild never ran the migration: nobody is
    // listed there, so a holder of its permit role (502) has no permit at all.
    assert!(permits::top_permit(&p, g, 32).await.unwrap().is_some());
    sqlx::query("INSERT INTO quarantines (guild_id, user_id, saved_roles) VALUES ($1, 32, $2)")
        .bind(g)
        .bind(vec![501_i64])
        .execute(&p)
        .await
        .unwrap();
    assert_eq!(permits::top_permit(&p, g, 32).await.unwrap(), None);
    assert_eq!(permits::top_permit(&p, old, 33).await.unwrap(), None);
    // Owner, EO and TA are not permits: a quarantine leaves them.
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, 32)")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    assert_eq!(
        permits::resolve(&p, g, 1, 32, false).await.unwrap(),
        Level::TrustedAdmin
    );
    // Member updates: only an unlisted, unquarantined holder of a permit role loses it.
    let un = |u: i64| permits::unentitled_roles(&p, g, u, 0, &[501, 999]);
    assert_eq!(un(31).await.unwrap(), Vec::<i64>::new(), "listed");
    assert_eq!(un(40).await.unwrap(), [501], "not listed");
    assert_eq!(un(32).await.unwrap(), Vec::<i64>::new(), "quarantined");
    assert_eq!(
        permits::unentitled_roles(&p, old, 40, 0, &[502])
            .await
            .unwrap(),
        Vec::<i64>::new(),
        "not migrated"
    );
    cleanup(&p, g).await;
    cleanup(&p, old).await;
}

/// security review (migration 4035): a grant gone at commit and a server purge leave a log line,
/// a delete-and-insert in one transaction does not, and `restore_erasures` applies the lines newer
/// than a restored dump again.
#[tokio::test]
async fn restore_applies_later_deletions_again() {
    let Some(p) = pool().await else { return };
    let (g, g2) = (gid(400), gid(401));
    let q = |sql: &'static str| sqlx::query(sql);
    let (mark,): (i64,) = sqlx::query_as("SELECT coalesce(max(id), 0) FROM erasure_log")
        .fetch_one(&p)
        .await
        .unwrap();
    q("INSERT INTO extra_owners VALUES ($1, 1), ($1, 2), ($2, 9)")
        .bind(g)
        .bind(g2)
        .execute(&p)
        .await
        .unwrap();
    q("INSERT INTO trusted_admins VALUES ($1, 3)")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    // A permits save rewrites the list: same rows back, nothing taken.
    let mut tx = p.begin().await.unwrap();
    q("DELETE FROM extra_owners WHERE guild_id = $1")
        .bind(g)
        .execute(&mut *tx)
        .await
        .unwrap();
    q("INSERT INTO extra_owners VALUES ($1, 1), ($1, 2)")
        .bind(g)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    q("DELETE FROM extra_owners WHERE guild_id = $1 AND user_id = 2")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    q("DELETE FROM trusted_admins WHERE guild_id = $1")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    db::purge_guild(&p, g2).await.unwrap();
    let logged: Vec<(String, Value)> = sqlx::query_as(
        "SELECT tbl, key FROM erasure_log WHERE id > $1 AND (key->>'guild_id')::bigint IN ($2, $3) ORDER BY id",
    )
    .bind(mark)
    .bind(g)
    .bind(g2)
    .fetch_all(&p)
    .await
    .unwrap();
    assert_eq!(
        logged,
        vec![
            ("extra_owners".into(), json!({"guild_id": g, "user_id": 2})),
            (
                "trusted_admins".into(),
                json!({"guild_id": g, "user_id": 3})
            ),
            ("purge_guild".into(), json!({"guild_id": g2})),
        ]
    );

    // A restore of a dump taken before those deletions, rolled back at the end. Other tests share the
    // database: the lock holds their log lines back, and only this test's lines are newer than the "dump".
    let mut tx = p.begin().await.unwrap();
    for sql in [
        "LOCK TABLE erasure_log IN EXCLUSIVE MODE",
        "CREATE TEMP TABLE saved ON COMMIT DROP AS SELECT * FROM erasure_log
         WHERE id > $1 AND (key->>'guild_id')::bigint IN ($2, $3)",
        "DELETE FROM erasure_log WHERE id IN (SELECT id FROM saved)",
        "UPDATE saved SET id = id + (SELECT max(id) FROM erasure_log)",
    ] {
        sqlx::query(sql)
            .bind(mark)
            .bind(g)
            .bind(g2)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    q("INSERT INTO extra_owners VALUES ($1, 2), ($2, 9)")
        .bind(g)
        .bind(g2)
        .execute(&mut *tx)
        .await
        .unwrap();
    q("INSERT INTO trusted_admins VALUES ($1, 3)")
        .bind(g)
        .execute(&mut *tx)
        .await
        .unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT restore_erasures()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 3);
    let left: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT guild_id, user_id FROM extra_owners WHERE guild_id IN ($1, $2)
         UNION ALL SELECT guild_id, user_id FROM trusted_admins WHERE guild_id = $1",
    )
    .bind(g)
    .bind(g2)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(left, vec![(g, 1)], "only the grant never removed stays");
    let (back,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM erasure_log WHERE id IN (SELECT id FROM saved)")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(back, 3, "the log lines are kept for the next restore");
    tx.rollback().await.unwrap();
    cleanup(&p, g).await;
}

/// A data deletion the bot carried out (migration 4034) is logged, and a restore queues it again,
/// approved by the same bot owner.
#[tokio::test]
async fn restore_queues_a_done_data_deletion_again() {
    let Some(p) = pool().await else { return };
    let user = gid(402);
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO data_requests (user_id, username, kind, status, decided_by)
         VALUES ($1, 'x', 'delete', 'approved', 42) RETURNING id",
    )
    .bind(user)
    .fetch_one(&p)
    .await
    .unwrap();
    sqlx::query("UPDATE data_requests SET status = 'done', done_at = now() WHERE id = $1")
        .bind(id)
        .execute(&p)
        .await
        .unwrap();
    let mut tx = p.begin().await.unwrap();
    for sql in [
        "LOCK TABLE erasure_log IN EXCLUSIVE MODE",
        "CREATE TEMP TABLE saved ON COMMIT DROP AS SELECT * FROM erasure_log
         WHERE tbl = 'data_request' AND (key->>'user_id')::bigint = $1",
        "DELETE FROM erasure_log WHERE id IN (SELECT id FROM saved)",
        "UPDATE saved SET id = id + (SELECT max(id) FROM erasure_log)",
        // The dump holds the request from before it was done.
        "DELETE FROM data_requests WHERE user_id = $1",
    ] {
        sqlx::query(sql).bind(user).execute(&mut *tx).await.unwrap();
    }
    let (n,): (i64,) = sqlx::query_as("SELECT restore_erasures()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let (rid, status, by): (i64, String, i64) =
        sqlx::query_as("SELECT id, status, decided_by FROM data_requests WHERE user_id = $1")
            .bind(user)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!((status.as_str(), by), ("approved", 42));
    let (job,): (Value,) = sqlx::query_as(
        "SELECT payload FROM scheduled_jobs WHERE guild_id = 0 AND kind = 'data_request' AND payload->>'id' = $1::text",
    )
    .bind(rid)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(job, json!({"id": rid, "by": "42"}));
    tx.rollback().await.unwrap();
    sqlx::query("DELETE FROM data_requests WHERE user_id = $1")
        .bind(user)
        .execute(&p)
        .await
        .unwrap();
}

/// Design plan P10: proofs and the edit history of a case.
#[tokio::test]
async fn case_proofs_and_edit_history() {
    let Some(p) = pool().await else { return };
    let g = gid(40);
    cleanup(&p, g).await;
    let by = (5, None);
    let mut c = case(g, "ban", 9, 0, None);
    c.proof = Some("https://example.com/a.png");
    let no = cases::create(&p, c).await.unwrap();
    let mut conn = p.acquire().await.unwrap();

    // Only a real change writes history: one row per changed field.
    let was = cases::edit(&mut conn, g, no, Some(Some("spam")), None, by)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((was.user_id, was.reason, was.active), (9, None, true));
    cases::edit(&mut conn, g, no, Some(Some("spam")), Some(true), by)
        .await
        .unwrap();
    cases::edit(&mut conn, g, no, Some(None), Some(false), (6, Some("tok")))
        .await
        .unwrap();
    assert_eq!(
        cases::edit(&mut conn, g, 99, None, Some(false), by)
            .await
            .unwrap(),
        None
    );
    type Edit = (String, Option<String>, Option<String>, i64, Option<String>);
    let edits: Vec<Edit> = sqlx::query_as(
        "SELECT field, old_value, new_value, editor_id, ai_client FROM case_edits WHERE guild_id = $1 ORDER BY id, field",
    )
    .bind(g)
    .fetch_all(&p)
    .await
    .unwrap();
    let t = |s: &str| Some(s.to_string());
    assert_eq!(
        edits,
        [
            ("reason".into(), None, t("spam"), 5, None),
            ("reason".into(), t("spam"), None, 6, t("tok")),
            ("active".into(), t("open"), t("closed"), 6, t("tok")),
        ]
    );

    // A verified proof keeps where and when the message was sent (the time is in its id).
    let copy = cases::MessageCopy {
        channel_id: 77,
        message_id: Some(1_300_000_000_000_000_000),
        text: "buy cheap nitro",
        files: vec!["a.png".into()],
    };
    let add = |kind: &'static str, body: &'static str, copy| {
        cases::add_proof(&p, g, no, (kind, body), by, copy)
    };
    assert_eq!(
        add("text", copy.text, Some(&copy)).await.unwrap(),
        cases::Added::Done
    );
    assert_eq!(
        add("text", "said it twice", None).await.unwrap(),
        cases::Added::Done
    );
    assert_eq!(
        cases::add_proof(&p, g, 99, ("text", "x"), by, None)
            .await
            .unwrap(),
        cases::Added::NoCase
    );
    type Proof = (i64, String, String, bool, Option<i32>, Vec<String>);
    let rows: Vec<Proof> = sqlx::query_as(
        "SELECT id, kind, body, verified, extract(year from sent_at)::int, files FROM case_proofs WHERE guild_id = $1 ORDER BY id",
    )
    .bind(g)
    .fetch_all(&p)
    .await
    .unwrap();
    let shape: Vec<_> = rows.iter().map(|r| (r.1.as_str(), r.3, r.4)).collect();
    assert_eq!(
        shape,
        [
            ("link", false, None),
            ("text", true, Some(2024)),
            ("text", false, None)
        ]
    );
    assert_eq!(rows[1].5, ["a.png"]);

    // The read-only MCP role sees what staff added, never the copy of the member's message.
    let seen: String =
        sqlx::query_scalar("SELECT proof FROM mcp_cases WHERE guild_id = $1 AND case_no = $2")
            .bind(g)
            .bind(no)
            .fetch_one(&p)
            .await
            .unwrap();
    assert_eq!(seen, "https://example.com/a.png\nsaid it twice");

    // A removed proof leaves only its kind in the history, and no copy in the old column.
    // (An older image writes the column: the trigger copies it, so the link is there twice.)
    sqlx::query("UPDATE cases SET proof = 'https://example.com/a.png' WHERE guild_id = $1")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    let old_col = || {
        sqlx::query_scalar::<_, Option<String>>("SELECT proof FROM cases WHERE guild_id = $1")
            .bind(g)
            .fetch_one(&p)
    };
    let twin: i64 = sqlx::query_scalar("SELECT max(id) FROM case_proofs WHERE guild_id = $1")
        .bind(g)
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(
        cases::remove_proof(&mut conn, g, no, rows[0].0, by)
            .await
            .unwrap(),
        Some(("link".into(), false))
    );
    // The second proof with the same link stays, and the column with it.
    assert!(old_col().await.unwrap().is_some());
    cases::remove_proof(&mut conn, g, no, twin, by)
        .await
        .unwrap();
    assert_eq!(old_col().await.unwrap(), None);
    let gone = cases::remove_proof(&mut conn, g, no, rows[1].0, by)
        .await
        .unwrap();
    assert_eq!(gone, Some(("text".into(), true)));
    assert_eq!(
        cases::remove_proof(&mut conn, g, no, rows[1].0, by)
            .await
            .unwrap(),
        None
    );
    let (old, new): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT old_value, new_value FROM case_edits WHERE guild_id = $1 AND field = 'proof' ORDER BY id DESC LIMIT 1",
    )
    .bind(g)
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!((old.as_deref(), new), (Some("verified message"), None));

    // An image from before the migration still writes cases.proof: the proof is not lost, and
    // one it replaces or clears does not come back.
    for proof in [Some("replaced"), Some("cleared"), None, Some("old image")] {
        sqlx::query("UPDATE cases SET proof = $2 WHERE guild_id = $1")
            .bind(g)
            .bind(proof)
            .execute(&p)
            .await
            .unwrap();
    }
    for _ in 0..30 {
        add("text", "more", None).await.unwrap();
    }
    assert_eq!(
        add("text", "one too many", None).await.unwrap(),
        cases::Added::Full
    );
    let (n, old): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE body = 'old image') FROM case_proofs WHERE guild_id = $1",
    )
    .bind(g)
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!((n, old), (cases::PROOFS_MAX, 1));

    // A person's data deletion keeps the case without the person: proofs and history go, also
    // when an image from before the migration runs it.
    sqlx::query("UPDATE cases SET user_id = 0, reason = NULL, proof = NULL WHERE guild_id = $1")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    let left = || {
        sqlx::query_scalar::<_, i64>(
            "SELECT (SELECT count(*) FROM case_proofs WHERE guild_id = $1) + (SELECT count(*) FROM case_edits WHERE guild_id = $1)",
        )
        .bind(g)
        .fetch_one(&p)
    };
    assert_eq!(left().await.unwrap(), 0);
    add("text", "after", None).await.unwrap();

    // Both go with their case.
    sqlx::query("DELETE FROM cases WHERE guild_id = $1")
        .bind(g)
        .execute(&p)
        .await
        .unwrap();
    assert_eq!(left().await.unwrap(), 0);
    assert_eq!(cases::proof_kind(" https://a.b/c "), "link");
    assert_eq!(cases::proof_kind("see https://a.b/c"), "text");
    cleanup(&p, g).await;
}

/// `holders` lists exactly the users `resolve` gives a level, in one query.
#[tokio::test]
async fn holders_match_resolve() {
    let Some(p) = pool().await else { return };
    let g = gid(403);
    cleanup(&p, g).await;
    permit(&p, g, "Moderators", 2, &[], 100).await;
    member(&p, g, "Moderators", 11).await;
    member(&p, g, "Moderators", 12).await;
    for sql in [
        "INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, 13)",
        "INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, 14)",
        // A quarantine takes the permit away, as in `top_permit`.
        "INSERT INTO quarantines (guild_id, user_id, saved_roles) VALUES ($1, 12, '{}')",
    ] {
        sqlx::query(sql).bind(g).execute(&p).await.unwrap();
    }
    let all = permits::holders(&p, g, 10).await.unwrap();
    for u in 10..=15 {
        let level = permits::resolve(&p, g, 10, u, false).await.unwrap();
        assert_eq!(all.contains(&u), level != permits::Level::None, "user {u}");
    }
    cleanup(&p, g).await;
}

/// Migration 4066 rewrites old variable names in saved texts to today's names, whole tokens only,
/// and a second run changes nothing.
#[tokio::test]
async fn variable_names_migration_rewrites_saved_texts() {
    let Some(p) = pool().await else { return };
    let g = gid(80);
    for t in [
        "bot_messages",
        "bot_messages_undo",
        "embeds",
        "guild_config",
        "feeds",
    ] {
        let sql = format!("DELETE FROM {t} WHERE guild_id = $1");
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(g)
            .execute(&p)
            .await
            .unwrap();
    }
    let old_ban = json!({"content": "", "embeds": [{"description": "You were banned from {server} by {moderator}: {reason}. {reasons} {e:ban} {server.name}"}]});
    sqlx::query("INSERT INTO bot_messages (guild_id, key, message, updated_by) VALUES ($1, 'dm.punish.ban', $2, 1), ($1, 'giveaway.running', $3, 1), ($1, 'dm.verify_reminder', '{\"content\": \"{server} {time}\", \"embeds\": []}', 1)")
        .bind(g)
        .bind(&old_ban)
        .bind(json!({"embeds": [{"title": "{prize}", "description": "{entries} · {host}", "image": {"url": "{banner}"}}]}))
        .execute(&p)
        .await
        .unwrap();
    sqlx::query("INSERT INTO bot_messages_undo (guild_id, key, message) VALUES ($1, 'reply.ticket.shown_type_set', $2)")
        .bind(g)
        .bind(json!({"content": "{user} set **{type}**", "embeds": []}))
        .execute(&p)
        .await
        .unwrap();
    sqlx::query("INSERT INTO embeds (guild_id, name, message, published, updated_by) VALUES ($1, 'Rules', $2, $2, 1)")
        .bind(g)
        .bind(json!({"content": "Welcome to {server.name} ({server.member_count}) {server.names} on {date}", "embeds": []}))
        .execute(&p)
        .await
        .unwrap();
    let tickets = json!({"tickets": {"greeting": "Hi {usermention} ({username}, {userid}), {ticket_type}", "snippets": [{"name": "r", "text": "Bye {username}"}]}, "other": "{username}"});
    sqlx::query(
        "INSERT INTO guild_config (guild_id, version, config) VALUES ($1, 1, $2), ($1, 2, $2)",
    )
    .bind(g)
    .bind(&tickets)
    .execute(&p)
    .await
    .unwrap();
    sqlx::query("INSERT INTO feeds (guild_id, channel_id, kind, target, created_by, message) VALUES ($1, 1, 'rss', 'https://a.b', 1, $2), ($1, 1, 'github', 'x', 1, $3)")
        .bind(g)
        .bind(json!({"content": "New: {link} {title}", "embeds": []}))
        .bind(json!({"push": {"embeds": [{"title": "{headline}", "url": "{link}"}]}}))
        .execute(&p)
        .await
        .unwrap();

    sqlx::query("INSERT INTO backups (guild_id, image, messages) VALUES ($1, '{}', $2)")
        .bind(g)
        .bind(json!({"embeds": [{"message": {"content": "{time} {server.icon}"}}], "bot_messages": [{"key": "dm.verify_reminder", "message": {"content": "{time}"}}]}))
        .execute(&p)
        .await
        .unwrap();
    let sql = include_str!("../migrations/4066_variable_names.sql");
    for _ in 0..2 {
        let mut tx = p.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let msg = |key: &'static str, table: &'static str| {
            let p = p.clone();
            async move {
                let sql = format!("SELECT message FROM {table} WHERE guild_id = $1 AND key = $2");
                sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(g)
                    .bind(key)
                    .fetch_one(&p)
                    .await
                    .unwrap()
            }
        };
        let ban = msg("dm.punish.ban", "bot_messages").await;
        assert_eq!(
            ban["embeds"][0]["description"],
            "You were banned from {guild.name} by {mod_moderator}: {mod_reason}. {reasons} {e:ban} {guild.name}"
        );
        let ga = msg("giveaway.running", "bot_messages").await;
        assert_eq!(ga["embeds"][0]["title"], "{ga_prize}");
        assert_eq!(ga["embeds"][0]["description"], "{ga_entries} · {ga_host}");
        assert_eq!(ga["embeds"][0]["image"]["url"], "{ga_banner}");
        let remind = msg("dm.verify_reminder", "bot_messages").await;
        assert_eq!(
            remind["content"], "{guild.name} {ver_time}",
            "a bot message is not a Message Builder text"
        );
        let undo = msg("reply.ticket.shown_type_set", "bot_messages_undo").await;
        assert_eq!(undo["content"], "{user} set **{tk_type}**");
        // The migrated copy is what the bot sends: today's names fill.
        let sent = brim_logging::botmsg::render(
            "dm.punish.ban",
            Some(&ban),
            &[("guild.name", "S"), ("moderator", "M"), ("reason", "R")],
        );
        assert!(
            sent["embeds"][0]["description"]
                .as_str()
                .unwrap()
                .contains("banned from S by M: R."),
            "{sent}"
        );

        let (m, pubd): (Value, Value) =
            sqlx::query_as("SELECT message, published FROM embeds WHERE guild_id = $1")
                .bind(g)
                .fetch_one(&p)
                .await
                .unwrap();
        for v in [m, pubd] {
            assert_eq!(
                v["content"],
                "Welcome to {guild.name} ({guild.member_count}) {server.names} on {msg_date}"
            );
        }
        let cfgs: Vec<Value> = sqlx::query_scalar(
            "SELECT config FROM guild_config WHERE guild_id = $1 ORDER BY version",
        )
        .bind(g)
        .fetch_all(&p)
        .await
        .unwrap();
        for c in cfgs {
            assert_eq!(
                c["tickets"]["greeting"],
                "Hi {member} ({member.username}, {member.id}), {tk_type}"
            );
            assert_eq!(c["tickets"]["snippets"][0]["text"], "Bye {member.username}");
            assert_eq!(c["other"], "{username}", "only the ticket texts");
        }
        let backup: Value = sqlx::query_scalar("SELECT messages FROM backups WHERE guild_id = $1")
            .bind(g)
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(
            backup["embeds"][0]["message"]["content"],
            "{msg_time} {guild.icon}"
        );
        // Bot messages in a backup are renamed when it is loaded (`to_public`).
        assert_eq!(backup["bot_messages"][0]["message"]["content"], "{time}");
        let feeds: Vec<(String, Value)> =
            sqlx::query_as("SELECT kind, message FROM feeds WHERE guild_id = $1 ORDER BY kind")
                .bind(g)
                .fetch_all(&p)
                .await
                .unwrap();
        assert_eq!(feeds[0].1["push"]["embeds"][0]["url"], "{feed_link}");
        assert_eq!(feeds[0].1["push"]["embeds"][0]["title"], "{feed_headline}");
        assert_eq!(feeds[1].1["content"], "New: {feed_link} {title}");
    }
}

/// Migration 4071 renames the ticket panel message's `{text}` to `{tk_panels}` in the saved copy and
/// the copy kept for Undo, and leaves other entries alone; a second run changes nothing.
#[tokio::test]
async fn ticket_panel_variable_migration_renames_text() {
    let Some(p) = pool().await else { return };
    let g = gid(97);
    for t in ["bot_messages", "bot_messages_undo"] {
        let sql = format!("DELETE FROM {t} WHERE guild_id = $1");
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(g).execute(&p).await.unwrap();
    }
    let old = json!({"content": "", "embeds": [{"description": "Hi\n{text}"}]});
    sqlx::query("INSERT INTO bot_messages (guild_id, key, message, updated_by) VALUES ($1, 'ticket.panel', $2, 1), ($1, 'ticket.reply', $2, 1)")
        .bind(g).bind(&old).execute(&p).await.unwrap();
    sqlx::query("INSERT INTO bot_messages_undo (guild_id, key, message) VALUES ($1, 'ticket.panel', $2)")
        .bind(g).bind(&old).execute(&p).await.unwrap();
    let sql = include_str!("../migrations/4071_ticket_panel_variable.sql");
    for _ in 0..2 {
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).execute(&p).await.unwrap();
        let get = |t: &'static str, k: &'static str| {
            let p = p.clone();
            async move {
                let sql = format!("SELECT message FROM {t} WHERE guild_id = $1 AND key = $2");
                sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(g).bind(k).fetch_one(&p).await.unwrap()
            }
        };
        let new = json!({"content": "", "embeds": [{"description": "Hi\n{tk_panels}"}]});
        assert_eq!(get("bot_messages", "ticket.panel").await, new);
        assert_eq!(get("bot_messages_undo", "ticket.panel").await, new);
        assert_eq!(get("bot_messages", "ticket.reply").await, old);
    }
    for t in ["bot_messages", "bot_messages_undo"] {
        let sql = format!("DELETE FROM {t} WHERE guild_id = $1");
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(g).execute(&p).await.unwrap();
    }
}

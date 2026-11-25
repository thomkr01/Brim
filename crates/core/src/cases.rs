//! Cases and the action log (undo data for /revert, spec 2.5.2).
use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

use crate::models::{GuildId, UserId};

/// Kinds that act on a channel, not a member: a purge without a member, a slowmode.
pub const CHANNEL_KINDS: [&str; 2] = ["purge", "slowmode"];

/// SQL condition (columns of `cases`, unqualified): the case has a member as its target. New
/// cases without one store `user_id = 0`; older purge and slowmode cases stored the moderator
/// (self-targets are refused, so that pair always means no target). Keeps `user_id = $n` indexed.
pub const HAS_TARGET: &str =
    "(user_id <> 0 AND NOT (kind IN ('purge', 'slowmode') AND user_id = mod_id))";

/// SQL expression for the target to show: NULL when the case has none ([`HAS_TARGET`]).
pub const TARGET: &str = "CASE WHEN user_id <> 0 AND NOT (kind IN ('purge', 'slowmode') AND user_id = mod_id) THEN user_id END";

/// The member a case targets, None for a case without one. The same rule as [`HAS_TARGET`].
pub fn target(kind: &str, user_id: i64, mod_id: i64) -> Option<i64> {
    let none = user_id == 0 || (CHANNEL_KINDS.contains(&kind) && user_id == mod_id);
    (!none).then_some(user_id)
}

pub struct NewCase<'a> {
    pub guild_id: GuildId,
    pub kind: &'a str,
    /// 0 when the case has no member (a purge without one, a slowmode).
    pub user_id: UserId,
    pub mod_id: UserId,
    pub reason: Option<&'a str>,
    pub points: i32,
    pub duration_s: Option<i64>,
    /// Attachment URL from the slash `proof:` option; becomes the case's first proof.
    pub proof: Option<&'a str>,
    /// Inverse operation, e.g. `{"op":"unban","user":123}`; stored in action_log.
    pub inverse: Option<Value>,
    /// The part of Brim that made the case, for the stats (`staff`, `automod`, `antinuke`, ...).
    pub feature: &'a str,
}

/// Inserts the case and its action_log row atomically; returns the per-guild case number.
pub async fn create(pool: &PgPool, c: NewCase<'_>) -> Result<i32> {
    let target = (c.user_id != 0).then_some(c.user_id);
    let mut tx = pool.begin().await?;
    // Serialize case numbering per guild. Its own key: the bare guild id is the config lock.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('cases:' || $1, 0))")
        .bind(c.guild_id)
        .execute(&mut *tx)
        .await?;
    let (case_no,): (i32,) = sqlx::query_as(
        "INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, reason, points, duration_s, expires_at)
         VALUES ($1, (SELECT COALESCE(MAX(case_no), 0) + 1 FROM cases WHERE guild_id = $1),
                 $2, $3, $4, $5, $6, $7,
                 CASE WHEN $2 IN ('warn') THEN now() + interval '1 month' END)
         RETURNING case_no",
    )
    .bind(c.guild_id)
    .bind(c.kind)
    .bind(c.user_id)
    .bind(c.mod_id)
    .bind(c.reason)
    .bind(c.points)
    .bind(c.duration_s)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(p) = c.proof {
        sqlx::query(
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(c.guild_id)
        .bind(case_no)
        .bind(proof_kind(p))
        .bind(p)
        .bind(c.mod_id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse, case_no)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(c.guild_id)
    .bind(c.mod_id)
    .bind(c.kind)
    .bind(target)
    .bind(c.inverse)
    .bind(case_no)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        ModAction {
            guild_id: c.guild_id,
            mod_id: c.mod_id,
            kind: c.kind,
            source: "bot",
            target_id: target,
            case_no: Some(case_no),
            audit_id: None,
            ai_client: None,
            feature: Some(c.feature),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(case_no)
}

/// One staff action for moderator stats (`mod_actions`, append-only).
pub struct ModAction<'a> {
    pub guild_id: GuildId,
    pub mod_id: UserId,
    pub kind: &'a str,
    /// `bot` (a case), `discord` (done in the Discord app), `appeal` or `dashboard`.
    pub source: &'a str,
    pub target_id: Option<UserId>,
    pub case_no: Option<i32>,
    /// The Discord audit log entry, so a replayed gateway event counts once.
    pub audit_id: Option<i64>,
    /// The MCP token that made the change for the bot owner (`mcp::Via`); None for a person.
    pub ai_client: Option<&'a str>,
    /// The part of Brim for the stats; None counts it under `source`. Bot only: the dashboard
    /// role has no right on this column.
    pub feature: Option<&'a str>,
}

/// Records a staff action, inside the caller's transaction when it has one.
pub async fn record(c: &mut sqlx::PgConnection, a: ModAction<'_>) -> Result<()> {
    // The trigger `stat_mod_action` (migration 4100) counts the row for the stats.
    let sql = if a.feature.is_some() {
        "INSERT INTO mod_actions (guild_id, mod_id, kind, source, target_id, case_no, audit_id, ai_client, feature)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING"
    } else {
        "INSERT INTO mod_actions (guild_id, mod_id, kind, source, target_id, case_no, audit_id, ai_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING"
    };
    let q = sqlx::query(sql)
        .bind(a.guild_id)
        .bind(a.mod_id)
        .bind(a.kind)
        .bind(a.source)
        .bind(a.target_id)
        .bind(a.case_no)
        .bind(a.audit_id)
        .bind(a.ai_client);
    let q = match a.feature {
        Some(f) => q.bind(f),
        None => q,
    };
    q.execute(c).await?;
    Ok(())
}

/// Why undoing this case would lift a punishment the case did not make: its ban or timeout has
/// run out, a later case banned, timed out, quarantined or released the member, or the member's
/// quarantine was made after it. `None` when the case's punishment is still the current one.
pub async fn not_current(pool: &PgPool, guild_id: GuildId, case_no: i32) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT CASE
            WHEN c.kind IN ('ban', 'tempban', 'timeout')
                 AND c.created_at + make_interval(secs => c.duration_s) < now() THEN 'it has ended'
            WHEN n.case_no IS NOT NULL THEN 'case #' || n.case_no || ' came after it'
            WHEN c.kind = 'quarantine' AND q.created_at > c.created_at
                THEN 'the member was quarantined again after it'
         END
         FROM cases c
         LEFT JOIN LATERAL (
            SELECT max(n.case_no) AS case_no FROM cases n
            WHERE n.guild_id = c.guild_id AND n.user_id = c.user_id AND n.case_no > c.case_no
              AND n.kind = ANY (CASE
                    WHEN c.kind IN ('ban', 'tempban', 'softban')
                        THEN ARRAY['ban', 'tempban', 'softban', 'unban']
                    WHEN c.kind = 'timeout' THEN ARRAY['timeout', 'untimeout']
                    WHEN c.kind = 'quarantine' THEN ARRAY['quarantine', 'unquarantine']
                    ELSE ARRAY[]::text[] END)
         ) n ON true
         LEFT JOIN quarantines q ON q.guild_id = c.guild_id AND q.user_id = c.user_id
         WHERE c.guild_id = $1 AND c.case_no = $2",
    )
    .bind(guild_id)
    .bind(case_no)
    .fetch_optional(pool)
    .await?
    .flatten())
}

/// Active, unexpired warn points for a user.
pub async fn warn_points(pool: &PgPool, guild_id: GuildId, user_id: UserId) -> Result<i32> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(points), 0) FROM cases
         WHERE guild_id = $1 AND user_id = $2 AND kind = 'warn' AND active AND expires_at > now()",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(n as i32)
}

/// Warns reset after a threshold punishment is applied (spec 4.9), or by `clearwarns`. Each warn
/// it closes gets its history line, by `editor`.
pub async fn reset_warns(
    pool: &PgPool,
    guild_id: GuildId,
    user_id: UserId,
    editor: UserId,
) -> Result<()> {
    sqlx::query(
        "WITH up AS (UPDATE cases SET active = false
                     WHERE guild_id = $1 AND user_id = $2 AND kind = 'warn' AND active RETURNING case_no)
         INSERT INTO case_edits (guild_id, case_no, field, old_value, new_value, editor_id)
         SELECT $1, case_no, 'active', 'open', 'closed', $3 FROM up",
    )
    .bind(guild_id)
    .bind(user_id)
    .bind(editor)
    .execute(pool)
    .await?;
    Ok(())
}

/// Most proofs one case holds.
pub const PROOFS_MAX: i64 = 25;
/// Longest reason or proof text, in characters.
pub const CASE_TEXT_MAX: usize = 1000;

/// A proof staff typed: a web address alone is a link, anything else is text.
pub fn proof_kind(body: &str) -> &'static str {
    let b = body.trim();
    let web = b.starts_with("https://") || b.starts_with("http://");
    if web && !b.contains(char::is_whitespace) {
        "link"
    } else {
        "text"
    }
}

/// What [`add_proof`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    Done,
    NoCase,
    /// The case already holds [`PROOFS_MAX`] proofs.
    Full,
}

/// A copy of the punished member's own message, stored by the bot when it acted on it.
pub struct MessageCopy<'a> {
    pub channel_id: i64,
    /// None for a message Discord AutoMod blocked before it was posted.
    pub message_id: Option<i64>,
    pub text: &'a str,
    /// The names of its files; the files themselves are not kept.
    pub files: Vec<String>,
}

/// Adds one proof to a case. `kind` is `link`, `text` or `image` (the mod log message that holds
/// the file). `copy` makes it a verified proof: only the bot passes it, never staff input.
pub async fn add_proof(
    pool: &PgPool,
    guild_id: GuildId,
    case_no: i32,
    (kind, body): (&str, &str),
    (added_by, ai_client): (UserId, Option<&str>),
    copy: Option<&MessageCopy<'_>>,
) -> sqlx::Result<Added> {
    // The case row is locked, so two adds at once cannot both pass the limit.
    let mut tx = pool.begin().await?;
    let held: Option<i64> = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM case_proofs p WHERE p.guild_id = c.guild_id AND p.case_no = c.case_no)
         FROM cases c WHERE c.guild_id = $1 AND c.case_no = $2 FOR UPDATE OF c",
    )
    .bind(guild_id)
    .bind(case_no)
    .fetch_optional(&mut *tx)
    .await?;
    match held {
        None => return Ok(Added::NoCase),
        Some(n) if n >= PROOFS_MAX => return Ok(Added::Full),
        Some(_) => {}
    }
    let body: String = body.trim().chars().take(CASE_TEXT_MAX).collect();
    let Some(copy) = copy else {
        // Staff proof names only these columns: the dashboard's database role may write no other
        // (`db::set_dashboard_role`), so it cannot make or change a verified proof.
        sqlx::query(
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, ai_client)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(guild_id)
        .bind(case_no)
        .bind(kind)
        .bind(body)
        .bind(added_by)
        .bind(ai_client)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Added::Done);
    };
    // The message's own time is in its id (a Discord snowflake).
    sqlx::query(
        "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, ai_client, verified,
                                  channel_id, message_id, sent_at, files)
         VALUES ($1, $2, $3, $4, $5, $6, true, $7, $8,
                 COALESCE(to_timestamp((($8::bigint >> 22) + 1420070400000) / 1000.0), now()), $9)",
    )
    .bind(guild_id)
    .bind(case_no)
    .bind(kind)
    .bind(body)
    .bind(added_by)
    .bind(ai_client)
    .bind(copy.channel_id)
    .bind(copy.message_id)
    .bind(&copy.files)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Added::Done)
}

/// A case before an [`edit`].
#[derive(Debug, PartialEq, Eq)]
pub struct Before {
    pub user_id: UserId,
    pub reason: Option<String>,
    pub active: bool,
}

/// Changes a case's reason (`Some(None)` clears it) and/or its open state, and writes one
/// `case_edits` row per field that really changed, in one statement. Returns the case as it was,
/// or None when the server has no such case.
pub async fn edit(
    conn: &mut sqlx::PgConnection,
    guild_id: GuildId,
    case_no: i32,
    reason: Option<Option<&str>>,
    active: Option<bool>,
    (editor, ai_client): (UserId, Option<&str>),
) -> sqlx::Result<Option<Before>> {
    let row: Option<(i64, Option<String>, bool)> = sqlx::query_as(
        "WITH old AS (SELECT user_id, reason, active FROM cases WHERE guild_id = $1 AND case_no = $2 FOR UPDATE),
              up AS (UPDATE cases c SET reason = CASE WHEN $3 THEN $4 ELSE c.reason END,
                                        active = COALESCE($5, c.active)
                     FROM old WHERE c.guild_id = $1 AND c.case_no = $2),
              ed AS (INSERT INTO case_edits (guild_id, case_no, field, old_value, new_value, editor_id, ai_client)
                     SELECT $1, $2, f.field, f.was, f.now, $6, $7 FROM old, LATERAL (VALUES
                         ('reason', old.reason, $4::text, $3 AND old.reason IS DISTINCT FROM $4),
                         ('active', CASE WHEN old.active THEN 'open' ELSE 'closed' END,
                                    CASE WHEN $5 THEN 'open' ELSE 'closed' END,
                                    $5::boolean IS NOT NULL AND old.active <> $5)) f(field, was, now, changed)
                     WHERE f.changed)
         SELECT user_id, reason, active FROM old",
    )
    .bind(guild_id)
    .bind(case_no)
    .bind(reason.is_some())
    .bind(reason.flatten())
    .bind(active)
    .bind(editor)
    .bind(ai_client)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(user_id, reason, active)| Before {
        user_id,
        reason,
        active,
    }))
}

/// Removes one proof and leaves a history line that says only what kind it was. Returns the
/// removed proof's (kind, verified), or None when the case has no such proof.
pub async fn remove_proof(
    conn: &mut sqlx::PgConnection,
    guild_id: GuildId,
    case_no: i32,
    proof_id: i64,
    (editor, ai_client): (UserId, Option<&str>),
) -> sqlx::Result<Option<(String, bool)>> {
    sqlx::query_as(
        "WITH gone AS (DELETE FROM case_proofs WHERE guild_id = $1 AND case_no = $2 AND id = $3
                       RETURNING kind, verified, body),
              -- The copy an older image left in `cases.proof` (kept for a rollback) goes too,
              -- unless a second proof with the same text stays (the 4037 trigger would remove it).
              old AS (UPDATE cases c SET proof = NULL FROM gone
                      WHERE c.guild_id = $1 AND c.case_no = $2 AND c.proof = gone.body
                        AND NOT EXISTS (SELECT 1 FROM case_proofs p
                                        WHERE p.guild_id = $1 AND p.case_no = $2 AND p.id <> $3
                                          AND p.body = gone.body AND NOT p.verified)),
              ed AS (INSERT INTO case_edits (guild_id, case_no, field, old_value, editor_id, ai_client)
                     SELECT $1, $2, 'proof', CASE WHEN verified THEN 'verified message' ELSE kind END, $4, $5
                     FROM gone)
         SELECT kind, verified FROM gone",
    )
    .bind(guild_id)
    .bind(case_no)
    .bind(proof_id)
    .bind(editor)
    .bind(ai_client)
    .fetch_optional(conn)
    .await
}

/// What changed on a case; the payload of the `case_log` job the dashboard queues, and what the
/// bot's own case commands send to the mod log.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CaseLog {
    pub case_no: i32,
    pub by: u64,
    /// The MCP token that made the change for the bot owner (`by`); None for a person.
    pub ai: Option<String>,
    /// (old, new) of an edited reason.
    pub reason: Option<(Option<String>, Option<String>)>,
    /// The new open state, when it changed.
    pub active: Option<bool>,
    /// The body of an added proof.
    pub proof_added: Option<String>,
    /// The kind of a removed proof.
    pub proof_removed: Option<String>,
}

/// A case a staff member deleted in the dashboard: what the mod log shows (job `case_delete_log`).
/// No proof text and no message content, only what the case row itself says.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CaseDelete {
    pub case_no: i32,
    pub kind: String,
    pub user: u64,
    /// The moderator of the case.
    pub moderator: u64,
    /// Who deleted it.
    pub by: u64,
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn an_undo_knows_when_its_case_is_not_the_current_punishment() {
        let Ok(url) = crate::db::test_url() else {
            return;
        };
        let pool = crate::db::connect(&url).await.unwrap();
        let g = 5_377_000_000 + std::process::id() as i64;
        crate::db::purge_guild(&pool, g).await.unwrap();
        let case = |kind: &'static str, user_id: i64, duration_s: Option<i64>| {
            let c = NewCase {
                guild_id: g,
                kind,
                user_id,
                mod_id: 9,
                reason: None,
                points: 0,
                duration_s,
                proof: None,
                inverse: None,
                feature: "staff",
            };
            create(&pool, c)
        };
        let why = |n: i32| not_current(&pool, g, n);
        let run = |sql: &'static str| sqlx::query(sql).bind(g).execute(&pool);

        // Member ids no other test uses: the data erasure tests change cases by member id alone.
        // A ban is current until a later case bans or unbans the same member.
        let ban = case("ban", 771_001, None).await.unwrap();
        let other = case("ban", 771_002, None).await.unwrap();
        case("warn", 771_001, None).await.unwrap();
        assert_eq!(why(ban).await.unwrap(), None);
        let unban = case("unban", 771_001, None).await.unwrap();
        let later = Some(format!("case #{unban} came after it"));
        assert_eq!(why(ban).await.unwrap(), later);
        assert_eq!(why(other).await.unwrap(), None, "another member's case");

        // A timeout that ran out is no longer the member's timeout.
        let timeout = case("timeout", 771_003, Some(60)).await.unwrap();
        assert_eq!(why(timeout).await.unwrap(), None);
        run("UPDATE cases SET created_at = now() - interval '2 minutes' WHERE guild_id = $1 AND user_id = 771003")
            .await
            .unwrap();
        assert_eq!(why(timeout).await.unwrap().as_deref(), Some("it has ended"));

        // A quarantine made after the case (anti-nuke opens no case here) is not the case's.
        let held = case("quarantine", 771_004, None).await.unwrap();
        run("INSERT INTO quarantines (guild_id, user_id, saved_roles, created_at) VALUES ($1, 771004, '{}', now() - interval '1 hour')")
            .await
            .unwrap();
        assert_eq!(why(held).await.unwrap(), None);
        run("UPDATE quarantines SET created_at = now() + interval '1 minute' WHERE guild_id = $1")
            .await
            .unwrap();
        let again = Some("the member was quarantined again after it");
        assert_eq!(why(held).await.unwrap().as_deref(), again);
        assert_eq!(why(9999).await.unwrap(), None, "no such case");
        crate::db::purge_guild(&pool, g).await.unwrap();
    }

    #[test]
    fn a_channel_case_has_no_target() {
        assert_eq!(target("purge", 0, 9), None, "new purge of a channel");
        assert_eq!(
            target("purge", 9, 9),
            None,
            "older purge: the moderator was stored"
        );
        assert_eq!(target("slowmode", 9, 9), None);
        assert_eq!(target("purge", 7, 9), Some(7), "purge of one member");
        assert_eq!(
            target("ban", 9, 9),
            Some(9),
            "only channel kinds use the old rule"
        );
        for k in CHANNEL_KINDS {
            assert!(HAS_TARGET.contains(&format!("'{k}'")) && TARGET.contains(&format!("'{k}'")));
        }
    }

    /// The SQL rule agrees with `target`, and a case without a member logs no target.
    #[tokio::test]
    async fn the_sql_target_rule_matches_the_code() {
        let Ok(url) = crate::db::test_url() else {
            return;
        };
        let pool = crate::db::connect(&url).await.unwrap();
        let g = 5_378_000_000 + std::process::id() as i64;
        crate::db::purge_guild(&pool, g).await.unwrap();
        let rows = [
            ("purge", 0),
            ("purge", 9),
            ("slowmode", 9),
            ("purge", 771_101),
            ("ban", 9),
        ];
        for (kind, user_id) in rows {
            let c = NewCase {
                guild_id: g,
                kind,
                user_id,
                mod_id: 9,
                reason: None,
                points: 0,
                duration_s: None,
                proof: None,
                inverse: None,
                feature: "staff",
            };
            create(&pool, c).await.unwrap();
        }
        let sql = format!(
            "SELECT kind, user_id, mod_id, {TARGET}, {HAS_TARGET} FROM cases WHERE guild_id = $1 ORDER BY case_no"
        );
        let got: Vec<(String, i64, i64, Option<i64>, bool)> =
            sqlx::query_as(sqlx::AssertSqlSafe(sql))
                .bind(g)
                .fetch_all(&pool)
                .await
                .unwrap();
        for (kind, user, by, shown, has) in got {
            assert_eq!(shown, target(&kind, user, by), "{kind} {user}");
            assert_eq!(has, shown.is_some(), "{kind} {user}");
        }
        let logged: Option<i64> = sqlx::query_scalar(
            "SELECT target_id FROM action_log WHERE guild_id = $1 AND case_no = 1",
        )
        .bind(g)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(logged, None);
        crate::db::purge_guild(&pool, g).await.unwrap();
    }
}

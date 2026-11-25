use anyhow::Result;
use sqlx::{postgres::PgPoolOptions, PgPool};

fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!("./migrations")
}

/// The dashboard's own database role (security review): not a
/// superuser, it reads and writes rows but cannot change the schema, other roles or files.
pub const DASHBOARD_ROLE: &str = "brim_dashboard";
/// The MCP server's read-only role (migration 2700).
pub const MCP_ROLE: &str = "brim_mcp";

/// Whether this connection logs in under a name from before the rename to Brim (the bot, the
/// dashboard or the MCP server of a server not moved yet).
async fn old_names(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT current_user IN ('tommybot', 'tommybot_dashboard', 'tommybot_mcp')",
    )
    .fetch_one(pool)
    .await?)
}

/// Clears every `ALTER ROLE ... SET` default of `brim_dashboard` (the caller swaps in the real name):
/// a role may set its own defaults, which then hold in each of its sessions.
const RESET_ROLE: &str = "DO $$ DECLARE d TEXT; BEGIN
    ALTER ROLE brim_dashboard RESET ALL;
    FOR d IN SELECT datname FROM pg_db_role_setting s JOIN pg_database b ON b.oid = s.setdatabase
             WHERE s.setrole = 'brim_dashboard'::regrole LOOP
        EXECUTE format('ALTER ROLE brim_dashboard IN DATABASE %I RESET ALL', d);
    END LOOP; END $$";

/// Connects and gets the schema ready. The role that owns the schema (the bot) runs the
/// migrations and sets the passwords of the other roles; any other role (the dashboard as
/// [`DASHBOARD_ROLE`]) may not, so it waits until the owner has run every migration it knows.
pub async fn connect(url: &str) -> Result<PgPool> {
    let mut tries = 0;
    let pool = loop {
        match PgPoolOptions::new().max_connections(10).connect(url).await {
            // 28xxx: a login refused. The bot sets the dashboard role's password at its start,
            // which can come after the dashboard's first try.
            Err(sqlx::Error::Database(e))
                if tries < 60 && e.code().is_some_and(|c| c.starts_with("28")) =>
            {
                tries += 1;
                tracing::warn!(error = %e, "database login refused, trying again in 2 s");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            r => break r?,
        }
    };
    // Before any migration or role change: this build knows only the Brim roles, and it would lock
    // the old ones out (`lock_old_roles`).
    anyhow::ensure!(
        !old_names(&pool).await?,
        "this server still runs under the TommyBot names (database user tommybot). This version runs only on the Brim names. Move the server first: DEPLOY.md, \"Old names\". Nothing was changed."
    );
    if !owns_schema(&pool).await? {
        wait_for_schema(&pool).await?;
        return Ok(pool);
    }
    // An older binary after a rollback must start on a newer schema: migrations stay additive.
    migrator().set_ignore_missing(true).run(&pool).await?;
    lock_old_roles(&pool).await?;
    if let Some(pw) = std::env::var("MCP_DB_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
    {
        set_mcp_password(&pool, &pw).await?;
    }
    if let Some(pw) = std::env::var("DASHBOARD_DB_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
    {
        set_dashboard_role(&pool, &pw).await?;
    }
    Ok(pool)
}

/// The old roles may not log in: migration 2700 makes `tommybot_mcp` in every database, and an image
/// from before the rename (a rollback) would make both old roles again with the new passwords and
/// wider grants, which nothing narrows once the Brim names are back. Roles belong to the whole
/// Postgres server: compose runs one per install.
async fn lock_old_roles(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "DO $$ DECLARE r TEXT; BEGIN
            FOR r IN SELECT rolname FROM pg_roles WHERE rolname IN ('tommybot_dashboard', 'tommybot_mcp') AND rolcanlogin LOOP
                EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL', r);
            END LOOP; END $$",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A superuser, the owner of the migrations table, or a new database (no table yet).
async fn owns_schema(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT (SELECT rolsuper FROM pg_roles WHERE rolname = current_user)
             OR COALESCE((SELECT tableowner = current_user FROM pg_tables
                          WHERE schemaname = 'public' AND tablename = '_sqlx_migrations'), true)",
    )
    .fetch_one(pool)
    .await?)
}

/// Waits (up to 10 minutes) until every migration this binary knows has run.
async fn wait_for_schema(pool: &PgPool) -> Result<()> {
    let want: Vec<i64> = migrator().iter().map(|m| m.version).collect();
    for i in 0..300 {
        // And the bot's grants, which follow its migrations (after a restore they are gone).
        let (have, granted): (i64, bool) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM _sqlx_migrations WHERE success AND version = ANY($1)),
                    has_table_privilege('scheduled_jobs', 'INSERT')",
        )
        .bind(&want)
        .fetch_one(pool)
        .await?;
        if have == want.len() as i64 && granted {
            return Ok(());
        }
        if i % 15 == 0 {
            tracing::info!("waiting for the bot to update the database schema");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    anyhow::bail!(
        "the database schema is older than this dashboard and the bot did not update it within 10 minutes: check `docker compose logs bot`"
    )
}

/// Makes the dashboard's role, gives it rows (not the schema) and sets its password from
/// `DASHBOARD_DB_PASSWORD`. Run at every bot start, so it also covers tables added since, and a
/// database restored into a new Postgres (`pg_dump` has no roles).
pub async fn set_dashboard_role(pool: &PgPool, pw: &str) -> Result<()> {
    // One transaction: no moment in which the grant on every table also covers the migration list.
    let mut tx = pool.begin().await?;
    for sql in [
        // 12 of max_connections 40: the dashboard's pool (10) and its config listener.
        "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'brim_dashboard') THEN CREATE ROLE brim_dashboard NOLOGIN; END IF; END $$",
        RESET_ROLE,
        "ALTER ROLE brim_dashboard NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 12",
        "GRANT USAGE ON SCHEMA public TO brim_dashboard",
        "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO brim_dashboard",
        "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO brim_dashboard",
        // Tables a later migration adds, before this runs again.
        "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO brim_dashboard",
        "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO brim_dashboard",
        // nextval needs no UPDATE; setval would let it break every insert.
        "REVOKE UPDATE ON ALL SEQUENCES IN SCHEMA public FROM brim_dashboard",
        "ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE UPDATE ON SEQUENCES FROM brim_dashboard",
        // It only reads which migrations ran.
        "REVOKE INSERT, UPDATE, DELETE ON _sqlx_migrations FROM brim_dashboard",
        // It adds change lines, never rewrites one (it still deletes them in a server purge).
        "REVOKE UPDATE ON dashboard_audit FROM brim_dashboard",
        // Deletions a restore applies again (migration 4035): written only by its triggers, so a
        // dashboard bug cannot remove a line and bring a revoked grant back with the next restore.
        "REVOKE ALL ON erasure_log, erasure_log_id_seq FROM brim_dashboard",
        // A verified proof (the bot's copy of a member's message) is made by the bot only: the
        // dashboard adds staff proof through these columns and changes no proof row. It still
        // deletes them (staff remove a proof; a case's delete and a person's data deletion).
        "REVOKE INSERT, UPDATE ON case_proofs FROM brim_dashboard",
        "GRANT INSERT (guild_id, case_no, kind, body, added_by, ai_client) ON case_proofs TO brim_dashboard",
        // The same rule for every table the bot writes and the dashboard only reads or marks: the
        // role gets the columns its routes change and no more. A new dashboard write to one of
        // these needs its column here and in migration 4043's trigger (which keeps these limits when
        // an older image grants everything again), and `dashboard_role_has_rows_only` runs it as the role.
        // A case: staff edit the reason and close or reopen it; who it is about and what kind it
        // is stay the bot's (a verified proof says what THAT member wrote). `proof` is the
        // column from before migration 4037, cleared when its proof is removed.
        "REVOKE INSERT, UPDATE ON cases FROM brim_dashboard",
        "GRANT UPDATE (reason, active, proof) ON cases TO brim_dashboard",
        // A report: staff mark it handled. Its text is what the bot copies as verified proof when
        // a moderator acts on it.
        "REVOKE INSERT, UPDATE ON reports FROM brim_dashboard",
        "GRANT UPDATE (status, handled_by, handled_at) ON reports TO brim_dashboard",
        // The bot's own records of what happened: read only (and deleted with their server).
        "REVOKE INSERT, UPDATE ON raid_log, joingate_actions, nuke_incidents, catch_hits FROM brim_dashboard",
        // The stats counts (migration 4100): read only. Its staff actions count through the
        // `mod_actions` trigger, its server purge through `purge_guild`, both as the owner.
        "REVOKE INSERT, UPDATE, DELETE ON stat_events, stat_daily, stat_daily_all, stat_folded, stat_servers_ever, stat_members FROM brim_dashboard",
        // Only the bot counts wrong giveaway entry codes.
        "REVOKE INSERT, UPDATE, DELETE ON giveaway_code_tries FROM brim_dashboard",
        // Members' stored names and avatars (migration 4044): it only counts them for a data
        // request. The bot deletes them (leave, retention, data delete request, server purge).
        "REVOKE INSERT, UPDATE, DELETE ON profile_snapshot FROM brim_dashboard",
        // History and log lines the dashboard adds (a case or note edit, a staff action, a raider
        // report) and never changes or deletes: only the columns its statements name, so the id
        // and time are the defaults (a chosen id would block the bot's next line). A case's or
        // note's delete removes its lines through the foreign key, which runs as the owner.
        "REVOKE INSERT, UPDATE, DELETE ON case_edits, note_edits, mod_actions, raider_evidence FROM brim_dashboard",
        "GRANT INSERT (guild_id, case_no, field, old_value, new_value, editor_id, ai_client) ON case_edits TO brim_dashboard",
        "GRANT INSERT (note_id, guild_id, old_text, new_text, editor_id, ai_client) ON note_edits TO brim_dashboard",
        "GRANT INSERT (guild_id, mod_id, kind, source, target_id, case_no, audit_id, ai_client) ON mod_actions TO brim_dashboard",
        "GRANT INSERT (user_id, source_guild, source_owner, kind, ref, detail, reporter_id, note) ON raider_evidence TO brim_dashboard",
        // Its server purge (SECURITY DEFINER, closed to PUBLIC by migration 4035).
        "GRANT EXECUTE ON FUNCTION purge_guild(BIGINT) TO brim_dashboard",
    ] {
        sqlx::query(sql).execute(&mut *tx).await?;
    }
    let sql: String =
        sqlx::query_scalar("SELECT format('ALTER ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)")
            .bind(DASHBOARD_ROLE)
            .bind(pw)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// `DATABASE_URL` for the DB tests, which delete rows and change the MCP role: refused (panic)
/// unless the database name ends in `_test` or `BRIM_TEST_DB=1` is set, so a production URL in
/// the shell is never touched. Unset is `Err` (the test skips), except in CI, where a skip would
/// report green without running anything.
pub fn test_url() -> Result<String, std::env::VarError> {
    let url = match std::env::var("DATABASE_URL") {
        Err(_) if std::env::var("CI").is_ok() => {
            panic!("CI must run the DB tests, but DATABASE_URL is not set")
        }
        r => r?,
    };
    let name = url
        .split('?')
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    let scratch = name.ends_with("_test") || std::env::var("BRIM_TEST_DB").is_ok_and(|v| v == "1");
    assert!(
        scratch,
        "DATABASE_URL names the database `{name}`: the DB tests only run on a database whose name ends in `_test`, or with BRIM_TEST_DB=1"
    );
    Ok(url)
}

/// Lets the MCP server's read-only role (migration 2700) log in. A migration cannot read .env.
/// It also makes the role, its settings and its rights again first: `pg_dump` has no roles and no
/// `ALTER ROLE ... SET`, so a database restored into a new Postgres has migration 2700 recorded but
/// no role, and the Brim name `brim_mcp` never had the grants of the migrations (they name the old one).
pub async fn set_mcp_password(pool: &PgPool, pw: &str) -> Result<()> {
    let reset = RESET_ROLE.replace("brim_dashboard", "brim_mcp");
    // One transaction: no session in between that is not read-only.
    let mut tx = pool.begin().await?;
    for sql in [
        "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'brim_mcp') THEN CREATE ROLE brim_mcp NOLOGIN CONNECTION LIMIT 5; END IF; END $$",
        // Before the two below: the role could have turned its own read-only default off.
        reset.as_str(),
        "ALTER ROLE brim_mcp SET default_transaction_read_only = on",
        "ALTER ROLE brim_mcp SET statement_timeout = '5s'",
        "ALTER ROLE brim_mcp CONNECTION LIMIT 5",
        // The views the MCP server reads (migrations 2700 to 3701; not mcp_nsfw_scans, 4021 Q12).
        "GRANT USAGE ON SCHEMA public TO brim_mcp",
        "GRANT SELECT ON mcp_guilds, mcp_guild_config, mcp_cases, mcp_raid_log, mcp_joingate_log, mcp_update_runs,
            mcp_action_log, mcp_nuke_incidents, mcp_quarantines, mcp_strikes, mcp_appeals, mcp_reports, mcp_notes,
            mcp_jobs, mcp_backups, mcp_raiders, mcp_mod_actions TO brim_mcp",
        // Its read log lines and token check (migration 4021).
        "GRANT INSERT (guild_id, tool, via) ON mcp_reads TO brim_mcp",
        "GRANT USAGE ON SEQUENCE mcp_reads_id_seq TO brim_mcp",
        "GRANT EXECUTE ON FUNCTION mcp_token_name(BYTEA) TO brim_mcp",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *tx).await?;
    }
    let sql: String =
        sqlx::query_scalar("SELECT format('ALTER ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)")
            .bind(MCP_ROLE)
            .bind(pw)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes every row stored for `guild` (migration 1501); the bot then falls back to defaults.
pub async fn purge_guild(pool: &PgPool, guild: i64) -> Result<()> {
    sqlx::query("SELECT purge_guild($1)")
        .bind(guild)
        .execute(pool)
        .await?;
    Ok(())
}

/// Every guild marked as left, and whether it left more than `days` ago (due for [`purge_guild`]).
pub async fn left_guilds(pool: &PgPool, days: i32) -> Result<Vec<(i64, bool)>> {
    Ok(sqlx::query_as(
        "SELECT guild_id, left_at < now() - make_interval(days => $1) FROM guild_left",
    )
    .bind(days)
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgConnectOptions;

    /// Two migration files with one number stop every start: the second insert into
    /// `_sqlx_migrations` fails (two 4049 files, 2026-11-20).
    #[test]
    fn migration_numbers_are_unique() {
        let m = migrator();
        let mut v: Vec<i64> = m.iter().map(|m| m.version).collect();
        let n = v.len();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), n, "two migration files share a number");
    }

    /// Tests that change the dashboard role one at a time: Postgres refuses two ALTER ROLE at once.
    static ROLE_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A server still on the TommyBot names stops at start with a clear error and changes nothing.
    #[tokio::test]
    async fn old_names_stop_at_start() {
        let Ok(url) = test_url() else { return };
        let _one = ROLE_TESTS.lock().await;
        let owner = connect(&url).await.unwrap();
        sqlx::query("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tommybot') THEN CREATE ROLE tommybot; END IF; END $$")
            .execute(&owner)
            .await
            .unwrap();
        sqlx::query("ALTER ROLE tommybot LOGIN PASSWORD 'oldtest1'")
            .execute(&owner)
            .await
            .unwrap();
        let (_, host) = url.split_once('@').expect("the test URL names a user");
        let got = connect(&format!("postgres://tommybot:oldtest1@{host}")).await;
        sqlx::query("ALTER ROLE tommybot NOLOGIN PASSWORD NULL")
            .execute(&owner)
            .await
            .unwrap();
        let err = got.unwrap_err().to_string();
        assert!(err.contains("TommyBot names"), "{err}");
    }

    /// The dashboard's role reads and writes rows but not the schema, roles or the migration list,
    /// and the database, not the writer, records who queued a job.
    #[tokio::test]
    async fn dashboard_role_has_rows_only() {
        let Ok(url) = test_url() else { return };
        let _one = ROLE_TESTS.lock().await;
        let owner = connect(&url).await.unwrap();
        set_dashboard_role(&owner, "dashtest1").await.unwrap();
        let role = DASHBOARD_ROLE;
        let opts: PgConnectOptions = url.parse().unwrap();
        let dash = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.username(role).password("dashtest1"))
            .await
            .unwrap();
        assert!(!owns_schema(&dash).await.unwrap());
        wait_for_schema(&dash).await.unwrap();
        let gid = 5_100_000_000 + std::process::id() as i64;
        let (producer,): (String,) = sqlx::query_as(
            "INSERT INTO scheduled_jobs (guild_id, kind, payload, run_at, producer)
             VALUES ($1, 'x', '{}', now() + interval '1 hour', 'tommybot') RETURNING producer",
        )
        .bind(gid)
        .fetch_one(&dash)
        .await
        .unwrap();
        assert_eq!(producer, role, "a writer cannot name another producer");
        let (producer,): (String,) = sqlx::query_as(
            "UPDATE scheduled_jobs SET producer = 'tommybot' WHERE guild_id = $1 RETURNING producer",
        )
        .bind(gid)
        .fetch_one(&owner)
        .await
        .unwrap();
        assert_eq!(producer, role, "a writer cannot name another producer");
        let (producer,): (String,) = sqlx::query_as(
            "UPDATE scheduled_jobs SET payload = '{\"b\":1}' WHERE guild_id = $1 RETURNING producer",
        )
        .bind(gid)
        .fetch_one(&owner)
        .await
        .unwrap();
        assert_ne!(producer, role, "a new payload names its writer");
        let (producer,): (String,) = sqlx::query_as(
            "UPDATE scheduled_jobs SET attempts = attempts + 1 WHERE guild_id = $1 RETURNING producer",
        )
        .bind(gid)
        .fetch_one(&dash)
        .await
        .unwrap();
        assert_ne!(producer, role, "a lease or attempt keeps it");
        for sql in [
            "CREATE TABLE dash_role_probe (x int)",
            "CREATE ROLE dash_role_probe",
            "DELETE FROM _sqlx_migrations WHERE false",
            "TRUNCATE scheduled_jobs",
            "SELECT setval('scheduled_jobs_id_seq', 1)",
            "UPDATE dashboard_audit SET text = 'x' WHERE false",
            "ALTER TABLE scheduled_jobs DISABLE TRIGGER scheduled_jobs_producer",
            "COPY (SELECT 1) TO PROGRAM 'true'",
            "SELECT 1 FROM erasure_log",
            "DELETE FROM erasure_log WHERE false",
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, verified) SELECT 1, 1, 'text', 'x', 1, true WHERE false",
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, message_id) SELECT 1, 1, 'text', 'x', 1, 1 WHERE false",
            "UPDATE case_proofs SET verified = true WHERE false",
            // Needs the UPDATE right too: the dashboard's proof removal must read without it.
            "SELECT verified FROM case_proofs WHERE false FOR UPDATE",
            "UPDATE case_proofs SET body = 'x' WHERE false",
            "UPDATE case_edits SET old_value = 'x' WHERE false",
            "UPDATE note_edits SET old_text = 'x' WHERE false",
            "INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id) SELECT 1, 1, 'warn', 1, 1 WHERE false",
            "UPDATE cases SET user_id = 1 WHERE false",
            "UPDATE cases SET kind = 'ban' WHERE false",
            "UPDATE cases SET created_at = now() WHERE false",
            "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, content) SELECT 1, 1, 1, 1, 'x' WHERE false",
            "UPDATE reports SET content = 'x' WHERE false",
            "UPDATE reports SET target_id = 1 WHERE false",
            "UPDATE reports SET message_id = 1 WHERE false",
            "UPDATE raid_log SET guild_id = 1 WHERE false",
            "UPDATE joingate_actions SET guild_id = 1 WHERE false",
            "UPDATE nuke_incidents SET guild_id = 1 WHERE false",
            "INSERT INTO raid_log SELECT * FROM raid_log WHERE false",
            "INSERT INTO joingate_actions SELECT * FROM joingate_actions WHERE false",
            "INSERT INTO nuke_incidents SELECT * FROM nuke_incidents WHERE false",
            "INSERT INTO profile_snapshot SELECT * FROM profile_snapshot WHERE false",
            "UPDATE profile_snapshot SET username = 'x' WHERE false",
            "DELETE FROM profile_snapshot WHERE false",
            "UPDATE mod_actions SET mod_id = 1 WHERE false",
            "UPDATE raider_evidence SET user_id = 1 WHERE false",
            "DELETE FROM case_edits WHERE false",
            "DELETE FROM note_edits WHERE false",
            "DELETE FROM mod_actions WHERE false",
            "DELETE FROM raider_evidence WHERE false",
            "INSERT INTO mod_actions (id, guild_id, mod_id, kind, source) SELECT 1, 1, 1, 'x', 'x' WHERE false",
            "INSERT INTO case_edits (guild_id, case_no, field, editor_id, edited_at) SELECT 1, 1, 'reason', 1, now() WHERE false",
            // The stats counts: read only, and no way past the triggers.
            "INSERT INTO stat_events (guild_id, kind, feature) SELECT 1, 'x', 'x' WHERE false",
            "INSERT INTO stat_daily SELECT * FROM stat_daily WHERE false",
            "UPDATE stat_daily SET n = 0 WHERE false",
            "DELETE FROM stat_daily WHERE false",
            "DELETE FROM stat_daily_all WHERE false",
            "UPDATE stat_folded SET day = now() WHERE false",
            "SELECT stat_add(1, 'x', 'x', 1, NULL) WHERE false",
            "INSERT INTO mod_actions (guild_id, mod_id, kind, source, feature) SELECT 1, 1, 'x', 'x', 'x' WHERE false",
        ] {
            let e = sqlx::query(sql).execute(&dash).await.expect_err(sql);
            // Refused for the right, not for a mistake in the statement.
            let code = e.as_database_error().and_then(|d| d.code());
            assert_eq!(code.as_deref(), Some("42501"), "{sql}: {e}");
        }
        // What the dashboard does do still works as its role: staff proof, an edit, a removal
        // (which clears the older image's proof column), closing the warns, a staff action line,
        // a report marked handled, a raider report's evidence, and the row locks its routes take.
        sqlx::query("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, proof) VALUES ($1, 1, 'warn', 7, 8, 'old')")
            .bind(gid)
            .execute(&owner)
            .await
            .unwrap();
        let by = (8, None);
        let added = crate::cases::add_proof(&dash, gid, 1, ("text", "seen"), by, None).await;
        assert_eq!(added.unwrap(), crate::cases::Added::Done);
        let mut conn = dash.acquire().await.unwrap();
        crate::cases::edit(&mut conn, gid, 1, Some(Some("why")), None, by)
            .await
            .unwrap();
        let id: i64 = sqlx::query_scalar("SELECT max(id) FROM case_proofs WHERE guild_id = $1")
            .bind(gid)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        let gone = crate::cases::remove_proof(&mut conn, gid, 1, id, by).await;
        assert_eq!(gone.unwrap(), Some(("text".into(), false)));
        for sql in [
            "SELECT active FROM cases WHERE guild_id = $1 FOR UPDATE",
            "SELECT status FROM reports WHERE guild_id = $1 FOR UPDATE",
            "UPDATE reports SET status = 'closed', handled_by = 8, handled_at = now() WHERE guild_id = $1",
            "INSERT INTO mod_actions (guild_id, mod_id, kind, source, target_id, case_no) VALUES ($1, 8, 'case_edit', 'dashboard', 7, 1) ON CONFLICT DO NOTHING",
            "INSERT INTO raider_evidence (user_id, source_guild, source_owner, kind, ref) VALUES (7, $1, 8, 'report', 'role test') ON CONFLICT DO NOTHING",
            "DELETE FROM cases WHERE guild_id = $1 AND case_no = 0",
        ] {
            let done = sqlx::query(sql).bind(gid).execute(&mut *conn).await;
            assert!(done.is_ok(), "{sql}: {done:?}");
        }
        drop(conn);
        crate::cases::reset_warns(&dash, gid, 7, 8).await.unwrap();
        // The bot's own role still stores the verified copy.
        let copy = crate::cases::MessageCopy {
            channel_id: 1,
            message_id: None,
            text: "said",
            files: vec![],
        };
        let kept = crate::cases::add_proof(&owner, gid, 1, ("text", "said"), by, Some(&copy)).await;
        assert_eq!(kept.unwrap(), crate::cases::Added::Done);
        // A rollback: an older image grants every row right again at its start. Migration 4043's
        // triggers still refuse the same writes, row by row, so each statement here meets a row.
        for sql in [
            "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, message_id, content) VALUES ($1, 7, 9, 1, 1, 'r')",
            "INSERT INTO raid_log (guild_id, raid_ms, user_id, name) VALUES ($1, 1, 7, 'n')",
            "INSERT INTO joingate_actions (guild_id, user_id, reason, action, ok) VALUES ($1, 7, 'r', 'kick', true)",
            "INSERT INTO nuke_incidents (guild_id, case_id, data) VALUES ($1, 'c', '{}')",
            "INSERT INTO profile_snapshot (guild_id, user_id, username) VALUES ($1, 7, 'n')",
            "WITH n AS (INSERT INTO notes (guild_id, user_id, author_id, text) VALUES ($1, 7, 8, 'a') RETURNING id)
             INSERT INTO note_edits (note_id, guild_id, old_text, new_text, editor_id) SELECT id, $1, 'a', 'b', 8 FROM n",
            "INSERT INTO raider_evidence (user_id, source_guild, source_owner, kind, ref) VALUES (7, $1, 8, 'report', 'rollback')",
        ] {
            sqlx::query(sql).bind(gid).execute(&owner).await.expect(sql);
        }
        let all = format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {role}"
        );
        sqlx::query(sqlx::AssertSqlSafe(all))
            .execute(&owner)
            .await
            .unwrap();
        for sql in [
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, verified) VALUES ($1, 1, 'text', 'x', 1, true)",
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, message_id) VALUES ($1, 1, 'text', 'x', 1, 1)",
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, files) VALUES ($1, 1, 'text', 'x', 1, '{a.png}')",
            "INSERT INTO case_proofs (id, guild_id, case_no, kind, body, added_by) VALUES (999999999, $1, 1, 'text', 'x', 1)",
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, added_at) VALUES ($1, 1, 'text', 'x', 1, now() - interval '1 day')",
            "UPDATE case_proofs SET verified = true WHERE guild_id = $1",
            "UPDATE case_proofs SET body = 'x' WHERE guild_id = $1",
            "UPDATE case_edits SET old_value = 'x' WHERE guild_id = $1",
            "UPDATE note_edits SET old_text = 'x' WHERE guild_id = $1",
            "INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id) VALUES ($1, 2, 'warn', 1, 1)",
            "UPDATE cases SET user_id = 1 WHERE guild_id = $1",
            "UPDATE cases SET kind = 'ban' WHERE guild_id = $1",
            "UPDATE cases SET created_at = now() WHERE guild_id = $1",
            "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, message_id, content) VALUES ($1, 1, 1, 1, 1, 'x')",
            "UPDATE reports SET content = 'x' WHERE guild_id = $1",
            "UPDATE reports SET target_id = 1 WHERE guild_id = $1",
            "UPDATE reports SET message_id = 2 WHERE guild_id = $1",
            "UPDATE raid_log SET name = 'x' WHERE guild_id = $1",
            "UPDATE joingate_actions SET reason = 'x' WHERE guild_id = $1",
            "UPDATE nuke_incidents SET data = '{\"a\":1}' WHERE guild_id = $1",
            "INSERT INTO raid_log (guild_id, raid_ms, user_id, name) VALUES ($1, 2, 7, 'n')",
            "INSERT INTO joingate_actions (guild_id, user_id, reason, action, ok) VALUES ($1, 7, 'r', 'kick', true)",
            "INSERT INTO nuke_incidents (guild_id, case_id, data) VALUES ($1, 'd', '{}')",
            "INSERT INTO profile_snapshot (guild_id, user_id, username) VALUES ($1, 8, 'n')",
            "UPDATE profile_snapshot SET username = 'x' WHERE guild_id = $1",
            "UPDATE mod_actions SET mod_id = 1 WHERE guild_id = $1",
            "UPDATE raider_evidence SET user_id = 1 WHERE source_guild = $1",
            "DELETE FROM case_edits WHERE guild_id = $1",
            "DELETE FROM note_edits WHERE guild_id = $1",
            "DELETE FROM mod_actions WHERE guild_id = $1",
            "DELETE FROM raider_evidence WHERE source_guild = $1",
            "INSERT INTO mod_actions (id, guild_id, mod_id, kind, source) VALUES (999999999, $1, 1, 'x', 'dashboard')",
            "INSERT INTO mod_actions (guild_id, mod_id, kind, source, created_at) VALUES ($1, 1, 'x', 'dashboard', now() - interval '1 day')",
            "INSERT INTO case_edits (guild_id, case_no, field, editor_id, edited_at) VALUES ($1, 1, 'reason', 1, now() - interval '1 day')",
            "INSERT INTO raider_evidence (id, user_id, source_guild, source_owner, kind, ref) VALUES (999999999, 7, $1, 8, 'report', 'id')",
        ] {
            let e = sqlx::query(sql).bind(gid).execute(&dash).await.expect_err(sql);
            let code = e.as_database_error().and_then(|d| d.code());
            assert_eq!(code.as_deref(), Some("42501"), "{sql}: {e}");
        }
        // What older dashboard images write still works: the #357 image names every proof column,
        // and an image from before migration 4037 sets and clears `cases.proof` (its copy trigger
        // then adds and removes the staff proof as the dashboard).
        for sql in [
            "INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, ai_client, verified, channel_id, message_id, sent_at, files)
             VALUES ($1, 1, 'text', 'all columns', 8, NULL, false, NULL, NULL, NULL, '{}')",
            "UPDATE cases SET proof = 'older image' WHERE guild_id = $1 AND case_no = 1",
            "UPDATE cases SET proof = NULL, reason = 'r', active = true WHERE guild_id = $1 AND case_no = 1",
            "UPDATE reports SET status = 'open', handled_by = NULL, handled_at = NULL WHERE guild_id = $1",
            "INSERT INTO mod_actions (guild_id, mod_id, kind, source, target_id, case_no) VALUES ($1, 8, 'case_edit', 'dashboard', 7, 1)",
            "INSERT INTO raider_evidence (user_id, source_guild, source_owner, kind, ref) VALUES (7, $1, 8, 'report', 'older image')",
            "INSERT INTO note_edits (note_id, guild_id, old_text, new_text, editor_id) SELECT id, $1, 'b', 'c', 8 FROM notes WHERE guild_id = $1",
            "INSERT INTO case_edits (guild_id, case_no, field, old_value, new_value, editor_id) VALUES ($1, 1, 'reason', 'a', 'b', 8)",
        ] {
            let done = sqlx::query(sql).bind(gid).execute(&dash).await;
            assert!(done.is_ok(), "{sql}: {done:?}");
        }
        crate::cases::reset_warns(&dash, gid, 7, 8).await.unwrap();
        set_dashboard_role(&owner, "dashtest1").await.unwrap();
        // Deleting a case or a note still removes its history lines (the foreign key runs as the owner).
        for sql in [
            "DELETE FROM cases WHERE guild_id = $1 AND case_no = 1",
            "DELETE FROM notes WHERE guild_id = $1",
        ] {
            sqlx::query(sql).bind(gid).execute(&dash).await.expect(sql);
        }
        let (left,): (i64,) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM case_edits WHERE guild_id = $1) + (SELECT count(*) FROM note_edits WHERE guild_id = $1)",
        )
        .bind(gid)
        .fetch_one(&owner)
        .await
        .unwrap();
        assert_eq!(left, 0);
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1")
            .bind(gid)
            .execute(&dash)
            .await
            .unwrap();
        // It cannot touch the deletion log, but its server purge still writes the log line.
        purge_guild(&dash, gid).await.unwrap();
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM erasure_log WHERE tbl = 'purge_guild' AND key = jsonb_build_object('guild_id', $1::bigint)",
        )
        .bind(gid)
        .fetch_one(&owner)
        .await
        .unwrap();
        assert!(n >= 1);
        let (left,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM profile_snapshot WHERE guild_id = $1")
                .bind(gid)
                .fetch_one(&owner)
                .await
                .unwrap();
        assert_eq!(left, 0, "the purge takes the stored names and avatars");
    }

    /// Only the owner's purge and restore skip the deletion log (PR B security pass): the dashboard's
    /// role setting `brim.erasing` itself, for a session or as its own default, still logs its deletes.
    #[tokio::test]
    async fn only_the_owner_skips_the_erasure_log() {
        let Ok(url) = test_url() else { return };
        let _one = ROLE_TESTS.lock().await;
        let owner = connect(&url).await.unwrap();
        set_dashboard_role(&owner, "dashtest1").await.unwrap();
        let role = DASHBOARD_ROLE;
        let opts: PgConnectOptions = url.parse().unwrap();
        let dash = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone().username(role).password("dashtest1"))
            .await
            .unwrap();
        let gid = 5_200_000_000 + std::process::id() as i64;
        let logged = |user: i64| {
            let owner = owner.clone();
            async move {
                let (n,): (i64,) = sqlx::query_as(
                    "SELECT count(*) FROM erasure_log WHERE tbl = 'extra_owners' AND key = jsonb_build_object('guild_id', $1::bigint, 'user_id', $2::bigint)",
                )
                .bind(gid)
                .bind(user)
                .fetch_one(&owner)
                .await
                .unwrap();
                n
            }
        };
        sqlx::query(
            "INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, 1), ($1, 2), ($1, 3)",
        )
        .bind(gid)
        .execute(&owner)
        .await
        .unwrap();
        // The flag for one transaction.
        let mut tx = dash.begin().await.unwrap();
        sqlx::query("SET LOCAL brim.erasing = on")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM extra_owners WHERE guild_id = $1 AND user_id = 1")
            .bind(gid)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(logged(1).await, 1, "a flag the role set itself is ignored");
        // The owner-only marker is out of its reach.
        for sql in [
            "INSERT INTO brim_owner.erasing (xid) VALUES (pg_current_xact_id())",
            "SELECT brim_owner.erasing_on()",
            "SELECT note_erasure('extra_owners', '{}')",
        ] {
            let e = sqlx::query(sql).execute(&dash).await.expect_err(sql);
            let code = e.as_database_error().and_then(|d| d.code());
            assert_eq!(code.as_deref(), Some("42501"), "{sql}: {e}");
        }
        // The flag as its own lasting default: Postgres 16 refuses it to a role that is no
        // superuser. Any other default it sets itself is cleared at the bot's next start.
        let own = format!("ALTER ROLE {role} SET brim.erasing = on");
        let e = sqlx::query(sqlx::AssertSqlSafe(own))
            .execute(&dash)
            .await
            .unwrap_err();
        let code = e.as_database_error().and_then(|d| d.code());
        assert_eq!(code.as_deref(), Some("42501"), "{e}");
        let own = format!("ALTER ROLE {role} SET statement_timeout = '1h'");
        sqlx::query(sqlx::AssertSqlSafe(own))
            .execute(&dash)
            .await
            .unwrap();
        set_dashboard_role(&owner, "dashtest1").await.unwrap();
        let (left,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM pg_db_role_setting WHERE setrole = (SELECT oid FROM pg_roles WHERE rolname = $1)",
        )
        .bind(role)
        .fetch_one(&owner)
        .await
        .unwrap();
        assert_eq!(left, 0, "the bot's start clears the role's own defaults");
        // The owner's purge (called by the dashboard) still logs only itself, not each row.
        purge_guild(&dash, gid).await.unwrap();
        assert_eq!(logged(3).await, 0, "the purge's own deletes are not logged");
        // A purge of another server in the same transaction skips only that server's rows.
        sqlx::query("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, 5)")
            .bind(gid)
            .execute(&owner)
            .await
            .unwrap();
        let mut tx = dash.begin().await.unwrap();
        sqlx::query("SELECT purge_guild($1)")
            .bind(gid + 1)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM extra_owners WHERE guild_id = $1 AND user_id = 5")
            .bind(gid)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            logged(5).await,
            1,
            "another server's purge does not hide this delete"
        );
        // On the Brim names the old roles cannot log in, also after an older image made them.
        {
            sqlx::query("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tommybot_dashboard') THEN CREATE ROLE tommybot_dashboard; END IF; END $$")
                .execute(&owner)
                .await
                .unwrap();
            sqlx::query("ALTER ROLE tommybot_dashboard LOGIN PASSWORD 'old'")
                .execute(&owner)
                .await
                .unwrap();
            lock_old_roles(&owner).await.unwrap();
            let (can,): (bool,) = sqlx::query_as("SELECT bool_or(rolcanlogin) FROM pg_roles WHERE rolname IN ('tommybot_dashboard', 'tommybot_mcp')")
                .fetch_one(&owner)
                .await
                .unwrap();
            assert!(!can, "an old role can still log in");
        }
        // And the flag is gone with its transaction.
        sqlx::query("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, 4)")
            .bind(gid)
            .execute(&owner)
            .await
            .unwrap();
        sqlx::query("DELETE FROM extra_owners WHERE guild_id = $1 AND user_id = 4")
            .bind(gid)
            .execute(&dash)
            .await
            .unwrap();
        assert_eq!(logged(4).await, 1);
        sqlx::query(
            "DELETE FROM erasure_log WHERE key ->> 'guild_id' IN ($1::text, ($1 + 1)::text)",
        )
        .bind(gid)
        .execute(&owner)
        .await
        .unwrap();
    }
}

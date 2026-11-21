//! The MCP role against a real Postgres. Skipped unless DATABASE_URL is set.
use axum::body::Body;
use axum::http::Request;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use tower::ServiceExt;

const TOKEN: &str = "tbmcp_test-token-test-token-test-token-x";

async fn rpc(st: &brim_mcp::State, auth: &str, body: Value) -> (u16, Value) {
    let req = Request::post("/mcp")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = brim_mcp::router(st.clone()).oneshot(req).await.unwrap();
    let code = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn tool(st: &brim_mcp::State, name: &str, args: Value) -> Value {
    let (code, r) = rpc(st, &format!("Bearer {TOKEN}"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})).await;
    assert_eq!(code, 200);
    assert!(r["result"]["isError"].is_null(), "{name}: {r}");
    serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn read_only_role_and_tools() {
    let Ok(url) = brim_core::db::test_url() else {
        return;
    };
    let admin = brim_core::db::connect(&url).await.unwrap();
    let (_, role) = brim_core::db::roles(&admin).await.unwrap();
    brim_core::db::set_mcp_password(&admin, "mcp-test-pw")
        .await
        .unwrap();
    // A restored dump has no `ALTER ROLE ... SET`, and the role can turn its own read-only default
    // off: set_mcp_password must put them back.
    for sql in [
        format!("ALTER ROLE {role} RESET ALL"),
        format!("ALTER ROLE {role} SET default_transaction_read_only = off"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&admin)
            .await
            .unwrap();
    }
    brim_core::db::set_mcp_password(&admin, "mcp-test-pw")
        .await
        .unwrap();
    let g = 8_000_000_000 + std::process::id() as i64;
    sqlx::query("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, reason) VALUES ($1, 1, 'warn', 42, 7, 'spam')")
        .bind(g).execute(&admin).await.unwrap();
    sqlx::query("INSERT INTO notes (guild_id, user_id, author_id, text) VALUES ($1, 42, 7, 'watch this one')")
        .bind(g)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO action_log (guild_id, actor_id, action, target_id) VALUES ($1, 7, 'ban', 42)",
    )
    .bind(g)
    .execute(&admin)
    .await
    .unwrap();
    sqlx::query("INSERT INTO mod_actions (guild_id, mod_id, kind, source, target_id) VALUES ($1, 7, 'kick', 'discord', 42)")
        .bind(g).execute(&admin).await.unwrap();
    sqlx::query("INSERT INTO guild_config (guild_id, version, config) VALUES ($1, 1, '{\"automod\": {\"on\": true}}')")
        .bind(g).execute(&admin).await.unwrap();

    let opts = PgConnectOptions::from_str(&url)
        .unwrap()
        .username(role)
        .password("mcp-test-pw");
    let db = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();

    let ro: String = sqlx::query_scalar("SHOW default_transaction_read_only")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(ro, "on");

    // The role sees only the views and cannot write.
    for sql in [
        "SELECT 1 FROM cases LIMIT 1",
        "SELECT 1 FROM dash_session LIMIT 1",
        "SELECT 1 FROM dash_totp LIMIT 1",
        "SELECT content FROM reports LIMIT 1",
        "SELECT answers FROM appeals LIMIT 1",
        "SELECT 1 FROM mcp_writes LIMIT 1",
        "SELECT 1 FROM mcp_tokens LIMIT 1",
        "SELECT 1 FROM mod_actions LIMIT 1",
        "SELECT 1 FROM mcp_reads LIMIT 1",
        "SELECT 1 FROM mcp_nsfw_scans LIMIT 1",
        // Only through a read-write transaction, as the server's read log does.
        "INSERT INTO mcp_reads (guild_id, tool, via) VALUES (1, 'x', 'y')",
    ] {
        assert!(
            sqlx::query(sql).execute(&db).await.is_err(),
            "{sql} was allowed"
        );
    }
    let del = sqlx::query("SELECT purge_guild($1)")
        .bind(g)
        .execute(&db)
        .await;
    assert!(del.is_err(), "purge_guild was allowed");

    // Logins are tokens made on the dashboard (stored as their SHA-256), no fixed token.
    use sha2::Digest;
    // A run that failed half way leaves its token behind.
    sqlx::query("DELETE FROM mcp_tokens WHERE token_hash = $1")
        .bind(sha2::Sha256::digest(TOKEN).to_vec())
        .execute(&admin)
        .await
        .unwrap();
    let tok_id: i64 = sqlx::query_scalar("INSERT INTO mcp_tokens (name, token_hash, created_by) VALUES ('mcp-test', $1, 7) RETURNING id")
        .bind(sha2::Sha256::digest(TOKEN).to_vec())
        .fetch_one(&admin)
        .await
        .unwrap();
    let st = brim_mcp::State {
        db,
        log_dir: "/nonexistent".into(),
        oauth: None,
        writes: Some(brim_mcp::writes::Writes {
            base: "http://127.0.0.1:1".into(),
            http: reqwest::Client::new(),
        }),
    };
    let (_, list) = rpc(
        &st,
        &format!("Bearer {TOKEN}"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"propose_change") && !names.contains(&"nsfw_scans"));
    let (code, _) = rpc(
        &st,
        "Bearer wrong",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(code, 401);
    let (_, init) = rpc(&st, &format!("Bearer {TOKEN}"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

    let cases = tool(&st, "cases", json!({"guild_id": g.to_string()})).await;
    assert_eq!(cases[0]["reason"], "spam");
    assert_eq!(cases[0]["user_id"], "42");
    assert_eq!(
        tool(&st, "case", json!({"guild_id": g, "case_no": 1})).await["kind"],
        "warn"
    );
    assert_eq!(
        tool(
            &st,
            "guild_config",
            json!({"guild_id": g.to_string(), "section": "automod"})
        )
        .await["config"]["on"],
        true
    );
    assert_eq!(
        tool(&st, "config_history", json!({"guild_id": g.to_string()})).await[0]["version"],
        1
    );
    assert!(tool(&st, "guilds", json!({}))
        .await
        .as_array()
        .unwrap()
        .contains(&json!(g.to_string())));
    tool(&st, "status", json!({})).await;
    tool(&st, "raid_log", json!({"guild_id": g.to_string()})).await;
    tool(&st, "joingate_log", json!({"guild_id": g.to_string()})).await;
    for t in [
        "nuke_incidents",
        "quarantines",
        "strikes",
        "appeals",
        "reports",
        "backups",
    ] {
        tool(&st, t, json!({"guild_id": g.to_string()})).await;
    }
    tool(&st, "jobs", json!({})).await;
    sqlx::query("INSERT INTO raiders (user_id, status, category) VALUES ($1, 'pending', 'raid') ON CONFLICT DO NOTHING")
        .bind(g)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO raider_evidence (user_id, source_guild, source_owner, kind, ref, reporter_id) VALUES ($1, 1, 2, 'report', '1', 3)")
        .bind(g)
        .execute(&admin)
        .await
        .unwrap();
    let raiders = tool(&st, "raiders", json!({"status": "pending"})).await;
    let r = raiders
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["user_id"].as_str() == Some(g.to_string().as_str()))
        .unwrap();
    assert_eq!(
        (r["evidence"].as_i64(), r["owners"].as_i64()),
        (Some(1), Some(1))
    );
    assert!(r.get("reporter_id").is_none() && r.get("detail").is_none());
    assert!(sqlx::query("SELECT 1 FROM raider_evidence LIMIT 1")
        .execute(&st.db)
        .await
        .is_err());
    sqlx::query("DELETE FROM raider_evidence WHERE user_id = $1")
        .bind(g)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("DELETE FROM raiders WHERE user_id = $1")
        .bind(g)
        .execute(&admin)
        .await
        .unwrap();
    let notes = tool(
        &st,
        "notes",
        json!({"guild_id": g.to_string(), "user_id": "42"}),
    )
    .await;
    assert_eq!(notes[0]["text"], "watch this one");
    let acts = tool(
        &st,
        "action_log",
        json!({"guild_id": g.to_string(), "actor_id": "7"}),
    )
    .await;
    assert_eq!(acts[0]["action"], "ban");
    assert_eq!(acts[0]["target_id"], "42");

    let stats = tool(
        &st,
        "mod_actions",
        json!({"guild_id": g.to_string(), "mod_id": "7"}),
    )
    .await;
    assert_eq!(
        (stats[0]["kind"].as_str(), stats[0]["source"].as_str()),
        (Some("kick"), Some("discord"))
    );
    assert_eq!(
        tool(
            &st,
            "mod_actions",
            json!({"guild_id": g.to_string(), "mod_id": "8"})
        )
        .await,
        json!([])
    );

    // A guild id given to a tool that does not take one logs nothing.
    tool(&st, "status", json!({"guild_id": g.to_string()})).await;
    // Each read of this server left a row its staff see; reads of no server left none.
    let logged: Vec<(String, String)> =
        sqlx::query_as("SELECT tool, via FROM mcp_reads WHERE guild_id = $1 ORDER BY id")
            .bind(g)
            .fetch_all(&admin)
            .await
            .unwrap();
    assert_eq!(logged[0], ("cases".into(), "token mcp-test".into()));
    assert!(logged.iter().any(|(t, _)| t == "notes"));
    assert!(!logged
        .iter()
        .any(|(t, _)| t == "guilds" || t == "raiders" || t == "status"));
    // The AI sees no NSFW scan rows (owner decision 2026-11-17).
    let (_, r) = rpc(&st, &format!("Bearer {TOKEN}"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "nsfw_scans", "arguments": {"guild_id": g.to_string()}}})).await;
    assert_eq!(r["result"]["isError"], true);
    // A revoked token is refused at once.
    sqlx::query("UPDATE mcp_tokens SET revoked_at = now() WHERE id = $1")
        .bind(tok_id)
        .execute(&admin)
        .await
        .unwrap();
    let (code, _) = rpc(
        &st,
        &format!("Bearer {TOKEN}"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
    )
    .await;
    assert_eq!(code, 401);
    sqlx::query("DELETE FROM mcp_tokens WHERE id = $1")
        .bind(tok_id)
        .execute(&admin)
        .await
        .unwrap();

    for t in [
        "cases",
        "guild_config",
        "notes",
        "action_log",
        "mod_actions",
        "mcp_reads",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {t} WHERE guild_id = $1"
        )))
        .bind(g)
        .execute(&admin)
        .await
        .unwrap();
    }
}

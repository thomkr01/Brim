//! Secure my account (owner decision 2026-11-20): the bot owner's emergency button. One press logs out
//! every dashboard session, cancels a queued update, revokes every MCP token and closes the changes
//! that wait for a code. It needs the owner's dashboard 2FA step-up AND the Rescue Key of a server
//! the owner holds Brim's Owner level in, with that key's own authenticator code. The key is
//! never used up; the code cannot be used again (`rescue_key.last_step`). It never removes 2FA: a
//! thief holding the Discord account could then set it up first (PITFALLS, data delete 11-19).
use crate::{api::Res, auth::Sess, ops, ApiError, AppState};
use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::json;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/secure-account", get(status).post(press))
}

/// The checks before any key: bot owner (so nobody else can use up the owner's tries), then 2FA.
async fn owner(st: &AppState, s: &Sess) -> Result<(), ApiError> {
    if !crate::update::is_bot_owner(st, s).await {
        return Err(ApiError::Forbidden(
            "only the bot owner can use Secure my account",
        ));
    }
    Ok(())
}

/// A key that can press: it has an authenticator, it has not expired, and it was made after the
/// server's last change of owner (a rescue claim and a give-back are one too), so a previous
/// owner's key never works here, not even in its 7 days.
fn usable_sql() -> String {
    format!(
        "k.totp_secret is not null and not ({}) and not exists(select 1 from owner_handovers h
           where h.guild_id = k.guild_id and h.at > k.created_at)",
        brim_core::permits::rescue_expired_sql()
    )
}

/// Whether the press can work at all: the owner has dashboard 2FA and holds the Owner level of a
/// server whose Rescue Key can press. Never says which server.
async fn status(State(st): State<AppState>, s: Sess) -> Res {
    owner(&st, &s).await?;
    let guilds: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select guild_id from rescue_key k where {} and guild_id in (
           select guild_id from guild_owners where owner_id = $1
           union select server_id from rescue_owners where user_id = $1)",
        usable_sql()
    )))
    .bind(s.user_id)
    .fetch_all(&st.db)
    .await?;
    let mut has_key = false;
    for g in guilds {
        // Fail closed: a server Discord cannot answer for does not count.
        if matches!(crate::api::is_discord_owner(&st, &s, g).await, Ok(true)) {
            has_key = true;
            break;
        }
    }
    Ok(Json(
        json!({"has_key": has_key, "totp": totp_on(&st, &s).await?}),
    ))
}

async fn totp_on(st: &AppState, s: &Sess) -> Result<bool, ApiError> {
    Ok(
        sqlx::query_scalar::<_, bool>("select confirmed from dash_totp where user_id = $1")
            .bind(s.user_id)
            .fetch_optional(&st.db)
            .await?
            .unwrap_or(false),
    )
}

#[derive(Deserialize)]
pub(crate) struct Press {
    key: Option<String>,
    code: Option<String>,
}

const BAD: &str = "invalid rescue key or code";

async fn press(
    State(st): State<AppState>,
    s: Sess,
    jar: CookieJar,
    Json(b): Json<Press>,
) -> Result<impl IntoResponse, ApiError> {
    owner(&st, &s).await?;
    if !totp_on(&st, &s).await? {
        return Err(ApiError::Forbidden(
            "Secure my account needs dashboard 2FA: without it, use the SSH steps in DEPLOY.md section 9",
        ));
    }
    if !s.stepped_up {
        return Err(ApiError::StepUp);
    }
    // The claim's limit, counted apart: a claim needs no step-up, so whoever holds only the
    // Discord account could otherwise use up the owner's presses (security pass).
    if !ops::within_limit(&st.db, "press", s.user_id).await? {
        return Err(ApiError::Forbidden(
            "too many attempts, try again in 10 minutes",
        ));
    }
    let key = b.key.as_deref().unwrap_or_default().trim();
    let code = b.code.as_deref().unwrap_or_default().trim();
    // The key names its server; a wrong key touches no code counter.
    let guild: Option<i64> =
        sqlx::query_scalar("select guild_id from rescue_key where key_hash = $1")
            .bind(ops::hash(key))
            .fetch_optional(&st.db)
            .await?;
    let Some(guild) = guild else {
        return Err(ApiError::Forbidden(BAD));
    };
    // Brim's Owner level there now, as the rescue claim moves it (#412): the Discord owner, or
    // the account that claimed the key; never a previous or paused owner. Checked before the lock:
    // no Discord call under it.
    if !matches!(crate::api::is_discord_owner(&st, &s, guild).await, Ok(true)) {
        return Err(ApiError::Forbidden(
            "this rescue key is for a server where you do not hold Brim's Owner level",
        ));
    }
    let mut tx = st.db.begin().await?;
    // The config lock before the row, the order the claim and the bot's on_owner use.
    crate::store::locked(&mut tx, guild).await?;
    let row: Option<(Option<String>, bool, bool, Option<i64>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select totp_secret, {}, code_fails >= $3 and code_fails_since > now() - interval '1 hour', last_step
             from rescue_key k where guild_id = $1 and key_hash = $2 for update",
            usable_sql()
        )))
        .bind(guild)
        .bind(ops::hash(key))
        .bind(ops::MAX_CODE_FAILS)
        .fetch_optional(&mut *tx)
        .await?;
    // Replaced or claimed between the lookup and the lock.
    let Some((secret, usable, waiting, last_step)) = row else {
        return Err(ApiError::Forbidden(BAD));
    };
    let Some(secret) = secret.filter(|_| usable) else {
        return Err(ApiError::Forbidden(
            "this rescue key cannot be used here: it has no authenticator, it expired, or it was made before the server's last change of owner. Make a new one in Auto Setup, or use the SSH steps in DEPLOY.md section 9",
        ));
    };
    if waiting {
        return Err(ApiError::TooMany(
            "Too many wrong authenticator codes for this rescue key. Try again in an hour.".into(),
        ));
    }
    let step = crate::otp::step(&ops::unseal(key, &secret)?, code)?;
    // A code used before (by a press; a claim deletes the key) counts as wrong.
    let Some(step) = step.filter(|n| last_step.is_none_or(|l| (*n as i64) > l)) else {
        ops::count_code_fail(&mut tx, guild).await?;
        tx.commit().await?;
        return Err(ApiError::Forbidden(BAD));
    };
    // The precondition in the write: under the row lock, but kept for a reader who drops the lock.
    let fresh = sqlx::query(
        "update rescue_key set last_step = $2 where guild_id = $1 and (last_step is null or last_step < $2)",
    )
    .bind(guild)
    .bind(step as i64)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if fresh == 0 {
        return Err(ApiError::Forbidden(BAD));
    }
    let sessions = sqlx::query("delete from dash_session")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let updates = sqlx::query(
        "update update_runs set status = 'failed', error = 'Cancelled with Secure my account', finished_at = now()
         where status = 'queued'",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // A running update cannot be stopped safely; the page sends the owner to the Update page.
    let running: bool =
        sqlx::query_scalar("select exists(select 1 from update_runs where status = 'running')")
            .fetch_one(&mut *tx)
            .await?;
    let tokens = sqlx::query("update mcp_tokens set revoked_at = now() where revoked_at is null")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let writes = sqlx::query(
        "update mcp_writes set status = 'revoked', body = null where status = 'pending'",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let by = s.user_id;
    let mut what = format!(
        "🛡️ Secure my account was pressed on the dashboard by <@{by}> ({by}), with the Rescue Key of server {guild}. It logged out {sessions} dashboard sessions, cancelled {updates} queued updates, revoked {tokens} MCP tokens and closed {writes} changes waiting for a code."
    );
    if running {
        what.push_str(
            " An update was already running and could not be stopped: check the Update page.",
        );
    }
    crate::raiders::alert_owner(&mut *tx, by, "secure", &what).await?;
    tx.commit().await?;
    tracing::warn!(
        user = by,
        guild,
        sessions,
        updates,
        tokens,
        writes,
        running,
        "secure my account pressed"
    );
    Ok((
        jar.remove(crate::auth::cookie("sid", String::new(), st.secure)),
        Json(
            json!({"sessions": sessions, "updates": updates, "tokens": tokens, "writes": writes,
            "update_running": running, "guild": guild.to_string()}),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Path;

    /// Its own database: the press touches every session, token and update run, which the other
    /// DB tests running in parallel use.
    async fn own_db() -> Option<sqlx::PgPool> {
        let url = brim_core::db::test_url().ok()?;
        let (base, _) = url.split('?').next().unwrap().rsplit_once('/').unwrap();
        let admin = sqlx::PgPool::connect(&url).await.unwrap();
        let _ = sqlx::query("create database secure_account_test")
            .execute(&admin)
            .await;
        Some(
            brim_core::db::connect(&format!("{base}/secure_account_test"))
                .await
                .unwrap(),
        )
    }

    async fn state(db: sqlx::PgPool, owner: i64, g: i64) -> AppState {
        let mock = Router::new()
            .route(
                "/applications/@me",
                get(move || async move { Json(json!({"owner": {"id": owner.to_string()}})) }),
            )
            // The owner holds server g only.
            .route(
                "/guilds/{g}",
                get(move |Path(x): Path<i64>| async move {
                    Json(json!({"owner_id": if x == g { owner } else { 1 }.to_string()}))
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, mock).await });
        AppState {
            db,
            discord: crate::discord::Discord {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                client_id: String::new(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                api,
                memo: Default::default(),
                limits: Default::default(),
            },
            secure: false,
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        }
    }

    fn sess(user: i64, stepped_up: bool) -> Sess {
        Sess {
            id: String::new(),
            user_id: user,
            username: "t".into(),
            access_token: String::new(),
            stepped_up,
            step_up_guild: None,
            csrf: String::new(),
        }
    }

    /// The press's answer: (status, body).
    async fn go(st: &AppState, s: Sess, key: &str, code: &str) -> (u16, Value) {
        let b = Press {
            key: Some(key.into()),
            code: Some(code.into()),
        };
        let r = match press(State(st.clone()), s, CookieJar::new(), Json(b)).await {
            Ok(r) => r.into_response(),
            Err(e) => e.into_response(),
        };
        let code = r.status().as_u16();
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (code, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    use serde_json::Value;

    #[tokio::test]
    async fn the_press_secures_everything_once_and_keeps_the_key() {
        let Some(db) = own_db().await else { return };
        let (owner, g, other) = (77, 9_700_000_001_i64, 9_700_000_002_i64);
        let st = state(db.clone(), owner, g).await;
        let q = |sql: &'static str| sqlx::query(sql);
        for sql in [
            "delete from dash_session",
            "delete from dash_totp",
            "delete from rescue_key",
            "delete from owner_handovers",
            "delete from guild_owners",
            "delete from scheduled_jobs",
            "delete from mcp_writes",
            "delete from mcp_tokens",
            "delete from update_runs",
        ] {
            q(sql).execute(&db).await.unwrap();
        }
        let secret = crate::otp::new_secret();
        for (guild, key) in [(g, "k"), (other, "o")] {
            sqlx::query(
                "insert into rescue_key (guild_id, key_hash, totp_secret) values ($1, $2, $3)",
            )
            .bind(guild)
            .bind(ops::hash(key))
            .bind(&secret)
            .execute(&db)
            .await
            .unwrap();
        }
        let fails = || async {
            sqlx::query_scalar::<_, i32>("select code_fails from rescue_key where guild_id = $1")
                .bind(g)
                .fetch_one(&db)
                .await
                .unwrap()
        };
        let code = crate::otp::current(&secret);
        q("delete from dash_fails where kind = 'press'")
            .execute(&db)
            .await
            .unwrap();
        // Claims used up by whoever holds only the Discord account do not block the press.
        for _ in 0..6 {
            ops::within_limit(&db, "claim", owner).await.unwrap();
        }
        // Not the bot owner: refused before anything, no try used.
        assert_eq!(go(&st, sess(owner + 1, true), "k", &code).await.0, 403);
        // No dashboard 2FA: off.
        assert_eq!(go(&st, sess(owner, true), "k", &code).await.0, 403);
        q("insert into dash_totp (user_id, secret, confirmed) values (77, 'x', true)")
            .execute(&db)
            .await
            .unwrap();
        let (c, b) = go(&st, sess(owner, false), "k", &code).await;
        assert_eq!((c, b["error"].as_str()), (403, Some("step_up_required")));
        sqlx::query("insert into guild_owners (guild_id, owner_id) values ($1, 77)")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        let Json(v) = status(State(st.clone()), sess(owner, false)).await.unwrap();
        assert_eq!(
            (v["has_key"].as_bool(), v["totp"].as_bool()),
            (Some(true), Some(true))
        );
        // A wrong key counts no code miss; the key of a server the owner does not hold is refused.
        assert_eq!(go(&st, sess(owner, true), "nope", &code).await.0, 403);
        assert_eq!(go(&st, sess(owner, true), "o", &code).await.0, 403);
        assert_eq!(fails().await, 0);
        // A wrong code counts on the key.
        assert_eq!(go(&st, sess(owner, true), "k", "000000").await.0, 403);
        assert_eq!(fails().await, 1);
        // Things to secure: two sessions, a queued update, a live token with a pending change.
        q("insert into dash_session (id, user_id, username, access_token, csrf, expires_at) values ('a', 77, 't', 't', 'c', now() + interval '1 day'), ('b', 5, 't', 't', 'c', now() + interval '1 day')")
            .execute(&db).await.unwrap();
        q("insert into update_runs (requested_by) values (77)")
            .execute(&db)
            .await
            .unwrap();
        q("insert into mcp_tokens (id, name, token_hash, created_by) values (1, 'x', '\\x01', 77)")
            .execute(&db).await.unwrap();
        q("insert into mcp_writes (token_id, client, guild_id, action, code_hash, expires_at) values (1, 'c', 1, 'a', '\\x01', now() + interval '1 hour')")
            .execute(&db).await.unwrap();
        let (c, b) = go(&st, sess(owner, true), "k", &code).await;
        assert_eq!(c, 200, "{b}");
        assert_eq!(
            (
                b["sessions"].as_u64(),
                b["updates"].as_u64(),
                b["tokens"].as_u64(),
                b["writes"].as_u64()
            ),
            (Some(2), Some(1), Some(1), Some(1))
        );
        let left: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "select (select count(*) from dash_session),
                    (select count(*) from update_runs where status = 'queued'),
                    (select count(*) from mcp_tokens where revoked_at is null),
                    (select count(*) from mcp_writes where status = 'pending'),
                    (select count(*) from scheduled_jobs where kind = 'raider_notify' and payload->>'fixed' = 'secure')",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(left, (0, 0, 0, 0, 1));
        // The key stays; the same code cannot press again, and counts as a miss.
        let (c, _) = go(&st, sess(owner, true), "k", &code).await;
        assert_eq!(c, 403);
        assert_eq!(fails().await, 2);
        // The press shares the claim's per-user limit (5 in 10 minutes).
        q("delete from dash_fails where kind = 'press'")
            .execute(&db)
            .await
            .unwrap();
        // A key made before the server's last change of owner never presses.
        q("update rescue_key set last_step = null where guild_id = 9700000001")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into owner_handovers (guild_id, old_owner, new_owner, at) values ($1, 1, 77, now() + interval '1 second')")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        let (c, b) = go(&st, sess(owner, true), "k", &code).await;
        assert_eq!(c, 403, "{b}");
        assert!(
            b["error"]
                .as_str()
                .unwrap_or_default()
                .contains("last change of owner"),
            "{b}"
        );
    }
}

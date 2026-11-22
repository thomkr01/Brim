use crate::{ApiError, AppState};
use axum::extract::ConnectInfo;
use axum::{
    extract::{FromRequestParts, Query, Request, State},
    http::{request::Parts, Method, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

const STEP_UP_MINUTES: i64 = 10;
/// A session nobody used for this long has ended.
const IDLE_HOURS: i32 = 24;
/// Wrong TOTP codes allowed per user within `TOTP_WINDOW_MINUTES`.
const TOTP_FAILS: i64 = 5;
const TOTP_WINDOW_MINUTES: i32 = 10;
/// Requests a session may make at once, refilled at `RATE` per second. Two page loads in a row
/// (the Tickets tab alone sends about 12) must fit, or the second page shows nothing.
pub(crate) const BURST: f64 = 40.0;
const RATE: f64 = 20.0;

pub use brim_core::ratelimit::Bucket;

/// The client address: the tunnel's `CF-Connecting-IP` (kept only from the tunnel, see [`edge_only`]),
/// else the connection's own address.
fn client(req: &Request) -> String {
    client_of(req.headers(), peer(req))
}

/// [`client`] from the headers (after [`edge_only`]) and the connection's address.
pub(crate) fn client_of(h: &axum::http::HeaderMap, peer: Option<IpAddr>) -> String {
    h.get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or_else(|| peer.map(|ip| ip.to_string()))
        .unwrap_or_default()
}

fn peer(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

/// 429 for a session that sends more than `BURST` requests at once (Discord's limit is the bot's, shared),
/// and for a client address that sends more than four times that, so requests without a login or with
/// a made-up session id are limited too (security review t-edge-unknown-rules).
pub async fn rate_limit(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let sid = CookieJar::from_headers(req.headers())
        .get("sid")
        .map(|c| c.value().to_string());
    let ok = brim_core::ratelimit::take(
        &st.rate,
        &format!("ip:{}", client(&req)),
        4.0 * BURST,
        4.0 * RATE,
    ) && sid
        .is_none_or(|sid| brim_core::ratelimit::take(&st.rate, &format!("sid:{sid}"), BURST, RATE));
    if !ok {
        return ApiError::TooMany(
            "You are sending requests too fast. Wait a second and try again.".into(),
        )
        .into_response();
    }
    next.run(req).await
}

/// The compose service of the Cloudflare Tunnel (DEPLOY.md 2): the only sender whose
/// `CF-Connecting-IP` is believed.
const EDGE_HOST: &str = "cloudflared";

/// The tunnel container's addresses, looked up again after 30 s (a restarted container gets a new one).
static EDGE: Mutex<Option<(Instant, Vec<IpAddr>)>> = Mutex::new(None);

async fn from_edge(ip: IpAddr) -> bool {
    let cached = EDGE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(at, _)| at.elapsed() < StdDuration::from_secs(30))
        .map(|(_, ips)| ips.clone());
    let ips = match cached {
        Some(ips) => ips,
        None => {
            let ips: Vec<IpAddr> = tokio::net::lookup_host((EDGE_HOST, 0))
                .await
                .map(|a| a.map(|a| a.ip()).collect())
                .unwrap_or_default();
            // A failed lookup is not kept: the next request with the header asks again.
            if !ips.is_empty() {
                *EDGE.lock().unwrap() = Some((Instant::now(), ips.clone()));
            }
            ips
        }
    };
    ips.contains(&ip)
}

/// Drops `CF-Connecting-IP` unless the connection comes from the tunnel container. From the host or
/// another container the header is the sender's own claim, which would pass web verification's
/// alt-account and VPN checks with a made-up address (security review t-eo-ip-header-forgery).
pub async fn edge_only(mut req: Request, next: Next) -> Response {
    if req.headers().contains_key("cf-connecting-ip") {
        let trusted = match peer(&req) {
            Some(ip) => from_edge(ip).await,
            None => false,
        };
        if !trusted {
            req.headers_mut().remove("cf-connecting-ip");
        }
    }
    next.run(req).await
}

pub fn token() -> String {
    let mut b = [0u8; 32];
    rand::fill(&mut b);
    B64.encode(b)
}

/// Also the removal cookie (`jar.remove(cookie(name, ..))`): a removal needs the same `Path=/`, or the
/// browser applies it to `/auth` only and keeps the cookie.
pub(crate) fn cookie(name: &'static str, val: String, secure: bool) -> Cookie<'static> {
    Cookie::build((name, val))
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .path("/")
        .build()
}

/// A dashboard session. The MCP write path (`mcp.rs`) puts one in the request's extensions instead of a cookie.
#[derive(Clone)]
pub struct Sess {
    pub id: String,
    pub user_id: i64,
    pub username: String,
    pub access_token: String,
    /// A fresh step-up, wherever it was done: enough for the account pages (profile, update).
    pub stepped_up: bool,
    /// The server the step-up was done for; a server's routes accept it there only.
    pub step_up_guild: Option<i64>,
    pub csrf: String,
}

impl Sess {
    /// A fresh 2FA step-up done for `guild` (security review): one server, never all of them.
    pub fn stepped_up_for(&self, guild: i64) -> bool {
        self.stepped_up && self.step_up_guild == Some(guild)
    }
}

impl FromRequestParts<AppState> for Sess {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        // Only code in this process can set extensions; a client cannot.
        if let Some(s) = parts.extensions.get::<Sess>() {
            return Ok(s.clone());
        }
        let jar = CookieJar::from_headers(&parts.headers);
        let sid = jar
            .get("sid")
            .ok_or(ApiError::Unauthorized)?
            .value()
            .to_string();
        // A login lasts 7 days, and ends after 24 hours without use (owner decision, security review).
        #[allow(clippy::type_complexity)]
        let row: (i64, String, String, String, Option<DateTime<Utc>>, Option<i64>, bool) = sqlx::query_as(
            "select user_id, username, access_token, csrf, step_up_until, step_up_guild, last_seen_at < now() - interval '5 minutes'
             from dash_session where id = $1 and expires_at > now() and last_seen_at > now() - make_interval(hours => $2)",
        ).bind(&sid).bind(IDLE_HOURS).fetch_optional(&st.db).await?.ok_or(ApiError::Unauthorized)?;
        let (user_id, username, access_token, csrf, step_up, step_up_guild, stale) = row;
        // At most one write per 5 minutes per session, not one per request.
        if stale {
            sqlx::query("update dash_session set last_seen_at = now() where id = $1")
                .bind(&sid)
                .execute(&st.db)
                .await?;
        }
        let access_token = open_token(st, &sid, &access_token);
        // CSRF: every state-changing request must echo the per-session token.
        if !csrf_ok(
            &parts.method,
            parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok()),
            &csrf,
        ) {
            return Err(ApiError::Forbidden("bad csrf token"));
        }
        Ok(Sess {
            id: sid,
            user_id,
            username,
            access_token,
            stepped_up: step_up.is_some_and(|t| t > Utc::now()),
            step_up_guild,
            csrf,
        })
    }
}

/// Safe methods need no token; everything else must echo the session's.
fn csrf_ok(method: &Method, header: Option<&str>, csrf: &str) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) || header == Some(csrf)
}

#[derive(Deserialize)]
pub struct Login {
    next: Option<String>,
}

pub async fn login(
    State(st): State<AppState>,
    jar: CookieJar,
    Query(q): Query<Login>,
) -> impl IntoResponse {
    let state = token();
    let url = st.discord.authorize_url(&state);
    let mut jar = jar.add(cookie("oauth_state", state, st.secure));
    if let Some(n) = q
        .next
        .filter(|n| (n.starts_with("/verify/") || n.starts_with("/#/")) && !n.contains("//"))
    {
        jar = jar.add(cookie("login_next", n, st.secure));
    }
    (jar, Redirect::to(&url))
}

/// The front page's Invite bot button: the client id lives on the server, not in the web UI.
/// With `?guild_id=<digits>` the same invite has that server preselected; anything else is ignored.
pub async fn invite(
    State(st): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Redirect {
    match q
        .get("guild_id")
        .filter(|g| (1..=20).contains(&g.len()) && g.bytes().all(|b| b.is_ascii_digit()))
    {
        Some(g) => Redirect::to(&st.discord.invite_url_for(g)),
        None => Redirect::to(&st.discord.invite_url()),
    }
}

/// Discord sends `error` and no `code` when the person cancels on its consent screen.
#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    #[serde(default)]
    state: String,
}

/// A login that cannot finish gets a page that says why, not raw JSON (the person is not in the app yet).
fn login_failed(status: StatusCode, why: &str) -> Response {
    let page = format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Login failed</title><p>{why}</p><p><a href=\"/auth/login\">Log in again</a></p>");
    (status, Html(page)).into_response()
}

pub async fn callback(
    State(st): State<AppState>,
    jar: CookieJar,
    Query(q): Query<Callback>,
) -> Result<Response, ApiError> {
    let Some(code) = q.code else {
        return Ok(login_failed(
            StatusCode::BAD_REQUEST,
            "The login was cancelled on Discord.",
        ));
    };
    if jar.get("oauth_state").map(|c| c.value()) != Some(&q.state) {
        tracing::warn!("dashboard login refused: oauth state mismatch");
        return Ok(login_failed(
            StatusCode::FORBIDDEN,
            "This login expired, or another login was started in a different tab.",
        ));
    }
    let tok = match st.discord.exchange(&code).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(?e, "dashboard login: the code exchange failed");
            return Ok(login_failed(
                StatusCode::BAD_REQUEST,
                "Discord did not accept this login: it was already used or has expired.",
            ));
        }
    };
    let me = match st.discord.me(&tok).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(?e, "dashboard login: reading the Discord user failed");
            return Ok(login_failed(
                StatusCode::BAD_GATEWAY,
                "Discord did not answer. Try to log in again in a minute.",
            ));
        }
    };
    tracing::info!(user = %me.id, "dashboard login");
    // A new session id at each login; the browser's old session ends, so a planted id never gets a login.
    if let Some(old) = jar.get("sid") {
        sqlx::query("delete from dash_session where id = $1")
            .bind(old.value())
            .execute(&st.db)
            .await?;
    }
    let (sid, csrf) = (token(), token());
    sqlx::query("insert into dash_session (id, user_id, username, access_token, csrf, expires_at) values ($1,$2,$3,$4,$5,$6)")
        .bind(&sid).bind(me.id.parse::<i64>().map_err(anyhow::Error::from)?).bind(me.username).bind(seal_token(&st, &sid, &tok)).bind(csrf)
        .bind(Utc::now() + Duration::days(7)).execute(&st.db).await?;
    let next = jar
        .get("login_next")
        .map_or("/".into(), |c| c.value().to_string());
    Ok((
        jar.remove(cookie("oauth_state", String::new(), st.secure))
            .remove(cookie("login_next", String::new(), st.secure))
            .add(cookie("sid", sid, st.secure)),
        Redirect::to(&next),
    )
        .into_response())
}

/// The session user's servers. When Discord refuses the session's token (the person removed the
/// app or the token expired), the session ends and the answer is 401, so the dashboard asks for a
/// new login instead of showing an internal error.
pub(crate) async fn user_guilds(
    st: &AppState,
    s: &Sess,
) -> Result<Vec<crate::discord::Guild>, ApiError> {
    if let Some(g) = st.discord.user_guilds(&s.access_token).await? {
        return Ok(g);
    }
    sqlx::query("delete from dash_session where id = $1")
        .bind(&s.id)
        .execute(&st.db)
        .await?;
    Err(ApiError::Unauthorized)
}

pub async fn logout(
    State(st): State<AppState>,
    s: Sess,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    sqlx::query("delete from dash_session where id = $1")
        .bind(&s.id)
        .execute(&st.db)
        .await?;
    Ok((
        jar.remove(cookie("sid", String::new(), st.secure)),
        Json(json!({"ok": true})),
    ))
}

pub async fn me(State(st): State<AppState>, s: Sess) -> Result<Json<serde_json::Value>, ApiError> {
    let csrf = sqlx::query_scalar::<_, String>("select csrf from dash_session where id = $1")
        .bind(&s.id)
        .fetch_one(&st.db)
        .await?;
    let enrolled =
        sqlx::query_scalar::<_, bool>("select confirmed from dash_totp where user_id = $1")
            .bind(s.user_id)
            .fetch_optional(&st.db)
            .await?
            .unwrap_or(false);
    Ok(Json(
        json!({"user_id": s.user_id.to_string(), "username": s.username, "csrf": csrf, "totp_enrolled": enrolled, "stepped_up": s.stepped_up,
            "step_up_guild": s.step_up_guild.filter(|_| s.stepped_up).map(|g| g.to_string()),
            "bot_owner": crate::update::is_bot_owner(&st, &s).await,
            "support_url": std::env::var("SUPPORT_URL").ok().filter(|u| !u.is_empty())}),
    ))
}

/// Starts (or restarts, until confirmed) enrollment; returns the otpauth URI to render as a QR code.
pub async fn totp_enroll(
    State(st): State<AppState>,
    s: Sess,
) -> Result<Json<serde_json::Value>, ApiError> {
    if sqlx::query_scalar::<_, bool>("select confirmed from dash_totp where user_id = $1")
        .bind(s.user_id)
        .fetch_optional(&st.db)
        .await?
        .unwrap_or(false)
    {
        return Err(ApiError::Forbidden(ALREADY_SET_UP));
    }
    let secret = crate::otp::new_secret();
    let dm_code = format!("{:06}", rand::random_range(0..1_000_000));
    // The code goes to the logged-in Discord account, which a stolen session cookie can't read.
    st.discord
        .dm(
            &st.db,
            None,
            s.user_id,
            "dm.dashboard_2fa",
            &[("code", &dm_code)],
        )
        .await
        .map_err(|e| {
            // Discord answers 403 (50007) only when it cannot open the DM: closed DMs or no shared server.
            let closed = e
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::status)
                == Some(StatusCode::FORBIDDEN);
            ApiError::BadRequest(if closed {
                "couldn't DM you; allow DMs from server members and try again".into()
            } else {
                "Discord did not accept the DM right now; try again in a minute".into()
            })
        })?;
    // The check above ran before the slow DM: a confirm in between must not get its secret replaced.
    let wrote = sqlx::query("insert into dash_totp (user_id, secret, dm_code) values ($1,$2,$3) on conflict (user_id) do update set secret = $2, dm_code = $3 where not dash_totp.confirmed")
        .bind(s.user_id).bind(&secret).bind(&dm_code).execute(&st.db).await?.rows_affected();
    if wrote == 0 {
        return Err(ApiError::Forbidden(ALREADY_SET_UP));
    }
    Ok(Json(
        json!({"secret": secret, "uri": crate::otp::uri(&secret, "Brim", &s.username)?}),
    ))
}

#[derive(Deserialize)]
pub struct Code {
    code: String,
    /// The code from the bot's DM; only needed to confirm a new enrollment.
    #[serde(default)]
    dm_code: String,
    /// The server page the prompt was opened on; the step-up counts there only. None: an account
    /// page, where no server action accepts it.
    #[serde(default)]
    guild: Option<String>,
}

/// The 2FA refusals the person reads: each says what to do, not an internal state (PITFALLS "A 2FA refusal showed a server state").
pub const WRONG_CODE: &str = "Wrong code. Enter the newest 6-digit code from the Brim entry in your authenticator app; when you are setting it up, also check the code Brim sent you in Discord DMs.";
const NOT_SET_UP: &str = "You have not set up an authenticator yet. Press Set up authenticator, scan the QR code, then enter a code.";
const ALREADY_SET_UP: &str = "Your authenticator is already set up. Enter a code from the Brim entry in your authenticator app.";
const NOT_SET_UP_MCP: &str = "You have not set up an authenticator yet. Set it up in the dashboard (Confirm 2FA in the account menu) first.";

/// Confirms enrollment on first use, and grants step-up for STEP_UP_MINUTES.
pub async fn totp_verify(
    State(st): State<AppState>,
    s: Sess,
    jar: CookieJar,
    Json(c): Json<Code>,
) -> Result<(CookieJar, Json<serde_json::Value>), ApiError> {
    let (secret, confirmed, dm_code) = sqlx::query_as::<_, (String, bool, Option<String>)>(
        "select secret, confirmed, dm_code from dash_totp where user_id = $1",
    )
    .bind(s.user_id)
    .fetch_optional(&st.db)
    .await?
    .ok_or(ApiError::Forbidden(NOT_SET_UP))?;
    let guild = c
        .guild
        .as_deref()
        .map(|g| g.parse::<i64>())
        .transpose()
        .map_err(|_| {
            ApiError::BadRequest("the server id of this 2FA step is not a number".into())
        })?;
    let step = crate::otp::step(&secret, &c.code)?;
    // A wrong DM code counts as a miss, so it shares the TOTP lockout.
    let step = step.filter(|_| confirmed || dm_code.as_deref() == Some(c.dm_code.as_str()));
    attempt(&st, s.user_id, step, false).await?;
    // Only the secret just checked: a new enrollment that landed meanwhile must not be confirmed by this code.
    let kept = sqlx::query(
        "update dash_totp set confirmed = true, dm_code = null where user_id = $1 and secret = $2",
    )
    .bind(s.user_id)
    .bind(&secret)
    .execute(&st.db)
    .await?
    .rows_affected();
    if kept == 0 {
        return Err(ApiError::Busy(
            "your authenticator setup was restarted meanwhile; scan the new code and try again",
        ));
    }
    // The step-up goes to a new session id: a copy of the old cookie taken before never gets it.
    // The old id lives 30 s more without the step-up, for the page's requests already on the way.
    let new = token();
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "insert into dash_session (id, user_id, username, access_token, csrf, expires_at, step_up_until, step_up_guild)
         select $2, user_id, username, $3, csrf, expires_at, $4, $5 from dash_session where id = $1",
    )
    .bind(&s.id)
    .bind(&new)
    .bind(seal_token(&st, &new, &s.access_token))
    .bind(Utc::now() + Duration::minutes(STEP_UP_MINUTES))
    .bind(guild)
    .execute(&mut *tx)
    .await?;
    sqlx::query("update dash_session set step_up_until = null, expires_at = least(expires_at, now() + interval '30 seconds') where id = $1")
        .bind(&s.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        jar.add(cookie("sid", new, st.secure)),
        Json(json!({"stepped_up": true})),
    ))
}

/// The Discord OAuth token as stored: XOR a key stream from the app's client secret and the session
/// id, so a copy of the session table alone does not give the token (security review t-ds-session-scope).
/// The session id is random and new per row, so no stream is used twice.
pub(crate) fn seal_token(st: &AppState, sid: &str, tok: &str) -> String {
    let b: String = xor_stream(st, sid, tok.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("v1:{b}")
}

/// The token from [`seal_token`]. A row from before sealing holds the token as it is.
fn open_token(st: &AppState, sid: &str, stored: &str) -> String {
    let Some(hex) = stored.strip_prefix("v1:") else {
        return stored.to_string();
    };
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok())
        .collect();
    // A wrong secret gives a token Discord refuses, which ends the session (`user_guilds`).
    String::from_utf8_lossy(&xor_stream(st, sid, &bytes)).into_owned()
}

fn xor_stream(st: &AppState, sid: &str, data: &[u8]) -> Vec<u8> {
    let block = |n: usize| {
        let mut m = Hmac::<Sha256>::new_from_slice(st.discord.client_secret.as_bytes())
            .expect("HMAC takes any key length");
        m.update(format!("dash-session-token:{sid}:{n}").as_bytes());
        m.finalize().into_bytes()
    };
    data.chunks(32)
        .enumerate()
        .flat_map(|(n, c)| {
            let k = block(n);
            c.iter().zip(k).map(|(a, b)| a ^ b).collect::<Vec<_>>()
        })
        .collect()
}

/// Counts a try of `kind` by `user` in the database, so a dashboard restart keeps it, and says how
/// many such tries came in the last `minutes`, this one included. `add` false only counts.
/// The advisory lock makes parallel tries of one counter wait, so none passes the limit unseen.
pub(crate) async fn tries(
    tx: &mut sqlx::PgConnection,
    (kind, user): (&str, i64),
    minutes: i32,
    add: bool,
) -> Result<i64, ApiError> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1 || ':' || $2, 0))")
        .bind(kind)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    if add {
        sqlx::query("insert into dash_fails (kind, user_id) values ($1, $2)")
            .bind(kind)
            .bind(user)
            .execute(&mut *tx)
            .await?;
    }
    Ok(sqlx::query_scalar(
        "select count(*) from dash_fails where kind = $1 and user_id = $2 and at > now() - make_interval(mins => $3)",
    )
    .bind(kind)
    .bind(user)
    .bind(minutes)
    .fetch_one(&mut *tx)
    .await?)
}

/// Deletes tries older than any window, outside the counters' locks (no lock waits between users).
pub(crate) async fn prune_tries(db: &sqlx::PgPool) {
    if let Err(e) = sqlx::query("delete from dash_fails where at < now() - interval '1 hour'")
        .execute(db)
        .await
    {
        tracing::warn!(?e, "old dashboard tries not deleted");
    }
}

/// Counts a TOTP attempt: refuses while locked out, and refuses a miss or a replayed step. Misses
/// sent through MCP have their own lockout, so an MCP client cannot lock the owner out of the
/// dashboard. The replay guard (`dash_totp.last_step`) is shared: a code used on one path is spent on both.
async fn attempt(st: &AppState, user: i64, step: Option<u64>, mcp: bool) -> Result<(), ApiError> {
    let key = (if mcp { "totp-mcp" } else { "totp" }, user);
    let mut tx = st.db.begin().await?;
    if tries(&mut tx, key, TOTP_WINDOW_MINUTES, false).await? >= TOTP_FAILS {
        tracing::warn!(user, mcp, "dashboard 2FA locked after too many wrong codes");
        let mins = TOTP_WINDOW_MINUTES;
        return Err(ApiError::TooMany(if mcp {
            format!("Too many wrong authenticator codes through MCP. MCP changes that need one are locked for up to {mins} minutes; the dashboard is not locked.")
        } else {
            format!("Too many wrong codes. You are locked out for up to {mins} minutes.")
        }));
    }
    let fresh = match step {
        Some(n) => {
            sqlx::query("update dash_totp set last_step = $2 where user_id = $1 and (last_step is null or last_step < $2)")
                .bind(user)
                .bind(n as i64)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                > 0
        }
        None => false,
    };
    if !fresh {
        tries(&mut tx, key, TOTP_WINDOW_MINUTES, true).await?;
    }
    tx.commit().await?;
    if !fresh {
        prune_tries(&st.db).await;
        tracing::warn!(user, mcp, "dashboard 2FA code rejected");
        return Err(ApiError::Forbidden(WRONG_CODE));
    }
    Ok(())
}

/// A code sent through MCP, from a confirmed enrollment: the replay guard of `totp_verify`, but its own
/// lockout (see [`attempt`]).
pub async fn check_totp(st: &AppState, user: i64, code: &str) -> Result<(), ApiError> {
    let secret = sqlx::query_scalar::<_, String>(
        "select secret from dash_totp where user_id = $1 and confirmed",
    )
    .bind(user)
    .fetch_optional(&st.db)
    .await?
    .ok_or(ApiError::Forbidden(NOT_SET_UP_MCP))?;
    attempt(st, user, crate::otp::step(&secret, code)?, true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only on a scratch Postgres. A login whose `state` is not the cookie's is refused before
    /// Discord is asked; an expired session or step-up no longer counts; logout ends the session.
    #[tokio::test]
    async fn login_state_session_expiry_step_up_and_logout() {
        let Some((st, _, _)) = crate::api::g3_handler_tests::setup(9_330_000_000).await else {
            return;
        };
        let q = |state: &str| {
            Query(Callback {
                code: Some("c".into()),
                state: state.into(),
            })
        };
        let jar = CookieJar::new().add(Cookie::new("oauth_state", "right"));
        let r = callback(State(st.clone()), jar.clone(), q("wrong")).await;
        assert_eq!(r.unwrap().status(), StatusCode::FORBIDDEN);
        let r = callback(State(st.clone()), CookieJar::new(), q("right")).await;
        assert_eq!(r.unwrap().status(), StatusCode::FORBIDDEN, "no cookie");

        let user = 9_340_000_000 + std::process::id() as i64;
        let add = |sid: String, expires: &'static str, step: &'static str| {
            let db = st.db.clone();
            async move {
                let sql = format!("insert into dash_session (id, user_id, username, access_token, csrf, expires_at, step_up_until) values ($1, $2, 't', 'tok', 'c', now() + interval '{expires}', now() + interval '{step}')");
                sqlx::query(sqlx::AssertSqlSafe(sql))
                    .bind(sid)
                    .bind(user)
                    .execute(&db)
                    .await
                    .unwrap();
            }
        };
        let sess = |sid: &str| {
            let (mut parts, _) = axum::http::Request::builder()
                .header("cookie", format!("sid={sid}"))
                .body(())
                .unwrap()
                .into_parts();
            let st = st.clone();
            async move { Sess::from_request_parts(&mut parts, &st).await }
        };
        let (live, old) = (format!("live-{user}"), format!("old-{user}"));
        add(live.clone(), "1 day", "-1 minute").await;
        add(old.clone(), "-1 minute", "1 hour").await;
        let s = sess(&live).await.unwrap();
        assert!(!s.stepped_up, "an expired step-up asks for 2FA again");
        // security review: a step-up counts for the server it was done for, and only there.
        let fresh = format!("fresh-{user}");
        add(fresh.clone(), "1 day", "5 minutes").await;
        sqlx::query("update dash_session set step_up_guild = 5 where id = $1")
            .bind(&fresh)
            .execute(&st.db)
            .await
            .unwrap();
        let f = sess(&fresh).await.unwrap();
        assert!(f.stepped_up_for(5) && !f.stepped_up_for(6));
        assert!(matches!(sess(&old).await, Err(ApiError::Unauthorized)));
        logout(State(st.clone()), s, CookieJar::new())
            .await
            .unwrap();
        assert!(matches!(sess(&live).await, Err(ApiError::Unauthorized)));
        sqlx::query("delete from dash_session where user_id = $1")
            .bind(user)
            .execute(&st.db)
            .await
            .unwrap();
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. Discord refuses the session's token:
    /// the answer is 401 (not 500) and the session is deleted, so the dashboard asks for a new login.
    #[tokio::test]
    async fn a_refused_discord_token_ends_the_session() {
        use axum::{http::StatusCode, routing::get, Router};
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let user = 9_481_000_000 + std::process::id() as i64;
        let sid = format!("refused-{user}");
        sqlx::query("insert into dash_session (id, user_id, username, access_token, csrf, expires_at) values ($1, $2, 't', 'old', 'c', now() + interval '1 day')")
            .bind(&sid)
            .bind(user)
            .execute(&db)
            .await
            .unwrap();
        let mock = Router::new().route(
            "/users/@me/guilds",
            get(|| async { (StatusCode::UNAUTHORIZED, axum::Json(json!({"code": 0}))) }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, mock).await });
        let st = AppState {
            db: db.clone(),
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
        };
        let s = Sess {
            id: sid.clone(),
            user_id: user,
            username: "t".into(),
            access_token: "old".into(),
            stepped_up: false,
            step_up_guild: None,
            csrf: String::new(),
        };
        assert!(matches!(
            user_guilds(&st, &s).await,
            Err(ApiError::Unauthorized)
        ));
        let left: i64 = sqlx::query_scalar("select count(*) from dash_session where id = $1")
            .bind(&sid)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. The user confirms their own setup while
    /// the bot is still sending another enroll request's DM: that request must not replace the secret.
    #[tokio::test]
    async fn enroll_racing_a_confirm_keeps_the_confirmed_secret() {
        use axum::{routing::post, Router};
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let user = 9_480_000_000 + std::process::id() as i64;
        sqlx::query(
            "insert into dash_totp (user_id, secret, dm_code) values ($1, 'MINE', '111111')",
        )
        .bind(user)
        .execute(&db)
        .await
        .unwrap();
        let during_dm = db.clone();
        let mock = Router::new()
            .route(
                "/users/@me/channels",
                post(|| async { axum::Json(json!({"id": "500"})) }),
            )
            .route(
                "/channels/500/messages",
                post(move || async move {
                    sqlx::query("update dash_totp set confirmed = true where user_id = $1")
                        .bind(user)
                        .execute(&during_dm)
                        .await
                        .unwrap();
                    axum::Json(json!({"id": "1"}))
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, mock).await });
        let st = AppState {
            db: db.clone(),
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
        };
        let s = Sess {
            id: String::new(),
            user_id: user,
            username: "t".into(),
            access_token: String::new(),
            stepped_up: false,
            step_up_guild: None,
            csrf: String::new(),
        };
        assert!(totp_enroll(State(st), s).await.is_err());
        let (secret, confirmed): (String, bool) =
            sqlx::query_as("select secret, confirmed from dash_totp where user_id = $1")
                .bind(user)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!((secret.as_str(), confirmed), ("MINE", true));
        sqlx::query("delete from dash_totp where user_id = $1")
            .bind(user)
            .execute(&db)
            .await
            .unwrap();
    }

    /// security review: a session ends after 24 hours without use; a step-up moves the session to a
    /// new id, and the old id keeps 30 s without the step-up; the OAuth token is stored sealed.
    #[tokio::test]
    async fn idle_sessions_end_and_a_step_up_gets_a_new_session_id() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_495_000_000).await else {
            return;
        };
        let user = 9_496_000_000 + std::process::id() as i64;
        let sess = |sid: String| {
            let (mut parts, _) = axum::http::Request::builder()
                .header("cookie", format!("sid={sid}"))
                .body(())
                .unwrap()
                .into_parts();
            let st = st.clone();
            async move { Sess::from_request_parts(&mut parts, &st).await }
        };
        let (idle, live) = (format!("idle-{user}"), format!("live-{user}"));
        for (sid, seen) in [(&idle, "25 hours"), (&live, "10 minutes")] {
            sqlx::query(sqlx::AssertSqlSafe(format!("insert into dash_session (id, user_id, username, access_token, csrf, expires_at, last_seen_at) values ($1, $2, 't', $3, 'c', now() + interval '1 day', now() - interval '{seen}')")))
                .bind(sid).bind(user).bind(seal_token(&st, sid, "discord-tok")).execute(&st.db).await.unwrap();
        }
        assert!(matches!(
            sess(idle.clone()).await,
            Err(ApiError::Unauthorized)
        ));
        let s = sess(live.clone()).await.unwrap();
        assert_eq!(s.access_token, "discord-tok");
        let stored: String =
            sqlx::query_scalar("select access_token from dash_session where id = $1")
                .bind(&live)
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert!(!stored.contains("discord-tok"));
        let fresh: bool = sqlx::query_scalar(
            "select last_seen_at > now() - interval '1 minute' from dash_session where id = $1",
        )
        .bind(&live)
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert!(fresh, "use moves last_seen_at");
        assert_eq!(
            open_token(&st, "x", "plain-old"),
            "plain-old",
            "rows from before sealing"
        );

        let secret = crate::otp::new_secret();
        sqlx::query("insert into dash_totp (user_id, secret, confirmed) values ($1, $2, true)")
            .bind(user)
            .bind(&secret)
            .execute(&st.db)
            .await
            .unwrap();
        let code = Code {
            code: crate::otp::current(&secret),
            dm_code: String::new(),
            guild: Some(g.to_string()),
        };
        let (jar, _) = totp_verify(State(st.clone()), s, CookieJar::new(), Json(code))
            .await
            .unwrap();
        let new = jar.get("sid").unwrap().value().to_string();
        assert_ne!(new, live);
        let n = sess(new).await.unwrap();
        assert!(n.stepped_up_for(g) && n.access_token == "discord-tok");
        let old = sess(live.clone()).await.unwrap();
        assert!(!old.stepped_up, "a copy of the old cookie gets no step-up");
        let short: bool = sqlx::query_scalar(
            "select expires_at < now() + interval '31 seconds' from dash_session where id = $1",
        )
        .bind(&live)
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert!(short);
        for t in ["dash_session", "dash_totp"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where user_id = $1"
            )))
            .bind(user)
            .execute(&st.db)
            .await
            .unwrap();
        }
    }

    /// `CF-Connecting-IP` from a sender that is not the tunnel container is dropped.
    #[tokio::test]
    async fn the_edge_header_is_dropped_from_other_senders() {
        use axum::{routing::get, Router};
        let app = Router::new()
            .route(
                "/",
                get(|h: axum::http::HeaderMap| async move {
                    h.get("cf-connecting-ip").is_some().to_string()
                }),
            )
            .layer(axum::middleware::from_fn(edge_only));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(l, app.into_make_service_with_connect_info::<SocketAddr>()).await
        });
        let body = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(&base)
            .header("cf-connecting-ip", "1.2.3.4")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "false");
    }

    #[test]
    fn csrf_token_required_on_writes() {
        for m in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            assert!(!csrf_ok(&m, None, "t"));
            assert!(!csrf_ok(&m, Some("x"), "t"));
            assert!(csrf_ok(&m, Some("t"), "t"));
        }
        assert!(csrf_ok(&Method::GET, None, "t"));
    }

    /// Runs only on a scratch Postgres. The lockout and the replay guard live in the database, so a
    /// dashboard restart keeps them (security review 2026-11-22).
    #[tokio::test]
    async fn totp_locks_after_five_misses_and_refuses_replay() {
        let Some((st, _, _)) = crate::api::g3_handler_tests::setup(9_497_000_000).await else {
            return;
        };
        let u = 9_498_000_000 + std::process::id() as i64;
        sqlx::query("delete from dash_fails where user_id = $1")
            .bind(u)
            .execute(&st.db)
            .await
            .unwrap();
        sqlx::query("insert into dash_totp (user_id, secret, confirmed) values ($1, 'x', true) on conflict (user_id) do update set last_step = null")
            .bind(u)
            .execute(&st.db)
            .await
            .unwrap();
        let wrong =
            |r: Result<(), ApiError>| matches!(r, Err(ApiError::Forbidden(m)) if m == WRONG_CODE);
        let locked = |r: Result<(), ApiError>| matches!(r, Err(ApiError::TooMany(_)));
        assert!(attempt(&st, u, Some(100), false).await.is_ok());
        assert!(
            wrong(attempt(&st, u, Some(100), false).await),
            "same step twice"
        );
        assert!(wrong(attempt(&st, u, Some(99), false).await), "older step");
        assert!(attempt(&st, u, Some(101), false).await.is_ok());
        for _ in 0..3 {
            assert!(wrong(attempt(&st, u, None, false).await));
        }
        assert!(locked(attempt(&st, u, Some(102), false).await));
        sqlx::query("update dash_fails set at = now() - interval '11 minutes' where kind = 'totp' and user_id = $1")
            .bind(u)
            .execute(&st.db)
            .await
            .unwrap();
        assert!(
            attempt(&st, u, Some(102), false).await.is_ok(),
            "misses expire"
        );
        // A step used on the dashboard is spent for MCP too; MCP misses lock MCP only.
        assert!(attempt(&st, u, Some(103), false).await.is_ok());
        assert!(
            wrong(attempt(&st, u, Some(103), true).await),
            "replay across paths"
        );
        for _ in 1..TOTP_FAILS {
            assert!(wrong(attempt(&st, u, None, true).await));
        }
        assert!(locked(attempt(&st, u, Some(104), true).await));
        assert!(attempt(&st, u, Some(104), false).await.is_ok());
        sqlx::query("delete from dash_fails where user_id = $1")
            .bind(u)
            .execute(&st.db)
            .await
            .unwrap();
        sqlx::query("delete from dash_totp where user_id = $1")
            .bind(u)
            .execute(&st.db)
            .await
            .unwrap();
    }

    #[test]
    fn removal_cookie_has_the_login_path() {
        let mut h = axum::http::HeaderMap::new();
        h.insert(axum::http::header::COOKIE, "sid=x".parse().unwrap());
        let jar = CookieJar::from_headers(&h).remove(cookie("sid", String::new(), true));
        let r = (jar, ()).into_response();
        let set = r.headers()[axum::http::header::SET_COOKIE]
            .to_str()
            .unwrap();
        assert!(
            set.starts_with("sid=;") && set.contains("Path=/") && set.contains("Max-Age=0"),
            "{set}"
        );
    }
}

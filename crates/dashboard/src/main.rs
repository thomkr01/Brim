mod access;
mod api;
mod appeals;
mod auth;
mod botmsgs;
mod channel_access;
mod channels;
mod datareq;
mod diff;
mod discord;
mod embeds;
mod feedfollow;
mod feedhooks;
mod feeds;
mod layout;
mod lockdown;
mod mcp;
mod msghooks;
mod msgrules;
mod msgsticky;
mod msgtags;
mod msgtrig;
mod ops;
mod otp;
mod profile;
mod raiders;
mod records;
mod recovery;
mod roles;
mod schema;
mod sections;
mod secure;
mod store;
mod tickets;
mod update;
mod verify;

use axum::{
    extract::DefaultBodyLimit,
    http::{
        header::{
            CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
            X_FRAME_OPTIONS,
        },
        HeaderValue, StatusCode,
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::json;
use sqlx::PgPool;
use tower_http::{
    services::ServeDir,
    set_header::SetResponseHeaderLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

// shortcut: in-memory state, correct only while the dashboard runs as one process; move to the DB if it scales out.
pub type Shared<K, V> = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<K, V>>>;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub discord: discord::Discord,
    pub secure: bool,
    /// Per-user TOTP failures and last accepted step.
    pub totp: Shared<i64, auth::TotpAttempts>,
    /// Per-session request buckets.
    pub rate: Shared<String, auth::Bucket>,
    /// Tor, VPN and hosting ranges for web verification's VPN block.
    pub ips: std::sync::Arc<brim_automod::iplist::IpLists>,
    /// When a member's VPN refusal was last logged, per (guild, user).
    pub vpn_logged: Shared<(i64, i64), i64>,
}

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    Forbidden(&'static str),
    /// Needs a fresh TOTP code (or enrollment) before this write.
    StepUp,
    NotFound,
    /// 429 with its own message: says which limit was hit and for how long.
    TooMany(String),
    Conflict(i32),
    /// 409 with its own message.
    Busy(&'static str),
    BadRequest(String),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError::Internal(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, body) = match self {
            ApiError::Unauthorized => {
                (StatusCode::UNAUTHORIZED, json!({"error": "login required"}))
            }
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, json!({"error": m})),
            ApiError::StepUp => (StatusCode::FORBIDDEN, json!({"error": "step_up_required"})),
            ApiError::NotFound => (StatusCode::NOT_FOUND, json!({"error": "not found"})),
            ApiError::TooMany(m) => (StatusCode::TOO_MANY_REQUESTS, json!({"error": m})),
            ApiError::Conflict(v) => (
                StatusCode::CONFLICT,
                json!({"error": "config changed, reload", "current_version": v}),
            ),
            ApiError::Busy(m) => (StatusCode::CONFLICT, json!({"error": m})),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, json!({"error": m})),
            ApiError::Internal(e) => {
                // The cause stays in the log (inside the request span: method and path); the user
                // gets only the random code that finds this line through logs_tail. The UI keeps a toast
                // with this code up (ui/src/common.rs `run`).
                let id = error_id();
                tracing::error!(error_id = %id, "{e:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": internal_text(&e, &id)}),
                )
            }
        };
        (code, Json(body)).into_response()
    }
}

/// 8 random characters without look-alikes (0/O, 1/I): 32 symbols, so a byte maps without bias.
fn error_id() -> String {
    const ABC: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = [0u8; 8];
    rand::fill(&mut b);
    b.iter().map(|x| ABC[usize::from(x % 32)] as char).collect()
}

/// What an unexpected error says to the user: the issue in plain words, never "internal
/// error" (owner decision 2026-11-15). Never the URL: a webhook path holds its token.
fn internal_text(e: &anyhow::Error, id: &str) -> String {
    let status = e
        .chain()
        .find_map(|c| c.downcast_ref::<reqwest::Error>())
        .and_then(|e| e.status());
    let plain = match status.map(|s| s.as_u16()) {
        Some(404) => "Discord says something this needs no longer exists (a deleted channel or role). Reload the page and try again.".into(),
        Some(403) => "Discord refused: Brim lacks a permission for this, or a role sits above Brim's role.".into(),
        Some(429) => "Discord is limiting requests right now. Try again in a minute.".into(),
        Some(s) => format!("Discord answered with error {s}. Try again in a minute."),
        None => return format!("Server error, code {id}. The cause is in the dashboard log."),
    };
    format!("{plain} (code {id})")
}

/// The UI page with the site's address in its link preview tags (they need a full URL).
fn site_page(html: &str, redirect_uri: &str) -> String {
    let base = redirect_uri.trim_end_matches("/auth/callback");
    html.replace("__SITE__", &base.replace(['"', '<', '>', '&'], ""))
}

pub fn router(st: AppState, dist: &str) -> Router {
    let g = "/api/guilds/{guild}";
    let page = std::fs::read_to_string(std::path::Path::new(dist).join("index.html"))
        .ok()
        .map(|h| axum::response::Html(site_page(&h, &st.discord.redirect_uri)));
    let root = Router::new();
    // Without a built UI the fallback below answers / with its "not built" message.
    let root = match page {
        Some(p) => root.route("/", get(move || async move { p })),
        None => root,
    };
    root
        .route("/auth/login", get(auth::login))
        .route("/invite", get(auth::invite))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/totp/enroll", post(auth::totp_enroll))
        .route("/api/totp/verify", post(auth::totp_verify))
        .route("/api/guilds", get(api::guilds))
        .route("/api/permit-catalog", get(api::permit_catalog))
        .route("/api/limits", get(api::limits))
        .route("/api/previews", get(api::previews))
        .route("/api/commands", get(api::commands))
        .route(&format!("{g}/config"), get(api::get_config))
        .route(&format!("{g}/access"), get(api::page_access))
        .route(
            &format!("{g}/config/{{section}}"),
            put(api::put_section).layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route(&format!("{g}/permits"), put(api::put_permits))
        .route(&format!("{g}/import"), post(api::import))
        .route(&format!("{g}/history"), get(api::history))
        .route(&format!("{g}/mcp-reads"), get(api::mcp_reads))
        .route(
            &format!("{g}/history/{{from}}/{{to}}"),
            get(api::diff_versions),
        )
        .route(
            &format!("{g}/history/{{version}}/rollback"),
            post(api::rollback),
        )
        .route(&format!("{g}/purge"), post(api::purge))
        .merge(sections::routes())
        .merge(ops::routes())
        .merge(lockdown::routes())
        .merge(recovery::routes())
        .merge(channels::routes())
        .merge(layout::routes())
        .merge(appeals::routes())
        .merge(records::routes())
        .merge(raiders::routes())
        .merge(datareq::routes())
        .merge(verify::routes())
        .merge(embeds::routes())
        .merge(msghooks::routes())
        .merge(msgrules::routes())
        .merge(msgtrig::routes())
        .merge(msgsticky::routes())
        .merge(feeds::routes())
        .merge(feedhooks::routes())
        .merge(msgtags::routes())
        .merge(botmsgs::routes())
        .merge(update::routes())
        .merge(mcp::routes())
        .merge(profile::routes())
        .merge(secure::routes())
        // Only routes, not the static UI: one page load fetches many assets.
        .route_layer(middleware::from_fn_with_state(st.clone(), auth::rate_limit))
        // Outside the limiter, which keys on the address this leaves.
        .layer(middleware::from_fn(auth::edge_only))
        // Hash routing means the server never needs an index.html fallback; one would also hide
        // missing assets by answering a stale hashed .js/.wasm request with HTML.
        .fallback_service(ServeDir::new(dist).not_found_service(
            axum::routing::any(|| async {
                (StatusCode::NOT_FOUND, "Not found. If this is the dashboard root, the web UI was not built into the image: rebuild with docker compose up -d --build.")
            }),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        // Only frame-ancestors: a full policy would have to allow the WASM UI and Turnstile.
        // if_not_present: /media sets its own stricter policy.
        .layer(SetResponseHeaderLayer::if_not_present(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(
            TraceLayer::new_for_http()
                // Path only: the query holds the Discord login code and state, and member search text.
                .make_span_with(|req: &axum::extract::Request| {
                    tracing::info_span!("request", method = %req.method(), path = req.uri().path(), version = ?req.version())
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(st)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _log = brim_core::log::init(
        "dashboard",
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    );
    let env = |k: &str| std::env::var(k).map_err(|_| anyhow::anyhow!("{k} not set"));
    let base = env("DASHBOARD_URL")?;
    // The session cookie is always Secure, except on a local test run (security review t-ds-session-scope).
    let secure = base.starts_with("https://");
    let local = ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|l| {
            base.strip_prefix(l)
                .is_some_and(|r| r.is_empty() || r.starts_with([':', '/']))
        });
    if !secure && !local {
        anyhow::bail!(
            "DASHBOARD_URL must start with https:// (only a local run may use http://localhost)"
        );
    }
    let db = brim_core::db::connect(&env("DATABASE_URL")?).await?;
    let _ = update::OLD_NAMES.set(brim_core::db::old_names(&db).await?);
    let st = AppState {
        db,
        secure,
        totp: Default::default(),
        rate: Default::default(),
        ips: Default::default(),
        vpn_logged: Default::default(),
        discord: discord::Discord {
            // A hung Discord call would otherwise hold a dashboard request forever.
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .user_agent(concat!(
                    "DiscordBot (https://github.com/thomkr01/Brim, ",
                    env!("CARGO_PKG_VERSION"),
                    ")"
                ))
                .build()?,
            client_id: env("DISCORD_CLIENT_ID")?,
            client_secret: env("DISCORD_CLIENT_SECRET")?,
            bot_token: brim_core::config::discord_token()?,
            redirect_uri: format!("{}/auth/callback", base.trim_end_matches('/')),
            api: std::env::var("DISCORD_API_BASE")
                .unwrap_or_else(|_| "https://discord.com/api/v10".into()),
            memo: Default::default(),
            limits: Default::default(),
        },
    };
    let ips = st.ips.clone();
    tokio::spawn(async move {
        let mut failures = 0;
        loop {
            let loaded = ips.refresh().await;
            let wait = brim_automod::sites::next_refresh(loaded, &mut failures);
            tokio::time::sleep(wait).await;
        }
    });
    tokio::spawn(msgrules::runner(st.clone()));
    tokio::spawn(feedfollow::run(st.clone()));
    if let Err(e) = st.discord.load_emojis().await {
        tracing::warn!(?e, "custom emojis not loaded, using Unicode fallbacks");
    }
    let dist =
        std::env::var("DASHBOARD_DIST").unwrap_or_else(|_| "crates/dashboard/ui/dist".into());
    if !std::path::Path::new(&dist).join("index.html").is_file() {
        tracing::error!(
            "no index.html in {dist}: the web UI was not built or copied, pages will be blank"
        );
    }
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("dashboard listening on {bind}, UI from {dist}, public URL {base}");
    // The peer address lets `auth::edge_only` tell the tunnel from other senders.
    axum::serve(
        listener,
        router(st, &dist).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn site_page_puts_the_site_address_in_the_preview_tags() {
        let html = r#"<meta property="og:image" content="__SITE__/link-preview.png" />"#;
        assert_eq!(
            super::site_page(html, "https://brim.example.com/auth/callback"),
            r#"<meta property="og:image" content="https://brim.example.com/link-preview.png" />"#
        );
        assert!(!super::site_page(html, "https://x\"><script>/auth/callback").contains("\"><"));
    }

    use super::*;

    #[tokio::test]
    async fn internal_error_shows_only_a_code_that_is_in_the_log() {
        let r = ApiError::Internal(anyhow::anyhow!("secret cause 404 Not Found")).into_response();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        let msg = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!msg.contains("secret cause"), "{msg}");
        let id = msg
            .strip_prefix("Server error, code ")
            .and_then(|m| m.strip_suffix(". The cause is in the dashboard log."))
            .unwrap();
        assert_eq!(id.len(), 8);
        assert!(id
            .bytes()
            .all(|c| b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(&c)));
        assert_ne!(error_id(), error_id());
    }

    #[tokio::test]
    async fn api_routes_are_rate_limited_and_every_response_has_security_headers() {
        let st = AppState {
            db: PgPool::connect_lazy("postgres://x@127.0.0.1:1/x").unwrap(),
            discord: discord::Discord {
                http: reqwest::Client::new(),
                client_id: String::new(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                api: String::new(),
                memo: Default::default(),
                limits: Default::default(),
            },
            secure: false,
            totp: Default::default(),
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(st, "/nonexistent");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        // /auth/login answers without the database, so only the limiter decides the status.
        let get = |path: &str| {
            http.get(format!("{base}{path}"))
                .header("cookie", "sid=x")
                .send()
        };
        for _ in 0..auth::BURST as usize {
            assert_ne!(get("/auth/login").await.unwrap().status(), 429);
        }
        // The bucket refills while the loop runs, so allow a few more before the refusal.
        let mut limited = get("/auth/login").await.unwrap();
        for _ in 0..10 {
            if limited.status() == 429 {
                break;
            }
            limited = get("/auth/login").await.unwrap();
        }
        assert_eq!(limited.status(), 429);
        assert_eq!(limited.headers()["x-frame-options"], "DENY");
        let asset = get("/app.wasm").await.unwrap();
        assert_eq!(asset.status(), 404, "static files are not rate limited");
        assert_eq!(
            asset.headers()["content-security-policy"],
            "frame-ancestors 'none'"
        );
        assert_eq!(asset.headers()["x-content-type-options"], "nosniff");
        assert_eq!(asset.headers()["referrer-policy"], "same-origin");
    }
}

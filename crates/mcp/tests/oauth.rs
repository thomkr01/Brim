//! The OAuth login flow end to end, with Discord mocked. Needs no database.
use axum::{
    body::Body,
    http::Request,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const OWNER: &str = "111";
const CB: &str = "https://client.example.com/oauth/callback";
const CB2: &str = "https://other.example.com/cb";

async fn mock_discord() -> String {
    let app = Router::new()
        .route(
            "/oauth2/token",
            post(|body: String| async move {
                // The code doubles as the user it logs in.
                let code = body
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("code="))
                    .unwrap_or("")
                    .to_string();
                Json(json!({"access_token": code}))
            }),
        )
        .route(
            "/users/@me",
            get(|h: axum::http::HeaderMap| async move {
                let who = h["authorization"]
                    .to_str()
                    .unwrap()
                    .trim_start_matches("Bearer ")
                    .to_string();
                Json(json!({"id": who}))
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

struct T(brim_mcp::State);

impl T {
    async fn req(&self, r: Request<Body>) -> (u16, axum::http::HeaderMap, Value) {
        let res = brim_mcp::router(self.0.clone()).oneshot(r).await.unwrap();
        let (code, h) = (res.status().as_u16(), res.headers().clone());
        let b = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (code, h, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }
    async fn get(&self, uri: &str) -> (u16, axum::http::HeaderMap, Value) {
        self.req(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }
    async fn form(&self, uri: &str, body: String) -> (u16, Value) {
        let r = Request::post(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        let (c, _, v) = self.req(r).await;
        (c, v)
    }
    async fn mcp(&self, tok: &str) -> u16 {
        let r = Request::post("/mcp")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {tok}"))
            .body(Body::from(
                json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string(),
            ))
            .unwrap();
        self.req(r).await.0
    }
}

fn enc(s: &str) -> String {
    s.replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
        .replace('.', "%2E")
        .replace('=', "%3D")
        .replace('&', "%26")
}

fn query(loc: &str, key: &str) -> String {
    let q = loc.split_once('?').unwrap().1;
    let v = q
        .split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .unwrap();
    // Our own encoding only escapes with %XX.
    let mut out = Vec::new();
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn owner_gets_a_token_others_do_not() {
    let api = mock_discord().await;
    let db = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://x@127.0.0.1:1/x")
        .unwrap();
    let t = T(brim_mcp::State {
        db,
        log_dir: "/nonexistent".into(),
        oauth: Some(brim_mcp::oauth::OAuth {
            secret: "s".repeat(40),
            base: "https://mcp.test".into(),
            owner_id: OWNER.into(),
            client_id: "cid".into(),
            client_secret: "csec".into(),
            discord_api: api,
            http: reqwest::Client::new(),
            redirects: brim_mcp::oauth::parse_redirects(&format!(" {CB} ,, {CB2}")).unwrap(),
        }),
        writes: Some(brim_mcp::writes::Writes {
            base: "http://127.0.0.1:1".into(),
            http: reqwest::Client::new(),
        }),
    });

    // 401 points the client at the metadata.
    let (code, h, _) = t
        .req(
            Request::post("/mcp")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(code, 401);
    assert!(h["www-authenticate"]
        .to_str()
        .unwrap()
        .contains("https://mcp.test/.well-known/oauth-protected-resource"));
    assert_eq!(
        t.get("/.well-known/oauth-authorization-server").await.2["token_endpoint"],
        "https://mcp.test/token"
    );

    // Registration.
    let reg = |uris: Value| {
        Request::post("/register")
            .header("content-type", "application/json")
            .body(Body::from(json!({"redirect_uris": uris}).to_string()))
            .unwrap()
    };
    let redirects = &t.0.oauth.as_ref().unwrap().redirects;
    // Only the configured callbacks and the owner's own computer: any other site could catch the owner's code.
    for bad in [
        "http://evil.example/cb",
        "https://evil.example/cb",
        "http://localhost.evil.example/cb",
        "http://127.0.0.1.evil.example/cb",
        "https://client.example.com.evil.example/oauth/callback",
        "https://x@evil.example/cb",
        "https://client.example.com/some/other/page",
        "https://client.example.com/oauth/callback?next=x",
        "https://client.example.com:8443/oauth/callback",
        "http://client.example.com/oauth/callback",
    ] {
        assert_eq!(t.req(reg(json!([bad]))).await.0, 400, "{bad}");
        assert!(!brim_mcp::oauth::allowed_redirect(redirects, bad), "{bad}");
    }
    for ok in [
        "http://localhost:4321/callback",
        "http://127.0.0.1:9/cb",
        CB,
        CB2,
    ] {
        assert!(brim_mcp::oauth::allowed_redirect(redirects, ok), "{ok}");
    }
    let (code, _, c) = t.req(reg(json!([CB]))).await;
    assert_eq!(code, 201);
    let cid = c["client_id"].as_str().unwrap().to_string();

    let verifier = "v".repeat(50);
    let chal = B64.encode(Sha256::digest(verifier.as_bytes()));
    let auth = |ruri: &str| {
        format!("/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge={chal}&code_challenge_method=S256&state=xyz", enc(&cid), enc(ruri))
    };
    // An unregistered redirect gets an error page, never a redirect.
    assert_eq!(t.get(&auth("https://evil.example/cb")).await.0, 400);
    // A client registered under the older host-only rule no longer gets a redirect to its loose page.
    let old = "https://client.example.com/some/other/page";
    let old_cid =
        t.0.oauth
            .as_ref()
            .unwrap()
            .sign("client", 3600, json!({"uris": [old]}));
    let old_auth = format!("/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge={chal}&code_challenge_method=S256&state=xyz", enc(&old_cid), enc(old));
    assert_eq!(t.get(&old_auth).await.0, 400);
    let (code, h, _) = t.get(&auth(CB)).await;
    assert_eq!(code, 303);
    let loc = h["location"].to_str().unwrap();
    assert!(loc.starts_with("https://discord.com/oauth2/authorize?"));
    let state = query(loc, "state");

    // Someone else logs in with Discord: refused.
    assert_eq!(
        t.get(&format!("/oauth/callback?code=222&state={}", enc(&state)))
            .await
            .0,
        403
    );
    // The owner: back to the client with a code and the original state.
    let (code, h, _) = t
        .get(&format!(
            "/oauth/callback?code={OWNER}&state={}",
            enc(&state)
        ))
        .await;
    assert_eq!(code, 303);
    let back = h["location"].to_str().unwrap().to_string();
    assert!(back.starts_with(CB));
    assert_eq!(query(&back, "state"), "xyz");
    let our_code = query(&back, "code");

    let exchange = |v: &str| {
        format!(
            "grant_type=authorization_code&code={}&client_id={}&redirect_uri={}&code_verifier={v}",
            enc(&our_code),
            enc(&cid),
            enc(CB)
        )
    };
    assert_eq!(t.form("/token", exchange("wrong-verifier")).await.0, 400);
    // The code only works with the client and redirect it was made for.
    let swapped = |cid: &str, ruri: &str| {
        format!(
            "grant_type=authorization_code&code={}&client_id={}&redirect_uri={}&code_verifier={verifier}",
            enc(&our_code),
            enc(cid),
            enc(ruri)
        )
    };
    assert_eq!(t.form("/token", swapped("other", CB)).await.0, 400);
    assert_eq!(
        t.form("/token", swapped(&cid, "http://localhost:4321/other"))
            .await
            .0,
        400
    );
    // Expired code, access and refresh tokens are refused (ttl 0 ends at once).
    let o = t.0.oauth.as_ref().unwrap();
    let dead_code = o.sign(
        "code",
        0,
        json!({"cid": &cid, "ruri": CB, "chal": &chal, "sub": OWNER}),
    );
    let dead = format!(
        "grant_type=authorization_code&code={}&client_id={}&redirect_uri={}&code_verifier={verifier}",
        enc(&dead_code),
        enc(&cid),
        enc(CB)
    );
    assert_eq!(t.form("/token", dead).await.0, 400);
    assert_eq!(
        t.mcp(&o.sign("access", 0, json!({"sub": OWNER}))).await,
        401
    );
    let dead_refresh = o.sign(
        "refresh",
        0,
        json!({"sub": OWNER, "cid": &cid, "auth": 1_900_000_000u64}),
    );
    let (code, _) = t
        .form(
            "/token",
            format!(
                "grant_type=refresh_token&refresh_token={}",
                enc(&dead_refresh)
            ),
        )
        .await;
    assert_eq!(code, 400);
    let (code, tok) = t.form("/token", exchange(&verifier)).await;
    assert_eq!(code, 200, "{tok}");
    let access = tok["access_token"].as_str().unwrap();
    assert_eq!(t.mcp(access).await, 200);
    // A tampered token, and a code or refresh token used as an access token, are refused.
    assert_eq!(t.mcp(&format!("{access}x")).await, 401);
    assert_eq!(t.mcp(&our_code).await, 401);
    assert_eq!(t.mcp(tok["refresh_token"].as_str().unwrap()).await, 401);

    let (code, again) = t
        .form(
            "/token",
            format!(
                "grant_type=refresh_token&refresh_token={}&client_id={}",
                enc(tok["refresh_token"].as_str().unwrap()),
                enc(&cid)
            ),
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(t.mcp(again["access_token"].as_str().unwrap()).await, 200);
    // A refresh keeps the login time, so the chain ends 30 days after the Discord login.
    let claims = |t: &str| -> Value {
        serde_json::from_slice(&B64.decode(t.split('.').next().unwrap()).unwrap()).unwrap()
    };
    let (r1, r2) = (
        claims(tok["refresh_token"].as_str().unwrap()),
        claims(again["refresh_token"].as_str().unwrap()),
    );
    assert!(r1["auth"].is_u64() && r1["exp"].is_u64());
    assert_eq!(r1["auth"], r2["auth"]);
    assert_eq!(r1["exp"], r2["exp"], "the chain end does not move");
    // A refresh token is bound to its client.
    let (code, _) = t
        .form(
            "/token",
            format!(
                "grant_type=refresh_token&refresh_token={}&client_id=other",
                enc(again["refresh_token"].as_str().unwrap())
            ),
        )
        .await;
    assert_eq!(code, 400);
    // A refresh token without a login time (made before the limit) is refused.
    let old =
        t.0.oauth
            .as_ref()
            .unwrap()
            .sign("refresh", 3600, json!({"sub": OWNER, "cid": cid}));
    let (code, _) = t
        .form(
            "/token",
            format!("grant_type=refresh_token&refresh_token={}", enc(&old)),
        )
        .await;
    assert_eq!(code, 400);
    // An OAuth login only reads: no write tools, and a write call is refused (owner decision 2026-11-17).
    let call = |method: &str, params: Value| {
        Request::post("/mcp")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {access}"))
            .body(Body::from(
                json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string(),
            ))
            .unwrap()
    };
    let (_, _, list) = t.req(call("tools/list", json!({}))).await;
    assert!(!list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "propose_change" || t["name"] == "confirm_change"));
    let (_, _, init) = t.req(call("initialize", json!({}))).await;
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .starts_with("Read-only access"));
    let (_, _, w) = t
        .req(call(
            "tools/call",
            json!({"name": "propose_change", "arguments": {"guild_id": "1", "action": "note_add"}}),
        ))
        .await;
    assert!(w.to_string().contains("this login only reads"), "{w}");
    // There is no fixed token any more; a dashboard token the database cannot check is refused.
    assert_eq!(
        t.mcp("tbmcp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").await,
        401
    );
}

#[test]
fn redirects_setting() {
    use brim_mcp::oauth::{allowed_redirect, parse_redirects};
    // Empty or unset: no https callback, but the owner's own computer still works.
    for empty in ["", " , "] {
        let r = parse_redirects(empty).unwrap();
        assert!(r.is_empty());
        assert!(!allowed_redirect(&r, CB));
        assert!(allowed_redirect(&r, "http://localhost:4321/callback"));
    }
    // A bad entry stops the start and names the entry.
    for bad in [
        "http://client.example.com/cb",
        "https://client.example.com:8443/cb",
        "https://client.example.com/cb?x=1",
        "https://client.example.com/cb#f",
        "https://u:p@client.example.com/cb",
        "not a url",
    ] {
        let e = parse_redirects(&format!("{CB},{bad}"))
            .unwrap_err()
            .to_string();
        assert!(e.contains(bad), "{e}");
    }
}

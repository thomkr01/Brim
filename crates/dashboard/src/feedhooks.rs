//! GitHub and GitLab feeds (feeds plan F4, 9a): the public endpoint `POST /hooks/feed/<id>` that
//! GitHub or GitLab calls for each event, and the event → post mapping.
//!
//! Order of checks: rate limits (per address, per feed), size (1 MB, the route's body limit), the
//! feed row, then the signature over the raw body (GitHub: HMAC-SHA256 with the feed's secret;
//! GitLab: the secret token), both compared in constant time. Only a signed body is parsed. Any
//! refusal before that answers 404 with no detail, so nobody learns which feeds exist or why.
//! A signed event whose type is switched off is answered and dropped; others are queued in
//! `feed_events` (a repeated delivery id is ignored) and the bot posts them (`bot/src/feeds.rs`),
//! with the same checks before posting as RSS items.
//!
//! The post looks as Discord's own GitHub webhook posts it, recorded from GitHub's sample events
//! (sample events recorded 2026-11-20, see the design notes): `{headline}` is that embed's title; GitLab
//! events get the same look.
use crate::AppState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde_json::{Map, Value};

/// The event types a feed can switch on, with what posts (feeds plan 9a), and the default ones.
pub const TOGGLES: &[(&str, &str)] = &[
    ("push", "Commits pushed"),
    (
        "pull_request",
        "Pull or merge requests opened, closed, reopened",
    ),
    ("issues", "Issues opened, closed, reopened"),
    ("release", "Releases published"),
    ("create", "Branches and tags made or deleted"),
    ("comments", "Comments on issues, pull requests and commits"),
    ("reviews", "Pull request reviews and review comments"),
    ("star", "Stars"),
    ("fork", "Forks"),
    ("ci", "Failed checks and pipelines"),
    ("discussions", "Discussions"),
    ("members", "Collaborator added, repository made public"),
];
pub const DEFAULT_ON: &[&str] = &["push", "pull_request", "issues", "release"];

/// Events posted per feed per minute; the rest are dropped and counted (plan section 6).
const PER_MINUTE: i64 = 30;
/// Largest event body taken (plan section 8).
const MAX_BODY: usize = 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/hooks/feed/{id}",
        post(receive).layer(DefaultBodyLimit::max(MAX_BODY)),
    )
}

/// The answer to anything not taken: no detail.
fn no() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

async fn receive(
    State(st): State<AppState>,
    Path(id): Path<String>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    // Before the signature only the address limit (on top of the dashboard's own): a per-feed
    // limit there would let anyone without the secret use up a feed's turns and make GitHub's real
    // deliveries fail. GitHub and GitLab call from a few shared addresses for every server, so it
    // is wide; the per-feed limit comes after the signature (`take`).
    let ip = crate::auth::client_of(&h, peer.map(|p| p.0 .0.ip()));
    if !brim_core::ratelimit::take(&st.rate, &format!("hook-ip:{ip}"), 300.0, 30.0) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(id) = id.parse::<i64>() else {
        return no();
    };
    match take(&st, id, &h, &body).await {
        Ok(code) => code.into_response(),
        Err(Taken::No) => no(),
        Err(Taken::TooMany) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(Taken::Bad(why)) => (StatusCode::BAD_REQUEST, why).into_response(),
        Err(Taken::Db(e)) => {
            tracing::error!(feed = id, "a feed event could not be saved: {e}");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

enum Taken {
    /// Unknown, paused or broken feed, or a wrong signature: 404.
    No,
    /// Signed, but not something Brim can take (said to the sender: only the repo admin sees it).
    Bad(&'static str),
    /// Signed, but the feed sends faster than 2 a second (60 at once).
    TooMany,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for Taken {
    fn from(e: sqlx::Error) -> Self {
        Taken::Db(e)
    }
}

async fn take(st: &AppState, id: i64, h: &HeaderMap, body: &[u8]) -> Result<StatusCode, Taken> {
    let row: Option<(String, Option<String>, Vec<String>, bool)> = sqlx::query_as(
        "select kind, secret, events, primed from feeds
         where id = $1 and kind in ('github', 'gitlab') and state = 'on'",
    )
    .bind(id)
    .fetch_optional(&st.db)
    .await?;
    let Some((kind, Some(secret), events, primed)) = row else {
        return Err(Taken::No);
    };
    let github = kind == "github";
    let signed = if github {
        signed_github(secret.as_bytes(), header(h, "x-hub-signature-256"), body)
    } else {
        same(secret.as_bytes(), header(h, "x-gitlab-token").as_bytes())
    };
    if !signed {
        return Err(Taken::No);
    }
    if !brim_core::ratelimit::take(&st.rate, &format!("hook-feed:{id}"), 60.0, 2.0) {
        return Err(Taken::TooMany);
    }
    // Retries of one delivery keep its id: GitHub's delivery GUID, GitLab's Idempotency-Key (17.4+)
    // else its event UUID.
    let delivery = if github {
        header(h, "x-github-delivery")
    } else {
        [
            header(h, "idempotency-key"),
            header(h, "x-gitlab-event-uuid"),
        ]
        .into_iter()
        .find(|d| !d.is_empty())
        .unwrap_or("")
    };
    if delivery.is_empty() || delivery.len() > 100 {
        return Err(Taken::Bad("no delivery id"));
    }
    let event = header(
        h,
        if github {
            "x-github-event"
        } else {
            "x-gitlab-event"
        },
    );
    if github && header(h, "content-type").starts_with("application/x-www-form-urlencoded") {
        return Err(Taken::Bad(
            "set Content type to application/json in the webhook's settings",
        ));
    }
    let Ok(p) = serde_json::from_slice::<Value>(body) else {
        return Err(Taken::Bad("the body is not JSON"));
    };
    // The repository's name from any signed delivery, the ping too: a webhook feed stores no
    // address, so this is the name its rows and log lines show.
    let named = if github {
        &p["repository"]["full_name"]
    } else {
        &p["project"]["path_with_namespace"]
    };
    if let Some(n) = named.as_str().filter(|n| !n.is_empty()) {
        sqlx::query("update feeds set source_name = $2 where id = $1 and source_name <> $2")
            .bind(id)
            .bind(n.chars().take(100).collect::<String>())
            .execute(&st.db)
            .await?;
    }
    let first = !primed;
    if first {
        // The first signed delivery shows the feed works ("connected" on the Feeds page, one log line).
        let now: bool = sqlx::query_scalar(
            "update feeds set primed = true where id = $1 and not primed returning true",
        )
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .unwrap_or(false);
        if now {
            queue(
                st,
                id,
                &format!("{delivery}:ping"),
                "ping",
                &Map::new(),
                None,
            )
            .await?;
        }
    }
    let out = if github {
        github_post(event, &p)
    } else {
        gitlab_post(event, &p)
    };
    let Some(out) = out.filter(|o| events.iter().any(|e| e == o.toggle)) else {
        return Ok(StatusCode::NO_CONTENT);
    };
    // The source's name and picture: the repository and its owner's picture.
    let _ = sqlx::query("update feeds set source_name = $2, source_picture = $3 where id = $1")
        .bind(id)
        .bind(out.repo.chars().take(100).collect::<String>())
        .bind(Some(out.picture.clone()).filter(|p| !p.is_empty()))
        .execute(&st.db)
        .await;
    let recent: i64 = sqlx::query_scalar(
        "select count(*) from feed_events where feed_id = $1 and event <> 'ping'
         and at > now() - interval '1 minute'",
    )
    .bind(id)
    .fetch_one(&st.db)
    .await?;
    if recent >= PER_MINUTE {
        sqlx::query("update feeds set dropped = dropped + 1 where id = $1")
            .bind(id)
            .execute(&st.db)
            .await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    // The signature covers only the body, not the delivery id: the same body under a new id is
    // the same delivery sent again (someone who saw it in GitHub's delivery log).
    let hash: String = <sha2::Sha256 as sha2::Digest>::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let seen: bool = sqlx::query_scalar(
        "select exists(select 1 from feed_events where feed_id = $1 and body_hash = $2)",
    )
    .bind(id)
    .bind(&hash)
    .fetch_one(&st.db)
    .await?;
    if seen {
        return Ok(StatusCode::NO_CONTENT);
    }
    queue(st, id, delivery, out.entry, &out.vars, Some(&hash)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Queues one event; a delivery id seen before (a retry, a replay) is ignored.
pub(crate) async fn queue(
    st: &AppState,
    id: i64,
    delivery: &str,
    event: &str,
    vars: &Map<String, Value>,
    hash: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "insert into feed_events (feed_id, delivery, event, vars, body_hash)
         values ($1, $2, $3, $4, $5) on conflict do nothing",
    )
    .bind(id)
    .bind(delivery)
    .bind(event)
    .bind(Value::Object(vars.clone()))
    .bind(hash)
    .execute(&st.db)
    .await
    .map(drop)
}

/// Constant-time equality (the length is not secret: every secret is 64 characters).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// GitHub's `X-Hub-Signature-256: sha256=<hex>` over the raw body, checked in constant time.
fn signed_github(secret: &[u8], sig: &str, body: &[u8]) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    let Some(hex) = sig.strip_prefix("sha256=") else {
        return false;
    };
    let Some(want) = unhex(hex) else { return false };
    let mut m = Hmac::<sha2::Sha256>::new_from_slice(secret).expect("HMAC takes any key length");
    m.update(body);
    m.verify_slice(&want).is_ok()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A new feed secret: 32 random bytes as 64 hex characters.
pub fn new_secret() -> String {
    let mut b = [0u8; 32];
    rand::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One event as a post.
#[derive(Debug)]
pub struct Out {
    /// The toggle that switches it on ([`TOGGLES`]).
    pub toggle: &'static str,
    /// The entry `feed.github.<entry>`.
    pub entry: &'static str,
    pub vars: Map<String, Value>,
    /// The repository with its owner, and the owner's picture (the webhook's default look).
    pub repo: String,
    pub picture: String,
}

const GREEN: u32 = 0x009800;
const RED: u32 = 0xFC2929;
const ISSUE: u32 = 0xEB6420;
const COMMENT: u32 = 0xE68D60;
const PR_COMMENT: u32 = 0xBFE5BF;
const PUSH: u32 = 0x7289DA;

fn st(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn num(v: &Value) -> String {
    v.as_u64().map(|n| n.to_string()).unwrap_or_default()
}

/// Only web links: Discord refuses a whole message with another kind in an embed.
fn web(v: &Value) -> String {
    let s = st(v);
    if s.starts_with("https://") || s.starts_with("http://") {
        s.into()
    } else {
        String::new()
    }
}

/// `s` cut to `max` characters the way GitHub's look cuts: the first `max - 3` and "...".
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max - 3).collect::<String>())
    } else {
        s.into()
    }
}

/// The parts every post has; the event fills in the rest.
struct Post {
    headline: String,
    link: String,
    summary: String,
    color: u32,
    /// (name, profile link, picture link); empty name = no author line.
    author: (String, String, String),
    action: String,
    number: String,
    title: String,
    branch: String,
    /// Recorded without an author line (checks): the sender is not added.
    no_author: bool,
}

impl Post {
    fn new(headline: String, link: String) -> Self {
        Post {
            headline,
            link,
            summary: String::new(),
            color: 0,
            author: Default::default(),
            action: String::new(),
            number: String::new(),
            title: String::new(),
            branch: String::new(),
            no_author: false,
        }
    }

    fn no_sender(mut self) -> Self {
        self.no_author = true;
        self
    }

    fn out(
        self,
        toggle: &'static str,
        entry: &'static str,
        repo: &str,
        repo_name: &str,
        picture: String,
    ) -> Out {
        let s = |v: &str| Value::String(v.into());
        let color = if self.color == 0 {
            String::new()
        } else {
            self.color.to_string()
        };
        let vars = [
            ("headline", s(&self.headline)),
            ("link", s(&self.link)),
            ("summary", s(&self.summary)),
            ("author", s(&self.author.0)),
            ("author_link", s(&self.author.1)),
            ("author_icon", s(&self.author.2)),
            ("repo", s(repo)),
            ("repo_name", s(repo_name)),
            ("branch", s(&self.branch)),
            ("action", s(&self.action)),
            ("number", s(&self.number)),
            ("title", s(&self.title)),
            ("color", s(&color)),
        ];
        Out {
            toggle,
            entry,
            vars: vars.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            repo: repo.into(),
            picture,
        }
    }
}

fn gh_user(u: &Value) -> (String, String, String) {
    (
        st(&u["login"]).into(),
        web(&u["html_url"]),
        web(&u["avatar_url"]),
    )
}

/// A GitHub event as Discord's GitHub webhook posts it; `None` for what that webhook does not post.
pub fn github_post(event: &str, p: &Value) -> Option<Out> {
    let repo = st(&p["repository"]["full_name"]);
    let name = st(&p["repository"]["name"]);
    let action = st(&p["action"]);
    let sender = gh_user(&p["sender"]);
    let (toggle, entry, mut post) = match (event, action) {
        ("push", _) => {
            let commits = p["commits"].as_array().filter(|c| !c.is_empty())?;
            let n = commits.len();
            // Exactly as recorded: a tag push shows an empty branch.
            let branch = st(&p["ref"]).strip_prefix("refs/heads/").unwrap_or("");
            let lines: Vec<String> = commits
                .iter()
                .take(5)
                .map(|c| {
                    let id = st(&c["id"]);
                    let first = st(&c["message"]).lines().next().unwrap_or("");
                    format!(
                        "[`{}`]({}) {} - {}",
                        id.get(..7).unwrap_or(id),
                        web(&c["url"]),
                        cut(first, 50),
                        st(&c["author"]["name"])
                    )
                })
                .collect();
            let s = if n == 1 { "" } else { "s" };
            let mut post = Post::new(
                format!("[{name}:{branch}] {n} new commit{s}"),
                web(&p["head_commit"]["url"]),
            );
            post.summary = lines.join("\n");
            post.color = PUSH;
            post.branch = branch.into();
            ("push", "push", post)
        }
        ("pull_request", "opened" | "closed" | "reopened") => {
            let pr = &p["pull_request"];
            let mut post = Post::new(
                format!(
                    "[{repo}] Pull request {action}: #{} {}",
                    num(&pr["number"]),
                    st(&pr["title"])
                ),
                web(&pr["html_url"]),
            );
            if action == "opened" {
                post.summary = cut(st(&pr["body"]), 500);
                post.color = GREEN;
            }
            post.number = num(&pr["number"]);
            post.title = st(&pr["title"]).into();
            ("pull_request", "pull_request", post)
        }
        ("issues", "opened" | "closed" | "reopened") => {
            let i = &p["issue"];
            let mut post = Post::new(
                format!(
                    "[{repo}] Issue {action}: #{} {}",
                    num(&i["number"]),
                    st(&i["title"])
                ),
                web(&i["html_url"]),
            );
            if action == "opened" {
                post.summary = cut(st(&i["body"]), 500);
                post.color = ISSUE;
            }
            post.number = num(&i["number"]);
            post.title = st(&i["title"]).into();
            ("issues", "issues", post)
        }
        ("release", "published") => {
            let r = &p["release"];
            let mut post = Post::new(
                format!("[{repo}] New release published: {}", st(&r["tag_name"])),
                web(&r["html_url"]),
            );
            post.title = st(&r["tag_name"]).into();
            ("release", "release", post)
        }
        ("create", _) => {
            let post = Post::new(
                format!(
                    "[{repo}] New {} created: {}",
                    st(&p["ref_type"]),
                    st(&p["ref"])
                ),
                String::new(),
            );
            ("create", "create", post)
        }
        ("delete", _) => {
            let post = Post::new(
                format!("[{repo}] {} deleted: {}", st(&p["ref_type"]), st(&p["ref"])),
                String::new(),
            );
            ("create", "delete", post)
        }
        ("issue_comment", "created") => {
            let i = &p["issue"];
            let on_pr = i["pull_request"].is_object();
            let mut post = Post::new(
                format!(
                    "[{repo}] New comment on {} #{}: {}",
                    if on_pr { "pull request" } else { "issue" },
                    num(&i["number"]),
                    st(&i["title"])
                ),
                web(&p["comment"]["html_url"]),
            );
            post.summary = cut(st(&p["comment"]["body"]), 500);
            post.color = if on_pr { PR_COMMENT } else { COMMENT };
            post.number = num(&i["number"]);
            post.title = st(&i["title"]).into();
            ("comments", "issue_comment", post)
        }
        ("commit_comment", "created") => {
            let c = &p["comment"];
            let id = st(&c["commit_id"]);
            let mut post = Post::new(
                format!(
                    "[{repo}] New comment on commit `{}`",
                    id.get(..7).unwrap_or(id)
                ),
                web(&c["html_url"]),
            );
            post.summary = cut(st(&c["body"]), 500);
            ("comments", "commit_comment", post)
        }
        ("pull_request_review", "submitted" | "dismissed") => {
            let pr = &p["pull_request"];
            let r = &p["review"];
            let mut post = Post::new(
                format!(
                    "[{repo}] Pull request review {action}: #{} {}",
                    num(&pr["number"]),
                    st(&pr["title"])
                ),
                web(&r["html_url"]),
            );
            if action == "submitted" {
                post.summary = cut(st(&r["body"]), 500);
                post.color = match st(&r["state"]) {
                    "approved" => GREEN,
                    "changes_requested" => RED,
                    _ => 0,
                };
            }
            post.number = num(&pr["number"]);
            post.title = st(&pr["title"]).into();
            ("reviews", "pull_request_review", post)
        }
        ("pull_request_review_comment", "created") => {
            let pr = &p["pull_request"];
            let mut post = Post::new(
                format!(
                    "[{repo}] New review comment on pull request #{}: {}",
                    num(&pr["number"]),
                    st(&pr["title"])
                ),
                web(&p["comment"]["html_url"]),
            );
            post.summary = cut(st(&p["comment"]["body"]), 500);
            post.color = PR_COMMENT;
            post.number = num(&pr["number"]);
            post.title = st(&pr["title"]).into();
            ("reviews", "pull_request_review_comment", post)
        }
        ("watch", "started") => (
            "star",
            "watch",
            Post::new(
                format!("[{repo}] New star added"),
                web(&p["repository"]["html_url"]),
            ),
        ),
        ("fork", _) => {
            let f = &p["forkee"];
            let mut post = Post::new(
                format!("[{repo}] Fork created: {}", st(&f["full_name"])),
                web(&f["html_url"]),
            );
            post.author = gh_user(&f["owner"]);
            ("fork", "fork", post)
        }
        ("check_suite", "completed") => {
            let c = &p["check_suite"];
            let conclusion = st(&c["conclusion"]);
            // Failures only (plan 9a).
            if !matches!(conclusion, "failure" | "timed_out" | "startup_failure") {
                return None;
            }
            let html = st(&p["repository"]["html_url"]);
            let (on, link) = match c["pull_requests"][0]["number"].as_u64() {
                Some(n) => (format!("pull request #{n}"), format!("{html}/pull/{n}")),
                None => (
                    st(&c["head_branch"]).to_string(),
                    format!("{html}/commit/{}", st(&c["head_sha"])),
                ),
            };
            let mut post = Post::new(
                format!(
                    "[{name}] {} checks {} on {on}",
                    st(&c["app"]["name"]),
                    conclusion.replace('_', " ")
                ),
                web(&Value::String(link)),
            );
            post.color = RED;
            post.action = conclusion.into();
            post.branch = st(&c["head_branch"]).into();
            // Recorded: no author line.
            ("ci", "check_suite", post.no_sender())
        }
        ("discussion", "created" | "answered") => {
            let d = &p["discussion"];
            let n = num(&d["number"]);
            let t = st(&d["title"]);
            let mut post = if action == "created" {
                let mut post = Post::new(
                    format!("[{name}] New discussion #{n}: {t}"),
                    web(&d["html_url"]),
                );
                post.summary = cut(st(&d["body"]), 500);
                post.color = COMMENT;
                post
            } else {
                let a = &p["answer"];
                let mut post = Post::new(
                    format!("[{name}] Discussion #{n} marked answered: {t}"),
                    web(&a["html_url"]),
                );
                post.summary = cut(st(&a["body"]), 500);
                post.color = GREEN;
                post.author = gh_user(&a["user"]);
                post
            };
            post.number = n;
            post.title = t.into();
            ("discussions", "discussion", post)
        }
        ("discussion_comment", "created") => {
            let d = &p["discussion"];
            let mut post = Post::new(
                format!(
                    "[{repo}] New comment on discussion #{}: {}",
                    num(&d["number"]),
                    st(&d["title"])
                ),
                web(&p["comment"]["html_url"]),
            );
            post.summary = cut(st(&p["comment"]["body"]), 500);
            post.color = COMMENT;
            post.number = num(&d["number"]);
            post.title = st(&d["title"]).into();
            ("discussions", "discussion_comment", post)
        }
        ("member", "added") => {
            let m = &p["member"];
            let mut post = Post::new(
                format!("[{repo}] New collaborator added: {}", st(&m["login"])),
                web(&m["html_url"]),
            );
            post.author = gh_user(m);
            ("members", "member", post)
        }
        ("public", _) => (
            "members",
            "public",
            Post::new(format!("[{repo}] Now open sourced!"), String::new()),
        ),
        _ => return None,
    };
    if post.author.0.is_empty() && !post.no_author {
        post.author = sender;
    }
    if post.action.is_empty() {
        post.action = action.into();
    }
    let picture = web(&p["repository"]["owner"]["avatar_url"]);
    Some(post.out(toggle, entry, repo, name, picture))
}

/// A GitLab event with the same look (Discord has no GitLab look of its own); `None` for events
/// with no GitHub match, and for anything confidential (it must never reach a Discord channel).
pub fn gitlab_post(event: &str, p: &Value) -> Option<Out> {
    let project = &p["project"];
    let repo = st(&project["path_with_namespace"]);
    let name = repo.rsplit('/').next().unwrap_or(repo);
    let web_url = st(&project["web_url"]);
    // The instance's own address (a self-hosted GitLab too), for profile links.
    let home = web_url.strip_suffix(repo).unwrap_or("");
    let user = |name: &str, avatar: &Value| {
        let link = if name.is_empty() || home.is_empty() {
            String::new()
        } else {
            web(&Value::String(format!("{home}{name}")))
        };
        (name.to_string(), link, web(avatar))
    };
    let o = &p["object_attributes"];
    let who = user(st(&p["user"]["username"]), &p["user"]["avatar_url"]);
    let kind = match st(&o["action"]) {
        "open" => "opened",
        "close" | "merge" => "closed",
        "reopen" => "reopened",
        _ => "",
    };
    let (toggle, entry, mut post) = match event {
        "Push Hook" => {
            let commits = p["commits"].as_array().filter(|c| !c.is_empty())?;
            let n = p["total_commits_count"]
                .as_u64()
                .unwrap_or(commits.len() as u64);
            let branch = st(&p["ref"]).strip_prefix("refs/heads/").unwrap_or("");
            let lines: Vec<String> = commits
                .iter()
                .take(5)
                .map(|c| {
                    let id = st(&c["id"]);
                    let first = st(&c["message"]).lines().next().unwrap_or("");
                    format!(
                        "[`{}`]({}) {} - {}",
                        id.get(..7).unwrap_or(id),
                        web(&c["url"]),
                        cut(first, 50),
                        st(&c["author"]["name"])
                    )
                })
                .collect();
            let head = commits
                .iter()
                .find(|c| c["id"] == p["checkout_sha"])
                .map(|c| web(&c["url"]))
                .unwrap_or_default();
            let s = if n == 1 { "" } else { "s" };
            let mut post = Post::new(format!("[{name}:{branch}] {n} new commit{s}"), head);
            post.summary = lines.join("\n");
            post.color = PUSH;
            post.branch = branch.into();
            post.author = user(st(&p["user_username"]), &p["user_avatar"]);
            ("push", "push", post)
        }
        "Tag Push Hook" => {
            let tag = st(&p["ref"]).strip_prefix("refs/tags/").unwrap_or("");
            let gone = st(&p["after"]).bytes().all(|b| b == b'0');
            let mut post = if gone {
                Post::new(format!("[{repo}] tag deleted: {tag}"), String::new())
            } else {
                Post::new(format!("[{repo}] New tag created: {tag}"), String::new())
            };
            post.author = user(st(&p["user_username"]), &p["user_avatar"]);
            ("create", if gone { "delete" } else { "create" }, post)
        }
        "Merge Request Hook" if !kind.is_empty() => {
            let mut post = Post::new(
                format!(
                    "[{repo}] Merge request {kind}: !{} {}",
                    num(&o["iid"]),
                    st(&o["title"])
                ),
                web(&o["url"]),
            );
            if kind == "opened" {
                post.summary = cut(st(&o["description"]), 500);
                post.color = GREEN;
            }
            post.number = num(&o["iid"]);
            post.title = st(&o["title"]).into();
            post.action = kind.into();
            ("pull_request", "pull_request", post)
        }
        // "Confidential Issue Hook" is a separate event and never matches.
        "Issue Hook" if !kind.is_empty() && o["confidential"] != true => {
            let mut post = Post::new(
                format!(
                    "[{repo}] Issue {kind}: #{} {}",
                    num(&o["iid"]),
                    st(&o["title"])
                ),
                web(&o["url"]),
            );
            if kind == "opened" {
                post.summary = cut(st(&o["description"]), 500);
                post.color = ISSUE;
            }
            post.number = num(&o["iid"]);
            post.title = st(&o["title"]).into();
            post.action = kind.into();
            ("issues", "issues", post)
        }
        "Release Hook" if st(&p["action"]) == "create" => {
            let mut post = Post::new(
                format!("[{repo}] New release published: {}", st(&p["tag"])),
                web(&p["url"]),
            );
            post.title = st(&p["tag"]).into();
            ("release", "release", post)
        }
        // "Confidential Note Hook" never matches; a note on a confidential issue is left out too.
        "Note Hook" if o["confidential"] != true && p["issue"]["confidential"] != true => {
            let (headline, color, entry) = match st(&o["noteable_type"]) {
                "Issue" => (
                    format!(
                        "[{repo}] New comment on issue #{}: {}",
                        num(&p["issue"]["iid"]),
                        st(&p["issue"]["title"])
                    ),
                    COMMENT,
                    "issue_comment",
                ),
                "MergeRequest" => (
                    format!(
                        "[{repo}] New comment on merge request !{}: {}",
                        num(&p["merge_request"]["iid"]),
                        st(&p["merge_request"]["title"])
                    ),
                    PR_COMMENT,
                    "issue_comment",
                ),
                "Commit" => {
                    let id = st(&p["commit"]["id"]);
                    (
                        format!(
                            "[{repo}] New comment on commit `{}`",
                            id.get(..7).unwrap_or(id)
                        ),
                        0,
                        "commit_comment",
                    )
                }
                _ => return None,
            };
            let mut post = Post::new(headline, web(&o["url"]));
            post.summary = cut(st(&o["note"]), 500);
            post.color = color;
            ("comments", entry, post)
        }
        "Pipeline Hook" if st(&o["status"]) == "failed" => {
            let (on, link) = match p["merge_request"]["iid"].as_u64() {
                Some(n) => (
                    format!("merge request !{n}"),
                    web(&p["merge_request"]["url"]),
                ),
                None => (
                    st(&o["ref"]).to_string(),
                    format!("{web_url}/-/pipelines/{}", num(&o["id"])),
                ),
            };
            let link = if o["url"].is_string() {
                web(&o["url"])
            } else {
                web(&Value::String(link))
            };
            let mut post = Post::new(format!("[{name}] pipeline checks failed on {on}"), link);
            post.color = RED;
            post.action = "failed".into();
            post.branch = st(&o["ref"]).into();
            ("ci", "check_suite", post.no_sender())
        }
        _ => return None,
    };
    if post.author.0.is_empty() && !post.no_author {
        post.author = who;
    }
    let picture = web(&project["avatar_url"]);
    Some(post.out(toggle, entry, repo, name, picture))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v<'a>(o: &'a Out, k: &str) -> &'a str {
        o.vars[k].as_str().unwrap()
    }

    fn gh_repo() -> Value {
        json!({"full_name": "Codertocat/Hello-World", "name": "Hello-World",
               "html_url": "https://github.com/Codertocat/Hello-World",
               "owner": {"avatar_url": "https://avatars1.githubusercontent.com/u/21031067?v=4"}})
    }

    fn sender() -> Value {
        json!({"login": "Codertocat", "html_url": "https://github.com/Codertocat",
               "avatar_url": "https://avatars1.githubusercontent.com/u/21031067?v=4"})
    }

    #[test]
    fn signatures() {
        // GitHub's own example (docs: "Validating webhook deliveries").
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(signed_github(
            b"It's a Secret to Everybody",
            sig,
            b"Hello, World!"
        ));
        assert!(!signed_github(
            b"It's a Secret to Everybody",
            sig,
            b"Hello, World?"
        ));
        assert!(!signed_github(b"another secret", sig, b"Hello, World!"));
        assert!(!signed_github(
            b"It's a Secret to Everybody",
            &sig[7..],
            b"Hello, World!"
        ));
        assert!(!signed_github(
            b"It's a Secret to Everybody",
            "sha256=zz",
            b"Hello, World!"
        ));
        assert!(!signed_github(b"x", "", b""));
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
        let s = new_secret();
        assert_eq!(s.len(), 64);
        assert!(s.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(s, new_secret());
    }

    /// The titles, links, text and colours recorded from Discord's GitHub webhook on 2026-11-20.
    #[test]
    fn github_look_as_recorded() {
        let commits: Vec<Value> = (1..=7)
            .map(|n| json!({"id": format!("{n:040x}"), "message": if n == 1 { "A very long commit message that goes on and on A very long commit\n\nbody".to_string() } else { format!("Commit {n}") },
                            "url": format!("https://github.com/Codertocat/Hello-World/commit/{n:040x}"), "author": {"name": "Codertocat"}}))
            .collect();
        let push = json!({"ref": "refs/heads/master", "repository": gh_repo(), "sender": sender(),
            "commits": commits, "head_commit": {"url": "https://github.com/Codertocat/Hello-World/commit/6113728"}});
        let o = github_post("push", &push).unwrap();
        assert_eq!((o.toggle, o.entry), ("push", "push"));
        assert_eq!(v(&o, "headline"), "[Hello-World:master] 7 new commits");
        assert_eq!(
            v(&o, "link"),
            "https://github.com/Codertocat/Hello-World/commit/6113728"
        );
        let lines: Vec<&str> = v(&o, "summary").lines().collect();
        assert_eq!(lines.len(), 5, "at most 5 commit lines");
        assert_eq!(lines[0], "[`0000000`](https://github.com/Codertocat/Hello-World/commit/0000000000000000000000000000000000000001) A very long commit message that goes on and on ... - Codertocat");
        assert_eq!(v(&o, "color"), "7506394");
        assert_eq!(v(&o, "author"), "Codertocat");
        assert_eq!(o.repo, "Codertocat/Hello-World");
        // A push without commits posts nothing.
        let mut empty = push.clone();
        empty["commits"] = json!([]);
        assert!(github_post("push", &empty).is_none());

        let pr = |action: &str| {
            json!({"action": action, "repository": gh_repo(), "sender": sender(),
            "pull_request": {"number": 2, "title": "Update the README with new information.", "body": "This is a pretty simple change that we need to pull into master.", "html_url": "https://github.com/Codertocat/Hello-World/pull/2"}})
        };
        let o = github_post("pull_request", &pr("opened")).unwrap();
        assert_eq!(v(&o, "headline"), "[Codertocat/Hello-World] Pull request opened: #2 Update the README with new information.");
        assert_eq!(
            v(&o, "summary"),
            "This is a pretty simple change that we need to pull into master."
        );
        assert_eq!(v(&o, "color"), "38912");
        let o = github_post("pull_request", &pr("closed")).unwrap();
        assert_eq!(v(&o, "headline"), "[Codertocat/Hello-World] Pull request closed: #2 Update the README with new information.");
        assert_eq!((v(&o, "summary"), v(&o, "color")), ("", ""));
        for quiet in ["synchronize", "ready_for_review", "labeled", "edited"] {
            assert!(github_post("pull_request", &pr(quiet)).is_none(), "{quiet}");
        }

        let issue = |action: &str, body: &str| {
            json!({"action": action, "repository": gh_repo(), "sender": sender(),
            "issue": {"number": 1, "title": "Spelling error in the README file", "body": body, "html_url": "https://github.com/Codertocat/Hello-World/issues/1"}})
        };
        let o = github_post("issues", &issue("opened", &"word ".repeat(200))).unwrap();
        assert_eq!(v(&o, "summary").chars().count(), 500);
        assert!(v(&o, "summary").ends_with("wo..."));
        assert_eq!(v(&o, "color"), "15426592");
        assert_eq!(
            v(
                &github_post("issues", &issue("reopened", "x")).unwrap(),
                "headline"
            ),
            "[Codertocat/Hello-World] Issue reopened: #1 Spelling error in the README file"
        );
        assert!(github_post("issues", &issue("deleted", "x")).is_none());

        let release = |action: &str| {
            json!({"action": action, "repository": gh_repo(), "sender": sender(),
            "release": {"tag_name": "0.0.1", "name": "Big", "body": "notes", "html_url": "https://github.com/Codertocat/Hello-World/releases/tag/0.0.1"}})
        };
        let o = github_post("release", &release("published")).unwrap();
        assert_eq!(
            v(&o, "headline"),
            "[Codertocat/Hello-World] New release published: 0.0.1"
        );
        assert_eq!(v(&o, "summary"), "");
        assert!(github_post("release", &release("created")).is_none());
        assert!(github_post("release", &release("prereleased")).is_none());

        let r#ref = json!({"ref": "simple-tag", "ref_type": "tag", "repository": gh_repo(), "sender": sender()});
        let o = github_post("create", &r#ref).unwrap();
        assert_eq!(
            (v(&o, "headline"), v(&o, "link")),
            ("[Codertocat/Hello-World] New tag created: simple-tag", "")
        );
        let o = github_post("delete", &r#ref).unwrap();
        assert_eq!((o.toggle, o.entry), ("create", "delete"));
        assert_eq!(
            v(&o, "headline"),
            "[Codertocat/Hello-World] tag deleted: simple-tag"
        );

        let mut c = json!({"action": "created", "repository": gh_repo(), "sender": sender(),
            "issue": {"number": 1, "title": "Spelling error in the README file"},
            "comment": {"body": "You are totally right! I'll get this fixed right away.", "html_url": "https://github.com/Codertocat/Hello-World/issues/1#issuecomment-492700400"}});
        let o = github_post("issue_comment", &c).unwrap();
        assert_eq!((o.toggle, v(&o, "color")), ("comments", "15109472"));
        assert_eq!(
            v(&o, "headline"),
            "[Codertocat/Hello-World] New comment on issue #1: Spelling error in the README file"
        );
        c["issue"]["pull_request"] = json!({});
        let o = github_post("issue_comment", &c).unwrap();
        assert_eq!(v(&o, "headline"), "[Codertocat/Hello-World] New comment on pull request #1: Spelling error in the README file");
        assert_eq!(v(&o, "color"), "12576191");
        c["action"] = json!("edited");
        assert!(github_post("issue_comment", &c).is_none());

        let review = |action: &str, state: &str| {
            json!({"action": action, "repository": gh_repo(), "sender": sender(),
            "pull_request": {"number": 2, "title": "Update the README with new information."},
            "review": {"state": state, "body": null, "html_url": "https://github.com/Codertocat/Hello-World/pull/2#pullrequestreview-237895671"}})
        };
        assert_eq!(
            v(
                &github_post("pull_request_review", &review("submitted", "approved")).unwrap(),
                "color"
            ),
            "38912"
        );
        assert_eq!(
            v(
                &github_post(
                    "pull_request_review",
                    &review("submitted", "changes_requested")
                )
                .unwrap(),
                "color"
            ),
            "16525609"
        );
        let o = github_post("pull_request_review", &review("dismissed", "dismissed")).unwrap();
        assert_eq!((o.toggle, v(&o, "color")), ("reviews", ""));
        assert_eq!(v(&o, "headline"), "[Codertocat/Hello-World] Pull request review dismissed: #2 Update the README with new information.");

        let star = json!({"action": "started", "repository": gh_repo(), "sender": sender()});
        assert_eq!(
            v(&github_post("watch", &star).unwrap(), "headline"),
            "[Codertocat/Hello-World] New star added"
        );
        assert!(github_post("star", &json!({"action": "created"})).is_none());

        let fork = json!({"repository": gh_repo(), "sender": sender(),
            "forkee": {"full_name": "Octocoders/Hello-World", "html_url": "https://github.com/Octocoders/Hello-World",
                       "owner": {"login": "Octocoders", "html_url": "https://github.com/Octocoders", "avatar_url": "https://avatars1.githubusercontent.com/u/38302899?v=4"}}});
        let o = github_post("fork", &fork).unwrap();
        assert_eq!(
            (v(&o, "headline"), v(&o, "author")),
            (
                "[Codertocat/Hello-World] Fork created: Octocoders/Hello-World",
                "Octocoders"
            )
        );

        let suite = |conclusion: &str, prs: Value| {
            json!({"action": "completed", "repository": gh_repo(), "sender": sender(),
            "check_suite": {"conclusion": conclusion, "app": {"name": "octocoders-linter"}, "pull_requests": prs, "head_branch": "changes", "head_sha": "ec26c3e"}})
        };
        let o = github_post("check_suite", &suite("failure", json!([{"number": 2}]))).unwrap();
        assert_eq!(
            v(&o, "headline"),
            "[Hello-World] octocoders-linter checks failure on pull request #2"
        );
        assert_eq!(
            (v(&o, "link"), v(&o, "author"), v(&o, "color")),
            (
                "https://github.com/Codertocat/Hello-World/pull/2",
                "",
                "16525609"
            )
        );
        let o = github_post("check_suite", &suite("timed_out", json!([]))).unwrap();
        assert_eq!(
            v(&o, "headline"),
            "[Hello-World] octocoders-linter checks timed out on changes"
        );
        assert_eq!(
            v(&o, "link"),
            "https://github.com/Codertocat/Hello-World/commit/ec26c3e"
        );
        assert!(
            github_post("check_suite", &suite("success", json!([]))).is_none(),
            "failures only"
        );

        let public = json!({"repository": gh_repo(), "sender": sender()});
        assert_eq!(
            v(&github_post("public", &public).unwrap(), "headline"),
            "[Codertocat/Hello-World] Now open sourced!"
        );
        for none in ["ping", "gollum", "workflow_run", "label", "check_run"] {
            assert!(github_post(none, &public).is_none(), "{none}");
        }
        // Only web links reach the embed: Discord refuses the whole message for another kind.
        let mut odd = pr("opened");
        odd["pull_request"]["html_url"] = json!("javascript:alert(1)");
        odd["sender"]["avatar_url"] = json!("data:image/png;base64,AA");
        let o = github_post("pull_request", &odd).unwrap();
        assert_eq!((v(&o, "link"), v(&o, "author_icon")), ("", ""));
    }

    #[test]
    fn gitlab_events() {
        let project = json!({"path_with_namespace": "group/app", "web_url": "https://gitlab.example/group/app",
                             "avatar_url": "https://gitlab.example/uploads/a.png"});
        let push = json!({"ref": "refs/heads/main", "checkout_sha": "b".repeat(40), "user_username": "jo",
            "user_avatar": "https://gitlab.example/u/jo.png", "project": project, "total_commits_count": 2,
            "commits": [{"id": "a".repeat(40), "message": "First\nmore", "url": "https://gitlab.example/group/app/-/commit/a", "author": {"name": "Jo"}},
                        {"id": "b".repeat(40), "message": "Second", "url": "https://gitlab.example/group/app/-/commit/b", "author": {"name": "Jo"}}]});
        let o = gitlab_post("Push Hook", &push).unwrap();
        assert_eq!((o.toggle, o.entry), ("push", "push"));
        assert_eq!(v(&o, "headline"), "[app:main] 2 new commits");
        assert_eq!(v(&o, "link"), "https://gitlab.example/group/app/-/commit/b");
        assert_eq!(
            v(&o, "summary").lines().next().unwrap(),
            "[`aaaaaaa`](https://gitlab.example/group/app/-/commit/a) First - Jo"
        );
        assert_eq!(
            (v(&o, "author"), v(&o, "author_link")),
            ("jo", "https://gitlab.example/jo")
        );
        assert_eq!(o.picture, "https://gitlab.example/uploads/a.png");

        let mr = |action: &str| {
            json!({"project": project, "user": {"username": "jo", "avatar_url": "https://gitlab.example/u/jo.png"},
            "object_attributes": {"action": action, "iid": 5, "title": "Fix", "description": "Body", "url": "https://gitlab.example/group/app/-/merge_requests/5"}})
        };
        let o = gitlab_post("Merge Request Hook", &mr("open")).unwrap();
        assert_eq!(
            v(&o, "headline"),
            "[group/app] Merge request opened: !5 Fix"
        );
        assert_eq!((v(&o, "summary"), v(&o, "color")), ("Body", "38912"));
        assert_eq!(
            v(
                &gitlab_post("Merge Request Hook", &mr("merge")).unwrap(),
                "headline"
            ),
            "[group/app] Merge request closed: !5 Fix"
        );
        assert!(gitlab_post("Merge Request Hook", &mr("update")).is_none());

        // Confidential issues and notes never reach Discord.
        let mut issue = mr("open");
        let o = gitlab_post("Issue Hook", &issue).unwrap();
        assert_eq!(v(&o, "headline"), "[group/app] Issue opened: #5 Fix");
        issue["object_attributes"]["confidential"] = json!(true);
        assert!(gitlab_post("Issue Hook", &issue).is_none());
        assert!(gitlab_post("Confidential Issue Hook", &mr("open")).is_none());
        let mut note = json!({"project": project, "user": {"username": "jo"},
            "object_attributes": {"noteable_type": "Issue", "note": "Hi", "url": "https://gitlab.example/n"},
            "issue": {"iid": 3, "title": "Bug"}});
        let o = gitlab_post("Note Hook", &note).unwrap();
        assert_eq!(
            (o.toggle, v(&o, "headline")),
            ("comments", "[group/app] New comment on issue #3: Bug")
        );
        note["issue"]["confidential"] = json!(true);
        assert!(gitlab_post("Note Hook", &note).is_none());
        assert!(gitlab_post("Confidential Note Hook", &note).is_none());

        let tag = |after: &str| json!({"ref": "refs/tags/v1", "after": after, "project": project, "user_username": "jo"});
        assert_eq!(
            gitlab_post("Tag Push Hook", &tag("abc")).unwrap().entry,
            "create"
        );
        let o = gitlab_post("Tag Push Hook", &tag(&"0".repeat(40))).unwrap();
        assert_eq!(
            (o.entry, v(&o, "headline")),
            ("delete", "[group/app] tag deleted: v1")
        );

        let pipe = |status: &str| json!({"project": project, "object_attributes": {"status": status, "ref": "main", "id": 9}, "user": {"username": "jo"}});
        let o = gitlab_post("Pipeline Hook", &pipe("failed")).unwrap();
        assert_eq!(
            (o.toggle, o.entry, v(&o, "author")),
            ("ci", "check_suite", "")
        );
        assert_eq!(
            v(&o, "link"),
            "https://gitlab.example/group/app/-/pipelines/9"
        );
        assert!(gitlab_post("Pipeline Hook", &pipe("success")).is_none());
        assert!(gitlab_post("Wiki Page Hook", &pipe("x")).is_none());
    }

    fn signed(secret: &str, body: &[u8], event: &str, delivery: &str) -> HeaderMap {
        use hmac::{Hmac, KeyInit, Mac};
        let mut m = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        m.update(body);
        let sig: String = m
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut h = HeaderMap::new();
        h.insert(
            "x-hub-signature-256",
            format!("sha256={sig}").parse().unwrap(),
        );
        h.insert("x-github-event", event.parse().unwrap());
        h.insert("x-github-delivery", delivery.parse().unwrap());
        h.insert("content-type", "application/json".parse().unwrap());
        h
    }

    /// The endpoint on a real database: signature, toggles, repeated deliveries, the limit.
    #[tokio::test]
    async fn receives_only_signed_events_once() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_496_000_000).await else {
            return;
        };
        let secret = new_secret();
        let id: i64 = sqlx::query_scalar(
            "insert into feeds (guild_id, channel_id, kind, target, created_by, secret, events)
             values ($1, 1, 'github', '', 7, $2, '{push,issues}') returning id",
        )
        .bind(g)
        .bind(&secret)
        .fetch_one(&st.db)
        .await
        .unwrap();
        let queued = || async {
            sqlx::query_scalar::<_, String>(
                "select event from feed_events where feed_id = $1 order by id",
            )
            .bind(id)
            .fetch_all(&st.db)
            .await
            .unwrap()
        };
        let star = serde_json::to_vec(
            &json!({"action": "started", "repository": gh_repo(), "sender": sender()}),
        )
        .unwrap();
        let issue = serde_json::to_vec(&json!({"action": "opened", "repository": gh_repo(), "sender": sender(),
            "issue": {"number": 1, "title": "T", "body": "B", "html_url": "https://github.com/x/y/issues/1"}})).unwrap();
        // Wrong secret, no signature, unknown feed: refused alike.
        assert!(matches!(
            take(&st, id, &signed("nope", &issue, "issues", "d1"), &issue).await,
            Err(Taken::No)
        ));
        let mut bare = signed(&secret, &issue, "issues", "d1");
        bare.remove("x-hub-signature-256");
        assert!(matches!(take(&st, id, &bare, &issue).await, Err(Taken::No)));
        assert!(matches!(
            take(
                &st,
                id + 1_000_000,
                &signed(&secret, &issue, "issues", "d1"),
                &issue
            )
            .await,
            Err(Taken::No)
        ));
        assert!(queued().await.is_empty());
        // Signed and switched on: queued once, with the "connected" note first; a repeat is ignored.
        for _ in 0..2 {
            assert!(
                take(&st, id, &signed(&secret, &issue, "issues", "d1"), &issue)
                    .await
                    .is_ok()
            );
        }
        assert_eq!(queued().await, ["ping", "issues"]);
        // The same signed body under a new delivery id (a replay): ignored too.
        assert!(take(
            &st,
            id,
            &signed(&secret, &issue, "issues", "d1-again"),
            &issue
        )
        .await
        .is_ok());
        assert_eq!(queued().await, ["ping", "issues"]);
        // Signed but switched off: answered, not queued.
        assert!(take(&st, id, &signed(&secret, &star, "watch", "d2"), &star)
            .await
            .is_ok());
        assert_eq!(queued().await.len(), 2);
        // A form-encoded GitHub hook is told what to change.
        let mut form = signed(&secret, &issue, "issues", "d3");
        form.insert(
            "content-type",
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        assert!(matches!(
            take(&st, id, &form, &issue).await,
            Err(Taken::Bad(_))
        ));
        // 30 a minute; the rest are counted for the log.
        for n in 0..32 {
            let body = serde_json::to_vec(&json!({"action": "opened", "repository": gh_repo(), "sender": sender(),
                "issue": {"number": n + 100, "title": "T", "body": "B", "html_url": "https://github.com/x/y/issues/1"}})).unwrap();
            take(
                &st,
                id,
                &signed(&secret, &body, "issues", &format!("x{n}")),
                &body,
            )
            .await
            .ok()
            .unwrap();
        }
        let (n, dropped): (i64, i32) = sqlx::query_as(
            "select (select count(*) from feed_events where feed_id = $1 and event <> 'ping'), dropped from feeds where id = $1",
        )
        .bind(id)
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert_eq!((n, dropped), (30, 3));
        // A paused feed takes nothing.
        sqlx::query("update feeds set state = 'paused' where id = $1")
            .bind(id)
            .execute(&st.db)
            .await
            .unwrap();
        assert!(matches!(
            take(&st, id, &signed(&secret, &issue, "issues", "p1"), &issue).await,
            Err(Taken::No)
        ));
        sqlx::query("delete from feeds where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
    }
}

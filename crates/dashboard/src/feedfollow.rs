//! Followed GitHub repositories: a GitHub feed with no webhook (no secret). Anyone can follow a
//! public repository this way, with no admin rights on it.
//!
//! Every 30 s the dashboard reads the due feeds (each 10 minutes, or asked for by Check now) from
//! GitHub's public list of the repository's events, and turns each new event into the body the
//! webhook would have sent. Then the same `feedhooks::github_post` makes the post, and the same
//! `feed_events` queue takes it, so a followed repository posts the same events, with the same
//! switches and the same look, as a webhook feed. The bot posts the queue.
//!
//! Limits (said in the dialog): GitHub's list can be some minutes late, it holds only the newest
//! events, and it has no check results (CI). Without `GITHUB_TOKEN`, GitHub allows 60 requests an
//! hour from the server; a check with no new events costs nothing (conditional request).
//! Requests start only at api.github.com, through the guarded feed client (a redirect must
//! still go to a public https address).
use crate::{feedhooks, AppState};
use brim_automod::feed;
use reqwest::{header, StatusCode};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::OnceLock, time::Duration};

const API: &str = "https://api.github.com/";
/// Failed checks in a row before the feed stops, as for RSS feeds.
const MAX_FAILS: i32 = 5;
/// Events turned into posts per check: each can cost one more request.
const PER_CHECK: usize = 20;

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(feed::client)
}

/// `owner/repo` from a pasted link or from `owner/repo` itself; `None` when it names no repository.
pub fn repo_of(input: &str) -> Option<String> {
    let t = input.trim().trim_end_matches('/');
    let rest = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t);
    let rest = rest
        .strip_prefix("www.github.com/")
        .or_else(|| rest.strip_prefix("github.com/"))
        .unwrap_or(rest);
    let mut parts = rest.split(['/', '?', '#']);
    let owner = parts.next()?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p != "."
            && p != ".."
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

/// The answer of one GitHub API request.
enum Got {
    Json(Value, Option<String>),
    Same,
}

/// Why a request failed, in words for the feed's row. `Limit`: GitHub's hourly limit is used up.
enum Why {
    Limit,
    /// 404, 410 or 422: the thing is gone (a deleted comment, a force-pushed branch).
    Gone,
    Other(String),
}

async fn get(path: &str, etag: Option<&str>) -> Result<Got, Why> {
    // Only GitHub's API: `path` comes from Brim or from GitHub's own answer, checked here.
    let url = if path.starts_with(API) {
        path.to_string()
    } else {
        format!("{API}{path}")
    };
    let only_api = reqwest::Url::parse(&url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some("api.github.com")
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
    });
    if !only_api {
        return Err(Why::Other(
            "GitHub sent an address Brim does not follow".into(),
        ));
    }
    let mut rq = client()
        .get(&url)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Ok(t) = std::env::var("GITHUB_TOKEN") {
        if !t.trim().is_empty() {
            rq = rq.bearer_auth(t.trim());
        }
    }
    if let Some(e) = etag {
        rq = rq.header(header::IF_NONE_MATCH, e);
    }
    let r = rq
        .send()
        .await
        .map_err(|_| Why::Other("GitHub could not be reached".into()))?;
    let left = r
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok());
    match r.status() {
        StatusCode::NOT_MODIFIED => return Ok(Got::Same),
        StatusCode::NOT_FOUND | StatusCode::GONE | StatusCode::UNPROCESSABLE_ENTITY => {
            return Err(Why::Gone)
        }
        // The hourly limit, or GitHub's limit on requests in a short time.
        StatusCode::TOO_MANY_REQUESTS => return Err(Why::Limit),
        StatusCode::FORBIDDEN if left == Some(0) || r.headers().contains_key("retry-after") => {
            return Err(Why::Limit)
        }
        StatusCode::UNAUTHORIZED => {
            return Err(Why::Other(
                "GitHub refused GITHUB_TOKEN: fix it or remove it".into(),
            ))
        }
        s if !s.is_success() => return Err(Why::Other(format!("GitHub answered {}", s.as_u16()))),
        _ => {}
    }
    let etag = r
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = brim_automod::sites::read_capped(r, 4 << 20)
        .await
        .map_err(|_| Why::Other("GitHub's answer is larger than 4 MB".into()))?;
    let v = serde_json::from_slice(&body)
        .map_err(|_| Why::Other("GitHub's answer could not be read".into()))?;
    Ok(Got::Json(v, etag))
}

async fn json(path: &str) -> Result<Value, Why> {
    match get(path, None).await? {
        Got::Json(v, _) => Ok(v),
        Got::Same => Err(Why::Other("GitHub sent no answer".into())),
    }
}

/// Checks the repository when a feed is added: (owner/repo as GitHub spells it, owner's picture).
pub async fn check_repo(repo: &str) -> Result<(String, String), String> {
    let v = json(&format!("repos/{repo}")).await.map_err(|w| match w {
        Why::Limit => "GitHub's hourly limit for Brim is used up: try again in an hour".into(),
        Why::Gone => "GitHub has no public repository with that name".into(),
        Why::Other(e) => e,
    })?;
    if v["private"] == true {
        return Err("GitHub has no public repository with that name".into());
    }
    let name = v["full_name"].as_str().unwrap_or(repo).to_string();
    let picture = v["owner"]["avatar_url"].as_str().unwrap_or("").to_string();
    Ok((name, picture))
}

/// One user as the webhook sends it, from an event's `actor`.
fn user(a: &Value) -> Value {
    let login = a["login"].as_str().unwrap_or("");
    json!({
        "login": login,
        "html_url": format!("https://github.com/{login}"),
        "avatar_url": a["avatar_url"],
    })
}

/// The webhook's name for an event of GitHub's public list, and its action as the webhook says it.
fn webhook_name(kind: &str, action: &str) -> Option<(&'static str, String)> {
    let name = match kind {
        "PushEvent" => "push",
        "PullRequestEvent" => "pull_request",
        "IssuesEvent" => "issues",
        "IssueCommentEvent" => "issue_comment",
        "CommitCommentEvent" => "commit_comment",
        "PullRequestReviewEvent" => "pull_request_review",
        "PullRequestReviewCommentEvent" => "pull_request_review_comment",
        "ReleaseEvent" => "release",
        "CreateEvent" => "create",
        "DeleteEvent" => "delete",
        "WatchEvent" => "watch",
        "ForkEvent" => "fork",
        "MemberEvent" => "member",
        "PublicEvent" => "public",
        "DiscussionEvent" => "discussion",
        _ => return None,
    };
    // The list says "merged" where the webhook says "closed" (with merged true on the pull request).
    let action = match (name, action) {
        ("pull_request", "merged") => "closed",
        // The list says "created" for a review; the webhook says "submitted".
        ("pull_request_review", "created") => "submitted",
        _ => action,
    };
    Some((name, action.to_string()))
}

/// The body the webhook would have sent for one event of the list. The list leaves out most of each
/// object, so each one is read again from its own `url` (one request each).
async fn body(e: &Value, repo: &Value) -> Result<Option<(&'static str, Value)>, Why> {
    let kind = e["type"].as_str().unwrap_or("");
    let mut p = e["payload"].clone();
    let Some((name, action)) = webhook_name(kind, p["action"].as_str().unwrap_or("")) else {
        return Ok(None);
    };
    if !p.is_object() {
        return Ok(None);
    }
    p["action"] = json!(action);
    p["repository"] = repo.clone();
    p["sender"] = user(&e["actor"]);
    for k in [
        "pull_request",
        "issue",
        "comment",
        "review",
        "release",
        "forkee",
        "member",
    ] {
        let part = &p[k];
        if part.is_object() && part["html_url"].is_null() {
            if let Some(u) = part["url"].as_str() {
                let full = json(u).await?;
                p[k] = full;
            }
        }
    }
    if name == "push" {
        let full = repo["full_name"].as_str().unwrap_or("");
        let (before, head) = (
            p["before"].as_str().unwrap_or("").to_string(),
            p["head"].as_str().unwrap_or("").to_string(),
        );
        let zero = |s: &str| s.chars().all(|c| c == '0');
        let hex = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());
        if !hex(&before) || !hex(&head) {
            return Ok(None);
        }
        // A deleted branch has no commits; a new branch shows its newest commit.
        let commits: Vec<Value> = if zero(&head) {
            Vec::new()
        } else if zero(&before) {
            vec![json(&format!("repos/{full}/commits/{head}")).await?]
        } else {
            json(&format!("repos/{full}/compare/{before}...{head}")).await?["commits"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        let commits: Vec<Value> = commits
            .iter()
            .map(|c| {
                json!({
                    "id": c["sha"],
                    "url": c["html_url"],
                    "message": c["commit"]["message"],
                    "author": {"name": c["commit"]["author"]["name"], "username": c["author"]["login"]},
                })
            })
            .collect();
        p["head_commit"] = commits.last().cloned().unwrap_or(Value::Null);
        p["commits"] = json!(commits);
        p["after"] = json!(head);
        p["compare"] = json!(format!(
            "https://github.com/{full}/compare/{}...{}",
            before.get(..12).unwrap_or(&before),
            head.get(..12).unwrap_or(&head)
        ));
    }
    Ok(Some((name, p)))
}

#[derive(sqlx::FromRow)]
struct Due {
    id: i64,
    target: String,
    events: Vec<String>,
    primed: bool,
    asked: Option<String>,
    fails: i32,
    source_picture: Option<String>,
}

/// Checks the due followed repositories every 30 s; never returns.
pub async fn run(st: AppState) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut etags: HashMap<i64, String> = HashMap::new();
    loop {
        tick.tick().await;
        if let Err(e) = round(&st, &mut etags).await {
            tracing::warn!(?e, "followed repository round failed");
        }
    }
}

async fn round(st: &AppState, etags: &mut HashMap<i64, String>) -> anyhow::Result<()> {
    let due: Vec<Due> = sqlx::query_as(
        "select id, target, events, primed, asked, fails, source_picture from feeds
         where kind = 'github' and secret is null
           and ((state <> 'paused' and asked is not null)
             or (state = 'on' and (last_check is null or last_check < now() - interval '10 minutes')))
         order by last_check nulls first limit 50",
    )
    .fetch_all(&st.db)
    .await?;
    for r in due {
        let res = check(st, &r, etags).await;
        let (error, limit) = match &res {
            Ok(()) => (None, false),
            Err(Why::Limit) => (
                Some(
                    "GitHub's hourly limit for Brim is used up: Brim checks again later"
                        .to_string(),
                ),
                true,
            ),
            Err(Why::Other(e)) => (Some(e.clone()), false),
            Err(Why::Gone) => (
                Some("GitHub has no public repository with that name".to_string()),
                false,
            ),
        };
        // The hourly limit is not the feed's fault: it does not count toward stopping it.
        let stop = error.is_some() && !limit && r.fails + 1 >= MAX_FAILS;
        sqlx::query(
            "update feeds set last_check = now(), last_error = $2,
                    asked = case when asked is not distinct from $5 then null else asked end,
                    fails = case when $2::text is null then 0 when $4 then fails else fails + 1 end,
                    state = case when $3 and state = 'on' then 'broken'
                                 when $2::text is null and state = 'broken' then 'on'
                                 else state end
             where id = $1",
        )
        .bind(r.id)
        .bind(&error)
        .bind(stop)
        .bind(limit)
        .bind(&r.asked)
        .execute(&st.db)
        .await?;
        if limit {
            break;
        }
    }
    Ok(())
}

/// Reads one repository's new events and queues their posts.
async fn check(st: &AppState, r: &Due, etags: &mut HashMap<i64, String>) -> Result<(), Why> {
    let db = |e: sqlx::Error| Why::Other(format!("the database failed: {e}"));
    let path = format!("repos/{}/events?per_page=100", r.target);
    // A first check reads the whole list; otherwise only a changed one.
    let etag = (r.primed).then(|| etags.get(&r.id).cloned()).flatten();
    let got = get(&path, etag.as_deref()).await.map_err(|w| match w {
        Why::Gone => Why::Other("GitHub has no public repository with that name".into()),
        w => w,
    })?;
    let (list, tag) = match got {
        Got::Same => return Ok(()),
        Got::Json(v, tag) => (v, tag),
    };
    let Some(list) = list.as_array() else {
        return Err(Why::Other("GitHub's answer could not be read".into()));
    };
    let seen: Vec<String> = sqlx::query_scalar("select item from feed_seen where feed_id = $1")
        .bind(r.id)
        .fetch_all(&st.db)
        .await
        .map_err(db)?;
    // Newest first in the list: post oldest first.
    let new: Vec<&Value> = list
        .iter()
        .filter(|e| {
            e["id"]
                .as_str()
                .is_some_and(|id| !seen.iter().any(|s| s == &format!("gh:{id}")))
        })
        .collect();
    let remember = |id: String| {
        sqlx::query("insert into feed_seen (feed_id, item) values ($1, $2) on conflict do nothing")
            .bind(r.id)
            .bind(id)
            .execute(&st.db)
    };
    if !r.primed {
        // The first check remembers what is there and posts nothing, as for RSS feeds.
        for e in &new {
            remember(format!("gh:{}", e["id"].as_str().unwrap_or("")))
                .await
                .map_err(db)?;
        }
        sqlx::query("update feeds set primed = true where id = $1")
            .bind(r.id)
            .execute(&st.db)
            .await
            .map_err(db)?;
    } else {
        let full = r.target.clone();
        let owner = full.split('/').next().unwrap_or("");
        let repo = json!({
            "full_name": full,
            "name": full.split('/').nth(1).unwrap_or(""),
            "html_url": format!("https://github.com/{full}"),
            "owner": {"login": owner, "avatar_url": r.source_picture.clone()
                .unwrap_or_else(|| format!("https://github.com/{owner}.png"))},
        });
        // Over PER_CHECK new events: the oldest are skipped, as the webhook's limit skips some.
        let skip = new.len().saturating_sub(PER_CHECK);
        for (i, e) in new.iter().rev().enumerate() {
            let id = e["id"].as_str().unwrap_or("").to_string();
            if i >= skip {
                // Gone since (a deleted comment, a force-pushed branch): only that event is skipped,
                // so it cannot stop the feed. Any other failure stops the check; it is read again.
                let read = match body(e, &repo).await {
                    Err(Why::Gone) => None,
                    Err(w) => return Err(w),
                    Ok(b) => b,
                };
                if let Some((name, p)) = read {
                    if let Some(out) = feedhooks::github_post(name, &p)
                        .filter(|o| r.events.iter().any(|t| t == o.toggle))
                    {
                        feedhooks::queue(st, r.id, &format!("gh:{id}"), out.entry, &out.vars, None)
                            .await
                            .map_err(db)?;
                    }
                }
            }
            // Remembered only once queued or skipped: a stopped check reads it again.
            remember(format!("gh:{id}")).await.map_err(db)?;
        }
        // Keep the newest 300 remembered events.
        sqlx::query(
            "delete from feed_seen where feed_id = $1 and item not in
               (select item from feed_seen where feed_id = $1 order by seen_at desc limit 300)",
        )
        .bind(r.id)
        .execute(&st.db)
        .await
        .map_err(db)?;
    }
    if let Some(t) = tag {
        etags.insert(r.id, t);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names() {
        for (input, want) in [
            ("https://github.com/rust-lang/rust", Some("rust-lang/rust")),
            ("github.com/a/b.git", Some("a/b")),
            ("https://www.github.com/a/b/tree/main", Some("a/b")),
            ("a/b", Some("a/b")),
            ("https://github.com/a", None),
            ("https://github.com/../x", None),
            ("https://github.com/a/b%2f", None),
            ("", None),
        ] {
            assert_eq!(repo_of(input).as_deref(), want, "{input}");
        }
    }

    #[test]
    fn list_names_match_the_webhook() {
        assert_eq!(
            webhook_name("PullRequestEvent", "merged"),
            Some(("pull_request", "closed".into()))
        );
        assert_eq!(
            webhook_name("PullRequestReviewEvent", "created"),
            Some(("pull_request_review", "submitted".into()))
        );
        assert_eq!(
            webhook_name("WatchEvent", "started"),
            Some(("watch", "started".into()))
        );
        assert_eq!(webhook_name("GollumEvent", ""), None);
    }

    /// A push read from the list makes the webhook's push post.
    #[tokio::test]
    async fn a_listed_push_posts_like_the_webhook() {
        let repo = json!({"full_name": "a/b", "name": "b", "html_url": "https://github.com/a/b",
                          "owner": {"login": "a", "avatar_url": "https://github.com/a.png"}});
        // A deleted branch: no request, no post.
        let e = json!({"type": "PushEvent", "actor": {"login": "x"},
                       "payload": {"ref": "refs/heads/old", "before": "abc1", "head": "0000"}});
        let (name, p) = body(&e, &repo).await.ok().flatten().unwrap();
        assert_eq!(name, "push");
        assert!(feedhooks::github_post(name, &p).is_none());
        // A star needs no request and posts as the webhook's star.
        let e = json!({"type": "WatchEvent", "actor": {"login": "x", "avatar_url": "https://a.example/x.png"},
                       "payload": {"action": "started"}});
        let (name, p) = body(&e, &repo).await.ok().flatten().unwrap();
        let out = feedhooks::github_post(name, &p).unwrap();
        assert_eq!(out.toggle, "star");
        assert_eq!(out.vars["author"], "x");
        assert_eq!(out.vars["headline"], "[a/b] New star added");
    }
}

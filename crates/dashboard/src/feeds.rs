//! Feeds page: RSS/Atom and YouTube feeds a server follows. Anyone
//! with dashboard access may look; Owner, Extra Owners and Trusted Admins add, change, pause and
//! delete (choice P1), each change audited. The bot polls and posts (`bot/src/feeds.rs`); Send latest
//! and the check after a feed is turned on ask it through `feeds.asked`. The webhook token never reaches this page.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    embeds::discord,
    lockdown::audit,
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use brim_automod::feed::{self, Got, YouTube};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// Feeds per server and per channel (choice L1).
pub const MAX_FEEDS: i64 = 10;
pub const MAX_PER_CHANNEL: i64 = 3;

/// Fetches at the same time from this page (each one is a request to a site staff named).
static FETCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(feed::client)
}

pub fn routes() -> Router<AppState> {
    let g = "/api/guilds/{guild}/feeds";
    Router::new()
        .route(g, get(list).post(add))
        .route(&format!("{g}/{{id}}"), post(save))
        .route(&format!("{g}/{{id}}/state"), post(state))
        .route(&format!("{g}/{{id}}/ask"), post(ask))
        .route(&format!("{g}/{{id}}/message"), post(message))
        .route(&format!("{g}/{{id}}/delete"), post(delete))
        .route(&format!("{g}/{{id}}/secret"), post(secret))
}

#[derive(sqlx::FromRow)]
struct FeedRow {
    id: i64,
    kind: String,
    target: String,
    channel_id: i64,
    ping_role: Option<i64>,
    name: Option<String>,
    picture: Option<String>,
    source_name: String,
    source_picture: Option<String>,
    send_as_bot: bool,
    message: Option<Value>,
    state: String,
    last_check: Option<f64>,
    last_post: Option<f64>,
    last_error: Option<String>,
    asked: Option<String>,
    events: Vec<String>,
    primed: bool,
    webhook: bool,
}

/// GitHub and GitLab feeds: events come in, nothing is fetched.
fn hooked(kind: &str) -> bool {
    matches!(kind, "github" | "gitlab")
}

async fn rows(st: &AppState, guild: i64, id: Option<i64>) -> Result<Vec<Value>, ApiError> {
    let r: Vec<FeedRow> = sqlx::query_as(
        "select id, kind, target, channel_id, ping_role, name, picture, source_name, source_picture,
                send_as_bot, message, state, extract(epoch from last_check)::float8 as last_check,
                extract(epoch from last_post)::float8 as last_post, last_error,
                case when kind in ('github', 'gitlab') and secret is not null then null else asked end as asked,
                events, primed,
                secret is not null as webhook
         from feeds where guild_id = $1 and ($2::bigint is null or id = $2) order by id",
    )
    .bind(guild)
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    let base = crate::msghooks::base(st);
    Ok(r.into_iter()
        .map(|r| {
            // The secret is never listed: it is shown once, when it is made.
            let hook = (hooked(&r.kind) && r.webhook).then(|| format!("{base}/hooks/feed/{}", r.id));
            let entry = (!hooked(&r.kind)).then(|| format!("feed.{}", r.kind));
            json!({
                "id": r.id, "kind": r.kind, "target": r.target, "channel": r.channel_id.to_string(),
                "ping_role": r.ping_role.map(|p| p.to_string()), "name": r.name, "picture": r.picture,
                "source_name": r.source_name, "source_picture": r.source_picture,
                "send_as_bot": r.send_as_bot, "message": r.message, "state": r.state,
                "last_check": r.last_check.map(|t| t as i64), "last_post": r.last_post.map(|t| t as i64),
                "last_error": r.last_error, "asked": r.asked, "entry": entry,
                "events": r.events, "connected": r.primed, "hook_url": hook, "webhook": r.webhook,
            })
        })
        .collect())
}

async fn list(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    // Admin like the page: a private feed address can hold a key.
    api::require(&st, &s, guild, Level::Admin, false).await?;
    Ok(Json(json!({
        "feeds": rows(&st, guild, None).await?,
        "max": MAX_FEEDS, "per_channel": MAX_PER_CHANNEL,
    })))
}

fn bad(s: impl Into<String>) -> ApiError {
    ApiError::BadRequest(s.into())
}

fn id_of(v: &Option<String>) -> Result<Option<i64>, ApiError> {
    v.as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>()
                .map_err(|_| bad("that is not a Discord id"))
        })
        .transpose()
}

/// A text channel or announcement channel of this server that is not one of Brim's own.
async fn check_channel(st: &AppState, guild: i64, channel: i64) -> Result<(), ApiError> {
    let ch = discord(st, Method::GET, &format!("/channels/{channel}"), None)
        .await?
        .unwrap_or_default();
    // The bot is in many servers: never let an editor of this one post into another's channel.
    if ch["guild_id"].as_str().and_then(|g| g.parse().ok()) != Some(guild) {
        return Err(bad("that channel is not in this server"));
    }
    if !matches!(ch["type"].as_u64(), Some(0 | 5)) {
        return Err(bad("pick a text or announcement channel"));
    }
    let (_, config) = crate::store::current(&st.db, guild).await?;
    let parent: Vec<i64> = ch["parent_id"]
        .as_str()
        .and_then(|p| p.parse().ok())
        .into_iter()
        .collect();
    if crate::embeds::bot_channel(&config, channel, &parent) {
        return Err(bad(
            "that is one of Brim's own channels (logs, appeals, tickets or verification): pick another",
        ));
    }
    Ok(())
}

/// A role of this server to ping, not @everyone.
async fn check_role(st: &AppState, guild: i64, role: Option<i64>) -> Result<(), ApiError> {
    let Some(role) = role else { return Ok(()) };
    if role == guild {
        return Err(bad("@everyone cannot be the ping role"));
    }
    if st
        .discord
        .role_name(guild, &role.to_string())
        .await?
        .is_none()
    {
        return Err(bad("that role is not in this server"));
    }
    Ok(())
}

/// The owner's own webhook name and picture, cleaned: (name, picture), each None for the source's.
fn check_look(
    st: &AppState,
    name: &Option<String>,
    picture: &Option<String>,
) -> Result<(Option<String>, Option<String>), ApiError> {
    let name = name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let picture = picture.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let v = crate::msghooks::clean(
        &json!({"name": name.unwrap_or("Feed"), "avatar": picture}),
        crate::msghooks::base(st),
    )
    .map_err(ApiError::BadRequest)?;
    Ok((
        name.map(String::from),
        v["avatar"].as_str().map(String::from),
    ))
}

/// Room for one more feed in the server and in `channel` (`except`: the feed being moved).
async fn check_room(
    st: &AppState,
    guild: i64,
    channel: i64,
    except: Option<i64>,
) -> Result<(), ApiError> {
    let (all, here): (i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where channel_id = $2) from feeds
         where guild_id = $1 and id is distinct from $3",
    )
    .bind(guild)
    .bind(channel)
    .bind(except)
    .fetch_one(&st.db)
    .await?;
    if except.is_none() && all >= MAX_FEEDS {
        return Err(bad(format!(
            "this server already has {MAX_FEEDS} feeds: delete one first"
        )));
    }
    if here >= MAX_PER_CHANNEL {
        return Err(bad(format!(
            "that channel already has {MAX_PER_CHANNEL} feeds: pick another channel"
        )));
    }
    Ok(())
}

/// What the owner typed, checked and read once: (target, source name, source picture).
async fn read_source(
    kind: &str,
    input: &str,
) -> Result<(String, String, Option<String>), ApiError> {
    let _slot = FETCHES.acquire().await.map_err(anyhow::Error::from)?;
    let c = client();
    let (target, url, mut name, picture) = match kind {
        "rss" => {
            let u = feed::check_url(input).map_err(ApiError::BadRequest)?;
            (u.to_string(), u.to_string(), String::new(), None)
        }
        "youtube" => {
            let (id, name, pic) = match feed::youtube_input(input).map_err(ApiError::BadRequest)? {
                YouTube::Id(id) => (id, String::new(), None),
                YouTube::Page(p) => feed::youtube_page(c, &p)
                    .await
                    .map_err(ApiError::BadRequest)?,
            };
            (id.clone(), feed::youtube_feed(&id), name, pic)
        }
        _ => return Err(bad("pick RSS, YouTube, GitHub or GitLab")),
    };
    // A website's own address works too: Brim reads the feed the page names.
    let got = if kind == "rss" {
        feed::find(c, &url).await.map(|(a, f)| (a, Got::Feed(f)))
    } else {
        feed::fetch(c, &url, None, None).await.map(|g| (target, g))
    };
    let (target, f) = match got {
        Ok((a, Got::Feed(f))) => (a, f),
        Ok((_, Got::Same)) => return Err(bad("the site did not send the feed")),
        Err(e) if kind == "youtube" && e.contains("404") => {
            return Err(bad("YouTube has no channel with that id"))
        }
        Err(e) => return Err(bad(format!("Brim could not read that feed: {e}"))),
    };
    if !f.title.is_empty() {
        name = f.title;
    }
    Ok((target, name, f.picture.or(picture)))
}

#[derive(Deserialize)]
struct Add {
    kind: String,
    target: String,
    channel: String,
    #[serde(default)]
    ping_role: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
    #[serde(default)]
    send_as_bot: bool,
    /// GitHub and GitLab: the event types that post; none given = the defaults.
    #[serde(default)]
    events: Option<Vec<String>>,
    /// GitHub: through a webhook (the repository's admin sets it up). False: Brim follows the
    /// public repository itself (`feedfollow`).
    #[serde(default)]
    webhook: bool,
}

/// The event types a GitHub or GitLab feed posts, checked.
fn check_events(events: &Option<Vec<String>>) -> Result<Vec<String>, ApiError> {
    use crate::feedhooks::{DEFAULT_ON, TOGGLES};
    let Some(list) = events else {
        return Ok(DEFAULT_ON.iter().map(|e| e.to_string()).collect());
    };
    let mut out: Vec<String> = Vec::new();
    for e in list {
        if !TOGGLES.iter().any(|t| t.0 == e) {
            return Err(bad("that is not an event type Brim posts"));
        }
        if !out.contains(e) {
            out.push(e.clone());
        }
    }
    Ok(out)
}

/// "group/project" from a GitLab project link (also self-hosted); `None` for anything else.
fn gitlab_path(link: &str) -> Option<String> {
    let u = reqwest::Url::parse(link.trim()).ok()?;
    let path = u.path().split("/-/").next()?.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (path.contains('/') && path.len() <= 100).then(|| path.to_string())
}

/// Adds a feed. The first check only remembers the items already there, so nothing old is posted.
async fn add(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Add>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let channel: i64 = b.channel.parse().map_err(|_| bad("pick a channel"))?;
    let role = id_of(&b.ping_role)?;
    let (name, picture) = check_look(&st, &b.name, &b.picture)?;
    check_room(&st, guild, channel, None).await?;
    check_channel(&st, guild, channel).await?;
    check_role(&st, guild, role).await?;
    let events = check_events(&b.events)?;
    // A webhook: nothing to read; the repository's name comes with its first event. A followed
    // GitHub repository is read once here, so a wrong name is said now.
    let (target, source_name, source_picture, secret) = match b.kind.as_str() {
        "github" if !b.webhook => {
            let repo = crate::feedfollow::repo_of(&b.target).ok_or_else(|| {
                bad("paste the link of a GitHub repository, like https://github.com/owner/name")
            })?;
            let (repo, picture) = crate::feedfollow::check_repo(&repo)
                .await
                .map_err(ApiError::BadRequest)?;
            (
                repo.clone(),
                repo,
                Some(picture).filter(|p| p.starts_with("https://")),
                None,
            )
        }
        // The pasted link, if any, names the repository at once; else its first event does.
        "github" => (
            String::new(),
            crate::feedfollow::repo_of(&b.target).unwrap_or_else(|| "GitHub".into()),
            None,
            Some(crate::feedhooks::new_secret()),
        ),
        "gitlab" => (
            String::new(),
            gitlab_path(&b.target).unwrap_or_else(|| "GitLab".into()),
            None,
            Some(crate::feedhooks::new_secret()),
        ),
        _ => {
            let (t, n, p) = read_source(&b.kind, &b.target).await?;
            (t, n, p, None)
        }
    };
    let mut tx = st.db.begin().await?;
    // One add at a time per server, so two at once cannot pass the limit together.
    sqlx::query("select pg_advisory_xact_lock(hashtext('feeds'), $1::int)")
        .bind((guild % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    let twice: bool = sqlx::query_scalar(
        "select exists(select 1 from feeds where guild_id = $1 and channel_id = $2 and kind = $3 and target = $4)",
    )
    .bind(guild)
    .bind(channel)
    .bind(&b.kind)
    .bind(&target)
    .fetch_one(&mut *tx)
    .await?;
    if twice && secret.is_none() {
        return Err(bad("that channel already follows this feed"));
    }
    let (all, here): (i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where channel_id = $2) from feeds where guild_id = $1",
    )
    .bind(guild)
    .bind(channel)
    .fetch_one(&mut *tx)
    .await?;
    if all >= MAX_FEEDS || here >= MAX_PER_CHANNEL {
        return Err(bad("the feed limit was reached meanwhile: reload the page"));
    }
    let id: i64 = sqlx::query_scalar(
        "insert into feeds (guild_id, channel_id, kind, target, ping_role, name, picture, source_name,
                            source_picture, send_as_bot, created_by, secret, events)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) returning id",
    )
    .bind(guild)
    .bind(channel)
    .bind(&b.kind)
    .bind(&target)
    .bind(role)
    .bind(&name)
    .bind(&picture)
    .bind(&source_name)
    .bind(&source_picture)
    .bind(b.send_as_bot)
    .bind(s.user_id)
    .bind(&secret)
    .bind(&events)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let what = if source_name.is_empty() {
        target.clone()
    } else {
        source_name.clone()
    };
    audit(
        &st,
        &s,
        guild,
        format!("added the feed “{what}” in <#{channel}>"),
    )
    .await?;
    let mut row = json!(rows(&st, guild, Some(id)).await?.pop());
    // The only time the secret is shown, with [`secret`] (a new one).
    if let Some(secret) = secret {
        row["secret"] = secret.into();
    }
    Ok(Json(row))
}

/// The feed's name for the audit line, or 404.
async fn label(st: &AppState, guild: i64, id: i64) -> Result<String, ApiError> {
    let r: Option<(Option<String>, String, String)> = sqlx::query_as(
        "select name, source_name, target from feeds where id = $1 and guild_id = $2",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let (name, source, target) = r.ok_or(ApiError::NotFound)?;
    Ok(name
        .filter(|n| !n.is_empty())
        .or(Some(source).filter(|n| !n.is_empty()))
        .unwrap_or(target))
}

#[derive(Deserialize)]
struct Save {
    channel: String,
    #[serde(default)]
    ping_role: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
    #[serde(default)]
    send_as_bot: bool,
    #[serde(default)]
    events: Option<Vec<String>>,
}

/// Changes where and how a feed posts.
async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Save>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let what = label(&st, guild, id).await?;
    let channel: i64 = b.channel.parse().map_err(|_| bad("pick a channel"))?;
    let role = id_of(&b.ping_role)?;
    let (name, picture) = check_look(&st, &b.name, &b.picture)?;
    let (old, kind, had): (i64, String, Vec<String>) = sqlx::query_as(
        "select channel_id, kind, events from feeds where id = $1 and guild_id = $2",
    )
    .bind(id)
    .bind(guild)
    .fetch_one(&st.db)
    .await?;
    // Only GitHub and GitLab feeds have event types; left out = unchanged.
    let events = match (&b.events, hooked(&kind)) {
        (Some(_), true) => check_events(&b.events)?,
        _ => had,
    };
    check_role(&st, guild, role).await?;
    let mut tx = st.db.begin().await?;
    if old != channel {
        check_channel(&st, guild, channel).await?;
        // The same lock as add, so two changes at once cannot pass the channel limit together.
        sqlx::query("select pg_advisory_xact_lock(hashtext('feeds'), $1::int)")
            .bind((guild % i64::from(i32::MAX)) as i32)
            .execute(&mut *tx)
            .await?;
        check_room(&st, guild, channel, Some(id)).await?;
    }
    // A broken feed whose channel was changed is tried again.
    sqlx::query(
        "update feeds set channel_id = $3, ping_role = $4, name = $5, picture = $6, send_as_bot = $7,
                events = $8,
                state = case when state = 'broken' and channel_id <> $3 then 'on' else state end,
                fails = case when channel_id <> $3 then 0 else fails end
         where id = $1 and guild_id = $2",
    )
    .bind(id)
    .bind(guild)
    .bind(channel)
    .bind(role)
    .bind(&name)
    .bind(&picture)
    .bind(b.send_as_bot)
    .bind(&events)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    audit(&st, &s, guild, format!("changed the feed “{what}”")).await?;
    if old != channel {
        crate::msghooks::forget_unused(&st, guild).await?;
    }
    Ok(Json(json!(rows(&st, guild, Some(id)).await?.pop())))
}

#[derive(Deserialize)]
struct State_ {
    on: bool,
}

/// Pause or resume. Resuming a broken feed checks it again.
async fn state(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<State_>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let what = label(&st, guild, id).await?;
    sqlx::query(
        "update feeds set state = $3, fails = 0,
                asked = case when $3 = 'on' and (kind not in ('github', 'gitlab') or secret is null) then 'check' when asked = 'check' then null else asked end
         where id = $1 and guild_id = $2",
    )
    .bind(id)
    .bind(guild)
    .bind(if b.on { "on" } else { "paused" })
    .execute(&st.db)
    .await?;
    let did = if b.on { "resumed" } else { "paused" };
    audit(&st, &s, guild, format!("{did} the feed “{what}”")).await?;
    Ok(Json(json!(rows(&st, guild, Some(id)).await?.pop())))
}

#[derive(Deserialize)]
struct Ask {
    what: String,
}

/// Send latest (the newest item, now; a good one also turns a stopped feed on again) or a check.
async fn ask(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Ask>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let what = label(&st, guild, id).await?;
    let (paused, kind, webhook, waiting): (bool, String, bool, bool) =
        sqlx::query_as("select state = 'paused', kind, secret is not null, asked = 'test' from feeds where id = $1 and guild_id = $2")
            .bind(id)
            .bind(guild)
            .fetch_one(&st.db)
            .await?;
    if paused && b.what == "check" {
        return Err(bad("the feed is paused: turn it on to check it"));
    }
    // One Send latest at a time per feed, so it cannot flood a channel.
    if waiting && b.what == "test" {
        return Err(bad(
            "Brim is still sending this feed's latest item: wait 30 seconds",
        ));
    }
    // A followed repository posts only while on (its posts go through the event queue).
    if paused && b.what == "test" && hooked(&kind) {
        return Err(bad(
            "the feed is paused: turn it on to send its latest event",
        ));
    }
    // A webhook sends its events: nothing to check or fetch. A check only takes a stopped feed
    // back. A followed repository is read like an RSS feed.
    let sql = match (b.what.as_str(), hooked(&kind) && webhook) {
        // A GitHub webhook feed's repository is read like a followed one (`feedfollow`).
        ("test", true) if kind == "github" => "update feeds set asked = 'test' where id = $1 and guild_id = $2",
        ("test", true) => return Err(bad("GitLab sends this feed's events as they happen, so Brim cannot fetch the latest one. To test it, open the webhook in GitLab's settings and click Test.")),
        ("check", true) => "update feeds set fails = 0, last_error = null, state = case when state = 'broken' then 'on' else state end where id = $1 and guild_id = $2",
        _ => match b.what.as_str() {
        "check" => "update feeds set asked = 'check', fails = 0, state = case when state = 'broken' then 'on' else state end where id = $1 and guild_id = $2",
        "test" => "update feeds set asked = 'test' where id = $1 and guild_id = $2",
        _ => return Err(bad("ask for check or test")),
        },
    };
    sqlx::query(sql)
        .bind(id)
        .bind(guild)
        .execute(&st.db)
        .await?;
    let did = if b.what == "test" {
        "asked for the latest item of"
    } else {
        "asked for a check of"
    };
    audit(&st, &s, guild, format!("{did} the feed “{what}”")).await?;
    Ok(Json(json!(rows(&st, guild, Some(id)).await?.pop())))
}

#[derive(Deserialize)]
struct Message {
    message: Value,
    /// GitHub and GitLab feeds: the event whose entry this is (`push`, `issues` ...).
    #[serde(default)]
    event: Option<String>,
}

/// The feed's own edited version of its entry; null goes back to the server's version.
async fn message(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Message>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let what = label(&st, guild, id).await?;
    let (kind, mut all): (String, Option<Value>) =
        sqlx::query_as("select kind, message from feeds where id = $1 and guild_id = $2")
            .bind(id)
            .bind(guild)
            .fetch_one(&st.db)
            .await?;
    // GitHub and GitLab feeds keep one edited version per event: {"push": {...}, ...}.
    let event = if hooked(&kind) {
        let e = b.event.as_deref().unwrap_or("");
        if !brim_logging::botmsg::GITHUB_EVENTS.iter().any(|g| g.0 == e) {
            return Err(bad("pick the event whose message this is"));
        }
        Some(e)
    } else {
        None
    };
    let m = if b.message.is_null() {
        Value::Null
    } else {
        let key = match event {
            Some(e) => format!("feed.github.{e}"),
            None => format!("feed.{kind}"),
        };
        if let Some(why) = crate::botmsgs::problem(&key, &b.message) {
            return Err(ApiError::BadRequest(why));
        }
        json!({"content": b.message["content"].as_str().unwrap_or(""), "embeds": b.message["embeds"].as_array().cloned().unwrap_or_default()})
    };
    let is_null = m.is_null();
    let m = match event {
        Some(e) => {
            let mut map = all
                .take()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            let o = map.as_object_mut().expect("an object");
            if is_null {
                o.remove(e);
            } else {
                o.insert(e.into(), m);
            }
            Some(map)
                .filter(|m| m.as_object().is_some_and(|o| !o.is_empty()))
                .unwrap_or(Value::Null)
        }
        None => m,
    };
    sqlx::query("update feeds set message = $3 where id = $1 and guild_id = $2")
        .bind(id)
        .bind(guild)
        .bind(&m)
        .execute(&st.db)
        .await?;
    let did = if is_null {
        "reverted the message of"
    } else {
        "changed the message of"
    };
    audit(&st, &s, guild, format!("{did} the feed “{what}”")).await?;
    Ok(Json(json!(rows(&st, guild, Some(id)).await?.pop())))
}

/// A new secret for a GitHub or GitLab feed. The old one stops working at once; the new one is
/// shown this once.
async fn secret(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let what = label(&st, guild, id).await?;
    let new = crate::feedhooks::new_secret();
    let done = sqlx::query(
        "update feeds set secret = $3 where id = $1 and guild_id = $2 and kind in ('github', 'gitlab') and secret is not null",
    )
    .bind(id)
    .bind(guild)
    .bind(&new)
    .execute(&st.db)
    .await?;
    if done.rows_affected() == 0 {
        return Err(bad("only GitHub and GitLab feeds have a secret"));
    }
    audit(
        &st,
        &s,
        guild,
        format!("made a new secret for the feed “{what}”"),
    )
    .await?;
    let mut row = json!(rows(&st, guild, Some(id)).await?.pop());
    row["secret"] = new.into();
    Ok(Json(row))
}

async fn delete(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let what = label(&st, guild, id).await?;
    sqlx::query("delete from feeds where id = $1 and guild_id = $2")
        .bind(id)
        .bind(guild)
        .execute(&st.db)
        .await?;
    audit(&st, &s, guild, format!("deleted the feed “{what}”")).await?;
    // Its webhook goes too when nothing else in that channel uses it.
    crate::msghooks::forget_unused(&st, guild).await?;
    Ok(Json(json!({})))
}

/// Overview: one warning per broken feed, with the cause (a missing Manage Webhooks among them).
pub(crate) async fn overview_check(
    st: &AppState,
    guild: i64,
    chans: &[brim_antinuke::ChannelImg],
) -> Result<Vec<brim_antinuke::security::Check>, ApiError> {
    use brim_antinuke::security::{steps, Check, Severity};
    let rows: Vec<(i64, String, i64, Option<String>)> = sqlx::query_as(
        "select id, coalesce(nullif(name, ''), nullif(source_name, ''), target), channel_id, last_error
         from feeds where guild_id = $1 and state = 'broken' order by id",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, channel, why)| {
            let at = chans
                .iter()
                .find(|c| c.id as i64 == channel)
                .map_or("a deleted channel".to_string(), |c| format!("#{}", c.name));
            let why = why.unwrap_or_else(|| "it failed 5 checks in a row".into());
            Check {
                id: format!("feed_broken_{id}"),
                severity: Severity::Warn,
                title: "Feed stopped",
                detail: format!(
                    "The feed “{name}” in {at} stopped: {why}. Fix that, then click Send latest on the Feeds page."
                ),
                fix: None,
                steps: steps("feed_broken"),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_gitlab_link_names_its_project() {
        use super::gitlab_path;
        assert_eq!(
            gitlab_path("https://gitlab.com/group/app").as_deref(),
            Some("group/app")
        );
        assert_eq!(
            gitlab_path("https://git.example/a/b/c.git/").as_deref(),
            Some("a/b/c")
        );
        assert_eq!(
            gitlab_path("https://gitlab.com/group/app/-/issues").as_deref(),
            Some("group/app")
        );
        assert_eq!(gitlab_path("https://gitlab.com/group"), None);
        assert_eq!(gitlab_path(""), None);
    }

    /// Limits and the broken-feed warning, on a real database.
    #[tokio::test]
    async fn limits_and_broken_feeds() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_495_000_000).await else {
            return;
        };
        let add = |ch: i64, state: &'static str| {
            sqlx::query("insert into feeds (guild_id, channel_id, kind, target, created_by, state, last_error) values ($1, $2, 'rss', 'https://a.example/f', 7, $3, 'the site answered 500')")
                .bind(g)
                .bind(ch)
                .bind(state)
        };
        for _ in 0..3 {
            add(1, "on").execute(&st.db).await.unwrap();
        }
        assert!(
            super::check_room(&st, g, 1, None).await.is_err(),
            "3 per channel"
        );
        assert!(super::check_room(&st, g, 2, None).await.is_ok());
        for _ in 0..6 {
            add(2, "on").execute(&st.db).await.unwrap();
        }
        add(3, "broken").execute(&st.db).await.unwrap();
        assert!(
            super::check_room(&st, g, 4, None).await.is_err(),
            "10 per server"
        );
        let c = super::overview_check(&st, g, &[]).await.unwrap();
        assert_eq!(c.len(), 1);
        assert!(c[0].detail.contains("answered 500"), "{}", c[0].detail);
        sqlx::query("delete from feeds where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
    }
}

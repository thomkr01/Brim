//! Feeds: RSS/Atom and YouTube. Every 30 s the poller takes the
//! feeds that are due (each 10 minutes, or asked for by the dashboard's Send latest),
//! fetches each address once for all servers that follow it, and posts new items: through
//! Brim's webhook in the channel with the source's name and picture (as if the source posted
//! it), or as the bot when "Send as Brim" is on. The first check only remembers what is there;
//! at most 3 posts per feed per check; 5 failed checks in a row, or a webhook that cannot be made
//! again, make the feed "broken" until someone clicks Send latest.
//! Automod skips only the posts Brim itself sent (`sent_by_us`, by message id), so every item passes the
//! scam/malicious and NSFW site lists here first (choice C1).
use crate::{app::App, events::send_log};
use anyhow::Result;
use brim_automod::{
    feed::{self, Got, Item},
    filters,
};
use brim_logging::{
    botmsg,
    embed::{GREEN, ORANGE, RED},
    Log, LogKind,
};
use futures_util::StreamExt;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use twilight_model::{
    channel::message::AllowedMentions,
    id::{marker::WebhookMarker, Id},
};

/// The name of Brim's webhook in a channel; the dashboard's Message Builder uses the same one.
pub const HOOK_NAME: &str = "Brim messages";
/// Posts per feed per check; newer items past this are counted, not posted.
const MAX_POSTS: usize = 3;
/// Failed checks in a row before a feed is broken.
const MAX_FAILS: i32 = 5;
/// Seen items kept per feed (more when the feed itself lists more).
const KEEP_SEEN: i64 = 200;
/// Discord's error code for "Unknown Webhook".
const UNKNOWN_WEBHOOK: u64 = 10015;

#[derive(sqlx::FromRow, Clone)]
struct Row {
    id: i64,
    guild_id: i64,
    channel_id: i64,
    kind: String,
    target: String,
    ping_role: Option<i64>,
    name: Option<String>,
    picture: Option<String>,
    source_name: String,
    source_picture: Option<String>,
    send_as_bot: bool,
    message: Option<Value>,
    primed: bool,
    asked: Option<String>,
    fails: i32,
}

impl Row {
    fn url(&self) -> String {
        match self.kind.as_str() {
            "youtube" => feed::youtube_feed(&self.target),
            _ => self.target.clone(),
        }
    }
}

/// The poller; never returns.
pub async fn run(app: Arc<App>) {
    tokio::spawn(events(app.clone()));
    let client = feed::client();
    // Per address: the ETag and Last-Modified of the last full answer. Lost at a restart, which
    // only costs one full fetch.
    let mut seen_headers: HashMap<String, Headers> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        if let Err(e) = round(&app, &client, &mut seen_headers).await {
            tracing::warn!(?e, "feed round failed");
        }
    }
}

async fn round(
    app: &App,
    client: &reqwest::Client,
    headers: &mut HashMap<String, Headers>,
) -> Result<()> {
    // A paused feed is only read for Send latest; a check on it waits for resume.
    let due: Vec<Row> = sqlx::query_as(
        "select id, guild_id, channel_id, kind, target, ping_role, name, picture, source_name,
                source_picture, send_as_bot, message, primed, asked, fails from feeds
         where kind in ('rss', 'youtube') and (asked = 'test'
            or (state <> 'paused' and asked is not null)
            or (state = 'on' and (last_check is null or last_check < now() - interval '10 minutes')
                -- The Messages module is off: no automatic checks; a test or Send latest still runs.
                and coalesce((select c.config->'modules'->>'messages' from guild_config c
                              where c.guild_id = feeds.guild_id order by c.version desc limit 1), 'true') <> 'false'))
         order by last_check nulls first limit 200",
    )
    .fetch_all(&app.pool)
    .await?;
    let mut by_url: HashMap<String, Vec<Row>> = HashMap::new();
    for r in due {
        by_url.entry(r.url()).or_default().push(r);
    }
    // shortcut: forget all headers past 2000 addresses (costs one full fetch each), prune by
    // address if servers ever hold that many feeds.
    if headers.len() > 2000 {
        headers.clear();
    }
    let known = &*headers;
    // Several addresses at once, so one slow site does not hold up every other server's feeds.
    let done_urls: Vec<(String, Option<Headers>)> = futures_util::stream::iter(by_url)
        .map(|(url, rows)| async move {
            let keep = check(app, client, &url, rows, known.get(&url)).await;
            (url, keep)
        })
        .buffer_unordered(8)
        .collect()
        .await;
    for (url, keep) in done_urls {
        if let Some(h) = keep {
            headers.insert(url, h);
        }
    }
    Ok(())
}

/// Reads one address and handles every feed on it. Returns the headers to send next time, only
/// when every feed finished: else a 304 next round would hide the items not yet posted.
async fn check(
    app: &App,
    client: &reqwest::Client,
    url: &str,
    rows: Vec<Row>,
    known: Option<&Headers>,
) -> Option<Headers> {
    // A feed that has to read every item (first check, test post) gets the full answer.
    let full = rows
        .iter()
        .any(|r| !r.primed || r.asked.as_deref() == Some("test"));
    let (etag, modified) = match (full, known) {
        (false, Some((e, m))) => (e.as_deref(), m.as_deref()),
        _ => (None, None),
    };
    let got = match feed::check_url(url) {
        Ok(_) => feed::fetch(client, url, etag, modified).await,
        Err(e) => Err(e),
    };
    let mut all_ok = true;
    for r in rows {
        let res = match &got {
            Ok(Got::Same) => Ok(()),
            Ok(Got::Feed(f)) => handle(app, &r, f).await,
            Err(e) => Err(Fail::Check(e.clone())),
        };
        all_ok &= res.is_ok();
        if let Err(e) = done(app, &r, res).await {
            tracing::warn!(?e, feed = r.id, "saving a feed check failed");
        }
    }
    match got {
        Ok(Got::Feed(f)) if all_ok => Some((f.etag, f.modified)),
        _ => None,
    }
}

/// ETag and Last-Modified of an address's last full answer.
type Headers = (Option<String>, Option<String>);

/// Why a feed check did not go through.
#[derive(Debug)]
enum Fail {
    /// The feed could not be read: counts toward [`MAX_FAILS`].
    Check(String),
    /// Posting failed in a way a retry does not fix (no webhook, no channel, Discord refused):
    /// the feed is broken at once.
    Post(String),
}

/// Records the outcome of one check of `r`.
async fn done(app: &App, r: &Row, res: Result<(), Fail>) -> Result<()> {
    let (broken, error) = match &res {
        Ok(()) => (false, None),
        Err(Fail::Check(e)) => (r.fails + 1 >= MAX_FAILS, Some(e.clone())),
        Err(Fail::Post(e)) => (true, Some(e.clone())),
    };
    let was: Option<String> = sqlx::query_scalar(
        // `asked` is cleared only if it is still the one this check answered: a new click meanwhile
        // stays. A good check turns a broken feed on again (Send latest).
        "update feeds f set last_check = now(), last_error = $2,
                asked = case when f.asked is not distinct from $4 then null else f.asked end,
                fails = case when $2::text is null then 0 else f.fails + 1 end,
                state = case when $3 and f.state = 'on' then 'broken'
                             when $2::text is null and f.state = 'broken' then 'on'
                             else f.state end
         from (select state from feeds where id = $1) old where f.id = $1 returning old.state",
    )
    .bind(r.id)
    .bind(&error)
    .bind(broken)
    .bind(&r.asked)
    .fetch_optional(&app.pool)
    .await?;
    if broken && was.as_deref() == Some("on") {
        let log = Log::new("Feed Broken", RED)
            .line("Feed", &feed_label(r))
            .line("Channel", &format!("<#{}>", r.channel_id))
            .line("Why", &error.unwrap_or_default())
            .line(
                "Effect",
                "Brim stops checking it. Fix the cause, then click Send latest on the dashboard's Feeds page.",
            );
        send_log(app, r.guild_id as u64, LogKind::Webhook, log).await?;
    }
    Ok(())
}

/// "**Name** (`address`)" for logs.
fn feed_label(r: &Row) -> String {
    label_of(r.name.as_deref(), &r.source_name, &r.target)
}

/// "**name** (`address`)". A webhook feed stores no address (GitHub or GitLab calls Brim), so it
/// shows the repository's name, which its first delivery stores, and no empty code span.
fn label_of(name: Option<&str>, source: &str, target: &str) -> String {
    let name = name.filter(|n| !n.is_empty()).unwrap_or(source);
    let name = if name.is_empty() { "Feed" } else { name }.replace('*', "");
    let target = target.replace('`', "");
    if target.is_empty() || target == name {
        format!("**{name}**")
    } else {
        format!("**{name}** (`{target}`)")
    }
}

/// One feed against a fresh read of its address.
async fn handle(app: &App, r: &Row, f: &feed::Feed) -> Result<(), Fail> {
    let db = |e: sqlx::Error| Fail::Check(format!("database: {e}"));
    // The source's own name and picture; a YouTube feed has no picture, so the one read when the
    // feed was added stays.
    let picture = f.picture.clone().or(r.source_picture.clone());
    let name = if f.title.is_empty() {
        r.source_name.clone()
    } else {
        f.title.clone()
    };
    sqlx::query("update feeds set source_name = $2, source_picture = $3 where id = $1")
        .bind(r.id)
        .bind(&name)
        .bind(&picture)
        .execute(&app.pool)
        .await
        .map_err(db)?;
    let r = &Row {
        source_name: name,
        source_picture: picture,
        ..r.clone()
    };
    let ids: Vec<String> = f.items.iter().map(|i| i.id.clone()).collect();
    if !r.primed {
        remember(app, r.id, &ids).await.map_err(db)?;
        sqlx::query("update feeds set primed = true where id = $1")
            .bind(r.id)
            .execute(&app.pool)
            .await
            .map_err(db)?;
    }
    // Send latest: only the newest item, also on a paused feed; new items wait for the next check.
    if r.asked.as_deref() == Some("test") {
        let Some(newest) = f.items.first() else {
            return Err(Fail::Check("the feed has no items to post yet".into()));
        };
        post(app, r, &Piece::item(r, newest), Some(&newest.id)).await?;
        return remember(app, r.id, std::slice::from_ref(&newest.id))
            .await
            .map_err(db);
    }
    if !r.primed {
        return Ok(());
    }
    let known: Vec<String> =
        sqlx::query_scalar("select item from feed_seen where feed_id = $1 and item = any($2)")
            .bind(r.id)
            .bind(&ids)
            .fetch_all(&app.pool)
            .await
            .map_err(db)?;
    let new: Vec<&Item> = f.items.iter().filter(|i| !known.contains(&i.id)).collect();
    let skipped = new.len().saturating_sub(MAX_POSTS);
    // Oldest of the newest three first, so the channel reads in order. Each posted item is
    // remembered at once: a failed post later in the list does not post the earlier ones again.
    for item in new.iter().take(MAX_POSTS).rev() {
        post(app, r, &Piece::item(r, item), Some(&item.id)).await?;
        remember(app, r.id, std::slice::from_ref(&item.id))
            .await
            .map_err(db)?;
    }
    if skipped > 0 {
        let rest: Vec<String> = new.iter().skip(MAX_POSTS).map(|i| i.id.clone()).collect();
        remember(app, r.id, &rest).await.map_err(db)?;
        let log = Log::new("Feed Items Skipped", ORANGE)
            .line("Feed", &feed_label(r))
            .line("Count", &format!("`{skipped}`"))
            .line(
                "Why",
                "At most 3 new items are posted per check; older new items are skipped.",
            );
        let _ = send_log(app, r.guild_id as u64, LogKind::Webhook, log).await;
    }
    // Forget the oldest, but never an item the feed still lists (it would come back as new).
    sqlx::query(
        "delete from feed_seen where feed_id = $1 and item <> all($2) and item not in
           (select item from feed_seen where feed_id = $1 order by seen_at desc limit $3)",
    )
    .bind(r.id)
    .bind(&ids)
    .bind(KEEP_SEEN)
    .execute(&app.pool)
    .await
    .map_err(db)?;
    Ok(())
}

async fn remember(app: &App, feed: i64, items: &[String]) -> sqlx::Result<()> {
    sqlx::query(
        "insert into feed_seen (feed_id, item) select $1, unnest($2::text[])
         on conflict (feed_id, item) do update set seen_at = now()",
    )
    .bind(feed)
    .bind(items)
    .execute(&app.pool)
    .await
    .map(drop)
}

/// One thing to post: a feed item or a GitHub/GitLab event, as its entry and variables.
struct Piece {
    /// The entry: `feed.rss`, `feed.github.push` ...
    key: String,
    link: String,
    vars: Vec<(String, String)>,
    /// GitHub/GitLab: the event's own colour, used while the default look is sent (0 = none).
    color: Option<u32>,
}

impl Piece {
    fn item(r: &Row, i: &Item) -> Self {
        let vars = [
            ("title", &i.title),
            ("link", &i.link),
            ("author", &i.author),
            ("summary", &i.summary),
            ("image", &i.image),
        ];
        Piece {
            key: format!("feed.{}", r.kind),
            link: i.link.clone(),
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            color: None,
        }
    }

    /// A queued event: its variables as the dashboard made them (strings only).
    fn event(event: &str, vars: &Value) -> Self {
        let get = |k: &str| vars[k].as_str().unwrap_or("").to_string();
        let list: Vec<(String, String)> = vars
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| *k != "color")
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect();
        Piece {
            key: format!("feed.github.{event}"),
            link: get("link"),
            vars: list,
            color: Some(get("color").parse().unwrap_or(0)),
        }
    }

    /// All of its text, for the link and word checks.
    fn text(&self, r: &Row) -> String {
        let mut t: Vec<&str> = self.vars.iter().map(|(_, v)| v.as_str()).collect();
        let name = hook_name(r);
        t.push(&name);
        t.join("\n")
    }
}

/// The site hosts a post would show: its links, and any link in its text, plus where up to 3
/// short links point.
async fn hosts(app: &App, r: &Row, p: &Piece) -> Vec<String> {
    let text = p.text(r);
    let mut out = filters::hosts(&text);
    let unroll = async {
        for u in filters::shortener_urls(&text).into_iter().take(3) {
            out.extend(app.sites.unshorten(u).await);
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(12), unroll).await;
    out
}

/// A webhook name Discord takes: the owner's, else the source's, else "Feed" (Discord refuses
/// "discord" and "clyde" in one, and more than 80 characters). A source never posts as "Brim".
/// GitHub and GitLab feeds post as "GitHub" / "GitLab", as GitHub's own webhook does.
fn hook_name(r: &Row) -> String {
    let ok = |n: &&str| {
        let low = n.to_lowercase();
        !n.trim().is_empty() && !low.contains("discord") && !low.contains("clyde")
    };
    // The whole word "Brim" only, so a source like "Brimstone" keeps its name; also the old name.
    let ours = |n: &str| {
        let low = n.to_lowercase();
        low.contains("tommybot")
            || low
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w == "brim")
    };
    let source = match r.kind.as_str() {
        "github" => Some("GitHub"),
        "gitlab" => Some("GitLab"),
        _ => Some(r.source_name.as_str()).filter(|n| !ours(n)),
    };
    let n = [r.name.as_deref(), source]
        .into_iter()
        .flatten()
        .find(ok)
        .unwrap_or("Feed");
    n.trim().chars().take(80).collect()
}

/// The message for `p`: the feed's own edited version, else the server's, else the default.
async fn body(app: &App, r: &Row, p: &Piece) -> Value {
    let key = &p.key;
    // A GitHub/GitLab feed keeps one edited version per event.
    let own = match key.strip_prefix("feed.github.") {
        Some(event) => r.message.as_ref().map(|m| m[event].clone()),
        None => r.message.clone(),
    };
    let own = own.filter(|m| botmsg::uses_stored(key, m));
    let stored = match own {
        Some(m) => Some(m),
        None => crate::reply::stored_msg(app, Id::new(r.guild_id as u64), key)
            .await
            .filter(|m| botmsg::uses_stored(key, m)),
    };
    let ping = r.ping_role.map(|p| format!("<@&{p}>")).unwrap_or_default();
    let feed = hook_name(r);
    let mut vars: Vec<(&str, &str)> = p
        .vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    vars.push(("feed", &feed));
    vars.push(("ping", &ping));
    let m = botmsg::render(key, stored.as_ref(), &vars);
    // An edited version can push {link} past Discord's length cut: the default always has it.
    let link = serde_json::to_string(&p.link).unwrap_or_default();
    if stored.is_some() && m.to_string().contains(link.trim_matches('"')) {
        return m;
    }
    let mut m = botmsg::render(key, None, &vars);
    // The default GitHub look: each event's own colour (none for some, as GitHub's webhook).
    if let (Some(c), Some(e)) = (p.color, m["embeds"][0].as_object_mut()) {
        if c == 0 {
            e.remove("color");
        } else {
            e.insert("color".into(), c.into());
        }
    }
    m
}

/// The server's own automod rules that a post through the webhook would trip: invite links,
/// blacklisted links and blacklisted words, with the channel whitelists of those filters. Checked
/// before posting, since automod skips the posts Brim sent.
async fn server_rules(app: &App, r: &Row, p: &Piece, found: &[String]) -> Option<&'static str> {
    let cfg = app.cfg(r.guild_id as u64).await.ok()?;
    let am = &cfg.automod;
    if !am.enabled {
        return None;
    }
    let f = &am.filters;
    let wl = &cfg.whitelist;
    let channel = Id::new(r.channel_id as u64);
    let (parent, category) = app
        .channel_scope(Id::new(r.guild_id as u64), channel)
        .await
        .unwrap_or((channel, None));
    let listed =
        |l: &brim_core::sections::WhitelistList| l.covers(app.me, &[], parent, category, None);
    let text = p.text(r);
    if f.invites.enabled
        && !wl.partner_channels.contains(&parent)
        && !listed(&wl.invites)
        && crate::automod::is_invite(&text, found)
    {
        return Some("a Discord invite (the server's invite filter)");
    }
    if f.links.enabled && filters::any_on_domains(&filters::hosts(&text), &f.blacklisted_domains) {
        return Some("a blacklisted site (the server's link filter)");
    }
    if f.words.filter.enabled {
        let forms = filters::word_forms(&text);
        if filters::word_hits(&forms, &crate::automod::blacklist_words(f))
            + f.words.regex.hits(&forms)
            > 0
        {
            return Some("a blacklisted word (the server's word filter)");
        }
    }
    None
}

/// Posts one item or event, after the link check. A blocked one is logged and counts as
/// handled. `item`: the feed item's id, remembered when Discord refuses the message.
async fn post(app: &App, r: &Row, p: &Piece, item: Option<&str>) -> Result<(), Fail> {
    let found = hosts(app, r, p).await;
    let why = if app.sites.phishing(&found) {
        Some("a link to a scam or malicious site")
    } else if app.sites.nsfw(&found) {
        Some("a link to an NSFW site")
    } else if filters::is_nitro_steam_scam(&p.text(r)) {
        Some("a scam link")
    } else {
        server_rules(app, r, p, &found).await
    };
    if let Some(why) = why {
        let log = Log::new("Feed Post Blocked", ORANGE)
            .line("Feed", &feed_label(r))
            .line("Channel", &format!("<#{}>", r.channel_id))
            .line("Why", &format!("The post has {why}, so it was not posted."))
            .line("Link", &format!("`{}`", p.link.replace('`', "")));
        let _ = send_log(app, r.guild_id as u64, LogKind::AutoMod, log).await;
        return Ok(());
    }
    let m = body(app, r, p).await;
    let (content, embeds) = crate::reply::parts(&m);
    // Never @everyone, @here or a member from feed text: only the chosen ping role.
    let mentions = AllowedMentions {
        roles: r.ping_role.map(|p| Id::new(p as u64)).into_iter().collect(),
        ..Default::default()
    };
    let refused = |e: &twilight_http::Error| {
        match crate::http_err::status(e) {
        Some(403) => Fail::Post("Brim may not post in that channel (it needs View Channel and Send Messages, or Manage Webhooks for the webhook)".into()),
        Some(404) => Fail::Post("the channel was deleted".into()),
        Some(400) => Fail::Post(format!("Discord refused the message (that item is skipped): {e}")),
        _ => Fail::Check(format!("Discord did not take the post: {e}")),
    }
    };
    // A message Discord refuses (400) is refused every time: remember the item, so Send latest
    // moves past it. The feed still breaks, so a bad edited message is seen.
    let mut skip_400 = false;
    let mut skip = |e: twilight_http::Error| {
        skip_400 = crate::http_err::status(&e) == Some(400);
        refused(&e)
    };
    let sent = post_to(app, r, &content, &embeds, &mentions, &mut skip).await;
    if let (true, Some(id)) = (skip_400, item) {
        let _ = remember(app, r.id, &[id.to_string()]).await;
    }
    sent?;
    // The post went out: a failed bookkeeping write must not post it again.
    let _ = sqlx::query("update feeds set last_post = now() where id = $1")
        .bind(r.id)
        .execute(&app.pool)
        .await;
    Ok(())
}

async fn post_to(
    app: &App,
    r: &Row,
    content: &str,
    embeds: &[twilight_model::channel::message::Embed],
    mentions: &AllowedMentions,
    skip: &mut impl FnMut(twilight_http::Error) -> Fail,
) -> Result<(), Fail> {
    let channel = Id::new(r.channel_id as u64);
    if r.send_as_bot {
        app.http
            .create_message(channel)
            .content(content)
            .embeds(embeds)
            .allowed_mentions(Some(mentions))
            .await
            .map_err(&mut *skip)?;
    } else {
        let name = hook_name(r);
        let avatar = r
            .picture
            .clone()
            .or(r.source_picture.clone())
            .filter(|p| p.starts_with("https://"));
        let mut stale = None;
        let _sending = Sending::new(r.channel_id as u64);
        loop {
            let (id, token) = hook(app, r, stale).await?;
            let mut rq = app
                .http
                .execute_webhook(id, &token)
                .content(content)
                .embeds(embeds)
                .username(&name)
                .allowed_mentions(Some(mentions));
            if let Some(a) = &avatar {
                rq = rq.avatar_url(a);
            }
            match rq.wait().await {
                Ok(res) => {
                    // The id comes from Discord's answer; without it automod checks the post.
                    if let Ok(m) = res.model().await {
                        sent(m.id.get());
                    }
                    break;
                }
                Err(e)
                    if stale.is_none()
                        && crate::http_err::api_code(&e) == Some(UNKNOWN_WEBHOOK) =>
                {
                    // Deleted in Discord (the audit log tells the bot too): made again once.
                    forget_hook(app, r.guild_id, id.get() as i64).await;
                    stale = Some(id.get() as i64);
                }
                Err(e) => return Err(skip(e)),
            }
        }
        let _ = sqlx::query("update message_webhooks set last_used = now() where channel_id = $1")
            .bind(r.channel_id)
            .execute(&app.pool)
            .await;
    }
    Ok(())
}

/// Panic mode runs, or an anti-nuke case is open: Brim makes no new webhook then.
pub(crate) fn under_attack(app: &App, g: u64) -> bool {
    app.panic_runs.lock().unwrap().contains_key(&g)
        || app
            .nukes
            .lock()
            .unwrap()
            .get(&g)
            .is_some_and(|(_, at)| at.elapsed() < crate::antinuke::NUKE_GAP)
}

/// Brim's webhook in the feed's channel: the stored one, or a new one. `stale`: the one
/// Discord said is gone. The dashboard's Message Builder takes the same per-channel lock, so two
/// first posts never make two webhooks.
async fn hook(app: &App, r: &Row, stale: Option<i64>) -> Result<(Id<WebhookMarker>, String), Fail> {
    let db = |e: sqlx::Error| Fail::Check(format!("database: {e}"));
    let mut tx = app.pool.begin().await.map_err(db)?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('message_webhook'), $1::int)")
        .bind((r.channel_id % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    let row: Option<(i64, String)> = sqlx::query_as(
        "select webhook_id, token from message_webhooks where channel_id = $1 and guild_id = $2",
    )
    .bind(r.channel_id)
    .bind(r.guild_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?;
    if let Some((id, token)) = row.filter(|h| Some(h.0) != stale) {
        return Ok((Id::new(id.max(1) as u64), token));
    }
    if under_attack(app, r.guild_id as u64) {
        return Err(Fail::Post(
            "Brim's webhook in that channel is gone, and Brim makes no new webhook during panic mode or an anti-nuke case".into(),
        ));
    }
    let made = app
        .http
        .create_webhook(Id::new(r.channel_id as u64), HOOK_NAME)
        .await
        .map_err(|e| match crate::http_err::status(&e) {
            Some(403) => {
                Fail::Post("Brim needs Manage Webhooks in that channel to post as the feed".into())
            }
            Some(404) => Fail::Post("the channel was deleted".into()),
            _ => Fail::Post(format!("Discord did not make the webhook: {e}")),
        })?;
    let w = made
        .model()
        .await
        .map_err(|e| Fail::Post(format!("Discord's webhook answer could not be read: {e}")))?;
    let Some(token) = w.token else {
        return Err(Fail::Post("Discord made a webhook without a token".into()));
    };
    sqlx::query(
        "insert into message_webhooks (channel_id, guild_id, webhook_id, token) values ($1, $2, $3, $4)
         on conflict (channel_id) do update set guild_id = $2, webhook_id = $3, token = $4, last_used = now()",
    )
    .bind(r.channel_id)
    .bind(r.guild_id)
    .bind(w.id.get() as i64)
    .bind(&token)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok((w.id, token))
}

/// A webhook found gone: its row goes (and its Message Builder copies are marked, as the
/// dashboard does), so the next post makes a new one.
async fn forget_hook(app: &App, guild: i64, webhook: i64) {
    let r = async {
        let mut tx = app.pool.begin().await?;
        sqlx::query(
            "update message_sends set hook_gone_at = now() where webhook_id = $1 and guild_id = $2
             and posted_id is not null and hook_gone_at is null",
        )
        .bind(webhook)
        .bind(guild)
        .execute(&mut *tx)
        .await?;
        sqlx::query("delete from message_webhooks where webhook_id = $1 and guild_id = $2")
            .bind(webhook)
            .bind(guild)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    };
    if let Err(e) = r.await {
        tracing::warn!(?e, "forgetting a deleted feed webhook failed");
    }
}

/// Messages Brim itself sent through its feed webhooks (id → when), and channels with a send
/// under way. Automod skips only these messages: anyone with the Discord permission Manage Webhooks
/// can read the webhook's token, so "from Brim's webhook" proves nothing.
#[derive(Default)]
struct Sent {
    ids: HashMap<u64, Instant>,
    sending: HashMap<u64, usize>,
}

static SENT: LazyLock<Mutex<Sent>> = LazyLock::new(Default::default);

/// How long a sent id is kept: the post's create event comes within seconds.
const KEEP_SENT: Duration = Duration::from_secs(600);

fn sent(id: u64) {
    let mut s = SENT.lock().unwrap();
    s.ids.retain(|_, at| at.elapsed() < KEEP_SENT);
    s.ids.insert(id, Instant::now());
}

/// Marks a send in a channel as under way while it lives.
struct Sending(u64);

impl Sending {
    fn new(channel: u64) -> Self {
        *SENT.lock().unwrap().sending.entry(channel).or_default() += 1;
        Sending(channel)
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        let mut s = SENT.lock().unwrap();
        if let Some(n) = s.sending.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                s.sending.remove(&self.0);
            }
        }
    }
}

/// Whether Brim itself sent message `id` through a feed webhook. The gateway can bring the
/// message before Discord's answer to the send: while a send in that channel is under way, this
/// waits up to 5 s for the id. Any other message, also one through Brim's webhook, is false.
pub async fn sent_by_us(channel: u64, id: u64) -> bool {
    for _ in 0..50 {
        {
            let s = SENT.lock().unwrap();
            if s.ids.contains_key(&id) {
                return true;
            }
            if !s.sending.contains_key(&channel) {
                return false;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// A GitHub/GitLab event the dashboard queued, with its feed.
#[derive(sqlx::FromRow)]
struct Queued {
    event_id: i64,
    event: String,
    vars: Value,
    #[sqlx(flatten)]
    feed: Row,
}

/// Posts the GitHub and GitLab events the dashboard queued (`feed_events`), oldest first, every
/// 5 s; never returns. Events of a paused or stopped feed wait (the dashboard takes no new ones
/// meanwhile); queued rows are kept 7 days, so a repeated delivery id is still known.
async fn events(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    let mut n: u32 = 0;
    // Feeds whose last post failed for now, and when: tried again a minute later.
    let mut wait: HashMap<i64, Instant> = HashMap::new();
    loop {
        tick.tick().await;
        if let Err(e) = events_round(&app, n.is_multiple_of(720), &mut wait).await {
            tracing::warn!(?e, "feed event round failed");
        }
        n = n.wrapping_add(1);
    }
}

async fn events_round(app: &App, prune: bool, wait: &mut HashMap<i64, Instant>) -> Result<()> {
    if prune {
        sqlx::query("delete from feed_events where at < now() - interval '7 days'")
            .execute(&app.pool)
            .await?;
    }
    // Events over the 30 a minute limit, counted by the dashboard: one log line per feed.
    let dropped: Vec<(i64, i64, String, i32)> = sqlx::query_as(
        "with d as (select id, dropped from feeds where dropped > 0 for update)
         update feeds f set dropped = f.dropped - d.dropped from d where f.id = d.id
         returning f.guild_id, f.channel_id, coalesce(nullif(f.name, ''), f.source_name), d.dropped",
    )
    .fetch_all(&app.pool)
    .await?;
    for (guild, channel, name, count) in dropped {
        let log = Log::new("Feed Items Skipped", ORANGE)
            .line("Feed", &format!("**{}**", name.replace('*', "")))
            .line("Channel", &format!("<#{channel}>"))
            .line("Count", &format!("`{count}`"))
            .line(
                "Why",
                "At most 30 events a minute are posted per GitHub or GitLab feed; the rest are skipped.",
            );
        let _ = send_log(app, guild as u64, LogKind::Webhook, log).await;
    }
    let todo: Vec<Queued> = sqlx::query_as(
        "select e.id as event_id, e.event, e.vars, f.id, f.guild_id, f.channel_id, f.kind, f.target,
                f.ping_role, f.name, f.picture, f.source_name, f.source_picture, f.send_as_bot,
                f.message, f.primed, f.asked, f.fails
         from feed_events e join feeds f on f.id = e.feed_id
         where not e.done and f.state = 'on' order by e.id limit 30",
    )
    .fetch_all(&app.pool)
    .await?;
    // A feed whose post failed for now keeps its later events for the next round, so they stay
    // in order.
    let mut held: Vec<i64> = Vec::new();
    // A failure for now (Discord down) counts once a minute, not every 5 s: 5 in a row (about
    // 5 minutes) stop the feed, as 5 failed checks stop an RSS feed.
    wait.retain(|_, at| at.elapsed() < Duration::from_secs(60));
    for q in todo {
        let r = &q.feed;
        if held.contains(&r.id) || wait.contains_key(&r.id) {
            continue;
        }
        let res = if q.event == "ping" {
            let log = Log::new("Feed Connected", GREEN)
                .line("Feed", &feed_label(r))
                .line("Channel", &format!("<#{}>", r.channel_id))
                .line(
                    "Effect",
                    &format!(
                        "{} reached Brim with the right secret: the event types switched on post here.",
                        if r.kind == "gitlab" { "GitLab" } else { "GitHub" }
                    ),
                );
            let _ = send_log(app, r.guild_id as u64, LogKind::Webhook, log).await;
            Ok(())
        } else {
            post(app, r, &Piece::event(&q.event, &q.vars), None).await
        };
        // Posted, blocked, or refused for good: the event is done. Failed for now: tried again.
        let finished = !matches!(res, Err(Fail::Check(_)));
        if finished {
            sqlx::query("update feed_events set done = true where id = $1")
                .bind(q.event_id)
                .execute(&app.pool)
                .await?;
        } else {
            wait.insert(r.id, Instant::now());
        }
        // After a failure the feed's later events wait (in order; a stopped feed is not
        // turned on again by a later blocked one).
        if res.is_err() {
            held.push(r.id);
        }
        if let Err(e) = done(app, r, res).await {
            tracing::warn!(?e, feed = r.id, "saving a feed event failed");
        }
    }
    Ok(())
}

/// Feeds whose channel was deleted: broken, with the reason.
pub async fn channel_deleted(app: &App, channel: u64) -> Result<()> {
    sqlx::query(
        "update feeds set state = 'broken', last_error = 'the channel was deleted: pick another channel'
         where channel_id = $1 and state <> 'paused'",
    )
    .bind(channel as i64)
    .execute(&app.pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_never_shows_an_empty_address() {
        // A webhook feed has no address: before its first delivery it shows the source alone.
        assert_eq!(label_of(None, "GitHub", ""), "**GitHub**");
        assert_eq!(label_of(Some(""), "owner/repo", ""), "**owner/repo**");
        // A followed repository's address is its name: shown once.
        assert_eq!(label_of(None, "owner/repo", "owner/repo"), "**owner/repo**");
        assert_eq!(
            label_of(Some("News"), "Site", "https://x.example/feed"),
            "**News** (`https://x.example/feed`)"
        );
        assert_eq!(label_of(None, "", "a`b"), "**Feed** (`ab`)");
    }

    fn row() -> Row {
        Row {
            id: 1,
            guild_id: 2,
            channel_id: 3,
            kind: "rss".into(),
            target: "https://a.example/feed".into(),
            ping_role: None,
            name: None,
            picture: None,
            source_name: "My Blog".into(),
            source_picture: None,
            send_as_bot: false,
            message: None,
            primed: true,
            asked: None,
            fails: 0,
        }
    }

    #[test]
    fn webhook_names_discord_takes() {
        assert_eq!(hook_name(&row()), "My Blog");
        let own = Row {
            name: Some("News".into()),
            ..row()
        };
        assert_eq!(hook_name(&own), "News");
        // Discord refuses these: the source's name, then "Feed".
        let bad = Row {
            name: Some("Discord news".into()),
            ..row()
        };
        assert_eq!(hook_name(&bad), "My Blog");
        let both = Row {
            name: Some("clyde".into()),
            source_name: "".into(),
            ..row()
        };
        assert_eq!(hook_name(&both), "Feed");
        let long = Row {
            source_name: "x".repeat(100),
            ..row()
        };
        assert_eq!(hook_name(&long).chars().count(), 80);
        let fake = Row {
            source_name: "Brim Staff".into(),
            ..row()
        };
        assert_eq!(hook_name(&fake), "Feed");
        for (name, out) in [("TommyBot news", "Feed"), ("Brimstone", "Brimstone")] {
            let r = Row {
                source_name: name.into(),
                ..row()
            };
            assert_eq!(hook_name(&r), out);
        }
        let yt = Row {
            kind: "youtube".into(),
            target: "UC_x5XG1OV2P6uZZ5FSM9Ttw".into(),
            ..row()
        };
        assert!(yt.url().ends_with("channel_id=UC_x5XG1OV2P6uZZ5FSM9Ttw"));
    }

    #[test]
    fn github_events_post_as_github() {
        let gh = Row {
            kind: "github".into(),
            source_name: "octo/hello".into(),
            ..row()
        };
        assert_eq!(hook_name(&gh), "GitHub");
        let lab = Row {
            kind: "gitlab".into(),
            name: Some("Builds".into()),
            ..row()
        };
        assert_eq!(hook_name(&lab), "Builds");
        let p = Piece::event(
            "issues",
            &serde_json::json!({"headline": "[o/r] Issue opened: #1 T", "link": "https://github.com/o/r/issues/1",
                                "color": "15426592", "summary": "text https://scam.example/x"}),
        );
        assert_eq!(p.key, "feed.github.issues");
        assert_eq!(p.link, "https://github.com/o/r/issues/1");
        assert_eq!(p.color, Some(15426592));
        // Every text is checked before posting, links in the text too; the colour is not a variable.
        let text = p.text(&gh);
        assert!(text.contains("scam.example") && text.contains("GitHub"));
        assert!(!p.vars.iter().any(|v| v.0 == "color"));
    }

    #[tokio::test]
    async fn automod_skips_only_posts_brim_sent() {
        // A post Brim sent: skipped.
        sent(9_001);
        assert!(sent_by_us(1, 9_001).await);
        // A foreign post through Brim's webhook (someone copied its token): checked.
        assert!(!sent_by_us(1, 9_002).await);
        // The gateway brings Brim's post before Discord's answer: it waits for the id.
        let guard = Sending::new(2);
        let later = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            sent(9_003);
            drop(guard);
        });
        assert!(sent_by_us(2, 9_003).await);
        later.await.unwrap();
        // A foreign post while a send is under way: checked once the send is done.
        let guard = Sending::new(3);
        let done = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            drop(guard);
        });
        assert!(!sent_by_us(3, 9_004).await);
        done.await.unwrap();
    }
}

use anyhow::{Context, Result};
use brim_automod::lockdown::VIEW_CHANNEL;
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long owner and member lookups are reused, so page loads do not drain the bot's rate limit.
const MEMO_TTL: Duration = Duration::from_secs(30);
/// How long [`Discord::call`] reuses a GET answer: one page load's parallel handlers share one
/// request, and a change made in Discord itself shows on the next load. A dashboard write clears it.
const GET_TTL: Duration = Duration::from_secs(2);
/// The most a request waits in all for Discord's rate limit before its 429 goes back.
const WAIT_BUDGET: f64 = 15.0;
const ADMINISTRATOR: u64 = 1 << 3;
/// [`Discord::unpermittable_roles`]'s reason for a role an Onboarding prompt gives.
pub(crate) const ONBOARDING_WHY: &str =
    "Discord Onboarding gives it to anyone who picks that answer";

#[derive(Clone)]
pub struct Discord {
    pub http: reqwest::Client,
    pub client_id: String,
    pub client_secret: String,
    pub bot_token: String,
    pub redirect_uri: String,
    /// Overridable (`DISCORD_API_BASE`) so the e2e script can point it at a mock.
    pub api: String,
    /// Bot GETs by path, with when they were fetched (None: 404).
    pub memo: crate::Shared<String, (Instant, Option<serde_json::Value>)>,
    pub limits: Arc<Limits>,
}

/// This process's view of Discord's rate limits; every bot request goes through [`Discord::send`].
#[derive(Default)]
pub struct Limits {
    /// "METHOD /path" (or "global") to when Discord opens it again, from its rate limit headers.
    until: Mutex<std::collections::HashMap<String, Instant>>,
    /// One lock per GET path, so handlers that ask at once share one request.
    flights: Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Counts writes: a GET that was under way during a write does not store its answer.
    writes: AtomicU64,
}

#[derive(Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
}

#[derive(Deserialize, serde::Serialize)]
pub struct Guild {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// Only the user's own list (`guilds` scope) carries these; the bot's list leaves them empty.
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub owner: bool,
    #[serde(default)]
    pub permissions: String,
}

impl Guild {
    /// Discord lets a person add a bot only where they own the server or have Manage Server or Administrator.
    pub fn can_add_bot(&self) -> bool {
        const MANAGE_GUILD: u64 = 1 << 5;
        self.owner
            || self
                .permissions
                .parse::<u64>()
                .is_ok_and(|p| p & (ADMINISTRATOR | MANAGE_GUILD) != 0)
    }
}

fn app_owner(app: Option<&serde_json::Value>) -> Option<String> {
    let app = app?;
    app["team"]["owner_user_id"]
        .as_str()
        .or(app["owner"]["id"].as_str())
        .map(String::from)
}

impl Discord {
    pub fn authorize_url(&self, state: &str) -> String {
        format!(
            "https://discord.com/oauth2/authorize?response_type=code&client_id={}&scope=identify%20guilds&state={state}&redirect_uri={}",
            self.client_id,
            urlencode(&self.redirect_uri)
        )
    }

    /// The bot invite from the front page; the same link as in DEPLOY.md.
    pub fn invite_url(&self) -> String {
        format!(
            "https://discord.com/oauth2/authorize?client_id={}&scope=bot+applications.commands&permissions=8",
            self.client_id
        )
    }

    /// The same invite with one server already chosen (the Servers page's Add to server button).
    pub fn invite_url_for(&self, guild_id: &str) -> String {
        format!(
            "{}&guild_id={guild_id}&disable_guild_select=true",
            self.invite_url()
        )
    }

    pub async fn exchange(&self, code: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct T {
            access_token: String,
        }
        let t: T = self
            .http
            .post(format!("{}/oauth2/token", self.api))
            .form(&[
                ("client_id", &self.client_id[..]),
                ("client_secret", &self.client_secret),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.redirect_uri),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(t.access_token)
    }

    pub async fn me(&self, token: &str) -> Result<User> {
        Ok(self
            .http
            .get(format!("{}/users/@me", self.api))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// The user's servers; `None` when Discord refuses the user's token (revoked or expired).
    pub async fn user_guilds(&self, token: &str) -> Result<Option<Vec<Guild>>> {
        let r = self
            .http
            .get(format!("{}/users/@me/guilds", self.api))
            .bearer_auth(token)
            .send()
            .await?;
        if r.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        Ok(Some(r.error_for_status()?.json().await?))
    }

    /// Ids of guilds the bot is in, paged 200 at a time.
    pub async fn bot_guild_ids(&self) -> Result<Vec<String>> {
        let mut out: Vec<String> = vec![];
        loop {
            let after = out
                .last()
                .map_or(String::new(), |id| format!("&after={id}"));
            let page: Vec<Guild> = serde_json::from_value(
                self.call(
                    reqwest::Method::GET,
                    &format!("/users/@me/guilds?limit=200{after}"),
                    None,
                )
                .await?,
            )?;
            let n = page.len();
            out.extend(page.into_iter().map(|g| g.id));
            if n < 200 {
                return Ok(out);
            }
        }
    }

    /// Loads the custom emoji ids, as the bot does, for the messages the dashboard sends.
    pub async fn load_emojis(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct E {
            name: String,
            id: String,
        }
        #[derive(Deserialize)]
        struct List {
            items: Vec<E>,
        }
        let list: List = serde_json::from_value(
            self.call(
                reqwest::Method::GET,
                &format!("/applications/{}/emojis", self.client_id),
                None,
            )
            .await?,
        )?;
        brim_logging::emoji::load(
            list.items
                .into_iter()
                .filter_map(|e| Some((e.name, e.id.parse().ok()?))),
        );
        Ok(())
    }

    /// A bot GET reused for [`MEMO_TTL`]; None when Discord answers 404.
    async fn memo_get(&self, path: String) -> Result<Option<serde_json::Value>> {
        self.memo_get_within(path, MEMO_TTL).await
    }

    /// [`Self::memo_get`] reusing an answer only while it is younger than `max_age`.
    async fn memo_get_within(
        &self,
        path: String,
        max_age: Duration,
    ) -> Result<Option<serde_json::Value>> {
        self.shared_get(&path, max_age, true).await
    }

    /// A bot GET that reuses an answer younger than `max_age`, and that handlers asking at once
    /// share. 404 is None when `none_on_404`, else an error like any other failed status.
    async fn shared_get(
        &self,
        path: &str,
        max_age: Duration,
        none_on_404: bool,
    ) -> Result<Option<serde_json::Value>> {
        let reuse = || {
            let m = self.memo.lock().unwrap();
            let (at, v) = m.get(path)?;
            (at.elapsed() < max_age && (none_on_404 || v.is_some())).then(|| v.clone())
        };
        if let Some(v) = reuse() {
            return Ok(v);
        }
        let flight = {
            let mut f = self.limits.flights.lock().unwrap();
            f.retain(|_, l| Arc::strong_count(l) > 1);
            f.entry(path.to_string()).or_default().clone()
        };
        let _one = flight.lock().await;
        if let Some(v) = reuse() {
            return Ok(v);
        }
        let writes = self.limits.writes.load(SeqCst);
        let r = self.send(reqwest::Method::GET, path, None).await?;
        let v = if none_on_404 && r.status() == 404 {
            None
        } else {
            let text = r.error_for_status()?.text().await?;
            // A bad body fails rather than being reused as Null; an empty one is Null.
            Some(match text.is_empty() {
                true => serde_json::Value::Null,
                false => serde_json::from_str(&text)?,
            })
        };
        let mut m = self.memo.lock().unwrap();
        if self.limits.writes.load(SeqCst) == writes {
            // shortcut: prunes the whole map on insert, fine for one process with a few thousand users.
            m.retain(|_, (at, _)| at.elapsed() < MEMO_TTL);
            m.insert(path.to_string(), (Instant::now(), v.clone()));
        }
        Ok(v)
    }

    /// Bot GET through [`Self::shared_get`] (2 s); None when Discord answers 404.
    pub async fn get_json(&self, path: &str) -> Result<Option<serde_json::Value>> {
        self.shared_get(path, GET_TTL, true).await
    }

    /// Why each role Discord itself hands out cannot be a permit role (owner decision): linked roles,
    /// server subscription or purchase roles (their tags), and the roles Onboarding prompts give.
    /// A guild without Onboarding (404) has none of those; any other failed lookup is an error.
    pub async fn unpermittable_roles(
        &self,
        guild: i64,
    ) -> Result<std::collections::HashMap<String, &'static str>> {
        let mut out = std::collections::HashMap::new();
        let roles = self.memo_get(format!("/guilds/{guild}/roles")).await?;
        for r in roles.iter().filter_map(|v| v.as_array()).flatten() {
            let tags = &r["tags"];
            // Discord sends these flags as `null` when set, and leaves them out when not.
            let why = if tags.get("guild_connections").is_some() {
                "it is a linked role, which Discord gives for a connected account"
            } else if tags.get("subscription_listing_id").is_some()
                || tags.get("available_for_purchase").is_some()
            {
                "it is a server subscription or purchase role, which Discord gives to anyone who pays"
            } else {
                continue;
            };
            out.insert(r["id"].as_str().unwrap_or_default().to_string(), why);
        }
        let onboarding = self.memo_get(format!("/guilds/{guild}/onboarding")).await?;
        let ids = onboarding
            .iter()
            .flat_map(brim_core::permits::onboarding_roles);
        for id in ids {
            out.entry(id.to_string()).or_insert(ONBOARDING_WHY);
        }
        Ok(out)
    }

    /// Ids of the guild's managed (bot, integration) roles, from the memoized role list.
    pub async fn managed_roles(&self, guild: i64) -> Result<Vec<String>> {
        let roles = self.memo_get(format!("/guilds/{guild}/roles")).await?;
        let roles = roles.iter().filter_map(|v| v.as_array()).flatten();
        let managed = roles.filter(|r| r["managed"] == true);
        Ok(managed
            .filter_map(|r| r["id"].as_str().map(String::from))
            .collect())
    }

    pub async fn guild_owner(&self, guild: i64) -> Result<String> {
        let g = self.memo_get(format!("/guilds/{guild}")).await?;
        g.as_ref()
            .and_then(|g| g["owner_id"].as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("guild {guild} not found"))
    }

    /// The Discord owner from an answer at most 2 s old, for owner-only writes: the
    /// 30 s memo can still name the previous owner.
    pub async fn guild_owner_fresh(&self, guild: i64) -> Result<String> {
        let g = self
            .memo_get_within(format!("/guilds/{guild}"), Duration::from_secs(2))
            .await?;
        g.as_ref()
            .and_then(|g| g["owner_id"].as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("guild {guild} not found"))
    }

    /// The server's name, or None when the bot is not in it.
    pub async fn guild_name(&self, guild: i64) -> Result<Option<String>> {
        let g = self.memo_get(format!("/guilds/{guild}")).await?;
        Ok(g.and_then(|g| g["name"].as_str().map(String::from)))
    }

    /// A role's name, or None when the server or the role is unknown.
    pub async fn role_name(&self, guild: i64, role: &str) -> Result<Option<String>> {
        let g = self.memo_get(format!("/guilds/{guild}")).await?;
        Ok(g.and_then(|g| {
            g["roles"].as_array()?.iter().find(|r| r["id"] == role)?["name"]
                .as_str()
                .map(String::from)
        }))
    }

    /// The server's name and approximate member count, or None when the bot is not in it.
    pub async fn guild_size(&self, guild: i64) -> Result<Option<(String, u64)>> {
        let g = self
            .memo_get(format!("/guilds/{guild}?with_counts=true"))
            .await?;
        Ok(g.map(|g| {
            let name = g["name"].as_str().unwrap_or_default().to_string();
            (name, g["approximate_member_count"].as_u64().unwrap_or(0))
        }))
    }

    /// The bot's own user id.
    pub async fn bot_id(&self) -> Result<String> {
        let me = self.memo_get("/users/@me".into()).await?;
        me.as_ref()
            .and_then(|m| m["id"].as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("bot user not found"))
    }

    /// When the bot joined `guild` (its member's `joined_at`), or None when it is not in it.
    pub async fn bot_joined(&self, guild: i64) -> Result<Option<String>> {
        let id = self.bot_id().await?;
        let m = self
            .memo_get(format!("/guilds/{guild}/members/{id}"))
            .await?;
        Ok(m.and_then(|m| m["joined_at"].as_str().map(String::from)))
    }

    /// A Discord user (name, avatar, bot flag), or None for an unknown id.
    pub async fn user(&self, id: i64) -> Result<Option<serde_json::Value>> {
        self.memo_get(format!("/users/{id}")).await
    }

    /// The user id of the bot application's owner (the team owner for a team app).
    pub async fn bot_owner(&self) -> Result<String> {
        let app = self.memo_get("/applications/@me".into()).await?;
        app_owner(app.as_ref()).ok_or_else(|| anyhow::anyhow!("application has no owner"))
    }

    /// The bot's own user id; for older applications it is not the application id.
    pub async fn bot_user(&self) -> Result<i64> {
        let me = self.memo_get("/users/@me".into()).await?;
        me.as_ref()
            .and_then(|u| u["id"].as_str()?.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("bot user not found"))
    }

    /// The member's role ids, or None when they are not in the guild.
    pub async fn member(&self, guild: i64, user: &str) -> Result<Option<Vec<String>>> {
        let m = self
            .memo_get(format!("/guilds/{guild}/members/{user}"))
            .await?;
        Ok(m.map(|m| {
            m["roles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r.as_str().map(String::from))
                .collect()
        }))
    }

    /// Whether `user` is in `guild` now (no memo): a rescue claim right after joining must see it.
    pub async fn is_member_fresh(&self, guild: i64, user: i64) -> Result<bool> {
        let m = self
            .memo_get_within(format!("/guilds/{guild}/members/{user}"), Duration::ZERO)
            .await?;
        Ok(m.is_some())
    }

    /// Whether the member is a bot account, from the same memoized lookup as [`Discord::member`].
    pub async fn member_is_bot(&self, guild: i64, user: &str) -> Result<bool> {
        let m = self
            .memo_get(format!("/guilds/{guild}/members/{user}"))
            .await?;
        Ok(m.is_some_and(|m| m["user"]["bot"].as_bool() == Some(true)))
    }

    /// Whether `ch` is in `guild` and hidden from @everyone and from each main role, by the
    /// permissions a lock saved (as the bot's `events::public`): the bot's rule for log channels.
    pub async fn private_channel_of(&self, guild: i64, ch: &str, who: &Readers) -> Result<bool> {
        let c = self
            .call(reqwest::Method::GET, &format!("/channels/{ch}"), None)
            .await?;
        if c["guild_id"].as_str() != Some(&guild.to_string()) {
            return Ok(false);
        }
        let g = self.memo_get(format!("/guilds/{guild}")).await?;
        let perms = |id: &str| -> u64 {
            let live: u64 = g
                .as_ref()
                .and_then(|g| g["roles"].as_array())
                .into_iter()
                .flatten()
                .find(|r| r["id"].as_str() == Some(id))
                .and_then(|r| r["permissions"].as_str()?.parse().ok())
                .unwrap_or(0);
            // A lock strips permissions for its duration; the channel is public again once it ends.
            who.saved
                .iter()
                .filter(|(r, _)| r.to_string() == id)
                .fold(live, |p, (_, s)| p | s)
        };
        let everyone = guild.to_string();
        let base = perms(&everyone);
        Ok(!sees(base, &c, &[&everyone])
            && !who
                .main
                .iter()
                .any(|r| sees(base | perms(r), &c, &[&everyone, r])))
    }

    pub async fn member_roles(&self, guild: i64, user: &str) -> Result<Vec<String>> {
        Ok(self.member(guild, user).await?.unwrap_or_default())
    }

    /// Bot-authenticated call for the less common endpoints; returns the JSON body (Null when empty).
    pub async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        if method == reqwest::Method::GET && body.is_none() {
            return Ok(self
                .shared_get(path, GET_TTL, false)
                .await?
                .unwrap_or_default());
        }
        let text = self
            .send(method, path, body)
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    /// DMs `user` the built-in message `key` (a server's saved copy, else the default), with mentions disabled.
    pub async fn dm(
        &self,
        db: &sqlx::PgPool,
        guild: Option<i64>,
        user: i64,
        key: &str,
        vars: &[(&str, &str)],
    ) -> Result<()> {
        let locked = brim_logging::botmsg::find(key).is_none_or(|d| d.locked);
        // A bot-wide entry's one copy is under guild 0, also when the DM is about a server.
        let scope = if brim_logging::botmsg::BOT_WIDE.contains(&key) {
            Some(0)
        } else {
            guild
        };
        let stored: Option<serde_json::Value> = match scope.filter(|_| !locked) {
            Some(g) => {
                sqlx::query_scalar(
                    "select message from bot_messages where guild_id = $1 and key = $2",
                )
                .bind(g)
                .bind(key)
                .fetch_optional(db)
                .await?
            }
            None => None,
        };
        let m = brim_logging::botmsg::render(key, stored.as_ref(), vars);
        let ch = self
            .call(
                reqwest::Method::POST,
                "/users/@me/channels",
                Some(json!({"recipient_id": user.to_string()})),
            )
            .await?;
        let ch = ch["id"].as_str().context("no DM channel id")?;
        self.call(
            reqwest::Method::POST,
            &format!("/channels/{ch}/messages"),
            Some(json!({"content": m["content"], "embeds": m["embeds"], "allowed_mentions": {"parse": []}})),
        )
        .await?;
        Ok(())
    }

    /// Like `call` but hands back the raw response, for callers that branch on the status.
    pub async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        // A 429, or a bucket Discord said is used up, waits the time Discord asks for and tries
        // again, up to 3 times and 15 s in all. A longer wait returns the 429 at once: the page
        // then says to try again, instead of retrying too early and failing after 30 s anyway
        // (the test server, 11-18). The wait is shared: other requests on that path wait too.
        let key = format!("{method} {}", path.split('?').next().unwrap_or(path));
        let (mut tries, mut waited) = (0, 0.0);
        loop {
            let open = {
                let u = self.limits.until.lock().unwrap();
                [u.get(&key), u.get("global")]
                    .into_iter()
                    .flatten()
                    .max()
                    .copied()
            };
            if let Some(at) = open {
                let wait = at
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64()
                    .min(WAIT_BUDGET - waited);
                if wait > 0.0 {
                    waited += wait;
                    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                }
            }
            let mut r = self
                .http
                .request(method.clone(), format!("{}{path}", self.api))
                .header("Authorization", format!("Bot {}", self.bot_token));
            if let Some(b) = &body {
                r = r.json(b);
            }
            // Without the URL: a webhook path holds its token, and errors end up in logs.
            let res = r.send().await.map_err(reqwest::Error::without_url)?;
            if method != reqwest::Method::GET {
                // Fresh data after a fix: no GET answer from before this write is reused.
                let mut m = self.memo.lock().unwrap();
                self.limits.writes.fetch_add(1, SeqCst);
                m.clear();
            }
            let limited = res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS;
            let h = res.headers();
            // Finite only: "NaN" parses, and a NaN wait would panic in `from_secs_f64`.
            let num = |k: &str| {
                let n = h.get(k)?.to_str().ok()?.parse::<f64>().ok()?;
                n.is_finite().then_some(n)
            };
            let wait = if limited {
                Some(num("retry-after").unwrap_or(1.0))
            } else if num("x-ratelimit-remaining") == Some(0.0) {
                num("x-ratelimit-reset-after")
            } else {
                None
            };
            if let Some(wait) = wait {
                let k = if h.contains_key("x-ratelimit-global") {
                    "global"
                } else {
                    &key
                };
                let mut u = self.limits.until.lock().unwrap();
                u.retain(|_, at| *at > Instant::now());
                u.insert(
                    k.into(),
                    Instant::now() + Duration::from_secs_f64(wait.clamp(0.0, 60.0)),
                );
            }
            if !limited || tries == 3 || waited + wait.unwrap_or(0.0) > WAIT_BUDGET {
                return Ok(res);
            }
            tries += 1;
        }
    }

    /// Posts a log embed of `guild` with mentions disabled.
    pub async fn post_log(
        &self,
        guild: i64,
        who: &Readers,
        channel: &str,
        log: brim_logging::Log,
    ) -> Result<()> {
        self.post_log_with(guild, who, channel, log, serde_json::json!([]))
            .await
            .map(drop)
    }

    /// [`Self::post_log`] with message components (raw JSON action rows).
    /// Only to a private channel of `guild`, checked at send time: a stored id from before the
    /// save-time checks must not post one server's log into another, or where every member reads it.
    pub async fn post_log_with(
        &self,
        guild: i64,
        who: &Readers,
        channel: &str,
        log: brim_logging::Log,
        components: serde_json::Value,
    ) -> Result<String> {
        let embed = serde_json::to_value(log.build())?;
        self.post_embed_with(guild, who, channel, embed, components)
            .await
    }

    /// [`Self::post_log_with`] for a finished embed (JSON, one [`framed`] already).
    pub async fn post_embed_with(
        &self,
        guild: i64,
        who: &Readers,
        channel: &str,
        embed: serde_json::Value,
        components: serde_json::Value,
    ) -> Result<String> {
        anyhow::ensure!(
            self.private_channel_of(guild, channel, who).await?,
            "not posting to channel {channel}: it is not in this server, or every member can read it; pick a private log channel of this server"
        );
        let sent = self
            .call(
                reqwest::Method::POST,
                &format!("/channels/{channel}/messages"),
                Some(json!({"embeds": [embed], "components": components, "allowed_mentions": {"parse": []}})),
            )
            .await?;
        Ok(sent["id"].as_str().unwrap_or_default().to_string())
    }
}

/// `log` with this server's edited version laid over it (Bot messages page): its own entry
/// first, else the frame of its group `group` (`log.<group>`). The copy's fields are added to the
/// log's own (the appeal answers stay). A failed read sends the log as built.
pub async fn framed(
    db: &sqlx::PgPool,
    guild: i64,
    group: &str,
    log: brim_logging::Log,
) -> serde_json::Value {
    use brim_logging::botmsg;
    let (entry, vars) = log.meta();
    let keys: Vec<String> = entry
        .map(String::from)
        .into_iter()
        .chain([group.to_string()])
        .collect();
    let vars = vars.to_vec();
    let embed = log.build();
    for k in &keys {
        if botmsg::find(k).is_none_or(|d| d.locked) {
            continue;
        }
        let stored: Option<serde_json::Value> =
            sqlx::query_scalar("select message from bot_messages where guild_id = $1 and key = $2")
                .bind(guild)
                .bind(k)
                .fetch_optional(db)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("reading the edited log failed: {e:#}");
                    None
                });
        // A copy that lost a required fact is never sent (`render` would fall back anyway).
        if let Some(s) = stored.filter(|s| botmsg::missing_text(k, s).is_none()) {
            let e = brim_logging::overlay::over(k, &s, &embed, &vars, false);
            return serde_json::to_value(e).unwrap_or_default();
        }
    }
    serde_json::to_value(embed).unwrap_or_default()
}

/// Who every member is, for [`Discord::private_channel_of`]: the main roles (`lockdown.main_roles`)
/// and the role permissions the active locks saved.
pub struct Readers {
    pub main: Vec<String>,
    pub saved: Vec<(u64, u64)>,
}

impl Readers {
    pub async fn load(db: &sqlx::PgPool, guild: i64, config: &serde_json::Value) -> Result<Self> {
        let main = config["lockdown"]["main_roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                r.as_str()
                    .map(String::from)
                    .or_else(|| r.as_u64().map(|n| n.to_string()))
            })
            .collect();
        let saved = brim_automod::store::locks(db, guild as u64)
            .await?
            .into_iter()
            .flat_map(|l| l.role_perms)
            .collect();
        Ok(Self { main, saved })
    }
}

/// Whether a member with `base` permissions and the overwrites of `ids` (@everyone first, then a
/// role) can view `ch`, as the bot's `events::sees`.
fn sees(base: u64, ch: &serde_json::Value, ids: &[&str]) -> bool {
    if base & ADMINISTRATOR != 0 {
        return true;
    }
    let bits = |o: &serde_json::Value, k: &str| {
        o[k].as_str()
            .and_then(|b| b.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let ows = ch["permission_overwrites"].as_array();
    ids.iter().fold(base, |p, id| {
        let (allow, deny) = ows
            .into_iter()
            .flatten()
            .find(|o| o["id"].as_str() == Some(id))
            .map_or((0, 0), |o| (bits(o, "allow"), bits(o, "deny")));
        (p & !deny) | allow
    }) & VIEW_CHANNEL
        != 0
}

pub(crate) fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use serde_json::json;

    #[test]
    fn a_person_may_add_the_bot_with_manage_server_administrator_or_ownership() {
        let g = |owner: bool, p: &str| Guild {
            id: "1".into(),
            name: "n".into(),
            icon: None,
            banner: None,
            owner,
            permissions: p.into(),
        };
        assert!(g(true, "0").can_add_bot());
        assert!(g(false, "32").can_add_bot());
        assert!(g(false, "8").can_add_bot());
        assert!(!g(false, "2048").can_add_bot());
        assert!(!g(false, "").can_add_bot());
    }

    #[test]
    fn app_owner_prefers_the_team_owner() {
        let solo = json!({"owner": {"id": "1"}});
        let team = json!({"owner": {"id": "9"}, "team": {"owner_user_id": "2"}});
        assert_eq!(app_owner(Some(&solo)).as_deref(), Some("1"));
        assert_eq!(app_owner(Some(&team)).as_deref(), Some("2"));
        assert_eq!(app_owner(None), None);
    }

    #[tokio::test]
    async fn send_retries_after_429() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static HITS: AtomicU32 = AtomicU32::new(0);
        let app = axum::Router::new().route(
            "/x",
            axum::routing::get(|| async {
                if HITS.fetch_add(1, Ordering::SeqCst) < 2 {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "0")],
                    )
                        .into_response()
                } else {
                    "{}".into_response()
                }
            }),
        );
        // A wait longer than the budget comes back at once instead of retrying too early.
        let app = app.route(
            "/long",
            axum::routing::get(|| async {
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "40")],
                )
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let d = mock(api);
        let res = d.send(reqwest::Method::GET, "/x", None).await.unwrap();
        assert!(res.status().is_success());
        assert_eq!(HITS.load(Ordering::SeqCst), 3);
        let at = std::time::Instant::now();
        let res = d.send(reqwest::Method::GET, "/long", None).await.unwrap();
        assert_eq!(res.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert!(at.elapsed() < Duration::from_secs(2));
    }

    fn mock(api: String) -> Discord {
        Discord {
            http: reqwest::Client::new(),
            client_id: String::new(),
            client_secret: String::new(),
            bot_token: String::new(),
            redirect_uri: String::new(),
            api,
            memo: Default::default(),
            limits: Default::default(),
        }
    }

    #[tokio::test]
    async fn parallel_gets_share_one_request_and_a_write_clears_them() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static GETS: AtomicU32 = AtomicU32::new(0);
        let app = axum::Router::new().route(
            "/guilds/1/channels",
            axum::routing::get(|| async {
                GETS.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                "[]"
            })
            .patch(|| async { "[]" }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let d = mock(api);
        let get = || d.call(reqwest::Method::GET, "/guilds/1/channels", None);
        let (a, b, c) = tokio::join!(get(), get(), get());
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        assert_eq!(GETS.load(Ordering::SeqCst), 1);
        let patch = d.call(
            reqwest::Method::PATCH,
            "/guilds/1/channels",
            Some(json!([])),
        );
        patch.await.unwrap();
        get().await.unwrap();
        assert_eq!(
            GETS.load(Ordering::SeqCst),
            2,
            "a write must not leave the old answer"
        );
    }

    #[tokio::test]
    async fn a_used_up_bucket_makes_the_next_request_wait() {
        let app = axum::Router::new().route(
            "/x",
            axum::routing::post(|| async {
                (
                    [
                        ("x-ratelimit-remaining", "0"),
                        ("x-ratelimit-reset-after", "0.4"),
                    ],
                    "{}",
                )
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let d = mock(api);
        d.call(reqwest::Method::POST, "/x", Some(json!({})))
            .await
            .unwrap();
        let at = std::time::Instant::now();
        d.call(reqwest::Method::POST, "/x", Some(json!({})))
            .await
            .unwrap();
        assert!(at.elapsed() >= Duration::from_millis(350));
    }

    #[test]
    fn sees_follows_base_and_overwrites() {
        let ow = |allow: u64, deny: u64| json!({"permission_overwrites": [{"id": "9", "allow": allow.to_string(), "deny": deny.to_string()}]});
        assert!(sees(VIEW_CHANNEL, &json!({}), &["9"]));
        assert!(!sees(VIEW_CHANNEL, &ow(0, VIEW_CHANNEL), &["9"]));
        assert!(!sees(0, &json!({}), &["9"]));
        assert!(sees(0, &ow(VIEW_CHANNEL, 0), &["9"]));
        assert!(sees(ADMINISTRATOR, &ow(0, VIEW_CHANNEL), &["9"]));
        // Hidden from @everyone but opened for a main role: that role's members read it.
        let both = json!({"permission_overwrites": [
            {"id": "9", "allow": "0", "deny": VIEW_CHANNEL.to_string()},
            {"id": "5", "allow": VIEW_CHANNEL.to_string(), "deny": "0"}]});
        assert!(!sees(VIEW_CHANNEL, &both, &["9"]));
        assert!(sees(VIEW_CHANNEL, &both, &["9", "5"]));
    }
}

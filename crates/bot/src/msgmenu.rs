//! Message Builder menus: a member picks an option of a menu Brim posted (`mb:<message>:<row>`)
//! and gets that option's answer as a reply only they can see. Nothing else happens.
use anyhow::Result;
use brim_core::msgvars::{self, Ctx};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};
use twilight_model::{
    application::interaction::{Interaction, InteractionData},
    channel::message::{AllowedMentions, MessageFlags},
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    id::{marker::GuildMarker, Id},
};

use crate::app::App;

pub const PREFIX: &str = "mb:";

/// Last answer per (member, saved message, row), and recent answers per server, in ms.
type Hits = (HashMap<(u64, i64, usize), i64>, HashMap<u64, Vec<i64>>);
static HITS: LazyLock<Mutex<Hits>> = LazyLock::new(Default::default);

/// 1 answer per 3 seconds per member and menu, 30 a minute per server.
fn allowed(guild: u64, user: u64, id: i64, row: usize, now: i64) -> bool {
    let mut h = HITS.lock().unwrap_or_else(|e| e.into_inner());
    let (per_member, per_server) = &mut *h;
    if per_member.len() > 10_000 {
        per_member.retain(|_, t| now - *t < 3_000);
    }
    if per_member
        .get(&(user, id, row))
        .is_some_and(|t| now - t < 3_000)
    {
        return false;
    }
    let s = per_server.entry(guild).or_default();
    s.retain(|t| now - t < 60_000);
    if s.len() >= 30 {
        return false;
    }
    s.push(now);
    per_member.insert((user, id, row), now);
    true
}

/// The answer for (saved message, copy, row, option) in this server: from that copy's Sent log row
/// (the message as that copy shows it), or, for a copy sent before the Sent log (`send` None), the
/// copy Brim last posted. Never a draft saved since. `None` when the message, the copy, the menu
/// or the option is gone. The server must match, so a menu never reads another server's message.
async fn answer(
    app: &App,
    guild: i64,
    id: i64,
    send: Option<i64>,
    posted: i64,
    row: usize,
    opt: usize,
) -> Result<Option<String>> {
    let m: Option<Option<Value>> = match send {
        Some(send) => sqlx::query_scalar(
            "select body from message_sends where id = $1 and embed_id = $2 and guild_id = $3",
        )
        .bind(send)
        .bind(id)
        .bind(guild)
        .fetch_optional(&app.pool)
        .await?
        .map(Some),
        // A copy from before the Sent log: its own seeded row, found by its Discord message id.
        None => match sqlx::query_scalar(
            "select body from message_sends where posted_id = $1 and embed_id = $2 and guild_id = $3",
        )
        .bind(posted)
        .bind(id)
        .bind(guild)
        .fetch_optional(&app.pool)
        .await?
        {
            Some(b) => Some(Some(b)),
            None => sqlx::query_scalar("select published from embeds where id = $1 and guild_id = $2")
                .bind(id)
                .bind(guild)
                .fetch_optional(&app.pool)
                .await?,
        },
    };
    Ok(m.flatten().and_then(|m| {
        m.pointer(&format!(
            "/components/{row}/components/0/options/{opt}/answer"
        ))
        .and_then(Value::as_str)
        .filter(|a| !a.trim().is_empty())
        .map(String::from)
    }))
}

/// The values for the variables an answer uses: the member who picked, the server, the channel.
async fn vars(app: &App, i: &Interaction, text: &str) -> Result<Ctx> {
    let used = msgvars::used(text);
    let mut c = Ctx::now();
    let Some(gid) = i.guild_id else {
        return Ok(c);
    };
    if let Some(m) = &i.member {
        if let Some(u) = &m.user {
            let uid = u.id.get();
            c.raw.insert("member", format!("<@{uid}>"));
            c.text.insert("member.id", uid.to_string());
            c.text.insert("member.username", u.name.clone());
            c.text.insert(
                "member.display_name",
                m.nick
                    .clone()
                    .or_else(|| u.global_name.clone())
                    .unwrap_or_else(|| u.name.clone()),
            );
            let avatar = match (m.avatar, u.avatar) {
                (Some(h), _) => {
                    format!("https://cdn.discordapp.com/guilds/{gid}/users/{uid}/avatars/{h}.png")
                }
                (None, Some(h)) => format!("https://cdn.discordapp.com/avatars/{uid}/{h}.png"),
                (None, None) => format!(
                    "https://cdn.discordapp.com/embed/avatars/{}.png",
                    (uid >> 22) % 6
                ),
            };
            c.raw.insert("member.avatar", avatar);
            c.raw.insert(
                "member.created_at",
                format!("<t:{}:R>", msgvars::snowflake_secs(uid)),
            );
        }
        if let Some(j) = m.joined_at {
            c.raw
                .insert("member.joined_at", format!("<t:{}:R>", j.as_secs()));
        }
        if used.contains(&"member.top_role") {
            let info = app.guild_info(gid).await?;
            match m
                .roles
                .iter()
                .max_by_key(|r| info.roles.get(r).copied().unwrap_or(0))
            {
                Some(r) => c.raw.insert("member.top_role", format!("<@&{r}>")),
                None => c.text.insert("member.top_role", "@everyone".into()),
            };
        }
    }
    if let Some(ch) = &i.channel {
        c.raw.insert("channel", format!("<#{}>", ch.id));
        c.text.insert("channel.id", ch.id.to_string());
        if let Some(n) = &ch.name {
            c.text.insert("channel.name", n.clone());
        }
        if let Some(t) = &ch.topic {
            c.text.insert("channel.topic", t.clone());
        }
        if let (true, Some(p)) = (used.contains(&"channel.category"), ch.parent_id) {
            if let Some(n) = app.http.channel(p).await?.model().await?.name {
                c.text.insert("channel.category", n);
            }
        }
    }
    if let (Some(ch), Some(msg)) = (&i.channel, &i.message) {
        c.raw.insert(
            "message.link",
            format!("https://discord.com/channels/{gid}/{}/{}", ch.id, msg.id),
        );
    }
    if used.iter().any(|v| v.starts_with("server.")) {
        let g = app.http.guild(gid).with_counts(true).await?.model().await?;
        c.text.insert("server.name", g.name);
        c.text.insert("server.id", gid.to_string());
        c.text.insert(
            "server.member_count",
            g.approximate_member_count.unwrap_or(0).to_string(),
        );
        c.text.insert(
            "server.boost_count",
            g.premium_subscription_count.unwrap_or(0).to_string(),
        );
        c.text.insert(
            "server.boost_level",
            match u8::from(g.premium_tier) {
                0 => "No level".into(),
                n => format!("Level {n}"),
            },
        );
        c.raw.insert("server.owner", format!("<@{}>", g.owner_id));
        if let Some(h) = g.icon {
            c.raw.insert(
                "server.icon",
                format!("https://cdn.discordapp.com/icons/{gid}/{h}.png"),
            );
        }
        c.raw.insert(
            "server.created_at",
            format!("<t:{}:R>", msgvars::snowflake_secs(gid.get())),
        );
    }
    if used.contains(&"role.member_count") {
        c.role_counts = role_counts(app, gid).await?;
    }
    Ok(c)
}

/// Role member counts per server, kept a minute: each count pages the member list, and menu
/// clicks must not make the bot page it again and again.
type Counts = HashMap<u64, (std::time::Instant, HashMap<u64, u64>)>;
static ROLE_COUNTS: LazyLock<Mutex<Counts>> = LazyLock::new(Default::default);

async fn role_counts(app: &App, gid: Id<GuildMarker>) -> Result<HashMap<u64, u64>> {
    const FRESH: std::time::Duration = std::time::Duration::from_secs(60);
    {
        let mut all = ROLE_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|_, (at, _)| at.elapsed() < FRESH);
        if let Some((_, counts)) = all.get(&gid.get()) {
            return Ok(counts.clone());
        }
    }
    let mut counts = HashMap::new();
    {
        // shortcut: counts the first 10 000 members; page further for bigger servers.
        let mut after = None;
        for _ in 0..10 {
            let mut req = app.http.guild_members(gid).limit(1000);
            if let Some(a) = after {
                req = req.after(a);
            }
            let page = req.await?.models().await?;
            for m in &page {
                for r in &m.roles {
                    *counts.entry(r.get()).or_default() += 1;
                }
            }
            match page.last() {
                Some(m) if page.len() == 1000 => after = Some(m.user.id),
                _ => break,
            }
        }
    }
    let mut all = ROLE_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    all.insert(gid.get(), (std::time::Instant::now(), counts.clone()));
    Ok(counts)
}

/// `<message>:<copy>:<row>`, or `<message>:<row>` from a copy sent before the Sent log.
fn parse(rest: &str) -> Option<(i64, Option<i64>, usize)> {
    let p: Vec<&str> = rest.split(':').collect();
    match p[..] {
        [id, row] => Some((id.parse().ok()?, None, row.parse().ok()?)),
        [id, send, row] => Some((
            id.parse().ok()?,
            Some(send.parse().ok()?),
            row.parse().ok()?,
        )),
        _ => None,
    }
}

/// Answers a pick in a Message Builder menu. Returns false for other interactions.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let Some(InteractionData::MessageComponent(c)) = &i.data else {
        return Ok(false);
    };
    let Some(rest) = c.custom_id.strip_prefix(PREFIX) else {
        return Ok(false);
    };
    let parsed = parse(rest);
    let opt = c.values.first().and_then(|v| v.parse::<usize>().ok());
    let user = i.author_id().map_or(0, |u| u.get());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let found = match (i.guild_id, parsed, opt) {
        (Some(g), Some((id, _, row)), Some(_)) if !allowed(g.get(), user, id, row, now) => {
            Err("reply.menu.slow_down")
        }
        (Some(g), Some((id, send, row)), Some(opt)) if row < 5 && opt < 25 => {
            let posted = i.message.as_ref().map_or(0, |m| m.id.get() as i64);
            Ok(answer(app, g.get() as i64, id, send, posted, row, opt).await?)
        }
        _ => Ok(None),
    };
    // A short reply entry when the pick gets no answer of the builder's own.
    let (text, refused) = match found {
        Err(key) => (String::new(), Some(key)),
        Ok(None) => (String::new(), Some("reply.menu.unused")),
        Ok(Some(a)) => (a, None),
    };
    // Discord waits 3 seconds for the first reply; reading members or the server can take longer,
    // so the private reply is announced first and filled in after.
    let client = app.http.interaction(app.me.cast());
    client
        .create_response(
            i.id,
            &i.token,
            &InteractionResponse {
                kind: InteractionResponseType::DeferredChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    flags: Some(MessageFlags::EPHEMERAL),
                    ..Default::default()
                }),
            },
        )
        .await?;
    let (content, embeds): (String, Vec<_>) = match (refused, i.guild_id) {
        (Some(key), Some(g)) => {
            let said = crate::reply::say(app, g, key, &[]).await;
            (said.text, said.embeds)
        }
        // Outside a server there is no edited version to use.
        (Some(key), None) => {
            let def = brim_logging::botmsg::render(key, None, &[]);
            (def["content"].as_str().unwrap_or_default().into(), vec![])
        }
        (None, _) => {
            // A failed lookup (a hidden category, no member list) leaves those variables as typed.
            let ctx = vars(app, i, &text).await.unwrap_or_else(|e| {
                tracing::warn!("menu answer variables: {e:#}");
                Ctx::now()
            });
            (msgvars::fill(&text, &ctx), vec![])
        }
    };
    let content: String = content.chars().take(2000).collect();
    client
        .update_response(&i.token)
        .content(Some(content.as_str()).filter(|c| !c.is_empty()))
        .embeds(Some(&embeds))
        // Names and mentions in an answer never notify anyone.
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_both_custom_id_shapes() {
        use super::parse;
        assert_eq!(parse("9:2"), Some((9, None, 2)));
        assert_eq!(parse("9:15:2"), Some((9, Some(15), 2)));
        assert_eq!(parse("9:x:2"), None);
        assert_eq!(parse("9:1:2:3"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn answers_are_rate_limited() {
        use super::allowed;
        assert!(allowed(1, 2, 3, 0, 0));
        assert!(
            !allowed(1, 2, 3, 0, 2_999),
            "same member and menu within 3 s"
        );
        assert!(allowed(1, 2, 3, 1, 10), "another menu row");
        assert!(allowed(1, 2, 3, 0, 3_000));
        for u in 10..37 {
            assert!(allowed(1, u, 3, 0, 4_000));
        }
        assert!(!allowed(1, 99, 3, 0, 4_001), "30 a minute per server");
        assert!(allowed(5, 99, 3, 0, 4_001), "other servers are not counted");
        assert!(allowed(1, 99, 3, 0, 64_001));
    }
}

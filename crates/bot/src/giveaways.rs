//! Giveaways on Discord: the message with its Enter button, the private entry answers, the draw
//! at the end time, rerolls and redraws, the re-post of a deleted message, entries that lose a
//! required role, and the `giveaway` command. The rows and the entry rules are in
//! `brim_core::giveaways`; the dashboard makes and changes giveaways through the same rows.
use anyhow::{Context, Result};
use brim_core::{
    giveaway_plans as plans,
    giveaways::{self as store, CodeTry, Entered, Facts, Giveaway, Refusal, Req},
    jobs,
    permits::Level,
};
use brim_logging::{botmsg, embed::ORANGE, Log, LogKind};
use serde_json::{json, Value};
use twilight_model::{
    application::interaction::{
        application_command::{CommandDataOption, CommandOptionValue},
        Interaction, InteractionData,
    },
    channel::{
        message::{
            component::{ActionRow, Button, ButtonStyle, Component, TextInput, TextInputStyle},
            AllowedMentions, Embed, EmojiReactionType,
        },
        thread::AutoArchiveDuration,
        ChannelType,
    },
    gateway::payload::incoming::ReactionAdd,
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    id::{
        marker::{ChannelMarker, GuildMarker, RoleMarker, UserMarker},
        Id,
    },
    util::Timestamp,
};

use crate::{
    app::{now_secs, App},
    events::send_log,
    http_err,
    reply::{self, parts},
};

const ENTER: &str = "gw:e:";
const ANSWER: &str = "gw:a:";
const CLAIM: &str = "gw:c:";
/// The entry code form.
const CODE: &str = "gw:k:";

/// The Claim button under the winners post and in the winner DM.
fn claim_row(id: i64) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components: vec![Component::Button(Button {
            id: None,
            custom_id: Some(format!("{CLAIM}{id}")),
            disabled: false,
            emoji: Some(EmojiReactionType::Unicode {
                name: "🏆".into()
            }),
            label: Some("Claim".into()),
            style: ButtonStyle::Success,
            url: None,
            sku_id: None,
        })],
    })
}

/// "48 hours", for the claim time in the posts.
fn hours_text(h: i64) -> String {
    match h {
        1 => "1 hour".into(),
        h => format!("{h} hours"),
    }
}

fn when(t: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>) -> String {
    let s = t.timestamp();
    format!("<t:{s}:R> (<t:{s}:f>)")
}

fn mentions(ids: &[i64]) -> String {
    ids.iter()
        .map(|u| format!("<@{u}>"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The public winner text: names up to the ping cap, above it only the count.
fn winners_text(ids: &[i64]) -> String {
    if ids.len() > store::PING_CAP {
        format!("{} winners", ids.len())
    } else {
        mentions(ids)
    }
}

fn link(g: &Giveaway) -> String {
    match g.message_id {
        Some(m) => format!(
            "https://discord.com/channels/{}/{}/{m}",
            g.guild_id, g.channel_id
        ),
        None => String::new(),
    }
}

fn roles(list: &[Id<RoleMarker>]) -> String {
    list.iter()
        .map(|r| format!("<@&{r}>"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Who may enter and how, one rule per line, for the running message.
fn requirements(g: &Giveaway) -> String {
    let req = &g.req.0;
    let mut lines = vec![];
    if g.extras.reaction {
        lines.push(format!(
            "**To enter:** react with {}",
            emoji_text(&g.button_emoji)
        ));
    }
    if g.has_code {
        lines.push("**Entry code:** press the button and type the code".to_string());
    }
    if !req.required.is_empty() {
        lines.push(format!("**Needs one of:** {}", roles(&req.required)));
    }
    if req.min_age_days > 0 {
        lines.push(format!(
            "**Account age:** at least {} days",
            req.min_age_days
        ));
    }
    if req.min_member_hours > 0 {
        lines.push(format!(
            "**Time in the server:** at least {} hours",
            req.min_member_hours
        ));
    }
    if !req.blocked.is_empty() {
        lines.push(format!("**Not open to:** {}", roles(&req.blocked)));
    }
    let extra: Vec<String> = req
        .extra
        .iter()
        .filter(|e| e.entries > 1)
        .map(|e| format!("<@&{}> {}", e.role, e.entries))
        .collect();
    if !extra.is_empty() {
        lines.push(format!("**Extra entries:** {}", extra.join(", ")));
    }
    lines.join("\n")
}

/// Fills `{e:name}` tags in every text of a rendered message.
fn fill_tags(v: &mut Value) {
    match v {
        Value::String(s) => *s = brim_logging::emoji::fill(s),
        Value::Array(a) => a.iter_mut().for_each(fill_tags),
        Value::Object(o) => o.values_mut().for_each(fill_tags),
        _ => {}
    }
}

/// The server's copy of `key` or its default, filled, with Brim emoji tags.
async fn rendered(
    app: &App,
    gid: Id<GuildMarker>,
    key: &str,
    vars: &[(&str, &str)],
) -> (String, Vec<Embed>) {
    let stored = reply::stored_msg(app, gid, key).await;
    // The prize is staff text in a post and DMs in Brim's voice: no masked links or formatting.
    let prize = vars.iter().find(|v| v.0 == "prize").map(|v| reply::md(v.1));
    let vars: Vec<(&str, &str)> = vars
        .iter()
        .map(|&(k, v)| {
            (
                k,
                if k == "prize" {
                    prize.as_deref().unwrap_or(v)
                } else {
                    v
                },
            )
        })
        .collect();
    let mut m = botmsg::render(key, stored.as_ref(), &vars);
    fill_tags(&mut m);
    parts(&m)
}

/// A text entry as one line of text (its text, or its first embed's description).
async fn line(app: &App, gid: Id<GuildMarker>, key: &str, vars: &[(&str, &str)]) -> String {
    let (text, embeds) = rendered(app, gid, key, vars).await;
    if text.is_empty() {
        return embeds
            .first()
            .and_then(|e| e.description.clone())
            .unwrap_or_default();
    }
    text
}

/// `🎉` or `<:name:id>` as a button emoji; anything else none.
fn button_emoji(s: &str) -> Option<EmojiReactionType> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(inner) = s.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
        let mut it = inner.split(':');
        let animated = it.next()? == "a";
        let name = it.next()?.to_string();
        let id = Id::new_checked(it.next()?.parse().ok()?)?;
        return Some(EmojiReactionType::Custom {
            animated,
            id,
            name: Some(name),
        });
    }
    (s.chars().count() <= 8 && !s.chars().any(char::is_alphanumeric))
        .then(|| EmojiReactionType::Unicode { name: s.into() })
}

/// The reaction that enters a reaction giveaway: the button emoji, or 🎉.
fn reaction_emoji(s: &str) -> EmojiReactionType {
    button_emoji(s).unwrap_or(EmojiReactionType::Unicode {
        name: "🎉".into()
    })
}

/// The reaction emoji as message text.
fn emoji_text(s: &str) -> String {
    match reaction_emoji(s) {
        EmojiReactionType::Custom { animated, id, name } => format!(
            "<{}:{}:{id}>",
            if animated { "a" } else { "" },
            name.unwrap_or_default()
        ),
        EmojiReactionType::Unicode { name } => name,
    }
}

/// The same emoji; a Unicode one also without its variation selector.
fn same_emoji(a: &EmojiReactionType, b: &EmojiReactionType) -> bool {
    match (a, b) {
        (EmojiReactionType::Custom { id: x, .. }, EmojiReactionType::Custom { id: y, .. }) => {
            x == y
        }
        (EmojiReactionType::Unicode { name: x }, EmojiReactionType::Unicode { name: y }) => {
            x.trim_end_matches('\u{fe0f}') == y.trim_end_matches('\u{fe0f}')
        }
        _ => false,
    }
}

fn enter_row(g: &Giveaway) -> Component {
    let label = Some(g.button_label.trim())
        .filter(|l| !l.is_empty())
        .unwrap_or("Enter");
    Component::ActionRow(ActionRow {
        id: None,
        components: vec![Component::Button(Button {
            id: None,
            custom_id: Some(format!("{ENTER}{}", g.id)),
            disabled: false,
            emoji: button_emoji(&g.button_emoji),
            label: Some(label.into()),
            style: ButtonStyle::Primary,
            url: None,
            sku_id: None,
        })],
    })
}

/// The message for the giveaway's state: the running one has the Enter button.
async fn message(app: &App, g: &Giveaway, entries: i64) -> (String, Vec<Embed>, Vec<Component>) {
    let gid = Id::new(g.guild_id as u64);
    let id = g.id.to_string();
    let host = g.host_id.map(|h| format!("<@{h}>")).unwrap_or_default();
    let sponsor = g.sponsor_id.map(|s| format!("<@{s}>")).unwrap_or_default();
    let entries = entries.to_string();
    match g.status.as_str() {
        "running" => {
            let (winners, req) = (g.winners.to_string(), requirements(g));
            let vars = [
                ("prize", g.prize.as_str()),
                ("ends", &when(g.ends_at)),
                ("host", &host),
                ("sponsor", &sponsor),
                ("entries", &entries),
                ("winners", &winners),
                ("requirements", &req),
                ("id", &id),
            ];
            let (t, e) = rendered(app, gid, "giveaway.running", &vars).await;
            let rows = match g.extras.reaction {
                true => vec![],
                false => vec![enter_row(g)],
            };
            (t, e, rows)
        }
        "ended" => {
            let ended = g.ended_at.map(when).unwrap_or_default();
            let winners = match g.winner_ids.is_empty() {
                true => "Nobody".to_string(),
                false => winners_text(&g.winner_ids),
            };
            let note = ended_note(g);
            let vars = [
                ("prize", g.prize.as_str()),
                ("ended", &ended),
                ("host", &host),
                ("sponsor", &sponsor),
                ("entries", &entries),
                ("winners", &winners),
                ("note", &note),
                ("id", &id),
            ];
            let (t, e) = rendered(app, gid, "giveaway.ended", &vars).await;
            (t, e, vec![])
        }
        _ => {
            let vars = [
                ("prize", g.prize.as_str()),
                ("host", &host),
                ("sponsor", &sponsor),
                ("id", &id),
            ];
            let (t, e) = rendered(app, gid, "giveaway.cancelled", &vars).await;
            (t, e, vec![])
        }
    }
}

/// Why the winners differ from what was asked: a late end, fewer winners, nobody.
fn ended_note(g: &Giveaway) -> String {
    let mut notes = vec![];
    let late = g
        .ended_at
        .is_some_and(|e| (e - g.ends_at).num_seconds() > 120);
    if late {
        notes.push("Ended late: Brim was offline at the end time.".to_string());
    }
    let n = g.winner_ids.len();
    if n == 0 && g.draws > 0 {
        notes.push("No valid entrants, so nobody won.".into());
    } else if g.draws > 0 && n < g.winners as usize {
        notes.push(format!("Only {n} members could win."));
    }
    notes.join("\n")
}

/// What Discord said about a post or edit in the giveaway's channel.
enum Posted {
    /// The message is gone: post it again.
    MessageGone,
    /// The channel is gone, or Brim cannot post there.
    Problem(&'static str),
}

const CHANNEL_GONE: &str =
    "The channel was deleted. Pick a new channel on the dashboard, and Brim posts the giveaway there.";
const BAD_PRIZE: &str =
    "The prize breaks a filter of this server (words, scam, phishing or invite links). Change the prize.";
const PLAN_CHANNEL_GONE: &str =
    "The channel of this plan was deleted. Pick a channel and turn the plan on again.";
const NO_ACCESS: &str =
    "Brim cannot post in the channel. Give Brim View Channel, Send Messages and Embed Links there.";

fn posted(e: &anyhow::Error) -> Option<Posted> {
    match http_err::code(e) {
        Some(10008) => Some(Posted::MessageGone),
        Some(10003) => Some(Posted::Problem(CHANNEL_GONE)),
        Some(50001 | 50013) => Some(Posted::Problem(NO_ACCESS)),
        _ => None,
    }
}

/// Posts the giveaway message in its channel and saves where it is. The first post pings the
/// sponsor only. A channel problem is saved as the issue for the dashboard and is not an error.
pub(crate) async fn post(app: &App, g: &Giveaway) -> Result<Option<i64>> {
    // Text Brim carries into a channel goes through the server's word, scam, phishing and
    // invite filters, as ticket relays do.
    let cfg = app.cfg(g.guild_id as u64).await?;
    if crate::tickets::dm::filtered(app, &cfg, &g.prize) {
        store::set_issue(&app.pool, g.guild_id, g.id, Some(BAD_PRIZE)).await?;
        problem_log(app, g, BAD_PRIZE).await;
        return Ok(None);
    }
    let entries = store::entry_count(&app.pool, g.id).await?;
    let (text, embeds, rows) = message(app, g, entries).await;
    let ping = AllowedMentions {
        users: g
            .sponsor_id
            .filter(|_| g.message_id.is_none() && g.shown_entries < 0)
            .and_then(|s| Id::new_checked(s as u64))
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let channel = Id::new(g.channel_id as u64);
    let r = app
        .http
        .create_message(channel)
        .content(&text)
        .embeds(&embeds)
        .components(&rows)
        .allowed_mentions(Some(&ping))
        .await;
    let sent = match r {
        Ok(r) => r.model().await?,
        Err(e) => {
            let e = anyhow::Error::from(e);
            return match posted(&e) {
                Some(Posted::Problem(why)) => {
                    store::set_issue(&app.pool, g.guild_id, g.id, Some(why)).await?;
                    problem_log(app, g, why).await;
                    Ok(None)
                }
                _ => Err(e),
            };
        }
    };
    let m = sent.id.get() as i64;
    store::set_posted(&app.pool, g.guild_id, g.id, g.channel_id, m).await?;
    if g.extras.reaction {
        remember_reactions(m as u64, g.id);
        add_reaction(app, g, channel, sent.id).await;
    }
    sqlx::query("UPDATE giveaways SET shown_entries = $2 WHERE id = $1 AND guild_id = $3")
        .bind(g.id)
        .bind(entries as i32)
        .bind(g.guild_id)
        .execute(&app.pool)
        .await?;
    Ok(Some(m))
}

/// Writes the giveaway's state into its message. A deleted message is posted again while the
/// giveaway runs; a channel problem is saved as the issue.
pub(crate) async fn refresh(app: &App, g: &Giveaway) -> Result<()> {
    let Some(m) = g.message_id else {
        return Ok(());
    };
    let entries = store::entry_count(&app.pool, g.id).await?;
    let (text, embeds, rows) = message(app, g, entries).await;
    let r = app
        .http
        .update_message(Id::new(g.channel_id as u64), Id::new(m as u64))
        .content(Some(&text))
        .embeds(Some(&embeds))
        .components(Some(&rows))
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await;
    match r.map_err(anyhow::Error::from) {
        Ok(_) => {
            sqlx::query("UPDATE giveaways SET shown_entries = $2 WHERE id = $1 AND guild_id = $3")
                .bind(g.id)
                .bind(entries as i32)
                .bind(g.guild_id)
                .execute(&app.pool)
                .await?;
            Ok(())
        }
        Err(e) => match posted(&e) {
            Some(Posted::MessageGone) if g.running() => queue_repost(app, g).await,
            Some(Posted::MessageGone) => Ok(()),
            Some(Posted::Problem(why)) => {
                store::set_issue(&app.pool, g.guild_id, g.id, Some(why)).await?;
                problem_log(app, g, why).await;
                Ok(())
            }
            None => Err(e),
        },
    }
}

const NO_REACTION: &str =
    "Brim could not add the entry reaction. Give Brim Add Reactions and Read Message History in the channel, or pick an emoji Brim can use.";

/// Brim's own reaction on a reaction giveaway, so members see what to press.
async fn add_reaction(
    app: &App,
    g: &Giveaway,
    channel: Id<ChannelMarker>,
    message: Id<twilight_model::id::marker::MessageMarker>,
) {
    use twilight_http::request::channel::reaction::RequestReactionType;
    let emoji = reaction_emoji(&g.button_emoji);
    let req = match &emoji {
        EmojiReactionType::Custom { id, name, .. } => RequestReactionType::Custom {
            id: *id,
            name: name.as_deref(),
        },
        EmojiReactionType::Unicode { name } => RequestReactionType::Unicode { name },
    };
    if let Err(e) = app.http.create_reaction(channel, message, &req).await {
        tracing::warn!(?e, id = g.id, "adding the giveaway entry reaction failed");
        let set = store::set_issue(&app.pool, g.guild_id, g.id, Some(NO_REACTION)).await;
        if let Err(e) = set {
            tracing::warn!(?e, "saving the giveaway reaction problem failed");
        }
        problem_log(app, g, NO_REACTION).await;
    }
}

async fn problem_log(app: &App, g: &Giveaway, why: &str) {
    let log = Log::new("Giveaway Message Problem", ORANGE)
        .line("Giveaway", &format!("`#{}` {}", g.id, reply::md(&g.prize)))
        .line("Channel", &format!("<#{}>", g.channel_id))
        .text(why);
    if let Err(e) = send_log(app, g.guild_id as u64, LogKind::Server, log).await {
        tracing::warn!(?e, "the giveaway problem log failed");
    }
}

/// Marks the giveaway for a new post and queues the post now.
async fn queue_repost(app: &App, g: &Giveaway) -> Result<()> {
    sqlx::query("UPDATE giveaways SET needs_repost = true, message_id = NULL WHERE id = $1 AND guild_id = $2")
        .bind(g.id)
        .bind(g.guild_id)
        .execute(&app.pool)
        .await?;
    jobs::enqueue(
        &app.pool,
        g.guild_id,
        store::REPOST_JOB,
        json!({"id": g.id}),
        0,
    )
    .await
    .map(drop)
}

/// The `giveaway_repost` job: posts a running giveaway that waits for a new message.
pub(crate) async fn repost_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let id = p["id"].as_i64().context("job payload has no id")?;
    let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? else {
        return Ok(());
    };
    if !g.running() || !g.needs_repost {
        return Ok(());
    }
    let posted = post(app, &g).await?.is_some();
    // A giveaway made on the dashboard: its first post.
    if p["first"] == true {
        started_log(app, &g).await;
    } else if posted {
        let log = Log::new("Giveaway Message Problem", ORANGE)
            .line("Giveaway", &format!("`#{}` {}", g.id, reply::md(&g.prize)))
            .line("Channel", &format!("<#{}>", g.channel_id))
            .text("The giveaway message was deleted. Brim posted it again; the entries stay.");
        if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
            tracing::warn!(?e, "the giveaway re-post log failed");
        }
    }
    Ok(())
}

/// The `giveaway_refresh` job (one per server, at most once a minute): writes the entry count
/// into each running giveaway whose message shows an older count.
pub(crate) async fn refresh_job(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT g.id FROM giveaways g WHERE g.guild_id = $1 AND g.status = 'running'
           AND g.message_id IS NOT NULL
           AND g.shown_entries <> (SELECT count(*) FROM giveaway_entries e WHERE e.giveaway_id = g.id)",
    )
    .bind(gid.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    for id in ids {
        if let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? {
            refresh(app, &g).await?;
        }
    }
    Ok(())
}

/// Queues the entry count refresh of the server, unless one is queued. Each process asks the
/// database at most once a minute per server, so 10,000 presses cost one extra write, not 10,000.
async fn queue_refresh(app: &App, gid: Id<GuildMarker>) {
    static ASKED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<u64, i64>>> =
        std::sync::LazyLock::new(Default::default);
    let now = now_secs();
    {
        let mut asked = ASKED.lock().unwrap();
        if asked.get(&gid.get()).is_some_and(|&t| now - t < 60) {
            return;
        }
        asked.retain(|_, t| now - *t < 60);
        asked.insert(gid.get(), now);
    }
    let r = jobs::enqueue_once(
        &app.pool,
        gid.get() as i64,
        store::REFRESH_JOB,
        now_secs() + 60,
    );
    if let Err(e) = r.await {
        tracing::warn!(?e, "queueing the giveaway entry count failed");
    }
}

/// The moderation facts about a member: quarantined, flagged (Raider List, or the join checks in
/// this stay), and caught by a join raid in this stay.
async fn flags(
    app: &App,
    gid: Id<GuildMarker>,
    user: u64,
    joined_ms: Option<i64>,
) -> Result<(bool, bool, bool)> {
    let joined = joined_ms
        .and_then(sqlx::types::chrono::DateTime::from_timestamp_millis)
        .unwrap_or_default();
    Ok(sqlx::query_as(
        "SELECT
           EXISTS (SELECT 1 FROM quarantines WHERE guild_id = $1 AND user_id = $2),
           (EXISTS (SELECT 1 FROM raiders WHERE user_id = $2 AND status = 'confirmed'
                      AND (expires_at IS NULL OR expires_at > now()))
              AND NOT EXISTS (SELECT 1 FROM raider_allow WHERE guild_id = $1 AND user_id = $2))
           OR EXISTS (SELECT 1 FROM joingate_actions WHERE guild_id = $1 AND user_id = $2
                        AND created_at >= $3),
           EXISTS (SELECT 1 FROM raid_log WHERE guild_id = $1 AND user_id = $2 AND created_at >= $3)",
    )
    .bind(gid.get() as i64)
    .bind(user as i64)
    .bind(joined)
    .fetch_one(&app.pool)
    .await?)
}

fn ms(t: Option<Timestamp>) -> Option<i64> {
    t.map(|t| t.as_micros() / 1000)
}

/// Why `user` cannot be in giveaway `g`, from their roles and join time as Discord gives them.
async fn check(
    app: &App,
    g: &Giveaway,
    user: u64,
    bot: bool,
    member_roles: &[Id<RoleMarker>],
    joined_ms: Option<i64>,
    timeout: Option<Timestamp>,
) -> Result<Option<Refusal>> {
    let gid = Id::new(g.guild_id as u64);
    let now_ms = crate::automod::now_ms();
    let cfg = app.cfg(gid.get()).await?;
    let (quarantined, flagged, raid_joiner) = flags(app, gid, user, joined_ms).await?;
    let v = &cfg.verification;
    let unverified =
        v.enabled && !v.roles.is_empty() && !member_roles.iter().any(|r| v.roles.contains(r));
    let f = Facts {
        user,
        roles: member_roles,
        bot,
        quarantined,
        flagged,
        timed_out: ms(timeout).is_some_and(|t| t > now_ms),
        raid_joiner,
        unverified,
        joined_ms,
        now_ms,
    };
    Ok(store::refusal(&g.req, g.host_id, g.sponsor_id, &f))
}

/// Handles the Enter button and the giveaway confirm buttons. False for other interactions.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let client = app.http.interaction(Id::new(app.me.get()));
    // The entry code form: its answer enters like a press.
    if let Some(InteractionData::ModalSubmit(m)) = &i.data {
        let Some(id) = m.custom_id.strip_prefix(CODE) else {
            return Ok(false);
        };
        let typed: String = m
            .components
            .iter()
            .find_map(crate::verify::typed_text)
            .unwrap_or_default()
            .chars()
            .take(store::CODE_LEN.1)
            .collect();
        let text = match enter(app, i, id.parse().unwrap_or(0), Some(&typed)).await {
            Ok(Answer::Text(t)) => t,
            Ok(Answer::AskCode(_)) => "Something went wrong. Try again in a moment.".into(),
            Err(e) => {
                tracing::warn!(?e, "a giveaway code entry failed");
                "Something went wrong. Try again in a moment.".into()
            }
        };
        let answer = crate::verify::ephemeral(
            InteractionResponseType::ChannelMessageWithSource,
            Some(&text),
        );
        client.create_response(i.id, &i.token, &answer).await?;
        return Ok(true);
    }
    let Some(InteractionData::MessageComponent(c)) = &i.data else {
        return Ok(false);
    };
    if let Some(id) = c.custom_id.strip_prefix(ENTER) {
        let id: i64 = id.parse().unwrap_or(0);
        let answer = match enter(app, i, id, None).await {
            Ok(Answer::Text(t)) => crate::verify::ephemeral(
                InteractionResponseType::ChannelMessageWithSource,
                Some(&t),
            ),
            Ok(Answer::AskCode(id)) => code_form(id),
            Err(e) => {
                tracing::warn!(?e, "a giveaway entry failed");
                crate::verify::ephemeral(
                    InteractionResponseType::ChannelMessageWithSource,
                    Some("Something went wrong. Try again in a moment."),
                )
            }
        };
        client.create_response(i.id, &i.token, &answer).await?;
        return Ok(true);
    }
    if let Some(rest) = c.custom_id.strip_prefix(ANSWER) {
        answer(app, i, rest).await?;
        return Ok(true);
    }
    if let Some(id) = c.custom_id.strip_prefix(CLAIM) {
        let id: i64 = id.parse().unwrap_or(0);
        let text = match claim(app, i, id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(?e, "a giveaway claim failed");
                "Something went wrong. Try again in a moment.".into()
            }
        };
        let answer = crate::verify::ephemeral(
            InteractionResponseType::ChannelMessageWithSource,
            Some(&text),
        );
        client.create_response(i.id, &i.token, &answer).await?;
        return Ok(true);
    }
    Ok(false)
}

/// The form a member types the entry code in.
fn code_form(id: i64) -> InteractionResponse {
    #[allow(deprecated)]
    // Discord moved labels to the Label component; a plain row still works.
    let input = Component::TextInput(TextInput {
        id: None,
        custom_id: "code".into(),
        label: Some("Entry code".into()),
        max_length: Some(store::CODE_LEN.1 as u16),
        min_length: Some(1),
        placeholder: None,
        required: Some(true),
        style: TextInputStyle::Short,
        value: None,
    });
    InteractionResponse {
        kind: InteractionResponseType::Modal,
        data: Some(InteractionResponseData {
            custom_id: Some(format!("{CODE}{id}")),
            title: Some("Enter the giveaway".into()),
            components: Some(vec![crate::verify::row(input)]),
            ..Default::default()
        }),
    }
}

/// The answer to an Enter press: a private text, or the entry code form of this giveaway.
enum Answer {
    Text(String),
    AskCode(i64),
}

/// The member who tries to enter, as Discord gives them in the press or the reaction.
struct Who<'a> {
    user: u64,
    bot: bool,
    roles: &'a [Id<RoleMarker>],
    joined: Option<Timestamp>,
    timeout: Option<Timestamp>,
}

/// What an entry try did.
enum Admit {
    Closed,
    Off,
    Paused,
    /// Already in, with this many entries.
    Again(u16),
    Refused(Refusal),
    /// In now, with this many entries and the role that gives them.
    New(u16, Option<Id<RoleMarker>>),
    /// The member may enter and must type the entry code first.
    NeedsCode,
    /// A wrong code; this many tries are left.
    WrongCode(i16),
    /// Too many wrong codes: minutes to wait.
    Wait(i64),
}

/// One entry try, for the button and the reaction alike. Only database reads before the one
/// insert: Discord drops a button answer after 3 s. `code`: what the member typed in the code form.
async fn admit(app: &App, g: &Giveaway, m: Who<'_>, code: Option<&str>) -> Result<Admit> {
    let gid = Id::new(g.guild_id as u64);
    if !g.running() {
        return Ok(Admit::Closed);
    }
    if !app.cfg(gid.get()).await?.giveaways.enabled {
        return Ok(Admit::Off);
    }
    // The live state, not only the mark: the round that sets `paused_at` runs every 15 s.
    if g.paused_at.is_some() || attacked(app, gid).await {
        if g.paused_at.is_none() {
            store::pause(&app.pool, g.guild_id).await?;
        }
        return Ok(Admit::Paused);
    }
    let me = m.user as i64;
    // A repeat press (the most common one in a rush) needs no checks: the draw checks again.
    let had: Option<i16> = sqlx::query_scalar(
        "SELECT weight FROM giveaway_entries WHERE giveaway_id = $1 AND user_id = $2",
    )
    .bind(g.id)
    .bind(me)
    .fetch_optional(&app.pool)
    .await?;
    let (weight, why_role) = store::weight(&g.req, m.roles);
    if let Some(w) = had {
        if w as u16 != weight {
            store::set_weight(&app.pool, g.guild_id, g.id, me, weight).await?;
        }
        return Ok(Admit::Again(weight));
    }
    let refused = check(app, g, m.user, m.bot, m.roles, ms(m.joined), m.timeout).await?;
    if let Some(r) = refused {
        return Ok(Admit::Refused(r));
    }
    // The code comes last: only a member who may enter is asked for it, and a wrong code is
    // counted only for them.
    if g.has_code {
        let Some(code) = code else {
            return Ok(match store::code_wait(&app.pool, g.id, me).await? {
                Some(w) => Admit::Wait(w),
                None => Admit::NeedsCode,
            });
        };
        match store::try_code(&app.pool, g.guild_id, g.id, me, code).await? {
            CodeTry::Right => {}
            CodeTry::Wrong(left) => return Ok(Admit::WrongCode(left)),
            CodeTry::Wait(w) => return Ok(Admit::Wait(w)),
        }
    }
    match store::enter(&app.pool, g.guild_id, g.id, me, weight).await? {
        Entered::New => {
            queue_refresh(app, gid).await;
            Ok(Admit::New(weight, why_role))
        }
        Entered::Already(w) => {
            // Roles gained since the entry count now.
            if w as u16 != weight {
                store::set_weight(&app.pool, g.guild_id, g.id, me, weight).await?;
            }
            Ok(Admit::Again(weight))
        }
        Entered::Closed => Ok(Admit::Closed),
    }
}

/// The private answer to an Enter press or to the code form.
async fn enter(app: &App, i: &Interaction, id: i64, code: Option<&str>) -> Result<Answer> {
    let (Some(gid), Some(member)) = (i.guild_id, i.member.as_ref()) else {
        return Ok(Answer::Text("This button works only in a server.".into()));
    };
    let Some(user) = member.user.as_ref().or(i.user.as_ref()) else {
        return Ok(Answer::Text("This button works only in a server.".into()));
    };
    let g = store::get(&app.pool, gid.get() as i64, id).await?;
    let prize = g.as_ref().map_or(String::new(), |g| g.prize.clone());
    let denied = async |reason: &str| {
        line(
            app,
            gid,
            "giveaway.entry_denied",
            &[("prize", &prize), ("reason", reason)],
        )
        .await
    };
    let Some(g) = g else {
        return Ok(Answer::Text(denied("this giveaway has ended.").await));
    };
    let who = Who {
        user: user.id.get(),
        bot: user.bot,
        roles: &member.roles,
        joined: member.joined_at,
        timeout: member.communication_disabled_until,
    };
    let again = async |w: u16| {
        let (prize, entries) = (g.prize.as_str(), w.to_string());
        let vars = [("prize", prize), ("entries", entries.as_str())];
        line(app, gid, "giveaway.entry_again", &vars).await
    };
    Ok(Answer::Text(match admit(app, &g, who, code).await? {
        Admit::NeedsCode => return Ok(Answer::AskCode(g.id)),
        Admit::Closed => denied("this giveaway has ended.").await,
        Admit::Off => reply::say_line(app, gid, "reply.giveaway.off", &[]).await,
        Admit::Paused => denied(PAUSED).await,
        Admit::Again(w) => again(w).await,
        Admit::Refused(r) => denied(&r.text()).await,
        Admit::WrongCode(left) => {
            let tries = if left == 1 { "try" } else { "tries" };
            let why = format!(
                "the entry code is wrong. You have {left} more {tries}, then a {} minute wait.",
                store::CODE_WAIT_MINS
            );
            denied(&why).await
        }
        Admit::Wait(w) => {
            let why = format!(
                "you typed a wrong entry code {} times. Try again in {w} minutes.",
                store::CODE_TRIES
            );
            denied(&why).await
        }
        Admit::New(weight, why_role) => {
            let entries = weight.to_string();
            let why = why_role
                .map(|r| format!("<@&{r}> role: {weight} entries"))
                .unwrap_or_default();
            let vars = [
                ("prize", g.prize.as_str()),
                ("entries", &entries),
                ("why", &why),
            ];
            line(app, gid, "giveaway.entry_ok", &vars).await
        }
    }))
}

/// Message ids of running reaction giveaways, with the giveaway id: a reaction on any other
/// message costs no database read. Filled every 15 s and at each post.
/// The time is when the entry came in, so a fill that read before a new post keeps it.
static REACTS: std::sync::LazyLock<
    std::sync::RwLock<std::collections::HashMap<u64, (i64, std::time::Instant)>>,
> = std::sync::LazyLock::new(Default::default);
/// The first fill is done; before it, a reaction asks the database.
static REACTS_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn remember_reactions(message: u64, id: i64) {
    REACTS
        .write()
        .unwrap()
        .insert(message, (id, std::time::Instant::now()));
}

/// Reads the messages of all running reaction giveaways again (one query for the whole bot).
pub(crate) async fn sync_reactions(app: &App) -> Result<()> {
    let read = std::time::Instant::now();
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT message_id, id FROM giveaways WHERE status = 'running' AND message_id IS NOT NULL
           AND extras->>'reaction' = 'true'",
    )
    .fetch_all(&app.pool)
    .await?;
    let mut map = REACTS.write().unwrap();
    // Posted while the read ran: not in the rows yet.
    map.retain(|_, (_, at)| *at > read);
    map.extend(rows.into_iter().map(|(m, id)| (m as u64, (id, read))));
    REACTS_READY.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// A reaction on a running reaction giveaway with its emoji enters the member: one insert, no
/// message edit (the entry count follows at the refresh) and no answer. A member the rules keep
/// out gets one DM with the reason; nobody is told during an attack or when moderation caught them.
/// Taking the reaction away does not leave: an entry is an entry.
pub async fn on_reaction(app: &App, r: &ReactionAdd) -> Result<()> {
    let (Some(gid), Some(member)) = (r.guild_id, r.member.as_ref()) else {
        return Ok(());
    };
    let known = REACTS.read().unwrap().get(&r.message_id.get()).map(|e| e.0);
    let id = match known {
        Some(id) => id,
        None if REACTS_READY.load(std::sync::atomic::Ordering::Acquire) => return Ok(()),
        None => {
            match store::by_message(&app.pool, gid.get() as i64, r.message_id.get() as i64).await? {
                Some(g) => g.id,
                None => return Ok(()),
            }
        }
    };
    if member.user.bot {
        return Ok(());
    }
    let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? else {
        return Ok(());
    };
    if !g.extras.reaction || !same_emoji(&r.emoji, &reaction_emoji(&g.button_emoji)) {
        return Ok(());
    }
    let who = Who {
        user: r.user_id.get(),
        bot: member.user.bot,
        roles: &member.roles,
        joined: member.joined_at,
        timeout: member.communication_disabled_until,
    };
    let Admit::Refused(why) = admit(app, &g, who, None).await? else {
        return Ok(());
    };
    if caught(why) || !may_tell(g.id, r.user_id.get()) {
        return Ok(());
    }
    let payload = json!({ "id": g.id, "user": r.user_id.get() as i64, "denied": why.text() });
    jobs::enqueue(&app.pool, g.guild_id, store::DM_JOB, payload, now_secs()).await?;
    Ok(())
}

/// A refusal moderation made: no DM, it would tell a raider what was seen.
fn caught(r: Refusal) -> bool {
    matches!(
        r,
        Refusal::Quarantined | Refusal::Flagged | Refusal::RaidJoiner
    )
}

/// Refusal DMs one giveaway may send in a minute: a wave of new accounts must not turn into a
/// wave of DMs.
const TELL_PER_MINUTE: u32 = 30;

/// True when `user`, refused in giveaway `id` by reaction, gets a DM: once per member, also when
/// they react again and again, and at most [`TELL_PER_MINUTE`] per giveaway.
fn may_tell(id: i64, user: u64) -> bool {
    type Told = (
        std::collections::HashSet<(i64, u64)>,
        std::collections::HashMap<i64, (i64, u32)>,
    );
    static TOLD: std::sync::LazyLock<std::sync::Mutex<Told>> =
        std::sync::LazyLock::new(Default::default);
    let mut t = TOLD.lock().unwrap();
    // shortcut: forgets everyone past 100,000 (and on a restart); a member may then get a second DM.
    if t.0.len() > 100_000 {
        t.0.clear();
        t.1.clear();
    }
    if t.0.contains(&(id, user)) {
        return false;
    }
    let minute = now_secs() / 60;
    let n = t.1.entry(id).or_insert((minute, 0));
    if n.0 != minute {
        *n = (minute, 0);
    }
    if n.1 >= TELL_PER_MINUTE {
        return false;
    }
    n.1 += 1;
    t.0.insert((id, user))
}

/// Reactions Brim did not see (made while it restarted, or in a pause) become entries before the
/// draw. Discord lists the reactions 100 at a time; only members with no entry yet cost a member
/// read, so a giveaway whose reactions all came in costs one list call per 100 reactions.
async fn catch_up_reactions(app: &App, g: &Giveaway) -> Result<()> {
    use twilight_http::request::channel::reaction::RequestReactionType;
    let Some(message) = g.message_id.and_then(|m| Id::new_checked(m as u64)) else {
        return Ok(());
    };
    let (gid, channel) = (Id::new(g.guild_id as u64), Id::new(g.channel_id as u64));
    let emoji = reaction_emoji(&g.button_emoji);
    let req = match &emoji {
        EmojiReactionType::Custom { id, name, .. } => RequestReactionType::Custom {
            id: *id,
            name: name.as_deref(),
        },
        EmojiReactionType::Unicode { name } => RequestReactionType::Unicode { name },
    };
    let mut after: Option<Id<UserMarker>> = None;
    loop {
        let mut call = app.http.reactions(channel, message, &req).limit(100);
        if let Some(a) = after {
            call = call.after(a);
        }
        let users = match call.await.map_err(anyhow::Error::from) {
            Ok(r) => r.models().await?,
            // The message or the emoji is gone: nothing to catch up.
            Err(e) if http_err::code(&e).is_some() => return Ok(()),
            Err(e) => return Err(e),
        };
        let Some(last) = users.last() else {
            return Ok(());
        };
        after = Some(last.id);
        let ids: Vec<i64> = users
            .iter()
            .filter(|u| !u.bot)
            .map(|u| u.id.get() as i64)
            .collect();
        let entered: Vec<i64> = sqlx::query_scalar(
            "SELECT user_id FROM giveaway_entries WHERE giveaway_id = $1 AND user_id = ANY($2)",
        )
        .bind(g.id)
        .bind(&ids)
        .fetch_all(&app.pool)
        .await?;
        for u in ids.into_iter().filter(|u| !entered.contains(u)) {
            let Some(m) = http_err::member(&app.http, gid, Id::new(u as u64)).await? else {
                continue;
            };
            let who = Who {
                user: u as u64,
                bot: m.user.bot,
                roles: &m.roles,
                joined: m.joined_at,
                timeout: m.communication_disabled_until,
            };
            admit(app, g, who, None).await?;
        }
        if users.len() < 100 {
            return Ok(());
        }
    }
}

/// The private answer to a Claim press, in the server or in the DM. Only the winner who holds
/// the place can claim it, once, before the claim time ends.
async fn claim(app: &App, i: &Interaction, id: i64) -> Result<String> {
    let Some(user) = i
        .member
        .as_ref()
        .and_then(|m| m.user.as_ref())
        .or(i.user.as_ref())
    else {
        return Ok("Something went wrong. Try again in a moment.".into());
    };
    let from = i.guild_id.map(|g| g.get() as i64);
    let r = store::claim(&app.pool, id, user.id.get() as i64, from).await?;
    let guild = match &r {
        store::Claimed::Done(g) => *g,
        _ => sqlx::query_scalar("SELECT guild_id FROM giveaways WHERE id = $1")
            .bind(id)
            .fetch_optional(&app.pool)
            .await?
            .unwrap_or(0),
    };
    let Some(gid) = Id::<GuildMarker>::new_checked(guild as u64) else {
        return Ok("This giveaway is gone.".into());
    };
    // A press in another server's channel cannot reach this giveaway.
    if i.guild_id.is_some_and(|g| g != gid) {
        return Ok("This giveaway is gone.".into());
    }
    let prize = store::get(&app.pool, guild, id)
        .await?
        .map_or(String::new(), |g| g.prize);
    let denied = async |reason: &str| {
        line(
            app,
            gid,
            "giveaway.claim_denied",
            &[("prize", &prize), ("reason", reason)],
        )
        .await
    };
    Ok(match r {
        store::Claimed::Done(_) => {
            let log = Log::new("Giveaway Prize Claimed", ORANGE)
                .line("Giveaway", &format!("`#{id}` {}", reply::md(&prize)))
                .line("Winner", &format!("<@{}> `{}`", user.id, user.id));
            if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
                tracing::warn!(?e, "the giveaway claim log failed");
            }
            line(app, gid, "giveaway.claimed", &[("prize", &prize)]).await
        }
        store::Claimed::Already => denied("you claimed it already.").await,
        store::Claimed::Late => denied("the claim time is over.").await,
        store::Claimed::NotWinner => denied("you are not a winner of this giveaway.").await,
    })
}

/// The `giveaway_claim` job, at the claim deadline of one place: when its winner did not claim,
/// that place alone is drawn again (the new winner has 24 hours). After 3 redraws of a place, or
/// when nobody is left, staff are told in the log and on the dashboard. Waits during an attack.
pub(crate) async fn claim_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let (Some(id), Some(place), Some(user)) =
        (p["id"].as_i64(), p["place"].as_i64(), p["user"].as_i64())
    else {
        return Ok(());
    };
    let guild = gid.get() as i64;
    let Some(redraws) = store::claim_due(&app.pool, guild, id, place as i16, user).await? else {
        return Ok(());
    };
    if attacked(app, gid).await {
        jobs::enqueue(
            &app.pool,
            guild,
            store::CLAIM_JOB,
            p.clone(),
            now_secs() + 600,
        )
        .await?;
        return Ok(());
    }
    let Some(g) = store::get(&app.pool, guild, id).await? else {
        return Ok(());
    };
    let tell = async |why: &str| -> Result<()> {
        store::set_issue(&app.pool, guild, id, Some(why)).await?;
        let log = Log::new("Giveaway Prize Not Claimed", ORANGE)
            .line("Giveaway", &format!("`#{id}` {}", reply::md(&g.prize)))
            .line("Last winner", &format!("<@{user}> `{user}`"))
            .text(why);
        if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
            tracing::warn!(?e, "the giveaway claim log failed");
        }
        Ok(())
    };
    if redraws >= store::MAX_REDRAWS {
        let why = format!(
            "Place {place} was not claimed after {} redraws. Redraw it by hand, or give the prize another way.",
            store::MAX_REDRAWS
        );
        return tell(&why).await;
    }
    // Places of one draw run out at the same time: one redraw, one post, for all of them.
    let mut due = store::claims_due(&app.pool, guild, id).await?;
    if !due.contains(&user) {
        due.push(user);
    }
    let reason = Some("the claim time ran out");
    match redraw(app, &g, &due, app.me, reason, true).await? {
        Redrawn::Done(_) | Redrawn::NotEnded => Ok(()),
        Redrawn::NoneLeft => {
            tell(&format!(
                "Place {place} was not claimed, and no entrant is left who can win."
            ))
            .await
        }
        // Another draw saved first: look again in a minute.
        Redrawn::Busy => {
            jobs::enqueue(
                &app.pool,
                guild,
                store::CLAIM_JOB,
                p.clone(),
                now_secs() + 60,
            )
            .await?;
            Ok(())
        }
    }
}

/// A join raid, a server lockdown (a lock of one channel does not count), panic mode or an open
/// anti-nuke case: giveaways pause. Memory reads, and the cached locks; a failed lock read counts
/// as an attack.
pub(crate) async fn attacked(app: &App, gid: Id<GuildMarker>) -> bool {
    let now = crate::automod::now_ms();
    let raid = app
        .raids
        .lock()
        .unwrap()
        .get(&gid.get())
        .is_some_and(|r| r.active(now));
    if raid || crate::feeds::under_attack(app, gid.get()) {
        return true;
    }
    crate::lockdown::active_locks(app, gid.get())
        .await
        .map_or(true, |l| {
            l.iter().any(|s| {
                s.kind
                    .as_ref()
                    .is_some_and(|k| !matches!(k, brim_automod::lockdown::LockKind::Channel(_)))
            })
        })
}

/// One round of the pause check (every 15 s): pauses the giveaways of a server that came under
/// attack, and resumes those of a server whose attack is over. One read for all servers; a write
/// only where the state changes.
pub(crate) async fn sync_pauses(app: &App) -> Result<()> {
    // One round at a time: a slow round must not start a second one beside it.
    static RUNS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNS.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return Ok(());
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            RUNS.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    let _done = Done;
    for (guild, all, any) in store::pause_states(&app.pool).await? {
        let gid = Id::new(guild as u64);
        let r = async {
            let attack = attacked(app, gid).await;
            if attack && !all {
                store::pause(&app.pool, guild).await?;
            } else if !attack
                && any
                && !recent_nuke(app, guild).await?
                && !store::resume(&app.pool, guild).await?.is_empty()
            {
                jobs::enqueue_once(&app.pool, guild, store::REFRESH_JOB, now_secs()).await?;
            }
            anyhow::Ok(())
        };
        if let Err(e) = r.await {
            tracing::warn!(?e, guild, "the giveaway pause check of a server failed");
        }
    }
    Ok(())
}

/// An anti-nuke case started in the last 10 minutes. The open case is kept in memory only, so
/// after a restart this keeps the pause until the case is old enough to be over.
async fn recent_nuke(app: &App, guild: i64) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM nuke_incidents WHERE guild_id = $1
           AND created_at > now() - make_interval(secs => $2))",
    )
    .bind(guild)
    .bind(crate::antinuke::NUKE_GAP.as_secs_f64())
    .fetch_one(&app.pool)
    .await?)
}

/// Why entries are refused while the server is under attack.
pub(crate) const PAUSED: &str =
    "the giveaway is paused while the server is under attack. Try again when it is over.";

/// A member's roles changed: an entry that now fails a rule goes (the member gets a DM with the
/// reason), and the weight follows the extra entry roles. Only members with an entry in a running
/// giveaway cost more than one indexed read.
pub async fn on_member_update(
    app: &App,
    m: &twilight_model::gateway::payload::incoming::MemberUpdate,
) -> Result<()> {
    let (gid, user) = (m.guild_id, m.user.id);
    let entered = store::entered_running(&app.pool, gid.get() as i64, user.get() as i64).await?;
    if entered.is_empty() {
        return Ok(());
    }
    // An update without the join time must not read as "joined just now": the time in the
    // server was checked at the press, and the draw checks it again.
    let joined = ms(m.joined_at).or(Some(0));
    for (g, had) in entered {
        // No timeout here: it ends by itself, so the entry stays and the draw skips the member
        // while it runs. The other rules are checked as usual.
        let refused = check(app, &g, user.get(), m.user.bot, &m.roles, joined, None).await?;
        if let Some(r) = refused {
            if store::remove_entry(&app.pool, g.guild_id, g.id, user.get() as i64).await? {
                queue_refresh(app, gid).await;
                // No DM to a member the moderation caught: it would tell a raider what was seen.
                if !caught(r) {
                    queue_removed_dm(app, &g, user, &r.text()).await;
                }
            }
            continue;
        }
        let (w, _) = store::weight(&g.req, &m.roles);
        if w as i16 != had {
            store::set_weight(&app.pool, g.guild_id, g.id, user.get() as i64, w).await?;
        }
    }
    Ok(())
}

/// Queues the DM that tells a member their entry is gone and why (a job: many members can lose
/// a role at once, and a DM can wait up to 3 s).
async fn queue_removed_dm(app: &App, g: &Giveaway, user: Id<UserMarker>, why: &str) {
    let payload = json!({ "id": g.id, "user": user.get() as i64, "removed": why });
    let queued = jobs::enqueue(&app.pool, g.guild_id, store::DM_JOB, payload, now_secs()).await;
    if let Err(e) = queued {
        tracing::warn!(?e, "could not queue a giveaway entry removed DM");
    }
}

/// Draws up to `n` winners. The database picks the candidates (`store::candidates`), and only
/// they are checked again with Discord (still in the server, the rules still met); a failed pick
/// is skipped and the next one taken. At most `3n + 50` checks, so a giveaway with 100,000 entries
/// whose members mostly left still ends in time. Skipped members are only counted: their names
/// are never shown. A Discord error stops the draw, so the job tries again.
async fn draw(app: &App, g: &Giveaway, n: usize) -> Result<(Vec<i64>, usize)> {
    let gid = Id::new(g.guild_id as u64);
    let (mut won, mut tried) = (vec![], vec![]);
    let max_checks = 3 * n + 50;
    while won.len() < n && tried.len() < max_checks {
        let want = ((n - won.len()) * 2).min(max_checks - tried.len()) as i64;
        let batch = store::candidates(&app.pool, g, &tried, want).await?;
        if batch.is_empty() {
            break;
        }
        for u in batch {
            if won.len() == n {
                break;
            }
            tried.push(u);
            let ok = match http_err::member(&app.http, gid, Id::new(u as u64)).await? {
                Some(m) => {
                    let (joined, timeout) = (ms(m.joined_at), m.communication_disabled_until);
                    check(app, g, u as u64, m.user.bot, &m.roles, joined, timeout)
                        .await?
                        .is_none()
                }
                None => false,
            };
            if ok {
                won.push(u);
            }
        }
    }
    let skipped = tried.len() - won.len();
    Ok((won, skipped))
}

/// The `giveaway_end` job: draws the winners and saves them, which queues the announcement and
/// the DMs. A run that stops before it saved its winners runs again (`claim_end`).
pub(crate) async fn end_job(app: &App, gid: Id<GuildMarker>, job: i64, p: &Value) -> Result<()> {
    let id = p["id"].as_i64().context("job payload has no id")?;
    // No draw during an attack: the end waits, and moves later by the pause.
    let attack = attacked(app, gid).await;
    if attack {
        store::pause(&app.pool, gid.get() as i64).await?;
    }
    if !store::end_due(&app.pool, gid.get() as i64, id, job).await? {
        return Ok(());
    }
    // A draw that failed before it saved (status `ended`, no draw saved) tries again after the
    // attack; nothing else is left to do for a giveaway that is not running.
    let unsaved = async {
        store::get(&app.pool, gid.get() as i64, id)
            .await
            .map(|g| g.is_some_and(|g| g.status == "ended" && g.draws == 0))
    };
    if attack && unsaved.await? {
        let job = json!({"id": id});
        jobs::enqueue(
            &app.pool,
            gid.get() as i64,
            store::END_JOB,
            job,
            now_secs() + 300,
        )
        .await?;
        return Ok(());
    }
    if let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? {
        if g.running() && g.extras.reaction {
            catch_up_reactions(app, &g).await?;
        }
    }
    let Some(g) = store::claim_end(&app.pool, gid.get() as i64, id).await? else {
        return Ok(());
    };
    let entrants = store::entry_count(&app.pool, g.id).await?;
    let (won, skipped) = draw(app, &g, g.winners.max(1) as usize).await?;
    let saved = store::save_draw(
        &app.pool,
        g.guild_id,
        g.id,
        g.draws,
        &won,
        &won,
        "giveaway.winners",
        false,
    );
    if !saved.await? {
        return Ok(());
    }
    let g = store::get(&app.pool, g.guild_id, g.id)
        .await?
        .context("the giveaway is gone")?;
    let log = Log::new("Giveaway Ended", ORANGE)
        .line("Giveaway", &format!("`#{}` {}", g.id, reply::md(&g.prize)))
        .line("Entrants", &entrants.to_string())
        .line("Winners", &staff_list(&won))
        .line("Skipped", &skipped.to_string());
    let log = match ended_note(&g) {
        n if n.is_empty() => log,
        n => log.text(&n),
    };
    if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
        tracing::warn!(?e, "the giveaway end log failed");
    }
    Ok(())
}

/// Every winner with their id, for staff (no cut-off).
fn staff_list(ids: &[i64]) -> String {
    if ids.is_empty() {
        return "Nobody".into();
    }
    ids.iter()
        .map(|u| format!("<@{u}> `{u}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `giveaway_announce` job, queued by the save of a draw: writes the drawn message and posts
/// the winners in the channel (pings up to the cap). A later draw makes it do nothing. Best
/// effort: a missing channel is skipped, the dashboard and the log still list the winners.
pub(crate) async fn announce_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let (Some(id), Some(draws), Some(key)) =
        (p["id"].as_i64(), p["draws"].as_i64(), p["key"].as_str())
    else {
        return Ok(());
    };
    let key = if key == "giveaway.reroll" {
        key
    } else {
        "giveaway.winners"
    };
    let won: Vec<i64> = p["won"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect();
    let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? else {
        return Ok(());
    };
    if g.draws as i64 != draws {
        return Ok(());
    }
    if let Err(e) = refresh(app, &g).await {
        tracing::warn!(?e, id = g.id, "writing the drawn giveaway message failed");
    }
    announce(app, &g, &won, key).await;
    Ok(())
}

async fn announce(app: &App, g: &Giveaway, won: &[i64], key: &str) {
    let gid = Id::new(g.guild_id as u64);
    let sponsor = g.sponsor_id.map(|s| format!("<@{s}>")).unwrap_or_default();
    let (link, names) = (link(g), winners_text(won));
    // The first saved draw gives the giveaway's claim time, every later one 24 hours.
    let claim = hours_text(match g.draws {
        ..=1 => g.claim_hours as i64,
        _ => store::REDRAW_HOURS as i64,
    });
    let (text, embeds) = match won.is_empty() {
        true => {
            let vars = [("prize", g.prize.as_str()), ("link", &link)];
            rendered(app, gid, "giveaway.no_winners", &vars).await
        }
        false => {
            let vars = [
                ("winners", names.as_str()),
                ("prize", &g.prize),
                ("sponsor", &sponsor),
                ("link", &link),
                ("claim", &claim),
            ];
            rendered(app, gid, key, &vars).await
        }
    };
    let pings = AllowedMentions {
        users: match won.len() <= store::PING_CAP {
            true => won
                .iter()
                .filter_map(|&u| Id::new_checked(u as u64))
                .collect(),
            false => vec![],
        },
        ..Default::default()
    };
    let channel = Id::new(g.channel_id as u64);
    let mut post = app
        .http
        .create_message(channel)
        .content(&text)
        .embeds(&embeds)
        .allowed_mentions(Some(&pings));
    let claim_button = [claim_row(g.id)];
    if !won.is_empty() {
        post = post.components(&claim_button);
    }
    if let Some(m) = g.message_id {
        post = post.reply(Id::new(m as u64)).fail_if_not_exists(false);
    }
    http_err::best_effort(post.await, "post the giveaway winners");
}

/// The `giveaway_dm` job: DMs one winner. A closed DM is skipped.
pub async fn dm_job(app: &App, gid: Id<GuildMarker>, payload: &serde_json::Value) -> Result<()> {
    let (Some(id), Some(user)) = (payload["id"].as_i64(), payload["user"].as_i64()) else {
        return Ok(());
    };
    let (Some(g), Some(user)) = (
        store::get(&app.pool, gid.get() as i64, id).await?,
        Id::new_checked(user as u64),
    ) else {
        return Ok(());
    };
    let server = crate::verify::guild_name(app, gid).await;
    if let Some(why) = payload["removed"].as_str() {
        let vars = [
            ("prize", g.prize.as_str()),
            ("server", &server),
            ("reason", why),
        ];
        let (text, embeds) = rendered(app, gid, "giveaway.entry_removed", &vars).await;
        reply::send_dm(app, user, &text, &embeds, &[]).await;
        return Ok(());
    }
    if let Some(why) = payload["denied"].as_str() {
        let vars = [
            ("prize", g.prize.as_str()),
            ("server", &server),
            ("reason", why),
        ];
        let (text, embeds) = rendered(app, gid, "giveaway.entry_denied_dm", &vars).await;
        reply::send_dm(app, user, &text, &embeds, &[]).await;
        return Ok(());
    }
    // A redraw since the save took this winner out.
    if !g.winner_ids.contains(&(user.get() as i64)) {
        return Ok(());
    }
    if let Some(role) = g.extras.winner_role {
        // Best effort: a role problem must not hold back the DM and its Claim button.
        if let Err(e) = give_role(app, &g, user, role).await {
            tracing::warn!(?e, id = g.id, "giving the giveaway winner role failed");
        }
    }
    let sponsor = g.sponsor_id.map(|s| format!("<@{s}>")).unwrap_or_default();
    let link = link(&g);
    let deadline: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>> =
        sqlx::query_scalar(
            "SELECT deadline FROM giveaway_claims WHERE giveaway_id = $1 AND user_id = $2",
        )
        .bind(g.id)
        .bind(user.get() as i64)
        .fetch_optional(&app.pool)
        .await?;
    let deadline = deadline.map(when).unwrap_or_default();
    let vars = [
        ("prize", g.prize.as_str()),
        ("server", &server),
        ("sponsor", &sponsor),
        ("link", &link),
        ("deadline", &deadline),
    ];
    let (text, embeds) = rendered(app, gid, "giveaway.winner_dm", &vars).await;
    reply::send_dm(app, user, &text, &embeds, &[claim_row(g.id)]).await;
    Ok(())
}

const ROLE_UNSAFE: &str = "Brim did not give the winner role: it has dangerous Discord permissions or a permit, or it is gone. Pick another role.";
const ROLE_NO_ACCESS: &str = "Brim cannot give or take the winner role. Give Brim Manage Roles and put Brim's role above the winner role.";

/// Gives a winner the winner role and queues its removal at the end of the role time. A role
/// with dangerous permissions or a permit is never given, whoever picked it.
async fn give_role(
    app: &App,
    g: &Giveaway,
    user: Id<UserMarker>,
    role: Id<RoleMarker>,
) -> Result<()> {
    let gid = Id::new(g.guild_id as u64);
    let (safe, _) = crate::verify::safe_roles_of(app, gid, &[role]).await?;
    if safe.is_empty() {
        store::set_issue(&app.pool, g.guild_id, g.id, Some(ROLE_UNSAFE)).await?;
        problem_log(app, g, ROLE_UNSAFE).await;
        return Ok(());
    }
    // A member who had the role before keeps it: no give and no removal later. A role an earlier
    // giveaway gave (its removal waits) is not "before": this win adds its own removal.
    let (u, r) = (user.get() as i64, role.get() as i64);
    let has = match http_err::member(&app.http, gid, user).await? {
        Some(m) => m.roles.contains(&role),
        None => return Ok(()),
    };
    if has && !store::role_off_waits(&app.pool, g.guild_id, g.id, u, r, false).await? {
        return Ok(());
    }
    if !has {
        if let Err(e) = app.http.add_guild_member_role(gid, user, role).await {
            let e = anyhow::Error::from(e);
            return match http_err::code(&e) {
                // The winner left.
                Some(10007) => Ok(()),
                Some(10011 | 50001 | 50013) => {
                    store::set_issue(&app.pool, g.guild_id, g.id, Some(ROLE_NO_ACCESS)).await?;
                    problem_log(app, g, ROLE_NO_ACCESS).await;
                    Ok(())
                }
                _ => Err(e),
            };
        }
    }
    // A redraw during the give may have replaced this winner: its removal job ran before the give.
    let still = store::get(&app.pool, g.guild_id, g.id)
        .await?
        .is_some_and(|n| n.winner_ids.contains(&(user.get() as i64)));
    let at = if still {
        now_secs() + g.extras.role_hours as i64 * 3600
    } else {
        now_secs()
    };
    let payload = json!({ "id": g.id, "user": user.get() as i64, "role": role.get() as i64 });
    jobs::enqueue(&app.pool, g.guild_id, store::ROLE_OFF_JOB, payload, at).await?;
    Ok(())
}

/// The `giveaway_role_off` job: the role time is over, or a redraw replaced the winner. A member
/// who won the role in another giveaway since keeps it until that role time is over too.
pub(crate) async fn role_off_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let (Some(user), Some(role)) = (
        p["user"].as_i64().and_then(|u| Id::new_checked(u as u64)),
        p["role"].as_i64().and_then(|r| Id::new_checked(r as u64)),
    ) else {
        return Ok(());
    };
    if store::role_off_waits(
        &app.pool,
        gid.get() as i64,
        p["id"].as_i64().unwrap_or(0),
        user.get() as i64,
        role.get() as i64,
        true,
    )
    .await?
    {
        return Ok(());
    }
    let Err(e) = app.http.remove_guild_member_role(gid, user, role).await else {
        return Ok(());
    };
    let e = anyhow::Error::from(e);
    match http_err::code(&e) {
        // The member left or the role is gone: nothing to take back.
        Some(10007 | 10011) => Ok(()),
        Some(50001 | 50013) => {
            let g = store::get(&app.pool, gid.get() as i64, p["id"].as_i64().unwrap_or(0)).await?;
            if let Some(g) = g {
                problem_log(app, &g, ROLE_NO_ACCESS).await;
            }
            Ok(())
        }
        _ => Err(e),
    }
}

const NO_THREAD: &str = "Brim cannot make the winner thread. Give Brim Create Private Threads and Send Messages in Threads in the channel (an announcement channel has no private threads).";

/// Giveaways whose winner thread is being made now, so two jobs never make two threads.
/// shortcut: one bot process; several processes would need a database lock.
static OPENING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<i64>>> =
    std::sync::LazyLock::new(Default::default);

/// The `giveaway_thread` job, queued by the save of a draw: puts its new winners (still winners
/// now) into the private winner thread, and takes the winners it replaced out. The first one makes
/// the thread, adds the host and the sponsor, posts the thread message and queues the close.
pub(crate) async fn thread_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let list = |k: &str| -> Vec<i64> {
        p[k].as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect()
    };
    let Some(g) = store::get(&app.pool, gid.get() as i64, p["id"].as_i64().unwrap_or(0)).await?
    else {
        return Ok(());
    };
    if !g.extras.thread || g.status != "ended" {
        return Ok(());
    }
    // A later draw may have run first: only who is a winner now goes in, only who is not goes out.
    let add: Vec<i64> = list("won")
        .into_iter()
        .filter(|u| g.winner_ids.contains(u))
        .collect();
    let out: Vec<i64> = list("gone")
        .into_iter()
        .filter(|u| !g.winner_ids.contains(u))
        .collect();
    // `setup`: an earlier run made the thread but failed before the host, sponsor and message.
    let setup = p["setup"] == true;
    let (thread, fresh) = match g.thread_id {
        Some(t) => (Id::new(t as u64), setup),
        None if add.is_empty() => return Ok(()),
        None => {
            if !OPENING.lock().unwrap().insert(g.id) {
                let job = p.clone();
                jobs::enqueue(
                    &app.pool,
                    g.guild_id,
                    store::THREAD_JOB,
                    job,
                    now_secs() + 30,
                )
                .await?;
                return Ok(());
            }
            // Another job may have made the thread between the read and the lock.
            let made = match store::get(&app.pool, g.guild_id, g.id).await {
                Ok(Some(n)) if n.thread_id.is_some() => {
                    Ok(n.thread_id.map(|t| (Id::new(t as u64), setup)))
                }
                Ok(_) => open_thread(app, &g).await.map(|t| t.map(|t| (t, true))),
                Err(e) => Err(e),
            };
            OPENING.lock().unwrap().remove(&g.id);
            match made? {
                Some(t) => t,
                None => return Ok(()),
            }
        }
    };
    let r = fill_thread(app, &g, thread, &add, &out, fresh).await;
    let tries = p["tries"].as_u64().unwrap_or(0);
    match r {
        Ok(()) => Ok(()),
        // Discord says the thread is gone: forget it, and this job makes a new one.
        Err(e) if http_err::code(&e) == Some(10003) => {
            sqlx::query("UPDATE giveaways SET thread_id = NULL WHERE id = $1 AND thread_id = $2")
                .bind(g.id)
                .bind(thread.get() as i64)
                .execute(&app.pool)
                .await?;
            tracing::warn!(?e, id = g.id, "the giveaway winner thread is gone");
            if tries < 2 {
                let mut job = p.clone();
                job["tries"] = json!(tries + 1);
                job["setup"] = json!(false);
                jobs::enqueue(&app.pool, g.guild_id, store::THREAD_JOB, job, now_secs()).await?;
            }
            Ok(())
        }
        // A new thread keeps its setup for the retry, which sees the saved thread as not new.
        Err(e) if fresh && tries < 2 => {
            tracing::warn!(
                ?e,
                id = g.id,
                "setting up the giveaway winner thread failed"
            );
            let mut job = p.clone();
            job["tries"] = json!(tries + 1);
            job["setup"] = json!(true);
            jobs::enqueue(
                &app.pool,
                g.guild_id,
                store::THREAD_JOB,
                job,
                now_secs() + 60,
            )
            .await?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Adds and removes the thread members; a new thread also gets the host, the sponsor and the
/// thread message. A member Discord refuses is skipped; a thread-wide failure is the error.
async fn fill_thread(
    app: &App,
    g: &Giveaway,
    thread: Id<ChannelMarker>,
    add: &[i64],
    out: &[i64],
    fresh: bool,
) -> Result<()> {
    let gid = Id::new(g.guild_id as u64);
    let mut people = add.to_vec();
    if fresh {
        people.extend(g.host_id.iter().chain(g.sponsor_id.iter()));
    }
    let mut opened = false;
    for u in people {
        loop {
            let r = app.http.add_thread_member(thread, Id::new(u as u64)).await;
            let Err(e) = r.map_err(anyhow::Error::from) else {
                break;
            };
            match http_err::code(&e) {
                // Discord archived the thread (a week with no message): open it once and retry.
                Some(50083) if !opened => {
                    opened = true;
                    app.http.update_thread(thread).archived(false).await?;
                }
                Some(10003) | None => return Err(e),
                // This member left or cannot join: the others still go in.
                Some(_) => {
                    tracing::warn!(?e, id = g.id, "a member did not go into the winner thread");
                    break;
                }
            }
        }
    }
    for &u in out {
        http_err::best_effort(
            app.http
                .remove_thread_member(thread, Id::new(u as u64))
                .await,
            "take a replaced winner out of the giveaway thread",
        );
    }
    if fresh {
        let at = now_secs() + g.extras.thread_hours as i64 * 3600;
        let closes = sqlx::types::chrono::DateTime::from_timestamp(at, 0).unwrap_or_default();
        let (host, sponsor) = (
            g.host_id.map(|h| format!("<@{h}>")).unwrap_or_default(),
            g.sponsor_id.map(|s| format!("<@{s}>")).unwrap_or_default(),
        );
        let (names, closes) = (winners_text(add), when(closes));
        let vars = [
            ("winners", names.as_str()),
            ("prize", &g.prize),
            ("host", &host),
            ("sponsor", &sponsor),
            ("closes", &closes),
        ];
        let (text, embeds) = rendered(app, gid, "giveaway.thread", &vars).await;
        // They are in the thread already: the message pings nobody.
        let quiet = AllowedMentions::default();
        let post = app
            .http
            .create_message(thread)
            .content(&text)
            .embeds(&embeds)
            .allowed_mentions(Some(&quiet));
        http_err::best_effort(post.await, "post the giveaway thread message");
    }
    Ok(())
}

/// Makes the private winner thread in the giveaway's channel, saves it and queues its close.
/// None when Discord refuses (the issue is saved for the dashboard).
async fn open_thread(app: &App, g: &Giveaway) -> Result<Option<Id<ChannelMarker>>> {
    let name: String = format!("Giveaway #{}: {}", g.id, g.prize)
        .chars()
        .take(100)
        .collect();
    let r = app
        .http
        .create_thread(
            Id::new(g.channel_id as u64),
            &name,
            ChannelType::PrivateThread,
        )
        .auto_archive_duration(AutoArchiveDuration::Week)
        .invitable(false)
        .await;
    let thread = match r.map_err(anyhow::Error::from) {
        Ok(r) => r.model().await?.id,
        Err(e) if http_err::code(&e).is_some() => {
            tracing::warn!(?e, id = g.id, "making the giveaway winner thread failed");
            store::set_issue(&app.pool, g.guild_id, g.id, Some(NO_THREAD)).await?;
            problem_log(app, g, NO_THREAD).await;
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    sqlx::query("UPDATE giveaways SET thread_id = $2 WHERE id = $1 AND guild_id = $3")
        .bind(g.id)
        .bind(thread.get() as i64)
        .bind(g.guild_id)
        .execute(&app.pool)
        .await?;
    let at = now_secs() + g.extras.thread_hours as i64 * 3600;
    let payload = json!({ "id": g.id, "thread": thread.get() as i64 });
    jobs::enqueue(&app.pool, g.guild_id, store::THREAD_CLOSE_JOB, payload, at).await?;
    Ok(Some(thread))
}

/// The `giveaway_thread_close` job: archives and locks the winner thread. A later redraw makes a
/// new thread for its winner.
pub(crate) async fn thread_close_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let (Some(id), Some(t)) = (
        p["id"].as_i64(),
        p["thread"]
            .as_i64()
            .and_then(|t| Id::<ChannelMarker>::new_checked(t as u64)),
    ) else {
        return Ok(());
    };
    let r = app.http.update_thread(t).archived(true).locked(true).await;
    if let Err(e) = r.map_err(anyhow::Error::from) {
        // Gone, or Brim lost access: nothing left to close.
        if !matches!(http_err::code(&e), Some(10003 | 50001 | 50013)) {
            return Err(e);
        }
    }
    sqlx::query(
        "UPDATE giveaways SET thread_id = NULL WHERE id = $1 AND guild_id = $2 AND thread_id = $3",
    )
    .bind(id)
    .bind(gid.get() as i64)
    .bind(t.get() as i64)
    .execute(&app.pool)
    .await?;
    Ok(())
}

/// What a reroll or a redraw did.
pub(crate) enum Redrawn {
    /// The new winners.
    Done(Vec<i64>),
    NotEnded,
    /// No entrant left who can win.
    NoneLeft,
    /// Another draw saved first.
    Busy,
}

/// Draws again for an ended giveaway. `places`: the winners to replace (none: every winner).
/// The other winners stay, and nobody who won this giveaway before is drawn.
pub(crate) async fn redraw(
    app: &App,
    g: &Giveaway,
    places: &[i64],
    by: Id<UserMarker>,
    reason: Option<&str>,
    auto: bool,
) -> Result<Redrawn> {
    // Also with no draw saved: a draw whose job gave up can be taken again here.
    if g.status != "ended" {
        return Ok(Redrawn::NotEnded);
    }
    let replace: Vec<i64> = match places.is_empty() {
        true => g.winner_ids.clone(),
        false => places
            .iter()
            .copied()
            .filter(|p| g.winner_ids.contains(p))
            .collect(),
    };
    let want = match places.is_empty() {
        true => g.winners.max(1) as usize,
        false => replace.len(),
    };
    if want == 0 {
        return Ok(Redrawn::NoneLeft);
    }
    let (new, skipped) = draw(app, g, want).await?;
    if new.is_empty() {
        return Ok(Redrawn::NoneLeft);
    }
    let kept: Vec<i64> = match places.is_empty() {
        true => vec![],
        false => g
            .winner_ids
            .iter()
            .copied()
            .filter(|w| !replace.contains(w))
            .collect(),
    };
    let all: Vec<i64> = kept.into_iter().chain(new.iter().copied()).collect();
    let saved = store::save_draw(
        &app.pool,
        g.guild_id,
        g.id,
        g.draws,
        &all,
        &new,
        "giveaway.reroll",
        auto,
    );
    if !saved.await? {
        return Ok(Redrawn::Busy);
    }
    let mut log = Log::new("Giveaway Rerolled", ORANGE)
        .line("Giveaway", &format!("`#{}` {}", g.id, reply::md(&g.prize)))
        .line("By", &format!("<@{by}>"))
        .line("Replaced", &staff_list(&replace))
        .line("New winners", &staff_list(&new))
        .line("Skipped", &skipped.to_string());
    if let Some(r) = reason.filter(|r| !r.is_empty()) {
        log = log.line("Reason", &reply::md(r));
    }
    if let Err(e) = send_log(app, g.guild_id as u64, LogKind::Server, log).await {
        tracing::warn!(?e, "the giveaway reroll log failed");
    }
    Ok(Redrawn::Done(new))
}

/// Cancels a running giveaway: no draw, the message says so.
pub(crate) async fn cancel(
    app: &App,
    gid: Id<GuildMarker>,
    id: i64,
    by: Id<UserMarker>,
) -> Result<bool> {
    if !store::cancel(&app.pool, gid.get() as i64, id).await? {
        return Ok(false);
    }
    if let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? {
        if let Err(e) = refresh(app, &g).await {
            tracing::warn!(?e, id, "writing the cancelled giveaway message failed");
        }
        let log = Log::new("Giveaway Cancelled", ORANGE)
            .line("Giveaway", &format!("`#{}` {}", g.id, reply::md(&g.prize)))
            .line("By", &format!("<@{by}>"));
        if let Err(e) = send_log(app, gid.get(), LogKind::Server, log).await {
            tracing::warn!(?e, "the giveaway cancel log failed");
        }
    }
    Ok(true)
}

/// The `giveaway_action` job: a cancel, reroll or redraw from the dashboard, which checked the
/// permit perm when it queued it. Its result shows on the dashboard and in the log.
pub(crate) async fn action_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let (Some(id), Some(by)) = (
        p["id"].as_i64(),
        p["by"].as_i64().and_then(|b| Id::new_checked(b as u64)),
    ) else {
        return Ok(());
    };
    match p["op"].as_str() {
        Some("cancel") => {
            cancel(app, gid, id, by).await?;
        }
        Some("reroll") => {
            let users: Vec<i64> = p["users"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect();
            let reason = p["reason"].as_str().filter(|r| !r.is_empty());
            let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? else {
                return Ok(());
            };
            // A press from before another draw (a second tab, a double click) does nothing.
            if p["draws"].as_i64() != Some(g.draws as i64) {
                return Ok(());
            }
            if let Redrawn::NoneLeft = redraw(app, &g, &users, by, reason, false).await? {
                let why = "The reroll found no entrant left who can win.";
                store::set_issue(&app.pool, g.guild_id, g.id, Some(why)).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Logs a new giveaway.
pub(crate) async fn started_log(app: &App, g: &Giveaway) {
    let mut log = Log::new("Giveaway Started", ORANGE)
        .line("Giveaway", &format!("`#{}` {}", g.id, reply::md(&g.prize)))
        .line("Channel", &format!("<#{}>", g.channel_id))
        .line("Ends", &when(g.ends_at))
        .line("Winners", &g.winners.to_string())
        .line("Made by", &format!("<@{}>", g.created_by));
    if let Some(s) = g.sponsor_id {
        log = log.line("Sponsor", &format!("<@{s}>"));
    }
    if let Err(e) = send_log(app, g.guild_id as u64, LogKind::Server, log).await {
        tracing::warn!(?e, "the giveaway start log failed");
    }
}

/// Logs a change staff made to a running giveaway.
pub(crate) async fn edited_log(app: &App, g: &Giveaway, by: Id<UserMarker>, what: &str) {
    let log = Log::new("Giveaway Edited", ORANGE)
        .line("Giveaway", &format!("`#{}` {}", g.id, reply::md(&g.prize)))
        .line("By", &format!("<@{by}>"))
        .text(what);
    if let Err(e) = send_log(app, g.guild_id as u64, LogKind::Server, log).await {
        tracing::warn!(?e, "the giveaway edit log failed");
    }
}

/// The message of a running giveaway was deleted: it is posted again with its entries.
pub async fn on_message_delete(app: &App, gid: Id<GuildMarker>, messages: &[i64]) -> Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM giveaways WHERE guild_id = $1 AND message_id = ANY($2) AND status = 'running'",
    )
    .bind(gid.get() as i64)
    .bind(messages)
    .fetch_all(&app.pool)
    .await?;
    for id in ids {
        if let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? {
            queue_repost(app, &g).await?;
        }
    }
    Ok(())
}

/// A member left (or was banned): their entries in running giveaways go, so the draw does not
/// spend its checks on them. One indexed delete.
pub async fn on_member_remove(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> Result<()> {
    let n = sqlx::query(
        "DELETE FROM giveaway_entries e USING giveaways g
         WHERE e.guild_id = $1 AND e.user_id = $2 AND g.id = e.giveaway_id AND g.status = 'running'",
    )
    .bind(gid.get() as i64)
    .bind(user.get() as i64)
    .execute(&app.pool)
    .await?
    .rows_affected();
    if n > 0 {
        queue_refresh(app, gid).await;
    }
    Ok(())
}

/// A restore or an anti-nuke revert made channels and roles again (old id -> new id): running
/// giveaways in a remade channel are posted there again with their entries, and their role rules
/// name the new roles, so a remade blocked role still keeps its members out. Only logs a failure.
pub async fn relink(app: &App, gid: Id<GuildMarker>, map: &std::collections::HashMap<u64, u64>) {
    let guild = gid.get() as i64;
    for (old, new) in map {
        let r = store::relink_roles(&app.pool, guild, *old as i64, *new as i64).await;
        if let Err(e) = r {
            tracing::warn!(?e, "moving giveaway rules to a remade role failed");
        }
        if let Err(e) = plans::relink_channel(&app.pool, guild, *old as i64, *new as i64).await {
            tracing::warn!(?e, "moving giveaway plans to a remade channel failed");
        }
        let ids: Result<Vec<i64>, _> = sqlx::query_scalar(
            "SELECT id FROM giveaways WHERE guild_id = $1 AND channel_id = $2 AND status = 'running'",
        )
        .bind(guild)
        .bind(*old as i64)
        .fetch_all(&app.pool)
        .await;
        for id in ids.unwrap_or_default() {
            let r = store::queue_post(&app.pool, guild, id, Some(*new as i64), false).await;
            if let Err(e) = r {
                tracing::warn!(?e, id, "posting a giveaway in a remade channel failed");
            }
        }
    }
}

/// A deleted channel: its running giveaways keep running and wait for staff to pick a channel.
pub async fn on_channel_delete(app: &App, gid: Id<GuildMarker>, channel: u64) -> Result<()> {
    plans::channel_gone(
        &app.pool,
        gid.get() as i64,
        channel as i64,
        PLAN_CHANNEL_GONE,
    )
    .await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "UPDATE giveaways SET message_id = NULL, needs_repost = true, issue = $3
         WHERE guild_id = $1 AND channel_id = $2 AND status = 'running' RETURNING id",
    )
    .bind(gid.get() as i64)
    .bind(channel as i64)
    .bind(CHANNEL_GONE)
    .fetch_all(&app.pool)
    .await?;
    for id in ids {
        if let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? {
            problem_log(app, &g, CHANNEL_GONE).await;
        }
    }
    Ok(())
}

// --- The `giveaway` command -------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub(crate) enum Sub {
    Start {
        channel: Option<u64>,
        secs: i64,
        winners: i64,
        prize: String,
    },
    End(i64),
    Cancel(i64),
    Reroll(i64),
    Redraw {
        id: i64,
        users: Vec<i64>,
        reason: Option<String>,
    },
    Time(i64, i64),
    /// Giveaway, claim time in hours.
    Claim(i64, i64),
    List,
}

fn id_arg(w: Option<&&str>) -> Result<i64, String> {
    let w = w.ok_or("Give the giveaway number, e.g. `12`")?;
    w.trim_start_matches('#')
        .parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("`{w}` is not a giveaway number"))
}

fn mention_id(w: &str) -> Option<u64> {
    let inner = w
        .strip_prefix("<@")
        .and_then(|r| r.strip_suffix('>'))
        .map(|r| r.trim_start_matches('!'))
        .unwrap_or(w);
    inner.parse().ok().filter(|n| *n > 0)
}

/// `args`: the words after `giveaway`.
pub(crate) fn parse(args: &[&str]) -> Result<Sub, String> {
    let sub = args.first().map(|w| w.to_lowercase());
    match sub.as_deref() {
        None | Some("list") => Ok(Sub::List),
        Some("start") => {
            let mut rest = &args[1..];
            let channel = rest
                .first()
                .and_then(|w| w.strip_prefix("<#")?.strip_suffix('>')?.parse().ok());
            if channel.is_some() {
                rest = &rest[1..];
            }
            let (Some(t), Some(w)) = (rest.first(), rest.get(1)) else {
                return Err("Use `giveaway start [CHANNEL] TIME WINNERS PRIZE`".into());
            };
            let secs = brim_core::duration::parse(t)
                .ok_or_else(|| format!("`{t}` is not a duration (10m, 2h, 7d)"))?;
            let winners = w
                .trim_end_matches(['w', 'W'])
                .parse()
                .map_err(|_| format!("`{w}` is not a number of winners"))?;
            let prize = rest[2..].join(" ");
            if prize.trim().is_empty() {
                return Err("Name the prize after the number of winners".into());
            }
            Ok(Sub::Start {
                channel,
                secs,
                winners,
                prize,
            })
        }
        Some("end") => Ok(Sub::End(id_arg(args.get(1))?)),
        Some("cancel") => Ok(Sub::Cancel(id_arg(args.get(1))?)),
        Some("reroll") => Ok(Sub::Reroll(id_arg(args.get(1))?)),
        Some("redraw") => {
            let id = id_arg(args.get(1))?;
            let rest = &args[2..];
            let at = rest.iter().position(|w| w.eq_ignore_ascii_case("?r"));
            let (who, reason) = match at {
                Some(i) => (&rest[..i], Some(rest[i + 1..].join(" "))),
                None => (rest, None),
            };
            let users = who
                .iter()
                .map(|w| {
                    mention_id(w)
                        .map(|u| u as i64)
                        .ok_or_else(|| format!("`{w}` is not a member"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Sub::Redraw {
                id,
                users,
                reason: reason.filter(|r| !r.trim().is_empty()),
            })
        }
        Some("time") => {
            let id = id_arg(args.get(1))?;
            let c = args.get(2).ok_or("Give the change, e.g. `+1h` or `-30m`")?;
            let (sign, d) = match c.strip_prefix('-') {
                Some(d) => (-1, d),
                None => (1, c.trim_start_matches('+')),
            };
            let secs = brim_core::duration::parse(d)
                .ok_or_else(|| format!("`{c}` is not a change of time (+1h, -30m)"))?;
            Ok(Sub::Time(id, sign * secs))
        }
        Some("claim") => {
            let id = id_arg(args.get(1))?;
            let t = args.get(2).ok_or("Give the claim time, e.g. `48h` or `3d`")?;
            let secs = brim_core::duration::parse(t)
                .ok_or_else(|| format!("`{t}` is not a duration (12h, 3d)"))?;
            Ok(Sub::Claim(id, (secs + 3599) / 3600))
        }
        Some(o) => Err(format!(
            "`{o}` is not a giveaway command. Use start, end, cancel, reroll, redraw, time, claim or list"
        )),
    }
}

/// `/giveaway <sub> ...` as the words of the prefix command.
pub(crate) fn slash_words(o: &[CommandDataOption]) -> Vec<String> {
    let mut w = vec!["giveaway".to_string()];
    let Some(CommandDataOption {
        name,
        value: CommandOptionValue::SubCommand(so),
    }) = o.first()
    else {
        return w;
    };
    w.push(name.clone());
    let get = |n: &str| so.iter().find(|o| o.name == n).map(|o| &o.value);
    let mut push = |v: Option<&CommandOptionValue>| match v {
        Some(CommandOptionValue::User(u)) => w.push(format!("<@{u}>")),
        Some(CommandOptionValue::Channel(c)) => w.push(format!("<#{c}>")),
        Some(CommandOptionValue::Integer(n)) => w.push(n.to_string()),
        Some(CommandOptionValue::String(s)) => w.push(s.clone()),
        _ => {}
    };
    match name.as_str() {
        "start" => ["channel", "duration", "winners", "prize"]
            .into_iter()
            .for_each(|n| push(get(n))),
        "redraw" => {
            push(get("id"));
            for n in ["user", "user2", "user3", "user4", "user5"] {
                push(get(n));
            }
            if let Some(CommandOptionValue::String(r)) = get("reason") {
                w.push("?r".into());
                w.push(r.clone());
            }
        }
        "time" => ["id", "change"].into_iter().for_each(|n| push(get(n))),
        "claim" => ["id", "duration"].into_iter().for_each(|n| push(get(n))),
        _ => push(get("id")),
    }
    w
}

pub(crate) fn definition() -> twilight_model::application::command::Command {
    use twilight_model::application::{command::CommandType, interaction::InteractionContextType};
    use twilight_model::channel::ChannelType as C;
    use twilight_util::builder::command::{
        ChannelBuilder, CommandBuilder, IntegerBuilder, StringBuilder, SubCommandBuilder,
        UserBuilder,
    };
    let id = || {
        IntegerBuilder::new("id", "The giveaway number")
            .min_value(1)
            .required(true)
    };
    let mut redraw = SubCommandBuilder::new("redraw", "Draw again for some winners, or for all")
        .option(id())
        .option(UserBuilder::new(
            "user",
            "A winner to replace (none: every winner)",
        ));
    for n in ["user2", "user3", "user4", "user5"] {
        redraw = redraw.option(UserBuilder::new(n, "Another winner to replace"));
    }
    redraw = redraw.option(StringBuilder::new("reason", "Why, for the staff log"));
    CommandBuilder::new(
        "giveaway",
        "Start and run giveaways",
        CommandType::ChatInput,
    )
    .contexts([InteractionContextType::Guild])
    .option(
        SubCommandBuilder::new("start", "Start a giveaway")
            .option(StringBuilder::new("duration", "How long it runs: 10m, 2h, 7d").required(true))
            .option(
                IntegerBuilder::new("winners", "How many winners")
                    .min_value(store::WINNERS.0)
                    .max_value(store::WINNERS.1)
                    .required(true),
            )
            .option(StringBuilder::new("prize", "What the winners get").required(true))
            .option(
                ChannelBuilder::new("channel", "Default: this channel")
                    .channel_types([C::GuildText, C::GuildAnnouncement]),
            ),
    )
    .option(SubCommandBuilder::new("end", "End a giveaway now and draw the winners").option(id()))
    .option(SubCommandBuilder::new("cancel", "Cancel a giveaway: nobody wins").option(id()))
    .option(SubCommandBuilder::new("reroll", "Draw new winners for an ended giveaway").option(id()))
    .option(redraw)
    .option(
        SubCommandBuilder::new("time", "Add or remove time on a running giveaway")
            .option(id())
            .option(StringBuilder::new("change", "+1h or -30m").required(true)),
    )
    .option(
        SubCommandBuilder::new("claim", "Set how long winners have to claim")
            .option(id())
            .option(StringBuilder::new("duration", "12h, 2d").required(true)),
    )
    .option(SubCommandBuilder::new(
        "list",
        "Running and recent giveaways",
    ))
    .build()
}

type Reply = (String, Option<Embed>, Vec<Component>);

/// The reply of `giveaway <args>`. The caller checked the Manage Giveaways permit perm.
pub(crate) async fn command(
    app: &App,
    gid: Id<GuildMarker>,
    invoker: Id<UserMarker>,
    level: &Level,
    here: Option<Id<ChannelMarker>>,
    args: &[&str],
) -> Result<Reply> {
    let text = |t: String| Ok((t, None, vec![]));
    let sub = match parse(args) {
        Ok(s) => s,
        Err(e) => {
            let said = reply::say_wrong(app, gid, &e).await;
            return Ok((said.text, said.embeds.into_iter().next(), vec![]));
        }
    };
    let say = async |key: &str, vars: &[(&str, &str)]| reply::say_line(app, gid, key, vars).await;
    if sub == Sub::List {
        return list(app, gid).await;
    }
    if let Some(no) = crate::commands::gated(app, gid, invoker, level, "giveaways").await? {
        return text(no);
    }
    let cfg = app.cfg(gid.get()).await?;
    let g = gid.get() as i64;
    let find = async |id: i64| store::get(&app.pool, g, id).await;
    match sub {
        Sub::Start {
            channel,
            secs,
            winners,
            prize,
        } => {
            if !cfg.giveaways.enabled {
                return text(say("reply.giveaway.off", &[]).await);
            }
            let Some(channel) = channel.or(here.map(Id::get)) else {
                return text("Name a channel".into());
            };
            let ch = Id::new_checked(channel).context("channel")?;
            let ours = match app.http.channel(ch).await {
                Ok(r) => r.model().await?.guild_id == Some(gid),
                Err(_) => false,
            };
            if !ours {
                return text("Pick a channel of this server".into());
            }
            let d = &cfg.giveaways;
            let new = store::New {
                guild_id: g,
                channel_id: channel as i64,
                prize,
                host_id: Some(invoker.get() as i64),
                sponsor_id: None,
                created_by: invoker.get() as i64,
                winners,
                duration_secs: secs,
                req: Req {
                    blocked: d.blocked_roles.clone(),
                    min_age_days: d.min_account_age_days,
                    min_member_hours: d.min_member_hours,
                    block_raid_joiners: d.block_raid_joiners,
                    ..Req::default()
                },
                button_label: d.button_label.clone(),
                button_emoji: d.button_emoji.clone(),
                claim_hours: d.claim_hours as i64,
                extras: Default::default(),
                code: None,
                post_job: false,
            };
            let made = store::create(&app.pool, &new, d.max_running as i64).await?;
            let gw = match made {
                Ok(gw) => gw,
                Err(why) => {
                    let said = reply::say_wrong(app, gid, &why).await;
                    return Ok((said.text, said.embeds.into_iter().next(), vec![]));
                }
            };
            // A failed post is tried again by the re-post job; the row and its end job stay.
            let posted = match post(app, &gw).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(?e, id = gw.id, "posting a new giveaway failed");
                    queue_repost(app, &gw).await?;
                    None
                }
            };
            started_log(app, &gw).await;
            if posted.is_none() {
                let issue = store::get(&app.pool, g, gw.id)
                    .await?
                    .and_then(|x| x.issue)
                    .unwrap_or_else(|| "Brim tries to post it again in a minute.".into());
                return text(format!(
                    "Giveaway `{}` is saved, but Brim could not post it: {issue}",
                    gw.id
                ));
            }
            let (id, ch, ends) = (gw.id.to_string(), format!("<#{channel}>"), when(gw.ends_at));
            let vars = [
                ("id", id.as_str()),
                ("prize", &reply::md(&gw.prize)),
                ("channel", &ch),
                ("ends", &ends),
            ];
            text(say("reply.giveaway.started", &vars).await)
        }
        Sub::End(id) | Sub::Cancel(id) | Sub::Reroll(id) => {
            let Some(gw) = find(id).await? else {
                return text(say("reply.giveaway.not_found", &[("id", &id.to_string())]).await);
            };
            let op = match sub {
                Sub::End(_) => "end",
                Sub::Cancel(_) => "cancel",
                _ => "reroll",
            };
            let n = id.to_string();
            if op != "reroll" && !gw.running() {
                return text(say("reply.giveaway.not_running", &[("id", &n)]).await);
            }
            if op == "reroll" && gw.status != "ended" {
                return text(say("reply.giveaway.not_ended", &[("id", &n)]).await);
            }
            if op == "reroll" && !old_draw(&gw) {
                return redraw_reply(app, &gw, &[], invoker, None)
                    .await
                    .map(|t| (t, None, vec![]));
            }
            let action = match op {
                "end" => "End",
                "cancel" => "Cancel",
                _ => "Reroll",
            };
            let q = say(
                "reply.giveaway.confirm",
                &[
                    ("action", action),
                    ("id", &n),
                    ("prize", &reply::md(&gw.prize)),
                ],
            )
            .await;
            Ok((q, None, vec![confirm_row(op, &gw, invoker)]))
        }
        Sub::Redraw { id, users, reason } => {
            let Some(gw) = find(id).await? else {
                return text(say("reply.giveaway.not_found", &[("id", &id.to_string())]).await);
            };
            if users.is_empty() && old_draw(&gw) {
                let n = id.to_string();
                let q = say(
                    "reply.giveaway.confirm",
                    &[
                        ("action", "Reroll"),
                        ("id", &n),
                        ("prize", &reply::md(&gw.prize)),
                    ],
                )
                .await;
                return Ok((q, None, vec![confirm_row("reroll", &gw, invoker)]));
            }
            text(redraw_reply(app, &gw, &users, invoker, reason.as_deref()).await?)
        }
        Sub::Time(id, secs) => {
            let n = id.to_string();
            let Some(ends) = store::shift(&app.pool, g, id, secs).await? else {
                return text(match find(id).await? {
                    Some(_) => say("reply.giveaway.not_running", &[("id", &n)]).await,
                    None => say("reply.giveaway.not_found", &[("id", &n)]).await,
                });
            };
            if let Some(gw) = find(id).await? {
                if let Err(e) = refresh(app, &gw).await {
                    tracing::warn!(?e, id, "writing the new end time failed");
                }
                let what = format!("End time moved by {secs} seconds to {}.", when(ends));
                edited_log(app, &gw, invoker, &what).await;
            }
            text(say("reply.giveaway.time", &[("id", &n), ("ends", &when(ends))]).await)
        }
        Sub::Claim(id, hours) => {
            let n = id.to_string();
            match store::set_claim_time(&app.pool, g, id, hours).await? {
                Err(e) => {
                    let said = reply::say_wrong(app, gid, &e).await;
                    Ok((said.text, said.embeds.into_iter().next(), vec![]))
                }
                Ok(false) => text(match find(id).await? {
                    Some(_) => say("reply.giveaway.not_running", &[("id", &n)]).await,
                    None => say("reply.giveaway.not_found", &[("id", &n)]).await,
                }),
                Ok(true) => {
                    if let Some(gw) = find(id).await? {
                        let what = format!("Claim time set to {}.", hours_text(hours));
                        edited_log(app, &gw, invoker, &what).await;
                    }
                    let claim = hours_text(hours);
                    text(
                        say(
                            "reply.giveaway.claim_time",
                            &[("id", &n), ("claim", &claim)],
                        )
                        .await,
                    )
                }
            }
        }
        Sub::List => unreachable!("answered above"),
    }
}

/// A reroll of a giveaway that ended this long ago asks Yes/No first.
fn old_draw(g: &Giveaway) -> bool {
    g.ended_at
        .is_some_and(|e| (sqlx::types::chrono::Utc::now() - e).num_days() >= store::OLD_REROLL_DAYS)
}

async fn redraw_reply(
    app: &App,
    gw: &Giveaway,
    users: &[i64],
    by: Id<UserMarker>,
    reason: Option<&str>,
) -> Result<String> {
    let gid = Id::new(gw.guild_id as u64);
    let n = gw.id.to_string();
    let key = match redraw(app, gw, users, by, reason, false).await? {
        Redrawn::Done(new) => {
            let names = winners_text(&new);
            let vars = [("id", n.as_str()), ("winners", &names)];
            return Ok(reply::say_line(app, gid, "reply.giveaway.rerolled", &vars).await);
        }
        Redrawn::NotEnded => "reply.giveaway.not_ended",
        Redrawn::NoneLeft => "reply.giveaway.none_left",
        Redrawn::Busy => "reply.giveaway.busy",
    };
    Ok(reply::say_line(app, gid, key, &[("id", &n)]).await)
}

/// Running giveaways and the last ended ones, one line each.
async fn list(app: &App, gid: Id<GuildMarker>) -> Result<Reply> {
    let all = store::list(&app.pool, gid.get() as i64, 10).await?;
    let lines: Vec<String> = all
        .iter()
        .map(|g| {
            let state = match g.status.as_str() {
                "running" if g.paused_at.is_some() => "paused".to_string(),
                "running" => format!("ends {}", when(g.ends_at)),
                "ended" => format!(
                    "ended, winners: {}",
                    match g.winner_ids.is_empty() {
                        true => "nobody".into(),
                        false => winners_text(&g.winner_ids),
                    }
                ),
                _ => "cancelled".into(),
            };
            format!(
                "`#{}` **{}** in <#{}>: {state}",
                g.id,
                reply::md(&g.prize),
                g.channel_id
            )
        })
        .collect();
    let text = match lines.is_empty() {
        true => "No giveaways yet.".to_string(),
        false => lines.join("\n"),
    };
    let e = reply::answer(app, gid, "reply.giveaway.list", &[("giveaways", text)]).await;
    Ok((String::new(), Some(e), vec![]))
}

/// Yes and No for a giveaway action. The button names the asker and the state it was asked in,
/// so a press after the giveaway changed does nothing.
fn confirm_row(op: &str, g: &Giveaway, actor: Id<UserMarker>) -> Component {
    let b = |yes: bool| {
        Component::Button(Button {
            id: None,
            custom_id: Some(format!(
                "{ANSWER}{}:{op}:{}:{actor}:{}",
                if yes { "y" } else { "n" },
                g.id,
                g.draws
            )),
            disabled: false,
            emoji: None,
            label: Some(if yes { "Yes" } else { "No" }.into()),
            style: if yes {
                ButtonStyle::Success
            } else {
                ButtonStyle::Danger
            },
            url: None,
            sku_id: None,
        })
    };
    Component::ActionRow(ActionRow {
        id: None,
        components: vec![b(true), b(false)],
    })
}

/// A Yes or No press on a giveaway question. Only the asker may answer, with the permit perm
/// they still hold; the question is closed at once (no second press), then the action runs.
async fn answer(app: &App, i: &Interaction, rest: &str) -> Result<()> {
    let client = &app.http.interaction(Id::new(app.me.get()));
    let mut it = rest.splitn(5, ':');
    let parsed = (|| {
        let yes = it.next()? == "y";
        let op = it.next()?.to_string();
        let id: i64 = it.next()?.parse().ok()?;
        let actor: u64 = it.next()?.parse().ok()?;
        let draws: i32 = it.next()?.parse().ok()?;
        Some((yes, op, id, actor, draws))
    })();
    let (Some(gid), Some((yes, op, id, actor, draws))) = (i.guild_id, parsed) else {
        return Ok(());
    };
    let private = |key: &'static str| async move {
        let t = reply::say_line(app, gid, key, &[]).await;
        let r =
            crate::verify::ephemeral(InteractionResponseType::ChannelMessageWithSource, Some(&t));
        client.create_response(i.id, &i.token, &r).await.map(drop)
    };
    if i.author_id().map(Id::get) != Some(actor) {
        return private("reply.confirm.not_yours").await.map_err(Into::into);
    }
    let close = |text: String| InteractionResponse {
        kind: InteractionResponseType::UpdateMessage,
        data: Some(InteractionResponseData {
            content: Some(text),
            components: Some(vec![]),
            embeds: Some(vec![]),
            ..Default::default()
        }),
    };
    if !yes {
        let t = reply::say_line(app, gid, "reply.confirm.cancelled", &[]).await;
        client.create_response(i.id, &i.token, &close(t)).await?;
        return Ok(());
    }
    let working = reply::say_line(app, gid, "reply.confirm.working", &[]).await;
    client
        .create_response(i.id, &i.token, &close(working))
        .await?;
    let result = async {
        let actor = Id::new(actor);
        let owner = app.guild_info(gid).await?.owner;
        let level = app.level(gid, owner, actor).await?;
        if !level.has("giveaways") {
            return Ok(reply::say_line(app, gid, "reply.error.no_permit_command", &[]).await);
        }
        if let Some(no) = crate::commands::gated(app, gid, actor, &level, "giveaways").await? {
            return Ok(no);
        }
        let n = id.to_string();
        let Some(g) = store::get(&app.pool, gid.get() as i64, id).await? else {
            return Ok(reply::say_line(app, gid, "reply.giveaway.not_found", &[("id", &n)]).await);
        };
        if g.draws != draws {
            return Ok(reply::say_line(app, gid, "reply.giveaway.stale", &[]).await);
        }
        anyhow::Ok(match op.as_str() {
            "end" => match store::end_now(&app.pool, g.guild_id, id).await? {
                store::EndNow::NotRunning => {
                    reply::say_line(app, gid, "reply.giveaway.not_running", &[("id", &n)]).await
                }
                store::EndNow::Paused => {
                    reply::say_line(app, gid, "reply.giveaway.paused", &[("id", &n)]).await
                }
                _ => reply::say_line(app, gid, "reply.giveaway.ending", &[("id", &n)]).await,
            },
            "cancel" => match cancel(app, gid, id, actor).await? {
                true => reply::say_line(app, gid, "reply.giveaway.cancelled", &[("id", &n)]).await,
                false => {
                    reply::say_line(app, gid, "reply.giveaway.not_running", &[("id", &n)]).await
                }
            },
            _ => redraw_reply(app, &g, &[], actor, None).await?,
        })
    }
    .await;
    let text = result.unwrap_or_else(|e| {
        tracing::warn!(?e, "a giveaway action failed");
        format!("The command did not finish: {}", http_err::why(&e))
    });
    client
        .update_response(&i.token)
        .content(Some(&text))
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_like_the_other_commands() {
        assert_eq!(parse(&[]).unwrap(), Sub::List);
        assert_eq!(parse(&["claim", "12", "3d"]).unwrap(), Sub::Claim(12, 72));
        assert_eq!(parse(&["claim", "12", "90m"]).unwrap(), Sub::Claim(12, 2));
        assert!(parse(&["claim", "12"]).is_err());
        assert_eq!(
            parse(&["start", "2h", "3w", "Nitro", "for", "a", "month"]).unwrap(),
            Sub::Start {
                channel: None,
                secs: 7200,
                winners: 3,
                prize: "Nitro for a month".into()
            }
        );
        assert_eq!(
            parse(&["start", "<#55>", "1d", "1", "Role"]).unwrap(),
            Sub::Start {
                channel: Some(55),
                secs: 86_400,
                winners: 1,
                prize: "Role".into()
            }
        );
        assert!(parse(&["start", "2h", "3"]).is_err(), "no prize");
        assert!(parse(&["start", "soon", "3", "x"]).is_err());
        assert_eq!(parse(&["end", "#12"]).unwrap(), Sub::End(12));
        assert!(parse(&["end", "x"]).is_err());
        assert_eq!(
            parse(&["redraw", "4", "<@10>", "<@!11>", "?r", "left", "the", "server"]).unwrap(),
            Sub::Redraw {
                id: 4,
                users: vec![10, 11],
                reason: Some("left the server".into())
            }
        );
        assert_eq!(
            parse(&["redraw", "4"]).unwrap(),
            Sub::Redraw {
                id: 4,
                users: vec![],
                reason: None
            }
        );
        assert_eq!(parse(&["time", "4", "-30m"]).unwrap(), Sub::Time(4, -1800));
        assert_eq!(parse(&["time", "4", "+1h"]).unwrap(), Sub::Time(4, 3600));
        assert!(parse(&["nothing"]).is_err());
    }

    #[test]
    fn button_emoji_takes_unicode_or_a_custom_tag() {
        assert!(matches!(
            button_emoji("🎉"),
            Some(EmojiReactionType::Unicode { .. })
        ));
        assert!(matches!(
            button_emoji("<a:party:123>"),
            Some(EmojiReactionType::Custom { animated: true, .. })
        ));
        assert_eq!(button_emoji("party"), None);
        assert_eq!(button_emoji(""), None);
    }

    #[test]
    fn many_winners_show_as_a_count() {
        let few: Vec<i64> = (1..=3).collect();
        assert_eq!(winners_text(&few), "<@1>, <@2>, <@3>");
        let many: Vec<i64> = (1..=26).collect();
        assert_eq!(winners_text(&many), "26 winners");
    }

    /// Panic mode pauses the server's giveaways within one round: no entry and no draw, and the
    /// end job waits. When panic ends, the next round resumes them and moves the end later.
    #[tokio::test]
    async fn panic_mode_pauses_giveaways_and_the_end_moves_after_it() {
        let Some(t) = crate::test_support::T::new(301).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"giveaways": {"enabled": true}}))
            .await;
        let (gid, g) = (t.g(0), t.g(0).get() as i64);
        let n = store::New {
            guild_id: g,
            channel_id: 2,
            prize: "Nitro".into(),
            host_id: None,
            sponsor_id: None,
            created_by: 9,
            winners: 1,
            duration_secs: 120,
            req: Req::default(),
            button_label: String::new(),
            button_emoji: String::new(),
            claim_hours: 48,
            extras: Default::default(),
            code: None,
            post_job: false,
        };
        let gw = store::create(&t.app.pool, &n, 25).await.unwrap().unwrap();
        assert!(!attacked(&t.app, gid).await);
        t.app.panic_runs.lock().unwrap().insert(
            gid.get(),
            crate::app::PanicRun {
                lock_mod_commands: false,
                starting: 0,
            },
        );
        assert!(attacked(&t.app, gid).await);
        sync_pauses(&t.app).await.unwrap();
        let paused = store::get(&t.app.pool, g, gw.id).await.unwrap().unwrap();
        assert!(paused.paused_at.is_some());
        let job: i64 =
            sqlx::query_scalar("SELECT id FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2")
                .bind(g)
                .bind(store::END_JOB)
                .fetch_one(&t.app.pool)
                .await
                .unwrap();
        end_job(&t.app, gid, job, &json!({"id": gw.id}))
            .await
            .unwrap();
        let still = store::get(&t.app.pool, g, gw.id).await.unwrap().unwrap();
        assert!(still.running(), "no draw during panic");

        t.app.panic_runs.lock().unwrap().remove(&gid.get());
        sqlx::query("UPDATE giveaways SET paused_at = now() - interval '5 minutes' WHERE id = $1")
            .bind(gw.id)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sync_pauses(&t.app).await.unwrap();
        let after = store::get(&t.app.pool, g, gw.id).await.unwrap().unwrap();
        assert!(after.paused_at.is_none());
        assert!((after.ends_at - gw.ends_at).num_seconds() >= 295);
        sqlx::query("DELETE FROM giveaways WHERE guild_id = $1")
            .bind(g)
            .execute(&t.app.pool)
            .await
            .unwrap();
        t.cleanup().await;
    }

    fn plain(g: i64) -> store::New {
        store::New {
            guild_id: g,
            channel_id: 2,
            prize: "Nitro".into(),
            host_id: None,
            sponsor_id: None,
            created_by: 9,
            winners: 1,
            duration_secs: 3600,
            req: Req::default(),
            button_label: String::new(),
            button_emoji: String::new(),
            claim_hours: 48,
            extras: Default::default(),
            code: None,
            post_job: false,
        }
    }

    async fn wipe(t: &crate::test_support::T, g: i64) {
        for q in [
            "DELETE FROM giveaways WHERE guild_id = $1",
            "DELETE FROM scheduled_jobs WHERE guild_id = $1",
        ] {
            sqlx::query(q).bind(g).execute(&t.app.pool).await.unwrap();
        }
        t.cleanup().await;
    }

    fn reaction(gid: Id<GuildMarker>, message: u64, user: u64, emoji: &str) -> ReactionAdd {
        ReactionAdd(
            serde_json::from_value(json!({
                "burst": false, "burst_colors": [], "channel_id": "2", "message_id": message.to_string(),
                "guild_id": gid.to_string(), "user_id": user.to_string(), "emoji": {"id": null, "name": emoji},
                "member": {"deaf": false, "mute": false, "flags": 0, "pending": false, "roles": [],
                    "joined_at": "2026-01-01T00:00:00.000000+00:00",
                    "user": {"id": user.to_string(), "username": "x", "discriminator": "0", "avatar": null}},
            }))
            .unwrap(),
        )
    }

    /// Reaction entry under load: 10,000 members react at once (and 2,000 of them twice). Each
    /// gets one entry, no reaction edits the message or sends anything, and one count refresh is
    /// queued. Another emoji and another message do nothing. Times go to stderr.
    #[tokio::test]
    async fn ten_thousand_reactions_enter_once_each() {
        let Some(t) = crate::test_support::T::new(302).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"giveaways": {"enabled": true}}))
            .await;
        let (gid, g) = (t.g(0), t.g(0).get() as i64);
        let mut n = plain(g);
        n.extras.reaction = true;
        let gw = store::create(&t.app.pool, &n, 25).await.unwrap().unwrap();
        post(&t.app, &gw).await.unwrap();
        assert_eq!(t.calls("/reactions/"), 1, "Brim adds the entry reaction");
        let message = t.base + 77;
        sqlx::query("UPDATE giveaways SET message_id = $2 WHERE id = $1")
            .bind(gw.id)
            .bind(message as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sync_reactions(&t.app).await.unwrap();
        let before = t.calls("");

        let started = std::time::Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for k in 0..12_000u64 {
            let (app, user) = (t.app.clone(), (1u64 << 40) + k % 10_000);
            set.spawn(async move { on_reaction(&app, &reaction(gid, message, user, "🎉")).await });
        }
        while let Some(r) = set.join_next().await {
            r.unwrap().unwrap();
        }
        let took = started.elapsed();
        eprintln!(
            "12000 reactions (10000 members): {took:?} ({:?} each)",
            took / 12_000
        );
        assert_eq!(
            store::entry_count(&t.app.pool, gw.id).await.unwrap(),
            10_000
        );
        assert_eq!(t.calls("") - before, 0, "no Discord call per reaction");
        let refresh: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
        )
        .bind(g)
        .bind(store::REFRESH_JOB)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(refresh, 1);

        on_reaction(&t.app, &reaction(gid, message, 5, "👍"))
            .await
            .unwrap();
        on_reaction(&t.app, &reaction(gid, message + 1, 6, "🎉"))
            .await
            .unwrap();
        assert_eq!(
            store::entry_count(&t.app.pool, gw.id).await.unwrap(),
            10_000
        );
        wipe(&t, g).await;
    }

    /// A refused reaction gets one DM job with the reason, also after more reactions; a member
    /// moderation caught gets none.
    /// Reactions made while Brim was offline are entered at the end: the reaction list is read
    /// page by page, and a member who entered already is not read again.
    #[tokio::test]
    async fn reactions_missed_while_offline_enter_at_the_end() {
        let Some(t) = crate::test_support::T::new(306).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"giveaways": {"enabled": true}}))
            .await;
        let (gid, g) = (t.g(0), t.g(0).get() as i64);
        let mut n = plain(g);
        n.extras.reaction = true;
        let gw = store::create(&t.app.pool, &n, 25).await.unwrap().unwrap();
        store::set_posted(&t.app.pool, g, gw.id, 2, (t.base + 79) as i64)
            .await
            .unwrap();
        let users: Vec<u64> = (0..150).map(|k| (1u64 << 42) + k).collect();
        {
            let mut m = t.mock.lock().unwrap();
            for u in &users {
                m.members.insert((gid.get(), *u), vec![]);
            }
            m.reacted = users.clone();
        }
        // One reacted while Brim was online.
        remember_reactions(t.base + 79, gw.id);
        on_reaction(&t.app, &reaction(gid, t.base + 79, users[0], "🎉"))
            .await
            .unwrap();
        let before = t.calls("/members/");
        let gw = store::get(&t.app.pool, g, gw.id).await.unwrap().unwrap();
        catch_up_reactions(&t.app, &gw).await.unwrap();
        assert_eq!(store::entry_count(&t.app.pool, gw.id).await.unwrap(), 150);
        assert_eq!(
            t.calls("/members/") - before,
            149,
            "only members with no entry are read"
        );
        assert_eq!(t.calls("/reactions/"), 2, "two pages of 100");
        wipe(&t, g).await;
    }

    #[tokio::test]
    async fn a_refused_reaction_is_told_once() {
        let Some(t) = crate::test_support::T::new(303).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"giveaways": {"enabled": true}}))
            .await;
        let (gid, g) = (t.g(0), t.g(0).get() as i64);
        let mut n = plain(g);
        n.extras.reaction = true;
        n.req.required = vec![Id::new(4242)];
        let gw = store::create(&t.app.pool, &n, 25).await.unwrap().unwrap();
        let message = t.base + 78;
        store::set_posted(&t.app.pool, g, gw.id, 2, message as i64)
            .await
            .unwrap();
        remember_reactions(message, gw.id);
        let (new, caught) = (1u64 << 41, (1u64 << 41) + 1);
        sqlx::query(
            "INSERT INTO quarantines (guild_id, user_id, saved_roles) VALUES ($1, $2, '{}')",
        )
        .bind(g)
        .bind(caught as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
        for _ in 0..3 {
            on_reaction(&t.app, &reaction(gid, message, new, "🎉"))
                .await
                .unwrap();
            on_reaction(&t.app, &reaction(gid, message, caught, "🎉"))
                .await
                .unwrap();
        }
        let dms: Vec<Value> = sqlx::query_scalar(
            "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
        )
        .bind(g)
        .bind(store::DM_JOB)
        .fetch_all(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(dms.len(), 1, "{dms:?}");
        assert_eq!(dms[0]["user"].as_i64(), Some(new as i64));
        assert_eq!(store::entry_count(&t.app.pool, gw.id).await.unwrap(), 0);
        sqlx::query("DELETE FROM quarantines WHERE guild_id = $1")
            .bind(g)
            .execute(&t.app.pool)
            .await
            .unwrap();
        wipe(&t, g).await;
    }

    /// The button asks for the code only from a member who may enter; a wrong code counts, three
    /// give a wait, and the right one enters.
    #[tokio::test]
    async fn an_entry_code_is_asked_then_checked() {
        let Some(t) = crate::test_support::T::new(304).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"giveaways": {"enabled": true}}))
            .await;
        let g = t.g(0).get() as i64;
        let mut n = plain(g);
        n.code = Some("Open Sesame".into());
        n.req.required = vec![Id::new(4243)];
        let gw = store::create(&t.app.pool, &n, 25).await.unwrap().unwrap();
        static ROLE: [Id<RoleMarker>; 1] = [Id::new(4243)];
        let (no_role, role): (&[Id<RoleMarker>], &[Id<RoleMarker>]) = (&[], &ROLE);
        let who = |user: u64, roles: &'static [Id<RoleMarker>]| Who {
            user,
            bot: false,
            roles,
            joined: None,
            timeout: None,
        };
        let (a, b) = (1u64 << 42, (1u64 << 42) + 1);
        let r = admit(&t.app, &gw, who(a, no_role), None).await.unwrap();
        assert!(
            matches!(r, Admit::Refused(Refusal::MissingRole)),
            "the rules come before the code"
        );
        let r = admit(&t.app, &gw, who(a, no_role), Some("open sesame"))
            .await
            .unwrap();
        assert!(
            matches!(r, Admit::Refused(_)),
            "a right code does not skip the rules"
        );
        assert!(matches!(
            admit(&t.app, &gw, who(b, role), None).await.unwrap(),
            Admit::NeedsCode
        ));
        assert!(matches!(
            admit(&t.app, &gw, who(b, role), Some("x")).await.unwrap(),
            Admit::WrongCode(2)
        ));
        assert!(matches!(
            admit(&t.app, &gw, who(b, role), Some(" OPEN SESAME "))
                .await
                .unwrap(),
            Admit::New(1, None)
        ));
        let c = b + 1;
        for _ in 0..3 {
            admit(&t.app, &gw, who(c, role), Some("x")).await.unwrap();
        }
        assert!(
            matches!(
                admit(&t.app, &gw, who(c, role), None).await.unwrap(),
                Admit::Wait(10)
            ),
            "no form in the wait"
        );
        assert!(matches!(
            admit(&t.app, &gw, who(c, role), Some("open sesame"))
                .await
                .unwrap(),
            Admit::Wait(10)
        ));
        assert_eq!(store::entry_count(&t.app.pool, gw.id).await.unwrap(), 1);
        wipe(&t, g).await;
    }

    /// A member who won the same role in two giveaways keeps it until the later role time is over.
    #[tokio::test]
    async fn the_winner_role_stays_until_the_latest_role_end() {
        let Some(t) = crate::test_support::T::new(307).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"giveaways": {"enabled": true}}))
            .await;
        let (gid, g) = (t.g(0), t.g(0).get() as i64);
        let winner = t.base + 11;
        t.set(|m| {
            m.roles.insert(
                gid.get(),
                vec![
                    (gid.get(), false, 0, 0),
                    (601, false, 0, 1),
                    (699, false, 0, 9),
                ],
            );
            m.members.insert((gid.get(), winner), vec![]);
            m.members
                .insert((gid.get(), crate::test_support::ME), vec![699]);
        });
        t.app.invalidate_guild(gid);
        let w = winner as i64;
        let mut ids = vec![];
        for hours in [24, 72] {
            let mut n = plain(g);
            n.extras = store::Extras {
                winner_role: Some(Id::new(601)),
                role_hours: hours,
                ..Default::default()
            };
            let gw = store::create(&t.app.pool, &n, 25).await.unwrap().unwrap();
            store::claim_end(&t.app.pool, g, gw.id)
                .await
                .unwrap()
                .unwrap();
            assert!(store::save_draw(
                &t.app.pool,
                g,
                gw.id,
                0,
                &[w],
                &[w],
                "giveaway.winners",
                false
            )
            .await
            .unwrap());
            dm_job(&t.app, gid, &json!({"id": gw.id, "draws": 1, "user": w}))
                .await
                .unwrap();
            ids.push(gw.id);
        }
        let jobs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
        )
        .bind(g)
        .bind(store::ROLE_OFF_JOB)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(
            jobs, 2,
            "the second win adds its own removal, the role was not there before"
        );
        let has = || t.mock.lock().unwrap().members[&(gid.get(), winner)].contains(&601);
        assert!(has());
        // The first role time ends while the second still runs: the member keeps the role.
        sqlx::query("UPDATE scheduled_jobs SET run_at = now() - interval '1 minute' WHERE guild_id = $1 AND payload->>'id' = $2")
            .bind(g)
            .bind(ids[0].to_string())
            .execute(&t.app.pool)
            .await
            .unwrap();
        let payload = |id: i64| json!({"id": id, "user": w, "role": 601});
        role_off_job(&t.app, gid, &payload(ids[0])).await.unwrap();
        assert!(has(), "the later role time has not ended");
        // That job is done. The second role time ends: now the role goes.
        sqlx::query("DELETE FROM scheduled_jobs WHERE guild_id = $1 AND payload->>'id' = $2")
            .bind(g)
            .bind(ids[0].to_string())
            .execute(&t.app.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE scheduled_jobs SET run_at = now() - interval '1 minute' WHERE guild_id = $1",
        )
        .bind(g)
        .execute(&t.app.pool)
        .await
        .unwrap();
        role_off_job(&t.app, gid, &payload(ids[1])).await.unwrap();
        assert!(!has(), "the latest role time is over");
        // A replaced winner loses the role at once, though the same giveaway's long job waits.
        t.mock
            .lock()
            .unwrap()
            .members
            .get_mut(&(gid.get(), winner))
            .unwrap()
            .push(601);
        sqlx::query("INSERT INTO scheduled_jobs (guild_id, kind, payload, run_at) VALUES ($1, $2, $3, now() + interval '3 days')")
            .bind(g)
            .bind(store::ROLE_OFF_JOB)
            .bind(payload(ids[1]))
            .execute(&t.app.pool)
            .await
            .unwrap();
        role_off_job(&t.app, gid, &payload(ids[1])).await.unwrap();
        assert!(!has(), "its own later job does not keep the role");
        wipe(&t, g).await;
    }

    /// A winner gets a safe winner role and loses it at the end of the role time; a role with
    /// dangerous permissions is never given. The first draw makes the private winner thread with
    /// the winners and the host, and queues its close.
    #[tokio::test]
    async fn winners_get_the_role_and_the_thread() {
        let Some(t) = crate::test_support::T::new(305).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"giveaways": {"enabled": true}}))
            .await;
        let (gid, g) = (t.g(0), t.g(0).get() as i64);
        let (winner, host) = (t.base + 11, t.base + 12);
        t.set(|m| {
            m.roles.insert(
                gid.get(),
                vec![
                    (gid.get(), false, 0, 0),
                    (601, false, 0, 1),
                    (602, false, 8, 2),
                    (699, false, 0, 9),
                ],
            );
            m.members.insert((gid.get(), winner), vec![]);
            m.members
                .insert((gid.get(), crate::test_support::ME), vec![699]);
        });
        // The test server starts with a seeded empty role list.
        t.app.invalidate_guild(gid);
        let mut n = plain(g);
        n.host_id = Some(host as i64);
        n.extras = store::Extras {
            winner_role: Some(Id::new(601)),
            thread: true,
            ..Default::default()
        };
        let gw = store::create(&t.app.pool, &n, 25).await.unwrap().unwrap();
        store::claim_end(&t.app.pool, g, gw.id)
            .await
            .unwrap()
            .unwrap();
        let w = winner as i64;
        assert!(store::save_draw(
            &t.app.pool,
            g,
            gw.id,
            0,
            &[w],
            &[w],
            "giveaway.winners",
            false
        )
        .await
        .unwrap());
        dm_job(&t.app, gid, &json!({"id": gw.id, "draws": 1, "user": w}))
            .await
            .unwrap();
        assert!(t.mock.lock().unwrap().members[&(gid.get(), winner)].contains(&601));
        let off: i64 = sqlx::query_scalar(
            "SELECT run_at - now() > interval '167 hours' FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
        )
        .bind(g)
        .bind(store::ROLE_OFF_JOB)
        .fetch_one(&t.app.pool)
        .await
        .map(|b: bool| b as i64)
        .unwrap();
        assert_eq!(off, 1, "the role goes after 7 days");
        // The scheduler runs the job when it is due.
        sqlx::query("UPDATE scheduled_jobs SET run_at = now() - interval '1 minute' WHERE guild_id = $1 AND kind = $2")
            .bind(g)
            .bind(store::ROLE_OFF_JOB)
            .execute(&t.app.pool)
            .await
            .unwrap();
        role_off_job(&t.app, gid, &json!({"id": gw.id, "user": w, "role": 601}))
            .await
            .unwrap();
        assert!(!t.mock.lock().unwrap().members[&(gid.get(), winner)].contains(&601));

        let thread: Value = sqlx::query_scalar(
            "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
        )
        .bind(g)
        .bind(store::THREAD_JOB)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        thread_job(&t.app, gid, &thread).await.unwrap();
        assert_eq!(t.calls("POST /channels/2/threads"), 1);
        assert_eq!(t.calls("/thread-members/"), 2, "the winner and the host");
        let saved = store::get(&t.app.pool, g, gw.id).await.unwrap().unwrap();
        assert_eq!(saved.thread_id, Some(880));
        thread_close_job(&t.app, gid, &json!({"id": gw.id, "thread": 880}))
            .await
            .unwrap();
        assert_eq!(
            store::get(&t.app.pool, g, gw.id)
                .await
                .unwrap()
                .unwrap()
                .thread_id,
            None
        );

        // A role with Administrator is not given, and the dashboard row says why.
        sqlx::query("UPDATE giveaways SET extras = jsonb_set(extras, '{winner_role}', '\"602\"') WHERE id = $1")
            .bind(gw.id)
            .execute(&t.app.pool)
            .await
            .unwrap();
        dm_job(&t.app, gid, &json!({"id": gw.id, "draws": 1, "user": w}))
            .await
            .unwrap();
        assert!(!t.mock.lock().unwrap().members[&(gid.get(), winner)].contains(&602));
        let issue = store::get(&t.app.pool, g, gw.id)
            .await
            .unwrap()
            .unwrap()
            .issue;
        assert_eq!(issue.as_deref(), Some(ROLE_UNSAFE));
        wipe(&t, g).await;
    }
}

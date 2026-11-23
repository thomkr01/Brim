//! Giveaways on Discord: the message with its Enter button, the private entry answers, the draw
//! at the end time, rerolls and redraws, the re-post of a deleted message, entries that lose a
//! required role, and the `giveaway` command. The rows and the entry rules are in
//! `brim_core::giveaways`; the dashboard makes and changes giveaways through the same rows.
use anyhow::{Context, Result};
use brim_core::{
    giveaways::{self as store, Entered, Facts, Giveaway, Refusal, Req},
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
    channel::message::{
        component::{ActionRow, Button, ButtonStyle, Component},
        AllowedMentions, Embed, EmojiReactionType,
    },
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

/// Who may enter, one rule per line, for the running message.
fn requirements(req: &Req) -> String {
    let mut lines = vec![];
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
            let (winners, req) = (g.winners.to_string(), requirements(&g.req));
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
            (t, e, vec![enter_row(g)])
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
) -> Result<Option<Refusal>> {
    let gid = Id::new(g.guild_id as u64);
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
        raid_joiner,
        unverified,
        joined_ms,
        now_ms: crate::automod::now_ms(),
    };
    Ok(store::refusal(&g.req, g.host_id, g.sponsor_id, &f))
}

/// Handles the Enter button and the giveaway confirm buttons. False for other interactions.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let Some(InteractionData::MessageComponent(c)) = &i.data else {
        return Ok(false);
    };
    if let Some(id) = c.custom_id.strip_prefix(ENTER) {
        let id: i64 = id.parse().unwrap_or(0);
        let text = match enter(app, i, id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(?e, "a giveaway entry failed");
                "Something went wrong. Try again in a moment.".into()
            }
        };
        let answer = crate::verify::ephemeral(
            InteractionResponseType::ChannelMessageWithSource,
            Some(&text),
        );
        let client = app.http.interaction(Id::new(app.me.get()));
        client.create_response(i.id, &i.token, &answer).await?;
        return Ok(true);
    }
    if let Some(rest) = c.custom_id.strip_prefix(ANSWER) {
        answer(app, i, rest).await?;
        return Ok(true);
    }
    Ok(false)
}

/// The private answer to an Enter press. Only database reads: Discord drops an answer after 3 s.
async fn enter(app: &App, i: &Interaction, id: i64) -> Result<String> {
    let (Some(gid), Some(member)) = (i.guild_id, i.member.as_ref()) else {
        return Ok("This button works only in a server.".into());
    };
    let Some(user) = member.user.as_ref().or(i.user.as_ref()) else {
        return Ok("This button works only in a server.".into());
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
    let Some(g) = g.filter(Giveaway::running) else {
        return Ok(denied("this giveaway has ended.").await);
    };
    if !app.cfg(gid.get()).await?.giveaways.enabled {
        return Ok(reply::say_line(app, gid, "reply.giveaway.off", &[]).await);
    }
    if g.paused_at.is_some() {
        return Ok(denied(PAUSED).await);
    }
    // A repeat press (the most common one in a rush) needs no checks: the draw checks again.
    let had: Option<i16> = sqlx::query_scalar(
        "SELECT weight FROM giveaway_entries WHERE giveaway_id = $1 AND user_id = $2",
    )
    .bind(g.id)
    .bind(user.id.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    if let Some(w) = had {
        let (weight, _) = store::weight(&g.req, &member.roles);
        if w as u16 != weight {
            store::set_weight(&app.pool, g.guild_id, g.id, user.id.get() as i64, weight).await?;
        }
        let entries = weight.to_string();
        let vars = [("prize", g.prize.as_str()), ("entries", &entries)];
        return Ok(line(app, gid, "giveaway.entry_again", &vars).await);
    }
    let joined = ms(member.joined_at);
    let refused = check(app, &g, user.id.get(), user.bot, &member.roles, joined).await?;
    if let Some(r) = refused {
        return Ok(denied(&r.text()).await);
    }
    let (weight, why_role) = store::weight(&g.req, &member.roles);
    let entries = weight.to_string();
    match store::enter(&app.pool, g.guild_id, g.id, user.id.get() as i64, weight).await? {
        Entered::New => {
            queue_refresh(app, gid).await;
            let why = why_role
                .map(|r| format!("<@&{r}> role: {weight} entries"))
                .unwrap_or_default();
            let vars = [
                ("prize", g.prize.as_str()),
                ("entries", &entries),
                ("why", &why),
            ];
            Ok(line(app, gid, "giveaway.entry_ok", &vars).await)
        }
        Entered::Already(w) => {
            // Roles gained since the entry count now.
            if w as u16 != weight {
                store::set_weight(&app.pool, g.guild_id, g.id, user.id.get() as i64, weight)
                    .await?;
            }
            let vars = [("prize", g.prize.as_str()), ("entries", &entries)];
            Ok(line(app, gid, "giveaway.entry_again", &vars).await)
        }
        Entered::Closed => Ok(denied("this giveaway has ended.").await),
    }
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
        let refused = check(app, &g, user.get(), m.user.bot, &m.roles, joined).await?;
        if let Some(r) = refused {
            if store::remove_entry(&app.pool, g.guild_id, g.id, user.get() as i64).await? {
                queue_refresh(app, gid).await;
                // No DM to a member the moderation caught: it would tell a raider what was seen.
                let caught = matches!(
                    r,
                    Refusal::Quarantined | Refusal::Flagged | Refusal::RaidJoiner
                );
                if !caught {
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
                    let joined = ms(m.joined_at);
                    check(app, g, u as u64, m.user.bot, &m.roles, joined)
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
pub(crate) async fn end_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let id = p["id"].as_i64().context("job payload has no id")?;
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
    // A redraw since the save took this winner out.
    if !g.winner_ids.contains(&(user.get() as i64)) {
        return Ok(());
    }
    let sponsor = g.sponsor_id.map(|s| format!("<@{s}>")).unwrap_or_default();
    let link = link(&g);
    let vars = [
        ("prize", g.prize.as_str()),
        ("server", &server),
        ("sponsor", &sponsor),
        ("link", &link),
    ];
    let (text, embeds) = rendered(app, gid, "giveaway.winner_dm", &vars).await;
    reply::send_dm(app, user, &text, &embeds, &[]).await;
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
            if let Redrawn::NoneLeft = redraw(app, &g, &users, by, reason).await? {
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

/// A deleted channel: its running giveaways keep running and wait for staff to pick a channel.
pub async fn on_channel_delete(app: &App, gid: Id<GuildMarker>, channel: u64) -> Result<()> {
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
        Some(o) => Err(format!(
            "`{o}` is not a giveaway command. Use start, end, cancel, reroll, redraw, time or list"
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
    let key = match redraw(app, gw, users, by, reason).await? {
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
}

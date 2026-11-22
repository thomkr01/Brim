//! Tickets: a member DMs the bot, picks a server they share with it, and the bot opens a channel
//! in that server's ticket category. Messages are relayed both ways until staff close the ticket,
//! which logs it (with the whole conversation as a file) to the ticket log.
use anyhow::Result;
use brim_core::{permits, sections::TicketsCfg};
use brim_logging::embed::{avatar, clip, Log, BLUE, GREEN, ORANGE, RED};
use brim_logging::emoji::{TICKET, TICKET_BLOCK, TICKET_NEW, TICKET_REPLY};
use serde_json::{json, Value};
use twilight_model::{
    application::interaction::{Interaction, InteractionData},
    channel::{
        message::{
            component::{
                ActionRow, Button, ButtonStyle, SelectMenu, SelectMenuOption, SelectMenuType,
            },
            AllowedMentions, Component, Embed, MentionType, Message,
        },
        permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
        Attachment as Uploaded, ChannelType,
    },
    gateway::payload::incoming::MessageCreate,
    guild::Permissions,
    http::{
        attachment::Attachment,
        interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    },
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, RoleMarker, UserMarker},
        Id,
    },
    user::User,
    util::Timestamp,
};
use twilight_util::builder::embed::{
    EmbedAuthorBuilder, EmbedBuilder, EmbedFieldBuilder, EmbedFooterBuilder, ImageSource,
};

use crate::{
    app::{now_secs, App},
    http_err::{self, best_effort},
    settings,
};

/// Prefix for commands in DMs, where no server prefix applies.
const DM_PREFIX: &str = "t!";
/// A confirmation or server menu stops working after this long.
const MENU_SECS: i64 = 180;
/// History read for the close log, newest first.
const HISTORY_PAGES: usize = 100;

const STAFF: Permissions = Permissions::VIEW_CHANNEL
    .union(Permissions::SEND_MESSAGES)
    .union(Permissions::READ_MESSAGE_HISTORY)
    .union(Permissions::ATTACH_FILES)
    .union(Permissions::EMBED_LINKS)
    .union(Permissions::ADD_REACTIONS);

/// Hidden from @everyone (its role id is the guild id); the bot and staff get in. The member never
/// sees the channel, everything reaches them by DM.
fn overwrites(gid: Id<GuildMarker>, bot: u64, staff: &[u64]) -> Vec<PermissionOverwrite> {
    let allow = |id: u64, kind| PermissionOverwrite {
        allow: STAFF,
        deny: Permissions::empty(),
        id: Id::new(id),
        kind,
    };
    let mut v = vec![
        PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
            id: gid.cast(),
            kind: PermissionOverwriteType::Role,
        },
        allow(bot, PermissionOverwriteType::Member),
    ];
    v.extend(
        staff
            .iter()
            .map(|r| allow(*r, PermissionOverwriteType::Role)),
    );
    v
}

/// Roles of permits with Manage Tickets: the only ticket staff (owner decision 11-15, no access roles).
async fn staff_roles(app: &App, gid: Id<GuildMarker>) -> Result<Vec<u64>> {
    let mut v: Vec<u64> = permits::roles_with(&app.pool, gid.get() as i64, "tickets")
        .await?
        .into_iter()
        .map(|r| r as u64)
        .collect();
    v.sort_unstable();
    v.dedup();
    Ok(live_roles(app, gid, v).await)
}

/// `live_roles` over role ids.
pub(super) async fn live_ids(
    app: &App,
    gid: Id<GuildMarker>,
    roles: &[Id<RoleMarker>],
) -> Vec<Id<RoleMarker>> {
    let ids = roles.iter().map(|r| r.get()).collect();
    let live = live_roles(app, gid, ids).await;
    live.into_iter().filter_map(Id::new_checked).collect()
}

/// Drops ids of roles the server no longer has (a permit or ping role that was deleted or re-made):
/// a ping of one shows as `@unknown-role`, and an overwrite for one is dead weight. A failed
/// lookup keeps the list, so a Discord hiccup never empties the staff view.
pub(super) async fn live_roles(app: &App, gid: Id<GuildMarker>, roles: Vec<u64>) -> Vec<u64> {
    match app.guild_info(gid).await {
        Ok(i) => roles
            .into_iter()
            .filter(|r| Id::new_checked(*r).is_some_and(|r| i.roles.contains_key(&r)))
            .collect(),
        Err(e) => {
            tracing::warn!(?e, %gid, "reading the server's roles failed; keeping every ticket role");
            roles
        }
    }
}

/// The message variables `tags` fills in, listed on the dashboard's Commands page.
pub const TAGS: [(&str, &str); 4] = [
    ("{username}", "The member's username"),
    ("{userid}", "The member's user id"),
    ("{usermention}", "A mention of the member"),
    (
        "{ticket_type}",
        "The ticket's type (empty when it has none)",
    ),
];
/// Where `tags` runs.
pub const TAGS_IN: &str = "Ticket greeting and closing messages and snippets";

/// Fills `TAGS` in greeting, closing and snippet texts; `kind` is the ticket's type or "".
fn tags(text: &str, u: &User, kind: &str) -> String {
    let values = [
        u.name.clone(),
        u.id.to_string(),
        format!("<@{}>", u.id),
        kind.to_string(),
    ];
    let text = TAGS
        .iter()
        .zip(values)
        .fold(text.to_string(), |t, ((k, _), v)| t.replace(k, &v));
    clip(&text, 2048)
}

/// The username without punctuation (or the id when nothing is left), then `digits` as five
/// figures so staff can tell two tickets of one member apart.
fn channel_name(name: &str, id: Id<UserMarker>, digits: u32) -> String {
    let n: String = name
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_ascii_punctuation() && !c.is_control())
        .collect();
    let n = if n.trim().is_empty() {
        id.to_string()
    } else {
        n
    };
    format!("{n}-{:05}", digits % 100_000)
}

/// The ticket type's slug (lowercase letters and digits, words joined by `-`, at most 20 characters).
fn kind_slug(kind: &str) -> String {
    let s: String = kind
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let words: Vec<&str> = s.split('-').filter(|w| !w.is_empty()).collect();
    let s: String = words.join("-").chars().take(20).collect();
    s.trim_end_matches('-').to_string()
}

/// `name` (`<head>-<number>`) with the ticket type between head and number: `tchicx-report-41226`.
/// `old` is the type the name carries now, replaced. No usable type (or no `-`) keeps `name`.
/// The result stays within Discord's 100 characters.
fn kind_name(name: &str, old: Option<&str>, kind: &str) -> String {
    let (slug, Some((head, tail))) = (kind_slug(kind), name.rsplit_once('-')) else {
        return name.to_string();
    };
    if slug.is_empty() {
        return name.to_string();
    }
    let old = old.map(kind_slug).filter(|o| !o.is_empty());
    let head = old
        .and_then(|o| head.strip_suffix(&format!("-{o}")))
        .unwrap_or(head);
    let room = 100usize.saturating_sub(tail.chars().count() + slug.chars().count() + 2);
    let head: String = head.chars().take(room).collect();
    format!("{head}-{slug}-{tail}")
}

/// Makes the staff-only category `name`, plus the `ticket-logs` channel in it when `log` is set.
async fn make_category(
    app: &App,
    gid: Id<GuildMarker>,
    name: &str,
    log: bool,
) -> Result<(Id<ChannelMarker>, Option<Id<ChannelMarker>>)> {
    let ows = overwrites(gid, app.me.get(), &staff_roles(app, gid).await?);
    let category = app
        .http
        .create_guild_channel(gid, name)
        .kind(ChannelType::GuildCategory)
        .permission_overwrites(&ows)
        .await?
        .model()
        .await?
        .id;
    if !log {
        return Ok((category, None));
    }
    Ok((category, Some(make_log(app, gid, &ows).await?)))
}

/// A new #ticket-logs in the Logs category, locked like every log channel (only Brim may
/// delete messages there). It follows the category when every ticket staff role in `ows`
/// already sees it ([`ticket_log_follows`], owner decision 2026-11-18), else it gets `ows`.
pub(super) async fn make_log(
    app: &App,
    gid: Id<GuildMarker>,
    ows: &[PermissionOverwrite],
) -> Result<Id<ChannelMarker>> {
    use brim_antinuke::security::ticket_log_follows;
    let logs = logs_category(app, gid).await?;
    let staff: Vec<u64> = ows
        .iter()
        .filter(|o| o.kind == PermissionOverwriteType::Role && o.id.get() != gid.get())
        .map(|o| o.id.get())
        .collect();
    let cat = crate::backup::channel_img(app.http.channel(logs).await?.model().await?);
    let mut b = app
        .http
        .create_guild_channel(gid, "ticket-logs")
        .kind(ChannelType::GuildText)
        .parent_id(logs);
    if !ticket_log_follows(&cat, &staff) {
        b = b.permission_overwrites(ows);
    }
    let l = b.await?.model().await?;
    if let Err(e) = crate::protected::lock(app, gid, &l).await {
        tracing::warn!(?e, %gid, "locking the new ticket log failed");
    }
    Ok(l.id)
}

/// The shared Logs category, made like the dashboard makes it when the server has none, so the
/// ticket log goes with the other logs (owner decision 11-15).
async fn logs_category(app: &App, gid: Id<GuildMarker>) -> Result<Id<ChannelMarker>> {
    use brim_antinuke::security::{private_category, LOGS_CATEGORY};
    let channels = app.http.guild_channels(gid).await?.models().await?;
    if let Some(c) = channels
        .iter()
        .find(|c| c.kind == ChannelType::GuildCategory && c.name.as_deref() == Some(LOGS_CATEGORY))
    {
        return Ok(c.id);
    }
    let ows: Vec<PermissionOverwrite> = private_category(gid.get(), app.me.get())
        .into_iter()
        .map(|o| PermissionOverwrite {
            allow: Permissions::from_bits_retain(o.allow),
            deny: Permissions::from_bits_retain(o.deny),
            id: Id::new(o.target),
            kind: if o.is_role {
                PermissionOverwriteType::Role
            } else {
                PermissionOverwriteType::Member
            },
        })
        .collect();
    Ok(app
        .http
        .create_guild_channel(gid, LOGS_CATEGORY)
        .kind(ChannelType::GuildCategory)
        .permission_overwrites(&ows)
        .await?
        .model()
        .await?
        .id)
}

/// Tickets are on but the category is unset or deleted: make it (and the log when that is unset)
/// so turning Tickets on is all the setup a server needs. Returns the category.
/// False when Discord says the saved category is gone (10003) or it is in another server. The cache can miss a
/// category the dashboard made a moment ago, so a miss asks Discord.
pub(crate) async fn category_live(
    app: &App,
    gid: Id<GuildMarker>,
    c: Id<ChannelMarker>,
) -> Result<bool> {
    if app.guild_info(gid).await?.channels.contains(&c) {
        return Ok(true);
    }
    // Not in the cache yet (just made), so ask Discord; another server's category is not ours.
    match app.http.channel(c).await.map_err(anyhow::Error::from) {
        Ok(r) => Ok(r.model().await?.guild_id == Some(gid)),
        Err(e) if http_err::code(&e) == Some(10003) => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn ensure_category(app: &App, gid: Id<GuildMarker>) -> Result<Option<Id<ChannelMarker>>> {
    // One at a time per server: two tickets opened together would each make a category, and the
    // first ticket would sit outside the one saved last. Per server, so a slow one stalls no other.
    type Locks =
        std::sync::Mutex<std::collections::HashMap<u64, std::sync::Arc<tokio::sync::Mutex<()>>>>;
    static ONE: std::sync::LazyLock<Locks> = std::sync::LazyLock::new(Default::default);
    let m = ONE.lock().unwrap().entry(gid.get()).or_default().clone();
    let _one = m.lock().await;
    let cfg = app.cfg(gid.get()).await?;
    let t = &cfg.tickets;
    if !t.enabled {
        return Ok(None);
    }
    if let Some(c) = t.category {
        if category_live(app, gid, c).await? {
            return Ok(Some(c));
        }
    }
    let (category, log) = make_category(app, gid, "Tickets", t.log_channel.is_none()).await?;
    settings::save(app, gid, app.me.get(), &["tickets"], |doc| {
        if !doc["tickets"].is_object() {
            doc["tickets"] = json!({});
        }
        doc["tickets"]["category"] = json!(category.to_string());
        if let Some(l) = log {
            doc["tickets"]["log_channel"] = json!(l.to_string());
        }
        Ok(())
    })
    .await?
    .map_err(anyhow::Error::msg)?;
    Ok(Some(category))
}

fn emb(title: &str, text: &str, color: u32) -> EmbedBuilder {
    let mut b = EmbedBuilder::new().color(color);
    if !title.is_empty() {
        b = b.title(title);
    }
    if !text.is_empty() {
        b = b.description(clip(text, 4096));
    }
    b
}

fn stamp(b: EmbedBuilder) -> EmbedBuilder {
    match Timestamp::from_secs(now_secs()) {
        Ok(t) => b.timestamp(t),
        Err(_) => b,
    }
}

fn footer(b: EmbedBuilder, text: &str, icon: Option<String>) -> EmbedBuilder {
    let mut f = EmbedFooterBuilder::new(clip(text, 2048));
    if let Some(src) = icon.and_then(|u| ImageSource::url(u).ok()) {
        f = f.icon_url(src);
    }
    b.footer(f)
}

fn author(b: EmbedBuilder, name: &str, icon: String) -> EmbedBuilder {
    let mut a = EmbedAuthorBuilder::new(clip(name, 256));
    if let Ok(src) = ImageSource::url(icon) {
        a = a.icon_url(src);
    }
    b.author(a)
}

fn user_footer(b: EmbedBuilder, u: &User) -> EmbedBuilder {
    footer(b, &format!("{} | {}", u.name, u.id), Some(avatar(u)))
}

fn error(text: &str) -> Embed {
    emb("", text, RED).build()
}

async fn say(app: &App, ch: Id<ChannelMarker>, e: Embed) -> Result<()> {
    send(app, ch, "", &[e], true).await
}

/// Sends the short reply entry `key` (`reply.ticket.*`) in `ch`. An edited version goes as it
/// is; the default goes through the command's frame like any other reply.
async fn say_key(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    key: &str,
    vars: &[(&str, &str)],
) -> Result<()> {
    let s = crate::reply::say(app, gid, key, vars).await;
    send(app, ch, &s.text, &s.embeds, !s.edited).await
}

/// Sends the bot-wide entry `key` (`reply.dm.*`) in the member's DM `ch`, where no server is known.
async fn say_dm(app: &App, ch: Id<ChannelMarker>, key: &str, vars: &[(&str, &str)]) -> Result<()> {
    let s = crate::reply::say_wide(app, key, vars).await;
    send(app, ch, &s.text, &s.embeds, !s.edited).await
}

/// A `reply.ticket.*` entry and its values, made where the result is known and said later.
pub(crate) struct Line(&'static str, Vec<(&'static str, String)>);

impl Line {
    fn with(key: &'static str, vars: &[(&'static str, &str)]) -> Self {
        Self(key, vars.iter().map(|(k, v)| (*k, v.to_string())).collect())
    }

    async fn said(&self, app: &App, gid: Id<GuildMarker>) -> crate::reply::Said {
        let vars: Vec<(&str, &str)> = self.1.iter().map(|(k, v)| (*k, v.as_str())).collect();
        crate::reply::say(app, gid, self.0, &vars).await
    }

    /// Sent in `ch` like [`say_key`].
    async fn send(&self, app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>) -> Result<()> {
        let s = self.said(app, gid).await;
        send(app, ch, &s.text, &s.embeds, !s.edited).await
    }
}

impl From<&'static str> for Line {
    fn from(key: &'static str) -> Self {
        Self(key, vec![])
    }
}

async fn send(
    app: &App,
    ch: Id<ChannelMarker>,
    text: &str,
    one: &[Embed],
    frame: bool,
) -> Result<()> {
    let framed = if frame {
        crate::reply::command_reply(app, false, text, one).await
    } else {
        None
    };
    let (text, embeds) = framed.as_ref().map_or((text, one), |f| (&f.0, &f.1));
    let mut post = app.http.create_message(ch).content(text).embeds(embeds);
    if let Some(to) = crate::cmdcfg::reply_to(ch) {
        post = post.reply(to).fail_if_not_exists(false);
    }
    let sent = post
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await?;
    crate::cmdcfg::sent(app, ch, sent).await
}

/// Every prefix word the ticket commands answer.
pub(crate) fn words() -> impl Iterator<Item = &'static str> {
    crate::general::ticket_commands()
        .iter()
        .filter_map(|n| crate::general::lookup(n))
        .flat_map(|c| std::iter::once(c.0).chain(c.1.iter().copied()))
        .chain(crate::cmdcfg::TICKET_WORDS.iter().copied())
}

/// Whether `ch` holds an open ticket, where `r`, `c` and `s` mean the ticket commands.
pub(crate) async fn in_ticket(
    app: &App,
    cfg: &crate::app::GuildCfg,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
) -> Result<bool> {
    Ok(ticket_here(app, &cfg.tickets, gid, ch).await?.is_some())
}

/// A server the bot is in, for embed footers and the server menu.
#[derive(Clone)]
pub(crate) struct Server {
    pub(crate) id: Id<GuildMarker>,
    pub(crate) name: String,
    pub(crate) icon: Option<String>,
    /// The server's own copies of the ticket messages (`bot_messages`, keys `ticket.*`).
    pub(crate) own: Vec<(String, Value)>,
}

impl Server {
    /// The built-in message `key` (the server's copy, else the default), with the server footer
    /// unless the copy has its own.
    fn tpl(&self, key: &str, vars: &[(&str, &str)]) -> EmbedBuilder {
        let stored = self.own.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let e = crate::reply::bot_embed(key, stored, vars);
        let own_footer = e.footer.is_some();
        let b = EmbedBuilder::from(e);
        if own_footer {
            b
        } else {
            self.footer(b)
        }
    }

    fn footer(&self, b: EmbedBuilder) -> EmbedBuilder {
        footer(
            b,
            &format!("{} | {}", self.name, self.id),
            self.icon.clone(),
        )
    }
}

/// `None` when the bot is not in the server.
async fn server(app: &App, gid: Id<GuildMarker>) -> Result<Option<Server>> {
    match app.http.guild(gid).await {
        Ok(r) => {
            let g = r.model().await?;
            let own = sqlx::query_as(
                "SELECT key, message FROM bot_messages WHERE guild_id = $1 AND key LIKE 'ticket.%'",
            )
            .bind(gid.get() as i64)
            .fetch_all(&app.pool)
            .await
            // The defaults still send when the copies cannot be read.
            .unwrap_or_else(|e| {
                tracing::warn!(?e, "reading the saved ticket messages failed");
                vec![]
            });
            Ok(Some(Server {
                own,
                id: g.id,
                icon: g
                    .icon
                    .map(|h| format!("https://cdn.discordapp.com/icons/{}/{h}.png", g.id)),
                name: g.name,
            }))
        }
        Err(e) if http_err::no_access(&e) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Discord's upload limit for a bot without boosts, per message; files past it are linked instead.
const COPY_MAX: u64 = 25 << 20;

/// Copies of a message's files, to post them again elsewhere, and the ones left as a link (past
/// [`COPY_MAX`] in total, or the download failed).
async fn download<'a>(app: &App, list: &'a [Uploaded]) -> (Vec<Attachment>, Vec<&'a Uploaded>) {
    let (mut out, mut linked, mut total) = (vec![], vec![], 0);
    for (i, a) in list.iter().enumerate() {
        if total + a.size > COPY_MAX {
            linked.push(a);
            continue;
        }
        let got = async {
            // The shared client has a timeout; a bare `reqwest::get` could hang the reply.
            let r = app
                .api
                .client()
                .get(&a.url)
                .send()
                .await?
                .error_for_status()?;
            brim_automod::sites::read_capped(r, (COPY_MAX - total) as usize).await
        };
        match got.await {
            Ok(b) => {
                total += b.len() as u64;
                out.push(Attachment::from_bytes(a.filename.clone(), b, i as u64))
            }
            Err(e) => {
                tracing::warn!(?e, file = %a.filename, "ticket attachment download failed");
                linked.push(a);
            }
        }
    }
    (out, linked)
}

/// Files that could not be copied onto the message, as links. Copied files sit on the message
/// itself: a link to them would stop working after about a day, like these ones do.
fn attachment_fields<'a>(
    mut b: EmbedBuilder,
    linked: impl IntoIterator<Item = &'a Uploaded>,
) -> EmbedBuilder {
    for (n, a) in linked.into_iter().enumerate() {
        // Plain name only: no formatting, mention or link text from the uploader.
        let name: String = a
            .filename
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
            .take(100)
            .collect();
        let name = if name.is_empty() { "file".into() } else { name };
        b = b.field(EmbedFieldBuilder::new(
            format!("Attachment {}", n + 1),
            format!(
                "[{name}]({}) (not copied; this link stops working after about a day)",
                a.url.replace(')', "%29")
            ),
        ));
    }
    b
}

/// Whether the member wants a confirmation before a DM goes to their one ticket server.
async fn user_state(app: &App, uid: Id<UserMarker>) -> Result<bool> {
    let r: Option<bool> =
        sqlx::query_scalar("SELECT confirmation FROM ticket_users WHERE user_id = $1")
            .bind(uid.get() as i64)
            .fetch_optional(&app.pool)
            .await?;
    Ok(r.unwrap_or(true))
}

/// The member's open DM ticket channel in a server.
async fn open_ticket(
    app: &App,
    gid: Id<GuildMarker>,
    uid: Id<UserMarker>,
) -> Result<Option<Id<ChannelMarker>>> {
    let ch: Option<Option<i64>> = sqlx::query_scalar(
        "SELECT channel_id FROM tickets WHERE guild_id = $1 AND user_id = $2 AND closed_at IS NULL AND panel IS NULL",
    )
    .bind(gid.get() as i64)
    .bind(uid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    Ok(ch.flatten().map(|c| Id::new(c as u64)))
}

/// The open ticket in a channel.
pub(crate) struct Here {
    pub(crate) number: i32,
    pub(crate) user: Id<UserMarker>,
    /// The panel id; `None` for a DM ticket, whose member is reached by DM.
    pub(crate) panel: Option<String>,
}

/// The open ticket in this channel. Without panels only channels in the ticket category are
/// looked up, so other messages cost no query.
async fn ticket_here(
    app: &App,
    t: &TicketsCfg,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
) -> Result<Option<Here>> {
    let Some(parent) = app.channel_scope(gid, ch).await?.1 else {
        return Ok(None);
    };
    let ours = t.category == Some(parent) || t.panels.iter().any(|p| p.category == Some(parent));
    // shortcut: a panel ticket left open after every panel was removed is no longer found here
    // (deleting its channel still closes it); keep a set of open ticket channels if that matters.
    if !ours && t.panels.is_empty() {
        return Ok(None);
    }
    let row: Option<(i32, i64, Option<String>)> = sqlx::query_as(
        "SELECT number, user_id, panel FROM tickets WHERE channel_id = $1 AND guild_id = $2 AND closed_at IS NULL",
    )
    .bind(ch.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    // A panel ticket stays one when its panel's category changes later.
    let row = row.filter(|r| ours || r.2.is_some());
    Ok(row.map(|(number, u, panel)| Here {
        number,
        user: Id::new(u as u64),
        panel,
    }))
}

/// New tickets one member, and the whole server, may open per hour, DM and panel together.
const TICKETS_PER_HOUR: i64 = 3;
const SERVER_TICKETS_PER_HOUR: i64 = 20;

#[derive(Debug, PartialEq)]
enum Claim {
    Number(i32),
    /// The member has an open DM ticket here, or all of the panel's slots are open.
    Open,
    UserCap,
    ServerCap,
}

/// Takes the next ticket number in a server for a new ticket, DM (`panel` None) or panel (its id
/// and how many of it a member may have open). The hourly caps and the free slot are read under the
/// same per-server lock as the insert, so parallel opens cannot pass a cap together.
async fn claim_number(
    pool: &sqlx::PgPool,
    gid: Id<GuildMarker>,
    uid: Id<UserMarker>,
    panel: Option<(&str, u8)>,
) -> Result<Claim> {
    let (g, u) = (gid.get() as i64, uid.get() as i64);
    let mut tx = pool.begin().await?;
    // Its own key: the bare guild id is the config lock.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('tickets:' || $1, 0))")
        .bind(g)
        .execute(&mut *tx)
        .await?;
    // T-04: a restart between taking the number and making the channel left the row behind, and
    // the open-ticket index then refused every new ticket of this member here.
    sqlx::query(
        "DELETE FROM tickets WHERE guild_id = $1 AND user_id = $2 AND status = 'creating'
         AND opened_at < now() - interval '10 minutes'",
    )
    .bind(g)
    .bind(u)
    .execute(&mut *tx)
    .await?;
    let (mine, all): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE user_id = $2), count(*) FROM tickets
         WHERE guild_id = $1 AND opened_at > now() - interval '1 hour'",
    )
    .bind(g)
    .bind(u)
    .fetch_one(&mut *tx)
    .await?;
    if mine >= TICKETS_PER_HOUR {
        return Ok(Claim::UserCap);
    }
    if all >= SERVER_TICKETS_PER_HOUR {
        return Ok(Claim::ServerCap);
    }
    let slot = match panel {
        None => 1,
        Some((id, most)) => {
            let used: Vec<i16> = sqlx::query_scalar(
                "SELECT open_slot FROM tickets WHERE guild_id = $1 AND user_id = $2 AND panel = $3 AND closed_at IS NULL",
            )
            .bind(g)
            .bind(u)
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
            match (1..=i16::from(most.clamp(1, 5))).find(|s| !used.contains(s)) {
                Some(s) => s,
                None => return Ok(Claim::Open),
            }
        }
    };
    let number = sqlx::query_scalar(
        "INSERT INTO tickets (guild_id, number, user_id, status, source, panel, open_slot)
         SELECT $1, COALESCE(MAX(number), 0) + 1, $2, 'creating',
                CASE WHEN $3::text IS NULL THEN 'dm' ELSE 'panel' END, $3, $4
         FROM tickets WHERE guild_id = $1
         ON CONFLICT DO NOTHING RETURNING number",
    )
    .bind(g)
    .bind(u)
    .bind(panel.map(|p| p.0))
    .bind(slot)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(number.map_or(Claim::Open, Claim::Number))
}

/// What a member is told when `claim_number` gives no number.
fn refusal(c: &Claim) -> &'static str {
    match c {
        Claim::UserCap => "reply.ticket.cap_user",
        Claim::ServerCap => "reply.ticket.cap_server",
        _ => "reply.ticket.being_opened",
    }
}

/// Records what happened to a ticket: who and what, never message text. Only logged on failure,
/// since the action itself is done.
pub(crate) async fn event(
    app: &App,
    gid: Id<GuildMarker>,
    number: i32,
    actor: Option<Id<UserMarker>>,
    action: &str,
    details: Value,
) {
    let r = sqlx::query(
        "INSERT INTO ticket_events (guild_id, number, actor_id, action, details) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(gid.get() as i64)
    .bind(number)
    .bind(actor.map(|a| a.get() as i64))
    .bind(action)
    .bind(details)
    .execute(&app.pool)
    .await;
    if let Err(e) = r {
        tracing::warn!(?e, number, action, "ticket event not saved");
    }
}

/// A member or staff message in a ticket: the inactivity clock restarts, and the first staff
/// answer the member gets is timed.
pub(crate) async fn touch(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    staff: bool,
) -> Result<()> {
    sqlx::query(
        "UPDATE tickets SET last_activity_at = now(),
                first_staff_response_at = CASE WHEN $2 THEN coalesce(first_staff_response_at, now())
                                          ELSE first_staff_response_at END
         WHERE channel_id = $1 AND closed_at IS NULL AND guild_id = $3",
    )
    .bind(ch.get() as i64)
    .bind(staff)
    .bind(gid.get() as i64)
    .execute(&app.pool)
    .await?;
    Ok(())
}

/// Closes tickets that had no message for `tickets.auto_close_hours` (0, the default, is off).
/// One sweep every 10 minutes for all servers, at most 25 tickets per server each; `staff::close`
/// reads the activity again.
pub async fn auto_close(app: std::sync::Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
    loop {
        tick.tick().await;
        let guilds = match ticket_guilds(&app.pool).await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(?e, "ticket auto-close sweep failed");
                continue;
            }
        };
        for g in guilds {
            if let Err(e) = auto_close_guild(&app, Id::new(g as u64)).await {
                tracing::warn!(?e, guild = g, "ticket auto-close failed");
            }
        }
    }
}

async fn auto_close_guild(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let hours = app.cfg(gid.get()).await?.tickets.auto_close_hours;
    if hours == 0 {
        return Ok(());
    }
    let due: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT channel_id, user_id FROM tickets WHERE guild_id = $1 AND status = 'open'
         AND channel_id IS NOT NULL AND last_activity_at < now() - make_interval(hours => $2)
         ORDER BY last_activity_at LIMIT 25",
    )
    .bind(gid.get() as i64)
    .bind(hours as i32)
    .fetch_all(&app.pool)
    .await?;
    if due.is_empty() {
        return Ok(());
    }
    let me = app.http.user(app.me).await?.model().await?;
    let reason = format!("Closed after {hours} hours without a message.");
    for (ch, user) in due {
        let (ch, user) = (Id::new(ch as u64), Id::new(user as u64));
        // Messages sent while the bot was offline never reached `touch`: ask Discord for the last one.
        let last = match app.http.channel(ch).await {
            Ok(r) => r.model().await?.last_message_id,
            Err(e) if http_err::gone(&e) => None,
            Err(e) => {
                tracing::warn!(?e, %ch, "reading a ticket channel for auto-close failed");
                continue;
            }
        };
        // A snowflake holds its time: milliseconds since 2015 in the top 42 bits.
        let at = last.map(|m| ((m.get() >> 22) + 1_420_070_400_000) / 1000);
        if at.is_some_and(|t| t as i64 > crate::app::now_secs() - i64::from(hours) * 3600) {
            sqlx::query(
                "UPDATE tickets SET last_activity_at = to_timestamp($2) WHERE channel_id = $1 AND guild_id = $3",
            )
            .bind(ch.get() as i64)
            .bind(at.unwrap_or_default() as f64)
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await?;
            continue;
        }
        if let Err(e) = staff::close(app, gid, ch, user, &me, &reason, false, hours).await {
            tracing::warn!(?e, %ch, "auto-closing a ticket failed");
            // Try again after a whole idle time, not at every sweep (staff already saw the error).
            sqlx::query(
                "UPDATE tickets SET last_activity_at = now() WHERE channel_id = $1 AND status = 'open' AND guild_id = $2",
            )
            .bind(ch.get() as i64)
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await
            .ok();
        }
    }
    Ok(())
}

/// Servers whose newest config has tickets on.
async fn ticket_guilds(pool: &sqlx::PgPool) -> Result<Vec<i64>> {
    // shortcut: reads every server's newest config; fine for hundreds of servers, index tickets.enabled when it grows.
    Ok(sqlx::query_scalar(
        "SELECT guild_id FROM (
            SELECT DISTINCT ON (guild_id) guild_id, config FROM guild_config ORDER BY guild_id, version DESC
         ) c WHERE c.config #>> '{tickets,enabled}' = 'true'",
    )
    .fetch_all(pool)
    .await?)
}

/// A deleted ticket channel ends its ticket. A ticket that is closing is left to the close, which
/// records who closed it.
pub async fn on_channel_delete(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
) -> Result<()> {
    sqlx::query(
        "UPDATE tickets SET status = 'closed', closed_at = now() WHERE channel_id = $1 AND status = 'open' AND guild_id = $2",
    )
    .bind(ch.get() as i64)
    .bind(gid.get() as i64)
    .execute(&app.pool)
    .await?;
    Ok(())
}

/// The `ticket_close_check` job, queued when a close starts (T-03): a ticket still closing then
/// was cut off by a restart. With its channel gone it is closed; else it opens again and staff are told.
pub async fn close_check(app: &App, gid: Id<GuildMarker>, payload: &Value) -> Result<()> {
    let Some(ch) = payload["channel"]
        .as_u64()
        .and_then(Id::<ChannelMarker>::new_checked)
    else {
        return Ok(());
    };
    let closing: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tickets WHERE channel_id = $1 AND status = 'closing' AND guild_id = $2)",
    )
    .bind(ch.get() as i64)
    .bind(gid.get() as i64)
    .fetch_one(&app.pool)
    .await?;
    if !closing {
        return Ok(());
    }
    // Any answer but 404 opens the ticket again: a job that failed out would leave it closing.
    let gone = matches!(app.http.channel(ch).await, Err(e) if http_err::gone(&e));
    let sql = if gone {
        "UPDATE tickets SET status = 'closed', closed_at = now() WHERE channel_id = $1 AND status = 'closing' AND guild_id = $2"
    } else {
        "UPDATE tickets SET status = 'open' WHERE channel_id = $1 AND status = 'closing' AND guild_id = $2"
    };
    sqlx::query(sql)
        .bind(ch.get() as i64)
        .bind(gid.get() as i64)
        .execute(&app.pool)
        .await?;
    if !gone {
        if let Err(e) = say_key(app, gid, ch, "reply.ticket.close_unfinished", &[]).await {
            tracing::warn!(?e, %ch, "ticket close check notice failed");
        }
    }
    Ok(())
}

fn button(label: &str, id: String, style: ButtonStyle) -> Component {
    Component::Button(Button {
        id: None,
        custom_id: Some(id),
        disabled: false,
        emoji: None,
        label: Some(label.into()),
        style,
        url: None,
        sku_id: None,
    })
}

fn action_row(components: Vec<Component>) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components,
    })
}

mod commands;
mod dm;
mod msgs;
mod panel;
pub(crate) mod public;
mod staff;
mod workflow;
#[cfg(test)]
pub(crate) use {
    staff::closed_log,
    workflow::{claimed_log, handed_log, type_log},
};

pub(crate) use msgs::*;

pub use commands::{blocked_page, close_all, on_message, snippets_page};
pub use dm::on_dm;

/// Ticket buttons, menus and modals: panel tickets first, then the DM ticket flow.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    Ok(panel::handle(app, i).await? || dm::handle(app, i).await?)
}

/// Seconds after a close starts until `close_check` looks at it.
const CLOSE_CHECK_SECS: i64 = 15 * 60;

/// "New Ticket" in the ticket log; `panel` names the panel it was opened from, `kind` is the
/// ticket's type if it has one.
pub(crate) fn new_ticket_log(user: &User, panel: Option<&str>, kind: Option<&str>) -> Log {
    let footer = match panel {
        Some(p) => format!("{} | {} | {p}", user.name, user.id),
        None => format!("{} | {}", user.name, user.id),
    };
    let mut e = Log::new("New Ticket", GREEN).user("User", user.id.get());
    if let Some(k) = kind.filter(|k| !k.is_empty()) {
        e = e.line("Type", k);
    }
    e.footer(&footer)
        .var("member", &brim_logging::embed::user(user.id.get()))
        .var("member.username", &user.name)
        .var("member.id", &user.id.to_string())
        .var("panel", panel.unwrap_or_default())
        .var("ticket_type", kind.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_is_hidden_from_everyone_but_bot_and_staff() {
        let o = overwrites(Id::new(9), 2, &[3]);
        assert_eq!(o[0].id.get(), 9);
        assert_eq!(o[0].deny, Permissions::VIEW_CHANNEL);
        assert_eq!(
            o.iter()
                .filter(|w| w.allow.contains(Permissions::VIEW_CHANNEL))
                .map(|w| w.id.get())
                .collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn every_documented_variable_is_filled_in() {
        let u: User = serde_json::from_value(
            json!({"id": "5", "username": "tom", "discriminator": "0", "avatar": null}),
        )
        .unwrap();
        let all: String = TAGS.iter().map(|(k, _)| *k).collect();
        assert_eq!(tags(&all, &u, "Report"), "tom5<@5>Report");
        assert_eq!(tags("[{ticket_type}]", &u, ""), "[]");
    }

    #[test]
    fn the_ticket_type_goes_between_the_name_and_the_number() {
        assert_eq!(
            kind_name("tchicx-41226", None, "Report"),
            "tchicx-report-41226"
        );
        assert_eq!(
            kind_name("tchicx-report-41226", Some("Report"), "Ban Appeal!"),
            "tchicx-ban-appeal-41226"
        );
        assert_eq!(kind_name("tchicx-41226", None, "???"), "tchicx-41226");
        assert_eq!(kind_name("nonumber", None, "Report"), "nonumber");
        let long = kind_name(&format!("{}-00042", "a".repeat(90)), None, "Report");
        assert_eq!(long.chars().count(), 100);
        assert!(long.ends_with("-report-00042"));
    }

    #[test]
    fn channel_names_drop_punctuation_and_fall_back_to_the_id() {
        assert_eq!(channel_name("Tom.Bot!", Id::new(5), 42), "tombot-00042");
        assert_eq!(channel_name("...", Id::new(5), 123_456), "5-23456");
    }

    /// Parallel opens of a two-slot panel give two tickets, and a purge leaves nothing behind.
    #[tokio::test]
    async fn panel_slots_hold_under_parallel_opens_and_purge_clears_all() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let g = 4_300_000_000_000_000_000 + std::process::id() as u64;
        let gid = Id::new(g);
        let mut opens = tokio::task::JoinSet::new();
        for _ in 0..6 {
            let pool = pool.clone();
            opens.spawn(
                async move { claim_number(&pool, gid, Id::new(1), Some(("help", 2))).await },
            );
        }
        let won = opens
            .join_all()
            .await
            .iter()
            .filter(|c| matches!(c, Ok(Claim::Number(_))))
            .count();
        assert_eq!(won, 2);
        for sql in [
            "INSERT INTO ticket_events (guild_id, number, action) VALUES ($1, 1, 'opened')",
            "INSERT INTO ticket_transcripts (guild_id, number, log_channel_id, log_message_id, complete) VALUES ($1, 1, 1, $1, true)",
            "INSERT INTO ticket_panel_posts (message_id, guild_id, channel_id, panels, posted_by) VALUES ($1, $1, 1, '{help}', 1)",
        ] {
            sqlx::query(sql).bind(g as i64).execute(&pool).await.unwrap();
        }
        brim_core::db::purge_guild(&pool, g as i64).await.unwrap();
        for t in [
            "tickets",
            "ticket_events",
            "ticket_transcripts",
            "ticket_panel_posts",
        ] {
            let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {t} WHERE guild_id = $1"
            )))
            .bind(g as i64)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 0, "{t}");
        }
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn one_open_ticket_per_member_and_enabled_servers_are_found() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let g = 4_100_000_000_000_000_000 + std::process::id() as u64;
        let gid = Id::new(g);
        let Claim::Number(first) = claim_number(&pool, gid, Id::new(1), None).await.unwrap() else {
            panic!("no number")
        };
        assert_eq!(
            claim_number(&pool, gid, Id::new(1), None).await.unwrap(),
            Claim::Open
        );
        assert_eq!(
            claim_number(&pool, gid, Id::new(2), None).await.unwrap(),
            Claim::Number(first + 1)
        );
        // A panel with two slots: the member's DM ticket does not count, a third is refused.
        for n in 2..=3 {
            assert_eq!(
                claim_number(&pool, gid, Id::new(1), Some(("help", 2)))
                    .await
                    .unwrap(),
                Claim::Number(first + n)
            );
        }
        // The hourly cap (3 per member) comes first now.
        assert_eq!(
            claim_number(&pool, gid, Id::new(1), Some(("help", 2)))
                .await
                .unwrap(),
            Claim::UserCap
        );
        sqlx::query(
            "UPDATE tickets SET status = 'open', opened_at = now() - interval '2 hours' WHERE guild_id = $1",
        )
        .bind(g as i64)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            claim_number(&pool, gid, Id::new(1), Some(("help", 2)))
                .await
                .unwrap(),
            Claim::Open
        );
        for (v, on) in [(1, false), (2, true)] {
            sqlx::query("INSERT INTO guild_config (guild_id, version, config) VALUES ($1, $2, $3)")
                .bind(g as i64)
                .bind(v)
                .bind(json!({"tickets": {"enabled": on}}))
                .execute(&pool)
                .await
                .unwrap();
        }
        assert!(ticket_guilds(&pool).await.unwrap().contains(&(g as i64)));
        sqlx::query("DELETE FROM tickets WHERE guild_id = $1")
            .bind(g as i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM guild_config WHERE guild_id = $1")
            .bind(g as i64)
            .execute(&pool)
            .await
            .unwrap();
    }
}

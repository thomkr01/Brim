//! Message context menus: Report Message (anyone) and the two purges (staff). A report is logged with
//! action buttons; the staff who press them need the Manage Reports permit plus the permit of the action.
use anyhow::Result;
use brim_logging::emoji::{Emoji, CLOSE, LINK, PAGE_FIRST, PAGE_LAST, PAGE_NEXT, PAGE_PREV};
use brim_logging::emoji::{AGE, MEMBER, NO, YES};
use brim_logging::{
    embed::{BLUE, HANDLED, ORANGE},
    Log, LogKind,
};
use twilight_model::{
    application::interaction::{application_command::CommandData, Interaction, InteractionData},
    channel::{
        message::{
            component::{ActionRow, Button, ButtonStyle, Component, TextInput, TextInputStyle},
            embed::EmbedField,
            AllowedMentions, MessageFlags,
        },
        Message,
    },
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    id::{
        marker::{AutoModerationRuleMarker, ChannelMarker, GuildMarker, UserMarker},
        Id,
    },
};

use crate::{
    app::App,
    commands::{self, ModKind},
    modcmds::{Anchor, ModCmd},
    reply::{self, Said},
};

/// One member cannot flood the report channel.
const REPORTS_PER_HOUR: i64 = 10;
const SNIPPET: usize = 300;
const ACTION_LOG: &str = "Action Log";

/// The purge a message context menu asks for; `Err` is the reply text.
pub fn message_menu(data: &CommandData) -> Result<ModCmd, String> {
    let msg = data
        .target_id
        .zip(data.resolved.as_ref())
        .and_then(|(t, r)| r.messages.get(&t.cast()));
    let Some(msg) = msg else {
        return Err("Message not found".into());
    };
    let from = match data.name.as_str() {
        "Purge 100 From Here" => Anchor::From(msg.id.get()),
        "Purge 100 Until Here" => Anchor::Until(msg.id.get()),
        _ => return Err(crate::reply::UNKNOWN_COMMAND.into()),
    };
    Ok(ModCmd::Purge {
        count: 100,
        user: None,
        from: Some(from),
        filters: vec![],
        own: vec![],
        before: None,
    })
}

fn button(label: &str, id: i64, action: &str, style: ButtonStyle) -> Component {
    Component::Button(Button {
        id: None,
        custom_id: Some(format!("report:{id}:{action}")),
        disabled: false,
        emoji: None,
        label: Some(label.into()),
        style,
        url: None,
        sku_id: None,
    })
}

fn rows(id: i64, jump: String, deletable: bool) -> Vec<Component> {
    let row = |components| {
        Component::ActionRow(ActionRow {
            id: None,
            components,
        })
    };
    let link = Component::Button(LINK.on(Button {
        id: None,
        custom_id: None,
        disabled: false,
        emoji: None,
        label: Some("View Message ↗".into()),
        style: ButtonStyle::Link,
        url: Some(jump),
        sku_id: None,
    }));
    vec![
        row(std::iter::once(link)
            .chain(deletable.then(|| button("Delete Message", id, "delete", ButtonStyle::Success)))
            .chain([button("Dismiss Report", id, "dismiss", ButtonStyle::Danger)])
            .collect()),
        row(vec![
            button("User Info", id, "cases", ButtonStyle::Secondary),
            button("Warn", id, "warn", ButtonStyle::Secondary),
            button("Timeout", id, "timeout", ButtonStyle::Secondary),
            button("Kick", id, "kick", ButtonStyle::Secondary),
            button("Ban", id, "ban", ButtonStyle::Secondary),
        ]),
    ]
}

async fn report(
    app: &App,
    gid: Id<GuildMarker>,
    reporter: Id<UserMarker>,
    msg: &Message,
    reporter_name: &str,
    reason: &str,
) -> Result<Option<Said>> {
    let g = gid.get() as i64;
    let no = |key| async move { Ok(Some(reply::say(app, gid, key, &[]).await)) };
    let Some(log) = app
        .cfg(gid.get())
        .await?
        .logging
        .target(LogKind::Report, None)
    else {
        return no("reply.report.not_set_up").await;
    };
    if msg.author.bot {
        return no("reply.report.bot_message").await;
    }
    if msg.author.id == reporter {
        return no("reply.report.own_message").await;
    }
    let banned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM report_blacklist WHERE guild_id=$1 AND user_id=$2)",
    )
    .bind(g)
    .bind(reporter.get() as i64)
    .fetch_one(&app.pool)
    .await?;
    if banned {
        return no("reply.report.blacklisted").await;
    }
    if app.whitelisted(gid, msg.author.id, "reports").await? {
        return no("reply.report.protected").await;
    }
    // Count and insert under one lock, or parallel reports all see room and pass the hourly cap.
    let mut tx = app.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('reports:' || $1 || ':' || $2, 0))")
        .bind(g)
        .bind(reporter.get() as i64)
        .execute(&mut *tx)
        .await?;
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reports WHERE guild_id=$1 AND reporter_id=$2 AND created_at > now() - interval '1 hour'",
    )
    .bind(g)
    .bind(reporter.get() as i64)
    .fetch_one(&mut *tx)
    .await?;
    if recent >= REPORTS_PER_HOUR {
        return no("reply.report.too_many").await;
    }
    let content: String = msg.content.chars().take(SNIPPET).collect();
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, message_id, content, reason)
         VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(g)
    .bind(reporter.get() as i64)
    .bind(msg.author.id.get() as i64)
    .bind(msg.channel_id.get() as i64)
    .bind(msg.id.get() as i64)
    .bind(&content)
    .bind(reason)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let Some(id) = id else {
        return no("reply.report.already").await;
    };
    let jump = format!(
        "https://discord.com/channels/{gid}/{}/{}",
        msg.channel_id, msg.id
    );
    let built = reported_log(
        &msg.author,
        (reporter, reporter_name),
        &jump,
        &content,
        reason,
    );
    let embed = crate::events::framed_log(app, gid, "log.report", built).await;
    let sent = app
        .http
        .create_message(log)
        .embeds(&[embed])
        .components(&rows(id, jump, true))
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await;
    if let Err(e) = sent {
        sqlx::query("DELETE FROM reports WHERE id=$1")
            .bind(id)
            .execute(&app.pool)
            .await?;
        return Err(e.into());
    }
    Ok(None)
}

/// Files a report for a message Discord's AutoMod already blocked, with the same action buttons
/// (minus Delete, the message is gone). `false` when this server has no report channel.
pub async fn file_native(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Option<Id<ChannelMarker>>,
    target: Id<UserMarker>,
    content: &str,
    rule: Id<AutoModerationRuleMarker>,
) -> Result<bool> {
    let Some(log) = app
        .cfg(gid.get())
        .await?
        .logging
        .target(LogKind::Report, None)
    else {
        return Ok(false);
    };
    let content: String = content.chars().take(SNIPPET).collect();
    // The blocked message has no id; milliseconds and 20 random bits keep the (guild, reporter,
    // message) key unique, also for a burst of blocks in one second.
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, message_id, content, reason)
         VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
    )
    .bind(gid.get() as i64)
    .bind(app.me.get() as i64)
    .bind(target.get() as i64)
    .bind(channel.map_or(0, |c| c.get() as i64))
    .bind((crate::automod::now_ms() << 20) | (rand::random::<u32>() & 0xF_FFFF) as i64)
    .bind(&content)
    .bind(format!("Discord AutoMod rule {rule}"))
    .fetch_one(&app.pool)
    .await?;
    let jump = format!(
        "https://discord.com/channels/{gid}/{}",
        channel.map_or(String::new(), |c| c.to_string())
    );
    let built = native_log(target, rule, &content);
    let embed = crate::events::framed_log(app, gid, "log.report", built).await;
    let sent = app
        .http
        .create_message(log)
        .embeds(&[embed])
        .components(&rows(id, jump, false))
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await;
    if let Err(e) = sent {
        sqlx::query("DELETE FROM reports WHERE id=$1")
            .bind(id)
            .execute(&app.pool)
            .await?;
        return Err(e.into());
    }
    Ok(true)
}

/// A stored excerpt as the report log shows it: in a spoiler, or "(no text)".
fn excerpt(content: &str) -> String {
    if content.is_empty() {
        "(no text)".into()
    } else {
        format!("||{}||", content.replace('|', "|\u{200b}"))
    }
}

/// The report log of a message `author` wrote, reported by `reporter`.
pub(crate) fn reported_log(
    author: &twilight_model::user::User,
    (reporter, reporter_name): (Id<UserMarker>, &str),
    jump: &str,
    content: &str,
    reason: &str,
) -> Log {
    let created = ((author.id.get() >> 22) + 1_420_070_400_000) / 1000;
    let (member, by) = (format!("<@{}>", author.id), format!("<@{reporter}>"));
    let created = format!("<t:{created}:F>");
    let excerpt = excerpt(content);
    Log::new("", ORANGE)
        .entry("log.report.message_reported")
        .author(
            author,
            &format!("A message by {} has been reported!", author.name),
        )
        .thumbnail(author)
        .text(&format!(
            "{member}'s [message]({jump}) has been reported by {by}"
        ))
        .text(&format!(
            "{MEMBER} **Member:** {} [{}]",
            author.name, author.id
        ))
        .text(&format!("{AGE} **Creation:** {created}"))
        .time()
        .field("Reported Message:", &excerpt)
        .field("Reason:", reason)
        .footer(&format!(
            "Reported by {reporter_name} | TARGET: {}",
            author.id
        ))
        .var("member", &member)
        .var("member.username", &author.name)
        .var("member.id", &author.id.to_string())
        .var("member.created_at", &created)
        .var("message.link", jump)
        .var("by", &by)
        .var("reporter", reporter_name)
        .var("content", &excerpt)
        .var("reason", reason)
}

/// The report log of a message Discord's AutoMod rule `rule` blocked.
pub(crate) fn native_log(
    target: Id<UserMarker>,
    rule: Id<AutoModerationRuleMarker>,
    content: &str,
) -> Log {
    let excerpt = excerpt(content);
    Log::new("Discord AutoMod Report", ORANGE)
        .text(&format!(
            "<@{target}>'s message was blocked by Discord AutoMod rule `{rule}`"
        ))
        .time()
        .field("Blocked Message:", &excerpt)
        .footer(&format!("Reported by Brim | TARGET: {target}"))
        .var("member", &format!("<@{target}>"))
        .var("member.id", &target.to_string())
        .var("rule", &format!("`{rule}`"))
        .var("content", &excerpt)
}

const MODAL: &str = "reportm:";

/// Report Message asks for a reason first ("Report Message" modal).
#[allow(deprecated)]
async fn open_modal(app: &App, i: &Interaction, data: &CommandData) -> Result<()> {
    let msg = data
        .target_id
        .zip(data.resolved.as_ref())
        .and_then(|(t, r)| r.messages.get(&t.cast()));
    let Some(msg) = msg else { return Ok(()) };
    let input = Component::TextInput(TextInput {
        id: None,
        custom_id: "reason".into(),
        label: Some("Reason".into()),
        max_length: Some(1000),
        min_length: Some(1),
        placeholder: None,
        required: Some(true),
        style: TextInputStyle::Paragraph,
        value: None,
    });
    let resp = InteractionResponse {
        kind: InteractionResponseType::Modal,
        data: Some(InteractionResponseData {
            custom_id: Some(format!("{MODAL}{}:{}", msg.channel_id, msg.id)),
            title: Some("Report Message".into()),
            components: Some(vec![Component::ActionRow(ActionRow {
                id: None,
                components: vec![input],
            })]),
            ..Default::default()
        }),
    };
    app.http
        .interaction(Id::new(app.me.get()))
        .create_response(i.id, &i.token, &resp)
        .await?;
    Ok(())
}

async fn submit_report(app: &App, i: &Interaction, ids: &str, reason: &str) -> Result<()> {
    let client = app.http.interaction(Id::new(app.me.get()));
    let defer = InteractionResponse {
        kind: InteractionResponseType::DeferredChannelMessageWithSource,
        data: Some(InteractionResponseData {
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    };
    client.create_response(i.id, &i.token, &defer).await?;
    let ids = ids
        .split_once(':')
        .and_then(|(c, m)| Some((c.parse::<u64>().ok()?, m.parse::<u64>().ok()?)));
    let (Some(gid), Some(reporter), Some((c, m))) = (i.guild_id, i.author(), ids) else {
        return Ok(());
    };
    let refusal = match app.http.message(Id::new(c), Id::new(m)).await {
        Ok(r) => {
            let msg = r.model().await?;
            report(app, gid, reporter.id, &msg, &reporter.name, reason).await?
        }
        Err(_) => Some(reply::say(app, gid, "reply.report.not_found", &[]).await),
    };
    let said = match refusal {
        Some(no) => no,
        None => reply::say(app, gid, "reply.report.sent", &[]).await,
    };
    let text = (!said.text.is_empty()).then_some(said.text.as_str());
    let up = client.update_response(&i.token).content(text);
    up.embeds(Some(&said.embeds)).await?;
    Ok(())
}

/// The pressed button's effect: the private answer and the history line it adds to the report.
struct Outcome {
    text: Said,
    /// `(line, closes the report)`
    line: Option<(String, bool)>,
}

fn refused(text: impl Into<Said>) -> Outcome {
    Outcome {
        text: text.into(),
        line: None,
    }
}

async fn act(
    app: &App,
    gid: Id<GuildMarker>,
    actor_id: Id<UserMarker>,
    id: i64,
    action: &str,
) -> Result<Outcome> {
    let owner = app.guild_info(gid).await?.owner;
    let actor = app.level(gid, owner, actor_id).await?;
    let no = |key| async move { Ok(refused(reply::say(app, gid, key, &[]).await)) };
    if !actor.has("reports") {
        return no("reply.report.no_permit").await;
    }
    if let Some(why) = crate::commands::gated(app, gid, actor_id, &actor, "reports").await? {
        return Ok(refused(why));
    }
    let row: Option<(i64, i64, i64, String, i64)> = sqlx::query_as(
        "SELECT target_id, channel_id, message_id, content, reporter_id FROM reports WHERE id=$1 AND guild_id=$2",
    )
    .bind(id)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some((target, channel, message, content, reporter)) = row else {
        return no("reply.report.gone").await;
    };
    let target = Id::new(target as u64);
    let who = (actor_id, &actor);
    let reason = format!("Report #{id}");
    let (kind, name) = match action {
        "dismiss" => {
            return Ok(Outcome {
                text: reply::say(app, gid, "reply.report.dismissed", &[]).await,
                line: Some((
                    format!(
                        "- **Report was dismissed by** <@{actor_id}> <t:{}:R>",
                        crate::app::now_secs()
                    ),
                    true,
                )),
            })
        }
        "delete" => {
            // A reported log message stays: nobody deletes in a log channel (owner decision 11-15).
            if app
                .cfg(gid.get())
                .await?
                .log_channels()
                .contains(&Id::new(channel as u64))
            {
                return no("reply.report.log_channel").await;
            }
            // The same immunity as `purge` of one member: never self, a permit holder or an immune member.
            let level = app.level(gid, owner, target).await?;
            let check = brim_core::permits::check(
                &actor,
                "reports",
                actor_id.get() as i64,
                &level,
                target.get() as i64,
                false,
            );
            if let Err(d) = check {
                let what = "delete the message of";
                return Ok(refused(crate::commands::deny_text(
                    what,
                    target.get(),
                    &level,
                    &d,
                )));
            }
            let gone = app
                .http
                .delete_message(Id::new(channel as u64), Id::new(message as u64))
                .await
                .map_err(anyhow::Error::from);
            return Ok(match gone {
                Ok(_) => Outcome {
                    text: reply::say(app, gid, "reply.report.deleted", &[]).await,
                    line: Some((
                        format!(
                            "- Message reported has been deleted by <@{actor_id}> <t:{}:R>",
                            crate::app::now_secs()
                        ),
                        true,
                    )),
                },
                Err(e) if crate::http_err::gone_any(&e) => {
                    refused(reply::say(app, gid, "reply.report.already_gone", &[]).await)
                }
                Err(e) => {
                    let why = crate::http_err::why(&e);
                    let key = "reply.report.delete_failed";
                    refused(reply::say(app, gid, key, &[("error", &why)]).await)
                }
            });
        }
        "warn" => (ModKind::Warn, "Warn"),
        "timeout" => (ModKind::Timeout, "Timeout"),
        "kick" => (ModKind::Kick, "Kick"),
        "ban" => (ModKind::Ban, "Ban"),
        _ => return no("reply.report.unknown_action").await,
    };
    // The button is the command: the server's settings for it count here too.
    let cmd = match kind {
        ModKind::Warn => "warn",
        ModKind::Timeout => "timeout",
        ModKind::Kick => "kick",
        _ => "ban",
    };
    if let Some(no) = crate::cmdcfg::button(app, gid, actor_id, &actor, cmd).await? {
        return Ok(refused(no));
    }
    let attempted = |text: Said| Outcome {
        line: Some((
            format!(
                "- <@{actor_id}> attempted to **{name}** <t:{}:R>",
                crate::app::now_secs()
            ),
            false,
        )),
        text,
    };
    if kind == ModKind::Warn && app.http.ban(gid, target).await.is_ok() {
        let said = reply::say(app, gid, "reply.report.already_banned", &[]).await;
        return Ok(attempted(said));
    }
    let points = app.cfg(gid.get()).await?.misc.warn_base_points;
    let done = commands::apply(
        app,
        gid,
        owner,
        who,
        kind,
        target,
        Some(&reason),
        None,
        points,
    )
    .await?;
    // A refusal never opens a case.
    let Some(case_no) = done.case else {
        return Ok(attempted(done.text.into()));
    };
    // The reported message (its stored excerpt) is the case's verified proof. A message Discord
    // AutoMod blocked has no id: the bot filed that report with a made-up one (`file_native`).
    let copy = brim_core::cases::MessageCopy {
        channel_id: channel,
        message_id: (reporter as u64 != app.me.get()).then_some(message),
        text: &content,
        files: vec![],
    };
    crate::proof::verified(app, gid, case_no, &copy).await;
    Ok(Outcome {
        text: done.text.into(),
        line: Some((
            format!(
                "- <@{actor_id}> issued a **{name}** <t:{}:R>",
                crate::app::now_secs()
            ),
            true,
        )),
    })
}

const PER_PAGE: i64 = 8;

/// The page to show (clamped) and the page count for `total` cases.
fn paging(page: i64, total: i64) -> (i64, i64) {
    let pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
    (page.clamp(0, pages - 1), pages)
}

/// "User Info": an overview embed and the member's cases, paged with the page buttons. `arg` is empty for
/// the first press (needs the Manage Reports permit) or `page:slot` / `x` from the pager.
async fn user_info(
    app: &App,
    i: &Interaction,
    gid: Id<GuildMarker>,
    actor_id: Id<UserMarker>,
    id: i64,
    arg: &str,
) -> Result<()> {
    let client = app.http.interaction(Id::new(app.me.get()));
    if arg == "x" {
        client.delete_response(&i.token).await?;
        return Ok(());
    }
    let first = arg.is_empty();
    if first {
        let owner = app.guild_info(gid).await?.owner;
        let level = app.level(gid, owner, actor_id).await?;
        let refused = if !level.has("reports") {
            Some(reply::say(app, gid, "reply.report.no_permit", &[]).await)
        } else {
            // The same panic lock and rate limit as the other report buttons.
            let gated = crate::commands::gated(app, gid, actor_id, &level, "reports").await?;
            gated.map(Said::from)
        };
        if let Some(why) = refused {
            return tell(app, &i.token, &why).await;
        }
    }
    let page: i64 = arg
        .split(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let target: i64 =
        sqlx::query_scalar("SELECT target_id FROM reports WHERE id=$1 AND guild_id=$2")
            .bind(id)
            .bind(gid.get() as i64)
            .fetch_one(&app.pool)
            .await?;
    let user = app.http.user(Id::new(target as u64)).await?.model().await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM cases WHERE guild_id=$1 AND user_id=$2")
            .bind(gid.get() as i64)
            .bind(target)
            .fetch_one(&app.pool)
            .await?;
    let (page, pages) = paging(page, total);
    let rows: Vec<(i32, String, bool, i64)> = sqlx::query_as(
        "SELECT case_no, kind, active, extract(epoch from created_at)::bigint FROM cases
         WHERE guild_id=$1 AND user_id=$2 ORDER BY case_no DESC LIMIT $3 OFFSET $4",
    )
    .bind(gid.get() as i64)
    .bind(target)
    .bind(PER_PAGE)
    .bind(page * PER_PAGE)
    .fetch_all(&app.pool)
    .await?;
    let created = ((user.id.get() >> 22) + 1_420_070_400_000) / 1000;
    let badges = user
        .public_flags
        .filter(|f| !f.is_empty())
        .map_or("None".into(), |f| format!("{f:?}"));
    let overview = Log::new(&format!("{} Overview:", user.name), BLUE)
        .thumbnail(&user)
        .line("Username", &user.name)
        .line("Tag", &format!("{}#{:04}", user.name, user.discriminator))
        .line("ID", &format!("`{}`", user.id))
        .line("Creation", &format!("<t:{created}:F>"))
        .line("Badges", &badges)
        .build();
    let list: Vec<String> = rows
        .iter()
        .map(|(n, k, active, at)| {
            format!(
                "{} {n} [+{}] <t:{at}:R>",
                if *active { YES } else { NO },
                k.to_uppercase()
            )
        })
        .collect();
    let cases = Log::new(&format!("{} Mod Cases ({total}):", user.name), BLUE)
        .text(&if list.is_empty() {
            "No cases".into()
        } else {
            list.join("\n")
        })
        .footer(&format!("Page {}/{pages}", page + 1))
        .build();
    let embeds = [overview, cases];
    let pager = pager(id, page, pages);
    if first {
        client
            .create_followup(&i.token)
            .embeds(&embeds)
            .components(&[pager])
            .flags(MessageFlags::EPHEMERAL)
            .await?;
    } else {
        client
            .update_response(&i.token)
            .embeds(Some(&embeds))
            .components(Some(&[pager]))
            .await?;
    }
    Ok(())
}

fn pager(id: i64, page: i64, pages: i64) -> Component {
    let last = pages - 1;
    let b = |icon: Emoji, to: i64, slot: &str, style, off| match button(
        icon.fallback,
        id,
        &format!("cases:{to}:{slot}"),
        style,
    ) {
        Component::Button(b) => Component::Button(icon.on(Button { disabled: off, ..b })),
        c => c,
    };
    let grey = ButtonStyle::Secondary;
    Component::ActionRow(ActionRow {
        id: None,
        components: vec![
            b(PAGE_FIRST, 0, "f", grey, page == 0),
            b(PAGE_PREV, (page - 1).max(0), "p", grey, page == 0),
            b(PAGE_NEXT, (page + 1).min(last), "n", grey, page >= last),
            b(PAGE_LAST, last, "l", grey, page >= last),
            b(CLOSE, 0, "x", ButtonStyle::Danger, false),
        ],
    })
}

/// Reports and cases a button is acting on now, so two presses at once do not both act (one bot
/// process per database). The key is (0, report) or (server, case). Freed on drop, also on an
/// error or a panic.
type Claims = std::sync::Mutex<std::collections::HashSet<(u64, i64)>>;
static ACTING: std::sync::LazyLock<Claims> = std::sync::LazyLock::new(Default::default);

pub(crate) struct Acting((u64, i64));

impl Acting {
    pub(crate) fn claim(key: (u64, i64)) -> Option<Self> {
        // Lazy: a guard made for a lost claim would remove the winner's entry on drop.
        let won = ACTING.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
        won.then(|| Self(key))
    }
}

impl Drop for Acting {
    fn drop(&mut self) {
        ACTING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Handles the report command's modal and the buttons of a report log message; `false` when the
/// interaction is not one of them.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let c = match &i.data {
        Some(InteractionData::ApplicationCommand(d)) if d.name == "Report Message" => {
            open_modal(app, i, d).await?;
            return Ok(true);
        }
        Some(InteractionData::ModalSubmit(m)) if m.custom_id.starts_with(MODAL) => {
            let reason = m
                .components
                .iter()
                .find_map(crate::verify::typed_text)
                .unwrap_or_default();
            submit_report(app, i, &m.custom_id[MODAL.len()..], reason.trim()).await?;
            return Ok(true);
        }
        Some(InteractionData::MessageComponent(c)) => c,
        _ => return Ok(false),
    };
    let parsed = c
        .custom_id
        .strip_prefix("report:")
        .and_then(|r| r.split_once(':'));
    let Some((id, action)) = parsed.and_then(|(id, a)| Some((id.parse::<i64>().ok()?, a))) else {
        return Ok(false);
    };
    let (Some(gid), Some(actor_id)) = (i.guild_id, i.author_id()) else {
        return Ok(true);
    };
    let client = app.http.interaction(Id::new(app.me.get()));
    let defer = InteractionResponse {
        kind: InteractionResponseType::DeferredUpdateMessage,
        data: None,
    };
    client.create_response(i.id, &i.token, &defer).await?;
    if let Some(arg) = action.strip_prefix("cases") {
        user_info(app, i, gid, actor_id, id, arg.trim_start_matches(':')).await?;
        return Ok(true);
    }
    let Some(_acting) = Acting::claim((0, id)) else {
        tell(
            app,
            &i.token,
            &reply::say(app, gid, "reply.report.busy", &[]).await,
        )
        .await?;
        return Ok(true);
    };
    let Outcome { text, line } = match act(app, gid, actor_id, id, action).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(?e, action, "report action failed");
            let why = crate::http_err::why(&e);
            refused(reply::say(app, gid, "reply.report.error", &[("error", &why)]).await)
        }
    };
    if let Some((line, closes)) = line {
        if closes {
            let status = if action == "dismiss" {
                "dismissed"
            } else {
                "actioned"
            };
            sqlx::query(
                // The first moderator who closed it stays recorded; a later Ban or Kick only adds a line.
                "UPDATE reports SET status=$2, action=$3, handled_by=$4, handled_at=now() WHERE id=$1 AND status='open' AND guild_id=$5",
            )
            .bind(id)
            .bind(status)
            .bind(action)
            .bind(actor_id.get() as i64)
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await?;
        }
        // It keeps the report's history in an "Action Log" field and turns the log green.
        if let Some(mut embed) = i.message.as_ref().and_then(|m| m.embeds.first().cloned()) {
            match embed.fields.iter_mut().find(|f| f.name == ACTION_LOG) {
                // A field holds 1024 characters; a very long history just stops growing.
                Some(f) if f.value.len() + line.len() < 1024 => {
                    f.value = format!("{}\n{line}", f.value)
                }
                Some(_) => {}
                None => embed.fields.push(EmbedField {
                    inline: false,
                    name: ACTION_LOG.into(),
                    value: line,
                }),
            }
            if closes {
                embed.color = Some(HANDLED);
            }
            let embeds = [embed];
            let u = client.update_response(&i.token).embeds(Some(&embeds));
            if matches!(action, "dismiss" | "delete") {
                u.components(Some(&[])).await?;
            } else {
                u.await?;
            }
        }
    }
    tell(app, &i.token, &text).await?;
    Ok(true)
}

/// A private answer to the presser: the entry's text and embeds.
async fn tell(app: &App, token: &str, said: &Said) -> Result<()> {
    let client = app.http.interaction(Id::new(app.me.get()));
    let mut followup = client.create_followup(token);
    if !said.text.is_empty() {
        followup = followup.content(&said.text);
    }
    followup
        .embeds(&said.embeds)
        .flags(MessageFlags::EPHEMERAL)
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_press_at_a_time_per_report() {
        let first = super::Acting::claim((0, -77)).unwrap();
        assert!(super::Acting::claim((0, -77)).is_none());
        drop(first);
        assert!(super::Acting::claim((0, -77)).is_some());
    }

    #[test]
    fn paging_clamps_and_counts() {
        assert_eq!(super::paging(5, 0), (0, 1));
        assert_eq!(super::paging(9, 17), (2, 3));
        assert_eq!(super::paging(1, 16), (1, 2));
    }
}

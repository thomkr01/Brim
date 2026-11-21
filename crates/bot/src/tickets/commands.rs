use super::staff::{close, relay};
use super::*;

pub(super) const STAFF_CMDS: &[&str] = &[
    "reply",
    "r",
    "areply",
    "ar",
    "close",
    "c",
    "aclose",
    "ac",
    "closeall",
    "acloseall",
    "blacklist",
    "block",
    "whitelist",
    "unblock",
    "blacklistclear",
    "viewblacklist",
    "snippet",
    "s",
    "asnippet",
    "as",
    "snippetadd",
    "snippetremove",
    "snippetclear",
    "viewsnippet",
    "viewsnippets",
    "snippetlist",
    "vs",
    "claim",
    "unclaim",
    "reassign",
    "priority",
    "tickettype",
    "ticketnote",
    "tnote",
    "ticketinfo",
];
pub(super) const ADMIN_CMDS: &[&str] = &[
    "ticketsetup",
    "category",
    "pingrole",
    "mentionrole",
    "logging",
    "logs",
    "commandonly",
    "commandrequired",
    "greetingmessage",
    "welcomemessage",
    "greetmessage",
    "closingmessage",
    "goodbyemessage",
    "closemessage",
    "loggingplus",
    "advancedlogging",
    "advancedlogs",
    "anonymous",
    "toggle",
    "enable",
    "disable",
    "viewconfig",
];
/// Short names other commands already use; they mean the ticket command only inside a ticket.
const TICKET_ONLY: &[&str] = &[
    "r",
    "c",
    "s",
    "whitelist",
    "claim",
    "unclaim",
    "reassign",
    "priority",
    "tickettype",
    "ticketnote",
    "tnote",
    "ticketinfo",
];

/// The ticket-only commands `staff_work` runs.
const WORKFLOW: &[&str] = &[
    "claim",
    "unclaim",
    "reassign",
    "priority",
    "tickettype",
    "ticketnote",
    "tnote",
    "ticketinfo",
];

/// Staff talk in a ticket is relayed to the member; ticket commands run anywhere. True when handled.
pub async fn on_message(
    app: &App,
    m: &MessageCreate,
    gid: Id<GuildMarker>,
    rest: Option<&str>,
) -> Result<bool> {
    let cfg = app.cfg(gid.get()).await?;
    let word = rest.map(|r| {
        let r = r.trim_start();
        let (w, a) = r.split_once(char::is_whitespace).unwrap_or((r, ""));
        (w.to_lowercase(), a.trim())
    });
    let Some((word, args)) = word else {
        // Plain message: relayed when in a DM ticket, unless it starts with a bot mention.
        if m.content.trim().is_empty() && m.attachments.is_empty() {
            return Ok(false);
        }
        let Some(here) = ticket_here(app, &cfg.tickets, gid, m.channel_id).await? else {
            return Ok(false);
        };
        if here.panel.is_some() {
            // The member reads the channel: nothing is relayed, the message restarts the clock.
            let member = here.user == m.author.id;
            let staff = !member && allowed(app, gid, m, false).await?;
            if member || staff {
                touch(app, gid, m.channel_id, staff).await?;
            }
            return Ok(false);
        }
        let me = [format!("<@{}>", app.me), format!("<@!{}>", app.me)];
        if cfg.tickets.command_only || me.iter().any(|p| m.content.starts_with(p.as_str())) {
            return Ok(false);
        }
        let user = here.user;
        // T-01: seeing the channel (a wrong overwrite, a manual edit) does not make one staff.
        if !allowed(app, gid, m, false).await? {
            say_key(app, gid, m.channel_id, "reply.ticket.no_reply_access", &[]).await?;
            return Ok(true);
        }
        relay(app, m, gid, user, &m.content, cfg.tickets.anonymous, false).await?;
        return Ok(true);
    };
    let w = word.as_str();
    let admin = ADMIN_CMDS.contains(&w);
    if !admin && !STAFF_CMDS.contains(&w) {
        return Ok(false);
    }
    let ticket = ticket_here(app, &cfg.tickets, gid, m.channel_id).await?;
    if TICKET_ONLY.contains(&w) && ticket.is_none() {
        return Ok(false);
    }
    let ch = m.channel_id;
    // The same silence rules as every other command: quarantined members and voice-channel chats.
    if crate::commands::ignored(app, m, gid, rest.unwrap_or_default()).await? {
        return Ok(true);
    }
    let owner = app.guild_info(gid).await?.owner;
    let level = app.level(gid, owner, m.author.id).await?;
    if !may(&level, admin) {
        let none = level == permits::Level::None;
        if none && (cfg.misc.ignore_no_permit || !app.cooled_down(gid.get(), m.author.id.get())) {
            return Ok(true);
        }
        let key = if admin {
            "reply.ticket.admin_only"
        } else {
            "reply.ticket.staff_only_command"
        };
        let command = format!("{}{w}", cfg.misc.prefix);
        say_key(app, gid, ch, key, &[("command", &command)]).await?;
        return Ok(true);
    }
    let pfx = cfg.misc.prefix.clone();
    let t = &cfg.tickets;
    if w != "ticketsetup" && w != "category" && t.category.is_none() {
        say_key(app, gid, ch, "reply.ticket.not_set_up", &[("prefix", &pfx)]).await?;
        return Ok(true);
    }
    let needs_ticket = matches!(
        w,
        "reply"
            | "r"
            | "areply"
            | "ar"
            | "close"
            | "c"
            | "aclose"
            | "ac"
            | "snippet"
            | "s"
            | "asnippet"
            | "as"
    );
    if needs_ticket && ticket.is_none() {
        say_key(app, gid, ch, "reply.ticket.not_ticket", &[]).await?;
        return Ok(true);
    }
    let relays = matches!(
        w,
        "reply" | "r" | "areply" | "ar" | "snippet" | "s" | "asnippet" | "as"
    );
    if relays && ticket.as_ref().is_some_and(|h| h.panel.is_some()) {
        say_key(app, gid, ch, "reply.ticket.panel_write_here", &[]).await?;
        return Ok(true);
    }
    if let Some(here) = ticket.as_ref().filter(|_| WORKFLOW.contains(&w)) {
        return staff_work(app, m, gid, &cfg, here, w, args)
            .await
            .map(|_| true);
    }
    run(app, m, gid, &cfg, ticket.map(|h| h.user), w, args).await?;
    Ok(true)
}

/// Ticket admin commands need Owner, Extra Owner or Trusted Admin; staff ones a Manage Tickets
/// permit (owner decision 11-15: permits only, no Discord roles). Checked live for every message.
async fn allowed(app: &App, gid: Id<GuildMarker>, m: &MessageCreate, admin: bool) -> Result<bool> {
    let owner = app.guild_info(gid).await?.owner;
    Ok(may(&app.level(gid, owner, m.author.id).await?, admin))
}

fn may(level: &permits::Level, admin: bool) -> bool {
    if admin {
        level.immune()
    } else {
        level.has("tickets")
    }
}

/// Claim, unclaim, reassign, priority, note and info in a ticket (Tickets 2.0).
async fn staff_work(
    app: &App,
    m: &MessageCreate,
    gid: Id<GuildMarker>,
    cfg: &crate::app::GuildCfg,
    here: &Here,
    w: &str,
    args: &str,
) -> Result<()> {
    let ch = m.channel_id;
    let pfx = &cfg.misc.prefix;
    let actor = m.author.id;
    let r = match w {
        "claim" => workflow::claim(app, gid, ch, actor).await?,
        "unclaim" => {
            let admin = allowed(app, gid, m, true).await?;
            workflow::unclaim(app, gid, ch, actor, admin).await?
        }
        "reassign" if !allowed(app, gid, m, true).await? => {
            return say_key(app, gid, ch, "reply.ticket.reassign_admin_only", &[]).await;
        }
        "reassign" => match user_ids(args).ok().and_then(|v| v.first().copied()) {
            Some(to) => workflow::reassign(app, gid, ch, actor, Id::new(to)).await?,
            None => return usage(app, gid, ch, &format!("`{pfx}reassign <member>`")).await,
        },
        "priority" => workflow::priority(app, gid, ch, actor, args).await?,
        "tickettype" => match cfg
            .tickets
            .types
            .iter()
            .find(|t| t.eq_ignore_ascii_case(args.trim()))
        {
            Some(k) => workflow::set_kind(app, gid, ch, actor, k, true).await?,
            None => {
                let choices = cfg.tickets.types.join(", ");
                Err(Line::with("reply.ticket.choices", &[("choices", &choices)]))
            }
        },
        "ticketnote" | "tnote" => {
            workflow::note(app, gid, here, m, args, cfg.tickets.log_channel).await?
        }
        _ => {
            return match workflow::summary(app, gid, ch).await? {
                Some(e) => say(app, ch, e).await,
                None => say_key(app, gid, ch, "reply.ticket.not_ticket", &[]).await,
            }
        }
    };
    // DM tickets have the buttons too; an older one without them is left as it is.
    if r.is_ok() && matches!(w, "claim" | "unclaim" | "reassign" | "tickettype") {
        if let Err(e) = panel::sync_intro(app, gid, ch, None).await {
            tracing::warn!(?e, %ch, "updating the ticket buttons failed");
        }
    }
    match r {
        Ok(None) => Ok(()),
        // The channel already got the visible notice.
        Ok(_) if matches!(w, "claim" | "unclaim" | "reassign") => Ok(()),
        Ok(Some(l)) | Err(l) => l.send(app, gid, ch).await,
    }
}

/// User ids typed as user mentions or numbers; `Err` holds the answer that names the issue.
fn user_ids(args: &str) -> Result<Vec<u64>, Line> {
    if let Some(t) = args
        .split_whitespace()
        .find(|t| t.starts_with("<@&") || t.starts_with("<#"))
    {
        return Err(Line::with("reply.ticket.users_bad", &[("typed", t)]));
    }
    ids(args).map_err(|()| "reply.ticket.users_not_found".into())
}

/// Ids typed as mentions or numbers; `Err` when a word is neither.
fn ids(args: &str) -> Result<Vec<u64>, ()> {
    args.split_whitespace()
        .map(|t| {
            let d = t
                .trim_start_matches(['<', '@', '!', '&', '#'])
                .trim_end_matches('>');
            d.parse::<u64>().ok().filter(|n| *n > 0).ok_or(())
        })
        .collect()
}

/// The usage line `line` of a ticket command, in its entry.
async fn usage(app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>, line: &str) -> Result<()> {
    say_key(app, gid, ch, "reply.ticket.usage", &[("text", line)]).await
}

async fn save(
    app: &App,
    m: &MessageCreate,
    gid: Id<GuildMarker>,
    done: Line,
    f: impl FnOnce(&mut Value),
) -> Result<()> {
    let mut full = None;
    let r = settings::save(app, gid, m.author.id.get(), &["tickets"], |doc| {
        if !doc["tickets"].is_object() {
            doc["tickets"] = json!({});
        }
        let before = doc["tickets"].clone();
        f(&mut doc["tickets"]);
        // On the locked version, so two adds at once cannot both pass the cap.
        full = brim_core::sections::tickets_cap_hit(&doc["tickets"], &before);
        match full {
            Some(_) => Err(String::new()),
            None => Ok(()),
        }
    })
    .await?;
    match (r, full) {
        (Ok(_), _) => done.send(app, gid, m.channel_id).await,
        (Err(_), Some((list, count, cap))) => {
            let key = match list {
                "snippets" => "reply.ticket.snippets_full",
                _ => "reply.ticket.blacklist_full",
            };
            let (count, cap) = (count.to_string(), cap.to_string());
            let vars = [("count", count.as_str()), ("cap", cap.as_str())];
            say_key(app, gid, m.channel_id, key, &vars).await
        }
        (Err(e), None) => say(app, m.channel_id, error(&e)).await,
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_str()
                        .map(String::from)
                        .or_else(|| x.as_u64().map(|n| n.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn blocked_page(t: &TicketsCfg, page: usize) -> (Embed, usize) {
    let pages = t.blocked.len().div_ceil(25).max(1);
    let page = page.min(pages - 1);
    let lines: Vec<String> = t
        .blocked
        .iter()
        .skip(page * 25)
        .take(25)
        .map(|u| format!("<@{u}> ({u})"))
        .collect();
    (
        emb(
            &format!("{TICKET_BLOCK} Blacklist"),
            &lines.join("\n"),
            BLUE,
        )
        .build(),
        pages,
    )
}

pub fn snippets_page(t: &TicketsCfg, page: usize) -> (Embed, usize) {
    let pages = t.snippets.len().div_ceil(10).max(1);
    let page = page.min(pages - 1);
    let mut e = emb("Snippets", "", BLUE);
    for s in t.snippets.iter().skip(page * 10).take(10) {
        e = e.field(EmbedFieldBuilder::new(
            clip(&s.name, 256),
            clip(&s.content, 100),
        ));
    }
    (e.build(), pages)
}

async fn paged(app: &App, m: &MessageCreate, kind: &str, (e, pages): (Embed, usize)) -> Result<()> {
    let rows: Vec<Component> = (pages > 1)
        .then(|| settings::page_row(m.author.id.get(), kind, 0, pages))
        .into_iter()
        .collect();
    let embeds = [e];
    let mut post = app.http.create_message(m.channel_id).embeds(&embeds);
    if let Some(to) = crate::cmdcfg::reply_to(m.channel_id) {
        post = post.reply(to).fail_if_not_exists(false);
    }
    let sent = post.components(&rows).await?;
    crate::cmdcfg::sent(app, m.channel_id, sent).await
}

async fn run(
    app: &App,
    m: &MessageCreate,
    gid: Id<GuildMarker>,
    cfg: &crate::app::GuildCfg,
    ticket: Option<Id<UserMarker>>,
    w: &str,
    args: &str,
) -> Result<()> {
    let ch = m.channel_id;
    let t = &cfg.tickets;
    let pfx = &cfg.misc.prefix;
    let no = |line: String| async move { usage(app, gid, ch, &line).await };
    let said = |key: &'static str| say_key(app, gid, ch, key, &[]);
    match w {
        "reply" | "r" | "areply" | "ar" => {
            if args.is_empty() && m.attachments.is_empty() {
                return no(format!("`{pfx}{w} <message>`")).await;
            }
            let Some(user) = ticket else { return Ok(()) };
            relay(app, m, gid, user, args, w.starts_with('a'), false).await
        }
        "snippet" | "s" | "asnippet" | "as" => {
            let Some(user) = ticket else { return Ok(()) };
            let name = args.to_lowercase();
            match t.snippets.iter().find(|s| s.name == name) {
                Some(s) => relay(app, m, gid, user, &s.content, w.starts_with('a'), true).await,
                None => said("reply.ticket.snippet_not_found").await,
            }
        }
        "close" | "c" | "aclose" | "ac" => {
            let Some(user) = ticket else { return Ok(()) };
            close(app, gid, ch, user, &m.author, args, w.starts_with('a'), 0).await
        }
        "closeall" | "acloseall" => {
            let action = crate::confirm::Action::CloseTickets {
                reason: args.to_string(),
                anon: w == "acloseall",
                here: Some(ch),
            };
            let job = crate::confirm::Pending::new(gid, m.author.id, action);
            crate::confirm::ask_confirm(app, m, job).await
        }
        "blacklist" | "block" | "whitelist" | "unblock" => {
            let add = matches!(w, "blacklist" | "block");
            let mut users = match user_ids(args) {
                Ok(users) => users,
                Err(why) => return why.send(app, gid, ch).await,
            };
            if users.is_empty() {
                users.extend(ticket.map(|u| u.get()));
            }
            if users.is_empty() {
                return said("reply.ticket.users_missing").await;
            }
            let done = if add {
                "reply.ticket.blacklisted"
            } else {
                "reply.ticket.unblacklisted"
            };
            save(app, m, gid, done.into(), |tk| {
                let mut list = strings(&tk["blocked"]);
                for u in users.iter().map(u64::to_string) {
                    list.retain(|x| *x != u);
                    if add {
                        list.push(u);
                    }
                }
                tk["blocked"] = json!(list);
            })
            .await
        }
        "blacklistclear" => {
            save(app, m, gid, "reply.ticket.blacklist_cleared".into(), |tk| {
                tk["blocked"] = json!([])
            })
            .await
        }
        "viewblacklist" if t.blocked.is_empty() => said("reply.ticket.blacklist_empty").await,
        "viewblacklist" => paged(app, m, "tkblock", blocked_page(t, 0)).await,
        "snippetadd" => {
            let (name, content) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
            let (name, content) = (name.to_lowercase(), content.trim());
            if name.is_empty() || content.is_empty() {
                return no(format!("`{pfx}snippetadd <name> <content>`")).await;
            }
            if name.chars().count() > 100 {
                return said("reply.ticket.snippet_name_long").await;
            }
            if content.chars().count() > 1000 {
                return said("reply.ticket.snippet_content_long").await;
            }
            if t.snippets.iter().any(|s| s.name == name) {
                return said("reply.ticket.snippet_exists").await;
            }
            let author = m.author.id.to_string();
            let entry = json!({"name": name, "content": content, "author": author});
            save(app, m, gid, "reply.ticket.snippet_added".into(), |tk| {
                let mut list = tk["snippets"].as_array().cloned().unwrap_or_default();
                list.push(entry);
                tk["snippets"] = json!(list);
            })
            .await
        }
        "snippetremove" => {
            let name = args.to_lowercase();
            if !t.snippets.iter().any(|s| s.name == name) {
                return said("reply.ticket.snippet_missing").await;
            }
            save(app, m, gid, "reply.ticket.snippet_removed".into(), |tk| {
                let mut list = tk["snippets"].as_array().cloned().unwrap_or_default();
                list.retain(|s| s["name"].as_str() != Some(name.as_str()));
                tk["snippets"] = json!(list);
            })
            .await
        }
        "snippetclear" => {
            save(app, m, gid, "reply.ticket.snippets_cleared".into(), |tk| {
                tk["snippets"] = json!([])
            })
            .await
        }
        "viewsnippet" | "viewsnippets" | "snippetlist" | "vs" if !args.is_empty() => {
            let name = args.to_lowercase();
            let Some(s) = t.snippets.iter().find(|s| s.name == name) else {
                return said("reply.ticket.snippet_missing").await;
            };
            let by = s
                .author
                .map_or("*Unknown*".to_string(), |a| format!("<@{a}>"));
            let e = emb("Snippet", "", BLUE)
                .field(EmbedFieldBuilder::new("Name", clip(&s.name, 1024)))
                .field(EmbedFieldBuilder::new("Content", clip(&s.content, 1024)))
                .field(EmbedFieldBuilder::new("Author", by));
            say(app, ch, e.build()).await
        }
        "viewsnippet" | "viewsnippets" | "snippetlist" | "vs" if t.snippets.is_empty() => {
            said("reply.ticket.snippets_empty").await
        }
        "viewsnippet" | "viewsnippets" | "snippetlist" | "vs" => {
            paged(app, m, "tksnip", snippets_page(t, 0)).await
        }
        "ticketsetup" | "category" => setup(app, m, gid, t, w, args).await,
        "pingrole" | "mentionrole" => {
            let mut here = false;
            let mut list = vec![];
            for tok in args.split_whitespace() {
                match tok.trim_start_matches('@').to_lowercase().as_str() {
                    "everyone" => list.push(gid.to_string()),
                    "here" => here = true,
                    _ => match ids(tok) {
                        Ok(v) => list.extend(v.iter().map(u64::to_string)),
                        Err(()) => return said("reply.ticket.roles_not_found").await,
                    },
                }
            }
            // The bot is in many servers: only this server's roles (the dashboard's check too).
            let roles = &app.guild_info(gid).await?.roles;
            if list.iter().any(|r| {
                r.parse()
                    .ok()
                    .and_then(Id::new_checked)
                    .is_none_or(|r| !roles.contains_key(&r))
            }) {
                return said("reply.ticket.roles_not_found").await;
            }
            if list.len() + usize::from(here) > 10 {
                return said("reply.ticket.roles_too_many").await;
            }
            save(app, m, gid, "reply.ticket.roles_updated".into(), |tk| {
                tk["ping_roles"] = json!(list);
                tk["ping_here"] = json!(here);
            })
            .await
        }
        "logging" | "logs" => {
            let given = match ids(args) {
                Ok(v) => v.first().copied(),
                Err(()) => return said("reply.ticket.log_channel_not_found").await,
            };
            // The bot is in many servers: transcripts go only to a channel of this one.
            if let Some(c) = given {
                let here = &app.guild_info(gid).await?.channels;
                if Id::new_checked(c).is_none_or(|c| !here.contains(&c)) {
                    return said("reply.ticket.log_channel_not_found").await;
                }
            }
            if t.log_channel.is_some() && given.is_none() {
                let done = "reply.ticket.logging_off".into();
                return save(app, m, gid, done, |tk| tk["log_channel"] = Value::Null).await;
            }
            if t.category.is_none() {
                return Ok(());
            }
            let log = match given {
                Some(c) => c,
                None => {
                    let ows = overwrites(gid, app.me.get(), &staff_roles(app, gid).await?);
                    super::make_log(app, gid, &ows).await?.get()
                }
            };
            save(app, m, gid, "reply.ticket.logging_on".into(), |tk| {
                tk["log_channel"] = json!(log.to_string())
            })
            .await
        }
        "commandonly" | "commandrequired" | "anonymous" | "loggingplus" | "advancedlogging"
        | "advancedlogs" => {
            let (key, now, [on, off]) = match w {
                "commandonly" | "commandrequired" => (
                    "command_only",
                    !t.command_only,
                    [
                        "reply.ticket.command_only_on",
                        "reply.ticket.command_only_off",
                    ],
                ),
                "anonymous" => (
                    "anonymous",
                    !t.anonymous,
                    ["reply.ticket.anonymous_on", "reply.ticket.anonymous_off"],
                ),
                _ => (
                    "full_log",
                    !t.full_log,
                    ["reply.ticket.full_log_on", "reply.ticket.full_log_off"],
                ),
            };
            let done = if now { on } else { off };
            save(app, m, gid, done.into(), |tk| tk[key] = json!(now)).await
        }
        "greetingmessage" | "welcomemessage" | "greetmessage" | "closingmessage"
        | "goodbyemessage" | "closemessage" => {
            let (key, done) = if matches!(w, "greetingmessage" | "welcomemessage" | "greetmessage")
            {
                ("greeting", "reply.ticket.greeting_set")
            } else {
                ("closing", "reply.ticket.closing_set")
            };
            save(app, m, gid, done.into(), |tk| tk[key] = json!(args)).await
        }
        "toggle" | "enable" | "disable" => {
            let pause = wanted_pause(w, t.paused);
            if w != "toggle" && pause == t.paused {
                return said(if pause {
                    "reply.ticket.creation_already_off"
                } else {
                    "reply.ticket.creation_already_on"
                })
                .await;
            }
            let done = if pause {
                "reply.ticket.creation_off"
            } else {
                "reply.ticket.creation_on"
            };
            save(app, m, gid, done.into(), |tk| {
                tk["paused"] = json!(pause);
                tk["pause_reason"] = json!(if pause { args } else { "" });
            })
            .await
        }
        "viewconfig" => {
            let roles = live_ids(app, gid, &t.ping_roles).await;
            say(app, ch, view_config(t, &roles, pfx, gid)).await
        }
        _ => Ok(()),
    }
}

/// Closes every open ticket of the server one by one (after the Yes of `closeall`) and says how
/// many closed.
pub async fn close_all(
    app: &App,
    gid: Id<GuildMarker>,
    staff: &User,
    reason: &str,
    anon: bool,
    here: Option<Id<ChannelMarker>>,
) -> Result<String> {
    let mut open: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT channel_id, user_id FROM tickets WHERE guild_id = $1 AND status = 'open'",
    )
    .bind(gid.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    // The ticket that holds the question stays open, or its delete would take the answer with it.
    let before = open.len();
    open.retain(|(c, _)| Some(*c as u64) != here.map(Id::get));
    let kept = open.len() < before;
    let total = open.len();
    let mut closed = 0;
    for (c, u) in open {
        let (c, u) = (Id::new(c as u64), Id::new(u as u64));
        match close(app, gid, c, u, staff, reason, anon, 0).await {
            Ok(()) => closed += 1,
            Err(e) => tracing::warn!(?e, channel = %c, "closing a ticket failed"),
        }
    }
    let mut text = closed_text(app, gid, closed, total, anon).await;
    if kept {
        text.push(' ');
        text.push_str(&crate::reply::say_line(app, gid, "reply.closeall.kept_open", &[]).await);
    }
    Ok(text)
}

/// `enable` and `disable` set the state they name; `toggle` flips it.
fn wanted_pause(w: &str, paused: bool) -> bool {
    match w {
        "enable" => false,
        "disable" => true,
        _ => !paused,
    }
}

async fn closed_text(
    app: &App,
    gid: Id<GuildMarker>,
    closed: usize,
    total: usize,
    anon: bool,
) -> String {
    let key = match (closed == total, anon) {
        (true, false) => "reply.closeall.done",
        (true, true) => "reply.closeall.done_anon",
        (false, false) => "reply.closeall.partly",
        (false, true) => "reply.closeall.partly_anon",
    };
    let [c, t, left] = [closed, total, total - closed].map(|n| n.to_string());
    let vars = [("closed", c.as_str()), ("total", &t), ("left", &left)];
    crate::reply::say_line(app, gid, key, &vars).await
}

fn view_config(
    t: &TicketsCfg,
    ping_roles: &[Id<RoleMarker>],
    pfx: &str,
    gid: Id<GuildMarker>,
) -> Embed {
    let or_unset = |s: String| {
        if s.is_empty() {
            "*Not set*".to_string()
        } else {
            s
        }
    };
    let on = |b: bool| if b { "Enabled" } else { "Disabled" }.to_string();
    let mut ping: Vec<String> = ping_roles
        .iter()
        .map(|r| {
            if r.get() == gid.get() {
                "@everyone".into()
            } else {
                format!("<@&{r}>")
            }
        })
        .collect();
    if t.ping_here {
        ping.push("@here".into());
    }
    let reason = if t.pause_reason.is_empty() {
        "No reason was provided."
    } else {
        &t.pause_reason
    };
    let creation = if t.paused {
        format!("Disabled ({})", clip(reason, 989))
    } else {
        "Enabled".into()
    };
    let f = |n: &str, v: String| EmbedFieldBuilder::new(n, v).inline();
    emb("Server Configurations", "", BLUE)
        .field(f("Prefix", pfx.to_string()))
        .field(f(
            "Category",
            t.category.map_or("*Not set*".into(), |c| format!("<#{c}>")),
        ))
        .field(f("Ping Roles", or_unset(ping.join(" "))))
        .field(f(
            "Logging",
            t.log_channel
                .map_or("*Not set*".into(), |c| format!("<#{c}>")),
        ))
        .field(f("Advanced Logging", on(t.full_log)))
        .field(f("Anonymous Messaging", on(t.anonymous)))
        .field(f("Command Only", on(t.command_only)))
        .field(f("Ticket Creation", creation))
        .field(EmbedFieldBuilder::new(
            "Greeting Message",
            or_unset(clip(&t.greeting, 1000)),
        ))
        .field(EmbedFieldBuilder::new(
            "Closing Message",
            or_unset(clip(&t.closing, 1000)),
        ))
        .build()
}

/// `ticketsetup` makes the category and the log channel; `category [name]` only a new category.
async fn setup(
    app: &App,
    m: &MessageCreate,
    gid: Id<GuildMarker>,
    t: &TicketsCfg,
    w: &str,
    args: &str,
) -> Result<()> {
    let ch = m.channel_id;
    let pfx = app.cfg(gid.get()).await?.misc.prefix.clone();
    let channels = &app.guild_info(gid).await?.channels;
    let exists = t.category.is_some_and(|c| channels.contains(&c));
    let name = if w == "category" && !args.is_empty() {
        args
    } else {
        "Tickets"
    };
    if name.chars().count() > 100 {
        return say_key(app, gid, ch, "reply.ticket.category_name_long", &[]).await;
    }
    if exists {
        let key = if w == "category" {
            "reply.ticket.category_exists"
        } else {
            "reply.ticket.already_set_up"
        };
        return say_key(app, gid, ch, key, &[]).await;
    }
    let (category, log) = make_category(app, gid, name, w == "ticketsetup").await?;
    let log = log.map(|l| l.to_string());
    let done = if w == "category" {
        "reply.ticket.category_made".into()
    } else {
        Line::with("reply.ticket.set_up", &[("prefix", &pfx)])
    };
    save(app, m, gid, done, |tk| {
        tk["enabled"] = json!(true);
        tk["category"] = json!(category.to_string());
        if let Some(l) = log {
            tk["log_channel"] = json!(l);
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R5: a new snippet past the cap is refused on the saved config; one already there can go.
    #[tokio::test]
    async fn snippets_stop_at_the_cap() {
        let Some(t) = crate::test_support::T::new(987).await else {
            return;
        };
        let full: Vec<Value> = (0..brim_core::sections::MAX_SNIPPETS)
            .map(|i| json!({"name": format!("s{i}"), "content": "x"}))
            .collect();
        let cfg = json!({"tickets": {"enabled": true, "category": "600", "snippets": full}});
        t.server_config(0, t.owner(), cfg).await;
        let gid = t.g(0);
        let info = crate::hierarchy::GuildInfo {
            owner: Id::new(t.owner()),
            roles: Default::default(),
            role_perms: Default::default(),
            managed: Default::default(),
            unpermittable: Default::default(),
            parents: [(Id::new(601), Id::new(600))].into(),
            channels: [Id::new(600), Id::new(601)].into(),
            nsfw: Default::default(),
            voice: Default::default(),
            bot_top: 10,
            fetched: std::time::Instant::now(),
        };
        t.app
            .guilds
            .lock()
            .unwrap()
            .insert(gid, std::sync::Arc::new(info));
        let msg = MessageCreate {
            channel_type: None,
            message: serde_json::from_value(json!({"id": "5", "channel_id": "601",
            "guild_id": gid.to_string(), "content": "t!",
            "author": {"id": t.owner().to_string(), "username": "o", "discriminator": "0", "avatar": null},
            "timestamp": "2026-01-01T00:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
            "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
            "embeds": [], "components": [], "pinned": false, "type": 0}))
            .unwrap(),
        };
        async fn count(app: &App, gid: Id<GuildMarker>) -> usize {
            app.forget_cfg(gid.get());
            app.cfg(gid.get()).await.unwrap().tickets.snippets.len()
        }
        assert!(on_message(&t.app, &msg, gid, Some("snippetadd new hello"))
            .await
            .unwrap());
        assert_eq!(t.calls("the most allowed is 100"), 1);
        assert_eq!(count(&t.app, gid).await, 100);
        assert!(on_message(&t.app, &msg, gid, Some("snippetremove s0"))
            .await
            .unwrap());
        assert_eq!(count(&t.app, gid).await, 99);
        assert!(on_message(&t.app, &msg, gid, Some("snippetadd new hello"))
            .await
            .unwrap());
        assert_eq!(count(&t.app, gid).await, 100);
    }

    #[test]
    fn enable_and_disable_set_their_state_and_toggle_flips() {
        for paused in [false, true] {
            assert!(!wanted_pause("enable", paused));
            assert!(wanted_pause("disable", paused));
            assert_eq!(wanted_pause("toggle", paused), !paused);
        }
    }

    #[test]
    fn blacklist_takes_users_only() {
        assert_eq!(user_ids("<@5> 6 <@!7>").ok(), Some(vec![5, 6, 7]));
        let key = |a| user_ids(a).err().map(|l| l.0);
        assert_eq!(key("<#5>"), Some("reply.ticket.users_bad"));
        assert_eq!(key("<@&5>"), Some("reply.ticket.users_bad"));
        assert_eq!(key("bob"), Some("reply.ticket.users_not_found"));
        assert_eq!(ids("<#5>"), Ok(vec![5]));
    }

    /// T-01, T-03, T-04: a plain message is relayed only for staff, a second close and a close
    /// cut off by a restart are handled, and a stale half-opened ticket does not block the member.
    #[tokio::test]
    async fn relay_needs_staff_and_closes_recover() {
        let Some(t) = crate::test_support::T::new(948).await else {
            return;
        };
        const CAT: u64 = 600;
        const CH: u64 = 601;
        const STAFF_ROLE: u64 = 55;
        const OLD_ACCESS: u64 = 56;
        // An access role left in an old config gives nothing any more (owner decision 11-15).
        let cfg = json!({"tickets": {"enabled": true, "category": CAT.to_string(),
            "access_roles": [OLD_ACCESS.to_string()]}});
        t.server_config(0, t.owner(), cfg).await;
        let gid = t.g(0);
        let info = crate::hierarchy::GuildInfo {
            owner: Id::new(t.owner()),
            roles: Default::default(),
            role_perms: Default::default(),
            managed: Default::default(),
            unpermittable: Default::default(),
            parents: [(Id::new(CH), Id::new(CAT))].into(),
            channels: [Id::new(CAT), Id::new(CH)].into(),
            nsfw: Default::default(),
            voice: Default::default(),
            bot_top: 10,
            fetched: std::time::Instant::now(),
        };
        t.app
            .guilds
            .lock()
            .unwrap()
            .insert(gid, std::sync::Arc::new(info));
        let (member, outsider, staff) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
        t.set(|m| {
            m.members.insert((gid.get(), member), vec![]);
            m.members.insert((gid.get(), outsider), vec![OLD_ACCESS]);
            m.members.insert((gid.get(), staff), vec![STAFF_ROLE]);
        });
        let (permit,): (i64,) = sqlx::query_as(
            "INSERT INTO permits (guild_id, name, rank, perms) VALUES ($1, 'Moderators', 1, '{tickets}') RETURNING id",
        )
        .bind(gid.get() as i64)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1, $2, $3)")
            .bind(permit)
            .bind(gid.get() as i64)
            .bind(STAFF_ROLE as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, $3)",
        )
        .bind(permit)
        .bind(gid.get() as i64)
        .bind(staff as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tickets (guild_id, number, user_id, channel_id) VALUES ($1, 1, $2, $3)",
        )
        .bind(gid.get() as i64)
        .bind(member as i64)
        .bind(CH as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
        let msg = |from: u64, roles: &[u64]| -> MessageCreate {
            let roles: Vec<String> = roles.iter().map(u64::to_string).collect();
            MessageCreate {
                channel_type: None,
                message: serde_json::from_value(json!({"id": "5", "channel_id": CH.to_string(),
                "guild_id": gid.to_string(), "content": "hello",
                "author": {"id": from.to_string(), "username": "s", "discriminator": "0", "avatar": null},
                "member": {"roles": roles, "joined_at": "2020-01-01T00:00:00.000000+00:00",
                    "deaf": false, "mute": false, "flags": 0},
                "timestamp": "2026-01-01T00:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
                "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
                "embeds": [], "components": [], "pinned": false, "type": 0}))
            .unwrap(),
            }
        };
        // Someone who only sees the channel, or holds an old access role: nothing reaches the member.
        assert!(on_message(&t.app, &msg(outsider, &[OLD_ACCESS]), gid, None)
            .await
            .unwrap());
        assert_eq!(t.calls("POST /users/@me/channels"), 0);
        assert_eq!(t.calls("not have access to reply"), 1);
        // Staff with a Manage Tickets permit: relayed.
        assert!(on_message(&t.app, &msg(staff, &[STAFF_ROLE]), gid, None)
            .await
            .unwrap());
        assert_eq!(t.calls("POST /users/@me/channels"), 1);
        // A close while another one runs loses and says so.
        t.exec("UPDATE tickets SET status = 'closing' WHERE guild_id = $1")
            .await;
        let u: User = serde_json::from_value(
            json!({"id": staff.to_string(), "username": "s", "discriminator": "0", "avatar": null}),
        )
        .unwrap();
        close(&t.app, gid, Id::new(CH), Id::new(member), &u, "", false, 0)
            .await
            .unwrap();
        assert_eq!(t.calls("already being closed"), 1);
        // Cut off by a restart with the channel still there: open again, staff told.
        t.set(|m| {
            m.text_channels.insert(CH, (gid.get(), false));
        });
        super::super::close_check(&t.app, gid, &json!({"channel": CH}))
            .await
            .unwrap();
        let status = "SELECT count(*) FROM tickets WHERE guild_id = $1 AND status = 'open' AND closed_at IS NULL";
        assert_eq!(t.count(status).await, 1);
        assert_eq!(t.calls("did not finish"), 1);
        // With the channel gone: closed.
        t.exec("UPDATE tickets SET status = 'closing' WHERE guild_id = $1")
            .await;
        t.set(|m| {
            m.text_channels.clear();
        });
        super::super::close_check(&t.app, gid, &json!({"channel": CH}))
            .await
            .unwrap();
        let closed = "SELECT count(*) FROM tickets WHERE guild_id = $1 AND status = 'closed' AND closed_at IS NOT NULL";
        assert_eq!(t.count(closed).await, 1);
        // A channel staff already deleted: the close still finishes (Discord answers 404).
        t.exec("INSERT INTO tickets (guild_id, number, user_id, channel_id) VALUES ($1, 3, $1 * 10 + 1, 602)").await;
        close(&t.app, gid, Id::new(602), Id::new(member), &u, "", false, 0)
            .await
            .unwrap();
        let by = "SELECT count(*) FROM tickets WHERE guild_id = $1 AND number = 3 AND status = 'closed' AND closed_by = $1 * 10 + 3";
        assert_eq!(t.count(by).await, 1);
        // A half-opened ticket from before a restart no longer blocks a new one (T-04).
        sqlx::query("INSERT INTO tickets (guild_id, number, user_id, status, opened_at) VALUES ($1, 2, $2, 'creating', now() - interval '1 hour')")
            .bind(gid.get() as i64)
            .bind(outsider as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        assert!(matches!(
            super::super::claim_number(&t.app.pool, gid, Id::new(outsider), None)
                .await
                .unwrap(),
            super::super::Claim::Number(_)
        ));
        t.exec("DELETE FROM tickets WHERE guild_id = $1").await;
        t.exec("DELETE FROM ticket_users WHERE user_id BETWEEN $1 * 10 AND $1 * 10 + 9")
            .await;
        t.cleanup().await;
    }

    #[test]
    fn ids_take_mentions_and_numbers_only() {
        assert_eq!(ids("<@1> <@!2> <@&3> 4"), Ok(vec![1, 2, 3, 4]));
        assert_eq!(ids("bob"), Err(()));
        assert_eq!(ids(""), Ok(vec![]));
    }
}

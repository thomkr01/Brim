use super::*;

/// Relays a staff message to the member: a reply, a snippet or a plain message in the ticket.
#[allow(clippy::too_many_arguments)]
pub(super) async fn relay(
    app: &App,
    m: &MessageCreate,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    text: &str,
    anon: bool,
    snippet: bool,
) -> Result<()> {
    let ch = m.channel_id;
    let cfg = app.cfg(gid.get()).await?;
    if cfg.tickets.blocked.contains(&user) {
        return say_key(app, gid, ch, "reply.ticket.staff_blacklisted", &[]).await;
    }
    let Some(member) = http_err::member(&app.http, gid, user).await? else {
        let vars = [("prefix", cfg.misc.prefix.as_str())];
        return say_key(app, gid, ch, "reply.ticket.user_not_found", &vars).await;
    };
    let Some(s) = server(app, gid).await? else {
        return Ok(());
    };
    let text = if snippet {
        tags(text, &member.user)
    } else {
        text.to_string()
    };
    let staff = &m.author;
    let (to_member, copy) = from_staff(&s, staff, &member.user, &text, anon);
    let (files, linked) = download(app, &m.attachments).await;
    let to_member = attachment_fields(to_member, linked.iter().copied());
    let dm = app
        .http
        .create_private_channel(user)
        .await?
        .model()
        .await?
        .id;
    let sent = app
        .http
        .create_message(dm)
        .embeds(&[to_member.build()])
        .attachments(&files)
        .await;
    if sent.is_err() {
        return say_key(app, gid, ch, "reply.ticket.dm_closed", &[]).await;
    }
    let copy = attachment_fields(copy, linked.iter().copied());
    app.http
        .create_message(ch)
        .embeds(&[copy.build()])
        .attachments(&files)
        .await?;
    // A linked file lives on the staff message, so that one stays.
    if linked.is_empty() {
        best_effort(
            app.http.delete_message(ch, m.id).await,
            "relayed ticket message",
        );
    }
    touch(app, gid, ch, true).await
}

/// Who wrote a line of the history, from the relay embeds the bot posts in the ticket.
fn speaker(title: &str, author: Option<&str>, footer: Option<&str>) -> Option<String> {
    // Titles start with an emoji, so match the words at the end.
    match title {
        t if t.ends_with("Message Received") => Some(format!(
            "{} (User)",
            footer
                .and_then(|f| f.rsplit_once(" | "))
                .map_or("Unknown", |x| x.0)
        )),
        t if t.ends_with("Message Sent") => Some(format!(
            "{} (Staff)",
            author.map_or("Unknown", |a| a.strip_suffix(" (Anonymous)").unwrap_or(a))
        )),
        t if t.ends_with("Staff Note") => Some(format!("{} (Note)", author.unwrap_or("Unknown"))),
        _ => None,
    }
}

/// The conversation as text, oldest first: member and staff messages plus staff comments, and
/// whether it is all of it (false past [`HISTORY_PAGES`]). In a panel ticket (`member` set) the
/// member writes in the channel, so their messages are the User's and everyone else's Staff.
async fn history(
    app: &App,
    ch: Id<ChannelMarker>,
    member: Option<Id<UserMarker>>,
) -> Result<(String, bool)> {
    let mut all: Vec<Message> = vec![];
    let mut complete = false;
    for _ in 0..HISTORY_PAGES {
        let req = app.http.channel_messages(ch);
        let page = match all.last() {
            Some(m) => req.before(m.id).limit(100).await?,
            None => req.limit(100).await?,
        }
        .models()
        .await?;
        let done = page.len() < 100;
        all.extend(page);
        if done {
            complete = true;
            break;
        }
    }
    let mut lines = vec![];
    for m in all.iter().rev() {
        let at: String = m
            .timestamp
            .iso_8601()
            .to_string()
            .replace('T', " ")
            .chars()
            .take(19)
            .collect();
        let (who, mut text) = if m.author.bot {
            let Some(e) = m.embeds.first().filter(|_| m.author.id == app.me) else {
                continue;
            };
            // A panel ticket's intro holds the member's form answers.
            let intro = e
                .title
                .as_deref()
                .is_some_and(|t| t.ends_with("New Ticket"));
            if member.is_some() && intro {
                let answers: Vec<String> = e
                    .fields
                    .iter()
                    .filter(|f| f.name != "Roles" && f.name != "Status")
                    .map(|f| format!("{}: {}", f.name, f.value))
                    .collect();
                if !answers.is_empty() {
                    lines.push(line(&at, "Form (User)", &answers.join(" | ")));
                }
                continue;
            }
            let Some(who) = speaker(
                e.title.as_deref().unwrap_or(""),
                e.author.as_ref().map(|a| a.name.as_str()),
                e.footer.as_ref().map(|f| f.text.as_str()),
            ) else {
                continue;
            };
            let mut text = e.description.clone().unwrap_or_default();
            for f in e
                .fields
                .iter()
                .filter(|f| f.name.starts_with("Attachment "))
            {
                text = format!("{text} (Attachment: {})", f.value)
                    .trim_start()
                    .to_string();
            }
            (who, text)
        } else {
            let role = match member {
                Some(u) if u == m.author.id => "User",
                Some(_) => "Staff",
                None => "Comment",
            };
            (format!("{} ({role})", m.author.name), m.content.clone())
        };
        for a in &m.attachments {
            text = format!("{text} (Attachment: {})", a.url)
                .trim_start()
                .to_string();
        }
        if text.is_empty() {
            continue;
        }
        lines.push(line(&at, &who, &text));
    }
    if !complete {
        lines.insert(
            0,
            "[Older messages were not read: this transcript is incomplete.]".into(),
        );
    }
    Ok((lines.join("\n"), complete))
}

/// One transcript line. Continuation lines are indented, so a member's text cannot start a line
/// that reads as `[time] Name (Staff): ...`.
fn line(at: &str, who: &str, text: &str) -> String {
    format!("[{at}] {who}: {}", text.replace('\n', "\n  "))
}

/// Closes a ticket: the log (with the conversation) first, since the channel holds the only copy,
/// then the channel, then the member hears about it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn close(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    user: Id<UserMarker>,
    staff: &User,
    reason: &str,
    anon: bool,
    idle_hours: u32,
) -> Result<()> {
    let cfg = app.cfg(gid.get()).await?;
    let t = &cfg.tickets;
    let member = http_err::member(&app.http, gid, user).await?;
    // T-03: the ticket counts as closed only once the log is saved and the channel is gone. A
    // second close meanwhile loses here; the check job, queued in the same transaction, finds a
    // close that a restart cut off.
    let mut tx = app.pool.begin().await?;
    let claimed: Option<(i32, Option<String>)> = sqlx::query_as(
        "UPDATE tickets SET status = 'closing' WHERE channel_id = $1 AND status = 'open'
         AND ($2 = 0 OR last_activity_at < now() - make_interval(hours => $2)) AND guild_id = $3
         RETURNING number, panel",
    )
    .bind(ch.get() as i64)
    // Auto-close (idle_hours > 0) reads the activity again here: a new message stops it.
    .bind(idle_hours as i32)
    .bind(gid.get() as i64)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((number, panel)) = claimed else {
        tx.rollback().await?;
        if idle_hours > 0 {
            return Ok(());
        }
        return say_key(app, gid, ch, "reply.ticket.already_closing", &[]).await;
    };
    brim_core::jobs::enqueue(
        &mut *tx,
        gid.get() as i64,
        "ticket_close_check",
        json!({"channel": ch.get()}),
        now_secs() + CLOSE_CHECK_SECS,
    )
    .await?;
    tx.commit().await?;
    say_key(app, gid, ch, "reply.ticket.closing", &[]).await?;
    let reopen = || async {
        sqlx::query(
            "UPDATE tickets SET status = 'open' WHERE channel_id = $1 AND status = 'closing' AND guild_id = $2",
        )
        .bind(ch.get() as i64)
        .bind(gid.get() as i64)
        .execute(&app.pool)
        .await
    };
    let reason = if reason.is_empty() {
        "No reason was provided."
    } else {
        reason
    };
    if let Some(log) = t.log_channel {
        if let Err(e) = close_log(
            app,
            (gid, number, panel.is_some()),
            ch,
            log,
            user,
            member.as_ref().map(|m| &m.user),
            staff,
            reason,
            anon,
            t.full_log,
        )
        .await
        {
            reopen().await?;
            // A Discord refusal, the log channel being public, or a database error.
            let why = match e.downcast_ref::<twilight_http::Error>() {
                Some(_) => http_err::why(&e),
                None => e.to_string(),
            };
            say_key(
                app,
                gid,
                ch,
                "reply.ticket.log_not_saved",
                &[("error", &why)],
            )
            .await?;
            return Err(e);
        }
    }
    // Gone already (staff deleted it meanwhile): the close still finishes.
    if let Err(e) = app.http.delete_channel(ch).await.map(drop).or_else(|e| {
        if http_err::gone(&e) {
            Ok(())
        } else {
            Err(e)
        }
    }) {
        tracing::warn!(?e, %ch, "deleting a ticket failed");
        reopen().await?;
        return say_key(app, gid, ch, "reply.ticket.no_delete_perm", &[]).await;
    }
    sqlx::query(
        "UPDATE tickets SET status = 'closed', closed_at = now(), closed_by = $2
         WHERE channel_id = $1 AND status IN ('open', 'closing') AND guild_id = $3",
    )
    .bind(ch.get() as i64)
    .bind(staff.id.get() as i64)
    .bind(gid.get() as i64)
    .execute(&app.pool)
    .await?;
    // The bot closes on its own (auto-close) as itself: that is recorded as no person.
    let actor = (staff.id != app.me).then_some(staff.id);
    event(
        app,
        gid,
        number,
        actor,
        "closed",
        json!({"anonymous": anon}),
    )
    .await;
    let Some(member) = member else {
        return Ok(());
    };
    let Some(s) = server(app, gid).await? else {
        return Ok(());
    };
    let dm = app
        .http
        .create_private_channel(user)
        .await?
        .model()
        .await?
        .id;
    if !t.closing.is_empty() {
        let e = notice(&s, true, &t.closing, &member.user);
        best_effort(
            app.http.create_message(dm).embeds(&[e]).await,
            "ticket closing message",
        );
    }
    let e = closed(&s, reason, (!anon).then_some(staff));
    best_effort(
        app.http.create_message(dm).embeds(&[e]).await,
        "ticket closed dm",
    );
    Ok(())
}

/// The ticket's type and its type and handler changes, oldest first, for the close log.
async fn history_of(
    app: &App,
    gid: Id<GuildMarker>,
    number: i32,
) -> Result<(Option<String>, Vec<String>)> {
    let g = gid.get() as i64;
    let kind: Option<String> =
        sqlx::query_scalar("SELECT kind FROM tickets WHERE guild_id = $1 AND number = $2")
            .bind(g)
            .bind(number)
            .fetch_optional(&app.pool)
            .await?
            .flatten();
    let rows: Vec<(Option<i64>, String, Value, i64)> = sqlx::query_as(
        "SELECT actor_id, action, details, extract(epoch from at)::bigint FROM ticket_events
         WHERE guild_id = $1 AND number = $2 AND action IN ('claimed', 'unclaimed', 'reassigned', 'type')
         ORDER BY at DESC, id DESC LIMIT 20",
    )
    .bind(g)
    .bind(number)
    .fetch_all(&app.pool)
    .await?;
    let s = |v: &Value| v.as_str().unwrap_or("?").to_string();
    // The newest 20, shown oldest first.
    let lines = rows
        .into_iter()
        .rev()
        .map(|(actor, action, d, at)| {
            let who = actor.map_or("Brim".into(), |a| format!("<@{a}>"));
            let what = match action.as_str() {
                "claimed" => "claimed it".into(),
                "unclaimed" => format!("removed the claim of <@{}>", s(&d["from"])),
                "reassigned" => format!("gave it to <@{}>", s(&d["to"])),
                _ => match d["from"].as_str() {
                    Some(f) => format!("changed the type from {f} to {}", s(&d["to"])),
                    None => format!("set the type to {}", s(&d["to"])),
                },
            };
            format!("<t:{at}:t> {who} {what}")
        })
        .collect();
    Ok((kind, lines))
}

#[allow(clippy::too_many_arguments)]
async fn close_log(
    app: &App,
    (gid, number, panel): (Id<GuildMarker>, i32, bool),
    ch: Id<ChannelMarker>,
    log: Id<ChannelMarker>,
    user: Id<UserMarker>,
    member: Option<&User>,
    staff: &User,
    reason: &str,
    anon: bool,
    full: bool,
) -> Result<()> {
    let shown = if anon {
        format!("{} (Anonymous)", staff.name)
    } else {
        staff.name.clone()
    };
    let who = member.map_or("Unknown | 000000000000000000".to_string(), |u| {
        format!("{} | {}", u.name, u.id)
    });
    // Read only: a failed lookup leaves the lines out, it never stops the close.
    let (kind, changes) = history_of(app, gid, number).await.unwrap_or_else(|e| {
        tracing::warn!(?e, number, "reading the ticket history failed");
        (None, vec![])
    });
    let e = closed_log((staff, &shown), reason, user, kind.as_deref(), &changes).footer(&who);
    let (file, complete) = if full {
        let (text, complete) = history(app, ch, panel.then_some(user)).await?;
        let f = Attachment::from_bytes(format!("ticket_log_{user}.txt"), text.into_bytes(), 1);
        (Some(f), complete)
    } else {
        (None, false)
    };
    let (posted, file) = crate::events::send_private(app, gid, log, e, file).await?;
    if let Some(f) = file {
        // The dashboard serves a transcript it finds here (T-02).
        sqlx::query(
            "INSERT INTO ticket_transcripts (guild_id, number, log_channel_id, log_message_id, complete)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (guild_id, number) DO UPDATE
             SET log_channel_id = $3, log_message_id = $4, complete = $5, created_at = now()",
        )
        .bind(gid.get() as i64)
        .bind(number)
        .bind(log.get() as i64)
        .bind(f.get() as i64)
        .bind(complete)
        .execute(&app.pool)
        .await?;
    }
    if let (Some(file), Some(base)) = (file, crate::general::dashboard()) {
        let url = format!(
            "{}/#/{gid}/tickets/log/{log}/{file}",
            base.trim_end_matches('/')
        );
        let link = Component::Button(Button {
            id: None,
            custom_id: None,
            disabled: false,
            emoji: None,
            label: Some("Message Logs".into()),
            style: ButtonStyle::Link,
            url: Some(url),
            sku_id: None,
        });
        best_effort(
            app.http
                .update_message(log, posted)
                .components(Some(&[action_row(vec![link])]))
                .await,
            "ticket log link",
        );
    }
    Ok(())
}

/// "Ticket Closed": the close reason, the member, the ticket type and its changes; `staff` is
/// the author line (with "(Anonymous)" when the close was anonymous).
pub(crate) fn closed_log(
    (staff, shown): (&User, &str),
    reason: &str,
    user: Id<UserMarker>,
    kind: Option<&str>,
    changes: &[String],
) -> Log {
    let mut e = Log::new("Ticket Closed", RED)
        .author(staff, shown)
        .text(reason)
        .user("User", user.get())
        .var("reason", reason)
        .var("member", &brim_logging::embed::user(user.get()))
        .var("staff", shown);
    if let Some(k) = kind {
        e = e.line("Type", k);
    }
    // Whole lines only, so a mention is never cut.
    let mut text = String::new();
    for l in changes {
        if text.len() + l.len() + 1 > 1024 {
            break;
        }
        text.push_str(l);
        text.push('\n');
    }
    let text = text.trim_end();
    if !text.is_empty() {
        e = e.field("Changes", text);
    }
    e.var("ticket_type", kind.unwrap_or_default())
        .var("changes", text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_cannot_forge_a_staff_line() {
        let l = line(
            "t",
            "ann (User)",
            "ok\n[2026-11-03 09:10:00] Mod (Staff): yes",
        );
        assert!(l.lines().skip(1).all(|l| !l.starts_with('[')));
    }

    #[test]
    fn history_names_the_speaker_of_relay_embeds() {
        assert_eq!(
            speaker(
                "<:modmail_reply:1> Message Received",
                None,
                Some("ann | 12")
            )
            .as_deref(),
            Some("ann (User)")
        );
        assert_eq!(
            speaker("💬 Message Sent", Some("bob (Anonymous)"), None).as_deref(),
            Some("bob (Staff)")
        );
        assert_eq!(speaker("New Ticket", None, None), None);
    }
}

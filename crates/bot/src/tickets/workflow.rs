//! Staff work on an open ticket (Tickets 2.0): claim, unclaim, reassign, priority, notes and info.
//! Every change is a compare-and-set on the row and is recorded in `ticket_events` without text.
//! `Ok(Err(line))` is a refusal to show, `Ok(Ok(None))` a change with nothing more to say; callers
//! have checked that the actor is ticket staff.
use super::*;

pub(super) const PRIORITIES: [&str; 4] = ["low", "normal", "high", "urgent"];

type Outcome = Result<Result<Option<Line>, Line>>;

pub(crate) async fn claim(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    actor: Id<UserMarker>,
) -> Outcome {
    let won: Option<i32> = sqlx::query_scalar(
        "UPDATE tickets SET claimed_by = $2, claimed_at = now()
         WHERE channel_id = $1 AND status = 'open' AND claimed_by IS NULL AND guild_id = $3 RETURNING number",
    )
    .bind(ch.get() as i64)
    .bind(actor.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some(number) = won else {
        return Ok(Err(match holder(app, gid, ch).await? {
            Some(h) if h == actor => "reply.ticket.claimed_self".into(),
            Some(h) => Line::with(
                "reply.ticket.claimed_other",
                &[("user", &format!("<@{h}>"))],
            ),
            None => "reply.ticket.claim_failed".into(),
        }));
    };
    event(app, gid, number, Some(actor), "claimed", json!({})).await;
    let user = format!("<@{actor}>");
    announce(
        app,
        gid,
        ch,
        Line::with("reply.ticket.shown_claimed", &[("user", &user)]),
    )
    .await;
    let log = claimed_log(number, &user);
    // The change is made: a failed config read only skips the log line.
    if let Ok(c) = app.cfg(gid.get()).await {
        log_update(app, gid, &c.tickets, log).await;
    }
    Ok(Ok(Some("reply.ticket.claimed".into())))
}

/// Only the one who claimed it gives it up; Owner, Extra Owner and Trusted Admin (`admin`) may
/// take anyone's claim off.
pub(crate) async fn unclaim(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    actor: Id<UserMarker>,
    admin: bool,
) -> Outcome {
    let was = match holder(app, gid, ch).await? {
        None => return Ok(Err("reply.ticket.not_claimed".into())),
        Some(h) if h != actor && !admin => {
            let user = format!("<@{h}>");
            return Ok(Err(Line::with(
                "reply.ticket.claim_not_yours",
                &[("user", &user)],
            )));
        }
        Some(h) => h,
    };
    let done: Option<i32> = sqlx::query_scalar(
        "UPDATE tickets SET claimed_by = NULL, claimed_at = NULL
         WHERE channel_id = $1 AND status = 'open' AND claimed_by = $2 AND guild_id = $3 RETURNING number",
    )
    .bind(ch.get() as i64)
    .bind(was.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some(number) = done else {
        return Ok(Err("reply.ticket.claim_changed".into()));
    };
    event(
        app,
        gid,
        number,
        Some(actor),
        "unclaimed",
        json!({"from": was.to_string()}),
    )
    .await;
    announce(app, gid, ch, "reply.ticket.shown_unclaimed".into()).await;
    let log = handed_log(number, "unclaimed", actor, was);
    // The change is made: a failed config read only skips the log line.
    if let Ok(c) = app.cfg(gid.get()).await {
        log_update(app, gid, &c.tickets, log).await;
    }
    Ok(Ok(None))
}

/// Gives the ticket to other staff: Owner, Extra Owner and Trusted Admin only (checked by the
/// caller), and only to someone with Manage Tickets.
pub(crate) async fn reassign(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    actor: Id<UserMarker>,
    to: Id<UserMarker>,
) -> Outcome {
    let owner = app.guild_info(gid).await?.owner;
    let member = http_err::member(&app.http, gid, to).await?;
    if member.as_ref().is_none_or(|m| m.user.bot)
        || !app.level(gid, owner, to).await?.has("tickets")
    {
        return Ok(Err("reply.ticket.reassign_no_permit".into()));
    }
    let done: Option<i32> = sqlx::query_scalar(
        "UPDATE tickets SET claimed_by = $2, claimed_at = now()
         WHERE channel_id = $1 AND status = 'open' AND guild_id = $3 RETURNING number",
    )
    .bind(ch.get() as i64)
    .bind(to.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some(number) = done else {
        return Ok(Err("reply.ticket.reassign_failed".into()));
    };
    event(
        app,
        gid,
        number,
        Some(actor),
        "reassigned",
        json!({"to": to.to_string()}),
    )
    .await;
    let shown = Line::with(
        "reply.ticket.shown_reassigned",
        &[("user", &format!("<@{to}>"))],
    );
    announce(app, gid, ch, shown).await;
    let log = handed_log(number, "reassigned", actor, to);
    // The change is made: a failed config read only skips the log line.
    if let Ok(c) = app.cfg(gid.get()).await {
        log_update(app, gid, &c.tickets, log).await;
    }
    Ok(Ok(None))
}

pub(crate) async fn priority(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    actor: Id<UserMarker>,
    arg: &str,
) -> Outcome {
    let Some(level) = PRIORITIES.iter().position(|p| *p == arg.to_lowercase()) else {
        let choices = PRIORITIES.join(", ");
        return Ok(Err(Line::with(
            "reply.ticket.choices",
            &[("choices", &choices)],
        )));
    };
    let done: Option<i32> = sqlx::query_scalar(
        "UPDATE tickets SET priority = $2 WHERE channel_id = $1 AND status = 'open' AND guild_id = $3 RETURNING number",
    )
    .bind(ch.get() as i64)
    .bind(level as i16)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some(number) = done else {
        return Ok(Err("reply.ticket.not_open".into()));
    };
    event(
        app,
        gid,
        number,
        Some(actor),
        "priority",
        json!({"priority": PRIORITIES[level]}),
    )
    .await;
    let priority = [("priority", PRIORITIES[level])];
    Ok(Ok(Some(Line::with("reply.ticket.priority_set", &priority))))
}

/// A staff note, never sent to the member. A DM ticket keeps it in its channel (the member is not
/// there); a panel ticket's member reads the channel, so the note goes to the ticket log.
pub(crate) async fn note(
    app: &App,
    gid: Id<GuildMarker>,
    here: &Here,
    m: &MessageCreate,
    text: &str,
    log: Option<Id<ChannelMarker>>,
) -> Outcome {
    if text.is_empty() {
        return Ok(Err("reply.ticket.note_empty".into()));
    }
    // The member reads a panel ticket's channel: take the command (with its text) away first,
    // also when the note is refused below.
    let gone = app.http.delete_message(m.channel_id, m.id).await;
    let Some(to) = note_target(here, m.channel_id, log) else {
        return Ok(Err(if gone.is_ok() {
            "reply.ticket.note_no_log_deleted".into()
        } else {
            "reply.ticket.note_no_log_kept".into()
        }));
    };
    post_note(app, gid, here, m.channel_id, &m.author, text, to).await?;
    match gone {
        Err(e) if here.panel.is_some() => {
            tracing::warn!(?e, "deleting a ticket note command failed");
            Ok(Err("reply.ticket.note_not_deleted".into()))
        }
        _ => Ok(Ok(None)),
    }
}

/// A note typed in the Add Note form; nothing is left in the channel to delete.
pub(crate) async fn note_form(
    app: &App,
    gid: Id<GuildMarker>,
    here: &Here,
    ch: Id<ChannelMarker>,
    staff: &User,
    text: &str,
    log: Option<Id<ChannelMarker>>,
) -> Outcome {
    if text.is_empty() {
        return Ok(Err("reply.ticket.note_form_empty".into()));
    }
    let Some(to) = note_target(here, ch, log) else {
        return Ok(Err("reply.ticket.note_no_log".into()));
    };
    post_note(app, gid, here, ch, staff, text, to).await?;
    Ok(Ok(Some("reply.ticket.note_saved".into())))
}

/// Where a note goes; `None` for a panel ticket with no ticket log.
fn note_target(
    here: &Here,
    ch: Id<ChannelMarker>,
    log: Option<Id<ChannelMarker>>,
) -> Option<Id<ChannelMarker>> {
    match (&here.panel, log) {
        (None, _) => Some(ch),
        (Some(_), log) => log,
    }
}

async fn post_note(
    app: &App,
    gid: Id<GuildMarker>,
    here: &Here,
    ch: Id<ChannelMarker>,
    staff: &User,
    text: &str,
    to: Id<ChannelMarker>,
) -> Result<()> {
    let title = format!("{TICKET} Staff Note");
    let mut e = author(stamp(emb(&title, text, ORANGE)), &staff.name, avatar(staff));
    if here.panel.is_some() {
        e = e.field(EmbedFieldBuilder::new(
            "Ticket",
            format!("#{} <#{ch}>", here.number),
        ));
    }
    say(app, to, e.build()).await?;
    event(app, gid, here.number, Some(staff.id), "note", json!({})).await;
    Ok(())
}

/// Number, kind, who claimed it, priority and times, for staff; None when `ch` is no ticket.
pub(crate) async fn summary(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
) -> Result<Option<Embed>> {
    // number, panel, claimed by, priority, opened, last message, first staff answer (seconds)
    type Row = (i32, Option<String>, Option<i64>, i16, i64, i64, Option<i64>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT number, panel, claimed_by, priority, extract(epoch FROM opened_at)::bigint,
                extract(epoch FROM last_activity_at)::bigint,
                extract(epoch FROM first_staff_response_at - opened_at)::bigint
         FROM tickets WHERE channel_id = $1 AND guild_id = $2",
    )
    .bind(ch.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some((number, panel, claimed, prio, opened, active, first)) = row else {
        return Ok(None);
    };
    let f = |n: &str, v: String| EmbedFieldBuilder::new(n, v).inline();
    Ok(Some(
        emb(&format!("{TICKET} Ticket #{number}"), "", BLUE)
            .field(f(
                "Kind",
                panel.map_or("DM".into(), |p| format!("Panel: {p}")),
            ))
            .field(f(
                "Claimed by",
                claimed.map_or("*Nobody*".into(), |c| format!("<@{c}>")),
            ))
            .field(f("Priority", PRIORITIES[prio.clamp(0, 3) as usize].into()))
            .field(f("Opened", format!("<t:{opened}:R>")))
            .field(f("Last message", format!("<t:{active}:R>")))
            .field(f(
                "First staff answer",
                first.map_or("*Not yet*".into(), |s| format!("after {} min", s / 60)),
            ))
            .build(),
    ))
}

pub(super) async fn holder(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
) -> Result<Option<Id<UserMarker>>> {
    let h: Option<Option<i64>> = sqlx::query_scalar(
        "SELECT claimed_by FROM tickets WHERE channel_id = $1 AND guild_id = $2",
    )
    .bind(ch.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    Ok(h.flatten().map(|h| Id::new(h as u64)))
}

/// Sets the ticket type (one of `tickets.types`). The member may pick it once while none is set;
/// staff may change it any time (`staff`). Logged to the ticket log and `ticket_events`.
pub(crate) async fn set_kind(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    actor: Id<UserMarker>,
    kind: &str,
    staff: bool,
) -> Outcome {
    let cfg = app.cfg(gid.get()).await?;
    if !cfg.tickets.types.iter().any(|t| t == kind) {
        return Ok(Err("reply.ticket.type_gone".into()));
    }
    // One statement: the old type and the change, so two picks at once cannot both win as "first".
    let done: Option<(i32, Option<String>)> = sqlx::query_as(
        "UPDATE tickets t SET kind = $2 FROM (SELECT number, kind FROM tickets
           WHERE channel_id = $1 AND status = 'open' AND guild_id = $5 FOR UPDATE) old
         WHERE t.channel_id = $1 AND t.status = 'open' AND t.number = old.number
           AND ($3 OR (t.kind IS NULL AND t.user_id = $4))
         RETURNING t.number, old.kind",
    )
    .bind(ch.get() as i64)
    .bind(kind)
    .bind(staff)
    .bind(actor.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some((number, from)) = done else {
        return Ok(Err(if staff {
            "reply.ticket.not_open".into()
        } else {
            "reply.ticket.type_staff_only".into()
        }));
    };
    event(
        app,
        gid,
        number,
        Some(actor),
        "type",
        json!({"from": from, "to": kind}),
    )
    .await;
    let user = format!("<@{actor}>");
    let (log, shown) = match &from {
        Some(f) => (
            type_log(number, &user, Some(f.as_str()), kind),
            Line::with(
                "reply.ticket.shown_type_changed",
                &[("user", &user), ("old_type", f), ("type", kind)],
            ),
        ),
        None => (
            type_log(number, &user, None, kind),
            Line::with(
                "reply.ticket.shown_type_set",
                &[("user", &user), ("type", kind)],
            ),
        ),
    };
    rename_for_kind(app, ch, from.as_deref(), kind).await;
    announce(app, gid, ch, shown).await;
    log_update(app, gid, &cfg.tickets, log).await;
    Ok(Ok(Some(Line::with(
        "reply.ticket.type_set",
        &[("type", kind)],
    ))))
}

/// Puts the type into the channel name (`kind_name`). Discord allows two renames per ten minutes
/// per channel, so a refused or slow one is only logged: the type change itself stands.
async fn rename_for_kind(app: &App, ch: Id<ChannelMarker>, old: Option<&str>, kind: &str) {
    let rename = async {
        let now = app.http.channel(ch).await?.model().await?.name;
        let Some(now) = now else {
            return anyhow::Ok(());
        };
        let name = kind_name(&now, old, kind);
        if name != now {
            app.http.update_channel(ch).name(&name).await?;
        }
        Ok(())
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), rename).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(?e, %ch, "renaming the ticket channel for its type failed"),
        Err(_) => tracing::warn!(%ch, "renaming the ticket channel for its type timed out"),
    }
}

/// A change to an open ticket (type, who handles it) in the ticket log.
pub(crate) async fn log_update(app: &App, gid: Id<GuildMarker>, t: &TicketsCfg, log: Log) {
    let Some(ch) = t.log_channel else { return };
    if let Err(e) = crate::events::send_private(app, gid, ch, log, None).await {
        tracing::warn!(?e, "ticket update log failed");
    }
}

/// "Ticket Updated" with entry `log.ticket.<slug>`: `text` follows "Ticket #N: ".
fn update_log(number: i32, slug: &str, text: &str, by: &str) -> Log {
    Log::new("Ticket Updated", BLUE)
        .entry(&format!("log.ticket.{slug}"))
        .text(&format!("Ticket #{number}: {text}"))
        .var("ticket", &number.to_string())
        .var("by", by)
}

pub(crate) fn claimed_log(number: i32, by: &str) -> Log {
    update_log(number, "claimed", &format!("{by} claimed this ticket."), by)
}

/// The claim removed (`unclaimed`) or the ticket given to `member` (`reassigned`).
pub(crate) fn handed_log(
    number: i32,
    slug: &str,
    actor: Id<UserMarker>,
    member: Id<UserMarker>,
) -> Log {
    let (by, m) = (format!("<@{actor}>"), format!("<@{member}>"));
    let text = match slug {
        "unclaimed" => format!("{by} removed the claim of {m}."),
        _ => format!("{by} gave this ticket to {m}."),
    };
    update_log(number, slug, &text, &by).var("member", &m)
}

/// The ticket type changed from `from`, or set for the first time.
pub(crate) fn type_log(number: i32, by: &str, from: Option<&str>, to: &str) -> Log {
    let (slug, text) = match from {
        Some(f) => (
            "type_changed",
            format!("{by} changed the ticket type from **{f}** to **{to}**."),
        ),
        None => ("type_set", format!("{by} set the ticket type to **{to}**.")),
    };
    update_log(number, slug, &text, by)
        .var("before", from.unwrap_or_default())
        .var("after", to)
}

/// The ticket's type, `None` while unset or not an open ticket.
pub(crate) async fn kind_of(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT kind FROM tickets WHERE channel_id = $1 AND status = 'open' AND guild_id = $2",
    )
    .bind(ch.get() as i64)
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?
    .flatten())
}

/// A line everyone in the ticket sees; it mentions but never pings.
async fn announce(app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>, line: Line) {
    if let Err(e) = line.send(app, gid, ch).await {
        tracing::warn!(?e, %ch, "ticket notice failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The member picks a type once, staff change it, an unknown type is refused, and each
    /// change is an event with only the type names.
    #[tokio::test]
    async fn ticket_type_member_once_staff_any_time() {
        let Some(t) = crate::test_support::T::new(944).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({"tickets": {"enabled": true}}))
            .await;
        let gid = t.g(0);
        t.exec("INSERT INTO tickets (guild_id, number, user_id, channel_id) VALUES ($1, 1, $1 * 10 + 1, $1 * 10 + 9)").await;
        let (member, other, mod_) = (
            Id::new(t.base * 10 + 1),
            Id::new(t.base * 10 + 2),
            Id::new(t.base * 10 + 3),
        );
        let ch = Id::new(t.base * 10 + 9);
        assert!(
            set_kind(&t.app, gid, ch, other, "Report", false)
                .await
                .unwrap()
                .is_err(),
            "not their ticket"
        );
        assert!(
            set_kind(&t.app, gid, ch, member, "Nope", false)
                .await
                .unwrap()
                .is_err(),
            "unknown type"
        );
        assert!(set_kind(&t.app, gid, ch, member, "Report", false)
            .await
            .unwrap()
            .is_ok());
        assert!(
            set_kind(&t.app, gid, ch, member, "Appeal", false)
                .await
                .unwrap()
                .is_err(),
            "the member picks once"
        );
        assert!(set_kind(&t.app, gid, ch, mod_, "Appeal", true)
            .await
            .unwrap()
            .is_ok());
        assert_eq!(
            kind_of(&t.app, gid, ch).await.unwrap().as_deref(),
            Some("Appeal")
        );
        assert_eq!(
            t.count("SELECT count(*) FROM ticket_events WHERE guild_id = $1 AND action = 'type' AND details = '{\"from\": \"Report\", \"to\": \"Appeal\"}'").await,
            1
        );
    }

    /// Claim is a race one staff member wins, only the holder (or an admin) unclaims, events keep
    /// no text, and auto-close stops when the ticket had a new message.
    #[tokio::test]
    async fn claim_race_unclaim_events_and_auto_close() {
        let Some(t) = crate::test_support::T::new(949).await else {
            return;
        };
        const CH: u64 = 611;
        t.server_config(0, t.owner(), json!({"tickets": {"enabled": true}}))
            .await;
        let gid = t.g(0);
        t.exec("INSERT INTO tickets (guild_id, number, user_id, channel_id, last_activity_at) VALUES ($1, 1, $1 * 10 + 1, 611, now() - interval '5 hours')").await;
        let (a, b) = (Id::new(t.base * 10 + 2), Id::new(t.base * 10 + 3));
        let ch = Id::new(CH);
        let (ra, rb) = tokio::join!(claim(&t.app, gid, ch, a), claim(&t.app, gid, ch, b));
        let wins = [ra.unwrap(), rb.unwrap()];
        assert_eq!(wins.iter().filter(|r| r.is_ok()).count(), 1);
        let (holder, other) = if wins[0].is_ok() { (a, b) } else { (b, a) };
        assert!(unclaim(&t.app, gid, ch, other, false)
            .await
            .unwrap()
            .is_err());
        assert!(unclaim(&t.app, gid, ch, holder, false)
            .await
            .unwrap()
            .is_ok());
        assert!(priority(&t.app, gid, ch, a, "URGENT")
            .await
            .unwrap()
            .is_ok());
        assert!(priority(&t.app, gid, ch, a, "soon").await.unwrap().is_err());
        assert_eq!(
            t.count("SELECT count(*) FROM tickets WHERE guild_id = $1 AND priority = 3 AND claimed_by IS NULL").await,
            1
        );
        // Who and what only: no event has a field beyond the ones the code writes.
        assert_eq!(
            t.count("SELECT count(*) FROM ticket_events WHERE guild_id = $1")
                .await,
            3
        );
        assert_eq!(
            t.count("SELECT count(*) FROM ticket_events WHERE guild_id = $1 AND details - 'from' - 'priority' <> '{}'").await,
            0
        );
        // A message after the sweep read the ticket: the close does nothing.
        let bot: twilight_model::user::User =
            serde_json::from_value(json!({"id": crate::test_support::ME.to_string(),
            "username": "bot", "discriminator": "0", "avatar": null, "bot": true}))
            .unwrap();
        super::super::touch(&t.app, gid, ch, false).await.unwrap();
        super::super::staff::close(
            &t.app,
            gid,
            ch,
            Id::new(t.base * 10 + 1),
            &bot,
            "idle",
            false,
            4,
        )
        .await
        .unwrap();
        let open = "SELECT count(*) FROM tickets WHERE guild_id = $1 AND status = 'open'";
        assert_eq!(t.count(open).await, 1);
        assert_eq!(t.calls("Closing ticket"), 0);
        // Idle long enough: closed, and the bot is recorded as no person.
        t.exec(
            "UPDATE tickets SET last_activity_at = now() - interval '5 hours' WHERE guild_id = $1",
        )
        .await;
        super::super::staff::close(
            &t.app,
            gid,
            ch,
            Id::new(t.base * 10 + 1),
            &bot,
            "idle",
            false,
            4,
        )
        .await
        .unwrap();
        assert_eq!(t.count(open).await, 0);
        assert_eq!(
            t.count("SELECT count(*) FROM ticket_events WHERE guild_id = $1 AND action = 'closed' AND actor_id IS NULL").await,
            1
        );
        t.exec("DELETE FROM tickets WHERE guild_id = $1").await;
        t.cleanup().await;
    }
}

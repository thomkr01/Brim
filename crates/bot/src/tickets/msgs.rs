//! The messages a ticket sends, built in one place so the dashboard previews show the same ones.
use super::*;

/// The DM asking the member to confirm the server before their first message goes out.
pub(crate) fn confirm(s: &Server, msg: Id<MessageMarker>) -> (Embed, Component) {
    let id = s.id.to_string();
    let e = s.tpl("ticket.confirm", &[("server", &s.name), ("server_id", &id)]);
    let row = action_row(vec![
        button(
            "Send",
            format!("tk:ok:{}:{msg}", s.id),
            ButtonStyle::Success,
        ),
        button(
            "Other server",
            format!("tk:other:{msg}"),
            ButtonStyle::Secondary,
        ),
        button("Cancel", "tk:no".into(), ButtonStyle::Danger),
    ]);
    (e.build(), row)
}

/// The member's message: (copy in their DMs, what staff get in the ticket).
pub(crate) fn from_member(s: &Server, u: &User, text: &str) -> (EmbedBuilder, EmbedBuilder) {
    let sent = stamp(s.tpl("ticket.sent", &[("text", text)]));
    let title = format!("{TICKET_REPLY} Message Received");
    (sent, user_footer(stamp(emb(&title, text, GREEN)), u))
}

/// A staff reply: (what the member gets in DMs, the copy in the ticket).
pub(crate) fn from_staff(
    s: &Server,
    staff: &User,
    member: &User,
    text: &str,
    anon: bool,
) -> (EmbedBuilder, EmbedBuilder) {
    let mut to_member = stamp(s.tpl("ticket.reply", &[("text", text)]));
    if !anon {
        to_member = author(to_member, &staff.name, avatar(staff));
    }
    let shown = if anon {
        format!("{} (Anonymous)", staff.name)
    } else {
        staff.name.clone()
    };
    let copy = author(
        stamp(emb(&format!("{TICKET_REPLY} Message Sent"), text, ORANGE)),
        &shown,
        avatar(staff),
    );
    (to_member, user_footer(copy, member))
}

/// The first message in a new ticket, telling staff how to reply.
/// `quarantined`: the member is quarantined there; their ticket is still opened, so they can
/// reach staff.
pub(crate) fn intro(
    pfx: &str,
    command_only: bool,
    u: &User,
    roles: &[Id<RoleMarker>],
    quarantined: bool,
) -> Embed {
    let how = if command_only {
        format!("Type `{pfx}reply <message>` in this channel to reply. All other messages are ignored, and can be used for staff discussion. Use the command `{pfx}close [reason]` to close this ticket. (Command only mode enabled)")
    } else {
        format!("Type a message in this channel to reply. Messages starting with the server prefix `{pfx}` are ignored, and can be used for staff discussion. Use the command `{pfx}close [reason]` to close this ticket.")
    };
    let role_list: String = roles.iter().map(|r| format!("<@&{r}> ")).collect();
    let role_list = match role_list.trim_end() {
        "" => "*None*".to_string(),
        l if l.len() <= 1024 => l.to_string(),
        _ => format!("*{} roles*", roles.len()),
    };
    let e = stamp(emb(&format!("{TICKET_NEW} New Ticket"), &how, BLUE))
        .field(EmbedFieldBuilder::new("User", format!("<@{}> ({})", u.id, u.id)).inline())
        .field(EmbedFieldBuilder::new("Roles", role_list).inline());
    let e = if quarantined {
        e.field(EmbedFieldBuilder::new("Status", "Quarantined").inline())
    } else {
        e
    };
    user_footer(e, u).build()
}

/// The greeting or closing message the server set, sent to the member.
pub(crate) fn notice(s: &Server, closing: bool, text: &str, u: &User, kind: &str) -> Embed {
    let key = if closing {
        "ticket.closing"
    } else {
        "ticket.greeting"
    };
    stamp(s.tpl(key, &[("text", &tags(text, u, kind))])).build()
}

/// The DM telling the member their ticket was closed.
pub(crate) fn closed(s: &Server, reason: &str, staff: Option<&User>) -> Embed {
    let mut e = stamp(s.tpl("ticket.closed", &[("server", &s.name), ("reason", reason)]));
    if let Some(staff) = staff {
        e = author(e, &staff.name, avatar(staff));
    }
    e.build()
}

/// The first message in a panel ticket: who opened it, the panel, the form answers, and how
/// staff handle it. The member reads it too, so it says nothing staff-only.
pub(crate) fn panel_intro(
    pfx: &str,
    p: &brim_core::sections::TicketPanel,
    u: &User,
    roles: &[Id<RoleMarker>],
    quarantined: bool,
    answers: &[String],
) -> Embed {
    let how = format!(
        "<@{}> opened a **{}** ticket. Staff will answer here.\nStaff: write in this channel to answer, `{pfx}claim` or **Claim** to take the ticket, and `{pfx}close [reason]` or **Close** to close it.",
        u.id,
        clip(&p.name, 80)
    );
    let mut e = stamp(emb(&format!("{TICKET_NEW} New Ticket"), &how, BLUE));
    for (f, a) in p.fields.iter().zip(answers) {
        let a = if a.is_empty() {
            "*No answer*"
        } else {
            a.as_str()
        };
        e = e.field(EmbedFieldBuilder::new(clip(&f.label, 256), clip(a, 1024)));
    }
    let role_list: String = roles.iter().map(|r| format!("<@&{r}> ")).collect();
    let role_list = match role_list.trim_end() {
        "" => "*None*".to_string(),
        l if l.len() <= 1024 => l.to_string(),
        _ => format!("*{} roles*", roles.len()),
    };
    e = e.field(EmbedFieldBuilder::new("Roles", role_list).inline());
    if quarantined {
        e = e.field(EmbedFieldBuilder::new("Status", "Quarantined").inline());
    }
    user_footer(e, u).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A saved copy is used, the server footer is added, and an empty member text sends no
    /// empty description.
    #[test]
    fn ticket_messages_use_the_saved_copy() {
        let mut s = Server {
            id: Id::new(7),
            name: "S".into(),
            icon: None,
            own: vec![],
        };
        let (c, _) = confirm(&s, Id::new(1));
        assert!(c
            .footer
            .unwrap()
            .text
            .contains(&format!("{DM_PREFIX}confirmation")));
        let e = closed(&s, "done", None);
        assert!(e.description.as_deref().unwrap().contains("`S`"));
        assert_eq!(e.footer.unwrap().text, "S | 7");
        let (sent, _) = from_member(
            &s,
            &serde_json::from_value(
                json!({"id": "1", "username": "m", "discriminator": "0", "avatar": null}),
            )
            .unwrap(),
            "",
        );
        assert!(sent.build().description.is_none());
        s.own = vec![(
            "ticket.closed".into(),
            json!({"embeds": [{"title": "Bye", "description": "{reason}", "footer": {"text": "mine"}}]}),
        )];
        let e = closed(&s, "done", None);
        assert_eq!(
            (e.title.as_deref(), e.description.as_deref()),
            (Some("Bye"), Some("done"))
        );
        assert_eq!(e.footer.unwrap().text, "mine");
        s.own[0].1["embeds"][0]["footer"]["text"] = "".into();
        assert_eq!(closed(&s, "done", None).footer.unwrap().text, "S | 7");
    }
}

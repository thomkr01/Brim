//! Panel tickets (Tickets 2.0): a member presses a button on a panel message the dashboard posted,
//! may fill in a form, and gets a private channel they share with staff. Nothing is relayed by DM.
//! The staff buttons on the ticket's first message (Claim and Close; after a claim Change
//! Priority, Add Note and Close Ticket) are handled here too.
use super::*;
use brim_core::sections::{button_types, type_hash, TicketPanel};
use twilight_model::{
    application::interaction::modal::{ModalInteractionComponent, ModalInteractionData},
    channel::message::{
        component::{Label, TextInput, TextInputStyle},
        Message, MessageFlags,
    },
    guild::PartialMember,
};

/// What a member may do in their own panel ticket channel.
const MEMBER: Permissions = STAFF;

/// A stable short hash of a panel's form, so a form opened before staff changed it is refused.
fn form_hash(p: &TicketPanel) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for f in &p.fields {
        let part = format!(
            "{}|{}|{}|{:?}|{:?};",
            f.label, f.long, f.required, f.min, f.max
        );
        for b in part.bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
    }
    format!("{h:08x}")
}

/// Whether `msg` is a panel message posted for this server that offers `panel`.
/// Whether `msg` is a message of ours that offers `panel`: a posted ticket panel, or a copy of a
/// saved message whose "Open a ticket" button names it (as that copy was last sent or edited).
async fn posted(app: &App, gid: Id<GuildMarker>, msg: u64, panel: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ticket_panel_posts
                        WHERE message_id = $1 AND guild_id = $2 AND $3 = ANY (panels))
             OR EXISTS (SELECT 1 FROM message_sends
                        WHERE posted_id = $1 AND guild_id = $2 AND jsonb_path_exists(body,
                            '$.components[*].components[*] ? (@.action == $a)',
                            jsonb_build_object('a', 'ticket:' || $3)))",
    )
    .bind(msg as i64)
    .bind(gid.get() as i64)
    .bind(panel)
    .fetch_one(&app.pool)
    .await?)
}

#[allow(deprecated)]
fn modal(p: &TicketPanel, msg: u64, type_hash: Option<&str>) -> InteractionResponse {
    let rows = p
        .fields
        .iter()
        .enumerate()
        .map(|(n, f)| {
            Component::Label(Label {
                id: None,
                description: None,
                label: clip(&f.label, 45),
                component: Box::new(Component::TextInput(TextInput {
                    id: None,
                    custom_id: format!("f{n}"),
                    label: None,
                    // An answer is one embed field (1024) in the ticket channel.
                    max_length: Some(f.max.unwrap_or(1000).min(1024) as u16),
                    min_length: f.min.map(|m| m.min(1024) as u16),
                    placeholder: None,
                    required: Some(f.required),
                    style: if f.long {
                        TextInputStyle::Paragraph
                    } else {
                        TextInputStyle::Short
                    },
                    value: None,
                })),
            })
        })
        .collect();
    InteractionResponse {
        kind: InteractionResponseType::Modal,
        data: Some(InteractionResponseData {
            custom_id: Some(match type_hash {
                Some(h) => format!("tk2:form:{}:{msg}:{}:{h}", p.id, form_hash(p)),
                None => format!("tk2:form:{}:{msg}:{}", p.id, form_hash(p)),
            }),
            title: Some(clip(&p.name, 45)),
            components: Some(rows),
            ..Default::default()
        }),
    }
}

/// The answers in field order (`f0`, `f1`, ...), wherever the modal nests the inputs.
fn answers(m: &ModalInteractionData, count: usize) -> Vec<String> {
    fn walk(c: &ModalInteractionComponent, out: &mut Vec<(String, String)>) {
        match c {
            ModalInteractionComponent::TextInput(t) => {
                out.push((t.custom_id.clone(), t.value.clone()))
            }
            ModalInteractionComponent::ActionRow(r) => {
                r.components.iter().for_each(|c| walk(c, out))
            }
            ModalInteractionComponent::Label(l) => walk(&l.component, out),
            _ => {}
        }
    }
    let mut got = vec![];
    m.components.iter().for_each(|c| walk(c, &mut got));
    (0..count)
        .map(|n| {
            let id = format!("f{n}");
            got.iter()
                .find(|(k, _)| *k == id)
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// The panel buttons, menu, form, and the staff buttons. False for other interactions.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let (id, values) = match &i.data {
        Some(InteractionData::MessageComponent(c)) => (c.custom_id.as_str(), c.values.as_slice()),
        Some(InteractionData::ModalSubmit(m)) => (m.custom_id.as_str(), &[][..]),
        _ => return Ok(false),
    };
    let Some(rest) = id.strip_prefix("tk2:") else {
        return Ok(false);
    };
    let (Some(gid), Some(member), Some(user)) = (i.guild_id, i.member.as_ref(), i.author()) else {
        return Ok(true);
    };
    let client = app.http.interaction(app.me.cast());
    let parts: Vec<&str> = rest.split(':').collect();
    // The panel and the message it was pressed on; the form carries the message id itself.
    // A button per type adds the type's hash (`brim_core::sections::type_hash`).
    let (panel_id, msg, th) = match (parts.as_slice(), i.message.as_ref()) {
        (["open", p], Some(m)) => (p.to_string(), m.id.get(), None),
        (["open", p, h], Some(m)) => (p.to_string(), m.id.get(), Some(h.to_string())),
        (["menu"], Some(m)) => match values.first().map(|v| v.split_once(':')) {
            Some(Some((p, h))) => (p.to_string(), m.id.get(), Some(h.to_string())),
            Some(None) => (values[0].clone(), m.id.get(), None),
            None => return Ok(true),
        },
        (["form", p, m, _], _) => match m.parse() {
            Ok(m) => (p.to_string(), m, None),
            Err(_) => return Ok(true),
        },
        (["form", p, m, _, h], _) => match m.parse() {
            Ok(m) => (p.to_string(), m, Some(h.to_string())),
            Err(_) => return Ok(true),
        },
        (
            [w @ ("claim" | "close" | "closeform" | "prio" | "prioset" | "note" | "noteform"
            | "kind" | "kindset")],
            _,
        ) => {
            staff_button(app, i, gid, user, w, values).await?;
            return Ok(true);
        }
        _ => return Ok(true),
    };
    let cfg = app.cfg(gid.get()).await?;
    let panel = cfg
        .tickets
        .panels
        .iter()
        .find(|p| p.id == panel_id && p.enabled)
        .cloned();
    // Every answer here is a `reply.ticket.*` entry of this server.
    let said =
        async |key: &str, vars: &[(&str, &str)]| crate::reply::say(app, gid, key, vars).await;
    let reply = |s: crate::reply::Said| InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            content: Some(s.text).filter(|t| !t.is_empty()),
            embeds: Some(s.embeds),
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    };
    // A forged or old button: the message must be one we posted here, for a panel still on.
    let Some(panel) = panel.filter(|_| cfg.tickets.enabled) else {
        let r = reply(said("reply.ticket.panel_unused", &[]).await);
        client.create_response(i.id, &i.token, &r).await?;
        return Ok(true);
    };
    if !posted(app, gid, msg, &panel.id).await? {
        let r = reply(said("reply.ticket.panel_unused", &[]).await);
        client.create_response(i.id, &i.token, &r).await?;
        return Ok(true);
    }
    // The type: the one the pressed button names (the list may have changed since the message
    // was posted), else the panel's preset one.
    let types = &cfg.tickets.types;
    let kind = match &th {
        Some(h) => match types.iter().find(|t| &type_hash(t) == h) {
            Some(t) if button_types(&panel, types, cfg.tickets.button_per_type).len() > 1 => {
                t.as_str()
            }
            _ => {
                let r = reply(said("reply.ticket.form_changed", &[]).await);
                client.create_response(i.id, &i.token, &r).await?;
                return Ok(true);
            }
        },
        None if types.contains(&panel.kind) => panel.kind.as_str(),
        None => "",
    };
    let submitted = match &i.data {
        Some(InteractionData::ModalSubmit(m)) => {
            if parts.get(3) != Some(&form_hash(&panel).as_str()) {
                let r = reply(said("reply.ticket.form_changed", &[]).await);
                client.create_response(i.id, &i.token, &r).await?;
                return Ok(true);
            }
            Some(answers(m, panel.fields.len()))
        }
        _ => None,
    };
    // The form must be the first answer; the checks run again once it is sent.
    if submitted.is_none() && !panel.fields.is_empty() {
        // Refused members are told before they fill in the form (owner decision 11-15).
        if let Some(why) = gate(app, gid, &cfg, user).await? {
            let r = reply(said(why, &[]).await);
            client.create_response(i.id, &i.token, &r).await?;
            return Ok(true);
        }
        client
            .create_response(i.id, &i.token, &modal(&panel, msg, th.as_deref()))
            .await?;
        return Ok(true);
    }
    let defer = InteractionResponse {
        kind: InteractionResponseType::DeferredChannelMessageWithSource,
        data: Some(InteractionResponseData {
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    };
    client.create_response(i.id, &i.token, &defer).await?;
    let answers = submitted.unwrap_or_default();
    let told = match open(app, gid, &cfg, &panel, kind, user, member, &answers).await {
        Ok(Ok(ch)) => said("reply.ticket.opened", &[("channel", &format!("<#{ch}>"))]).await,
        Ok(Err(why)) => said(why, &[]).await,
        Err(e) => {
            tracing::warn!(?e, %gid, panel = panel.id, "opening a panel ticket failed");
            said("reply.ticket.error_generic", &[]).await
        }
    };
    client
        .update_response(&i.token)
        .content(Some(told.text.as_str()).filter(|t| !t.is_empty()))
        .embeds(Some(&told.embeds))
        .await?;
    Ok(true)
}

/// A private answer made of the short reply entry `key`.
async fn answer(app: &App, gid: Id<GuildMarker>, key: &str) -> InteractionResponse {
    let s = crate::reply::say(app, gid, key, &[]).await;
    InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            content: Some(s.text).filter(|t| !t.is_empty()),
            embeds: Some(s.embeds),
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    }
}

/// Who may not open a ticket now, checked before the form is shown and again once it is sent.
async fn gate(
    app: &App,
    gid: Id<GuildMarker>,
    cfg: &crate::app::GuildCfg,
    user: &User,
) -> Result<Option<&'static str>> {
    // The key of the reply entry that tells the member why not.
    let t = &cfg.tickets;
    if t.blocked.contains(&user.id) {
        return Ok(Some("reply.ticket.blacklisted_panel"));
    }
    if t.paused {
        return Ok(Some("reply.ticket.paused_panel"));
    }
    if crate::lockdown::raid_or_lock(app, gid).await? {
        return Ok(Some("reply.ticket.lockdown_panel"));
    }
    if dm::quarantined_with_reason(app, gid, user.id).await? {
        return Ok(Some("reply.ticket.quarantined_panel"));
    }
    Ok(None)
}

/// Opens a panel ticket: the same refusals as a DM ticket, then a private channel for the member
/// and staff. `Err(text)` tells the member why not.
#[allow(clippy::too_many_arguments)]
async fn open(
    app: &App,
    gid: Id<GuildMarker>,
    cfg: &crate::app::GuildCfg,
    p: &TicketPanel,
    kind: &str,
    user: &User,
    member: &PartialMember,
    answers: &[String],
) -> Result<Result<Id<ChannelMarker>, &'static str>> {
    let t = &cfg.tickets;
    if let Some(why) = gate(app, gid, cfg, user).await? {
        return Ok(Err(why));
    }
    if dm::filtered(app, cfg, &answers.join("\n")) && !dm::bypasses(app, gid, user.id).await {
        return Ok(Err("reply.ticket.answers_filtered"));
    }
    // A deleted or full panel category must not stop every ticket: use the ticket category.
    let own = match p.category {
        Some(c) if category_live(app, gid, c).await? => Some(c),
        gone => {
            if let Some(old) = gone {
                tracing::warn!(%gid, panel = %p.id, %old, "panel category is gone, ticket made in the ticket category");
            }
            None
        }
    };
    let category = match own {
        Some(c) if !full(app, gid, c).await? => c,
        _ => match ensure_category(app, gid).await? {
            Some(c) if !full(app, gid, c).await? => c,
            Some(_) => {
                categories_full(app, gid).await;
                return Ok(Err("reply.ticket.categories_full"));
            }
            None => return Ok(Err("reply.ticket.not_taking")),
        },
    };
    let number = match claim_number(&app.pool, gid, user.id, Some((&p.id, p.per_member))).await? {
        Claim::Number(n) => n,
        Claim::Open => return Ok(Err("reply.ticket.panel_cap")),
        other => return Ok(Err(refusal(&other))),
    };
    let g = gid.get() as i64;
    let mut ows = overwrites(gid, app.me.get(), &staff_roles(app, gid).await?);
    ows.push(PermissionOverwrite {
        allow: MEMBER,
        deny: Permissions::empty(),
        id: user.id.cast(),
        kind: PermissionOverwriteType::Member,
    });
    let made = app
        .http
        .create_guild_channel(
            gid,
            &kind_name(&format!("{}-{number:04}", p.id), None, kind),
        )
        .kind(ChannelType::GuildText)
        .parent_id(category)
        .permission_overwrites(&ows)
        .await;
    let made = match made {
        Ok(r) => r.model().await.map_err(anyhow::Error::from),
        Err(e) => Err(e.into()),
    };
    let ch = match made {
        Ok(c) => c.id,
        Err(e) => {
            tracing::warn!(?e, %gid, "making a panel ticket channel failed");
            sqlx::query("DELETE FROM tickets WHERE guild_id = $1 AND number = $2")
                .bind(g)
                .bind(number)
                .execute(&app.pool)
                .await?;
            return Ok(Err("reply.ticket.channel_failed_panel"));
        }
    };
    let bound = sqlx::query(
        "UPDATE tickets SET channel_id = $3, status = 'open', kind = NULLIF($5, '')
         WHERE guild_id = $1 AND number = $2 AND user_id = $4 AND status = 'creating'",
    )
    .bind(g)
    .bind(number)
    .bind(ch.get() as i64)
    .bind(user.id.get() as i64)
    .bind(kind)
    .execute(&app.pool)
    .await?
    .rows_affected();
    if bound == 0 {
        best_effort(app.http.delete_channel(ch).await, "unbound ticket channel");
        return Ok(Err("reply.ticket.open_failed_panel"));
    }
    let details = json!({"source": "panel", "panel": p.id, "fields": answers.len(), "type": kind});
    event(app, gid, number, Some(user.id), "opened", details).await;
    if let Some(log) = t.log_channel {
        let e = super::new_ticket_log(user, Some(&p.name), Some(kind));
        if let Err(e) = crate::events::send_private(app, gid, log, e, None).await {
            tracing::warn!(?e, "ticket log failed");
        }
    }
    let quarantined = crate::antinuke::quarantined(app, gid, user.id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(?e, "quarantine check for a ticket intro failed");
            false
        });
    let e = panel_intro(
        &cfg.misc.prefix,
        p,
        user,
        &member.roles,
        quarantined,
        answers,
    );
    let mut am = AllowedMentions {
        users: vec![user.id],
        ..Default::default()
    };
    let mut content = format!("<@{}>", user.id);
    let mut ping = live_ids(app, gid, &p.ping_roles).await;
    if t.ping_staff {
        // The ticket is open already: a failed lookup only skips the staff ping.
        ping.extend(ping_staff_roles(app, gid).await.unwrap_or_else(|e| {
            tracing::warn!(?e, "reading the staff ping roles failed");
            vec![]
        }));
    }
    ping.sort_unstable();
    ping.dedup();
    // Never @everyone from a panel, whatever an old config says.
    for r in ping.into_iter().filter(|r| r.get() != gid.get()) {
        content.push_str(&format!(" <@&{r}>"));
        am.roles.push(r);
    }
    match app
        .http
        .create_message(ch)
        .content(&content)
        .embeds(&[e])
        .components(&intro(false, pick(&t.types, kind)))
        .allowed_mentions(Some(&am))
        .await
    {
        // The ticket is open now: a failure from here on is logged, not shown as "no ticket".
        Ok(r) => match r.model().await {
            Ok(m) => {
                best_effort(app.http.create_pin(ch, m.id).await, "pin ticket intro");
            }
            Err(e) => tracing::warn!(?e, %ch, "reading the panel ticket intro failed"),
        },
        Err(e) => tracing::warn!(?e, %ch, "panel ticket intro failed"),
    }
    // The panel's own greeting, else the server's (empty there too: none).
    let greeting = if p.greeting.is_empty() {
        &t.greeting
    } else {
        &p.greeting
    };
    if !greeting.is_empty() {
        let text = tags(greeting, user, kind);
        let e = crate::reply::bot_embed_for(app, gid, "ticket.greeting", &[("text", &text)]).await;
        if let Err(e) = say(app, ch, e).await {
            tracing::warn!(?e, %ch, "panel ticket greeting failed");
        }
    }
    Ok(Ok(ch))
}

/// Roles of the Moderators permit and every higher-ranked permit with Manage Tickets: the staff a
/// new panel ticket pings (`tickets.ping_staff`). Permit members get these roles from Brim, so
/// a role ping reaches them; Owner, EO and TA hold no permit role and are not pinged. With no
/// Moderators permit, every permit with Manage Tickets counts.
pub(super) async fn ping_staff_roles(
    app: &App,
    gid: Id<GuildMarker>,
) -> Result<Vec<Id<RoleMarker>>> {
    let roles: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT r.role_id FROM permit_roles r JOIN permits p ON p.id = r.permit_id
         WHERE p.guild_id = $1 AND 'tickets' = ANY(p.perms) AND p.rank >= COALESCE(
           (SELECT min(rank) FROM permits WHERE guild_id = $1 AND name = 'Moderators'), -2147483648)",
    )
    .bind(gid.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    let roles = super::live_roles(app, gid, roles.into_iter().map(|r| r as u64).collect()).await;
    Ok(roles.into_iter().filter_map(Id::new_checked).collect())
}

/// The types to offer while a ticket has none (`kind` is its type or "").
pub(super) fn pick<'a>(types: &'a [String], kind: &str) -> Option<&'a [String]> {
    (kind.is_empty() && !types.is_empty()).then_some(types)
}

/// A menu of ticket types; `tk2:kindset` sets the one picked.
fn kind_menu(types: &[String], placeholder: &str) -> Component {
    kind_menu_as("tk2:kindset", types, placeholder)
}

/// A menu of ticket types under `custom_id` (the DM one is `tk:kind:<server>:<dm message>`, see `dm::handle`).
pub(super) fn kind_menu_as(custom_id: &str, types: &[String], placeholder: &str) -> Component {
    let options = types
        .iter()
        .map(|t| SelectMenuOption {
            default: false,
            description: None,
            emoji: None,
            label: clip(t, 100),
            value: clip(t, 100),
        })
        .collect();
    action_row(vec![Component::SelectMenu(SelectMenu {
        id: None,
        channel_types: None,
        custom_id: custom_id.into(),
        default_values: None,
        disabled: false,
        kind: SelectMenuType::Text,
        max_values: Some(1),
        min_values: Some(1),
        options: Some(options),
        placeholder: Some(placeholder.into()),
        required: None,
    })])
}

/// The first message's rows: the staff buttons, and while the ticket has no type, the type menu
/// for the member (staff can use it too).
pub(super) fn intro(claimed: bool, pick: Option<&[String]>) -> Vec<Component> {
    let mut rows = vec![intro_buttons(claimed)];
    if let Some(types) = pick {
        rows.push(kind_menu(types, "What is this ticket about?"));
    }
    rows
}

/// The staff buttons on the first message: Claim and Close, then once claimed Change Priority,
/// Change Type, Add Note and Close Ticket (owner decision 11-15).
fn intro_buttons(claimed: bool) -> Component {
    action_row(if claimed {
        vec![
            button("Change Priority", "tk2:prio".into(), ButtonStyle::Secondary),
            button("Change Type", "tk2:kind".into(), ButtonStyle::Secondary),
            button("Add Note", "tk2:note".into(), ButtonStyle::Secondary),
            button("Close Ticket", "tk2:close".into(), ButtonStyle::Danger),
        ]
    } else {
        vec![
            button("Claim", "tk2:claim".into(), ButtonStyle::Primary),
            button("Close", "tk2:close".into(), ButtonStyle::Danger),
        ]
    })
}

/// Whether `m` is a panel ticket's first message, by its staff buttons.
fn is_intro(m: &Message, me: u64) -> bool {
    m.author.id.get() == me
        && m.components.iter().any(|c| match c {
            Component::ActionRow(r) => r.components.iter().any(|b| {
                matches!(b, Component::Button(b)
                    if matches!(b.custom_id.as_deref(), Some("tk2:claim" | "tk2:prio")))
            }),
            _ => false,
        })
}

/// Puts the buttons that fit the claim on a panel ticket's first message: `msg` when the press
/// came from it, else the pinned one. Best effort: a missing message, or two claim changes at
/// the same moment, only leave old buttons until the next press.
pub(super) async fn sync_intro(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    msg: Option<Id<MessageMarker>>,
) -> Result<()> {
    let claimed = workflow::holder(app, gid, ch).await?.is_some();
    let kind = workflow::kind_of(app, gid, ch).await?.unwrap_or_default();
    let types = app.cfg(gid.get()).await?.tickets.types.clone();
    let msg = match msg {
        Some(m) => Some(m),
        None => app
            .http
            .pins(ch)
            .await?
            .model()
            .await?
            .items
            .into_iter()
            .map(|p| p.message)
            .find(|m| is_intro(m, app.me.get()))
            .map(|m| m.id),
    };
    if let Some(msg) = msg {
        app.http
            .update_message(ch, msg)
            .components(Some(&intro(claimed, pick(&types, &kind))))
            .await?;
    }
    Ok(())
}

/// One text box in a modal; the answer comes back as `f0`.
#[allow(deprecated)]
fn text_modal(id: &str, title: &str, label: &str, required: bool, max: u16) -> InteractionResponse {
    let input = Component::Label(Label {
        id: None,
        description: None,
        label: label.into(),
        component: Box::new(Component::TextInput(TextInput {
            id: None,
            custom_id: "f0".into(),
            label: None,
            max_length: Some(max),
            min_length: None,
            placeholder: None,
            required: Some(required),
            style: TextInputStyle::Paragraph,
            value: None,
        })),
    });
    InteractionResponse {
        kind: InteractionResponseType::Modal,
        data: Some(InteractionResponseData {
            custom_id: Some(id.into()),
            title: Some(title.into()),
            components: Some(vec![input]),
            ..Default::default()
        }),
    }
}

/// The answer of a staff button: its entry in the server's words. Only unclaim, reassign and the
/// note command give `Ok(None)`, and no button calls them.
pub(super) async fn shown_of(
    app: &App,
    gid: Id<GuildMarker>,
    r: Result<Option<Line>, Line>,
) -> crate::reply::Said {
    match r {
        Ok(Some(l)) | Err(l) => l.said(app, gid).await,
        Ok(None) => String::new().into(),
    }
}

/// The staff buttons, menu and forms on a panel ticket's first message: staff only, except the
/// member's own type pick.
async fn staff_button(
    app: &App,
    i: &Interaction,
    gid: Id<GuildMarker>,
    user: &User,
    what: &str,
    values: &[String],
) -> Result<()> {
    let client = app.http.interaction(app.me.cast());
    let say_back = |s: crate::reply::Said| InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            content: Some(s.text).filter(|t| !t.is_empty()),
            embeds: Some(s.embeds),
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    };
    let cfg = app.cfg(gid.get()).await?;
    let ch = i.channel.as_ref().map(|c| c.id);
    let here = match ch {
        Some(ch) => ticket_here(app, &cfg.tickets, gid, ch).await?,
        None => None,
    };
    let (Some(ch), Some(here)) = (ch, here) else {
        let r = answer(app, gid, "reply.ticket.closed").await;
        client.create_response(i.id, &i.token, &r).await?;
        return Ok(());
    };
    let owner = app.guild_info(gid).await?.owner;
    let staff = app.level(gid, owner, user.id).await?.has("tickets");
    // The member picks the type once, from the first message; staff change it any time.
    if what == "kindset" && (staff || user.id == here.user) {
        let kind = values.first().map_or("", String::as_str);
        let r = workflow::set_kind(app, gid, ch, user.id, kind, staff).await?;
        let s = shown_of(app, gid, r).await;
        let from_menu = i
            .message
            .as_ref()
            .filter(|m| m.flags.is_some_and(|f| f.contains(MessageFlags::EPHEMERAL)));
        let r = InteractionResponse {
            kind: if from_menu.is_some() {
                InteractionResponseType::UpdateMessage
            } else {
                InteractionResponseType::ChannelMessageWithSource
            },
            data: Some(InteractionResponseData {
                content: Some(s.text).filter(|t| !t.is_empty()),
                embeds: Some(s.embeds),
                components: from_menu.map(|_| vec![]),
                flags: from_menu.is_none().then_some(MessageFlags::EPHEMERAL),
                ..Default::default()
            }),
        };
        client.create_response(i.id, &i.token, &r).await?;
        let msg = i
            .message
            .as_ref()
            .filter(|m| is_intro(m, app.me.get()))
            .map(|m| m.id);
        if let Err(e) = sync_intro(app, gid, ch, msg).await {
            tracing::warn!(?e, %ch, "updating the ticket buttons failed");
        }
        return Ok(());
    }
    if !staff {
        let r = answer(app, gid, "reply.ticket.staff_only").await;
        client.create_response(i.id, &i.token, &r).await?;
        return Ok(());
    }
    let typed = match &i.data {
        Some(InteractionData::ModalSubmit(m)) => answers(m, 1).remove(0),
        _ => String::new(),
    };
    match what {
        "kind" => {
            let types = &cfg.tickets.types;
            if types.is_empty() {
                let r = answer(app, gid, "reply.ticket.no_types").await;
                client.create_response(i.id, &i.token, &r).await?;
                return Ok(());
            }
            let r = InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    components: Some(vec![kind_menu(types, "Select a ticket type")]),
                    flags: Some(MessageFlags::EPHEMERAL),
                    ..Default::default()
                }),
            };
            client.create_response(i.id, &i.token, &r).await?;
        }
        "close" => {
            let r = text_modal(
                "tk2:closeform",
                "Close Ticket",
                "Reason (optional)",
                false,
                1000,
            );
            client.create_response(i.id, &i.token, &r).await?;
        }
        "closeform" => {
            let defer = InteractionResponse {
                kind: InteractionResponseType::DeferredUpdateMessage,
                data: None,
            };
            client.create_response(i.id, &i.token, &defer).await?;
            return staff::close(app, gid, ch, here.user, user, &typed, false, 0).await;
        }
        "note" => {
            let r = text_modal("tk2:noteform", "Add Note", "Staff note", true, 2000);
            client.create_response(i.id, &i.token, &r).await?;
        }
        "noteform" => {
            let log = cfg.tickets.log_channel;
            let r = workflow::note_form(app, gid, &here, ch, user, &typed, log).await?;
            client
                .create_response(i.id, &i.token, &say_back(shown_of(app, gid, r).await))
                .await?;
        }
        "prio" => {
            let options = workflow::PRIORITIES
                .iter()
                .map(|p| SelectMenuOption {
                    default: false,
                    description: None,
                    emoji: None,
                    label: format!("{}{}", p[..1].to_uppercase(), &p[1..]),
                    value: (*p).into(),
                })
                .collect();
            let menu = Component::SelectMenu(SelectMenu {
                id: None,
                channel_types: None,
                custom_id: "tk2:prioset".into(),
                default_values: None,
                disabled: false,
                kind: SelectMenuType::Text,
                max_values: Some(1),
                min_values: Some(1),
                options: Some(options),
                placeholder: Some("Select a priority".into()),
                required: None,
            });
            let r = InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    components: Some(vec![action_row(vec![menu])]),
                    flags: Some(MessageFlags::EPHEMERAL),
                    ..Default::default()
                }),
            };
            client.create_response(i.id, &i.token, &r).await?;
        }
        "prioset" => {
            let level = values.first().map_or("", String::as_str);
            let r = workflow::priority(app, gid, ch, user.id, level).await?;
            let s = shown_of(app, gid, r).await;
            let r = InteractionResponse {
                kind: InteractionResponseType::UpdateMessage,
                data: Some(InteractionResponseData {
                    content: Some(s.text).filter(|t| !t.is_empty()),
                    embeds: Some(s.embeds),
                    components: Some(vec![]),
                    ..Default::default()
                }),
            };
            client.create_response(i.id, &i.token, &r).await?;
        }
        _ => {
            let r = workflow::claim(app, gid, ch, user.id).await?;
            let mut reply = say_back(shown_of(app, gid, r).await);
            // No type yet: staff get the same menu the member had (owner decision 11-15).
            let unset = workflow::kind_of(app, gid, ch).await?.is_none();
            if let (true, Some(d)) = (unset && !cfg.tickets.types.is_empty(), reply.data.as_mut()) {
                d.components = Some(vec![kind_menu(
                    &cfg.tickets.types,
                    "What is this ticket about?",
                )]);
            }
            client.create_response(i.id, &i.token, &reply).await?;
            // Also after "already claimed": a claim by command left the old buttons.
            let msg = i.message.as_ref().map(|m| m.id);
            if let Err(e) = sync_intro(app, gid, ch, msg).await {
                tracing::warn!(?e, %ch, "updating the ticket buttons failed");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_form_has_another_hash() {
        let mut p: TicketPanel = serde_json::from_value(json!({"id": "help", "name": "Help",
            "fields": [{"label": "What happened?", "long": true}]}))
        .unwrap();
        let before = form_hash(&p);
        assert_eq!(before, form_hash(&p.clone()));
        p.fields[0].required = false;
        assert_ne!(before, form_hash(&p));
        // The form's custom_id stays under Discord's 100 characters at the longest id.
        p.id = "a".repeat(32);
        let r = modal(&p, u64::MAX, None);
        assert!(r.data.unwrap().custom_id.unwrap().len() <= 100);
    }

    /// Moderators and higher permits with Manage Tickets are pinged; lower ones and permits
    /// without it are not.
    #[tokio::test]
    async fn staff_ping_is_moderators_and_up_with_manage_tickets() {
        let Some(t) = crate::test_support::T::new(969).await else {
            return;
        };
        for (n, name, rank, perms) in [
            (1, "Helpers", 5, "{tickets}"),
            (2, "Moderators", 10, "{tickets}"),
            (3, "Admins", 20, "{tickets,ban}"),
            (4, "Lead", 30, "{ban}"),
            // Its role was deleted in the server: no `@unknown-role` ping.
            (5, "Seniors", 40, "{tickets}"),
        ] {
            let id = t.base as i64 + n;
            sqlx::query("INSERT INTO permits (id, guild_id, name, rank, perms) VALUES ($1, $2, $3, $4, $5::text[])")
                .bind(id)
                .bind(t.base as i64)
                .bind(name)
                .bind(rank)
                .bind(perms)
                .execute(&t.app.pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(t.base as i64)
            .bind(600 + n)
            .execute(&t.app.pool)
            .await
            .unwrap();
        }
        let g = t.g(0).get();
        t.server_config(0, t.owner(), json!({})).await;
        t.app.invalidate_guild(t.g(0));
        t.set(|m| {
            m.roles
                .insert(g, (601..605).map(|r| (r, false, 0, 1)).collect());
            m.members.insert((g, crate::test_support::ME), vec![604]);
        });
        let mut got: Vec<u64> = ping_staff_roles(&t.app, t.g(0))
            .await
            .unwrap()
            .iter()
            .map(|r| r.get())
            .collect();
        got.sort_unstable();
        assert_eq!(got, [602, 603]);
        t.cleanup().await;
    }

    /// A button works only on a message recorded for this server, for a panel still on.
    #[tokio::test]
    async fn forged_stale_and_disabled_panel_buttons_are_refused() {
        let Some(t) = crate::test_support::T::new(947).await else {
            return;
        };
        let panels = json!([{"id": "help", "name": "Help", "fields": [{"label": "What?"}]},
                            {"id": "old", "name": "Old", "enabled": false}]);
        t.server_config(
            0,
            t.owner(),
            json!({"tickets": {"enabled": true, "panels": panels}}),
        )
        .await;
        t.server_config(
            1,
            t.owner(),
            json!({"tickets": {"enabled": true, "panels": panels}}),
        )
        .await;
        t.exec("INSERT INTO ticket_panel_posts (message_id, guild_id, channel_id, panels, posted_by) VALUES ($1, $1, 1, '{help,old}', 1)").await;
        let press = |guild: u64, msg: u64, custom: &str| -> Interaction {
            let user = json!({"id": "77", "username": "m", "discriminator": "0", "avatar": null});
            serde_json::from_value(json!({
                "id": "5", "application_id": "1", "type": 3, "token": "tok", "version": 1,
                "guild_id": guild.to_string(), "channel_id": "1",
                "data": {"custom_id": custom, "component_type": 2},
                "member": {"user": user, "roles": [], "joined_at": "2020-01-01T00:00:00.000000+00:00",
                    "deaf": false, "mute": false, "flags": 0, "permissions": "0"},
                "message": {"id": msg.to_string(), "channel_id": "1", "content": "",
                    "author": {"id": "1", "username": "bot", "discriminator": "0", "avatar": null},
                    "timestamp": "2026-01-01T00:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
                    "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
                    "embeds": [], "components": [], "pinned": false, "type": 0},
                "entitlements": [], "authorizing_integration_owners": {}, "attachment_size_limit": 8388608
            }))
            .unwrap()
        };
        let post = t.base;
        for (guild, msg, custom) in [
            (t.g(0).get(), post + 1, "tk2:open:help"), // a message we never posted
            (t.g(1).get(), post, "tk2:open:help"),     // our post, another server
            (t.g(0).get(), post, "tk2:open:old"),      // a panel turned off
            (t.g(0).get(), post, "tk2:open:nope"),     // a panel that is gone
        ] {
            let _ = handle(&t.app, &press(guild, msg, custom)).await;
        }
        assert_eq!(t.calls("no longer in use"), 4);
        // A blacklisted member is refused before the form (owner decision 11-15).
        let blocked = json!({"tickets": {"enabled": true, "panels": panels, "blocked": ["77"]}});
        t.server_config(2, t.owner(), blocked).await;
        t.exec("INSERT INTO ticket_panel_posts (message_id, guild_id, channel_id, panels, posted_by) VALUES ($1 + 2, $1 + 2, 1, '{help}', 1)").await;
        let _ = handle(&t.app, &press(t.g(2).get(), post + 2, "tk2:open:help")).await;
        assert_eq!(t.calls("blacklisted"), 1);
        assert_eq!(t.calls("tk2:form:help:"), 0);
        // The real button shows the form.
        let _ = handle(&t.app, &press(t.g(0).get(), post, "tk2:open:help")).await;
        assert_eq!(t.calls("tk2:form:help:"), 1);
        // A button per type works only while the switch is on and the type is listed.
        let typed = format!("tk2:open:help:{}", type_hash("Report"));
        let _ = handle(&t.app, &press(t.g(0).get(), post, &typed)).await;
        assert_eq!(t.calls("form was changed"), 1);
        let on = json!({"tickets": {"enabled": true, "panels": panels, "button_per_type": true}});
        t.server_config(3, t.owner(), on).await;
        t.exec("INSERT INTO ticket_panel_posts (message_id, guild_id, channel_id, panels, posted_by) VALUES ($1 + 3, $1 + 3, 1, '{help}', 1)").await;
        let _ = handle(&t.app, &press(t.g(3).get(), post + 3, &typed)).await;
        assert_eq!(t.calls(&format!(":{}\"", type_hash("Report"))), 1);
        let gone = format!("tk2:open:help:{}", type_hash("Nope"));
        let _ = handle(&t.app, &press(t.g(3).get(), post + 3, &gone)).await;
        assert_eq!(t.calls("form was changed"), 2);
        // A saved message's "Open a ticket" button: only on a copy recorded for this server whose
        // button names that panel.
        t.exec("WITH e AS (INSERT INTO embeds (guild_id, name, message, updated_by) VALUES ($1, 'Rules', '{}', 1) RETURNING id)
                INSERT INTO message_sends (embed_id, guild_id, channel_id, posted_id, body, sent_by, detail)
                SELECT id, $1, 1, $1 + 50, '{\"components\": [{\"type\": 1, \"components\": [{\"type\": 2, \"style\": 1, \"label\": \"Help\", \"action\": \"ticket:help\"}]}]}', 1, '' FROM e").await;
        let before = t.calls("no longer in use");
        let _ = handle(&t.app, &press(t.g(0).get(), post + 50, "tk2:open:old")).await;
        let _ = handle(&t.app, &press(t.g(1).get(), post + 50, "tk2:open:help")).await;
        assert_eq!(t.calls("no longer in use"), before + 2);
        let _ = handle(&t.app, &press(t.g(0).get(), post + 50, "tk2:open:help")).await;
        assert_eq!(t.calls("tk2:form:help:"), 3);
        t.exec("DELETE FROM embeds WHERE guild_id = $1").await;
        t.exec("DELETE FROM ticket_panel_posts WHERE guild_id = $1")
            .await;
        t.cleanup().await;
    }
}

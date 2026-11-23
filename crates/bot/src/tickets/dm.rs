use super::*;

/// `t!word args` or `@bot word args` in a DM.
fn dm_command(content: &str, me: Id<UserMarker>) -> Option<(String, &str)> {
    let rest = [
        DM_PREFIX.to_string(),
        format!("<@{me}> "),
        format!("<@!{me}> "),
    ]
    .iter()
    .find_map(|p| content.strip_prefix(p.as_str()))?
    .trim_start();
    let (word, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((word.to_lowercase(), args.trim()))
}

/// What a DM asks to send: the text after `new` or `send ID`, else the whole message.
fn body(content: &str, me: Id<UserMarker>) -> String {
    match dm_command(content, me) {
        Some((w, a)) if matches!(w.as_str(), "new" | "create" | "switch" | "change") => a.into(),
        Some((w, a)) if w == "send" => a
            .split_once(char::is_whitespace)
            .map_or("", |x| x.1)
            .trim()
            .into(),
        _ => content.into(),
    }
}

pub async fn on_dm(app: &App, m: &Message) -> Result<()> {
    let dm = m.channel_id;
    if let Some((word, args)) = dm_command(&m.content, app.me) {
        return match word.as_str() {
            "new" | "create" | "switch" | "change"
                if args.is_empty() && m.attachments.is_empty() =>
            {
                say_dm(app, dm, "reply.dm.usage_new", &[("prefix", DM_PREFIX)]).await
            }
            "new" | "create" | "switch" | "change" => pick(app, m).await,
            "send" => match args
                .split_whitespace()
                .next()
                .and_then(|g| g.parse().ok())
                .and_then(Id::new_checked)
            {
                Some(g) if !body(&m.content, app.me).is_empty() || !m.attachments.is_empty() => {
                    send_mail(app, m, g, None).await
                }
                _ => say_dm(app, dm, "reply.dm.usage_send", &[("prefix", DM_PREFIX)]).await,
            },
            "confirmation" => confirmation(app, m).await,
            _ => Ok(()),
        };
    }
    if m.content.trim().is_empty() && m.attachments.is_empty() {
        return Ok(());
    }
    // Only the one ticket server the member shares with the bot is taken for granted. With
    // several they pick each time, so a DM meant for one server's staff never reaches another's
    // (no "last server used", owner decision 2026-11-17).
    match (
        user_state(app, m.author.id).await?,
        shared(app, m.author.id).await?,
    ) {
        // `sure`: a failed lookup may hide a second server, so one listed server proves nothing.
        (true, (list, true)) if list.len() == 1 => ask(app, m, &list[0]).await,
        (false, (list, true)) if list.len() == 1 => send_mail(app, m, list[0].id, None).await,
        _ => pick(app, m).await,
    }
}

async fn confirmation(app: &App, m: &Message) -> Result<()> {
    let on = user_state(app, m.author.id).await?;
    sqlx::query(
        "INSERT INTO ticket_users (user_id, confirmation) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET confirmation = $2",
    )
    .bind(m.author.id.get() as i64)
    .bind(!on)
    .execute(&app.pool)
    .await?;
    let key = if on {
        "reply.dm.confirmation_off"
    } else {
        "reply.dm.confirmation_on"
    };
    say_dm(app, m.channel_id, key, &[("prefix", DM_PREFIX)]).await
}

async fn ask(app: &App, m: &Message, s: &Server) -> Result<()> {
    let (e, row) = confirm(s, m.id);
    app.http
        .create_message(m.channel_id)
        .embeds(&[e])
        .components(&[row])
        .await?;
    Ok(())
}

/// Servers the member shares with the bot that take tickets, and whether a ticket is open there.
async fn mutual(app: &App, uid: Id<UserMarker>) -> Result<Vec<(Server, bool)>> {
    let open: Vec<i64> = sqlx::query_scalar(
        "SELECT guild_id FROM tickets WHERE user_id = $1 AND closed_at IS NULL AND panel IS NULL",
    )
    .bind(uid.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    let (list, _) = shared(app, uid).await?;
    Ok(list
        .into_iter()
        .map(|s| {
            let here = open.contains(&(s.id.get() as i64));
            (s, here)
        })
        .collect())
}

/// Per member: the shared ticket servers, kept a minute. Each lookup asks Discord once per
/// ticket server, so pressing "Other server" again and again must not repeat it.
type Shared = std::collections::HashMap<u64, (std::time::Instant, Vec<Server>)>;
static SHARED: std::sync::LazyLock<std::sync::Mutex<Shared>> =
    std::sync::LazyLock::new(Default::default);

/// Also whether every ticket server answered (a failed lookup leaves the list short).
async fn shared(app: &App, uid: Id<UserMarker>) -> Result<(Vec<Server>, bool)> {
    const FRESH: std::time::Duration = std::time::Duration::from_secs(60);
    {
        let mut all = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|_, (at, _)| at.elapsed() < FRESH);
        if let Some((_, list)) = all.get(&uid.get()) {
            return Ok((list.clone(), true));
        }
    }
    let guilds = ticket_guilds(&app.pool).await?;
    let mut out = vec![];
    let mut unsure = false;
    for g in guilds.into_iter().filter_map(|g| Id::new_checked(g as u64)) {
        // A server the bot left answers with an error; it is simply not shared.
        match http_err::member(&app.http, g, uid).await {
            Ok(Some(_)) => {}
            Ok(None) => continue,
            Err(_) => {
                unsure = true;
                continue;
            }
        }
        if let Some(s) = server(app, g).await? {
            out.push(s);
        }
        if out.len() == 25 {
            break;
        }
    }
    // A failed lookup is asked again next time, not kept as "not shared" for a minute.
    if !unsure {
        let mut all = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        all.insert(uid.get(), (std::time::Instant::now(), out.clone()));
    }
    Ok((out, !unsure))
}

/// The server menu, or the error to show instead.
async fn pick_view(
    app: &App,
    uid: Id<UserMarker>,
    msg: Id<MessageMarker>,
) -> Result<(crate::reply::Said, Vec<Component>)> {
    let list = mutual(app, uid).await?;
    if list.is_empty() {
        return Ok((
            crate::reply::say_wide(app, "reply.dm.no_server", &[]).await,
            vec![],
        ));
    }
    // The texts inside the menu are message text of their entries, cut to what Discord takes.
    let line = |key| crate::reply::say_wide_line(app, key, &[]);
    let (existing, new) = (
        clip(&line("reply.dm.option_existing").await, 100),
        clip(&line("reply.dm.option_new").await, 100),
    );
    let placeholder = clip(&line("reply.dm.placeholder").await, 150);
    let options = list
        .iter()
        .map(|(s, open)| SelectMenuOption {
            default: false,
            description: Some(if *open { existing.clone() } else { new.clone() }),
            emoji: None,
            label: clip(&s.name, 100),
            value: s.id.to_string(),
        })
        .collect();
    let menu = Component::SelectMenu(SelectMenu {
        id: None,
        channel_types: None,
        custom_id: format!("tk:pick:{msg}"),
        default_values: None,
        disabled: false,
        kind: SelectMenuType::Text,
        max_values: Some(1),
        min_values: Some(1),
        options: Some(options),
        placeholder: Some(placeholder),
        required: None,
    });
    let said = crate::reply::say_wide(app, "reply.dm.menu", &[]).await;
    Ok((said, vec![action_row(vec![menu])]))
}

async fn pick(app: &App, m: &Message) -> Result<()> {
    // The menu asks Discord about every ticket server, so one menu per member every few seconds.
    if !app.cooled_down(0, m.author.id.get()) {
        return Ok(());
    }
    let (said, rows) = pick_view(app, m.author.id, m.id).await?;
    app.http
        .create_message(m.channel_id)
        .content(&said.text)
        .embeds(&said.embeds)
        .components(&rows)
        .await?;
    Ok(())
}

/// Sends a member's DM to a server, opening a ticket there first when none is open. `kind`: the
/// type the member picked, `None` while not asked yet; a server with types asks before the channel.
async fn send_mail(app: &App, m: &Message, gid: Id<GuildMarker>, kind: Option<&str>) -> Result<()> {
    let dm = m.channel_id;
    let Some(s) = server(app, gid).await? else {
        return say_key(app, gid, dm, "reply.ticket.dm_server_missing", &[]).await;
    };
    let Some(member) = http_err::member(&app.http, gid, m.author.id).await? else {
        return say_key(app, gid, dm, "reply.ticket.dm_not_member", &[]).await;
    };
    let category = match ensure_category(app, gid).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return say_key(app, gid, dm, "reply.ticket.dm_not_taking", &[]).await;
        }
        Err(e) => {
            tracing::warn!(?e, %gid, "making the ticket category failed");
            return say_key(app, gid, dm, "reply.ticket.dm_category_failed", &[]).await;
        }
    };
    let cfg = app.cfg(gid.get()).await?;
    let t = &cfg.tickets;
    if t.blocked.contains(&m.author.id) {
        return say_key(app, gid, dm, "reply.ticket.dm_blacklisted", &[]).await;
    }
    let text = body(&m.content, app.me);
    if filtered(app, &cfg, &text) && !bypasses(app, gid, m.author.id).await {
        return say_key(app, gid, dm, "reply.ticket.dm_filtered", &[]).await;
    }
    let ch = match open_ticket(app, gid, m.author.id).await? {
        Some(ch) => ch,
        // D27: no new tickets while a raid or lock is on, and a few an hour per member.
        None if crate::lockdown::raid_or_lock(app, gid).await? => {
            return say_key(app, gid, dm, "reply.ticket.dm_lockdown", &[]).await;
        }
        // Owner decision: a quarantined member reaches staff only when nobody told them why.
        None if quarantined_with_reason(app, gid, m.author.id).await? => {
            return say_key(app, gid, dm, "reply.ticket.dm_quarantined", &[]).await;
        }
        None if t.paused => {
            let reason = if t.pause_reason.is_empty() {
                "No reason was provided."
            } else {
                &t.pause_reason
            };
            let e = stamp(s.tpl("ticket.paused", &[("reason", reason)]));
            return say(app, dm, e.build()).await;
        }
        None => {
            if super::full(app, gid, category).await? {
                super::categories_full(app, gid).await;
                return say_key(app, gid, dm, "reply.ticket.categories_full", &[]).await;
            }
            // A type removed since the menu was sent asks again with the types of now.
            let kind = match kind {
                Some(k) if t.types.iter().any(|x| x == k) => k,
                _ if !t.types.is_empty() => return ask_type(app, gid, m, &t.types).await,
                _ => "",
            };
            match create(app, &s, m, &member.roles, t, category, kind).await? {
                Some(ch) => ch,
                None => return Ok(()),
            }
        }
    };
    let (files, linked) = download(app, &m.attachments).await;
    let (sent, got) = from_member(&s, &m.author, &text);
    let dm_msg = app
        .http
        .create_message(dm)
        .embeds(&[sent.build()])
        .attachments(&files)
        .await?
        .model()
        .await?;
    // The member's own message stays in the DM, so its links keep working.
    let got = attachment_fields(got, linked);
    if let Err(e) = app
        .http
        .create_message(ch)
        .embeds(&[got.build()])
        .attachments(&files)
        .await
    {
        tracing::warn!(?e, %ch, "relaying a ticket message failed");
        // The channel was deleted while the bot was offline, so no delete event closed the
        // ticket; without this the member could never open a new one there.
        if http_err::gone(&e) {
            sqlx::query(
                "UPDATE tickets SET status = 'closed', closed_at = now()
                 WHERE channel_id = $1 AND status = 'open' AND guild_id = $2",
            )
            .bind(ch.get() as i64)
            .bind(gid.get() as i64)
            .execute(&app.pool)
            .await?;
            best_effort(
                app.http.delete_message(dm, dm_msg.id).await,
                "undo ticket dm",
            );
            return say_key(app, gid, dm, "reply.ticket.dm_earlier_closed", &[]).await;
        }
        best_effort(
            app.http.delete_message(dm, dm_msg.id).await,
            "undo ticket dm",
        );
        return say_key(app, gid, dm, "reply.ticket.bot_missing_perms", &[]).await;
    }
    touch(app, gid, ch, false).await
}

/// The type menu for a new DM ticket. It carries the member's DM, which is sent once they pick.
async fn ask_type(app: &App, gid: Id<GuildMarker>, m: &Message, types: &[String]) -> Result<()> {
    let id = format!("tk:kind:{gid}:{}", m.id);
    let rows = [panel::kind_menu_as(&id, types, "Choose a ticket type")];
    let ask = crate::reply::say(app, gid, "reply.ticket.ask_type", &[]).await;
    app.http
        .create_message(m.channel_id)
        .content(&ask.text)
        .embeds(&ask.embeds)
        .components(&rows)
        .await?;
    Ok(())
}

/// Quarantined, and the newest quarantine case since this quarantine began is staff's with a
/// reason (an older, lifted case does not count). Automatic quarantines
/// (the bot's own cases) and staff quarantines without a reason told the member nothing.
pub(super) async fn quarantined_with_reason(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT coalesce((SELECT c.mod_id <> $3 AND coalesce(c.reason, '') <> ''
                          FROM quarantines q JOIN cases c
                            ON c.guild_id = q.guild_id AND c.user_id = q.user_id
                           AND c.kind = 'quarantine' AND c.created_at >= q.created_at
                          WHERE q.guild_id = $1 AND q.user_id = $2
                          ORDER BY c.case_no DESC LIMIT 1), false)",
    )
    .bind(gid.get() as i64)
    .bind(user.get() as i64)
    .bind(app.me.get() as i64)
    .fetch_one(&app.pool)
    .await?)
}

/// Owner, Extra Owner and Trusted Admin bypass the filters, as everywhere. A failed lookup
/// counts as not immune, so the filter applies.
pub(super) async fn bypasses(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> bool {
    app.is_immune(gid, user).await.unwrap_or_else(|e| {
        tracing::warn!(?e, %gid, %user, "ticket filter: level lookup failed, filtering");
        false
    })
}

/// A ticket DM goes through the server's word, scam and invite filters. The local word
/// list is used even when words go to Discord's AutoMod, which never sees a DM.
/// shortcut: no shortener lookup and no attachment scan; add them if scams move there.
pub(crate) fn filtered(app: &App, cfg: &crate::app::GuildCfg, text: &str) -> bool {
    use brim_automod::filters;
    let am = &cfg.automod;
    if !am.enabled {
        return false;
    }
    let f = &am.filters;
    let hosts = filters::hosts(text);
    let words = f.words.filter.enabled && {
        let forms = filters::word_forms(text);
        filters::word_hits(&forms, &crate::automod::blacklist_words(f)) + f.words.regex.hits(&forms)
            > 0
    };
    words
        || f.scam_links.enabled && filters::is_nitro_steam_scam(text)
        || f.bad_sites.enabled && app.sites.phishing(&hosts)
        || f.invites.enabled && crate::automod::is_invite(text, &hosts)
}

/// Opens the ticket channel, logs it and greets the member. `None` when the member was told why not.
async fn create(
    app: &App,
    s: &Server,
    m: &Message,
    roles: &[Id<RoleMarker>],
    t: &TicketsCfg,
    category: Id<ChannelMarker>,
    kind: &str,
) -> Result<Option<Id<ChannelMarker>>> {
    let (gid, user, dm) = (s.id, &m.author, m.channel_id);
    let g = gid.get() as i64;
    let number = match claim_number(&app.pool, gid, user.id, None).await? {
        Claim::Number(n) => n,
        refused => {
            say_key(app, gid, dm, refusal(&refused), &[]).await?;
            return Ok(None);
        }
    };
    let ows = overwrites(gid, app.me.get(), &staff_roles(app, gid).await?);
    let make = |name: String| {
        let ows = &ows;
        async move {
            let r = app
                .http
                .create_guild_channel(gid, &name)
                .kind(ChannelType::GuildText)
                .parent_id(category)
                .permission_overwrites(ows)
                .await?;
            anyhow::Ok(r.model().await?.id)
        }
    };
    let name = channel_name(&user.name, user.id, rand::random());
    let made = match make(kind_name(&name, None, kind)).await {
        // Discord refuses names with words it does not allow (50035); the id always passes.
        Err(e) if http_err::code(&e) == Some(50035) => make(user.id.to_string()).await,
        r => r,
    };
    let ch = match made {
        Ok(ch) => ch,
        Err(e) => {
            sqlx::query("DELETE FROM tickets WHERE guild_id = $1 AND number = $2")
                .bind(g)
                .bind(number)
                .execute(&app.pool)
                .await?;
            let why = http_err::why(&e);
            match http_err::code(&e) {
                Some(30013) => say_key(app, gid, dm, "reply.ticket.max_active", &[]).await?,
                _ => say_key(app, gid, dm, "reply.ticket.dm_error", &[("error", &why)]).await?,
            }
            return Ok(None);
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
    // The row was dropped as stale while this ran (over 10 minutes): no ticket owns the channel.
    if bound == 0 {
        best_effort(app.http.delete_channel(ch).await, "unbound ticket channel");
        say_key(app, gid, dm, "reply.ticket.dm_open_failed", &[]).await?;
        return Ok(None);
    }
    event(
        app,
        gid,
        number,
        Some(user.id),
        "opened",
        json!({"source": "dm", "type": kind}),
    )
    .await;
    if let Some(log) = t.log_channel {
        let e = super::new_ticket_log(user, None, Some(kind));
        if let Err(e) = crate::events::send_private(app, gid, log, e, None).await {
            tracing::warn!(?e, "ticket log failed");
        }
    }
    let pfx = &app.cfg(gid.get()).await?.misc.prefix;
    let quarantined = crate::antinuke::quarantined(app, gid, user.id).await?;
    let intro = intro(pfx, t.command_only, user, roles, quarantined);
    let (mut pings, mut mentions) = pings(
        gid,
        &super::live_ids(app, gid, &t.ping_roles).await,
        t.ping_here,
    );
    if t.ping_staff {
        // The ticket is open already: a failed lookup only skips the staff ping.
        let staff = panel::ping_staff_roles(app, gid).await.unwrap_or_else(|e| {
            tracing::warn!(?e, "reading the staff ping roles failed");
            vec![]
        });
        for r in staff {
            if r.get() != gid.get() && !mentions.roles.contains(&r) {
                pings.push_str(&format!(" <@&{r}>"));
                mentions.roles.push(r);
            }
        }
    }
    // The same staff buttons as a panel ticket (owner decision 11-15); the member answers by DM.
    let sent = app
        .http
        .create_message(ch)
        .content(pings.trim())
        .embeds(&[intro])
        .components(&panel::intro(false, panel::pick(&t.types, kind)))
        .allowed_mentions(Some(&mentions))
        .await;
    match sent {
        // Pinned, so staff always find who the member is and how to reply.
        Ok(r) => {
            let id = r.model().await?.id;
            best_effort(app.http.create_pin(ch, id).await, "pin ticket intro");
        }
        Err(e) => {
            tracing::warn!(?e, %ch, "ticket intro failed");
            say_key(app, gid, dm, "reply.ticket.bot_missing_perms", &[]).await?;
            return Ok(None);
        }
    }
    if !t.greeting.is_empty() {
        say(app, dm, notice(s, false, &t.greeting, user, kind)).await?;
    }
    Ok(Some(ch))
}

/// The ping line for a new ticket and the mentions it may use.
fn pings(gid: Id<GuildMarker>, roles: &[Id<RoleMarker>], here: bool) -> (String, AllowedMentions) {
    let mut words = vec![];
    let mut am = AllowedMentions::default();
    for r in roles {
        if r.get() == gid.get() {
            words.push("@everyone".to_string());
            am.parse = vec![MentionType::Everyone];
        } else {
            words.push(format!("<@&{r}>"));
            am.roles.push(*r);
        }
    }
    if here {
        words.push("@here".into());
        am.parse = vec![MentionType::Everyone];
    }
    (words.join(" "), am)
}

/// Buttons and the server menu in DMs.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let Some(InteractionData::MessageComponent(c)) = &i.data else {
        return Ok(false);
    };
    let Some(rest) = c.custom_id.strip_prefix("tk:") else {
        return Ok(false);
    };
    let (Some(user), Some(prompt)) = (i.author_id(), i.message.as_ref()) else {
        return Ok(true);
    };
    let client = app.http.interaction(app.me.cast());
    let update = |s: crate::reply::Said, rows: Vec<Component>| InteractionResponse {
        kind: InteractionResponseType::UpdateMessage,
        data: Some(InteractionResponseData {
            // An empty text clears the old menu text.
            content: Some(s.text),
            embeds: Some(s.embeds),
            components: Some(rows),
            ..Default::default()
        }),
    };
    let made_ms = ((prompt.id.get() >> 22) + 1_420_070_400_000) as i64;
    if now_secs() * 1000 - made_ms > MENU_SECS * 1000 {
        let r = update(
            crate::reply::say_wide(app, "reply.dm.expired", &[]).await,
            vec![],
        );
        client.create_response(i.id, &i.token, &r).await?;
        return Ok(true);
    }
    let parts: Vec<&str> = rest.split(':').collect();
    // The member's own DM, fetched again so nothing is kept between the prompt and the answer.
    let original = |id: &str| {
        let id = id.parse().ok().and_then(Id::new_checked);
        async move {
            let Some(id) = id else {
                return anyhow::Ok(None);
            };
            match app.http.message(prompt.channel_id, id).await {
                Ok(r) => Ok(Some(r.model().await?)),
                Err(e) if http_err::gone(&e) => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
    };
    let chosen = match parts.as_slice() {
        // A type menu from before the type was asked first: the member sends the DM again.
        ["kind", _] => {
            let r = update(
                crate::reply::say_wide(app, "reply.dm.expired", &[]).await,
                vec![],
            );
            client.create_response(i.id, &i.token, &r).await?;
            return Ok(true);
        }
        ["no"] => {
            let r = update(
                crate::reply::say_wide(app, "reply.dm.cancelled", &[]).await,
                vec![],
            );
            client.create_response(i.id, &i.token, &r).await?;
            return Ok(true);
        }
        ["other", msg] => {
            // The same per-member limit as a DM: the first menu asks Discord once per ticket server.
            if !app.cooled_down(0, user.get()) {
                // A separate private answer: the server prompt stays with its buttons.
                let said = crate::reply::say_wide(app, "reply.dm.wait", &[]).await;
                let wait = InteractionResponse {
                    kind: InteractionResponseType::ChannelMessageWithSource,
                    data: Some(InteractionResponseData {
                        content: Some(said.text).filter(|t| !t.is_empty()),
                        embeds: Some(said.embeds),
                        flags: Some(twilight_model::channel::message::MessageFlags::EPHEMERAL),
                        ..Default::default()
                    }),
                };
                client.create_response(i.id, &i.token, &wait).await?;
                return Ok(true);
            }
            let defer = InteractionResponse {
                kind: InteractionResponseType::DeferredUpdateMessage,
                data: None,
            };
            client.create_response(i.id, &i.token, &defer).await?;
            let Some(msg) = msg.parse().ok().and_then(Id::new_checked) else {
                return Ok(true);
            };
            let (said, rows) = pick_view(app, user, msg).await?;
            client
                .update_response(&i.token)
                .content(Some(said.text.as_str()).filter(|t| !t.is_empty()))
                .embeds(Some(&said.embeds))
                .components(Some(&rows))
                .await?;
            return Ok(true);
        }
        ["ok", g, msg] => g
            .parse()
            .ok()
            .and_then(Id::new_checked)
            .map(|g| (g, *msg, None)),
        ["pick", msg] => c
            .values
            .first()
            .and_then(|v| v.parse().ok())
            .and_then(Id::new_checked)
            .map(|g| (g, *msg, None)),
        // The ticket type, asked before the channel is made.
        ["kind", g, msg] => g
            .parse()
            .ok()
            .and_then(Id::new_checked)
            .map(|g| (g, *msg, Some(c.values.first().map_or("", String::as_str)))),
        _ => None,
    };
    let Some((gid, msg, kind)) = chosen else {
        return Ok(true);
    };
    let defer = InteractionResponse {
        kind: InteractionResponseType::DeferredUpdateMessage,
        data: None,
    };
    client.create_response(i.id, &i.token, &defer).await?;
    // Taking the prompt away is the claim: a second quick press finds it gone and sends nothing.
    let gone = app.http.delete_message(prompt.channel_id, prompt.id).await;
    match gone.map_err(anyhow::Error::from) {
        Ok(_) => {}
        Err(e) if http_err::code(&e) == Some(10008) => return Ok(true),
        Err(e) => tracing::warn!(?e, "deleting the ticket prompt failed"),
    }
    match original(msg).await? {
        Some(m) => send_mail(app, &m, gid, kind).await,
        None => say_dm(app, prompt.channel_id, "reply.dm.message_deleted", &[]).await,
    }?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_commands_and_message_bodies() {
        let me = Id::new(7);
        assert_eq!(
            dm_command("t!New hi there", me),
            Some(("new".into(), "hi there"))
        );
        assert_eq!(
            dm_command("<@7> confirmation", me),
            Some(("confirmation".into(), ""))
        );
        assert_eq!(dm_command("hello", me), None);
        assert_eq!(body("t!send 123 hi there", me), "hi there");
        assert_eq!(body("t!switch hi", me), "hi");
        assert_eq!(body("plain text", me), "plain text");
    }

    #[test]
    fn pings_cover_everyone_here_and_roles() {
        let (line, am) = pings(Id::new(9), &[Id::new(9), Id::new(4)], true);
        assert_eq!(line, "@everyone <@&4> @here");
        assert_eq!(am.roles, vec![Id::new(4)]);
        assert_eq!(am.parse, vec![MentionType::Everyone]);
    }

    /// D27: a ticket DM is filtered like a server message, and no new ticket opens during a raid
    /// or lockdown or past the hourly limit. Every refusal comes before a channel is made.
    #[tokio::test]
    async fn ticket_dms_are_filtered_and_new_tickets_limited() {
        let Some(t) = crate::test_support::T::new(173).await else {
            return;
        };
        const CAT: u64 = 600;
        let cfg = json!({
            "tickets": {"enabled": true, "category": CAT.to_string()},
            "automod": {"enabled": true, "filters": {
                "words": {"filter": {"enabled": true}, "strict": ["badword"]},
                "invites": {"enabled": true}}},
        });
        t.server_config(0, t.owner(), cfg).await;
        let gid = t.g(0);
        let info = crate::hierarchy::GuildInfo {
            owner: Id::new(t.owner()),
            roles: Default::default(),
            role_perms: Default::default(),
            managed: Default::default(),
            unpermittable: Default::default(),
            parents: Default::default(),
            channels: [Id::new(CAT)].into(),
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
        let u = t.base * 10 + 1;
        t.set(|m| {
            m.members.insert((gid.get(), u), vec![]);
        });
        let dm = |text: &str| -> Message {
            serde_json::from_value(json!({"id": "5", "channel_id": "77", "content": text,
                "author": {"id": u.to_string(), "username": "m", "discriminator": "0", "avatar": null},
                "timestamp": "2026-01-01T00:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
                "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
                "embeds": [], "components": [], "pinned": false, "type": 0}))
            .unwrap()
        };
        let said = |what: &str| t.calls(what);
        send_mail(&t.app, &dm("you are a badword"), gid, None)
            .await
            .unwrap();
        send_mail(&t.app, &dm("join discord.gg/abc"), gid, None)
            .await
            .unwrap();
        assert_eq!(said("words or links that server does not allow"), 2);
        let raid = brim_automod::join::RaidDetector::restored(0, i64::MAX, String::new());
        t.app.raids.lock().unwrap().insert(gid.get(), raid);
        send_mail(&t.app, &dm("help"), gid, Some("Report"))
            .await
            .unwrap();
        assert_eq!(said("does not take new tickets right now"), 1);
        t.app.raids.lock().unwrap().clear();
        for n in 1..=3 {
            sqlx::query("INSERT INTO tickets (guild_id, number, user_id, closed_at, status) VALUES ($1, $2, $3, now(), 'closed')")
                .bind(gid.get() as i64)
                .bind(n)
                .bind(u as i64)
                .execute(&t.app.pool)
                .await
                .unwrap();
        }
        send_mail(&t.app, &dm("help"), gid, Some("Report"))
            .await
            .unwrap();
        assert_eq!(said("too many tickets in that server"), 1);
        assert_eq!(
            said(&format!("POST /guilds/{gid}/channels")),
            0,
            "no ticket opened"
        );
        // A quarantined member still reaches staff; the intro tells staff they are quarantined.
        sqlx::query("DELETE FROM tickets WHERE guild_id = $1")
            .bind(gid.get() as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO quarantines (guild_id, user_id, saved_roles) VALUES ($1, $2, '{}')",
        )
        .bind(gid.get() as i64)
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
        // Quarantined by staff with a reason: told why, so no ticket.
        let case = |m: u64, reason: Option<&str>, n: i32| {
            let t = &t;
            let reason = reason.map(String::from);
            async move {
                sqlx::query("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, reason) VALUES ($1, $2, 'quarantine', $3, $4, $5)")
                    .bind(gid.get() as i64)
                    .bind(n)
                    .bind(u as i64)
                    .bind(m as i64)
                    .bind(reason)
                    .execute(&t.app.pool)
                    .await
                    .unwrap();
            }
        };
        case(crate::test_support::ME, Some("anti-nuke"), 1).await;
        case(9, Some("spamming"), 2).await;
        send_mail(&t.app, &dm("let me out"), gid, None)
            .await
            .unwrap();
        assert_eq!(said("were given the reason"), 1);
        // That staff case is from an earlier, lifted quarantine: the current one is the bot's,
        // which told them nothing.
        sqlx::query("UPDATE cases SET created_at = now() - interval '1 day' WHERE guild_id = $1 AND case_no = 2")
            .bind(gid.get() as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        // The server has types: the member picks one before the channel is made.
        send_mail(&t.app, &dm("why am I quarantined?"), gid, None)
            .await
            .unwrap();
        assert_eq!(said(&format!("tk:kind:{gid}:5")), 1);
        assert_eq!(said(&format!("POST /guilds/{gid}/channels")), 0);
        // A type the server no longer has asks again.
        send_mail(&t.app, &dm("why am I quarantined?"), gid, Some("Gone"))
            .await
            .unwrap();
        assert_eq!(said(&format!("tk:kind:{gid}:5")), 2);
        send_mail(&t.app, &dm("why am I quarantined?"), gid, Some("Report"))
            .await
            .unwrap();
        assert_eq!(said("-report-"), 1, "the type is in the channel name");
        assert_eq!(
            said(&format!("POST /guilds/{gid}/channels")),
            1,
            "ticket opened"
        );
        assert_eq!(said("Quarantined"), 1);
        // The staff buttons on the first message, and no type menu: the ticket has its type.
        assert_eq!(said("tk2:claim"), 1);
        assert_eq!(said("tk2:kindset"), 0);
        // The server cap: 20 new tickets an hour from anyone.
        sqlx::query("INSERT INTO tickets (guild_id, number, user_id, closed_at, status) SELECT $1, n, n, now(), 'closed' FROM generate_series(100, 119) n")
            .bind(gid.get() as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE tickets SET closed_at = now(), status = 'closed' WHERE guild_id = $1")
            .bind(gid.get() as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM quarantines WHERE guild_id = $1")
            .bind(gid.get() as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM tickets WHERE guild_id = $1 AND user_id = $2")
            .bind(gid.get() as i64)
            .bind(u as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        send_mail(&t.app, &dm("help"), gid, Some("Report"))
            .await
            .unwrap();
        assert_eq!(said("too many new tickets right now"), 1);
        sqlx::query("DELETE FROM ticket_users WHERE user_id = $1")
            .bind(u as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM tickets WHERE guild_id = $1")
            .bind(gid.get() as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        t.cleanup().await;
    }
}

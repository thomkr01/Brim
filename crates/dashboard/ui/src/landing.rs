//! The front page for visitors who are not logged in (design system: FrontPage, FeaturePicker,
//! SiteFooter, AppealSheet). Classes come from `design/bundle.css`; this file adds only behaviour.

use leptos::{html, prelude::*};

/// One piece of a scene: a sample message from the bot (E), a small mock panel (P: title and rows) or .
#[derive(Clone, Copy)]
enum Item {
    E(&'static str),
    P(&'static str, &'static [&'static str]),
    /// The same sample three times, each with its own edit from EDITS.
    X(&'static str),
    /// The Channel Setup grid: role columns, rows of (channel or category name, one mark per role).
    G(&'static [&'static str], &'static [(&'static str, &'static [&'static str])]),
    /// A dashboard list card: title, header button, rows of (name, grey text, action pills joined by |).
    L(&'static str, &'static str, &'static [(&'static str, &'static str, &'static str)]),
    /// A dashboard table: title and (name, state) rows.
    T(&'static str, &'static [(&'static str, &'static str)]),
}
use Item::{E, G, L, P, T, X};

/// Line text, layout class (see .l-* in style.css) and the pieces of its scene.
type Scene = (&'static str, &'static str, &'static [Item]);
/// Feature sections: title and clickable lines; each line shows its own scene of overlapping windows.
const SECTIONS: [(&str, &[Scene]); 9] = [
    ("Setup Wizard.", &[
        ("Answer a few questions", "base", &[P("Logging channel", &["-", "?Where should Brim log its actions?", "Logs|#brim-logs ▾", "Moderation|#mod-logs ▾", "Appeals|#appeals ▾", ">Next"]), P("", &["~Main role|@Member"]), P("Main roles", &["?Who may enter the server.", "Main role|@Member", ">Next"])]),
        ("Switch on the protection you want", "baseb", &[P("Protection", &["?Pick what Brim watches.", "Auto Mod", "Anti-nuke", "Anti-raid", "Verification", ">Next"]), P("", &["~Anti-raid|Join gate|On"]), P("Antinuke status", &["?Watch your staff.", "Monitor staff actions", "Quarantine at the limit"])]),
        ("Set up verification in one step", "basec", &[P("Verification", &["-", "?Members solve a captcha first.", "Verification", "Web verify", ">Set up channel"]), P("", &["~Verify channel|#verify"]), E("verify_panel")])]),
    ("Verification.", &[
        ("Members solve a captcha before they talk", "base", &[P("Captcha", &["?Members type the letters shown.", "Time|2 min", "Attempts|3", "Captcha", "Web verify"]), P("", &["~Captcha|2 min|3 tries"]), E("verify_captcha")]),
        ("A panel they press to start", "baseb", &[P("Verify panel", &["?The message members press.", "Channel|#verify", "Button|Verify", "Help button", ">Post"]), P("", &["~Post panel|#verify"]), E("verify_panel")]),
        ("Set up in the dashboard", "basec", &[P("Verification", &["?Who gets in, and how.", "Captcha", "Web verify", "Give role on pass", "Role|@Member", ">Save"]), P("", &["~Give role|@Member"]), E("verify_help")])]),
    ("Anti-raid.", &[
        ("Joins pause when a wave of accounts arrives", "base", &[P("Join gate", &["?Pause joins during a wave.", "Join gate", "Raider list", "Limit|20 per 10s", "Pause|30m ▾", ">Save"]), P("", &["~Pause joins|30m|20 per 10s"]), E("raid")]),
        ("Every raid gets a report", "baseb", &[P("Raid reports", &["?A report after every raid.", "Post report", "Channel|#raid-log ▾", "Ping|@Staff", ">Save"]), P("", &["~Report|#raid-log"]), E("raid_report")]),
        ("Known raiders are flagged on arrival", "basec", &[P("Anti-raid", &["?Flag known raiders.", "Join gate", "Raider list", "Quarantine newcomers", ">Save"]), P("", &["~Raider list|Flag|Quarantine"]), E("raider_alert")])]),
    ("Anti-nuke.", &[
        ("Mass bans and deletes are stopped", "base", &[P("Anti-nuke limits", &["?Stop mass bans and deletes.", "Bans|6 per hour", "Role deletes|3 per hour", "Channel deletes|4 per min", "Watch staff actions", ">Save"]), P("", &["~Bans|6 per hour|Quarantine ▾"]), E("nuke_quarantined")]),
        ("Rogue bots are kicked", "baseb", &[P("Bots", &["?Unknown bots are kicked.", "Kick unknown bots", "Allow list|2 bots", "Notify|@Owner", ">Save"]), P("", &["~Bot kick|Instant"]), E("nuke_bot_kicked")]),
        ("Staff powers are watched", "basec", &[P("Anti-nuke", &["?Stop staff who delete or ban too fast.", "Bans|6 per hour", "Role deletes|3 per hour", "Watch staff actions", "Quarantine at the limit", "Restore from backup", ">Save"]), P("", &["~Channel deletes|4 per min"]), E("panic_report_loaded")])]),
    ("Auto Mod.", &[
        ("Warn for blocked words", "base", &[P("Blocked words", &["?Remove messages with words you list.", "Words|free nitro", "Exempt|@Staff", "Filter", "Action|Warn ▾", "Strikes", "Delete", "Log", ">Save"]), P("", &["~Warn|3 strikes|Spam"]), E("automod_warn")]),
        ("Kick for invite links", "baseb", &[P("Invite links", &["?Remove invite links to other servers.", "Filter", "Action|Kick ▾", "Allow|#partners", "Strikes", "Delete", "Log", ">Save"]), P("", &["~Kick|1 strike|Invite"]), E("automod_kick")]),
        ("Time out spammers", "basec", &[P("Spam", &["?Stop floods of repeated messages.", "Filter", "Action|Timeout ▾", "Limit|5 per 4s", "Strikes", "Delete", "Log", ">Save"]), P("", &["~Timeout|10m|Spam"]), E("automod_timeout")])]),
    ("Moderation.", &[
        ("Ban with a reason", "base", &[P("Ban", &["?Ban with a reason.", "Member|@Mika", "Reason|DM ads", "Delete messages|7d ▾", "Notify member", ">Ban"]), P("", &["~Reason|DM ads|3d"]), E("ban_result")]),
        ("Short or full replies, your choice", "baseb", &[P("Replies", &["?Pick how Brim answers.", "Style|Full ▾", "Show reason", "Show case", "Show duration", ">Save"]), P("", &["~Style|Compact"]), E("ban_compact")]),
        ("Cases your whole team can see", "basec", &[T("Cases", &[("#4151 ban", "DM ads"), ("#4150 timeout", "Spam"), ("#4149 warn", "Bad words"), ("#4148 ban", "Raid")]), P("", &["~Case 4151|Ban|3d"]), E("note_modified")])]),
    ("Logging.", &[
        ("Monitor every action on your server", "base", &[P("Log channels", &["?Each kind of log has its own channel.", "Moderation|#mod-logs ▾", "Auto Mod|#automod-logs ▾", "Anti-nuke|#nuke-logs ▾", "Verification|#verify-logs ▾", ">Save"]), P("", &["~Moderation|#mod-logs"]), E("mod_log")]),
        ("A log for every kind of event", "baseb", &[P("Log types", &["?Pick what Brim writes.", "Message deleted", "Member banned", "Role updated", "Channel created", "Invite created"]), P("", &["~Role updated|#brim-logs"]), E("note_modified")]),
        ("Ignore users, roles or channels", "basec", &[P("Ignored", &["?Brim skips these in your logs.", "Users|Add", "Roles|Add", "Channels|Add", ">Save"]), P("", &["~Ignore|@Bots|#spam"]), E("mod_log")])]),
    ("Tickets.", &[
        ("Members open a private ticket", "base", &[P("Ticket panel", &["?Members press to open.", "Channel|#support", "Button|Open ticket", "Category|Tickets ▾", ">Post"]), P("", &["~Open ticket|1 per member"]), E("ticket_intro")]),
        ("Staff see the new ticket", "baseb", &[P("Tickets", &["?Where members open a ticket.", "Panel|#support", "Staff|@Support", "Limit|3 per member", "Transcript on close", ">Save"]), P("", &["~Close idle|3d|Notify"]), E("ticket_received")]),
        ("Replies reach the member", "basec", &[P("Replies", &["?Staff answer in the ticket.", "Staff|@Support", "Reply by DM", "Transcript", ">Save"]), P("", &["~Reply|DM|Transcript"]), E("ticket_reply")])]),
    ("Tools.", &[
        ("Build any message in the editor", "base", &[P("Message Builder", &["?Edit the message with the visual fields.", "Title|Verification", "Colour|Green", "Buttons", "Schedule", ">Preview", ">Post"]), P("", &["~Colour|Green|Button"]), X("verify_panel")]),
        ("See every change in History", "baseb", &[L("History", "", &[("Welcome message", "Brim · today", "Diff|Revert"), ("Log channels", "Mika · yesterday", "Diff|Revert"), ("Anti-raid", "Sam · Nov 20", "Diff|Revert")]), P("", &["~Diff|+ Asked twice|− Asked"]), P("Reverted", &["?Welcome message is back."])]),
        ("Set up channels and roles", "basec", &[G(&["Everyone", "Member", "Moderator"], &[("\u{25be} Info", &["\u{2713}", "\u{2713}", "\u{2713}"]), ("# rules", &["\u{2713}", "\u{2713}", "\u{2713}"]), ("\u{25be} Chat", &["\u{2717}", "\u{2713}", "\u{2713}"]), ("# general", &["\u{2717}", "\u{2713}", "\u{2713}"])]), P("", &["~# general|Member|Deny"]), P("Add role", &["?Pick a role.", "Role|@Member ▾", ">Add"])]),
        ("Follow feeds into a channel", "base", &[L("Feeds", "Add feed", &[("Channel uploads", "YouTube", "Edit|Delete"), ("Blog posts", "RSS", "Edit|Delete"), ("Releases", "GitHub", "Edit|Delete")]), P("", &["~Sources|YouTube|RSS|GitHub|GitLab"]), P("New post", &["?Posted to #videos.", "Title|New video is out"])]),
        ("Back up and load a layout", "baseb", &[L("Backups", "Create backup", &[("#a1f3", "Brim · today", "View|Load"), ("#9c20", "Brim · yesterday", "View|Load"), ("#77de", "Brim · Nov 18", "View|Load")]), P("", &["~Load|#a1f3|Preview"]), P("Restore", &["?Preview first, then load.", "Roles", "Channels", ">Load"])])]),
];

/// Each scene is drawn 616 wide and scaled to fit its box (at most 0.85), so it keeps every window.
/// shortcut: polled twice a second, because the samples and tab changes alter a scene's height.
/// Shortens a tall message to whole fields: later lines of the embed are hidden until the card fits under `limit` (design px),
/// and a heading is never the last visible line. The card and its embed box keep their normal padding.
fn cap_message(scene: &web_sys::HtmlElement, limit: f64) {
    use wasm_bindgen::JsCast;
    let Ok(Some(win)) = scene.query_selector(":scope > div:nth-of-type(3):not(.pane):not(.xs)") else { return };
    if scene.query_selector(".feat-skel").ok().flatten().is_some() || win.get_attribute("data-cap").is_some() { return }
    let _ = win.set_attribute("data-cap", "1");
    let (Ok(win), Ok(items)) = (win.clone().dyn_into::<web_sys::HtmlElement>(), win.query_selector_all(".embed .fields > .blk")) else { return };
    // Hide whole field rows (name and value together, an inline row as one) from the end only; the first row always stays.
    let top = |i| items.item(i).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok());
    let mut keep = items.length();
    while keep > 1 && f64::from(win.offset_height()) > limit {
        let Some(last) = top(keep - 1) else { return };
        let y = last.offset_top();
        while keep > 1 && top(keep - 1).is_some_and(|l| l.offset_top() == y) {
            if let Some(l) = top(keep - 1) { let _ = l.style().set_property("display", "none"); }
            keep -= 1;
        }
    }
}

fn fit() {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(boxes) = doc.query_selector_all(".feat-shot") else { return };
    use wasm_bindgen::JsCast;
    let html = |n: Option<web_sys::Node>| n.and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok());
    for i in 0..boxes.length() {
        let Some(b) = html(boxes.item(i)) else { continue };
        let w = f64::from(b.client_width());
        let Ok(scenes) = b.query_selector_all(".scene") else { continue };
        // Every scene gets its own scale, so a scene that fades in already has its final size on the first frame.
        for j in 0..scenes.length() {
            let Some(s) = html(scenes.item(j)) else { continue };
            cap_message(&s, if w < 500.0 { 336.0 } else { 280.0 });
            let nat = f64::from(s.offset_width());
            let k = (w / nat).min(0.85).min(((if w < 500.0 { 900.0 } else { 520.0 }) / f64::from(s.offset_height())).max(0.55));
            let _ = s.style().set_property("--k", &k.to_string());
            let _ = s.style().set_property("--dx", &format!("{}px", (w - nat * k) / 2.0));
            if s.class_list().contains("on") {
                let _ = b.style().set_property("--k", &k.to_string());
                let _ = b.style().set_property("--sh", &format!("{}px", s.offset_height()));
                let _ = b.style().set_property("--fxl", &format!("{}px", (w - nat * k) / 2.0));
                let _ = b.style().set_property("--fxw", &format!("{}px", nat * k));
            }
        }
    }
}

/// The three edits of a default message: accent colour and a new first line of text.
const EDITS: [(u64, &str); 3] = [(0x5865f2, "Welcome! One step left."), (0xe91e63, "Hey there, press the button."), (0x1be725, "Verify to join the fun.")];

/// A sample as the bot sends it, with the template's `{prefix}` filled from the sample prefix (the one in the bot's sample variables).
/// A sample as a window body; until the samples load, a calm pulsing block of about the same size.
fn shown(m: serde_json::Value) -> AnyView {
    if m.is_null() { view! { <div class="skel feat-skel"></div> }.into_any() } else { crate::discord::show_msg(m) }
}

fn sample(key: &str) -> serde_json::Value {
    let m = crate::discord::sample(key);
    if m.is_null() { return m }
    serde_json::from_str(&m.to_string().replace("{prefix}", "t!")).unwrap_or(m)
}

/// Shapes that belong to a scene: a pair at two random corners (a big sparkle with a small hex, and the other way round),
/// one at a side edge, and a few rising motes. Each has its own float and twinkle timing. They sit behind the windows,
/// so they only peek out at corners and edges.
fn effects() -> impl IntoView {
    let r = || js_sys::Math::random();
    // Three of the four corners, each with a shield and a spark outside the group's edge.
    let mut c = [0, 1, 2, 3];
    for n in (1..4).rev() { c.swap(n, (r() * (n + 1) as f64) as usize); }
    (0..3)
        .flat_map(|k| {
            let side = if c[k] % 2 == 0 { "left" } else { "right" };
            let edge = if c[k] < 2 { "top" } else { "bottom" };
            let (a, b) = if k % 2 == 0 { ("spark ol lg", "spark") } else { ("spark lg", "spark ol") };
            [(a, side, edge, 1.0, 1.0), (b, side, edge, 12.0, 0.5 + r() * 1.5)]
        })
        .map(|(class, side, edge, x, y)| {
            // Only the spark turns; the shield always stands upright.
            let turn = if class.starts_with("spark") { -40.0 + r() * 80.0 } else { 0.0 };
            let style = format!("{side}:{x:.0}%;{edge}:{y:.0}%;rotate:{turn:.0}deg;--gp:{:.0}%", r() * 100.0);
            view! { <i class=class style=style aria-hidden="true"></i> }
        })
        .collect::<Vec<_>>()
}

const REPO: &str = "https://github.com/thomkr01/Brim/blob/main";

/// The appeal form needs a login; this sends the visitor there and back to the form.
fn appeal_login(id: &str) -> Option<String> {
    let ok = (17..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| format!("/auth/login?next=/%23/appeal/{id}"))
}

#[component]
pub fn Landing(
    /// The front page opened from the dashboard's logo (`#/front`): its buttons go back to the dashboard.
    #[prop(optional)]
    signed_in: bool,
) -> impl IntoView {
    // A ban DM links to #/appeal/<id>, and the login comes back to the page that was open.
    let hash = web_sys::window().unwrap().location().hash().unwrap_or_default();
    let login = if signed_in {
        "#/".to_string()
    } else {
        format!("/auth/login?next=/{}", js_sys::encode_uri_component(&hash))
    };

    // The top bar stays hidden until the visitor has scrolled past the hero's copper line. On a short
    // screen the page ends before the line can leave the top, so half of the scroll range counts too.
    let hero = NodeRef::<html::Section>::new();
    let bar = RwSignal::new(false);
    let check = move || {
        let (Some(Some(h)), Some(w)) = (hero.try_get_untracked(), web_sys::window()) else { return };
        let y = w.scroll_y().unwrap_or(0.0);
        let room = w.document().and_then(|d| d.document_element()).map_or(0.0, |e| f64::from(e.scroll_height()))
            - w.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
        let past = y > 0.0 && y >= (y + h.get_bounding_client_rect().bottom()).min(room / 2.0);
        if bar.try_get_untracked() != Some(past) {
            bar.try_set(past);
        }
    };
    let on_scroll = window_event_listener(leptos::ev::scroll, move |_| check());
    let on_resize = window_event_listener(leptos::ev::resize, move |_| check());
    on_cleanup(move || {
        on_scroll.remove();
        on_resize.remove();
    });
    // A reload keeps the scroll position, so the first check waits for the page to be drawn.
    request_animation_frame(check);

    request_animation_frame(fit);
    if let Ok(h) = set_interval_with_handle(fit, std::time::Duration::from_millis(500)) {
        on_cleanup(move || h.clear());
    }

    let sheet = NodeRef::<html::Dialog>::new();
    let id = RwSignal::new(String::new());
    let err = RwSignal::new("");

    view! {
        <div class="tb-root tb-page land">
            // "line" = version A, a copper line closes the section. Remove it for version B, the long fade (both in style.css).
            <section class="hero line" node_ref=hero>
                <div class="hero-split" aria-hidden="true"></div>
                <svg class="hero-lines" viewBox="0 0 1000 600" preserveAspectRatio="none" aria-hidden="true">
                    <path d="M500 0 L315 600" /><path d="M0 130 H210 L240 100 H340" /><path class="hot" d="M0 70 H70" />
                </svg>
                <div class="hero-stripes" aria-hidden="true"></div>
                <header class="tb-header" class:is-shown=move || bar.get()>
                    <a class="tb-brand" href="#/"><img src="/logo.svg" alt="" width="30" height="30" />"Brim"</a>
                    <a class="tb-btn tb-btn-header" href=login.clone()>{if signed_in { view! { "Dashboard" }.into_any() } else { view! { <span class="tb-login-long">"Log in with Discord"</span><span class="tb-login-short">"Log in"</span> }.into_any() }}</a>
                </header>
                <main class="hero-body">
                    <div class="hero-copy">
                        <h1 class="hero-name">"Brim"</h1>
                        <p class="hero-slogan">"Sleep. Brim doesn\u{2019}t."</p>
                        <p class="tb-lead">"Brim keeps out raids, scams and rogue staff, so you can enjoy your community. Free, with every feature."</p>
                        <div class="tb-actions">
                            <a class="tb-btn tb-btn-primary" href="/invite">"Invite bot"</a>
                            <a class="tb-btn tb-btn-quiet" href=login.clone()>"Go to dashboard"</a>
                        </div>
                    </div>
                    <div class="hero-brim">
                        <img src="/brim.svg" alt="Brim, a smirking mushroom with an orange cap" width="560" height="560" />
                    </div>
                </main>
                <div class="hero-fade" aria-hidden="true"></div>
            </section>
            <div class="land-rest">
                <p class="tb-tagline">"Built in Rust: fast, light and hard to break."</p>
                <h2 class="feat-head">"Features that do the work."</h2>
                <div class="feat-line" aria-hidden="true"></div>
                <p class="feat-sub">"Free, with every feature."</p>
                <div class="feats">
                {SECTIONS.iter().enumerate().map(|(i, (title, tabs))| {
                    let on = RwSignal::new(0usize);
                    view! {
                        <section class="feat" class:flip=i % 2 == 1 class:big=i == 0>
                            <div class="feat-copy">
                                <h3>{*title}</h3>
                                <div class="feat-tabs" role="group" aria-label=format!("{title} examples")>
                                    {tabs.iter().enumerate().map(|(t, (label, _, _))| view! {
                                        <button class="feat-tab" type="button" aria-pressed=move || (on.get() == t).to_string() on:click=move |_| { on.set(t); request_animation_frame(fit); }>
                                            <span class="feat-dot" aria-hidden="true"></span>{*label}<span class="feat-arrow" aria-hidden="true">"\u{203a}"</span>
                                        </button>
                                    }).collect_view()}
                                </div>
                            </div>
                            <div class="feat-shot"><div class="fx">{effects()}</div>
                                {tabs.iter().enumerate().map(|(t, tab)| view! { <div class=format!("scene l-{}", tab.1) class:on=move || on.get() == t>{move || tab.2.iter().map(|it| match *it {
                                    E(key) => view! { <div class="win">{shown(sample(key))}</div> }.into_any(),
                                    P(name, rows) => view! { <div class="win pane card">{(!name.is_empty()).then(|| view! { <b class="ph">{name}</b> })}{rows.iter().map(|row| match (row.strip_prefix('?'), row.strip_prefix('>'), row.split_once('|')) {
                                        // One row with several fields: a switch, its name, then one chip per field.
                                        _ if row.starts_with('~') => { let mut f = row[1..].split('|'); view! { <div class="tog"><input type="checkbox" class="sw" prop:checked=true disabled=true aria-label="on" /><label>{f.next()}</label>{f.map(|c| view! { <span class=if c.ends_with('▾') { "sel" } else { "chip" }>{c}</span> }).collect_view()}</div> }.into_any() }
                                        (Some(t), _, _) => view! { <p class="muted">{t}</p> }.into_any(),
                                        (_, Some(t), _) => view! { <span class="tb-btn tb-btn-primary fakebtn">{t}</span> }.into_any(),
                                        (_, _, Some((a, b))) => view! { <div class="tog"><label>{a}</label><span class=if b.starts_with('@') { "rolechip" } else if b.ends_with('▾') { "sel" } else { "chip" }>{b}</span></div> }.into_any(),
                                        _ if *row == "-" => view! { <div class="pbar"><i></i></div> }.into_any(),
                                        _ => view! { <div class="tog"><label>{*row}</label><input type="checkbox" class="sw" prop:checked=true disabled=true aria-label=*row /></div> }.into_any(),
                                    }).collect_view()}</div> }.into_any(),
                                    L(title, btn, rows) => view! { <div class="win lcard card"><div class="lhead"><b>{title}</b>{(!btn.is_empty()).then(|| view! { <span class="tb-btn tb-btn-primary fakebtn">{btn}</span> })}</div>{rows.iter().map(|(id, meta, acts)| view! { <div class="lrow"><span class="lname"><b>{*id}</b><small>{*meta}</small></span><span class="lacts">{acts.split('|').map(|a| view! { <span class="pill">{a}</span> }).collect_view()}</span></div> }).collect_view()}</div> }.into_any(),
                                    G(roles, rows) => view! { <div class="win lcard card"><div class="lhead"><b>"Channel Setup"</b></div><p class="muted">"Each role's Discord permissions here: \u{2713} allow, \u{2717} deny, \u{2013} neutral."</p>
                                        <table class="data matrix"><thead><tr><th></th>{roles.iter().map(|r| view! { <th>{*r}</th> }).collect_view()}</tr></thead><tbody>{rows.iter().map(|(name, marks)| view! { <tr class:cat=name.starts_with('\u{25be}')><td><b>{*name}</b></td>{marks.iter().map(|m| view! { <td><span class="gmark" class:al=*m == "\u{2713}" class:de=*m == "\u{2717}">{*m}</span></td> }).collect_view()}</tr> }).collect_view()}</tbody></table>
                                        <div class="lhead"><span class="sel">"Member \u{25be}"</span><span class="pill">"Add role"</span></div></div> }.into_any(),
                                    X(key) => {
                                        let m = sample(key);
                                        view! { <div class="win xs">{EDITS.iter().take(2).map(|(color, text)| {
                                            let mut m = m.clone();
                                            if let Some(e) = m["embeds"].get_mut(0) {
                                                e["color"] = (*color).into();
                                                if e["description"].is_string() { e["description"] = (*text).into(); }
                                            }
                                            shown(m)
                                        }).collect_view()}</div> }.into_any()
                                    }
                                    T(name, rows) => view! { <div class="win tbl"><b>{name}</b><table class="data"><tbody>{rows.iter().map(|(a, b)| view! { <tr><td>{*a}</td><td>{b.split('|').map(|c| view! { <span class="chip">{c}</span>" " }).collect_view()}</td></tr> }).collect_view()}</tbody></table></div> }.into_any(),
                                }).collect_view()}</div> }).collect_view()}
                            </div>
                        </section>
                    }
                }).collect_view()}
                </div>
                <p class="land-edit">"Every embed Brim sends can be "<span class="u">"edited"</span>" in the message editor."</p>
                <section class="land-close">
                    <i class="spark" aria-hidden="true"></i><i class="spark ol lg" aria-hidden="true"></i>
                    <p class="feat-sub">"Free, with every feature."</p>
                    <h2>"Put the "<span class="u">"safeguards"</span>" in place now."</h2>
                    <div class="tb-actions">
                        <a class="tb-btn tb-btn-primary" href="/invite">"Invite bot"</a>
                        <a class="tb-btn tb-btn-quiet" href=login>"Go to dashboard"</a>
                    </div>
                </section>
            </div>
            <footer class="tb-footer">
                <a class="tb-link tb-link-strong" href="#/commands">"Docs"</a>
                <button class="tb-link tb-link-strong" type="button" on:click=move |_| { if let Some(d) = sheet.get() { let _ = d.show_modal(); } }>"Appeal a ban"</button>
                <a class="tb-link" href=format!("{REPO}/TERMS.md")>"Terms"</a>
                <a class="tb-link" href=format!("{REPO}/PRIVACY.md")>"Privacy"</a>
                <a class="tb-link tb-link-rust" href="https://www.rust-lang.org/">"Made in Rust "<span aria-hidden="true">"🦀"</span></a>
            </footer>
            <dialog class="tb-sheet" aria-labelledby="tb-appeal-title" node_ref=sheet
                on:click=move |e| {
                    // A tap on the backdrop lands on the dialog itself; taps inside land on its form.
                    if event_target::<web_sys::Element>(&e).tag_name() == "DIALOG" { if let Some(d) = sheet.get() { d.close(); } }
                }>
                <form novalidate on:submit=move |e| {
                    e.prevent_default();
                    match appeal_login(id.get_untracked().trim()) {
                        Some(to) => { let _ = web_sys::window().unwrap().location().set_href(&to); }
                        None => err.set("A server ID is 17 to 20 digits, with no letters or spaces."),
                    }
                }>
                    <h2 class="tb-sheet-title" id="tb-appeal-title">"Appeal a ban"</h2>
                    <label class="tb-label" for="tb-server-id">"Server ID"</label>
                    <input class="tb-field" id="tb-server-id" inputmode="numeric" enterkeyhint="go" autocomplete="off" spellcheck="false" autocapitalize="none" aria-describedby="tb-appeal-err"
                        prop:value=id on:input=move |e| { id.set(event_target_value(&e)); err.set(""); } />
                    <p class="land-err" id="tb-appeal-err" role="alert">{move || err.get()}</p>
                    <p class="land-help">"The link in your ban message opens the form directly. A friend in the server can copy the ID: right-click the server icon, then Copy Server ID."</p>
                    <button class="tb-btn tb-btn-primary" type="submit">"Appeal"</button>
                </form>
            </dialog>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::appeal_login;

    #[test]
    fn appeal_id_is_17_to_20_digits() {
        assert_eq!(appeal_login("12345678901234567").as_deref(), Some("/auth/login?next=/%23/appeal/12345678901234567"));
        assert!(appeal_login("123456789012345678901").is_none());
        assert!(appeal_login("1234567890123456").is_none());
        assert!(appeal_login("1234567890123456a7").is_none());
        assert!(appeal_login("").is_none());
    }
}

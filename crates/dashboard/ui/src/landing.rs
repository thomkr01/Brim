//! The front page for visitors who are not logged in (design system: FrontPage, FeaturePicker,
//! SiteFooter, AppealSheet). Classes come from `design/bundle.css`; this file adds only behaviour.

use leptos::{html, prelude::*};

/// One piece of a scene: a sample message from the bot (E), a small mock panel (P: title and rows) or .
#[derive(Clone, Copy)]
enum Item {
    E(&'static str),
    P(&'static str, &'static [&'static str]),
    /// A dashboard list card: title, header button, rows of (name, grey text, action pills joined by |).
    L(&'static str, &'static str, &'static [(&'static str, &'static str, &'static str)]),
    /// A dashboard table: title and (name, state) rows.
    T(&'static str, &'static [(&'static str, &'static str)]),
    /// A member's message in the channel: name and text.
    M(&'static str, &'static str),
    /// A big orange label on the scene, like the action a rule takes.
    C(&'static str),
    /// The Cases page: rows of (number, kind, open or closed, grey text).
    K(&'static [(&'static str, &'static str, &'static str, &'static str)]),
    /// A dashboard settings page, built from the page's own classes: breadcrumb, title and cards of (card title, fields of (kind, label, value)).
    /// Kinds: s switch (on), c choice, p channel picker, r role picker with chips, n text field.
    D(&'static str, &'static str, &'static [(&'static str, &'static [(char, &'static str, &'static str)])]),
}
use Item::{C, D, E, K, L, M, P, T};

/// Line text, layout class (see .l-* in style.css) and the pieces of its scene.
type Scene = (&'static str, &'static str, &'static [Item]);
/// Feature sections: title and clickable lines; each line shows its own scene of overlapping windows.
const SECTIONS: [(&str, &[Scene]); 9] = [
    ("Setup Wizard.", &[
        ("Answer a few questions", "stk", &[P("Logging channel", &["?Where do you want Brim to log its actions?", "Logs channel|#brim-logs ▾", ">Next step"]), P("Moderation logs channel", &["?Where do you want Brim to log moderation actions made by your staff members?", "Modlogs channel|#mod-logs ▾", ">Next step"]), P("Main roles", &["?What is the role your members need to access the server, something like @Member or @Verified? Select it.", "Main roles|@Member", ">Next step"])]),
        ("Switch on the protection you want", "stk", &[P("Automod status", &["?Do you want Brim to monitor messages?", "Enable Automod", "Moderate invite links posted", "Filter out NSFW links", ">Next step"]), P("Antinuke status", &["?Do you want Brim to monitor moderation actions made by your staff members?", "Enable Antinuke"]), P("Quarantine", &["?Do you want the quarantine system that keeps your server safe from being destroyed?", "Use the auto quarantine module", "Guard the server's vanity url", "Quarantine role|@Quarantine"])]),
        ("Set up verification in one step", "basec", &[P("Verification status", &["?Do you want to filter in only genuine members? Use the verification and fend off bots!", "Enable Verification", "Verification mode|Captcha ▾", ">Next step"]), E("verify_panel"), P("Join Gate status", &["Enable Join Gate"])])]),
    ("Verification.", &[
        ("Members solve a captcha before they talk", "base", &[T("Join log", &[("Mika joined", "new"), ("Mika solved the captcha", "verified"), ("bot-4821 failed the captcha", "kicked")]), E("verify_captcha"), M("Mika", "Hi, I just joined!"), C("Captcha")]),
        ("Make the panel your own", "base", &[P("Messages", &["Title|Verification Required!", "Description|To access `{guild.name}`, you need to pass the verification first.", ">Add field"]), E("verify_panel"), P("Preview", &["?Brim adds the buttons itself.", ">Edit message"]), C("Panel")]),
        ("Set up in the dashboard", "dash", &[D("Verification \u{203a} General Settings", "Verification", &[
            ("General", &[('s', "Use the verification module", ""), ('c', "Verification mode", "Captcha"), ('p', "Verification channel", "#verify"), ('c', "Fail action", "Kick"), ('c', "Verification targets", "Everyone")]),
            ("Captcha", &[('n', "Captcha answer length", "6"), ('s', "Show a colored trace that follows the captcha characters", ""), ('s', "Show small spread-around characters to trick scripts", "")])]), C("Settings")])]),
    ("Anti-raid.", &[
        ("Joins pause when a wave of accounts arrives", "base", &[T("Raider List", &[("raid-acct-1", "listed"), ("raid-acct-2", "listed"), ("raid-acct-3", "listed")]), E("raid"), M("New accounts", "8 joined in 10 seconds"), C("Raid")]),
        ("Every raid gets a report", "base", &[T("Raid report", &[("8 joins in 10 s", "paused"), ("6 accounts without avatar", "flagged"), ("Joins resumed", "ok")]), E("raid_report"), M("Mika", "Joined 3 minutes ago, no avatar"), C("Report")]),
        ("Known raiders are flagged on arrival", "base", &[T("Raider List", &[("raid-acct-1", "listed"), ("raid-acct-2", "listed"), ("raid-acct-3", "listed")]), E("raider_action"), M("Mika", "Hey, I just joined"), C("Listed")])]),
    ("Anti-nuke.", &[
        ("Mass bans and deletes are stopped", "base", &[K(&[("#31", "nuke", "open", "rogue-admin \u{b7} 9 deleted"), ("#30", "nuke", "closed", "old-bot \u{b7} 14 bans")]), E("nuke_quarantined"), M("rogue-admin", "Deleting every channel..."), C("Stopped")]),
        ("Rogue bots are kicked", "base", &[K(&[("#32", "bot kicked", "open", "added Unknown Bot"), ("#29", "bot kicked", "closed", "Jo added Music Bot")]), E("nuke_bot_kicked"), M("rogue-admin", "Adding a bot to the server"), C("Kicked")]),
        ("Staff powers are watched", "base", &[K(&[("#33", "panic", "open", "rogue-admin \u{b7} 5 bans"), ("#31", "nuke", "closed", "rogue-admin \u{b7} roles")]), E("panic_report_loaded"), M("rogue-admin", "Banning 5 members at once"), C("Panic")])]),
    ("Auto Mod.", &[
        ("Warn for blocked words", "base", &[T("Action log", &[("Mika said a blocked word", "warn"), ("Mika: 2nd blocked word", "warn"), ("Mika: 3rd blocked word", "timeout")]), E("automod_warn"), M("Mika", "Free nitro, click here!!"), C("Warn")]),
        ("Kick for invite links", "base", &[T("Action log", &[("Mika posted discord.gg/free", "kick"), ("Lee posted an invite", "delete"), ("Jo posted an invite", "delete")]), E("automod_kick"), M("Mika", "Join my server: discord.gg/free"), C("Kick")]),
        ("Time out spammers", "base", &[T("Action log", &[("Mika spammed 5 messages", "timeout"), ("Mika timed out 10 min", "timeout"), ("Lee spammed 6 messages", "timeout")]), E("automod_timeout"), M("Mika", "buy now! buy now! buy now!"), C("Timeout")])]),
    ("Moderation.", &[
        ("Ban with a reason", "base", &[K(&[("#4151", "ban", "open", "Mika by Sam \u{b7} toxic \u{b7} 2 proofs"), ("#4150", "kick", "closed", "Lee by Sam \u{b7} advertising"), ("#4149", "timeout", "closed", "Jo by Brim \u{b7} spam \u{b7} 10 min")]), E("dm_ban"), M("Sam", "t!ban @Mika toxic"), C("Ban")]),
        ("Short or full replies, your choice", "base", &[K(&[("#4152", "ban", "open", "Mika by Sam \u{b7} toxic"), ("#4151", "warn", "closed", "Lee by Sam \u{b7} language"), ("#4150", "purge", "closed", "40 messages by Sam")]), E("ban_compact"), M("Sam", "t!ban @Mika"), C("Compact")]),
        ("Cases your whole team can see", "base", &[K(&[("#4151", "ban", "open", "Mika by Sam \u{b7} toxic \u{b7} 2 proofs"), ("#4150", "kick", "closed", "Lee by Sam \u{b7} advertising"), ("#4149", "quarantine", "open", "Ola by Brim \u{b7} raid account"), ("#4148", "slowmode", "closed", "#general by Sam \u{b7} 10 s")]), E("note_modified"), C("Cases")])]),
    ("Logging.", &[
        ("Monitor every action on your server", "base", &[T("Action log", &[("Sam banned Mika", "ban"), ("Sam noted Mika", "note"), ("Brim kicked Bot", "kick")]), E("mod_log"), M("Sam", "t!ban @Mika toxic"), C("Logs")]),
        ("A log for every kind of event", "base", &[T("Member log", &[("Mika joined", "join"), ("Mika left", "leave"), ("Mika renamed", "name")]), E("note_modified"), M("Sam", "t!note @Mika asked for a refund"), C("Events")]),
        ("Ignore users, roles or channels", "base", &[T("Action log", &[("Sam muted Lee", "timeout"), ("Sam warned Jo", "warn")]), E("mod_log"), M("Mika", "Posting in an ignored channel"), C("Ignored")])]),
    ("Tickets.", &[
        ("Members open a private ticket", "base", &[K(&[("#102", "ticket", "open", "Mika \u{b7} order help"), ("#101", "ticket", "closed", "Jo \u{b7} refund")]), E("ticket_intro"), M("Mika", "I need help with my order"), C("Ticket")]),
        ("Staff see the new ticket", "base", &[K(&[("#103", "ticket", "open", "Mika \u{b7} scam links"), ("#102", "ticket", "open", "Lee \u{b7} order help")]), E("ticket_received"), M("Mika", "Someone keeps sending me scam links"), C("Staff")]),
        ("Replies reach the member", "base", &[K(&[("#102", "ticket", "open", "Mika \u{b7} we are looking into it"), ("#99", "ticket", "closed", "Lee \u{b7} solved")]), E("ticket_reply"), M("Sam", "t!reply We are looking into it"), C("Reply")])]),
    ("Tools.", &[
        ("Build any message in the editor", "base", &[P("Messages", &["Title|Verification Required!", "Description|One step left.", "Field name|Rules", "Footer text|Brim", ">Add field", ">Add button row"]), E("verify_panel"), C("Editor")]),
        ("See every change in History", "stk", &[L("History", "", &[("v12", "by Mika · Nov 22", "Roll back"), ("v11", "by Sam · Nov 20", "Roll back"), ("v10", "by Brim · Nov 18", "Roll back")]), L("Server layout (Layout Guard)", "", &[("Layout v3", "saved from the server, by Mika", "Current"), ("Layout v2", "Channel Setup push, by Sam", "Roll back")]), P("Import settings", &["?This file changes nothing on this server.", ">Import"]), C("History")]),
        ("Set up channels and roles", "stk", &[L("Channels", "", &[("\u{25be} Information", "announcements, rules", "Read-only"), ("\u{25be} Text Channels", "general", "Public"), ("\u{25be} Voice Channels", "Voice", "Public"), ("\u{25be} Logs", "brim-logs, mod-logs", "Staff"), ("\u{25be} Staff", "staff-chat, Staff Voice", "Staff")]), P("Who can see this", &["Public|Verified members", "Read-only|Members read", "Staff|Only staff"]), C("Layout")]),
        ("Follow feeds into a channel", "stk", &[L("Feeds", "Add new feed", &[("Channel uploads", "YouTube", "Edit"), ("Blog posts", "RSS", "Edit"), ("Releases", "GitHub", "Edit")]), P("Add new feed", &["Link|youtube.com/@brim", "Channel|#uploads ▾", "Ping role|@Feeds ▾"]), P("More options", &["Source|Detect from the link ▾", "Send as Brim"]), C("Feeds")]),
        ("Back up and load a layout", "stk", &[L("Backups", "Create backup", &[("#a1f3", "Brim · today", "View|Load"), ("#9c20", "Brim · yesterday", "View|Load"), ("#77de", "Brim · Nov 18", "View|Load")]), P("Load backup #a1f3", &["Show permissions", ">Apply"]), C("Backups")])]),
];

/// A long sample message ends at a whole block: fields (name and value together, an inline row as one) go from the end,
/// the first always stays. `limit` is about 1.3 times the scene's first panel.
fn cap_message(scene: &web_sys::HtmlElement) {
    use wasm_bindgen::JsCast;
    let html = |n: Option<web_sys::Element>| n.and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok());
    let Some(first) = html(scene.query_selector(":scope > .pane, :scope > .lcard, :scope > .tbl").ok().flatten()) else { return };
    let Some(win) = html(scene.query_selector(":scope > .win:not(.pane):not(.lcard):not(.tbl):not(.xs):not(.mm)").ok().flatten()) else { return };
    if scene.query_selector(".feat-skel").ok().flatten().is_some() || win.get_attribute("data-cap").is_some() { return }
    let _ = win.set_attribute("data-cap", "1");
    let limit = (f64::from(first.offset_height()) * 1.3).max(280.0);
    let Ok(items) = win.query_selector_all(".embed .fields > .blk") else { return };
    let top = |i| html(items.item(i).and_then(|n| n.dyn_into::<web_sys::Element>().ok()));
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

/// The natural size of a scene's group of pieces and of its first piece: (group w, group h, first w, first h).
fn group(s: &web_sys::HtmlElement) -> (f64, f64, f64, f64) {
    use wasm_bindgen::JsCast;
    let kids = s.children();
    let (mut gw, mut gh, mut w1, mut h1) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for i in 0..kids.length() {
        let Some(c) = kids.item(i).and_then(|c| c.dyn_into::<web_sys::HtmlElement>().ok()) else { continue };
        gw = gw.max(f64::from(c.offset_left() + c.offset_width()));
        gh = gh.max(f64::from(c.offset_top() + c.offset_height()));
        if i == 0 { w1 = f64::from(c.offset_width()); h1 = f64::from(c.offset_height()); }
    }
    (gw, gh, w1, h1)
}

/// Every block has one fixed box (784 wide on a desktop, 392 on a phone, as tall as its tallest group plus 48 above and below).
/// Every scene is scaled to fit and starts at the same corner of the union of all groups, which is centred; the stars sit at fixed places in the box, outside every scene's pieces:
/// above the second piece, left of the group, and under the first piece beside the third. The proportions are the owner's
/// approved layout. shortcut: polled twice a second, because the samples and tab changes alter a scene's size.
fn fit() {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Ok(boxes) = doc.query_selector_all(".feat-shot") else { return };
    use wasm_bindgen::JsCast;
    let html = |n: Option<web_sys::Node>| n.and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok());
    let phone = win.inner_width().ok().and_then(|v| v.as_f64()).is_some_and(|v| v <= 760.0);
    let bw = if phone { 392.0 } else { 784.0 };
    for i in 0..boxes.length() {
        let Some(b) = html(boxes.item(i)) else { continue };
        let w = f64::from(b.client_width());
        let Ok(scenes) = b.query_selector_all(".scene") else { continue };
        let mut all = vec![];
        for j in 0..scenes.length() {
            let Some(s) = html(scenes.item(j)) else { continue };
            cap_message(&s);
            all.push((s.clone(), group(&s)));
        }
        let max = |f: fn(&(f64, f64, f64, f64)) -> f64| all.iter().map(|(_, g)| f(g)).fold(0.0, f64::max);
        let (mgw, mgh, mw1, mh1) = (max(|g| g.0), max(|g| g.1), max(|g| g.2), max(|g| g.3));
        let mnw1 = all.iter().map(|(_, g)| g.2).fold(f64::MAX, f64::min);
        // The bottom stars sit 72 px under the first piece, so a first piece that is the tallest gets more room below.
        let pad = (mh1 + 72.0 - mgh).max(48.0);
        let bh = mgh + 2.0 * pad;
        let k = (w / bw).min(0.85);
        let (l, t) = ((bw - mgw) / 2.0, pad);
        // Every scene starts at the same corner (the union of all groups is centred), so the stars stay clear of each one.
        for (s, _) in &all {
            let _ = s.style().set_property("--k", &k.to_string());
            let _ = s.style().set_property("--dx", &format!("{}px", (w - bw * k) / 2.0 + l * k));
            let _ = s.style().set_property("--dy", &format!("{}px", t * k));
        }
        let _ = b.style().set_property("--k", &k.to_string());
        let _ = b.style().set_property("--sh", &format!("{}px", bh * k));
        let _ = b.style().set_property("--fxl", &format!("{}px", (w - bw * k) / 2.0));
        let _ = b.style().set_property("--fxw", &format!("{}px", bw * k));
        // Centres of the six stars in the box: top left pair, top right pair, bottom left pair.
        let slots = if phone {
            [(l + 22.0, t - 22.0), (l + 52.0, t - 30.0), (l + mgw - 24.0, t - 20.0), (l + mgw - 52.0, t - 28.0), (l + 24.0, t + mgh + 22.0), (l + 54.0, t + mgh + 30.0)]
        } else {
            let bx = l + 0.115 * mnw1; // left of the third piece, which starts 23% into the first
            [(l - 41.0, t + 64.0), (l - 64.0, t + 38.0), (l + mnw1 + 14.0, t + 10.0), (l + mnw1 + 40.0, t + 30.0), (bx, t + mh1 + 22.0), (bx - 6.0, t + mh1 + 56.0)]
        };
        let Ok(stars) = b.query_selector_all(".fx > i") else { continue };
        for n in 0..stars.length().min(6) {
            let Some(st) = html(stars.item(n)) else { continue };
            let (x, y) = slots[n as usize];
            let half = f64::from(st.offset_width()) / 2.0;
            let _ = st.style().set_property("left", &format!("{}px", { let l = x * k - half; if phone { l.max(0.0).min(w - half * 2.0) } else { l } }));
            let _ = st.style().set_property("top", &format!("{}px", y * k - half));
        }
    }
}

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

/// The stars of a block, fixed in its box (`fit` sets their places): three pairs of a big and a small star, filled and outline.
/// Every star stands upright with the same gradient and glow, like the layout editor. The block's number decides which 0 to 2 stars are left out.
fn effects(seed: usize) -> impl IntoView {
    let mut x = (seed as u32 + 1).wrapping_mul(2_654_435_761) | 1;
    let mut r = move || { x ^= x << 13; x ^= x >> 17; x ^= x << 5; f64::from(x) / 4_294_967_296.0 };
    // The block's number decides how many stars are left out (0, 1 or 2) and the seed which.
    let left_out = seed % 3;
    let off: Vec<usize> = (0..left_out).map(|n| (n * 3 + (r() * 3.0) as usize) % 6).collect();
    (0..6)
        .map(|n| {
            let class = match n { 0 | 4 => "spark ol lg", 1 | 5 => "spark", 2 => "spark lg", _ => "spark ol" };
            let hide = if off.contains(&n) { "display:none;" } else { "" };
            view! { <i class=class style=hide aria-hidden="true"></i> }
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
                                <h3 class:deep=title.chars().take(4).any(|c| "gjpqy".contains(c))>{*title}</h3>
                                <div class="feat-tabs" role="group" aria-label=format!("{title} examples")>
                                    {tabs.iter().enumerate().map(|(t, (label, _, _))| view! {
                                        <button class="feat-tab" type="button" aria-pressed=move || (on.get() == t).to_string() on:click=move |_| { on.set(t); request_animation_frame(fit); }>
                                            <span class="feat-dot" aria-hidden="true"></span>{*label}<span class="feat-arrow" aria-hidden="true">"\u{203a}"</span>
                                        </button>
                                    }).collect_view()}
                                </div>
                            </div>
                            <div class="feat-shot"><div class="fx">{effects(i)}</div>
                                {tabs.iter().enumerate().map(|(t, tab)| view! { <div class=format!("scene l-{}", tab.1) class:on=move || on.get() == t>{move || tab.2.iter().map(|it| match *it {
                                    E(key) => view! { <div class="win">{shown(sample(key))}</div> }.into_any(),
                                    P(name, rows) => view! { <div class="win pane card">{(!name.is_empty()).then(|| view! { <b class="ph">{name}</b> })}{rows.iter().map(|row| match (row.strip_prefix('?'), row.strip_prefix('>'), row.split_once('|')) {
                                        // One row with several fields: a switch, its name, then one chip per field.
                                        (Some(t), _, _) => view! { <p class="muted">{t}</p> }.into_any(),
                                        (_, Some(t), _) => view! { <span class="tb-btn tb-btn-primary fakebtn">{t}</span> }.into_any(),
                                        (_, _, Some((a, b))) => view! { <div class="tog"><label>{a}</label><span class=if b.starts_with('@') { "rolechip" } else if b.ends_with('▾') { "sel" } else { "chip" }>{b}</span></div> }.into_any(),
                                        _ if *row == "-" => view! { <div class="pbar"><i></i></div> }.into_any(),
                                        _ => view! { <div class="tog"><label>{*row}</label><input type="checkbox" class="sw" prop:checked=true disabled=true aria-label=*row /></div> }.into_any(),
                                    }).collect_view()}</div> }.into_any(),
                                    L(title, btn, rows) => view! { <div class="win lcard card"><div class="lhead"><b>{title}</b>{(!btn.is_empty()).then(|| view! { <span class="tb-btn tb-btn-primary fakebtn">{btn}</span> })}</div>{rows.iter().map(|(id, meta, acts)| view! { <div class="lrow"><span class="lname"><b>{*id}</b><small>{*meta}</small></span><span class="lacts">{acts.split('|').map(|a| view! { <span class="pill">{a}</span> }).collect_view()}</span></div> }).collect_view()}</div> }.into_any(),
                                    C(text) => view! { <div class="co">{text}</div> }.into_any(),
                                    M(name, text) => view! { <div class="win mm"><i class="mmav" aria-hidden="true"></i><div><b>{name}</b><p>{text}</p></div></div> }.into_any(),
                                    K(rows) => view! { <div class="win tbl caselist stack">{rows.iter().map(|(no, kind, state, meta)| view! { <div class="card link"><b class="no">{*no}</b><span class="what"><span><b>{*kind}</b>" "<span class="chip" class:ok=(*state == "open")>{*state}</span></span><span class="muted">{*meta}</span></span></div> }).collect_view()}</div> }.into_any(),
                                    D(crumb, title, cards) => view! { <div class="win dash" inert=true><div class="head"><small>{crumb}</small><h1>{title}</h1></div><div class="cards">{cards.iter().map(|(name, fields)| view! { <div class="col"><div class="card"><h2>{*name}</h2>{fields.iter().map(|(k, label, v)| match k {
                                        's' => view! { <div class="row tog"><label>{*label}</label><input type="checkbox" class="sw" prop:checked=true /></div> }.into_any(),
                                        'c' => view! { <div class="row"><label>{*label}</label><select><option>{*v}</option></select></div> }.into_any(),
                                        'p' => view! { <div class="row"><label>{*label}</label><div class="lp"><input readonly value=*v /></div></div> }.into_any(),
                                        'r' => view! { <div class="row"><label>{*label}</label><div class="pick"><span class="chip">{*v}" "<button class="link" tabindex="-1">"\u{d7}"</button></span><div class="lp"><input readonly placeholder="Search" /></div></div></div> }.into_any(),
                                        _ => view! { <div class="row"><label>{*label}</label><input readonly value=*v /></div> }.into_any(),
                                    }).collect_view()}</div></div> }).collect_view()}</div></div> }.into_any(),
                                    T(name, rows) => view! { <div class="win tbl"><b>{name}</b><table class="data"><tbody>{rows.iter().map(|(a, b)| view! { <tr><td>{*a}</td><td>{b.split('|').map(|c| view! { <span class="chip">{c}</span>" " }).collect_view()}</td></tr> }).collect_view()}</tbody></table></div> }.into_any(),
                                }).collect_view()}</div> }).collect_view()}
                            </div>
                        </section>
                    }
                }).collect_view()}
                </div>
                <p class="land-edit">"Every embed Brim sends can be "<span class="u">"edited"</span>" in the message editor."</p>
                <div class="land-line" aria-hidden="true"></div>
                <p class="hero-name land-brand" aria-hidden="true">"Brim"</p>
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
    use super::{appeal_login, Item, SECTIONS};

    #[test]
    fn appeal_id_is_17_to_20_digits() {
        assert_eq!(appeal_login("12345678901234567").as_deref(), Some("/auth/login?next=/%23/appeal/12345678901234567"));
        assert!(appeal_login("123456789012345678901").is_none());
        assert!(appeal_login("1234567890123456").is_none());
        assert!(appeal_login("1234567890123456a7").is_none());
        assert!(appeal_login("").is_none());
    }

    /// The editor text of the verification panel scene is the bot's own default (botmsg.rs) in its public form, so a change there fails this test until the scene follows.
    #[test]
    fn verification_scene_text_is_the_bot_default() {
        let bot = include_str!("../../../logging/src/botmsg.rs");
        let rows: Vec<&str> = SECTIONS.iter().flat_map(|(_, t)| t.iter()).flat_map(|(_, _, items)| items.iter()).filter_map(|it| match it { Item::P(_, rows) => Some(rows.iter().copied()), _ => None }).flatten().collect();
        let desc = rows.iter().find_map(|r| r.strip_prefix("Description|To access")).expect("the verification scene has the panel description");
        // The editor shows the public name (`{guild.name}`). The code holds either that or the old code name `{server}`, which a mapping turns into it.
        let esc = |t: String| t.replace('{', "{{").replace('}', "}}");
        let public = esc(format!("To access{desc}"));
        let old = esc(format!("To access{desc}").replace("{guild.name}", "{server}"));
        assert!(bot.contains(&public) || bot.contains(&old), "default verify.panel text changed: {public}");
    }

    /// Every setting, card title and button a scene names exists in the dashboard's own source (schema, wizard cards, pages).
    #[test]
    fn scenes_name_real_settings() {
        let src: String = [include_str!("../../src/schema.rs"), include_str!("wizard.rs"), include_str!("main.rs"), include_str!("layout.rs"), include_str!("recovery.rs"), include_str!("feeds.rs"), include_str!("channels.rs"), include_str!("embeds.rs"), include_str!("raiders.rs"), include_str!("tickets.rs"), include_str!("pages.rs"), include_str!("discord.rs"), include_str!("form.rs")].concat();
        // Sample data a scene shows (names, ids, times) is not a setting.
        let mut bad = vec![];
        // A field without a title shows its path as words ("ignored_channels" is "Ignored channels"); a backup title carries its id.
        let mut check = |what: &str| {
            let what = what.replace("#a1f3", "#{id}");
            let path = what.to_lowercase().replace(' ', "_");
            if !what.is_empty() && !src.contains(&format!("\"{what}\"")) && !src.contains(&format!("\"{path}\"")) { bad.push(what) }
        };
        for (_, tabs) in SECTIONS.iter() {
            for (_, _, items) in tabs.iter() {
                for it in items.iter() {
                    match it {
                        Item::P(title, rows) => {
                            check(title);
                            for r in rows.iter() {
                                let r = r.trim_start_matches(['?', '>']);
                                check(r.split('|').next().unwrap_or(r).trim_end_matches(" \u{25be}"));
                            }
                        }
                        Item::L(title, btn, _) => { check(title); check(btn) }
                        Item::D(_, title, cards) => { check(title); for (name, fields) in cards.iter() { check(name); for (_, label, _) in fields.iter() { check(label) } } }
                        _ => {}
                    }
                }
            }
        }
        assert!(bad.is_empty(), "not found in the dashboard source: {bad:?}");
    }
}

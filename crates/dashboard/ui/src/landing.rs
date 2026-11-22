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
        ("Answer a few questions", "stk", &[P("Logging channel", &["?Where do you want Brim to log its actions?", "Logs channel|#brim-logs ▾", ">Next step"]), P("Moderation logs channel", &["?Where do you want Brim to log moderation actions made by your staff members?", "Modlogs channel|#mod-logs ▾", ">Next step"]), P("Main roles", &["?What is the role your members need to access the server, something like @Member or @Verified? Select it.", "Main roles|@Member", ">Next step"])]),
        ("Switch on the protection you want", "stk", &[P("Automod status", &["?Do you want Brim to monitor messages?", "Enable Automod", "Moderate invite links posted", "Filter out NSFW links", ">Next step"]), P("Antinuke status", &["?Do you want Brim to monitor moderation actions made by your staff members?", "Enable Antinuke"]), P("Quarantine", &["?Do you want the quarantine system that keeps your server safe from being destroyed?", "Use the auto quarantine module", "Guard the server's vanity url", "Quarantine role|@Quarantine"])]),
        ("Set up verification in one step", "basec", &[P("Verification status", &["?Do you want to filter in only genuine members? Use the verification and fend off bots!", "Enable Verification", "Verification mode|Captcha ▾", ">Next step"]), E("verify_panel"), P("Join Gate status", &["Enable Join Gate", "Accounts that have no profile picture", "Accounts younger than the minimum age"])])]),
    ("Verification.", &[
        ("Members solve a captcha before they talk", "base", &[P("Captcha", &["Captcha answer length|6", "Captcha characters|azAZ09 ▾", "Show a colored trace that follows the captcha characters", "Show small spread-around characters to trick scripts"]), E("verify_captcha"), P("Verification", &["Verification duration|10 min", "Fail action|Kick ▾"])]),
        ("A panel they press to start", "baseb", &[P("Verification", &["Verification mode|Button ▾", "Verification channel|#verify ▾", "Verification roles|@Member", "Display helpful notes alongside the captcha"]), E("verify_panel"), P("Captcha", &["Captcha answer length|6", "Captcha characters|azAZ09 ▾"])]),
        ("Set up in the dashboard", "basec", &[P("Verification", &["Verification duration|10 min", "Fail action|Kick ▾", "Block VPNs and proxies"]), E("verify_help"), P("Verification", &["Verification channel|#verify ▾", "Verification roles|@Member"])])]),
    ("Anti-raid.", &[
        ("Joins pause when a wave of accounts arrives", "base", &[P("Join raid", &["Minimum trigger (accounts)|8", "Join history|10 s", "Trigger duration|5 min", "Suspicious accounts only"]), E("raid"), P("Raider List", &["Use the Raider List", "When a listed account joins|Quarantine ▾"])]),
        ("Every raid gets a report", "baseb", &[P("Join raid", &["Flag accounts without an avatar", "Flag accounts Discord marks as likely spammers", "Flag accounts younger than|7 days"]), E("raid_report"), P("Join raid", &["Minimum trigger (accounts)|8", "Join history|10 s"])]),
        ("Known raiders are flagged on arrival", "basec", &[P("Raider List", &["Use the Raider List", "When a listed account joins|Quarantine ▾"]), E("raider_alert"), P("Raider List", &["Check members already in the server"])])]),
    ("Anti-nuke.", &[
        ("Mass bans and deletes are stopped", "base", &[P("Kicks & Bans", &["Monitor this action", "Minute limit|3", "Hour limit|6", "Heat per action|10"]), E("nuke_quarantined"), P("Status", &["Use the antinuke module", "Detect and react to member prunes"])]),
        ("Rogue bots are kicked", "baseb", &[P("Bot additions", &["Target bots added by unauthorized members", "Trigger action|Kick ▾", "Exclude bots added by the owner or an Extra Owner"]), E("nuke_bot_kicked"), P("Quarantine", &["Use the auto quarantine module", "Guard the server's vanity url", "Quarantine role|@Quarantine"])]),
        ("Staff powers are watched", "basec", &[P("Status", &["Use the antinuke module", "Punish anyone who touches a quarantined member", "Detect and react to member prunes", "Have Brim guard your server's vanity url"]), E("panic_report_loaded"), P("Kicks & Bans", &["Monitor this action", "Minute limit|3"])])]),
    ("Auto Mod.", &[
        ("Warn for blocked words", "base", &[P("Word blacklist", &["Use this filter", "Trigger action|Warn ▾", "Action duration|10 min"]), E("automod_warn"), P("Regular message", &["Use this filter", "Trigger action|Timeout ▾"])]),
        ("Kick for invite links", "baseb", &[P("Invite links", &["Use this filter", "Trigger action|Kick ▾", "Action duration|10 min"]), E("automod_kick"), P("Word blacklist", &["Use this filter", "Trigger action|Warn ▾"])]),
        ("Time out spammers", "basec", &[P("Regular message", &["Use this filter", "Trigger action|Timeout ▾", "Action duration|10 min"]), E("automod_timeout"), P("Invite links", &["Use this filter", "Trigger action|Kick ▾"])])]),
    ("Moderation.", &[
        ("Ban with a reason", "base", &[P("Moderation commands", &["Default timeout duration|10 min", "Days purged on bans|1", "Ask for a Yes/No confirmation when the target is given as an ID or a name"]), E("ban_result"), P("Punishment DMs", &["?Should Brim DM members when they are punished or when a punishment is removed?", "DM punished members"])]),
        ("Short or full replies, your choice", "baseb", &[P("Replies", &["Compact replies", "Delete auto messages after|30 s"]), E("ban_compact"), P("Moderation commands", &["Default timeout duration|10 min", "Days purged on bans|1"])]),
        ("Cases your whole team can see", "basec", &[T("Cases", &[("#4151 ban", "DM ads"), ("#4150 timeout", "Spam"), ("#4149 warn", "Bad words"), ("#4148 ban", "Raid")]), E("note_modified"), P("Confirmation", &["?Should Brim ask Yes or No before punishing a member typed as an ID or a name?", "Ask for a Yes/No confirmation"])])]),
    ("Logging.", &[
        ("Monitor every action on your server", "base", &[P("Channels", &["Brim logs|#brim-logs ▾", "Mod logs|#mod-logs ▾", "Ignore messages with embeds"]), E("mod_log"), P("Log types", &["Message|#message-logs ▾", "Member|#member-logs ▾"])]),
        ("A log for every kind of event", "baseb", &[P("Log types", &["Message|#message-logs ▾", "Member|#member-logs ▾", "Role|#role-logs ▾", "Join Raid|#raid-log ▾"]), E("note_modified"), P("Channels", &["Brim logs|#brim-logs ▾", "Mod logs|#mod-logs ▾"])]),
        ("Ignore users, roles or channels", "basec", &[P("Ignored", &["Ignored channels|Add", "Ignored roles|Add", "Ignored users|Add"]), E("mod_log"), P("Channels", &["Ignore messages with embeds"])])]),
    ("Tickets.", &[
        ("Members open a private ticket", "base", &[P("General", &["Ticket category|Tickets ▾", "Ticket log channel|#ticket-log ▾", "Roles pinged on a new ticket|@Support", "Full conversation in the ticket log"]), E("ticket_intro"), P("Auto-close", &["Close a ticket after this many hours without a message|72"])]),
        ("Staff see the new ticket", "baseb", &[P("Auto-close", &["Close a ticket after this many hours without a message|72"]), E("ticket_received"), P("Replies", &["Anonymous replies by default", "Only the reply command reaches the member"])]),
        ("Replies reach the member", "basec", &[P("Replies", &["Anonymous replies by default", "Only the reply command reaches the member"]), E("ticket_reply"), P("Tickets status", &["?Do you want members to reach your staff by sending Brim a DM?", "Enable Tickets"])])]),
    ("Tools.", &[
        ("Build any message in the editor", "base", &[P("Messages", &["Title|Verification Required!", "Description|Welcome! One step left."]), X("verify_panel"), P("", &[">Add embed", ">Add field", ">Add button row"])]),
        ("See every change in History", "stk", &[L("History", "", &[("v12", "by Mika · Nov 22", "Diff vs current|Roll back"), ("v11", "by Sam · Nov 20", "Diff vs current|Roll back"), ("v10", "by Brim · Nov 18", "Diff vs current|Roll back")]), L("Server layout (Layout Guard)", "", &[("Layout v3", "saved from the server, by Mika", "Current"), ("Layout v2", "Channel Setup push, by Sam", "Roll back")]), P("Import settings", &["?This file changes nothing on this server.", ">Import"])]),
        ("Set up channels and roles", "stk", &[G(&["Everyone", "Member", "Moderator"], &[("\u{25be} Info", &["\u{2713}", "\u{2713}", "\u{2713}"]), ("# rules", &["\u{2713}", "\u{2713}", "\u{2713}"]), ("\u{25be} Chat", &["\u{2717}", "\u{2713}", "\u{2713}"]), ("# general", &["\u{2717}", "\u{2713}", "\u{2713}"])]), P("", &["?A backup is taken first, so Backups can undo it.", "Add a role|@Member ▾", ">Add"])]),
        ("Follow feeds into a channel", "stk", &[L("Feeds", "Add new feed", &[("Channel uploads", "YouTube", "Edit"), ("Blog posts", "RSS", "Edit"), ("Releases", "GitHub", "Edit")]), P("Add new feed", &["Link|youtube.com/@brim", "Channel|#uploads ▾", "Ping role|@Feeds ▾"]), P("More options", &["Source|Detect from the link ▾", "Send as Brim"])]),
        ("Back up and load a layout", "stk", &[L("Backups", "Create backup", &[("#a1f3", "Brim · today", "View|Load"), ("#9c20", "Brim · yesterday", "View|Load"), ("#77de", "Brim · Nov 18", "View|Load")]), P("Load backup #a1f3", &["Show permissions", ">Apply"])])]),
];

/// A long sample message ends at a whole block: fields (name and value together, an inline row as one) go from the end,
/// the first always stays. `limit` is about 1.3 times the scene's first panel.
fn cap_message(scene: &web_sys::HtmlElement) {
    use wasm_bindgen::JsCast;
    let html = |n: Option<web_sys::Element>| n.and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok());
    let Some(first) = html(scene.query_selector(":scope > .pane, :scope > .lcard, :scope > .tbl").ok().flatten()) else { return };
    let Some(win) = html(scene.query_selector(":scope > .win:not(.pane):not(.lcard):not(.tbl):not(.xs)").ok().flatten()) else { return };
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
        let bh = mgh + 96.0;
        let k = (w / bw).min(0.85);
        let (l, t) = ((bw - mgw) / 2.0, (bh - mgh) / 2.0);
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
            [(l - 41.0, t + 64.0), (l - 64.0, t + 38.0), (l + mw1 + 14.0, t + 10.0), (l + mw1 + 40.0, t + 30.0), (bx, t + mh1 + 22.0), (bx - 6.0, t + mh1 + 56.0)]
        };
        let Ok(stars) = b.query_selector_all(".fx > i") else { continue };
        for n in 0..stars.length().min(6) {
            let Some(st) = html(stars.item(n)) else { continue };
            let (x, y) = slots[n as usize];
            let half = f64::from(st.offset_width()) / 2.0;
            let _ = st.style().set_property("left", &format!("{}px", (x * k - half).max(0.0).min(if phone { w - half * 2.0 } else { 1e9 })));
            let _ = st.style().set_property("top", &format!("{}px", y * k - half));
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

/// The stars of a block, fixed in its box (`fit` sets their places): three pairs of a big and a small star, filled and outline.
/// The seed (the block's number) makes the same few stars vary on every load: 0 to 2 stars are left out, each turns by its own
/// angle and shows its own slice of the gradient.
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
            view! { <i class=class style=format!("{hide}rotate:{:.0}deg;--gp:{:.0}%;--d:-{:.1}s", -40.0 + r() * 80.0, r() * 100.0, r() * 7.0) aria-hidden="true"></i> }
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
    use super::{appeal_login, Item, SECTIONS};

    #[test]
    fn appeal_id_is_17_to_20_digits() {
        assert_eq!(appeal_login("12345678901234567").as_deref(), Some("/auth/login?next=/%23/appeal/12345678901234567"));
        assert!(appeal_login("123456789012345678901").is_none());
        assert!(appeal_login("1234567890123456").is_none());
        assert!(appeal_login("1234567890123456a7").is_none());
        assert!(appeal_login("").is_none());
    }

    /// Every setting, card title and button a scene names exists in the dashboard's own source (schema, wizard cards, pages).
    #[test]
    fn scenes_name_real_settings() {
        let src: String = [include_str!("../../src/schema.rs"), include_str!("wizard.rs"), include_str!("main.rs"), include_str!("layout.rs"), include_str!("recovery.rs"), include_str!("feeds.rs"), include_str!("channels.rs"), include_str!("embeds.rs"), include_str!("raiders.rs"), include_str!("tickets.rs"), include_str!("pages.rs")].concat();
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
                        _ => {}
                    }
                }
            }
        }
        assert!(bad.is_empty(), "not found in the dashboard source: {bad:?}");
    }
}

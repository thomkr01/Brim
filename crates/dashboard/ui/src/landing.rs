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
    /// A line a member types, as it appears in Discord.
    C(&'static str),
    /// A dashboard table: title and (name, state) rows.
    T(&'static str, &'static [(&'static str, &'static str)]),
}
use Item::{C, E, P, T, X};

/// Line text, layout class (see .l-* in style.css) and the pieces of its scene.
type Scene = (&'static str, &'static str, &'static [Item]);
/// Feature sections: title and clickable lines; each line shows its own scene of overlapping windows.
const SECTIONS: [(&str, &[Scene]); 9] = [
    ("Setup Wizard.", &[
        ("Answer a few questions", "col", &[P("Logging channel", &["-", "?Where do you want Brim to log its actions?", "Logs channel|brim-logs", "Moderation logs channel|mod-logs", ">Next"]), P("Main roles", &["?The role your members need to access the server.", "Main role|Member", ">Next"])]),
        ("Switch on the protection you want", "col", &[P("Automod status", &["?Moderate what members post.", "Moderate invite links", "Filter out malicious links", "Monitor repetitive message spam", "Slow down channels that flood"]), P("Antinuke status", &["?Watch the moderation actions of your staff.", "Monitor staff actions", "Quarantine at the limit"]), P("Anti raid status", &["?Keep bad raids out.", "Join gate", "Raider list"])]),
        ("Set up verification in one step", "break", &[P("Verification status", &["-", "?Members solve a captcha before they can talk.", "Verification", "Web verify", ">Set up channel"]), E("verify_panel")])]),
    ("Verification.", &[
        ("Members solve a captcha before they talk", "diag", &[E("verify_captcha"), E("verify_panel")]),
        ("A panel they press to start", "side", &[E("verify_panel"), E("verify_help"), E("verify_captcha")]),
        ("Set up in the dashboard", "break", &[P("Verification", &["Captcha", "Web verify", "Give role on pass"]), E("verify_panel")])]),
    ("Anti-raid.", &[
        ("Joins pause when a wave of accounts arrives", "tall", &[E("raid")]),
        ("Every raid gets a report", "diag", &[E("raid_report"), E("raider_alert"), E("raid")]),
        ("Known raiders are flagged on arrival", "side", &[P("Anti-raid", &["Join gate", "Raider list", "Quarantine newcomers"]), E("raider_alert")])]),
    ("Anti-nuke.", &[
        ("Mass bans and deletes are stopped", "diag", &[E("nuke_quarantined"), E("panic_report_loaded")]),
        ("Rogue bots are kicked", "tall", &[E("nuke_bot_kicked")]),
        ("Staff powers are watched", "diag", &[P("Anti-nuke", &["Watch staff actions", "Quarantine at the limit", "Restore from backup"]), E("nuke_quarantined"), E("panic_report_loaded")])]),
    ("Auto Mod.", &[
        ("Warn for blocked words", "diag", &[P("Blocked words", &["?Remove messages that contain words you list.", "Filter", "Action|Warn", "Strikes", "Delete message", "Log to automod-logs", ">Save"]), E("automod_warn")]),
        ("Kick for invite links", "break", &[P("Invite links", &["?Remove invite links to other servers.", "Filter", "Action|Kick", "Strikes", "Delete message", "Log to automod-logs", ">Save"]), E("automod_kick")]),
        ("Time out spammers", "tall", &[P("Spam", &["?Stop floods of repeated messages.", "Filter", "Action|Timeout", "Strikes", "Delete message", "Log to automod-logs", ">Save"]), E("automod_timeout"), E("automod_warn")])]),
    ("Moderation.", &[
        ("Ban with a reason", "col", &[C("/ban @Mika DM ads"), E("ban_result"), E("mod_log")]),
        ("Short or full replies, your choice", "break", &[E("ban_result"), E("ban_compact")]),
        ("Cases your whole team can see", "diag", &[T("Cases", &[("#4151 ban", "DM ads"), ("#4150 timeout", "Spam"), ("#4149 warn", "Bad words")]), E("note_modified")])]),
    ("Logging.", &[
        ("Monitor every action on your server", "tall", &[P("Log channels", &["?Each kind of log goes to its own channel.", "Moderation|mod-logs", "Auto Mod|automod-logs", "Anti-nuke|antinuke-logs", "Verification|verification-logs", "Everything else|brim-logs", ">Save"]), E("mod_log")]),
        ("A log for every kind of event", "side", &[P("Log types", &["?Pick what Brim writes to your logs.", "Message deleted", "Member banned", "Role updated", "Channel created", "Invite created"]), E("note_modified"), E("verify_deleted"), E("mod_log")]),
        ("Ignore users, roles or channels", "break", &[P("Ignored", &["?Brim skips these in your logs.", "Users|Add", "Roles|Add", "Channels|Add", ">Save"]), E("mod_log")])]),
    ("Tickets.", &[
        ("Members open a private ticket", "diag", &[E("ticket_intro"), E("ticket_received")]),
        ("Staff see the new ticket", "side", &[T("Tickets", &[("general-help", "Open"), ("ban-appeal", "Open"), ("billing", "Closed")]), E("ticket_received")]),
        ("Replies reach the member", "tall", &[E("ticket_reply")])]),
    ("Tools.", &[
        ("Build any message in the editor", "side", &[P("Message Builder", &["?Edit the message with the visual fields.", "Title|Verification Required", "Colour|Green", "Buttons", "Schedule", ">Preview", ">Post"]), X("verify_panel")]),
        ("See every change in History", "diag", &[T("History", &[("Welcome message", "Diff|Revert"), ("Log channels", "Diff|Revert"), ("Anti-raid", "Diff|Revert"), ("Auto Mod", "Diff|Revert")])]),
        ("Back up and load a layout", "tall", &[T("Backups", &[("Before the raid", "View|Preview & load|Delete"), ("Daily", "View|Preview & load|Delete"), ("Fresh server", "View|Preview & load|Delete")]), P("Backups", &["?Save your layout and load it later.", ">Create backup"])])]),
];

/// The accent colour of the first bot message in a scene, as a CSS variable for its glow.
fn glow(items: &[Item]) -> String {
    let c = items.iter().find_map(|i| match i { E(k) => sample(k)["embeds"][0]["color"].as_u64(), _ => None });
    format!("--glow:#{:06x}", c.unwrap_or(0xf07f22))
}

/// Each scene is drawn 616 wide and scaled to fit its box (at most 0.85), so it keeps every window.
/// shortcut: polled twice a second, because the samples and tab changes alter a scene's height.
fn fit() {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(boxes) = doc.query_selector_all(".feat-shot") else { return };
    for i in 0..boxes.length() {
        use wasm_bindgen::JsCast;
        let Some(b) = boxes.item(i).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) else { continue };
        let Some(s) = b.query_selector(".scene").ok().flatten().and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) else { continue };
        let w = f64::from(b.client_width());
        let k = (w / 616.0).min(0.85).min((520.0 / f64::from(s.offset_height())).max(0.55));
        let st = b.style();
        let _ = st.set_property("--k", &k.to_string());
        let _ = st.set_property("--dx", &format!("{}px", (w - 616.0 * k) / 2.0));
        let _ = st.set_property("--sh", &format!("{}px", s.offset_height()));
    }
}

/// The three edits of a default message: accent colour and a new first line of text.
const EDITS: [(u64, &str); 3] = [(0x5865f2, "Welcome! One step left."), (0xe91e63, "Hey there, press the button."), (0x1be725, "Verify to join the fun.")];

/// A sample as the bot sends it, with the template's `{prefix}` filled from the sample prefix (the one in the bot's sample variables).
fn sample(key: &str) -> serde_json::Value {
    let m = crate::discord::sample(key);
    if m.is_null() { return m }
    serde_json::from_str(&m.to_string().replace("{prefix}", "t!")).unwrap_or(m)
}

const REPO: &str = "https://github.com/thomkr01/Brim/blob/main";

/// The appeal form needs a login; this sends the visitor there and back to the form.
fn appeal_login(id: &str) -> Option<String> {
    let ok = (17..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| format!("/auth/login?next=/%23/appeal/{id}"))
}

/// Animations off: the user's choice (`data-motion`), or the system setting, under which
/// bundle.css stops every animation, so no animationend would ever end a phase.
fn calm() -> bool {
    let w = web_sys::window().unwrap();
    w.match_media("(prefers-reduced-motion: reduce)").ok().flatten().is_some_and(|m| m.matches())
        || w.document()
        .and_then(|d| d.document_element())
        .and_then(|r| r.get_attribute("data-motion"))
        .as_deref()
        == Some("reduced")
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

    fit();
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
                        <section class="feat" class:flip=i % 2 == 1>
                            <div class="feat-copy">
                                <h3>{*title}</h3>
                                <div class="feat-tabs" role="group" aria-label=format!("{title} examples")>
                                    {tabs.iter().enumerate().map(|(t, (label, _, _))| view! {
                                        <button class="feat-tab" type="button" aria-pressed=move || (on.get() == t).to_string() on:click=move |_| on.set(t)>
                                            <span class="feat-dot" aria-hidden="true"></span>{*label}<span class="feat-arrow" aria-hidden="true">"\u{203a}"</span>
                                        </button>
                                    }).collect_view()}
                                </div>
                            </div>
                            <div class="feat-shot" aria-live="polite" style=move || glow(tabs[on.get()].2)>
                                <i class="spark" aria-hidden="true"></i><i class="hex" aria-hidden="true"></i><i class="mote m1" aria-hidden="true"></i><i class="mote m2" aria-hidden="true"></i><i class="mote m3" aria-hidden="true"></i><i class="mote m4" aria-hidden="true"></i><i class="spark" aria-hidden="true"></i><i class="spark" aria-hidden="true"></i><i class="spark" aria-hidden="true"></i><i class="hex" aria-hidden="true"></i><i class="hex" aria-hidden="true"></i><i class="mote m1" aria-hidden="true"></i>
                                {move || view! { <div class=format!("scene l-{}", tabs[on.get()].1)><i class="burst" aria-hidden="true"></i>{tabs[on.get()].2.iter().map(|it| match *it {
                                    E(key) => view! { <div class="win">{crate::discord::show_msg(sample(key))}</div> }.into_any(),
                                    P(name, rows) => view! { <div class="win pane card"><b class="ph">{name}</b>{rows.iter().map(|row| match (row.strip_prefix('?'), row.strip_prefix('>'), row.split_once('|')) {
                                        (Some(t), _, _) => view! { <p class="muted">{t}</p> }.into_any(),
                                        (_, Some(t), _) => view! { <button class="primary" type="button" disabled=true>{t}</button> }.into_any(),
                                        (_, _, Some((a, b))) => view! { <div class="tog"><label>{a}</label><span class="chip">{b}</span></div> }.into_any(),
                                        _ if *row == "-" => view! { <div class="pbar"><i></i></div> }.into_any(),
                                        _ => view! { <div class="tog"><label>{*row}</label><input type="checkbox" class="sw" prop:checked=true disabled=true aria-label=*row /></div> }.into_any(),
                                    }).collect_view()}</div> }.into_any(),
                                    X(key) => {
                                        let m = sample(key);
                                        EDITS.iter().map(|(color, text)| {
                                            let mut m = m.clone();
                                            if let Some(e) = m["embeds"].get_mut(0) {
                                                e["color"] = (*color).into();
                                                if e["description"].is_string() { e["description"] = (*text).into(); }
                                            }
                                            view! { <div class="win">{crate::discord::show_msg(m)}</div> }
                                        }).collect_view().into_any()
                                    }
                                    C(text) => view! { <div class="win cmd">{crate::discord::chat("Mika", view! { <span class="md">{text}</span> }.into_any())}</div> }.into_any(),
                                    T(name, rows) => view! { <div class="win tbl"><b>{name}</b><table class="data"><tbody>{rows.iter().map(|(a, b)| view! { <tr><td>{*a}</td><td>{b.split('|').map(|c| view! { <span class="chip">{c}</span>" " }).collect_view()}</td></tr> }).collect_view()}</tbody></table></div> }.into_any(),
                                }).collect_view()}</div> }}
                            </div>
                        </section>
                    }
                }).collect_view()}
                </div>
                <p class="land-edit">"Every embed Brim sends can be "<span class="u">"edited"</span>" in the message editor."</p>
                <section class="land-close">
                    <i class="spark" aria-hidden="true"></i><i class="hex" aria-hidden="true"></i>
                    <p class="feat-sub">"Free, with every feature."</p>
                    <h2>"Give your server a "<span class="u">"guard"</span>"."</h2>
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

//! The front page for visitors who are not logged in (design system: FrontPage, FeaturePicker,
//! SiteFooter, AppealSheet). Classes come from `design/bundle.css`; this file adds only behaviour.

use leptos::{html, prelude::*};

/// One piece of a scene: a sample message from the bot (E), a small mock panel (P: title and rows) or a number badge (N).
#[derive(Clone, Copy)]
enum Item {
    E(&'static str),
    P(&'static str, &'static [&'static str]),
    N(&'static str),
    /// A dashboard table: title and (name, state) rows.
    T(&'static str, &'static [(&'static str, &'static str)]),
}
use Item::{E, N, P, T};

type Scene = (&'static str, &'static [Item]);
/// Feature sections: title and clickable lines; each line shows its own scene of overlapping windows.
const SECTIONS: [(&str, &[Scene]); 7] = [
    ("Verification.", &[("Members solve a captcha before they talk", &[E("verify_captcha"), E("verify_panel")]), ("A panel they press to start", &[E("verify_panel"), E("verify_help"), E("verify_captcha")]), ("Help for members who get stuck", &[E("verify_help")])]),
    ("Anti-raid.", &[("Joins pause when a wave of accounts arrives", &[E("raid"), E("raid_report")]), ("Newcomers wait in quarantine", &[E("raid_report")]), ("Known raiders are flagged on arrival", &[E("raider_alert"), E("raid")])]),
    ("Anti-nuke.", &[("Mass bans and deletes are stopped", &[E("nuke_quarantined"), E("panic_report_loaded")]), ("Rogue bots are kicked", &[E("nuke_bot_kicked")]), ("A panic report shows what was restored", &[E("panic_report_loaded"), E("nuke_quarantined"), E("nuke_bot_kicked")])]),
    ("Auto Mod.", &[
        ("Warn for blocked words", &[P("Blocked words", &["Filter on", "Action: Warn", "Strikes on"]), E("automod_warn")]),
        ("Kick for invite links", &[P("Invite links", &["Filter on", "Action: Kick", "Allow list"]), E("automod_kick")]),
        ("Time out spammers", &[P("Spam", &["Filter on", "Action: Timeout", "Strikes on"]), E("automod_timeout")])]),
    ("Moderation.", &[("Ban with a reason", &[P("Command", &["/ban @member DM ads"]), E("ban_result")]), ("Short or full replies, your choice", &[E("ban_result"), E("ban_compact")]), ("Cases your whole team can see", &[T("Cases", &[("#4151 ban", "DM ads"), ("#4150 timeout", "Spam"), ("#4149 warn", "Bad words")]), E("note_modified")])]),
    ("Logging.", &[
        ("Monitor every action on your server", &[P("Log channels", &["# mod-log", "# message-log", "# member-log", "# server-log"]), E("mod_log")]),
        ("Make use of 62 log types", &[N("62"), P("Log types", &["Message deleted", "Member banned", "Role updated", "Channel created", "Invite created"]), E("automod_warn")]),
        ("Ignore users, roles or channels", &[P("Ignored", &["Users", "Roles", "Channels"]), E("mod_log")])]),
    ("Tickets.", &[("Members open a private ticket", &[E("ticket_intro"), E("ticket_received")]), ("Staff see the new ticket", &[T("Tickets", &[("general-help", "Open"), ("ban-appeal", "Open"), ("billing", "Closed")]), E("ticket_received")]), ("Replies reach the member", &[E("ticket_reply"), E("ticket_intro")])]),
];

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
                            <a class="tb-btn tb-btn-quiet" href=login>"Go to dashboard"</a>
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
                                    {tabs.iter().enumerate().map(|(t, (label, _))| view! {
                                        <button class="feat-tab" type="button" aria-pressed=move || (on.get() == t).to_string() on:click=move |_| on.set(t)>
                                            <span class="feat-dot" aria-hidden="true"></span>{*label}<span class="feat-arrow" aria-hidden="true">"\u{203a}"</span>
                                        </button>
                                    }).collect_view()}
                                </div>
                            </div>
                            <div class="feat-shot" aria-live="polite">
                                <i class="spark" aria-hidden="true"></i><i class="hex" aria-hidden="true"></i><i class="mote m1" aria-hidden="true"></i><i class="mote m2" aria-hidden="true"></i><i class="mote m3" aria-hidden="true"></i><i class="mote m4" aria-hidden="true"></i>
                                {move || view! { <div class="scene"><i class="burst" aria-hidden="true"></i>{tabs[on.get()].1.iter().map(|it| match *it {
                                    E(key) => view! { <div class="win">{crate::discord::show(key)}</div> }.into_any(),
                                    P(name, rows) => view! { <div class="win pane"><b>{name}</b>{rows.iter().enumerate().map(|(r, row)| view! { <span class:on=(r == 0)>{*row}</span> }).collect_view()}</div> }.into_any(),
                                    T(name, rows) => view! { <div class="win tbl"><b>{name}</b><table class="data"><tbody>{rows.iter().map(|(a, b)| view! { <tr><td>{*a}</td><td><span class="chip">{*b}</span></td></tr> }).collect_view()}</tbody></table></div> }.into_any(),
                                    N(n) => view! { <div class="badge" aria-hidden="true">{n}</div> }.into_any(),
                                }).collect_view()}</div> }}
                            </div>
                        </section>
                    }
                }).collect_view()}
                </div>
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

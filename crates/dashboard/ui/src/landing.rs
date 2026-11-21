//! The front page for visitors who are not logged in (design system: FrontPage, FeaturePicker,
//! SiteFooter, AppealSheet). Classes come from `design/bundle.css`; this file adds only behaviour.

use leptos::{html, prelude::*};
use std::time::Duration;

const FEATURES: [(&str, &str, &str); 10] = [
    ("🧩", "Verification", "New members solve a short captcha before they can talk, so bots stay out."),
    ("🌊", "Anti-raid", "When a wave of new accounts arrives, joins pause and the newcomers wait in quarantine."),
    ("🧹", "Auto Mod", "Spam, scam links and slurs are removed, and repeat offenders are timed out."),
    ("🛡️", "Anti-nuke", "If a staff account starts mass banning or deleting, Brim takes its powers away."),
    ("📐", "Layout Guard", "Channels, categories and roles that are changed without you are put back."),
    ("💾", "Backups", "Save your whole server layout and restore it in one go."),
    ("🔨", "Moderation", "Warn, mute, kick and ban, with cases and notes your whole team can see."),
    ("🎫", "Tickets", "Members reach your staff in a private ticket with one button."),
    ("✉️", "Message Builder", "Make embeds with buttons, then post them now or on a schedule."),
    ("📜", "Logs", "Every action and change is written to your log channels, so you always know who did what."),
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

    let picked = RwSignal::new(None::<usize>);
    // Pebbles pop in (0), one wave runs across them (1), then one nudges every 3 s until the first tap (2).
    let phase = RwSignal::new(if calm() { 2 } else { 0 });
    let nudge = RwSignal::new(None::<usize>);
    let last = StoredValue::new(usize::MAX);
    if !calm() {
        if let Ok(h) = set_interval_with_handle(
            move || {
                if phase.get_untracked() == 2 && picked.get_untracked().is_none() {
                    // Never the same pebble twice in a row.
                    let mut n = (js_sys::Math::random() * (FEATURES.len() - 1) as f64) as usize;
                    if n >= last.get_value() {
                        n += 1;
                    }
                    last.set_value(n);
                    nudge.set(Some(n));
                }
            },
            Duration::from_secs(3),
        ) {
            on_cleanup(move || h.clear());
        }
    }
    // The last pebble's animation ends each phase (its delay is the longest).
    let ended = move |i: usize| {
        if i == FEATURES.len() - 1 && phase.get_untracked() < 2 {
            phase.update(|p| *p += 1);
        }
        if nudge.get_untracked() == Some(i) {
            nudge.set(None);
        }
    };

    let sheet = NodeRef::<html::Dialog>::new();
    let id = RwSignal::new(String::new());
    let err = RwSignal::new("");

    view! {
        <div class="tb-root tb-page land">
            // "line" = version A, a copper line closes the section. Remove it for version B, the long fade (both in style.css).
            <section class="hero line">
                <div class="hero-split" aria-hidden="true"></div>
                <svg class="hero-lines" viewBox="0 0 1000 600" preserveAspectRatio="none" aria-hidden="true">
                    <path d="M500 0 L315 600" /><path d="M0 130 H210 L240 100 H340" /><path class="hot" d="M0 70 H70" />
                </svg>
                <div class="hero-stripes" aria-hidden="true"></div>
                <header class="tb-header">
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
                <div class="land-rule" aria-hidden="true"></div>
            <div class="tb-picker" class:is-entering=move || phase.get() == 0 class:is-waving=move || phase.get() == 1>
                <div class="tb-pebbles">
                    {FEATURES.iter().enumerate().map(|(i, (emoji, name, _))| view! {
                        <button class="tb-pebble" type="button" style=format!("--tb-i:{i}")
                            aria-pressed=move || (picked.get() == Some(i)).to_string()
                            class:is-nudging=move || nudge.get() == Some(i)
                            on:animationend=move |_| ended(i)
                            on:click=move |_| { picked.set(Some(i)); nudge.set(None); }>
                            <span aria-hidden="true">{*emoji}</span>{*name}
                        </button>
                    }).collect_view()}
                </div>
                <p class="tb-feature-line" aria-live="polite">{move || match picked.get() {
                    // Keyed by the pick, so the line's fade-in runs again on each tap.
                    Some(i) => view! { <span class="tb-feature-text"><span class="tb-feature-name">{format!("{}:", FEATURES[i].1)}</span>" "{FEATURES[i].2}</span> }.into_any(),
                    None => view! { <span class="land-hint">"Tap a feature to see what it does."</span> }.into_any(),
                }}</p>
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

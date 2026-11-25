//! The server's start page (`#/<server>`): Setup Wizard and Overview as the two main cards, then the task
//! cards with one button each (review), all in the shared cards grid (universal layout step 6).
use crate::{common::*, modules::blurb, pages::Ring};
use leptos::{prelude::*, task::spawn_local};
use serde_json::Value;

/// (page, card title, button) for the task cards; a card is hidden when the viewer has no access to its page.
const CARDS: &[(&str, &str, &str)] = &[
    ("cases", "Cases", "View cases"),
    ("report_list", "Reports", "Open reports"),
    ("embeds", "Message Builder", "Build a message"),
    ("permits", "Permits", "Open permits"),
    ("backups", "Backups", "Open backups"),
    ("logging", "Logging", "Set up logging"),
];

#[component]
pub fn Home(guild: String) -> impl IntoView {
    let access = expect_context::<Access>();
    let score = RwSignal::new(None::<Value>);
    // A failed request shows its error: without it the card kept its loading line forever.
    let failed = RwSignal::new(None::<String>);
    let g = guild.clone();
    spawn_local(async move {
        let r = api("GET", &format!("/api/guilds/{g}/score"), None).await;
        // Leaving the page while the request runs disposes the signal.
        if !score.is_disposed() {
            match r {
                Ok(v) => score.set(Some(v)),
                Err(e) => failed.set(Some(e)),
            }
        }
    });
    // The server's score ignores notes (ids "note_..."), as the Overview does.
    let rows = move || -> Vec<Value> {
        score.get().and_then(|v| v["rows"].as_array().cloned()).unwrap_or_default().into_iter().filter(|x| !x["id"].as_str().unwrap_or("").starts_with("note_")).collect()
    };
    let pct = Signal::derive(move || {
        let r = rows();
        if r.is_empty() { 0 } else { r.iter().filter(|x| x["severity"] == "ok").count() * 100 / r.len() }
    });
    let to_fix = move || rows().iter().filter(|x| x["severity"] != "ok").count();
    let (g1, g2) = (guild.clone(), guild.clone());
    let wizard = view! {
        <div class="card">
            <h2>"Setup Wizard"</h2>
            <p class="muted">"Roles, log channels and the main protections, set up step by step."</p>
            {move || (score.get().is_some_and(|v| v["setup_done"] == false)).then(|| view! { <p class="chip orange">"Not finished yet"</p> })}
            <a class="btn" href=format!("#/{g1}/wizard")>"Open the wizard"</a>
        </div>
    }.into_any();
    let overview = view! {
        <div class="card home-score">
            <div class="home-score-text">
                <h2>"Overview"</h2>
                {move || match score.get() {
                    None => match failed.get() {
                        Some(e) => view! { <p class="err">{format!("The overview could not be loaded: {e}")}</p> }.into_any(),
                        None => view! { <p><Working /></p> }.into_any(),
                    },
                    Some(_) => match to_fix() {
                        0 => view! { <p class="muted">"Nothing to fix."</p> }.into_any(),
                        1 => view! { <p class="muted">"1 thing to fix."</p> }.into_any(),
                        n => view! { <p class="muted">{format!("{n} things to fix.")}</p> }.into_any(),
                    },
                }}
                <a class="btn" href=format!("#/{g2}/overview")>"Review and fix"</a>
            </div>
            <Show when=move || score.get().is_some()><Ring pct /></Show>
        </div>
    }.into_any();
    // The task cards sit on a real grid (every row aligned); a task card the viewer cannot open is left out.
    let tasks = move || {
        let cards = CARDS.iter().filter(|(p, ..)| access.of(p) != "hidden").map(|(p, t, b)| {
            view! { <div class="card"><h3>{*t}</h3><p class="muted">{blurb(p)}</p><a class="ghost" href=format!("#/{guild}/{p}")>{*b}</a></div> }.into_any()
        }).collect::<Vec<_>>();
        view! { <div class="home-grid">{cards}</div> }
    };
    view! {
        <div class="home">
            <div class="home-grid">{wizard}{overview}</div>
            {tasks}
        </div>
    }
}

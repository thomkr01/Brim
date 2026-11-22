//! `#/ui`: every shared building block once, with the size tokens, so the look is judged in one place.
//! Not in the sidebar. Static sample content only: it reads no server data.
use crate::common::{AddButton, Brim, Head, PageTabs, Pose, Tab, Working};
use leptos::prelude::*;

/// (token, role) for the size table, smallest last.
const SIZES: &[(&str, &str)] = &[
    ("page-title", "Page title (32 px on a phone)"),
    ("figure", "Big figure (score, stats)"),
    ("section-title", "Heading: section and card titles"),
    ("ui", "Body text, buttons, inputs"),
    ("caption", "Small text: help lines, captions, chips, table headers, code"),
];

#[component]
pub fn StylePage() -> impl IntoView {
    let on = RwSignal::new(true);
    view! {
        <Head guild=String::new() page="Page".into() big="Page title".into() module=Some(("Module".to_string(), "#/ui".to_string())) />
        <div class="card">
            <h2>"Text sizes"</h2>
            <p class="muted">"Five sizes. A page uses these tokens, never its own size (CI checks style.css)."</p>
            {SIZES.iter().map(|(t, role)| view! {
                <div class="sg-size"><code>{format!("--fs-{t}")}</code><span class=format!("sg-fs-{t}")>{*role}</span></div>
            }).collect_view()}
        </div>
        <div class="cards"><div class="col">
            <div class="card">
                <h2>"Section title"</h2>
                <p class="muted">"Help text: one grey line that says what the section does."</p>
                <div class="row"><label>"Text field"</label><input aria-label="text field" placeholder="Type here" /></div>
                <div class="row"><label>"Select"</label><select aria-label="select"><option>"First choice"</option><option>"Second choice"</option></select></div>
                <div class="row tog" role="group" aria-label="toggle row sample">
                    <label>"Toggle row"<crate::common::Tip text="The info icon next to the name says what the switch does." /></label>
                    <input type="checkbox" class="sw" aria-label="toggle row" prop:checked=move || on.get() on:change=move |e| on.set(event_target_checked(&e)) />
                </div>
            </div>
            </div><div class="col">
            <div class="card">
                <h2>"Buttons"</h2>
                <p class="muted">"Orange only for the main button of a view."</p>
                <div class="row"><button>"Main action"</button><button class="ghost">"Quiet"</button><button class="go">"Turn on"</button><button class="danger">"Delete"</button></div>
                <div class="row"><button class="link">"Text link"</button><button disabled>"Disabled"</button><button class="ghost" aria-busy="true"><Working /></button></div>
                <h2>"Chips"</h2>
                <div class="row"><span class="chip">"Neutral"</span><span class="chip green">"On"</span><span class="chip blue">"Info"</span><span class="chip orange">"Warning"</span></div>
            </div>
        </div></div>
        <div class="card">
            <h2>"Tabs"</h2>
            <PageTabs tabs=vec![Tab::link("a", "Settings", "#/ui".into()), Tab::link("b", "Log", "#/ui".into()), Tab::link("c", "Blacklist", "#/ui".into())] active="a".to_string() />
            <div class="facts"><h2>"Stats"</h2><span class="stat"><span>"Members"</span><b>"1,204"</b></span><span class="stat"><span>"Cases"</span><b>"38"</b></span></div>
        </div>
        <div class="card empty pointing">
            <Pose pose=Brim::PointingRight />
            <div>
                <h3>"Nothing here yet"</h3>
                <p class="muted">"An empty list says what to do first."</p>
                <AddButton label="Add new item" />
            </div>
        </div>
        <div class="card">
            <h2>"Brim poses"</h2>
            <p class="muted">"Each pose means one thing and is used only there, beside text that says the same."</p>
            <div class="poses">{Brim::ALL.into_iter().map(|p| { let (name, rule) = p.rule(); view! { <figure><Pose pose=p /><figcaption><b>{name}</b><span class="muted">{rule}</span></figcaption></figure> } }).collect_view()}</div>
        </div>
        <div class="card"><h2>"Loading"</h2><p class="muted">"The one loading sign, for pages, panels and buttons."</p><Working /></div>
    }
}

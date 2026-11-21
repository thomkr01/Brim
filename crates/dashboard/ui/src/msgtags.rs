//! Message Builder tags: the tag card in the Automate tab. Staff with the tags permit post the
//! message by its tag with `/tag` in the channel where they type it.
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

pub fn tag_tab(base: String, cur: RwSignal<Value>, dirty: Memo<bool>, rank: Signal<u8>, msg: Msg) -> impl IntoView {
    let embed = Signal::derive(move || cur.with(|c| c["id"].as_i64()));
    let url = StoredValue::new(base);
    let url = move || embed.get_untracked().map(|id| format!("{}/{id}/tag", url.get_value()));
    let saved = RwSignal::new(String::new());
    let text = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    // True when Remove tag was pressed, so the working sign shows on the button that was pressed.
    let removing = RwSignal::new(false);
    let locked = Signal::derive(move || rank.get() < 3);
    Effect::new(move |_| {
        embed.track();
        let Some(u) = url() else { return };
        let asked = embed.get_untracked();
        spawn_local(async move {
            let r = api("GET", &u, None).await;
            if embed.try_get_untracked() != Some(asked) {
                return;
            }
            match r {
                Ok(v) => {
                    let t = v["tag"].as_str().unwrap_or("").to_string();
                    saved.set(t.clone());
                    text.set(t);
                }
                Err(e) => msg.err(e),
            }
        })
    });
    let save = move |tag: String| {
        let Some(u) = url() else { return };
        busy.set(true);
        spawn_local(async move {
            let body = json!({"tag": tag, "seen": cur.with_untracked(|c| c["version"].clone())});
            let res = api("POST", &u, Some(body)).await;
            busy.set(false);
            match res {
                Ok(v) => {
                    // A tag vouches for the message, which moves its version on.
                    cur.update(|c| c["version"] = v["version"].clone());
                    let t = v["tag"].as_str().unwrap_or("").to_string();
                    msg.ok(if t.is_empty() { "Tag removed." } else { "Tag saved. Staff can post it with /tag." });
                    saved.set(t.clone());
                    text.set(t);
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let why = move || dirty.get().then_some("Save or cancel your changes to the message first.");
    view! {
        <div class="card sentlog rules">
            <h2>"Tag"</h2>
            {move || match embed.get() {
                None => view! { <p class="muted">"Save the message first, then give it a tag."</p> }.into_any(),
                Some(_) => view! {
                    <p class="muted small">"Staff with the Post Tags permit post this message with /tag in the channel where they type it, if they can write there. It posts as Brim, never pings, and pauses while panic mode, a lockdown or an anti-nuke case is on."</p>
                    <div class="row"><label for="mbtag">"Tag"</label>
                        <input id="mbtag" maxlength="32" placeholder="e.g. rules" prop:value=move || text.get() disabled=locked
                            on:input=move |e| text.set(event_target_value(&e)) />
                        <Tip text="Letters a to z, digits, - and _. Each tag is used once per server." icon="info" />
                    </div>
                    {move || why().map(|w| view! { <p class="err">{w}</p> })}
                    {move || if locked.get() {
                        view! { <p class="muted small">"Only the Owner, Extra Owners and Trusted Admins set tags."</p> }.into_any()
                    } else {
                        view! {
                            <div class="row">
                                <button disabled=move || busy.get() || dirty.get() || text.get().trim().to_lowercase() == saved.get()
                                    on:click=move |_| { removing.set(false); save(text.get_untracked()) }>{busy_label(move || busy.get() && !removing.get(), "Save tag")}</button>
                                {move || (!saved.get().is_empty()).then(|| view! {
                                    <button class="ghost" disabled=move || busy.get() || dirty.get() on:click=move |_| { removing.set(true); save(String::new()) }>{busy_label(move || busy.get() && removing.get(), "Remove tag")}</button>
                                })}
                            </div>
                        }.into_any()
                    }}
                }.into_any(),
            }}
        </div>
    }
}

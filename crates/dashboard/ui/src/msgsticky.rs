//! Message Builder phase 7: the sticky card in the Automate tab. One card per channel where the message stays the last
//! message: its order (with more than one sticky in a channel) and "Also after other bots".
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn blank(position: i64) -> Value {
    json!({"id": null, "channel": null, "position": position, "bots": false, "enabled": true})
}

#[allow(clippy::too_many_lines)]
pub fn sticky_tab(
    base: String,
    cur: RwSignal<Value>,
    dirty: Memo<bool>,
    rank: Signal<u8>,
    chan_name: impl Fn(&str) -> String + Copy + Send + Sync + 'static,
    picker: impl Fn(RwSignal<Value>) + Copy + Send + Sync + 'static,
    msg: Msg,
) -> impl IntoView {
    let embed = Signal::derive(move || cur.with(|c| c["id"].as_i64()));
    // A sticky has no member, so member variables make the message not ready here.
    let ready_why = Signal::derive(move || {
        if dirty.get() {
            Some("save or cancel your changes to the message first".to_string())
        } else {
            cur.with(|c| c["missing"].as_str().map(String::from))
        }
    });
    let base = StoredValue::new(base);
    let rows = RwSignal::new(Vec::<Value>::new());
    let editing = RwSignal::new(None::<RwSignal<Value>>);
    let busy = RwSignal::new(false);
    let locked = Signal::derive(move || rank.get() < 3);
    let url = move || {
        embed
            .get_untracked()
            .map(|id| format!("{}/{id}/stickies", base.get_value()))
    };
    let load = move || {
        let Some(u) = url() else {
            return rows.set(vec![]);
        };
        let asked = embed.get_untracked();
        spawn_local(async move {
            let r = api("GET", &u, None).await;
            // Another message was opened meanwhile, or the page left.
            if embed.try_get_untracked() != Some(asked) {
                return;
            }
            match r {
                Ok(v) => rows.set(v.as_array().cloned().unwrap_or_default()),
                Err(e) => msg.err(e),
            }
        })
    };
    Effect::new(move |_| {
        embed.track();
        load()
    });
    let save = move |r: Value| {
        let Some(u) = url() else { return };
        busy.set(true);
        spawn_local(async move {
            let body = json!({"sticky": r["id"], "channel": r["channel"], "position": r["position"], "bots": r["bots"],
                "enabled": r["enabled"], "seen": cur.with_untracked(|c| c["version"].clone())});
            let res = api("POST", &u, Some(body)).await;
            busy.set(false);
            match res {
                Ok(v) => {
                    // Turning a sticky on vouches for the message, which moves its version on.
                    cur.update(|c| c["version"] = v["version"].clone());
                    editing.set(None);
                    msg.ok(if r["enabled"] == true {
                        "Sticky saved. Brim posts it now."
                    } else {
                        "Sticky saved, off."
                    });
                    load()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let remove = move |id: i64| {
        let Some(u) = url() else { return };
        ask(
            Ask::new("Stop keeping this message at the bottom?")
                .text("Brim deletes its sticky copy in that channel.")
                .yes("Remove")
                .danger(),
            move |_| {
                let u = u.clone();
                spawn_local(async move {
                    match api("POST", &format!("{u}/{id}/delete"), Some(json!({}))).await {
                        Ok(_) => {
                            msg.ok("Sticky removed.");
                            load()
                        }
                        Err(e) => msg.err(e),
                    }
                })
            },
        )
    };
    let editor = move |r: RwSignal<Value>| {
        let why = move || {
            r.with(|v| {
                if v["channel"].is_null() {
                    Some("Choose a channel.".to_string())
                } else if v["enabled"] == true {
                    ready_why
                        .get()
                        .map(|w| format!("The message is not ready: {w}"))
                } else {
                    None
                }
            })
        };
        view! {
            <div class="card send rule">
                <h2>{move || if r.with(|v| v["id"].is_null()) { "New sticky" } else { "Change sticky" }}</h2>
                <div class="row"><label>"Channel"</label>
                    <button class="ghost chbtn" class:need=move || r.with(|v| v["channel"].is_null()) aria-haspopup="dialog" disabled=locked on:click=move |_| picker(r)>
                        {move || r.with(|v| v["channel"].as_str().map(chan_name)).unwrap_or_else(|| "Choose channel...".into())}</button>
                </div>
                <div class="row"><label>"Order"</label>
                    <div class="seg" role="group" aria-label="order">
                        {[1, 2, 3].map(|n| view! {
                            <button class:on=move || r.with(|v| v["position"] == n) disabled=locked on:click=move |_| r.update(|v| v["position"] = json!(n))>{n.to_string()}</button>
                        }).collect_view()}
                    </div>
                    <Tip text="With more than one sticky message in a channel, the highest number is at the very bottom. A channel has at most 3." icon="info" />
                </div>
                <div class="row"><label>"Also after other bots"</label>
                    <input type="checkbox" class="sw" aria-label="also after other bots" disabled=locked prop:checked=move || r.with(|v| v["bots"] == true)
                        on:change=move |e| { let c = event_target_checked(&e); r.update(|v| v["bots"] = json!(c)) } />
                    <Tip text="Off: only members' messages move it down. On: other bots' messages do too. Brim's own messages never do. If other bots alone make it move 10 times in a row, it pauses." icon="info" />
                </div>
                <div class="row"><label>"On"</label>
                    <div class="seg" role="group" aria-label="sticky on or off">
                        <button class:on=move || r.with(|v| v["enabled"] == true) on:click=move |_| r.update(|v| v["enabled"] = json!(true))>"On"</button>
                        <button class:on=move || r.with(|v| v["enabled"] != true) on:click=move |_| r.update(|v| v["enabled"] = json!(false))>"Off"</button>
                    </div>
                </div>
                {move || why().map(|w| view! { <p class="err">{w}</p> })}
                <div class="row">
                    <button disabled=move || why().is_some() || busy.get() on:click=move |_| save(r.get_untracked())>{busy_label(move || busy.get(), "Save sticky")}</button>
                    <button class="ghost" on:click=move |_| editing.set(None)>"Cancel"</button>
                </div>
            </div>
        }
    };
    let card = move |v: Value| {
        let id = v["id"].as_i64().unwrap_or(0);
        let on = v["enabled"] == true;
        let ch = chan_name(v["channel"].as_str().unwrap_or(""));
        let mut extra = vec![format!("order {}", v["position"].as_i64().unwrap_or(1))];
        extra.push(if v["bots"] == true {
            "also after other bots".into()
        } else {
            "after members".into()
        });
        let issue = v["issue"].as_str().map(String::from);
        // Its webhook was deleted in Discord, as the Sent log says for other copies.
        let gone = v["hook_gone"].is_object().then(|| format!(
            "Brim's webhook for this copy was deleted in Discord on {}. The next repost makes a new webhook and deletes this copy.",
            crate::discord::time(v["hook_gone"]["at"].as_i64().unwrap_or(0), "f")
        ));
        let v2 = v.clone();
        view! {
            <li class="rcard" class:off=!on>
                <div><b>{crate::icons::icon("pin")}" "{ch}</b>
                    <div class="muted small">{if on { "On" } else { "Off" }}" · "{extra.join(" · ")}</div>
                    {gone.map(|i| view! { <div class="err small">{i}</div> })}
                    {issue.map(|i| view! { <div class="err small">{i}</div> })}
                </div>
                <span class="sp"></span>
                <button class="ghost" disabled=locked on:click=move |_| editing.set(Some(RwSignal::new(v2.clone())))>"Change"</button>
                <button class="ghost" aria-label="remove sticky" disabled=locked on:click=move |_| remove(id)>{crate::icons::icon("trash")}</button>
            </li>
        }
    };
    view! {
        <div class="card sentlog rules">
            <h2>"Sticky"</h2>
            {move || match embed.get() {
                None => view! { <p class="muted">"Save the message first, then make it sticky."</p> }.into_any(),
                Some(_) => view! {
                    <p class="muted small">"Keep this message as the last message in a channel. After new messages, Brim waits until the channel is quiet for 3 seconds, then posts it again and deletes its old copy. Reposts pause while panic mode, a lockdown or an anti-nuke case is on."</p>
                    {move || {
                        let list = rows.get();
                        (!list.is_empty()).then(|| view! { <ul>{list.into_iter().map(card).collect_view()}</ul> })
                    }}
                    {move || {
                        let new = move || editing.set(Some(RwSignal::new(blank(1))));
                        let busy = Signal::derive(move || editing.with(Option::is_some));
                        if locked.get() {
                            view! { <p class="muted small">"Only the Owner, Extra Owners and Trusted Admins make messages sticky."</p> }.into_any()
                        } else if rows.with(Vec::is_empty) {
                            view! { <Empty title="Not sticky anywhere yet" hint="Pick a channel, and this message stays the last one there." label="Add channel" add=new disabled=busy /> }.into_any()
                        } else {
                            view! { <div class="row"><AddButton label="Add channel" attr:disabled=move || busy.get() || !rows.with(|l| room(l.len(), "per_message")) on:click=move |_| new() /><Count n=Signal::derive(move || rows.with(Vec::len)) key="per_message" /></div> }.into_any()
                        }
                    }}
                }.into_any(),
            }}
        </div>
        {move || editing.get().map(editor)}
    }
}

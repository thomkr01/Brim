//! Message Builder phase 6: the Automate tab's trigger rules ("When members write"): watched
//! channels, words and patterns with their own on/off, a "Try a message" box, the answer (channel
//! or DM, reply), cooldown, delete after, DM on cooldown, and member roles.
use crate::common::*;
use crate::msgparts::RuleList;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn blank() -> Value {
    json!({"id": null, "enabled": true, "spec": {
        "channels": [], "words": [], "patterns": [], "ignore_case": true, "whole_words": true,
        "answer": "channel", "reply": true, "cooldown": 30, "delete_after": null,
        "dm_on_cooldown": null, "roles": {"mode": "skip", "ids": []},
    }})
}

/// "30 s", "5 min", "2 h".
fn span(s: i64) -> String {
    match s {
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// A seconds box for an optional setting: a switch, then the number.
fn seconds_row(
    r: RwSignal<Value>,
    key: &'static str,
    label: &'static str,
    default: i64,
    min: i64,
    locked: Signal<bool>,
    tip: &'static str,
) -> impl IntoView {
    let on = move || r.with(|v| !v["spec"][key].is_null());
    view! {
        <div class="row"><label>{label}</label>
            <input type="checkbox" class="sw" aria-label=label prop:checked=on disabled=locked
                on:change=move |e| { let c = event_target_checked(&e); r.update(|v| v["spec"][key] = if c { json!(default) } else { Value::Null }) } />
            <Show when=on>
                <input type="number" min=min max="86400" aria-label=format!("{label} seconds") disabled=locked
                    prop:value=move || r.with(|v| v["spec"][key].as_i64().unwrap_or(default).to_string())
                    on:change=move |e| { let n = event_target_value(&e).parse::<i64>().unwrap_or(default); r.update(|v| v["spec"][key] = json!(n)) } />
                <span class="muted small">"seconds"</span>
            </Show>
            <Tip text=tip icon="info" />
        </div>
    }
}

#[allow(clippy::too_many_lines)]
pub fn triggers(
    base: String,
    cur: RwSignal<Value>,
    dirty: Memo<bool>,
    rank: Signal<u8>,
    chan_name: impl Fn(&str) -> String + Copy + Send + Sync + 'static,
    picker: impl Fn(RwSignal<Value>) + Copy + Send + Sync + 'static,
    msg: Msg,
) -> impl IntoView {
    // A trigger answer has a member, so member variables are fine there.
    let l = RuleList::new(base, "triggers", "missing_answer", cur, dirty, rank, msg);
    let RuleList { embed, ready_why, rows: rules, editing, busy, locked, .. } = l;
    let (open_editing, url, load) = (move |v| l.open(v), move || l.url(), move || l.load());
    let roles = use_context::<crate::msgparts::Parts>().map(|p| p.roles);
    let role_name = move |id: &str| {
        roles
            .and_then(|r| r.with(|l| l.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone())))
            .unwrap_or_else(|| id.to_string())
    };
    let save = move |r: Value| {
        let Some(u) = url() else { return };
        busy.set(true);
        spawn_local(async move {
            let body = json!({"rule": r["id"], "spec": r["spec"], "enabled": r["enabled"], "seen": cur.with_untracked(|c| c["version"].clone())});
            let res = api("POST", &u, Some(body)).await;
            busy.set(false);
            match res {
                Ok(v) => {
                    // Turning a trigger on vouches for the message, which moves its version on.
                    cur.update(|c| c["version"] = v["version"].clone());
                    editing.set(None);
                    msg.ok("Trigger saved.");
                    load()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let remove = move |id: i64| {
        let Some(u) = url() else { return };
        ask(Ask::new("Delete this trigger?").text("Brim stops answering it. Answers it sent stay in the Sent log.").yes("Delete").danger(), move |_| {
            let u = u.clone();
            spawn_local(async move {
                match api("POST", &format!("{u}/{id}/delete"), Some(json!({}))).await {
                    Ok(_) => { msg.ok("Trigger deleted."); load() }
                    Err(e) => msg.err(e),
                }
            })
        })
    };
    let editor = move |r: RwSignal<Value>| {
        // The channel picker writes "channel" here; it is then added to the watched list.
        let add = RwSignal::new(json!({"channel": null}));
        Effect::new(move |_| {
            if let Some(c) = add.with(|a| a["channel"].as_str().map(String::from)) {
                r.update(|v| {
                    if let Some(l) = v["spec"]["channels"].as_array_mut() {
                        if !l.iter().any(|x| x == c.as_str()) && l.len() < 25 {
                            l.push(json!(c))
                        }
                    }
                });
                add.set(json!({"channel": null}));
            }
        });
        let typed = RwSignal::new(String::new());
        let try_text = RwSignal::new(String::new());
        let tried = RwSignal::new(None::<Result<Option<String>, String>>);
        let push = move |k: &'static str| {
            let t = typed.get_untracked().trim().to_string();
            if t.is_empty() {
                return;
            }
            r.update(|v| {
                if let Some(l) = v["spec"][k].as_array_mut() {
                    l.push(json!({"text": t, "on": true}))
                }
            });
            typed.set(String::new());
        };
        // Asked 300 ms after the last key; an answer for older text is dropped (like CaptchaPreview).
        let seq = StoredValue::new(0u32);
        let run_try = move || {
            seq.update_value(|n| *n += 1);
            let mine = seq.get_value();
            set_timeout(
                move || {
                    if seq.try_get_value() != Some(mine) {
                        return;
                    }
                    let Some(u) = url() else { return };
                    let spec = r.with_untracked(|v| v["spec"].clone());
                    let text = try_text.get_untracked();
                    spawn_local(async move {
                        let v = api("POST", &format!("{u}/try"), Some(json!({"spec": spec, "text": text}))).await;
                        if seq.try_get_value() == Some(mine) {
                            tried.set(Some(v.map(|v| v["matched"].as_str().map(String::from))));
                        }
                    })
                },
                std::time::Duration::from_millis(300),
            );
        };
        let chip = move |k: &'static str, i: usize, w: Value| {
            let on = w["on"] != false;
            let t = w["text"].as_str().unwrap_or("").to_string();
            view! {
                <span class="tchip" class:off=!on>
                    <button class="ghost" title=if on { "On: click to switch off" } else { "Off: click to switch on" } aria-pressed=on.to_string() disabled=locked
                        on:click=move |_| r.update(|v| v["spec"][k][i]["on"] = json!(!on))>
                        <b>{if k == "words" { "Aa" } else { ".*" }}</b>" "{t}
                    </button>
                    <button class="ghost x" aria-label="remove" disabled=locked on:click=move |_| r.update(|v| if let Some(l) = v["spec"][k].as_array_mut() { if i < l.len() { l.remove(i); } })>"×"</button>
                </span>
            }
        };
        let why = move || {
            r.with(|v| {
                let s = &v["spec"];
                let any_on = ["words", "patterns"].iter().any(|k| s[k].as_array().is_some_and(|l| l.iter().any(|w| w["on"] != false)));
                if s["channels"].as_array().is_none_or(Vec::is_empty) {
                    Some("Choose a channel to watch.".to_string())
                } else if !any_on {
                    Some("Add a word or pattern that is on.".to_string())
                } else if v["enabled"] == true {
                    ready_why.get().map(|w| format!("The message is not ready: {w}"))
                } else {
                    None
                }
            })
        };
        let seg = move |key: &'static str, val: &'static str, label: &'static str| view! {
            <button class:on=move || r.with(|v| v["spec"][key] == val) disabled=locked on:click=move |_| r.update(|v| v["spec"][key] = json!(val))>{label}</button>
        };
        view! {
            <div class="card send rule trig">
                <h2>{move || if r.with(|v| v["id"].is_null()) { "New trigger" } else { "Change trigger" }}</h2>
                <div class="row"><label>"Watch"</label>
                    <div class="chipset">
                        {move || r.with(|v| v["spec"]["channels"].as_array().cloned().unwrap_or_default()).into_iter().enumerate().map(|(i, c)| {
                            let c = c.as_str().unwrap_or("").to_string();
                            view! { <span class="tchip"><span>{chan_name(&c)}</span>
                                <button class="ghost x" aria-label="stop watching" disabled=locked on:click=move |_| r.update(|v| if let Some(l) = v["spec"]["channels"].as_array_mut() { if i < l.len() { l.remove(i); } })>"×"</button></span> }
                        }).collect_view()}
                        <AddButton label="Add channel" class="chbtn" class:need=move || r.with(|v| v["spec"]["channels"].as_array().is_none_or(Vec::is_empty)) attr:aria-haspopup="dialog" attr:disabled=locked on:click=move |_| picker(add) />
                    </div>
                </div>
                <div class="row"><label>"Triggers"</label>
                    <input aria-label="word, phrase or pattern" placeholder="A word, phrase or pattern" maxlength="200" disabled=locked
                        prop:value=move || typed.get() on:input=move |e| typed.set(event_target_value(&e))
                        on:keydown=move |e| if crate::common::is_enter(&e) { push("words") } />
                    <AddButton label="Add word" attr:disabled=move || locked.get() || typed.with(|t| t.trim().is_empty()) on:click=move |_| push("words") />
                    <AddButton label="Add pattern" attr:disabled=move || locked.get() || typed.with(|t| t.trim().is_empty()) on:click=move |_| push("patterns") />
                    <Tip text="A word or phrase matches as typed. A pattern is a regular expression (Rust regex: no look-around), up to 200 characters. Click a trigger to switch it off or on." icon="info" />
                </div>
                <div class="chipset tlist" class:need=move || r.with(|v| ["words", "patterns"].iter().all(|k| v["spec"][k].as_array().is_none_or(|l| l.iter().all(|w| w["on"] == false))))>
                    {move || ["words", "patterns"].into_iter().flat_map(|k| r.with(|v| v["spec"][k].as_array().cloned().unwrap_or_default()).into_iter().enumerate().map(move |(i, w)| chip(k, i, w))).collect_view()}
                </div>
                <div class="row">
                    <label class="inl"><input type="checkbox" class="sw" aria-label="ignore capitalization" disabled=locked prop:checked=move || r.with(|v| v["spec"]["ignore_case"] != false)
                        on:change=move |e| { let c = event_target_checked(&e); r.update(|v| v["spec"]["ignore_case"] = json!(c)) } />"Ignore capitalization"</label>
                    <label class="inl"><input type="checkbox" class="sw" aria-label="whole words only" disabled=locked prop:checked=move || r.with(|v| v["spec"]["whole_words"] != false)
                        on:change=move |e| { let c = event_target_checked(&e); r.update(|v| v["spec"]["whole_words"] = json!(c)) } />"Whole words only"</label>
                </div>
                <div class="row"><label>"Try a message"</label>
                    <input aria-label="try a message" placeholder="Type a message a member could write" prop:value=move || try_text.get()
                        on:input=move |e| { try_text.set(event_target_value(&e)); run_try() } />
                </div>
                {move || tried.get().map(|t| match t {
                    Ok(Some(w)) => view! { <p class="ok small">"Matches “"{w}"”: Brim would answer."</p> }.into_any(),
                    Ok(None) => view! { <p class="muted small">"No trigger matches."</p> }.into_any(),
                    Err(e) => view! { <p class="err small">{e}</p> }.into_any(),
                })}
                <div class="row"><label>"Answer"</label>
                    <div class="seg" role="group" aria-label="answer where">{seg("answer", "channel", "Channel")}{seg("answer", "dm", "DM")}</div>
                    <Tip text="Channel: in the channel where the member wrote. DM: only to the member who wrote, nobody else. Brim sends at most 30 DMs an hour per server." icon="info" />
                </div>
                <Show when=move || r.with(|v| v["spec"]["answer"] != "dm")>
                    <div class="row"><label>"Reply"</label>
                        <input type="checkbox" class="sw" aria-label="reply to the message" disabled=locked prop:checked=move || r.with(|v| v["spec"]["reply"] != false)
                            on:change=move |e| { let c = event_target_checked(&e); r.update(|v| v["spec"]["reply"] = json!(c)) } />
                        <span class="muted small">"Reply to the member's message (not as a webhook)"</span>
                    </div>
                </Show>
                <div class="row"><label>"Cooldown"</label>
                    <input type="number" min="10" max="86400" aria-label="cooldown seconds" disabled=locked
                        prop:value=move || r.with(|v| v["spec"]["cooldown"].as_i64().unwrap_or(30).to_string())
                        on:change=move |e| { let n = event_target_value(&e).parse::<i64>().unwrap_or(30); r.update(|v| v["spec"]["cooldown"] = json!(n)) } />
                    <span class="muted small">"seconds per channel"</span>
                    <Tip text="After an answer, this trigger waits this long in that channel (10 seconds at least). A member gets one answer every 30 seconds at most." icon="info" />
                </div>
                {seconds_row(r, "delete_after", "Delete after", 30, 10, locked, "Brim deletes its answer after this long (10 seconds to 24 hours).")}
                {seconds_row(r, "dm_on_cooldown", "DM on cooldown", 300, 60, locked, "When a member sets the trigger off during its cooldown, Brim tells them by DM, at most once per this wait (1 minute to 24 hours).")}
                <div class="row"><label>"Members"</label>
                    <div class="seg" role="group" aria-label="roles mode">
                        <button class:on=move || r.with(|v| v["spec"]["roles"]["mode"] != "only") disabled=locked on:click=move |_| r.update(|v| v["spec"]["roles"]["mode"] = json!("skip"))>"Skip members with"</button>
                        <button class:on=move || r.with(|v| v["spec"]["roles"]["mode"] == "only") disabled=locked on:click=move |_| r.update(|v| v["spec"]["roles"]["mode"] = json!("only"))>"Only members with"</button>
                    </div>
                    <crate::form::ListPick items=Signal::derive(move || roles.map(|r| r.get()).unwrap_or_default()) get=Signal::derive(String::new) set=Callback::new(move |id: String| { if !id.is_empty() { r.update(|v| if let Some(l) = v["spec"]["roles"]["ids"].as_array_mut() { if !l.iter().any(|x| x == id.as_str()) && l.len() < 25 { l.push(json!(id)) } }) } }) none="+ Role" label="add a role" disabled=locked />
                </div>
                <div class="chipset">
                    {move || r.with(|v| v["spec"]["roles"]["ids"].as_array().cloned().unwrap_or_default()).into_iter().enumerate().map(|(i, id)| {
                        let id = id.as_str().unwrap_or("").to_string();
                        view! { <span class="tchip"><span>"@"{role_name(&id)}</span>
                            <button class="ghost x" aria-label="remove role" disabled=locked on:click=move |_| r.update(|v| if let Some(l) = v["spec"]["roles"]["ids"].as_array_mut() { if i < l.len() { l.remove(i); } })>"×"</button></span> }
                    }).collect_view()}
                </div>
                <div class="row"><label>"On"</label>
                    <div class="seg" role="group" aria-label="trigger on or off">
                        <button class:on=move || r.with(|v| v["enabled"] == true) on:click=move |_| r.update(|v| v["enabled"] = json!(true))>"On"</button>
                        <button class:on=move || r.with(|v| v["enabled"] != true) on:click=move |_| r.update(|v| v["enabled"] = json!(false))>"Off"</button>
                    </div>
                </div>
                {move || why().map(|w| view! { <p class="err">{w}</p> })}
                <div class="row">
                    <button disabled=move || why().is_some() || busy.get() on:click=move |_| save(r.get_untracked())>{busy_label(move || busy.get(), "Save trigger")}</button>
                    <button class="ghost" on:click=move |_| editing.set(None)>"Cancel"</button>
                </div>
            </div>
        }
    };
    let card = move |v: Value| {
        let id = v["id"].as_i64().unwrap_or(0);
        let on = v["enabled"] == true;
        let s = &v["spec"];
        let words: Vec<String> = ["words", "patterns"]
            .iter()
            .flat_map(|k| s[*k].as_array().cloned().unwrap_or_default())
            .filter(|w| w["on"] != false)
            .filter_map(|w| w["text"].as_str().map(|t| format!("“{t}”")))
            .collect();
        let chans: Vec<String> = s["channels"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(chan_name)).collect();
        let how = if s["answer"] == "dm" { "DM to the member".to_string() } else { "answer in the channel".to_string() };
        let head = format!("{} in {} → {how}", words.join(", "), chans.join(", "));
        let mut extra = vec![format!("cooldown {}", span(s["cooldown"].as_i64().unwrap_or(30)))];
        if let Some(d) = s["delete_after"].as_i64() {
            extra.push(format!("deleted after {}", span(d)));
        }
        let issue = v["issue"].as_str().map(String::from);
        let v2 = v.clone();
        view! {
            <li class="rcard" class:off=!on>
                <div><b>{head}</b>
                    <div class="muted small">{if on { "On" } else { "Off" }}" · "{extra.join(" · ")}</div>
                    {issue.map(|i| view! { <div class="err small">{i}</div> })}
                </div>
                <span class="sp"></span>
                <button class="ghost" disabled=locked on:click=move |_| open_editing(v2.clone())>"Change"</button>
                <button class="ghost" aria-label="delete trigger" disabled=locked on:click=move |_| remove(id)>{crate::icons::icon("trash")}</button>
            </li>
        }
    };
    view! {
        <div class="card sentlog rules">
            <h2>"Triggers"</h2>
            {move || match embed.get() {
                None => view! { <p class="muted">"Save the message first, then add a trigger."</p> }.into_any(),
                Some(_) => view! {
                    <p class="muted small">"Answer members who write a word or phrase in a channel, with this message. Member variables work here. Answers never ping, and pause while panic mode, a lockdown or an anti-nuke case is on."</p>
                    {move || {
                        let list = rules.get();
                        (!list.is_empty()).then(|| view! { <ul>{list.into_iter().map(card).collect_view()}</ul> })
                    }}
                    {move || {
                        let new = move || open_editing(blank());
                        let busy = Signal::derive(move || editing.with(Option::is_some));
                        if locked.get() {
                            view! { <p class="muted small">"Only the Owner, Extra Owners and Trusted Admins make or change triggers."</p> }.into_any()
                        } else if rules.with(Vec::is_empty) {
                            view! { <Empty title="No triggers yet" hint="Pick the words that make Brim answer with this message, and the channels it listens in." label="Add new trigger" add=new disabled=busy /> }.into_any()
                        } else {
                            view! { <div class="row"><AddButton label="Add new trigger" attr:disabled=move || busy.get() || !rules.with(|l| room(l.len(), "per_message")) on:click=move |_| new() /><Count n=Signal::derive(move || rules.with(Vec::len)) key="per_message" /></div> }.into_any()
                        }
                    }}
                }.into_any(),
            }}
        </div>
        {move || editing.get().map(editor)}
    }
}

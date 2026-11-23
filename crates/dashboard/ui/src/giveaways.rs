//! Giveaways page: the settings card (on/off and the defaults of a new giveaway), links to the
//! three messages in the Message Builder, the Start form, and the running and ended giveaways with
//! their actions. Needs the Manage Giveaways permit perm (the server says so on refusal).
use crate::common::*;
use crate::form::{items, one_member, ListPick, Section};
use crate::giveaway_plans as plans;
use crate::records::{learn_names, who};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(crate) fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

pub(crate) fn now() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// "2d 12h", "90m", "1w": seconds; `None` when a part is not a number and a unit.
fn parse_duration(t: &str) -> Option<i64> {
    let mut total = 0i64;
    let mut any = false;
    for part in t.split_whitespace() {
        let (n, unit) = part.split_at(part.find(|c: char| !c.is_ascii_digit())?);
        let n: i64 = n.parse().ok()?;
        let unit = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86_400,
            "w" => 604_800,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(unit)?)?;
        any = true;
    }
    any.then_some(total)
}

/// Seconds as the text the "Runs for" box reads: "2d 3h 5m".
fn fmt_duration(secs: i64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    let t: Vec<String> = [(d, "d"), (h, "h"), (m, "m")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, u)| format!("{n}{u}"))
        .collect();
    if t.is_empty() {
        "1m".into()
    } else {
        t.join(" ")
    }
}

/// Role chips plus the shared picker, for one role list of the entry rules.
#[component]
fn Roles(
    dc: RwSignal<Value>,
    d: RwSignal<Value>,
    key: &'static str,
    label: &'static str,
) -> impl IntoView {
    let get = move || crate::form::strs(&d.with(|v| v["req"][key].clone()));
    let set = move |l: Vec<String>| d.update(|v| v["req"][key] = json!(l));
    view! { <div class="row"><label>{label}</label><div class="pick">
        {move || get().into_iter().map(|id| {
            let name = items(&dc.get(), "roles").into_iter().find(|(i, _)| *i == id).map_or_else(|| id.clone(), |(_, l)| l);
            view! { <span class="chip">{name}" "<button class="link" aria-label=format!("remove from {label}") on:click=move |_| { let mut l = get(); l.retain(|x| *x != id); set(l) }>"×"</button></span> }
        }).collect_view()}
        <ListPick items=Signal::derive(move || { let on = get(); items(&dc.get(), "roles").into_iter().filter(|(i, _)| !on.contains(i)).collect() })
            get=Signal::derive(String::new) none="" label=label
            set=Callback::new(move |id: String| { let mut l = get(); if !id.is_empty() && !l.contains(&id) { l.push(id); set(l) } }) />
    </div></div> }
}

#[component]
pub fn Giveaways(guild: String) -> impl IntoView {
    let g = StoredValue::new(guild.clone());
    let base = move || format!("/api/guilds/{}/giveaways", g.get_value());
    let data = RwSignal::new(None::<Value>);
    let failed = RwSignal::new(None::<String>);
    let dc = RwSignal::new(json!({}));
    let names = RwSignal::new(HashMap::<String, String>::new());
    let msg = Msg::default();
    let busy = RwSignal::new(false);
    // The open Start or Edit form ("id" null: a new giveaway). Made here, not in a click handler.
    let draft = RwSignal::new(None::<Value>);
    let d = RwSignal::new(Value::Null);
    let (host, sponsor) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let length = RwSignal::new(String::from("1d"));
    // The giveaway whose entrants are open, with the page.
    let people = RwSignal::new(None::<(i64, i64, Value)>);
    let load = move || {
        spawn_local(async move {
            let r = api("GET", &base(), None).await;
            if data.is_disposed() {
                return;
            }
            match r {
                Ok(v) => {
                    let mut ids: Vec<String> = vec![];
                    for x in v["giveaways"].as_array().into_iter().flatten() {
                        ids.extend(crate::form::strs(&x["winner_ids"]));
                        ids.extend([s(&x["host"]), s(&x["sponsor"])]);
                    }
                    learn_names(g.get_value(), ids, names);
                    failed.set(None);
                    data.set(Some(v))
                }
                Err(e) => failed.set(Some(e)),
            }
        })
    };
    load();
    spawn_local(async move {
        if let Ok(v) = api(
            "GET",
            &format!("/api/guilds/{}/discord", g.get_value()),
            None,
        )
        .await
        {
            dc.try_set(v);
        }
    });
    let plans_url = move || format!("/api/guilds/{}/giveaway-plans", g.get_value());
    // One action at a time; the list reads again after it.
    let send = move |url: String, body: Value, done: &'static str| {
        busy.set(true);
        spawn_local(async move {
            let r = api("POST", &url, Some(body)).await;
            if busy.try_set(false).is_some() {
                return;
            }
            match r {
                Ok(_) => {
                    draft.set(None);
                    msg.done(done)
                }
                Err(e) => msg.err(e),
            }
            load();
        })
    };
    let act = move |path: String, body: Value, done: &'static str| {
        let url = if path.is_empty() { base() } else { format!("{}/{path}", base()) };
        send(url, body, done)
    };
    // A saved template or plan with no claim time follows the server's default.
    let claim_default = move || {
        data.with_untracked(|v| v.as_ref().map(|v| v["limits"]["claim_default"].clone()))
            .unwrap_or_else(|| json!(48))
    };
    let open_form = move |mut v: Value| {
        if v["claim_hours"].is_null() {
            v["claim_hours"] = claim_default();
        }
        host.set(s(&v["host"]));
        sponsor.set(s(&v["sponsor"]));
        if let Some(secs) = v["duration_secs"].as_i64() {
            length.set(fmt_duration(secs));
        }
        d.set(v.clone());
        draft.set(Some(v));
    };
    let start_new = move || {
        let defaults = data
            .with_untracked(|v| v.as_ref().map(|v| v["defaults"].clone()))
            .unwrap_or_else(|| json!({}));
        length.set("1d".into());
        let claim = data
            .with_untracked(|v| v.as_ref().map(|v| v["limits"]["claim_default"].clone()))
            .unwrap_or_else(|| json!(48));
        let extras = data
            .with_untracked(|v| v.as_ref().map(|v| v["extras"].clone()))
            .unwrap_or_else(|| json!({}));
        open_form(json!({"id": null, "channel": "", "prize": "", "winners": 1, "req": defaults, "claim_hours": claim, "extras": extras, "code": ""}));
    };
    let new_plan = move || {
        start_new();
        d.update(|v| *v = plans::as_plan(v.take()));
        draft.set(Some(d.get_untracked()));
    };
    // A saved template or plan, or a giveaway, as a new Start form: same settings, no entry code.
    let copy_into_form = move |mut v: Value| {
        v["id"] = Value::Null;
        v["code"] = json!("");
        v["kind"] = Value::Null;
        if v["channel"].is_null() {
            v["channel"] = json!("");
        }
        open_form(v);
    };
    let duplicate = move |x: Value| {
        let mut v = x.clone();
        let secs = x["ends_at"].as_i64().unwrap_or(0) - x["created"].as_i64().unwrap_or(0);
        v["duration_secs"] = json!((secs / 60 * 60).max(60));
        copy_into_form(v);
    };
    let edit_saved = move |mut v: Value| {
        v["kind"] = json!(if v["spec"].is_null() {
            "template"
        } else {
            "plan"
        });
        if v["channel"].is_null() {
            v["channel"] = json!("");
        }
        open_form(v);
    };
    // The settings of a template go into the open form; the name, channel and schedule stay.
    let apply_template = move |t: Value| {
        host.set(s(&t["host"]));
        sponsor.set(s(&t["sponsor"]));
        length.set(fmt_duration(t["duration_secs"].as_i64().unwrap_or(86_400)));
        d.update(|v| {
            for k in ["prize", "winners", "claim_hours", "req", "extras"] {
                v[k] = t[k].clone();
            }
            if v["claim_hours"].is_null() {
                v["claim_hours"] = claim_default();
            }
        });
    };
    let delete_saved = move |v: Value| {
        let (id, name) = (v["id"].as_i64().unwrap_or(0), s(&v["name"]));
        ask(
            Ask::new(format!("Delete “{name}”?"))
                .text("Giveaways it already started keep running.")
                .yes("Delete")
                .danger(),
            move |_| {
                send(
                    format!("{}/{id}/delete", plans_url()),
                    json!({}),
                    "Deleted.",
                )
            },
        )
    };
    let save_form = move || {
        let mut v = d.get_untracked();
        let new = v["id"].is_null();
        v["host"] = json!(host.get_untracked());
        v["sponsor"] = json!(sponsor.get_untracked());
        let saved = matches!(v["kind"].as_str(), Some("plan" | "template"));
        if saved {
            v["duration_secs"] = json!(parse_duration(&length.get_untracked()).unwrap_or(0));
            if v["kind"] == "template" {
                v["channel"] = Value::Null;
                v["spec"] = Value::Null;
            }
            send(plans_url(), v, "Saved.");
        } else if new {
            v["duration_secs"] = json!(parse_duration(&length.get_untracked()).unwrap_or(0));
            act(
                String::new(),
                v,
                "Giveaway saved. Brim posts it in a few seconds.",
            );
        } else {
            act(
                v["id"].to_string(),
                v,
                "Saved! The message shows it within a minute.",
            );
        }
    };
    // The open Start form as a template (a name is asked first).
    let save_template = move || {
        let mut v = d.get_untracked();
        v["host"] = json!(host.get_untracked());
        v["sponsor"] = json!(sponsor.get_untracked());
        let Some(secs) = parse_duration(&length.get_untracked()) else {
            return msg.err("Type how long it runs first, for example 2d or 1h 30m.");
        };
        v["duration_secs"] = json!(secs);
        v["id"] = Value::Null;
        v["channel"] = Value::Null;
        v["spec"] = Value::Null;
        ask(
            Ask::new("Save as a template")
                .text("Name of the template:")
                .input("")
                .yes("Save"),
            move |name| {
                v["name"] = json!(name.trim());
                send(plans_url(), v, "Template saved.");
            },
        )
    };
    let chan = move |id: &str| crate::form::name_of(&dc.get(), "channels", id);
    let form_view = move || {
        draft.get().map(|start| {
            let kind = s(&start["kind"]);
            let saved = kind == "plan" || kind == "template";
            let new = start["id"].is_null();
            // The fields of a whole giveaway: a new one, or a template or plan.
            let full = new || saved;
            let template = kind == "template";
            let title = match (kind.as_str(), new) {
                ("plan", true) => "New plan",
                ("plan", false) => "Change plan",
                ("template", _) => "Change template",
                (_, true) => "Start a giveaway",
                _ => "Change giveaway",
            };
            let templates = data
                .with_untracked(|v| v.as_ref().map(|v| v["plans"]["templates"].as_array().cloned().unwrap_or_default()))
                .unwrap_or_default();
            let close = move || draft.set(None);
            let limits = data.with_untracked(|v| v.as_ref().map(|v| v["limits"].clone())).unwrap_or_default();
            let max_w = limits["winners"].as_i64().unwrap_or(100);
            let why = move || -> Option<&'static str> {
                let v = d.get();
                if saved && s(&v["name"]).trim().is_empty() {
                    return Some("Type a name.");
                }
                if full && !template && s(&v["channel"]).is_empty() {
                    return Some("Pick a channel.");
                }
                if s(&v["prize"]).trim().is_empty() {
                    return Some("Type the prize.");
                }
                if full && parse_duration(&length.get()).is_none() {
                    return Some("Type how long it runs, for example 2d or 1h 30m.");
                }
                if !saved && v["extras"]["reaction"] == true && !s(&v["code"]).trim().is_empty() {
                    return Some("An entry code needs the Enter button: turn reaction entry off.");
                }
                None
            };
            let num = move |key: &'static str| move |e: leptos::ev::Event| {
                let n: i64 = event_target_value(&e).parse().unwrap_or(0);
                d.update(|v| match key {
                    "winners" | "claim_hours" => v[key] = json!(n),
                    // An emptied field goes back to the default, not to 1 hour.
                    "role_hours" => v["extras"][key] = json!(if n < 1 { 168 } else { n }),
                    "thread_hours" => v["extras"][key] = json!(if n < 1 { 72 } else { n }),
                    _ => v["req"][key] = json!(n.max(0)),
                })
            };
            let r = NodeRef::<leptos::html::Div>::new();
            focus_on_open(r);
            view! {
                <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label=title on:keydown=move |e| dialog_keys(&e, close)><div class="card">
                    <h2>{title}</h2>
                    {(new && !templates.is_empty()).then(|| view! {
                        <div class="row"><label>"Template"</label><div class="chipset" role="group" aria-label="templates">
                            {templates.iter().cloned().map(|t| view! { <button class="chip" on:click=move |_| apply_template(t.clone())>{s(&t["name"])}</button> }).collect_view()}
                        </div></div>
                    })}
                    {saved.then(|| view! {
                        <div class="row"><label for="gw-name">"Name"</label>
                            <input id="gw-name" maxlength=limits["name"].as_i64().unwrap_or(60).to_string() prop:value=move || d.with(|v| s(&v["name"])) on:input=move |e| { let t = event_target_value(&e); d.update(|v| v["name"] = json!(t)) } /></div>
                    })}
                    {(full && !template).then(|| view! {
                        <div class="row"><label>"Channel"</label>
                            <ListPick items=Signal::derive(move || items(&dc.get(), "channels")) get=Signal::derive(move || d.with(|v| s(&v["channel"]))) set=Callback::new(move |x: String| d.update(|v| v["channel"] = json!(x))) none="choose a channel" label="channel" /></div>
                    })}
                    <div class="row"><label for="gw-prize">"Prize"</label>
                        <input id="gw-prize" maxlength=limits["prize"].as_i64().unwrap_or(200).to_string() prop:value=move || d.with(|v| s(&v["prize"])) on:input=move |e| { let t = event_target_value(&e); d.update(|v| v["prize"] = json!(t)) } placeholder="Nitro for a month" /></div>
                    <div class="row"><label for="gw-winners">"Winners"</label>
                        <input id="gw-winners" type="number" min="1" max=max_w.to_string() prop:value=move || d.with(|v| v["winners"].to_string()) on:change=num("winners") /></div>
                    {full.then(|| view! {
                        <div class="row"><label for="gw-length">"Runs for"</label>
                            <input id="gw-length" prop:value=move || length.get() on:input=move |e| length.set(event_target_value(&e)) placeholder="2d, 12h or 1h 30m" /></div>
                        <div class="row"><label for="gw-claim">"Claim time (hours)"</label>
                            <input id="gw-claim" type="number" min="1" max=limits["claim_hours"].as_i64().unwrap_or(720).to_string() prop:value=move || d.with(|v| v["claim_hours"].to_string()) on:change=num("claim_hours") /></div>
                        <p class="muted small hint">"Each winner presses Claim within this time. If not, Brim draws that place again (at most 3 times); a new winner has 24 hours."</p>
                        <div class="row"><label>"Host"</label>{one_member(host)}</div>
                        <div class="row"><label>"Sponsor"</label>{one_member(sponsor)}</div>
                        <p class="muted small hint">"Both are optional. The host cannot enter. The sponsor is shown on the message and cannot enter; the winners' message names them."</p>
                    })}
                    <details class="more"><summary>"Who can enter"</summary>
                        <Roles dc d key="required" label="Needs one of these roles" />
                        <Roles dc d key="blocked" label="Never with these roles" />
                        <Roles dc d key="bypass" label="These roles skip age, time and needed roles" />
                        <div class="row"><label for="gw-age">"Account age (days)"</label>
                            <input id="gw-age" type="number" min="0" prop:value=move || d.with(|v| v["req"]["min_age_days"].as_i64().unwrap_or(0).to_string()) on:change=num("min_age_days") /></div>
                        <div class="row"><label for="gw-member">"Time in the server (hours)"</label>
                            <input id="gw-member" type="number" min="0" prop:value=move || d.with(|v| v["req"]["min_member_hours"].as_i64().unwrap_or(0).to_string()) on:change=num("min_member_hours") /></div>
                        <div class="row"><label for="gw-raid">"Block raid joiners"</label>
                            <input id="gw-raid" type="checkbox" class="sw" role="switch" prop:checked=move || d.with(|v| v["req"]["block_raid_joiners"] == true) on:change=move |e| { let c = event_target_checked(&e); d.update(|v| v["req"]["block_raid_joiners"] = json!(c)) } /></div>
                        <p class="muted small hint">"Quarantined members, Raider List entries and members the Join Gate caught can never enter."</p>
                        <Extra dc d max=limits["extra"].as_i64().unwrap_or(50) />
                    </details>
                    <details class="more"><summary>"Extras"</summary>
                        <div class="row"><label>"Winner role"</label>
                            <ListPick items=Signal::derive(move || items(&dc.get(), "roles")) get=Signal::derive(move || d.with(|v| s(&v["extras"]["winner_role"]))) set=Callback::new(move |x: String| d.update(|v| v["extras"]["winner_role"] = if x.is_empty() { Value::Null } else { json!(x) })) none="no role" label="winner role" /></div>
                        <div class="row"><label for="gw-rolehours">"Keep the role for (hours)"</label>
                            <input id="gw-rolehours" type="number" min="1" max="8760" prop:value=move || d.with(|v| v["extras"]["role_hours"].as_i64().unwrap_or(168).to_string()) on:change=num("role_hours") /></div>
                        <div class="row"><label for="gw-thread">"Winner thread"</label>
                            <input id="gw-thread" type="checkbox" class="sw" role="switch" prop:checked=move || d.with(|v| v["extras"]["thread"] == true) on:change=move |e| { let c = event_target_checked(&e); d.update(|v| v["extras"]["thread"] = json!(c)) } /></div>
                        <div class="row"><label for="gw-threadhours">"Close the thread after (hours)"</label>
                            <input id="gw-threadhours" type="number" min="1" max="720" prop:value=move || d.with(|v| v["extras"]["thread_hours"].as_i64().unwrap_or(72).to_string()) on:change=num("thread_hours") /></div>
                        <p class="muted small hint">"Winners get the role at the draw and lose it after this time. The winner thread is private: the winners, the host and the sponsor. Brim closes it after this time."</p>
                        {full.then(|| view! {
                            <div class="row"><label for="gw-react">"Enter by reaction"</label>
                                <input id="gw-react" type="checkbox" class="sw" role="switch" prop:checked=move || d.with(|v| v["extras"]["reaction"] == true) on:change=move |e| { let c = event_target_checked(&e); d.update(|v| v["extras"]["reaction"] = json!(c)) } /></div>
                        })}
                        {(!saved && new).then(|| view! {
                            <div class="row"><label for="gw-code">"Entry code"</label>
                                <input id="gw-code" maxlength="50" prop:value=move || d.with(|v| s(&v["code"])) on:input=move |e| { let t = event_target_value(&e); d.update(|v| v["code"] = json!(t)) } placeholder="optional" /></div>
                        })}
                        <p class="muted small hint">"Reaction: members react with the button emoji instead of the button, and get no private answer. Entry code: members press Enter and type it; 3 wrong codes give a 10 minute wait. Brim keeps only a hash, so you cannot see the code again; a template, plan or duplicate never keeps it. Both stay as set once the giveaway starts."</p>
                    </details>
                    {(kind == "plan").then(|| view! { <plans::When d url=plans_url() /> })}
                    <div class="acts">
                        <button class="ghost" on:click=move |_| close()>"Cancel"</button>
                        {(new && !saved).then(|| view! { <button class="ghost" disabled=move || busy.get() on:click=move |_| save_template()>"Save as template"</button> })}
                        <button disabled=move || why().is_some() || busy.get() title=move || why().unwrap_or("") on:click=move |_| save_form()>{busy_label(move || busy.get(), if new && !saved { "Start" } else { "Save" })}</button>
                    </div>
                </div></div>
            }
        })
    };
    let open_people = move |id: i64, page: i64| {
        spawn_local(async move {
            let r = api("GET", &format!("{}/{id}/entries?page={page}", base()), None).await;
            if people.is_disposed() {
                return;
            }
            match r {
                Ok(v) => {
                    let ids = v["entries"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|e| s(&e["user"]))
                        .collect();
                    learn_names(g.get_value(), ids, names);
                    people.set(Some((id, page, v)))
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let people_view = move || {
        people.get().map(|(id, page, v)| {
            let close = move || people.set(None);
            let total = v["total"].as_i64().unwrap_or(0);
            let per = v["per_page"].as_i64().unwrap_or(50).max(1);
            let last = (total - 1).max(0) / per;
            let r = NodeRef::<leptos::html::Div>::new();
            focus_on_open(r);
            view! {
                <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Entrants" on:keydown=move |e| dialog_keys(&e, close)><div class="card">
                    <h2>{format!("Entrants of giveaway #{id}")}</h2>
                    <p class="muted small">{format!("{total} entrants. Page {} of {}.", page + 1, last + 1)}</p>
                    <table class="data"><thead><tr><th>"Member"</th><th>"Entries"</th></tr></thead><tbody>
                        {v["entries"].as_array().cloned().unwrap_or_default().into_iter().map(|e| view! {
                            <tr><td>{who(names, s(&e["user"]))}</td><td data-label="Entries">{e["weight"].to_string()}</td></tr>
                        }).collect_view()}
                    </tbody></table>
                    <div class="acts">
                        <button class="ghost" disabled=page == 0 on:click=move |_| open_people(id, page - 1)>"Previous"</button>
                        <button class="ghost" disabled={ page >= last } on:click=move |_| open_people(id, page + 1)>"Next"</button>
                        <span class="sp"></span>
                        <button on:click=move |_| close()>"Close"</button>
                    </div>
                </div></div>
            }
        })
    };
    let row_view = move |x: Value| {
        let id = x["id"].as_i64().unwrap_or(0);
        let running = x["status"] == "running";
        let prize = s(&x["prize"]);
        let when = |t: &Value| crate::discord::relative(t.as_i64().unwrap_or(0) - now());
        let mut line = vec![
            chan(&s(&x["channel"])),
            format!(
                "{} winner{}",
                x["winners"],
                if x["winners"] == 1 { "" } else { "s" }
            ),
            format!("{} entrants", x["entries"]),
        ];
        line.push(match x["status"].as_str() {
            Some("running") => format!("ends {}", when(&x["ends_at"])),
            Some("cancelled") => "cancelled".into(),
            _ => format!("ended {}", when(&x["ended_at"])),
        });
        if x["paused"] == true {
            line.push("paused".into());
        }
        if x["has_code"] == true {
            line.push("entry code".into());
        }
        if x["extras"]["reaction"] == true {
            line.push("reaction entry".into());
        }
        if running && x["message"].is_null() && x["issue"].is_null() {
            line.push("not posted yet".into());
        }
        let issue = x["issue"].as_str().map(String::from);
        let missing = running && x["message"].is_null() && issue.is_some();
        let pick = RwSignal::new(String::new());
        let winners = crate::form::strs(&x["winner_ids"]);
        let claims = x["claims"].as_array().cloned().unwrap_or_default();
        let claim_of = move |w: &str| -> String {
            match claims.iter().find(|c| c["user"] == w) {
                Some(c) if !c["claimed"].is_null() => "claimed".into(),
                Some(c) => format!("claim {}", crate::discord::relative(c["deadline"].as_i64().unwrap_or(0) - now())),
                None => String::new(),
            }
        };
        let (p1, p2, p3) = (prize.clone(), prize.clone(), prize.clone());
        let x2 = x.clone();
        let x3 = x.clone();
        view! {
            <li class="card">
                <div class="row"><b>{format!("#{id} {prize}")}</b><span class="sp"></span>
                    {(!running && x["status"] == "cancelled").then(|| view! { <span class="chip">"Cancelled"</span> })}</div>
                <div class="muted small">{line.join(" · ")}</div>
                {issue.map(|i| view! { <div class="err small">{i}</div> })}
                {missing.then(|| view! {
                    <div class="row"><label>"Post in"</label>
                        <ListPick items=Signal::derive(move || items(&dc.get(), "channels")) get=Signal::derive(move || pick.get()) set=Callback::new(move |c: String| pick.set(c)) none="choose a channel" label="new channel" />
                        <button disabled=move || busy.get() || pick.get().is_empty() on:click=move |_| act(format!("{id}/channel"), json!({"channel": pick.get_untracked()}), "Brim posts the giveaway there in a few seconds.")>"Post here"</button></div>
                })}
                {(!winners.is_empty()).then(|| view! {
                    <div class="row"><span class="muted">"Winners:"</span>
                        {winners.clone().into_iter().map(|w| { let w2 = w.clone(); let p = p3.clone(); view! {
                            <span class="chip">{who(names, w.clone())}" "{let c = claim_of(&w); (!c.is_empty()).then(|| view! { <span class="muted small">{c}" "</span> })}
                                <button class="link" aria-label="redraw this winner" disabled=move || busy.get() on:click=move |_| {
                                    let w = w2.clone();
                                    ask(Ask::new(format!("Redraw a winner of “{p}”?")).text("Brim draws a new winner for this place and tells them. Reason (staff only, in the log):").input("").yes("Redraw"),
                                        move |reason| act(format!("{id}/reroll"), json!({"users": [w], "reason": reason}), "Redrawing. The new winner shows here in a few seconds."))
                                }>"Redraw"</button></span>
                        } }).collect_view()}</div>
                })}
                <div class="row">
                    <span class="sp"></span>
                    <button class="ghost" disabled=move || busy.get() on:click=move |_| duplicate(x3.clone())>"Duplicate"</button>
                    <button class="ghost" on:click=move |_| open_people(id, 0)>"Entrants"</button>
                    {running.then(|| view! {
                        <button class="ghost" disabled=move || busy.get() on:click=move |_| { let mut v = x2.clone(); v["req"] = x2["req"].clone(); open_form(v) }>"Edit"</button>
                        <button class="ghost" disabled=move || busy.get() on:click=move |_| ask(Ask::new("Change the end time").text("Type + or - and a time, for example +1h or -30m.").input("+1h").yes("Change"), move |t| {
                            let t = t.trim().to_string();
                            let (sign, rest) = if let Some(r) = t.strip_prefix('-') { (-1, r.to_string()) } else { (1, t.trim_start_matches('+').to_string()) };
                            match parse_duration(&rest) {
                                Some(secs) => act(format!("{id}/time"), json!({"secs": sign * secs}), "End time changed."),
                                None => msg.err("Type + or - and a time, for example +1h or -30m."),
                            }
                        })>"Time"</button>
                        <button class="ghost" disabled=move || busy.get() on:click=move |_| ask(Ask::new(format!("End “{p1}” now?")).text("Brim draws the winners now.").yes("End now"), move |_| act(format!("{id}/end"), json!({}), "Ending. The winners show here in a few seconds."))>"End now"</button>
                        <button class="danger" disabled=move || busy.get() on:click=move |_| ask(Ask::new(format!("Cancel “{p2}”?")).text("No winners are drawn. The message says the giveaway was cancelled.").yes("Cancel giveaway").danger(), move |_| act(format!("{id}/cancel"), json!({}), "Cancelled."))>"Cancel"</button>
                    })}
                    {(x["status"] == "ended").then(|| { let p = prize.clone(); view! {
                        <button class="ghost" disabled=move || busy.get() on:click=move |_| ask(Ask::new("Change the claim time").text("Hours from now that every winner who has not claimed yet has to press Claim (1 to 720).").input("48").yes("Change"), move |t| {
                            match t.trim().parse::<i64>() {
                                Ok(h) if (1..=720).contains(&h) => act(format!("{id}/claim"), json!({"hours": h}), "Claim time changed."),
                                _ => msg.err("Type a number of hours from 1 to 720."),
                            }
                        })>"Claim time"</button>
                        <button class="ghost" disabled=move || busy.get() on:click=move |_| { let p = p.clone(); ask(Ask::new(format!("Reroll “{p}”?")).text("Brim draws all winners again. Past winners cannot win again.").yes("Reroll"), move |_| act(format!("{id}/reroll"), json!({}), "Rerolling. The new winners show here in a few seconds.")) }>"Reroll"</button>
                    } })}
                </div>
            </li>
        }
    };
    let list = move || {
        if let Some(e) = failed.get() {
            return view! { <p class="err">{e}</p> }.into_any();
        }
        let Some(v) = data.get() else {
            return view! { <p><Working /></p> }.into_any();
        };
        dc.track();
        let all = v["giveaways"].as_array().cloned().unwrap_or_default();
        let (run, done): (Vec<Value>, Vec<Value>) =
            all.into_iter().partition(|x| x["status"] == "running");
        let on = v["on"] == true;
        view! {
            <div class="row"><h2>"Running"</h2><span class="sp"></span>
                <AddButton label="Start a giveaway" attr:disabled=move || busy.get() || !on attr:title=(!on).then_some("Turn giveaways on in the settings first") on:click=move |_| start_new() /></div>
            {if run.is_empty() { view! { <p class="muted">"No giveaway is running."</p> }.into_any() } else { view! { <ul class="stack">{run.into_iter().map(row_view).collect_view()}</ul> }.into_any() }}
            <h2>"Ended"</h2>
            {if done.is_empty() { view! { <p class="muted">"No ended giveaways yet."</p> }.into_any() } else { view! { <ul class="stack">{done.into_iter().map(row_view).collect_view()}</ul> }.into_any() }}
            <plans::Lists data chan busy on_new=Callback::new(move |_| new_plan()) on_use=Callback::new(copy_into_form)
                on_edit=Callback::new(edit_saved) on_delete=Callback::new(delete_saved) />
        }.into_any()
    };
    let gl = guild.clone();
    view! {
        <Section guild=guild.clone() name="giveaways".to_string() on_save=Callback::new(move |_| load()) />
        <div class="card">
            <h3>"Messages"</h3>
            <p class="muted small">"Each message opens with its preview and an Edit message button."</p>
            <div class="row">
                <a class="btn ghost" href=format!("#/{gl}/bot_messages/giveaway.running")>"Giveaway message"</a>
                <a class="btn ghost" href=format!("#/{gl}/bot_messages/giveaway.ended")>"Ended message"</a>
                <a class="btn ghost" href=format!("#/{gl}/bot_messages/giveaway.winners")>"Winners post"</a>
                <a class="btn ghost" href=format!("#/{gl}/bot_messages/giveaway.winner_dm")>"Winner DM"</a>
            </div>
        </div>
        {list}
        {form_view}
        {people_view}
        <Toast msg />
    }
}

/// Extra entries: a role and how many entries its members get (the highest one counts).
#[component]
fn Extra(dc: RwSignal<Value>, d: RwSignal<Value>, max: i64) -> impl IntoView {
    let rows = move || d.with(|v| v["req"]["extra"].as_array().cloned().unwrap_or_default());
    let pick = RwSignal::new(String::new());
    let count = RwSignal::new(2i64);
    view! {
        <div class="row"><label>"Extra entries"</label><div class="pick">
            {move || rows().into_iter().enumerate().map(|(i, e)| {
                let name = items(&dc.get(), "roles").into_iter().find(|(id, _)| *id == s(&e["role"])).map_or_else(|| s(&e["role"]), |(_, l)| l);
                view! { <span class="chip">{format!("{name}: {} entries", e["entries"])}" "<button class="link" aria-label="remove extra entries" on:click=move |_| d.update(|v| { if let Some(a) = v["req"]["extra"].as_array_mut() { a.remove(i); } })>"×"</button></span> }
            }).collect_view()}
        </div></div>
        <div class="row">
            <ListPick items=Signal::derive(move || items(&dc.get(), "roles")) get=Signal::derive(move || pick.get()) set=Callback::new(move |r: String| pick.set(r)) none="choose a role" label="role with extra entries" />
            <input type="number" min="2" max=max.to_string() aria-label="entries" prop:value=move || count.get().to_string() on:change=move |e| count.set(event_target_value(&e).parse().unwrap_or(2)) />
            <button class="ghost" disabled=move || pick.get().is_empty() on:click=move |_| {
                let (r, n) = (pick.get_untracked(), count.get_untracked().clamp(2, max));
                d.update(|v| {
                    if !v["req"]["extra"].is_array() { v["req"]["extra"] = json!([]); }
                    if let Some(a) = v["req"]["extra"].as_array_mut() { a.retain(|e| e["role"] != r.as_str()); a.push(json!({"role": r, "entries": n})); }
                });
                pick.set(String::new());
            }>"Add"</button>
        </div>
        <p class="muted small hint">"A member counts with the highest number of their roles. Everyone else has 1 entry."</p>
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_duration, parse_duration};

    #[test]
    fn a_length_written_back_reads_the_same() {
        for secs in [60, 3600, 5400, 86_400, 183_900, 7_776_000] {
            assert_eq!(parse_duration(&fmt_duration(secs)), Some(secs));
        }
        assert_eq!(fmt_duration(0), "1m");
    }

    #[test]
    fn durations_read_like_the_command() {
        assert_eq!(parse_duration("2d"), Some(172_800));
        assert_eq!(parse_duration("1h 30m"), Some(5400));
        assert_eq!(parse_duration("1w"), Some(604_800));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("99999999999999999w"), None);
    }
}

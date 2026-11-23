//! Bot messages: Brim's built-in messages, each editable on the Message Builder editor. A row
//! says Default or Edited; Revert puts the default back after a confirmation, and Undo brings the
//! edited copy back for 24 hours. Security texts a Trusted Admin changes, with a 2FA
//! code; locked texts
//! can be looked at, not changed.
use crate::common::*;
use crate::embeds::Editor;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

/// Variables of a template, as the editor's list wants them.
fn vars_of(m: &Value) -> Vec<(String, String, String)> {
    m["vars"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
            (s("name"), s("label"), s("example"))
        })
        .collect()
}

/// Important variables the edited copy no longer has: the page warns in red, the save still goes.
fn dropped(row: &Value, body: &Value) -> Vec<String> {
    let text = body.to_string();
    row["important"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|v| !text.contains(&format!("{{{v}}}")))
        .map(String::from)
        .collect()
}

/// Required placeholders the edited version does not show in its first embed (or in its text, for
/// an entry whose default is text only: `in_text`). The server refuses such a save
/// (`botmsg::missing_text`, the same rule); this only drives the notice.
pub(crate) fn missing(row: &Value, body: &Value) -> Vec<String> {
    let e = &body["embeds"][0];
    let mut shown = String::new();
    if row["in_text"] == true {
        shown.push_str(body["content"].as_str().unwrap_or(""));
        shown.push('\n');
    }
    for p in ["/title", "/description", "/footer/text", "/author/name"] {
        shown.push_str(e.pointer(p).and_then(Value::as_str).unwrap_or(""));
        shown.push('\n');
    }
    // A feed's link may be what the first embed's title opens (the GitHub look), as the server
    // allows (`botmsg::missing_text`).
    if row["key"].as_str().is_some_and(|k| k.starts_with("feed.")) {
        shown.push_str(e["url"].as_str().unwrap_or(""));
        shown.push('\n');
    }
    for f in e["fields"].as_array().into_iter().flatten() {
        shown.push_str(f["name"].as_str().unwrap_or(""));
        shown.push('\n');
        shown.push_str(f["value"].as_str().unwrap_or(""));
        shown.push('\n');
    }
    // A log or answer of facts accepts a copy made before it had placeholders: its `{text}` holds
    // every fact (`botmsg::missing_text`).
    let framed = row["details"] == true && shown.contains("{text}");
    row["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|v| !framed && !shown.contains(&format!("{{{v}}}")))
        .map(String::from)
        .collect()
}

/// Puts the entry's missing required placeholders back: at the end of the first embed's description
/// (an embed is added when there is none; `{title}` goes in an empty title), or for a text-only
/// entry at the end of the message text.
fn insert_required(row: &Value, body: &mut Value) {
    for var in missing(row, body) {
        let line = format!("{{{var}}}");
        let d = if row["in_text"] == true {
            &mut body["content"]
        } else {
            if body["embeds"].as_array().is_none_or(Vec::is_empty) {
                body["embeds"] = json!([{}]);
            }
            let e = &mut body["embeds"][0];
            if var == "title" && e["title"].as_str().is_none_or(|t| t.trim().is_empty()) {
                e["title"] = line.into();
                continue;
            }
            &mut e["description"]
        };
        let old = d.as_str().unwrap_or("").trim_end().to_string();
        *d = if old.is_empty() {
            line.into()
        } else {
            format!("{old}\n{line}").into()
        };
    }
}

/// Whether the search box holds text, which opens every area and sub-group with hits.
fn searching(q: &RwSignal<String>) -> bool {
    q.with(|q| !q.trim().is_empty())
}

/// The page order of the main areas (the server's `botmsg::AREAS`).
const AREAS: &[&str] = &[
    "Commands",
    "Logging",
    "Moderation",
    "Tickets",
    "Members and server",
    "Security",
];

/// The icon and the one-line text of a main area's row.
fn area_info(area: &str) -> (&'static str, &'static str) {
    match area {
        "Commands" => ("code", "Answers to commands, settings panels, buttons and error replies."),
        "Logging" => ("logs", "The log messages Brim posts in your log channels."),
        "Moderation" => ("moderator", "DMs to members, cases, reports and lockdown notices."),
        "Tickets" => ("ticket", "Ticket panels, ticket messages and ticket commands."),
        "Members and server" => ("member_join", "Verification, welcome, feeds and messages to the bot owner."),
        "Security" => ("lock", "Security texts. Trusted Admins and above can change them, with a 2FA code."),
        _ => ("message", ""),
    }
}

/// Rows grouped by main area, then sub-group, each in page order.
type Areas = Vec<(String, Vec<(String, Vec<Value>)>)>;

fn by_area(all: &[Value]) -> Areas {
    let mut out: Areas = AREAS.iter().map(|a| (a.to_string(), vec![])).collect();
    for r in all {
        let (a, sub) = (
            r["area"].as_str().unwrap_or(""),
            r["sub"].as_str().unwrap_or(""),
        );
        let i = out.iter().position(|x| x.0 == a).unwrap_or_else(|| {
            out.push((a.to_string(), vec![]));
            out.len() - 1
        });
        let subs = &mut out[i].1;
        match subs.iter_mut().find(|x| x.0 == sub) {
            Some(x) => x.1.push(r.clone()),
            None => subs.push((sub.to_string(), vec![r.clone()])),
        }
    }
    out.retain(|a| !a.1.is_empty());
    out
}

fn now() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

#[component]
pub fn BotMessages(
    guild: String,
    /// An entry key from the link (the Tickets tab's Edit embed): opened once the list loads.
    #[prop(optional)]
    open_key: String,
    /// Opened from the Tickets tab (`.../from-tickets` in the link): a Back button returns there.
    #[prop(optional)]
    from_tickets: bool,
) -> impl IntoView {
    let g = StoredValue::new(guild);
    let pending = StoredValue::new(open_key);
    let base = move || format!("/api/guilds/{}/bot-messages", g.get_value());
    let rows = RwSignal::new(None::<Vec<Value>>);
    let msg = Msg::default();
    let access = expect_context::<Access>();
    let admin = move || access.rank() >= 3;
    let owner = move || access.rank() >= 4;
    // Who may change entry `r`: a Trusted Admin (a security text also needs the 2FA code).
    let may = move |_: &Value| admin();
    // The open message's row, and the editor on its body.
    let open = RwSignal::new(None::<Value>);
    let cur = RwSignal::new(json!({"body": {}}));
    let orig = RwSignal::new(Value::Null);
    let dirty = Memo::new(move |_| cur.with(|c| c["body"].clone()) != orig.get());
    let busy = RwSignal::new(false);
    // Search text and the opened areas and sub-groups (all closed at first); kept for this visit only.
    let query = RwSignal::new(String::new());
    let opened = RwSignal::new(Vec::<String>::new());
    let is_open = move |id: &str| searching(&query) || opened.with(|f| f.iter().any(|x| x == id));
    let flip = move |id: &str| {
        opened.update(|f| {
            if let Some(i) = f.iter().position(|x| x == id) {
                f.remove(i);
            } else {
                f.push(id.to_string())
            }
        })
    };
    // Which action runs ("save", "test" or a post's path), so only its button shows the working sign.
    let doing = RwSignal::new(String::new());
    let ed = Editor::new(cur, g, true);
    let (message_tab, preview_tab, pop_view) = ed.views();
    on_cleanup(|| crate::msgparts::set_bot_vars(&[]));
    let load = move || {
        spawn_local(async move {
            let r = api("GET", &base(), None).await;
            if rows.is_disposed() {
                return;
            }
            match r {
                Ok(v) => {
                    let list = v.as_array().cloned().unwrap_or_default();
                    // The open message's row follows the list (Edited, Undo).
                    if let Some(key) = open
                        .try_with_untracked(|o| o.as_ref().map(|o| o["key"].clone()))
                        .flatten()
                    {
                        if let Some(r) = list.iter().find(|r| r["key"] == key) {
                            open.set(Some(r.clone()));
                        }
                    }
                    rows.set(Some(list))
                }
                Err(e) => msg.err(e),
            }
        })
    };
    load();
    let show = move |r: Value| {
        msg.clear();
        crate::msgparts::set_bot_vars(&vars_of(&r));
        let body = if r["message"].is_object() {
            r["message"].clone()
        } else {
            r["default"].clone()
        };
        orig.set(body.clone());
        cur.set(json!({"body": body, "added": r["buttons"]}));
        ed.reset();
        ed.edit.set((!(r["locked"] == true || !may(&r))).then_some(false));
        open.set(Some(r));
    };
    Effect::new(move |_| {
        let key = pending.get_value();
        if key.is_empty() {
            return;
        }
        let found = rows.with(|r| r.iter().flatten().find(|r| r["key"] == key.as_str()).cloned());
        if let Some(r) = found {
            pending.set_value(String::new());
            show(r);
        }
    });
    // `to`: a page to go to after the entry closes (the Tickets tab), else the list.
    let close = move |to: Option<String>| {
        crate::msgparts::set_bot_vars(&[]);
        open.set(None);
        if let Some(to) = to {
            let _ = web_sys::window().unwrap().location().set_hash(&to);
        }
    };
    let back = move |to: Option<String>| crate::guard::leave(move || close(to));
    // A security text's save, revert, undo or import asks for a 2FA code; after it the request goes again.
    let need = step_up();
    // `label` names the button that shows the working sign.
    let send = move |path: String, body: Value, label: String| {
        let url = format!("{}/{path}", base());
        move || {
            busy.set(true);
            doing.set(label.clone());
            let (url, body) = (url.clone(), body.clone());
            async move {
                let r = api("POST", &url, Some(body)).await;
                busy.try_set(false);
                r
            }
        }
    };
    let post = move |path: String, body: Value, done: &'static str| {
        run_resumable(send(path.clone(), body, path), msg, need, move |_| {
            msg.done(done);
            load();
        })
    };
    let save = move || {
        let Some(r) = open.get_untracked() else {
            return;
        };
        if ed.json_view.get_untracked() && ed.jerr.with_untracked(Option::is_some) {
            return msg.err("Not saved: the JSON has an error.");
        }
        let key = r["key"].as_str().unwrap_or("").to_string();
        let body = cur.with_untracked(|c| c["body"].clone());
        let make = send(key, json!({"message": body.clone()}), "save".into());
        run_resumable(make, msg, need, move |v| {
            orig.set(body);
            let gone: Vec<String> = v["dropped"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(|s| format!("{{{s}}}")))
                .collect();
            if gone.is_empty() {
                msg.done("Saved!")
            } else {
                msg.err(format!(
                    "Saved, but it no longer shows {}. Members will not see it.",
                    gone.join(", ")
                ))
            }
            load();
        })
    };
    crate::guard::guard(
        move || open.with(Option::is_some) && dirty.try_get().unwrap_or(false),
        move || busy.try_get().unwrap_or(false),
        move || {
            let gone = open.with(|o| cur.with(|c| o.as_ref().map(|r| missing(r, &c["body"])))).unwrap_or_default();
            let vars: Vec<String> = gone.iter().map(|v| format!("{{{v}}}")).collect();
            (!vars.is_empty()).then(|| format!("This message must show {} (press Insert in the editor).", vars.join(", ")))
        },
        save,
    );
    let revert = move |r: Value| {
        let key = r["key"].as_str().unwrap_or("").to_string();
        let name = r["name"].as_str().unwrap_or("").to_string();
        ask(
            Ask::new(format!("Revert “{name}” to the default?"))
                .text("Your edited version is replaced by Brim's default. You can undo this for 24 hours with Undo on the list.")
                .yes("Revert")
                .danger(),
            move |_| {
                let path = format!("{key}/revert");
                // Only after the revert went (a 2FA prompt may come first): the editor shows the
                // default, and unsaved changes go with the revert.
                run_resumable(send(path.clone(), json!({}), path), msg, need, move |_| {
                    if open.with_untracked(|o| o.as_ref().is_some_and(|o| o["key"] == key.as_str())) {
                        let d = open.with_untracked(|o| o.as_ref().map(|o| o["default"].clone())).unwrap_or_default();
                        orig.set(d.clone());
                        cur.set(json!({"body": d}));
                    }
                    msg.done("Reverted to the default.");
                    load();
                })
            },
        )
    };
    let revert_all = move || {
        ask(
            Ask::new("Revert all bot messages to the default?")
                .text("Every edited message gets Brim's default back, except the security texts: revert those one by one. You can undo each one for 24 hours with Undo on the list.")
                .yes("Revert all")
                .danger(),
            move |_| post("revert-all".into(), json!({}), "All bot messages are back to the default."),
        )
    };
    let undo = move |key: String| {
        post(
            format!("{key}/undo"),
            json!({}),
            "Your edited version is back.",
        )
    };
    let test = move || {
        let Some(r) = open.get_untracked() else {
            return;
        };
        let key = r["key"].as_str().unwrap_or("").to_string();
        let body = (r["locked"] != true).then(|| cur.with_untracked(|c| c["body"].clone()));
        busy.set(true);
        doing.set("test".into());
        spawn_local(async move {
            let res = api(
                "POST",
                &format!("{}/{key}/test", base()),
                Some(json!({"message": body})),
            )
            .await;
            if busy.try_set(false).is_some() {
                return;
            }
            match res {
                Ok(_) => msg.ok("Sent. Check your DMs."),
                Err(e) => msg.err(e),
            }
        })
    };
    // Message Kits: Admin exports the edited messages; the Owner imports after a preview.
    let kit_file = NodeRef::<leptos::html::Input>::new();
    let kit_plan = RwSignal::new(None::<(Value, Value)>);
    let export_kit = move || {
        busy.set(true);
        doing.set("kit".into());
        spawn_local(async move {
            let r = api("GET", &format!("{}/kit", base()), None).await;
            if busy.try_set(false).is_some() {
                return;
            }
            match r {
                Ok(v) if v["messages"].as_object().is_some_and(|m| !m.is_empty()) => download(
                    "brim-message-kit.json",
                    &format!(
                        "data:application/json;charset=utf-8,{}",
                        js_sys::encode_uri_component(
                            &serde_json::to_string_pretty(&v).unwrap_or_default()
                        )
                    ),
                ),
                Ok(_) => msg.err("No edited messages to export."),
                Err(e) => msg.err(e),
            }
        })
    };
    let pick_kit = move |e: leptos::ev::Event| {
        let input = event_target::<web_sys::HtmlInputElement>(&e);
        let Some(file) = input.files().and_then(|l| l.get(0)) else {
            return;
        };
        input.set_value("");
        if file.size() > 1_040_000.0 {
            return msg.err("This file is larger than 1 MB: it is not a message kit.");
        }
        busy.set(true);
        doing.set("kit".into());
        spawn_local(async move {
            let text = wasm_bindgen_futures::JsFuture::from(file.text())
                .await
                .ok()
                .and_then(|t| t.as_string())
                .unwrap_or_default();
            let Ok(kit) = serde_json::from_str::<Value>(&text) else {
                busy.try_set(false);
                return msg.err("This file is not valid JSON.");
            };
            let r = api(
                "POST",
                &format!("{}/import", base()),
                Some(json!({"kit": kit, "apply": false})),
            )
            .await;
            if busy.try_set(false).is_some() {
                return;
            }
            match r {
                Ok(p) => kit_plan.set(Some((kit, p))),
                Err(e) => msg.err(e),
            }
        })
    };
    let apply_kit = move || {
        let Some((kit, _)) = kit_plan.get_untracked() else {
            return;
        };
        let make = send("import".into(), json!({"kit": kit, "apply": true}), "kit-apply".into());
        run_resumable(make, msg, need, move |p| {
            kit_plan.set(None);
            let n = p["messages"]
                .as_array()
                .map_or(0, |m| m.iter().filter(|x| x["action"] != "skip").count());
            msg.done(format!("Imported {n} messages."));
            load();
        })
    };
    let badge = |r: &Value| {
        let security = (r["security"] == true).then(|| view! { <span class="chip">{crate::icons::icon("lock")}"Security"</span><Tip text="Trusted Admins and above can change it, with a 2FA code, and every change is in the dashboard log with the old and new version. Its key variables (who, what, the counts) must stay in it, as plain text." icon="info" /> });
        let state = if r["locked"] == true {
            view! { <span class="chip">{crate::icons::icon("lock")}"Locked"</span><Tip text="You can look at it, but nobody can change it, so it can never be hidden or blanked." icon="info" /> }.into_any()
        } else if r["message"].is_object() && !missing(r, &r["message"]).is_empty() {
            let var = format!("{{{}}}", r["required"][0].as_str().unwrap_or("text"));
            let lost = missing(r, &r["message"]).iter().map(|v| format!("{{{v}}}")).collect::<Vec<_>>().join(", ");
            let (chip, tip) = if r["in_text"] == true {
                (format!("Needs {var}"), format!("This edited version has no {var}, so Brim sends the default instead. Open it and press Insert {var}, or revert it."))
            } else if r["required"].as_array().is_some_and(|a| a.len() > 1 || a.first() != Some(&json!("text"))) {
                (format!("Needs {lost}"), format!("This edited version has no {lost}, so Brim sends the default instead. Open it and press Insert {lost}, or revert it."))
            } else {
                ("Needs Bot text".to_string(), "This edited version has no {text}, so Brim sends the default instead. Open it and press Insert Bot text, or revert it.".to_string())
            };
            view! { <span class="chip info">"Edited"</span><span class="chip bad">{chip}</span><Tip text=tip icon="info" /> }.into_any()
        } else if r["message"].is_object() {
            view! { <span class="chip info">"Edited"</span> }.into_any()
        } else {
            view! { <span class="chip">"Default"</span> }.into_any()
        };
        view! { {security}{state} }.into_any()
    };
    let list = move || {
        let Some(all) = rows.get() else {
            return view! { <p><Working /></p> }.into_any();
        };
        let edited = all.iter().filter(|r| r["message"].is_object()).count();
        // Revert all leaves the security texts (the server does too).
        let revertable = all
            .iter()
            .filter(|r| r["message"].is_object() && r["security"] != true)
            .count();
        let areas = by_area(&all);
        view! {
            <div class="intro"><p>"The messages Brim sends by itself. Edit one to change what members get; Revert puts Brim's default back."</p>
                <span class="count">{format!("{edited} edited")}</span></div>
            {move || admin().then(|| view! {
                <div class="card row">
                    <b>"Message Kits"</b>
                    <span class="muted">"Export your edited messages to a file, or import a kit from another server."</span>
                    <span class="sp"></span>
                    <button class="ghost" disabled=move || busy.get() on:click=move |_| export_kit()>{busy_label(move || busy.get() && doing.get() == "kit", "Export kit")}</button>
                    {owner().then(|| view! {
                        <button class="ghost" disabled=move || busy.get() title="Only the Owner and Extra Owners can import a kit" on:click=move |_| { if let Some(i) = kit_file.get() { i.click() } }>"Import kit"</button>
                        <input type="file" accept=".json,application/json" hidden=true aria-label="message kit file" node_ref=kit_file on:change=pick_kit />
                    })}
                </div>
            })}
            {move || kit_plan.get().map(|(_, p)| {
                let items = p["messages"].as_array().cloned().unwrap_or_default();
                let count = |a: &str| items.iter().filter(|x| x["action"] == a).count();
                let (n_apply, n_replace, n_skip) = (count("apply"), count("replace"), count("skip"));
                let skipped: Vec<_> = items.iter().filter(|x| x["action"] == "skip").cloned().collect();
                view! { <div class="card">
                    <b>"Import kit"</b>
                    <p>{format!("Applies {} new messages and replaces {} edited ones. Skips {}. A replaced message can be brought back with Undo for 24 hours.", n_apply, n_replace, n_skip)}</p>
                    {skipped.into_iter().map(|x| view! { <p class="muted small">{format!("Skipped “{}”: {}", x["name"].as_str().unwrap_or(""), x["reason"].as_str().unwrap_or(""))}</p> }).collect_view()}
                    <div class="row"><button class="ghost" on:click=move |_| kit_plan.set(None)>"Cancel"</button>
                        <button disabled=move || busy.get() || n_apply + n_replace == 0 on:click=move |_| apply_kit()>{busy_label(move || busy.get() && doing.get() == "kit-apply", "Import")}</button></div>
                </div> }
            })}
            {move || (admin() && revertable > 0).then(|| view! {
                <div class="listtop"><span class="sp"></span><button class="danger" disabled=move || busy.get() on:click=move |_| revert_all()>{busy_label(move || busy.get() && doing.get() == "revert-all", "Revert all")}</button></div>
            })}
            <div class="listtop"><input type="search" placeholder="Search messages" aria-label="search messages" prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) /></div>
            {move || {
                let needle = query.get().trim().to_lowercase();
                let hit = |r: &Value| needle.is_empty() || [r["name"].as_str().unwrap_or(""), r["group"].as_str().unwrap_or(""), r["area"].as_str().unwrap_or(""), r["sub"].as_str().unwrap_or(""), &r["message"].to_string(), &r["default"].to_string()].iter().any(|t| t.to_lowercase().contains(&needle));
                let shown: Areas = areas.iter().map(|(a, subs)| (a.clone(), subs.iter().map(|(g, v)| (g.clone(), v.iter().filter(|r| hit(r)).cloned().collect::<Vec<_>>())).filter(|(_, v)| !v.is_empty()).collect::<Vec<_>>())).filter(|(_, v)| !v.is_empty()).collect();
                if shown.is_empty() {
                    return view! { <p class="card empty muted">"No messages found."</p> }.into_any();
                }
                let row = move |r: Value| {
                        let color = format!("#{:06x}", r["message"]["embeds"][0]["color"].as_u64().or(r["default"]["embeds"][0]["color"].as_u64()).unwrap_or(0x4e5058));
                        let name = r["name"].as_str().unwrap_or("").to_string();
                        let key = r["key"].as_str().unwrap_or("").to_string();
                        let edited = r["message"].is_object() && r["locked"] != true;
                        let undo_ok = r["undo_until"].as_i64().is_some_and(|t| t > now()) && !edited;
                        let (r1, r2, r3) = (r.clone(), r.clone(), r.clone());
                        let can_edit = r["locked"] != true && may(&r);
                        view! {
                            <div class="card mrow">
                                <span class="sw8" style=format!("background:{color}")></span>
                                <button class="nm" on:click=move |_| show(r1.clone())><b>{name}</b></button>
                                <span class="states">{badge(&r)}</span>
                                <span class="acts">
                                    {(may(&r) && undo_ok).then(|| { let k = key.clone(); view! { <button class="ghost" disabled=move || busy.get() title="Bring back your edited version (for 24 hours after the revert)" on:click=move |_| undo(k.clone())>{crate::icons::icon("refresh")}"Undo"</button> } })}
                                    <button class="ghost" title=if can_edit { "Edit" } else { "View" } aria-label=if can_edit { "Edit" } else { "View" } on:click=move |_| show(r2.clone())>{crate::icons::icon(if can_edit { "edit" } else { "eye" })}</button>
                                    {(may(&r) && edited).then(|| view! { <button class="ghost" title="Revert to default" aria-label="Revert to default" disabled=move || busy.get() on:click=move |_| revert(r3.clone())>{crate::icons::icon("revert")}</button> })}
                                </span>
                            </div>
                        }
                };
                // A card row that opens and closes its part, like the hub rows on the Messages page.
                let head = move |id: String, label: String, n: usize, line: Option<&'static str>, ico: Option<&'static str>| {
                    let id = StoredValue::new(id);
                    let open_now = move || id.with_value(|i| is_open(i));
                    view! { <button class="card hub" class:sub=ico.is_none() class:on=open_now aria-expanded=move || open_now().to_string() on:click=move |_| id.with_value(|i| flip(i))>
                        {ico.map(|i| view! { <span class="ico">{crate::icons::icon(i)}</span> })}
                        <span class="txt"><b>{label}</b>{line.map(|l| view! { <span class="muted">{l}</span> })}</span>
                        <span class="chip">{n.to_string()}</span>
                        <span class="chev">{crate::icons::icon("chevron_right")}</span>
                    </button> }
                };
                shown.into_iter().map(|(area, subs)| {
                    let n: usize = subs.iter().map(|s| s.1.len()).sum();
                    let (ico, line) = area_info(&area);
                    let a2 = area.clone();
                    let one = subs.len() == 1;
                    let subs = StoredValue::new(subs);
                    view! {
                        <div class="hubarea">
                        {head(area.clone(), area.clone(), n, Some(line), Some(ico))}
                        <Show when=move || is_open(&a2)>
                        <div class="hubsubs">
                        {let area = area.clone(); subs.get_value().into_iter().map(move |(sub, items)| {
                            let id = format!("{area}/{sub}");
                            let id1 = id.clone();
                            let items = StoredValue::new(items);
                            let list = move || view! { <div class="mlist">{items.get_value().into_iter().map(row).collect_view()}</div> };
                            if one {
                                list().into_any()
                            } else {
                                view! {
                                    {head(id, sub, items.with_value(Vec::len), None, None)}
                                    <Show when=move || is_open(&id1)>{list()}</Show>
                                }.into_any()
                            }
                        }).collect_view()}
                        </div>
                        </Show>
                        </div>
                    }
                }).collect_view().into_any()
            }}
        }.into_any()
    };
    let editor = move || {
        let Some(r) = open.get() else {
            return ().into_any();
        };
        let locked = r["locked"] == true;
        let can_edit = !locked && may(&r);
        let (r1, r2) = (r.clone(), r.clone());
        let rkey = format!("{}/revert", r["key"].as_str().unwrap_or(""));
        let warn = move || {
            let gone = cur.with(|c| dropped(&r1, &c["body"]));
            (can_edit && !gone.is_empty()).then(|| {
                let list: Vec<String> = gone.iter().map(|v| format!("{{{v}}}")).collect();
                view! { <p class="err">{format!("This message no longer shows {}. Members will not see it. You can still save.", list.join(", "))}</p> }
            })
        };
        let what = r["what"].as_str().unwrap_or("").to_string();
        let needs = !what.is_empty() && can_edit;
        let r3 = r.clone();
        let bot_text = move || {
            needs.then(|| {
                let gone = cur.with(|c| !missing(&r3, &c["body"]).is_empty());
                let what = what.clone();
                let r4 = r3.clone();
                let all: Vec<String> = r3["required"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|v| format!("{{{v}}}")).collect();
                let var = all.join(" and ");
                let key = if all.len() > 1 { "Key variables" } else { "Key variable" };
                let (place, button) = if r3["in_text"] == true {
                    ("in the message text or the first embed", format!("Insert {var}"))
                } else if all.len() > 1 {
                    ("in the first embed", format!("Insert {var}"))
                } else {
                    ("in the first embed", "Insert Bot text".to_string())
                };
                let place = if r3["plain_facts"] == true { format!("{place}, as plain text (not in a spoiler, a link or <...>)") } else { place.to_string() };
                view! {
                    <div class="note req-text" class:bad=gone>
                        <b>{if gone { format!("{key} {var} {}: this message cannot be saved without {}.", if all.len() > 1 { "are missing" } else { "is missing" }, if all.len() > 1 { "them" } else { "it" }) } else { format!("This message must use the {} {var}.", key.to_lowercase()) }}</b>
                        <span class="muted">{format!("What is {var}? Brim puts {what} there. Write anything you like around it, but keep {var} {place}.")}</span>
                        {gone.then(|| view! { <button class="ghost" on:click=move |_| { cur.update(|c| insert_required(&r4, &mut c["body"])); ed.reset() }>{button}</button> })}
                    </div>
                }
            })
        };
        let edited = r["message"].is_object() && !locked;
        view! {
            <div class="mbhead">
                <button class="ghost back" on:click=move |_| back(None)>"← Default messages"</button>
                {from_tickets.then(|| view! { <button class="ghost back" on:click=move |_| back(Some(format!("/{}/tickets", g.get_value())))>"← Back to Tickets"</button> })}
                <b class="name">{r["name"].as_str().unwrap_or("").to_string()}</b>
                <span class="states">{badge(&r)}</span>
                <span class="sp"></span>
                {admin().then(|| view! { <button class="ghost" disabled=move || busy.get() title="Brim DMs you this message with example values" on:click=move |_| test()>{crate::icons::icon("send")}"Send test to me"</button> })}
                {(may(&r) && edited).then(|| view! { <button class="danger" disabled=move || busy.get() on:click=move |_| revert(r2.clone())>{busy_label(move || busy.get() && doing.get() == rkey, "Revert to default")}</button> })}
            </div>
            {locked.then(|| view! { <p class="muted">{crate::icons::icon("lock")}" It is shown here, but it cannot be changed."</p> })}
            {(!locked && !can_edit).then(|| view! { <p class="muted">{if r["security"] == true { "Trusted Admins and above can change security texts, with a 2FA code." } else { "Only the Owner, Extra Owners and Trusted Admins can change bot messages." }}</p> })}
            {move || (ed.edit.get() == Some(true)).then(|| view! { <p class="muted small">"Type { or press { } for the values Brim fills in."</p> })}
            {bot_text}
            {warn}
            {crate::embeds::split(ed.edit, message_tab, preview_tab)}
            {pop_view}
            {crate::msgparts::sub_view(ed.parts)}
            <Show when=move || can_edit && dirty.get()>
                <div class="bar"><b>"Unsaved changes"</b>
                    <button class="ghost" on:click=move |_| { let o = orig.get_untracked(); cur.update(|c| c["body"] = o); ed.reset() }>"Cancel"</button>
                    <button disabled=move || busy.get() on:click=move |_| save()>{busy_label(move || busy.get() && doing.get() == "save", "Save")}</button></div>
            </Show>
        }.into_any()
    };
    view! {
        {move || if open.with(Option::is_some) { editor() } else { list() }}
        <Toast msg />
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feed_link_may_be_the_title_link() {
        // The GitHub default keeps {link} only as the title's link: no error before any edit.
        let row = json!({"key": "feed.github.push", "required": ["link"]});
        let default = json!({"embeds": [{"title": "{headline}", "url": "{link}"}]});
        assert!(missing(&row, &default).is_empty());
        // Other entries do not count a link.
        let row = json!({"key": "log.x", "required": ["link"]});
        assert_eq!(missing(&row, &default), ["link"]);
    }

    #[test]
    fn missing_reads_the_first_embed_only() {
        let row = json!({"required": ["text"]});
        assert!(missing(
            &row,
            &json!({"embeds": [{"fields": [{"name": "a", "value": "{text}"}]}]})
        )
        .is_empty());
        assert_eq!(
            missing(
                &row,
                &json!({"embeds": [{"description": "x"}, {"description": "{text}"}]})
            ),
            ["text"]
        );
        assert_eq!(
            missing(
                &row,
                &json!({"embeds": [{"image": {"url": "https://a/{text}"}}]})
            ),
            ["text"]
        );
        assert!(missing(&json!({}), &json!({})).is_empty());
        assert_eq!(missing(&row, &json!({"embeds": [{"title": "{te", "description": "xt}"}]})), ["text"]);
        let mut b = json!({"embeds": [{"description": "Hi "}]});
        insert_required(&row, &mut b);
        assert_eq!(b["embeds"][0]["description"], "Hi\n{text}");
        let mut b = json!({"content": ""});
        insert_required(&row, &mut b);
        assert!(missing(&row, &b).is_empty());
        // A security log needs its title too: it goes in the empty title.
        let sec = json!({"required": ["title", "text"]});
        let mut b = json!({"embeds": [{"description": "Alert"}]});
        insert_required(&sec, &mut b);
        assert_eq!(b["embeds"][0]["title"], "{title}");
        assert_eq!(b["embeds"][0]["description"], "Alert\n{text}");
        assert!(missing(&sec, &b).is_empty());
        // A text-only entry (a feed post) keeps its placeholder in the message text.
        let feed = json!({"required": ["link"], "in_text": true});
        assert!(missing(&feed, &json!({"content": "New: {link}"})).is_empty());
        assert_eq!(missing(&row, &json!({"content": "{text}"})), ["text"]);
        let mut b = json!({"content": "New video "});
        insert_required(&feed, &mut b);
        assert_eq!(b["content"], "New video\n{link}");
    }

    #[test]
    fn dropped_lists_missing_important_variables() {
        let row = json!({"important": ["reason"]});
        assert!(dropped(&row, &json!({"embeds": [{"description": "Why: {reason}"}]})).is_empty());
        assert_eq!(
            dropped(&row, &json!({"embeds": [{"description": "Why"}]})),
            ["reason"]
        );
    }
}

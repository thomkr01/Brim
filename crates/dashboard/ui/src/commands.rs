//! Public Commands page: every bot command and message variable, as the bot published them at startup.
use crate::common::{AddButton, 
    api, busy_label, dialog_keys, focus_on_open, limit, room, run_change, step_up, Access, Count, Head, Msg, Toast,
};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// A setting as the text of its input; nothing for an unset one.
fn show(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(t) => t.clone(),
        v => v.to_string(),
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().map(s).collect()
}

/// Whether the list `v` holds the word.
fn has(v: &Value, word: &str) -> bool {
    v.as_array().is_some_and(|l| l.iter().any(|x| x == word))
}

/// Whether command `c` matches the lowercase filter `q` by name, alias, description or usage.
fn hit(c: &Value, q: &str) -> bool {
    let all = format!(
        "{} {} {} {}",
        s(&c["name"]),
        c["aliases"],
        s(&c["what"]),
        s(&c["usage"])
    )
    .to_lowercase();
    q.split_whitespace().all(|w| all.contains(w))
}

/// A per-command setting's value when the server never changed it (`core::cmdcfg::CmdCfg`).
fn default(key: &str) -> Value {
    match key {
        "prefix" | "slash" => json!(true),
        "aliases" => json!([]),
        "only" => json!("permit"),
        "delete_command" | "reply" => json!(false),
        "cooldown_secs" => json!(0),
        _ => Value::Null,
    }
}

/// One setting of a command's entry, or its default.
fn val(entry: &Value, key: &str) -> Value {
    match &entry[key] {
        Value::Null => default(key),
        v => v.clone(),
    }
}

/// The entry without the settings left at their default; `None` when nothing is left, so an
/// untouched command leaves no entry behind.
fn tidy(entry: &Value) -> Option<Value> {
    let kept: serde_json::Map<String, Value> = entry
        .as_object()?
        .iter()
        .filter(|(k, v)| **v != default(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    (!kept.is_empty()).then_some(Value::Object(kept))
}

/// The section with `name` set to `entry` (tidied; a default entry is removed).
fn with_entry(mut all: Value, name: &str, entry: &Value) -> Value {
    if !all.is_object() {
        all = json!({});
    }
    if let Some(o) = all.as_object_mut() {
        match tidy(entry) {
            Some(e) => o.insert(name.to_string(), e),
            None => o.remove(name),
        };
    }
    all
}

/// Settings a safety command never takes (it stays on, for everyone its permit allows, at once).
const LOCKED_ON_SAFETY: [&str; 4] = ["prefix", "slash", "only", "cooldown_secs"];

/// The settings "Edit several" can change, with their names. Aliases are per command.
const SEVERAL: [(&str, &str); 7] = [
    ("prefix", "Prefix command"),
    ("slash", "Slash command"),
    ("only", "Who may use it"),
    ("delete_command", "Delete the command message"),
    ("delete_reply_secs", "Delete the answer after"),
    ("reply", "Answer as a reply"),
    ("cooldown_secs", "Cooldown"),
];

const ONLY: [(&str, &str); 4] = [
    ("permit", "Everyone the permit allows"),
    ("ta", "Trusted Admins, Extra Owners and the Owner"),
    ("eo", "Extra Owners and the Owner"),
    ("owner", "Only the Owner"),
];

/// A typed number of seconds as the setting stores it; an emptied box is the default.
fn secs(key: &str, typed: &str) -> Value {
    match typed.trim().parse::<u32>() {
        Ok(n) => json!(n),
        Err(_) => default(key),
    }
}

#[component]
pub fn Commands() -> impl IntoView {
    let data = RwSignal::new(Value::Null);
    let (q, set_q) = signal(String::new());
    let access = use_context::<Access>();
    let guilds = use_context::<ReadSignal<Vec<Value>>>();
    let denied = move |name: &str| {
        access.is_some_and(|a| {
            a.pages.with(|p| {
                p["commands_denied"]
                    .as_array()
                    .is_some_and(|d| d.iter().any(|n| n == name))
            })
        })
    };
    let failed = RwSignal::new(false);
    spawn_local(async move {
        match api("GET", "/api/commands", None).await {
            Ok(v) => data.set(v),
            Err(_) => failed.set(true),
        }
    });
    // Per-command settings (review): for the server last opened, and only for its owners.
    // Memos, so the access answer that comes again on every page change rebuilds nothing.
    let guild = Memo::new(move |_| access.and_then(|a| a.pages.with(|p| p["guild"].as_str().map(str::to_string))));
    let owner = Memo::new(move |_| access.is_some_and(|a| a.pages.with(|p| p["level"] == "owner")));
    let (msg, need) = (Msg::default(), step_up());
    // The stored `commands` section and its version, set together; `None` until it loaded.
    let settings = RwSignal::new(None::<Value>);
    let version = RwSignal::new(0i64);
    let busy = RwSignal::new(false);
    let picked = RwSignal::new(Vec::<String>::new());
    // The command whose settings are open: (name, safety command, has a slash form), and its draft.
    let open = RwSignal::new(None::<(String, bool, bool)>);
    let draft = RwSignal::new(json!({}));
    let alias = RwSignal::new(String::new());
    Effect::new(move |_| {
        // Another server's settings and version never stay on screen while this one loads.
        settings.set(None);
        let (true, Some(g)) = (owner.get(), guild.get()) else {
            return;
        };
        spawn_local(async move {
            let r = api("GET", &format!("/api/guilds/{g}/config"), None).await;
            if settings.is_disposed() || guild.get_untracked().as_deref() != Some(g.as_str()) {
                return;
            }
            match r {
                Ok(v) => {
                    version.set(v["version"].as_i64().unwrap_or(0));
                    let stored = &v["config"]["commands"];
                    settings.set(Some(if stored.is_object() { stored.clone() } else { json!({}) }));
                }
                Err(e) => msg.err(format!("The command settings did not load, so they can not be changed now: {e}")),
            }
        });
    });
    let can_edit = Memo::new(move |_| owner.get() && settings.with(Option::is_some));
    let server = move || {
        let g = guild.get().unwrap_or_default();
        guilds
            .and_then(|l| l.get().into_iter().find(|x| x["id"] == g.as_str()))
            .and_then(|x| x["name"].as_str().map(str::to_string))
            .unwrap_or_else(|| "your last server".into())
    };
    // One save at a time; the stored copy follows the server's answer only.
    let put = move |next: Value, saved: String| {
        let Some(g) = guild.get_untracked() else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let v = version.get_untracked();
        run_change(
            async move {
                let body = json!({"expected_version": v, "value": next.clone()});
                let r = api("PUT", &format!("/api/guilds/{g}/config/commands"), Some(body)).await;
                busy.try_set(false);
                if r.is_ok() {
                    settings.try_set(Some(next));
                    open.try_set(None);
                    picked.try_set(vec![]);
                }
                r
            },
            msg,
            need,
            move |v| {
                version.set(v["version"].as_i64().unwrap_or(0));
                msg.done(if saved.is_empty() { "Saved!".into() } else { saved });
            },
        );
    };
    let start = move |name: String, safety: bool, slash: bool| {
        draft.set(settings.with_untracked(|s| s.as_ref().map_or(Value::Null, |s| s[name.as_str()].clone())));
        alias.set(String::new());
        open.set(Some((name, safety, slash)));
    };
    let set = move |key: &'static str, v: Value| {
        draft.update(|d| {
            if !d.is_object() {
                *d = json!({});
            }
            d[key] = v;
        })
    };
    let save_one = move |_| {
        let Some((name, ..)) = open.get_untracked() else {
            return;
        };
        let all = settings.get_untracked().unwrap_or_default();
        put(with_entry(all, &name, &draft.get_untracked()), String::new());
    };
    let add_alias = move || {
        let a = alias.get_untracked().trim().to_lowercase();
        let mut list = strs(&val(&draft.get_untracked(), "aliases"));
        if a.is_empty() || list.contains(&a) || !room(list.len(), "command_aliases") {
            return;
        }
        list.push(a);
        set("aliases", json!(list));
        alias.set(String::new());
    };
    // Edit several: one setting for every ticked command.
    let (several, several_val) = (RwSignal::new("prefix".to_string()), RwSignal::new("on".to_string()));
    let apply_several = move |_| {
        let (key, typed) = (several.get_untracked(), several_val.get_untracked());
        let Some(key) = SEVERAL.iter().map(|s| s.0).find(|k| *k == key) else {
            return;
        };
        let v = match key {
            "only" => json!(typed),
            "cooldown_secs" | "delete_reply_secs" => secs(key, &typed),
            _ => json!(typed == "on"),
        };
        let mut all = settings.get_untracked().unwrap_or_default();
        let (mut done, mut skipped) = (0, vec![]);
        for name in picked.get_untracked() {
            let c = data.with_untracked(|d| d["commands"].as_array().and_then(|l| l.iter().find(|c| c["name"] == name.as_str()).cloned()));
            let Some(c) = c else {
                continue;
            };
            let safety = data.with_untracked(|d| has(&d["safety"], &name));
            let no_slash = key == "slash" && c["slash"].as_array().is_none_or(Vec::is_empty);
            if (safety && LOCKED_ON_SAFETY.contains(&key)) || no_slash {
                skipped.push(name);
                continue;
            }
            let mut entry = all[name.as_str()].clone();
            if !entry.is_object() {
                entry = json!({});
            }
            entry[key] = v.clone();
            all = with_entry(all, &name, &entry);
            done += 1;
        }
        if done == 0 {
            return msg.err(format!("This setting does not apply to the ticked commands ({}).", skipped.join(", ")));
        }
        let note = if skipped.is_empty() { String::new() } else { format!(" Not changed (the setting does not apply): {}.", skipped.join(", ")) };
        put(all, format!("Changed {done} commands.{note}"));
    };
    let list = move || {
        let d = data.get();
        let pfx = s(&d["prefix"]);
        let q = q.get().to_lowercase();
        let edit = can_edit.get();
        let cmds: Vec<Value> = d["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| hit(c, &q))
            .cloned()
            .collect();
        let mut cats: Vec<String> = vec![];
        for c in &cmds {
            let k = s(&c["category"]);
            if !cats.contains(&k) {
                cats.push(k)
            }
        }
        // Before the first answer nothing is known: not "the bot has not published yet".
        if d.is_null() {
            if failed.get() {
                return view! { <p class="err">"The commands could not be loaded. Reload the page to try again."</p> }.into_any();
            }
            return view! { <p><crate::common::Working /></p> }.into_any();
        }
        if d["commands"].is_null() {
            return view! { <p class="muted">"The bot has not published its commands yet: start it once."</p> }.into_any();
        }
        if cmds.is_empty() {
            return view! { <p class="muted">"No command matches."</p> }.into_any();
        }
        cats.into_iter().map(|cat| {
            let rows = cmds.iter().filter(|c| s(&c["category"]) == cat).map(|c| {
                let p = pfx.clone();
                let line = |label: &'static str, v: String| (!v.is_empty()).then(|| view! { <p><b>{label}</b>" "<code>{v}</code></p> });
                let aliases = c["aliases"].as_array().map(|a| a.iter().map(s).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                let has_slash = c["slash"].as_array().is_some_and(|a| !a.is_empty());
                let slash = c["slash"].as_array().map(|a| a.iter().map(s).collect::<Vec<_>>().join("  ·  ")).unwrap_or_default();
                let name = s(&c["name"]);
                let no = denied(&name);
                let safety = has(&d["safety"], &name);
                let (n1, n2, n3, n4, n5) = (name.clone(), name.clone(), name.clone(), name.clone(), name.clone());
                // The row's own settings, read in small closures so a save rebuilds no row.
                let entry = move |key: &str| settings.with(|s| s.as_ref().map_or(Value::Null, |s| val(&s[n4.as_str()], key)));
                let entry2 = entry.clone();
                view! {
                    <details class="card" class:na=no>
                        <summary>
                            {edit.then(|| view! { <input type="checkbox" aria-label=format!("select {name}") prop:checked=move || picked.with(|l| l.contains(&n1)) on:click=move |e| e.stop_propagation() on:change=move |e| picked.update(|l| { l.retain(|x| *x != n2); if event_target_checked(&e) { l.push(n2.clone()) } }) /> })}
                            <code>{format!("{p}{name}")}</code><span>{s(&c["what"])}</span>
                            {move || (entry("prefix") == false).then(|| view! { <span class="chip warn" title="The prefix form is turned off on this server">"Prefix off"</span> })}
                            {move || (has_slash && entry2("slash") == false).then(|| view! { <span class="chip warn" title="The slash form is turned off on this server">"Slash off"</span> })}
                            <span class="chip">{s(&c["who"])}</span>{no.then(|| view! { <span class="chip warn" title="Your permit on this server does not include this command">"No access"</span> })}
                            {edit.then(|| view! { <button class="ghost kebab" aria-label=format!("settings for {n5}") title="Settings on this server" on:click=move |e| { e.prevent_default(); e.stop_propagation(); start(n3.clone(), safety, has_slash) }>"⋮"</button> })}
                        </summary>
                        <div class="cmd-body">
                        {(!s(&c["who"]).is_empty()).then(|| view! { <p><b>"Who may use it"</b>" "{s(&c["who"])}</p> })}
                        {line("Usage", format!("{p}{}", s(&c["usage"])))}
                        <ul>{c["args"].as_array().into_iter().flatten().map(|a| view! { <li><code>{s(&a[0])}</code>" "{s(&a[1])}</li> }).collect_view()}</ul>
                        {line("Example", format!("{p}{}", s(&c["example"])))}
                        {(!aliases.is_empty()).then(|| view! { <p><b>"Aliases"</b>" "{aliases}</p> })}
                        {line("Slash", slash)}
                        </div>
                    </details>
                }
            }).collect_view();
            view! { <h3>{cat}</h3>{rows} }
        }).collect_view().into_any()
    };
    let apps = move || {
        data.get()["apps"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|a| view! { <li><code>{s(&a["name"])}</code>" "{s(&a["what"])}</li> })
            .collect_view()
    };
    let vars = move || {
        data.get()["variables"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|v| view! { <li><code>{s(&v[0])}</code>" "{s(&v[1])}</li> })
            .collect_view()
    };
    // The value control of "Edit several" for the chosen setting.
    let several_value = move || match several.get().as_str() {
        "only" => view! { <select aria-label="new value" prop:value=move || several_val.get() on:change=move |e| several_val.set(event_target_value(&e))>
            {ONLY.iter().map(|(k, t)| view! { <option value=*k>{*t}</option> }).collect_view()}</select> }.into_any(),
        k @ ("cooldown_secs" | "delete_reply_secs") => {
            let hint = if k == "cooldown_secs" { "Seconds, 0 for none" } else { "Seconds, empty for the server's setting" };
            view! { <input type="number" min="0" aria-label="new value" placeholder=hint prop:value=move || several_val.get() on:input=move |e| several_val.set(event_target_value(&e)) /> }.into_any()
        }
        _ => view! { <select aria-label="new value" prop:value=move || several_val.get() on:change=move |e| several_val.set(event_target_value(&e))>
            <option value="on">"On"</option><option value="off">"Off"</option></select> }.into_any(),
    };
    let dialog = move || open.get().map(|(name, safety, has_slash)| {
        let r = NodeRef::<leptos::html::Div>::new();
        focus_on_open(r);
        let pfx = data.with_untracked(|d| s(&d["prefix"]));
        let get = move |key: &'static str| draft.with(|d| val(d, key));
        let tog = move |key: &'static str, label: &'static str, help: String, locked: bool| view! {
            <div class="row tog" role="group" aria-label=label>
                <label>{label}</label>
                <input type="checkbox" class="sw" aria-label=label disabled=locked prop:checked=move || get(key) == true on:change=move |e| set(key, json!(event_target_checked(&e))) />
                <span class="muted">{help}</span>
            </div>
        };
        let most = |key: &str, fallback: usize| limit(key).unwrap_or(fallback).to_string();
        let count = Signal::derive(move || strs(&get("aliases")).len());
        view! { <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label=format!("Settings for {name}") on:keydown=move |e| dialog_keys(&e, move || open.set(None))>
            <div class="card cmdset">
                <h2>"Settings for "<code>{format!("{pfx}{name}")}</code></h2>
                <p class="muted">{format!("On {} only. The command's permit still decides who may use it.", server())}</p>
                {safety.then(|| view! { <p class="muted"><b>"Safety command. "</b>"Staff need it to end a lock or a quarantine, so it can not be turned off, narrowed or given a cooldown."</p> })}
                {tog("prefix", "Prefix command", format!("Off: {pfx}{name} does not run on this server."), safety)}
                {has_slash.then(|| tog("slash", "Slash command", format!("Off: /{name} and its right-click entry do not run on this server."), safety))}
                <div class="row">
                    <label>"Aliases"</label>
                    <span class="muted fullrow">{format!("Extra words for the prefix form on this server, like {pfx}hammer. Lower-case letters, digits, - and _.")}</span>
                    {move || strs(&get("aliases")).into_iter().map(|a| {
                        let a2 = a.clone();
                        view! { <span class="chip">{a.clone()}" "<button class="link" aria-label=format!("remove alias {a}") on:click=move |_| { let mut l = strs(&get("aliases")); l.retain(|x| *x != a2); set("aliases", json!(l)) }>"×"</button></span> }
                    }).collect_view()}
                    <input aria-label="new alias" placeholder="New alias" maxlength="32" prop:value=move || alias.get() on:input=move |e| alias.set(event_target_value(&e)) on:keydown=move |e| if crate::common::is_enter(&e) { e.prevent_default(); add_alias() } />
                    <AddButton label="Add alias" attr:disabled=move || !room(count.get(), "command_aliases") on:click=move |_| add_alias() />
                    <Count n=count key="command_aliases" />
                </div>
                <div class="row">
                    <label>"Who may use it"</label>
                    <select aria-label="who may use it" disabled=safety prop:value=move || show(&get("only")) on:change=move |e| set("only", json!(event_target_value(&e)))>
                        {ONLY.iter().map(|(k, t)| view! { <option value=*k selected=move || get("only") == *k>{*t}</option> }).collect_view()}
                    </select>
                    <span class="muted fullrow">"This only narrows it. Nobody gets a command their permit does not include."</span>
                </div>
                {tog("delete_command", "Delete the command message", "Prefix form: Brim deletes the typed command once it ran.".into(), false)}
                <div class="row">
                    <label>"Delete the answer after"</label>
                    <input type="number" min="1" max=most("command_delete_secs", 300) aria-label="delete the answer after, seconds" placeholder="Server setting" prop:value=move || show(&get("delete_reply_secs")) on:input=move |e| set("delete_reply_secs", secs("delete_reply_secs", &event_target_value(&e))) />
                    <span class="muted fullrow">{format!("Prefix form: seconds until Brim deletes its answer, 1 to {}. Empty: the server's own setting.", most("command_delete_secs", 300))}</span>
                </div>
                {tog("reply", "Answer as a reply", "Prefix form: the answer is a Discord reply to the command message.".into(), false)}
                <div class="row">
                    <label>"Cooldown"</label>
                    <input type="number" min="0" max=most("command_cooldown_secs", 3600) aria-label="cooldown, seconds" disabled=safety prop:value=move || show(&get("cooldown_secs")) on:input=move |e| set("cooldown_secs", secs("cooldown_secs", &event_target_value(&e))) />
                    <span class="muted fullrow">{format!("Seconds one member waits between two uses, at most {}. 0: no cooldown. The Owner, Extra Owners and Trusted Admins never wait.", most("command_cooldown_secs", 3600))}</span>
                </div>
                <div class="acts">
                    <button class="ghost" on:click=move |_| open.set(None)>"Cancel"</button>
                    <button disabled=move || busy.get() on:click=save_one>{busy_label(move || busy.get(), "Save")}</button>
                </div>
            </div>
        </div> }
    });
    view! {
        <Head guild=String::new() page="Brim".into() big="Commands".into() />
        <div class="stack cmds">
            {move || guild.get().map(|_| view! { <p class="muted">{format!("Commands your permit does not include on {} are greyed out.", server())}</p> })}
            <Show when=move || can_edit.get()>
                <p class="muted">{move || format!("The ⋮ on a command opens its settings for {}. Tick commands to change one setting for all of them.", server())}</p>
            </Show>
            <input placeholder="Filter: [name, alias or word]" aria-label="filter commands" prop:value=q on:input=move |e| { picked.set(vec![]); set_q.set(event_target_value(&e)) } />
            <p class="muted">{move || format!("Default prefix {}. Add -h after any command for its help in Discord.", s(&data.get()["prefix"]))}</p>
            <Show when=move || !picked.with(Vec::is_empty)>
                <div class="row cmdbulk" role="group" aria-label="edit several commands">
                    <span class="muted">{move || format!("{} selected", picked.with(Vec::len))}</span>
                    <select aria-label="setting to change" prop:value=move || several.get() on:change=move |e| { let k = event_target_value(&e); several_val.set(match k.as_str() { "only" => "permit", "cooldown_secs" => "0", "delete_reply_secs" => "", _ => "on" }.into()); several.set(k) }>
                        {SEVERAL.iter().map(|(k, t)| view! { <option value=*k>{*t}</option> }).collect_view()}
                    </select>
                    {several_value}
                    <button disabled=move || busy.get() on:click=apply_several>{busy_label(move || busy.get(), "Apply")}</button>
                    <button class="ghost" on:click=move |_| picked.set(vec![])>"Clear"</button>
                </div>
            </Show>
            {list}
            <h3>"Slash-only commands and apps"</h3>
            <div class="card"><ul>{apps}</ul></div>
            <h3>"Message variables"</h3>
            <div class="card"><p class="muted">{move || format!("{}:", s(&data.get()["variables_in"]))}</p><ul>{vars}</ul></div>
        </div>
        {dialog}
        <Toast msg />
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn filter_matches_every_word() {
        let c = serde_json::json!({"name": "ban", "aliases": ["b"], "what": "Ban, softban with ?s", "usage": "ban MEMBERS"});
        assert!(super::hit(&c, "ban soft"));
        assert!(!super::hit(&c, "kick"));
    }

    #[test]
    fn a_default_setting_leaves_no_entry() {
        use serde_json::json;
        let all = super::with_entry(json!({}), "ban", &json!({"prefix": false, "reply": false, "aliases": [], "cooldown_secs": 0}));
        assert_eq!(all, json!({"ban": {"prefix": false}}));
        // Back to the default: the entry goes, and the other commands stay.
        let all = super::with_entry(json!({"ban": {"prefix": false}, "kick": {"only": "ta"}}), "ban", &json!({"prefix": true, "delete_reply_secs": null}));
        assert_eq!(all, json!({"kick": {"only": "ta"}}));
        assert_eq!(super::secs("cooldown_secs", ""), json!(0));
        assert_eq!(super::secs("delete_reply_secs", " "), serde_json::Value::Null);
        assert_eq!(super::secs("delete_reply_secs", "15"), json!(15));
    }
}

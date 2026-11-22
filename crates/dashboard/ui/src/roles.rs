//! Channel Setup's Roles and Server tabs (Layout Guard plan 3.3, 3.4). They edit the same draft as
//! the Channels tab and go out with the same Push. The server holds every rule; the page only shows
//! what it may change and why the rest is locked.
use crate::common::*;
use crate::icons::icon;
use leptos::prelude::*;
use serde_json::{json, Value};

/// (bit, name): what the editor sets. The server takes no other bit.
const BASICS: [(u32, &str); 11] = [
    (10, "View Channels"),
    (11, "Send Messages"),
    (16, "Read Message History"),
    (6, "Add Reactions"),
    (20, "Connect"),
    (21, "Speak"),
    (25, "Use Voice Activity"),
    (26, "Change Nickname"),
    (31, "Use Application Commands"),
    (35, "Create Public Threads"),
    (38, "Send Messages in Threads"),
];
const HELPERS: [(u32, &str); 13] = [
    (0, "Create Invite"),
    (9, "Video"),
    (14, "Embed Links"),
    (15, "Attach Files"),
    (18, "Use External Emoji"),
    (36, "Create Private Threads"),
    (37, "Use External Stickers"),
    (39, "Use Activities"),
    (42, "Use Soundboard"),
    (45, "Use External Sounds"),
    (46, "Send Voice Messages"),
    (49, "Create Polls"),
    (50, "Use External Apps"),
];
/// Names of the other bits, shown locked.
const OTHERS: [(u32, &str); 26] = [
    (1, "Kick Members"),
    (2, "Ban Members"),
    (3, "Administrator"),
    (4, "Manage Channels"),
    (5, "Manage Server"),
    (7, "View Audit Log"),
    (8, "Priority Speaker"),
    (12, "Send TTS Messages"),
    (13, "Manage Messages"),
    (17, "Mention @everyone"),
    (19, "View Server Insights"),
    (22, "Mute Members"),
    (23, "Deafen Members"),
    (24, "Move Members"),
    (27, "Manage Nicknames"),
    (28, "Manage Roles"),
    (29, "Manage Webhooks"),
    (30, "Manage Expressions"),
    (32, "Request to Speak"),
    (33, "Manage Events"),
    (34, "Manage Threads"),
    (40, "Timeout Members"),
    (41, "View Monetization"),
    (43, "Create Expressions"),
    (44, "Create Events"),
    (48, "Set Voice Status"),
];

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn role_item<'a>(d: &'a mut Value, id: &str) -> &'a mut Value {
    let it = &mut d["roles"][id];
    if !it.is_object() {
        *it = json!({});
    }
    it
}

/// The movable roles top first, as the page shows them.
fn movable(roles: &[Value]) -> Vec<String> {
    roles
        .iter()
        .filter(|r| r["movable"] == true)
        .map(|r| s(&r["id"]))
        .collect()
}

/// Puts `id` at `at` in the movable order and stores the whole order in the draft.
fn reorder(d: &mut Value, roles: &[Value], id: &str, at: usize) {
    // From the local draft: an earlier move whose reply is not back yet stays.
    let mut order = movable(roles);
    if let Some(local) = d["role_order"].as_array() {
        let local: Vec<String> = local.iter().filter_map(|v| v.as_str().map(String::from)).filter(|k| order.contains(k)).collect();
        let rest: Vec<String> = order.iter().filter(|k| !local.contains(k)).cloned().collect();
        order = local.into_iter().chain(rest).collect();
    }
    let Some(from) = order.iter().position(|x| x == id) else {
        return;
    };
    let k = order.remove(from);
    order.insert(at.min(order.len()), k);
    d["role_order"] = json!(order);
}

/// The draft key for a new role: the same numbers as new channels.
fn new_key(d: &Value) -> String {
    let n = ["items", "roles"]
        .iter()
        .flat_map(|m| d[*m].as_object().into_iter().flatten())
        .filter_map(|(k, _)| k.strip_prefix("new-")?.parse::<u64>().ok())
        .map(|n| n + 1)
        .chain(d["next"].as_u64())
        .max();
    format!("new-{}", n.unwrap_or(1).max(1))
}

fn dot(color: u64) -> String {
    match color {
        0 => "background:var(--muted)".into(),
        c => format!("background:#{c:06x}"),
    }
}

/// The Roles tab: the list in role order on the left, the picked role on the right.
pub(crate) fn roles_tab<E>(data: RwSignal<Value>, edit: E) -> impl IntoView
where
    E: Fn(&dyn Fn(&mut Value, &[Value])) + Copy + Send + Sync + 'static,
{
    let sel = RwSignal::new(None::<String>);
    let (drag, over) = (RwSignal::new(None::<String>), RwSignal::new(None::<String>));
    let roles =
        Memo::new(move |_| data.with(|v| v["roles"].as_array().cloned().unwrap_or_default()));
    let set = move |id: String, f: &'static str, v: Value| {
        edit(&move |d: &mut Value, _: &[Value]| role_item(d, &id)[f] = v.clone())
    };
    let move_to = move |id: String, at: usize| {
        let list = roles.get_untracked();
        edit(&move |d: &mut Value, _: &[Value]| reorder(d, &list, &id, at))
    };
    let add = move || {
        if data.with_untracked(Value::is_null) {
            return;
        }
        let key = data.with_untracked(|v| new_key(&v["draft"]));
        let k = key.clone();
        edit(&move |d: &mut Value, _: &[Value]| role_item(d, &k)["name"] = json!("new role"));
        sel.set(Some(key));
    };
    // A new role is dropped from the draft; a live one is marked deleted.
    let remove = move |id: String, new: bool| {
        edit(&move |d: &mut Value, _: &[Value]| {
            if new {
                if let Some(o) = d["roles"].as_object_mut() {
                    o.remove(&id);
                }
                if let Some(o) = d["role_order"].as_array_mut() {
                    o.retain(|x| x != id.as_str());
                }
            } else {
                role_item(d, &id)["delete"] = json!(true);
            }
        })
    };
    let undo = move |id: String| set(id, "delete", json!(false));

    let drop_on = move |d: String, target: &str| {
        if let Some(at) = movable(&roles.get_untracked()).iter().position(|x| x == target) { move_to(d, at) }
    };
    let hold = crate::common::HoldDrag::new();

    let row = move |r: Value| {
        let id = s(&r["id"]);
        let order = movable(&roles.get_untracked());
        let at = order.iter().position(|x| *x == id);
        let last = order.len().saturating_sub(1);
        let can_drag = at.is_some() && !crate::channels::coarse_pointer();
        let (i1, i2, i3, i4, i5) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
        let name = s(&r["name"]);
        let label = name.clone();
        let locked = r["locked"].as_str().map(String::from);
        view! {
            <div class="tr" class:sel=move || sel.get().as_deref() == Some(i1.as_str()) class:del={r["deleted"] == true}
                class:over=move || over.get().as_deref() == Some(i2.as_str())
                class:moving={ let id = id.clone(); move || drag.get().as_deref() == Some(id.as_str()) }
                draggable={ if can_drag { "true" } else { "false" } } tabindex="0" aria-label=label
                on:click=move |_| sel.set(Some(i3.clone()))
                on:keydown=move |e| {
                    let Some(at) = at else { return };
                    if e.alt_key() && (e.key() == "ArrowUp" || e.key() == "ArrowDown") {
                        e.prevent_default();
                        let down = e.key() == "ArrowDown";
                        if (down && at < last) || (!down && at > 0) {
                            move_to(i4.clone(), if down { at + 1 } else { at - 1 });
                            crate::channels::refocus(".cs-grid", event_target::<web_sys::Element>(&e).get_attribute("aria-label").unwrap_or_default());
                        }
                    }
                }
                on:dragstart=move |e| { if let Some(dt) = e.data_transfer() { let _ = dt.set_data("text/plain", &i5); } drag.set(Some(i5.clone())) }
                on:dragover={ let id = id.clone(); move |e| { if drag.get_untracked().is_some() && at.is_some() { e.prevent_default(); over.set(Some(id.clone())) } } }
                on:dragend=move |_| { drag.set(None); over.set(None) }
                on:drop={ let id = id.clone(); move |e| {
                    e.prevent_default();
                    over.set(None);
                    let Some(d) = drag.get_untracked() else { return };
                    drag.set(None);
                    drop_on(d, &id)
                } }
                data-drop=at.map(|_| id.clone())
                on:touchstart={ let id = id.clone(); move |e| { let id = id.clone(); hold.start(&e, at.is_some() && sel.get_untracked().as_deref() == Some(id.as_str()), move || drag.set(Some(id.clone()))) } }
                on:touchmove=move |e| hold.moved(&e, |o| over.set(o))
                on:touchend=move |e| { if drag.is_disposed() { return } let d = drag.get_untracked(); hold.end(&e, |to| { drag.set(None); over.set(None); if let (Some(d), Some(to)) = (d, to) { drop_on(d, &to) } }); }
                on:touchcancel=move |_| { if drag.is_disposed() { return } hold.stop(); drag.set(None); over.set(None) }>
                <span class="h" title=if can_drag { "Drag to move" } else { "" }>{if at.is_some() { icon("menu") } else { icon("lock") }}</span>
                <span class="rdot" style=dot(r["color"].as_u64().unwrap_or(0)) aria-hidden="true"></span>
                <span class="nm">{name}</span>
                {(r["new"] == true).then(|| view! { <span class="chip blue">"New"</span> })}
                {locked.map(|t| view! { <Tip icon="lock" text=t /> })}
                <span class="sp"></span>
                {r["tag"].as_str().map(|t| view! { <span class="mini">{t.to_string()}</span> })}
            </div>
        }
    };

    let group = move |id: String,
                      perms: u64,
                      title: &'static str,
                      list: &'static [(u32, &'static str)],
                      off: bool| {
        let boxes = list.iter().map(|&(bit, name)| {
            let on = perms & (1u64 << bit) != 0;
            let id = id.clone();
            view! { <label class="row2"><input type="checkbox" prop:checked=on disabled=off aria-label=name
                on:change=move |e| {
                    let on = event_target_checked(&e);
                    let id = id.clone();
                    // From the local draft, so two quick ticks both stay.
                    edit(&move |d: &mut Value, _: &[Value]| {
                        let it = role_item(d, &id);
                        let cur = it["permissions"].as_u64().unwrap_or(perms);
                        it["permissions"] = json!(if on { cur | (1u64 << bit) } else { cur & !(1u64 << bit) });
                    })
                } /><span>{name}</span></label> }
        }).collect_view();
        view! { <details class="perm" open=true><summary class="lbl">{title}</summary>{boxes}</details> }
    };

    let panel = move || {
        // Every reply rebuilds the panel; the control that was used gets the focus back.
        let had = crate::channels::panel_focus();
        let list = roles.get();
        let Some(r) = sel
            .get()
            .and_then(|id| list.iter().find(|r| s(&r["id"]) == id).cloned())
        else {
            return view! { <div class="card cs-panel"><p class="muted">"Pick a role to see what it can do."</p></div> }.into_any();
        };
        let id = s(&r["id"]);
        if let Some(l) = had {
            crate::channels::refocus(".cs-panel", l);
        }
        let sid = StoredValue::new(id.clone());
        let new = r["new"] == true;
        let locked = r["locked"].as_str().map(String::from);
        let off = locked.is_some();
        let perms = r["permissions"].as_u64().unwrap_or(0);
        let order = movable(&list);
        let at = order.iter().position(|x| *x == id);
        let last = order.len().saturating_sub(1);
        if r["deleted"] == true {
            return view! { <div class="card cs-panel"><div class="ttl"><h2>{s(&r["name"])}</h2></div>
                <p class="muted">"Deleted when you push."</p>
                <button class="ghost" on:click=move |_| undo(sid.get_value())>{icon("restore")}"Undo delete"</button></div> }.into_any();
        }
        let dangerous: Vec<&str> = OTHERS
            .iter()
            .filter(|(b, _)| perms & (1u64 << b) != 0)
            .map(|(_, n)| *n)
            .collect();
        let danger = if dangerous.is_empty() {
            "None.".to_string()
        } else {
            format!("{}.", dangerous.join(", "))
        };
        let color = format!("#{:06x}", r["color"].as_u64().unwrap_or(0));
        let (h, m) = (r["hoist"] == true, r["mentionable"] == true);
        view! { <div class="card cs-panel">
            <div class="ttl"><h2>{s(&r["name"])}</h2><span class="sp"></span>
                {(!off).then(|| view! { <button class="link" title="Delete" aria-label="delete" on:click=move |_| remove(sid.get_value(), new)>{icon("trash")}</button> })}
            </div>
            {locked.map(|t| view! { <p class="line muted">{icon("lock")}{t}</p> })}
            <label class="lbl" for="role-name">"Name"</label>
            <input id="role-name" aria-label="role name" maxlength="100" prop:value=s(&r["name"]) disabled=off
                on:change=move |e| { let v = event_target_value(&e); if !v.trim().is_empty() { set(sid.get_value(), "name", json!(v.trim())) } } />
            <div class="row2"><label class="row2"><input type="color" aria-label="colour" prop:value=color disabled=off
                on:change=move |e| { let c = u64::from_str_radix(event_target_value(&e).trim_start_matches('#'), 16).unwrap_or(0); set(sid.get_value(), "color", json!(c)) } /><span>"Colour"</span></label></div>
            <label class="row2"><input type="checkbox" class="sw" aria-label="show separately" prop:checked=h disabled=off on:change=move |e| set(sid.get_value(), "hoist", json!(event_target_checked(&e))) /><span>"Show members separately"</span></label>
            <label class="row2"><input type="checkbox" class="sw" aria-label="mentionable" prop:checked=m disabled=off on:change=move |e| set(sid.get_value(), "mentionable", json!(event_target_checked(&e))) /><span>"Anyone can mention it"</span></label>
            <div class="lbl">"What this role can do"</div>
            {group(id.clone(), perms, "Member basics", &BASICS, off)}
            {group(id.clone(), perms, "Helper tools", &HELPERS, off)}
            <details class="perm"><summary class="lbl">{icon("lock")}"Dangerous"</summary>
                <p class="line muted">"Brim does this with permits instead."</p>
                <p class="line muted">{format!("This role has: {danger}")}</p>
            </details>
            {at.map(|at| view! {
                <div class="lbl">"Position"</div>
                <div class="row2">
                    <button class="ghost" aria-label="Move up" disabled={at == 0} on:click=move |_| move_to(sid.get_value(), at.saturating_sub(1))>{icon("arrow_up")}"Move up"</button>
                    <button class="ghost" aria-label="Move down" disabled={at >= last} on:click=move |_| move_to(sid.get_value(), at + 1)>{icon("arrow_down")}"Move down"</button>
                </div>
            })}
        </div> }.into_any()
    };

    view! {
        <div class="cs-grid">
            <div class="card">
                <div class="ttl"><h2>{icon("role")}"Roles"</h2><span class="sp"></span>
                    <AddButton label="Add role" attr:disabled=move || data.with(Value::is_null) on:click=move |_| add() /></div>
                <p class="muted">"Top is highest. Drag or use Alt+Up and Alt+Down to move a role."</p>
                {move || if data.with(Value::is_null) { view! { <div class="skel"></div> }.into_any() } else { roles.get().into_iter().map(row).collect_view().into_any() }}
            </div>
            {panel}
        </div>
    }
}

/// The Server tab: guarded server settings.
pub(crate) fn server_tab<E>(data: RwSignal<Value>, edit: E, guild: String) -> impl IntoView
where
    E: Fn(&dyn Fn(&mut Value, &[Value])) + Copy + Send + Sync + 'static,
{
    let msg = Msg::default();
    let set = move |f: &'static str, v: Value| {
        edit(&move |d: &mut Value, _: &[Value]| {
            if !d["server"].is_object() {
                d["server"] = json!({});
            }
            d["server"][f] = v.clone();
        })
    };
    let file = NodeRef::<leptos::html::Input>::new();
    let pick = move |_| {
        let Some(f) = file
            .get_untracked()
            .and_then(|i| i.files())
            .and_then(|l| l.get(0))
        else {
            return;
        };
        if !matches!(f.type_().as_str(), "image/png" | "image/jpeg") {
            return msg.err("Only PNG and JPEG images can be used.");
        }
        if f.size() > 500.0 * 1024.0 {
            return msg.err("The image is larger than 500 KB.");
        }
        let mime = f.type_();
        leptos::task::spawn_local(async move {
            let buf = wasm_bindgen_futures::JsFuture::from(f.array_buffer()).await;
            // The page or the tab may be gone by now (UI PITFALLS 2).
            if data.is_disposed() || msg.text_disposed() {
                return;
            }
            let Ok(buf) = buf else {
                return msg.err("The image could not be read.");
            };
            let bin: String = js_sys::Uint8Array::new(&buf)
                .to_vec()
                .into_iter()
                .map(char::from)
                .collect();
            let Some(b64) = web_sys::window().and_then(|w| w.btoa(&bin).ok()) else {
                return msg.err("The image could not be read.");
            };
            set("icon", json!(format!("data:{mime};base64,{b64}")));
        });
    };
    let g = StoredValue::new(guild);
    let body = move || {
        let (want, live, tree, icon_draft) = data.with(|v| {
            (
                v["server"]["want"].clone(),
                v["server"]["live"].clone(),
                v["tree"].as_array().cloned().unwrap_or_default(),
                v["draft"]["server"].get("icon").cloned(),
            )
        });
        if want.is_null() {
            return view! { <div class="skel"></div> }.into_any();
        }
        let pick_list = |kind: u64, cur: &Value| {
            let cur = cur
                .as_u64()
                .map(|c| c.to_string())
                .or_else(|| cur.as_str().map(String::from));
            let opts = tree
                .iter()
                .filter(|c| {
                    c["kind"].as_u64() == Some(kind) && c["new"] != true && c["deleted"] != true
                })
                .map(|c| (s(&c["id"]), s(&c["name"])))
                .collect::<Vec<_>>();
            (opts, cur.unwrap_or_default())
        };
        let (sys, sys_now) = pick_list(0, &want["system_channel"]);
        let (afk, afk_now) = pick_list(2, &want["afk_channel"]);
        let opt = |list: &'static [(u64, &'static str)], cur: u64| {
            list.iter().map(|&(v, n)| view! { <option value=v.to_string() prop:selected={v == cur}>{n}</option> }).collect_view()
        };
        let num = |e: &leptos::ev::Event| event_target_value(e).parse::<u64>().unwrap_or(0);
        let chan = |v: String| if v.is_empty() { Value::Null } else { json!(v) };
        let img = match icon_draft {
            Some(Value::String(uri)) => Some(uri),
            Some(Value::Null) => None,
            _ => live["icon"].as_str().map(|h| {
                format!(
                    "https://cdn.discordapp.com/icons/{}/{h}.png?size=64",
                    g.get_value()
                )
            }),
        };
        let has_icon = img.is_some();
        view! {
            <label class="lbl" for="srv-name">"Server name"</label>
            <input id="srv-name" maxlength="100" prop:value=s(&want["name"]) on:change=move |e| { let v = event_target_value(&e); if v.trim().chars().count() >= 2 { set("name", json!(v.trim())) } } />
            <div class="lbl">"Icon"</div>
            <div class="row2">
                {img.map(|src| view! { <img class="srv-icon" src=src alt="server icon" width="48" height="48" /> })}
                <input type="file" accept="image/png,image/jpeg" aria-label="icon file" node_ref=file on:change=pick />
                {has_icon.then(|| view! { <button class="ghost" on:click=move |_| set("icon", Value::Null)>"Remove"</button> })}
            </div>
            <label class="lbl" for="srv-ver">"Verification level"</label>
            <select id="srv-ver" on:change=move |e| set("verification", json!(num(&e)))>
                {opt(&[(0, "None"), (1, "Low: a verified email"), (2, "Medium: on Discord for 5 minutes"), (3, "High: a member for 10 minutes"), (4, "Highest: a verified phone")], want["verification"].as_u64().unwrap_or(0))}
            </select>
            <label class="lbl" for="srv-cf">"Content filter"</label>
            <select id="srv-cf" on:change=move |e| set("content_filter", json!(num(&e)))>
                {opt(&[(0, "Off"), (1, "Members without roles"), (2, "Everyone")], want["content_filter"].as_u64().unwrap_or(0))}
            </select>
            <label class="lbl" for="srv-sys">"System messages channel"</label>
            <crate::form::ListPick items=Signal::derive(move || sys.clone()) get=Signal::derive(move || sys_now.clone()) set=Callback::new(move |v: String| set("system_channel", chan(v))) none="None" label="system messages channel" />
            <label class="lbl" for="srv-afk">"AFK channel"</label>
            <crate::form::ListPick items=Signal::derive(move || afk.clone()) get=Signal::derive(move || afk_now.clone()) set=Callback::new(move |v: String| set("afk_channel", chan(v))) none="None" label="AFK channel" />
            <label class="lbl" for="srv-afkt">"AFK timeout"</label>
            <select id="srv-afkt" on:change=move |e| set("afk_timeout", json!(num(&e)))>
                {opt(&[(60, "1 minute"), (300, "5 minutes"), (900, "15 minutes"), (1800, "30 minutes"), (3600, "1 hour")], want["afk_timeout"].as_u64().unwrap_or(300))}
            </select>
        }.into_any()
    };
    view! {
        <div class="card cs-server">
            <div class="ttl"><h2>{icon("server")}"Server"</h2></div>
            <p class="muted">"Layout Guard keeps these settings as saved here. Changes go out with Push."</p>
            {body}
            <Toast msg />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_keeps_only_movable_roles_and_new_keys_share_the_numbers() {
        let roles = vec![
            json!({"id": "9", "movable": false}),
            json!({"id": "1", "movable": true}),
            json!({"id": "2", "movable": true}),
            json!({"id": "3", "movable": true}),
            json!({"id": "0", "movable": false}),
        ];
        let mut d = json!({});
        reorder(&mut d, &roles, "3", 0);
        assert_eq!(d["role_order"], json!(["3", "1", "2"]));
        reorder(&mut d, &roles, "9", 0);
        assert_eq!(d["role_order"], json!(["3", "1", "2"]));
        let d = json!({"items": {"new-4": {}}, "roles": {"new-7": {}}, "next": 2});
        assert_eq!(new_key(&d), "new-8");
    }
}

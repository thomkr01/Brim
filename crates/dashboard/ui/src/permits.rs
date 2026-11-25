use crate::{common::*, form::Field};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

/// Adds or removes `key` in `c.custom[i][field]` (perms / whitelist); keys the catalog doesn't list stay untouched.
fn toggle(c: &mut Value, i: usize, field: &str, key: &str, on: bool) {
    let list = &mut c["custom"][i][field];
    if !list.is_array() {
        *list = json!([]);
    }
    let a = list.as_array_mut().unwrap();
    a.retain(|x| x != key);
    if on {
        a.push(json!(key));
    }
}

fn has(c: &Value, i: usize, field: &str, key: &str) -> bool {
    c["custom"][i][field]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x == key))
}

#[component]
pub fn Permits(guild: String) -> impl IntoView {
    let cfg = RwSignal::new(json!({}));
    let dc = RwSignal::new(json!({}));
    let catalog = RwSignal::new(json!([]));
    let version = RwSignal::new(0i64);
    let msg = Msg::default();
    let need = step_up();
    let open = RwSignal::new(None::<i32>); // permit being edited
    let tab = RwSignal::new(0u8);
    let orig = RwSignal::new(json!({}));
    let deleting = RwSignal::new(None::<usize>);
    let g = guild.clone();
    spawn_local(async move {
        // Role names first: a permit opened before they came showed no roles to tick.
        match api("GET", &format!("/api/guilds/{g}/discord"), None).await {
            Ok(v) => dc.set(v),
            Err(e) => msg.err(format!("The role names could not be loaded, so the role lists are empty: {e}")),
        }
        if let Ok(v) = api("GET", &format!("/api/guilds/{g}/config"), None).await {
            version.set(v["version"].as_i64().unwrap_or(0));
            // Old stored ranks may tie; the list order becomes the hierarchy from the first save.
            let mut p = v["config"]["permits"].clone();
            rank_by_order(&mut p);
            cfg.set(p.clone());
            orig.set(p);
        }
        if let Ok(v) = api("GET", "/api/permit-catalog", None).await {
            catalog.set(v);
        }
    });
    let access = expect_context::<Access>();
    let staff_guild = guild.clone();
    let saving = RwSignal::new(false);
    let save = move |_| {
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        let (g, p, v) = (guild.clone(), cfg.get_untracked(), version.get_untracked());
        run_save(
            async move {
                let r = api(
                    "PUT",
                    &format!("/api/guilds/{g}/permits"),
                    Some(json!({"expected_version": v, "value": p.clone()})),
                )
                .await;
                saving.try_set(false);
                // Only a stored save hides the Save bar; a refused or 2FA-held one keeps it.
                if r.is_ok() {
                    orig.try_set(p);
                    access.tick.update(|n| *n += 1);
                }
                r
            },
            version,
            msg,
            need,
        );
    };
    let set_field = move |i: usize, field: &'static str, val: String| {
        cfg.update(|c| c["custom"][i][field] = json!(val))
    };
    // Dialog target: -1 Extra Owner, -2 Trusted Admin, 0.. a custom permit.
    let snap = RwSignal::new(json!({}));
    let section = RwSignal::new(None::<String>);
    let start = move |i: i32| {
        snap.set(cfg.get_untracked());
        tab.set(0);
        section.set(None);
        open.set(Some(i));
    };
    let abort = move |_| {
        cfg.set(snap.get_untracked());
        open.set(None);
    };
    let dialog = move |i: i32| {
        let n = i.max(0) as usize;
        let ids = (i < 0).then_some(if i == -1 { "eo" } else { "ta" });
        let r = NodeRef::new();
        focus_on_open(r);
        view! { <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Edit permit" on:keydown=move |e| dialog_keys(&e, || { cfg.set(snap.get_untracked()); open.set(None) })><div class="card">
            <PageTabs tabs={let mut t = vec![Tab::state("0", "General")]; if i >= 0 { t.push(Tab::state("1", "Permissions")); } t}
                active=Signal::derive(move || tab.get().to_string())
                on_pick=Callback::new(move |k: &'static str| { if k == "1" { section.set(None); } tab.set(if k == "1" { 1 } else { 0 }) }) />
            <Show when=move || tab.get() == 0>
                <div class="row"><label>"Permit Name"</label><input aria-label="permit name" disabled=i < 0 prop:value=move || match ids { Some("eo") => "Extra Owner".to_string(), Some(_) => "Trusted Admin".to_string(), None => cfg.get()["custom"][n]["name"].as_str().unwrap_or("").to_string() } on:change=move |e| set_field(n, "name", event_target_value(&e)) /></div>
                {match ids {
                    Some(k) => view! { <Field f=json!({"path": k, "kind": "users", "title": "Permit Users"}) val=cfg dc /> }.into_any(),
                    None => view! {
                        <Field f=json!({"path": format!("custom.{n}.members"), "kind": "users", "title": "Members"}) val=cfg dc />
                        <Field f=json!({"path": format!("custom.{n}.roles"), "kind": "roles", "title": "Roles Brim gives"}) val=cfg dc />
                        <p class="muted">"Brim gives these roles to the members and takes them back from anyone else. A role alone gives no permit."</p> }.into_any(),
                }}
            </Show>
            <Show when=move || tab.get() == 1>
                <For each=move || catalog.get().as_array().cloned().unwrap_or_default() key=|s| s["id"].as_str().unwrap_or("").to_string() let:s>
                    {
                        let id = s["id"].as_str().unwrap_or("").to_string();
                        let (id2, title, title2) = (id.clone(), s["title"].as_str().unwrap_or("").to_string(), s["title"].as_str().unwrap_or("").to_string());
                        view! {
                            <Show when=move || section.get().is_none()><button class="ghost menu" on:click={let id = id.clone(); move |_| section.set(Some(id.clone()))}>{title.clone()}</button></Show>
                            <Show when={let id = id2.clone(); move || section.get().as_deref() == Some(id.as_str())}>
                                <p class="crumbline"><button class="link" on:click=move |_| section.set(None)>"Permissions"</button><span class="sep" aria-hidden="true">" › "</span>{title2.clone()}</p>
                                {s["items"].as_array().cloned().unwrap_or_default().into_iter().map(|it| {
                                    let key = it["key"].as_str().unwrap_or("").to_string();
                                    let field = if it["flag"].as_bool().unwrap_or(false) { "whitelist" } else { "perms" };
                                    // Mirrors the server's keep_exemptions: below Owner these toggles are refused.
                                    let flag = field == "whitelist";
                                    let owner_only = key == "mod_stats" || (flag && (key == "all" || key == "antinuke" || key == "quarantine" || key.starts_with("wl_")));
                                    let locked = it["locked"].as_bool().unwrap_or(false) || (owner_only && access.rank() < 4);
                                    let k = key.clone();
                                    view! {
                                        <div class="row tog">
                                            <input type="checkbox" class="sw" aria-label=it["label"].as_str().unwrap_or("").to_string() disabled=locked
                                                prop:checked=move || has(&cfg.get(), n, field, &k)
                                                on:change=move |e| { let on = event_target_checked(&e); let k = key.clone(); cfg.update(|c| toggle(c, n, field, &k, on)) } />
                                            <label>{it["label"].as_str().unwrap_or("").to_string()}</label><span class="muted">{it["help"].as_str().unwrap_or("").to_string()}</span>
                                        </div>
                                    }
                                }).collect_view()}
                            </Show>
                        }
                    }
                </For>
            </Show>
            <div class="row">
                <button class="ghost" on:click=abort>"Abort"</button>
                {(i >= 0).then(|| view! { <button class="danger" title="Delete Permit" on:click=move |_| deleting.set(Some(n))>"Delete"</button> })}
                <span class="sp"></span>
                <button on:click=move |_| open.set(None)>"Apply"</button>
            </div>
        </div></div> }
    };
    let (drag, over, said) = (RwSignal::new(None::<usize>), RwSignal::new(None::<usize>), RwSignal::new(String::new()));
    let name = move |i: usize| cfg.get_untracked()["custom"][i]["name"].as_str().unwrap_or("").to_string();
    // Moves a permit to the place of another; the screen reader hears where it went.
    let reorder = move |(from, to): (usize, usize)| {
        if from == to { return; }
        cfg.update(|c| move_permit(c, from, to));
        let n = cfg.with_untracked(|c| c["custom"].as_array().map_or(0, Vec::len));
        said.set(format!("{} moved to place {} of {}.", name(to), to + 1, n));
    };
    let delete = move |i: usize| {
        cfg.update(|c| {
            c["custom"].as_array_mut().map(|a| a.remove(i));
        })
    };
    // The last dialog stays rendered while it fades out.
    let del =
        Memo::new(move |prev: Option<&Option<usize>>| deleting.get().or(prev.copied().flatten()));
    let dlg = Memo::new(move |prev: Option<&Option<i32>>| open.get().or(prev.copied().flatten()));
    let new_permit = move || { let before = cfg.get_untracked(); cfg.update(|c| { if !c["custom"].is_array() { c["custom"] = json!([]); } c["custom"].as_array_mut().unwrap().push(json!({"name": "New permit", "rank": 0, "perms": [], "members": [], "roles": [], "whitelist": [], "limits": {}})); rank_by_order(c); }); start(cfg.get_untracked()["custom"].as_array().map_or(0, |a| a.len() as i32 - 1)); snap.set(before) };
    view! {
            <div class="row"><AddButton label="Add new permit" on:click=move |_| new_permit() /></div>
            <p class="muted">"A permit higher in the list outranks the ones below it. Nobody can act against their own permit or a higher one."</p>
            {move || (access.rank() < 2).then(|| view! { <p class="muted">"Viewers do not see who holds Extra Owner, Trusted Admin or a permit."</p> })}
            <p class="sr" id="sort-hint">"Use the up and down arrow keys to move the focused permit."</p>
            <p class="sr" aria-live="polite">{move || said.get()}</p>
            <div class="card slist" role="list">
                <div class="srow fixed" role="listitem"><span class="grip" title="Fixed place">{crate::icons::icon("lock")}</span><span class="sname"><b>"Extra Owner"</b><span class="chip">"OWNER"</span><span class="chip">"IMMUNITY"</span></span><button class="ghost" title="Edit Permit" on:click=move |_| start(-1)>"Edit"</button></div>
                <div class="srow fixed" role="listitem"><span class="grip" title="Fixed place">{crate::icons::icon("lock")}</span><span class="sname"><b>"Trusted Admin"</b><span class="chip">"IMMUNITY"</span></span><button class="ghost" title="Edit Permit" on:click=move |_| start(-2)>"Edit"</button></div>
                {move || { let n = cfg.get()["custom"].as_array().map_or(0, Vec::len); let drag_ok = !crate::channels::coarse_pointer(); (0..n).map(|i| view! {
                    <div class="srow" role="listitem" tabindex="0" aria-label=name(i) aria-describedby="sort-hint" data-i=i.to_string()
                        draggable=if drag_ok { "true" } else { "false" }
                        class:moving=move || drag.get() == Some(i)
                        class:above=move || over.get() == Some(i) && drag.get().is_some_and(|d| d > i)
                        class:below=move || over.get() == Some(i) && drag.get().is_some_and(|d| d < i)
                        on:keydown=move |e| {
                            // Keys typed on the Edit button are not moves.
                            if e.target() != e.current_target() { return; }
                            let to = match e.key().as_str() { "ArrowUp" if i > 0 => i - 1, "ArrowDown" if i + 1 < n => i + 1, _ => return };
                            e.prevent_default();
                            reorder((i, to));
                            focus_row(to);
                        }
                        on:dragstart=move |e| { if let Some(dt) = e.data_transfer() { let _ = dt.set_data("text/plain", ""); } drag.set(Some(i)) }
                        on:dragover=move |e| { if drag.get_untracked().is_some() { e.prevent_default(); over.set(Some(i)) } }
                        on:dragend=move |_| { drag.set(None); over.set(None) }
                        on:drop=move |e| { e.prevent_default(); if let Some(d) = drag.get_untracked() { reorder((d, i)) } drag.set(None); over.set(None) }>
                        <Grip i drag over drop=Callback::new(reorder) />
                        <span class="sname"><b>{name(i)}</b></span>
                        <button class="ghost" title="Edit Permit" on:click=move |_| start(i as i32)>"Edit"</button>
                    </div>
                }).collect_view() }}
                {move || cfg.with(|c| c["custom"].as_array().is_none_or(Vec::is_empty)).then(|| view! { <p class="muted" role="listitem">"No permits of your own yet. Make one for a staff group, such as Moderators, with Add new permit, then add people to it by their Discord user ID."</p> })}
            </div>
            <StaffRole guild=staff_guild />
            <Fade when=Signal::derive(move || open.get().is_some())>{move || dlg.get().map(|i| dialog(i))}</Fade>
            <Fade when=Signal::derive(move || deleting.get().is_some())>{move || del.get().map(|n| { let r = NodeRef::new(); focus_on_open(r); view! { <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Delete permit" on:keydown=move |e| dialog_keys(&e, || deleting.set(None))><div class="card"><p>{format!("Delete the permit \"{}\"?", cfg.get_untracked()["custom"][n]["name"].as_str().unwrap_or(""))}</p>
                <div class="row"><button class="ghost" on:click=move |_| deleting.set(None)>"Abort"</button><button class="danger" on:click=move |_| { open.set(None); delete(n); deleting.set(None) }>"Delete"</button></div></div></div> } })}
    </Fade>
            <Fade when=Signal::derive(move || cfg.get() != orig.get())>
                <div class="bar"><b>"Changes detected!"</b><button class="ghost" on:click=move |_| cfg.set(orig.get_untracked())>"Cancel"</button><button disabled=move || saving.get() on:click=save.clone()>{busy_label(move || saving.get(), "Save")}</button></div>
            </Fade>
            <Toast msg />
        }
}

/// The list order is the hierarchy (owner decision 2026-11-15): the top permit gets the highest rank.
fn rank_by_order(c: &mut Value) {
    if let Some(a) = c["custom"].as_array_mut() {
        let n = a.len();
        for (i, p) in a.iter_mut().enumerate() {
            p["rank"] = json!((n - 1 - i) as i64);
        }
    }
}

/// Focuses permit row `i` once the rebuilt list is in (by place, so two permits with one name still work).
fn focus_row(i: usize) {
    use wasm_bindgen::JsCast;
    request_animation_frame(move || {
        let el = document().query_selector(&format!(".slist .srow[data-i=\"{i}\"]")).ok().flatten();
        if let Some(el) = el.and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()) { let _ = el.focus(); }
    });
}

fn move_permit(c: &mut Value, from: usize, to: usize) {
    if let Some(a) = c["custom"]
        .as_array_mut()
        .filter(|a| from < a.len() && to < a.len())
    {
        let p = a.remove(from);
        a.insert(to, p);
    }
    rank_by_order(c);
}

#[cfg(test)]
mod order_tests {
    use super::*;

    #[test]
    fn moving_a_permit_renumbers_by_order() {
        let mut c = json!({"custom": [{"name": "A", "rank": 5}, {"name": "B", "rank": 5}, {"name": "C", "rank": 1}]});
        move_permit(&mut c, 2, 1);
        let got: Vec<(&str, i64)> = c["custom"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p["name"].as_str().unwrap(), p["rank"].as_i64().unwrap()))
            .collect();
        assert_eq!(got, [("A", 2), ("C", 1), ("B", 0)]);
        move_permit(&mut c, 0, 3);
        move_permit(&mut c, 0, 2); // a drag jumps places: A goes last, the others move up
        let names: Vec<&str> = c["custom"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["C", "B", "A"]);
        assert_eq!(c["custom"][2]["rank"], 0);
        move_permit(&mut c, 0, 3);
        assert_eq!(c["custom"][0]["name"], "C", "out of range: no move");
    }
}

/// The live text of the turn-off popup, from `GET /staff-role`.
pub fn off_text(v: &Value) -> String {
    let n = v["members"].as_i64().unwrap_or(0);
    let who = if n == 1 { "1 member loses".to_string() } else { format!("{n} members lose") };
    let role = if v["made"] == true { "Brim deletes the Staff role." } else { "The role stays, and Brim takes it from everyone." };
    let mut t = format!("{who} the Staff role. {role} Staff channels and log channels stay open to the staff permit roles, and the Staff role loses its access there. New tickets ping the staff permit roles again.");
    let warned: Vec<&str> = v["warned"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !warned.is_empty() {
        t.push_str(&format!(" Brim takes the Staff role out of the alert pings of {}.", warned.join(" and ")));
    }
    t
}

/// The Staff role switch (owner decision 2026-11-25): one card, the same on the Permits page and in
/// the setup wizard. Off by default. On: Brim makes a "Staff" role (or uses the one picked) and gives it to
/// every staff member. Off asks first and says what changes. Only the Owner and Extra Owners can change it.
#[component]
pub fn StaffRole(guild: String) -> impl IntoView {
    let state = RwSignal::new(None::<Value>);
    let roles = RwSignal::new(Vec::<(String, String)>::new());
    let pick = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let (msg, need) = (Msg::default(), step_up());
    let load = {
        let g = guild.clone();
        move || {
            let g = g.clone();
            spawn_local(async move {
                match api("GET", &format!("/api/guilds/{g}/staff-role"), None).await {
                    Ok(v) => _ = state.try_set(Some(v)),
                    // The page may be gone after the await (ui PITFALLS rule 1).
                    Err(e) if !msg.text_disposed() => msg.err(format!("The Staff role could not be loaded: {e}")),
                    Err(_) => {}
                }
                if let Ok(d) = api("GET", &format!("/api/guilds/{g}/discord"), None).await {
                    let list = d["roles"].as_array().into_iter().flatten()
                        .filter(|r| r["id"].as_str() != Some(g.as_str()) && r["managed"] != true)
                        .map(|r| (r["id"].as_str().unwrap_or("").to_string(), r["name"].as_str().unwrap_or("?").to_string()))
                        .collect();
                    _ = roles.try_set(list);
                }
            });
        }
    };
    load();
    let on = move || state.with(|s| s.as_ref().is_some_and(|v| v["on"] == true));
    let can = move || state.with(|s| s.as_ref().is_some_and(|v| v["can_change"] == true));
    let click = {
        let g = guild.clone();
        let load = load.clone();
        move |e: leptos::ev::MouseEvent| {
            // The switch follows the saved state only.
            e.prevent_default();
            if busy.get_untracked() || !can() {
                return;
            }
            let (g, load, now) = (g.clone(), load.clone(), on());
            let role = pick.get_untracked();
            let g2 = g.clone();
            let go = move |_: String| {
                busy.set(true);
                let body = if now { json!({"on": false}) } else if role.is_empty() { json!({"on": true}) } else { json!({"on": true, "role": role}) };
                run_act(
                    async move {
                        let r = api("POST", &format!("/api/guilds/{g}/staff-role"), Some(body)).await;
                        busy.try_set(false);
                        r
                    },
                    msg,
                    need,
                    move |v| {
                        load();
                        match v["channels"].as_u64() {
                            Some(n) if n > 0 => msg.done(format!("Staff role on. Push the {n} channel changes in Channel Setup to let it into the Staff channels.")),
                            _ => msg.done(if now { "Staff role off" } else { "Staff role on" }),
                        }
                    },
                )
            };
            if now {
                // Live numbers, read when the popup opens.
                spawn_local(async move {
                    let v = api("GET", &format!("/api/guilds/{g2}/staff-role"), None).await.unwrap_or_default();
                    if msg.text_disposed() {
                        return;
                    }
                    ask(Ask::new("Turn the Staff role off?").text(off_text(&v)).yes("Turn off").danger(), go);
                });
            } else {
                go(String::new())
            }
        }
    };
    view! {
        <div class="card staffrole">
            <div class="row tog">
                <input type="checkbox" class="sw" role="switch" aria-label="Staff role on or off" prop:checked=on disabled=move || busy.get() || !can() on:click=click />
                <label><b>"Staff role"</b></label>
            </div>
            <p class="muted">"Brim gives this role to all staff and takes it from everyone else. It gives no rights. Staff are the Owner, Extra Owners, Trusted Admins and every permit from Moderators up."</p>
            {move || match state.get() {
                None => view! { <p role="status"><Working />" Loading"</p> }.into_any(),
                Some(v) if v["on"] == true => {
                    let id = v["role"].as_str().unwrap_or("").to_string();
                    let name = roles.with(|l| l.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone()));
                    let gone = v["alive"] != true;
                    view! { <p>{match (gone, name) {
                        (true, _) => "The Staff role was deleted in Discord. Overview has a fix that makes it again.".to_string(),
                        (false, Some(n)) => format!("On: Brim gives @{n} to {} members.", v["members"].as_i64().unwrap_or(0)),
                        (false, None) => "On.".to_string(),
                    }}</p> }.into_any()
                }
                Some(_) => view! { <label class="field">"Role to use"
                    <crate::form::ListPick items=Signal::derive(move || roles.get()) get=pick set=Callback::new(move |v| pick.set(v)) none="Make a new Staff role" label="Staff role" disabled=Signal::derive(move || !can()) />
                </label> }.into_any(),
            }}
            {move || (state.with(|s| s.is_some()) && !can()).then(|| view! { <p class="muted">"Only the Owner or an Extra Owner can change this."</p> })}
            <Toast msg />
        </div>
    }
}

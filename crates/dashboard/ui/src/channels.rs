//! Channel Setup: the server's channels plus a draft of changes. Nothing changes in Discord until Push.
//! Every edit sends the whole draft and the page shows the server's reply (tree, issues, change count).
use crate::common::*;
use crate::icons::icon;
use leptos::{html, prelude::*};
use serde_json::{json, Value};

/// Icon for a Discord channel type. Kept in one place so #164's `ChannelTree` mapping can replace it.
/// Also the Backups tree icons.
pub(crate) fn kind_icon(kind: u64) -> &'static str {
    match kind {
        2 => "channel_voice",
        4 => "category",
        5 => "announcement",
        13 => "channel_stage",
        15 => "forum",
        _ => "channel_text",
    }
}

fn kind_name(kind: u64) -> &'static str {
    match kind {
        2 => "Voice channel",
        4 => "Category",
        5 => "Announcement channel",
        13 => "Stage channel",
        15 => "Forum",
        _ => "Text channel",
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn arr(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(String::from))
        .collect()
}

fn is_cat(n: &Value) -> bool {
    n["kind"] == 4
}

/// The name as Discord shows it: `#name` for text-like channels.
fn label(n: &Value) -> String {
    match n["kind"].as_u64() {
        Some(2 | 4 | 13) => s(&n["name"]),
        _ => format!("#{}", s(&n["name"])),
    }
}

fn find<'a>(t: &'a [Value], id: &str) -> Option<&'a Value> {
    t.iter().find(|n| n["id"] == id)
}

/// The channel's category, when that category is in the tree (otherwise it is shown as loose).
fn parent(t: &[Value], n: &Value) -> Option<String> {
    let p = n["parent"].as_str()?;
    t.iter()
        .any(|c| is_cat(c) && c["id"] == p)
        .then(|| p.to_string())
}

/// Voice and stage channels: Discord always lists them below the text channels of a category.
fn is_voice(t: &[Value], id: &str) -> bool {
    find(t, id).is_some_and(|n| matches!(n["kind"].as_u64(), Some(2 | 13)))
}

/// Ids in tree order (the server sends Discord's order): the categories (`cat`), or the channels in
/// `parent` (None: no category), voice ones last.
fn siblings(t: &[Value], parent_id: Option<&str>, cat: bool) -> Vec<String> {
    let mut list: Vec<String> = t
        .iter()
        .filter(|n| is_cat(n) == cat && (cat || parent(t, n).as_deref() == parent_id))
        .map(|n| s(&n["id"]))
        .collect();
    list.sort_by_key(|x| is_voice(t, x));
    list
}

/// A category the draft deletes: nothing new goes into it.
fn gone(t: &[Value], id: &str) -> bool {
    find(t, id).is_some_and(|n| is_cat(n) && n["deleted"] == true)
}

/// The draft item for `id` (an omitted field means unchanged).
fn item<'a>(d: &'a mut Value, id: &str) -> &'a mut Value {
    let it = &mut d["items"][id];
    if !it.is_object() {
        *it = json!({});
    }
    it
}

/// Access as the draft takes it: `limited` false is Everyone, `roles` empty and limited is staff only.
fn access(roles: &[String], staff: bool, limited: bool) -> Value {
    json!({"roles": roles, "staff": staff, "limited": limited})
}

/// Puts `id` at index `at` among the siblings in `parent` and renumbers the ones that moved.
fn place(d: &mut Value, t: &[Value], id: &str, parent_id: Option<String>, at: usize) {
    let Some(n) = find(t, id) else { return };
    let cat = is_cat(n);
    let mut list = siblings(t, parent_id.as_deref(), cat);
    list.retain(|x| x != id);
    list.insert(at.min(list.len()), id.to_string());
    list.sort_by_key(|x| is_voice(t, x));
    if !cat && parent(t, n) != parent_id {
        item(d, id)["parent"] = json!(parent_id);
    }
    for (i, x) in list.iter().enumerate() {
        if find(t, x).and_then(|n| n["position"].as_i64()) != Some(i as i64) {
            item(d, x)["position"] = json!(i);
        }
    }
}

/// Where a row dropped on `target` goes: just before it. A channel dropped on a category goes first in
/// it; a category dropped on a channel goes before that channel's category.
fn drop_target(t: &[Value], dragged: &str, target: &str) -> Option<(Option<String>, usize)> {
    let (d, tg) = (find(t, dragged)?, find(t, target)?);
    if dragged == target || gone(t, target) || parent(t, tg).is_some_and(|p| gone(t, &p)) {
        return None;
    }
    if is_cat(d) {
        let c = if is_cat(tg) {
            target.to_string()
        } else {
            parent(t, tg)?
        };
        let list: Vec<String> = siblings(t, None, true)
            .into_iter()
            .filter(|x| x != dragged)
            .collect();
        return Some((None, list.iter().position(|x| *x == c)?));
    }
    if is_cat(tg) {
        return Some((Some(target.to_string()), 0));
    }
    let p = parent(t, tg);
    let list: Vec<String> = siblings(t, p.as_deref(), false)
        .into_iter()
        .filter(|x| x != dragged)
        .collect();
    Some((p, list.iter().position(|x| x == target)?))
}

/// Applies an issue's fix to the draft: `{strip: target}` adds the target to the item's strip list,
/// `{access}` sets the item's access.
fn apply_fix(d: &mut Value, id: &str, fix: &Value) {
    let it = item(d, id);
    if let Some(r) = fix["strip"].as_str() {
        let mut cur = strs(&it["strip"]);
        if !cur.iter().any(|x| x == r) {
            cur.push(r.to_string());
        }
        it["strip"] = json!(cur);
    }
    if fix["access"].is_object() {
        it["access"] = fix["access"].clone();
    }
}

/// A key for a new draft item: `new-<n>`, one higher than any in the draft and not below the
/// server's `next` (keys a push already created are refused).
fn new_id(d: &Value) -> String {
    // Roles take keys from the same numbers.
    let n = ["items", "roles"]
        .iter()
        .flat_map(|m| d[*m].as_object().into_iter().flatten())
        .filter_map(|(k, _)| k.strip_prefix("new-")?.parse::<u64>().ok())
        .map(|n| n + 1)
        .chain(d["next"].as_u64())
        .max();
    format!("new-{}", n.unwrap_or(1).max(1))
}

/// The aria-label of the focused element inside `.cs-panel`, so it can be focused again after the
/// panel is rebuilt.
pub(crate) fn panel_focus() -> Option<String> {
    let a = web_sys::window()?.document()?.active_element()?;
    a.closest(".cs-panel").ok()??;
    a.get_attribute("aria-label")
}

/// Focuses the control labelled `label` inside `scope` once the rebuilt view is in (PITFALLS 16).
pub(crate) fn refocus(scope: &'static str, label: String) {
    use wasm_bindgen::JsCast;
    request_animation_frame(move || {
        let Some(list) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector_all(&format!("{scope} [aria-label]")).ok())
        else {
            return;
        };
        for i in 0..list.length() {
            let el = list
                .item(i)
                .and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok());
            if let Some(el) =
                el.filter(|e| e.get_attribute("aria-label").as_deref() == Some(&label))
            {
                let _ = el.focus();
                return;
            }
        }
    });
}

fn summary(t: &[Value], n: u64) -> String {
    let parts: Vec<String> = [
        ("new", "new"),
        ("deleted", "deleted"),
        ("renamed", "renamed"),
        ("moved", "moved"),
    ]
    .iter()
    .filter_map(|(k, l)| {
        let c = t
            .iter()
            .filter(|x| x[*k] == true && (*k == "deleted" || x["deleted"] != true))
            .count();
        (c > 0).then(|| format!("{c} {l}"))
    })
    .collect();
    let more = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };
    format!("{n} change{}{more}", if n == 1 { "" } else { "s" })
}

/// A count that the server may send as a number or as a list.
fn count(v: &Value) -> usize {
    v.as_u64()
        .map_or_else(|| v.as_array().map_or(0, Vec::len), |n| n as usize)
}

/// How open a channel or category is, for the safe default after a move (plan 3.2a): higher is
/// more open. Uses the server's badge and access, so it matches what the row shows.
fn openness(n: &Value) -> u8 {
    let a = &n["access"];
    if n["tag"] == "Unverified" {
        3
    } else if a["limited"] != true {
        if n["tag"] == "Members read only" {
            4
        } else {
            5
        }
    } else if !strs(&a["roles"]).is_empty() {
        2
    } else if a["staff"] != false {
        1
    } else {
        0
    }
}

fn level_name(r: u8) -> &'static str {
    match r {
        5 => "Public",
        4 => "Read-only",
        3 => "Unverified",
        2 => "Some roles",
        1 => "Staff",
        _ => "Hidden from staff",
    }
}

/// The levels of "Who can see this" (plan 3.2): key, name, what it means.
const LEVELS: [(&str, &str, &str); 6] = [
    ("public", "Public", "Everyone who is verified sees and writes"),
    ("read", "Read-only", "Members read, staff write"),
    ("staff", "Staff", "Only staff (the Admins and Moderators permits) see it"),
    ("admins", "Admins", "Only Admins, Trusted Admins and the Owner role see it"),
    ("owner", "Owner", "Only the Owner role sees it"),
    ("roles", "Some roles", "Only the roles you pick, plus staff"),
];

/// The level a channel's access is at.
fn level_key(a: &Value) -> &'static str {
    match (a["limited"] == true, a["only"].as_str()) {
        (false, _) if a["read_only"] == true => "read",
        (false, _) => "public",
        (true, Some("owner")) => "owner",
        (true, Some("admins")) => "admins",
        (true, _) if !strs(&a["roles"]).is_empty() => "roles",
        _ => "staff",
    }
}

/// The draft access a level button sets.
fn level_access(k: &str) -> Value {
    match k {
        "public" => access(&[], true, false),
        "read" => json!({"roles": [], "staff": true, "limited": false, "read_only": true}),
        "admins" | "owner" => json!({"roles": [], "staff": true, "limited": true, "only": k}),
        _ => access(&[], true, true),
    }
}

/// The Advanced grid's Discord permissions, in the order shown: bit, short name, for voice.
const GRID: [(u64, &str, bool); 9] = [
    (1 << 10, "View", true),
    (1 << 11, "Send", true),
    (1 << 16, "History", false),
    (1 << 15, "Files", false),
    (1 << 14, "Links", false),
    (1 << 6, "Reactions", false),
    (1 << 18, "Emoji", false),
    (1 << 20, "Connect", true),
    (1 << 21, "Speak", true),
];

/// Neutral, then Allow, then Deny, then Neutral again.
fn cycle(allow: u64, deny: u64, bit: u64) -> (u64, u64) {
    if allow & bit != 0 {
        (allow & !bit, deny | bit)
    } else if deny & bit != 0 {
        (allow, deny & !bit)
    } else {
        (allow | bit, deny)
    }
}

/// "Advanced" (folded): per role, each harmless Discord permission as Allow / Neutral / Deny.
/// Dangerous Discord permissions are not in it (plan question 4).
fn grid_view(
    n: &Value,
    kind: u64,
    sid: StoredValue<String>,
    (open, added): (RwSignal<bool>, RwSignal<Vec<String>>),
    set_grid: impl Fn(String, String, u64, u64) + Copy + 'static,
    dc: RwSignal<Value>,
    grid_roles: Vec<String>,
) -> impl IntoView {
    let voice = matches!(kind, 2 | 13);
    let cols: Vec<(u64, &str)> = GRID
        .iter()
        .filter(|(_, _, v)| *v || !voice)
        .filter(|(b, _, _)| voice || !matches!(*b, x if x == 1 << 20 || x == 1 << 21))
        .map(|(b, name, _)| (*b, *name))
        .collect();
    let mut rows: Vec<(String, u64, u64)> = n["grid"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| Some((r[0].as_str()?.to_string(), r[1].as_u64()?, r[2].as_u64()?)))
        .collect();
    let chan = sid.get_value();
    for k in added.get() {
        if let Some(r) = k.strip_prefix(&format!("{chan} ")) {
            if !rows.iter().any(|x| x.0 == r) {
                rows.push((r.to_string(), 0, 0));
            }
        }
    }
    let name_of = move |id: &str| {
        dc.with(|d| {
            d["roles"]
                .as_array()
                .and_then(|a| a.iter().find(|r| r["id"] == id))
                .and_then(|r| r["name"].as_str().map(String::from))
        })
        .unwrap_or_else(|| id.to_string())
    };
    let shown: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let add = NodeRef::<html::Select>::new();
    let options: Vec<_> = dc
        .with(|d| arr(&d["roles"]))
        .into_iter()
        .filter(|r| grid_roles.contains(&s(&r["id"])) && !shown.contains(&s(&r["id"])))
        .map(|r| view! { <option value=s(&r["id"])>{s(&r["name"])}</option> })
        .collect();
    let head = cols.iter().map(|(_, c)| view! { <th scope="col">{*c}</th> }).collect_view();
    let body = rows
        .into_iter()
        .map(|(role, allow, deny)| {
            let rname = name_of(&role);
            let cells = cols
                .iter()
                .map(|&(bit, cname)| {
                    let (mark, state) = if allow & bit != 0 {
                        ("✓", "allowed")
                    } else if deny & bit != 0 {
                        ("✗", "denied")
                    } else {
                        ("–", "neutral")
                    };
                    let (role, label) = (role.clone(), format!("{cname} for {rname}: {state}"));
                    view! { <td><button class="link cell" class:al=allow & bit != 0 class:de=deny & bit != 0 aria-label=label on:click=move |_| { let (a, d) = cycle(allow, deny, bit); set_grid(sid.get_value(), role.clone(), a, d) }>{mark}</button></td> }
                })
                .collect_view();
            view! { <tr><th scope="row">{rname}</th>{cells}</tr> }
        })
        .collect_view();
    view! {
        <details class="adv" prop:open=move || open.get() on:toggle=move |e| open.set(event_target::<web_sys::HtmlElement>(&e).has_attribute("open"))>
            <summary>"Advanced"</summary>
            <p class="muted">"Each role's Discord permissions here: ✓ allow, ✗ deny, – neutral. Dangerous Discord permissions are not here: Brim does those with permits."</p>
            <div class="gridwrap"><table class="data matrix"><thead><tr><th></th>{head}</tr></thead><tbody>{body}</tbody></table></div>
            <div class="row2">
                <select node_ref=add aria-label="role to add to the grid"><option value="">"Add a role"</option>{options}</select>
                <button class="ghost" aria-label="add the role to the grid" on:click=move |_| { let Some(r) = add.get_untracked().map(|x| x.value()).filter(|r| !r.is_empty()) else { return }; added.update(|a| a.push(format!("{} {r}", sid.get_value()))) }>"Add"</button>
            </div>
        </details>
    }
}

/// A move into another category that waits for "Permissions after the move".
#[derive(Clone, PartialEq)]
struct MoveAsk {
    id: String,
    parent: Option<String>,
    at: usize,
    /// How open the channel is now, and the new category.
    from: u8,
    to: u8,
    /// The feature whose channel it is: it keeps its own permissions.
    fixed: Option<String>,
}

impl MoveAsk {
    /// "Keep its own" is not offered when it would leave a more open channel in Staff or stricter.
    fn can_keep(&self) -> bool {
        self.fixed.is_some() || self.to > 1 || self.from <= self.to
    }
    /// The safe default: whichever is less open.
    fn keep_by_default(&self) -> bool {
        self.fixed.is_some() || (self.can_keep() && self.from <= self.to)
    }
}

/// The ⋮ menu on touch screens and for keyboard users: drag is off on touch (plan 3.2a).
pub(crate) fn coarse_pointer() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(pointer: coarse)").ok().flatten())
        .is_some_and(|m| m.matches())
}

#[component]
pub fn ChannelSetup(guild: String) -> impl IntoView {
    let data = RwSignal::new(Value::Null);
    let dc = RwSignal::new(Value::Null);
    let sel = RwSignal::new(None::<String>);
    // (id, true when the name is edited in the right panel rather than in the tree)
    let editing = RwSignal::new(None::<(String, bool)>);
    let (drag, over) = (RwSignal::new(None::<String>), RwSignal::new(None::<String>));
    let kind_pick = NodeRef::<html::Select>::new();
    // Some(true): push stopped by issues; Some(false): confirm the push.
    let dialog = RwSignal::new(None::<bool>);
    let (msg, need) = (Msg::default(), step_up());
    let seq = StoredValue::new(0u32);
    let g = StoredValue::new(guild);
    let path = move || format!("/api/guilds/{}/channel-setup", g.get_value());
    let load = move || {
        if data.is_disposed() {
            return;
        }
        let (p, gid) = (path(), g.get_value());
        leptos::task::spawn_local(async move {
            let r = api("GET", &p, None).await;
            let d = api("GET", &format!("/api/guilds/{gid}/discord"), None).await;
            if data.is_disposed() {
                return;
            }
            match r {
                Ok(v) => data.set(v),
                Err(e) => msg.err(e),
            }
            if let Ok(d) = d {
                dc.set(d);
            }
        });
    };
    load();
    // One request at a time: two PUTs in flight could be stored in the wrong order. Edits made
    // meanwhile stay in the local draft, and the newest one is sent when the reply is back.
    let pending = RwSignal::new(false);
    let queued = StoredValue::new(false);
    let sender = StoredValue::new_local(None::<Box<dyn Fn(&'static str)>>);
    let send = move |method: &'static str| sender.with_value(|f| f.as_ref().map(|f| f(method)));
    let send_now = move |method: &'static str| {
        if pending.get_untracked() {
            queued.set_value(true);
            return;
        }
        pending.set(true);
        let n = seq.get_value() + 1;
        seq.set_value(n);
        let sent = data.with_untracked(|v| v["draft"].clone());
        let body = (method == "PUT").then(|| json!({"draft": sent.clone()}));
        let p = path();
        run_act(
            async move {
                let r = api(method, &p, body).await;
                pending.set(false);
                // The refused edit is already in the local draft: show the server's draft again.
                if r.is_err() {
                    queued.set_value(false);
                    if let Ok(v) = api("GET", &p, None).await {
                        if seq.try_get_value() == Some(n) {
                            data.set(v);
                        }
                    }
                }
                r
            },
            msg,
            need,
            move |mut v| {
                if seq.get_value() != n {
                    return;
                }
                let again = queued.get_value() && method == "PUT";
                queued.set_value(false);
                let local = data.with_untracked(|d| d["draft"].clone());
                if again && local != sent {
                    v["draft"] = local;
                    data.set(v);
                    send("PUT");
                } else {
                    data.set(v);
                }
            },
        );
    };
    sender.set_value(Some(Box::new(send_now)));
    let edit = move |f: &dyn Fn(&mut Value, &[Value])| {
        // Before the draft is loaded an edit would replace the stored one.
        if data.with_untracked(Value::is_null) {
            return;
        }
        let (mut d, t) = data.with_untracked(|v| (v["draft"].clone(), arr(&v["tree"])));
        f(&mut d, &t);
        // The next edit builds on this one before the reply is back.
        data.update_untracked(|v| v["draft"] = d);
        send("PUT");
    };
    let tree = Memo::new(move |_| arr(&data.get()["tree"]));
    // Channels | Roles | Server (plan 3.1); one draft and one Push for all three.
    let tab = RwSignal::new("channels");
    let changes = move || data.with(|v| v["changes"].as_u64().unwrap_or(0));

    let rename = move |id: String, name: String| {
        if editing.get_untracked().is_none_or(|(e, _)| e != id) {
            return;
        }
        editing.set(None);
        let name = name.trim().to_string();
        if name.is_empty()
            || find(&tree.get_untracked(), &id).is_some_and(|n| n["name"] == name.as_str())
        {
            return;
        }
        edit(&move |d: &mut Value, _: &[Value]| item(d, &id)["name"] = json!(name));
    };
    let set_delete = move |id: String, on: bool| {
        edit(&move |d: &mut Value, _: &[Value]| item(d, &id)["delete"] = json!(on))
    };
    let set_access = move |id: String, a: Value| {
        edit(&move |d: &mut Value, _: &[Value]| {
            let it = item(d, &id);
            it["access"] = a.clone();
            it["reset"] = json!(false);
            // A new level starts clean: the Advanced grid is set on top of it again.
            if let Some(o) = it.as_object_mut() {
                o.remove("grid");
            }
        })
    };
    let set_grid = move |id: String, role: String, allow: u64, deny: u64| {
        edit(&move |d: &mut Value, _: &[Value]| {
            item(d, &id)["grid"][role.as_str()] = json!([allow, deny]);
        })
    };
    // "Some roles" picked with no role yet, and the Advanced grid folded open: kept per page.
    let roles_mode = RwSignal::new(None::<String>);
    let adv_open = RwSignal::new(false);
    // Roles added to the grid with nothing set yet: "channel role" keys, so they show as a row.
    let grid_added = RwSignal::new(Vec::<String>::new());
    let reset = move |id: String| {
        edit(&move |d: &mut Value, _: &[Value]| {
            let it = item(d, &id);
            it["reset"] = json!(true);
            if let Some(o) = it.as_object_mut() {
                o.remove("access");
            }
        })
    };
    let move_to = move |id: String, parent_id: Option<String>, at: usize| {
        edit(&move |d: &mut Value, t: &[Value]| place(d, t, &id, parent_id.clone(), at))
    };
    // Into another category: ask what happens to the permissions unless both levels are the same.
    let moving = RwSignal::new(None::<MoveAsk>);
    let ask_move = move |id: String, parent_id: Option<String>, at: usize| {
        let t = tree.get_untracked();
        let Some(n) = find(&t, &id) else { return };
        let cat = parent_id.as_deref().and_then(|p| find(&t, p));
        // A new channel always takes its category's permissions: nothing to ask.
        let (Some(c), false, false) = (cat, is_cat(n), n["new"] == true) else {
            return move_to(id, parent_id, at);
        };
        if parent(&t, n) == parent_id {
            return move_to(id, parent_id, at);
        }
        let m = MoveAsk {
            id: id.clone(),
            parent: parent_id.clone(),
            at,
            from: openness(n),
            to: openness(c),
            fixed: n["fixed"].as_str().map(String::from),
        };
        // The same level (and, for some roles, the same roles): the move just happens, and an
        // earlier "Keep its own" no longer holds, so it follows the category it is now in.
        let roles = |v: &Value| {
            let mut r = strs(&v["access"]["roles"]);
            r.sort();
            r
        };
        if m.fixed.is_none() && m.from == m.to && (m.from != 2 || roles(n) == roles(c)) {
            return edit(&move |d: &mut Value, t: &[Value]| {
                place(d, t, &id, parent_id.clone(), at);
                if let Some(o) = item(d, &id).as_object_mut() {
                    o.remove("keep");
                }
            });
        }
        moving.set(Some(m));
    };
    let finish_move = move |keep: bool| {
        let Some(m) = moving.get_untracked() else { return };
        moving.set(None);
        edit(&move |d: &mut Value, t: &[Value]| {
            place(d, t, &m.id, m.parent.clone(), m.at);
            let it = item(d, &m.id);
            it["keep"] = json!(keep);
            it["reset"] = json!(!keep);
            if !keep {
                if let Some(o) = it.as_object_mut() {
                    o.remove("access");
                }
            }
        })
    };
    let create = move |kind: u8| {
        if data.with_untracked(Value::is_null) {
            return;
        }
        let t = tree.get_untracked();
        let id = data.with_untracked(|v| new_id(&v["draft"]));
        let parent_id = (kind != 4)
            .then(|| {
                sel.get_untracked().and_then(|x| {
                    find(&t, &x).and_then(|n| {
                        if is_cat(n) {
                            Some(x.clone())
                        } else {
                            parent(&t, n)
                        }
                    })
                })
            })
            .flatten()
            .filter(|p| !gone(&t, p));
        let name = match kind {
            4 => "New Category",
            2 => "New Voice",
            5 => "new-announcements",
            _ => "new-channel",
        };
        let at = siblings(&t, parent_id.as_deref(), kind == 4).len();
        {
            let id = id.clone();
            edit(&move |d: &mut Value, _: &[Value]| {
                let it = item(d, &id);
                it["kind"] = json!(kind);
                it["name"] = json!(name);
                it["parent"] = json!(parent_id);
                it["position"] = json!(at);
            });
        }
        sel.set(Some(id.clone()));
        editing.set(Some((id, true)));
    };
    let role_of = move |id: &str| {
        dc.with(|d| {
            d["roles"]
                .as_array()
                .and_then(|a| a.iter().find(|r| r["id"] == id).cloned())
        })
        .unwrap_or(Value::Null)
    };
    let role_style = |r: &Value| {
        r["color"]
            .as_u64()
            .filter(|c| *c > 0)
            .map(|c| format!("--rc:#{c:06x}"))
    };
    // The server picks the badge (owner decision 2026-11-18): none for a members' channel.
    let chips = move |n: &Value| {
        let roles = strs(&n["tag_roles"]);
        if roles.is_empty() {
            return n["tag"].as_str().map(|t| view! { <span class="mini">{t.to_string()}</span> }).into_any();
        }
        roles.iter().map(|id| { let r = role_of(id); view! { <span class="mini r" style=role_style(&r)>{r["name"].as_str().unwrap_or(id).to_string()}</span> } }).collect_view().into_any()
    };
    let name_input = move |id: String, cur: String| {
        let r = NodeRef::<html::Input>::new();
        // The value is set here too: the effect can run before `prop:value` is applied, and then there is nothing to select.
        let v = cur.clone();
        Effect::new(move || {
            if let Some(i) = r.get() {
                i.set_value(&v);
                let _ = i.focus();
                i.select();
            }
        });
        let id2 = id.clone();
        view! { <input class="nm" node_ref=r aria-label="name" maxlength="100" prop:value=cur on:click=|e| e.stop_propagation()
        on:keydown=move |e| { if crate::common::is_enter(&e) { rename(id.clone(), event_target_value(&e)) } else if e.key() == "Escape" { editing.set(None) } }
        on:blur=move |e| rename(id2.clone(), event_target_value(&e)) /> }
    };
    let acts = move |id: String, deleted: bool, panel: bool| {
        if deleted {
            return view! { <button class="link" title="Undo delete" aria-label="undo delete" on:click=move |_| set_delete(id.clone(), false)>{icon("restore")}</button> }.into_any();
        }
        let id2 = id.clone();
        view! {
            <button class="link" title="Rename" aria-label="rename" on:click=move |_| editing.set(Some((id.clone(), panel)))>{icon("edit")}</button>
            <button class="link" title="Delete" aria-label="delete" on:click=move |_| set_delete(id2.clone(), true)>{icon("trash")}</button>
        }
        .into_any()
    };

    // A drop on a row (`target`), or at the end of a category (`p`) or of the channels without one; the
    // last end also takes a category, which then goes last.
    let drop_row = move |d: String, target: &str| {
        if let Some((p, at)) = drop_target(&tree.get_untracked(), &d, target) { ask_move(d, p, at) }
    };
    let drop_slot = move |d: String, p: Option<String>, last: bool| {
        let t = tree.get_untracked();
        match find(&t, &d) {
            Some(n) if is_cat(n) => if last { move_to(d, None, siblings(&t, None, true).len()) },
            Some(_) if !p.as_deref().is_some_and(|p| gone(&t, p)) => { let at = siblings(&t, p.as_deref(), false).len(); ask_move(d, p, at) }
            _ => {}
        }
    };
    // A slot's `data-drop` is "|<category id>|<last>"; a row's is its id.
    let drop_at = move |d: String, place: &str| match place.strip_prefix('|').and_then(|r| r.split_once('|')) {
        Some((p, last)) => drop_slot(d, (!p.is_empty()).then(|| p.to_string()), last == "1"),
        None => drop_row(d, place),
    };
    let hold = HoldDrag::new();

    // `sel` and `over` are read per row: rebuilding the tree on every dragover would replace the dragged row.
    // `up`: the menu opens upwards, so on the last rows it is not cut off by the page end or the push bar.
    let row = move |n: Value, up: bool| {
        let t = tree.get_untracked();
        let id = s(&n["id"]);
        let (cat, deleted) = (is_cat(&n), n["deleted"] == true);
        let ed = editing.get().is_some_and(|(e, p)| e == id && !p);
        let issues = arr(&n["issues"]);
        let risk = issues
            .iter()
            .map(|i| s(&i["title"]))
            .collect::<Vec<_>>()
            .join(" ");
        let (i1, i2, i3, i4, i5, i6) = (
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
        );
        // Order inside its list, for Alt+Up / Alt+Down and the ⋮ menu.
        let (pv, list) = {
            let p = if cat { None } else { parent(&t, &n) };
            (p.clone(), siblings(&t, p.as_deref(), cat))
        };
        let at = list.iter().position(|x| *x == id).unwrap_or(0);
        let last = list.len().saturating_sub(1);
        let (k1, pv2) = (id.clone(), pv.clone());
        let row_label = label(&n);
        let rl = StoredValue::new(row_label.clone());
        let step = move |id: String, p: Option<String>, down: bool| {
            if (down && at < last) || (!down && at > 0) {
                move_to(id, p, if down { at + 1 } else { at - 1 });
                // The tree is rebuilt: the moved row gets the focus back, for the next Alt+Arrow.
                refocus(".cs-grid", rl.get_value());
            }
        };
        let drag_ok = !ed && !coarse_pointer();
        let menu = (!deleted).then(|| {
            let (m1, m2, m3) = (id.clone(), id.clone(), id.clone());
            let (p1, p2) = (pv.clone(), pv.clone());
            let close = |e: &leptos::ev::MouseEvent| {
                if let Some(d) = event_target::<web_sys::Element>(e).closest("details").ok().flatten() {
                    let _ = d.remove_attribute("open");
                }
            };
            view! { <details class="rowmenu" class:up=up on:click=|e| e.stop_propagation() on:touchstart=|e| e.stop_propagation() on:dragstart=|e| { e.prevent_default(); e.stop_propagation() }
                on:keydown=|e| { e.stop_propagation(); if e.key() == "Escape" { if let Some(d) = event_target::<web_sys::Element>(&e).closest("details").ok().flatten() { let _ = d.remove_attribute("open"); } } }><summary class="link" title="More" aria-label="more actions">"⋮"</summary><div>
                <button disabled={at == 0} on:click=move |e| { close(&e); step(m1.clone(), p1.clone(), false) }>"Move up"</button>
                <button disabled={at >= last} on:click=move |e| { close(&e); step(m2.clone(), p2.clone(), true) }>"Move down"</button>
                // A picker in the menu itself: focusing the panel's picker from here did nothing on iOS
                // (no focus outside a tap) and the panel sits under the whole list on a phone.
                {(!cat).then(|| {
                    let opts = t.iter().filter(|c| is_cat(c) && c["deleted"] != true).map(|c| { let cid = s(&c["id"]); let on = pv.as_deref() == Some(cid.as_str()); view! { <option value=cid selected=on>{s(&c["name"])}</option> } }).collect_view();
                    view! { <label class="mv">"Move to category"<select aria-label="move to category" on:change=move |e| {
                        if let Some(d) = event_target::<web_sys::Element>(&e).closest("details").ok().flatten() { let _ = d.remove_attribute("open"); }
                        let v = event_target_value(&e); let np = (!v.is_empty()).then_some(v);
                        // Shows the current category again: a cancelled move does not rebuild the row; a done one does.
                        event_target::<web_sys::HtmlSelectElement>(&e).set_value(pv.as_deref().unwrap_or(""));
                        let at = siblings(&tree.get_untracked(), np.as_deref(), false).len();
                        ask_move(m3.clone(), np, at)
                    }><option value="" selected=pv.is_none()>"No category"</option>{opts}</select></label> }
                })}
            </div></details> }
        });
        let name = if ed {
            name_input(id.clone(), s(&n["name"])).into_any()
        } else {
            view! { <button class="link nm">{s(&n["name"])}</button> }.into_any()
        };
        view! {
            <div class="tr" class:cat=cat class:child={!cat && parent(&t, &n).is_some()} class:sel=move || sel.get().as_deref() == Some(i6.as_str()) class:del=deleted class:risk={!issues.is_empty()} class:moving={ let id = id.clone(); move || drag.get().as_deref() == Some(id.as_str()) } class:over={ let id = id.clone(); move || over.get().as_deref() == Some(id.as_str()) }
                draggable={ if drag_ok { "true" } else { "false" } }
                on:click=move |_| sel.set(Some(i1.clone()))
                tabindex="0" aria-label=row_label
                on:keydown=move |e| {
                    let typing = event_target::<web_sys::Element>(&e).tag_name() == "INPUT";
                    if !typing && e.alt_key() && (e.key() == "ArrowUp" || e.key() == "ArrowDown") { e.prevent_default(); step(k1.clone(), pv2.clone(), e.key() == "ArrowDown") }
                }
                on:dragstart=move |e| { if let Some(dt) = e.data_transfer() { let _ = dt.set_data("text/plain", &i2); } drag.set(Some(i2.clone())) }
                on:dragover=move |e| { if drag.get_untracked().is_some() { e.prevent_default(); over.set(Some(i3.clone())) } }
                on:dragend=move |_| { drag.set(None); over.set(None) }
                on:drop=move |e| {
                    e.prevent_default();
                    over.set(None);
                    let Some(d) = drag.get_untracked() else { return };
                    drag.set(None);
                    drop_row(d, &i4)
                }
                data-drop=id.clone()
                on:touchstart={ let id = id.clone(); move |e| { let id = id.clone(); hold.start(&e, !ed && sel.get_untracked().as_deref() == Some(id.as_str()), move || drag.set(Some(id.clone()))) } }
                on:touchmove=move |e| hold.moved(&e, |o| over.set(o))
                on:touchend=move |e| { if drag.is_disposed() { return } let d = drag.get_untracked(); hold.end(&e, |to| { drag.set(None); over.set(None); if let (Some(d), Some(to)) = (d, to) { drop_at(d, &to) } }); }
                on:touchcancel=move |_| { if drag.is_disposed() { return } hold.stop(); drag.set(None); over.set(None) }>
                <span class="h" title="Drag to move">{icon("menu")}</span>
                {icon(kind_icon(n["kind"].as_u64().unwrap_or(0)))}
                {name}
                {(n["new"] == true).then(|| view! { <span class="chip blue">"New"</span> })}
                {(n["exception"] == true).then(|| view! { <span class="exc"><Tip text="Has its own settings. Category changes do not change it.".to_string() /></span> })}
                {(!issues.is_empty()).then(|| view! { <span class="risk-i"><Tip icon="warning" text=risk /></span> })}
                <span class="sp"></span>
                {chips(&n)}
                <span class="acts">{acts(i5, deleted, false)}</span>
                {menu}
            </div>
        }
    };

    // A drop slot at the end of a category (`p`) or of the channels without one; the last one also
    // takes a category, which then goes last.
    let slot = move |p: Option<String>, last: bool| {
        let place = format!("|{}|{}", p.clone().unwrap_or_default(), u8::from(last));
        view! { <div class="slot" class:on=move || drag.get().is_some() class:over={ let pl = place.clone(); move || over.get().as_deref() == Some(pl.as_str()) } aria-hidden="true" data-drop=place
        on:dragover=move |e| { if drag.get_untracked().is_some() { e.prevent_default(); over.set(None) } }
        on:drop=move |e| {
            e.prevent_default();
            let Some(d) = drag.get_untracked() else { return };
            drag.set(None);
            drop_slot(d, p.clone(), last)
        }></div> }
    };
    let rows = move || {
        let t = tree.get();
        let mut out = vec![];
        let mut group = None::<String>;
        let len = t.len();
        for (i, n) in t.into_iter().enumerate() {
            if is_cat(&n) {
                out.push(slot(group.replace(s(&n["id"])), false).into_any());
            }
            out.push(row(n, len > 4 && i + 3 >= len).into_any());
        }
        out.push(slot(group, true).into_any());
        out.collect_view()
    };

    let panel = move || {
        // Every reply rebuilds the panel; the control that was used gets the focus back.
        let had = panel_focus();
        let t = tree.get();
        let Some(n) = sel.get().and_then(|id| find(&t, &id).cloned()) else {
            return view! { <div class="card cs-panel"><p class="muted">"Pick a channel or category to see who can see it."</p></div> }.into_any();
        };
        let id = s(&n["id"]);
        let (cat, deleted, kind) = (
            is_cat(&n),
            n["deleted"] == true,
            n["kind"].as_u64().unwrap_or(0),
        );
        let ed = editing.get().is_some_and(|(e, p)| e == id && p);
        // The name box focuses itself.
        if let Some(l) = had.filter(|_| !ed) {
            refocus(".cs-panel", l);
        }
        let title = if ed {
            name_input(id.clone(), s(&n["name"])).into_any()
        } else {
            view! { <h2>{s(&n["name"])}</h2> }.into_any()
        };
        let head = view! { <div class="ttl">{title}<span class="muted">{kind_name(kind)}</span><span class="sp"></span><span class="acts">{acts(id.clone(), deleted, true)}</span></div> };
        if deleted {
            return view! { <div class="card cs-panel">{head}<p class="muted">"Deleted when you push. Undo keeps it."</p></div> }.into_any();
        }
        let acc = n["access"].clone();
        let picked = strs(&acc["roles"]);
        let staff = acc["staff"].as_bool().unwrap_or(true);
        let (can, pickable) = data.with(|v| {
            (
                v["can_unstaff"].as_bool().unwrap_or(false),
                strs(&v["pickable_roles"]),
            )
        });
        let p = parent(&t, &n);
        let list = siblings(&t, p.as_deref(), cat);
        let at = list.iter().position(|x| *x == id).unwrap_or(0);
        let last = list.len().saturating_sub(1);
        let sid = StoredValue::new(id.clone());
        let pv = StoredValue::new(p.clone());
        let cur = StoredValue::new((picked.clone(), staff));
        let role_pick = NodeRef::<html::Select>::new();
        let role_chips = picked.iter().map(|r| {
            let (role, r2) = (role_of(r), r.clone());
            let name = role["name"].as_str().unwrap_or(r).to_string();
            view! { <span class="mini r big" style=role_style(&role)>{name.clone()}" "<button class="link" aria-label=format!("remove {name}") on:click=move |_| { let (mut roles, st) = cur.get_value(); roles.retain(|x| *x != r2); set_access(sid.get_value(), access(&roles, st, true)) }>"×"</button></span> }
        }).collect_view();
        let options: Vec<_> = dc
            .with(|d| arr(&d["roles"]))
            .into_iter()
            .filter(|r| pickable.contains(&s(&r["id"])) && !picked.contains(&s(&r["id"])))
            .map(|r| view! { <option value=s(&r["id"])>{s(&r["name"])}</option> })
            .collect();
        let none_left = options.is_empty();
        // Owner decision 11-18: Information reads as "Everyone" but members only read there.
        let note = n["tag"].as_str().filter(|t| *t != "Staff").map(|t| {
            let text = match t {
                "Unverified" => "Only members who have not verified yet see it.",
                _ => "Members read it; staff write.",
            };
            view! { <p class="line muted">{text}</p> }
        });
        let follow = if cat {
            t.iter().filter(|c| c["exception"] == true && c["fixed"].is_null() && parent(&t, c).as_deref() == Some(id.as_str())).map(|c| {
                let cid = s(&c["id"]);
                view! { <p class="line"><span class="exc">{icon("info")}</span>{format!("{} has its own settings. ", label(c))}<button class="link info" on:click=move |_| reset(cid.clone())>"Reset to category"</button></p> }
            }).collect_view().into_any()
        } else if p.is_none() {
            ().into_any()
        } else if let Some(f) = n["fixed"].as_str() {
            view! { <p class="line muted">{format!("Keeps its own permissions: {f} needs it.")}</p> }.into_any()
        } else if n["exception"] == true {
            view! { <p class="line"><span class="exc">{icon("info")}</span>"Has its own settings. Category changes do not change it. "<button class="link info" on:click=move |_| reset(sid.get_value())>"Reset to category"</button></p> }.into_any()
        } else {
            view! { <p class="line muted">"Follows the category."</p> }.into_any()
        };
        let cats = (!cat).then(|| {
            let opts = t.iter().filter(|c| is_cat(c) && c["deleted"] != true).map(|c| { let cid = s(&c["id"]); let on = p.as_deref() == Some(cid.as_str()); view! { <option value=cid prop:selected=on>{s(&c["name"])}</option> } }).collect_view();
            view! { <select aria-label="category" on:change=move |e| { let v = event_target_value(&e); let np = (!v.is_empty()).then_some(v); let at = siblings(&tree.get_untracked(), np.as_deref(), false).len(); ask_move(sid.get_value(), np, at) }>
                <option value="" prop:selected=p.is_none()>"No category"</option>{opts}</select> }
        });
        // The level list (plan 3.2): one control instead of Everyone / Only these roles.
        let key = level_key(&acc);
        let show_roles = key == "roles" || roles_mode.get().as_deref() == Some(id.as_str());
        let tiers = data.with(|v| v["tiers"].clone());
        let unverified = n["tag"] == "Unverified";
        let levels = (!unverified).then(|| {
            let opts: Vec<_> = LEVELS
                .iter()
                .filter(|(k, _, _)| match *k {
                    "admins" => tiers["admins"] == true,
                    "owner" => tiers["owner"] == true,
                    _ => true,
                })
                .map(|&(k, name, tip)| {
                    let on = if show_roles { k == "roles" } else { k == key };
                    let roles = picked.clone();
                    // Admins only and Owner only hide it from part of staff: owner or extra owner only.
                    let locked = !can && matches!(k, "admins" | "owner") && !on;
                    let tip = if locked { "Only the owner or an extra owner can hide a channel from staff." } else { tip };
                    view! { <button class="ghost" class:on=on aria-label=name aria-pressed=on.to_string() title=tip disabled=locked on:click=move |_| {
                        if on { return }
                        let id = sid.get_value();
                        if k == "roles" {
                            roles_mode.set(Some(id.clone()));
                            if !roles.is_empty() { return }
                        } else {
                            roles_mode.set(None);
                        }
                        set_access(id, level_access(k));
                    }>{name}</button> }
                })
                .collect();
            view! { <div class="seg lv" role="group" aria-label="who can see this">{opts}</div> }
        });
        let grid_roles = data.with(|v| strs(&v["grid_roles"]));
        let grid = grid_view(&n, kind, sid, (adv_open, grid_added), set_grid, dc, grid_roles);
        let staff_tip = if can {
            "Staff are the roles of the Admins and Moderators permits. Turn this off to hide it from them too."
        } else {
            "Staff are the roles of the Admins and Moderators permits. Only the owner or an extra owner can hide a channel from staff."
        };
        view! { <div class="card cs-panel">
            {head}
            <div class="lbl">"Who can see this"</div>
            {levels}
            {note}
            {show_roles.then(|| view! {
                <div class="chips">{role_chips}
                    // One control in two parts: pick, then Add. Not on change (see the channel kind pick).
                    {none_left.then(|| view! { <span class="muted">"No other roles to add. Staff, member, bot and quarantine roles are set elsewhere."</span> })}
                    <select class="add" node_ref=role_pick aria-label="role to add" hidden=none_left>
                        <option value="">"Choose a role to add"</option>{options}
                    </select>
                    <button class="ghost" aria-label="add the chosen role" hidden=none_left on:click=move |_| { let Some(r) = role_pick.get_untracked().map(|s| s.value()).filter(|r| !r.is_empty()) else { return }; let (mut roles, st) = cur.get_value(); roles.push(r); set_access(sid.get_value(), access(&roles, st, true)) }>"Add"</button>
                </div>
                <div class="row2">
                    <input type="checkbox" class="sw" aria-label="staff can see it" prop:checked=staff disabled={ !can } title=(!can).then_some("Only the owner or an extra owner can change this")
                        on:change=move |e| { let (roles, _) = cur.get_value(); set_access(sid.get_value(), access(&roles, event_target_checked(&e), true)) } />
                    <span>"Staff can see it"</span><Tip text=staff_tip.to_string() />
                </div>
            })}
            {follow}
            {grid}
            <div class="lbl">"Position"</div>
            <div class="row2">
                <button class="ghost" aria-label="Move up" disabled={at == 0} on:click=move |_| move_to(sid.get_value(), pv.get_value(), at.saturating_sub(1))>{icon("arrow_up")}"Move up"</button>
                <button class="ghost" aria-label="Move down" disabled={at >= last} on:click=move |_| move_to(sid.get_value(), pv.get_value(), at + 1)>{icon("arrow_down")}"Move down"</button>
                {cats}
            </div>
        </div> }.into_any()
    };

    let fix = move || {
        let issues = data.with_untracked(|v| arr(&v["issues"]));
        dialog.set(None);
        edit(&move |d: &mut Value, _: &[Value]| {
            for i in &issues {
                apply_fix(d, &s(&i["channel"]), &i["fix"]);
            }
        });
    };
    let layout_tick = RwSignal::new(0u32);
    let push = move || {
        dialog.set(None);
        let p = path();
        // The version the user reviewed: the server refuses a push of any other draft.
        let body = data.with_untracked(|v| json!({"version": v["version"]}));
        let n = seq.get_value();
        // The dialog's Push button is gone once it closes, so a 2FA code resumes this push itself.
        let make = move || {
            let (p, body) = (p.clone(), body.clone());
            async move {
                let r = api("POST", &format!("{p}/push"), Some(body)).await;
                // Refused (unsafe or changed since): show the server's draft and what stops it.
                if r.as_ref().is_err_and(|e| e != "step_up_required") {
                    if let Ok(v) = api("GET", &p, None).await {
                        if seq.try_get_value() == Some(n) {
                            let stopped = v["issues"].as_array().is_some_and(|a| !a.is_empty());
                            data.set(v);
                            if stopped {
                                dialog.set(Some(true));
                            }
                        }
                    }
                }
                r
            }
        };
        run_resumable(
            make,
            msg,
            need,
            move |v| {
                let failed = strs(&v["failed"]);
                let done = count(&v["done"]);
                let backup = v["backup"]
                    .as_i64()
                    .map_or(String::new(), |b| format!(" Backup #{b} was taken first."));
                let layout = match &v["layout"] {
                    l if l["version"].is_number() => format!(" Saved as layout v{}.", l["version"]),
                    l if l["unsafe"].is_array() => format!(" Not saved in the server layout, it has a serious problem: {}.", strs(&l["unsafe"]).join("; ")),
                    l if l["skipped"] == true => " The server layout changed during the push, so the push is not in it: check Layout Guard.".into(),
                    l if l["failed"] == true => " The server layout could not be saved: check Layout Guard.".into(),
                    _ => String::new(),
                };
                layout_tick.update(|n| *n += 1);
                if failed.is_empty() {
                    msg.ok(format!("Pushed {done} change(s).{backup}{layout}"));
                } else {
                    msg.err(format!(
                        "Pushed {done} change(s), {} failed.{backup}\n{}",
                        failed.len(),
                        failed.join("\n")
                    ));
                }
                load();
            },
        );
    };
    // Every moved channel with what its permissions do (plan 3.2a).
    let moves = move |t: &[Value]| {
        let items = data.with_untracked(|v| v["draft"]["items"].clone());
        let list: Vec<_> = t
            .iter()
            .filter(|n| n["new"] != true && n["deleted"] != true && !is_cat(n))
            .filter(|n| items[s(&n["id"]).as_str()].get("parent").is_some())
            .map(|n| {
                let it = &items[s(&n["id"]).as_str()];
                let how = if parent(t, n).is_none() {
                    "no category, keeps its permissions"
                } else if it["keep"] == true || (it["reset"] != true && n["exception"] == true) {
                    "keeps its own permissions"
                } else {
                    "follows its category"
                };
                view! { <li>{format!("{}: {how}", label(n))}</li> }
            })
            .collect();
        (!list.is_empty()).then(|| view! { <ul class="muted">{list}</ul> })
    };
    let dlg =
        Memo::new(move |prev: Option<&Option<bool>>| dialog.get().or(prev.copied().flatten()));
    let dialog_view = move |stopped: bool| {
        let r = NodeRef::new();
        focus_on_open(r);
        let (t, issues, n) = data.with_untracked(|v| {
            (
                arr(&v["tree"]),
                arr(&v["issues"]),
                v["changes"].as_u64().unwrap_or(0),
            )
        });
        let sum = summary(&t, n);
        let body = if stopped {
            let fixable = issues.iter().any(|i| i["fix"].is_object());
            let list = issues.iter().map(|i| {
                let ch = s(&i["channel"]);
                let name = find(&t, &ch).map_or(ch, label);
                view! { <div class="issue"><b>{name}</b>" "{s(&i["title"])}<small>{s(&i["detail"])}</small></div> }
            }).collect_view();
            view! {
                <h2><span class="risk-i">{icon("warning")}</span>{format!("Push stopped: {} unsafe change{}", issues.len(), if issues.len() == 1 { "" } else { "s" })}</h2>
                <p class="muted">"Nothing was sent to Discord. Fix this first, then push again."</p>
                {list}
                <p class="muted">{format!("Also in this push: {sum}. A backup is taken first.")}</p>
                <div class="row end"><button class="ghost" on:click=move |_| dialog.set(None)>"Close"</button><button class="go" disabled={ !fixable } on:click=move |_| fix()>"Fix"</button></div>
            }.into_any()
        } else {
            view! {
                <h2>"Push to Discord?"</h2>
                <p>{format!("This push makes {sum}.")}</p>
                {moves(&t)}
                <p class="muted">"A backup is taken first, so Backups can undo it."</p>
                <div class="row end"><button class="ghost" on:click=move |_| dialog.set(None)>"Cancel"</button><button class="go" on:click=move |_| push()>"Push"</button></div>
            }.into_any()
        };
        view! { <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label={ if stopped { "Push stopped" } else { "Push to Discord" } } on:keydown=move |e| dialog_keys(&e, move || dialog.set(None))><div class="card">{body}</div></div> }
    };

    let shown_move =
        Memo::new(move |prev: Option<&Option<MoveAsk>>| moving.get().or(prev.cloned().flatten()));
    let move_view = move |m: MoveAsk| {
        let t = tree.get_untracked();
        let name = find(&t, &m.id).map(label).unwrap_or_default();
        let default = NodeRef::<html::Button>::new();
        Effect::new(move || {
            if let Some(b) = default.get() {
                let _ = b.focus();
            }
        });
        let keep_first = m.keep_by_default();
        let line = format!("Now: {}. New category: {}.", level_name(m.from), level_name(m.to));
        let body = match &m.fixed {
            Some(f) => view! {
                <p>{format!("This channel keeps its own permissions: {f} needs it.")}</p>
                <div class="row end"><button class="ghost" on:click=move |_| moving.set(None)>"Cancel"</button><button class="go" node_ref=default on:click=move |_| finish_move(true)>"OK"</button></div>
            }.into_any(),
            None => {
                let keep = m.can_keep().then(|| view! { <button class=if keep_first { "go" } else { "ghost" } node_ref=if keep_first { default } else { NodeRef::new() } on:click=move |_| finish_move(true)>"Keep its own"</button> });
                view! {
                    <p>{line}</p>
                    <p class="muted">"Use the new category's: it follows the category from now on. Keep its own: it keeps what it has now and gets the yellow mark."</p>
                    <div class="row end"><button class="ghost" on:click=move |_| moving.set(None)>"Cancel"</button>{keep}
                        <button class=if keep_first { "ghost" } else { "go" } node_ref=if keep_first { NodeRef::new() } else { default } on:click=move |_| finish_move(false)>"Use the new category's"</button></div>
                }.into_any()
            }
        };
        view! { <div class="modal" role="dialog" aria-modal="true" aria-label="Permissions after the move" on:keydown=move |e| dialog_keys(&e, move || moving.set(None))><div class="card">
            <h2>"Permissions after the move"</h2><p class="muted">{name}</p>{body}
        </div></div> }
    };

    view! {
        <div class="cs">
            <Fade when=Signal::derive(move || moving.get().is_some())>{move || shown_move.get().map(move_view)}</Fade>
            <crate::layout::LayoutGuard guild=g.get_value() msg reload=layout_tick />
            <div class="seg cs-tabs" role="tablist" aria-label="what to set up">
                {[("channels", "Channels"), ("roles", "Roles"), ("server", "Server")].map(|(k, name)| view! {
                    <button role="tab" class:on=move || tab.get() == k aria-selected=move || (tab.get() == k).to_string() on:click=move |_| tab.set(k)>{name}</button>
                })}
            </div>
            {move || match tab.get() {
                "roles" => crate::roles::roles_tab(data, edit).into_any(),
                "server" => crate::roles::server_tab(data, edit, g.get_value()).into_any(),
                _ => ().into_any(),
            }}
            <div class="cs-grid" hidden=move || tab.get() != "channels">
                <div class="card">
                    <div class="ttl"><h2>{icon("channel_text")}"Channels"</h2><span class="sp"></span>
                        // A pick plus a button: Chrome fires `change` on every arrow key of a closed select, so acting on change made a keyboard user create channels while browsing.
                        <span class="addgrp"><select class="addch" node_ref=kind_pick aria-label="kind to add" disabled=move || data.with(Value::is_null)>
                            <option value="0">"Text channel"</option><option value="2">"Voice channel"</option><option value="5">"Announcement channel"</option><option value="4">"Category"</option>
                        </select>
                        <AddButton label="Add" attr:aria-label="add it" attr:disabled=move || data.with(Value::is_null) on:click=move |_| { if let Some(k) = kind_pick.get_untracked().and_then(|s| s.value().parse().ok()) { create(k) } } /></span>
                    </div>
                    {move || if data.with(Value::is_null) { view! { <p class="loading"><Working /></p> }.into_any() } else { rows().into_any() }}
                </div>
                {panel}
            </div>
            <Fade when=Signal::derive(move || dialog.get().is_some())>{move || dlg.get().map(dialog_view)}</Fade>
            <Fade when=Signal::derive(move || { changes() > 0 })>
                <div class="bar cs-bar"><span class="dot"></span><span><b>{move || format!("{} change{}", changes(), if changes() == 1 { "" } else { "s" })}</b>" not pushed yet"</span>
                    <button class="ghost" disabled=move || pending.get() on:click=move |_| { ask(Ask::new(format!("Discard all {} change(s)?", changes())).text("Discord is not touched.").yes("Discard").danger(), move |_| { send("DELETE"); }) }>"Discard"</button>
                    <button class="go" disabled=move || pending.get() on:click=move |_| dialog.set(Some(data.with_untracked(|v| v["issues"].as_array().is_some_and(|a| !a.is_empty()))))>"Push"</button>
                </div>
            </Fade>
            <Toast msg />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_read_and_set_the_access() {
        for (k, _, _) in LEVELS.iter().filter(|(k, _, _)| *k != "roles") {
            assert_eq!(level_key(&level_access(k)), *k);
        }
        assert_eq!(level_key(&json!({"limited": true, "roles": ["9"]})), "roles");
        // Neutral -> allow -> deny -> neutral.
        assert_eq!(cycle(0, 0, 4), (4, 0));
        assert_eq!(cycle(4, 0, 4), (0, 4));
        assert_eq!(cycle(0, 4, 4), (0, 0));
    }

    #[test]
    fn a_move_defaults_to_the_less_open_choice() {
        let m = |from, to, fixed: Option<&str>| MoveAsk {
            id: "1".into(),
            parent: None,
            at: 0,
            from,
            to,
            fixed: fixed.map(String::from),
        };
        // Staff channel into a public category: keep its own.
        assert!(m(1, 5, None).keep_by_default());
        // Public channel into Staff: only the new category's is offered.
        assert!(!m(5, 1, None).can_keep() && !m(5, 1, None).keep_by_default());
        // Public channel into Information (read-only): the new category's by default, keep offered.
        assert!(m(5, 4, None).can_keep() && !m(5, 4, None).keep_by_default());
        // A feature's channel always keeps its own.
        assert!(m(5, 1, Some("Logging")).keep_by_default());
        let n = json!({"tag": "Members read only", "access": {"limited": false}});
        assert_eq!(openness(&n), 4);
        assert_eq!(openness(&json!({"access": {"limited": true, "roles": ["9"]}})), 2);
        assert_eq!(openness(&json!({"tag": "Staff", "access": {"limited": true, "roles": [], "staff": true}})), 1);
    }

    fn tree() -> Vec<Value> {
        json!([
            {"id": "r", "name": "rules", "kind": 0, "parent": null, "position": 0},
            {"id": "o", "name": "orphan", "kind": 0, "parent": "gone", "position": 1},
            {"id": "c1", "name": "TEXT", "kind": 4, "parent": null, "position": 0},
            {"id": "a", "name": "general", "kind": 0, "parent": "c1", "position": 0},
            {"id": "b", "name": "memes", "kind": 0, "parent": "c1", "position": 1},
            {"id": "c2", "name": "VOICE", "kind": 4, "parent": null, "position": 1},
            {"id": "v", "name": "General", "kind": 2, "parent": "c2", "position": 0}
        ])
        .as_array()
        .cloned()
        .unwrap()
    }

    #[test]
    fn siblings_follow_the_tree_and_treat_a_missing_category_as_none() {
        assert_eq!(siblings(&tree(), None, false), ["r", "o"]);
        assert_eq!(siblings(&tree(), None, true), ["c1", "c2"]);
        assert_eq!(siblings(&tree(), Some("c1"), false), ["a", "b"]);
    }

    #[test]
    fn place_and_drop_renumber_only_what_moved() {
        let t = tree();
        let mut d = json!({"items": {}});
        place(&mut d, &t, "b", Some("c1".into()), 0);
        assert_eq!(d["items"]["b"]["position"], 0);
        assert_eq!(d["items"]["a"]["position"], 1);
        assert!(d["items"]["b"].get("parent").is_none());
        // A channel dropped on a category goes first in it and changes parent.
        assert_eq!(drop_target(&t, "r", "c2"), Some((Some("c2".into()), 0)));
        let mut d = json!({});
        place(&mut d, &t, "r", Some("c2".into()), 0);
        assert_eq!(d["items"]["r"]["parent"], "c2");
        assert_eq!(d["items"]["v"]["position"], 1);
        // A category dropped on a channel goes before that channel's category.
        assert_eq!(drop_target(&t, "c2", "a"), Some((None, 0)));
        assert_eq!(drop_target(&t, "c2", "r"), None);
        assert_eq!(drop_target(&t, "b", "a"), Some((Some("c1".into()), 0)));
    }

    #[test]
    fn fix_merges_into_the_draft_item() {
        let mut d = json!({"items": {"new-2": {"strip": ["1"]}}});
        apply_fix(&mut d, "new-2", &json!({"strip": "1"}));
        apply_fix(&mut d, "new-2", &json!({"strip": "2"}));
        apply_fix(&mut d, "x", &json!({"access": access(&[], true, true)}));
        assert_eq!(d["items"]["new-2"]["strip"], json!(["1", "2"]));
        assert_eq!(
            d["items"]["x"],
            json!({"access": {"roles": [], "staff": true, "limited": true}})
        );
        assert_eq!(new_id(&d), "new-3");
        assert_eq!(new_id(&json!({})), "new-1");
        // Keys a push created are not used again.
        assert_eq!(new_id(&json!({"items": {}, "next": 5})), "new-5");
    }

    #[test]
    fn voice_stays_below_text_and_deleted_categories_take_nothing() {
        let mut t = tree();
        t.push(json!({"id": "w", "name": "Talk", "kind": 2, "parent": "c1", "position": 2}));
        t.push(json!({"id": "c3", "name": "OLD", "kind": 4, "parent": null, "position": 2, "deleted": true}));
        t.push(json!({"id": "z", "name": "z", "kind": 0, "parent": "c3", "position": 0, "deleted": true}));
        assert_eq!(siblings(&t, Some("c1"), false), ["a", "b", "w"]);
        // Put after the voice channel, a text channel still sorts above it.
        let mut d = json!({"items": {}});
        place(&mut d, &t, "r", Some("c1".into()), 3);
        assert_eq!(d["items"]["r"]["position"], 2);
        assert_eq!(d["items"]["w"]["position"], 3);
        // A voice channel moved up stays below the text ones.
        let mut d = json!({"items": {}});
        place(&mut d, &t, "w", Some("c1".into()), 0);
        assert_eq!(d["items"], json!({}));
        assert_eq!(drop_target(&t, "r", "c3"), None);
        assert_eq!(drop_target(&t, "r", "z"), None);
    }
}

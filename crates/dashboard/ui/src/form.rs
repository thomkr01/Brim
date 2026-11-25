use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

/// A number segment indexes an array (`custom.0.roles`).
fn get_path(v: &Value, p: &str) -> Value {
    p.split('.')
        .fold(v, |c, k| match (c, k.parse::<usize>()) {
            (Value::Array(a), Ok(i)) => a.get(i).unwrap_or(&Value::Null),
            _ => &c[k],
        })
        .clone()
}

/// Writes `x` at the dotted path, creating (or replacing non-object) intermediate objects.
fn set_path(v: &mut Value, p: &str, x: Value) {
    let mut c = v;
    for k in p.split('.') {
        if let (Some(a), Ok(i)) = (c.as_array().map(Vec::len), k.parse::<usize>()) {
            if i < a {
                c = &mut c[i];
                continue;
            }
        }
        if !c.is_object() {
            *c = json!({});
        }
        let Some(o) = c.as_object_mut() else { return };
        c = o.entry(k).or_insert(Value::Null);
    }
    *c = x;
}

/// A field's `only`/`used` condition ({path, in}): absent, or the value at `path` is one of `in`.
fn holds(v: &Value, c: &Value) -> bool {
    c.is_null()
        || c["in"]
            .as_array()
            .is_some_and(|a| a.contains(&get_path(v, c["path"].as_str().unwrap_or(""))))
}

/// An appeal question's type name and chip colour.
fn field_type(f: &Value) -> (&'static str, &'static str) {
    match (
        strs(&f["options"]).is_empty(),
        f["multi"] == true,
        f["long"] == true,
    ) {
        (false, true, _) => ("Multi choice", "orange"),
        (false, false, _) => ("Single choice", "blue"),
        (true, _, true) => ("Textarea", "green"),
        (true, _, false) => ("Text", ""),
    }
}

/// (name, seconds) for the duration unit switcher.
const UNITS: [(&str, f64); 4] = [
    ("second", 1.0),
    ("minute", 60.0),
    ("hour", 3600.0),
    ("day", 86400.0),
];

/// `3 hour(s)`: a duration in its largest whole unit.
fn span(ms: f64) -> String {
    let secs = ms / 1000.0;
    let u = best_unit(secs);
    format!("{} {}(s)", secs / UNITS[u].1, UNITS[u].0)
}

/// The schema range of a number field, in stored units; a side without a bound is infinite.
fn range(f: &Value) -> (f64, f64) {
    (
        f["min"].as_f64().unwrap_or(f64::NEG_INFINITY),
        f["max"].as_f64().unwrap_or(f64::INFINITY),
    )
}

/// The note under a number outside its range; `per` (seconds per stored unit) shows it as a duration.
fn range_error((lo, hi): (f64, f64), v: f64, per: Option<f64>) -> Option<String> {
    let show = |x: f64| per.map_or(x.to_string(), |p| span(x * p * 1000.0));
    (v < lo || v > hi).then(|| format!("Must be between {} and {}", show(lo), show(hi)))
}

/// The Join Raid page's one-line reading of its trigger settings; unset values use the bot's defaults.
fn raid_summary(cfg: &Value) -> String {
    let at = |k: &str| get_path(cfg, &format!("joinraid.{k}"));
    let num = |k: &str, d: f64| at(k).as_f64().unwrap_or(d);
    let verb = match at("action").as_str().unwrap_or("Ban") {
        "Log" => "logging",
        "Warn" => "warning",
        "Timeout" => "timing out",
        "Kick" => "kicking",
        _ => "banning",
    };
    format!(
        "If {} flagged accounts join within {}, Brim will start {verb} flagged accounts for {}.",
        num("count", 10.0),
        span(num("span_ms", 3.0 * 3_600_000.0)),
        span(num("active_ms", 1_800_000.0)),
    )
}

fn best_unit(secs: f64) -> usize {
    UNITS
        .iter()
        .rposition(|(_, m)| secs >= *m && secs % *m == 0.0)
        .unwrap_or(0)
}

pub fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn show(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        o => o.to_string(),
    }
}

/// Display name for a channel/role id (falls back to the id).
pub fn name_of(dc: &Value, key: &str, id: &str) -> String {
    items(dc, key)
        .into_iter()
        .find(|(i, _)| i == id)
        .map_or_else(|| id.to_string(), |(_, l)| l)
}

/// Wording for the stored option values; anything else just gets a capital letter.
pub(crate) fn opt_label(v: &str) -> String {
    match v {
        "Nothing" => "Keep Unverified".into(),
        "SuspiciousOnly" => "Suspicious".into(),
        "Dm" => "DMs".into(),
        "Web" => "Website".into(),
        "Button" => "Instant Access".into(),
        "Captcha" => "Brim Captcha (Channel)".into(),
        "SimpleCaptcha" => "Simple Captcha (Channel)".into(),
        _ => v.chars().next().map_or(String::new(), |c| {
            c.to_uppercase().chain(v.chars().skip(1)).collect()
        }),
    }
}

/// What the trigger action does, for the help text under an automod filter's action.
fn action_desc(a: &str) -> &'static str {
    match a {
        "timeout" => "Discord's native timeout feature used to prevent the member from sending messages or joining voice channels",
        "warn" => "issuing a warn to the member triggering this filter, multiple warns lead to an actual punishment",
        "kick" => "Discord's native kick feature",
        "ban" => "Discord's native ban feature",
        "delete" => "deleting the message",
        _ => "left to the strike system",
    }
}

/// (anti-nuke action, verb, things, per-second noun) for the limit sentences.
const NUKE_WORDS: [(&str, &str, &str, &str); 17] = [
    ("Ban", "kick/ban", "members", "removal"),
    ("RoleCreate", "create", "roles", "creation"),
    ("RoleDelete", "delete", "roles", "deletion"),
    ("ChannelCreate", "create", "channels", "creation"),
    ("ChannelDelete", "delete", "channels", "deletion"),
    (
        "ThreadDelete",
        "delete",
        "threads or forum posts",
        "deletion",
    ),
    ("WebhookCreate", "create", "webhooks", "creation"),
    ("WebhookDelete", "delete", "webhooks", "deletion"),
    ("EmojiCreate", "create", "emojis", "creation"),
    ("StickerCreate", "create", "stickers", "creation"),
    (
        "SoundboardDelete",
        "delete",
        "soundboard sounds",
        "deletion",
    ),
    ("ScheduledEventCreate", "create", "events", "creation"),
    ("IntegrationCreate", "add", "apps", "addition"),
    ("IntegrationDelete", "remove", "apps", "removal"),
    (
        "GuildUpdate",
        "change the server name or icon",
        "times",
        "change",
    ),
    (
        "VoiceDisconnect",
        "disconnect",
        "members from voice",
        "disconnect",
    ),
    (
        "VoiceMove",
        "move",
        "members between voice channels",
        "move",
    ),
];

/// Pick one of (id, label): a box that opens the whole list on focus and filters it as you type
/// (not a native datalist: Firefox shows none on focus). `none` is the label of "nothing" (id "").
/// The one picker for every channel, role and category field (ui PITFALLS "One picker").
#[component]
pub fn ListPick(
    #[prop(into)] items: Signal<Vec<(String, String)>>,
    #[prop(into)] get: Signal<String>,
    set: Callback<String>,
    none: &'static str,
    label: &'static str,
    #[prop(optional, into)] disabled: Signal<bool>,
) -> impl IntoView {
    let (open, q, hi) = (RwSignal::new(false), RwSignal::new(String::new()), RwSignal::new(0usize));
    let (field, place) = (NodeRef::<leptos::html::Input>::new(), RwSignal::new(String::new()));
    // The list is `position:fixed`, so no dialog or scrolling card clips it. It is placed from the box
    // each frame while open (follows any scroll or resize), below it, or above when there is no room.
    let follow = move || {
        let Some(el) = field.get_untracked() else { return };
        let (r, vh) = (el.get_bounding_client_rect(), window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0));
        // The page is scaled (`--z` zoom on `#app`): fixed offsets are in unscaled units, rects in scaled ones.
        let z = if el.offset_width() > 0 { (r.width() / f64::from(el.offset_width())).max(0.1) } else { 1.0 };
        let (below, above) = ((vh - r.bottom()) / z - 12.0, r.top() / z - 12.0);
        let up = below < 160.0 && above > below;
        let side = if up { format!("bottom:{}px;max-height:{}px", (vh - r.top()) / z + 4.0, above.clamp(80.0, 320.0)) } else { format!("top:{}px;max-height:{}px", r.bottom() / z + 4.0, below.clamp(80.0, 320.0)) };
        place.set(format!("left:{}px;width:{}px;{side}", r.left() / z, r.width() / z));
    };
    Effect::new(move || {
        fn run(open: RwSignal<bool>, follow: impl Fn() + Copy + 'static) {
            follow();
            request_animation_frame(move || if open.get_untracked() { run(open, follow) });
        }
        if open.get() { run(open, follow) }
    });
    let name = move |v: &str| {
        if v.is_empty() {
            return none.to_string();
        }
        items.with(|l| l.iter().find(|(i, _)| i == v).map(|(_, n)| n.clone())).unwrap_or_else(|| "not found (deleted, or not loaded)".into())
    };
    // The rows that match the search; the "nothing" row comes first when `none` is set.
    let rows = Memo::new(move |_| {
        let f = q.get().trim().to_lowercase();
        let mut v: Vec<(String, String)> = if none.is_empty() { vec![] } else { vec![(String::new(), none.to_string())] };
        v.extend(items.get());
        v.retain(|(_, n)| n.to_lowercase().contains(&f));
        v
    });
    let choose = move |id: String| {
        open.set(false);
        set.run(id)
    };
    let input = |e: &web_sys::Event| event_target::<web_sys::HtmlInputElement>(e);
    view! {
        <div class="lp">
            <input node_ref=field role="combobox" aria-expanded=move || open.get().to_string() aria-label=label autocomplete="off" prop:disabled=disabled
                placeholder=move || Some(name(&get.get())).filter(|n| !n.is_empty()).unwrap_or_else(|| "Search".into()) prop:value=move || if open.get() { q.get() } else { name(&get.get()) }
                on:focus=move |_| { q.set(String::new()); hi.set(0); open.set(true) }
                on:click=move |_| open.set(true)
                on:blur=move |_| open.set(false)
                on:input=move |e| { q.set(event_target_value(&e)); hi.set(0); open.set(true) }
                on:keydown=move |e| match e.key().as_str() {
                    "ArrowDown" => { e.prevent_default(); open.set(true); hi.update(|h| *h = (*h + 1).min(rows.with(Vec::len).saturating_sub(1))) }
                    "ArrowUp" => { e.prevent_default(); hi.update(|h| *h = h.saturating_sub(1)) }
                    "Enter" => { e.prevent_default(); if let Some((id, _)) = rows.with(|r| r.get(hi.get_untracked()).cloned()).filter(|_| open.get_untracked()) { choose(id); let _ = input(&e.into()).blur(); } }
                    "Escape" => { if open.get_untracked() { e.stop_propagation() } open.set(false); let _ = input(&e.into()).blur(); }
                    _ => {}
                } />
            <Show when=move || open.get()>
                <ul class="lpl" role="listbox" aria-label=label style=move || place.get()>
                    {move || rows.with(Vec::is_empty).then(|| view! { <li class="muted">"No match"</li> })}
                    {move || rows.get().into_iter().enumerate().map(|(i, (id, n))| {
                        let on = id == get.get_untracked();
                        view! { <li role="option" aria-selected=on.to_string() class:hi=move || hi.get() == i
                            // mousedown, not click: the box loses focus (and the list closes) before a click lands.
                            on:mousedown=move |e| { e.prevent_default(); choose(id.clone()) }>{n}</li> }
                    }).collect_view()}
                </ul>
            </Show>
        </div>
    }
}

/// One member (an ID string) through the shared member picker: the newest pick replaces the old one.
pub fn one_member(id: RwSignal<String>) -> impl IntoView {
    view! { <UserPick get=Signal::derive(move || { let v = id.get(); if v.is_empty() { json!([]) } else { json!([v]) } })
        set=Callback::new(move |v: Value| id.set(strs(&v).pop().unwrap_or_default())) /> }
}

/// (id, label) pairs from `/discord` for `key`: "channels" (no categories), "categories" or "roles".
pub(crate) fn items(dc: &Value, key: &str) -> Vec<(String, String)> {
    let (list, pre) = match key {
        "roles" => ("roles", "@"),
        "categories" => ("channels", ""),
        _ => ("channels", "#"),
    };
    dc[list]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|x| list == "roles" || (x["kind"] == "category") == (key == "categories"))
                .map(|x| {
                    (
                        x["id"].as_str().unwrap_or("").to_string(),
                        // @everyone's name already starts with "@".
                        format!(
                            "{pre}{}",
                            x["name"].as_str().unwrap_or("?").trim_start_matches(pre)
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Member select: chips of the picked users, a search box that also takes a pasted ID.
#[component]
pub fn UserPick(get: Signal<Value>, set: Callback<Value>) -> impl IntoView {
    let g = StoredValue::new(crate::hash().into_iter().next().unwrap_or_default());
    let names = RwSignal::new(std::collections::HashMap::<String, String>::new());
    let found = RwSignal::new(Vec::<(String, String)>::new());
    let learn = move |v: &Value| {
        let l: Vec<(String, String)> = v
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| (show(&m["id"]), show(&m["name"])))
            .collect();
        names.update(|n| n.extend(l.clone()));
        l
    };
    let ids = strs(&get.get_untracked());
    if !ids.is_empty() {
        spawn_local(async move {
            if let Ok(v) = api(
                "GET",
                &format!(
                    "/api/guilds/{}/members?ids={}",
                    g.get_value(),
                    ids.join(",")
                ),
                None,
            )
            .await
            {
                learn(&v);
            }
        });
    }
    let add = move |id: String| {
        let mut cur = strs(&get.get_untracked());
        if !cur.contains(&id) {
            cur.push(id);
        }
        set.run(json!(cur));
        found.set(vec![]);
    };
    let seq = StoredValue::new(0u32);
    let search = move |q: String| {
        let q = q.trim().to_string();
        if q.len() >= 17 && q.bytes().all(|b| b.is_ascii_digit()) {
            return add(q);
        }
        seq.update_value(|n| *n += 1);
        let mine = seq.get_value();
        if q.is_empty() {
            return found.set(vec![]);
        }
        spawn_local(async move {
            let r = api(
                "GET",
                &format!(
                    "/api/guilds/{}/members?q={}",
                    g.get_value(),
                    js_sys::encode_uri_component(&q)
                ),
                None,
            )
            .await;
            // An answer for an older search is dropped.
            if let (Ok(v), Some(true)) = (r, seq.try_get_value().map(|n| n == mine)) {
                found.set(learn(&v));
            }
        });
    };
    view! { <div class="pick">
        {move || strs(&get.get()).into_iter().map(|id| {
            let label = names.with(|n| n.get(&id).cloned()).unwrap_or_else(|| id.clone());
            view! { <span class="chip">{label}" "<button class="link" aria-label="remove" on:click=move |_| { let mut cur = strs(&get.get_untracked()); cur.retain(|x| *x != id); set.run(json!(cur)) }>"×"</button></span> }
        }).collect_view()}
        <input placeholder="Search a member or paste an ID" aria-label="search a member or paste an ID" on:change=move |e| search(event_target_value(&e)) />
        {move || found.get().into_iter().map(|(id, name)| view! { <button class="ghost" on:click=move |_| add(id.clone())>{name}</button> }).collect_view()}
    </div> }
}

/// Customize list over a `{key: channel}` object (review): a search box, "Set/Remove channel for
/// all types", folded categories with "Set category channel", and a tick box per type. Ticked types get one
/// channel with Apply. A type is unset by dropping its key.
#[component]
fn RoutePick(
    f: Value,
    get: Signal<Value>,
    set: Callback<Value>,
    dc: RwSignal<Value>,
) -> impl IntoView {
    let opts = StoredValue::new(f["options"].as_array().cloned().unwrap_or_default());
    let (query, picked, channel) = (
        RwSignal::new(String::new()),
        RwSignal::new(Vec::<String>::new()),
        RwSignal::new(String::new()),
    );
    // The types a channel is being chosen for (all, one category or the ticked ones); None while closed.
    let target = RwSignal::new(None::<Vec<String>>);
    let keys_of = move |group: Option<String>| -> Vec<String> {
        opts.with_value(|o| {
            o.iter()
                .filter(|x| group.as_ref().is_none_or(|g| show(&x["group"]) == *g))
                .map(|x| show(&x["key"]))
                .collect()
        })
    };
    let groups = move || {
        let mut g = Vec::<String>::new();
        opts.with_value(|o| {
            for x in o {
                let n = show(&x["group"]);
                if !g.contains(&n) {
                    g.push(n)
                }
            }
        });
        g
    };
    let title = move |k: &str| {
        opts.with_value(|o| {
            o.iter()
                .find(|x| x["key"] == k)
                .map_or_else(|| k.to_string(), |x| show(&x["title"]))
        })
    };
    let write = move |keys: Vec<String>, ch: Option<String>| {
        let mut cur = get.get_untracked();
        if !cur.is_object() {
            cur = json!({})
        }
        for k in keys {
            match &ch {
                Some(c) => cur[k.as_str()] = json!(c),
                None => {
                    if let Some(o) = cur.as_object_mut() {
                        o.remove(&k);
                    }
                }
            }
        }
        set.run(cur)
    };
    let close = move || {
        target.set(None);
        channel.set(String::new())
    };
    let apply = move |_| {
        if let Some(keys) = target.get_untracked() {
            write(keys.clone(), Some(channel.get_untracked()));
            picked.update(|p| p.retain(|k| !keys.contains(k)));
        }
        close();
    };
    let remove_all = move |_| {
        ask(
            Ask::new("Remove the channel for all types?")
                .text("Every type goes back to the default logs channel.")
                .yes("Remove")
                .danger(),
            move |_| {
                picked.set(vec![]);
                write(keys_of(None), None)
            },
        )
    };
    let matches = move |x: &Value| {
        let q = query.get().to_lowercase();
        q.is_empty() || show(&x["title"]).to_lowercase().contains(&q) || show(&x["key"]).to_lowercase().contains(&q)
    };
    let r = NodeRef::<leptos::html::Div>::new();
    focus_on_open(r);
    view! { <div class="grow logcats">
        <input type="search" aria-label="search log types" placeholder="Search log types" prop:value=move || query.get() on:input=move |e| { picked.set(vec![]); query.set(event_target_value(&e)) } />
        <div class="row">
            <button class="ghost" disabled=move || !query.get().is_empty() title=move || (!query.get().is_empty()).then_some("Clear the search first") on:click=move |_| target.set(Some(keys_of(None)))>"Set channel for all types"</button>
            <button class="ghost" disabled=move || !query.get().is_empty() title=move || (!query.get().is_empty()).then_some("Clear the search first") on:click=remove_all>"Remove channel for all types"</button>
        </div>
        <Show when=move || !picked.get().is_empty()>
            <div class="row"><span class="muted">{move || format!("{} selected", picked.get().len())}</span>
                <button on:click=move |_| target.set(Some(picked.get_untracked()))>"Set channel"</button>
                <button class="ghost" on:click=move |_| picked.set(vec![])>"Clear"</button></div>
        </Show>
        {move || groups().into_iter().map(|g| {
            let rows: Vec<Value> = opts.with_value(|o| o.iter().filter(|x| show(&x["group"]) == g && matches(x)).cloned().collect());
            if rows.is_empty() { return ().into_any(); }
            let (g1, g2) = (g.clone(), g.clone());
            let set_count = move || { let ks = keys_of(Some(g1.clone())); get.with(|v| ks.iter().filter(|k| v[k.as_str()].is_string()).count()) };
            let total = keys_of(Some(g.clone())).len();
            view! { <details class="card logcat" open=!query.get_untracked().is_empty()>
                <summary><b>{g.clone()}</b><span class="chip">{move || format!("{} / {total} set", set_count())}</span><span class="sp"></span>
                    <button class="ghost" on:click=move |e| { e.prevent_default(); e.stop_propagation(); target.set(Some(keys_of(Some(g2.clone())))) }>"Set category channel"</button></summary>
                {rows.into_iter().map(|x| {
                    let k = show(&x["key"]);
                    let (k1, k2, k3, k4) = (k.clone(), k.clone(), k.clone(), k.clone());
                    view! { <div class="row logrow">
                        <label><input type="checkbox" aria-label=format!("select {}", show(&x["title"])) prop:checked=move || picked.get().contains(&k1) on:change=move |e| picked.update(|p| { p.retain(|y| *y != k2); if event_target_checked(&e) { p.push(k2.clone()) } }) /> <b>{show(&x["title"])}</b></label>
                        {x["help"].as_str().map(|h| view! { <crate::common::Tip text=h.to_string() /> })}
                        <span class="sp"></span>
                        {move || { let (k, k4) = (k3.clone(), k4.clone()); get.with(|v| v[k.as_str()].is_string()).then(move || { view! { <span class="chip">{move || name_of(&dc.get(), "channels", &show(&get.get()[k.as_str()]))}</span><button class="link err" aria-label="remove channel" on:click=move |_| { picked.update(|p| p.retain(|y| *y != k4)); write(vec![k4.clone()], None) }>"Remove"</button> } }) }}
                    </div> }
                }).collect_view()}
            </details> }.into_any()
        }).collect_view()}
        <Show when=move || target.get().is_some()>
            <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Pick a channel" on:keydown=move |e| dialog_keys(&e, close)><div class="card">
                <b>{move || { let t = target.get().unwrap_or_default(); if t.len() <= 3 { t.iter().map(|k| title(k)).collect::<Vec<_>>().join(", ") } else { format!("{} types", t.len()) } }}</b>
                <div class="row"><label>"Pick a channel"</label><ListPick items=Signal::derive(move || items(&dc.get(), "channels")) get=channel set=Callback::new(move |v| channel.set(v)) none="choose a channel" label="channel" /></div>
                <div class="row"><button class="ghost" on:click=move |_| close()>"Abort"</button><button disabled=move || channel.get().is_empty() on:click=apply>"Apply"</button></div>
            </div></div>
        </Show>
    </div> }
}

#[component]
pub fn Field(f: Value, val: RwSignal<Value>, dc: RwSignal<Value>) -> impl IntoView {
    let path = f["path"].as_str().unwrap_or("").to_string();
    let kind = f["kind"].as_str().unwrap_or("text").to_string();
    let p = StoredValue::new(path.clone());
    let default = StoredValue::new(f["default"].clone());
    let get = move || {
        let v = get_path(&val.get(), &p.get_value());
        if v.is_null() {
            default.get_value()
        } else {
            v
        }
    };
    // An emptied number or text field goes back to its default: most of these settings cannot be null,
    // and the save was refused with "invalid type: null".
    let numeric = matches!(kind.as_str(), "int" | "float" | "pct");
    let text = kind == "text";
    let set = move |x: Value| {
        let x = if (numeric || text) && x.is_null() {
            default.get_value()
        } else {
            x
        };
        val.update(|v| set_path(v, &p.get_value(), x))
    };
    let tail = path.rsplit('.').next().unwrap_or("").to_string();
    let dur = kind == "int" && (tail.ends_with("_secs") || tail.ends_with("_ms"));
    // Typed values outside the range stay (with a note, and the server refuses to save them); +/- stop at the bounds.
    let (lo, fixed_hi) = range(&f);
    // A heat weight goes up to the max heat set in the same section.
    let max_path = StoredValue::new(f["max_path"].as_str().map(str::to_string));
    let hi = move || {
        max_path
            .get_value()
            .and_then(|m| get_path(&val.get(), &m).as_f64())
            .unwrap_or(fixed_hi)
    };
    let num = move || get().as_f64().unwrap_or(0.0);
    // Without a default, empty means off: - at the bottom of the range empties the field again.
    let at_floor = move || default.get_value().is_null() && !get().is_null() && num() == lo;
    // A single category picker is the channel picker listing categories.
    let input = match if kind == "category" { "channel" } else { kind.as_str() } {
        "bool" => {
            // `confirm_off` (schema): turning the switch off asks first; Cancel leaves it on.
            let confirm = f["confirm_off"].as_str().map(String::from);
            view! { <input type="checkbox" class="sw" prop:checked=move || get().as_bool().unwrap_or(false) on:change=move |e| {
                let on = event_target_checked(&e);
                match (&confirm, on) {
                    (Some(text), false) => {
                        event_target::<web_sys::HtmlInputElement>(&e).set_checked(true);
                        ask(Ask::new("Turn this off?").text(text.clone()).yes("Turn off").danger(), move |_| set(json!(false)));
                    }
                    _ => set(json!(on)),
                }
            } /> }.into_any()
        }
        "int" if dur => {
            let per = if tail.ends_with("_ms") { 0.001 } else { 1.0 }; // seconds per stored unit
            let secs = move || get().as_f64().unwrap_or(0.0) * per;
            let chosen = RwSignal::new(None::<usize>);
            let unit = move || chosen.get().unwrap_or_else(|| best_unit(secs()));
            let set_n = move |n: f64, clamp: bool| {
                let v = (n.max(0.0) * UNITS[unit()].1 / per).round();
                let v = if clamp { v.clamp(lo, hi()) } else { v } as i64;
                set(if v == 0 && default.get_value().is_null() { Value::Null } else { json!(v) });
            };
            let bad = RwSignal::new(false);
            view! { <div class="step"><button class="ghost" aria-label="less" disabled=move || num() <= lo && !at_floor() on:click=move |_| if at_floor() { set(Value::Null) } else { set_n(secs() / UNITS[unit()].1 - 1.0, true) }>"-"</button>
                <input prop:value=move || format!("{}", secs() / UNITS[unit()].1) on:change=move |e| { let t = event_target_value(&e); let x = t.trim().parse::<f64>(); bad.set(x.is_err() && !t.trim().is_empty()); if let Ok(x) = x { set_n(x, false) } } />
                <button class="ghost" aria-label="more" disabled=move || hi() <= num() on:click=move |_| set_n(secs() / UNITS[unit()].1 + 1.0, true)>"+"</button>
                <select aria-label="unit" on:change=move |e| chosen.set(UNITS.iter().position(|(u, _)| *u == event_target_value(&e)))>
                    {UNITS.iter().map(|(u, _)| view! { <option value=*u prop:selected=move || UNITS[unit()].0 == *u>{*u}</option> }).collect_view()}
                </select></div>
                <span class="chip">{move || { let s = secs(); let u = best_unit(s); let n = s / UNITS[u].1; format!("{n} {}{}", UNITS[u].0, if n == 1.0 { "" } else { "s" }) }}</span>
                {not_a_number(bad)}
                {move || range_error((lo, hi()), num(), Some(per)).map(|e| view! { <span class="err">{e}</span> })} }.into_any()
        }
        "int" | "float" | "pct" => {
            let float = kind != "int";
            // "pct" is stored as a fraction (0.05) and shown as a percent (5).
            let k = if kind == "pct" { 100.0 } else { 1.0 };
            // Floats print through f64, so a stepped 100.0 shows as 100.
            let shown = move || if !float { show(&get()) } else { get().as_f64().map_or(String::new(), |x| format!("{}", (x * k * 1e6).round() / 1e6)) };
            let step = move |d: f64| {
                let x = (get().as_f64().unwrap_or(0.0) * k + d).clamp(lo * k, hi() * k);
                // Rounded, so 0.01 + 1 - 1 is 0.01 again and not 0.010000000000000009.
                // Without a default, empty means off; stepping back down to 0 empties it again, like the duration field.
                // (Join Raid "ID flag" at 0 would flag every joiner in a raid.)
                if x == 0.0 && default.get_value().is_null() { return set(Value::Null) }
                set(if k != 1.0 { json!((x / k * 1e6).round() / 1e6) } else if float { json!((x * 1e6).round() / 1e6) } else { json!(x as i64) })
            };
            let unit = match path.rsplit('.').next().unwrap_or("") { n if n.ends_with("_secs") => "seconds", n if n.ends_with("_ms") => "ms", n if n.ends_with("_days") => "days", n if n.ends_with("points") => "Points", n if n.ends_with("_pct") || n == "heat" || kind == "pct" => "%", "per_minute" => "/min", "per_hour" => "/hour", "width" | "height" | "font_size" => "Pixels", "length" => "Characters", _ => "" };
            let bad = RwSignal::new(false);
            view! { <div class="step"><button class="ghost" aria-label="less" disabled=move || num() <= lo && !at_floor() on:click=move |_| if at_floor() { set(Value::Null) } else { step(-1.0) }>"-"</button>
                <input prop:value=shown on:change=move |e| {
                    let s = event_target_value(&e);
                    let s = s.trim();
                    bad.set(false);
                    if s.is_empty() { set(Value::Null) }
                    else if float { match s.parse::<f64>() { Ok(x) => set(json!(x / k)), Err(_) => bad.set(true) } }
                    else { match s.parse::<i64>() { Ok(x) => set(json!(x)), Err(_) => bad.set(true) } }
                } />
                <button class="ghost" aria-label="more" disabled=move || hi() <= num() on:click=move |_| step(1.0)>"+"</button>{(!unit.is_empty()).then(|| view! { <span>{unit}</span> })}</div>
                {not_a_number(bad)}
                {move || range_error((lo, hi()), num(), None).map(|e| view! { <span class="err">{e}</span> })} }.into_any()
        }
        "list" | "lines" => {
            // A comma box would split an entry that holds a comma (an imported phrase): such a list
            // shows one entry per line instead.
            let is_lines = kind == "lines";
            let lines = Memo::new(move |_| is_lines || strs(&get()).iter().any(|x| x.contains(',')));
            let box_ = move || if lines.get() {
                view! { <textarea class="ta-m" prop:value=move || strs(&get()).join("\n") on:change=move |e| set(json!(event_target_value(&e).lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>())) /> }.into_any()
            } else {
                view! { <input prop:value=move || csv(&get()) on:change=move |e| set(from_csv(&event_target_value(&e))) /> }.into_any()
            };
            let export = move || format!("data:text/plain;charset=utf-8,{}", js_sys::encode_uri_component(&strs(&get()).join("\n")));
            // Merges a text file (one entry per line, `#` comments skipped) into the list.
            let import = move |e: leptos::ev::Event| {
                let Some(file) = event_target::<web_sys::HtmlInputElement>(&e).files().and_then(|l| l.get(0)) else { return };
                spawn_local(async move {
                    let Ok(text) = wasm_bindgen_futures::JsFuture::from(file.text()).await else { return };
                    if p.is_disposed() { return }
                    let mut cur = strs(&get());
                    for l in text.as_string().unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                        if !cur.iter().any(|c| c == l) { cur.push(l.to_string()) }
                    }
                    set(json!(cur));
                });
            };
            view! { <div class="fullrow">{box_}<div class="io">
                <a class="ghost" download=format!("{tail}.txt") href=export>"Export"</a>
                <label class="ghost">"Import"<input type="file" accept=".txt,text/plain" hidden=true on:change=import /></label>
            </div></div> }.into_any()
        }
        "color" => view! { <input type="color" prop:value=move || show(&get()) on:change=move |e| set(json!(event_target_value(&e))) /> }.into_any(),
        // Pictures to click (the giveaway banner): each option is a picture, "none" has no picture.
        "choice" if f["pictures"] == true => {
            let opts = f["options"].as_array().cloned().unwrap_or_default();
            view! { <div class="pics" role="radiogroup" aria-label="Banner">
                {opts.iter().map(|o| { let v = show(o); let on = { let v = v.clone(); move || show(&get()) == v };
                    let (c, n) = (v.clone(), v.clone());
                    let inner = if v == "none" { view! { <span>"No banner"</span> }.into_any() } else { view! { <img src=format!("/giveaway-{n}.png") alt=format!("Banner {}", n.to_uppercase()) loading="lazy" /> }.into_any() };
                    view! { <button type="button" class="pic" role="radio" aria-checked={ let on = on.clone(); move || on().to_string() } class:on=on on:click=move |_| set(json!(c))>{inner}</button> } }).collect_view()}
            </div> }.into_any()
        }
        "choice" => {
            let opts = f["options"].as_array().cloned().unwrap_or_default();
            let none = f["none_label"].as_str().unwrap_or("Off").to_string();
            view! { <select on:change=move |e| { let s = event_target_value(&e); set(if s.is_empty() { Value::Null } else { json!(s) }) }>
                {opts.iter().map(|o| { let v = show(o); let l = if v.is_empty() { none.clone() } else { opt_label(&v) };
                    view! { <option value=v.clone() prop:selected=move || show(&get()) == v>{l}</option> } }).collect_view()}
            </select> }.into_any()
        }
        "form" => {
            let upd = move |i: usize, k: &'static str, x: Value| { let mut cur = get(); cur[i][k] = x; set(cur) };
            let count = move || get().as_array().map_or(0, Vec::len);
            // A ticket panel's form is one Discord modal: fewer fields and no choice questions.
            let key = if f["limit"] == "panel_fields" { "panel_fields" } else { "appeal_fields" };
            let choices = f["choices"] != false;
            view! { <div class="pick">
                <div class="row">
                    <AddButton label="Add new field" attr:disabled=move || !room(count(), key) on:click=move |_| { let mut cur = get().as_array().cloned().unwrap_or_default(); cur.push(json!({"label": "New question", "long": true, "required": false, "min": 4, "max": 500})); set(json!(cur)) } />
                    <Count n=Signal::derive(count) key />
                    <button class="danger" on:click=move |_| set(json!([]))>"Delete all fields"</button>
                </div>
                {move || (0..count()).map(|i| view! { <div class="row">
                    <input prop:value=move || get()[i]["label"].as_str().unwrap_or("").to_string() on:change=move |e| upd(i, "label", json!(event_target_value(&e))) />
                    <Show when=move || get()[i]["required"].as_bool().unwrap_or(true)><b class="err">"*"</b></Show>
                    {move || { let (label, color) = field_type(&get()[i]); view! { <span class=format!("chip {color}")>{label}</span> } }}
                    <Show when=move || get()[i]["label"] == "Appeal Message"><span class="chip">"default"</span></Show>
                    <label><input type="checkbox" prop:checked=move || get()[i]["long"].as_bool().unwrap_or(false) on:change=move |e| upd(i, "long", json!(event_target_checked(&e))) /> "long"</label>
                    {choices.then(|| view! {
                    <input aria-label="choices, comma separated" placeholder="choices (comma separated)" prop:value=move || strs(&get()[i]["options"]).join(", ") on:change=move |e| upd(i, "options", json!(event_target_value(&e).split(',').map(str::trim).filter(|o| !o.is_empty()).collect::<Vec<_>>())) />
                    <label><input type="checkbox" prop:checked=move || get()[i]["multi"].as_bool().unwrap_or(false) on:change=move |e| upd(i, "multi", json!(event_target_checked(&e))) /> "multi"</label> })}
                    <input aria-label="min length" placeholder="min" prop:value=move || show(&get()[i]["min"]) on:change=move |e| upd(i, "min", event_target_value(&e).trim().parse::<u32>().map_or(Value::Null, |x| json!(x))) />
                    <input aria-label="max length" placeholder="max" prop:value=move || show(&get()[i]["max"]) on:change=move |e| upd(i, "max", event_target_value(&e).trim().parse::<u32>().map_or(Value::Null, |x| json!(x))) />
                    <label><input type="checkbox" prop:checked=move || get()[i]["required"].as_bool().unwrap_or(true) on:change=move |e| upd(i, "required", json!(event_target_checked(&e))) /> "required"</label>
                    <button class="ghost" on:click=move |_| { let mut cur = get(); if let Some(a) = cur.as_array_mut() { a.remove(i); } set(cur) }>"Remove"</button>
                </div> }).collect_view()}
                <details class="card"><summary>"Preview: what members see"</summary>
                    {move || { let fs = get().as_array().cloned().unwrap_or_default(); let answers = RwSignal::new(vec![String::new(); fs.len()]); crate::appeals::questions(fs, answers) }}
                </details>
            </div> }.into_any()
        }
        "panels" => {
            // Each panel's settings are ordinary fields at `<path>.<i>.<key>`. A memo, so typing does not
            // rebuild the list and close the open panel.
            let count = Memo::new(move |_| get().as_array().map_or(0, Vec::len));
            // The button in #open-a-ticket is the first panel: its four settings up front, with the
            // message as members see it. No panel yet: the bot's default ones (`default_panels`).
            let first = move |key: &'static str, kind: &'static str, title: &'static str, help: &'static str, extra: Value| {
                let mut f = json!({"path": format!("{}.0.{key}", p.get_value()), "kind": kind, "title": title, "help": help});
                for (k, v) in extra.as_object().into_iter().flatten() { f[k] = v.clone() }
                view! { <Field f val dc /> }
            };
            // The default panels come from the schema (core `default_panels`).
            let fallback = StoredValue::new(f["panel_default"].clone());
            let guild = crate::hash().first().cloned().unwrap_or_default();
            let entry = format!("#/{guild}/bot_messages/ticket.panel");
            // The default panels, less a ticket type the server removed (the save would refuse it).
            let types_path = StoredValue::new(match p.get_value().rsplit_once('.') {
                Some((head, _)) => format!("{head}.types"),
                None => "types".into(),
            });
            let start = move |_| {
                let types = get_path(&val.get_untracked(), &types_path.get_value());
                let mut ps = fallback.get_value();
                if let Some(list) = types.as_array() {
                    for p in ps.as_array_mut().into_iter().flatten() {
                        if p["kind"].as_str().is_some_and(|k| !list.iter().any(|t| t == k)) {
                            p.as_object_mut().map(|o| o.remove("kind"));
                        }
                    }
                }
                set(ps)
            };
            let add = move |_| {
                let mut cur = get().as_array().cloned().unwrap_or_default();
                let id = (1..).map(|n| format!("panel-{n}")).find(|id| !cur.iter().any(|p| p["id"] == id.as_str())).unwrap_or_default();
                // Each new button is posted in #open-a-ticket: a label members understand, no category (the ticket category).
                cur.push(json!({"id": id, "name": fallback.with_value(|f| f[0]["name"].clone()), "emoji": "{e:ticket}", "enabled": true, "style": "primary", "per_member": 1}));
                set(json!(cur))
            };
            view! { <div class="fullrow">
                <div class="card">
                    <h3>"The ticket button"</h3>
                    <p class="muted">"Members press it in #open-a-ticket. Save, and Brim edits the posted message."</p>
                    <p class="muted">"The message's title, text and color are on Default messages."</p>
                    // One row, so the two buttons keep the row gap and do not touch.
                    <div class="row">{move || (count.get() == 0).then(|| view! { <button on:click=start>"Edit the ticket button"</button> })}</div>
                    {move || if count.get() == 0 {
                        ().into_any()
                    } else {
                        view! {
                            {first("enabled", "bool", "Members can open tickets with this button", "Off: the button is left out of the message.", json!({"default": true}))}
                            {first("name", "text", "Button label", "1-80 characters.", json!({"default": ""}))}
                            {first("emoji", "text", "Button emoji", "One standard emoji, a Brim emoji like {e:ticket}, or empty. Server emoji are not supported.", json!({"default": ""}))}
                            {first("style", "choice", "Button color", "", json!({"default": "primary", "options": ["primary", "secondary", "success", "danger"]}))}
                        }.into_any()
                    }}
                    <TicketPreview guild=guild.clone() val greeting=false edit=entry.clone() />
                </div>
                <details class="card"><summary>"More buttons and forms"</summary>
                <p class="muted">"Each extra panel adds a button, with its own form, greeting, category and pings."</p>
                {move || (0..count.get()).map(|i| {
                    let at = format!("{}.{i}", p.get_value());
                    let sub = move |key: &str, kind: &str, title: &str, help: &str, extra: Value| {
                        let mut f = json!({"path": format!("{at}.{key}"), "kind": kind, "title": title});
                        if !help.is_empty() { f["help"] = json!(help) }
                        for (k, v) in extra.as_object().into_iter().flatten() { f[k] = v.clone() }
                        view! { <Field f val dc /> }
                    };
                    view! { <details class="card">
                        <summary>{move || { let p = get(); let off = if p[i]["enabled"] == false { " (off)" } else { "" }; format!("{} {}{off}", show(&p[i]["emoji"]), show(&p[i]["name"])).trim_start().to_string() }}</summary>
                        {sub("id", "text", "Panel ID", "1-32 characters: a-z, 0-9 and -. Unique per server. Posted buttons use it, so publish again after you change it.", json!({"default": ""}))}
                        {sub("name", "text", "Name", "The button or menu label, 1-80 characters.", json!({"default": ""}))}
                        {sub("enabled", "bool", "Members can open this panel", "", json!({"default": true}))}
                        {sub("emoji", "text", "Emoji", "One standard emoji on the button, a Brim emoji like {e:ticket}, or empty. Server emoji are not supported.", json!({"default": ""}))}
                        {sub("style", "choice", "Button color", "", json!({"default": "primary", "options": ["primary", "secondary", "success", "danger"]}))}
                        {sub("description", "text", "Description", "Shown under the name in the menu and the panel message, up to 100 characters.", json!({"default": ""}))}
                        {sub("category", "category", "Ticket category", "Where this panel's tickets go. None: the ticket category of the General card.", Value::Null)}
                        {sub("per_member", "int", "Open tickets per member", "", json!({"default": 1, "min": 1, "max": 5}))}
                        {sub("greeting", "text", "Greeting", "Posted in the new ticket, up to 2000 characters. Empty: the server's greeting message is posted. Tags: {member.username} {member.id} {member} {tk_type}", json!({"default": ""}))}
                        {sub("ping_roles", "roles", "Roles pinged on a new ticket", "At most 10.", Value::Null)}
                        {sub("kind", "text", "Ticket type", "Its tickets start with this type, one from Ticket types. Empty: the member picks one, or staff at claim.", json!({"default": ""}))}
                        {sub("fields", "form", "Form questions", "Asked before the ticket opens, at most 5, with labels of 1-45 characters and a length of at most 1024. The answers go only to the ticket channel.", json!({"limit": "panel_fields", "choices": false}))}
                        <button class="ghost" on:click=move |_| { let mut cur = get(); if let Some(a) = cur.as_array_mut() { a.remove(i); } set(cur) }>"Remove panel"</button>
                    </details> }
                }).collect_view()}
                <div class="row">
                    <AddButton label="Add panel" attr:disabled=move || !room(count.get(), "ticket_panels") on:click=add />
                    <Count n=count key="ticket_panels" />
                </div>
                </details>
            </div> }.into_any()
        }
        "warn_actions" => {
            let upd = move |i: usize, k: &'static str, x: Value| { let mut cur = get(); cur[i][k] = x; set(cur) };
            let count = move || get().as_array().map_or(0, Vec::len);
            view! { <div class="pick">
                <div class="row">
                    <AddButton label="Add new action" attr:disabled=move || !room(count(), "warn_actions") on:click=move |_| { let mut cur = get().as_array().cloned().unwrap_or_default(); cur.push(json!({"points": 10, "action": "timeout", "secs": 3600})); set(json!(cur)) } />
                    <Count n=Signal::derive(count) key="warn_actions" />
                    <button class="danger" on:click=move |_| set(json!([]))>"Delete all actions"</button>
                </div>
                <Show when=move || count() == 0><span class="muted">"No Custom Actions"</span></Show>
                {move || (0..count()).map(|i| { let bad = RwSignal::new(false); view! { <div class="row">
                    <input aria-label="points" prop:value=move || show(&get()[i]["points"]) on:change=move |e| { let t = event_target_value(&e); let x = t.trim().parse::<i64>(); bad.set(x.is_err() && !t.trim().is_empty()); if let Ok(x) = x { upd(i, "points", json!(x)) } } /><span>"points"</span>
                    <select aria-label="action" on:change=move |e| upd(i, "action", json!(event_target_value(&e)))>
                        {["timeout", "kick", "ban"].map(|a| view! { <option value=a prop:selected=move || get()[i]["action"] == a>{a}</option> }).collect_view()}
                    </select>
                    <Show when=move || get()[i]["action"] == "timeout">
                        <input aria-label="seconds" placeholder="default" prop:value=move || show(&get()[i]["secs"]) on:change=move |e| { let t = event_target_value(&e); let x = t.trim().parse::<i64>(); bad.set(x.is_err() && !t.trim().is_empty()); if let Ok(x) = x { upd(i, "secs", json!(x)) } } /><span>"seconds"</span>
                    </Show>
                    <button class="ghost" on:click=move |_| { let mut cur = get(); if let Some(a) = cur.as_array_mut() { a.remove(i); } set(cur) }>"Remove"</button>
                    {not_a_number(bad)}
                </div> } }).collect_view()}
            </div> }.into_any()
        }
        "snippets" => {
            let cap = f["cap"].as_u64().unwrap_or(100);
            let upd = move |i: usize, k: &'static str, x: Value| { let mut cur = get(); cur[i][k] = x; set(cur) };
            let count = move || get().as_array().map_or(0, Vec::len);
            view! { <div class="vstack">
                <span><span class="count">{move || format!("{} / {cap}", count())}</span></span>
                {move || (0..count()).map(|i| view! { <div class="vstack tight">
                    <input aria-label="name" placeholder="name" prop:value=move || show(&get()[i]["name"]) on:change=move |e| upd(i, "name", json!(event_target_value(&e).trim().to_lowercase())) />
                    <textarea aria-label="content" maxlength="1000" class="ta-s" prop:value=move || show(&get()[i]["content"]) on:change=move |e| upd(i, "content", json!(event_target_value(&e))) />
                    <div><button class="ghost" on:click=move |_| { let mut cur = get(); if let Some(a) = cur.as_array_mut() { a.remove(i); } set(cur) }>"Remove"</button></div>
                </div> }).collect_view()}
                <div><AddButton label="Add snippet" attr:disabled=move || { count() as u64 >= cap } on:click=move |_| { let mut cur = get().as_array().cloned().unwrap_or_default(); cur.push(json!({"name": "", "content": ""})); set(json!(cur)) } /></div>
            </div> }.into_any()
        }
        "customs" => {
            let opts = f["options"].as_array().cloned().unwrap_or_default();
            view! { <div class="pick">
                {move || (0..get().as_array().map_or(0, Vec::len)).map(|i| {
                    let boxes = opts.iter().map(|o| {
                        let k = show(o);
                        let label = k.replace('_', " ");
                        let k2 = k.clone();
                        view! { <label><input type="checkbox" prop:checked=move || strs(&get()[i]["events"]).contains(&k) on:change=move |e| {
                            let mut cur = get();
                            let mut ev = strs(&cur[i]["events"]);
                            ev.retain(|x| *x != k2);
                            if event_target_checked(&e) { ev.push(k2.clone()); }
                            cur[i]["events"] = json!(ev);
                            set(cur);
                        } /> {label}</label> }
                    }).collect_view();
                    view! { <div class="row">
                        <ListPick items=Signal::derive(move || items(&dc.get(), "channels")) get=Signal::derive(move || get()[i]["channel"].as_str().unwrap_or("").to_string()) set=Callback::new(move |v: String| { let mut cur = get(); cur[i]["channel"] = json!(v); set(cur) }) none="choose a channel" label="channel" />
                        <div class="pick">{boxes}</div>
                        <button class="ghost" on:click=move |_| { let mut cur = get(); if let Some(a) = cur.as_array_mut() { a.remove(i); } set(cur) }>"Remove"</button>
                    </div> }
                }).collect_view()}
                <AddButton label="Add a new logging type" on:click=move |_| { let mut cur = get(); if !cur.is_array() { cur = json!([]); }
 if let Some(a) = cur.as_array_mut() { a.push(json!({"events": [], "channel": null})); } set(cur) } />
            </div> }.into_any()
        }
        "channel" | "role" => {
            let key = match kind.as_str() { "channel" => "channels", "category" => "categories", _ => "roles" };
            // A log route/event entry is unset by dropping its key: the bot cannot parse a null channel there.
            let entry = path.rsplit_once('.').filter(|(m, _)| m.ends_with(".routes") || m.ends_with(".events")).map(|(m, k)| (m.to_string(), k.to_string()));
            view! { <ListPick items=Signal::derive(move || items(&dc.get(), key)) get=Signal::derive(move || get().as_str().unwrap_or("").to_string())
                set=Callback::new(move |s: String| match (&entry, s.is_empty()) {
                    (Some((m, k)), true) => val.update(|v| { let mut o = get_path(v, m); if let Some(o) = o.as_object_mut() { o.remove(k); } set_path(v, m, o) }),
                    _ => set(if s.is_empty() { Value::Null } else { json!(s) }),
                }) none="none" label=if kind == "role" { "role" } else { "channel" } /> }.into_any()
        }
        "users" => {
            let cap = f["cap"].as_u64();
            view! { <div class="vstack tight">
                <UserPick get=Signal::derive(get) set=Callback::new(set) />
                {cap.map(|cap| view! { <span class="muted">{move || format!("{} of {cap}", get().as_array().map_or(0, Vec::len))}</span> })}
            </div> }.into_any()
        }
        "route_pick" => view! { <RoutePick f=f.clone() get=Signal::derive(get) set=Callback::new(set) dc /> }.into_any(),
        "multi" => {
            let opts: Vec<String> = strs(&f["options"]);
            view! { <div class="pick">
                {opts.into_iter().map(|o| {
                    let (o2, l) = (o.clone(), opt_label(&o));
                    view! { <label><input type="checkbox" prop:checked=move || strs(&get()).contains(&o) on:change=move |e| {
                        let mut cur = strs(&get());
                        cur.retain(|x| *x != o2);
                        if event_target_checked(&e) { cur.push(o2.clone()); }
                        set(json!(cur));
                    } /> {l}</label> }
                }).collect_view()}
            </div> }.into_any()
        }
        "channels" | "categories" | "roles" => {
            // Chips of the picked ones and the shared picker (without them) to add another.
            let key = StoredValue::new(kind.clone());
            let label = if kind == "roles" { "add a role" } else if kind == "categories" { "add a category" } else { "add a channel" };
            view! { <div class="pick">
                {move || strs(&get()).into_iter().map(|id| {
                    let label = items(&dc.get(), &key.get_value()).into_iter().find(|(i, _)| *i == id).map_or_else(|| id.clone(), |(_, l)| l);
                    view! { <span class="chip">{label}" "<button class="link" aria-label="remove" on:click=move |_| { let mut cur = strs(&get()); cur.retain(|x| *x != id); set(json!(cur)) }>"×"</button></span> }
                }).collect_view()}
                <ListPick items=Signal::derive(move || { let on = strs(&get()); items(&dc.get(), &key.get_value()).into_iter().filter(|(i, _)| !on.contains(i)).collect() })
                    get=Signal::derive(String::new) none="" label=label
                    set=Callback::new(move |id: String| { let mut cur = strs(&get()); if !id.is_empty() && !cur.contains(&id) { cur.push(id); set(json!(cur)) } }) />
            </div> }.into_any()
        }
        _ => view! { <input prop:value=move || show(&get()) on:change=move |e| { let s = event_target_value(&e); set(if s.is_empty() { Value::Null } else { json!(s) }) } /> }.into_any(),
    };
    // Tip and help both go behind the info icon (tip first), with the live texts below.
    let fixed = [f["tip"].as_str(), f["help"].as_str()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let mut live: Vec<Signal<String>> = vec![];
    if f["helper"].is_object() {
        let h = f["helper"].clone();
        // Heat weights live at `<section>.heat.<w>`; max and decay are their siblings, the strike duration their cousin.
        let heat = path
            .rsplit_once('.')
            .map_or(String::new(), |(p, _)| p.to_string());
        let strike = heat.rsplit_once('.').map_or(String::new(), |(p, _)| {
            format!("{p}.strike.strike_timeout_ms")
        });
        live.push(Signal::derive(move || {
            let v = val.get();
            let num = |p: &str, d: &Value| get_path(&v, p).as_f64().or(d.as_f64()).unwrap_or(0.0);
            let (max, decay) = (num(&format!("{heat}.max"), &h["max"]), num(&format!("{heat}.decay_per_sec"), &h["decay"]));
            let w = get().as_f64().unwrap_or(0.0);
            let secs = num(&strike, &h["strike_ms"]) / 1000.0;
            let u = best_unit(secs);
            let n = if w > 0.0 { (max * (1.0 - decay) / w).max(1.0) } else { f64::INFINITY };
            format!("A member approximately needs to send {n:.2} {} before getting timed out for {} {}(s), the Auto Timeouts regular strike duration.", h["noun"].as_str().unwrap_or("messages"), secs / UNITS[u].1, UNITS[u].0)
        }));
    }
    if let Some((w, hour)) = path
        .strip_prefix("antinuke.limits.")
        .and_then(|r| r.split_once('.'))
        .and_then(|(a, k)| {
            NUKE_WORDS
                .iter()
                .find(|w| w.0 == a)
                .map(|w| (w, k == "per_hour"))
        })
        .filter(|_| tail == "per_minute" || tail == "per_hour")
    {
        live.push(Signal::derive(move || {
            let n = get().as_f64().unwrap_or(0.0);
            let (span, per, div) = if hour { ("1 hour", "minute", 60.0) } else { ("60 seconds", "second", 60.0) };
            format!("A staff member needs to {} {n} {} in less than {span}. That's an average of {:.2} {} every single {per} that they need to stay under if they want Brim not coming after them.", w.1, w.2, n / div, w.3)
        }));
    }
    if path.starts_with("automod.filters.") && tail == "action" {
        live.push(Signal::derive(move || {
            let a = get();
            let a = a.as_str().unwrap_or("");
            format!("Trigger Action represents what Brim does once this filter triggers and the total heat accumulated for the member surpasses the total allowed heat. {} is {}.", if a.is_empty() { "Strike system" } else { a }, action_desc(a))
        }));
    }
    if path == "joinraid.active_ms" {
        live.push(Signal::derive(move || raid_summary(&val.get())));
    }
    // Help text of any kind sits behind the info icon next to the name (owner decision 2026-11-22).
    let tip = (!fixed.is_empty() || !live.is_empty()).then(|| {
        let text = Signal::derive(move || {
            let mut t = fixed.clone();
            for s in &live {
                if !t.is_empty() {
                    t.push(' ')
                }
                t.push_str(&s.get());
            }
            t
        });
        view! { <crate::common::Tip text /> }
    });
    let label = f["title"]
        .as_str()
        .map_or_else(|| tail.replace('_', " "), String::from);
    // The group's name labels the control inside, whatever kind it is.
    view! { <div class="row" class:tog=kind == "bool" role="group" aria-label=label.clone()><label>{label.clone()}{tip}</label>{input}{f["warn"].as_str().map(|w| view! { <span class="warn wline">{w.to_string()}</span> })}</div> }
}

/// Raw JSON of one section of the config in `val`.
fn raw_editor(val: RwSignal<Value>, name: String, msg: Msg) -> impl IntoView {
    let n = StoredValue::new(name);
    view! { <textarea prop:value=move || serde_json::to_string_pretty(&val.get()[n.get_value()]).unwrap_or_default()
    on:change=move |e| { match serde_json::from_str::<Value>(&event_target_value(&e)) { Ok(v) => val.update(|c| c[n.get_value()] = v), Err(e) => msg.err(format!("Not valid JSON, so nothing changed: {e}")) } }></textarea> }
}

/// The captcha as a member sees it, drawn by the server from the settings on the page (saved or
/// not), redrawn 400 ms after the last change.
#[component]
fn CaptchaPreview(guild: String, val: RwSignal<Value>) -> impl IntoView {
    let png = RwSignal::new(None::<String>);
    let seq = StoredValue::new(0u32);
    Effect::new(move |_| {
        let cfg = val.with(|v| v["verification"]["captcha"].clone());
        seq.update_value(|n| *n += 1);
        let (mine, guild) = (seq.get_value(), guild.clone());
        set_timeout(
            move || {
                if seq.is_disposed() || seq.get_value() != mine {
                    return;
                }
                let body = if cfg.is_object() { cfg } else { json!({}) };
                spawn_local(async move {
                    let r = api(
                        "POST",
                        &format!("/api/guilds/{guild}/verification/captcha-preview"),
                        Some(body),
                    )
                    .await;
                    if let (Ok(v), false) = (r, png.is_disposed()) {
                        if seq.get_value() == mine {
                            png.set(v["png"].as_str().map(String::from));
                        }
                    }
                });
            },
            std::time::Duration::from_millis(400),
        );
    });
    let mode = Signal::derive(move || {
        val.with(|v| {
            v["verification"]["mode"]
                .as_str()
                .unwrap_or("Captcha")
                .to_string()
        })
    });
    view! { <div class="capprev"><p class="muted">"Preview"</p><crate::discord::VerifyPreview mode captcha=png /></div> }
}

/// The ticket messages as members see them, drawn by the server from the settings on the page
/// (saved or not; `POST .../tickets/preview` runs the code the bot posts with), asked 400 ms after
/// the last change. `greeting`: the greeting of a new ticket, else the panel message.
#[component]
pub fn TicketPreview(guild: String, val: RwSignal<Value>, greeting: bool, #[prop(into)] edit: String) -> impl IntoView {
    let shown = RwSignal::new(Value::Null);
    let seq = StoredValue::new(0u32);
    Effect::new(move |_| {
        let t = val.with(|v| v["tickets"].clone());
        seq.update_value(|n| *n += 1);
        let (mine, guild) = (seq.get_value(), guild.clone());
        set_timeout(
            move || {
                if seq.is_disposed() || seq.get_value() != mine {
                    return;
                }
                let body = json!({"panels": if t["panels"].is_array() { t["panels"].clone() } else { json!([]) },
                    "types": t["types"], "button_per_type": t["button_per_type"] == true, "greeting": t["greeting"]});
                spawn_local(async move {
                    let r = api(
                        "POST",
                        &format!("/api/guilds/{guild}/tickets/preview"),
                        Some(body),
                    )
                    .await;
                    if !shown.is_disposed() && seq.get_value() == mine {
                        shown.set(r.unwrap_or_else(|e| json!({"error": e})));
                    }
                });
            },
            std::time::Duration::from_millis(400),
        );
    });
    move || {
        let v = shown.get();
        if v.is_null() {
            return view! { <p class="loading"><Working /></p> }.into_any();
        }
        if let Some(e) = v["error"].as_str() {
            return view! { <p class="err">{e.to_string()}</p> }.into_any();
        }
        let m = if greeting {
            json!({"embeds": [v["greeting"].clone()]})
        } else {
            v["panel"].clone()
        };
        if greeting && v["greeting"].is_null() {
            return view! { <p class="muted">"The greeting is empty: no message is sent."</p> }
                .into_any();
        }
        crate::discord::preview_box(&m, &edit)
    }
}

/// The note under a number box whose typed text is not a number: the stored value did not change.
fn not_a_number(bad: RwSignal<bool>) -> impl IntoView {
    move || {
        bad.get()
            .then(|| view! { <span class="err">"That is not a number, so the value did not change."</span> })
    }
}

/// The confirm text before Remove dangerous roles goes on, from `/enforce-permits`: who loses
/// which roles now, or that the list could not be read (`Err`).
pub fn enforce_text(r: Result<Value, String>) -> String {
    const HEAD: &str = "Brim takes roles with dangerous permissions from every member without a permit, the next time their roles or name change. The owner, Extra Owners, Trusted Admins, permit holders and bots keep theirs.";
    let v = match r {
        Ok(v) => v,
        Err(e) => return format!("{HEAD}\n\nBrim could not read who holds such roles now ({e}). Check Permits first."),
    };
    let n = v["count"].as_u64().unwrap_or(0);
    if n == 0 {
        return format!("{HEAD}\n\nNobody loses a role now.");
    }
    let at = if v["more"] == true { "At least " } else { "" };
    let rows: Vec<String> = v["members"].as_array().into_iter().flatten().map(|m| {
        let roles: Vec<&str> = m["roles"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        format!("- {}: {}", m["name"].as_str().unwrap_or_default(), roles.join(", "))
    }).collect();
    let rest = n.saturating_sub(rows.len() as u64);
    let tail = if rest > 0 { format!("\n- and {rest} more") } else { String::new() };
    let who = if n == 1 { "member loses" } else { "members lose" };
    format!("{HEAD}\n\n{at}{n} {who} roles now. Give a permit first to anyone who must keep them:\n{}{tail}", rows.join("\n"))
}

/// Reads who loses roles (`busy` is true meanwhile), then asks before `yes` turns Remove
/// dangerous roles on.
pub fn ask_enforce(
    guild: String,
    yes_label: &'static str,
    busy: impl Fn(bool) + 'static,
    yes: impl FnOnce() + 'static,
) {
    busy(true);
    spawn_local(async move {
        let r = api("GET", &format!("/api/guilds/{guild}/enforce-permits"), None).await;
        busy(false);
        let a = Ask::new("Remove dangerous roles from members without permits?")
            .text(enforce_text(r))
            .yes(yes_label)
            .danger();
        ask(a, move |_| yes());
    });
}

/// Schema-driven form for the sections the backend describes; raw JSON stays available underneath.
/// Fields of other sections (`"section"` in the schema) are shown and saved alongside.
#[component]
pub fn Section(
    guild: String,
    name: String,
    #[prop(into, default = String::new())] part: String,
    /// Wizard mode: these cards one at a time instead of the grouped form.
    #[prop(optional)]
    cards: Option<&'static [crate::wizard::Card]>,
    /// Called by the forward arrow on the last card.
    #[prop(optional)]
    next: Option<Callback<()>>,
    /// Called after each good save.
    #[prop(optional)]
    on_save: Option<Callback<()>>,
    /// Set while the form has unsaved edits, for a page action that would rebuild the form.
    #[prop(optional)]
    dirty: Option<RwSignal<bool>>,
) -> impl IntoView {
    // The whole config, so fields of other sections share one form and one save.
    let val = RwSignal::new(json!({}));
    let version = RwSignal::new(0i64);
    let dc = RwSignal::new(json!({}));
    let fields = RwSignal::new(Vec::<Value>::new());
    // The default of the section's `enabled` field, from the whole schema: a page may not show the switch.
    let switch_default = RwSignal::new(None::<bool>);
    let orig = RwSignal::new(json!({}));
    let failed = RwSignal::new(false);
    if let Some(d) = dirty {
        Effect::new(move |_| d.set(val.with(|v| orig.with(|o| v != o))));
    }
    // The bot could not read the saved section and runs it on its defaults.
    let unread = RwSignal::new(false);
    let (msg, need) = (Msg::default(), step_up());
    // Until the schema answers, show a loading line instead of flashing the raw JSON editor.
    let loaded = RwSignal::new(false);
    let (g, n) = (guild.clone(), name.clone());
    let gs = StoredValue::new(guild.clone());
    spawn_local(async move {
        let r = api("GET", &format!("/api/guilds/{g}/config"), None).await;
        // Leaving the page while a request runs disposes the signals, and reading one then panics.
        if val.is_disposed() {
            return;
        }
        match r {
            Ok(v) => {
                version.set(v["version"].as_i64().unwrap_or(0));
                unread.set(
                    v["unread"]
                        .as_array()
                        .is_some_and(|u| u.iter().any(|x| x == n.as_str())),
                );
                let mut c = if v["config"].is_object() {
                    v["config"].clone()
                } else {
                    json!({})
                };
                if !c[&n].is_object() {
                    c[&n] = json!({});
                }
                val.set(c);
                orig.set(val.get_untracked());
            }
            Err(e) => {
                // The form would show defaults as if they were stored: no Save from that.
                failed.set(true);
                msg.err(format!("The settings could not be loaded, so Save is off until you reload the page: {e}"));
            }
        }
        // Missing schema/discord data degrades to the raw editor.
        let s = api("GET", &format!("/api/guilds/{g}/schema"), None).await;
        if val.is_disposed() {
            return;
        }
        if let Ok(s) = s {
            if let Some(sec) = s
                .as_array()
                .and_then(|a| a.iter().find(|x| x["name"] == n.as_str()))
            {
                // `part` picks the dashboard pages of the section: "" all fields, else a comma list where "general" means the fields without a page.
                let keep = |f: &Value| {
                    part.is_empty()
                        || part.split(',').any(|p| {
                            if p == "general" {
                                f["page"].is_null()
                            } else {
                                f["page"] == p
                            }
                        })
                };
                switch_default.set(
                    sec["fields"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|f| {
                            f["path"] == "enabled"
                                && f["section"].as_str().is_none_or(|x| x == n.as_str())
                        })
                        .map(|f| f["default"].as_bool().unwrap_or(false)),
                );
                // The channel and role names come before the fields: drawn first, a set channel showed "none" until they came.
                let d = api("GET", &format!("/api/guilds/{g}/discord"), None).await;
                if val.is_disposed() {
                    return;
                }
                match d {
                    Ok(d) => dc.set(d),
                    Err(e) => msg.err(format!("The channel and role names could not be loaded, so set channels and roles show as not found: {e}")),
                }
                fields.set(
                    sec["fields"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|f| keep(f))
                        .map(|f| {
                            let mut f = f.clone();
                            f["path"] = json!(format!(
                                "{}.{}",
                                f["section"].as_str().unwrap_or(&n),
                                f["path"].as_str().unwrap_or("")
                            ));
                            f
                        })
                        .collect(),
                );
                // Unset values take their defaults, so changing one and back leaves nothing to save.
                let mut c = val.get_untracked();
                // The wizard fills the recommended values (`profile.json`) where nothing is saved yet.
                let prefix = format!("{n}.");
                let unset: Vec<(&str, Value)> = if cards.is_some() {
                    crate::wizard::profile().iter().filter(|(p, _)| p.starts_with(&prefix)).map(|(p, v)| (p.as_str(), v.clone())).filter(|(p, _)| get_path(&c, p).is_null()).collect()
                } else {
                    vec![]
                };
                for f in fields.get_untracked() {
                    let p = f["path"].as_str().unwrap_or("");
                    // `fallback`: an older field of the same section that held this value in other units.
                    let old = f["fallback"]["path"].as_str().map(|o| {
                        get_path(
                            &c,
                            &format!("{}.{o}", p.rsplit_once('.').map_or("", |(s, _)| s)),
                        )
                    });
                    let old = old.and_then(|o| o.as_f64()).map(|o| {
                        json!((o * f["fallback"]["scale"].as_f64().unwrap_or(1.0)) as i64)
                    });
                    if get_path(&c, p).is_null() {
                        if let Some(d) =
                            old.or_else(|| (!f["default"].is_null()).then(|| f["default"].clone()))
                        {
                            set_path(&mut c, p, d)
                        }
                    }
                }
                orig.set(c.clone());
                // A wizard switch that starts on and was never saved shows as an unsaved change.
                for (p, v) in unset {
                    set_path(&mut c, p, v);
                }
                val.set(c);
            }
        }
        loaded.set(true);
    });
    let n = StoredValue::new(name);
    let reload = {
        let g = guild.clone();
        move |v: i64| {
            let g = g.clone();
            spawn_local(async move {
                // The new version goes with the config it belongs to: with old values it would
                // pass the conflict check and overwrite what the action changed.
                match api("GET", &format!("/api/guilds/{g}/config"), None).await {
                    Ok(c) if !val.is_disposed() => {
                        version.set(c["version"].as_i64().unwrap_or(v));
                        val.set(c["config"].clone());
                        orig.set(c["config"].clone());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        if !failed.is_disposed() {
                            failed.set(true);
                            msg.err(format!("The settings could not be loaded again, so Save is off until you reload the page: {e}"));
                        }
                    }
                }
            })
        }
    };
    let at = RwSignal::new(0usize);
    let act = {
        let g = guild.clone();
        let reload = reload.clone();
        move |path: &'static str| {
            let (g, v, reload) = (g.clone(), version.get_untracked(), reload.clone());
            // Also sent after a save's await (no click to repeat), so a 2FA code resumes it itself.
            run_resumable(
                move || {
                    let g = g.clone();
                    async move {
                        api(
                            "POST",
                            &format!("/api/guilds/{g}/{path}"),
                            Some(json!({"expected_version": v})),
                        )
                        .await
                    }
                },
                msg,
                need,
                move |r| {
                    match r["warning"].as_str() {
                        Some(w) => msg.err(w),
                        None => msg.ok("Done"),
                    }
                    reload(r["version"].as_i64().unwrap_or(0))
                },
            );
        }
    };
    for &(src, dst) in cards.into_iter().flatten().flat_map(|c| c.links) {
        let x = Memo::new(move |_| val.with(|v| get_path(v, src)));
        Effect::new(move |_| {
            let x = x.get();
            if !loaded.get_untracked() {
                return;
            }
            // Back to the saved value: the follower goes back to its saved value too.
            let o = orig.get_untracked();
            let y = if x == get_path(&o, src) {
                get_path(&o, dst)
            } else {
                x
            };
            if val.with_untracked(|v| get_path(v, dst) != y) {
                val.update(|v| set_path(v, dst, y))
            }
        });
    }
    let card_view = cards.map(|cs| {
        let pv = NodeRef::<leptos::html::Div>::new();
        // (card, texts of its preview): a new card starts over, so only a settings change flashes.
        let seen = StoredValue::new((usize::MAX, None::<Vec<String>>));
        Effect::new(move |_| {
            val.track();
            dc.track();
            let (i, ready) = (at.get(), loaded.get());
            request_animation_frame(move || {
                let Some(el) = pv.try_get_untracked().flatten() else { return };
                seen.update_value(|(card, texts)| {
                    if *card != i || !ready { *card = i; *texts = None; }
                    crate::discord::flash_changes(&el, texts);
                });
            });
        });
        let act = act.clone();
        view! { <div class="card wiz">
            <button class="nav back" aria-label="previous" disabled=move || at.get() == 0 on:click=move |_| at.update(|i| *i = i.saturating_sub(1))>{crate::icons::icon("page_prev")}</button>
            <div class="wizbody">
                {move || { let c = &cs[at.get()]; let all = fields.get();
                    let fs: Vec<Value> = c.fields.iter().filter_map(|(p, l)| all.iter().find(|f| f["path"] == *p).map(|f| { let mut f = f.clone(); if !l.is_empty() { f["title"] = json!(l); } f })).collect();
                    let act = act.clone();
                    let is_on = move |p: &'static str| Memo::new(move |_| val.with(|v| get_path(v, p) == json!(true)));
                    // A field tied to a switch slides open while it is on; `inert` keeps the closed one out of tab order.
                    let field = move |f: Value| match c.when.iter().find(|(p, _)| f["path"] == *p) {
                        Some(&(_, sw)) => { let open = is_on(sw); view! { <div class="cond" class:open=open prop:inert=move || !open.get()><div><div class="pan"><Field f val dc /></div></div></div> }.into_any() }
                        None => view! { <Field f val dc /> }.into_any(),
                    };
                    // Placeholder rows hold the card's height until the schema arrives.
                    let fs = if all.is_empty() { view! { <p class="loading"><Working /></p> }.into_any() } else if c.boxed && fs.len() > 1 {
                        let off = Memo::new(move |_| !is_on(c.fields[0].0).get());
                        let mut fs = fs.into_iter();
                        let first = fs.next().map(field);
                        view! { {first}<div class="atit" class:off=off><i>"While you are at it:"</i>{fs.map(field).collect_view()}</div>
                            <Show when=move || off.get()><span class="chip bad dot">"These don't matter now!"</span></Show> }.into_any()
                    } else { fs.into_iter().map(field).collect_view().into_any() };
                    let dirty = move || val.get() != orig.get();
                    view! { <div class="enter">
                        <h2>{c.title}</h2><p class="muted">{c.desc}</p>
                        {c.alert.map(|t| view! { <div class="alert" role="alert"><b>"!"</b><p>{t}</p></div> })}
                        {c.notes.iter().map(|t| view! { <p class="note">{*t}</p> }).collect_view()}
                        <div class="split"><div>{fs}
                            {c.action.map(|(label, path)| {
                                // A channel the action would make that already exists needs no button.
                                let made = move || c.done.is_some_and(|p| get_path(&val.get(), p).as_str().is_some_and(|id| items(&dc.get(), "channels").iter().any(|(i, _)| i == id)));
                                view! { <div class="act" class:hide=made><button class="go" disabled=dirty on:click=move |_| act(path)>{label}</button>
                                <Show when=dirty><span class="chip orange">"Save your changes first"</span></Show></div> } })}</div>
                            <div class="preview loop" node_ref=pv>{move || (cs[at.get_untracked()].preview)(&val.get(), &dc.get())}</div></div>
                    </div> } }}
                <p class="muted">{move || format!("{} / {}", at.get() + 1, cs.len())}</p>
            </div>
            <button class="nav" aria-label="next" on:click=move |_| if at.get() + 1 < cs.len() { at.update(|i| *i += 1) } else if let Some(n) = next { n.run(()) }>{crate::icons::icon("page_next")}</button>
        </div> }
    });
    let saving = RwSignal::new(false);
    let do_save = move |_: ()| {
        // A second click while a save runs would send the same version again and get a conflict.
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        let (g, cur, before) = (guild.clone(), val.get_untracked(), orig.get_untracked());
        let mut names = vec![n.get_value()];
        names.extend(
            fields
                .get_untracked()
                .iter()
                .filter_map(|f| f["section"].as_str().map(String::from)),
        );
        names.sort();
        names.dedup();
        // A wizard card whose switch was just turned on runs its setup (channel, category) right after.
        let setup = cards
            .and_then(|cs| cs.get(at.get_untracked()))
            .and_then(|c| {
                let switch = c.fields.first()?.0;
                (get_path(&cur, switch) == json!(true) && get_path(&before, switch) != json!(true))
                    .then_some(c.action?.1)
            });
        let act = act.clone();
        run_change(
            async move {
                let r = async {
                    let mut last = json!({"version": version.get_untracked()});
                    for s in names
                        .iter()
                        .filter(|s| cur[s.as_str()] != before[s.as_str()])
                    {
                        let body =
                            json!({"expected_version": last["version"], "value": cur[s.as_str()]});
                        last =
                            api("PUT", &format!("/api/guilds/{g}/config/{s}"), Some(body)).await?;
                        // Only a stored section counts as saved: a refused or 2FA-held one keeps the
                        // Save bar, and a later section builds on the version this one made.
                        version.try_set(last["version"].as_i64().unwrap_or(0));
                        orig.try_update(|o| o[s.as_str()] = cur[s.as_str()].clone());
                    }
                    Ok(last)
                }
                .await;
                saving.try_set(false);
                r
            },
            msg,
            need,
            move |v| {
                version.set(v["version"].as_i64().unwrap_or(0));
                msg.done("Saved!");
                if let Some(c) = on_save {
                    c.run(())
                }
                if let Some(path) = setup {
                    act(path)
                }
            },
        );
    };
    // Turning the public ticket channel off deletes it: ask first (owner decision 11-15).
    let save = move |()| {
        let public = |v: &Value| v["tickets"]["public_channel"] != json!(false);
        let (cur, before) = (val.get_untracked(), orig.get_untracked());
        let enforce = |v: &Value| v["misc"]["enforce_permits"] == json!(true);
        // Turning it on takes roles from members: they are listed first, as Overview's Fix does.
        if enforce(&cur) && !enforce(&before) {
            if saving.get_untracked() {
                return;
            }
            let go = do_save.clone();
            ask_enforce(gs.get_value(), "Turn on and save", move |b| { saving.try_set(b); }, move || go(()));
        } else if public(&before) && !public(&cur) && before["tickets"]["public_channel_id"].is_string() {
            let go = do_save.clone();
            ask(
                Ask::new("Delete the public ticket channel?")
                    .text("Brim deletes #open-a-ticket and its panel message. Members can no longer open a ticket there.")
                    .yes("Delete and save")
                    .danger(),
                move |_| go(()),
            )
        } else {
            do_save(())
        }
    };
    {
        let save = save.clone();
        crate::guard::guard(
            move || val.try_with(|v| orig.with(|o| v != o)).unwrap_or(false),
            move || saving.try_get().unwrap_or(false),
            || None,
            move || save(()),
        );
    }
    // Read per card, not in the grouping closure: rebuilding the cards on every switch flip cut the knob's animation.
    // A missing key reads as the switch's own default, like the switch shows it (every system starts off).
    // A section without a master switch (Logging, Lockdown, ...) is never off.
    let off = Memo::new(move |_| {
        let sec = n.get_value();
        switch_default
            .get()
            .is_some_and(|d| !val.with(|v| v[sec.as_str()]["enabled"].as_bool().unwrap_or(d)))
    });
    view! {
        <Show when=move || unread.get()><p class="card err" role="alert">"Brim could not read these saved settings, so it runs this section on its defaults (off for most systems) whatever this page shows. A newer Brim version saved them: update Brim again, or fix and save this page."</p></Show>
        {card_view}
        <Show when=move || cards.is_none() && !fields.get().is_empty() fallback=move || cards.is_none().then(|| if loaded.get() { raw_editor(val, n.get_value(), msg).into_any() } else { view! { <p><Working /></p> }.into_any() })>
            <div>
                {move || {
                    // One card per path prefix ("limits.ban" -> "limits ban"); unprefixed fields go under "General".
                    let mut groups: Vec<(String, Vec<Value>)> = vec![];
                    // While searching, only the matching fields show (all of them when none match here).
                    let q = use_context::<crate::search::Search>().map(|s| s.q.get()).unwrap_or_default();
                    let all = fields.get();
                    let hits: Vec<Value> = all.iter().filter(|f| crate::search::field_score(f, &q) > 0).cloned().collect();
                    for f in if hits.is_empty() { all } else { hits } {
                        let p = f["path"].as_str().unwrap_or("").split_once('.').map_or("", |(_, p)| p);
                        let g = f["group"].as_str().map_or_else(|| p.rsplit_once('.').map_or("General".to_string(), |(g, _)| g.replace(['.', '_'], " ")), String::from);
                        match groups.iter_mut().find(|(k, _)| *k == g) { Some((_, v)) => v.push(f), None => groups.push((g, vec![f])) }
                    }
                    // A system that is switched off rings its cards red (the card holding the switch stays plain).
                    let sec = n.get_value();
                    let switch = format!("{sec}.enabled");
                    let note = format!("Your whole {sec} is disabled, the settings of this card have no effect.");
                    let id = |g: &str| format!("grp-{}", g.to_lowercase().replace(' ', "-"));
                    let cards = groups.into_iter().map(|(g, fs)| {
                        // Rows of the card (title and padding count as two, the captcha preview as six), to balance the columns.
                        let weight = fs.len() + 2 + if sec == "verification" && g == "Captcha" { 6 } else if sec == "tickets" && g == "Messages" { 5 } else { 0 };
                        let gid = id(&g);
                        let plain = fs.iter().any(|f| f["path"] == switch.as_str());
                        // A card whose `used` condition fails (e.g. captcha look in website mode) rings red too.
                        let used: Vec<Value> = fs.iter().map(|f| f["used"].clone()).filter(|c| !c.is_null()).collect();
                        let unused = move || val.with(|v| !used.iter().all(|c| holds(v, c)));
                        let ring = Signal::derive(move || (!plain && off.get()) || unused());
                        let note = note.clone();
                        let text = move || if !plain && off.get() { note.clone() } else { "Your current choices do not use this card, so its settings have no effect.".into() };
                        let fields = fs.into_iter().map(|f| { let only = f["only"].clone(); view! { <Show when=move || val.with(|v| holds(v, &only))><Field f=f.clone() val dc /></Show> } }).collect_view();
                        let captcha = (sec == "verification" && g == "Captcha").then(|| view! { <CaptchaPreview guild=gs.get_value() val /> });
                        let look = (sec == "tickets" && g == "Messages").then(|| view! { <TicketPreview guild=gs.get_value() val greeting=true edit=format!("#/{}/bot_messages/ticket.greeting/from-tickets", gs.get_value()) /> });
                        (weight, view! { <div class="card" id=gid class:off=ring><h2>{g}</h2>{fields}{captcha}{look}<Show when=move || ring.get()><p class="offnote"><crate::common::Tip text=Signal::derive(text.clone()) />"Not in use right now"</p></Show></div> }.into_any())
                    }).collect::<Vec<_>>();
                    view! { <crate::common::Cards cards /> }
                }}
            </div>
            <details><summary>"Advanced (raw JSON)"</summary>
                {raw_editor(val, n.get_value(), msg)}
            </details>
        </Show>
        <Fade when=Signal::derive(move || val.get() != orig.get())>
            <div class="bar"><b>"Changes detected!"</b><button class="ghost" on:click=move |_| val.set(orig.get_untracked())>"Cancel"</button><button disabled=move || saving.get() || failed.get() on:click={ let save = save.clone(); move |_| save(()) }>{busy_label(move || saving.get(), "Save")}</button></div>
        </Fade>
        <Toast msg />
    }
}

#[cfg(test)]
mod enforce_tests {
    /// The confirm names who loses which role, says how many more, and still warns on a read error.
    #[test]
    fn enforce_confirm_lists_who_loses_roles() {
        use serde_json::json;
        let t = super::enforce_text(Ok(json!({"count": 27, "more": false, "members": [{"name": "Ada", "roles": ["@Admin", "@Mod"]}]})));
        assert!(t.contains("27 members lose roles now"), "{t}");
        assert!(t.contains("- Ada: @Admin, @Mod") && t.contains("- and 26 more"), "{t}");
        let t = super::enforce_text(Ok(json!({"count": 1, "more": true, "members": [{"name": "Ada", "roles": ["@Admin"]}]})));
        assert!(t.contains("At least 1 member loses roles now"), "{t}");
        assert!(super::enforce_text(Ok(json!({"count": 0}))).contains("Nobody loses a role now."));
        let t = super::enforce_text(Err("timeout".into()));
        assert!(t.contains("could not read") && t.contains("without a permit"), "{t}");
    }
}

#[cfg(test)]
mod raid_tests {
    #[test]
    fn numbers_outside_the_schema_range_get_a_note() {
        use super::{range, range_error};
        let f = serde_json::json!({"min": 2, "max": 500});
        assert_eq!(range_error(range(&f), 500.0, None), None);
        assert_eq!(
            range_error(range(&f), 501.0, None).as_deref(),
            Some("Must be between 2 and 500")
        );
        let age = serde_json::json!({"min": 5_000, "max": 8_640_000_000_i64});
        assert_eq!(
            range_error(range(&age), 0.0, Some(0.001)).as_deref(),
            Some("Must be between 5 second(s) and 100 day(s)")
        );
        assert_eq!(range_error(range(&serde_json::json!({})), -9.0, None), None);
    }

    #[test]
    fn role_names() {
        use super::name_of;
        let dc = serde_json::json!({"roles": [{"id": "1", "name": "@everyone"}, {"id": "2", "name": "Member"}]});
        assert_eq!(name_of(&dc, "roles", "1"), "@everyone");
        assert_eq!(name_of(&dc, "roles", "2"), "@Member");
    }

    #[test]
    fn raid_summary_reads_the_settings() {
        assert_eq!(
            super::raid_summary(&serde_json::json!({})),
            "If 10 flagged accounts join within 3 hour(s), Brim will start banning flagged accounts for 30 minute(s)."
        );
        let c = serde_json::json!({"joinraid": {"count": 5, "span_ms": 600_000, "action": "Kick", "active_ms": 86_400_000}});
        assert_eq!(super::raid_summary(&c), "If 5 flagged accounts join within 10 minute(s), Brim will start kicking flagged accounts for 1 day(s).");
    }
}

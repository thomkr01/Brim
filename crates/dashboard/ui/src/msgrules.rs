//! Message Builder "Automate" tab, phase 5: schedule rules. Owner, Extra Owners and Trusted Admins
//! make them; everyone with access sees them. The next runs come from the server, worked out on the
//! real calendar in the rule's time zone.
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

const KINDS: [(&str, &str); 6] = [
    ("once", "Once"),
    ("every", "Every N minutes or hours"),
    ("daily", "Every day"),
    ("weekly", "Every week"),
    ("monthly", "Every month"),
    ("messages", "Every N messages"),
];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days in a month (1 to 12) of the real calendar.
pub fn month_len(y: i32, m: u32) -> u32 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The weekday of the 1st of a month, 0 = Monday (Sakamoto's method).
pub fn first_weekday(y: i32, m: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let sun0 = (y + y / 4 - y / 100 + y / 400 + T[m as usize - 1] + 1).rem_euclid(7);
    ((sun0 + 6) % 7) as u32
}

#[cfg(target_arch = "wasm32")]
fn browser_zone() -> String {
    let f = js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new());
    js_sys::Reflect::get(&f.resolved_options(), &"timeZone".into())
        .ok()
        .and_then(|z| z.as_string())
        .unwrap_or_else(|| "UTC".into())
}
#[cfg(not(target_arch = "wasm32"))]
fn browser_zone() -> String {
    "UTC".into()
}

/// The browser's list of time zone names (IANA).
#[cfg(target_arch = "wasm32")]
fn zones() -> Vec<String> {
    let intl = js_sys::Reflect::get(&js_sys::global(), &"Intl".into()).ok();
    let f = intl
        .as_ref()
        .and_then(|i| js_sys::Reflect::get(i, &"supportedValuesOf".into()).ok())
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
    match (intl, f) {
        (Some(i), Some(f)) => f
            .call1(&i, &"timeZone".into())
            .ok()
            .map(|a| js_sys::Array::from(&a).iter().filter_map(|z| z.as_string()).collect())
            .unwrap_or_default(),
        _ => vec![],
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn zones() -> Vec<String> {
    vec![]
}
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

/// A time in a rule's zone: "Fri, 20 Nov 2026, 18:00".
#[cfg(target_arch = "wasm32")]
fn when(secs: i64, tz: &str) -> String {
    let d = js_sys::Date::new(&(secs as f64 * 1000.0).into());
    let o = js_sys::Object::new();
    for (k, v) in [
        ("timeZone", tz),
        ("weekday", "short"),
        ("day", "numeric"),
        ("month", "short"),
        ("year", "numeric"),
        ("hour", "2-digit"),
        ("minute", "2-digit"),
        ("hourCycle", "h23"),
    ] {
        let _ = js_sys::Reflect::set(&o, &k.into(), &v.into());
    }
    d.to_locale_string("en-GB", &o).into()
}
#[cfg(not(target_arch = "wasm32"))]
fn when(secs: i64, _: &str) -> String {
    secs.to_string()
}

/// Today in the browser as (year, month).
#[cfg(target_arch = "wasm32")]
fn this_month() -> (i32, u32) {
    let d = js_sys::Date::new_0();
    (d.get_full_year() as i32, d.get_month() + 1)
}
#[cfg(not(target_arch = "wasm32"))]
fn this_month() -> (i32, u32) {
    (2026, 11)
}

fn new_line(kind: &str) -> Value {
    match kind {
        "once" => json!({"kind": "once", "date": "", "time": "09:00"}),
        "every" => json!({"kind": "every", "minutes": 60}),
        "weekly" => json!({"kind": "weekly", "days": [1], "time": "09:00"}),
        "monthly" => json!({"kind": "monthly", "days": [1], "time": "09:00"}),
        "messages" => json!({"kind": "messages", "n": 20}),
        _ => json!({"kind": "daily", "time": "09:00"}),
    }
}

fn blank_rule() -> Value {
    json!({"id": null, "enabled": true, "channel": null, "spec": {"tz": browser_zone(), "lines": [new_line("daily")]}})
}

/// The month calendar for "Every month": the real month with its weekdays, ‹ › to browse;
/// picked days are highlighted and count in every month that has them.
fn month_calendar(rule: RwSignal<Value>, i: usize, locked: Signal<bool>) -> impl IntoView {
    let shown = RwSignal::new(this_month());
    let picked = move |d: u32| {
        rule.with(|r| {
            r["spec"]["lines"][i]["days"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x.as_u64() == Some(u64::from(d))))
        })
    };
    let toggle = move |d: u32| {
        rule.update(|r| {
            let mut days: Vec<u64> = r["spec"]["lines"][i]["days"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .collect();
            if let Some(p) = days.iter().position(|x| *x == u64::from(d)) {
                days.remove(p);
            } else {
                days.push(u64::from(d));
                days.sort_unstable();
            }
            r["spec"]["lines"][i]["days"] = json!(days);
        })
    };
    let step = move |by: i32| {
        shown.update(|(y, m)| {
            let n = *y * 12 + *m as i32 - 1 + by;
            *y = n.div_euclid(12);
            *m = n.rem_euclid(12) as u32 + 1;
        })
    };
    view! {
        <div class="mcal">
            <div class="mcalh">
                <button class="ghost" aria-label="previous month" on:click=move |_| step(-1)>"‹"</button>
                <b>{move || { let (y, m) = shown.get(); format!("{} {y}", MONTHS[m as usize - 1]) }}</b>
                <button class="ghost" aria-label="next month" on:click=move |_| step(1)>"›"</button>
            </div>
            <div class="mcalg">
                {WEEKDAYS.map(|w| view! { <span class="wd">{w}</span> }).collect_view()}
                {move || {
                    let (y, m) = shown.get();
                    let blanks = (0..first_weekday(y, m)).map(|_| view! { <span></span> }.into_any());
                    let days = (1..=month_len(y, m)).map(move |d| view! {
                        <button class="day" class:on=move || picked(d) aria-pressed=move || picked(d).to_string() disabled=locked on:click=move |_| toggle(d)>{d}</button>
                    }.into_any());
                    blanks.chain(days).collect_view()
                }}
            </div>
        </div>
    }
}

/// One "when" line of the rule being edited.
fn line_view(rule: RwSignal<Value>, i: usize, next: RwSignal<Value>, locked: Signal<bool>) -> impl IntoView {
    let get = move |k: &'static str| rule.with(|r| r["spec"]["lines"][i][k].clone());
    let set = move |k: &'static str, v: Value| rule.update(|r| r["spec"]["lines"][i][k] = v);
    let kind = move || get("kind").as_str().unwrap_or("daily").to_string();
    let tz = move || rule.with(|r| r["spec"]["tz"].as_str().unwrap_or("UTC").to_string());
    let time_box = move || view! {
        <input type="time" aria-label="time" disabled=locked prop:value=move || get("time").as_str().unwrap_or("").to_string()
            on:change=move |e| set("time", json!(event_target_value(&e))) />
    };
    view! {
        <div class="rline">
            <div class="row">
                <select aria-label="kind of time" disabled=locked on:change=move |e| { let k = event_target_value(&e); rule.update(|r| r["spec"]["lines"][i] = new_line(&k)) }>
                    {KINDS.map(|(k, t)| view! { <option value=k selected=move || kind() == k>{t}</option> }).collect_view()}
                </select>
                {move || match kind().as_str() {
                    "once" => view! {
                        <input type="date" aria-label="date" class:need=move || get("date").as_str().is_none_or(str::is_empty) disabled=locked
                            prop:value=move || get("date").as_str().unwrap_or("").to_string() on:change=move |e| set("date", json!(event_target_value(&e))) />
                        {time_box()}
                    }.into_any(),
                    "every" => {
                        let mins = move || get("minutes").as_i64().unwrap_or(60);
                        let hours = move || mins() % 60 == 0;
                        view! {
                            <input type="number" min="1" aria-label="how often" disabled=locked
                                prop:value=move || if hours() { mins() / 60 } else { mins() }.to_string()
                                on:change=move |e| { let n = event_target_value(&e).parse::<i64>().unwrap_or(1).max(1); set("minutes", json!(if hours() { n * 60 } else { n })) } />
                            <select aria-label="minutes or hours" disabled=locked on:change=move |e| { let h = event_target_value(&e) == "h"; let m = mins(); set("minutes", json!(if h { (m.max(60) / 60) * 60 } else { m.max(15) })) }>
                                <option value="m" selected=move || !hours()>"minutes"</option>
                                <option value="h" selected=hours>"hours"</option>
                            </select>
                        }.into_any()
                    }
                    "weekly" => {
                        let on = move |d: u64| get("days").as_array().is_some_and(|a| a.iter().any(|x| x.as_u64() == Some(d)));
                        view! {
                            <div class="chipset" role="group" aria-label="weekdays">
                                {(1..=7u64).map(|d| view! {
                                    <button class="chip" class:on=move || on(d) aria-pressed=move || on(d).to_string() disabled=locked on:click=move |_| rule.update(|r| {
                                        let mut v: Vec<u64> = r["spec"]["lines"][i]["days"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
                                        if let Some(p) = v.iter().position(|x| *x == d) { v.remove(p); } else { v.push(d); v.sort_unstable(); }
                                        r["spec"]["lines"][i]["days"] = json!(v);
                                    })>{WEEKDAYS[d as usize - 1]}</button>
                                }).collect_view()}
                            </div>
                            {time_box()}
                        }.into_any()
                    }
                    "monthly" => view! { <span class="muted small">"Pick days below, at"</span>{time_box()} }.into_any(),
                    "messages" => view! {
                        <input type="number" min="5" aria-label="messages" disabled=locked prop:value=move || get("n").as_i64().unwrap_or(20).to_string()
                            on:change=move |e| set("n", json!(event_target_value(&e).parse::<i64>().unwrap_or(5))) />
                        <span class="muted small">"messages from members in that channel"</span>
                    }.into_any(),
                    _ => time_box().into_any(),
                }}
                <button class="ghost" aria-label="remove this time" disabled=move || locked.get() || rule.with(|r| r["spec"]["lines"].as_array().map_or(0, Vec::len) <= 1)
                    on:click=move |_| rule.update(|r| { if let Some(a) = r["spec"]["lines"].as_array_mut() { if i < a.len() { a.remove(i); } } })>"✕"</button>
            </div>
            {move || (kind() == "monthly").then(|| month_calendar(rule, i, locked))}
            <p class="muted small nexts">{move || {
                if kind() == "messages" {
                    return "Sends after every N member messages (bots and webhooks do not count). The count starts again when Brim restarts.".to_string();
                }
                let n = next.with(|v| v["lines"][i]["next"].as_array().cloned().unwrap_or_default());
                if n.is_empty() {
                    return next.with(|v| v["error"].as_str().map(|e| format!("Not valid: {e}"))).unwrap_or_else(|| "No sends ahead.".into());
                }
                let t = tz();
                format!("Next: {}", n.iter().filter_map(Value::as_i64).map(|s| when(s, &t)).collect::<Vec<_>>().join(" · "))
            }}</p>
        </div>
    }
}

/// The Automate tab. `embed`: the open message's id (None until it is saved). `picker`: opens the
/// shared channel picker for a draft rule (it writes the draft's "channel").
pub fn automate_tab(
    base: String,
    cur: RwSignal<Value>,
    dirty: Memo<bool>,
    rank: Signal<u8>,
    chan_name: impl Fn(&str) -> String + Copy + Send + Sync + 'static,
    picker: impl Fn(RwSignal<Value>) + Copy + Send + Sync + 'static,
    msg: Msg,
) -> impl IntoView {
    let embed = Signal::derive(move || cur.with(|c| c["id"].as_i64()));
    // Why a schedule cannot be on yet: unsaved changes (a schedule sends the saved message), or a
    // message that is not ready.
    let ready_why = Signal::derive(move || {
        if dirty.get() {
            Some("save or cancel your changes to the message first".into())
        } else {
            cur.with(|c| c["missing"].as_str().map(String::from))
        }
    });
    let base = StoredValue::new(base);
    let rules = RwSignal::new(Vec::<Value>::new());
    let editing = RwSignal::new(None::<RwSignal<Value>>);
    // The open editor's draft, made here: a signal made in a click handler belongs to the list,
    // and a list redraw would drop it while the editor still reads it.
    let draft = RwSignal::new(Value::Null);
    let open_editing = move |v: Value| {
        draft.set(v);
        editing.set(Some(draft))
    };
    let next = RwSignal::new(json!({}));
    let busy = RwSignal::new(false);
    let locked = Signal::derive(move || rank.get() < 3);
    let url = move || embed.get_untracked().map(|id| format!("{}/{id}/rules", base.get_value()));
    let load = move || {
        let Some(u) = url() else { return rules.set(vec![]) };
        let asked = embed.get_untracked();
        spawn_local(async move {
            let r = api("GET", &u, None).await;
            // Another message was opened meanwhile, or the page left.
            if embed.try_get_untracked() != Some(asked) {
                return;
            }
            match r {
                Ok(v) => rules.set(v.as_array().cloned().unwrap_or_default()),
                Err(e) => msg.err(e),
            }
        })
    };
    Effect::new(move |_| {
        embed.track();
        load()
    });
    // The next runs of the rule being edited, asked again when its times or zone change.
    // `seq` drops an older answer that lands last; another rule starts with no times shown.
    let seq = StoredValue::new(0u32);
    Effect::new(move |prev: Option<Option<RwSignal<Value>>>| {
        let open = editing.get();
        if prev.is_some_and(|p| p != open) {
            next.set(json!({}));
        }
        let Some(r) = open else { return open };
        let spec = r.with(|r| r["spec"].clone());
        let Some(u) = url() else { return open };
        seq.update_value(|n| *n += 1);
        let mine = seq.get_value();
        spawn_local(async move {
            let v = api("POST", &format!("{u}/preview"), Some(json!({"spec": spec}))).await;
            if seq.try_get_value() == Some(mine) {
                next.set(v.unwrap_or_else(|e| json!({"error": e})));
            }
        });
        open
    });
    let save = move |r: Value| {
        let Some(u) = url() else { return };
        busy.set(true);
        spawn_local(async move {
            let body = json!({"rule": r["id"], "channel": r["channel"], "spec": r["spec"], "enabled": r["enabled"], "seen": cur.with_untracked(|c| c["version"].clone())});
            let res = api("POST", &u, Some(body)).await;
            busy.set(false);
            match res {
                Ok(v) => {
                    // Turning a schedule on vouches for the message, which moves its version on.
                    cur.update(|c| c["version"] = v["version"].clone());
                    editing.set(None);
                    msg.ok("Schedule saved.");
                    load()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let remove = move |id: i64| {
        let Some(u) = url() else { return };
        ask(Ask::new("Delete this schedule?").text("It stops sending. Copies it sent stay in the Sent log.").yes("Delete").danger(), move |_| {
            let u = u.clone();
            spawn_local(async move {
                match api("POST", &format!("{u}/{id}/delete"), Some(json!({}))).await {
                    Ok(_) => { msg.ok("Schedule deleted."); load() }
                    Err(e) => msg.err(e),
                }
            })
        })
    };
    let zone_list = StoredValue::new(zones());
    let editor = move |r: RwSignal<Value>| {
        let why = move || {
            r.with(|v| {
                if v["channel"].is_null() {
                    Some("Choose a channel first.".to_string())
                } else if v["enabled"] == true {
                    ready_why.get().map(|w| format!("The message is not ready: {w}"))
                } else {
                    None
                }
            })
        };
        view! {
            <div class="card send rule">
                <h2>{move || if r.with(|v| v["id"].is_null()) { "New schedule" } else { "Change schedule" }}</h2>
                <div class="row"><label>"Where"</label>
                    <div class="seg" role="group" aria-label="where">
                        <button class="on">"Channel"</button>
                        <button disabled=true title="A DM only goes to the member who triggered a rule.">"DM"</button>
                    </div>
                    <Tip text="A schedule posts in a channel. A DM only goes to the member who triggered a rule, so DM is switched on in trigger answers, not here." icon="info" />
                </div>
                <div class="row"><label>"Channel"</label>
                    <button class="ghost chbtn" class:need=move || r.with(|v| v["channel"].is_null()) aria-haspopup="dialog" on:click=move |_| picker(r)>
                        {move || r.with(|v| v["channel"].as_str().map(chan_name)).unwrap_or_else(|| "Choose channel...".into())}
                    </button>
                </div>
                <div class="row"><label>"Time zone"</label>
                    <input list="tzlist" aria-label="time zone" prop:value=move || r.with(|v| v["spec"]["tz"].as_str().unwrap_or("").to_string())
                        on:change=move |e| { let z = event_target_value(&e); r.update(|v| v["spec"]["tz"] = json!(z)) } />
                    <datalist id="tzlist">{zone_list.get_value().into_iter().map(|z| view! { <option value=z.clone()></option> }).collect_view()}</datalist>
                    <Tip text="Times are in this zone, so 09:00 stays 09:00 when summer time starts or ends. It starts as your browser's zone." icon="info" />
                </div>
                <div class="rlines">
                    {move || (0..r.with(|v| v["spec"]["lines"].as_array().map_or(0, Vec::len))).map(|i| line_view(r, i, next, locked)).collect_view()}
                </div>
                <div class="row">
                    <AddButton label="Add time" attr:disabled=move || r.with(|v| v["spec"]["lines"].as_array().map_or(0, Vec::len) >= 10)
                        on:click=move |_| r.update(|v| if let Some(a) = v["spec"]["lines"].as_array_mut() { a.push(new_line("daily")) }) />
                </div>
                <div class="row"><label>"On"</label>
                    <div class="seg" role="group" aria-label="on or off">
                        <button class:on=move || r.with(|v| v["enabled"] == true) on:click=move |_| r.update(|v| v["enabled"] = json!(true))>"On"</button>
                        <button class:on=move || r.with(|v| v["enabled"] != true) on:click=move |_| r.update(|v| v["enabled"] = json!(false))>"Off"</button>
                    </div>
                </div>
                {move || why().map(|w| view! { <p class="err">{w}</p> })}
                <div class="row">
                    <button disabled=move || why().is_some() || busy.get() on:click=move |_| save(r.get_untracked())>{busy_label(move || busy.get(), "Save schedule")}</button>
                    <button class="ghost" on:click=move |_| editing.set(None)>"Cancel"</button>
                </div>
            </div>
        }
    };
    let card = move |v: Value| {
        let id = v["id"].as_i64().unwrap_or(0);
        let on = v["enabled"] == true;
        let tz = v["spec"]["tz"].as_str().unwrap_or("UTC").to_string();
        let lines: Vec<String> = v["lines"].as_array().into_iter().flatten().filter_map(|l| l["text"].as_str().map(String::from)).collect();
        let next = v["next"].as_i64().map(|n| format!(" · next: {}", when(n, &tz))).unwrap_or_default();
        let ch = v["channel"].as_str().map(chan_name).unwrap_or_else(|| "no channel".into());
        let head = format!("{} → {ch}{next}", lines.join(" · "));
        let issue = v["issue"].as_str().map(String::from);
        let v2 = v.clone();
        view! {
            <li class="rcard" class:off=!on>
                <div><b>{head}</b>
                    <div class="muted small">{if on { format!("On · {tz}") } else { format!("Off · {tz}") }}</div>
                    {issue.map(|i| view! { <div class="err small">{i}</div> })}
                </div>
                <span class="sp"></span>
                <button class="ghost" disabled=locked on:click=move |_| open_editing(v2.clone())>"Change"</button>
                <button class="ghost" aria-label="delete schedule" disabled=locked on:click=move |_| remove(id)>{crate::icons::icon("trash")}</button>
            </li>
        }
    };
    view! {
        <div class="card sentlog rules">
            <h2>"Schedules"</h2>
            {move || match embed.get() {
                None => view! { <p class="muted">"Save the message first, then add a schedule."</p> }.into_any(),
                Some(_) => view! {
                    <p class="muted small">"Send this message by itself, on a date, on repeat or after a number of member messages. It pauses while panic mode, a lockdown or an anti-nuke case is on."</p>
                    {move || {
                        let list = rules.get();
                        (!list.is_empty()).then(|| view! { <ul>{list.into_iter().map(card).collect_view()}</ul> })
                    }}
                    {move || {
                        let new = move || open_editing(blank_rule());
                        let busy = Signal::derive(move || editing.with(Option::is_some));
                        if locked.get() {
                            view! { <p class="muted small">"Only the Owner, Extra Owners and Trusted Admins make or change schedules."</p> }.into_any()
                        } else if rules.with(Vec::is_empty) {
                            view! { <Empty title="No schedules yet" hint="Pick when Brim sends this message: a date, a repeat or every so many member messages." label="Add new schedule" add=new disabled=busy /> }.into_any()
                        } else {
                            view! { <div class="row"><AddButton label="Add new schedule" attr:disabled=move || busy.get() || !rules.with(|l| room(l.len(), "per_message")) on:click=move |_| new() /><Count n=Signal::derive(move || rules.with(Vec::len)) key="per_message" /></div> }.into_any()
                        }
                    }}
                }.into_any(),
            }}
        </div>
        {move || editing.get().map(editor)}
        {crate::msgtrig::triggers(base.get_value(), cur, dirty, rank, chan_name, picker, msg)}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_calendar() {
        assert_eq!(month_len(2028, 2), 29);
        assert_eq!(month_len(2027, 2), 28);
        assert_eq!(month_len(2100, 2), 28);
        assert_eq!(month_len(2000, 2), 29);
        assert_eq!(month_len(2026, 11), 30);
        // 1 November 2026 is a Sunday; 1 March 2028 a Wednesday.
        assert_eq!(first_weekday(2026, 11), 6);
        assert_eq!(first_weekday(2028, 3), 2);
        assert_eq!(first_weekday(2026, 1), 3);
    }
}

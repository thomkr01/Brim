//! The Tickets page's cards under the settings: Publish, the ticket list and the 30-day stats.
use crate::common::*;
use crate::form::{items, name_of};
use crate::records::{learn_names, pager};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// "45 s", "12 min", "3 h", "2 days": a length of time in its largest unit.
fn span(secs: i64) -> String {
    match secs.max(0) {
        s @ 0..60 => format!("{s} s"),
        s @ 60..3600 => format!("{} min", s / 60),
        s @ 3600..86400 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86400),
    }
}

/// "12 min ago" for a unix time in seconds.
fn ago(at: i64) -> String {
    let now = (js_sys::Date::now() / 1000.0) as i64;
    format!("{} ago", span(now - at))
}

/// Posts one message with a button (or a dropdown entry) per picked panel.
#[component]
pub fn Publish(guild: String, saved: RwSignal<u32>) -> impl IntoView {
    let (msg, need) = (Msg::default(), step_up());
    let (panels, dc) = (RwSignal::new(Vec::<Value>::new()), RwSignal::new(json!({})));
    let (channel, picked, menu) = (
        RwSignal::new(String::new()),
        RwSignal::new(Vec::<String>::new()),
        RwSignal::new(false),
    );
    // Panel messages outside the public ticket channel, offered for deletion once it exists.
    let others = RwSignal::new(Vec::<(String, String)>::new());
    let g = StoredValue::new(guild);
    let path = g.get_value();
    spawn_local(async move {
        let r = api("GET", &format!("/api/guilds/{path}/discord"), None).await;
        if let Ok(d) = r {
            dc.set(d)
        }
    });
    // The saved panels, read again after each save: only enabled ones can be posted.
    Effect::new(move || {
        saved.track();
        let path = format!("/api/guilds/{}/config", g.get_value());
        spawn_local(async move {
            let r = api("GET", &path, None).await;
            if panels.is_disposed() {
                return;
            }
            match r {
                Ok(c) => {
                    let on: Vec<Value> = c["config"]["tickets"]["panels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|p| p["enabled"] != false)
                        .cloned()
                        .collect();
                    picked.update(|ids| ids.retain(|id| on.iter().any(|p| p["id"] == id.as_str())));
                    panels.set(on);
                    let public = s(&c["config"]["tickets"]["public_channel_id"]);
                    if public.is_empty() {
                        return others.set(vec![]);
                    }
                    let posts = api("GET", &format!("/api/guilds/{}/tickets/panels/posts", g.get_value()), None).await;
                    if let Ok(p) = posts {
                        let list = p["posts"].as_array().into_iter().flatten()
                            .map(|p| (s(&p["message"]), s(&p["channel"])))
                            .filter(|(_, ch)| *ch != public)
                            .collect();
                        if !others.is_disposed() {
                            others.set(list)
                        }
                    }
                }
                Err(e) => msg.err(e),
            }
        });
    });
    let (busy, removing) = (RwSignal::new(false), RwSignal::new(false));
    let publish = move |_| {
        let g = g.get_value();
        let body = json!({"channel": channel.get_untracked(), "panels": picked.get_untracked(), "menu": menu.get_untracked()});
        run_act(
            guard(busy, async move {
                let c = api("GET", &format!("/api/guilds/{g}/config"), None).await?;
                let mut body = body;
                body["expected_version"] = c["version"].clone();
                api(
                    "POST",
                    &format!("/api/guilds/{g}/tickets/panels/publish"),
                    Some(body),
                )
                .await
            }),
            msg,
            need,
            move |_| msg.done("Published!"),
        );
    };
    view! { <div class="card">
        <h2>"Publish panels"</h2>
        <p class="muted">"Posts one message in the channel with the panels you tick. Members open a ticket from it. Save your panel changes first."</p>
        <div class="row" role="group" aria-label="Channel"><label>"Channel"</label>
            <select aria-label="channel" on:change=move |e| channel.set(event_target_value(&e))>
                <option value="" prop:selected=move || channel.get().is_empty()>"choose a channel"</option>
                {move || items(&dc.get(), "channels").into_iter().map(|(id, l)| view! { <option value=id.clone() prop:selected=move || channel.get() == id>{l}</option> }).collect_view()}
            </select>
        </div>
        <div class="row" role="group" aria-label="Panels"><label>"Panels"</label>
            <div class="pick">
                <Show when=move || panels.with(Vec::is_empty)><span class="muted">"No enabled panels saved yet."</span></Show>
                {move || panels.get().into_iter().map(|p| {
                    let (id, id2) = (s(&p["id"]), s(&p["id"]));
                    view! { <label><input type="checkbox" prop:checked=move || picked.get().contains(&id) on:change=move |e| picked.update(|ids| { ids.retain(|x| *x != id2); if event_target_checked(&e) { ids.push(id2.clone()) } }) />" "{s(&p["name"])}</label> }
                }).collect_view()}
            </div>
        </div>
        <div class="row" role="group" aria-label="Look"><label>"Look"</label>
            <div class="chips">
                <button class="ghost" class:on=move || !menu.get() on:click=move |_| menu.set(false)>"Buttons"</button>
                <button class="ghost" class:on=move || menu.get() on:click=move |_| menu.set(true)>"Dropdown"</button>
            </div>
        </div>
        <button class="go" disabled=move || channel.get().is_empty() || picked.get().is_empty() || busy.get() on:click=publish>{busy_label(move || busy.get(), "Publish")}</button>
        {move || others.get().into_iter().map(|(m, ch)| {
            let name = name_of(&dc.get(), "channels", &ch);
            let remove = move |_| {
                let (g, m, name) = (g.get_value(), m.clone(), name.clone());
                ask(
                    Ask::new("Delete the old panel message?")
                        .text(format!("Members now open tickets in #open-a-ticket. The panel message in {name} stops working once it is deleted."))
                        .yes("Delete")
                        .danger(),
                    move |_| run_act(
                        { let path = format!("/api/guilds/{g}/tickets/panels/posts/{m}/delete"); guard(removing, async move { api("POST", &path, None).await }) },
                        msg,
                        need,
                        move |_| { others.update(|o| o.retain(|(x, _)| *x != m)); msg.done("Deleted!") },
                    ),
                )
            };
            view! { <p class="info">{format!("A panel message is also posted in {}. ", name_of(&dc.get(), "channels", &ch))}
                <button class="danger" disabled=move || removing.get() on:click=remove>{busy_label(move || removing.get(), "Delete it")}</button></p> }
        }).collect_view()}
        <Toast msg />
    </div> }
}

/// Open or closed tickets, newest first, a page at a time.
#[component]
pub fn Cases(guild: String) -> impl IntoView {
    let (rows, more, msg, loaded) = (
        RwSignal::new(Vec::<Value>::new()),
        RwSignal::new(false),
        Msg::default(),
        RwSignal::new(false),
    );
    let (status, page) = (RwSignal::new("open"), RwSignal::new(0i64));
    let names = RwSignal::new(std::collections::HashMap::<String, String>::new());
    let g = StoredValue::new(guild);
    let load = move || {
        if g.is_disposed() {
            return;
        }
        let (st, pg) = (status.get_untracked(), page.get_untracked());
        let path = format!(
            "/api/guilds/{}/tickets/cases?status={st}&page={pg}",
            g.get_value()
        );
        spawn_local(async move {
            let r = api("GET", &path, None).await;
            // The page is gone, or a newer status or page was asked for meanwhile.
            if (status.try_get_untracked(), page.try_get_untracked()) != (Some(st), Some(pg)) {
                return;
            }
            match r {
                Ok(v) => {
                    msg.clear();
                    let list = v["rows"].as_array().cloned().unwrap_or_default();
                    let ids = list
                        .iter()
                        .flat_map(|r| [s(&r["user"]), s(&r["claimed_by"])])
                        .collect();
                    learn_names(g.get_value(), ids, names);
                    more.set(v["more"] == true);
                    rows.set(list)
                }
                Err(e) => msg.err(e),
            }
            loaded.set(true);
        });
    };
    load();
    let name = move |id: String| names.with(|n| n.get(&id).cloned()).unwrap_or(id);
    let pick = move |k: &'static str| {
        status.set(k);
        page.set(0);
        load()
    };
    view! { <div class="card">
        <h2>"Tickets"</h2>
        <div class="chips">
            <button class="ghost" class:on=move || status.get() == "open" on:click=move |_| pick("open")>"Open"</button>
            <button class="ghost" class:on=move || status.get() == "closed" on:click=move |_| pick("closed")>"Closed"</button>
        </div>
        <Show when=move || loaded.get() && rows.with(Vec::is_empty) fallback=move || view! {
            <table class="data">
                <thead><tr><th>"#"</th><th>"Member"</th><th>"Panel"</th><th>"Claimed by"</th><th>"Priority"</th><th>"Last activity"</th>{move || (status.get() == "closed").then(|| view! { <th>"Transcript"</th> })}</tr></thead>
                <tbody>{move || rows.get().into_iter().map(|r| {
                    let (chip, prio) = match r["priority"].as_i64() { Some(0) => ("", "low"), Some(2) => ("chip orange", "high"), Some(3) => ("chip bad", "urgent"), _ => ("", "normal") };
                    let claimed = r["claimed_by"].as_str().map(|c| name(c.to_string())).unwrap_or_else(|| "-".into());
                    let log = (status.get_untracked() == "closed").then(|| {
                        let t = &r["transcript"];
                        view! { <td data-label="Transcript">{t.is_object().then(|| view! { <a href=format!("#/{}/tickets/log/{}/{}", g.get_value(), s(&t["channel"]), s(&t["message"]))>"View"</a> })}</td> }
                    });
                    view! { <tr>
                        <td>{r["number"].as_i64().unwrap_or(0)}</td>
                        <td data-label="Member">{name(s(&r["user"]))}</td>
                        <td data-label="Panel">{r["panel"].as_str().map_or_else(|| "DM".to_string(), String::from)}</td>
                        <td data-label="Claimed by">{claimed}</td>
                        <td data-label="Priority"><span class=chip>{prio}</span></td>
                        <td data-label="Last activity" class="muted">{r["last_activity_at"].as_i64().map(ago)}</td>
                        {log}
                    </tr> }
                }).collect_view()}</tbody>
            </table>
        }><p class="muted">{move || format!("No {} tickets.", status.get())}</p></Show>
        // The pager reads 50 rows as "a next page exists"; this API says so with `more`.
        {pager(page, move || if more.get() { 50 } else { 0 }, load)}
        <Toast msg />
    </div> }
}

/// Tickets of the last 30 days: counts, the median first staff answer and opens per panel.
#[component]
pub fn Stats(guild: String) -> impl IntoView {
    let (data, msg) = (RwSignal::new(Value::Null), Msg::default());
    let path = format!("/api/guilds/{guild}/tickets/stats");
    spawn_local(async move {
        let r = api("GET", &path, None).await;
        if data.is_disposed() {
            return;
        }
        match r {
            Ok(v) => data.set(v),
            Err(e) => msg.err(e),
        }
    });
    let num = move |k: &str| data.with(|d| d[k].as_i64().unwrap_or(0).to_string());
    view! { <div class="card">
        <h2>"Last 30 days"</h2>
        <Show when=move || !data.with(Value::is_null)>
            <div class="facts">
                <crate::pages::Stat label="Opened" value=num("opened") copy=false />
                <crate::pages::Stat label="Closed" value=num("closed") copy=false />
                <crate::pages::Stat label="Open now" value=num("open") copy=false />
                <crate::pages::Stat label="Median first staff answer" value=data.with(|d| d["median_first_response"].as_i64().map_or_else(|| "-".into(), span)) copy=false />
            </div>
            <table class="data">
                <thead><tr><th>"Panel"</th><th>"Opened"</th></tr></thead>
                <tbody>{data.with(|d| d["panels"].as_array().cloned().unwrap_or_default()).into_iter().map(|p| view! { <tr>
                    <td>{p["panel"].as_str().map_or_else(|| "DM".to_string(), String::from)}</td>
                    <td data-label="Opened">{p["opened"].as_i64().unwrap_or(0)}</td>
                </tr> }).collect_view()}</tbody>
            </table>
        </Show>
        <Toast msg />
    </div> }
}

#[cfg(test)]
mod tests {
    #[test]
    fn spans_use_the_largest_unit() {
        use super::span;
        assert_eq!(span(-5), "0 s");
        assert_eq!(span(59), "59 s");
        assert_eq!(span(60), "1 min");
        assert_eq!(span(7300), "2 h");
        assert_eq!(span(86400 * 3), "3 days");
    }
}

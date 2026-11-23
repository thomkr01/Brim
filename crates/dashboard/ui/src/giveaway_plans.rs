//! Giveaway templates and plans on the Giveaways page: the two lists, and the "When" part of the
//! form (a schedule written like a message schedule, with the same time lines). The form itself is
//! the Start form (`giveaways.rs`).
use crate::common::*;
use crate::form::ListPick;
use crate::giveaways::{now, s};
use crate::msgrules::{browser_zone, line_view, new_line, zones};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

/// The Start form's draft as a new plan: a name, a daily time and no wait.
pub fn as_plan(mut v: Value) -> Value {
    v["kind"] = json!("plan");
    v["name"] = json!("");
    v["enabled"] = json!(true);
    v["wait_for_end"] = json!(false);
    v["spec"] = json!({"tz": browser_zone(), "lines": [new_line("daily")]});
    v
}

/// The schedule of the open plan: zone, times with their next runs, wait, on or off. `url`: the
/// plans route, for the preview of the next runs.
#[component]
pub fn When(d: RwSignal<Value>, url: String) -> impl IntoView {
    let next = RwSignal::new(json!({}));
    let url = StoredValue::new(url);
    let seq = StoredValue::new(0u32);
    let spec = Memo::new(move |_| d.with(|v| v["spec"].clone()));
    // The next runs, asked again when a time or the zone changes; an older answer that lands last is dropped.
    Effect::new(move |_| {
        let spec = spec.get();
        seq.update_value(|n| *n += 1);
        let mine = seq.get_value();
        let u = format!("{}/preview", url.get_value());
        spawn_local(async move {
            let v = api(
                "POST",
                &u,
                Some(json!({"spec": spec})),
            )
            .await;
            if seq.try_get_value() == Some(mine) {
                next.try_set(v.unwrap_or_else(|e| json!({"error": e})));
            }
        });
    });
    let zone_list = StoredValue::new(zones());
    let count = move || d.with(|v| v["spec"]["lines"].as_array().map_or(0, Vec::len));
    view! {
        <h3>"When"</h3>
        <div class="row"><label>"Time zone"</label>
            <ListPick items=Signal::derive(move || zone_list.get_value().into_iter().map(|z| (z.clone(), z)).collect())
                get=Signal::derive(move || d.with(|v| s(&v["spec"]["tz"])))
                set=Callback::new(move |z: String| d.update(|v| v["spec"]["tz"] = json!(z)))
                none="choose a time zone" label="time zone" /></div>
        <div class="rlines">{move || (0..count()).map(|i| line_view(d, i, next, Signal::derive(|| false), false)).collect_view()}</div>
        <div class="row"><AddButton label="Add time" attr:disabled=move || { count() >= 10 }
            on:click=move |_| d.update(|v| if let Some(a) = v["spec"]["lines"].as_array_mut() { a.push(new_line("daily")) }) /></div>
        <div class="row"><label for="gw-wait">"Wait for the last giveaway"</label>
            <input id="gw-wait" type="checkbox" class="sw" role="switch" prop:checked=move || d.with(|v| v["wait_for_end"] == true)
                on:change=move |e| { let c = event_target_checked(&e); d.update(|v| v["wait_for_end"] = json!(c)) } /></div>
        <p class="muted small hint">"On: a run starts only after the giveaway this plan started before has ended. Off: it starts on time. A run that is over the limit of running giveaways is skipped."</p>
        <div class="row"><label for="gw-on">"Plan on"</label>
            <input id="gw-on" type="checkbox" class="sw" role="switch" prop:checked=move || d.with(|v| v["enabled"] == true)
                on:change=move |e| { let c = event_target_checked(&e); d.update(|v| v["enabled"] = json!(c)) } /></div>
    }
}

/// Plans, then templates, with their actions. `on_use`: a template into a new Start form.
#[component]
pub fn Lists(
    data: RwSignal<Option<Value>>,
    chan: impl Fn(&str) -> String + Copy + Send + Sync + 'static,
    busy: RwSignal<bool>,
    on_new: Callback<()>,
    on_use: Callback<Value>,
    on_edit: Callback<Value>,
    on_delete: Callback<Value>,
) -> impl IntoView {
    let list = move |key: &'static str| {
        data.with(|v| {
            v.as_ref()
                .map(|v| v["plans"][key].as_array().cloned().unwrap_or_default())
                .unwrap_or_default()
        })
    };
    let plan_row = move |p: Value| {
        let (p1, p2) = (p.clone(), p.clone());
        let lines: Vec<String> = p["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["text"].as_str().map(String::from))
            .collect();
        let mut line = vec![lines.join(" · "), format!("in {}", chan(&s(&p["channel"])))];
        match p["next"].as_i64() {
            Some(n) if p["held"] == true => line.push(format!(
                "waits for the last giveaway, looks again {}",
                crate::discord::relative(n - now())
            )),
            Some(n) => line.push(format!("next {}", crate::discord::relative(n - now()))),
            None => {}
        }
        let issue = p["issue"].as_str().map(String::from);
        view! {
            <li class="card">
                <div class="row"><b>{s(&p["name"])}</b><span class="sp"></span>
                    {(p["enabled"] != true).then(|| view! { <span class="chip">"Off"</span> })}</div>
                <div class="muted small">{format!("{} · {}", s(&p["prize"]), line.join(" · "))}</div>
                {issue.map(|i| view! { <div class="err small">{i}</div> })}
                <div class="row"><span class="sp"></span>
                    <button class="ghost" disabled=move || busy.get() on:click=move |_| on_edit.run(p1.clone())>"Edit"</button>
                    <button class="danger" aria-label="delete this plan" disabled=move || busy.get() on:click=move |_| on_delete.run(p2.clone())>"Delete"</button></div>
            </li>
        }
    };
    let template_row = move |t: Value| {
        let (t1, t2, t3) = (t.clone(), t.clone(), t.clone());
        view! {
            <li class="card">
                <div class="row"><b>{s(&t["name"])}</b><span class="sp"></span></div>
                <div class="muted small">{format!("{} · {} winner{}", s(&t["prize"]), t["winners"], if t["winners"] == 1 { "" } else { "s" })}</div>
                <div class="row"><span class="sp"></span>
                    <button disabled=move || busy.get() on:click=move |_| on_use.run(t1.clone())>"Use"</button>
                    <button class="ghost" disabled=move || busy.get() on:click=move |_| on_edit.run(t2.clone())>"Edit"</button>
                    <button class="danger" aria-label="delete this template" disabled=move || busy.get() on:click=move |_| on_delete.run(t3.clone())>"Delete"</button></div>
            </li>
        }
    };
    view! {
        <div class="row"><h2>"Plans"</h2><span class="sp"></span>
            <AddButton label="New plan" attr:disabled=move || busy.get() on:click=move |_| on_new.run(()) /></div>
        <p class="muted small">"A plan starts a giveaway for you: on a date, or again and again. It stops when the member who saved it can no longer manage giveaways."</p>
        {move || { let l = list("plans"); if l.is_empty() { view! { <p class="muted">"No plans yet."</p> }.into_any() } else { view! { <ul class="stack">{l.into_iter().map(plan_row).collect_view()}</ul> }.into_any() } }}
        <h2>"Templates"</h2>
        <p class="muted small">"A template keeps the settings of a giveaway. Start a giveaway, then press Save as template."</p>
        {move || { let l = list("templates"); if l.is_empty() { view! { <p class="muted">"No templates yet."</p> }.into_any() } else { view! { <ul class="stack">{l.into_iter().map(template_row).collect_view()}</ul> }.into_any() } }}
    }
}

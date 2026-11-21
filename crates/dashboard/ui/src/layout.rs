//! Layout Guard card on the Channel Setup page (package L1): save the server as its server layout
//! and read what differs in Discord. The layout versions on History, with Roll back.
//! Nothing here changes Discord.
use crate::common::*;
use crate::icons::icon;
use leptos::prelude::*;
use serde_json::Value;

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn list(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

#[component]
pub fn LayoutGuard(
    guild: String,
    msg: Msg,
    /// Raised after a push, which may save a new layout version.
    reload: RwSignal<u32>,
) -> impl IntoView {
    let data = RwSignal::new(Value::Null);
    // Serious problems that stopped a save, as Overview shows them.
    let blocking = RwSignal::new(Vec::<Value>::new());
    let busy = RwSignal::new(false);
    let need = step_up();
    let access = expect_context::<Access>();
    let g = StoredValue::new(guild);
    let load = move || {
        let p = format!("/api/guilds/{}/layout", g.get_value());
        leptos::task::spawn_local(async move {
            let r = api("GET", &p, None).await;
            if data.is_disposed() {
                return;
            }
            match r {
                Ok(v) => data.set(v),
                Err(e) => msg.err(format!("Layout Guard could not be loaded: {e}")),
            }
        });
    };
    Effect::new(move |_| {
        reload.track();
        load();
    });
    let saved = move || data.with(|v| !v["layout"].is_null());
    let save = move |_| {
        let again = saved();
        let a = if again {
            Ask::new("Save the server again?").text("The server as it is in Discord now becomes the new server layout, and the differences list starts empty. Nothing in Discord changes.")
        } else {
            Ask::new("Save current server as layout?").text("Brim saves the roles and their order, categories, channels, who can see and write, and the main server settings as they are now. Nothing in Discord changes.")
        };
        ask(a.yes("Save"), move |_| {
            busy.set(true);
            let p = format!("/api/guilds/{}/layout/save", g.get_value());
            run_act(
                async move {
                    let r = api("POST", &p, None).await;
                    busy.set(false);
                    r
                },
                msg,
                need,
                move |v| {
                    if v["saved"] == true {
                        blocking.set(vec![]);
                        msg.done(format!(
                            "Done. The server is saved as layout v{}.",
                            v["layout"]["version"]
                        ));
                        data.set(v);
                    } else {
                        blocking.set(list(&v["blocking"]));
                        msg.err(
                            "Not saved: fix the serious problems listed under Layout Guard first.",
                        );
                    }
                },
            )
        })
    };
    let low = move || access.rank() < 3;
    let button = move || {
        let again = saved();
        // Saving again takes whatever Discord holds now, so only the Owner or an Extra Owner may.
        let can = if again {
            data.with(|v| v["can_save_again"] == true)
        } else {
            !low()
        };
        let label = if again {
            "Save again"
        } else {
            "Save current server as layout"
        };
        let why = if again {
            "Needs the Owner or an Extra Owner"
        } else {
            "Needs a Trusted Admin or the owner"
        };
        view! { <button class=if again { "ghost" } else { "go" } disabled=move || busy.get() || !can title=(!can).then_some(why) on:click=save>{move || if busy.get() { view! { <Working /> }.into_any() } else { label.into_any() }}</button> }
    };
    let diffs = move || data.with(|v| list(&v["differences"]));
    let status = move || {
        let n = diffs().len();
        data.with(|v| {
            let l = &v["layout"];
            let chip = if n == 0 {
                view! { <span class="chip green">"Matches Discord"</span> }.into_any()
            } else {
                view! { <span class="chip orange">{format!("{n} difference{}", if n == 1 { "" } else { "s" })}</span> }.into_any()
            };
            view! { <span class="muted">{format!("Layout v{}, saved {}", l["version"], s(&l["saved_at"]))}</span>" "{chip} }
        })
    };
    let row = |d: Value| {
        let who = match (d["by"].as_str(), d["at"].as_str()) {
            (Some(b), Some(t)) => format!(" By {b}, {t}."),
            _ => String::new(),
        };
        view! { <div class="note"><b>{s(&d["name"])}</b>" "<span class="muted">{s(&d["what"])}</span>" "{s(&d["change"])}"."<span class="muted">{who}</span></div> }
    };
    let gl = g.get_value();
    view! {
        <div class="card lg">
            <div class="ttl"><h2>{icon("shield")}"Layout Guard"</h2><span class="sp"></span>{move || saved().then(status)}{button}</div>
            {move || if data.with(Value::is_null) {
                view! { <p class="loading"><Working /></p> }.into_any()
            } else if !saved() {
                view! { <p class="muted">"No server layout is saved yet. Save the server as it is now, and this list shows every change made in Discord after that. Nothing in Discord changes."</p> }.into_any()
            } else if diffs().is_empty() {
                view! { <p class="muted">"Discord matches the saved server layout."</p> }.into_any()
            } else {
                view! { <details><summary>"What differs from the saved layout"</summary>{diffs().into_iter().map(row).collect_view()}<p class="muted">"Only shown: nothing is put back yet."</p></details> }.into_any()
            }}
            {move || (!blocking.with(Vec::is_empty)).then(|| view! {
                <div class="note chk bad"><div><b class="err">{icon("error")}" Fix these first"</b>" "<span class="muted">"A server with these problems is not saved as correct."</span>
                    <ul>{blocking.get().into_iter().map(|c| view! { <li><b>{s(&c["title"])}</b>" "<span class="muted">{s(&c["detail"])}</span></li> }).collect_view()}</ul></div>
                    <span class="acts"><a class="btn" href=format!("#/{gl}/overview")>"Fix on Overview"</a></span></div>
            })}
        </div>
    }
}

/// The server layout versions on History, newest first. Roll back makes an older version the
/// current layout as a new version; Discord is not changed.
#[component]
pub fn LayoutVersions(guild: String) -> impl IntoView {
    // The notes name channels, so the list needs the level that reads Channel Setup.
    if expect_context::<Access>().rank() < 3 {
        return ().into_any();
    }
    let data = RwSignal::new(Value::Null);
    let blocking = RwSignal::new(Vec::<Value>::new());
    let (msg, need) = (Msg::default(), step_up());
    let g = StoredValue::new(guild);
    let load = move || {
        let p = format!("/api/guilds/{}/layout/versions", g.get_value());
        leptos::task::spawn_local(async move {
            let r = api("GET", &p, None).await;
            if data.is_disposed() {
                return;
            }
            match r {
                Ok(v) => data.set(v),
                Err(e) => msg.err(format!("Server layout versions could not be loaded: {e}")),
            }
        });
    };
    load();
    let newest = move || data.with(|v| v["versions"][0]["version"].as_i64().unwrap_or(0));
    let roll = move |v: i64| {
        let cur = newest();
        ask(
            Ask::new(format!("Roll the server layout back to v{v}?"))
                .text(format!("v{v} becomes the current server layout as v{}. Discord is not changed: Layout Guard then lists what differs.", cur + 1))
                .yes("Roll back"),
            move |_| {
                let p = format!("/api/guilds/{}/layout/{v}/rollback", g.get_value());
                let body = serde_json::json!({"expected_version": cur});
                run_change(
                    async move { api("POST", &p, Some(body)).await },
                    msg,
                    need,
                    move |r| {
                        if r["saved"] == true {
                            blocking.set(vec![]);
                            msg.done(format!("Done. v{v} is the current server layout as v{}.", r["version"]));
                        } else {
                            blocking.set(list(&r["blocking"]));
                            msg.err(format!("Not rolled back: v{v} has serious problems, listed below."));
                        }
                        load();
                    },
                )
            },
        )
    };
    let source = |v: &Value| match v["source"].as_str().unwrap_or_default() {
        "import" => "saved from the server".to_string(),
        "dashboard push" => "Channel Setup push".to_string(),
        "rollback" => "roll back".to_string(),
        o => o.to_string(),
    };
    let row = move |r: Value| {
        let v = r["version"].as_i64().unwrap_or(0);
        let current = v == newest();
        let can = data.with(|d| d["can_roll_back"] == true);
        let note = s(&r["note"]);
        view! {
            <div class="card row">
                <b>{format!("Layout v{v}")}</b>
                <span class="muted">{format!("{}, by {} at {}", source(&r), s(&r["by"]), s(&r["at"]))}</span>
                {(!note.is_empty()).then(|| view! { <span class="muted">{note}</span> })}
                {if current {
                    view! { <span class="chip green">"Current"</span> }.into_any()
                } else {
                    view! { <button class="ghost" disabled=!can title=(!can).then_some("Needs the Owner or an Extra Owner") on:click=move |_| roll(v)>"Roll back"</button> }.into_any()
                }}
            </div>
        }
    };
    view! {
        <h3>"Server layout (Layout Guard)"</h3>
        {move || match data.with(|d| d.is_null()) {
            true => view! { <div class="skel"></div> }.into_any(),
            false => {
                let rows = data.with(|d| list(&d["versions"]));
                if rows.is_empty() {
                    view! { <p class="muted">"No server layout is saved yet. Save one on the Channel Setup page."</p> }.into_any()
                } else {
                    rows.into_iter().map(row).collect_view().into_any()
                }
            }
        }}
        {move || (!blocking.with(Vec::is_empty)).then(|| view! {
            <div class="note chk bad"><div><b class="err">{icon("error")}" Not rolled back"</b>" "<span class="muted">"A layout with these problems is not saved as correct."</span>
                <ul>{blocking.get().into_iter().map(|c| view! { <li><b>{s(&c["title"])}</b>" "<span class="muted">{s(&c["detail"])}</span></li> }).collect_view()}</ul></div></div>
        })}
        <Toast msg />
    }
    .into_any()
}

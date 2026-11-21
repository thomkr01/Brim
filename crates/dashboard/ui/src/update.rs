//! Bot-owner page that installs a published build of a self-hosted Brim (the newest, or one of
//! the last 10 to go back to), with live progress.
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::Value;
use std::time::Duration;

/// The repository of this server's builds: the old one until its owner moves it to the Brim names.
fn repo(d: &Value) -> &'static str {
    if d["old_names"].as_bool() == Some(true) {
        "https://github.com/thomkr01/TommyBot"
    } else {
        "https://github.com/thomkr01/Brim"
    }
}

/// What the Update button does not update: the updater and docker-compose.yml. As the dashboard's
/// `update::server_cmd`: no `git pull` under the old names (it would bring the Brim compose file).
fn server_cmd(d: &Value) -> &'static str {
    if d["old_names"].as_bool() == Some(true) {
        "cd ~/tommybot && docker compose pull && docker compose up -d"
    } else {
        "cd ~/brim && git pull && docker compose pull && docker compose up -d"
    }
}

const STEPS: [&str; 5] = [
    "Checking the running version",
    "Downloading the new version",
    "Backing up the database and restarting the bot",
    "Restarting the dashboard (and MCP server)",
    "Checking that everything runs",
];

fn short(v: &Value) -> String {
    v.as_str()
        .map(|s| s.chars().take(7).collect())
        .unwrap_or_else(|| "unknown".into())
}

/// The updater is down only when the server said so: before the first answer nothing is known.
fn updater_down(d: &Value) -> bool {
    !d.is_null() && !d["updater"].as_bool().unwrap_or(false)
}

fn dur(s: i64) -> String {
    if s >= 60 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

/// Share done (0 to 1) and seconds left, from the per-step estimate and the time spent in the current step.
fn progress(est: &[i64], step: usize, elapsed: i64) -> (f64, i64) {
    let step = step.min(est.len() - 1);
    let total: i64 = est.iter().sum::<i64>().max(1);
    let done = est[..step].iter().sum::<i64>() + elapsed.clamp(0, est[step]);
    let left = est[step + 1..].iter().sum::<i64>() + (est[step] - elapsed).max(1);
    (done as f64 / total as f64, left)
}

#[component]
pub fn Update() -> impl IntoView {
    let data = RwSignal::new(Value::Null);
    // Set while the dashboard cannot be reached, which is expected while it restarts itself.
    let away = RwSignal::new(false);
    // Set when the first answer failed: the page then says so instead of waiting for ever.
    let failed = RwSignal::new(false);
    let loaded_sha = StoredValue::new(None::<Value>);
    // The server's step time is whole seconds and only as fresh as the last answer, so the page
    // counts on from it with its own clock: the time left and the bar move smoothly, not once per
    // slow answer.
    let (fetched, now) = (StoredValue::new(js_sys::Date::now()), RwSignal::new(js_sys::Date::now()));
    let clock = set_interval_with_handle(move || now.set(js_sys::Date::now()), Duration::from_millis(250));
    on_cleanup(move || {
        if let Ok(h) = clock {
            h.clear()
        }
    });
    let msg = Msg::default();
    let need = step_up();
    let load = move || {
        spawn_local(async move {
            let r = api("GET", "/api/update", None).await;
            // The user may have left the page while the request ran (the dashboard restarts mid-update).
            if data.is_disposed() {
                return;
            }
            match r {
                Ok(v) => {
                    if loaded_sha.get_value().is_none() {
                        loaded_sha.set_value(Some(v["current"].clone()))
                    }
                    fetched.set_value(js_sys::Date::now());
                    data.set(v);
                    away.set(false);
                }
                Err(e) if data.get_untracked().is_null() => {
                    failed.set(true);
                    msg.err(e)
                }
                Err(_) => away.set(true),
            }
        })
    };
    load();
    let active = move || {
        matches!(
            data.get()["run"]["status"].as_str(),
            Some("queued" | "running")
        )
    };
    let poll = set_interval_with_handle(
        move || {
            if active() || away.get_untracked() {
                load()
            }
        },
        Duration::from_secs(1),
    );
    on_cleanup(move || {
        if let Ok(h) = poll {
            h.clear()
        }
    });
    // Installs exactly this build: the server pulls its images by digest and refuses anything else.
    let start = move |sha: String| {
        run_act(
            api("POST", "/api/update", Some(serde_json::json!({"sha": sha}))),
            msg,
            need,
            move |_| load(),
        )
    };

    let head = move || {
        let d = data.get();
        let gh = repo(&d);
        let (cur, new) = (
            d["current"].as_str().map(String::from),
            d["latest"]["sha"].as_str().map(String::from),
        );
        // "Newest" can be older than what runs (it is cached, and a finished update may run images
        // of a newer commit): only an answer that Newest is ahead offers the update.
        let offer = d["newer"]
            .as_bool()
            .unwrap_or_else(|| cur.is_some() && new.is_some() && cur != new);
        let fresh = cur.is_some() && new.is_some() && !offer;
        let news = cur.zip(new).filter(|_| offer).map(|(c, n)| view! { " " <a href=format!("{GH}/compare/{c}...{n}") target="_blank" rel="noopener">"See what's new"</a> });
        // What the update brings: one line per change, newest first (plain text).
        let more = d["more"].as_u64().unwrap_or(0);
        let changes = (offer && d["changes"].as_array().is_some_and(|a| !a.is_empty())).then(|| view! {
            <ul class="small">
                {d["changes"].as_array().cloned().unwrap_or_default().into_iter().map(|c| {
                    let pr = c["pr"].as_u64().unwrap_or(0);
                    let what = c["what"].as_str().unwrap_or("").to_string();
                    view! { <li><a href=format!("{gh}/pull/{pr}") target="_blank" rel="noopener">{format!("#{pr}")}</a>" "
                        {if what.is_empty() { view! { <span class="muted">"No description"</span> }.into_any() } else { what.into_any() }}</li> }
                }).collect_view()}
                {(more > 0).then(|| view! { <li class="muted">{format!("and {more} more")}</li> })}
            </ul>
        });
        let latest_sha = d["latest"]["sha"].as_str().unwrap_or("").to_string();
        // Up to date but on another commit: what runs is newer than the list, so it is the newest.
        let newest = if fresh { &d["current"] } else { &d["latest"]["sha"] };
        // An updater from before it reported its commit would pull whatever is tagged: it must go first.
        let old_updater = d["updater"].as_bool().unwrap_or(false) && d["updater_sha"].is_null();
        let behind: Vec<String> = d["behind"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let side = (!old_updater && !behind.is_empty()).then(|| view! {
            <p class="warn">{format!("Changed since the server was last updated by hand: {}. The Update button does not update these. On the server run ", behind.join(", "))}<code>{server_cmd(&d)}</code>" once."</p>
        });
        view! {
            <div class="card">
                <p><b>"Running: "</b><code>{short(&d["current"])}</code></p>
                <p><b>"Newest: "</b>{if d["latest"].is_null() { view! { <span class="muted">"Could not reach GitHub right now."</span> }.into_any() } else { view! { <code>{short(newest)}</code>" "{news} }.into_any() }}</p>
                {changes}
                {side}
                {if updater_down(&d) {
                    view! { <p class="warn">"The updater is not running, so updates cannot start. On the server run "<code>{if d["old_names"].as_bool() == Some(true) { "cd ~/tommybot && docker compose up -d updater" } else { "cd ~/brim && docker compose up -d updater" }}</code>" once."</p> }.into_any()
                } else if old_updater {
                    view! { <p class="warn">"The updater on the server is too old to install an approved build. On the server run "<code>{server_cmd(&d)}</code>" once."</p> }.into_any()
                } else if fresh {
                    view! { <p class="ok">"Brim is up to date."</p> }.into_any()
                } else if active() {
                    ().into_any()
                } else {
                    view! { <button class="go" on:click=move |_| start(latest_sha.clone())>"Update now"</button> }.into_any()
                }}
            </div>
        }
    };

    // The other published builds, to go back to one (GHCR keeps the newest 10).
    let earlier = move || {
        let d = data.get();
        let gh = repo(&d);
        let cur = d["current"].as_str().unwrap_or("").to_string();
        let builds: Vec<Value> = d["builds"].as_array().cloned().unwrap_or_default();
        let can =
            d["updater"].as_bool().unwrap_or(false) && !d["updater_sha"].is_null() && !active();
        let rows = builds.into_iter().filter(|b| b["sha"].as_str() != Some(cur.as_str())).map(|b| {
            let sha = b["sha"].as_str().unwrap_or("").to_string();
            let at = b["at"].as_str().unwrap_or("").replace('T', " ").chars().take(16).collect::<String>();
            let pick = sha.clone();
            let lines = b["changes"].as_array().cloned().unwrap_or_default().into_iter().map(|c| {
                let pr = c["pr"].as_u64().unwrap_or(0);
                let what = c["what"].as_str().unwrap_or("").to_string();
                view! { <li><a href=format!("{gh}/pull/{pr}") target="_blank" rel="noopener">{format!("#{pr}")}</a>" "{if what.is_empty() { view! { <span class="muted">"No description"</span> }.into_any() } else { what.into_any() }}</li> }
            }).collect_view();
            view! { <li><a href=format!("{gh}/commit/{sha}") target="_blank" rel="noopener"><code>{short(&b["sha"])}</code></a>" "<span class="muted">{at}" UTC"</span>" "
                {can.then(|| view! { <button class="ghost" on:click=move |_| start(pick.clone())>"Install"</button> })}
                <ul class="small builds">{lines}</ul></li> }
        }).collect_view();
        view! {
            <div class="card">
                <h3>"Older published builds"</h3>
                <p class="muted">"Install any of the recent builds, for example to go back to one that worked. The database stays as it is: the server keeps a backup from before each update."</p>
                <ul class="checks">{rows}</ul>
            </div>
        }
    };

    let run = move || {
        let d = data.get();
        let gh = repo(&d);
        let r = &d["run"];
        let status = r["status"].as_str()?.to_string();
        let est: Vec<i64> = d["estimate"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .filter(|e: &Vec<i64>| e.len() == STEPS.len())
            .unwrap_or_else(|| vec![10; STEPS.len()]);
        let step = r["step"].as_u64().unwrap_or(0) as usize;
        let live = status == "running";
        let elapsed0 = r["step_elapsed"].as_i64().unwrap_or(0);
        let rows = STEPS
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let class = match status.as_str() {
                    "done" => "ok",
                    "running" if i == step => "run",
                    "failed" | "rolled_back" if i == step => "err",
                    _ if i < step => "ok",
                    _ => "",
                };
                let took = r["step_times"]
                    .get(i)
                    .and_then(Value::as_i64)
                    .map(|s| format!(" · {}", dur(s)));
                view! { <li class=class>{*name}<span class="muted">{took}</span></li> }
            })
            .collect_view();
        let summary = match status.as_str() {
            "queued" => view! { <p class="muted"><Working />" Waiting for the updater to pick it up..."</p> }.into_any(),
            "running" => {
                // Whole seconds from the server, plus the time since that answer.
                let more = move || (now.get() - fetched.get_value()).max(0.0) / 1000.0;
                let est2 = est.clone();
                let share = move || {
                    let k = step.min(est.len() - 1);
                    let done = est[..k].iter().sum::<i64>() as f64 + (elapsed0 as f64 + more()).clamp(0.0, est[k] as f64);
                    done / est.iter().sum::<i64>().max(1) as f64
                };
                let left = move || progress(&est2, step, elapsed0).1 - more() as i64;
                view! {
                    <Show when=move || away.get()><Pose pose=Brim::AsleepOnDuty class="pose-top" /></Show>
                    <div class="pbar" role="progressbar" aria-label="update progress"><i style=move || format!("width:{:.1}%", share() * 100.0)></i></div>
                    <p class="muted">{move || format!("About {} left", dur(left().max(1)))}{move || away.get().then_some(". The dashboard is restarting, this page reconnects by itself.")}</p>
                }.into_any()
            }
            "done" => {
                // What came in with the installed build, from the build list (one line per change).
                let to = r["to_sha"].as_str().unwrap_or("");
                let found = d["builds"].as_array().into_iter().flatten().find(|b| b["sha"].as_str() == Some(to)).and_then(|b| b["changes"].as_array()).cloned().unwrap_or_default();
                let n = found.len();
                let lines = found.into_iter().map(|c| {
                    let pr = c["pr"].as_u64().unwrap_or(0);
                    let what = c["what"].as_str().unwrap_or("").to_string();
                    view! { <li><a href=format!("{gh}/pull/{pr}") target="_blank" rel="noopener">{format!("#{pr}")}</a>" "{if what.is_empty() { view! { <span class="muted">"No description"</span> }.into_any() } else { what.into_any() }}</li> }
                }).collect_view();
                let reload = loaded_sha.get_value().is_some_and(|s| s != d["current"]);
                view! {
                    <p class="ok">{format!("Updated to {}{}", short(&r["to_sha"]), if n == 0 { "." } else { ":" })}</p>
                    <ul class="small">{lines}</ul>
                    {reload.then(|| view! { <button on:click=|_| { web_sys::window().unwrap().location().reload().ok(); }>"Reload the dashboard"</button> })}
                }.into_any()
            }
            _ => view! { <p class="err">{r["error"].as_str().unwrap_or("The update failed.").to_string()}</p> }.into_any(),
        };
        Some(
            view! { <div class="card"><h3>{if live { "Updating" } else { "Last update" }}</h3>{(status != "done").then(|| view! { <ul class="checks">{rows}</ul> })}{summary}</div> },
        )
    };

    view! {
        <Head guild=String::new() page="Brim".into() big="Update".into() />
        {move || match (data.with(Value::is_null), failed.get()) {
            (true, false) => view! { <p><Working /></p> }.into_any(),
            (true, true) => view! { <p class="err">"The update page could not be loaded. Reload the page to try again."</p> }.into_any(),
            _ => view! { {head} {run} {earlier} }.into_any(),
        }}
        <Toast msg />
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_warning_before_the_first_answer() {
        assert!(!updater_down(&Value::Null));
        assert!(!updater_down(&serde_json::json!({"updater": true})));
        assert!(updater_down(&serde_json::json!({"updater": false})));
    }

    #[test]
    fn progress_follows_the_estimate_and_never_runs_out() {
        let est = [2, 60, 10, 10, 18];
        assert_eq!(progress(&est, 0, 0), (0.0, 100));
        assert_eq!(progress(&est, 1, 30), (0.32, 68));
        // Slower than estimated: the bar waits at the step's end and says about a second is left for it.
        assert_eq!(progress(&est, 4, 99), (1.0, 1));
    }
}

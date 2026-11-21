//! Bot-owner page for people's own data requests (`/mydata show` and `/mydata delete` in a DM).
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::Value;

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// "2026-11-19 07:40" of an API time.
pub(crate) fn when(v: &Value) -> String {
    v.as_str()
        .map(|t| t.chars().take(16).collect::<String>().replace('T', " "))
        .unwrap_or_default()
}

/// "cases.user_id: 3, notes.user_id: 1", or "nothing found".
fn tables(v: &Value) -> String {
    let parts: Vec<String> = v
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, n)| format!("{k}: {n}"))
        .collect();
    if parts.is_empty() {
        "nothing found".into()
    } else {
        parts.join(", ")
    }
}

#[component]
pub fn DataRequests() -> impl IntoView {
    let list = RwSignal::new(Vec::<Value>::new());
    let failed = RwSignal::new(false);
    let (msg, need) = (Msg::default(), step_up());
    let load = move || {
        if list.is_disposed() {
            return;
        }
        spawn_local(async move {
            let r = api("GET", "/api/data-requests", None).await;
            if list.is_disposed() {
                return;
            }
            match r {
                Ok(v) => {
                    failed.set(false);
                    list.set(v.as_array().cloned().unwrap_or_default())
                }
                Err(e) => {
                    failed.set(true);
                    msg.err(e)
                }
            }
        })
    };
    load();
    let answer = move |id: i64, approve: bool, what: String| {
        let (title, text, yes) = if approve {
            (
                format!("Approve: {what}?"),
                "Brim does it now and DMs the person. A delete cannot be undone.",
                "Approve",
            )
        } else {
            (
                format!("Deny: {what}?"),
                "Brim DMs the person that you declined.",
                "Deny",
            )
        };
        let mut a = Ask::new(title).text(text).yes(yes);
        if approve {
            a = a.danger();
        }
        ask(a, move |_| {
            let path = format!(
                "/api/data-requests/{id}/{}",
                if approve { "approve" } else { "deny" }
            );
            run_act(
                async move { api("POST", &path, None).await },
                msg,
                need,
                move |_| {
                    msg.ok("Done. Brim DMs the person.");
                    load();
                },
            )
        })
    };
    let rows = move || {
        list.get()
            .into_iter()
            .map(|r| {
                let id = r["id"].as_i64().unwrap_or_default();
                let kind = if r["kind"] == "delete" { "Delete" } else { "Show" };
                let who = format!("{} ({})", s(&r["username"]), s(&r["user_id"]));
                let what = format!("{kind} the data of {who}");
                let pending = r["status"] == "pending";
                let detail = match (pending, &r["result"]) {
                    (true, _) => tables(&r["found"]),
                    (false, Value::Null) => String::new(),
                    (false, v) => tables(v),
                };
                let w2 = what.clone();
                view! {
                    <tr class:muted=!pending>
                        <td>{who}</td>
                        <td data-label="Asks to">{kind}</td>
                        <td data-label="Asked">{when(&r["created_at"])}</td>
                        <td data-label="Rows">{detail}</td>
                        <td class="tact">{if pending {
                            view! { <span class="row">
                                <button class="danger" on:click=move |_| answer(id, true, what.clone())>"Approve"</button>
                                <button class="ghost" on:click=move |_| answer(id, false, w2.clone())>"Deny"</button>
                            </span> }.into_any()
                        } else {
                            view! { <span>{format!("{} {}", s(&r["status"]), when(&r["decided_at"]))}</span> }.into_any()
                        }}</td>
                    </tr>
                }
            })
            .collect_view()
    };
    view! {
        <Head guild=String::new() page="Brim".into() big="Data requests".into() />
        <div class="card">
            <p class="muted">"People ask by DMing Brim "<code>"/mydata show"</code>" or "<code>"/mydata delete"</code>". Answering needs 2FA."<Tip text="Show sends the person a file with what Brim stores about them, without staff members' or reporters' ids, report reasons or the text of staff notes. Delete removes what it stores about them as a member in every server: cases, notes, reports, appeals, closed tickets, strikes, invites, verification and join records, and their dashboard login, and their id in older config versions. Raider List entries, appeal and report blacklists, staff lists, their dashboard 2FA, a running quarantine and open reports and appeals are kept (marked kept); an open report, appeal or ticket and a warning that still counts stay until they end and are removed then; a server's newest case and ticket stay without their name. Check the person is not using a delete to wipe their record before approving." /></p>
        </div>
        <div class="card">
            <Show when=move || !list.get().is_empty() fallback=move || if failed.get() {
                view! { <p class="err">"Could not load the requests. "<button class="ghost" on:click=move |_| load()>"Retry"</button></p> }.into_any()
            } else {
                view! { <p class="muted">"No requests yet."</p> }.into_any()
            }>
                <table class="data">
                    <thead><tr><th>"Person"</th><th>"Asks to"</th><th>"Asked"</th><th>"Rows"</th><th></th></tr></thead>
                    <tbody>{rows}</tbody>
                </table>
            </Show>
        </div>
        <Toast msg />
    }
}

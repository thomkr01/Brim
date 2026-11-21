//! Bot-owner page for the MCP tokens: the logins of AI clients that read and propose changes (DEPLOY.md 6.1).
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn when(v: &Value) -> String {
    v.as_str()
        .map(|t| t.chars().take(16).collect::<String>().replace('T', " "))
        .unwrap_or_else(|| "never".into())
}

#[component]
pub fn McpTokens() -> impl IntoView {
    let list = RwSignal::new(Vec::<Value>::new());
    // A failed load must not read as "No tokens yet.": live tokens may exist.
    let failed = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    // The new token, shown once: (name, token).
    let made = RwSignal::new(None::<(String, String)>);
    let shown = RwSignal::new(false);
    let copied = RwSignal::new(None::<bool>);
    let (msg, need) = (Msg::default(), step_up());
    // The cap counts live tokens; revoked ones stay in the list.
    let live = move || list.with(|l| l.iter().filter(|t| t["revoked_at"].is_null()).count());
    let load = move || {
        if list.is_disposed() {
            return;
        }
        spawn_local(async move {
            let r = api("GET", "/api/mcp-tokens", None).await;
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
    let create = move |_| {
        let n = name.get_untracked();
        run_act(
            async move { api("POST", "/api/mcp-tokens", Some(json!({"name": n}))).await },
            msg,
            need,
            move |v| {
                made.set(Some((s(&v["name"]), s(&v["token"]))));
                shown.set(false);
                copied.set(None);
                name.set(String::new());
                load();
            },
        )
    };
    let revoke = move |id: i64, n: String| {
        ask(Ask::new(format!("Revoke the token \"{n}\"?")).text("The MCP server that uses it can no longer propose changes, and changes waiting for a code are closed.").yes("Revoke").danger(), move |_| run_act(
            async move { api("POST", &format!("/api/mcp-tokens/{id}/revoke"), None).await },
            msg,
            need,
            move |_| {
                msg.ok("Token revoked.");
                load();
            },
        ))
    };
    let rows = move || {
        list.get()
            .into_iter()
            .map(|t| {
                let id = t["id"].as_i64().unwrap_or_default();
                let n = s(&t["name"]);
                let live = t["revoked_at"].is_null();
                let n2 = n.clone();
                view! {
                    <tr class:muted=!live>
                        <td>{n}</td>
                        <td data-label="Made">{when(&t["created_at"])}</td>
                        <td data-label="Last used">{when(&t["last_used_at"])}</td>
                        <td class="tact">{if live {
                            view! { <button class="danger" on:click=move |_| revoke(id, n2.clone())>"Revoke"</button> }.into_any()
                        } else {
                            view! { <span>{format!("Revoked {}", when(&t["revoked_at"]))}</span> }.into_any()
                        }}</td>
                    </tr>
                }
            })
            .collect_view()
    };
    view! {
        <Head guild=String::new() page="Brim".into() big="MCP tokens".into() />
        <div class="card">
            <p class="muted">"One token per place that uses the MCP server. Making or revoking a token needs 2FA."<Tip text="A token is the login of an AI client: it reads through the MCP server and can propose changes. An OAuth login only reads. Nothing changes until you give the AI client the code the bot sends you by DM, and the DM names the token. Revoke a token you no longer use, or at once if you get a DM you did not ask for." /></p>
            <div class="row">
                <input placeholder="Name, for example VPS" aria-label="token name" maxlength="40" prop:value=name on:input=move |e| name.set(event_target_value(&e)) />
                <button class="go" disabled=move || name.get().trim().is_empty() || !room(live(), "mcp_tokens") on:click=create>"Make token"</button>
                <Count n=Signal::derive(live) key="mcp_tokens" />
            </div>
            <Show when=move || made.get().is_some()>
                <p><b>{move || format!("Token \"{}\" (shown once, it is not stored):", made.get().map(|m| m.0).unwrap_or_default())}</b></p>
                <div class="row"><pre class:blur=move || !shown.get()>{move || made.get().map(|m| m.1).unwrap_or_default()}</pre>
                    <button class="danger" on:click=move |_| shown.update(|v| *v = !*v)>{move || if shown.get() { "Hide" } else { "Show" }}</button>
                    <button class="ghost" on:click=move |_| { let t = made.get_untracked().map(|m| m.1).unwrap_or_default(); spawn_local(async move { copied.set(Some(copy_text(&t).await)) }) }>{move || copy_label(copied.get())}</button></div>
                <p class="muted">"Give it to the AI client as its login: in the client's MCP settings, the "<code>"Authorization: Bearer"</code>" header of the MCP server (DEPLOY.md 6.2). Never paste it in a chat."</p>
            </Show>
        </div>
        <div class="card">
            <Show when=move || !list.get().is_empty() fallback=move || if failed.get() {
                view! { <p class="err">"Could not load the tokens. "<button class="ghost" on:click=move |_| load()>"Retry"</button></p> }.into_any()
            } else {
                view! { <p class="muted">"No tokens yet."</p> }.into_any()
            }>
                <table class="data">
                    <thead><tr><th>"Name"</th><th>"Made"</th><th>"Last used"</th><th></th></tr></thead>
                    <tbody>{rows}</tbody>
                </table>
            </Show>
        </div>
        <Toast msg />
    }
}

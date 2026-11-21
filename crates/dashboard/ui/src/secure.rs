//! Bot-owner page "Secure my account" (owner decision 2026-11-20): one press logs everyone out of the
//! dashboard, cancels a queued update, revokes every MCP token and closes the changes that wait for
//! a code. It needs dashboard 2FA plus a Rescue Key and its authenticator code (DEPLOY.md section 9).
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

/// The steps the dashboard cannot do for the owner, from DEPLOY.md section 9.
fn manual_steps() -> impl IntoView {
    view! {
        <ol>
            <li>"Get your Discord account back (Discord's password reset or support) and turn on Discord's own 2FA."</li>
            <li>"If that account owns the bot's Discord application: in the Developer Portal press Bot > Reset Token and OAuth2 > Reset Secret, and put the new values in .env as DISCORD_TOKEN= and DISCORD_CLIENT_SECRET=."</li>
            <li>"Log every OAuth login out: put a new MCP_OAUTH_SECRET in .env (openssl rand -hex 32), then restart with docker compose up -d."</li>
            <li>"Check the Update page, each server's History page and the Raider List for changes you did not make."</li>
        </ol>
    }
}

#[component]
pub fn SecureAccount(me: Signal<Option<Value>>) -> impl IntoView {
    let owner = move || me.get().is_some_and(|m| m["bot_owner"] == true);
    // {has_key, totp}; None while loading or when it failed.
    let state = RwSignal::new(None::<Value>);
    let (key, code) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let done = RwSignal::new(None::<Value>);
    let (msg, need) = (Msg::default(), step_up());
    Effect::new(move |_| {
        if owner() {
            spawn_local(async move {
                let r = api("GET", "/api/secure-account", None).await;
                if state.is_disposed() {
                    return;
                }
                match r {
                    Ok(v) => state.set(Some(v)),
                    Err(e) => msg.err(e),
                }
            })
        }
    });
    let ready = move || state.get().is_some_and(|s| s["has_key"] == true && s["totp"] == true);
    let press = move |_| {
        ask(Ask::new("Secure my account now?").text("Everyone is logged out of the dashboard, you too, a queued update is cancelled, every MCP token is revoked and changes waiting for a code are closed. Your 2FA and the Rescue Key stay.").yes("Secure my account").danger(), move |_| run_resumable(
            // Read again after a 2FA prompt: the rescue code may have changed meanwhile.
            move || {
                let b = json!({"key": key.get_untracked(), "code": code.get_untracked()});
                async move { api("POST", "/api/secure-account", Some(b)).await }
            },
            msg,
            need,
            move |v| {
                key.set(String::new());
                code.set(String::new());
                done.set(Some(v));
            },
        ))
    };
    let n = |v: &Value, k: &str| v[k].as_u64().unwrap_or_default();
    view! {
        <Head guild=String::new() page="Account".into() big="Secure my account".into() pose=Some(Brim::HoldingAKey) />
        {move || if !owner() {
            view! { <div class="card empty"><h2>"Only the bot owner can use this page"</h2></div> }.into_any()
        } else if let Some(v) = done.get() {
            view! {
                <div class="card">
                    <h2>"Your account is secured"</h2>
                    <p>{format!("Logged out {} dashboard sessions, cancelled {} queued updates, revoked {} MCP tokens and closed {} changes waiting for a code.", n(&v, "sessions"), n(&v, "updates"), n(&v, "tokens"), n(&v, "writes"))}</p>
                    {(v["update_running"] == true).then(|| view! { <p class="err">"An update was already running and could not be stopped. When it is done, check the Update page and Install the build you trust."</p> })}
                    <p>{format!("Make a new Rescue Key for server {} when you are safe: the key you typed here may have been seen.", v["guild"].as_str().unwrap_or_default())}</p>
                    <p>"Whoever has your Discord account can still log in again. Do these steps now:"</p>
                    {manual_steps()}
                    <a class="go" href="/auth/login">"Log in again"</a>
                </div>
            }.into_any()
        } else {
            view! {
                <div class="card">
                    <p>"Use this if someone may have your Discord account or dashboard login. One press:"</p>
                    <ul>
                        <li>"logs out every dashboard session (all staff, you too)"</li>
                        <li>"cancels an update that is queued"</li>
                        <li>"revokes every MCP token and closes the changes that wait for a code"</li>
                    </ul>
                    <p class="muted">"Your dashboard 2FA and the Rescue Key stay as they are."<Tip text="It needs your dashboard 2FA and the Rescue Key of a server where you hold the Owner level, made after you became its owner, with the code of that key's own entry in your authenticator (\"Brim Rescue\"). The page finds the server from the key." /></p>
                    {move || match state.get() {
                        None => view! { <Working /> }.into_any(),
                        Some(s) if s["totp"] != true => view! { <p class="err">"You have no dashboard 2FA, so the button is off. Use the SSH steps in DEPLOY.md section 9 (Stolen Discord account) instead."</p> }.into_any(),
                        Some(s) if s["has_key"] != true => view! { <p class="err">"You have no Rescue Key that works here, so the button is off. Make one on a server you own (Auto Setup, the Rescue Key step), or use the SSH steps in DEPLOY.md section 9, which need no key."</p> }.into_any(),
                        Some(_) => view! {
                            <label>"Rescue Key"<input type="password" autocomplete="off" spellcheck="false" prop:value=key on:input=move |e| key.set(event_target_value(&e)) /></label>
                            <label>"Rescue authenticator code"<input inputmode="numeric" autocomplete="one-time-code" maxlength="6" placeholder="123456" prop:value=code on:input=move |e| code.set(event_target_value(&e)) /></label>
                        }.into_any(),
                    }}
                    <button class="danger" disabled=move || !ready() || key.get().trim().is_empty() || code.get().trim().len() != 6 on:click=press>"Secure my account"</button>
                </div>
                <div class="card">
                    <h2>"After the press"</h2>
                    <p class="muted">"The press cannot stop a new login with a stolen Discord account. These steps can:"</p>
                    {manual_steps()}
                </div>
            }.into_any()
        }}
        <Toast msg />
    }
}

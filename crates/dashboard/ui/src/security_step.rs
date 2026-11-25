//! The wizard's one security step: a switch for each protection and one plain sentence about what
//! Brim does. The values come from `profile.json` and are not shown as options; every setting is
//! tuned later on its own page. A protection the owner never saved starts on.
use crate::common::*;
use crate::form::{get_path, set_path};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

/// (config section, name, what Brim does with the recommended values).
pub const ROWS: &[(&str, &str, &str)] = &[
    ("antinuke", "Anti Nuke", "Quarantines staff who ban, delete or add too much too fast, and puts back what they deleted."),
    ("automod", "Auto Mod", "Deletes spam, scam, harmful and invite links, and times out repeat offenders."),
    ("joingate", "Join Gate", "Stops bots added by members without a permit, accounts younger than 5 days, and accounts that look like spammers."),
    ("joinraid", "Join Raid", "Bans flagged accounts when 10 join within 3 hours, and stops raids in voice channels."),
    ("verification", "Verification", "New members solve a captcha within 5 minutes or they are kicked."),
    ("raider_list", "Raider List", "Bans accounts caught raiding other servers. The bot owner checks every entry."),
    ("backups", "Backups", "Saves the server every 3 hours, so Brim can put back deleted channels and roles."),
    ("panic", "Panic mode", "Locks the server when 2 accounts are quarantined within 10 minutes."),
];

/// Panic mode is on while an account count is set; the others have an `enabled` switch.
fn key(sec: &str) -> &'static str {
    if sec == "panic" { "executors" } else { "enabled" }
}

fn is_on(c: &Value, sec: &str) -> bool {
    let v = get_path(c, &format!("{sec}.{}", key(sec)));
    v.as_bool().unwrap_or_else(|| v.as_u64().is_some_and(|n| n > 0))
}

fn unset(c: &Value, sec: &str) -> bool {
    get_path(c, &format!("{sec}.{}", key(sec))).is_null()
}

/// Turns a protection on: its switch, and every recommended value with no saved value.
fn turn_on(c: &mut Value, sec: &str) {
    let prefix = format!("{sec}.");
    for (p, v) in crate::wizard::profile().iter().filter(|(p, _)| p.starts_with(&prefix)) {
        if get_path(c, p).is_null() {
            set_path(c, p, v.clone());
        }
    }
    if sec != "panic" {
        set_path(c, &format!("{sec}.enabled"), json!(true));
    }
}

fn turn_off(c: &mut Value, sec: &str) {
    set_path(c, &format!("{sec}.{}", key(sec)), if sec == "panic" { json!(0) } else { json!(false) });
}

#[component]
pub fn Security(guild: String) -> impl IntoView {
    let (val, orig) = (RwSignal::new(json!({})), RwSignal::new(json!({})));
    let version = RwSignal::new(0i64);
    let (msg, need) = (Msg::default(), step_up());
    let (saving, failed) = (RwSignal::new(false), RwSignal::new(false));
    let g = guild.clone();
    spawn_local(async move {
        match api("GET", &format!("/api/guilds/{g}/config"), None).await {
            Ok(v) if !val.is_disposed() => {
                version.set(v["version"].as_i64().unwrap_or(0));
                let c = if v["config"].is_object() { v["config"].clone() } else { json!({}) };
                orig.set(c.clone());
                // A protection never saved starts on, so the owner only has to save.
                let mut c = c;
                for (sec, _, _) in ROWS {
                    if unset(&c, sec) {
                        turn_on(&mut c, sec);
                    }
                }
                val.set(c);
            }
            Ok(_) => {}
            Err(e) => {
                if !failed.is_disposed() {
                    failed.set(true);
                    msg.err(format!("The settings could not be loaded, so Save is off until you reload the page: {e}"));
                }
            }
        }
    });
    let g = guild.clone();
    let save = move |()| {
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        let (g, cur, before) = (g.clone(), val.get_untracked(), orig.get_untracked());
        run_change(
            async move {
                let r = async {
                    let mut last = json!({"version": version.get_untracked()});
                    for (s, _, _) in ROWS.iter().filter(|(s, _, _)| cur[*s] != before[*s]) {
                        let body = json!({"expected_version": last["version"], "value": cur[*s]});
                        last = api("PUT", &format!("/api/guilds/{g}/config/{s}"), Some(body)).await?;
                        version.try_set(last["version"].as_i64().unwrap_or(0));
                        orig.try_update(|o| o[*s] = cur[*s].clone());
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
            },
        );
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
    let g = guild.clone();
    let discord = move |_| {
        let (g, v) = (g.clone(), version.get_untracked());
        run_resumable(
            move || {
                let g = g.clone();
                async move { api("POST", &format!("/api/guilds/{g}/wizard/discord-settings"), Some(json!({"expected_version": v}))).await }
            },
            msg,
            need,
            move |_| msg.ok("Done"),
        );
    };
    view! {
        <div class="card">
            <h2>"Recommended security"</h2>
            <p class="muted">"Brim sets these up with the settings we recommend. Turn off what you do not want, then save. Every setting can be tuned later on its own page in the menu on the left."</p>
            {ROWS.iter().map(|(sec, name, what)| view! {
                <div class="row note sec-row">
                    <div class="sec-text"><b>{*name}</b><br/><span class="muted">{*what}</span></div>
                    <input type="checkbox" class="sw" aria-label=format!("Use {name}") prop:disabled=move || failed.get() prop:checked=move || is_on(&val.get(), sec)
                        on:change=move |e| { let on = event_target_checked(&e); val.update(|c| if on { turn_on(c, sec) } else { turn_off(c, sec) }) } />
                </div>
            }).collect_view()}
            <div class="row note sec-row">
                <div class="sec-text"><b>"Discord's own server settings"</b><br/><span class="muted">"Sets a medium verification level, the media filter for everyone, and notifications for mentions only. Only where Discord has a weaker one now."</span></div>
                <button class="ghost" disabled=move || val.get() != orig.get() on:click=discord>"Set them"</button>
            </div>
        </div>
        <Fade when=Signal::derive(move || val.get() != orig.get())>
            <div class="bar"><b>"Changes detected!"</b><button class="ghost" on:click=move |_| val.set(orig.get_untracked())>"Cancel"</button><button disabled=move || saving.get() || failed.get() on:click={ let save = save.clone(); move |_| save(()) }>{busy_label(move || saving.get(), "Save")}</button></div>
        </Fade>
        <Toast msg />
    }
}

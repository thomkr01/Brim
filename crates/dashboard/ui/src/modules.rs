//! Module pages (`#/<server>/module/<slug>`): one row per part of a sidebar module, with a short line and an
//! arrow, or a blue "Set up" while the part is still off. The open module's master switch sits in the sidebar
//! ([`ModuleSwitch`], review, question Q2 option b); the module's General page has the same switch.
use crate::{common::*, icons, nav_key, page_icon, NAV, TOP};
use leptos::prelude::*;
use serde_json::{json, Value};

/// The server config the sidebar and the module pages read their on/off state from.
#[derive(Clone, Copy)]
pub struct Cfg(pub RwSignal<Value>);

/// Sidebar "Modules", in order. A NAV group name opens its module page; a page key opens that page.
pub const MODULE_ORDER: &[&str] = &["Auto Mod", "Anti Nuke", "Server Joins", "Moderation", "tickets", "Messages", "logging"];

/// NAV groups that are no module: their pages are general sidebar links.
pub const GENERAL_GROUPS: &[&str] = &["Tools"];

pub fn slug(group: &str) -> String {
    group.to_lowercase().replace(' ', "_")
}

pub fn is_module(group: &str) -> bool {
    MODULE_ORDER.contains(&group)
}

/// The module (NAV group) a page belongs to.
pub fn group_of(p: &str) -> Option<&'static str> {
    let key = nav_key(p);
    NAV.iter().find(|(_, items)| items.iter().any(|(k, _)| *k == key)).map(|(g, _)| *g)
}

/// The config section whose `enabled` switch says whether a part is on. Parts without one always show an arrow.
fn flag(page: &str) -> Option<&'static str> {
    ["automod", "antinuke", "joingate", "joinraid", "verification", "tickets", "catch", "raider_list"].into_iter().find(|k| *k == page)
}

/// Whether a part is on; `None` when it has no switch or the config is not loaded.
fn part_on(cfg: &Value, page: &str) -> Option<bool> {
    cfg["config"][flag(page)?]["enabled"].as_bool().or_else(|| cfg["config"].is_object().then_some(false))
}

/// The config section behind a module's master switch (Auto Mod, Anti Nuke); other modules have none.
fn switch_section(group: &str) -> Option<&'static str> {
    match group {
        "Auto Mod" => Some("automod"),
        "Anti Nuke" => Some("antinuke"),
        _ => None,
    }
}

/// The module's own on/off state, for modules with a master switch (Auto Mod, Anti Nuke).
pub fn module_on(cfg: &Value, group: &str) -> Option<bool> {
    part_on(cfg, switch_section(group)?)
}

/// What turning the module off stops, for the confirm question (same words as the General page's help line).
fn off_effect(section: &str) -> &'static str {
    match section {
        "antinuke" => "Nobody is counted or punished for dangerous actions. Changes only permit holders may make (dangerous permissions, the vanity URL) are still undone.",
        _ => "Auto Mod checks no messages on this server.",
    }
}

/// The open module's on/off switch in the sidebar. It saves the module's section through the same
/// `PUT /config/<section>` as the General page, so access and the 2FA step-up are the server's. Turning
/// off asks first; a section the bot could not read is left to the General page, which shows the problem.
#[component]
pub fn ModuleSwitch(guild: String, group: &'static str, on: bool, msg: Msg, need: RwSignal<bool>, busy: RwSignal<bool>) -> impl IntoView {
    let access = expect_context::<Access>();
    let cfg = expect_context::<Cfg>().0;
    let section = switch_section(group).unwrap_or_default();
    let click = move |e: leptos::ev::MouseEvent| {
        // The switch sits in the module's link: no navigation, and the checkbox follows the saved state only.
        e.prevent_default();
        e.stop_propagation();
        let v = cfg.get_untracked();
        if busy.get_untracked() {
            return;
        }
        if v["unread"].as_array().is_some_and(|u| u.iter().any(|x| x == section)) {
            return msg.err(format!("Brim could not read the {group} settings, so open General to see why."));
        }
        let (g, version, now) = (guild.clone(), v["version"].as_i64().unwrap_or(0), module_on(&v, group).unwrap_or(false));
        let mut value = if v["config"][section].is_object() { v["config"][section].clone() } else { json!({}) };
        value["enabled"] = json!(!now);
        let go = move |_: String| {
            busy.set(true);
            run_act(
                async move {
                    let r = api("PUT", &format!("/api/guilds/{g}/config/{section}"), Some(json!({"expected_version": version, "value": value}))).await;
                    // The config is read again, except while the 2FA prompt is open: rebuilding the nav would
                    // drop the control that is clicked again after the code. A conflict means another tab saved first.
                    if r.as_ref().err().is_none_or(|e| e != "step_up_required") && crate::hash().first().map(String::as_str) == Some(g.as_str()) {
                        if let Ok(c) = api("GET", &format!("/api/guilds/{g}/config"), None).await {
                            cfg.try_set(c);
                        }
                    }
                    busy.try_set(false);
                    r
                },
                msg,
                need,
                move |_| msg.done("Saved!"),
            )
        };
        if now {
            ask(Ask::new(format!("Turn {group} off?")).text(off_effect(section)).yes("Turn off").danger(), go)
        } else {
            go(String::new())
        }
    };
    view! { <input type="checkbox" class="sw" role="switch" aria-label=format!("{group} module") prop:checked=on disabled=move || busy.get() || access.of(section) != "full" on:click=click /> }
}

pub fn blurb(p: &str) -> &'static str {
    match p {
        "automod" => "Heat, strikes, auto timeouts, auto lockdown and auto slowmode.",
        "filters" => "Spam, invite links, malicious and NSFW links, word and link blacklists.",
        "discord_automod" => "Rules for Discord's built-in AutoMod.",
        "whitelist" => "What Auto Mod leaves alone.",
        "automod_panic" => "What happens when many members are punished in a short time.",
        "antinuke" => "Limits for dangerous actions and what happens when one is passed.",
        "antinuke_filters" => "Which dangerous actions are counted, and their limits.",
        "antinuke_panic" => "What happens when many accounts are quarantined in a short time.",
        "joingate" => "Checks new members against your filters when they join.",
        "joinraid" => "Looks for many accounts joining in a short time.",
        "verification" => "New members pass a check before they get access.",
        "cases" => "Every punishment: who, why and when.",
        "mod_stats" => "What each moderator did.",
        "notes" => "Notes about members.",
        "report_list" => "Reports members sent about others.",
        "tickets" => "Members DM Brim, and staff answer in a private ticket channel.",
        "catch" => "A channel that punishes anyone who writes in it, for hacked accounts.",
        "raider_list" => "Accounts caught raiding many servers, checked by the bot owner.",
        "embeds" => "Build your own messages, then post them now, on a schedule or by tag.",
        "bot_messages" => "The messages Brim sends by itself, which you can edit.",
        "feeds" => "New YouTube videos and RSS posts, posted in your channels.",
        "permits" => "Who may do what with Brim.",
        "backups" => "Backups of your server, by hand or on a schedule.",
        "logging" => "Choose where Brim logs what happens.",
        _ => "",
    }
}

#[component]
pub fn ModulePage(guild: String, group: &'static str) -> impl IntoView {
    let access = expect_context::<Access>();
    let cfg = expect_context::<Cfg>().0;
    let items = NAV.iter().find(|(g, _)| *g == group).map_or(&[][..], |(_, i)| *i);
    let g = guild.clone();
    view! {
        <Head guild=guild.clone() page="Modules".to_string() big=group.to_string() />
        {move || module_on(&cfg.get(), group).map(|on| view! { <p class="mod-state"><span class="chip" class:green=on>{if on { "On" } else { "Off" }}</span><span class="muted">"Turn it on or off with the switch in the menu."</span></p> })}
        <div class="stack">
            {items.iter().filter(|(p, _)| access.of(p) != "hidden").map(|(p, t)| {
                let (g, p) = (g.clone(), *p);
                view! { <a class="card link" href=format!("#/{g}/{p}")>
                    <span class="ico">{icons::icon(page_icon(p))}</span>
                    <span><h3>{*t}</h3><span class="muted">{blurb(p)}</span></span>
                    <span class="sp"></span>
                    {move || if part_on(&cfg.get(), p) == Some(false) { view! { <span class="setup">"Set up"</span> }.into_any() } else { icons::icon("chevron_right") }}
                </a> }
            }).collect_view()}
        </div>
    }
}

/// The group a `module` route names.
pub fn group_by_slug(s: &str) -> Option<&'static str> {
    NAV.iter().map(|(g, _)| *g).find(|g| is_module(g) && slug(g) == s)
}

/// Sidebar title of a module entry (a group name or a page key).
pub fn label(entry: &str) -> String {
    TOP.iter().chain(NAV.iter().flat_map(|(_, i)| i.iter())).find(|(k, _)| *k == entry).map_or(entry.to_string(), |(_, t)| t.to_string())
}

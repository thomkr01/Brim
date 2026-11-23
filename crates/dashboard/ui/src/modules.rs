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
pub const MODULE_ORDER: &[&str] = &["Auto Mod", "Anti Nuke", "Server Joins", "Moderation", "Tickets", "Messages", "Logging"];

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
    ["automod", "antinuke", "joingate", "joinraid", "verification", "tickets", "catch", "raider_list", "giveaways"].into_iter().find(|k| *k == page)
}

/// The module key a part belongs to (same as `brim_core::sections::module_of`).
fn module_key(part: &str) -> Option<&'static str> {
    match part {
        "joingate" | "joinraid" | "verification" | "raider_list" => Some("server_joins"),
        "catch" => Some("moderation"),
        "giveaways" => Some("messages"),
        _ => None,
    }
}

/// The off module a part waits on: its own setting may be on, but nothing runs.
fn module_off(cfg: &Value, part: &str) -> Option<&'static str> {
    module_key(part).filter(|m| cfg["config"]["modules"][*m].as_bool() == Some(false))
}

/// Whether a part runs; `None` when it has no switch or the config is not loaded. A part of an off
/// module shows off, so its switch does not ask to stop what already does not run.
pub fn part_on(cfg: &Value, page: &str) -> Option<bool> {
    if module_off(cfg, page).is_some() && cfg["config"].is_object() {
        return Some(false);
    }
    cfg["config"][flag(page)?]["enabled"].as_bool().or_else(|| cfg["config"].is_object().then_some(false))
}

/// The config section and key behind a module's master switch. Auto Mod, Anti Nuke and Tickets use
/// their own `enabled`; the others a key of the `modules` section.
pub fn switch_of(group: &str) -> Option<(&'static str, &'static str)> {
    match group {
        "Auto Mod" => Some(("automod", "enabled")),
        "Anti Nuke" => Some(("antinuke", "enabled")),
        "tickets" | "Tickets" => Some(("tickets", "enabled")),
        "Server Joins" => Some(("modules", "server_joins")),
        "Moderation" => Some(("modules", "moderation")),
        "Messages" => Some(("modules", "messages")),
        "logging" | "Logging" => Some(("modules", "logging")),
        _ => None,
    }
}

/// Whether a switch is on; `None` while the config is not loaded. A missing module key is on (the
/// server's default), a missing `enabled` is off.
pub fn switch_on(cfg: &Value, section: &str, key: &str) -> Option<bool> {
    if section != "modules" {
        return part_on(cfg, section);
    }
    cfg["config"]["modules"][key].as_bool().or_else(|| cfg["config"].is_object().then_some(true))
}

/// The module's own on/off state, for modules with a master switch.
pub fn module_on(cfg: &Value, group: &str) -> Option<bool> {
    let (section, key) = switch_of(group)?;
    switch_on(cfg, section, key)
}

/// What turning the module off stops, for the confirm question (same words as the General page's help line).
fn off_effect(section: &str) -> &'static str {
    match section {
        "server_joins" => "New members are not checked: Join Gate, Join Raid, Verification and the Raider List stop. Their settings are kept.",
        "moderation" => "Staff lose the moderation commands: ban, kick, timeout, warn, purge, slowmode, cases, notes and the others. Members cannot report messages and the catch channel stops. Lock, unlock, unquarantine, unban, untimeout, unwarn and closecase still work, and Anti Nuke and Auto Mod still punish and record cases. Their settings are kept.",
        "messages" => "Triggers, sticky messages, schedules, tags and automatic feed posts stop. Their settings are kept.",
        "logging" => "Event logs (messages, members, channels, roles and more) stop. Security and moderation logs still post.",
        "antinuke" => "Nobody is counted or punished for dangerous actions. Changes only permit holders may make (dangerous permissions, the vanity URL) are still undone.",
        "automod" => "Auto Mod checks no messages on this server.",
        "tickets" => "Members can no longer open new tickets.",
        "joingate" => "New members are no longer checked against your join filters.",
        "joinraid" => "Brim no longer looks for many accounts joining in a short time.",
        "verification" => "New members no longer pass a check before they get access.",
        "catch" => "Brim no longer punishes anyone who writes in the catch channel.",
        "raider_list" => "New members are no longer checked against the Raider List.",
        _ => "",
    }
}

/// The on/off switch of a module (sidebar) or a part (module page). It saves the section through the same
/// `PUT /config/<section>` as the section's own page, so access and the 2FA step-up are the server's. Turning
/// off asks first; a section the bot could not read is left to its own page, which shows the problem.
#[component]
pub fn ModuleSwitch(guild: String, section: &'static str, #[prop(default = "enabled")] key: &'static str, group: String, on: bool, msg: Msg, need: RwSignal<bool>, busy: RwSignal<bool>) -> impl IntoView {
    let access = expect_context::<Access>();
    let cfg = expect_context::<Cfg>().0;
    let aria = format!("{group} on or off");
    // `modules` is no page: its bar is a Trusted Admin (the server checks it, and the 2FA step-up).
    // A part of an off module turns the module on too, so it needs that bar as well.
    let may = move || match section {
        "modules" => access.rank() >= 3,
        _ if module_off(&cfg.get(), section).is_some() => access.rank() >= 3 && access.of(section) == "full",
        _ => access.of(section) == "full",
    };
    let click = move |e: leptos::ev::MouseEvent| {
        // The switch sits in the module's link: no navigation, and the checkbox follows the saved state only.
        e.prevent_default();
        e.stop_propagation();
        let v = cfg.get_untracked();
        if busy.get_untracked() {
            return;
        }
        if v["unread"].as_array().is_some_and(|u| u.iter().any(|x| x == section)) {
            return msg.err(format!("Brim could not read the {group} settings, so open its page to see why."));
        }
        let (g, group, version, now) = (guild.clone(), group.clone(), v["version"].as_i64().unwrap_or(0), switch_on(&v, section, key).unwrap_or(false));
        // A part that is kept on in an off module: turning it on means turning its module on.
        let (section, key) = match module_off(&v, section) {
            Some(m) if v["config"][section]["enabled"] == true => ("modules", m),
            _ => (section, key),
        };
        let mut value = if v["config"][section].is_object() { v["config"][section].clone() } else { json!({}) };
        value[key] = json!(!now);
        let go = move |_: String| {
            busy.set(true);
            run_act(
                async move {
                    let mut r = api("PUT", &format!("/api/guilds/{g}/config/{section}"), Some(json!({"expected_version": version, "value": value}))).await;
                    // The config is read again, except while the 2FA prompt is open: rebuilding the nav would
                    // drop the control that is clicked again after the code. A conflict means another tab saved first.
                    if r.as_ref().err().is_none_or(|e| e != "step_up_required") && crate::hash().first().map(String::as_str) == Some(g.as_str()) {
                        match api("GET", &format!("/api/guilds/{g}/config"), None).await {
                            Ok(c) => _ = cfg.try_set(c),
                            // The switch still shows the old state: say so, not "Saved!".
                            Err(_) if r.is_ok() => r = Err("Saved, but the page could not load the new state. Reload the page.".into()),
                            Err(_) => {}
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
            ask(Ask::new(format!("Turn {group} off?")).text(off_effect(if section == "modules" { key } else { section })).yes("Turn off").danger(), go)
        } else {
            go(String::new())
        }
    };
    view! { <input type="checkbox" class="sw" role="switch" aria-label=aria prop:checked=on disabled=move || busy.get() || !may() on:click=click /> }
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
        "giveaways" => "Members press Enter to join; Brim draws the winners at the end time.",
        "permits" => "Who may do what with Brim.",
        "backups" => "Backups of your server, by hand or on a schedule.",
        "logging/brim" => "The channel for Brim's logs and mod logs, and what to ignore.",
        "logging/discord" => "Where messages, members, channels, roles and other Discord events are logged.",
        "tickets/publish" => "Post the ticket panels members press to open a ticket.",
        "tickets/cases" => "Open and closed tickets.",
        "tickets/stats" => "How many tickets, and how fast staff answer.",
        "logging" => "Choose where Brim logs what happens.",
        _ => "",
    }
}

/// The module's On/Off line under its title (nothing while the config loads).
#[component]
pub fn ModState(group: String) -> impl IntoView {
    let cfg = expect_context::<Cfg>().0;
    move || module_on(&cfg.get(), &group).map(|on| view! { <p class="mod-state"><span class="chip" class:green=on>{if on { "On" } else { "Off" }}</span><span class="muted">{if on { "Turn it on or off with the switch in the menu." } else { "Nothing below runs until you turn it on with the switch in the menu. The settings are kept." }}</span></p> })
}

#[component]
pub fn ModulePage(guild: String, group: &'static str) -> impl IntoView {
    let access = expect_context::<Access>();
    let cfg = expect_context::<Cfg>().0;
    let items = NAV.iter().find(|(g, _)| *g == group).map_or(&[][..], |(_, i)| *i);
    let g = guild.clone();
    let (msg, need, busy) = (Msg::default(), step_up(), RwSignal::new(false));
    view! {
        <Head guild=guild.clone() page="Modules".to_string() big=group.to_string() />
        <Toast msg />
        <ModState group=group.to_string() />
        <div class="stack">
            {items.iter().filter(|(p, _)| access.of(p) != "hidden").map(|(p, t)| {
                let (g, p, t) = (g.clone(), *p, *t);
                let g2 = g.clone();
                // A part with its own on/off shows its switch; the others are pages without one.
                // A part that is a module itself (Moderation's Tickets) opens that module's page.
                let href = if is_module(t) { format!("#/{g}/module/{}", slug(t)) } else { format!("#/{g}/{p}") };
                view! { <a class="card link" href=href>
                    <span class="ico">{icons::icon(page_icon(p))}</span>
                    <span><h3>{t}</h3><span class="muted">{blurb(p)}</span></span>
                    <span class="sp"></span>
                    {move || part_on(&cfg.get(), p).filter(|_| group != "Tickets").map(|on| view! { <ModuleSwitch guild=g2.clone() section=p group=t.to_string() on msg need busy /> })}
                    {icons::icon("chevron_right")}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_of_off_module_shows_off() {
        let cfg = json!({"config": {"modules": {"server_joins": false}, "joingate": {"enabled": true}, "automod": {"enabled": true}}});
        assert_eq!(part_on(&cfg, "joingate"), Some(false));
        assert_eq!(module_off(&cfg, "joingate"), Some("server_joins"));
        assert_eq!(part_on(&cfg, "automod"), Some(true));
        let on = json!({"config": {"joingate": {"enabled": true}}});
        assert_eq!(part_on(&on, "joingate"), Some(true));
        assert_eq!(part_on(&json!(null), "joingate"), None);
    }
}

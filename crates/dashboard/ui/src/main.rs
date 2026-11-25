use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

mod appeals;
mod botmsgs;
mod feeds;
mod channels;
mod commands;
mod common;
mod datareq;
mod discord;
mod histdiff;
mod embeds;
mod form;
mod giveaway_plans;
mod giveaways;
mod guard;
mod help;
mod home;
mod icons;
mod layout;
mod landing;
mod lockdown;
mod mcp;
mod modules;
mod msgrules;
mod msgtrig;
mod msgsticky;
mod msgtags;
mod md;
mod msgcats;
mod msgparts;
mod pages;
mod permits;
mod profile;
mod raiders;
mod records;
mod roles;
mod recovery;
mod search;
mod secure;
mod security_step;
mod styleguide;
mod tickets;
mod update;
mod wizard;
use appeals::{AppealForm, AppealList, Blacklist};
use common::*;
use embeds::Embeds;
use form::Section;
use lockdown::{DiscordRules, LockdownManage, LockdownSettings};
use permits::Permits;
use records::{CaseList, CasePage, JoinGateLog, ModStats, NoteList, RaidLog, ReportList, TicketLog};
/// The server list's empty state; its text is the marker that shows the help card.
const NO_SERVERS: &str = "No servers";

use channels::ChannelSetup;
use commands::Commands;
use datareq::DataRequests;
use mcp::McpTokens;
use pages::{
    Catch, Logging, LostControl, NukeCoverage, PurgeData, Rescue, RescuePrompt, Revert, Score,
    Tickets, Wizard,
};
use raiders::{RaiderAppeal, RaiderMatches, RaiderReview};
use recovery::{Backups, Panic};
use search::{Search, SearchBox};
use update::Update;

/// Sidebar with only with the pages Brim has.
const TOP: &[(&str, &str)] = &[
    ("wizard", "Wizard"),
    ("overview", "Overview"),
    ("misc", "Miscellaneous"),
    ("permits", "Permits"),
    ("backups", "Backups"),
    ("channel_setup", "Channel Setup"),
    ("lockdown", "Lockdown"),
    ("appeal_list", "Appeals"),
];
const NAV: &[(&str, &[(&str, &str)])] = &[
    (
        "Auto Mod",
        &[
            ("automod", "General"),
            ("filters", "Filters"),
            ("discord_automod", "Discord"),
            ("whitelist", "Whitelist"),
            ("automod_panic", "Panic Mode"),
        ],
    ),
    (
        "Anti Nuke",
        &[
            ("antinuke", "General"),
            ("antinuke_filters", "Filters"),
            ("antinuke_panic", "Panic Mode"),
        ],
    ),
    (
        "Server Joins",
        &[
            ("joingate", "Join Gate"),
            ("joinraid", "Join Raid"),
            ("raider_list", "Raider List"),
            ("verification", "Verification"),
        ],
    ),
    (
        "Tickets",
        &[("tickets", "Settings"), ("tickets/publish", "Panels"), ("tickets/cases", "Cases"), ("tickets/stats", "Stats")],
    ),
    (
        "Logging",
        &[("logging/brim", "Brim logs"), ("logging/discord", "Discord logs")],
    ),
    (
        "Moderation",
        &[
            ("cases", "Cases"),
            ("mod_stats", "Mod Stats"),
            ("notes", "Notes"),
            ("report_list", "Reports"),
            ("tickets", "Tickets"),
            ("catch", "Golden Channel"),
        ],
    ),
    (
        "Messages",
        &[("embeds", "Custom messages"), ("bot_messages", "Default messages"), ("feeds", "Feeds"), ("giveaways", "Giveaways")],
    ),
    (
        "Tools",
        &[
            ("revert", "Revert Center"),
            ("history", "History"),
            ("bot_profile", "Bot Profile"),
            ("rescue", "Rescue Key"),
        ],
    ),
];
/// Icon per sidebar page key, group name or Filters card, from the emoji set.
fn page_icon(p: &str) -> &'static str {
    match p {
        "wizard" => "setup_wizard",
        "overview" => "info",
        "misc" => "sparkle",
        "permits" => "fingerprint",
        "logging" | "Logging" | "logging/brim" | "logging/discord" => "logs",
        "tickets/publish" => "announcement",
        "tickets/cases" => "case",
        "tickets/stats" => "analytics",
        "backups" => "tray",
        "channel_setup" => "channel_text",
        "lockdown" => "lock",
        "appeal_list" => "page",
        "Auto Mod" => "ban",
        "automod" | "antinuke" => "globe",
        "filters" | "antinuke_filters" => "sliders",
        "discord_automod" => "discord",
        "whitelist" => "tag",
        "automod_panic" | "antinuke_panic" => "shield",
        "Anti Nuke" => "hand",
        "Server Joins" => "member_join",
        "joingate" => "glasses",
        "joinraid" => "rocket",
        "verification" => "qr_code",
        "Moderation" => "moderator",
        "cases" => "case",
        "mod_stats" => "analytics",
        "notes" => "note",
        "report_list" => "report",
        "tickets" | "Tickets" => "ticket",
        "catch" => "target",
        "raider_list" => "suspicious_account",
        "Tools" => "settings",
        "Messages" => "message",
        "revert" => "restore",
        "history" => "history",
        "embeds" => "palette",
        "bot_messages" => "message",
        "feeds" => "notifications_feed",
        "giveaways" => "trophy",
        "rescue" => "rescue_key",
        "bot_profile" => "bot",
        "spam_text" => "chat_shield",
        "invites" => "announcement",
        "malicious" => "shield",
        "nsfw" => "age_18",
        "words" => "word_censor",
        "links" => "link",
        _ => "",
    }
}
const LOCK_TABS: &[(&str, &str)] = &[("lockdown", "Manage"), ("lockdown_settings", "Settings")];
const APPEAL_TABS: &[(&str, &str)] = &[
    ("appeal_list", "List"),
    ("appeals", "Settings"),
    ("appeal_blacklist", "Blacklist"),
];
const RAID_TABS: &[(&str, &str)] = &[("joinraid", "Settings"), ("raid_log", "Raid Log")];
const RAIDER_TABS: &[(&str, &str)] = &[("raider_list", "Settings"), ("raider_matches", "Matches")];
const GATE_TABS: &[(&str, &str)] = &[("joingate", "Settings"), ("joingate_log", "Join Gate Log")];
const REPORT_TABS: &[(&str, &str)] = &[("report_list", "List"), ("report_blacklist", "Blacklist")];
const WL_TABS: &[(&str, &str)] = &[
    ("whitelist", "Spam"),
    ("whitelist/invites", "Invite Links"),
    ("whitelist/mentions", "Mentions"),
    ("whitelist/everyone", "@everyone"),
];
const SPAM_TABS: &[(&str, &str)] = &[
    ("filters/spam_text", "Text messages"),
    ("filters/spam_mentions", "Mentions"),
    ("filters/spam_attach", "Attachments"),
];
/// (sub page, title, description) for the Filters index.
const FILTER_PAGES: &[(&str, &str, &str)] = &[
    (
        "spam_text",
        "Anti Spam",
        "Target message, mention, attachment spam and a lot more.",
    ),
    (
        "invites",
        "Invite Links",
        "Target Discord Server invites sent by advertisers.",
    ),
    (
        "malicious",
        "Malicious Links",
        "Target phishing/malicious/scam websites sent in your server.",
    ),
    (
        "nsfw",
        "NSFW Links",
        "Target NSFW websites posted in SFW channels.",
    ),
    (
        "words",
        "Word Blacklist",
        "Target premade profanity words or add your own.",
    ),
    (
        "links",
        "Link Blacklist",
        "Target websites found in messages that you manage.",
    ),
];

/// (small label, big title) page header; no title means the server name.
fn title(p: &str) -> (String, Option<String>) {
    let two = |a: &str, b: &str| (a.to_string(), Some(b.to_string()));
    match p {
        "permits" => two("Permits", "Customize your permissions."),
        "logging" => ("Custom Logging".into(), None),
        _ if p.starts_with("lockdown") => two("Lockdown", "Manage your lockdown modes"),
        _ if p.starts_with("appeal") => ("Appeals".into(), None),
        _ if p.starts_with("report") => ("Reports".into(), None),
        "wizard" => ("Wizard".into(), Some(String::new())),
        "discord_automod" => ("Discord built-in AutoMod".into(), None),
        "automod" => two("Automod", "General Settings"),
        "filters" => two("Automod", "Filters"),
        "whitelist" => two("Automod", "Whitelist"),
        "antinuke" => two("Anti Nuke", "General Settings"),
        "antinuke_filters" => two("Anti Nuke", "Filters"),
        "joingate" => two("Join gate", "General Settings"),
        "joingate_log" => ("Join gate".into(), None),
        "joinraid" | "raid_log" => ("Join Raid".into(), None),
        "raider_list" | "raider_matches" => ("Raider List".into(), None),
        "verification" => two("Verification", "General Settings"),
        "automod_panic" => ("Auto Mod Panic Mode".into(), None),
        "antinuke_panic" => ("Anti Nuke Panic Mode".into(), None),
        _ => (
            NAV.iter()
                .flat_map(|(_, i)| i.iter())
                .chain(TOP)
                .find(|(k, _)| *k == p)
                .map_or_else(|| p.replace('_', " "), |(_, t)| t.to_string()),
            None,
        ),
    }
}

/// Brim beside a page's title: on guard on the security pages, looking closer on the logs and the
/// dashboard log, holding the key on Rescue Key (design system "Brim poses").
fn page_pose(p: &str) -> Option<Brim> {
    match p {
        "antinuke" | "antinuke_filters" | "antinuke_panic" | "automod_panic" => Some(Brim::OnGuard),
        "history" | "logging" | "raid_log" | "joingate_log" => Some(Brim::LookingCloser),
        "rescue" => Some(Brim::HoldingAKey),
        _ => None,
    }
}

/// The sidebar page a page belongs to: a tab page (Raid Log, Join Gate Log ...) counts as its first tab.
fn nav_key(p: &str) -> &str {
    match p {
        "raid_log" => "joinraid",
        "joingate_log" => "joingate",
        "raider_matches" => "raider_list",
        "lockdown_settings" => "lockdown",
        _ if p.starts_with("appeal") => "appeal_list",
        _ if p.starts_with("report") => "report_list",
        _ => p,
    }
}

/// The card of a hidden page, or of a server whose access answer failed (with a retry).
fn no_access(access: Access, failed: bool, text: &'static str) -> AnyView {
    if failed {
        return view! { <div class="card empty"><Pose pose=Brim::Stop class="pose-top" /><h2>"Could not load your access"</h2><p>"Brim could not load what you may see on this server. Check your connection and try again."</p><button on:click=move |_| access.tick.update(|n| *n += 1)>"Try again"</button></div> }.into_any();
    }
    view! { <div class="card empty"><Pose pose=Brim::Stop class="pose-top" /><h2>"No access"</h2><p>{text}</p></div> }.into_any()
}

/// "Module › Page" for a page in a sidebar group: ((group name, link to the group's first page this viewer
/// may open), page name).
fn module_of(guild: &str, p: &str, part: Option<&str>, visible: impl Fn(&str) -> bool) -> Option<((String, String), String)> {
    let key = nav_key(p);
    // A hub's part (Tickets > Panels) is a key like "tickets/publish".
    let own = part.map(|s| format!("{key}/{s}"));
    NAV.iter().find_map(|(grp, items)| {
        let (_, name) = items.iter().find(|(k, _)| Some(*k) == own.as_deref()).or_else(|| items.iter().find(|(k, _)| *k == key))?;
        let first = items.iter().find(|(k, _)| visible(k)).map_or(key, |(k, _)| *k);
        let href = if modules::is_module(grp) { format!("#/{guild}/module/{}", modules::slug(grp)) } else { format!("#/{guild}/{first}") };
        Some(((grp.to_string(), href), name.to_string()))
    })
}

#[component]
fn Tabs(
    guild: String,
    items: &'static [(&'static str, &'static str)],
    #[prop(into)] active: Signal<String>,
    #[prop(into)] read: Signal<bool>,
) -> impl IntoView {
    let access = expect_context::<Access>();
    // A memo: the access is fetched again on every page change, and an unchanged answer must not build the row again.
    let shown = Memo::new(move |_| items.iter().filter(|(p, _)| access.of(p.split('/').next().unwrap_or(p)) != "hidden").copied().collect::<Vec<_>>());
    view! { {move || {
        let tabs = shown.get().into_iter().map(|(p, t)| Tab::link(p, t, format!("#/{guild}/{p}"))).collect();
        view! { <PageTabs tabs active dim=read /> }
    }} }
}

#[component]
fn Bell(route: ReadSignal<Vec<String>>) -> impl IntoView {
    let rows = RwSignal::new(Vec::<Value>::new());
    let appeals = RwSignal::new(Vec::<Value>::new());
    let fetch = move || {
        spawn_local(async move {
            if let Ok(v) = api("GET", "/api/appeal/notifications", None).await {
                appeals.set(v.as_array().cloned().unwrap_or_default())
            }
        })
    };
    fetch();
    let guild = move || {
        route
            .get_untracked()
            .first()
            .cloned()
            .filter(|g| g.bytes().all(|b| b.is_ascii_digit()))
    };
    let load = move |_| {
        // Opening the bell shows the verdicts once; the next fetch no longer lists them.
        if !appeals.get_untracked().is_empty() {
            spawn_local(async move {
                let _ = api("POST", "/api/appeal/notifications", None).await;
            });
        }
        let Some(g) = guild() else { return };
        spawn_local(async move {
            if let Ok(v) = api("GET", &format!("/api/guilds/{g}/history"), None).await {
                rows.set(v.as_array().cloned().unwrap_or_default())
            }
        });
    };
    view! { <details class="ico" on:toggle=load><summary aria-label="notifications">{icons::icon("bell")}{move || (!appeals.get().is_empty()).then(|| format!(" {}", appeals.get().len()))}</summary><div>
        {move || appeals.get().into_iter().map(|a| view! { <a href=format!("#/appeal/{}", a["guild"].as_str().unwrap_or(""))>{format!("Your {} appeal #{} was {}", a["kind"].as_str().unwrap_or(""), a["id"], if a["status"] == "accepted" { "approved" } else { "denied" })}</a> }).collect_view()}
        {move || if rows.get().is_empty() { view! { <p class="muted">"No recent changes"</p> }.into_any() } else {
            let history = format!("#/{}/history", guild().unwrap_or_default());
            rows.get().into_iter().take(5).map(|r| view! { <a href=history.clone()>{format!("v{} · {}", r["version"], r["at"].as_str().unwrap_or("").get(..16).unwrap_or(""))}</a> }).collect_view().into_any() }}
    </div></details> }
}

pub(crate) fn hash() -> Vec<String> {
    let h = web_sys::window()
        .unwrap()
        .location()
        .hash()
        .unwrap_or_default();
    h.trim_start_matches('#')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// The open "Diff vs current" box: the version, the current version and its lines (None while they load).
type DiffBox = Option<(i64, i64, Option<Result<Vec<histdiff::Line>, String>>)>;

/// A history time ("2026-11-20T11:42:05.39Z") in the viewer's local time; the text itself if it is no date.
fn local_time(at: &str) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(at));
    if d.get_time().is_nan() {
        return at.to_string();
    }
    d.to_locale_string("en-GB", &wasm_bindgen::JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_else(|| at.to_string())
}

#[component]
fn History(guild: String) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    // The "Diff vs current" box: open, and its text (None while it loads).
    // `Some` while the box is open; the memo keeps the last one for the fade-out.
    let diff = RwSignal::new(None::<(i64, i64, Option<Result<Vec<histdiff::Line>, String>>)>);
    // The schema (labels) and the channel and role names, fetched once on the first diff that needs them.
    let (schema, dc) = (StoredValue::new(None::<Value>), StoredValue::new(None::<Value>));
    let diff_shown = Memo::new(move |prev: Option<&DiffBox>| diff.get().or(prev.cloned().flatten()));
    // Counts the opens, so a slow answer to an earlier click cannot fill a later one.
    let diff_seq = StoredValue::new(0u32);
    let names = RwSignal::new(std::collections::HashMap::<String, String>::new());
    let version = RwSignal::new(0i64);
    let busy = RwSignal::new(false);
    let access = expect_context::<Access>();
    let (msg, need) = (Msg::default(), step_up());
    let load = {
        let g = guild.clone();
        move || {
            let g = g.clone();
            spawn_local(async move {
                match api("GET", &format!("/api/guilds/{g}/history"), None).await {
                    Ok(v) => {
                        version.set(v[0]["version"].as_i64().unwrap_or(0));
                        let ids = v.as_array().into_iter().flatten().map(|r| r["changed_by"].as_str().unwrap_or_default().to_string()).collect();
                        records::learn_names(g.clone(), ids, names);
                        set_rows.set(v.as_array().cloned().unwrap_or_default());
                    }
                    Err(e) if !version.is_disposed() => msg.err(format!("The history could not be loaded: {e}")),
                    Err(_) => {}
                }
            })
        }
    };
    load();
    // Import is previewed first: the server answers with the diff and the ids it will clear.
    let preview = RwSignal::new(None::<(Value, Value)>);
    let shown = Memo::new(move |prev: Option<&Option<(Value, Value)>>| {
        preview.get().or(prev.cloned().flatten())
    });
    let export = {
        let g = guild.clone();
        move |_| {
            let g = g.clone();
            spawn_local(async move {
                match api("GET", &format!("/api/guilds/{g}/config"), None).await {
                    Ok(v) => download(
                        &format!("brim-settings-{g}.json"),
                        &format!(
                            "data:application/json;charset=utf-8,{}",
                            js_sys::encode_uri_component(
                                &serde_json::to_string_pretty(
                                    &json!({"version": v["version"], "config": v["config"]})
                                )
                                .unwrap_or_default()
                            )
                        ),
                    ),
                    Err(e) => msg.err(e),
                }
            })
        }
    };
    let send = {
        let g = guild.clone();
        move |cfg: Value, apply: bool| {
            let g = g.clone();
            async move {
                api("POST", &format!("/api/guilds/{g}/import"), Some(json!({"expected_version": version.get_untracked(), "config": cfg, "apply": apply}))).await
            }
        }
    };
    let import = {
        let send = send.clone();
        move |e: leptos::ev::Event| {
            let input = event_target::<web_sys::HtmlInputElement>(&e);
            let Some(file) = input.files().and_then(|l| l.get(0)) else {
                return;
            };
            input.set_value("");
            let send = send.clone();
            spawn_local(async move {
                let text = wasm_bindgen_futures::JsFuture::from(file.text())
                    .await
                    .ok()
                    .and_then(|t| t.as_string())
                    .unwrap_or_default();
                if version.is_disposed() {
                    return;
                }
                let Ok(f) = serde_json::from_str::<Value>(&text) else {
                    return msg.err("This file is not valid JSON");
                };
                let cfg = f["config"].clone();
                // Started after the file read: no click to repeat after a 2FA code.
                let c = cfg.clone();
                run_resumable(move || send(c.clone(), false), msg, need, move |p| {
                    preview.set(Some((cfg, p)))
                });
            });
        }
    };
    let apply = {
        let load = load.clone();
        move |_| {
            let Some((cfg, _)) = preview.get_untracked() else {
                return;
            };
            preview.set(None);
            let load = load.clone();
            // The Apply button is gone with the preview, so a 2FA code resumes this import itself.
            let send = send.clone();
            run_change_resumable(move || send(cfg.clone(), true), msg, need, move |v| {
                load();
                let cleared = csv(&v["cleared"]);
                msg.done(if cleared.is_empty() {
                    "Imported!".to_string()
                } else {
                    format!("Imported! Set these again, their ids were cleared: {cleared}")
                });
            });
        }
    };
    let file = NodeRef::<leptos::html::Input>::new();
    let reads = RwSignal::new(Vec::<Value>::new());
    {
        let g = guild.clone();
        spawn_local(async move {
            if let Ok(v) = api("GET", &format!("/api/guilds/{g}/mcp-reads"), None).await {
                if !reads.is_disposed() {
                    reads.set(v.as_array().cloned().unwrap_or_default());
                }
            }
        });
    }
    let (g1, g2, layout_guild) = (guild.clone(), guild.clone(), guild);
    view! {
        <div class="row">
            <button class="ghost" disabled=move || access.rank() < 2 title=move || if access.rank() < 2 { "Viewers cannot export the settings" } else { "" } on:click=export>"Export settings"</button>
            <button class="ghost" disabled=move || access.rank() < 2 on:click=move |_| { if let Some(i) = file.get() { i.click() } }>"Import settings"</button>
            <input type="file" accept=".json,application/json" hidden=true node_ref=file on:change=import />
        </div>
        <Fade when=Signal::derive(move || preview.get().is_some())>{let apply = apply.clone(); move || shown.get().map(|(_, p)| {
            let apply = apply.clone();
            let r = NodeRef::new();
            focus_on_open(r);
            let changes = p["diff"].as_array().cloned().unwrap_or_default();
            let cleared = csv(&p["cleared"]);
            let lines = changes.iter().map(|c| format!("{}: {}", c["path"].as_str().unwrap_or(""), if c["new"].is_null() { "removed".into() } else { c["new"].to_string() })).collect::<Vec<_>>().join("\n");
            view! { <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Import settings" on:keydown=move |e| dialog_keys(&e, || preview.set(None))><div class="card">
                <b>"Import settings"</b>
                {if changes.is_empty() { view! { <p class="muted">"This file changes nothing on this server."</p> }.into_any() } else { view! { <p>{format!("{} changes:", changes.len())}</p><pre>{lines}</pre> }.into_any() }}
                {(!cleared.is_empty()).then(|| view! { <p class="info">{format!("Ids from the other server are cleared; set these again after the import: {cleared}")}</p> })}
                <div class="row"><button class="ghost" on:click=move |_| preview.set(None)>"Abort"</button><button disabled=changes.is_empty() on:click=apply>"Import"</button></div>
            </div></div> }
        })}</Fade>
        <For each=move || rows.get() key=|r| r["version"].as_i64() let:r>
            {
                let v = r["version"].as_i64().unwrap_or(0);
                let by = r["changed_by"].as_str().unwrap_or("?").to_string();
                let (g1, g2, load) = (g1.clone(), g2.clone(), load.clone());
                view! {
                    <div class="card row">
                        <b>{format!("v{v}")}</b>
                        <span class="muted">"by "{records::who(names, by.clone())}{move || names.with(|n| n.contains_key(&by)).then(|| format!(" ({by})"))}{format!(" at {}", local_time(r["at"].as_str().unwrap_or("")))}</span>
                        <button class="ghost" on:click=move |_| { let g = g1.clone(); let cur = version.get_untracked(); diff.set(Some((v, cur, None))); diff_seq.update_value(|n| *n += 1); let id = diff_seq.get_value(); spawn_local(async move {
                            let r = match api("GET", &format!("/api/guilds/{g}/history/{v}/{cur}"), None).await {
                                Ok(d) => {
                                    if schema.try_get_value().flatten().is_none() {
                                        if let Ok(s) = api("GET", &format!("/api/guilds/{g}/schema"), None).await { schema.try_set_value(Some(s)); }
                                    }
                                    let sc = schema.try_get_value().flatten().unwrap_or_default();
                                    if dc.try_get_value().flatten().is_none() && histdiff::needs_names(&d, &sc) {
                                        if let Ok(x) = api("GET", &format!("/api/guilds/{g}/discord"), None).await { dc.try_set_value(Some(x)); }
                                    }
                                    Ok(histdiff::lines(&d, &sc, &dc.try_get_value().flatten().unwrap_or_default()))
                                }
                                Err(e) => Err(format!("The changes could not be loaded: {e}")),
                            };
                            if !diff.is_disposed() && diff_seq.get_value() == id && diff.get_untracked().is_some() { diff.set(Some((v, cur, Some(r)))) }
                        }) }>"Diff vs current"</button>
                        <button disabled=move || access.rank() < 3 || busy.get() title=move || if access.rank() < 3 { "Only Trusted Admins and owners can roll back" } else { "" } on:click=move |_| { let (g, load) = (g2.clone(), load.clone()); run_save(guard(busy, async move {
                            let r = api("POST", &format!("/api/guilds/{g}/history/{v}/rollback"), Some(json!({"expected_version": version.get_untracked()}))).await;
                            load(); r }), version, msg, need) }>{busy_label(move || busy.get(), "Roll back")}</button>
                    </div>
                }
            }
        </For>
        <Toast msg />
        <Fade when=Signal::derive(move || diff.get().is_some())>{move || diff_shown.get().map(|(v, cur, body)| {
            let r = NodeRef::new();
            focus_on_open(r);
            view! { <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Diff vs current" on:keydown=move |e| dialog_keys(&e, || diff.set(None))><div class="card">
                <b>{format!("Diff of v{v} vs current")}</b>
                {match body {
                    None => view! { <p><Working /></p> }.into_any(),
                    Some(Ok(l)) if l.is_empty() => view! { <p class="muted">"No differences."</p> }.into_any(),
                    Some(Ok(l)) => {
                        let (was, now) = (format!("v{v}"), format!("Current (v{cur})"));
                        view! { <table class="data diffs"><thead><tr><th>"Setting"</th><th>{was.clone()}</th><th>{now.clone()}</th></tr></thead><tbody>
                            {l.into_iter().map(|l| view! { <tr><td><b>{l.what}</b></td><td class="err" data-label=was.clone()>{l.old}</td><td class="ok" data-label=now.clone()>{l.new}</td></tr> }).collect_view()}
                        </tbody></table> }.into_any()
                    }
                    Some(Err(e)) => view! { <p class="err">{e}</p> }.into_any(),
                }}
                <div class="row"><button class="ghost" on:click=move |_| diff.set(None)>"Close"</button></div>
            </div></div> }
        })}</Fade>
        <layout::LayoutVersions guild=layout_guild />
        <h3>"AI reads (MCP)"</h3>
        <p class="muted">"Each time an AI client read this server's data through Brim's MCP server. Newest 100, kept 90 days."</p>
        {move || reads.get().is_empty().then(|| view! { <p class="muted">"No AI reads of this server."</p> })}
        <For each=move || reads.get() key=|r| r.to_string() let:r>
            <div class="card row">
                <b>{r["tool"].as_str().unwrap_or("").to_string()}</b>
                <span class="muted">{format!("by {} at {}", r["via"].as_str().unwrap_or("?"), r["at"].as_str().unwrap_or(""))}</span>
            </div>
        </For>
    }
}

/// Closes every open dropdown (header menus and row ⋯ menus), except the one whose button `click`
/// hit (the browser toggles that). A click inside an open menu's list is outside its button, so a
/// list must stop its own clicks (Channel Setup) or close itself after a choice (Feeds).
fn close_menus(click: Option<web_sys::EventTarget>) {
    use wasm_bindgen::JsCast;
    let doc = web_sys::window().unwrap().document().unwrap();
    let Ok(open) = doc.query_selector_all("header details[open], details.rowmenu[open]") else {
        return;
    };
    let hit = click.and_then(|t| t.dyn_into::<web_sys::Node>().ok());
    for i in 0..open.length() {
        let Some(d) = open
            .item(i)
            .and_then(|n| n.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let on_button = d
            .query_selector("summary")
            .ok()
            .flatten()
            .is_some_and(|s| s.contains(hit.as_ref()));
        if !on_button {
            let _ = d.remove_attribute("open");
        }
    }
}

#[component]
fn App() -> impl IntoView {
    let (route, set_route) = signal(hash());
    window_event_listener(leptos::ev::hashchange, move |e| {
        let next = hash();
        // A new page (server or page part changed) starts at the top; tab switches keep the scroll.
        // Tickets keeps its parts mounted, so a switch between its tabs loses no edits and does not ask.
        let keeps = route.with_untracked(|r| r.get(1).is_some_and(|p| p == "tickets") && body_of(r) == body_of(&next));
        if !keeps && guard::hold(&e) {
            return;
        }
        if !route.get_untracked().iter().take(2).eq(next.iter().take(2)) {
            web_sys::window().unwrap().scroll_to_with_x_and_y(0.0, 0.0)
        }
        set_route.set(next)
    });
    let (me, set_me) = signal(None::<Value>);
    let (enrolled, set_enrolled) = signal(false);
    let (guilds, set_guilds) = signal(Vec::<Value>::new());
    // Servers the bot is not in yet but the person may add it to (the Servers page only).
    let (addable, set_addable) = signal(Vec::<Value>::new());
    provide_context(guilds);
    provide_context(me);
    let search = Search {
        q: RwSignal::new(String::new()),
        index: RwSignal::new(vec![]),
    };
    provide_context(search);
    // Per-page access from the server, refetched on every page change and after a permit save.
    let access = Access {
        pages: RwSignal::new(Value::Null),
        tick: RwSignal::new(0),
    };
    provide_context(access);
    let cfg = modules::Cfg(RwSignal::new(Value::Null));
    provide_context(cfg);
    // Newest config request and its server: an older answer or another server's must not show.
    let cfg_seq = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let cfg_guild = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let pages = StoredValue::new(
        TOP.iter()
            .chain(NAV.iter().flat_map(|(_, i)| i.iter()))
            .copied()
            .collect::<Vec<_>>(),
    );
    let guild = Signal::derive(move || {
        route
            .get()
            .first()
            .cloned()
            .filter(|g| g.bytes().all(|b| b.is_ascii_digit()))
    });
    // The server the user last opened, so pages without a server (Update, Commands) can mark it in the menu.
    let storage = || web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    let stored_last = move || {
        let g = storage().and_then(|s| s.get_item("last_guild").ok().flatten());
        // It goes into a request path, like the routed server.
        g.filter(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()))
    };
    Effect::new(move |_| {
        access.tick.track();
        route.track();
        let routed = guild.get();
        // A page without a server (Commands) shows the last server's access. After a reload
        // nothing has fetched it yet, so it is fetched here, once, for a logged-in user.
        let unknown = move || me.with(Option::is_some) && access.pages.with_untracked(Value::is_null);
        let Some(g) = routed.clone().or_else(|| unknown().then(stored_last).flatten()) else {
            return;
        };
        let g2 = g.clone();
        let (seq, n) = (cfg_seq.clone(), cfg_seq.get() + 1);
        seq.set(n);
        if *cfg_guild.borrow() != g {
            cfg.0.set(Value::Null);
            *cfg_guild.borrow_mut() = g.clone();
        }
        spawn_local(async move {
            let v = api("GET", &format!("/api/guilds/{g}/access"), None)
                .await
                .ok()
                // A 200 without a body (proxy, cut answer) is no answer: it left the page on its skeleton.
                .filter(|v| v["guild"].is_string())
                // Failed: nothing is shown as allowed (the server still refuses); the page offers a retry.
                .unwrap_or_else(|| serde_json::json!({ "guild": g, "failed": true }));
            // A late answer for a server the user already left must not overwrite the new one; on a
            // page without a server only the last server's answer counts.
            let Some(now) = guild.try_get_untracked() else {
                return;
            };
            if now.or_else(stored_last).as_deref() == Some(g.as_str()) {
                access.pages.set(v);
            }
        });
        if routed.is_none() {
            return;
        }
        // Module on/off state for the sidebar and module pages; a failure just shows no state.
        spawn_local(async move {
            let (g, v) = (g2.clone(), api("GET", &format!("/api/guilds/{g2}/config"), None).await.unwrap_or(Value::Null));
            if seq.get() == n && guild.try_get_untracked().flatten().as_deref() == Some(g.as_str()) {
                cfg.0.set(v);
            }
        });
    });
    // The open page's access as one value. The access is fetched again on every page change; an
    // unchanged answer must not rebuild the page (that loaded its settings twice), and another
    // server's answer must not show while this one's is still on its way.
    // What the router builds, from the address: a tab click changes only the parts it must (see the page arm below).
    let frame_key = Memo::new(move |_| route.with(|r| frame_of(r)));
    let body_key = Memo::new(move |_| route.with(|r| body_of(r)));
    let top_key = Memo::new(move |_| route.with(|r| (r.get(1).cloned().unwrap_or_default(), r.get(2..).map(<[String]>::to_vec).unwrap_or_default())));
    let page_access = Memo::new(move |_| {
        let r = route.get();
        match (r.first(), r.get(1)) {
            (Some(g), Some(p))
                if access
                    .pages
                    .with(|a| a["guild"].as_str() == Some(g.as_str())) =>
            {
                // "failed" apart from "hidden", so a retry that changes the kind rebuilds the card.
                if access.pages.with(|a| a["failed"] == true) { "failed".to_string() } else { access.of(p) }
            }
            (Some(g), Some(_)) if g.bytes().all(|b| b.is_ascii_digit()) => "loading".to_string(),
            (_, Some(p)) => access.of(p),
            _ => String::new(),
        }
    });
    // Only a change between "may open" and loading, hidden or failed builds the whole page again.
    let page_gate = Memo::new(move |_| Some(page_access.get()).filter(|a| matches!(a.as_str(), "loading" | "hidden" | "failed")).unwrap_or_default());
    // Home of a server with no level, or whose access failed. A memo, so an unchanged answer does not
    // rebuild the page.
    // Some(failed), so a retry that changes the kind rebuilds the card.
    let home_blocked = Memo::new(move |_| {
        let r = route.get();
        access.pages.with(|a| (r.len() == 1 && a["guild"].as_str() == r.first().map(String::as_str) && (a["level"] == "none" || a["failed"] == true)).then(|| a["failed"] == true))
    });
    // 2FA on request, for protected changes made in Discord (the bot checks this step-up).
    let two_fa = step_up();
    // The sidebar module switch: its toast and busy flag outlive the nav, which is rebuilt when the config changes.
    let (switch_msg, switch_busy) = (Msg::default(), RwSignal::new(false));
    let (last, set_last) = signal(stored_last());
    Effect::new(move |_| {
        if let Some(g) = guild.get() {
            if let Some(s) = storage() {
                let _ = s.set_item("last_guild", &g);
            }
            set_last.set(Some(g));
        }
    });
    let server_list = move || {
        let cur = last.get();
        view! {
            <a href="#/">"Servers"</a>
            {guilds.get().into_iter().map(|x| {
                let id = x["id"].as_str().unwrap_or("").to_string();
                let name = x["name"].as_str().unwrap_or("").to_string();
                let icon = match x["icon"].as_str() {
                    Some(i) => view! { <img src=format!("https://cdn.discordapp.com/icons/{id}/{i}.png?size=64") alt="" /> }.into_any(),
                    None => view! { <i>{name.chars().next().unwrap_or('?').to_string()}</i> }.into_any(),
                };
                view! { <a class="gl" class:on=cur.as_deref() == Some(id.as_str()) href=format!("#/{id}")>{icon}<span>{name}</span></a> }
            }).collect_view()}
        }
    };
    let (nav_open, set_nav_open) = signal(false);
    window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" {
            set_nav_open.set(false);
            close_menus(None)
        }
    });
    // A dropdown is a <details>: it stays open until closed, also across a page change.
    window_event_listener(leptos::ev::click, |e| close_menus(e.target()));
    window_event_listener(leptos::ev::hashchange, |_| close_menus(None));
    let (filter, set_filter) = signal(String::new());
    let (guilds_note, set_guilds_note) = signal(String::from("Loading servers..."));
    let load_guilds = move || async move {
        match api("GET", "/api/guilds", None).await {
            Ok(g) => {
                let (more, list): (Vec<Value>, Vec<Value>) = g.as_array().cloned().unwrap_or_default().into_iter().partition(|x| x["addable"] == true);
                // Only servers to add: the cards say so, not the "no servers you can manage" help.
                let none = list.is_empty() && more.is_empty();
                set_addable.set(more);
                set_guilds_note.set(if none {
                    NO_SERVERS.into()
                } else {
                    String::new()
                });
                set_guilds.set(list);
            }
            // "login required" (Discord refused the login) never lands here: `api` sends a 401 to sign-in.
            Err(e) => set_guilds_note.set(format!("Could not load servers: {e}")),
        }
    };
    // Refresh: the server list (name, icon) again, and the open page built anew so it fetches its
    // data again. Before, it fetched only the server list, so nothing on the page changed.
    let (page_tick, refreshing) = (RwSignal::new(0u32), RwSignal::new(false));
    let refresh = move || {
        let go = move || {
            refreshing.set(true);
            spawn_local(async move {
                load_guilds().await;
                let note = guilds_note.get_untracked();
                if note.starts_with("Could not") {
                    switch_msg.err(note);
                }
                page_tick.update(|n| *n += 1);
                refreshing.set(false);
            })
        };
        // A save bar shows unsaved edits; building the page anew would drop them. A bar inside `.out` is
        // fading away after Save or Cancel.
        let unsaved = document().query_selector("main :not(.out) > .bar").ok().flatten().is_some();
        if unsaved {
            ask(Ask::new("Refresh without saving?").text("The changes since the last save are lost.").yes("Refresh").danger(), move |_| go());
        } else {
            go();
        }
    };
    // The server under the logo: in the sidebar, and on phones in a line under the header.
    let srv_row = move |g: String| {
        let gi = guilds.get().into_iter().find(|x| x["id"] == g.as_str());
        let name = gi.as_ref().and_then(|x| x["name"].as_str()).unwrap_or("Server").to_string();
        let icon = match gi.as_ref().and_then(|x| x["icon"].as_str()) {
            Some(i) => view! { <img src=format!("https://cdn.discordapp.com/icons/{g}/{i}.png?size=64") alt="" loading="lazy" decoding="async" /> }.into_any(),
            None => view! { <i>{name.chars().next().unwrap_or('?').to_string()}</i> }.into_any(),
        };
        // The button sits next to the link, not inside it: a button inside a link is invalid HTML.
        view! { <div class="srv-row"><a class="srv" href="#/" title="Switch server">{icon}<span>{name}</span></a><button class="ghost refresh" title="Refresh the server and this page" aria-label="refresh the server and this page" disabled=move || refreshing.get() on:click=move |_| refresh()>{icons::icon("refresh")}</button></div> }
    };
    // Nothing is drawn until /api/me answers: the landing page drawn first showed for a moment
    // after every login before the app replaced it.
    let (checked, me_err) = (RwSignal::new(false), RwSignal::new(None::<String>));
    spawn_local(async move {
        let r = api("GET", "/api/me", None).await;
        // `me` first: the landing page must not show between the two sets.
        let ok = r.is_ok();
        match r {
            Ok(v) => {
                CSRF.with(|c| *c.borrow_mut() = v["csrf"].as_str().unwrap_or("").to_string());
                set_enrolled.set(v["totp_enrolled"].as_bool().unwrap_or(false));
                set_me.set(Some(v));
            }
            // Only "not signed in" is the landing page; a failed server is said as it is.
            Err(e) if e != "login required" => me_err.set(Some(e)),
            Err(_) => {}
        }
        checked.set(true);
        if ok {
            load_guilds().await;
        }
    });
    // The header logo opens the public front page, also when signed in.
    let front = move || route.get().first().is_some_and(|a| a == "front");
    // The servers list and the public appeal page have no menu to open.
    let bare = move || route.get().first().is_none_or(|a| a == "appeal");
    view! {
        <Show when=move || me.get().is_some() && !front() fallback=move || if !checked.get() { view! { <main class="boot"><Working /></main> }.into_any() } else if let Some(e) = me_err.get() { view! { <main><div class="card empty"><h2>"Brim could not load your sign-in"</h2><p>{e}</p><button on:click=|_| { let _ = web_sys::window().unwrap().location().reload(); }>"Try again"</button></div></main> }.into_any() } else if route.get().first().is_some_and(|a| a == "commands") { view! { <main><Commands /></main> }.into_any() } else { view! { <landing::Landing signed_in=me.get().is_some() /> }.into_any() }>
            <header>
                <button class="ghost menu-btn" class:hide=bare aria-label="menu" aria-expanded=move || nav_open.get().to_string() on:click=move |_| set_nav_open.update(|o| *o = !*o)>{icons::icon("menu")}</button>
                <a class="logo" href="#/front" aria-label="Brim front page"><img src="/logo.svg" alt="" width="32" height="32" /><span>"Brim"</span></a><Show when=move || guild.get().is_some()><SearchBox guild pages=pages.get_value() /></Show><span class="sp"></span>
                <a class="ghost" href="#/commands">"Commands"</a>
                <Show when=move || me.get().is_some_and(|m| m["bot_owner"] == true)><a class="ghost" href="#/update">"Update"</a></Show>
                <Bell route />
                <details>
                    <summary aria-label="account menu">{move || me.get().and_then(|m| m["username"].as_str().and_then(|u| u.chars().next()).map(|c| c.to_uppercase().to_string())).unwrap_or_default()}</summary>
                    <div><a href="#/">"Servers"</a><a class="m-only" href="#/commands">"Commands"</a><Show when=move || me.get().is_some_and(|m| m["bot_owner"] == true)><a class="m-only" href="#/update">"Update"</a><a href="#/mcp-tokens">"MCP tokens"</a><a href="#/raiders">"Raider List review"</a><a href="#/data-requests">"Data requests"</a><a href="#/secure-account">"Secure my account"</a></Show><a href="#/raider-appeal">"Raider List status"</a><button title="Lets you make protected changes in Discord for 10 minutes" on:click=move |_| two_fa.set(true)>"Confirm 2FA"</button><button on:click=move |_| spawn_local(async { let _ = api("POST", "/auth/logout", None).await; signed_out(); web_sys::window().unwrap().location().reload().ok(); })>"Logout"</button></div>
                </details>
            </header>
            <Toast msg=switch_msg /><StepUp enrolled /><Dialog /><RescuePrompt guild page=Signal::derive(move || route.get().get(1).cloned()) />
            {move || guild.get().filter(|_| !route.get().is_empty()).map(|g| view! { <div class="m-srv">{srv_row(g)}</div> })}
            <div class="shell">
                <Show when=move || nav_open.get()><div class="scrim" on:click=move |_| set_nav_open.set(false)></div></Show>
                <nav class:open=move || nav_open.get() class:hide=bare on:click=move |_| set_nav_open.set(false)>
                    {move || match route.get().first() {
                        None => ().into_any(),
                        Some(a) if ["commands", "update", "ui", "mcp-tokens", "raiders", "raider-appeal", "data-requests", "secure-account"].contains(&a.as_str()) => server_list().into_any(),
                        Some(a) if a == "appeal" => ().into_any(),
                        Some(g) => {
                            let g = g.clone();
                            let page = route.get().get(1).cloned().unwrap_or_default();
                            let hits = search::counts(search);
                            let open = if page == "module" { route.get().get(2).and_then(|s| modules::group_by_slug(s)) } else if modules::MODULE_ORDER.contains(&nav_key(&page)) { None } else { modules::group_of(&page).filter(|g| modules::is_module(g)) };
                            // Every module row with a single on/off shows its switch, open or not (owner decision 2026-11-21).
                            let switch = |entry: &str| modules::switch_of(entry).and_then(|(sec, key)| modules::switch_on(&cfg.0.get(), sec, key).map(|on| view! { <modules::ModuleSwitch guild=g.clone() section=sec key group=modules::label(entry) on msg=switch_msg need=two_fa busy=switch_busy /> }));
                            let link = |p: &str, t: &str| { if access.of(p) == "hidden" { return ().into_any(); } let n: usize = hits.iter().filter(|(k, _)| *k == p || k.starts_with(&format!("{p}/"))).map(|(_, n)| n).sum(); let on = nav_key(&page) == p; view! { <a class:on=on href=format!("#/{g}/{p}")>{icons::icon(page_icon(p))}<span>{t.to_string()}</span>{switch(p)}{(n > 0).then(|| view! { <span class="chip hits">{n}</span> })}</a> }.into_any() };
                            view! {
                                {srv_row(g.clone())}
                                <a class:on=page.is_empty() href=format!("#/{g}")>{icons::icon("dashboard")}<span>"Home"</span></a>
                                {TOP.iter().filter(|(p, _)| !modules::MODULE_ORDER.contains(p)).map(|(p, t)| link(p, t)).collect_view()}
                                <p class="nav-label">"Modules"</p>
                                {modules::MODULE_ORDER.iter().map(|entry| match NAV.iter().find(|(grp, _)| grp == entry) {
                                    None => link(entry, &modules::label(entry)),
                                    Some((grp, items)) => {
                                        if items.iter().all(|(p, _)| access.of(p) == "hidden") { return ().into_any(); }
                                        let n: usize = hits.iter().filter(|(k, _)| items.iter().any(|(p, _)| k == p || k.starts_with(&format!("{p}/")))).map(|(_, n)| n).sum();
                                        let is_open = open == Some(*grp);
                                        view! { <a class:on=is_open href=format!("#/{g}/module/{}", modules::slug(grp))>{icons::icon(page_icon(grp))}<span>{*grp}</span>{switch(grp)}{(n > 0).then(|| view! { <span class="chip hits">{n}</span> })}</a> }.into_any()
                                    }
                                }).collect_view()}
                                <p class="nav-label">"Tools"</p>
                                {NAV.iter().filter(|(grp, _)| modules::GENERAL_GROUPS.contains(grp)).flat_map(|(_, items)| items.iter()).map(|(p, t)| link(p, t)).collect_view()}
                            }.into_any()
                        }
                    }}
                    <Show when=move || !route.get().is_empty()><Pose pose=Brim::Peeking class="peek" /></Show>
                </nav>
                <main>
                    {move || { page_tick.track(); match frame_key.get().as_slice() {
                        [] => view! {
                            <Head guild=String::new() page="Brim".into() big="Servers".into() />
                            <div class="row"><input placeholder="Filter: [Type name or ID]" aria-label="filter servers" prop:value=filter on:input=move |e| set_filter.set(event_target_value(&e)) /><LostControl /><button class="ghost" on:click=move |_| { set_guilds_note.set("Loading servers...".into()); spawn_local(load_guilds()) }>"Refresh servers"</button></div>
                            {move || if guilds_note.get() == NO_SERVERS { view! { <div class="card empty">
                                <Pose pose=Brim::Waving class="pose-top" />
                                <h2>"No servers you can manage."</h2>
                                <ol>
                                    <li>"To manage a server you need to be either:"<ul>
                                        <li>"The "<b>"physical"</b>" server owner (the one with the crown)."</li>
                                        <li>"An "<b>"Extra Owner"</b>" (added by the server owner in Permits)."</li>
                                        <li>"A "<b>"Trusted Admin"</b>" (added by the server owner in Permits)."</li>
                                        <li>"A member whose permit can view the dashboard."</li></ul></li>
                                    <li>"Is the server freshly made, or Brim just added? "<button class="go" on:click=move |_| { set_guilds_note.set("Loading servers...".into()); spawn_local(load_guilds()) }>"Refresh"</button></li>
                                </ol></div> }.into_any() } else if guilds_note.get() == "Loading servers..." { view! { <p><Working /></p> }.into_any() } else { view! { <p class="muted">{guilds_note.get()}</p> }.into_any() }}
                            {move || {
                                let f = filter.get().to_lowercase();
                                let hit = move |g: &Value| g["name"].as_str().unwrap_or("").to_lowercase().contains(&f) || g["id"].as_str().unwrap_or("").contains(&f);
                                let card = |g: &Value| {
                                    let id = g["id"].as_str().unwrap_or("").to_string();
                                    let name = g["name"].as_str().unwrap_or("").to_string();
                                    // Discord's own sizes: banner 16:9 (480 wide), icon 128 shown at 64.
                                    let banner = g["banner"].as_str().map(|b| format!("https://cdn.discordapp.com/banners/{id}/{b}.png?size=512"));
                                    let icon = match g["icon"].as_str() {
                                        Some(i) => view! { <img class="gicon" src=format!("https://cdn.discordapp.com/icons/{id}/{i}.png?size=128") alt="" loading="lazy" decoding="async" /> }.into_any(),
                                        None => view! { <i class="gicon">{name.chars().next().unwrap_or('?').to_string()}</i> }.into_any(),
                                    };
                                    // No banner (most servers have none): the icon, enlarged and dimmed, so the card still looks like the server.
                                    let banner = match (banner, g["icon"].as_str()) {
                                        (Some(u), _) => view! { <img class="gbanner" src=u alt="" loading="lazy" decoding="async" /> }.into_any(),
                                        (None, Some(i)) => view! { <img class="gbanner gfill" src=format!("https://cdn.discordapp.com/icons/{id}/{i}.png?size=128") alt="" loading="lazy" decoding="async" /> }.into_any(),
                                        (None, None) => view! { <div class="gbanner"></div> }.into_any(),
                                    };
                                    let foot = if g["addable"] == true {
                                        view! { <a class="ghost" href=format!("/auth/invite?guild_id={id}") target="_blank" rel="noopener noreferrer">"Add to server"</a> }.into_any()
                                    } else {
                                        view! { <span class="muted">{g["level"].as_str().unwrap_or("").to_string()}</span> }.into_any()
                                    };
                                    let inner = view! { <div class="gtop">{banner}{icon}</div><div class="gbody"><b>{name}</b>{foot}</div> };
                                    if g["addable"] == true {
                                        view! { <div class="gcard">{inner}</div> }.into_any()
                                    } else {
                                        view! { <a class="gcard" href=format!("#/{id}")>{inner}</a> }.into_any()
                                    }
                                };
                                let mine: Vec<_> = guilds.get().iter().filter(|g| hit(g)).map(card).collect();
                                let more: Vec<_> = addable.get().iter().filter(|g| hit(g)).map(card).collect();
                                view! {
                                    <div class="gcards">{mine}</div>
                                    {(!more.is_empty()).then(|| view! { <h2 class="gsub">"Add Brim to"</h2><div class="gcards">{more}</div> })}
                                }
                            }}
                        }.into_any(),
                        [a] if a == "commands" => view! { <Commands /> }.into_any(),
                        [a] if a == "update" => view! { <Update /> }.into_any(),
                        [a] if a == "ui" => view! { <styleguide::StylePage /> }.into_any(),
                        [a] if a == "mcp-tokens" => view! { <McpTokens /> }.into_any(),
                        [a] if a == "raiders" => view! { <RaiderReview /> }.into_any(),
                        [a] if a == "data-requests" => view! { <DataRequests /> }.into_any(),
                        [a] if a == "secure-account" => view! { <secure::SecureAccount me=Signal::derive(move || me.get()) /> }.into_any(),
                        [a] if a == "raider-appeal" => view! { <RaiderAppeal /> }.into_any(),
                        [a, g] if a == "appeal" =>view! { <AppealForm guild=g.clone() /> }.into_any(),
                        // No level on this server, or its access did not load: no Home (it offered the wizard).
                        [g] if home_blocked.get().is_some() => view! { <Head guild=g.clone() page="Home".to_string() />{no_access(access, home_blocked.get_untracked() == Some(true), "You have no access to this server in Brim. Ask a server owner if you need it.")} }.into_any(),
                        [g] => view! { <Head guild=g.clone() page="Home".to_string() /><home::Home guild=g.clone() /> }.into_any(),
                        [g, _, ..] => {
                            let p = &route.with_untracked(|r| r[1].clone());
                            // Nothing of the page is built before its access is known, so a hidden page loads nothing.
                            let acc = page_gate.get();
                            if acc == "loading" || acc == "hidden" || acc == "failed" {
                                let body = if acc != "loading" { no_access(access, acc == "failed", "Your permit does not give access to this page. Ask a server owner if you need it.") } else { view! { <div class="skel"></div> }.into_any() };
                                return view! { <Head guild=g.clone() page=title(p).0 />{body} }.into_any();
                            }
                            // A tab click must not build the tab row or the page again (that blanked the page, faded it in twice
                            // and moved the underline late): the header, the tab row and the body each follow only their own part.
                            let read = Signal::derive(move || page_access.get() == "read");
                            let tabs = tab_group(p, route.with_untracked(|r| r.get(2).cloned()).as_deref()).map(|items| {
                                let active = Signal::derive(move || route.with(|r| tab_key(r.get(1).map_or("", String::as_str), r.get(2).map(String::as_str))));
                                view! { <Tabs guild=g.clone() items active read /> }
                            });
                            let g2 = g.clone();
                            let top = move || top_key.with(|(p, rest)| if p.is_empty() { ().into_any() } else { page_top(&g2, p, rest, &page_access.get(), access) });
                            let g3 = g.clone();
                            let (tab, sub) = (part_at(route, 2), part_at(route, 3));
                            let body = move || body_key.with(|r| {
                                if r.len() < 2 { return ().into_any(); }
                                let page = page_body(&g3, &r[1], &r[2..], tab, sub);
                                if read.get() { view! { <fieldset class="ro" disabled>{page}</fieldset> }.into_any() } else { page }
                            });
                            view! { {top}{tabs}{body} }.into_any()
                        }
                    }}}
                </main>
            </div>
        </Show>
    }
}

/// The tab of a route tab row that an address opens: the page, or "page/part" for Filters and Whitelist.
fn tab_key(p: &str, part: Option<&str>) -> String {
    match part {
        Some(s) if p == "filters" || p == "whitelist" => format!("{p}/{s}"),
        _ => p.to_string(),
    }
}

/// The route tab row a page belongs to.
fn tab_group(p: &str, part: Option<&str>) -> Option<&'static [(&'static str, &'static str)]> {
    let key = tab_key(p, part);
    [LOCK_TABS, APPEAL_TABS, RAID_TABS, RAIDER_TABS, GATE_TABS, REPORT_TABS, WL_TABS, SPAM_TABS].into_iter().find(|t| t.iter().any(|(k, _)| *k == key))
}

/// The address without the parts a page switches itself (the tabs of Tickets and Logging).
fn body_of(r: &[String]) -> Vec<String> {
    match r.get(1).map(String::as_str) {
        Some("logging") => r[..2].to_vec(),
        Some("tickets") if !(r.len() == 5 && r[2] == "log") => r[..2].to_vec(),
        _ => r.to_vec(),
    }
}

/// `body_of`, with every page of one route tab row as one key: the header and the tab row stay, only the body changes.
fn frame_of(r: &[String]) -> Vec<String> {
    let f = body_of(r);
    match (f.first(), f.get(1).and_then(|p| tab_group(p, f.get(2).map(String::as_str)))) {
        (Some(g), Some(items)) => vec![g.clone(), format!("tabs:{}", items[0].0)],
        _ => f,
    }
}

/// One part of the address, as a signal.
fn part_at(route: ReadSignal<Vec<String>>, n: usize) -> Signal<String> {
    Signal::derive(move || route.with(|r| r.get(n).cloned().unwrap_or_default()))
}

/// The body of a server page (below the header and its tab row). `tab` and `sub` are the address parts after the page,
/// for the pages that switch their own tabs (Tickets, Logging) without being built again.
fn page_body(g: &String, p: &str, rest: &[String], tab: Signal<String>, sub: Signal<String>) -> AnyView {
    let sec = |name: &str, part: &str| view! { <Section guild=g.clone() name=name.to_string() part=part.to_string() /> }.into_any();
    match (p, rest.first()) {
        ("module", Some(slug)) => match modules::group_by_slug(slug) { Some(grp) => view! { <modules::ModulePage guild=g.clone() group=grp /> }.into_any(), None => view! { <div class="card empty"><Pose pose=Brim::Shrug class="pose-top" /><h2>"No such module"</h2></div> }.into_any() },
        ("permits", _) => view! { <Permits guild=g.clone() /> }.into_any(),
        ("logging", _) => view! { <Logging guild=g.clone() tab sub /> }.into_any(),
        ("embeds", _) => view! { <Embeds guild=g.clone() open_id=rest.first().cloned().unwrap_or_default() /> }.into_any(),
        ("bot_messages", _) => view! { <botmsgs::BotMessages guild=g.clone() open_key=rest.first().cloned().unwrap_or_default() from_tickets=rest.get(1).is_some_and(|r| r == "from-tickets") /> }.into_any(),
        ("feeds", _) => view! { <feeds::Feeds guild=g.clone() /> }.into_any(),
        ("giveaways", _) => view! { <giveaways::Giveaways guild=g.clone() /> }.into_any(),
        ("history", _) => view! { <History guild=g.clone() /> }.into_any(),
        ("overview", _) => view! { <Score guild=g.clone() /> }.into_any(),
        ("wizard", _) => view! { <Wizard guild=g.clone() /> }.into_any(),
        ("revert", _) => view! { <Revert guild=g.clone() /> }.into_any(),
        ("backups", _) => view! { <Backups guild=g.clone() open=rest.first().cloned().unwrap_or_default() /> }.into_any(),
        ("channel_setup", _) => view! { <ChannelSetup guild=g.clone() /> }.into_any(),
        ("automod_panic", _) => view! { <Panic guild=g.clone() part="heat,general" /> }.into_any(),
        ("antinuke_panic", _) => view! { <Panic guild=g.clone() part="nuke,general" /> }.into_any(),
        ("tickets", Some(l)) if l == "log" && rest.len() == 3 => view! { <TicketLog guild=g.clone() channel=rest[1].clone() message=rest[2].clone() /> }.into_any(),
        ("tickets", _) => view! { <Tickets guild=g.clone() tab /> }.into_any(),
        ("rescue", _) => view! { <Rescue guild=g.clone() /> }.into_any(),
        ("bot_profile", _) => view! { <profile::BotProfile guild=g.clone() /> }.into_any(),
        ("misc", _) => view! { {sec("misc", "")}<PurgeData guild=g.clone() /> }.into_any(),
        ("lockdown", _) => view! { <LockdownManage guild=g.clone() /> }.into_any(),
        ("lockdown_settings", _) => view! { <LockdownSettings guild=g.clone() /> }.into_any(),
        ("discord_automod", _) => view! { <DiscordRules guild=g.clone() /> }.into_any(),
        ("appeal_list", _) => view! { <AppealList guild=g.clone() /> }.into_any(),
        ("appeal_blacklist", _) => view! { <Blacklist guild=g.clone() base="appeals" purge=true /> }.into_any(),
        ("joinraid", _) => view! { {sec("joinraid", "")} }.into_any(),
        ("raid_log", _) => view! { <RaidLog guild=g.clone() /> }.into_any(),
        ("raider_list", _) => view! { {sec("raider_list", "")} }.into_any(),
        ("raider_matches", _) => view! { <RaiderMatches guild=g.clone() /> }.into_any(),
        ("joingate", _) => view! { {sec("joingate", "")} }.into_any(),
        ("joingate_log", _) => view! { <JoinGateLog guild=g.clone() /> }.into_any(),
        ("catch", _) => view! { <Catch guild=g.clone() /> }.into_any(),
        ("cases", Some(no)) => view! { <CasePage guild=g.clone() no=no.clone() /> }.into_any(),
        ("cases", _) => view! { <CaseList guild=g.clone() /> }.into_any(),
        ("mod_stats", _) => view! { <ModStats guild=g.clone() /> }.into_any(),
        ("notes", _) => view! { <NoteList guild=g.clone() /> }.into_any(),
        ("report_list", _) => view! { <ReportList guild=g.clone() /> }.into_any(),
        ("report_blacklist", _) => view! { <Blacklist guild=g.clone() base="reports" /> }.into_any(),
        ("appeals", _) => view! { {sec("appeals", "")} }.into_any(),
        ("filters", None) => view! { <div class="stack">{FILTER_PAGES.iter().map(|(sub, t, d)| view! { <a class="card link" href=format!("#/{g}/filters/{sub}")><span class="ico">{icons::icon(page_icon(sub))}</span><span><h3>{*t}</h3><span class="muted">{*d}</span></span><span class="sp"></span>{icons::icon("chevron_right")}</a> }).collect_view()}</div> }.into_any(),
        ("filters", Some(sub)) => {
                                                view! { <h3 class="center">{match sub.as_str() { "spam_text" => "Text Message Filters", "spam_mentions" => "Mention Filters", "spam_attach" => "Attachment Filters", _ => "" }}</h3>{sec("automod", sub)} }.into_any()
        }
        ("whitelist", sub) => {
            view! { {sec("whitelist", sub.map_or("general", String::as_str))} }.into_any()
        }
        ("automod", _) => sec("automod", "general"),
        ("antinuke", _) => view! { {sec("antinuke", "general")}<NukeCoverage guild=g.clone() /> }.into_any(),
        ("antinuke_filters", _) => sec("antinuke", "limits"),
        _ => sec(p, ""),
    }
}

/// The header of a server page: crumb and title, the folded "How it works" box and the read-only note.
fn page_top(g: &String, p: &String, rest: &[String], acc: &str, access: Access) -> AnyView {
    if p == "module" {
        return ().into_any();
    }
    { let (page_name, big) = title(p);
      // Untracked: reading the access here rebuilt the whole page (and cleared typed text) each time the access was fetched again.
      let (module, page_name) = untrack(|| module_of(g, p, if p == "logging" { Some(if rest.first().is_some_and(|r| r == "discord") { "discord" } else { "brim" }) } else { rest.first().map(String::as_str) }, |k| access.of(k) != "hidden")).map_or((None, page_name), |(m, n)| (Some(m), n)); let help = help::steps(nav_key(p));
      // A detail page's crumb adds its parent ("Moderation › Filters › Anti Spam"); there is no back button.
      let detail = match (p.as_str(), rest.first()) {
          ("filters", Some(sub)) => Some(("Filters", "filters", FILTER_PAGES.iter().find(|(s, ..)| sub.starts_with(s) || (*s == "spam_text" && sub.starts_with("spam_"))).map_or(String::new(), |(_, t, _)| t.to_string()))),
          ("cases", Some(no)) => Some(("Cases", "cases", format!("Case #{no}"))),
          ("backups", Some(code)) => Some(("Backups", "backups", format!("Backup #{code}"))),
          _ => None,
      };
      let (up, page_name, big) = match detail {
          Some((parent, path, name)) => (Some((parent.to_string(), format!("#/{g}/{path}"))), name.clone(), Some(name)),
          None => (None, page_name, big),
      };
      // A page inside a module shows its own name as the big title.
      let big = big.or_else(|| module.as_ref().map(|_| page_name.clone()));
      let pose = page_pose(p);
      let head = match big { Some(b) => view! { <Head guild=g.clone() page=page_name big=b module up help pose /> }.into_any(), None => view! { <Head guild=g.clone() page=page_name module up help pose /> }.into_any() };
      let note = (acc == "read").then(|| view! { <p class="card info" role="note">"You can view this page, but your permit does not let you change it."</p> });
      // Hard pages open with a folded "How it works" box (the same steps as the header's "?").
      let how = help.map(|steps| view! { <details class="card how"><summary>"How it works"</summary><ol>{steps.iter().map(|s| view! { <li>{*s}</li> }).collect_view()}</ol></details> });
      view! { {head}{how}{note} }.into_any() }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        console_error_panic_hook::hook(info);
        if let Some(body) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.body())
        {
            let _ = body.insert_adjacent_text("beforeend", &format!("Dashboard crashed: {info}"));
        }
    }));
    // The app gets its own box: the screen scale (`zoom`) sits there, not on the page root, so a
    // password manager's button added to the page is not scaled twice (style.css, --z).
    let app = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("app"))
        .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(e).ok());
    match app {
        Some(el) => leptos::mount::mount_to(el, App).forget(),
        None => leptos::mount::mount_to_body(App),
    }
}


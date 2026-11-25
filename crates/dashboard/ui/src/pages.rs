use crate::modules;
use crate::common::*;
use crate::form::Section;
use crate::wizard::{Card, MISC, TICKETS, WHITELIST};
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn s(v: &Value) -> String {
    v.as_str()
        .map(String::from)
        .unwrap_or_else(|| v.to_string())
}

/// Auto Setup's steps as the setup page lists them; the names match the server's `steps` result.
const SETUP_STEPS: [&str; 7] = [
    "Roles",
    "Quarantine",
    "Logs",
    "Staff channels",
    "Information",
    "Verification channel",
    "Member-only channels",
];

/// Log channels the wizard's Custom choice can add to the essentials: (channel name, label).
const OPTIONAL_LOGS: [(&str, &str); 10] = [
    ("reports", "Reports"),
    ("appeals", "Appeals"),
    ("automod-logs", "Automod"),
    ("antinuke-logs", "Antinuke"),
    ("verification-logs", "Verification"),
    ("joingate-logs", "Joingate"),
    ("joinraid-logs", "Joinraid"),
    ("panic-logs", "Panic Mode"),
    ("server-logs", "Server"),
    ("discord-logs", "Discord events"),
];

#[component]
pub fn Wizard(guild: String) -> impl IntoView {
    const STEPS: [&str; 7] = [
        "Auto Setup",
        "Misc",
        "Security",
        "Whitelist",
        "Tickets",
        "Rescue",
        "Wrap",
    ];
    const CARDS: [(&str, &[Card]); 3] = [("misc", MISC), ("whitelist", WHITELIST), ("tickets", TICKETS)];
    // Step and Auto Setup picks are kept per server in this browser, so leaving the wizard and
    // coming back resumes it; the last step clears them. Nothing secret is stored.
    let key = format!("wizard:{guild}");
    let saved: Value = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(&key).ok().flatten())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let step = RwSignal::new((saved["step"].as_u64().unwrap_or(0) as usize).min(STEPS.len() - 1));
    // The last step tells the server once that setup is done, so Overview stops asking for it.
    let marked = StoredValue::new(false);
    // The ring scores the saved setup; it is worked out again each time the step changes.
    let pct = RwSignal::new(0usize);
    let dc = RwSignal::new(Value::Null);
    let g = guild.clone();
    Effect::new(move |_| {
        step.track();
        let g = g.clone();
        spawn_local(async move {
            if dc.get_untracked().is_null() {
                dc.set(
                    api("GET", &format!("/api/guilds/{g}/discord"), None)
                        .await
                        .unwrap_or(json!({})),
                );
            }
            if let Ok(c) = api("GET", &format!("/api/guilds/{g}/config"), None).await {
                // The page may be gone by now; a disposed signal panics on read.
                let Some(d) = dc.try_get_untracked() else {
                    return;
                };
                pct.set(crate::wizard::score(&c["config"], &d, &g));
            }
        });
    });
    let (msg, need) = (Msg::default(), step_up());
    let g = guild.clone();
    let custom = RwSignal::new(saved["custom"].as_bool().unwrap_or(false));
    let picks = RwSignal::new(
        saved["picks"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>(),
    );
    // Custom only: the main role's name (made, or the first main role renamed).
    let member_name = RwSignal::new(saved["name"].as_str().unwrap_or_default().to_string());
    // Custom only: the optional Admin-only and Owner-only chats.
    let (admin_chat, owner_chat) = (RwSignal::new(saved["admin"].as_bool().unwrap_or(false)), RwSignal::new(saved["owner"].as_bool().unwrap_or(false)));
    // #staff-chat and Staff Voice in the Staff category: on unless Custom turns it off.
    let staff_chat = RwSignal::new(saved["staff"].as_bool().unwrap_or(true));
    Effect::new(move |_| {
        let v = json!({"step": step.get(), "custom": custom.get(), "picks": picks.get(), "name": member_name.get(), "admin": admin_chat.get(), "owner": owner_chat.get(), "staff": staff_chat.get()});
        if let Some(st) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
            // The last step is the end: the next visit starts fresh.
            let _ = if step.get_untracked() + 1 >= STEPS.len() { st.remove_item(&key) } else { st.set_item(&key, &v.to_string()) };
        }
    });
    // The run's own state: the toast closes itself after 3 s, so it cannot carry the result.
    let phase = RwSignal::new(Phase::Idle);
    // The server's step list after a run: (step, note when it needs you); empty before one.
    let results = RwSignal::new(Vec::<(String, Option<String>)>::new());
    let summary = RwSignal::new(String::new());
    let running = move || phase.get() == Phase::Running;
    let start = move |logs: Option<Vec<String>>| {
        let g = g.clone();
        // Only Custom sends a name; plain Auto Setup keeps an existing main role as it is.
        let name = logs.as_ref().map(|_| member_name.get_untracked()).filter(|n| !n.trim().is_empty());
        let custom = logs.is_some();
        let chats = (custom && admin_chat.get_untracked(), custom && owner_chat.get_untracked());
        let lounge = !custom || staff_chat.get_untracked();
        phase.set(Phase::Running);
        summary.set(String::new());
        results.set(vec![]);
        run_act(
            async move {
                let r = async {
                    let c = api("GET", &format!("/api/guilds/{g}/config"), None).await?;
                    api(
                        "POST",
                        &format!("/api/guilds/{g}/wizard/auto-setup"),
                        Some(json!({"expected_version": c["version"], "logs": logs, "member_name": name, "admin_chat": chats.0, "owner_chat": chats.1, "staff_chat": lounge})),
                    )
                    .await
                }
                .await;
                // Set is safe after the page is gone; a 2FA prompt is not a failure.
                match &r {
                    Err(e) if e == "step_up_required" => phase.set(Phase::Idle),
                    Err(e) => {
                        phase.set(Phase::Failed);
                        summary.set(format!("Auto Setup stopped: {e}. The steps before it were done. Fix that, then run Auto Setup again."));
                    }
                    Ok(_) => {}
                }
                r
            },
            msg,
            need,
            move |v| {
                let rows: Vec<(String, Option<String>)> = v["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| (r["step"].as_str().unwrap_or_default().to_string(), r["note"].as_str().map(String::from)))
                    .collect();
                let need = rows.iter().filter(|(_, n)| n.is_some()).count();
                results.set(rows);
                phase.set(Phase::Done);
                msg.ok("Auto Setup finished");
                summary.set(match need {
                    0 => "Done".into(),
                    1 => "Done, 1 step needs you".into(),
                    n => format!("Done, {n} steps need you"),
                });
            },
        );
    };
    // 2FA first: the prompt must come before anything says the setup started.
    // Owner decision 2026-11-16: a warning first that it creates channels, with Cancel or Create Channels.
    let auto = move |logs: Option<Vec<String>>| {
        let start = start.clone();
        let a = Ask::new("Create channels?")
            .text("Auto Setup creates roles and channels in your server: the log channels, the Information category with #announcements and #rules, and the staff roles. Nothing you already set is removed.")
            .yes("Create Channels");
        ask(a, move |_| {
            spawn_local(async move {
                let fresh = api("GET", "/api/me", None)
                    .await
                    .is_ok_and(|v| crate::common::stepped_up_here(&v));
                if msg.text_disposed() {
                    return;
                }
                if fresh {
                    start(logs)
                } else {
                    // After the code, this setup starts: the one confirmed above.
                    need_step_up(need, move || if !msg.text_disposed() { start(logs) })
                }
            })
        })
    };
    let guilds = expect_context::<ReadSignal<Vec<Value>>>();
    let g_id = guild.clone();
    let g = guild.clone();
    let me = expect_context::<ReadSignal<Option<Value>>>();
    let support = move || {
        me.get()
            .and_then(|m| m["support_url"].as_str().map(String::from))
            .unwrap_or_else(|| "https://github.com/thomkr01/Brim".into())
    };
    // Same side arrows as the settings cards of steps 1-8.
    let arrows = move |body: AnyView| {
        view! { <div class="card wiz">
            <button class="nav back" aria-label="previous" disabled=move || step.get() == 0 on:click=move |_| step.update(|i| *i = i.saturating_sub(1))>{crate::icons::icon("page_prev")}</button>
            <div class="wizbody">{body}</div>
            <button class="nav" aria-label="next" on:click=move |_| step.update(|i| *i += 1)>{crate::icons::icon("page_next")}</button>
        </div> }.into_any()
    };
    // Which way the last step change went, so the new slide comes in from that side.
    let prev = StoredValue::new(0usize);
    let last = STEPS.len() - 1;
    let server = move || {
        guilds
            .get()
            .iter()
            .find(|x| x["id"] == g_id.as_str())
            .and_then(|x| x["name"].as_str().map(String::from))
            .unwrap_or_default()
    };
    view! {
        <div class="card wizhead">
            <div class="wiztitle">
                <h2>{move || format!("{} setup", server())}</h2>
                <p class="muted">"Go step by step, or pick any step. The whole setup takes about 3 minutes."</p>
                <div class="acts">
                    <button class="go" on:click=move |_| { step.update(|i| *i = (*i + 1).min(last)); scroll_to("wizstep") }>{move || if step.get() == 0 { "Start wizard" } else { "Next step" }}</button>
                    <a class="btn ghost" href=support target="_blank" rel="noopener">"Support"</a>
                </div>
            </div>
            <div class="brand"><Ring pct=pct.into() label="Setup" /></div>
        </div>
        <div class="wizprog" style=move || format!("--s:{}", step.get() * 100 / last)>
            <div class="line" role="progressbar" aria-valuemin="1" aria-valuemax=STEPS.len() aria-valuenow=move || step.get() + 1 aria-valuetext=move || format!("Step {} of {}: {}", step.get() + 1, STEPS.len(), STEPS[step.get()]) aria-label="wizard progress"><i></i></div>
            <span>{move || format!("Step {} of {}", step.get() + 1, STEPS.len())}</span>
        </div>
        <div class="steps">
            {STEPS.iter().enumerate().map(|(i, t)| view! { <button class:on=move || step.get() == i class:done=move || i < step.get() aria-current=move || (step.get() == i).then_some("step") on:click=move |_| step.set(i)><i aria-hidden="true">{i + 1}</i>{*t}{move || (i < step.get()).then_some(view! { <span class="sr">" (done)"</span> })}</button> }).collect_view()}
        </div>
        {move || {
            let now = step.get();
            let dir = if now >= prev.get_value() { "slide fwd" } else { "slide back" };
            prev.set_value(now);
            center_step();
            view! { <div class=dir id="wizstep">{match now {
            0 => {
                let (all, run) = (auto.clone(), auto.clone());
                arrows(view! { <div>
                    <h2>"Initial Setup"</h2>
                    <p>"Brim sets up roles, channels and permissions. What you already set is kept."</p>
                    <details class="more"><summary>"What does it make?"</summary>
                        <ul class="plain">
                            <li><b>"Roles: "</b>"Quarantine, Member, and the staff roles Owner, Trusted Admin, Admins and Moderators."</li>
                            <li><b>"Logs: "</b>"#brim-logs and #mod-logs, for staff only."</li>
                            <li><b>"Staff channels: "</b>"a Staff category with a chat and a voice channel."</li>
                            <li><b>"Information: "</b>"#announcements and #rules. Members read, staff write."</li>
                            <li><b>"Verification: "</b>"only members who have not verified see the verification channel; only verified members see the other channels. Members without the verified role get a DM and the verification time limit, then are removed as your verification settings say."</li>
                        </ul>
                    </details>
                    <ul class="checks">{move || {
                        let done = results.get();
                        SETUP_STEPS.iter().enumerate().map(|(i, t)| {
                            let note = done.iter().find(|(s, _)| s == t).and_then(|(_, n)| n.clone());
                            let (ok, warn) = (!done.is_empty() && note.is_none(), note.is_some());
                            view! { <li style=format!("--i:{i}") class:run=running class:ok=ok class:warn=warn class:err=move || phase.get() == Phase::Failed>{*t}{note.map(|n| view! { <p class="muted note">{n}</p> })}</li> }
                        }).collect_view()
                    }}</ul>
                    // Owner decision 11-18: the run takes about a minute, so it says it is working.
                    <Show when=running><p role="status"><Working />" Auto Setup is running. This takes a little while on servers with many channels."</p></Show>
                    <Show when=move || !summary.get().is_empty()><p class:ok=move || phase.get() == Phase::Done class:err=move || phase.get() == Phase::Failed role="status"><b>{move || summary.get()}</b></p></Show>
                    <div class="acts"><button disabled=running on:click=move |_| all(None)>"Auto Setup"</button>
                    <button class="ghost" class:on=custom on:click=move |_| custom.update(|c| *c = !*c)>"Custom"</button><Pose pose=Brim::PointingLeft class="pose-point" /></div>
                    <Show when=move || custom.get()>
                        <div class="custom">
                            <label>"Member role name"<input type="text" maxlength="100" placeholder="Member" prop:value=move || member_name.get() on:input=move |e| member_name.set(event_target_value(&e)) /></label>
                            <fieldset><legend>"Staff channels"</legend>
                                <label><input type="checkbox" prop:checked=move || staff_chat.get() on:change=move |e| staff_chat.set(event_target_checked(&e)) />" Staff chat and voice (all staff)"</label>
                                <label><input type="checkbox" prop:checked=move || admin_chat.get() on:change=move |e| admin_chat.set(event_target_checked(&e)) />" Admin-only chat"</label>
                                <label><input type="checkbox" prop:checked=move || owner_chat.get() on:change=move |e| owner_chat.set(event_target_checked(&e)) />" Owner-only chat"</label>
                            </fieldset>
                            <fieldset><legend>"Extra log channels"</legend>
                                <div class="cols">{OPTIONAL_LOGS.iter().map(|(name, label)| view! { <label><input type="checkbox" prop:checked=move || picks.get().iter().any(|p| p == name) on:change=move |e| picks.update(|l| if event_target_checked(&e) { l.push(name.to_string()) } else { l.retain(|p| p != name) }) />" "{*label}</label> }).collect_view()}</div>
                            </fieldset>
                            <button disabled=running on:click={let run = run.clone(); move |_| run(Some(picks.get()))}>"Run with selected"</button>
                        </div>
                    </Show>
                    <crate::permits::StaffRole guild=g.clone() />
                    <Toast msg />
                </div> }.into_any())
            }
            2 => arrows(view! { <crate::security_step::Security guild=g.clone() /> }.into_any()),
            5 => arrows(view! { <Rescue guild=g.clone() /> }.into_any()),
            6 => { if !marked.get_value() { marked.set_value(true); let g = g.clone(); spawn_local(async move { let _ = api("POST", &format!("/api/guilds/{g}/wizard/done"), None).await; }) }
                view! { <div class="card"><Pose pose=Brim::ThumbsUp class="pose-side" /><h2>"All done!"</h2><p>"Brim is set up. Every setting can be changed later on its own page, and the Overview shows your security score."</p><a class="btn go" href=format!("#/{}/overview", g)>"Go to Overview"</a></div> }.into_any() }
            i => { let (name, cards) = CARDS[if i > 2 { i - 2 } else { i - 1 }]; view! { <Section guild=g.clone() name=name.to_string() cards next=Callback::new(move |_| step.update(|i| *i += 1)) /> }.into_any() }
        }}</div> }
        }}
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Running,
    Done,
    Failed,
}

/// Scrolls the step pills sideways so the current one is in view (one row on a phone).
fn center_step() {
    use wasm_bindgen::JsCast;
    request_animation_frame(|| {
        let el = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector(".steps button.on").ok().flatten())
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok());
        if let Some((el, row)) = el.and_then(|e| Some((e.clone(), e.parent_element()?))) {
            row.set_scroll_left(el.offset_left() - (row.client_width() - el.offset_width()) / 2);
        }
    });
}

/// The score ring, shared by the wizard and Overview (owner decision 2026-11-16, "sharing is caring"):
/// a flat middle in a sunken track, the arc fills to `pct` and the number counts to it each time it
/// changes, then pops once when it lands (owner decision 2026-11-18, option A). Percent only. At 100 the
/// ring lights up, so reaching it feels like a reward.
#[component]
pub fn Ring(
    pct: Signal<usize>,
    #[prop(default = None)] color: Option<&'static str>,
    #[prop(default = false)] small: bool,
    /// What the screen reader says before the percent.
    #[prop(default = "Security score")]
    label: &'static str,
) -> impl IntoView {
    // Without a fixed colour the ring is red under 50, orange under 80 and green above.
    let c = move || {
        color.unwrap_or(match pct.get() {
            0..50 => "var(--bad)",
            50..80 => "var(--warn)",
            _ => "var(--ok)",
        })
    };
    let shown = Memo::new(move |_| pct.get());
    let first = StoredValue::new(true);
    // The arc's length is 100 (pathLength), so the dash offset is what is still missing.
    view! {
        <div class="ring" class:small=small class:full=move || 100 <= pct.get() class:zero=move || pct.get() == 0 role="img" aria-label=move || format!("{label} {}%", pct.get()) style=move || format!("--p:{};--c:{}", pct.get().min(100), c())>
            <svg viewBox="0 0 120 120" aria-hidden="true">
                <defs>
                    <linearGradient id="ring-brand" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#FFA36C"/><stop offset=".35" stop-color="#FF6F91"/><stop offset=".7" stop-color="#9B7BFF"/><stop offset="1" stop-color="#4FC3F7"/></linearGradient>
                </defs>
                <circle class="track" cx="60" cy="60" r="50"/>
                <circle class="arc" cx="60" cy="60" r="50" pathLength="100"/>
            </svg>
            // A new element per new score, so the landing pop plays on every change. Not on the
            // first one: nothing counted up to it. The memo skips sets that keep the same score.
            {move || {
                let pop = !first.get_value();
                first.set_value(false);
                // Reaching 100 bursts a little confetti from the number as it lands (owner decision 2026-11-18).
                let confetti = (pop && shown.get() >= 100).then(|| view! {
                    <span class="confetti" aria-hidden="true">
                        {(0..16).map(|_| {
                            let r = js_sys::Math::random;
                            // Random direction, distance, colour and spin on every burst (owner decision 2026-11-18).
                            let style = format!("--a:{}deg;--d:{}px;--k:{};--r:{}deg", (r() * 360.0) as u32, 40 + (r() * 36.0) as u32, (r() * 4.0) as u32 % 4, (r() * 720.0) as i32 - 360);
                            view! { <i style=style /> }
                        }).collect_view()}
                    </span>
                });
                view! { <b class="num" class:landed=pop></b>{confetti} }
            }}
        </div>
    }
}

/// (section, title, description, has a master switch). A missing switch reads as off, like the bot.
const SYSTEMS: [(&str, &str, &str, bool); 5] = [
    ("automod", "Auto Mod", "Anti Spam, Anti Advertising, Anti Phishing and all the message filters.", true),
    ("antinuke", "Anti Nuke", "The system that monitors your staff members and guards your server away from getting nuked.", true),
    ("joingate", "Join Gate", "All of the instant filters that scan new members and bots joining your server.", true),
    ("joinraid", "Join Raid", "The advanced system that monitors your server joins and reacts if a raid is targeting your server.", true),
    ("verification", "Verification", "Brim's verification gate that your new members need to pass in order to get access to your server.", true),
];

/// (module, description) of the modules shown after the systems. Their switches are the sidebar's (`modules::switch_of`).
const MODULE_CARDS: [(&str, &str); 4] = [
    ("Moderation", "Cases, notes, reports, mod stats and the catch channel."),
    ("Tickets", "Members DM Brim, and staff answer in a private ticket channel."),
    ("Messages", "Your own messages, the messages Brim sends by itself, and feeds."),
    ("Logging", "Choose where Brim logs what happens."),
];

/// Master switch of a system: reads and writes its `enabled` key.
/// `all` is the guild's config with its version, loaded once and shared by every switch: with a
/// version each, the second switch flipped was refused with "config changed, reload".
#[component]
fn SystemToggle(
    guild: String,
    section: &'static str,
    title: &'static str,
    all: RwSignal<Option<Value>>,
    /// True while an Overview fix runs: it may save a version this switch would build on (pitfall 9).
    #[prop(into, optional)]
    locked: Signal<bool>,
    /// Runs after a save, so the Overview checks and score follow the switch at once.
    saved: Callback<()>,
) -> impl IntoView {
    // Greyed like the section's own page: a viewer could flip it, and only the server refused.
    let access = expect_context::<Access>();
    let on = RwSignal::new(false);
    let (msg, need) = (Msg::default(), step_up());
    // Locked while the config loads and while a save runs, so the knob never shows a state the server does not have.
    let busy = RwSignal::new(true);
    Effect::new(move |_| {
        if let Some(v) = all.get() {
            // A section the bot cannot read runs on defaults, so it never shows as on.
            let unread = v["unread"]
                .as_array()
                .is_some_and(|u| u.iter().any(|x| x == section));
            on.set(!unread && v["config"][section]["enabled"].as_bool().unwrap_or(false));
            busy.set(false);
        }
    });
    let change = move |e| {
        let x = event_target_checked(&e);
        let cur = all.get_untracked().unwrap_or_default();
        let mut value = if cur["config"][section].is_object() {
            cur["config"][section].clone()
        } else {
            json!({})
        };
        value["enabled"] = json!(x);
        let (g, v, stored) = (
            guild.clone(),
            cur["version"].as_i64().unwrap_or(0),
            value.clone(),
        );
        on.set(x);
        busy.set(true);
        run_act(
            async move {
                let r = api(
                    "PUT",
                    &format!("/api/guilds/{g}/config/{section}"),
                    Some(json!({"expected_version": v, "value": value})),
                )
                .await;
                if r.is_err() {
                    on.set(!x);
                }
                busy.set(false);
                r
            },
            msg,
            need,
            move |r| {
                msg.ok(format!("{title} turned {}", if x { "on" } else { "off" }));
                all.update(|a| {
                    if let Some(a) = a {
                        a["version"] = r["version"].clone();
                        a["config"][section] = stored.clone();
                    }
                });
                saved.run(())
            },
        );
    };
    view! { <input type="checkbox" class="sw" aria-label=format!("Enable {title}") aria-busy=move || busy.get().to_string() prop:disabled=move || busy.get() || locked.get() || access.of(section) != "full" prop:checked=move || on.get() on:change=change /><Toast msg /> }
}

/// What a fix that answered ok still could not do, in plain words: Discord refused some
/// changes (`failed`), or the catch channel kept its access (`catch_stays`). Such a fix is not
/// counted as done (the test server, 2026-11-18).
fn refused(v: &Value) -> Option<String> {
    let mut why = vec![];
    match v["failed"].as_u64() {
        Some(0) | None => {}
        Some(1) => why.push("Discord refused 1 change. Check that Brim has Manage Roles and the permissions it gives, then try again".to_string()),
        Some(n) => why.push(format!("Discord refused {n} changes. Check that Brim has Manage Roles and the permissions it gives, then try again")),
    }
    if let Some(c) = v["catch_stays"].as_str() {
        why.push(format!("the catch channel kept its access: {c}"));
    }
    (!why.is_empty()).then(|| why.join("; "))
}

/// What a fix does, in plain words, for Fix all's confirm.
fn action(r: &Value) -> String {
    let (f, title) = (&r["fix"], s(&r["title"]));
    match (f["setting"].as_str(), r["id"].as_str()) {
        (Some("quarantine_role"), _) => {
            "Create the Quarantine role and deny it in every channel".into()
        }
        (Some("member_role"), _) if f["label"] == "Use main roles" => {
            "Give members who verify your main roles".into()
        }
        (Some("member_role"), _) => "Create a Member role and make it the main role".into(),
        (Some("staff_access"), _) => "Let every staff role into the Logs and Staff categories".into(),
        (Some("catch_channel"), _) => "Make a new catch channel where Auto Setup puts it".into(),
        (Some("ticket_log_place"), _) => "Move the ticket log into the Logs category".into(),
        (Some("bot_hoist"), _) => {
            "Give Brim its own role, so it is its own group in the member list".into()
        }
        (Some("staff_hoist"), _) => {
            "Show each staff role as its own group in the member list".into()
        }
        (Some("mod_log_channel"), _) => "Create a private #mod-logs channel".into(),
        (Some("fallback_log_channel"), _) => "Create a private #brim-logs channel".into(),
        (Some("enforce_permits"), _) => {
            "Turn on: remove dangerous roles from members without permits".into()
        }
        (Some("backups_on"), _) => "Turn on backups".into(),
        (Some(x), _) if x.starts_with("enable_") => {
            format!("Turn on {title} with its default settings")
        }
        (Some("quarantine_channels"), _) => "Deny the quarantine role in every channel".into(),
        (Some("private_channel"), _) => "Hide a public log channel from members".into(),
        (Some("expression_perms"), _) => {
            "Strip emoji, sticker, sound and event permissions from members and Moderators".into()
        }
        (Some(_), _) => format!("Change the server setting: {title}"),
        (None, Some("roles_bulk")) => {
            "Strip dangerous permissions from every role and channel below the bot".into()
        }
        (None, Some("everyone_perms")) => "Strip permissions from @everyone".into(),
        (None, Some("quarantine_not_right_under")) => {
            "Move the quarantine role right below the bot".into()
        }
        _ => format!("{}: {}", s(&f["label"]), s(&r["detail"])),
    }
}

/// A single fix's confirm text: the problem, plus who loses access or which values change.
fn confirm_text(r: &Value, detail: &str) -> String {
    match r["fix"]["setting"].as_str() {
        Some("everyone_to_main") => match r["members_lose"].as_u64() {
            Some(n) => {
                let at = if r["members_lose_more"] == true { "At least " } else { "" };
                format!("{detail} {at}{n} members hold no main role and lose access to every channel @everyone opened.")
            }
            None => format!("{detail} Brim could not count the members who hold no main role. They lose access to every channel @everyone opened."),
        },
        Some("standard_access") => match r["members_lose"].as_u64() {
            Some(0) => detail.to_string(),
            Some(n) => {
                let at = if r["members_lose_more"] == true { "At least " } else { "" };
                format!("{detail} {at}{n} members hold no main role and lose access to these channels.")
            }
            None => format!("{detail} Brim could not count the members who hold no main role. They lose access to these channels."),
        },
        Some("security_profile") => {
            let m: Vec<String> = r["changes"].as_array().into_iter().flatten().map(|c| format!("- {}", s(c))).collect();
            format!("{detail}\n{}", m.join("\n"))
        }
        Some("clamp_stored") => {
            let m: Vec<String> = r["moves"].as_array().into_iter().flatten().map(s).collect();
            format!("These values change: {}.", m.join(", "))
        }
        _ => detail.to_string(),
    }
}

#[component]
pub fn Score(guild: String) -> impl IntoView {
    let rows = RwSignal::new(Vec::<Value>::new());
    let (msg, need) = (Msg::default(), step_up());
    let guilds = expect_context::<ReadSignal<Vec<Value>>>();
    let access = expect_context::<Access>();
    let members = RwSignal::new(Value::Null);
    let all = RwSignal::new(None::<Value>);
    let g = guild.clone();
    spawn_local(async move {
        if let Ok(d) = api("GET", &format!("/api/guilds/{g}/discord"), None).await {
            members.set(d["members"].clone())
        }
    });
    let setup_done = RwSignal::new(true);
    // The row whose fix runs ("*" for Fix all); every fix button waits for it.
    let busy = RwSignal::new(None::<String>);
    let sw_busy = RwSignal::new(false);
    let g = guild.clone();
    // Also reloads the config, as a fix can save a version the system switches build on.
    let score = move |g: String| async move {
        match api("GET", &format!("/api/guilds/{g}/score"), None).await {
            Ok(v) => {
                setup_done.set(v["setup_done"] != false);
                rows.set(v["rows"].as_array().cloned().unwrap_or_default())
            }
            Err(e) => msg.err(e),
        }
    };
    let load = move || {
        let g = g.clone();
        spawn_local(async move {
            // An unread config is not shown as every system off.
            let r = api("GET", &format!("/api/guilds/{g}/config"), None).await;
            // Left the page while the request ran: the Err arm below reads `all`.
            if all.is_disposed() {
                return;
            }
            match r {
                Ok(v) => all.set(Some(v)),
                // First load: the switches stay locked. A reload keeps the last loaded settings.
                Err(e) if all.get_untracked().is_none() => msg.err(format!("The settings could not be loaded, so the system switches stay locked. Reload the page to try again: {e}")),
                Err(e) => msg.err(format!("The settings could not be loaded again, so the system switches still show the settings from before and a switch can be refused as a conflict. Reload the page: {e}")),
            }
            score(g).await
        })
    };
    load();
    // Fixes that take something away get the red confirm button.
    let danger = |label: &str| {
        label.starts_with("Strip") || label.starts_with("Remove") || label == "Move to main roles"
    };
    // What Fix all runs: every fix that needs no choice. Keep and Dismiss are the owner's choice,
    // as are Move to main roles (who loses access) and Move into range (a stored 0 may mean off,
    // D70b); Strip all covers each role and overwrite strip. The quarantine and Member roles go
    // first, as other fixes build on them.
    let todo = move || {
        let r = rows.get();
        let bulk = r
            .iter()
            .any(|x| x["id"] == "roles_bulk" && x["fix"].is_object());
        let mut v: Vec<Value> = r
            .into_iter()
            .filter(|x| x["severity"] != "ok" && x["fix"].is_object())
            .filter(|x| {
                // Remove dangerous roles takes roles from members: its own confirm lists them.
                // Move to members gives Automod Immunity: who gets it is the owner's choice.
                // The recommended setup changes many sections at once: the owner presses it on purpose.
                !["everyone_to_main", "clamp_stored", "enforce_permits", "exempt_roles", "security_profile"]
                    .contains(&x["fix"]["setting"].as_str().unwrap_or_default())
            })
            .filter(|x| {
                let id = s(&x["id"]);
                !id.starts_with("owner_handover")
                    && !(bulk && (id.starts_with("role_") || id.starts_with("overwrite_")))
            })
            .collect();
        v.sort_by_key(|x| match x["id"].as_str() {
            Some("quarantine_unset" | "quarantine_missing") => 0,
            Some("main_roles") => 1,
            _ => 2,
        });
        v
    };
    let (g, reload) = (guild.clone(), load.clone());
    let fix_all = move |_| {
        let (g, load, list) = (g.clone(), reload.clone(), untrack(todo));
        let steps: Vec<String> = list.iter().map(|r| format!("- {}", action(r))).collect();
        let a = Ask::new(format!("Fix all ({})?", list.len()))
            .text(format!(
                "Brim does this, one after another:\n{}",
                steps.join("\n")
            ))
            .yes("Fix all");
        let a = if list
            .iter()
            .any(|r| danger(r["fix"]["label"].as_str().unwrap_or_default()))
        {
            a.danger()
        } else {
            a
        };
        ask(a, move |_| {
            // A run stopped by the 2FA prompt goes on with the fixes it had not done yet once the
            // code is verified: it never starts again from a list read before it ran.
            let left = std::rc::Rc::new(std::cell::RefCell::new((list, 0u64, Vec::<String>::new())));
            let make = move || {
                let (g, left) = (g.clone(), left.clone());
                async move {
                    busy.set(Some("*".into()));
                    loop {
                        let Some(r) = left.borrow().0.first().cloned() else { break };
                        let res = api("POST", &format!("/api/guilds/{g}/score/fix"), Some(r["fix"].clone())).await;
                        let mut l = left.borrow_mut();
                        match res {
                            Ok(v) => match refused(&v) {
                                None => l.1 += 1,
                                Some(why) => l.2.push(format!("{} ({why})", s(&r["title"]))),
                            },
                            Err(e) if e == "step_up_required" => {
                                busy.set(None);
                                return Err(e);
                            }
                            Err(e) => l.2.push(format!("{} ({e})", s(&r["title"]))),
                        }
                        l.0.remove(0);
                    }
                    busy.set(None);
                    let l = left.borrow();
                    Ok(json!({"n": l.1, "failed": l.2}))
                }
            };
            run_resumable(make, msg, need, move |v| {
                let n = v["n"].as_u64().unwrap_or(0);
                let failed: Vec<&str> = v["failed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let done = if n == 1 {
                    "Done. 1 issue has been fixed.".to_string()
                } else {
                    format!("Done. {n} issues have been fixed.")
                };
                if failed.is_empty() {
                    msg.done(done)
                } else {
                    msg.err(format!("{done} These failed: {}.", failed.join("; ")))
                }
                load()
            })
        })
    };
    let pct = Signal::derive(move || {
        // A note (id "note_...") is shown but not counted, as the server's score does.
        let r: Vec<Value> = rows.get().into_iter().filter(|x| !x["id"].as_str().unwrap_or("").starts_with("note_")).collect();
        if r.is_empty() {
            0
        } else {
            r.iter().filter(|x| x["severity"] == "ok").count() * 100 / r.len()
        }
    });
    // Problems first, the serious ones (bad) before the minor ones; the server's order within each.
    let problems = move || {
        let mut v: Vec<Value> = rows
            .get()
            .into_iter()
            .filter(|r| r["severity"] != "ok")
            .collect();
        v.sort_by_key(|r| r["severity"] != "bad");
        v
    };
    let passed = Memo::new(move |_| {
        rows.get()
            .into_iter()
            .filter(|r| r["severity"] == "ok")
            .collect::<Vec<_>>()
    });
    // Only the score: the switch already stored the config it saved, and a config read here could
    // land after another switch's save and show that switch in its old state.
    let gs = guild.clone();
    let refresh = Callback::new(move |_| spawn_local(score(gs.clone())));
    // The module cards use the sidebar's config, so the checks follow their switches too.
    let side = expect_context::<modules::Cfg>().0;
    Effect::new(move |first: Option<()>| { side.track(); if first.is_some() { refresh.run(()); } });
    let g = guild.clone();
    let row = move |r: Value| {
        let (g, load) = (g.clone(), load.clone());
        let class = match r["severity"].as_str() {
            Some("ok") => "ok",
            Some("warn") => "warn",
            _ => "err",
        };
        let (id, title, detail) = (s(&r["id"]), s(&r["title"]), s(&r["detail"]));
        let fix = r["fix"]["label"].as_str().map(|l| {
            let (body, g, r2) = (r["fix"].clone(), g.clone(), r.clone());
            let (l, id, title, detail) = (l.to_string(), id.clone(), title.clone(), detail.clone());
            let label = l.clone();
            let me = id.clone();
            // The fix route needs a Trusted Admin, like Roll back on History; Miscellaneous (Remove
            // dangerous roles) needs the owner or an Extra Owner.
            let (bar, why) = if matches!(body["setting"].as_str(), Some("enforce_permits" | "security_profile" | "backups_on")) { (4, "Needs the owner or an Extra Owner") } else { (3, "Needs a Trusted Admin or the owner") };
            view! { <button disabled=move || access.rank() < bar || busy.get().is_some() title=move || (access.rank() < bar).then_some(why) on:click=move |_| {
                let (g, body, load, l, id, title) = (g.clone(), body.clone(), load.clone(), l.clone(), id.clone(), title.clone());
                let enforce = (body["setting"] == "enforce_permits").then(|| (g.clone(), id.clone()));
                let a = Ask::new(format!("{l}?")).text(confirm_text(&r2, &detail)).yes(l.clone());
                let a = if danger(&l) { a.danger() } else { a };
                let go = move || {
                    // Resumed after a 2FA code with this row's own fix, also if the row was drawn again meanwhile.
                    let make = move || {
                        let (g, body, id) = (g.clone(), body.clone(), id.clone());
                        async move {
                            busy.set(Some(id));
                            let r = api("POST", &format!("/api/guilds/{g}/score/fix"), Some(body)).await;
                            busy.set(None);
                            r
                        }
                    };
                    run_resumable(make, msg, need, move |v| { match refused(&v) { None if v["queued"] == true => msg.done("Brim is moving the members now. The result goes to the security log.".to_string()), None => msg.done(format!("Done. {title} has been fixed.")), Some(why) => msg.err(format!("{title} was only partly fixed: {why}.")) } load() })
                };
                // Its confirm lists who loses which roles, read live when clicked.
                if let Some((g, me)) = enforce {
                    crate::form::ask_enforce(g, "Turn on", move |b| { busy.try_set(b.then(|| me.clone())); }, go)
                } else {
                    ask(a, move |_| go())
                } }>{move || if busy.get().as_deref() == Some(me.as_str()) { view! { <Working /> }.into_any() } else { label.clone().into_any() }}</button> }
        });
        let steps: Vec<String> = r["steps"].as_array().into_iter().flatten().map(s).collect();
        // Rows fixed on another dashboard page link there; the rest are done in Discord.
        // Update is for the bot owner only, so that row gets no link (steps only).
        let page = match id.as_str() {
            "whitelist" => Some(Some(format!("#/{g}/permits"))),
            "channel_setup_draft" => Some(Some(format!("#/{g}/channel_setup"))),
            i if i.starts_with("message_webhook_gone_") => Some(Some(format!("#/{g}/embeds/{}", &i["message_webhook_gone_".len()..]))),
            i if i.starts_with("feed_broken_") => Some(Some(format!("#/{g}/feeds"))),
            "sections_unread" => Some(None),
            i if i.starts_with("automod_paused") => Some(Some(format!("#/{g}/discord_automod"))),
            _ => None,
        };
        let manual = (!steps.is_empty()).then(|| view! { <ol class="manual muted">{steps.into_iter().map(|t| view! { <li>{t}</li> }).collect_view()}</ol> });
        let chip = manual.is_some().then(|| match page {
            Some(href) => view! { <span class="chip warn">"On another page"</span>{href.map(|h| view! { <a class="btn" href=h>"Open page"</a> })} }.into_any(),
            None => view! { <span class="chip warn">"In Discord"</span> }.into_any(),
        });
        let icon = match class {
            "ok" => "success",
            "warn" => "warning",
            _ => "error",
        };
        view! { <div class="row note chk" class:bad={class == "err"}><div><b class=class>{crate::icons::icon(icon)}" "{title}</b>" "<span class="muted">{detail}</span>{manual}</div>{(chip.is_some() || fix.is_some()).then(|| view! { <span class="acts">{chip}{fix}</span> })}</div> }
    };
    let (id, g2, g3) = (guild.clone(), guild.clone(), guild.clone());
    let shown = crate::common::eased(problems, |r: &Value| s(&r["id"]));
    view! {
        <div class="card facts"><h2>"Details"</h2>
            {move || guilds.get().into_iter().find(|x| x["id"] == id.as_str()).map(|x| view! {
                <Stat label="Server Name" value=s(&x["name"]) />
                <Stat label="Server ID" value=s(&x["id"]) />
                })}
            {move || (!members.get().is_null()).then(|| view! { <Stat label="Members" value=s(&members.get()) copy=false /> })}
        </div>
        <div class="card"><div class="row"><h2>"Security score"</h2><Ring pct small=true /></div>
        {move || if !setup_done.get() {
            view! { <div class="note setup-first"><b>"Run the setup wizard first"</b><span class="muted">"Most fixes here build on the roles and log channels the wizard sets up."</span><a class="btn go" href=format!("#/{g3}/wizard")>"Open the wizard"</a></div> }.into_any()
        } else {
            let n = todo().len();
            let fix_all = fix_all.clone();
            (n > 0).then(|| view! { <div class="fixall"><button class="go" disabled=move || access.rank() < 3 || busy.get().is_some() on:click=fix_all>{move || if busy.get().as_deref() == Some("*") { view! { <Working /> }.into_any() } else { format!("Fix all ({n})").into_any() }}</button><span class="muted">"Runs every fix below that needs no choice from you, one by one."</span></div> }).into_any()
        }}
        // A fixed check eases out and a new one eases in, so the cards below do not jump.
        <For each=move || shown.get() key=|(r, ..)| s(&r["id"]) children={ let row = row.clone(); move |(r, open, appear)| { let row = row.clone(); view! { <Grow when=open appear>{row(r.clone())}</Grow> } } } />
        <Grow when=Signal::derive(move || passed.with(|p| !p.is_empty()))>
            <details class="passed"><summary>{move || match passed.with(Vec::len) { 1 => "1 check passed".to_string(), n => format!("{n} checks passed") }}</summary>
            <For each=move || passed.get() key=|r| s(&r["id"]) children=row.clone() /></details>
        </Grow></div>
        <h2 class="center">"Quick Systems Overview"</h2>
        <div class="tiles">
            {SYSTEMS.iter().map(|(k, t, d, sw)| view! { <div class="card sys"><h3>{*t}</h3><p>{*d}</p><div class="foot"><a class="btn" href=format!("#/{g2}/{k}")>"Settings"</a>{sw.then(|| view! { <SystemToggle guild=g2.clone() section=*k title=*t all locked=Signal::derive(move || busy.get().is_some()) saved=refresh /> })}</div></div> }).collect_view()}
            {MODULE_CARDS.iter().map(|(m, d)| {
                let (g, cfg) = (g2.clone(), expect_context::<modules::Cfg>().0);
                let sw = move || modules::module_on(&cfg.get(), m).zip(modules::switch_of(m)).map(|(on, (sec, key))| view! { <modules::ModuleSwitch guild=g.clone() section=sec key group=m.to_string() on msg need busy=sw_busy /> });
                view! { <div class="card sys"><h3>{*m}</h3><p>{*d}</p><div class="foot"><a class="btn" href=format!("#/{g2}/module/{}", modules::slug(m))>"Settings"</a>{sw}</div></div> }
            }).collect_view()}
        </div>
        <Toast msg />
    }
}

#[component]
pub(crate) fn Stat(
    label: &'static str,
    value: String,
    #[prop(default = true)] copy: bool,
) -> impl IntoView {
    let copied = RwSignal::new(None::<bool>);
    let (v, show) = (value.clone(), copy);
    let copy = move |_| {
        let v = v.clone();
        spawn_local(async move { copied.set(Some(copy_text(&v).await)) });
    };
    view! { <span class="stat"><span>{label}</span><b>{value}</b>{show.then(|| view! { <button class="ghost copy" on:click=copy>{move || copy_label(copied.get())}</button> })}</span> }
}

#[component]
pub fn Revert(guild: String) -> impl IntoView {
    let (actor, hours) = (
        RwSignal::new(String::new()),
        RwSignal::new("24".to_string()),
    );
    let rows = RwSignal::new(Vec::<Value>::new());
    let picked = RwSignal::new(Vec::<i64>::new());
    let report = RwSignal::new(String::new());
    let (msg, need) = (Msg::default(), step_up());
    let g = guild.clone();
    let load = move |_| {
        let g = g.clone();
        spawn_local(async move {
            let q = format!(
                "/api/guilds/{g}/actions?actor={}&hours={}",
                actor.get_untracked().trim(),
                hours.get_untracked().trim()
            );
            match api("GET", &q, None).await {
                Ok(v) => {
                    msg.clear();
                    picked.set(vec![]);
                    rows.set(v.as_array().cloned().unwrap_or_default())
                }
                Err(e) => msg.err(e),
            }
        })
    };
    // Actions by an Owner, Extra Owner or TA that the server held back; owners may confirm them.
    let held = RwSignal::new(Vec::<Value>::new());
    let can_include = RwSignal::new(false);
    let gs = StoredValue::new(guild.clone());
    let (busy, busy_owners) = (RwSignal::new(false), RwSignal::new(false));
    let send = move |ids: Vec<i64>, owners: bool| {
        let g = gs.get_value();
        run_act(
            guard(if owners { busy_owners } else { busy }, async move {
                api(
                    "POST",
                    &format!("/api/guilds/{g}/revert"),
                    Some(json!({"ids": ids, "owners": owners})),
                )
                .await
            }),
            msg,
            need,
            move |v| {
                let fails: Vec<String> = v["failed"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|f| format!("{}: {}", s(&f["id"]), s(&f["error"])))
                            .collect()
                    })
                    .unwrap_or_default();
                let kept = v["protected"].as_array().cloned().unwrap_or_default();
                report.set(format!(
                    "Done: {}\nFailed: {}\nSkipped: {}\nRe-invite these users manually: {}{}",
                    v["done"],
                    fails.join("; "),
                    v["skipped"],
                    v["reinvite"],
                    if kept.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\nNot reverted, done by an Owner, Extra Owner or Trusted Admin: {}",
                            kept.len()
                        )
                    }
                ));
                can_include.set(v["can_include"].as_bool().unwrap_or(false));
                held.set(kept);
                msg.clear();
            },
        );
    };
    let revert = move |_| {
        let ids = picked.get_untracked();
        if ids.is_empty() {
            return msg.err("Select at least one action");
        }
        send(ids, false)
    };
    let include = move |_| {
        let ids: Vec<i64> = held
            .get_untracked()
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        ask(
            Ask::new(format!(
                "Also revert {} action(s) done by an Owner, Extra Owner or Trusted Admin?",
                ids.len()
            ))
            .yes("Revert"),
            move |_| send(ids, true),
        )
    };
    view! {
        <div class="row">
            {crate::form::one_member(actor)}
            <input placeholder="Hours" aria-label="hours back" prop:value=hours on:input=move |e| hours.set(event_target_value(&e)) />
            <button on:click=load>"Load"</button>
        </div>
        <Toast msg />
        <For each=move || rows.get() key=|r| s(&r["id"]) let:r>
            {
                let id = r["id"].as_i64().unwrap_or(0);
                view! { <div class="card row">
                    <input type="checkbox" aria-label=format!("select action {id}") disabled=r["reverted"].as_bool().unwrap_or(false) on:change=move |e| picked.update(|p| { p.retain(|x| *x != id); if event_target_checked(&e) { p.push(id) } }) />
                    <b>{match s(&r["action"]).as_str() { "bot_role_cleared" => "Restore bot role permissions".into(), a => a.to_string() }}</b>
                    <span class="muted">{format!("target {} at {}{}", s(&r["target_id"]), s(&r["at"]), if r["reverted"].as_bool().unwrap_or(false) { " (reverted)" } else { "" })}</span>
                </div> }
            }
        </For>
        <button disabled=move || busy.get() || busy_owners.get() on:click=revert>{busy_label(move || busy.get(), "Revert selected")}</button>
        <Show when=move || can_include.get() && !held.get().is_empty()>
            <button class="ghost" disabled=move || busy.get() || busy_owners.get() on:click=include>{busy_label(move || busy_owners.get(), "Also revert the Owner / Extra Owner / TA actions")}</button>
        </Show>
        <pre>{move || report.get()}</pre>
    }
}

#[component]
pub fn Rescue(guild: String) -> impl IntoView {
    let has_key = RwSignal::new(false);
    // After an ownership change: the old key expired, or when a new key can be made (its 7 days).
    let (expired, opens) = (RwSignal::new(false), RwSignal::new(None::<String>));
    let no_code = RwSignal::new(false);
    // The server owner account this user took Brim's Owner level from by a rescue claim.
    let paused = RwSignal::new(None::<String>);
    let access_tick = use_context::<Access>().map(|a| a.tick);
    let key = RwSignal::new(String::new());
    let qr = RwSignal::new(String::new());
    let shown = RwSignal::new(false);
    let qr_shown = RwSignal::new(false);
    let key_copied = RwSignal::new(None::<bool>);
    let (msg, need) = (Msg::default(), step_up());
    let (busy, back_busy) = (RwSignal::new(false), RwSignal::new(false));
    let g = guild.clone();
    spawn_local(async move {
        match api("GET", &format!("/api/guilds/{g}/rescue"), None).await {
            Ok(v) => {
                has_key.set(v["has_key"].as_bool().unwrap_or(false));
                expired.set(v["expired"] == true);
                no_code.set(v["no_code"] == true);
                opens.set(v["opens_at"].as_str().map(String::from));
                paused.set(v["paused_owner"].as_str().map(String::from));
            }
            Err(e) => msg.err(e),
        }
    });
    let g = guild.clone();
    let regen = move |_| {
        let g = g.clone();
        let make = move |_: String| {
            run_act(
                guard(busy, async move { api("POST", &format!("/api/guilds/{g}/rescue/regenerate"), None).await }),
                msg,
                need,
                move |v| {
                    key.set(s(&v["key"]));
                    has_key.set(true);
                    expired.set(false);
                    no_code.set(false);
                    // Fetch the page access again, so the new-key prompt does not come back from it.
                    if let Some(t) = access_tick {
                        t.update(|t| *t += 1);
                    }
                    shown.set(false);
                    qr_shown.set(false);
                    qr.set(
                        qrcode::QrCode::new(v["uri"].as_str().unwrap_or(""))
                            .map(|q| {
                                q.render::<qrcode::render::svg::Color>()
                                    .min_dimensions(200, 200)
                                    .build()
                            })
                            .unwrap_or_default(),
                    );
                },
            );
        };
        if has_key.get_untracked() {
            ask(
                Ask::new("Make a new rescue key?")
                    .text("The old key and its authenticator code stop working.")
                    .yes("Make new key")
                    .danger(),
                make,
            )
        } else {
            make(String::new())
        }
    };
    let g = guild.clone();
    let give_back = move |_| {
        let g = g.clone();
        ask(
            Ask::new("Give Brim's Owner level back?")
                .text("The server owner account gets Brim's Owner level back and you lose it. Your rescue key stops working.")
                .yes("Give it back")
                .danger(),
            move |_| {
                let g = g.clone();
                run_act(
                    guard(back_busy, async move { api("POST", &format!("/api/guilds/{g}/rescue/give-back"), None).await }),
                    msg,
                    need,
                    move |_| {
                        web_sys::window().unwrap().location().reload().ok();
                    },
                );
            },
        )
    };
    view! {
        <Show when=move || paused.get().is_some()>
            <div class="card">
                <h2>"Owner level from a rescue"</h2>
                <p class="muted">{move || format!("You hold Brim's Owner level in this server because you used its rescue key. The server owner account (ID {}) has no Brim control here until you give it back. This stays when Discord ownership moves to another account. It ends when you give it back, or when Discord ownership moves to this account.", paused.get().unwrap_or_default())}</p>
                <button class="danger" disabled=move || back_busy.get() on:click=give_back.clone()>{busy_label(move || back_busy.get(), "Give it back")}</button>
            </div>
        </Show>
        <div class="card">
            <p class="muted">{move || if let Some(at) = opens.get() { format!("This server changed owner. The rescue key made before that keeps working until {at}, then it stops working. Nobody can make a new key before then.") } else if no_code.get() { "The rescue key stopped working: it was made before rescue keys needed an authenticator code. Make a new one.".into() } else if expired.get() { "The rescue key stopped working: it was made before this server changed owner, more than 7 days ago. Make a new one.".into() } else if has_key.get() { "A rescue key exists. Regenerating invalidates the old one.".into() } else { "No rescue key yet.".into() }}</p>
            <p class="warn">"If this server changes owner, this rescue key keeps working for 7 days after the change, so whoever holds it can still take Brim's Owner level in that time. Then it stops working, and the new owner makes a new key."</p>
            <button class="warn" disabled=move || busy.get() on:click=regen>{busy_label(move || busy.get(), "Regenerate")}</button>
            <Show when=move || !key.get().is_empty()>
                <p><b>"Rescue Key (shown once, store it somewhere safe):"</b></p>
                <div class="row"><pre class:blur=move || !shown.get()>{move || key.get()}</pre>
                    <button class="danger" on:click=move |_| shown.update(|v| *v = !*v)>{move || if shown.get() { "Hide" } else { "Show" }}</button>
                    <button class="ghost" on:click=move |_| { let k = key.get_untracked(); spawn_local(async move { key_copied.set(Some(copy_text(&k).await)) }) }>{move || copy_label(key_copied.get())}</button></div>
                <p><b>"Scan this with Google Authenticator or Authy too: a rescue needs the key and its 6-digit code. The app shows it as \"Brim Rescue\": never use that code to sign in."</b></p>
                <div class="row"><div class="qr" class:blur=move || !qr_shown.get() inner_html=move || qr.get()></div>
                    <button class="danger" on:click=move |_| qr_shown.update(|v| *v = !*v)>{move || if qr_shown.get() { "Hide QR code" } else { "Show QR code" }}</button></div>
                <ul class="checks warn"><li>"View this rescue key alone!"</li><li>"Never share a screenshot of this QR code."</li><li>"Anyone with this rescue key can get dangerous Brim permissions."</li><li>"This rescue key is used ONLY when you lose physical owner account access."</li><li>"It grants Brim owner permissions, not Discord ownership."</li></ul>
            </Show>
        </div>
        <Toast msg />
    }
}

/// The old rescue key expired after an ownership change: asks the physical owner for a new one on
/// every page of that server, in the 2FA prompt's style (owner decision 11-17). "Later" hides it until the
/// next load.
#[component]
pub fn RescuePrompt(guild: Signal<Option<String>>, page: Signal<Option<String>>) -> impl IntoView {
    let access = expect_context::<Access>();
    let later = RwSignal::new(Vec::<String>::new());
    let open = Signal::derive(move || {
        let Some(g) = guild.get() else { return false };
        page.get().as_deref() != Some("rescue")
            && !later.with(|l| l.contains(&g))
            && access
                .pages
                .with(|a| a["guild"].as_str() == Some(g.as_str()) && a["rescue_prompt"] == true)
    });
    let close = move || {
        if let Some(g) = guild.get_untracked() {
            later.update(|l| l.push(g))
        }
    };
    view! {
        <Fade when=open>
            {move || { let r = NodeRef::new(); focus_on_open(r); let g = guild.get_untracked().unwrap_or_default(); view! {
            <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Make a new rescue key" on:keydown=move |e| dialog_keys(&e, close)>
                <div class="card ask">
                    {crate::icons::icon("rescue_key")}
                    <h2>"Make a new rescue key"</h2>
                    <p class="muted">"This server changed owner more than 7 days ago, so the rescue key made before that stopped working. Make a new one. It needs the code from the Brim entry in your authenticator app."</p>
                    <div class="acts">
                        <button class="ghost" on:click=move |_| close()>"Later"</button>
                        <a class="btn" href=format!("#/{g}/rescue")>"Make a new rescue key"</a>
                    </div>
                </div>
            </div> } }}
        </Fade>
    }
}

/// Anti Nuke "Rule coverage": per action, whether the bot watches it, at which limit, and who is exempt.
#[component]
pub fn NukeCoverage(guild: String) -> impl IntoView {
    let (data, msg) = (RwSignal::new(Value::Null), Msg::default());
    spawn_local(async move {
        let r = api("GET", &format!("/api/guilds/{guild}/antinuke/coverage"), None).await;
        if data.is_disposed() {
            return;
        }
        match r {
            Ok(v) => data.set(v),
            Err(e) => msg.err(e),
        }
    });
    let rows = move || {
        data.with(|d| d["rows"].as_array().cloned().unwrap_or_default()).into_iter().map(|r| {
            let (cls, state, sub) = match r["state"].as_str() {
                Some("on") => ("chip green", "On", ""),
                Some("opt_in") => ("chip orange", "Opt-in", "not set up"),
                _ => ("chip bad", "Off", ""),
            };
            let limit = match (r["per_minute"].as_u64(), r["per_hour"].as_u64()) {
                (Some(m), Some(h)) => format!("{m}/min, {h}/hour"),
                _ => "-".into(),
            };
            let heat = r["heat"].as_u64().map(|h| view! { <br /><small class="muted">{format!("{h}% heat")}</small> });
            let permits: Vec<String> = r["permits"].as_array().into_iter().flatten().map(s).collect();
            let note = s(&r["note"]);
            view! { <tr>
                <td><b>{s(&r["label"])}</b>{(!note.is_empty()).then(|| view! { <br /><small class="muted">{note}</small> })}</td>
                <td data-label="State"><span class=cls>{state}</span>{(!sub.is_empty()).then(|| view! { <br /><small class="muted">{sub}</small> })}</td><td data-label="Limit">{limit}{heat}</td>
                <td data-label="Undone">{match r["undo"].as_str() { Some("yes") => "Yes", Some("backup") => "From backup", _ => "No" }}</td>
                <td data-label="Exempt permits">{if permits.is_empty() { "None".to_string() } else { permits.join(", ") }}</td>
            </tr> }
        }).collect_view()
    };
    view! {
        <div class="card scroll">
            <h3>"Rule coverage"</h3>
            <p class="muted">"The limits anti-nuke counts with the saved settings. The Owner, Extra Owners and Trusted Admins are always exempt; the permits listed are exempt from the count too. A member counts only their highest permit."</p>
            <p class="muted">"Dangerous permissions (a role, a member's role, a channel permission or an added bot) are always guarded, also with Anti Nuke off: only the Owner, Extra Owners and Trusted Admins may give them. No permit or bot is exempt."</p>
            {move || data.with(|d| d["enabled"] == false).then(|| view! { <p class="warn">"Anti Nuke is off: the limits below are not counted until you turn it on. Dangerous permissions and permit roles are still taken back, and nobody is punished."</p> })}
            <table class="data">
                <thead><tr><th>"Action"</th><th>"State"</th><th>"Limit"</th><th>"Undone"</th><th>"Exempt permits"</th></tr></thead>
                <tbody>{rows}</tbody>
            </table>
            <Toast msg />
        </div>
    }
}

/// Owner-only "delete all stored data" for the server, confirmed by typing the server's name.
#[component]
pub fn PurgeData(guild: String) -> impl IntoView {
    let guilds = expect_context::<ReadSignal<Vec<Value>>>();
    let gid = StoredValue::new(guild);
    let name = Memo::new(move |_| {
        guilds
            .get()
            .iter()
            .find(|x| x["id"] == gid.get_value().as_str())
            .map(|x| s(&x["name"]))
            .unwrap_or_default()
    });
    let (open, typed) = (RwSignal::new(false), RwSignal::new(String::new()));
    let (msg, need) = (Msg::default(), step_up());
    let busy = RwSignal::new(false);
    let matches = move || !typed.get().trim().is_empty() && typed.get().trim() == name.get().trim();
    let purge = move |_| {
        let (g, n) = (gid.get_value(), typed.get_untracked());
        open.set(false);
        // The dialog's button is gone once it closes, so a 2FA code resumes this delete itself.
        run_resumable(
            move || {
                let (g, n) = (g.clone(), n.clone());
                guard(busy, async move { api("POST", &format!("/api/guilds/{g}/purge"), Some(json!({ "name": n }))).await })
            },
            msg,
            need,
            move |_| {
                typed.set(String::new());
                msg.ok("All stored data for this server was deleted. Brim now runs on default settings.")
            },
        );
    };
    let r = NodeRef::new();
    focus_on_open(r);
    view! {
        <div class="card">
            <h3>"Delete server data"</h3>
            <p class="muted">"Deletes everything Brim stores for this server: settings, cases, logs, backups, tickets and permits. This cannot be undone. It waits while a lock, panic mode or a quarantine runs, because their saved roles and overwrites are deleted too. Data is also deleted automatically 30 days after Brim is removed."</p>
            <button class="danger" disabled=move || busy.get() on:click=move |_| open.set(true)>{busy_label(move || busy.get(), "Delete all data")}</button>
        </div>
        <Fade when=open>
            <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Delete server data" on:keydown=move |e| dialog_keys(&e, || open.set(false))><div class="card">
                <p>"Type the server name "<b>{name}</b>" to delete all of its stored data."</p>
                <input aria-label="server name" autocomplete="off" prop:value=typed on:input=move |e| typed.set(event_target_value(&e)) />
                <div class="row"><button class="ghost" on:click=move |_| open.set(false)>"Abort"</button><button class="danger" disabled=move || !matches() || busy.get() on:click=purge>{busy_label(move || busy.get(), "Delete")}</button></div>
            </div></div>
        </Fade>
        <Toast msg />
    }
}

/// "Lost control over Brim in a server?": takes Brim's Owner level with the rescue key and
/// its authenticator's 6-digit code. Works from the Servers page, from any account, also one that
/// is not in the server: the answer then holds a one-use invite.
#[component]
pub fn LostControl() -> impl IntoView {
    let (open, guild, secret, code) = (
        RwSignal::new(false),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
    );
    let (msg, need) = (Msg::default(), step_up());
    let invite = RwSignal::new(None::<(String, String)>);
    let busy = RwSignal::new(false);
    let claim = move |_| {
        let g = guild.get_untracked().trim().to_string();
        let body = json!({"guild_id": g, "key": secret.get_untracked().trim(), "code": code.get_untracked().trim()});
        run_act(
            guard(busy, async move { api("POST", "/api/rescue/claim", Some(body)).await }),
            msg,
            need,
            // Opens the server at once (owner decision 11-18); the reload loads the server list with it.
            // Not a member yet: the invite first, the server one click later.
            move |v| {
                let id = g.parse::<i64>().map_or(g.clone(), |n| n.to_string());
                if let Some(link) = v["invite"].as_str() {
                    return invite.set(Some((link.to_string(), id)));
                }
                let l = web_sys::window().unwrap().location();
                let _ = l.set_hash(&format!("/{id}"));
                l.reload().ok();
            },
        );
    };
    view! {
        <button class="link err" class:on=open aria-expanded=move || open.get().to_string() on:click=move |_| open.update(|o| *o = !*o)>"Lost control over Brim in a server?"</button>
        <Show when=move || open.get()>
            <div class="card">
                <h2>"Rescue process"</h2>
                <p class="muted">"You have lost access to your owner account. Sign in with any account, also a new one, and enter the server ID, its rescue key and the 6-digit code from the authenticator you scanned with it. That account then holds Brim's Owner level in the server; Discord ownership does not change."</p>
                <div class="row"><input placeholder="Server ID" aria-label="server ID" spellcheck="false" autocapitalize="none" prop:value=guild on:input=move |e| guild.set(event_target_value(&e)) /><input placeholder="Rescue key" aria-label="rescue key" autocomplete="off" spellcheck="false" autocapitalize="none" prop:value=secret on:input=move |e| secret.set(event_target_value(&e)) /><input placeholder="6-digit code" aria-label="6-digit code" autocomplete="one-time-code" inputmode="numeric" prop:value=code on:input=move |e| code.set(event_target_value(&e)) /><button disabled=move || busy.get() on:click=claim>{busy_label(move || busy.get(), "Confirm")}</button></div>
                <Toast msg />
                {move || invite.get().map(|(link, id)| view! {
                    <p>"Done. This account is not in the server yet: join it with this one-use invite (valid 24 hours), then open the server."</p>
                    <div class="row"><a class="btn" href=link target="_blank" rel="noopener">"Join the server"</a>
                        <button class="ghost" on:click=move |_| { let l = web_sys::window().unwrap().location(); let _ = l.set_hash(&format!("/{id}")); l.reload().ok(); }>"Open the server"</button></div>
                })}
            </div>
        </Show>
    }
}

/// Logging page: Brim and Discord tabs, each with essential and customize sub-tabs.
#[component]
pub fn Logging(guild: String, #[prop(into)] tab: Signal<String>, #[prop(into)] sub: Signal<String>) -> impl IntoView {
    // The address picks the tabs; the page stays while they switch (a new page lost the setup button until it loaded again).
    // Any other tab, also the old `logging/tommybot/...` links from before the rename, opens Brim's.
    let tab = Memo::new(move |_| if tab.get() == "discord" { "discord" } else { "brim" });
    let sub = Memo::new(move |_| if sub.get() == "customize" { "customize" } else { "essential" });
    let (msg, need) = (Msg::default(), step_up());
    // Bumped after a setup so the form below reloads the config the setup changed.
    // A setup rebuilds the form from the server: edits not saved yet would be lost.
    let dirty = RwSignal::new(false);
    let fresh = RwSignal::new(0u32);
    let g = guild.clone();
    let busy = RwSignal::new(false);
    // Optional: private "Logs" and "Staff" categories with a channel per log type that has none yet.
    let setup = move |_| {
        let g = g.clone();
        run_act(
            guard(busy, async move {
                let c = api("GET", &format!("/api/guilds/{g}/config"), None).await?;
                let all: Vec<&str> = OPTIONAL_LOGS.iter().map(|(n, _)| *n).collect();
                api(
                    "POST",
                    &format!("/api/guilds/{g}/logging/setup"),
                    Some(json!({"expected_version": c["version"], "logs": all})),
                )
                .await
            }),
            msg,
            need,
            move |_| {
                msg.ok("Done");
                fresh.update(|n| *n += 1)
            },
        );
    };
    // The button hides while every log type already has a channel: the setup would make nothing.
    let todo = RwSignal::new(false);
    let g3 = guild.clone();
    Effect::new(move |_| {
        fresh.track();
        let g = g3.clone();
        spawn_local(async move {
            let t = api("GET", &format!("/api/guilds/{g}/logging/setup"), None).await;
            todo.try_set(t.is_ok_and(|t| t["todo"] == true));
        });
    });
    view! {
        <div class="row"><Show when=move || todo.get()><button disabled=move || dirty.get() || busy.get() on:click=setup.clone()>{busy_label(move || busy.get(), "Create a channel per log type")}</button><Show when=move || dirty.get()><span class="chip orange">"Save your changes first"</span></Show></Show><Toast msg /></div>
        // Each row's links keep the other row's tab, so a row is built again only when the other row switches.
        {let g = guild.clone(); move || { let sub = sub.get(); view! { <PageTabs tabs=vec![Tab::link("brim", "brim", format!("#/{g}/logging/brim/{sub}")), Tab::link("discord", "discord", format!("#/{g}/logging/discord/{sub}"))] active=Signal::derive(move || tab.get().to_string()) /> } }}
        {let g = guild.clone(); move || { let tab = tab.get(); view! { <PageTabs tabs=vec![Tab::link("essential", "essential", format!("#/{g}/logging/{tab}/essential")), Tab::link("customize", "customize", format!("#/{g}/logging/{tab}/customize"))] active=Signal::derive(move || sub.get().to_string()) /> } }}
        {move || { fresh.track(); let (g, tab, sub) = (guild.clone(), tab.get(), sub.get()); view! {
            {(tab == "brim" && sub != "customize").then(|| view! { <RouteNote guild=g.clone() /> })}
            <Section guild=g name="logging".to_string() part=format!("{tab}_{sub}") dirty />
        } }}
    }
}

/// Note under Brim logs listing the types that have their own channel.
#[component]
fn RouteNote(guild: String) -> impl IntoView {
    let moved = RwSignal::new(None::<Vec<String>>);
    spawn_local(async move {
        if let Ok(v) = api("GET", &format!("/api/guilds/{guild}/config"), None).await {
            let routes = v["config"]["logging"]["routes"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            let mut names: Vec<String> = routes
                .iter()
                .filter(|(k, r)| *k != "mod_command" && !r.is_null())
                .map(|(k, _)| k.replace('_', " "))
                .collect();
            names.sort();
            moved.set(Some(names));
        }
    });
    view! {
        {move || moved.get().filter(|n| !n.is_empty()).map(|n| view! { <details class="card" open=true><summary class="info">"Note"</summary>
            <p>"These log types have their own channel, so they skip the Brim logs channel:"</p>
            <div class="chips">{n.into_iter().map(|t| view! { <span class="chip blue">{t}</span> }).collect_view()}</div>
            <p class="muted">"Everything else, except mod logs, still lands in the Brim logs channel."</p></details> })}
    }
}

/// Catch channel: one click makes the honeypot channel and turns it on, then the settings form.
#[component]
pub fn Catch(guild: String) -> impl IntoView {
    let (msg, need) = (Msg::default(), step_up());
    // Bumped after a setup so the form below reloads the config the setup changed.
    // A setup rebuilds the form from the server: edits not saved yet would be lost.
    let dirty = RwSignal::new(false);
    let fresh = RwSignal::new(0u32);
    let g = guild.clone();
    let busy = RwSignal::new(false);
    let setup = move |_| {
        let sure = "This creates #do-not-write at the top of the server and turns the catch channel on: anyone without a permit who writes there gets the action below. Continue?";
        if !web_sys::window()
            .unwrap()
            .confirm_with_message(sure)
            .unwrap_or(false)
        {
            return;
        }
        let g = g.clone();
        run_act(
            guard(busy, async move {
                let c = api("GET", &format!("/api/guilds/{g}/config"), None).await?;
                api(
                    "POST",
                    &format!("/api/guilds/{g}/catch/setup"),
                    Some(json!({"expected_version": c["version"]})),
                )
                .await
            }),
            msg,
            need,
            move |_| {
                msg.ok("Done");
                fresh.update(|n| *n += 1)
            },
        );
    };
    // The setup button hides once the catch channel is set and still exists in Discord.
    let set_up = RwSignal::new(true);
    let g3 = guild.clone();
    Effect::new(move |_| {
        fresh.track();
        let g = g3.clone();
        spawn_local(async move {
            let (c, d) = (api("GET", &format!("/api/guilds/{g}/config"), None).await, api("GET", &format!("/api/guilds/{g}/discord"), None).await);
            let done = match (c, d) {
                (Ok(c), Ok(d)) => c["config"]["catch"]["channel"].as_str().is_some_and(|id| crate::form::items(&d, "channels").iter().any(|(i, _)| i == id)),
                _ => false,
            };
            set_up.try_set(done);
        });
    });
    // Members caught in this server; a dash until it loads, so the row keeps its size.
    let caught = RwSignal::new("-".to_string());
    let g4 = guild.clone();
    spawn_local(async move {
        if let Ok(v) = api("GET", &format!("/api/guilds/{g4}/catch/count"), None).await {
            caught.try_set(v["count"].to_string());
        }
    });
    view! {
        <div class="row">{move || view! { <Stat label="Caught" value=caught.get() copy=false /> }}<Show when=move || !set_up.get()><button disabled=move || dirty.get() || busy.get() on:click=setup.clone()>{busy_label(move || busy.get(), "Set up channel")}</button><Show when=move || dirty.get()><span class="chip orange">"Save your changes first"</span></Show></Show><Toast msg /></div>
        {move || { fresh.track(); view! { <Section guild=guild.clone() name="catch".to_string() dirty /> } }}
    }
}

/// Tickets settings with a one-click setup that creates the category and the ticket log.
#[component]
pub fn Tickets(guild: String, #[prop(into)] tab: Signal<String>) -> impl IntoView {
    let (msg, need) = (Msg::default(), step_up());
    // Bumped after a setup so the form below reloads the config the setup changed.
    // A setup rebuilds the form from the server: edits not saved yet would be lost.
    let dirty = RwSignal::new(false);
    let fresh = RwSignal::new(0u32);
    // Bumped after a save, so Publish lists the saved panels.
    let saved = RwSignal::new(0u32);
    let (g, g2) = (guild.clone(), guild.clone());
    let busy = RwSignal::new(false);
    let setup = move |_| {
        let g = g.clone();
        run_act(
            guard(busy, async move {
                let c = api("GET", &format!("/api/guilds/{g}/config"), None).await?;
                api(
                    "POST",
                    &format!("/api/guilds/{g}/tickets/setup"),
                    Some(json!({"expected_version": c["version"]})),
                )
                .await
            }),
            msg,
            need,
            move |_| {
                msg.ok("Done");
                fresh.update(|n| *n += 1)
            },
        );
    };
    // The setup button hides once the ticket category and log channel are set and still exist in Discord.
    let set_up = RwSignal::new(true);
    let g3 = guild.clone();
    Effect::new(move |_| {
        fresh.track();
        saved.track();
        let g = g3.clone();
        spawn_local(async move {
            let (c, d) = (api("GET", &format!("/api/guilds/{g}/config"), None).await, api("GET", &format!("/api/guilds/{g}/discord"), None).await);
            let done = match (c, d) {
                (Ok(c), Ok(d)) => ["category", "log_channel"].iter().all(|k| {
                    let key = if *k == "category" { "categories" } else { "channels" };
                    c["config"]["tickets"][k].as_str().is_some_and(|id| crate::form::items(&d, key).iter().any(|(i, _)| i == id))
                }),
                _ => false,
            };
            set_up.try_set(done);
        });
    });
    // The parts stay mounted and are only hidden; the address picks the open one (the hub's cards link to them).
    let tab = Memo::new(move |_| tab.with(|t| if ["publish", "cases", "stats"].contains(&t.as_str()) { t.clone() } else { "settings".into() }));
    let tabs: [(&str, &str); 4] = [("settings", "Settings"), ("publish", "Panels"), ("cases", "Cases"), ("stats", "Stats")];
    let g4 = guild.clone();
    let not = move |k: &'static str| move || tab.with(|t| t != k);
    view! {
        <PageTabs tabs=tabs.map(|(k, t)| Tab::link(k, t, if k == "settings" { format!("#/{g4}/tickets") } else { format!("#/{g4}/tickets/{k}") })).to_vec() active=tab />
        <div class:hide=not("settings")>
        <div class="row"><Show when=move || !set_up.get()><button disabled=move || dirty.get() || busy.get() on:click=setup.clone()>{busy_label(move || busy.get(), "Set up the ticket category and log")}</button><Show when=move || dirty.get()><span class="chip orange">"Save your changes first"</span></Show></Show><Toast msg /></div>
        {move || { fresh.track(); view! { <Section guild=guild.clone() name="tickets".to_string() dirty on_save=Callback::new(move |_| saved.update(|n| *n += 1)) /> } }}
        </div>
        <div class:hide=not("publish")>
        <crate::tickets::Publish guild=g2.clone() saved />
        </div>
        <div class:hide=not("cases")>
        <crate::tickets::Cases guild=g2.clone() open=Signal::derive(move || tab.get() == "cases") />
        </div>
        <div class:hide=not("stats")>
        <crate::tickets::Stats guild=g2 open=Signal::derive(move || tab.get() == "stats") />
        </div>
    }
}

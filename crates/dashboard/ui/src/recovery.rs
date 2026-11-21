use crate::common::*;
use crate::form::Section;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) fn when(secs: i64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(secs as f64 * 1000.0));
    d.to_locale_string("en-GB", &wasm_bindgen::JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_default()
}

/// Runs a queued bot action, then reloads after a few seconds, once the bot has picked the job up.
/// After a 2FA code, `make` sends the same request again (its button may be gone or show the other action by then).
fn queued<F: std::future::Future<Output = Result<Value, String>> + 'static>(
    make: impl Fn() -> F + 'static,
    msg: Msg,
    need: RwSignal<bool>,
    reload: impl Fn() + 'static,
) {
    run_resumable(make, msg, need, move |_| {
        msg.ok("Queued: the bot runs it within a few seconds.");
        set_timeout(reload, Duration::from_secs(6));
    });
}

#[component]
pub fn Backups(guild: String, open: String) -> impl IntoView {
    let access = expect_context::<Access>();
    let rows = RwSignal::new(Vec::<Value>::new());
    let preview = RwSignal::new(None::<(i64, Vec<String>)>);
    let detail = RwSignal::new(None::<(i64, Value)>);
    let (msg, need) = (Msg::default(), step_up());
    let loaded = RwSignal::new(false);
    let load = {
        let g = guild.clone();
        move || {
            let g = g.clone();
            spawn_local(async move {
                match api("GET", &format!("/api/guilds/{g}/backups"), None).await {
                    Ok(v) => rows.set(v.as_array().cloned().unwrap_or_default()),
                    Err(e) => msg.err(e),
                }
                loaded.set(true);
            })
        }
    };
    load();
    let (gc, lc) = (guild.clone(), load.clone());
    let create = move |_| {
        let g = gc.clone();
        queued(
            move || { let g = g.clone(); async move { api("POST", &format!("/api/guilds/{g}/backups"), None).await } },
            msg,
            need,
            lc.clone(),
        )
    };
    let (gp, gl, gd) = (guild.clone(), guild.clone(), guild.clone());
    let show = Callback::new(move |id: i64| {
        let g = gp.clone();
        msg.clear();
        run_act(
            async move {
                api(
                    "GET",
                    &format!("/api/guilds/{g}/backups/{id}/preview"),
                    None,
                )
                .await
            },
            msg,
            need,
            move |v| {
                let lines = v["lines"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|l| l.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                preview.set(Some((id, lines)));
            },
        )
    });
    let gv = guild.clone();
    let fetch = Callback::new(move |(id, code): (i64, String)| {
        let g = gv.clone();
        run_act(
            async move { api("GET", &format!("/api/guilds/{g}/backups/{id}"), None).await },
            msg,
            need,
            move |mut v| {
                v["code"] = code.into();
                detail.set(Some((id, v)))
            },
        )
    });
    // `#/<server>/backups/<code>` is one backup's page (the crumb names it); it opens once the list has loaded.
    // The list shows until then, so a failed or unknown code never leaves an empty page.
    let opened = StoredValue::new(open.is_empty());
    Effect::new(move || {
        if !loaded.get() || opened.get_value() {
            return;
        }
        opened.set_value(true);
        match rows.with(|r| r.iter().find(|r| r["code"].as_str() == Some(open.as_str())).and_then(|r| r["id"].as_i64())) {
            Some(id) => fetch.run((id, open.clone())),
            None => msg.err(format!("There is no backup #{open}.")),
        }
    });
    let gv = guild.clone();
    let view_it = move |code: String| { let _ = web_sys::window().unwrap().location().set_hash(&format!("/{gv}/backups/{code}")); };
    let apply = Callback::new(move |id: i64| {
        let g = gl.clone();
        preview.set(None);
        queued(
            move || { let g = g.clone(); async move { api("POST", &format!("/api/guilds/{g}/backups/{id}/load"), None).await } },
            msg,
            need,
            || (),
        )
    });
    let remove = Callback::new(move |id: i64| {
        let (g, g2, l) = (gd.clone(), gd.clone(), load.clone());
        ask(
            Ask::new("Delete this backup for good?")
                .yes("Delete")
                .danger(),
            move |_| {
                run_act(
                    async move {
                        api(
                            "POST",
                            &format!("/api/guilds/{g}/backups/{id}/delete"),
                            None,
                        )
                        .await
                    },
                    msg,
                    need,
                    move |_| {
                        // Deleted from its own page: back to the list (that page loads it again).
                        if detail.get_untracked().is_some() {
                            let _ = web_sys::window().unwrap().location().set_hash(&format!("/{g2}/backups"));
                        } else {
                            l()
                        }
                    },
                )
            },
        )
    });
    view! {
        <div class:hide=move || detail.with(Option::is_some)>
        // Saving Backups settings needs the owner level (write_bar); below it they are only shown.
        {move || (access.rank() < 4).then(|| view! { <p class="card info" role="note">"Only the owner and Extra Owners can change these settings or create or delete backups."</p> })}
        <fieldset class:ro=move || access.rank() < 4 disabled=move || access.rank() < 4><Section guild=guild.clone() name="backups".to_string() /></fieldset>
        <div class="card">
            <div class="row"><h2>"Backups"</h2><span class="sp"></span><button disabled=move || access.rank() < 4 title=move || (access.rank() < 4).then_some("Needs the owner or an Extra Owner") on:click=create>"Create backup"</button></div>
            <p class="muted">"Brim also saves one on the interval set above; each of those removes the oldest past Max Images. A backup you create never removes one. Loading a backup syncs the server to it: it recreates what is missing, fixes what changed and removes what was added since."</p>
            <For each=move || rows.get() key=|r| r["id"].as_i64() let:r>
                {
                    let id = r["id"].as_i64().unwrap_or(0);
                    view! {
                        <div class="row">
                            <b>{format!("#{}", r["code"].as_str().unwrap_or_default())}</b>
                            <span class="muted">{format!("{} · {} · {} roles · {} channels", if r["by"].is_null() { "Brim".to_string() } else { format!("user {}", r["by"]) }, when(r["at"].as_i64().unwrap_or(0)), r["roles"], r["channels"])}</span>
                            <span class="sp"></span>
                            <button class="ghost" on:click={let (c, v) = (r["code"].as_str().unwrap_or_default().to_string(), view_it.clone()); move |_| v(c.clone())}>"View"</button>
                            <button class="ghost" on:click=move |_| show.run(id)>"Preview & load"</button>
                            <button class="ghost" disabled=move || access.rank() < 4 title=move || (access.rank() < 4).then_some("Needs the owner or an Extra Owner") on:click=move |_| remove.run(id)>"Delete"</button>
                        </div>
                    }
                }
            </For>
            <Show when=move || loaded.get() && rows.get().is_empty()><p class="muted">"No backups yet."</p></Show>
        </div>
        </div>
        {move || detail.get().map(|(id, v)| view! { <BackupView id v load=show remove /> })}
        {move || preview.get().map(|(id, lines)| view! {
            <div class="card">
                <h2>{format!("Load backup #{id}")}</h2>
                {if lines.is_empty() { view! { <p class="info">"The server already matches this backup."</p> }.into_any() } else {
                    view! { <p class="muted">{format!("This would change {} things:", lines.len())}</p><pre>{lines.join("\n")}</pre> }.into_any() }}
                <div class="row">
                    <button class="ghost" on:click=move |_| preview.set(None)>"Cancel"</button>
                    <Show when={let n = lines.len(); move || n > 0}><button on:click=move |_| apply.run(id)>"Apply"</button></Show>
                </div>
            </div>
        })}
        <Toast msg />
    }
}

fn verification_text(n: u64) -> (&'static str, &'static str) {
    match n {
        0 => ("NONE", "unrestricted"),
        1 => ("LOW", "must have a verified email"),
        2 => ("MEDIUM", "must be registered for longer than 5 minutes"),
        3 => ("HIGH", "must be a member for longer than 10 minutes"),
        _ => ("HIGHEST", "must have a verified phone number"),
    }
}

fn filter_text(n: u64) -> (&'static str, &'static str) {
    match n {
        0 => ("DISABLED", "media content will not be scanned"),
        1 => (
            "MEMBERS WITHOUT ROLES",
            "media from members without a role is scanned",
        ),
        _ => ("ALL MEMBERS", "media from all members is scanned"),
    }
}

fn channel_icon(kind: u64) -> AnyView {
    crate::icons::icon(crate::channels::kind_icon(kind))
}

/// One channel in the tree; a click shows its permission overwrites.
fn channel_row(c: &Value, child: bool) -> AnyView {
    let open = RwSignal::new(false);
    let ows: Vec<String> = c["overwrites"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            let p = |k: &str| {
                o[k].as_str()
                    .filter(|p| !p.is_empty())
                    .unwrap_or("none")
                    .to_string()
            };
            format!(
                "{}: allow {} / deny {}",
                o["target"].as_str().unwrap_or(""),
                p("allow"),
                p("deny")
            )
        })
        .collect();
    let cat = c["kind"] == 4;
    let has = !ows.is_empty();
    view! {
        <div class="tr" class:cat=cat class:child=child class:has=has title=if has { "Show permissions" } else { "" } on:click=move |_| open.update(|o| *o = !*o)>
            {channel_icon(c["kind"].as_u64().unwrap_or(0))}" "{c["name"].as_str().unwrap_or("").to_string()}
        </div>
        <Show when=move || has && open.get()>
            <div class="perms" class:child=child>{ows.iter().map(|l| view! { <div>{l.clone()}</div> }).collect_view()}</div>
        </Show>
    }
    .into_any()
}

/// Channels as Discord shows them: loose channels first, then each category with its channels.
/// Takes the backup detail shape (`id`, `parent_id`, `kind`, `name`, `overwrites`), in order.
#[component]
pub fn ChannelTree(channels: Vec<Value>) -> impl IntoView {
    let top = channels
        .iter()
        .filter(|c| c["parent_id"].is_null() && c["kind"] != 4)
        .map(|c| channel_row(c, false))
        .collect_view();
    let cats = channels
        .iter()
        .filter(|c| c["kind"] == 4)
        .map(|c| {
            let kids = channels
                .iter()
                .filter(|k| k["parent_id"] == c["id"] && k["kind"] != 4)
                .map(|k| channel_row(k, true))
                .collect_view();
            view! { {channel_row(c, false)}{kids} }
        })
        .collect_view();
    view! { {top}{cats} }
}

/// One backup as its own page: the channel tree, server settings, webhooks and roles.
#[component]
fn BackupView(
    id: i64,
    v: Value,
    load: Callback<i64>,
    remove: Callback<i64>,
) -> impl IntoView {
    let access = expect_context::<Access>();
    let (show_ch, show_roles) = (RwSignal::new(true), RwSignal::new(true));
    // Remembered per browser, like the motion choice: big servers want it on every time.
    let storage = || web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    let compact = RwSignal::new(
        storage()
            .and_then(|s| s.get_item("backup_compact").ok().flatten())
            .is_some(),
    );
    let toggle_compact = move |_| {
        compact.update(|c| *c = !*c);
        if let Some(s) = storage() {
            let _ = if compact.get_untracked() {
                s.set_item("backup_compact", "1")
            } else {
                s.remove_item("backup_compact")
            };
        }
    };
    let role_open = RwSignal::new(None::<usize>);
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    let (channels, roles, hooks) = (list("channels"), list("roles"), list("webhooks"));
    let st = v["settings"].clone();
    let settings = if st.is_null() {
        view! { <p class="muted">"This backup was taken before server settings were saved."</p> }
            .into_any()
    } else {
        let (vl, vd) = verification_text(st["verification"].as_u64().unwrap_or(0));
        let (fl, fd) = filter_text(st["content_filter"].as_u64().unwrap_or(0));
        view! {
            <div class="set"><b>{crate::icons::icon("server")}" Server Name"</b><div>{st["name"].as_str().unwrap_or("").to_string()}</div></div>
            <div class="set"><b>{crate::icons::icon("verification")}" Verification Level"</b><div>{vl}<small>{vd}</small></div></div>
            <div class="set"><b>{crate::icons::icon("nsfw_filter")}" Explicit Media Content Filter"</b><div>{fl}<small>{fd}</small></div></div>
        }
        .into_any()
    };
    let role_perms = roles.clone();
    view! {
        <div class="bk">
            <div class="bh">
                <span class="sp"></span>
                <button class="go" on:click=move |_| load.run(id)>{crate::icons::icon("restore")}"Load"</button>
                <button class="danger" disabled=move || access.rank() < 4 title=move || (access.rank() < 4).then_some("Needs the owner or an Extra Owner") on:click=move |_| remove.run(id)>{crate::icons::icon("trash")}"Delete"</button>
            </div>
            <div class="cards">
                <div class="col">
                <div class="card">
                    <div class="ttlrow">
                    <button class="ttl" aria-expanded=move || show_ch.get().to_string() on:click=move |_| show_ch.update(|o| *o = !*o)>
                        {crate::icons::icon("channel_text")}" Channels"<span class="sp"></span><span class="chev" class:shut=move || !show_ch.get()>{crate::icons::icon("dropdown")}</span>
                    </button>
                        <button class="ghost compact-btn" class:on=move || compact.get() aria-pressed=move || compact.get().to_string() on:click=toggle_compact>"Compact"</button>
                    </div>
                    <div class="tree" class:compact=move || compact.get() class:hide=move || !show_ch.get()><ChannelTree channels /></div>
                </div>
                </div>
                <div class="col">
                <div class="card">
                    <h3 class="ttl">{crate::icons::icon("info")}" General Settings"</h3>
                    {settings}
                    <h3 class="ttl mt">{crate::icons::icon("webhook")}" Webhooks"</h3>
                    <div class="chips">
                        {hooks.iter().map(|w| view! { <span class="hook" title=format!("#{}", w["channel"].as_str().unwrap_or("?"))>{w["name"].as_str().unwrap_or("").to_string()}</span> }).collect_view()}
                    </div>
                    <Show when={let n = hooks.is_empty(); move || n}><p class="muted">"No webhooks."</p></Show>
                </div>
                </div>
            </div>
            <div class="card">
                <button class="ttl" aria-expanded=move || show_roles.get().to_string() on:click=move |_| show_roles.update(|o| *o = !*o)>
                    {crate::icons::icon("role")}" Roles"<span class="sp"></span><span class="chev" class:shut=move || !show_roles.get()>{crate::icons::icon("dropdown")}</span>
                </button>
                <div class:hide=move || !show_roles.get()>
                    <div class="chips">
                        {roles.iter().enumerate().map(|(i, r)| {
                            let c = r["color"].as_u64().unwrap_or(0);
                            let style = if c == 0 { String::new() } else { format!("--rc:#{c:06x}") };
                            view! {
                                <button class="role" class:on=move || role_open.get() == Some(i) class:tinted=c != 0 style=style title="Show permissions"
                                    on:click=move |_| role_open.update(|o| *o = if *o == Some(i) { None } else { Some(i) })>
                                    <i></i>{r["name"].as_str().unwrap_or("").to_string()}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                    {move || role_open.get().and_then(|i| role_perms.get(i)).map(|r| view! {
                        <p class="perms">{format!("{}: {}", r["name"].as_str().unwrap_or(""), r["perm_names"].as_str().filter(|p| !p.is_empty()).unwrap_or("no permissions"))}</p>
                    })}
                </div>
            </div>
        </div>
    }
}

/// Live state of panic mode with a manual start and end; `part` picks the settings the page shows.
#[component]
pub fn Panic(guild: String, part: &'static str) -> impl IntoView {
    let status = RwSignal::new(json!({}));
    let (msg, need) = (Msg::default(), step_up());
    let load = {
        let g = guild.clone();
        move || {
            let g = g.clone();
            spawn_local(async move {
                if let Ok(v) = api("GET", &format!("/api/guilds/{g}/panic"), None).await {
                    status.set(v)
                }
            })
        }
    };
    load();
    let active = move || status.get()["active"].as_bool().unwrap_or(false);
    let g = guild.clone();
    let toggle = move |_| {
        let (g, what) = (g.clone(), if active() { "end" } else { "start" });
        queued(
            move || { let g = g.clone(); async move { api("POST", &format!("/api/guilds/{g}/panic/{what}"), None).await } },
            msg,
            need,
            load.clone(),
        );
    };
    view! {
        <div class="card">
            <Show when=active><Pose pose=Brim::Alarm class="pose-side" /></Show>
            <h2>"Status"</h2>
            <p>{move || match status.get()["ends_at"].as_i64() {
                Some(t) if active() => format!("Panic mode is on until {}.", when(t)),
                _ => "Panic mode is off.".to_string(),
            }}</p>
            <div class="row"><button on:click=toggle>{move || if active() { "End panic mode now" } else { "Start panic mode now" }}</button></div>
            <Toast msg />
        </div>
        <Section guild=guild.clone() name="panic".to_string() part=part />
    }
}

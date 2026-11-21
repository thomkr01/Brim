//! Feeds page: the RSS and YouTube feeds a server follows, one
//! row each with its on switch and a menu (Check now, Test post, Edit message, Change, Delete).
//! Posts look as if the source posted them; "Send as Brim" is an option, off by default.
//! Owner, Extra Owners and Trusted Admins change feeds; others only look.
//! GitHub and GitLab feeds  get no address: GitHub or GitLab sends each event to Brim,
//! signed with the feed's secret (shown once, in the setup window), and each event type has a switch.
use crate::common::*;
use crate::embeds::Editor;
use crate::form::items;
use crate::icons::icon;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// The feed's name: the owner's own, else the source's, else its address.
fn name_of(f: &Value) -> String {
    [&f["name"], &f["source_name"], &f["target"]]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|n| !n.trim().is_empty())
        .unwrap_or("Feed")
        .to_string()
}

/// The list in the chosen order: by name, by channel name or newest post first.
fn sorted(mut list: Vec<Value>, by: &str, channel: impl Fn(&str) -> String) -> Vec<Value> {
    match by {
        "channel" => list.sort_by_cached_key(|f| {
            (
                channel(&s(&f["channel"])).to_lowercase(),
                name_of(f).to_lowercase(),
            )
        }),
        "post" => list.sort_by_key(|f| std::cmp::Reverse(f["last_post"].as_i64().unwrap_or(0))),
        _ => list.sort_by_cached_key(|f| name_of(f).to_lowercase()),
    }
    list
}

fn ago(t: &Value) -> String {
    match t.as_i64() {
        Some(t) => format!(
            "{} ago",
            crate::discord::relative((js_sys::Date::now() / 1000.0) as i64 - t)
        ),
        None => "never".into(),
    }
}

fn blank() -> Value {
    json!({"id": null, "kind": "auto", "target": "", "channel": "", "ping_role": null, "name": "", "picture": "", "send_as_bot": false,
           "events": ["push", "pull_request", "issues", "release"]})
}

/// GitHub and GitLab feeds: events come in, nothing is fetched.
fn hooked(f: &Value) -> bool {
    f["kind"] == "github" || f["kind"] == "gitlab"
}

/// What a pasted link is: the source a new feed gets when the owner picks none. `None` while empty.
/// YouTube: an @handle, a channel id or a youtube.com page (its own `/feeds/` links are plain RSS).
/// GitHub and GitLab: a link on github.com or gitlab.com. Anything else is an RSS or Atom address.
/// A self-hosted GitLab shows as RSS: the owner then picks GitLab in "More options".
fn guess_kind(input: &str) -> Option<&'static str> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    let rest = t.split_once("://").map_or(t, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let id = t.len() == 24
        && t.starts_with("UC")
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    // A feed file on these hosts (releases.atom, ?format=atom) is plain Atom, not a repository.
    let file = [".atom", ".rss", ".xml", "format=atom"]
        .iter()
        .any(|e| rest.to_ascii_lowercase().contains(e));
    let page = matches!(host, "youtube.com" | "m.youtube.com") && !path.starts_with("feeds/");
    Some(if t.starts_with('@') || id || page {
        "youtube"
    } else if file {
        "rss"
    } else if host == "github.com" {
        "github"
    } else if host == "gitlab.com" {
        "gitlab"
    } else {
        "rss"
    })
}

/// The source a dialog's feed will have: the owner's pick, else the one the link shows.
fn kind_of(v: &Value) -> &'static str {
    match v["kind"].as_str() {
        Some("rss") => "rss",
        Some("youtube") => "youtube",
        Some("github") => "github",
        Some("gitlab") => "gitlab",
        _ => guess_kind(v["target"].as_str().unwrap_or("")).unwrap_or("rss"),
    }
}

/// The event switches of a GitHub or GitLab feed (the dashboard's `feedhooks::TOGGLES`).
const TOGGLES: &[(&str, &str)] = &[
    ("push", "Commits pushed"),
    (
        "pull_request",
        "Pull or merge requests opened, closed, reopened",
    ),
    ("issues", "Issues opened, closed, reopened"),
    ("release", "Releases published"),
    ("create", "Branches and tags made or deleted"),
    ("comments", "Comments on issues, pull requests and commits"),
    (
        "reviews",
        "Pull request reviews and review comments (GitHub)",
    ),
    ("star", "Stars (GitHub)"),
    ("fork", "Forks (GitHub)"),
    ("ci", "Failed checks and pipelines"),
    ("discussions", "Discussions (GitHub)"),
    (
        "members",
        "Collaborator added, repository made public (GitHub)",
    ),
];

/// The entries of a GitHub or GitLab feed, one per event (the Bot messages page's `feed.github.*`).
const EVENTS: &[(&str, &str)] = &[
    ("push", "Commits pushed"),
    ("pull_request", "Pull request"),
    ("issues", "Issue"),
    ("release", "Release published"),
    ("create", "Branch or tag made"),
    ("delete", "Branch or tag deleted"),
    ("issue_comment", "Comment on an issue or pull request"),
    ("commit_comment", "Comment on a commit"),
    ("pull_request_review", "Pull request review"),
    ("pull_request_review_comment", "Pull request review comment"),
    ("watch", "Star added"),
    ("fork", "Fork made"),
    ("check_suite", "Checks failed"),
    ("discussion", "Discussion"),
    ("discussion_comment", "Discussion comment"),
    ("member", "Collaborator added"),
    ("public", "Repository made public"),
];

/// The source's icon and name.
fn source_of(f: &Value) -> (&'static str, &'static str) {
    match f["kind"].as_str() {
        Some("youtube") => ("video_new", "YouTube"),
        Some("github") => ("code", "GitHub"),
        Some("gitlab") => ("code", "GitLab"),
        _ => ("notifications_feed", "RSS"),
    }
}

#[component]
pub fn Feeds(guild: String) -> impl IntoView {
    let g = StoredValue::new(guild);
    let base = move || format!("/api/guilds/{}/feeds", g.get_value());
    let feeds = RwSignal::new(None::<Vec<Value>>);
    let max = RwSignal::new(10usize);
    let dc = RwSignal::new(json!({}));
    let msg = Msg::default();
    let access = expect_context::<Access>();
    let admin = move || access.rank() >= 3;
    let busy = RwSignal::new(false);
    // Which action runs ("add", "<id>:check" ...), so only its button shows the working sign.
    let doing = RwSignal::new(String::new());
    let sort = RwSignal::new("name");
    let dialog = RwSignal::new(None::<RwSignal<Value>>);
    // The open dialog's feed. Made here, not in a click handler: a signal made there belongs to the
    // list, and the list redraws (channel names arrive, a reload) while the dialog still reads it.
    let draft = RwSignal::new(Value::Null);
    let open_dialog = move |v: Value| {
        draft.set(v);
        dialog.set(Some(draft))
    };
    // The setup window of a GitHub or GitLab feed (its secret only right after it was made).
    let setup = RwSignal::new(None::<Value>);
    // The feed whose own message is open in the editor, with its entry from Bot messages.
    let open = RwSignal::new(None::<(Value, Value)>);
    // GitHub and GitLab feeds: the event whose message is open.
    let open_event = RwSignal::new(String::from("push"));
    let cur = RwSignal::new(json!({"body": {}}));
    let orig = RwSignal::new(Value::Null);
    let dirty = Memo::new(move |_| cur.with(|c| c["body"].clone()) != orig.get());
    let tab = RwSignal::new("message");
    let ed = Editor::new(cur, g, true);
    let (message_tab, preview_tab, pop_view) = ed.views();
    on_cleanup(|| crate::msgparts::set_bot_vars(&[]));
    let load = move || {
        spawn_local(async move {
            let r = api("GET", &base(), None).await;
            if feeds.is_disposed() {
                return;
            }
            match r {
                Ok(v) => {
                    max.set(v["max"].as_u64().unwrap_or(10) as usize);
                    feeds.set(Some(v["feeds"].as_array().cloned().unwrap_or_default()))
                }
                Err(e) => msg.err(e),
            }
        })
    };
    load();
    spawn_local(async move {
        if let Ok(v) = api(
            "GET",
            &format!("/api/guilds/{}/discord", g.get_value()),
            None,
        )
        .await
        {
            dc.try_set(v);
        }
    });
    let chan = move |id: &str| crate::form::name_of(&dc.get_untracked(), "channels", id);
    let act = move |path: String, body: Value, key: String, done: &'static str| {
        busy.set(true);
        doing.set(key);
        spawn_local(async move {
            let r = api("POST", &format!("{}/{path}", base()), Some(body)).await;
            // `try_set` hands the value back (Some) only when the page is gone.
            if busy.try_set(false).is_some() {
                return;
            }
            match r {
                Ok(_) => msg.done(done),
                Err(e) => msg.err(e),
            }
            load();
        })
    };
    // `link`: what was pasted, for the setup window's button to the repository's webhook settings.
    let save_dialog = move |d: Value, link: String| {
        let new = d["id"].is_null();
        let path = if new {
            String::new()
        } else {
            format!("/{}", d["id"])
        };
        busy.set(true);
        doing.set("dialog".into());
        spawn_local(async move {
            let r = api("POST", &format!("{}{path}", base()), Some(d)).await;
            if busy.try_set(false).is_some() {
                return;
            }
            match r {
                Ok(v) => {
                    dialog.set(None);
                    if new && hooked(&v) {
                        let mut v = v;
                        v["settings"] = json!(settings_link(&s(&v["kind"]), &link));
                        setup.set(Some(v));
                    } else {
                        msg.done(if new {
                            "Feed added. Brim remembers what is there now and posts only new items."
                        } else {
                            "Saved!"
                        });
                    }
                    load()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let remove = move |f: Value| {
        let id = f["id"].to_string();
        ask(
            Ask::new(format!("Delete the feed “{}”?", name_of(&f)))
                .text("Brim stops posting it. Posts already sent stay in Discord.")
                .yes("Delete")
                .danger(),
            move |_| {
                act(
                    format!("{id}/delete"),
                    json!({}),
                    format!("{id}:delete"),
                    "Feed deleted.",
                )
            },
        )
    };
    // A new secret: the old one stops at once, so GitHub or GitLab needs the new one.
    let new_secret = move |f: Value| {
        let id = f["id"].to_string();
        ask(
            Ask::new("Make a new secret?")
                .text("The old secret stops working at once: events GitHub or GitLab sends with it are refused until you paste the new one in the webhook's settings.")
                .yes("Make new secret")
                .danger(),
            move |_| {
                busy.set(true);
                doing.set(format!("{id}:secret"));
                let id = id.clone();
                spawn_local(async move {
                    let r = api("POST", &format!("{}/{id}/secret", base()), Some(json!({}))).await;
                    if busy.try_set(false).is_some() {
                        return;
                    }
                    match r {
                        Ok(v) => setup.set(Some(v)),
                        Err(e) => msg.err(e),
                    }
                })
            },
        )
    };
    // Opens the feed's own message: its edited version, else the server's, else the default.
    // A GitHub or GitLab feed opens the message of `open_event`.
    let edit_message = move |f: Value| {
        busy.set(true);
        doing.set(format!("{}:message", f["id"]));
        spawn_local(async move {
            let r = api(
                "GET",
                &format!("/api/guilds/{}/bot-messages", g.get_value()),
                None,
            )
            .await;
            if busy.try_set(false).is_some() {
                return;
            }
            let Ok(list) = r else {
                return msg.err(r.err().unwrap_or_default());
            };
            let (key, own) = if hooked(&f) {
                let e = open_event.get_untracked();
                (
                    json!(format!("feed.github.{e}")),
                    f["message"][e.as_str()].clone(),
                )
            } else {
                (f["entry"].clone(), f["message"].clone())
            };
            let Some(row) = list
                .as_array()
                .and_then(|l| l.iter().find(|r| r["key"] == key).cloned())
            else {
                return msg.err("This feed's entry was not found on the Bot messages page.");
            };
            let vars: Vec<(String, String, String)> = row["vars"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| (s(&v["name"]), s(&v["label"]), s(&v["example"])))
                .collect();
            crate::msgparts::set_bot_vars(&vars);
            let body = [&own, &row["message"]]
                .into_iter()
                .find(|m| m.is_object())
                .cloned()
                .unwrap_or_else(|| row["default"].clone());
            orig.set(body.clone());
            cur.set(json!({"body": body}));
            ed.reset();
            tab.set(if admin() { "message" } else { "preview" });
            open.set(Some((f, row)));
        })
    };
    let close_editor = move || {
        crate::msgparts::set_bot_vars(&[]);
        open.set(None);
        load();
    };
    let save_message = move |id: String, m: Value, done: &'static str| {
        busy.set(true);
        doing.set("save".into());
        let event = open_event.get_untracked();
        spawn_local(async move {
            let r = api(
                "POST",
                &format!("{}/{id}/message", base()),
                Some(json!({"message": m, "event": event})),
            )
            .await;
            if busy.try_set(false).is_some() {
                return;
            }
            match r {
                Ok(v) => {
                    open.update(|o| {
                        if let Some(o) = o {
                            o.0 = v
                        }
                    });
                    orig.set(cur.with_untracked(|c| c["body"].clone()));
                    msg.done(done)
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let row_view = move |f: Value| {
        let id = f["id"].to_string();
        let state = s(&f["state"]);
        let on = state == "on";
        let (f1, f2, f3, f4) = (f.clone(), f.clone(), f.clone(), f.clone());
        let (f5, f6) = (f.clone(), f.clone());
        let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
        let source = source_of(&f);
        let hook = hooked(&f);
        let mut line = vec![
            format!("#{}", chan(&s(&f["channel"])).trim_start_matches('#')),
            format!("last post {}", ago(&f["last_post"])),
        ];
        if hook && f["connected"] != true {
            line.push(format!("waiting for the first event from {}", source.1));
        }
        if f["send_as_bot"] == true {
            line.push("sends as Brim".into());
        }
        if f["message"].as_object().is_some_and(|m| !m.is_empty()) {
            line.push("own message".into());
        }
        let chip = match state.as_str() {
            "broken" => Some(view! { <span class="chip bad">"Stopped"</span> }),
            "paused" => Some(view! { <span class="chip">"Paused"</span> }),
            _ => None,
        };
        let asked = (!f["asked"].is_null()).then(|| view! { <div class="muted small">"Asked: Brim does it within 30 seconds."</div> });
        let error = f["last_error"]
            .as_str()
            .map(|e| view! { <div class="err small">{format!("Last check: {e}")}</div> });
        let toggle = move |e: leptos::ev::Event| {
            let want = event_target_checked(&e);
            act(
                format!("{i1}/state"),
                json!({"on": want}),
                format!("{i1}:state"),
                if want {
                    "Feed on. Brim checks it now."
                } else {
                    "Feed paused."
                },
            );
        };
        let menu_btn = move |label: &'static str, run: Box<dyn Fn() + Send + Sync>| view! { <button disabled=move || busy.get() on:click=move |e| { close_menu(&e); run() }>{label}</button> };
        view! {
            <li class="frow" class:off=!on>
                <input type="checkbox" class="sw" role="switch" aria-label=format!("{} on", name_of(&f)) prop:checked=on disabled=move || busy.get() || !admin() on:change=toggle />
                <span class="ico" title=source.1>{icon(source.0)}</span>
                <div class="fmain"><b>{name_of(&f)}</b>{chip}
                    <div class="muted small">{source.1}" · "{line.join(" · ")}</div>
                    {asked}{error}
                </div>
                <span class="sp"></span>
                {move || (busy.get() && doing.get().starts_with(&format!("{i3}:"))).then(|| view! { <Working /> })}
                {admin().then(|| view! {
                    <details class="rowmenu"><summary aria-label=format!("actions for {}", name_of(&f4))>{icon("more")}</summary><div>
                        {(state != "paused" && !hook).then(|| menu_btn("Check now", Box::new(move || act(format!("{i2}/ask"), json!({"what": "check"}), format!("{i2}:ask"), "Asked. Brim checks the feed within 30 seconds."))))}
                        {(hook && state == "broken").then(|| { let i = id.clone(); menu_btn("Turn on again", Box::new(move || act(format!("{i}/ask"), json!({"what": "check"}), format!("{i}:ask"), "Feed on again. New events post from now."))) })}
                        {(!hook).then(|| menu_btn("Test post", { let i = id.clone(); Box::new(move || act(format!("{i}/ask"), json!({"what": "test"}), format!("{i}:ask"), "Asked. Brim posts the newest item within 30 seconds.")) }))}
                        {hook.then(|| menu_btn("Show setup", Box::new(move || setup.set(Some(f5.clone())))))}
                        {hook.then(|| menu_btn("Make new secret", Box::new(move || new_secret(f6.clone()))))}
                        {menu_btn("Edit message", Box::new(move || edit_message(f1.clone())))}
                        {menu_btn("Change", Box::new(move || open_dialog(f2.clone())))}
                        {menu_btn("Delete", Box::new(move || remove(f3.clone())))}
                    </div></details>
                })}
            </li>
        }
    };
    let list = move || {
        let Some(all) = feeds.get() else {
            return view! { <p><Working /></p> }.into_any();
        };
        // Channel names can arrive after the feeds: redraw then.
        dc.track();
        let n = all.len();
        let off = move || busy.get() || n >= max.get();
        let add_btn = move || {
            admin().then(|| view! {
            <AddButton label="Add new feed" attr:disabled=off attr:title=move || (n >= max.get()).then_some("This server has the most feeds it can have: delete one first") on:click=move |_| open_dialog(blank()) />
        })
        };
        if all.is_empty() {
            return view! { <div class="card empty">
                <h3>"No feeds yet"</h3>
                <p class="muted">"Follow a website, a YouTube channel, or a GitHub or GitLab repository: paste its address. New posts appear in the channel you pick, as if the source posted them."</p>
                {add_btn}
                {(!admin()).then(|| view! { <p class="muted small">"Only the Owner, Extra Owners and Trusted Admins add feeds."</p> })}
            </div> }.into_any();
        }
        let rows = sorted(all, sort.get(), chan);
        view! {
            <div class="row fhead">
                <span class="count">{format!("{n} / {} feeds", max.get())}</span>
                <span class="muted small">"At most 3 per channel."</span>
                <span class="sp"></span>
                <div class="seg" role="group" aria-label="sort feeds">
                    {[("name", "Name"), ("channel", "Channel"), ("post", "Last post")].map(|(k, l)| view! { <button class:on=move || sort.get() == k on:click=move |_| sort.set(k)>{l}</button> }).collect_view()}
                </div>
                {add_btn}
            </div>
            <ul class="feeds-list">{rows.into_iter().map(row_view).collect_view()}</ul>
            {(!admin()).then(|| view! { <p class="muted small">"Only the Owner, Extra Owners and Trusted Admins change feeds."</p> })}
        }.into_any()
    };
    let dialog_view = move || {
        dialog.get().map(|d| {
            let new = d.with_untracked(|v| v["id"].is_null());
            let close = move || dialog.set(None);
            let text = move |k: &'static str| move |e: leptos::ev::Event| d.update(|v| v[k] = json!(event_target_value(&e)));
            let kind = move || d.with(kind_of);
            let hook = move || matches!(kind(), "github" | "gitlab");
            // What is missing, said on the disabled button's tooltip: the form itself stays calm.
            let why = move || d.with(|v| {
                if new && !hook() && s(&v["target"]).trim().is_empty() {
                    Some("Paste a link first.")
                } else if s(&v["channel"]).is_empty() {
                    Some("Pick a channel.")
                } else {
                    None
                }
            });
            // One plain line under the link: what Brim took it for.
            let seen = move || d.with(|v| match (kind(), s(&v["target"]).trim().is_empty() && v["kind"] == "auto") {
                (_, true) => "A website, a YouTube channel, or a GitHub or GitLab repository: open it in your browser and copy the address bar.",
                ("youtube", _) => "A YouTube channel. New videos are posted.",
                ("github", _) => "A GitHub repository. Brim gives you an address to paste into its webhook settings.",
                ("gitlab", _) => "A GitLab project. Brim gives you an address to paste into its webhook settings.",
                _ => "A website. Brim finds its feed (RSS or Atom) and posts new posts. A self-hosted GitLab? Pick GitLab under More options.",
            });
            let save = move |_| {
                let mut v = d.get_untracked();
                let link = s(&v["target"]);
                v["kind"] = json!(kind_of(&v));
                if matches!(kind_of(&v), "github" | "gitlab") {
                    v["target"] = json!("");
                }
                save_dialog(v, link)
            };
            let r = NodeRef::<leptos::html::Div>::new();
            focus_on_open(r);
            view! {
                <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label=if new { "Add new feed" } else { "Change feed" } on:keydown=move |e| dialog_keys(&e, close)><div class="card feeds-dialog">
                    <h2>{if new { "Add new feed".to_string() } else { format!("Change “{}”", d.with_untracked(name_of)) }}</h2>
                    {new.then(|| view! {
                        <div class="row"><label for="feed-target">"Link"</label>
                            <input id="feed-target" autocomplete="off" prop:value=move || d.with(|v| s(&v["target"])) on:input=text("target")
                                placeholder="Paste a website, YouTube or repository link" /></div>
                        <p class="muted small hint">{seen}</p>
                    })}
                    <div class="row"><label for="feed-channel">"Channel"</label>
                        <select id="feed-channel" on:change=text("channel")>
                            <option value="" prop:selected=move || d.with(|v| s(&v["channel"]).is_empty())>"choose a channel"</option>
                            {move || items(&dc.get(), "channels").into_iter().map(|(id, l)| { let i = id.clone(); view! { <option value=id prop:selected=move || d.with(|v| s(&v["channel"]) == i)>{l}</option> } }).collect_view()}
                        </select></div>
                    <div class="row"><label for="feed-role">"Ping role"</label>
                        <select id="feed-role" on:change=move |e| { let x = event_target_value(&e); d.update(|v| v["ping_role"] = if x.is_empty() { Value::Null } else { json!(x) }) }>
                            <option value="" prop:selected=move || d.with(|v| v["ping_role"].is_null())>"no ping"</option>
                            {move || { let gid = g.get_value(); items(&dc.get(), "roles").into_iter().filter(|(id, _)| *id != gid).map(|(id, l)| { let i = id.clone(); view! { <option value=id prop:selected=move || d.with(|v| s(&v["ping_role"]) == i)>{l}</option> } }).collect_view() }}
                        </select></div>
                    <p class="muted small hint">"Only this role is pinged. A feed never pings @everyone, @here or members."</p>
                    <details class="more"><summary>"More options"</summary>
                        {new.then(|| view! {
                            <div class="row"><label for="feed-source">"Source"</label>
                                <select id="feed-source" on:change=text("kind")>
                                    {[("auto", "Detect from the link"), ("rss", "RSS or Atom"), ("youtube", "YouTube"), ("github", "GitHub"), ("gitlab", "GitLab (also self-hosted)")].map(|(k, l)| view! { <option value=k prop:selected=move || d.with(|v| v["kind"] == k)>{l}</option> }).collect_view()}
                                </select></div>
                        })}
                        <div class="row"><label for="feed-name">"Name"</label>
                            <input id="feed-name" maxlength="80" prop:value=move || d.with(|v| s(&v["name"])) on:input=text("name") placeholder="the source's own name" /></div>
                        <div class="row"><label for="feed-pic">"Picture link"</label>
                            <input id="feed-pic" prop:value=move || d.with(|v| s(&v["picture"])) on:input=text("picture") placeholder="https://..." /></div>
                        <p class="muted small hint">"Right-click a picture on a web page, pick Copy image address, and paste it here. Empty: the source's picture."</p>
                        <div class="row"><label for="feed-bot">"Send as Brim"</label>
                            <input id="feed-bot" type="checkbox" class="sw" role="switch" prop:checked=move || d.with(|v| v["send_as_bot"] == true) on:change=move |e| { let c = event_target_checked(&e); d.update(|v| v["send_as_bot"] = json!(c)) } /></div>
                        <p class="muted small hint">"Off: posts carry the source's name and picture. On: Brim posts them as itself."</p>
                        <Show when=hook>
                            <fieldset class="events"><legend>"Post these events"</legend>
                                {TOGGLES.iter().map(|&(k, l)| view! {
                                    <label class="row"><input type="checkbox" class="sw" role="switch"
                                        prop:checked=move || d.with(|v| v["events"].as_array().is_some_and(|a| a.iter().any(|e| e == k)))
                                        on:change=move |e| { let on = event_target_checked(&e); d.update(|v| {
                                            let mut list: Vec<Value> = v["events"].as_array().cloned().unwrap_or_default();
                                            list.retain(|x| x != k);
                                            if on { list.push(json!(k)); }
                                            v["events"] = Value::Array(list);
                                        }) } />{l}</label>
                                }).collect_view()}
                            </fieldset>
                        </Show>
                    </details>
                    <div class="acts">
                        <button class="ghost" on:click=move |_| close()>"Cancel"</button>
                        <button disabled=move || why().is_some() || busy.get() title=move || why().unwrap_or("") on:click=save>{busy_label(move || busy.get() && doing.get() == "dialog", if new { "Add feed" } else { "Save" })}</button>
                    </div>
                </div></div>
            }
        })
    };
    let editor = move || {
        let (f, row) = open.get()?;
        let id = f["id"].to_string();
        let can = admin();
        let (i1, i2) = (id.clone(), id.clone());
        let r3 = row.clone();
        let r4 = r3.clone();
        let gone = move || cur.with(|c| !crate::botmsgs::missing(&r4, &c["body"]).is_empty());
        let own = if hooked(&f) {
            f["message"][open_event.get_untracked().as_str()].is_object()
        } else {
            f["message"].is_object()
        };
        Some(view! {
            <div class="mbhead">
                <b class="name">{format!("Message of “{}”", name_of(&f))}</b>
                <span class="states">{if own { view! { <span class="chip info">"Own version"</span> }.into_any() } else { view! { <span class="chip">"Server's version"</span> }.into_any() }}</span>
                <span class="sp"></span>
                <button class="ghost" on:click=move |_| { if dirty.get_untracked() { ask(Ask::new("Leave without saving?").text("The changes since the last save are lost.").yes("Leave").danger(), move |_| close_editor()) } else { close_editor() } }>"Close"</button>
                {(can && own).then(|| view! { <button class="danger" disabled=move || busy.get() on:click=move |_| { let d = r3_default(&row); cur.set(json!({"body": d})); save_message(i1.clone(), Value::Null, "This feed uses the server's version again.") }>"Use the server's version"</button> })}
            </div>
            {hooked(&f).then(|| { let f7 = f.clone(); view! {
                <div class="row"><label for="feed-event">"Event"</label>
                    <select id="feed-event" disabled=move || dirty.get() || busy.get() on:change=move |e| { open_event.set(event_target_value(&e)); edit_message(f7.clone()) }>
                        {EVENTS.iter().map(|&(k, l)| view! { <option value=k prop:selected=move || open_event.get() == k>{l}</option> }).collect_view()}
                    </select>
                    <Tip text="Each event has its own message. Save or cancel your changes before you pick another event." icon="info" /></div>
            } })}
            <p class="muted small">"Only this feed sends this version. The entry on the Bot messages page is the version for every other feed of this kind. Edits apply to posts sent after the save."</p>
            {can.then(|| view! {
                <PageTabs tabs=vec![Tab::state("message", "Message").icon("edit"), Tab::state("preview", "Preview").icon("eye")]
                    active=Signal::derive(move || tab.get().to_string())
                    on_pick=Callback::new(move |k| { ed.pop.set(None); ed.parts.sub.set(None); ed.answer.set(None); tab.set(k) }) />
            })}
            {move || gone().then(|| view! { <p class="err">"{link} is missing: keep {link} in the message text or the first embed, or this cannot be saved."</p> })}
            {move || if tab.get() == "preview" || !can { preview_tab() } else { message_tab() }}
            {pop_view}
            {crate::msgparts::sub_view(ed.parts)}
            <Show when=move || can && dirty.get()>
                <div class="bar"><b>"Unsaved changes"</b>
                    <button class="ghost" on:click=move |_| { let o = orig.get_untracked(); cur.update(|c| c["body"] = o); ed.reset() }>"Cancel"</button>
                    <button disabled=move || busy.get() on:click={ let i = i2.clone(); move |_| save_message(i.clone(), cur.with_untracked(|c| c["body"].clone()), "Saved!") }>{busy_label(move || busy.get() && doing.get() == "save", "Save")}</button></div>
            </Show>
        })
    };
    let setup_view = move || {
        setup.get().map(|f| {
            let close = move || setup.set(None);
            let gitlab = f["kind"] == "gitlab";
            let url = s(&f["hook_url"]);
            let secret = f["secret"].as_str().map(String::from);
            let r = NodeRef::<leptos::html::Div>::new();
            focus_on_open(r);
            let copied_url = RwSignal::new(None::<bool>);
            let copied_secret = RwSignal::new(None::<bool>);
            let shown = RwSignal::new(false);
            let u = url.clone();
            let steps: &[&str] = if gitlab {
                &["In GitLab, open the project, then Settings > Webhooks > Add new webhook.",
                  "Paste the address as URL and the secret as Secret token.",
                  "Tick every trigger you may want (Brim posts only the events switched on here), then Add webhook."]
            } else {
                &["In GitHub, open the repository, then Settings > Webhooks > Add webhook.",
                  "Paste the address as Payload URL, pick Content type application/json, and paste the secret as Secret.",
                  "Pick \"Send me everything\" (Brim posts only the events switched on here), then Add webhook."]
            };
            view! {
                <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Feed setup" on:keydown=move |e| dialog_keys(&e, close)><div class="card feeds-dialog">
                    <h2>{if gitlab { "Connect GitLab" } else { "Connect GitHub" }}</h2>
                    {f["settings"].as_str().map(|u| view! { <p><a class="btn" href=u.to_string() target="_blank" rel="noopener noreferrer">{if gitlab { "Open GitLab's webhook settings" } else { "Open GitHub's webhook settings" }}</a></p> })}
                    <ol>{steps.iter().map(|t| view! { <li>{*t}</li> }).collect_view()}</ol>
                    <div class="row"><label>"Address"</label><code class="grow">{url.clone()}</code>
                        <button class="ghost" on:click=move |_| { let t = u.clone(); spawn_local(async move { copied_url.set(Some(copy_text(&t).await)) }) }>{move || copy_label(copied_url.get())}</button></div>
                    {match secret {
                        Some(sec) => { let s2 = sec.clone(); view! {
                            <div class="row"><label>"Secret"</label><code class="grow" class:blur=move || !shown.get()>{sec}</code>
                                <button class="ghost" on:click=move |_| shown.update(|v| *v = !*v)>{move || if shown.get() { "Hide" } else { "Show" }}</button>
                                <button class="ghost" on:click=move |_| { let t = s2.clone(); spawn_local(async move { copied_secret.set(Some(copy_text(&t).await)) }) }>{move || copy_label(copied_secret.get())}</button></div>
                            <p class="muted small">"The secret is shown only now. Lost it? Use Make new secret in the feed's menu."</p>
                        } }.into_any(),
                        None => view! { <p class="muted small">"The secret was shown once, when it was made. Lost it? Use Make new secret in the feed's menu."</p> }.into_any(),
                    }}
                    <p class="muted small">{format!("When the first event arrives the feed shows as connected. If {} reports 404, the secret or the address is wrong, or the feed is paused.", if gitlab { "GitLab" } else { "GitHub" })}</p>
                    <div class="row"><span class="sp"></span><button on:click=move |_| close()>"Done"</button></div>
                </div></div>
            }
        })
    };
    view! {
        <div class="feeds">
            {move || if open.with(Option::is_some) { editor().into_any() } else { list().into_any() }}
        </div>
        {dialog_view}
        {setup_view}
        <Toast msg />
    }
}

/// The page where a GitHub repository or GitLab project adds a webhook, from the pasted link:
/// `https://github.com/<owner>/<repo>/settings/hooks/new` or `https://<host>/<path>/-/hooks`.
/// `None` when the link names no repository.
fn settings_link(kind: &str, link: &str) -> Option<String> {
    let t = link.trim().trim_end_matches('/');
    let rest = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(t);
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let ok = |p: &&str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    // GitLab's own pages under a project start at "/-/"; GitHub's after owner/repo.
    let mut parts: Vec<&str> = path.split(['?', '#']).next()?.split("/-/").next()?.split('/').filter(|p| !p.is_empty()).collect();
    if let Some(last) = parts.last_mut() {
        *last = last.strip_suffix(".git").unwrap_or(last);
    }
    match kind {
        "github" if matches!(host.as_str(), "github.com" | "www.github.com") && parts.len() >= 2 && parts[..2].iter().all(ok) => {
            Some(format!("https://github.com/{}/{}/settings/hooks/new", parts[0], parts[1]))
        }
        "gitlab" if parts.len() >= 2 && parts.iter().all(ok) && host.contains('.') && host.chars().all(|c| c.is_ascii_alphanumeric() || "-.".contains(c)) => {
            Some(format!("https://{host}/{}/-/hooks", parts.join("/")))
        }
        _ => None,
    }
}

/// The server's version of the entry, else its default (what the editor shows after a revert).
fn r3_default(row: &Value) -> Value {
    if row["message"].is_object() {
        row["message"].clone()
    } else {
        row["default"].clone()
    }
}

/// Closes the ⋮ menu a click came from.
fn close_menu(e: &leptos::ev::MouseEvent) {
    use wasm_bindgen::JsCast;
    if let Some(d) = e
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|el| el.closest("details").ok().flatten())
    {
        let _ = d.remove_attribute("open");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_shows_the_source() {
        let k = guess_kind;
        assert_eq!(k("  "), None);
        for y in [
            "@mkbhd",
            "UCBJycsmduvYEL83R_U4JriQ",
            "https://www.youtube.com/@mkbhd",
            "youtube.com/channel/UCBJycsmduvYEL83R_U4JriQ",
        ] {
            assert_eq!(k(y), Some("youtube"), "{y}");
        }
        assert_eq!(
            k("https://www.youtube.com/feeds/videos.xml?channel_id=UCBJycsmduvYEL83R_U4JriQ"),
            Some("rss")
        );
        assert_eq!(k("https://github.com/rust-lang/rust"), Some("github"));
        assert_eq!(k("https://gitlab.com/a/b"), Some("gitlab"));
        assert_eq!(
            k("https://github.com/rust-lang/rust/releases.atom"),
            Some("rss")
        );
        assert_eq!(k("https://gitlab.com/a/b/-/tags?format=atom"), Some("rss"));
        assert_eq!(k("https://gitlab.example.org/a/b"), Some("rss"));
        assert_eq!(k("https://example.com/feed.xml"), Some("rss"));
        assert_eq!(k("https://evil.com/github.com/x"), Some("rss"));
        assert_eq!(
            kind_of(&json!({"kind": "gitlab", "target": "https://example.com/f"})),
            "gitlab"
        );
        assert_eq!(
            kind_of(&json!({"kind": "auto", "target": "@abc"})),
            "youtube"
        );
    }

    #[test]
    fn webhook_settings_links() {
        let l = settings_link;
        assert_eq!(l("github", "https://github.com/rust-lang/rust/tree/main/src").as_deref(), Some("https://github.com/rust-lang/rust/settings/hooks/new"));
        assert_eq!(l("github", "github.com/a/b").as_deref(), Some("https://github.com/a/b/settings/hooks/new"));
        assert_eq!(l("github", "https://github.com/a"), None);
        assert_eq!(l("github", "https://www.github.com/a/b.git").as_deref(), Some("https://github.com/a/b/settings/hooks/new"));
        assert_eq!(l("github", "https://evil.com/a/b"), None);
        assert_eq!(l("gitlab", "https://gitlab.com/grp/sub/proj/-/issues?x=1").as_deref(), Some("https://gitlab.com/grp/sub/proj/-/hooks"));
        assert_eq!(l("gitlab", "https://git.example.org/a/b/").as_deref(), Some("https://git.example.org/a/b/-/hooks"));
        assert_eq!(l("gitlab", "https://git.example.org/a/b\"x"), None);
        assert_eq!(l("gitlab", ""), None);
    }

    #[test]
    fn names_and_order() {
        let a = json!({"name": "", "source_name": "Zed", "target": "https://z", "channel": "2", "last_post": 5});
        let b = json!({"name": "alpha", "source_name": "x", "target": "https://a", "channel": "1", "last_post": 9});
        let c = json!({"name": null, "source_name": "", "target": "UCabc", "channel": "1"});
        assert_eq!(name_of(&a), "Zed");
        assert_eq!(name_of(&c), "UCabc");
        let ch = |id: &str| {
            if id == "1" {
                "#news".to_string()
            } else {
                "#blog".to_string()
            }
        };
        let names = |l: Vec<Value>| l.iter().map(name_of).collect::<Vec<_>>();
        assert_eq!(
            names(sorted(vec![a.clone(), b.clone(), c.clone()], "name", ch)),
            ["alpha", "UCabc", "Zed"]
        );
        assert_eq!(
            names(sorted(vec![a.clone(), b.clone(), c.clone()], "channel", ch)),
            ["Zed", "alpha", "UCabc"]
        );
        assert_eq!(
            names(sorted(vec![a, b, c], "post", ch)),
            ["alpha", "Zed", "UCabc"]
        );
    }
}

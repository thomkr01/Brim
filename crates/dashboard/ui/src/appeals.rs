use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// Staff view: filter, search, accept or deny.
#[component]
pub fn AppealList(guild: String) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    let (kind, q, msg) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        Msg::default(),
    );
    let loaded = RwSignal::new(false);
    let (g, page) = (StoredValue::new(guild), RwSignal::new(0i64));
    let list = move || {
        format!(
            "/api/guilds/{}/appeals?kind={}&q={}&page=",
            g.get_value(),
            kind.get_untracked(),
            js_sys::encode_uri_component(&q.get_untracked())
        )
    };
    let seq = StoredValue::new(0u64);
    let load = move || {
        if g.is_disposed() {
            return;
        }
        seq.update_value(|n| *n += 1);
        let mine = seq.get_value();
        spawn_local(async move {
            let path = format!("{}{}", list(), page.get_untracked());
            let r = api("GET", &path, None).await;
            // A newer filter or page was asked for meanwhile, or the page left.
            if seq.try_get_value() != Some(mine) {
                return;
            }
            match r {
                Ok(v) => {
                    msg.clear();
                    set_rows.set(v.as_array().cloned().unwrap_or_default())
                }
                // A failed load is no empty list: only the error shows.
                Err(e) => return msg.err(e),
            }
            loaded.set(true);
        })
    };
    let first = move || {
        page.set(0);
        load()
    };
    load();
    let reply = RwSignal::new(String::new());
    // The appeal and button being decided: every decision button is off meanwhile, so a double click decides once.
    let busy = RwSignal::new((0i64, ""));
    let decide = move |id: i64, key: &'static str, accept: bool, blacklist: bool, dismiss: bool| {
        busy.set((id, key));
        spawn_local(async move {
            let path = format!("/api/guilds/{}/appeals/{id}/decide", g.get_value());
            let body = json!({"accept": accept, "blacklist": blacklist, "dismiss": dismiss, "reply": reply.get_untracked()});
            let r = api("POST", &path, Some(body)).await;
            busy.try_set((0, ""));
            match r {
                // An accept runs in the bot (its rate limit); the appeal closes once it is done.
                Ok(v) if v["queued"] == true => {
                    msg.ok("Sent to the bot. The appeal closes when the punishment is lifted; a refusal is posted in the appeal log.");
                    load()
                }
                Ok(_) => load(),
                Err(e) => return msg.err(e),
            }
            // The reply was for this member; the next decision starts empty.
            reply.try_set(String::new());
        })
    };
    let ask = move |id: i64| {
        busy.set((id, "ask"));
        spawn_local(async move {
            let path = format!("/api/guilds/{}/appeals/{id}/ask", g.get_value());
            let r = api("POST", &path, Some(json!({"text": reply.get_untracked()}))).await;
            busy.try_set((0, ""));
            match r {
                Ok(_) => {
                    msg.ok("Question sent. The member's answer shows under the appeal.");
                    reply.try_set(String::new());
                    load()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    // The link members get in the ban DM: staff can post it in the rules for members whose DMs are closed.
    let link = format!(
        "{}/#/appeal/{}",
        web_sys::window()
            .and_then(|w| w.location().origin().ok())
            .unwrap_or_default(),
        g.get_value()
    );
    let copied = RwSignal::new(None::<bool>);
    let link_copy = link.clone();
    view! {
        <div class="card row">
            <b>"Appeal link"</b>
            <code>{link}</code>
            <button class="ghost" on:click=move |_| { let t = link_copy.clone(); spawn_local(async move { copied.set(Some(copy_text(&t).await)) }) }>{move || copy_label(copied.get())}</button>
        </div>
        <div class="card row">
            <div class="chips">
                {["ban", "timeout"].map(|k| view! { <button class="ghost" class:on=move || kind.get() == k on:click=move |_| { kind.set(if kind.get_untracked() == k { String::new() } else { k.into() }); first() }>{k}</button> }).collect_view()}
            </div>
            <input placeholder="User ID/tag" aria-label="user ID or tag" prop:value=q on:input=move |e| q.set(event_target_value(&e)) on:change=move |_| first() />
            <button class="ghost" on:click=move |_| first()>"Search"</button>
            <input placeholder="Reply to the member (optional)" aria-label="reply to the member" prop:value=reply on:input=move |e| reply.set(event_target_value(&e)) />
            {crate::records::export_csv(list, "appeals", &["id", "user_id", "username", "kind", "answers", "status", "at"], msg)}
            <Toast msg />
        </div>
        <Show when=move || loaded.get() && rows.get().is_empty()><p class="card empty muted">"No pending appeals."</p></Show>
        <For each=move || rows.get() key=|r| (r["id"].as_i64(), r["status"].as_str().map(String::from)) let:r>
            {
                let id = r["id"].as_i64().unwrap_or(0);
                let open = r["status"] == "open";
                view! { <div class="card">
                    <div class="row"><b>{s(&r["username"])}</b><code>{s(&r["user_id"])}</code><span class="muted">{format!("{} appeal, {}", s(&r["kind"]), s(&r["status"]))}</span></div>
                    {r["answers"].as_array().cloned().unwrap_or_default().into_iter().map(|a| view! { <p><b>{s(&a["label"])}</b><br/>{s(&a["answer"])}</p> }).collect_view()}
                    <Show when=move || open>
                        <div class="row">
                            <button disabled=move || busy.get().0 != 0 on:click=move |_| decide(id, "accept", true, false, false)>{busy_label(move || busy.get() == (id, "accept"), "Accept")}</button>
                            <button class="ghost" disabled=move || busy.get().0 != 0 on:click=move |_| decide(id, "deny", false, false, false)>{busy_label(move || busy.get() == (id, "deny"), "Deny")}</button>
                            <button class="ghost" disabled=move || busy.get().0 != 0 on:click=move |_| decide(id, "blacklist", false, true, false)>{busy_label(move || busy.get() == (id, "blacklist"), "Deny and blacklist")}</button>
                            <button class="ghost" disabled=move || busy.get().0 != 0 on:click=move |_| decide(id, "dismiss", false, false, true)>{busy_label(move || busy.get() == (id, "dismiss"), "Dismiss")}</button>
                            <button class="ghost" disabled=move || busy.get().0 != 0 || reply.get().trim().is_empty() title="Sends the reply text above as a question. The member answers on the appeal page." on:click=move |_| ask(id)>{busy_label(move || busy.get() == (id, "ask"), "Ask the member")}</button>
                        </div>
                    </Show>
                </div> }
            }
        </For>
        {crate::records::pager(page, move || rows.get().len(), load)}
    }
}

/// Blocklist of one feature (`base` is "appeals" or "reports"); only appeals can purge it.
#[component]
pub fn Blacklist(
    guild: String,
    base: &'static str,
    #[prop(optional)] purge: bool,
) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    let (user, msg) = (RwSignal::new(String::new()), Msg::default());
    let loaded = RwSignal::new(false);
    let g = StoredValue::new(guild);
    let load = move || {
        if g.is_disposed() {
            return;
        }
        spawn_local(async move {
            match api(
                "GET",
                &format!("/api/guilds/{}/{base}/blacklist", g.get_value()),
                None,
            )
            .await
            {
                Ok(v) => set_rows.set(v.as_array().cloned().unwrap_or_default()),
                // A failed load is no empty list: only the error shows.
                Err(e) => return msg.err(e),
            }
            loaded.set(true);
        })
    };
    load();
    let post = move |path: String, body: Option<Value>| {
        spawn_local(async move {
            match api(
                "POST",
                &format!("/api/guilds/{}/{base}/blacklist{path}", g.get_value()),
                body,
            )
            .await
            {
                Ok(_) => {
                    msg.clear();
                    load()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    view! {
        <div class="card row">
            {crate::form::one_member(user)}
            <button on:click=move |_| post(String::new(), Some(json!({"user_id": user.get_untracked()})))>"Blacklist user"</button>
            <Show when=move || purge><button class="ghost" on:click=move |_| { ask(Ask::new("Remove every user from the appeals blacklist?").yes("Purge all").danger(), move |_| post("/purge".into(), None)) }>"Purge all"</button></Show>
            <Toast msg />
        </div>
        <Show when=move || loaded.get() && rows.get().is_empty()><p class="card empty muted">"No blacklisted users."</p></Show>
        <For each=move || rows.get() key=|r| s(&r["user_id"]) let:r>
            {
                let id = s(&r["user_id"]);
                view! { <div class="card row"><code>{id.clone()}</code><span class="muted">{s(&r["at"])}</span>
                    <button class="ghost" on:click=move |_| post(format!("/{id}/remove"), None)>"Remove"</button></div> }
            }
        </For>
    }
}

/// Public page a banned or timed-out user opens from the DM link.
#[component]
pub fn AppealForm(guild: String) -> impl IntoView {
    let info = RwSignal::new(Value::Null);
    let (answers, msg, done) = (
        RwSignal::new(Vec::<String>::new()),
        Msg::default(),
        RwSignal::new(false),
    );
    let (fail, sending) = (RwSignal::new(None::<String>), RwSignal::new(false));
    let g = StoredValue::new(guild);
    let mine = RwSignal::new(Vec::<Value>::new());
    let load_mine = move || {
        if g.is_disposed() {
            return;
        }
        spawn_local(async move {
            if let Ok(v) = api("GET", &format!("/api/appeal/{}/mine", g.get_value()), None).await {
                mine.set(v.as_array().cloned().unwrap_or_default());
            }
        })
    };
    load_mine();
    let withdraw = move |_| {
        spawn_local(async move {
            match api(
                "POST",
                &format!("/api/appeal/{}/withdraw", g.get_value()),
                None,
            )
            .await
            {
                Ok(_) => load_mine(),
                Err(e) => msg.err(e),
            }
        })
    };
    let load_info = move || {
        if g.is_disposed() {
            return;
        }
        spawn_local(async move {
            match api("GET", &format!("/api/appeal/{}", g.get_value()), None).await {
                Ok(v) => {
                    answers.try_set(vec![
                        String::new();
                        v["fields"].as_array().map_or(0, Vec::len)
                    ]);
                    info.try_set(v);
                }
                Err(e) => {
                    fail.try_set(Some(e));
                }
            }
        })
    };
    load_info();
    // More for the open appeal: as often as the member wants until it is decided.
    let more = RwSignal::new(String::new());
    let add = move |_| {
        sending.set(true);
        spawn_local(async move {
            let r = api(
                "POST",
                &format!("/api/appeal/{}/add", g.get_value()),
                Some(json!({"text": more.get_untracked()})),
            )
            .await;
            sending.try_set(false);
            match r {
                Ok(_) => {
                    more.try_set(String::new());
                    msg.ok("Added to your appeal.");
                    load_mine()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    let submit = move |_| {
        sending.set(true);
        spawn_local(async move {
            let r = api(
                "POST",
                &format!("/api/appeal/{}", g.get_value()),
                Some(json!({"answers": answers.get_untracked()})),
            )
            .await;
            sending.try_set(false);
            match r {
                Ok(_) => {
                    done.set(true);
                    load_mine();
                    load_info()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    view! {
        <div class="appeal">
        <Toast msg />
        <For each=move || mine.get() key=|r| (r["id"].as_i64(), r["status"].as_str().map(String::from)) let:r>
            {
                let status = s(&r["status"]);
                let (chip, text) = match status.as_str() {
                    "open" => ("info", "Waiting for staff"),
                    "accepted" => ("ok", "Accepted"),
                    "denied" => ("bad", "Denied"),
                    _ => ("", "Withdrawn"),
                };
                let open = status == "open";
                view! { <div class="card appeal-row">
                    <div class="row"><span class=format!("chip {chip}")>{text}</span><b>{format!("{} appeal", s(&r["kind"]))}</b><span class="sp"></span><span class="muted small">{format!("Sent {}", crate::profile::when(&r["at"]))}</span></div>
                    {r["reply"].as_str().map(|t| view! { <p><b>"Staff reply: "</b>{t.to_string()}</p> })}
                    {open.then(|| r["answers"].as_array().cloned().unwrap_or_default().into_iter().map(|a| view! { <p><b>{s(&a["label"])}</b><br/>{s(&a["answer"])}</p> }).collect_view())}
                    <Show when=move || open><button class="ghost" on:click=withdraw>"Withdraw"</button></Show>
                </div> }
            }
        </For>
        {move || if done.get() {
            panel(Brim::ThumbsUp, "Appeal sent", "Staff will review it. Check this page later for the answer.").into_any()
        } else if let Some(e) = fail.get() {
            if e == "not found" { panel(Brim::Shrug, "Appeals are off", "Appeals are not enabled in this server.").into_any() } else { panel(Brim::Stop, "Could not load the appeal page", e).into_any() }
        } else if info.with(Value::is_null) {
            view! { <p class="center"><Working /></p> }.into_any()
        } else {
            let i = info.get();
            if i["blacklisted"] == true { panel(Brim::Stop, "Appeals are closed for you", "You can no longer appeal in this server.").into_any() }
            else if i["kind"].is_null() { panel(Brim::ThumbsUp, "Nothing to appeal", "You are not banned or timed out in this server.").into_any() }
            else if i["open"] == true { view! { <div class="card">
                {about(&i, g.get_value())}
                <h2>"Add to your appeal"</h2>
                <p class="muted">"Staff have not decided yet. Add more, or answer their question. Once they decide, nothing more can be added."</p>
                <textarea class="short" aria-label="more for your appeal" maxlength="1000" prop:value=more on:input=move |e| more.set(event_target_value(&e))></textarea>
                <button class="send" disabled=move || sending.get() || more.get().trim().is_empty() on:click=add>{busy_label(move || sending.get(), "Add")}</button>
            </div> }.into_any() }
            else { view! { <div class="card">
                {about(&i, g.get_value())}
                <h2>{if i["kind"] == "ban" { "Appeal your ban" } else { "Appeal your timeout" }}</h2>
                <p class="muted">"Answer the questions below. Staff read your appeal and answer it on this page."</p>
                {questions(i["fields"].as_array().cloned().unwrap_or_default(), answers)}
                <button class="send" disabled=move || sending.get() on:click=submit>{busy_label(move || sending.get(), "Send appeal")}</button>
            </div> }.into_any() }
        }}
        </div>
    }
}

/// The server, the reason the member was told and, for a temporary punishment only, its end.
fn about(i: &Value, guild: String) -> impl IntoView {
    let name = s(&i["server"]["name"]);
    let icon = i["server"]["icon"].as_str().map(|h| format!("https://cdn.discordapp.com/icons/{guild}/{h}.png?size=64"));
    let reason = i["reason"].as_str().filter(|r| !r.is_empty()).map(String::from);
    let ends = i["ends"].as_str().map(|_| crate::profile::when(&i["ends"]));
    view! {
        <Show when={let n = name.clone(); move || !n.is_empty()}><div class="row">{icon.clone().map(|src| view! { <img class="gicon" src=src alt="" /> })}<b>{name.clone()}</b></div></Show>
        {reason.map(|r| view! { <p><b>"Reason: "</b>{r}</p> })}
        {ends.map(|t| view! { <p><b>"Ends: "</b>{t}</p> })}
    }
}

/// A centred card for a state with nothing to fill in: the mascot, a headline and one line.
fn panel(pose: Brim, title: &'static str, text: impl IntoView + 'static) -> impl IntoView {
    view! { <div class="card empty"><Pose pose class="pose-top" /><h2>{title}</h2><p class="muted">{text}</p></div> }
}

/// The appeal questions as inputs writing into `answers` (one entry per field); also the settings preview.
pub fn questions(fields: Vec<Value>, answers: RwSignal<Vec<String>>) -> impl IntoView {
    fields.into_iter().enumerate().map(|(n, f)| {
        let on = move |e| answers.update(|a| a[n] = event_target_value(&e));
        let opts: Vec<String> = f["options"].as_array().map(|o| o.iter().map(s).collect()).unwrap_or_default();
        let max = f["max"].as_u64().unwrap_or(1000).to_string();
        let (max2, opts_empty) = (max.clone(), opts.is_empty());
        let input = if opts.is_empty() {
            if f["long"] == true { view! { <textarea class="short" aria-label=s(&f["label"]) maxlength=max on:input=on></textarea> }.into_any() } else { view! { <input aria-label=s(&f["label"]) maxlength=max on:input=on /> }.into_any() }
        } else if f["multi"] == true {
            view! { <div class="pick" role="group" aria-label=s(&f["label"])>{opts.into_iter().map(|o| { let o2 = o.clone(); view! { <label><input type="checkbox" on:change=move |e| answers.update(|a| {
                let mut picked: Vec<String> = a[n].lines().filter(|l| *l != o2).map(String::from).collect();
                if event_target_checked(&e) { picked.push(o2.clone()) }
                a[n] = picked.join("\n");
            }) /> {o}</label> } }).collect_view()}</div> }.into_any()
        } else {
            view! { <select aria-label=s(&f["label"]) on:change=on><option value="">"Choose"</option>{opts.into_iter().map(|o| { let v = o.clone(); view! { <option value=v>{o}</option> } }).collect_view()}</select> }.into_any()
        };
        let count = opts_empty.then(|| view! { <span class="muted small">{move || format!("{} / {max2}", answers.with(|a| a.get(n).map_or(0, |t| t.chars().count())))}</span> });
        view! { <div class="row"><label>{s(&f["label"])}</label>{input}{count}</div> }
    }).collect_view()
}

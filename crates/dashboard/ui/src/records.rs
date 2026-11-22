use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// Who wrote a note or an edit: a person, or an AI client through MCP acting as that person (the bot owner).
fn by(id: &Value, ai: &Value) -> String {
    match ai.as_str() {
        Some(t) => format!("AI via bot owner {}, token {t}", s(id)),
        None => s(id),
    }
}

/// Filter chips writing into `sig`; every change reloads through `load`.
fn chips(
    sig: RwSignal<String>,
    opts: &'static [(&'static str, &'static str)],
    load: impl Fn() + Copy + 'static,
) -> impl IntoView {
    view! { <div class="chips">{opts.iter().map(|(k, l)| view! { <button class="ghost" class:on=move || sig.get() == *k on:click=move |_| { sig.set((*k).into()); load() }>{*l}</button> }).collect_view()}</div> }
}

thread_local! {
    /// The newest request number per list, so an older answer that lands last is dropped.
    /// shortcut: entries of closed pages stay (a few bytes each), clear them if this ever matters.
    static LATEST: std::cell::RefCell<std::collections::HashMap<WriteSignal<Vec<Value>>, u64>> =
        Default::default();
}

/// Loads `/api/guilds/{g}/{path}?{query}` into `rows`, errors into `msg`; `loaded` turns on after the first answer.
pub(crate) fn fetch(path: String, rows: WriteSignal<Vec<Value>>, msg: Msg, loaded: RwSignal<bool>) {
    let seq = LATEST.with_borrow_mut(|l| {
        let n = l.entry(rows).or_default();
        *n += 1;
        *n
    });
    spawn_local(async move {
        let r = api("GET", &path, None).await;
        if LATEST.with_borrow(|l| l.get(&rows) != Some(&seq)) || loaded.is_disposed() {
            return;
        }
        match r {
            Ok(v) => {
                msg.clear();
                rows.set(v.as_array().cloned().unwrap_or_default())
            }
            // A failed load is no empty list: only the error shows.
            Err(e) => return msg.err(e),
        }
        loaded.set(true);
    });
}

/// "Export CSV": loads every page of `list` (a list URL ending in `page=`, so the export keeps the
/// current filter) and downloads it as `{name}.csv`.
pub fn export_csv(
    list: impl Fn() -> String + Copy + 'static,
    name: &'static str,
    cols: &'static [&'static str],
    msg: Msg,
) -> impl IntoView {
    let go = move |_| {
        let base = list();
        spawn_local(async move {
            let mut all = vec![];
            for p in 0.. {
                match api("GET", &format!("{base}{p}"), None).await {
                    Ok(v) => {
                        let page = v.as_array().cloned().unwrap_or_default();
                        let last = page.len() < 50;
                        all.extend(page);
                        if last {
                            break;
                        }
                    }
                    Err(e) => return msg.err(e),
                }
            }
            download(
                &format!("{name}.csv"),
                &format!("data:text/csv;charset=utf-8,{}", enc(&to_csv(cols, &all))),
            );
        });
    };
    view! { <button class="ghost" on:click=go>"Export CSV"</button> }
}

/// Every cell quoted; text starting with = + - @ gets a leading ' so spreadsheets do not run it as a formula.
fn csv_cell(v: &Value) -> String {
    let t = match v {
        Value::String(t) => t.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    };
    let t = if t.starts_with(['=', '+', '-', '@']) {
        format!("'{t}")
    } else {
        t
    };
    format!("\"{}\"", t.replace('"', "\"\""))
}

fn to_csv(cols: &[&str], rows: &[Value]) -> String {
    let mut out = cols.join(",") + "\r\n";
    for r in rows {
        out += &cols
            .iter()
            .map(|c| csv_cell(&r[*c]))
            .collect::<Vec<_>>()
            .join(",");
        out += "\r\n";
    }
    out
}

/// Previous/next buttons for lists the API serves 50 rows a page.
pub fn pager(
    page: RwSignal<i64>,
    count: impl Fn() -> usize + Copy + Send + Sync + 'static,
    load: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let shown = move || page.get() > 0 || count() == 50;
    view! { <Show when=shown><div class="row pager">
        <button class="ghost" aria-label="previous page" disabled=move || page.get() == 0 on:click=move |_| { page.update(|p| *p -= 1); load() }>{crate::icons::icon("page_prev")}</button>
        <span>{move || format!("Page {}", page.get() + 1)}</span>
        <button class="ghost" aria-label="next page" disabled=move || count() < 50 on:click=move |_| { page.update(|p| *p += 1); load() }>{crate::icons::icon("page_next")}</button>
    </div></Show> }
}

/// Looks up member names for `ids` (25 per request, the API's cap); members who left keep their ID.
pub(crate) fn learn_names(
    guild: String,
    mut ids: Vec<String>,
    names: RwSignal<std::collections::HashMap<String, String>>,
) {
    ids.sort();
    ids.dedup();
    ids.retain(|i| !i.is_empty() && !names.with_untracked(|n| n.contains_key(i)));
    for chunk in ids.chunks(25) {
        let path = format!("/api/guilds/{guild}/members?ids={}", chunk.join(","));
        spawn_local(async move {
            if let Ok(v) = api("GET", &path, None).await {
                names.update(|n| {
                    n.extend(
                        v.as_array()
                            .into_iter()
                            .flatten()
                            .map(|m| (s(&m["id"]), s(&m["name"]))),
                    )
                });
            }
        });
    }
}

fn enc(v: &str) -> String {
    js_sys::encode_uri_component(v).into()
}

/// Name with the ID on hover; the bare ID until the name arrives or when the member left.
pub(crate) fn who(names: RwSignal<std::collections::HashMap<String, String>>, id: String) -> impl IntoView {
    let tip = id.clone();
    view! { <b title=tip>{move || names.with(|n| n.get(&id).cloned()).unwrap_or_else(|| id.clone())}</b> }
}

/// The case list: one clean row per case; a click opens the case page.
#[component]
pub fn CaseList(guild: String) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    let loaded = RwSignal::new(false);
    let (q, kind, state, msg) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        Msg::default(),
    );
    let g = StoredValue::new(guild);
    let page = RwSignal::new(0i64);
    let list = move || {
        format!(
            "/api/guilds/{}/cases?q={}&kind={}&state={}&page=",
            g.get_value(),
            enc(&q.get_untracked()),
            kind.get_untracked(),
            state.get_untracked()
        )
    };
    let load = move || {
        if page.is_disposed() {
            return;
        }
        fetch(
            format!("{}{}", list(), page.get_untracked()),
            set_rows,
            msg,
            loaded,
        )
    };
    let first = move || {
        page.set(0);
        load()
    };
    load();
    let names = RwSignal::new(std::collections::HashMap::<String, String>::new());
    Effect::new(move || {
        learn_names(
            g.get_value(),
            rows.get()
                .iter()
                .flat_map(|r| [s(&r["user_id"]), s(&r["mod_id"])])
                .collect(),
            names,
        )
    });
    view! {
        <div class="card row">
            {chips(state, &[("", "All"), ("open", "Open"), ("closed", "Closed")], first)}
            <select aria-label="case type" on:change=move |e| { kind.set(event_target_value(&e)); first() }>
                <option value="">"Any type"</option>
                {["ban", "kick", "timeout", "warn", "quarantine", "sanitize", "purge", "slowmode", "verify"].map(|k| view! { <option value=k>{k}</option> }).to_vec()}
            </select>
            <input placeholder="User or mod ID" aria-label="user or mod ID" prop:value=q on:input=move |e| q.set(event_target_value(&e)) on:change=move |_| first() />
            <button class="ghost" on:click=move |_| first()>"Search"</button>
            {export_csv(list, "cases", &["case_no", "kind", "user_id", "mod_id", "reason", "proofs", "points", "active", "at"], msg)}
            <Toast msg />
        </div>
        <Show when=move || loaded.get() && rows.get().is_empty()><p class="card empty muted">"No cases found."</p></Show>
        <div class="stack caselist">
        <For each=move || rows.get() key=|r| r.to_string() let:r>
            {
                let no = r["case_no"].as_i64().unwrap_or(0);
                let active = r["active"].as_bool().unwrap_or(true);
                view! { <a class="card link" href=format!("#/{}/cases/{no}", g.get_value())>
                    <b class="no">{format!("#{no}")}</b>
                    <span class="what"><span><b>{s(&r["kind"])}</b>" "<span class="chip" class:ok=active>{if active { "open" } else { "closed" }}</span></span>
                        <span class="muted">{who(names, s(&r["user_id"]))}" by "{who(names, s(&r["mod_id"]))}" · "{crate::datareq::when(&r["at"])}</span></span>
                    <span class="sp"></span>{crate::icons::icon("chevron_right")}
                </a> }
            }
        </For>
        </div>
        {pager(page, move || rows.get().len(), load)}
    }
}

/// One proof's label: what kind it is, with a ✓ on the bot's own copy of the member's message.
fn proof_label(p: &Value) -> (&'static str, bool) {
    match (p["verified"].as_bool().unwrap_or(false), p["kind"].as_str()) {
        (true, _) => ("✓ Verified", true),
        (_, Some("link")) => ("Link", false),
        (_, Some("image")) => ("File", false),
        _ => ("Text", false),
    }
}

/// One history line: "reason: old → new", "closed", or "removed a proof (link)".
fn edit_text(e: &Value) -> String {
    let (old, new) = (s(&e["old"]), s(&e["new"]));
    let none = |t: String| if t.is_empty() { "(none)".to_string() } else { t };
    match e["field"].as_str() {
        Some("reason") => format!("Reason: {} → {}", none(old), none(new)),
        Some("active") => format!("State: {old} → {new}"),
        _ => format!("Removed a proof ({old})"),
    }
}

/// The case page: details with Edit, the proofs and the edit history.
#[component]
pub fn CasePage(guild: String, no: String) -> impl IntoView {
    let case = RwSignal::new(None::<Value>);
    let (msg, need) = (Msg::default(), step_up());
    let rank = expect_context::<Access>().rank();
    let g = StoredValue::new(guild);
    let base = StoredValue::new(format!("/api/guilds/{}/cases/{no}", g.get_value()));
    let back = StoredValue::new(format!("#/{}/cases", g.get_value()));
    let missing = RwSignal::new(false);
    // The newest request wins: an older answer that lands last would show the case before a change.
    let seq = StoredValue::new(0u32);
    let load = move || {
        seq.update_value(|n| *n += 1);
        let mine = seq.get_value();
        spawn_local(async move {
            let r = api("GET", &base.get_value(), None).await;
            if case.is_disposed() || seq.get_value() != mine {
                return;
            }
            match r {
                Ok(v) => case.set(Some(v)),
                Err(e) => {
                    missing.set(true);
                    msg.err(e)
                }
            }
        });
    };
    load();
    let names = RwSignal::new(std::collections::HashMap::<String, String>::new());
    let asked = StoredValue::new(std::collections::HashSet::<String>::new());
    Effect::new(move || {
        let Some(c) = case.get() else { return };
        let people = |list: &Value| -> Vec<String> {
            list.as_array().into_iter().flatten().map(|x| s(&x["by"])).collect()
        };
        let mut ids = vec![s(&c["user_id"]), s(&c["mod_id"])];
        ids.extend(people(&c["proofs"]));
        ids.extend(people(&c["edits"]));
        // Asked once: a member who left never gets a name, and every change loads the case again.
        ids.retain(|i| asked.try_update_value(|a| a.insert(i.clone())).unwrap_or(false));
        learn_names(g.get_value(), ids, names)
    });
    let (editing, reason, open, proof) = (
        RwSignal::new(false),
        RwSignal::new(String::new()),
        RwSignal::new(true),
        RwSignal::new(String::new()),
    );
    // Every change loads the case again, so the proofs and the history show what the server kept.
    // One request at a time: a double click must not add a proof twice.
    let busy = RwSignal::new(false);
    let post = move |path: String, body: Value, done: fn(RwSignal<bool>, RwSignal<String>)| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let url = format!("{}{path}", base.get_value());
        let sent = async move {
            let r = api("POST", &url, Some(body)).await;
            busy.try_set(false);
            r
        };
        run_act(sent, msg, need, move |_| {
            done(editing, proof);
            load()
        });
    };
    let by = move |id: &Value, ai: &Value| {
        let ai = ai.as_str().map(|t| format!(" (AI via bot owner, token {t})"));
        view! { {who(names, s(id))}{ai} }
    };
    view! {
        <div class="stack casepage">
        <Toast msg />
        <Show when=move || missing.get() && case.get().is_none()><p class="card empty muted">"This case does not exist (any more)."</p></Show>
        {move || case.get().map(|c| {
            let active = c["active"].as_bool().unwrap_or(true);
            let kind = s(&c["kind"]);
            let proofs = c["proofs"].as_array().cloned().unwrap_or_default();
            let edits = c["edits"].as_array().cloned().unwrap_or_default();
            let (count, max) = (proofs.len(), c["proofs_max"].as_u64().unwrap_or(0) as usize);
            let old_reason = s(&c["reason"]);
            let start = old_reason.clone();
            let case_no = c["case_no"].as_i64().unwrap_or(0);
            view! {
                <div class="card">
                    <div class="row"><h2>{format!("#{case_no} {kind}")}</h2><span class="chip" class:ok=active>{if active { "open" } else { "closed" }}</span><span class="sp"></span>
                        <button on:click=move |_| { reason.set(start.clone()); open.set(active); editing.set(true) }>"Edit"</button>
                        {(rank >= 3).then(|| view! { <button class="ghost" disabled=move || busy.get() on:click=move |_| { ask(Ask::new("Delete this case for good?").text("Its proofs and its history go with it.").yes("Delete").danger(), move |_| { let (url, to) = (format!("{}/delete", base.get_value()), back.get_value()); run_act(guard(busy, async move { api("POST", &url, Some(json!({}))).await }), msg, need, move |_| { let _ = window().location().set_hash(&to); }) }) }>"Delete"</button> })}
                    </div>
                    <dl class="facts">
                        <dt>"Member"</dt><dd>{who(names, s(&c["user_id"]))}</dd>
                        <dt>"Moderator"</dt><dd>{who(names, s(&c["mod_id"]))}</dd>
                        <dt>"Date"</dt><dd>{crate::datareq::when(&c["at"])}</dd>
                        {c["expires_at"].as_str().map(|_| view! { <dt>"Ends"</dt><dd>{crate::datareq::when(&c["expires_at"])}</dd> })}
                        {(kind == "warn").then(|| view! { <dt>"Points"</dt><dd>{c["points"].to_string()}</dd> })}
                        <dt>"Reason"</dt><dd>{if old_reason.is_empty() { "(none)".to_string() } else { old_reason.clone() }}</dd>
                    </dl>
                    <Show when=move || editing.get()>
                        <div class="row">
                            <input placeholder="Reason" aria-label="reason" maxlength="1000" prop:value=reason on:input=move |e| reason.set(event_target_value(&e)) />
                            <select aria-label="case state" on:change=move |e| open.set(event_target_value(&e) == "open")>
                                <option value="open" prop:selected=move || open.get()>"Open"</option>
                                <option value="closed" prop:selected=move || !open.get()>"Closed"</option>
                            </select>
                            <button on:click=move |_| post(String::new(), json!({"reason": reason.get_untracked(), "active": open.get_untracked()}), |editing, _| editing.set(false))>"Save"</button>
                            <button class="ghost" on:click=move |_| editing.set(false)>"Cancel"</button>
                        </div>
                    </Show>
                    {(rank == 4 && matches!(kind.as_str(), "ban" | "kick" | "quarantine")).then(|| view! { <div class="row">{report(g.get_value(), case_no, msg, need)}</div> })}
                </div>
                <div class="card">
                    <div class="row"><h3>"Proof"</h3><span class="count">{format!("{count} / {max}")}</span></div>
                    {proofs.is_empty().then(|| view! { <p class="muted">"No proof yet. Add a link or a short text here, or a file in Discord with the proof command."</p> })}
                    <ul class="proofs">{proofs.iter().map(|p| {
                        let (label, verified) = proof_label(p);
                        let id = p["id"].as_i64().unwrap_or(0);
                        let body = s(&p["body"]);
                        let web = body.starts_with("https://") || body.starts_with("http://");
                        let shown = match (p["kind"].as_str(), web) {
                            (Some("image"), true) => view! { <a href=body.clone() target="_blank" rel="noopener noreferrer">"File on the mod log"</a> }.into_any(),
                            (Some("link"), true) => view! { <a href=body.clone() target="_blank" rel="noopener noreferrer"><code>{body.clone()}</code></a> }.into_any(),
                            _ => view! { <span class="said">{body.clone()}</span> }.into_any(),
                        };
                        let files: Vec<String> = p["files"].as_array().into_iter().flatten().map(s).collect();
                        // Where the message was; the link works only while the message still exists.
                        let jump = match (verified, p["channel_id"].as_str().filter(|c| *c != "0"), p["message_id"].as_str()) {
                            (true, Some(ch), Some(m)) => Some(format!("https://discord.com/channels/{}/{ch}/{m}", g.get_value())),
                            _ => None,
                        };
                        let from = if verified {
                            // A message Discord AutoMod blocked has no id, and its time is when the copy was stored.
                            let sent = if p["message_id"].is_null() { "stored" } else { "sent" };
                            format!("Message {sent} {}, stored by Brim", crate::datareq::when(&p["sent_at"]))
                        } else {
                            format!("Added {} by ", crate::datareq::when(&p["at"]))
                        };
                        view! { <li>
                            <span class="chip" class:ok=verified>{label}</span>
                            <span class="body">{shown}
                                {(!files.is_empty()).then(|| view! { <span class="muted">{format!("Files (not kept): {}", files.join(", "))}</span> })}
                                <span class="muted">{from}{(!verified).then(|| by(&p["by"], &p["ai_client"]))}{jump.map(|j| view! { ". "<a href=j target="_blank" rel="noopener noreferrer">"Open in Discord"</a>" (if it still exists)" })}</span></span>
                            {(!verified || rank >= 3).then(|| view! { <button class="ghost" aria-label="remove proof" on:click=move |_| { ask(Ask::new("Remove this proof?").text("It cannot be brought back. The history keeps only that a proof was removed.").yes("Remove").danger(), move |_| post(format!("/proofs/{id}/delete"), json!({}), |_, _| ())) }>"Remove"</button> })}
                        </li> }
                    }).collect_view()}</ul>
                    <div class="row">
                        <input placeholder="Link or short text" aria-label="new proof" maxlength="1000" prop:value=proof on:input=move |e| proof.set(event_target_value(&e)) />
                        <AddButton label="Add proof" attr:disabled={move || count >= max && max > 0} on:click=move |_| post("/proofs".into(), json!({"text": proof.get_untracked()}), |_, proof| proof.set(String::new())) />
                    </div>
                </div>
                <div class="card">
                    <h3>"History"</h3>
                    {edits.is_empty().then(|| view! { <p class="muted">"No edits. Every change staff make to this case is listed here; a case that ends by itself is not."</p> })}
                    <ul class="edits">{edits.iter().map(|e| view! { <li><span class="muted">{crate::datareq::when(&e["at"])}" by "{by(&e["by"], &e["ai_client"])}": "</span><span class="said">{edit_text(e)}</span></li> }).collect_view()}</ul>
                </div>
            }
        })}
        </div>
    }
}

/// "3 days ago" for an age in seconds.
fn ago(secs: f64) -> String {
    let d = (secs / 86400.0).floor() as i64;
    match d {
        i64::MIN..=0 => "today".into(),
        1 => "yesterday".into(),
        d => format!("{d} days ago"),
    }
}

/// The change against the period before: "new" when that one was empty.
fn trend(now: i64, prev: i64) -> (String, &'static str) {
    match (now, prev) {
        (0, 0) => ("–".into(), "muted"),
        (_, 0) => ("new".into(), "info"),
        _ => {
            let pct = (now - prev) * 100 / prev;
            let cls = if pct > 0 {
                "warn"
            } else if pct < 0 {
                "muted"
            } else {
                ""
            };
            (format!("{}{pct} %", if pct > 0 { "+" } else { "" }), cls)
        }
    }
}

/// Small bar row of a member's actions over the period, oldest first.
fn bars(series: &[i64]) -> impl IntoView {
    let max = series.iter().copied().max().unwrap_or(0).max(1) as f64;
    let w = series.len().max(1) * 4;
    let rects = series
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(i, n)| {
            let h = (*n as f64 / max * 20.0).max(2.0);
            view! { <rect x=(i * 4).to_string() y=format!("{:.1}", 20.0 - h) width="3" height=format!("{h:.1}") rx="1" /> }
        })
        .collect_view();
    view! { <svg class="bars" viewBox=format!("0 0 {w} 20") preserveAspectRatio="none" aria-hidden="true">{rects}</svg> }
}

#[component]
pub fn ModStats(guild: String) -> impl IntoView {
    let data = RwSignal::new(Value::Null);
    let open = RwSignal::new(None::<String>);
    let detail = RwSignal::new(Vec::<Value>::new());
    let (days, msg) = (RwSignal::new("30".to_string()), Msg::default());
    let g = StoredValue::new(guild);
    let load = move || {
        if days.is_disposed() {
            return;
        }
        let d = days.get_untracked();
        let path = format!("/api/guilds/{}/mod_stats?days={d}", g.get_value());
        spawn_local(async move {
            let r = api("GET", &path, None).await;
            // A slower answer for a period chip clicked earlier must not overwrite the newer one.
            if days.try_get_untracked().as_deref() != Some(d.as_str()) {
                return;
            }
            match r {
                Ok(v) => {
                    msg.clear();
                    data.set(v)
                }
                Err(e) => msg.err(e),
            }
        });
    };
    load();
    let names = RwSignal::new(std::collections::HashMap::<String, String>::new());
    Effect::new(move || {
        let ids: Vec<String> = data.with(|d| {
            d["mods"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| s(&m["mod_id"]))
                .collect()
        });
        learn_names(g.get_value(), ids, names)
    });
    let name = move |id: &str| {
        names
            .with(|n| n.get(id).cloned())
            .unwrap_or_else(|| id.to_string())
    };
    let show = move |id: String| {
        if open.get_untracked().as_deref() == Some(&id) {
            return open.set(None);
        }
        detail.set(vec![]);
        open.set(Some(id.clone()));
        let path = format!("/api/guilds/{}/mod_stats/{id}", g.get_value());
        spawn_local(async move {
            let r = api("GET", &path, None).await;
            // Another row may have been opened meanwhile, or the page left.
            if open.try_get_untracked().flatten().as_deref() != Some(id.as_str()) {
                return;
            }
            match r {
                Ok(v) => {
                    let ids = v
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|r| s(&r["target_id"]))
                        .collect();
                    learn_names(g.get_value(), ids, names);
                    detail.set(v.as_array().cloned().unwrap_or_default())
                }
                Err(e) => {
                    open.set(None);
                    msg.err(e)
                }
            }
        });
    };
    let kinds = move || {
        data.with(|d| {
            d["kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .map(s)
                .collect::<Vec<_>>()
        })
    };
    let csv = move |_| {
        let ks = kinds();
        let mut out = format!(
            "mod_id,name,{},total,previous,last_action\r\n",
            ks.join(",")
        );
        data.with_untracked(|d| {
            for m in d["mods"].as_array().into_iter().flatten() {
                let id = s(&m["mod_id"]);
                let mut row = vec![json!(id), json!(name(&id))];
                row.extend(ks.iter().map(|k| m["by_kind"][k].clone()));
                row.extend([
                    m["total"].clone(),
                    m["prev_total"].clone(),
                    m["last_at"].clone(),
                ]);
                out += &row.iter().map(csv_cell).collect::<Vec<_>>().join(",");
                out += "\r\n";
            }
        });
        download(
            "mod-stats.csv",
            &format!("data:text/csv;charset=utf-8,{}", enc(&out)),
        );
    };
    let now = js_sys::Date::now();
    let age = move |m: &Value| {
        m["last_at"]
            .as_str()
            .map(|t| ago((now - js_sys::Date::parse(t)) / 1000.0))
            .unwrap_or_default()
    };
    let rows = move || {
        let ks = kinds();
        data.with(|d| d["mods"].as_array().cloned().unwrap_or_default()).into_iter().map(|m| {
            let id = s(&m["mod_id"]);
            let total = m["total"].as_i64().unwrap_or(0);
            let (change, cls) = trend(total, m["prev_total"].as_i64().unwrap_or(0));
            let series: Vec<i64> = m["series"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
            let (tip, key, who) = (id.clone(), id.clone(), id.clone());
            let cells = ks.iter().map(|k| view! { <td data-label=k.clone()>{m["by_kind"][k].as_i64().unwrap_or(0)}</td> }).collect_view();
            let span = (ks.len() + 5).to_string();
            view! {
                <tr class="click" class:quiet=total == 0 on:click=move |_| show(key.clone())>
                    <td><b title=tip>{move || name(&who)}</b></td>{cells}<td data-label="Total"><b>{total}</b></td>
                    <td data-label="Change" class=cls>{change}</td><td data-label="Last action" class="muted">{age(&m)}</td><td data-label="Over time">{bars(&series)}</td>
                </tr>
                <Show when={ let id = id.clone(); move || open.get().as_deref() == Some(&id) }>
                    <tr class="detail"><td colspan=span.clone()>
                        <Show when=move || !detail.get().is_empty() fallback=|| view! { <Working /> }>
                            <ul>{move || detail.get().into_iter().map(|r| {
                                let t = s(&r["target_id"]);
                                let case = r["case_no"].as_i64().map(|n| format!(" (case #{n})")).unwrap_or_default();
                                let at = s(&r["at"]).chars().take(16).collect::<String>().replace('T', " ");
                                view! { <li><span class="muted">{at}</span>" "<b>{s(&r["kind"])}</b>" "{move || name(&t)}{case}<span class="muted">{format!(" · {}", s(&r["source"]))}</span></li> }
                            }).collect_view()}</ul>
                        </Show>
                    </td></tr>
                </Show>
            }
        }).collect_view()
    };
    let empty = move || data.with(|d| d["mods"].as_array().is_some_and(|m| m.is_empty()));
    view! {
        <div class="card row">
            {chips(days, &[("7", "7 days"), ("30", "30 days"), ("90", "90 days"), ("365", "1 year")], move || { open.set(None); load() })}
            <button class="ghost" on:click=csv>"Export CSV"</button>
            <Toast msg />
        </div>
        <p class="muted">"Actions per staff member in the period, against the period before. Bans, kicks and timeouts done in the Discord app count too. Staff with nothing in the period are dimmed. Click a row for their last 50 actions."</p>
        <Show when=empty fallback=move || view! {
            <div class="card scroll"><table class="data stats">
                <thead><tr><th>"Staff"</th>{move || kinds().into_iter().map(|k| view! { <th>{k}</th> }).collect_view()}<th>"Total"</th><th>"Change"</th><th>"Last action"</th><th>"Over time"</th></tr></thead>
                <tbody>{rows}</tbody>
            </table></div>
        }><p class="card empty muted">"No staff actions recorded yet."</p></Show>
    }
}

/// "Report to Raider List": a category and a button. The report waits for the bot owner's review.
fn report(guild: String, no: i64, msg: Msg, need: RwSignal<bool>) -> impl IntoView {
    let cat = RwSignal::new("raid".to_string());
    let reason = RwSignal::new(String::new());
    let path = StoredValue::new(format!("/api/guilds/{guild}/cases/{no}/raider_report"));
    let busy = RwSignal::new(false);
    let go = move |_| {
        ask(
            Ask::new("Report this user to the Raider List?")
                .text("The bot owner reviews every report before other servers use it.")
                .yes("Report"),
            move |_| {
                let body =
                    json!({"category": cat.get_untracked(), "reason": reason.get_untracked()});
                let path = path.get_value();
                run_act(
                    guard(busy, async move { api("POST", &path, Some(body)).await }),
                    msg,
                    need,
                    move |_| msg.ok("Reported. The bot owner reviews it."),
                );
            },
        )
    };
    view! {
        <select aria-label="Raider List category" on:change=move |e| cat.set(event_target_value(&e))>
            {["raid", "nuke", "scam", "spam"].map(|c| view! { <option value=c prop:selected=move || cat.get() == c>{c}</option> }).to_vec()}
        </select>
        <input placeholder="Reason (only the bot owner sees it)" aria-label="Raider List report reason" maxlength="400" prop:value=reason on:input=move |e| reason.set(event_target_value(&e)) />
        <button class="ghost" disabled=move || busy.get() on:click=go>{busy_label(move || busy.get(), "Report to Raider List")}</button>
    }
}

#[component]
pub fn NoteList(guild: String) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    let loaded = RwSignal::new(false);
    let (q, text, msg) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        Msg::default(),
    );
    let g = StoredValue::new(guild);
    let page = RwSignal::new(0i64);
    let list = move || {
        format!(
            "/api/guilds/{}/notes?q={}&page=",
            g.get_value(),
            enc(&q.get_untracked())
        )
    };
    let load = move || {
        if page.is_disposed() {
            return;
        }
        fetch(
            format!("{}{}", list(), page.get_untracked()),
            set_rows,
            msg,
            loaded,
        )
    };
    let first = move || {
        page.set(0);
        load()
    };
    load();
    let post = move |path: String, body: Value| {
        spawn_local(async move {
            match api(
                "POST",
                &format!("/api/guilds/{}/notes{path}", g.get_value()),
                Some(body),
            )
            .await
            {
                Ok(_) => {
                    // A new note's text is cleared only once it is stored, so a refusal keeps it.
                    if path.is_empty() {
                        text.try_set(String::new());
                    }
                    load()
                }
                Err(e) => msg.err(e),
            }
        })
    };
    view! {
        <div class="card row">
            <input placeholder="User ID" aria-label="user ID" prop:value=q on:input=move |e| q.set(event_target_value(&e)) on:change=move |_| first() />
            <button class="ghost" on:click=move |_| first()>"Search"</button>
            <input placeholder="New note" aria-label="new note" maxlength="500" prop:value=text on:input=move |e| text.set(event_target_value(&e)) />
            <AddButton label="Add note" on:click=move |_| { post(String::new(), json!({"user_id": q.get_untracked(), "text": text.get_untracked()})) } />
            <button class="ghost" on:click=move |_| { ask(Ask::new("Remove every note of this user?").yes("Sweep user").danger(), move |_| post("/sweep".into(), json!({"user_id": q.get_untracked()}))) }>"Sweep user"</button>
            <button class="ghost" on:click=move |_| { ask(Ask::new("Remove every note in this server?").text("This cannot be undone.").yes("Sweep server").danger(), move |_| post("/sweep".into(), json!({"all": true}))) }>"Sweep server"</button>
            {export_csv(list, "notes", &["id", "user_id", "author_id", "ai_client", "text", "at"], msg)}
            <Toast msg />
        </div>
        <Show when=move || loaded.get() && rows.get().is_empty()><p class="card empty muted">"No notes found."</p></Show>
        <For each=move || rows.get() key=|r| (s(&r["id"]), s(&r["text"])) let:r>
            {
                let id = s(&r["id"]);
                let del = id.clone();
                let body = RwSignal::new(s(&r["text"]));
                let edits = r["edits"].as_array().cloned().unwrap_or_default();
                view! { <div class="card">
                    <div class="row"><b>{id.clone()}</b><code>{s(&r["user_id"])}</code><span class="muted">{format!("Created {} by {}", s(&r["at"]), by(&r["author_id"], &r["ai_client"]))}</span></div>
                    {(!edits.is_empty()).then(|| view! { <details><summary>{format!("History ({})", edits.len())}</summary><ul>
                        {edits.iter().map(|e| view! { <li><span class="muted">{format!("{} by {}: ", s(&e["at"]), by(&e["by"], &e["ai_client"]))}</span>{s(&e["old"])}" → "{s(&e["new"])}</li> }).collect_view()}
                    </ul></details> })}
                    <div class="row"><input maxlength="500" aria-label="note text" prop:value=body on:input=move |e| body.set(event_target_value(&e)) />
                        <button on:click=move |_| post(format!("/{id}"), json!({"text": body.get_untracked()}))>"Save"</button>
                        <button class="ghost" on:click=move |_| { let del = del.clone(); ask(Ask::new("Delete this note?").yes("Delete").danger(), move |_| post(format!("/{del}/delete"), json!({}))) }>"Delete"</button></div>
                </div> }
            }
        </For>
        {pager(page, move || rows.get().len(), load)}
    }
}

#[component]
pub fn ReportList(guild: String) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    let loaded = RwSignal::new(false);
    let (state, msg) = (RwSignal::new("open".to_string()), Msg::default());
    let g = StoredValue::new(guild);
    let page = RwSignal::new(0i64);
    let list = move || {
        format!(
            "/api/guilds/{}/reports?state={}&page=",
            g.get_value(),
            state.get_untracked()
        )
    };
    let load = move || {
        if page.is_disposed() {
            return;
        }
        fetch(
            format!("{}{}", list(), page.get_untracked()),
            set_rows,
            msg,
            loaded,
        )
    };
    let first = move || {
        page.set(0);
        load()
    };
    load();
    let set_open = move |id: i64, open: bool| {
        spawn_local(async move {
            match api(
                "POST",
                &format!("/api/guilds/{}/reports/{id}/status", g.get_value()),
                Some(json!({"open": open})),
            )
            .await
            {
                Ok(_) => load(),
                Err(e) => msg.err(e),
            }
        })
    };
    view! {
        <div class="card row">
            {chips(state, &[("open", "Open"), ("dismissed", "Dismissed"), ("actioned", "Actioned"), ("", "All")], first)}
            <span class="muted">"Warn, timeout, kick and ban are done with the buttons on the report in Discord."</span>
            {export_csv(list, "reports", &["id", "reporter_id", "target_id", "channel_id", "message_id", "content", "status", "action", "handled_by", "at"], msg)}
            <Toast msg />
        </div>
        <Show when=move || loaded.get() && rows.get().is_empty()><p class="card empty muted">"No reports."</p></Show>
        <For each=move || rows.get() key=|r| (r["id"].as_i64(), r["status"].as_str().map(String::from)) let:r>
            {
                let id = r["id"].as_i64().unwrap_or(0);
                let open = r["status"] == "open";
                let g_id = g.get_value();
                let jump = format!("https://discord.com/channels/{g_id}/{}/{}", s(&r["channel_id"]), s(&r["message_id"]));
                let handled = match r["handled_by"].as_str() {
                    Some(by) => format!("{} by {by}", r["action"].as_str().unwrap_or(r["status"].as_str().unwrap_or(""))),
                    None => s(&r["status"]),
                };
                view! { <div class="card">
                    <div class="row"><b>{format!("Report #{id}")}</b><span class="muted">{handled}</span>
                        <span>"reporter "<code>{s(&r["reporter_id"])}</code></span><span>"target "<code>{s(&r["target_id"])}</code></span>
                        <a href=jump target="_blank" rel="noopener">"Jump to message"</a></div>
                    <p>{s(&r["content"])}</p>
                    <div class="row"><button class="ghost" on:click=move |_| set_open(id, !open)>{if open { "Dismiss" } else { "Reopen" }}</button></div>
                </div> }
            }
        </For>
        {pager(page, move || rows.get().len(), load)}
    }
}

/// Every account each join raid caught; the raid trigger log links here.
#[component]
pub fn RaidLog(guild: String) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    let msg = Msg::default();
    let loaded = RwSignal::new(false);
    fetch(
        format!("/api/guilds/{guild}/raid_log"),
        set_rows,
        msg,
        loaded,
    );
    view! {
        <Toast msg />
        <Show when=move || loaded.get() && rows.get().is_empty()><p class="card empty muted">"No join raids caught yet."</p></Show>
        {move || rows.get().into_iter().map(|r| {
            let accounts = r["accounts"].as_array().cloned().unwrap_or_default();
            view! { <div class="card">
                <h3>{format!("Raid at {} ({} caught)", crate::recovery::when(r["raid_ms"].as_i64().unwrap_or(0) / 1000), accounts.len())}</h3>
                {accounts.iter().map(|a| view! { <div class="row"><code>{s(&a["user_id"])}</code><span>{s(&a["name"])}</span></div> }).collect_view()}
            </div> }
        }).collect_view()}
    }
}

/// Every Join Gate punishment: the filters that matched, the bot and who added it, and Discord's refusal.
#[component]
pub fn JoinGateLog(guild: String) -> impl IntoView {
    let (rows, set_rows) = signal(Vec::<Value>::new());
    let (msg, loaded, page) = (Msg::default(), RwSignal::new(false), RwSignal::new(0i64));
    let load = move || {
        fetch(
            format!(
                "/api/guilds/{guild}/joingate_log?page={}",
                page.get_untracked()
            ),
            set_rows,
            msg,
            loaded,
        )
    };
    let load = StoredValue::new(load);
    load.get_value()();
    let reload = move || load.get_value()();
    view! {
        <Toast msg />
        <Show when=move || loaded.get() && rows.get().is_empty()><p class="card empty muted">"Join Gate has not punished anyone yet."</p></Show>
        {move || rows.get().into_iter().map(|r| {
            let filters: Vec<String> = r["filters"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(String::from)).collect();
            let bot = r["is_bot"].as_bool().unwrap_or(false).then(|| match r["adder_id"].as_str() {
                Some(a) => format!("Bot added by {a}"),
                None => "Bot, adder unknown".into(),
            });
            let result = match r["ok"].as_bool() {
                Some(true) => "Done".to_string(),
                _ => format!("Failed: {}", s(&r["error"])),
            };
            view! { <div class="card">
                <div class="row"><b>{s(&r["action"])}</b><code>{s(&r["user_id"])}</code><span class="muted">{s(&r["at"])}</span><span>{result}</span></div>
                <p>{s(&r["reason"])}{(!filters.is_empty()).then(|| format!(" ({})", filters.join(", ")))}</p>
                {bot.map(|b| view! { <p class="muted">{b}</p> })}
            </div> }
        }).collect_view()}
        {pager(page, move || rows.get().len(), reload)}
    }
}

/// One transcript line, `[time] Name (Role): text`, as (time, name, role, text).
fn log_line(l: &str) -> (&str, &str, &str, &str) {
    let Some((time, rest)) = l.strip_prefix('[').and_then(|r| r.split_once("] ")) else {
        return ("", "", "", l);
    };
    let Some((who, text)) = rest.split_once(": ") else {
        return (time, "", "", rest);
    };
    match who.rsplit_once(" (") {
        Some((name, role)) => (time, name, role.trim_end_matches(')'), text),
        None => (time, who, "", text),
    }
}

/// A closed ticket's conversation, opened from the link on its ticket log.
#[component]
pub fn TicketLog(guild: String, channel: String, message: String) -> impl IntoView {
    let (text, set_text) = signal(String::new());
    let msg = Msg::default();
    let path = format!("/api/guilds/{guild}/tickets/log/{channel}/{message}");
    spawn_local(async move {
        match api("GET", &path, None).await {
            Ok(v) => set_text.set(v["text"].as_str().unwrap_or("").to_string()),
            Err(e) => msg.err(e),
        }
    });
    view! {
        <Toast msg />
        <div class="card">
            <h3>{crate::icons::icon("modmail")}" Ticket transcript"</h3>
            {move || text.get().lines().map(|l| {
                let (time, name, role, body) = log_line(l);
                let chip = match role { "User" => "chip blue", "Staff" => "chip", _ => "chip muted" };
                view! { <div class="row">
                    <span class="muted">{time.to_string()}</span>
                    <b>{name.to_string()}</b>
                    {(!role.is_empty()).then(|| view! { <span class=chip>{role.to_string()}</span> })}
                    <span>{body.to_string()}</span>
                </div> }
            }).collect_view()}
        </div>
    }
}

#[cfg(test)]
mod log_tests {
    #[test]
    fn transcript_lines_split_into_parts() {
        assert_eq!(
            super::log_line("[2026-11-03 08:55:29] ann (User): hi: there"),
            ("2026-11-03 08:55:29", "ann", "User", "hi: there")
        );
        assert_eq!(super::log_line("odd"), ("", "", "", "odd"));
        // A continuation line (indented by the bot) is never parsed as a speaker.
        let l = "  [2026-11-03 09:10:00] Mod (Staff): yes";
        assert_eq!(super::log_line(l), ("", "", "", l));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_stats_texts() {
        assert_eq!(ago(10.0), "today");
        assert_eq!(ago(86400.0 * 1.5), "yesterday");
        assert_eq!(ago(86400.0 * 45.0), "45 days ago");
        assert_eq!(trend(0, 0).0, "–");
        assert_eq!(trend(4, 0).0, "new");
        assert_eq!(trend(14, 10), ("+40 %".into(), "warn"));
        assert_eq!(trend(5, 10).0, "-50 %");
    }

    #[test]
    fn csv_quotes_cells_and_defuses_formulas() {
        let rows = [
            json!({"a": "say \"hi\"", "b": 3, "c": null}),
            json!({"a": "=HYPERLINK(1)", "b": true}),
        ];
        assert_eq!(
            to_csv(&["a", "b", "c"], &rows),
            "a,b,c\r\n\"say \"\"hi\"\"\",\"3\",\"\"\r\n\"'=HYPERLINK(1)\",\"true\",\"\"\r\n"
        );
    }
}

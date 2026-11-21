//! Bot Profile page: one panel with Brim's look in one server (nickname, bio, avatar and banner,
//! for the owner and Extra Owners) and its bot-wide status and activity (only the bot's owner may
//! change those), with a live member list and profile preview and one Save.
use crate::common::*;
use leptos::{html, prelude::*, task::spawn_local};
use serde_json::{json, Value};

const STATUSES: [(&str, &str); 4] = [
    ("online", "Online"),
    ("idle", "Idle"),
    ("dnd", "Do Not Disturb"),
    ("invisible", "Invisible"),
];
const KINDS: [(&str, &str); 6] = [
    ("", "None"),
    ("playing", "Playing"),
    ("watching", "Watching"),
    ("listening", "Listening to"),
    ("competing", "Competing in"),
    ("custom", "Custom status"),
];
const NICK_MAX: usize = 32;
const BIO_MAX: usize = 190;
const TEXT_MAX: usize = 128;
/// (field, label, MB, smallest size) for the two images, in `Page::picked` order.
const IMAGES: [(&str, &str, f64, &str); 2] = [
    ("avatar", "avatar", 8.0, "128 x 128"),
    ("banner", "banner", 10.0, "600 x 240"),
];

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// The profile, or why it cannot be shown (the server's 403 text, or Brim is not there).
async fn load(path: &str) -> Result<Value, String> {
    api("GET", path, None).await.map_err(|e| {
        if e == "not found" {
            "Brim is not in this server.".into()
        } else {
            e
        }
    })
}

fn when(at: &Value) -> String {
    js_sys::Date::new(&s(at).into())
        .to_locale_string("en-GB", &wasm_bindgen::JsValue::UNDEFINED)
        .into()
}

/// The activity line Discord shows under the name ("Listening to /help"), empty for none.
fn activity(kind: &str, text: &str) -> String {
    let text = text.trim();
    match (kind, KINDS.iter().find(|(k, _)| *k == kind)) {
        // No activity type means no activity, whatever is left in the text box.
        ("", _) => String::new(),
        (_, _) if text.is_empty() => String::new(),
        ("custom", _) => text.to_string(),
        (_, Some((_, label))) => format!("{label} {text}"),
        _ => text.to_string(),
    }
}

/// One write of a save. `None` text resets the field.
enum Job {
    Text(&'static str, Option<String>),
    Image(usize, web_sys::File),
    Presence(Value),
}

#[derive(Clone, Copy)]
struct Page {
    base: StoredValue<String>,
    /// `None` while the first load runs.
    data: RwSignal<Option<Result<Value, String>>>,
    nick: RwSignal<String>,
    bio: RwSignal<String>,
    status: RwSignal<String>,
    /// Empty means no activity.
    kind: RwSignal<String>,
    text: RwSignal<String>,
    /// A picked avatar and banner file, as object URLs, shown in the previews before the save.
    picked: [RwSignal<Option<String>>; 2],
    files: [NodeRef<html::Input>; 2],
    /// One save at a time (UI PITFALLS 15).
    sending: RwSignal<bool>,
    msg: Msg,
    need: RwSignal<bool>,
}

impl Page {
    fn get(self, k: &str) -> Value {
        self.data.with(|d| match d {
            Some(Ok(v)) => v[k].clone(),
            _ => Value::Null,
        })
    }
    fn get_untracked(self, k: &str) -> Value {
        self.data.with_untracked(|d| match d {
            Some(Ok(v)) => v[k].clone(),
            _ => Value::Null,
        })
    }
    fn locked(self) -> bool {
        self.sending.get() || self.get("busy").as_bool().unwrap_or(false)
    }
    fn editable(self) -> bool {
        self.get("presence")["editable"].as_bool().unwrap_or(false)
    }
    /// Puts the stored values back into the boxes.
    fn sync(self) {
        let pr = self.get_untracked("presence");
        self.nick.set(s(&self.get_untracked("nick")));
        self.bio.set(s(&self.get_untracked("bio")));
        self.status
            .set(pr["status"].as_str().unwrap_or("online").to_string());
        self.kind.set(s(&pr["kind"]));
        self.text.set(s(&pr["text"]));
    }
    /// Drops a picked image (after its save, a reset, or a refused file). Safe after the page is gone.
    fn unpick(self, i: usize) {
        if let Some(Some(old)) = self.picked[i].try_get_untracked() {
            let _ = web_sys::Url::revoke_object_url(&old);
        }
        self.picked[i].try_set(None);
        if let Some(input) = self.files[i].try_get_untracked().flatten() {
            input.set_value("");
        }
    }
    /// The changes not saved yet, in send order.
    fn changes(self) -> Vec<&'static str> {
        let mut out = vec![];
        if self.nick.with(|n| n.trim().to_string()) != s(&self.get("nick")) {
            out.push("nickname");
        }
        if self.bio.with(|b| b.trim().to_string()) != s(&self.get("bio")) {
            out.push("bio");
        }
        for (i, (_, label, ..)) in IMAGES.iter().enumerate() {
            if self.picked[i].with(Option::is_some) {
                out.push(*label);
            }
        }
        let pr = self.get("presence");
        let kind = self.kind.get();
        // Text left in the box counts only with an activity type, as only then is it saved.
        let text = if kind.is_empty() {
            String::new()
        } else {
            self.text.with(|t| t.trim().to_string())
        };
        let typed = (self.status.get(), kind, text);
        if self.editable() && typed != (s(&pr["status"]), s(&pr["kind"]), s(&pr["text"])) {
            out.push("status");
        }
        out
    }
    /// Sends the writes one after another, then loads the profile again whatever the answers: a
    /// refusal can still mean Discord changed it. Stops at the first refusal and says what was saved.
    fn run(
        self,
        jobs: Vec<(&'static str, Job)>,
        done: &'static str,
        after: impl FnOnce() + 'static,
    ) {
        if self.sending.get_untracked() {
            return self
                .msg
                .err("Another change is still being sent; try again in a moment.");
        }
        self.sending.set(true);
        let base = self.base.get_value();
        run_act(
            async move {
                let mut saved: Vec<&str> = vec![];
                let mut res = Ok(Value::Null);
                for (label, job) in jobs {
                    let r = match job {
                        Job::Text(f, Some(v)) => {
                            api("PUT", &format!("{base}/{f}"), Some(json!({ "value": v }))).await
                        }
                        Job::Text(f, None) => api("DELETE", &format!("{base}/{f}"), None).await,
                        Job::Image(i, file) => {
                            let r =
                                api_file("PUT", &format!("{base}/{}", IMAGES[i].0), file.into())
                                    .await;
                            if r.is_ok() {
                                self.unpick(i);
                            }
                            r
                        }
                        Job::Presence(body) => api("PUT", "/api/bot-presence", Some(body)).await,
                    };
                    match r {
                        Ok(_) => saved.push(label),
                        // Unchanged when nothing was saved yet or 2FA is asked, so the prompt still opens.
                        Err(e) if saved.is_empty() || e == "step_up_required" => {
                            res = Err(e);
                            break;
                        }
                        Err(e) => {
                            res = Err(format!(
                                "The {label} was not changed: {e}. Saved: {}.",
                                saved.join(", ")
                            ));
                            break;
                        }
                    }
                }
                // A failed reload keeps the form; the toast shows the write's own answer.
                if let Ok(fresh) = load(&base).await {
                    self.data.try_set(Some(Ok(fresh)));
                }
                self.sending.try_set(false);
                res
            },
            self.msg,
            self.need,
            move |_| {
                self.sync();
                after();
                self.msg.done(done);
            },
        );
    }
    fn save(self) {
        let nick = self.nick.get_untracked().trim().to_string();
        let bio = self.bio.get_untracked().trim().to_string();
        if nick.chars().count() > NICK_MAX {
            return self.msg.err(format!(
                "The bot name is longer than {NICK_MAX} characters."
            ));
        }
        if bio.chars().count() > BIO_MAX {
            return self
                .msg
                .err(format!("The bio is longer than {BIO_MAX} characters."));
        }
        let (kind, text) = (
            self.kind.get_untracked(),
            self.text.get_untracked().trim().to_string(),
        );
        let changes = untrack(|| self.changes());
        if changes.contains(&"status") {
            if !kind.is_empty() && text.is_empty() {
                return self.msg.err("Enter the status text, or pick no activity.");
            }
            if text.chars().count() > TEXT_MAX {
                return self.msg.err(format!(
                    "The status text is longer than {TEXT_MAX} characters."
                ));
            }
        }
        let mut jobs = vec![];
        for c in changes {
            let job = match c {
                "nickname" => Job::Text("nick", (!nick.is_empty()).then(|| nick.clone())),
                "bio" => Job::Text("bio", (!bio.is_empty()).then(|| bio.clone())),
                "status" => Job::Presence(if kind.is_empty() {
                    json!({"status": self.status.get_untracked(), "kind": null, "text": null})
                } else {
                    json!({"status": self.status.get_untracked(), "kind": kind, "text": text})
                }),
                image => {
                    let i = usize::from(image == "banner");
                    let Some(f) = self.files[i]
                        .get_untracked()
                        .and_then(|i| i.files())
                        .and_then(|l| l.get(0))
                    else {
                        continue;
                    };
                    Job::Image(i, f)
                }
            };
            jobs.push((c, job));
        }
        if jobs.is_empty() {
            return self.msg.err("Nothing to save: change a field first.");
        }
        self.run(jobs, "Saved!", || {});
    }
    /// Removes this server's nickname, bio, avatar and banner (only those that are set). Status and
    /// activity are bot-wide and stay.
    fn reset(self) {
        let set: Vec<(&'static str, Job)> = [
            ("nickname", "nick"),
            ("bio", "bio"),
            ("avatar", "avatar"),
            ("banner", "banner"),
        ]
        .into_iter()
        .filter(|(_, f)| !self.get_untracked(f).is_null())
        .map(|(label, f)| (label, Job::Text(f, None)))
        .collect();
        if set.is_empty() {
            return self
                .msg
                .err("Brim already uses its normal profile in this server.");
        }
        let jobs = std::cell::Cell::new(Some(set));
        ask(
            Ask::new("Reset to default?")
                .text("Brim's name, bio, avatar and banner in this server go back to its normal profile. Status and activity stay.")
                .yes("Reset")
                .danger(),
            move |_| {
                if let Some(j) = jobs.take() {
                    // A picked image goes only once the reset worked.
                    self.run(j, "Reset!", move || (0..2).for_each(|i| self.unpick(i)));
                }
            },
        )
    }
}

#[component]
pub fn BotProfile(guild: String) -> impl IntoView {
    let p = Page {
        base: StoredValue::new(format!("/api/guilds/{guild}/bot-profile")),
        data: RwSignal::new(None),
        nick: RwSignal::new(String::new()),
        bio: RwSignal::new(String::new()),
        status: RwSignal::new("online".into()),
        kind: RwSignal::new(String::new()),
        text: RwSignal::new(String::new()),
        picked: [RwSignal::new(None), RwSignal::new(None)],
        files: [NodeRef::new(), NodeRef::new()],
        sending: RwSignal::new(false),
        msg: Msg::default(),
        need: step_up(),
    };
    let path = p.base.get_value();
    spawn_local(async move {
        let r = load(&path).await;
        if p.data.is_disposed() {
            return;
        }
        p.data.set(Some(r));
        p.sync();
    });
    // Picked images are freed when the page goes (UI PITFALLS 2: the signal may already be gone).
    on_cleanup(move || {
        for sig in p.picked {
            if let Some(Some(u)) = sig.try_get_untracked() {
                let _ = web_sys::Url::revoke_object_url(&u);
            }
        }
    });
    // Rebuild the form only when it appears or goes, not on every reload (keeps focus and files).
    let state = Memo::new(move |_| {
        p.data.with(|d| match d {
            None => None,
            Some(Ok(_)) => Some(Ok(())),
            Some(Err(e)) => Some(Err(e.clone())),
        })
    });
    view! {
        {move || match state.get() {
            None => view! { <p class="loading"><Working /></p> }.into_any(),
            Some(Err(e)) => view! { <div class="card empty"><p>{e}</p></div> }.into_any(),
            Some(Ok(())) => view! { <Form p /> }.into_any(),
        }}
        <Toast msg=p.msg />
    }
}

#[component]
fn Form(p: Page) -> impl IntoView {
    let tile = |i: usize| {
        let (field, label, mb, min) = IMAGES[i];
        let pick = move |_| {
            let f = p.files[i]
                .get_untracked()
                .and_then(|i| i.files())
                .and_then(|l| l.get(0));
            let Some(f) = f else { return p.unpick(i) };
            if !matches!(f.type_().as_str(), "image/png" | "image/jpeg") {
                p.unpick(i);
                return p.msg.err("Only PNG and JPEG images can be used.");
            }
            if f.size() > mb * 1024.0 * 1024.0 {
                p.unpick(i);
                return p.msg.err(format!("The {label} is larger than {mb} MB."));
            }
            if let Some(old) = p.picked[i].get_untracked() {
                let _ = web_sys::Url::revoke_object_url(&old);
            }
            p.picked[i].set(web_sys::Url::create_object_url_with_blob(&f).ok());
        };
        let shown = move || {
            p.picked[i]
                .get()
                .or_else(|| p.get(field).as_str().map(String::from))
        };
        view! {
            <span class="bp-cap">{if i == 0 { "Avatar" } else { "Banner" }}</span>
            <label class=if i == 0 { "bp-tile bp-avtile" } else { "bp-tile bp-bntile" }>
                <input class="sr" type="file" accept="image/png,image/jpeg" aria-label=format!("{label} file") node_ref=p.files[i] prop:disabled=move || p.locked() on:change=pick />
                {move || match shown() {
                    Some(u) => view! { <img src=u alt="" /> }.into_any(),
                    None => view! { <span>{format!("Click to upload your {label}")}</span> }.into_any(),
                }}
            </label>
            <p class="muted small">{format!("PNG or JPEG, at least {min}, up to {mb} MB.")}</p>
        }
    };
    let options = |list: &'static [(&'static str, &'static str)], cur: RwSignal<String>| {
        list.iter()
            .map(|(v, t)| view! { <option value=*v prop:selected=move || cur.get() == *v>{*t}</option> })
            .collect_view()
    };
    let count = |sig: RwSignal<String>, max: usize| {
        move || format!("{} / {max}", sig.with(|v| v.trim().chars().count()))
    };
    let name = move || {
        let typed = p.nick.with(|n| n.trim().to_string());
        if typed.is_empty() {
            s(&p.get("name"))
        } else {
            typed
        }
    };
    let avatar = move || {
        p.picked[0]
            .get()
            .or_else(|| p.get("avatar").as_str().map(String::from))
            .or_else(|| p.get("user_avatar").as_str().map(String::from))
    };
    let av = move |class: &'static str| match avatar() {
        Some(u) => view! { <img class=class src=u alt="" /> }.into_any(),
        None => view! { <span class=class></span> }.into_any(),
    };
    let dot = move || format!("bp-dot {}", p.status.get());
    let act = move || activity(&p.kind.get(), &p.text.get());
    let dirty = Memo::new(move |_| !p.changes().is_empty());
    let ops = move || {
        let list = p.get("ops").as_array().cloned().unwrap_or_default();
        if list.is_empty() {
            return view! { <p class="muted">"No changes yet."</p> }.into_any();
        }
        view! {
            <ul>
                {list.into_iter().map(|o| {
                    let st = s(&o["status"]);
                    let class = match st.as_str() { "succeeded" => "ok", "rejected" | "unknown" => "err", _ => "muted" };
                    let err = matches!(st.as_str(), "rejected" | "unknown").then(|| view! { <br /><span class="err">{s(&o["error"])}</span> });
                    view! { <li><b>{s(&o["field"])}</b>" "{s(&o["op"])}" · "<span class=class>{st.clone()}</span>" · "{when(&o["at"])}" · "<span class="muted">{format!("by {}", s(&o["actor"]))}</span>{err}</li> }
                }).collect_view()}
            </ul>
        }
        .into_any()
    };
    view! {
        <p class="card info" role="note">"The name, bio, avatar and banner change Brim's profile in this server only. Status and activity are the same in every server."</p>
        {move || p.get("busy").as_bool().unwrap_or(false).then(|| view! { <p class="warn">"A profile change is still running. Reload the page in a moment."</p> })}
        <div class="card bp">
            <div class="bp-imgs">{tile(0)}{tile(1)}</div>
            <div class="bp-fields">
                <div class="row"><label>"Bot name"</label><input aria-label="bot name" placeholder=move || s(&p.get("name")) prop:value=p.nick on:input=move |e| p.nick.set(event_target_value(&e)) /><span class="muted small bp-count">{count(p.nick, NICK_MAX)}</span></div>
                <div class="row"><label>"Bio"</label><textarea class="short" aria-label="bio" prop:value=p.bio on:input=move |e| p.bio.set(event_target_value(&e))></textarea><span class="muted small bp-count">{count(p.bio, BIO_MAX)}</span></div>
                <div class="row"><label>"Bot status"</label><select aria-label="bot status" prop:disabled=move || !p.editable() on:change=move |e| p.status.set(event_target_value(&e))>{options(&STATUSES, p.status)}</select></div>
                <div class="bp-two">
                    <div class="row"><label>"Activity type"</label><select aria-label="activity type" prop:disabled=move || !p.editable() on:change=move |e| p.kind.set(event_target_value(&e))>{options(&KINDS, p.kind)}</select></div>
                    <div class="row"><label>"Status text"</label><input aria-label="status text" placeholder="What Brim is doing" prop:disabled=move || !p.editable() || p.kind.with(String::is_empty) prop:value=p.text on:input=move |e| p.text.set(event_target_value(&e)) /></div>
                </div>
                <p class="muted small">{move || if p.editable() { "Status and activity are the same in every server." } else { "Status and activity are the same in every server. Only Brim's owner can change them." }}</p>
                <div class="row">
                    <button prop:disabled=move || !dirty.get() || p.locked() on:click=move |_| p.save()>"Save changes"</button>
                    <button class="ghost" prop:disabled=move || p.locked() on:click=move |_| p.reset()>"Reset to default"</button>
                </div>
            </div>
            // Live: typed text, a picked image and the chosen status show before the save.
            <div class="bp-prevs" aria-label="previews">
                <span class="bp-cap">"Member list preview"</span>
                <div class="bp-member">
                    <span class="bp-avw">{move || av("bp-av")}<span class=dot></span></span>
                    <span class="bp-nm"><b>{name}</b><small>{act}</small></span>
                </div>
                <span class="bp-cap">"Profile preview"</span>
                <div class="bp-prev">
                    {move || match p.picked[1].get().or_else(|| p.get("banner").as_str().map(String::from)) {
                        Some(u) => view! { <img class="bp-banner" src=u alt="" /> }.into_any(),
                        None => view! { <div class="bp-banner"></div> }.into_any(),
                    }}
                    <span class="bp-avw big">{move || av("bp-av")}<span class=dot></span></span>
                    <div class="bp-text">
                        <b>{name}</b>
                        <p class="bp-bio">{move || p.bio.with(|b| b.trim().to_string())}</p>
                        {move || { let a = act(); (!a.is_empty()).then(|| view! { <p class="bp-act">{a}</p> }) }}
                    </div>
                </div>
            </div>
        </div>
        <details class="card more"><summary>"Recent changes"</summary>{ops}</details>
    }
}

#[cfg(test)]
mod tests {
    use super::activity;

    #[test]
    fn activity_line_reads_like_discord() {
        assert_eq!(activity("listening", " /help "), "Listening to /help");
        assert_eq!(activity("playing", "chess"), "Playing chess");
        assert_eq!(activity("custom", "On guard"), "On guard");
        assert_eq!(activity("", ""), "");
        assert_eq!(activity("", "left over"), "");
        assert_eq!(activity("watching", "  "), "");
    }
}

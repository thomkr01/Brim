use gloo_net::http::Request;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};
use std::{cell::RefCell, time::Duration};

/// How long a closing bar, toast or dialog fades out; matches `--dur` in style.css.
pub const OUT: Duration = Duration::from_millis(200);

/// `AnimatedShow` with the fade-out we use everywhere. Leptos starts a hide timer on every rerun while
/// `when` is false and only cancels the last one, so false->false reruns (e.g. a form loading) leave a
/// timer that hides the box again right after it showed. A memo only reruns on a real change.
#[component]
pub fn Fade(#[prop(into)] when: Signal<bool>, children: ChildrenFn) -> impl IntoView {
    let when = Memo::new(move |_| when.get());
    view! { <AnimatedShow when=when show_class="fade-wrap" hide_class="fade-wrap out" hide_delay=OUT>{children()}</AnimatedShow> }
}

thread_local! { pub static CSRF: RefCell<String> = const { RefCell::new(String::new()) }; }

/// After a logout: no later 401 may send the page to sign-in (that signed the person in again).
pub fn signed_out() {
    CSRF.with(|c| c.borrow_mut().clear());
}

fn request(method: &str, path: &str) -> gloo_net::http::RequestBuilder {
    let csrf = CSRF.with(|c| c.borrow().clone());
    match method {
        "GET" => Request::get(path),
        "PUT" => Request::put(path),
        "DELETE" => Request::delete(path),
        _ => Request::post(path),
    }
    .header("x-csrf-token", &csrf)
}

pub async fn api(method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    let b = request(method, path);
    let req = match body {
        Some(v) => b.json(&v).map_err(|e| e.to_string())?,
        None => b.build().map_err(|e| e.to_string())?,
    };
    reply(req).await
}

/// [`api`] with a file (or other blob) as the raw body, for image uploads.
pub async fn api_file(method: &str, path: &str, file: web_sys::Blob) -> Result<Value, String> {
    let req = request(method, path)
        .body(file)
        .map_err(|e| e.to_string())?;
    reply(req).await
}

async fn reply(req: gloo_net::http::Request) -> Result<Value, String> {
    // Logout itself may meet a dead session: that 401 must not send the person to sign-in again.
    let own = !req.url().contains("/auth/");
    let resp = req.send().await.map_err(|e| {
        format!("Could not reach the dashboard server. Check your connection and try again. ({e})")
    })?;
    let (ok, status, status_text) = (resp.ok(), resp.status(), resp.status_text());
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    // A signed-in page whose session ended (expired, idle, deleted): back to sign-in and then to this
    // page, not an error under a shell that no longer works. Signed out, CSRF is empty and the page
    // (landing, appeal, verify) handles the 401 itself.
    if status == 401 && own && CSRF.with(|c| !c.borrow().is_empty()) {
        if let Some(w) = web_sys::window() {
            let hash = w.location().hash().unwrap_or_default();
            let _ = w.location().set_href(&format!("/auth/login?next=/{}", js_sys::encode_uri_component(&hash)));
        }
    }
    if ok {
        Ok(v)
    } else {
        // A proxy error or a too-large body (413) comes as plain text, not as our JSON error.
        Err(v["error"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("The server answered {status} {status_text}.")))
    }
}

/// What the one [`Dialog`] asks; `input` turns it into a text prompt with that start value.
#[derive(Clone)]
pub struct Ask {
    title: String,
    text: String,
    yes: String,
    danger: bool,
    input: Option<String>,
    /// A second answer next to the confirm button; `yes` is then given [`ALT`].
    alt: String,
}

/// What `yes` is given when the dialog's second button was pressed.
pub const ALT: &str = "\u{0}alt";

impl Ask {
    pub fn new(title: impl Into<String>) -> Self {
        Ask {
            title: title.into(),
            text: String::new(),
            yes: "Confirm".into(),
            danger: false,
            input: None,
            alt: String::new(),
        }
    }
    pub fn alt(mut self, t: impl Into<String>) -> Self {
        self.alt = t.into();
        self
    }
    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.text = t.into();
        self
    }
    pub fn yes(mut self, t: impl Into<String>) -> Self {
        self.yes = t.into();
        self
    }
    /// A red confirm button, for what deletes or takes something away.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn input(mut self, start: impl Into<String>) -> Self {
        self.input = Some(start.into());
        self
    }
}

type OnYes = Box<dyn FnOnce(String)>;
/// Runs the action again; it is given the 2FA prompt's toast.
type Retry = Box<dyn FnOnce(Msg)>;
/// The dialog's question and the 2FA prompt's open state.
type Ui = (RwSignal<Option<Ask>>, RwSignal<bool>);

thread_local! {
    /// The dialog and 2FA prompt signals, owned by `App` so they outlive every page.
    static UI: std::cell::Cell<Option<Ui>> = const { std::cell::Cell::new(None) };
    static ON_YES: RefCell<Option<OnYes>> = const { RefCell::new(None) };
    /// What had focus when the dialog opened: focused again on close, so a request started from
    /// the confirm button knows its trigger.
    static TRIGGER: RefCell<Option<web_sys::HtmlElement>> = const { RefCell::new(None) };
    /// The action whose request needed 2FA; run again once the code is verified (see [`run_resumable`]).
    static RETRY: RefCell<Option<Retry>> = const { RefCell::new(None) };
    /// See [`active`].
    static LAST: RefCell<Option<web_sys::HtmlElement>> = const { RefCell::new(None) };
    /// Set while that click runs, so its confirm dialog is not asked a second time.
    static SKIP_ASK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ui() -> Ui {
    UI.with(|u| match u.get() {
        Some(x) => x,
        None => {
            let x = (RwSignal::new(None), RwSignal::new(false));
            u.set(Some(x));
            watch_clicks();
            x
        }
    })
}

/// The control the current click landed on. Safari does not focus a clicked button, so
/// `document.activeElement` cannot say which control asked; a capture listener records it, and it
/// is cleared once the click is handled, so a request not started by a click retries nothing.
fn active() -> Option<web_sys::HtmlElement> {
    LAST.with_borrow(Clone::clone)
}

fn watch_clicks() {
    use wasm_bindgen::{closure::Closure, JsCast};
    let on = Closure::<dyn Fn(web_sys::Event)>::new(|e: web_sys::Event| {
        let hit = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|t| t.closest("button, input, a, select").ok().flatten())
            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok());
        LAST.set(hit);
        set_timeout(|| LAST.set(None), Duration::ZERO);
    });
    if let Some(w) = web_sys::window() {
        let _ =
            w.add_event_listener_with_callback_and_bool("click", on.as_ref().unchecked_ref(), true);
    }
    on.forget(); // lives as long as the page
}

/// Asks in the dashboard's own dialog, then runs `yes` with the typed text (empty without `input`).
pub fn ask(a: Ask, yes: impl FnOnce(String) + 'static) {
    if SKIP_ASK.get() {
        return yes(a.input.unwrap_or_default());
    }
    TRIGGER.set(active());
    ON_YES.set(Some(Box::new(yes)));
    ui().0.set(Some(a));
}

/// The 2FA prompt's open state, shared by every page; pass it to [`run_act`] and friends.
pub fn step_up() -> RwSignal<bool> {
    ui().1
}

/// Whether `/api/me` says the step-up was done for the server page open now (it counts there only).
pub fn stepped_up_here(me: &Value) -> bool {
    me["stepped_up"] == true && me["step_up_guild"].as_str() == crate::hash().first().map(String::as_str)
}

/// Opens the 2FA prompt; `again` runs once after the code is verified (dropped on Cancel or a page change).
pub fn need_step_up(need: RwSignal<bool>, again: impl FnOnce() + 'static) {
    RETRY.set(Some(Box::new(move |_| again())));
    need.set(true);
}

/// The control the click landed on, clicked again after the code (the handler builds its request again).
fn click_again(trigger: Option<web_sys::HtmlElement>) -> Option<Retry> {
    let el = trigger?;
    Some(Box::new(move |msg: Msg| {
        // A control rebuilt meanwhile is a new element: the old one is no longer in the page.
        if !el.is_connected() {
            return msg.err("Verified, but the button you pressed is no longer on the page. Press it again.");
        }
        SKIP_ASK.set(true);
        el.click();
        SKIP_ASK.set(false);
    }))
}

fn close_dialog(yes: Option<String>) {
    ui().0.set(None);
    let f = ON_YES.take();
    let t = TRIGGER.take();
    if let Some(t) = &t {
        let _ = t.focus();
    }
    if let (Some(f), Some(v)) = (f, yes) {
        // The answer acts for the control that opened the dialog, not for the dialog's own button.
        LAST.set(t);
        f(v);
        LAST.set(None);
    }
}

/// The one confirm dialog, mounted in `App` over every page.
#[component]
pub fn Dialog() -> impl IntoView {
    let cur = ui().0;
    let text = RwSignal::new(String::new());
    // Leaving the page drops the question: its answer would read the old page's disposed signals.
    let _ = window_event_listener(leptos::ev::hashchange, |_| close_dialog(None));
    Effect::new(move || {
        if let Some(a) = cur.get() {
            text.set(a.input.unwrap_or_default())
        }
    });
    view! {
        <Fade when=Signal::derive(move || cur.with(Option::is_some))>
            {move || cur.get().map(|a| {
                let r = NodeRef::new();
                focus_on_open(r);
                let yes = move || close_dialog(Some(text.get_untracked()));
                view! { <div class="modal top" node_ref=r role="dialog" aria-modal="true" aria-label=a.title.clone() on:keydown=move |e| dialog_keys(&e, || close_dialog(None))>
                    <div class="card ask">
                        {crate::icons::icon(if a.input.is_some() { "edit" } else { "warning" })}
                        <h2>{a.title.clone()}</h2>
                        {(!a.text.is_empty()).then(|| view! { <p class="muted">{a.text.clone()}</p> })}
                        {a.input.is_some().then(|| view! { <input aria-label=a.title.clone() prop:value=text on:input=move |e| text.set(event_target_value(&e)) on:keydown=move |e| if is_enter(&e) { e.prevent_default(); yes() } /> })}
                        <div class="acts">
                            <button class="ghost" on:click=move |_| close_dialog(None)>"Cancel"</button>
                            {(!a.alt.is_empty()).then(|| view! { <button class="ghost" on:click=move |_| close_dialog(Some(ALT.into()))>{a.alt.clone()}</button> })}
                            {(!a.yes.is_empty()).then(|| view! { <button class:danger=a.danger on:click=move |_| yes()>{a.yes.clone()}</button> })}
                        </div>
                    </div>
                </div> }
            })}
        </Fade>
    }
}

/// Saves `href` (a data: URL) as the file `name`.
pub fn download(name: &str, href: &str) {
    let Some(a) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.create_element("a").ok())
    else {
        return;
    };
    let _ = a.set_attribute("href", href);
    let _ = a.set_attribute("download", name);
    if let Ok(a) = wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(a) {
        a.click()
    }
}

pub fn csv(v: &Value) -> String {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

pub fn from_csv(s: &str) -> Value {
    json!(s
        .split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>())
}

/// Asks for a TOTP code (or enrollment) when a request was refused with `step_up_required`. One
/// copy lives in `App`; after a good code the control that needed it is clicked again.
#[component]
pub fn StepUp(enrolled: ReadSignal<bool>) -> impl IntoView {
    let need = step_up();
    let (code, set_code) = signal(String::new());
    let (dm_code, set_dm_code) = signal(String::new());
    let (err, set_err) = signal(String::new());
    let (qr, set_qr) = signal(String::new());
    let (secret, set_secret) = signal(String::new());
    let (qr_shown, key_shown) = (RwSignal::new(false), RwSignal::new(false));
    let busy = RwSignal::new(false);
    let msg = Msg::default();
    // A resume must not run an earlier page's action on the page opened since.
    let _ = window_event_listener(leptos::ev::hashchange, |_| drop(RETRY.take()));
    let enroll = move |_| {
        spawn_local(async move {
            match api("POST", "/api/totp/enroll", None).await {
                Ok(v) => {
                    let svg = qrcode::QrCode::new(v["uri"].as_str().unwrap_or("")).map(|q| {
                        q.render::<qrcode::render::svg::Color>()
                            .min_dimensions(200, 200)
                            .build()
                    });
                    set_qr.set(svg.unwrap_or_default());
                    qr_shown.set(false);
                    key_shown.set(false);
                    set_secret.set(v["secret"].as_str().unwrap_or("").to_string());
                }
                Err(e) => set_err.set(e),
            }
        })
    };
    let close = move || {
        need.set(false);
        set_err.set(String::new());
        set_code.set(String::new());
        RETRY.take();
    };
    let access = use_context::<Access>();
    let verify = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        // The step-up counts for the server page it was opened on, and only there.
        // The Commands page has no server in its address: it saves to the server whose access
        // it shows, so the code counts for that one (not for a server another tab opened).
        let shown = || access?.pages.with_untracked(|p| p["guild"].as_str().map(str::to_string));
        let page = crate::hash().into_iter().next();
        let guild = if page.as_deref() == Some("commands") { shown() } else { page }.filter(|g| g.bytes().all(|b| b.is_ascii_digit()));
        spawn_local(async move {
            let r = api(
                "POST",
                "/api/totp/verify",
                Some(json!({"code": code.get_untracked(), "dm_code": dm_code.get_untracked(), "guild": guild})),
            )
            .await;
            busy.set(false);
            match r {
                Ok(_) => {
                    let again = RETRY.take();
                    close();
                    set_qr.set(String::new());
                    match again {
                        Some(f) => f(msg),
                        // Opened by hand (Confirm 2FA in the menu): nothing was waiting.
                        None => msg.done("Verified."),
                    }
                }
                Err(e) => set_err.set(e),
            }
        })
    };
    view! {
        <Fade when=need>
            {move || { let r = NodeRef::new(); focus_on_open(r); view! {
            <div class="modal top" node_ref=r role="dialog" aria-modal="true" aria-label="Confirm it's you" on:keydown=move |e| dialog_keys(&e, close)>
                <div class="card ask">
                    {move || if err.with(String::is_empty) { crate::icons::icon("fingerprint").into_any() } else { view! { <Pose pose=Brim::Stop /> }.into_any() }}
                    <h2>"Confirm it's you"</h2>
                    <p class="muted">"This needs the code from the Brim entry in your authenticator app, not from Brim Rescue."</p>
                    <Show when=move || !enrolled.get()><button class="ghost" on:click=enroll>"Set up authenticator"</button></Show>
                    <Show when=move || !qr.get().is_empty()>
                        <p class="info">"Scan this QR code with your authenticator app, then enter a code below."</p>
                        <div class="qr" class:blur=move || !qr_shown.get() inner_html=move || qr.get()></div>
                        <button class="ghost" on:click=move |_| qr_shown.update(|v| *v = !*v)>{move || if qr_shown.get() { "Hide QR code" } else { "Show QR code" }}</button>
                        <p class="muted">"Can't scan? Enter this key manually: " <code class:blur=move || !key_shown.get()>{move || secret.get()}</code>
                            " " <button class="ghost" on:click=move |_| key_shown.update(|v| *v = !*v)>{move || if key_shown.get() { "Hide key" } else { "Show key" }}</button></p>
                        <p class="info">"Brim also sent you a code in Discord DMs. Enter it here to confirm the setup."</p>
                        <input placeholder="Discord DM code" aria-label="code from Discord DM" autocomplete="off" prop:value=dm_code on:input=move |e| set_dm_code.set(event_target_value(&e)) />
                    </Show>
                    <input class="code" placeholder="123456" inputmode="numeric" aria-label="authenticator code" autocomplete="one-time-code" prop:value=code on:input=move |e| set_code.set(event_target_value(&e)) on:keydown=move |e| if is_enter(&e) { e.prevent_default(); verify() } />
                    <p class="err">{move || err.get()}</p>
                    <div class="acts">
                        <button class="ghost" on:click=move |_| close()>"Cancel"</button>
                        <button prop:disabled=busy on:click=move |_| verify()>{move || if busy.get() { view! { <Working /> }.into_any() } else { "Verify and continue".into_any() }}</button>
                    </div>
                </div>
            </div> } }}
        </Fade>
        <Toast msg />
    }
}

/// Page header: "Module › Page" over the big title (the server name unless `big` is given; an empty `big`
/// hides it). `module` is the module's name and link; `help` adds a "?" that opens the page's "How it works".
#[component]
pub fn Head(
    guild: String,
    page: String,
    #[prop(optional)] big: Option<String>,
    #[prop(optional_no_strip)] module: Option<(String, String)>,
    // A detail page's parent page ("Filters", link): "Module › Parent › Page".
    #[prop(optional_no_strip)] up: Option<(String, String)>,
    #[prop(optional_no_strip)] help: Option<&'static [&'static str]>,
    // Brim beside the title on wide screens: the page's kind (security, logs, keys), see `page_pose` in main.rs.
    #[prop(optional_no_strip)] pose: Option<Brim>,
) -> impl IntoView {
    let guilds = expect_context::<ReadSignal<Vec<Value>>>();
    let title = move || {
        big.clone().unwrap_or_else(|| {
            guilds
                .get()
                .iter()
                .find(|g| g["id"] == guild.as_str())
                .and_then(|g| g["name"].as_str().map(String::from))
                .unwrap_or_else(|| guild.clone())
        })
    };
    let link = |(name, href): (String, String)| view! { <a href=href>{name}</a><span class="sep" aria-hidden="true">" › "</span> };
    let crumb = (module.map(link), up.map(link));
    view! {
        <div class="head">
            {pose.map(|pose| view! { <Pose pose class="pose-head" /> })}
            {help.map(|steps| view! { <HelpPanel steps /> })}
            <small>{crumb}{page}</small>
            {move || Some(title()).filter(|t| !t.is_empty()).map(|t| view! { <h1>{t}</h1> })}
        </div>
    }
}

/// One tab row for every page: links (route tabs, `href` set) or buttons (tabs held in page state).
/// The look is fixed by the owner (quiet row, orange underline on the open tab); it lives in `.tabs` in style.css.
#[derive(Clone)]
pub struct Tab {
    pub key: &'static str,
    pub label: &'static str,
    pub href: Option<String>,
    pub icon: Option<&'static str>,
}

impl Tab {
    pub fn state(key: &'static str, label: &'static str) -> Self {
        Self { key, label, href: None, icon: None }
    }
    pub fn link(key: &'static str, label: &'static str, href: String) -> Self {
        Self { key, label, href: Some(href), icon: None }
    }
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }
}

thread_local! { static TAB_AT: std::cell::RefCell<std::collections::HashMap<&'static str, (f64, f64)>> = std::cell::RefCell::new(std::collections::HashMap::new()); static TAB_FADE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// `active` is the open tab's key. A state tab calls `on_pick` with its key; a route tab is a plain link.
/// `row`: a name for a second tab row on the same page, so each row slides from its own last place.
#[component]
pub fn PageTabs(
    tabs: Vec<Tab>,
    #[prop(into)] active: Signal<String>,
    #[prop(optional, into)] on_pick: Option<Callback<&'static str>>,
    #[prop(default = "page")] id: &'static str,
) -> impl IntoView {
    let row = NodeRef::<leptos::html::Div>::new();
    // The orange underline slides to the open tab and the content below fades in (transform and opacity only, style.css `.tabs`).
    // A route tab builds this row again on each page, so the last place is kept to slide from.
    Effect::new(move |_| {
        active.track();
        let go = move |fade: bool| {
            use wasm_bindgen::JsCast;
            let Some(row) = row.try_get_untracked().flatten() else { return };
            let row: &web_sys::HtmlElement = row.unchecked_ref();
            let Ok(Some(on)) = row.query_selector(".on") else {
                let _ = row.remove_attribute("data-slide");
                return;
            };
            let on: web_sys::HtmlElement = on.unchecked_into();
            let (x, w) = (f64::from(on.offset_left()), f64::from(on.offset_width()));
            let from = TAB_AT.with(|t| t.borrow_mut().insert(id, (x, w)));
            let first = from.is_none();
            let st = row.style();
            if let Some((fx, fw)) = from {
                let _ = st.set_property("--tx", &format!("{fx}px"));
                let _ = st.set_property("--tw", &fw.to_string());
                let _ = row.offset_width(); // the old place is drawn once, then the new one slides in
            }
            let _ = st.set_property("--tx", &format!("{x}px"));
            let _ = st.set_property("--tw", &w.to_string());
            let _ = row.set_attribute("data-slide", "");
            if fade && !first && from != Some((x, w)) {
                let mut next = row.next_element_sibling();
                let flip = TAB_FADE.with(|f| f.replace(!f.get()));
                while let Some(el) = next {
                    let c = el.class_list();
                    let _ = c.remove_2("tf-a", "tf-b");
                    let _ = row.offset_width();
                    let _ = c.add_1(if flip { "tf-a" } else { "tf-b" });
                    next = el.next_element_sibling();
                }
            }
        };
        request_animation_frame(move || go(true));
        // Fonts can still change the widths a moment after the first draw.
        set_timeout(move || go(false), Duration::from_millis(400));
    });
    view! {
        <div class="tabs" node_ref=row>
            {tabs.into_iter().map(|t| {
                let (key, ico) = (t.key, t.icon.map(crate::icons::icon));
                let on = move || active.get() == key;
                match t.href {
                    Some(h) => view! { <a class:on=on aria-current=move || on().then_some("page") href=h>{ico}{t.label}</a> }.into_any(),
                    None => view! { <button class:on=on aria-pressed=move || on().to_string() on:click=move |_| if let Some(f) = on_pick { f.run(key) }>{ico}{t.label}</button> }.into_any(),
                }
            }).collect_view()}
        </div>
    }
}

/// The header's "?" and the side panel it opens with the page's "How it works" steps.
#[component]
fn HelpPanel(steps: &'static [&'static str]) -> impl IntoView {
    let open = RwSignal::new(false);
    let close_btn = NodeRef::<leptos::html::Button>::new();
    let q_btn = NodeRef::<leptos::html::Button>::new();
    // Focus goes back to the "?" when the panel closes (read with try_, the page may be gone).
    let close = move || {
        open.set(false);
        request_animation_frame(move || {
            if let Some(Some(b)) = q_btn.try_get_untracked() {
                let _ = b.focus();
            }
        });
    };
    let show = move |_| {
        open.set(true);
        // Focus moves into the panel once it is drawn; the page may be gone by then (PITFALLS 2).
        request_animation_frame(move || {
            if let Some(Some(b)) = close_btn.try_get_untracked() {
                let _ = b.focus();
            }
        });
    };
    view! {
        <button class="ghost helpq" aria-label="How this page works" title="How it works" aria-expanded=move || open.get().to_string() node_ref=q_btn on:click=show>"?"</button>
        <Show when=move || open.get()>
            <div class="helpback" on:click=move |_| close()></div>
            <aside class="helppanel" tabindex="-1" role="dialog" aria-modal="true" aria-label="How it works" on:keydown=move |e| dialog_keys(&e, close)>
                <div class="row"><h2>"How it works"</h2><span class="sp"></span><button class="ghost" node_ref=close_btn on:click=move |_| close()>"Close"</button></div>
                <ol>{steps.iter().map(|s| view! { <li>{*s}</li> }).collect_view()}</ol>
            </aside>
        </Show>
    }
}

/// Runs a request, routing `step_up_required` to the prompt and other errors to `msg`.
pub fn run_act(
    fut: impl std::future::Future<Output = Result<Value, String>> + 'static,
    msg: Msg,
    need: RwSignal<bool>,
    ok: impl FnOnce(Value) + 'static,
) {
    run(fut, msg, need, ok, false, None)
}

/// [`run_act`] for a request started after an await (no click to repeat) or by a control that is
/// gone or rebuilt meanwhile: after a good 2FA code, `make` builds the same request again and it is sent.
pub fn run_resumable<F: std::future::Future<Output = Result<Value, String>> + 'static>(
    make: impl Fn() -> F + 'static,
    msg: Msg,
    need: RwSignal<bool>,
    ok: impl FnOnce(Value) + 'static,
) {
    resumable(make, msg, need, Box::new(ok), false)
}

/// [`run_resumable`] for a settings save, whose refusal stays up like [`run_change`].
pub fn run_change_resumable<F: std::future::Future<Output = Result<Value, String>> + 'static>(
    make: impl Fn() -> F + 'static,
    msg: Msg,
    need: RwSignal<bool>,
    ok: impl FnOnce(Value) + 'static,
) {
    resumable(make, msg, need, Box::new(ok), true)
}

fn resumable<F: std::future::Future<Output = Result<Value, String>> + 'static>(
    make: impl Fn() -> F + 'static,
    msg: Msg,
    need: RwSignal<bool>,
    ok: Box<dyn FnOnce(Value)>,
    stay: bool,
) {
    let fut = make();
    // `ok` runs once: it moves into the resume only when this try was refused for 2FA.
    let ok = std::rc::Rc::new(std::cell::Cell::new(Some(ok)));
    let (ok1, ok2) = (ok.clone(), ok);
    let again = move |_: Msg| {
        if let Some(f) = ok2.take() {
            resumable(make, msg, need, f, stay)
        }
    };
    run(fut, msg, need, move |v| if let Some(f) = ok1.take() { f(v) }, stay, Some(Box::new(again)))
}

/// [`run_act`] for a settings save: a refused save (an invalid choice) stays up until it is fixed.
pub fn run_change(
    fut: impl std::future::Future<Output = Result<Value, String>> + 'static,
    msg: Msg,
    need: RwSignal<bool>,
    ok: impl FnOnce(Value) + 'static,
) {
    run(fut, msg, need, ok, true, None)
}

fn run(
    fut: impl std::future::Future<Output = Result<Value, String>> + 'static,
    msg: Msg,
    need: RwSignal<bool>,
    ok: impl FnOnce(Value) + 'static,
    stay: bool,
    again: Option<Retry>,
) {
    let (trigger, page) = (active(), crate::hash());
    spawn_local(async move {
        let r = fut.await;
        // The page that started this may be gone, and reading its signals would panic.
        if msg.text.is_disposed() {
            return;
        }
        match r {
            Ok(v) => ok(v),
            // Refused after the user opened another page: no prompt, and nothing resumes there.
            Err(e) if e == "step_up_required" && crate::hash() != page => {}
            Err(e) if e == "step_up_required" => {
                // Nothing to repeat (sent after an await, not on run_resumable): say so, never a bare "Verified."
                let fallback = || -> Retry { Box::new(|msg: Msg| msg.err("Verified. Press the button again to finish.")) };
                RETRY.set(Some(again.or_else(|| click_again(trigger)).unwrap_or_else(fallback)));
                need.set(true)
            }
            Err(e) => {
                // A server error's code must stay readable to be copied (text from main.rs `ApiError::Internal`).
                let keep = stay || e.starts_with("Server error, code ") || (e.ends_with(')') && e.contains(" (code "));
                msg.err(e);
                msg.stay.set(keep)
            }
        }
    });
}

/// A toast's text, whether it reports a failure, and whether it stays until clicked or replaced.
#[derive(Clone, Copy, Default)]
pub struct Msg {
    text: RwSignal<String>,
    bad: RwSignal<bool>,
    stay: RwSignal<bool>,
    /// Shown big, green and centred: "Saved!", "Imported!", "Done. ..." (owner decision 2026-11-13).
    big: RwSignal<bool>,
}

impl Msg {
    pub fn ok(self, t: impl Into<String>) {
        self.big.set(false);
        self.bad.set(false);
        self.stay.set(false);
        self.text.set(t.into())
    }
    /// A finished save or fix: the big green toast.
    pub fn done(self, t: impl Into<String>) {
        self.ok(t);
        self.big.set(true)
    }
    /// A failure or a warning: it stays until clicked or replaced, so it can be read and acted on.
    pub fn err(self, e: impl Into<String>) {
        self.big.set(false);
        self.bad.set(true);
        self.stay.set(true);
        self.text.set(e.into())
    }
    pub fn clear(self) {
        self.text.set(String::new())
    }
    pub fn get(self) -> String {
        self.text.get()
    }
    /// Whether the page that owns this toast is gone; reading its signals would panic then.
    pub fn text_disposed(self) -> bool {
        self.text.is_disposed()
    }
}

/// How long a short toast stays up before it closes itself; matches the `.life` strip in style.css.
const TOAST_LIFE: Duration = Duration::from_secs(3);

/// A longer text gets more time to read: [`TOAST_LIFE`] plus 50 ms per character past 60.
fn toast_life(text: &str) -> Duration {
    TOAST_LIFE + Duration::from_millis(50 * text.chars().count().saturating_sub(60) as u64)
}

/// Message that closes itself after [`TOAST_LIFE`] (or on click), with a strip along the bottom (red
/// for failures); keeps its last text while it fades out.
#[component]
pub fn Toast(msg: Msg) -> impl IntoView {
    let text = Memo::new(move |prev: Option<&String>| {
        let m = msg.get();
        if m.is_empty() {
            prev.cloned().unwrap_or_default()
        } else {
            m
        }
    });
    let saved = msg.big;
    // Counts messages, so the strip restarts on each one and stays drawn while the toast fades out.
    let life = Memo::new(move |n: Option<&u32>| {
        let n = n.copied().unwrap_or(0);
        if msg.get().is_empty() {
            n
        } else {
            n + 1
        }
    });
    // Each message restarts the timer; a plain cell (not StoredValue) since it may fire after unmount.
    let shown = std::rc::Rc::new(std::cell::Cell::new(0u32));
    Effect::new(move || {
        if msg.get().is_empty() {
            return;
        }
        shown.set(shown.get() + 1);
        let (mine, shown) = (shown.get(), shown.clone());
        let life = msg.text.with_untracked(|t| toast_life(t));
        set_timeout(
            move || {
                if shown.get() == mine && msg.stay.try_get_untracked() == Some(false) {
                    msg.clear()
                }
            },
            life,
        );
    });
    view! { <Fade when=Signal::derive(move || !msg.get().is_empty())><p class="toast" class:bad=move || msg.bad.get() class:saved=move || saved.get() role=move || if msg.bad.get() { "alert" } else { "status" } title=move || msg.stay.get().then_some("Click to close") on:click=move |_| msg.clear()>{move || saved.get().then(|| view! { <Pose pose=Brim::ThumbsUp /> })}{move || text.get()}{move || { life.track(); view! { <span class="life" class:stay=move || msg.stay.get() style=move || format!("animation-duration:{}ms", text.with(|t| toast_life(t).as_millis()))></span> } }}</p></Fade> }
}

/// Runs a save, recording the new version.
pub fn run_save(
    fut: impl std::future::Future<Output = Result<Value, String>> + 'static,
    version: RwSignal<i64>,
    msg: Msg,
    need: RwSignal<bool>,
) {
    run_change(fut, msg, need, move |v| {
        version.set(v["version"].as_i64().unwrap_or(0));
        // A rollback names the numbers it moved into their range.
        let clamped: Vec<&str> = v["clamped"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let moved = if clamped.is_empty() {
            String::new()
        } else {
            format!(" Moved into the allowed range: {}.", clamped.join(", "))
        };
        // A rollback to before an ownership change keeps what the change paused.
        let held: Vec<&str> = v["held"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let held = if held.is_empty() {
            String::new()
        } else {
            format!(" The version is from before the ownership change, so {}. Use Keep on Overview for those.", held.join(", "))
        };
        msg.done(format!("Saved!{moved}{held}"));
    });
}

fn focusables(root: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    use wasm_bindgen::JsCast;
    let Ok(l) = root.query_selector_all(
        "button:not([disabled]), input:not([disabled]), select, textarea, a[href]",
    ) else {
        return vec![];
    };
    (0..l.length())
        .filter_map(|i| l.item(i)?.dyn_into().ok())
        .collect()
}

/// Enter that is not confirming an IME composition (CJK input), which must never submit.
pub fn is_enter(e: &web_sys::KeyboardEvent) -> bool {
    e.key() == "Enter" && !e.is_composing() && e.key_code() != 229
}

/// Escape closes a dialog and Tab cycles inside it; `e` is the dialog's own keydown.
pub fn dialog_keys(e: &leptos::ev::KeyboardEvent, close: impl FnOnce()) {
    use wasm_bindgen::JsCast;
    if e.key() == "Escape" {
        return close();
    }
    if e.key() != "Tab" {
        return;
    }
    let Some(root) = e
        .current_target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let list = focusables(&root);
    let (Some(first), Some(last)) = (list.first(), list.last()) else {
        return;
    };
    let active = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element());
    let on = |el: &web_sys::HtmlElement| active.as_ref().is_some_and(|a| a.is_same_node(Some(el)));
    let to = if e.shift_key() && on(first) {
        last
    } else if !e.shift_key() && on(last) {
        first
    } else {
        return;
    };
    e.prevent_default();
    let _ = to.focus();
}

/// Scrolls the element with `id` into view, smoothly.
pub fn scroll_to(id: &str) {
    let id = id.to_string();
    request_animation_frame(move || {
        if let Some(el) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id(&id))
        {
            let o = web_sys::ScrollIntoViewOptions::new();
            o.set_behavior(web_sys::ScrollBehavior::Smooth);
            el.scroll_into_view_with_scroll_into_view_options(&o);
        }
    });
}

/// Moves focus into a dialog once it is on the page.
pub fn focus_on_open(r: NodeRef<leptos::html::Div>) {
    Effect::new(move || {
        if let Some(d) = r.get() {
            if let Some(f) = focusables(&d).first() {
                let _ = f.focus();
            }
        }
    });
}

/// The server's per-page access (`full`, `read` or `hidden`) and the bot commands the user lacks,
/// for the last server opened. Only a hint: the API and the bot check every call.
#[derive(Clone, Copy)]
pub struct Access {
    pub pages: RwSignal<Value>,
    /// Bumped after a permit save so the access is fetched again.
    pub tick: RwSignal<u32>,
}

impl Access {
    /// Unknown pages and a not yet loaded answer count as `full`; the server still refuses. A user with
    /// no level on this server sees no page (Home offered the wizard to them). A failed answer
    /// (`failed`) hides every page and button until a retry loads it.
    pub fn of(&self, page: &str) -> String {
        self.pages
            .with(|p| if p["level"] == "none" || p["failed"] == true { "hidden" } else { p["pages"][page.split('/').next().unwrap_or(page)].as_str().unwrap_or("full") }.to_string())
    }
    /// The user's level as a rank (viewer 1 .. owner 4) for buttons on pages everyone may open.
    /// Unknown counts as owner; the server still refuses.
    pub fn rank(&self) -> u8 {
        self.pages.with(|p| match p["level"].as_str() {
            Some("none") => 0,
            _ if p["failed"] == true => 0,
            Some("viewer") => 1,
            Some("editor") => 2,
            Some("admin") => 3,
            _ => 4,
        })
    }
}

/// Copies text and says whether the browser allowed it; it refuses without focus or permission.
pub async fn copy_text(text: &str) -> bool {
    let Some(w) = web_sys::window() else {
        return false;
    };
    wasm_bindgen_futures::JsFuture::from(w.navigator().clipboard().write_text(text))
        .await
        .is_ok()
}

/// The label of a Copy button: before, after a copy, after a refused copy.
pub fn copy_label(state: Option<bool>) -> &'static str {
    match state {
        None => "Copy",
        Some(true) => "Copied!",
        Some(false) => "Copy failed, select and copy by hand",
    }
}

/// Info icon (or another `icon`) with a hover card (also on keyboard focus and tap) that explains a setting in plain words.
#[component]
pub fn Tip(
    #[prop(into)] text: Signal<String>,
    #[prop(default = "info")] icon: &'static str,
) -> impl IntoView {
    // The card is centred over the icon; near an edge it is shifted to stay inside the page area
    // (not under the sidebar or past the screen), and below the icon when the header would cover it.
    let place = |e: &web_sys::Event| {
        use wasm_bindgen::JsCast;
        let Some(t) = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let Some(b) = t
            .query_selector(".bubble")
            .ok()
            .flatten()
            .and_then(|b| b.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        let _ = t.class_list().remove_1("below");
        let _ = b.style().set_property("--dx", "0px");
        let r = b.get_bounding_client_rect();
        let w = web_sys::window()
            .and_then(|w| w.inner_width().ok())
            .and_then(|x| x.as_f64())
            .unwrap_or(r.right());
        // Inside a dialog card (which scrolls, so it clips) the card is the edge; else the page area.
        let card = t.closest(".modal .card").ok().flatten();
        let (lo, hi) = card
            .clone()
            .or_else(|| t.closest("main").ok().flatten())
            .map_or((0.0, w), |m| {
                let m = m.get_bounding_client_rect();
                (m.left(), m.right().min(w))
            });
        let top = card.map_or(88.0, |c| c.get_bounding_client_rect().top());
        let dx = if r.left() < lo + 8.0 {
            lo + 8.0 - r.left()
        } else if r.right() > hi - 8.0 {
            hi - 8.0 - r.right()
        } else {
            0.0
        };
        let _ = b.style().set_property("--dx", &format!("{}px", dx / screen_zoom()));
        if r.top() < top {
            let _ = t.class_list().add_1("below");
        }
    };
    view! { <span class="tip" tabindex="0" role="note" aria-label=text on:mouseenter=move |e| place(&e) on:focus=move |e| place(&e)>{crate::icons::icon(icon)}<span class="bubble" aria-hidden="true">{text}</span></span> }
}

/// Made-up words the [`Working`] animation shows (the owner's fun-words.md).
const FUN_WORDS: [&str; 32] = [
    "Flibbertigibbeting",
    "Hyperspacing",
    "Hocus-pocusing",
    "Topsy-turvying",
    "Gobbledygooking",
    "Jiggery-pokerying",
    "Mumbo-jumboing",
    "Abracadabraing",
    "Jamboreeing",
    "Fandangoing",
    "Helter-skeltering",
    "Hokey-pokeying",
    "Hurly-burlying",
    "Whatchamacalliting",
    "Lollapaloozing",
    "Balderdashing",
    "Malarkeying",
    "Hullaballooing",
    "Shindigging",
    "Hootenannying",
    "Sock-hopping",
    "Shenaniganing",
    "Tomfoolering",
    "Razzmatazzing",
    "Higgledy-piggledying",
    "Hanky-pankying",
    "Skullduggering",
    "Snollygostering",
    "Brouhahaing",
    "Kerfuffling",
    "Rigmaroling",
    "Willy-nillying",
];

/// A button's text that turns into the working sign while its action runs (the one loading sign).
pub fn busy_label(busy: impl Fn() -> bool + Send + Sync + 'static, label: &'static str) -> impl IntoView {
    move || if busy() { view! { <Working /> }.into_any() } else { label.into_any() }
}

/// Sets `busy` now and clears it when `fut` answers (ok, refusal or 2FA prompt): pass the result to
/// [`run_act`] and show `disabled=busy` with [`busy_label`], so a slow request cannot be sent twice.
pub fn guard<T>(busy: RwSignal<bool>, fut: impl std::future::Future<Output = T>) -> impl std::future::Future<Output = T> {
    busy.set(true);
    async move {
        let r = fut.await;
        busy.try_set(false);
        r
    }
}

/// "Working" animation: four dots in the logo colours that swap corners, and a fun word that
/// changes every 4 s (2 s changed too fast, owner decision 2026-11-18).
#[component]
pub fn Working() -> impl IntoView {
    let pick =
        || FUN_WORDS[(js_sys::Math::random() * FUN_WORDS.len() as f64) as usize % FUN_WORDS.len()];
    let word = RwSignal::new(pick());
    // A plain interval: setting a disposed signal does nothing, and the handle stops it on unmount.
    if let Ok(h) = set_interval_with_handle(move || word.set(pick()), Duration::from_secs(4)) {
        on_cleanup(move || h.clear());
    }
    view! { <span class="working"><span class="dots" aria-hidden="true"><i></i><i></i><i></i><i></i></span><span class="word">{move || format!("{}...", word.get())}</span></span> }
}

thread_local! {
    static LIMITS: ArcRwSignal<Value> = {
        let s = ArcRwSignal::new(Value::Null);
        #[cfg(target_arch = "wasm32")]
        {
            let s = s.clone();
            leptos::task::spawn_local(async move {
                if let Ok(v) = api("GET", "/api/limits", None).await { s.set(v) }
            });
        }
        s
    };
}

/// A "+" list's limit from the server (`/api/limits`): None until it has loaded.
pub fn limit(key: &str) -> Option<usize> {
    LIMITS.with(|s| s.with(|v| v[key].as_u64().map(|n| n as usize)))
}

/// Room for one more item. True while the limit loads: the server still refuses past it.
pub fn room(n: usize, key: &str) -> bool {
    limit(key).is_none_or(|max| n < max)
}

/// The list counter shown next to its "+": "3 / 10", or "3" while the limit loads.
#[component]
pub fn Count(#[prop(into)] n: Signal<usize>, key: &'static str) -> impl IntoView {
    view! { <span class="count">{move || match limit(key) { Some(max) => format!("{} / {max}", n.get()), None => n.get().to_string() }}</span> }
}

/// The one add button (design rule "Add button"): a green pill with a circle plus and a text naming the thing
/// ("Add new permit"). `class` only places it (e.g. `addf` in the message preview). Events and `attr:`
/// attributes on it (`on:click`, `attr:disabled`, `attr:aria-label`) go to the button. CI fails on a button
/// whose text starts with "+" outside this component (tools/ui-sizes.mjs).
#[component]
pub fn AddButton(label: &'static str, #[prop(optional)] class: &'static str) -> impl IntoView {
    view! { <button class=format!("add {class}")><span class="plus" aria-hidden="true">"+"</span>{label}</button> }
}

/// The row that opens and closes a group (Default messages areas, saved message categories): a card row with
/// an optional icon and line, the count and the arrow. Other attributes (`on:keydown`, `attr:...`) go to the button.
#[component]
pub fn HubRow(
    #[prop(into)] label: String,
    n: usize,
    #[prop(into)] open: Signal<bool>,
    #[prop(into)] flip: Callback<()>,
    #[prop(optional)] line: Option<&'static str>,
    #[prop(optional)] ico: Option<&'static str>,
) -> impl IntoView {
    view! { <button class="card hub" class:sub=ico.is_none() class:on=open aria-expanded=move || open.get().to_string() on:click=move |_| flip.run(())>
        {ico.map(|i| view! { <span class="ico">{crate::icons::icon(i)}</span> })}
        <span class="txt"><b>{label}</b>{line.map(|l| view! { <span class="muted">{l}</span> })}</span>
        <span class="chip">{n.to_string()}</span>
        <span class="chev">{crate::icons::icon("chevron_right")}</span>
    </button> }
}

/// The three grey lines on a sortable row (design rule "Sortable list"). A mouse drags the whole row (native drag
/// on the row); a finger drags from these lines only, so the rest of the row still scrolls the page.
/// shortcut: the finger drag is provisional (owner decision 2026-11-20); to remove it, delete the pointer handlers and `.touch-grip`.
#[component]
pub fn Grip(
    i: usize,
    drag: RwSignal<Option<usize>>,
    over: RwSignal<Option<usize>>,
    #[prop(into)] drop: Callback<(usize, usize)>,
) -> impl IntoView {
    use wasm_bindgen::JsCast;
    let armed = StoredValue::new(false);
    let end = move || {
        armed.set_value(false);
        drag.set(None);
        over.set(None);
    };
    view! { <span class="grip touch-grip" aria-hidden="true" title="Drag to move"
        on:pointerdown=move |e| if e.pointer_type() != "mouse" {
            e.prevent_default();
            if let Some(t) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) { let _ = t.set_pointer_capture(e.pointer_id()); }
            armed.set_value(true);
        }
        // The row lifts on the first move, so a plain tap does not flash it.
        on:pointermove=move |e| if armed.get_value() { drag.set(Some(i)); over.set(row_at(e.client_x(), e.client_y())) }
        on:pointerup=move |_| { if armed.get_value() && drag.get_untracked() == Some(i) { if let Some(to) = over.get_untracked() { drop.run((i, to)) } } end() }
        on:pointercancel=move |_| end()>
        {crate::icons::icon("menu")}</span> }
}

/// Finger drag on a row that is already selected (owner decision 2026-11-21): a tap selects, then a press held
/// still on the selected row lifts it and the finger slides it to its place. A finger that moves first
/// scrolls the page, so scrolling never moves a row. A mouse keeps the native drag on the row.
/// Every place a row can go carries `data-drop`; `over` and `drop` get that value (None: nowhere).
#[derive(Clone, Copy)]
pub struct HoldDrag {
    timer: StoredValue<Option<TimeoutHandle>>,
    row: StoredValue<Option<web_sys::HtmlElement>, LocalStorage>,
    /// Where the finger went down: x, y and the page's scroll then.
    from: StoredValue<(i32, i32, f64)>,
    lifted: StoredValue<bool>,
}

impl HoldDrag {
    const HOLD: Duration = Duration::from_millis(400);
    /// How far (px) a finger may wander during the hold before it counts as a scroll.
    const SLOP: i32 = 10;

    pub fn new() -> Self {
        Self { timer: StoredValue::new(None), row: StoredValue::new_local(None), from: StoredValue::new((0, 0, 0.0)), lifted: StoredValue::new(false) }
    }

    pub fn start(&self, e: &web_sys::TouchEvent, selected: bool, lift: impl Fn() + 'static) {
        // A touch outlives the page it started on: its row's handlers still run after a page change.
        if self.lifted.is_disposed() { return }
        self.stop();
        let Some(t) = e.touches().get(0).filter(|_| selected && e.touches().length() == 1) else { return };
        self.from.set_value((t.client_x(), t.client_y(), window().scroll_y().unwrap_or(0.0)));
        self.row.set_value(e.current_target().and_then(|t| wasm_bindgen::JsCast::dyn_into(t).ok()));
        let lifted = self.lifted;
        self.timer.set_value(set_timeout_with_handle(move || if !lifted.is_disposed() { lifted.set_value(true); lift() }, Self::HOLD).ok());
    }

    pub fn moved(&self, e: &web_sys::TouchEvent, over: impl Fn(Option<String>)) {
        if self.lifted.is_disposed() { return }
        let Some(t) = e.touches().get(0) else { return };
        let (x, y) = (t.client_x(), t.client_y());
        if !self.lifted.get_value() {
            let (x0, y0, _) = self.from.get_value();
            if (x - x0).abs() > Self::SLOP || (y - y0).abs() > Self::SLOP { self.stop() }
            return;
        }
        // A lifted row: the finger moves the row, not the page; near the top or bottom edge the page scrolls.
        e.prevent_default();
        // The row follows the finger (zoomed px, see --z in style.css) and lets the place under it be found.
        self.style(&[("translate", &format!("0 calc({}px / var(--z))", { let (_, y0, s0) = self.from.get_value(); f64::from(y - y0) + window().scroll_y().unwrap_or(0.0) - s0 })), ("pointer-events", "none"), ("position", "relative"), ("z-index", "5")]);
        let h = window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
        if y < 64 { window().scroll_by_with_x_and_y(0.0, -12.0) } else if y > h - 64 { window().scroll_by_with_x_and_y(0.0, 12.0) }
        over(drop_at(x, y));
    }

    /// Returns true when a lifted row was let go (the page then gets no click from this touch).
    pub fn end(&self, e: &web_sys::TouchEvent, drop: impl FnOnce(Option<String>)) -> bool {
        if self.lifted.is_disposed() { return false }
        let lifted = self.lifted.get_value();
        self.stop();
        if lifted {
            e.prevent_default();
            drop(e.changed_touches().get(0).and_then(|t| drop_at(t.client_x(), t.client_y())));
        }
        lifted
    }

    pub fn stop(&self) {
        if self.lifted.is_disposed() { return }
        if let Some(h) = self.timer.try_update_value(Option::take).flatten() { h.clear() }
        self.lifted.set_value(false);
        self.style(&[("translate", ""), ("pointer-events", ""), ("position", ""), ("z-index", "")]);
    }

    fn style(&self, props: &[(&str, &str)]) {
        self.row.with_value(|r| if let Some(r) = r {
            for (k, v) in props { let _ = r.style().set_property(k, v); }
        });
    }
}

/// The `data-drop` place under a finger.
fn drop_at(x: i32, y: i32) -> Option<String> {
    document().element_from_point(x as f32, y as f32)?.closest("[data-drop]").ok()??.get_attribute("data-drop")
}

/// The sortable row (`data-i`) under a finger.
fn row_at(x: i32, y: i32) -> Option<usize> {
    document().element_from_point(x as f32, y as f32)?.closest("[data-i]").ok()??.get_attribute("data-i")?.parse().ok()
}

/// Where the settings cards grid splits: the first `k` cards make the left column, the rest the right one,
/// so the columns are as close to equal height as the order allows (weights are rows per card).
/// Ties keep the left column fuller, so a single card sits on the left.
pub fn split_at(weights: &[usize]) -> usize {
    let total: usize = weights.iter().sum();
    let (mut left, mut best) = (0, (usize::MAX, 0));
    for k in 0..=weights.len() {
        let d = left.max(total - left);
        if d <= best.0 { best = (d, k) }
        left += weights.get(k).copied().unwrap_or(0);
    }
    best.1
}

/// The settings cards grid (design rule "Cards grid"): two fixed columns from 900 px, one below. Each card sits
/// right under the card above it, so there are no holes; the order stays the list order (left column, then right).
/// `cards` are (weight, card): the weight is about the card's rows, used to balance the columns.
#[component]
pub fn Cards(cards: Vec<(usize, AnyView)>) -> impl IntoView {
    let k = split_at(&cards.iter().map(|(w, _)| *w).collect::<Vec<_>>());
    let mut left: Vec<AnyView> = cards.into_iter().map(|(_, c)| c).collect();
    let right = left.split_off(k);
    view! { <div class="cards"><div class="col">{left}</div><div class="col">{right}</div></div> }
}

/// An empty "+" list: what to do first, and the add button that makes the first item.
#[component]
pub fn Empty(
    title: &'static str,
    hint: &'static str,
    label: &'static str,
    #[prop(into)] add: Callback<()>,
    #[prop(optional, into)] disabled: Signal<bool>,
) -> impl IntoView {
    view! { <div class="card empty pointing">
        <Pose pose=Brim::PointingRight />
        <div>
            <h3>{title}</h3>
            <p class="muted">{hint}</p>
            <AddButton label attr:disabled=disabled on:click=move |_| add.run(()) />
        </div>
    </div> }
}

#[cfg(test)]
mod tests {
    use super::split_at;

    #[test]
    fn cards_split_into_two_even_columns_in_order() {
        assert_eq!(split_at(&[]), 0);
        assert_eq!(split_at(&[5]), 1); // one card: left column
        assert_eq!(split_at(&[3, 3]), 1);
        assert_eq!(split_at(&[10, 2, 2, 2]), 1); // a tall card alone on the left
        assert_eq!(split_at(&[2, 2, 2, 10]), 3);
        assert_eq!(split_at(&[4, 4, 4]), 2); // ties keep the left fuller
    }
}

/// The screen scale (`--z` in style.css): window coordinates are real pixels, lengths set in CSS are zoomed ones.
pub fn screen_zoom() -> f64 {
    zoom_value().unwrap_or(1.0)
}

fn zoom_value() -> Option<f64> {
    let w = web_sys::window()?;
    let root = w.document()?.document_element()?;
    let v = w.get_computed_style(&root).ok()??.get_property_value("--z").ok()?;
    v.trim().parse().ok()
}

/// Brim's poses (design system "Brim poses"; files in assets/mascot/poses, 384 px on the long side).
/// Each pose has one meaning, so a pose is used only for its state.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Brim {
    OnGuard,
    Stop,
    PointingLeft,
    PointingRight,
    ThumbsUp,
    AsleepOnDuty,
    LookingCloser,
    Shrug,
    Waving,
    Alarm,
    HoldingAKey,
    Peeking,
}

impl Brim {
    pub const ALL: [Brim; 12] = [
        Brim::OnGuard, Brim::Stop, Brim::PointingLeft, Brim::PointingRight, Brim::ThumbsUp, Brim::AsleepOnDuty,
        Brim::LookingCloser, Brim::Shrug, Brim::Waving, Brim::Alarm, Brim::HoldingAKey, Brim::Peeking,
    ];
    /// The pose's name and the one place it is used (the style page #/ui lists them).
    pub fn rule(self) -> (&'static str, &'static str) {
        match self {
            Brim::OnGuard => ("On Guard", "Security pages: Anti-nuke and the panic pages."),
            Brim::Stop => ("Stop", "No access to a page, a wrong 2FA code, and a closed or failed appeal page."),
            Brim::PointingLeft => ("Pointing Left", "A setup step, pointing at its button."),
            Brim::PointingRight => ("Pointing Right", "An empty list, pointing at the button that adds the first item."),
            Brim::ThumbsUp => ("Thumbs Up", "Saved, setup done, an appeal sent, or nothing to appeal."),
            Brim::AsleepOnDuty => ("Asleep On Duty", "Only while Brim is offline or restarting for an update."),
            Brim::LookingCloser => ("Looking Closer", "Search with no hits, the log pages and the dashboard log."),
            Brim::Shrug => ("Shrug", "A page or module that does not exist, or appeals that are off."),
            Brim::Waving => ("Waving", "A welcome: the server list when there is no server yet."),
            Brim::Alarm => ("Alarm", "A raid or anti-nuke alert, such as panic mode on."),
            Brim::HoldingAKey => ("Holding A Key", "Rescue Key and Secure my account."),
            Brim::Peeking => ("Peeking", "The bottom of the dashboard menu."),
        }
    }
    /// (file name without .webp, width, height): the size keeps the space while the picture loads.
    pub fn file(self) -> (&'static str, u32, u32) {
        match self {
            Brim::OnGuard => ("on-guard", 384, 316),
            Brim::Stop => ("stop", 384, 316),
            Brim::PointingLeft => ("pointing-left", 384, 312),
            Brim::PointingRight => ("pointing-right", 384, 295),
            Brim::ThumbsUp => ("thumbs-up", 384, 302),
            Brim::AsleepOnDuty => ("asleep-on-duty", 384, 325),
            Brim::LookingCloser => ("looking-closer", 384, 296),
            Brim::Shrug => ("shrug", 384, 364),
            Brim::Waving => ("waving", 384, 310),
            Brim::Alarm => ("alarm", 384, 355),
            Brim::HoldingAKey => ("holding-a-key", 384, 294),
            Brim::Peeking => ("peeking", 384, 212),
        }
    }
}

/// Brim in a pose, beside the state it belongs to. Decoration only: the text next to it says everything,
/// so screen readers skip it. The class sets the height (`--pose` in style.css); the width follows.
#[component]
pub fn Pose(pose: Brim, #[prop(optional)] class: &'static str) -> impl IntoView {
    let (name, w, h) = pose.file();
    view! { <img class=format!("pose {class}") src=format!("/poses/{name}.webp") alt="" aria-hidden="true" width=w height=h decoding="async" /> }
}

#[cfg(test)]
mod pose_tests {
    use super::Brim;

    #[test]
    fn every_pose_has_its_file_at_its_size() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/mascot/poses");
        for p in Brim::ALL {
            let (name, w, h) = p.file();
            let b = std::fs::read(format!("{dir}/{name}.webp")).unwrap_or_else(|_| panic!("{name}.webp is missing"));
            // VP8X header: canvas width-1 and height-1 as 24-bit little endian at bytes 24 and 27.
            assert_eq!(&b[12..16], b"VP8X", "{name}");
            let n = |i: usize| (b[i] as u32 | (b[i + 1] as u32) << 8 | (b[i + 2] as u32) << 16) + 1;
            assert_eq!((n(24), n(27)), (w, h), "{name}");
        }
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), Brim::ALL.len(), "a pose file has no Brim variant");
    }
}

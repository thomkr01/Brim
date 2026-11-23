//! Message Builder: a list of saved messages, and a builder with Message, Preview and Send tabs.
//! A message is text, up to 10 embeds and up to 5 rows of link buttons or select menus; it is typed
//! on the message itself, the bot posts it and later edits it in place.
use crate::common::*;
use crate::msgparts::{chips, Dest, Parts, Sub};
use leptos::{ev, portal::Portal, prelude::*, task::spawn_local};
use serde_json::{json, Value};
use wasm_bindgen::JsCast;

/// The string at a JSON pointer ("" when missing).
fn get_str(c: &Value, ptr: &str) -> String {
    c.pointer(ptr)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Writes `v` at `ptr`, making the objects on the way; empty removes the key, and an object it
/// leaves empty inside an embed (author, footer, image, thumbnail), since Discord rejects "".
fn put(root: &mut Value, ptr: &str, v: String) {
    let Some((parent, key)) = ptr.rsplit_once('/') else {
        return;
    };
    let mut cur = &mut *root;
    for seg in parent.split('/').skip(1) {
        cur = match (seg.parse::<usize>(), cur.is_array()) {
            (Ok(i), true) => match cur.get_mut(i) {
                Some(c) => c,
                None => return,
            },
            _ => {
                // A stored embed may hold `"image": "url"`; indexing a string by key panics.
                if !cur.is_object() {
                    *cur = json!({});
                }
                if !cur[seg].is_object() && !cur[seg].is_array() {
                    cur[seg] = json!({});
                }
                &mut cur[seg]
            }
        };
    }
    if !v.is_empty() {
        // Typed JSON can put text or a number where an object belongs: then nothing is written.
        if let Some(o) = cur.as_object_mut() {
            o.insert(key.into(), json!(v));
        }
        return;
    }
    if let Some(o) = cur.as_object_mut() {
        o.remove(key);
    }
    if let Some((gp, pk)) = parent.rsplit_once('/') {
        let emptied = root
            .pointer(parent)
            .and_then(Value::as_object)
            .is_some_and(|o| o.is_empty());
        if emptied && !gp.is_empty() && pk.parse::<usize>().is_err() {
            if let Some(o) = root.pointer_mut(gp).and_then(Value::as_object_mut) {
                o.remove(pk);
            }
        }
    }
}

fn blank() -> Value {
    json!({"id": null, "name": "", "channel": null, "pings": "none", "webhook": null, "body": {"content": "", "embeds": [{"color": 0x0d9df8}], "components": []}})
}

/// An embed with nothing but a colour is left out on save, so a text-only message stays text only.
fn body_to_save(c: &Value) -> Value {
    let mut b = c["body"].clone();
    if let Some(a) = b["embeds"].as_array_mut() {
        a.retain(|e| {
            e.as_object()
                .is_some_and(|o| o.keys().any(|k| k != "color"))
        });
    }
    b
}

/// `msg` with the buttons Brim adds itself (`c["added"]`, rows as Discord takes them), marked so
/// the preview draws them as not editable.
fn with_added(c: &Value, mut msg: Value) -> Value {
    let mut rows = msg["components"].as_array().cloned().unwrap_or_default();
    for mut r in c["added"].as_array().cloned().unwrap_or_default() {
        for b in r["components"].as_array_mut().into_iter().flatten() {
            b["added"] = json!(true);
        }
        rows.push(r);
    }
    msg["components"] = json!(rows);
    msg
}

/// The one notice for parts Brim adds when it posts (`c["added"]`): none when it adds nothing.
/// Shown in the editor and beside the preview, with the same words.
pub fn added_notice(c: &Value) -> Option<AnyView> {
    let names: Vec<String> = c["added"]
        .as_array()?
        .iter()
        .flat_map(|r| r["components"].as_array().into_iter().flatten())
        .filter_map(|b| b["label"].as_str().map(String::from))
        .collect();
    (!names.is_empty()).then(|| view! { <p class="added-note">{crate::icons::icon("info")}<span><b>"Brim adds these buttons itself when it posts: "</b>{names.join(", ")}". They are not part of the message text, so you cannot edit or press them here."</span></p> }.into_any())
}

/// The message preview on the right, as members see it, and the editor on the left behind an
/// "Edit message" button. One shared layout for every place that has a message; on a phone the
/// preview sits on top and the editor opens below it.
pub fn split(
    editing: RwSignal<bool>,
    can_edit: bool,
    cur: RwSignal<Value>,
    message: impl Fn() -> AnyView + Copy + Send + Sync + 'static,
    preview: impl Fn() -> AnyView + Copy + Send + Sync + 'static,
) -> AnyView {
    view! {
        <div class="msplit">
            <div class="mside">
                {move || if !can_edit {
                    ().into_any()
                } else if editing.get() {
                    view! {
                        <div class="mtop"><b>"Edit message"</b><span class="sp"></span>
                            <button class="ghost" on:click=move |_| editing.set(false)>"Close editor"</button></div>
                        {move || cur.with(added_notice)}
                        {message()}
                    }.into_any()
                } else {
                    view! { <button class="editbtn" on:click=move |_| editing.set(true)>{crate::icons::icon("edit")}"Edit message"</button> }.into_any()
                }}
            </div>
            <div class="mview">{preview()}</div>
        </div>
    }.into_any()
}

/// What a save compares: the parts the user edits.
fn snapshot(c: &Value) -> Value {
    json!([c["name"], c["body"], c["channel"], c["pings"], c["webhook"]])
}

fn https(u: &str) -> bool {
    u.strip_prefix("https://")
        .is_some_and(|r| !r.trim().is_empty())
}

/// Toolbar wraps: (label, tooltip, open, close, shortcut key, needs Shift). Keys follow Discord and Word.
const WRAPS: [(&str, &str, &str, &str, &str, bool); 6] = [
    ("B", "Bold (Ctrl+B)", "**", "**", "b", false),
    ("I", "Italic (Ctrl+I)", "*", "*", "i", false),
    ("U", "Underline (Ctrl+U)", "__", "__", "u", false),
    ("S", "Strikethrough (Ctrl+Shift+X)", "~~", "~~", "x", true),
    ("▒", "Spoiler (Ctrl+Shift+P)", "||", "||", "p", true),
    ("</>", "Code (Ctrl+E)", "`", "`", "e", false),
];
const LINES: [(&str, &str, &str); 4] = [
    ("H", "Heading", "## "),
    ("❝", "Quote", "> "),
    ("•", "List", "- "),
    ("ˢ", "Small text", "-# "),
];

pub(crate) fn exec(cmd: &str, v: &str) {
    if let Ok(d) = document().dyn_into::<web_sys::HtmlDocument>() {
        let _ = d.exec_command_with_show_ui_and_value(cmd, false, v);
    }
}

/// The focused text box of the message that renders markdown (`data-md`).
fn md_box() -> Option<web_sys::HtmlElement> {
    document()
        .active_element()
        .filter(|e| e.has_attribute("data-md"))
        .and_then(|e| e.dyn_into().ok())
}

/// Wraps the selection in `o`…`c`, or unwraps it when it already is. `insertText` keeps Ctrl+Z working.
fn wrap_sel(o: &str, c: &str) {
    let (Some(_), Some(sel)) = (md_box(), window().get_selection().ok().flatten()) else {
        return;
    };
    let t = String::from(sel.to_string());
    let inner = t
        .strip_prefix(o)
        .and_then(|r| r.strip_suffix(c))
        .filter(|_| t.len() >= o.len() + c.len());
    exec(
        "insertText",
        &inner.map_or_else(|| format!("{o}{t}{c}"), String::from),
    );
}

/// Puts `p` at the start of the line with the cursor.
fn line_sel(p: &str) {
    let (Some(_), Some(sel)) = (md_box(), window().get_selection().ok().flatten()) else {
        return;
    };
    let _ = sel.collapse_to_start();
    let _ = sel.modify("move", "backward", "lineboundary");
    exec("insertText", p);
}

fn link() {
    let (Some(el), Some(sel)) = (md_box(), window().get_selection().ok().flatten()) else {
        return;
    };
    // The dialog takes the focus, so the selection is kept and put back before the link goes in.
    let Ok(range) = sel.get_range_at(0) else {
        return;
    };
    let t = String::from(range.to_string());
    ask(
        Ask::new("Add a link")
            .text("Only https:// links. Members see the text, a click opens the link.")
            .input("https://")
            .yes("Add link"),
        move |url| {
            if !https(&url) {
                return;
            }
            let _ = el.focus();
            if let Some(sel) = window().get_selection().ok().flatten() {
                let _ = sel.remove_all_ranges();
                let _ = sel.add_range(&range);
            }
            let t = if t.is_empty() { url.clone() } else { t };
            exec("insertText", &format!("[{t}]({url})"));
        },
    )
}

fn on_key(e: ev::KeyboardEvent) {
    if !(e.ctrl_key() || e.meta_key()) {
        return;
    }
    let k = e.key().to_lowercase();
    if k == "k" {
        e.prevent_default();
        return link();
    }
    if let Some(&(.., o, c, _, _)) = WRAPS.iter().find(|w| w.4 == k && w.5 == e.shift_key()) {
        e.prevent_default();
        wrap_sel(o, c);
    }
}

/// A text box's content back to text: line breaks and chips (`data-tok`) become text again.
fn serialize(el: &web_sys::Element) -> String {
    let Some(c) = el
        .clone_node_with_deep(true)
        .ok()
        .and_then(|n| n.dyn_into::<web_sys::Element>().ok())
    else {
        return String::new();
    };
    let each = |sel: &str, f: &dyn Fn(&web_sys::Element)| {
        if let Ok(l) = c.query_selector_all(sel) {
            for i in 0..l.length() {
                if let Some(e) = l.item(i).and_then(|n| n.dyn_into().ok()) {
                    f(&e)
                }
            }
        }
    };
    each("[data-tok]", &|e| {
        let _ = e.replace_with_with_str_1(&e.get_attribute("data-tok").unwrap_or_default());
    });
    each("br", &|e| e.set_text_content(Some("\n")));
    // A browser may wrap a new line in a <div>.
    each("div", &|e| {
        if e.previous_sibling().is_some() {
            let _ = e.insert_adjacent_text("afterbegin", "\n");
        }
    });
    c.text_content()
        .unwrap_or_default()
        .trim_end_matches('\n')
        .to_string()
}

/// One text part of the message, typed in place (contenteditable). It is only redrawn while it does
/// not have the focus, so typing never moves the cursor.
#[component]
fn Ed(
    cur: RwSignal<Value>,
    #[prop(into)] ptr: String,
    ph: &'static str,
    max: usize,
    #[prop(optional)] multi: bool,
    #[prop(optional)] md: bool,
    #[prop(optional)] req: bool,
    /// Text Brim does not fill in (menu names): `{` types a plain brace.
    #[prop(optional)]
    novars: bool,
    /// A menu answer: member variables are allowed.
    #[prop(optional)]
    answer: bool,
) -> impl IntoView {
    let el = NodeRef::<leptos::html::Div>::new();
    let p = StoredValue::new(ptr);
    let parts = use_context::<Parts>();
    let val = Memo::new(move |_| cur.with(|c| get_str(c, &p.read_value())));
    Effect::new(move |_| {
        let v = val.get();
        if let Some(p) = parts {
            p.roles.track(); // role chips show the role's name
        }
        let Some(e) = el.get() else { return };
        let focused = document()
            .active_element()
            .is_some_and(|a| a.is_same_node(Some(&e)));
        let html = chips(&v);
        if !focused && e.inner_html() != html {
            e.set_inner_html(&html);
        }
    });
    let input = move |_| {
        let Some(e) = el.get() else { return };
        let v = serialize(&e);
        if v.is_empty() {
            e.set_inner_html(""); // leaves no <br>, so the grey name shows again
        }
        cur.update(|c| put(c, &p.read_value(), v));
    };
    let key = move |e: ev::KeyboardEvent| {
        if let (Some(p), "{", false) = (parts, e.key().as_str(), novars) {
            p.on_brace();
        }
        if crate::common::is_enter(&e) {
            e.prevent_default();
            if multi {
                exec("insertLineBreak", "");
            }
        }
    };
    // Pasted text comes in as plain text, never as another page's HTML.
    let paste = move |e: ev::ClipboardEvent| {
        e.prevent_default();
        let t = e
            .clipboard_data()
            .and_then(|d| d.get_data("text/plain").ok())
            .unwrap_or_default();
        exec(
            "insertText",
            &if multi {
                t
            } else {
                t.replace(['\r', '\n'], " ")
            },
        );
    };
    let over = move || val.with(|v| v.chars().count() > max);
    let title = move || over().then(|| format!("Longer than {max} characters"));
    view! {
        <div node_ref=el class="ed" contenteditable="true" spellcheck="false" role="textbox" aria-label=ph data-ph=ph
            aria-multiline=multi.to_string() data-md=md.then_some("") data-answer=answer.then_some("")
            class:need=move || over() || (req && val.with(|v| v.trim().is_empty())) title=title
            on:input=input on:keydown=key on:paste=paste
            on:click=move |e| if let (Some(p), Some(d)) = (parts, el.get()) { p.chip_click(d.unchecked_into(), &e) }></div>
    }
}

/// Discord's "is this link safe" step, for every link a click opens in the preview.
fn open_link(url: String) {
    // A typed `javascript:` link would run with the dashboard's session.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    let text = format!("This link goes to {url}");
    ask(
        Ask::new("Open this link?").text(text).yes("Open link"),
        move |_| {
            let _ = window().open_with_url_and_target_and_features(&url, "_blank", "noopener");
        },
    )
}

/// The ready-made buttons a server's config offers: (action, what it does).
fn ready_buttons(config: &Value) -> Vec<(String, String)> {
    let tickets = &config["tickets"];
    let mut list: Vec<(String, String)> = tickets["panels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| tickets["enabled"] == true && p["enabled"] != false)
        .filter_map(|p| {
            Some((
                format!("ticket:{}", p["id"].as_str()?),
                format!("Open a ticket: {}", p["name"].as_str().unwrap_or("")),
            ))
        })
        .collect();
    let v = &config["verification"];
    if v["enabled"] == true && v["mode"] == "Button" {
        list.push(("rules".into(), "I accept the rules".into()));
    }
    list
}

/// Clicks in the preview act as in Discord: links and buttons ask first, spoilers open.
fn preview_click(e: ev::MouseEvent) {
    let Some(t) = e
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    if let Some(s) = t.closest(".spoiler").ok().flatten() {
        let _ = s.class_list().add_1("rev");
        return;
    }
    let url = t
        .closest("[data-url], a[href]")
        .ok()
        .flatten()
        .and_then(|a| {
            a.get_attribute("data-url")
                .or_else(|| a.get_attribute("href"))
        });
    if let Some(url) = url {
        e.prevent_default();
        open_link(url);
    }
}

/// The preview's menus act as in Discord: a click opens one, a pick shows its answer only to the
/// member, Dismiss message hides it again.
fn preview_menu(e: ev::MouseEvent, answer: RwSignal<Option<(usize, usize)>>) {
    let Some(t) = e
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let near = |s: &str| t.closest(s).ok().flatten();
    if let Some(o) = near("[data-opt]") {
        let v = o.get_attribute("data-opt").unwrap_or_default();
        if let Some((r, o)) = v
            .split_once(':')
            .and_then(|(r, o)| Some((r.parse().ok()?, o.parse().ok()?)))
        {
            answer.set(Some((r, o)));
        }
        if let Some(m) = near(".dmenu") {
            let _ = m.class_list().remove_1("open");
        }
    } else if near("[data-dismiss]").is_some() {
        answer.set(None);
    } else if let Some(m) = near(".dmenu") {
        let _ = m.class_list().toggle("open");
    } else {
        preview_click(e)
    }
}

/// A change to a list inside the message (a menu's options).
type EditList = Box<dyn FnOnce(&mut Vec<Value>)>;

/// Which popup is open: an image or link slot (embed, slot), the colour (embed), the footer time
/// (embed), a button (row, button), a menu option's answer (row, option).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Pop {
    Slot(usize, usize),
    Color(usize),
    Time(usize),
    Button(usize, usize),
    Answer(usize, usize),
}

/// (key, JSON pointer inside an embed, title, is an image).
const SLOTS: [(&str, &str, &str, bool); 6] = [
    ("thumb", "/thumbnail/url", "Thumbnail (top right)", true),
    ("image", "/image/url", "Large image", true),
    ("author_icon", "/author/icon_url", "Author icon", true),
    ("footer_icon", "/footer/icon_url", "Footer icon", true),
    ("author_url", "/author/url", "Author link", false),
    ("title_url", "/url", "Title link", false),
];

fn slot_ptr(k: usize, s: usize) -> String {
    format!("/body/embeds/{k}{}", SLOTS[s].1)
}

const PRESETS: [u32; 9] = [
    0xFFA36C, 0xFF6F91, 0x9B7BFF, 0x4FC3F7, 0x0d9df8, 0x00db5f, 0xff9900, 0xff2445, 0xa1a1a1,
];

/// Hue 0..360, saturation and value 0..1, as the colour square shows them.
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> u32 {
    let f = |n: f64| {
        let k = (n + h / 60.0) % 6.0;
        (255.0 * (v - v * s * k.min(4.0 - k).clamp(0.0, 1.0))).round() as u32
    };
    (f(5.0) << 16) | (f(3.0) << 8) | f(1.0)
}

fn rgb_to_hsv(c: u32) -> (f64, f64, f64) {
    let (r, g, b) = (
        (c >> 16 & 255) as f64 / 255.0,
        (c >> 8 & 255) as f64 / 255.0,
        (c & 255) as f64 / 255.0,
    );
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, if mx == 0.0 { 0.0 } else { d / mx }, mx)
}

/// Days in a month (1..=12), leap years included, as a calendar shows them.
fn days_in(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 0 = Monday for the 1st of a month (Sakamoto's method).
fn first_weekday(y: i32, m: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let sun0 = (y + y / 4 - y / 100 + y / 400 + T[m as usize - 1] + 1).rem_euclid(7);
    ((sun0 + 6) % 7) as u32
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A real month: weekdays, ‹ › to browse, and only the days that month has.
#[component]
pub(crate) fn Cal(
    ym: RwSignal<(i32, u32)>,
    #[prop(into)] picked: Signal<Option<(i32, u32, u32)>>,
    pick: impl Fn(u32) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let step = move |d: i32| {
        ym.update(|(y, m)| {
            let n = *y * 12 + *m as i32 - 1 + d;
            *y = n.div_euclid(12);
            *m = n.rem_euclid(12) as u32 + 1;
        })
    };
    view! {
        <div class="mo"><button class="ghost" aria-label="Previous month" on:click=move |_| step(-1)>"‹"</button>
            <b>{move || { let (y, m) = ym.get(); format!("{} {y}", MONTHS[m as usize - 1]) }}</b>
            <button class="ghost" aria-label="Next month" on:click=move |_| step(1)>"›"</button></div>
        <div class="cal">
            {["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].map(|d| view! { <b>{d}</b> }).collect_view()}
            {move || { let (y, m) = ym.get(); (0..first_weekday(y, m)).map(|_| view! { <span></span> }).collect_view() }}
            {move || { let (y, m) = ym.get(); (1..=days_in(y, m)).map(|d| view! {
                <button class:on=move || picked.get() == Some((y, m, d)) on:click=move |_| pick(d)>{d}</button> }).collect_view() }}
        </div>
    }
}

/// The popups' content. Each writes straight into the message.
fn pop_body(
    cur: RwSignal<Value>,
    pop: RwSignal<Option<(Pop, f64, f64)>>,
    p: Pop,
    recent: RwSignal<Vec<u32>>,
    parts: Parts,
) -> AnyView {
    let close = move || pop.set(None);
    let head = move |icon: &'static str, title: String| {
        view! {
            <h3>{crate::icons::icon(icon)}{title}<span class="sp"></span><button class="ghost" aria-label="Close" on:click=move |_| close()>"✕"</button></h3>
        }
    };
    match p {
        Pop::Slot(k, s) => {
            let (_, _, title, img) = SLOTS[s];
            let ptr = slot_ptr(k, s);
            let v = RwSignal::new(cur.with_untracked(|c| get_str(c, &ptr)));
            let had = !v.get_untracked().is_empty();
            let ptr = StoredValue::new(ptr);
            let done = move || {
                let x = v.get_untracked().trim().to_string();
                if x.is_empty() || https(&x) {
                    cur.update(|c| put(c, &ptr.read_value(), x));
                    close();
                }
            };
            view! {
                {head(if img { "image" } else { "link" }, title.into())}
                <input placeholder="https://" aria-label=title prop:value=move || v.get() class:need=move || v.with(|x| !x.trim().is_empty() && !https(x.trim()))
                    on:input=move |e| v.set(event_target_value(&e)) on:keydown=move |e: ev::KeyboardEvent| if crate::common::is_enter(&e) { done() } />
                <p class="muted small">{if img { "A link to a PNG, JPG, GIF or WebP image." } else { "Where a click on it goes." }}" Must start with https://"</p>
                <div class="row">
                    {had.then(|| view! { <button class="ghost" on:click=move |_| { cur.update(|c| put(c, &ptr.read_value(), String::new())); close() }>"Remove"</button> })}
                    <span class="sp"></span><button on:click=move |_| done()>"Done"</button>
                </div>
            }.into_any()
        }
        Pop::Button(r, b) => {
            let ptr = move |f: &str| format!("/body/components/{r}/components/{b}/{f}");
            let get = move |f: &'static str| move || cur.with(|c| get_str(c, &ptr(f)));
            let remove = move || {
                cur.update(|c| {
                    if let Some(rows) = c["body"]["components"].as_array_mut() {
                        if let Some(bs) = rows
                            .get_mut(r)
                            .and_then(|x| x.get_mut("components"))
                            .and_then(Value::as_array_mut)
                        {
                            if b < bs.len() {
                                bs.remove(b);
                            }
                            if bs.is_empty() {
                                rows.remove(r);
                            }
                        }
                    }
                });
                close()
            };
            let action = move || get("action")();
            // Sets what the button does; the link or the action goes, so only one is saved.
            let set_action = move |a: String| {
                let name = parts
                    .ready
                    .with_untracked(|l| l.iter().find(|(k, _)| *k == a).map(|(_, n)| n.clone()));
                cur.update(|c| {
                    let Some(o) = c
                        .pointer_mut(ptr("").trim_end_matches('/'))
                        .and_then(Value::as_object_mut)
                    else {
                        return;
                    };
                    if a.is_empty() {
                        o.remove("action");
                        o.insert("style".into(), json!(5));
                        o.insert("url".into(), json!(""));
                        return;
                    }
                    o.remove("url");
                    o.insert("style".into(), json!(if a == "rules" { 3 } else { 1 }));
                    if o.get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                    {
                        let label = name.unwrap_or_default();
                        o.insert(
                            "label".into(),
                            json!(label.strip_prefix("Open a ticket: ").unwrap_or(&label)),
                        );
                    }
                    o.insert("action".into(), json!(a));
                })
            };
            view! {
                {head("link", "Button".into())}
                <div class="row"><label>"Does"</label><select aria-label="what the button does" on:change=move |e| set_action(event_target_value(&e))>
                    <option value="" prop:selected=move || action().is_empty()>"Opens a link"</option>
                    {move || { let now = action(); let mut list = parts.ready.get();
                        // A choice the server no longer offers stays visible, so the popup shows what is saved.
                        if !now.is_empty() && !list.iter().any(|(k, _)| *k == now) { list.push((now.clone(), "The saved choice (not offered right now)".into())) }
                        list.into_iter().map(|(k, n)| { let sel = k == now; view! { <option value=k selected=sel>{n}</option> } }).collect_view() }}
                </select></div>
                <div class="row"><label>"Label"</label><input maxlength="80" aria-label="button label" class:need=move || get("label")().trim().is_empty()
                    prop:value=get("label") on:input=move |e| { let v = event_target_value(&e); cur.update(|c| put(c, &ptr("label"), v)) } /></div>
                <Show when=move || action().is_empty()>
                <div class="row"><label>"Link"</label><input placeholder="https://" aria-label="button link" class:need=move || !https(&get("url")())
                    prop:value=get("url") on:input=move |e| { let v = event_target_value(&e).trim().to_string(); cur.update(|c| put(c, &ptr("url"), v)) } /></div>
                </Show>
                <div class="row"><label>"Emoji"</label><button class="ghost eslot" aria-label="button emoji" on:click=move |e| parts.open(Sub::Emoji(Dest::Button(r, b)), e.current_target().and_then(|t| t.dyn_into().ok()).as_ref())>
                    {move || crate::msgparts::face(cur.with(|c| c.pointer(&ptr("emoji")).cloned()))}</button></div>
                <p class="muted small">{move || match action().as_str() {
                    "" => "A click opens the link.",
                    "rules" => "A click gives the member the verification roles. A member who must solve a captcha is sent to the Verify button instead, and a quarantined member stays quarantined.",
                    _ => "A click opens a ticket of this type, with the same checks as the ticket panel. It does not work in a DM copy.",
                }}</p>
                <div class="row"><button class="ghost" on:click=move |_| remove()>"Remove"</button><span class="sp"></span><button on:click=move |_| close()>"Done"</button></div>
            }.into_any()
        }
        Pop::Answer(r, o) => {
            let ptr = format!("/body/components/{r}/components/0/options/{o}/answer");
            view! {
                {head("message", "Answer".into())}
                <p class="muted small">"Brim sends this to the member who picked the option, as a reply only they can see. Links, emojis and variables such as {member} work."</p>
                <div class="tools">{crate::msgparts::tool_buttons(parts)}</div>
                <div class="live ansbox md" on:keydown=on_key><Ed cur ptr ph="Answer" max=2000 multi=true md=true req=true answer=true /></div>
                <div class="row"><span class="sp"></span><button on:click=move |_| close()>"Done"</button></div>
            }.into_any()
        }
        Pop::Color(k) => {
            let now = move || {
                cur.with(|c| {
                    c.pointer(&format!("/body/embeds/{k}/color"))
                        .and_then(Value::as_u64)
                        .map(|x| x as u32)
                })
            };
            let hsv = RwSignal::new(rgb_to_hsv(now().unwrap_or(0x0d9df8)));
            let set = move |c: Option<u32>| {
                // The embed may be gone, or not an object in typed JSON.
                cur.update(|v| {
                    if let Some(o) = v
                        .pointer_mut(&format!("/body/embeds/{k}"))
                        .and_then(Value::as_object_mut)
                    {
                        match c {
                            Some(c) => o.insert("color".into(), json!(c)),
                            None => o.remove("color"),
                        };
                    }
                })
            };
            let remember = move || {
                if let Some(c) = now() {
                    recent.update(|r| {
                        r.retain(|x| *x != c);
                        r.insert(0, c);
                        r.truncate(5);
                    })
                }
            };
            let from_sv = move |e: &ev::PointerEvent| {
                let Some(t) = e
                    .current_target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                else {
                    return;
                };
                let r = t.get_bounding_client_rect();
                let s = ((e.client_x() as f64 - r.left()) / r.width()).clamp(0.0, 1.0);
                let v = 1.0 - ((e.client_y() as f64 - r.top()) / r.height()).clamp(0.0, 1.0);
                hsv.update(|x| {
                    x.1 = s;
                    x.2 = v;
                });
                let (h, s, v) = hsv.get_untracked();
                set(Some(hsv_to_rgb(h, s, v)));
            };
            let hex = move || now().map_or(String::new(), |c| format!("#{c:06X}"));
            let swatch = move |c: Option<u32>| {
                view! {
                    <button class:none=c.is_none() title=c.map_or("No colour".to_string(), |c| format!("#{c:06X}")) aria-label=c.map_or("No colour".to_string(), |c| format!("#{c:06X}"))
                        style=c.map(|c| format!("background:#{c:06x}")) on:click=move |_| { set(c); if let Some(c) = c { hsv.set(rgb_to_hsv(c)) } remember() }></button>
                }
            };
            view! {
                {head("palette", "Embed colour".into())}
                <div class="sv" style=move || format!("--h:{}", hsv.get().0) on:pointerdown=move |e| { if let Some(t) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) { let _ = t.set_pointer_capture(e.pointer_id()); } from_sv(&e) }
                    on:pointermove=move |e| if e.buttons() & 1 == 1 { from_sv(&e) } on:pointerup=move |_| remember()>
                    <i style=move || { let (_, s, v) = hsv.get(); format!("left:{}%;top:{}%", s * 100.0, (1.0 - v) * 100.0) }></i></div>
                <input type="range" class="hue" min="0" max="359" aria-label="hue" prop:value=move || hsv.get().0.round().to_string()
                    on:input=move |e| { let h = event_target_value(&e).parse().unwrap_or(0.0); hsv.update(|x| x.0 = h); let (h, s, v) = hsv.get_untracked(); set(Some(hsv_to_rgb(h, s, v))) } on:change=move |_| remember() />
                <div class="sw10">{PRESETS.map(|c| swatch(Some(c))).collect_view()}{swatch(None)}</div>
                <div class="hexrow"><i style=move || format!("background:{}", if hex().is_empty() { "#1e1f22".into() } else { hex() })></i>
                    <input aria-label="hex colour" maxlength="7" placeholder="#0D9DF8" prop:value=hex
                        on:change=move |e| { let v = event_target_value(&e); if let Ok(c) = u32::from_str_radix(v.trim().trim_start_matches('#'), 16) { if v.trim().trim_start_matches('#').len() == 6 { set(Some(c)); hsv.set(rgb_to_hsv(c)); remember() } } } /></div>
                {move || (!recent.with(Vec::is_empty)).then(|| view! { <small>"Last used"</small><div class="sw10">{recent.get().into_iter().map(|c| swatch(Some(c))).collect_view()}</div> })}
            }.into_any()
        }
        Pop::Time(k) => {
            let tptr = StoredValue::new(format!("/body/embeds/{k}/timestamp"));
            let t = move || cur.with(|c| get_str(c, &tptr.read_value()));
            let set = move |v: String| cur.update(|c| put(c, &tptr.read_value(), v));
            let kind = move || match t().as_str() {
                "" => "none",
                "now" => "now",
                x if x.starts_with('+') => "after",
                _ => "fixed",
            };
            // The date shown and picked, in the browser's time zone.
            let d = js_sys::Date::new_0();
            if kind() == "fixed" {
                d.set_time(js_sys::Date::parse(&t()));
            }
            let ym = RwSignal::new((d.get_full_year() as i32, d.get_month() + 1));
            let day = RwSignal::new((d.get_full_year() as i32, d.get_month() + 1, d.get_date()));
            let hm = RwSignal::new(format!("{:02}:{:02}", d.get_hours(), d.get_minutes()));
            let write_fixed = move || {
                let (y, m, dd) = day.get_untracked();
                let (h, mi) = hm
                    .get_untracked()
                    .split_once(':')
                    .map_or((18, 0), |(h, m)| {
                        (h.parse().unwrap_or(18), m.parse().unwrap_or(0))
                    });
                let at = js_sys::Date::new_with_year_month_day_hr_min(
                    y as u32,
                    m as i32 - 1,
                    dd as i32,
                    h,
                    mi,
                );
                set(String::from(at.to_iso_string()));
            };
            let after = RwSignal::new(
                t().strip_prefix('+')
                    .and_then(|n| n.parse::<i64>().ok())
                    .unwrap_or(7200),
            );
            let unit = RwSignal::new(if after.get_untracked() % 86_400 == 0 {
                86_400
            } else if after.get_untracked() % 3600 == 0 {
                3600
            } else {
                60
            });
            let write_after = move || {
                let n =
                    (after.get_untracked() / unit.get_untracked()).max(1) * unit.get_untracked();
                set(format!("+{}", n.min(365 * 86_400)));
            };
            let seg = [
                ("none", "None"),
                ("now", "When sent"),
                ("fixed", "Date and time"),
                ("after", "After sending"),
            ];
            view! {
                {head("calendar", "Time in the footer".into())}
                <div class="seg wrap">{seg.map(|(key, l)| view! { <button class:on=move || kind() == key on:click=move |_| match key {
                    "none" => set(String::new()), "now" => set("now".into()), "fixed" => write_fixed(), _ => write_after() }>{l}</button> }).collect_view()}</div>
                {move || match kind() {
                    "now" => view! { <p class="muted small">"Shows the moment Brim posts it. Each member sees it in their own time zone."</p> }.into_any(),
                    "fixed" => view! {
                        <Cal ym picked=Signal::derive(move || Some(day.get())) pick=move |d| { let (y, m) = ym.get_untracked(); day.set((y, m, d)); write_fixed() } />
                        <div class="row"><span class="muted small">"Time"</span><input class="hm" maxlength="5" aria-label="time" prop:value=move || hm.get()
                            class:need=move || !valid_hm(&hm.get()) on:input=move |e| { hm.set(event_target_value(&e)); if valid_hm(&hm.get_untracked()) { write_fixed() } } /></div>
                        <p class="muted small">"In your time zone. Each member sees it in their own."</p>
                    }.into_any(),
                    "after" => view! {
                        <div class="row"><input class="num" type="number" min="1" aria-label="how long after" prop:value=move || (after.get() / unit.get()).to_string()
                            on:input=move |e| { after.set(event_target_value(&e).parse::<i64>().unwrap_or(1) * unit.get_untracked()); write_after() } />
                            <select aria-label="unit" on:change=move |e| { let n = after.get_untracked() / unit.get_untracked(); unit.set(event_target_value(&e).parse().unwrap_or(3600)); after.set(n * unit.get_untracked()); write_after() }>
                                {[(60, "minutes"), (3600, "hours"), (86_400, "days")].map(|(v, l)| view! { <option value=v.to_string() prop:selected=move || unit.get() == v>{l}</option> }).collect_view()}
                            </select></div>
                        <p class="muted small">"Counted from when Brim posts it. Good for “starts in 2 hours”. At most a year."</p>
                    }.into_any(),
                    _ => ().into_any(),
                }}
            }.into_any()
        }
    }
}

pub(crate) fn valid_hm(s: &str) -> bool {
    s.split_once(':').is_some_and(|(h, m)| {
        h.len() == 2
            && m.len() == 2
            && h.parse::<u32>().is_ok_and(|h| h < 24)
            && m.parse::<u32>().is_ok_and(|m| m < 60)
    })
}

/// The message editor: the Message and Preview tabs and their popups, on `cur["body"]`. The Message
/// Builder and the Bot messages page share it.
#[derive(Clone, Copy)]
pub(crate) struct Editor {
    pub cur: RwSignal<Value>,
    pub pop: RwSignal<Option<(Pop, f64, f64)>>,
    recent: RwSignal<Vec<u32>>,
    pub json_view: RwSignal<bool>,
    jtext: RwSignal<String>,
    pub jerr: RwSignal<Option<String>>,
    phone: RwSignal<bool>,
    /// The preview's open menu answer: (row, option).
    pub answer: RwSignal<Option<(usize, usize)>>,
    pub parts: Parts,
    /// A built-in bot message: no buttons, menus or embed times (Brim adds its own buttons).
    plain: bool,
}

impl Editor {
    pub fn new(cur: RwSignal<Value>, g: StoredValue<String>, plain: bool) -> Self {
        let pop = RwSignal::new(None::<(Pop, f64, f64)>);
        let recent = RwSignal::new(Vec::<u32>::new());
        let (json_view, jtext, jerr) = (
            RwSignal::new(false),
            RwSignal::new(String::new()),
            RwSignal::new(None::<String>),
        );
        let phone = RwSignal::new(false);
        let parts = Parts::new(cur, g);
        provide_context(parts);
        // Escape closes the top popup, wherever the focus is (often still in the text box).
        let esc_key = window_event_listener(ev::keydown, move |e| {
            if e.key() != "Escape" {
                return;
            }
            if parts.sub.get_untracked().is_some() {
                parts.close()
            } else {
                pop.set(None)
            }
        });
        on_cleanup(move || esc_key.remove());
        // The preview's open menu answer: (row, option).
        let answer = RwSignal::new(None::<(usize, usize)>);
        Editor {
            cur,
            pop,
            recent,
            json_view,
            jtext,
            jerr,
            phone,
            answer,
            parts,
            plain,
        }
    }

    /// Closes the editor's popups and goes back to the visual view, for a newly opened message.
    pub fn reset(self) {
        self.json_view.set(false);
        self.pop.set(None);
        self.parts.sub.set(None);
        self.answer.set(None);
    }

    /// (Message tab, Preview tab, the popup layer).
    #[allow(clippy::type_complexity)]
    pub fn views(
        self,
    ) -> (
        impl Fn() -> AnyView + Copy + Send + Sync + 'static,
        impl Fn() -> AnyView + Copy + Send + Sync + 'static,
        impl Fn() -> Option<AnyView> + Copy + Send + Sync + 'static,
    ) {
        let Editor {
            cur,
            pop,
            recent,
            json_view,
            jtext,
            jerr,
            phone,
            answer,
            parts,
            plain,
        } = self;
        // Opens a popup under the control that was clicked (where it was clicked, for an embed's tall edge).
        let open_pop = move |e: ev::MouseEvent, p: Pop| {
            e.stop_propagation();
            let Some(t) = e
                .current_target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            else {
                return;
            };
            let r = t.get_bounding_client_rect();
            let y = if r.height() > 60.0 {
                e.client_y() as f64
            } else {
                r.bottom()
            };
            let (left, top) = crate::msgparts::place(r.left(), y);
            pop.set(Some((p, left, top)));
        };
        // The message's structure; the editor is rebuilt only when it changes, never per key typed.
        // Per embed: whether it has a thumbnail, and each field's inline flag. Then the buttons per row.
        let shape = Memo::new(move |_| {
            cur.with(|c| {
                let embeds: Vec<(bool, Vec<bool>)> = c["body"]["embeds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        (
                            e.pointer("/thumbnail/url")
                                .and_then(Value::as_str)
                                .is_some_and(|u| !u.is_empty()),
                            e["fields"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|f| f["inline"] == true)
                                .collect(),
                        )
                    })
                    .collect();
                // (is a menu, buttons or menu options)
                let rows: Vec<(bool, usize)> = c["body"]["components"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| match r.pointer("/components/0") {
                        Some(m) if m["type"] == 3 => {
                            (true, m["options"].as_array().map_or(0, Vec::len))
                        }
                        _ => (false, r["components"].as_array().map_or(0, Vec::len)),
                    })
                    .collect();
                (embeds, rows)
            })
        });
        let edit_embed = move |k: usize, f: Box<dyn FnOnce(&mut Value)>| {
            cur.update(|c| {
                // Typed JSON can hold a number where an embed belongs; indexing that by key panics.
                if let Some(e) = c["body"]["embeds"].get_mut(k).filter(|e| e.is_object()) {
                    f(e)
                }
            })
        };
        let slot = move |k: usize, s: usize, cls: &'static str| {
            let (_, _, title, img) = SLOTS[s];
            let url = move || cur.with(|c| get_str(c, &slot_ptr(k, s)));
            if !img {
                return view! { <button class="lnk" class:set=move || !url().is_empty() title=title aria-label=title on:click=move |e| open_pop(e, Pop::Slot(k, s))>{crate::icons::icon("link")}</button> }.into_any();
            }
            view! {
            <button class=move || if url().is_empty() { format!("slot {cls}") } else { "slotbtn".into() } class:need=move || { let u = url(); !u.is_empty() && !https(&u) }
                title=title aria-label=title on:click=move |e| open_pop(e, Pop::Slot(k, s))>
                {move || { let u = url(); if https(&u) { view! { <img class=cls src=u alt="" /> }.into_any() } else { crate::icons::icon("image") } }}
            </button>
        }.into_any()
        };
        let field = move |k: usize, i: usize, span: usize| {
            let fp = move |x: &str| format!("/body/embeds/{k}/fields/{i}/{x}");
            let mv = move |d: isize| {
                edit_embed(
                    k,
                    Box::new(move |e| {
                        if let Some(a) = e["fields"].as_array_mut() {
                            let j = i as isize + d;
                            if j >= 0 && (j as usize) < a.len() {
                                a.swap(i, j as usize)
                            }
                        }
                    }),
                )
            };
            view! {
                <div class="blk fbox" style=format!("grid-column:span {span}")>
                    <div class="fctl">
                        <button class="ghost" title="Move up" aria-label="Move field up" on:click=move |_| mv(-1)>"▲"</button>
                        <button class="ghost" title="Move down" aria-label="Move field down" on:click=move |_| mv(1)>"▼"</button>
                        <label><input type="checkbox" aria-label=format!("Inline: embed {}, field {}", k + 1, i + 1) prop:checked=move || cur.with(|c| c.pointer(&fp("inline")) == Some(&json!(true)))
                            on:change=move |e| { let v = event_target_checked(&e); edit_embed(k, Box::new(move |em| {
                                if let Some(f) = em.pointer_mut(&format!("/fields/{i}")).and_then(Value::as_object_mut) { f.insert("inline".into(), json!(v)); }
                            }));
                            // The new layout rebuilds the fields, so the focus goes back to this tick box.
                            request_animation_frame(move || {
                                let sel = format!("[aria-label=\"Inline: embed {}, field {}\"]", k + 1, i + 1);
                                if let Some(e) = document().query_selector(&sel).ok().flatten().and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()) { let _ = e.focus(); }
                            }) } />"Inline"</label>
                        <span class="sp"></span>
                        <button class="ghost" title="Remove field" aria-label="Remove field" on:click=move |_| edit_embed(k, Box::new(move |e| {
                            if let Some(a) = e["fields"].as_array_mut() { a.remove(i); if a.is_empty() { e.as_object_mut().map(|o| o.remove("fields")); } } }))>"✕"</button>
                    </div>
                    <b><Ed cur ptr=fp("name") ph="Field name" max=256 md=true req=true /></b>
                    <div class="md"><Ed cur ptr=fp("value") ph="Field value" max=1024 multi=true md=true req=true /></div>
                </div>
            }
        };
        let embed_view = move |k: usize, thumb: bool, inline: Vec<bool>, many: bool| {
            let ep = move |x: &str| format!("/body/embeds/{k}{x}");
            let color = move || {
                cur.with(|c| {
                    c.pointer(&ep("/color"))
                        .and_then(Value::as_u64)
                        .map_or("#1e1f22".into(), |c| format!("#{c:06x}"))
                })
            };
            let ts = move || cur.with(|c| get_str(c, &ep("/timestamp")));
            let nf = inline.len();
            view! {
                <div class="embed th" style=move || format!("border-left-color:{}", color())>
                    <button class="edge" title="Change colour" aria-label="Change embed colour" on:click=move |e| open_pop(e, Pop::Color(k))></button>
                    {many.then(|| view! { <button class="x ghost" title="Remove embed" aria-label="Remove embed" on:click=move |_| { pop.set(None); cur.update(|c| { if let Some(a) = c["body"]["embeds"].as_array_mut() { a.remove(k); } }) }>"✕"</button> })}
                    <span class="thumbslot">{slot(k, 0, "thumbimg")}</span>
                    <small class="author blk">{slot(k, 2, "ico")}{slot(k, 4, "")}<b><Ed cur ptr=ep("/author/name") ph="Author name" max=256 /></b></small>
                    <b class="blk ttl">{slot(k, 5, "")}<Ed cur ptr=ep("/title") ph="Title" max=256 md=true /></b>
                    <div class="md"><Ed cur ptr=ep("/description") ph="Description" max=4096 multi=true md=true /></div>
                    {(nf > 0).then(|| view! { <div class="fields">{crate::discord::field_rows(&inline, thumb).into_iter().flat_map(|r| { let span = 12 / r.len(); r.into_iter().map(move |i| field(k, i, span)) }).collect_view()}</div> })}
                    <AddButton label="Add field" class="addf" attr:disabled={nf >= 25} on:click=move |_| edit_embed(k, Box::new(|e| {
                        let f = json!({"name": "", "value": "", "inline": true});
                        match e["fields"].as_array_mut() { Some(a) => a.push(f), None => { e.as_object_mut().map(|o| o.insert("fields".into(), json!([f]))); } } })) />
                    <div class="imgslot">{slot(k, 1, "bigimg")}</div>
                    <small class="muted author blk foot">{slot(k, 3, "ico")}<Ed cur ptr=ep("/footer/text") ph="Footer text" max=2048 />{(!plain).then(|| view! { <span class="dotsep">"•"</span>
                        <button class="tsbtn" class:set=move || !ts().is_empty() on:click=move |e| open_pop(e, Pop::Time(k))>{crate::icons::icon("calendar")}
                            {move || { let t = ts(); match t.as_str() { "" => "Add time".to_string(), "now" => "When sent".to_string(), _ => crate::discord::stamp(&t) } }}</button> })}</small>
                </div>
            }
        };
        let remove_row = move |r: usize| {
            pop.set(None);
            cur.update(|c| {
                if let Some(a) = c["body"]["components"].as_array_mut() {
                    if r < a.len() {
                        a.remove(r);
                    }
                }
            })
        };
        let menu_live = move |r: usize, n: usize| {
            let base = format!("/body/components/{r}/components/0");
            let op = move |o: usize, f: &str| {
                format!("/body/components/{r}/components/0/options/{o}/{f}")
            };
            let opts = move |f: EditList| {
                pop.set(None);
                cur.update(|c| {
                    if let Some(a) = c
                        .pointer_mut(&format!("/body/components/{r}/components/0/options"))
                        .and_then(Value::as_array_mut)
                    {
                        f(a)
                    }
                })
            };
            view! {
                <div class="dmenu edit">
                    <div class="dsel"><Ed cur ptr=format!("{base}/placeholder") ph="Text on the closed menu (optional)" max=150 novars=true /><span class="caret">"⌄"</span>
                        <button class="ghost" title="Remove menu" aria-label="Remove menu" on:click=move |_| remove_row(r)>"✕"</button></div>
                    {(0..n).map(|o| view! {
                        <div class="dopt">
                            <button class="eslot" title="Emoji" aria-label=format!("Emoji for option {}", o + 1) on:click=move |e| parts.open(Sub::Emoji(Dest::Opt(r, o)), e.current_target().and_then(|t| t.dyn_into().ok()).as_ref())>
                                {move || crate::msgparts::face(cur.with(|c| c.pointer(&op(o, "emoji")).cloned()))}</button>
                            <div class="otext"><b><Ed cur ptr=op(o, "label") ph="Option name" max=100 req=true novars=true /></b>
                                <small><Ed cur ptr=op(o, "description") ph="Description (optional)" max=100 novars=true /></small></div>
                            <span class="fctl">
                                <button class="ghost" title="Move up" aria-label="Move option up" on:click=move |_| opts(Box::new(move |a| if o > 0 { a.swap(o, o - 1) }))>"▲"</button>
                                <button class="ghost" title="Move down" aria-label="Move option down" on:click=move |_| opts(Box::new(move |a| if o + 1 < a.len() { a.swap(o, o + 1) }))>"▼"</button>
                                <button class="ghost" title="Remove option" aria-label="Remove option" on:click=move |_| opts(Box::new(move |a| { if o < a.len() { a.remove(o); } }))>"✕"</button>
                            </span>
                            <button class="ans ghost" class:need=move || cur.with(|c| get_str(c, &op(o, "answer")).trim().is_empty()) on:click=move |e| open_pop(e, Pop::Answer(r, o))>{crate::icons::icon("message")}"Answer"</button>
                        </div>
                    }).collect_view()}
                    {(n < 25).then(|| view! { <AddButton label="Add option" class="addf" on:click=move |_| opts(Box::new(|a| a.push(json!({"label": "", "answer": ""})))) /> })}
                </div>
            }
        };
        let buttons_live = move |rows: Vec<(bool, usize)>| {
            rows.into_iter().enumerate().map(|(r, (is_menu, n))| if is_menu { menu_live(r, n).into_any() } else { view! {
            <div class="drow">
                {(0..n).map(|b| {
                    let get = move |f: &str| cur.with(|c| get_str(c, &format!("/body/components/{r}/components/{b}/{f}")));
                    let emo = move || cur.with(|c| c.pointer(&format!("/body/components/{r}/components/{b}/emoji")).cloned()).filter(|e| !e.is_null());
                    let ready = move || !get("action").is_empty();
                    let style = move || cur.with(|c| c.pointer(&format!("/body/components/{r}/components/{b}/style")).and_then(Value::as_u64));
                    view! { <button class="dbtn link-b" class:primary=move || style() == Some(1) class:success=move || style() == Some(3) class:need=move || get("label").trim().is_empty() || (!ready() && !https(&get("url"))) on:click=move |e| open_pop(e, Pop::Button(r, b))>
                        {move || emo().map(|e| crate::msgparts::face(Some(e)))}
                        {move || { let l = get("label"); if l.is_empty() { "Button".to_string() } else { l } }}{move || (!ready()).then_some(" ↗")}</button> }
                }).collect_view()}
                {(n < 5).then(|| view! { <AddButton label="Add button" class="addf inl" on:click=move |_| cur.update(|c| { if let Some(a) = c.pointer_mut(&format!("/body/components/{r}/components")).and_then(Value::as_array_mut) { a.push(json!({"type": 2, "style": 5, "label": "", "url": ""})) } }) /> })}
                <button class="addf inl ghost" title="Remove row" aria-label="Remove button row" on:click=move |_| remove_row(r)>"✕"</button>
            </div>
        }.into_any() }).collect_view()
        };
        let message_live = move || {
            let (embeds, brows) = shape.get();
            let (ne, nr) = (embeds.len(), brows.len());
            view! {
                <div class="dmsg live">
                    <img class="avatar" src="/logo.svg" alt="" />
                    <div>
                        <b>"Brim "</b><span class="chip">"APP"</span>
                        <Ed cur ptr="/body/content" ph="Text above the embeds (optional)" max=2000 multi=true md=true />
                        {embeds.into_iter().enumerate().map(|(k, (thumb, inl))| embed_view(k, thumb, inl, true)).collect_view()}
                        {buttons_live(brows)}
                    </div>
                </div>
                <div class="addembed">
                    <AddButton label="Add embed" attr:disabled={ne >= 10} on:click=move |_| cur.update(|c| match c["body"]["embeds"].as_array_mut() { Some(a) => a.push(json!({"color": 0x0d9df8})), None => c["body"]["embeds"] = json!([{"color": 0x0d9df8}]) }) />
                    {(!plain).then(|| view! {
                    <AddButton label="Add button row" attr:disabled={nr >= 5} on:click=move |_| cur.update(|c| { let r = json!({"type": 1, "components": [{"type": 2, "style": 5, "label": "", "url": ""}]}); match c["body"]["components"].as_array_mut() { Some(a) => a.push(r), None => c["body"]["components"] = json!([r]) } }) />
                    <AddButton label="Add menu" attr:disabled={nr >= 5} on:click=move |_| cur.update(|c| { let r = json!({"type": 1, "components": [{"type": 3, "options": [{"label": "", "answer": ""}]}]}); match c["body"]["components"].as_array_mut() { Some(a) => a.push(r), None => c["body"]["components"] = json!([r]) } }) />
                    })}
                    <span class="muted small">{format!("{ne} / 10 embeds · click an embed's coloured edge to change its colour · grey squares are images you can add")}</span>
                </div>
            }
        };
        let to_json = move || {
            jtext.set(
                serde_json::to_string_pretty(&cur.with_untracked(|c| c["body"].clone()))
                    .unwrap_or_default(),
            );
            jerr.set(None);
            json_view.set(true);
        };
        let message_tab = move || {
            view! {
                <div class="card mcard" on:keydown=on_key>
                    <div class="pvtop"><h2>"Message"</h2><span class="sp"></span>
                        <div class="seg" role="group" aria-label="editor view">
                            <button class:on=move || !json_view.get() on:click=move |_| json_view.set(false)>"Visual"</button>
                            <button class:on=move || json_view.get() on:click=move |_| to_json()>"JSON"</button>
                        </div>
                        <Tip text="Type straight into the message. Grey names show what can be filled. Grey squares are images you can add, the link icons add a link to the author or title. Tick Inline to put a field next to the one before it, as Discord does. Type { or press { } for variables Brim fills in when it sends. JSON is for advanced users and is checked when you save." />
                    </div>
                    {move || if json_view.get() { view! {
                        <textarea class="json" spellcheck="false" aria-label="message JSON" class:need=move || jerr.with(Option::is_some) prop:value=move || jtext.get()
                            on:input=move |e| { let v = event_target_value(&e); jtext.set(v.clone()); match serde_json::from_str::<Value>(&v) {
                                Ok(b) if b.is_object() => { cur.update(|c| c["body"] = b); jerr.set(None) }
                                Ok(_) => jerr.set(Some("The message must be an object: { ... }".into())),
                                Err(e) => jerr.set(Some(format!("Not valid JSON: {e}"))),
                            } }></textarea>
                        {move || match jerr.get() { Some(e) => view! { <p class="err small">{e}</p> }.into_any(), None => view! { <p class="muted small">"Text, embeds and link buttons, as Discord's JSON. Saving checks every key and Discord's limits and says what is wrong."</p> }.into_any() }}
                    }.into_any() } else { view! {
                        <div class="tools">
                            {WRAPS.map(|(l, tip, o, c, ..)| view! { <button class="ghost" title=tip aria-label=tip on:mousedown=move |e| { e.prevent_default(); wrap_sel(o, c) }>{l}</button> }).collect_view()}
                            <button class="ghost" title="Link (Ctrl+K)" aria-label="Link (Ctrl+K)" on:mousedown=|e| { e.prevent_default(); link() }>{crate::icons::icon("link")}</button>
                            {LINES.map(|(l, tip, p)| view! { <button class="ghost" title=tip aria-label=tip on:mousedown=move |e| { e.prevent_default(); line_sel(p) }>{l}</button> }).collect_view()}
                            {crate::msgparts::tool_buttons(parts)}
                        </div>
                        {message_live}
                    }.into_any() }}
                </div>
            }
        };
        let preview_tab = move || {
            view! {
                <div class="card mcard">
                    <div class="pvtop"><h2>"As members see it"</h2><span class="muted small">"Press buttons, links, menus and spoilers to test them. Variables show example values."</span><span class="sp"></span>
                        <div class="seg" role="group" aria-label="preview width">
                            <button class:on=move || !phone.get() on:click=move |_| phone.set(false)>"Desktop"</button>
                            <button class:on=move || phone.get() on:click=move |_| phone.set(true)>"Phone"</button>
                        </div>
                    </div>
                    <div class="pvwrap" class:phone=move || phone.get() on:click=move |e| preview_menu(e, answer)>
                        {move || cur.with(|c| crate::discord::bot(&with_added(c, crate::msgparts::examples(&body_to_save(c)))))}
                        {move || cur.with(added_notice)}
                        {move || answer.get().and_then(|(r, o)| cur.with(|c| c.pointer(&format!("/body/components/{r}/components/0/options/{o}/answer")).cloned()))
                            .map(|a| crate::discord::bot(&json!({"content": crate::msgparts::examples(&a), "flags": 64})))}
                    </div>
                </div>
            }
        };
        let pop_view = move || {
            pop.get().map(|(p, left, top)| {
            let el = NodeRef::<leptos::html::Div>::new();
            Effect::new(move |_| {
                cur.track(); // a choice inside the popup can make it taller
                crate::msgparts::fit(el, top)
            });
            let cls = match p { Pop::Color(_) => "pop cpop", _ => "pop slotpop" };
            view! {
                <Portal>
                    <div class="popbg" on:click=move |_| pop.set(None)></div>
                    <div node_ref=el class=cls role="dialog" style=format!("left:{left}px;top:{top}px")>
                        {pop_body(cur, pop, p, recent, parts)}
                    </div>
                </Portal>
            }
        })
        };
        (
            move || message_tab().into_any(),
            move || preview_tab().into_any(),
            move || pop_view().map(IntoAny::into_any),
        )
    }
}

#[component]
pub fn Embeds(
    guild: String,
    /// A message id from the link (Overview links here): opened on its Send tab once the list loads.
    #[prop(optional)]
    open_id: String,
) -> impl IntoView {
    let g = StoredValue::new(guild);
    let pending = StoredValue::new(open_id);
    let rows = RwSignal::new(Vec::<Value>::new());
    // The channel picker's list ({channels, can_create}) and this message's Sent log.
    let chans = RwSignal::new(json!({"channels": []}));
    let sends = RwSignal::new(Vec::<Value>::new());
    let picking = RwSignal::new(false);
    // Counts opened messages, so a reply that comes back after another one opened is not written into it.
    let opened = StoredValue::new(0u32);
    // A send or edit on its way: its buttons wait, so a double click does not post twice.
    // The save that runs (None = Save, Some(0) = Send, Some(id) = edit that copy), so only its button shows the working sign.
    let busy = RwSignal::new(None::<Option<i64>>);
    // false: the list of messages; true: the builder for `cur`.
    let open = RwSignal::new(false);
    let tab = RwSignal::new("message");
    let editing = RwSignal::new(false);
    let q = RwSignal::new(String::new());
    let (cur, msg) = (RwSignal::new(blank()), Msg::default());
    let orig = RwSignal::new(snapshot(&blank()));
    let dirty = Memo::new(move |_| cur.with(snapshot) != orig.get());
    let ed = Editor::new(cur, g, false);
    let Editor {
        pop,
        json_view,
        jerr,
        answer,
        parts,
        ..
    } = ed;
    let (message_tab, preview_tab, pop_view) = ed.views();
    let base = move || format!("/api/guilds/{}/embeds", g.get_value());
    let load = move || {
        if g.is_disposed() {
            return;
        }
        spawn_local(async move {
            match api("GET", &base(), None).await {
                Ok(v) => rows.set(v.as_array().cloned().unwrap_or_default()),
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
            parts.roles.set(
                v["roles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|r| r["name"] != "@everyone" && r["managed"] != true)
                    .map(|r| {
                        (
                            r["id"].as_str().unwrap_or("").to_string(),
                            format!("@{}", r["name"].as_str().unwrap_or("?")),
                        )
                    })
                    .collect(),
            );
        }
    });
    // The ready-made buttons: one per ticket type that is on, and "I accept the rules" only while
    // verification gives its roles on a click (with a captcha on, the button is not offered).
    spawn_local(async move {
        let Ok(c) = api(
            "GET",
            &format!("/api/guilds/{}/config", g.get_value()),
            None,
        )
        .await
        else {
            return;
        };
        if parts.ready.is_disposed() {
            return;
        }
        let mut list = ready_buttons(&c["config"]);
        // A message with an Open a ticket button is a Trusted Admin's to save and send.
        if !matches!(c["level"].as_str(), Some("admin" | "owner")) {
            list.retain(|(a, _)| !a.starts_with("ticket:"));
        }
        parts.ready.set(list);
    });
    let load_chans = move || {
        spawn_local(async move {
            match api("GET", &format!("{}/channels", base()), None).await {
                Ok(mut v) => {
                    v["loaded"] = json!(true);
                    chans.set(v)
                }
                Err(e) => msg.err(e),
            }
        })
    };
    load_chans();
    let load_sends = move |id: Value| {
        spawn_local(async move {
            let v = match id.as_i64() {
                Some(n) => api("GET", &format!("{}/{n}/sends", base()), None)
                    .await
                    .ok(),
                None => None,
            };
            // Another message may be open by now.
            if cur.try_with_untracked(|c| c["id"] == id) == Some(true) {
                sends.set(v.and_then(|v| v.as_array().cloned()).unwrap_or_default())
            }
        })
    };
    let show = move |m: Value| {
        msg.clear();
        let mut m = m;
        if !m["body"]["components"].is_array() {
            m["body"]["components"] = json!([]);
        }
        orig.set(snapshot(&m));
        cur.set(m);
        tab.set("message");
        editing.set(true);
        json_view.set(false);
        pop.set(None);
        parts.sub.set(None);
        answer.set(None);
        picking.set(false);
        sends.set(vec![]);
        opened.update_value(|n| *n += 1);
        load_sends(cur.with_untracked(|c| c["id"].clone()));
        open.set(true);
    };
    Effect::new(move |_| {
        let id = pending.get_value();
        if id.is_empty() {
            return;
        }
        let found = rows.with(|r| {
            r.iter()
                .find(|m| m["id"].as_i64().is_some_and(|n| n.to_string() == id))
                .cloned()
        });
        if let Some(m) = found {
            pending.set_value(String::new());
            show(m);
            tab.set("send");
        }
    });
    // Saves the draft; then `Some(0)` sends a new copy and `Some(send)` puts it into that copy from the Sent log.
    let save = move |post: Option<i64>| {
        if busy.get_untracked().is_some() {
            return;
        }
        // The JSON view keeps the last valid body: saving now would drop what was typed.
        if let Some(e) = jerr.get_untracked().filter(|_| json_view.get_untracked()) {
            return msg.err(format!("Not saved: the JSON has an error. {e}"));
        }
        // Every save: two quick clicks on a new message would insert it twice.
        busy.set(Some(post));
        let n = opened.get_value();
        let same = move || opened.try_get_value() == Some(n);
        spawn_local(async move {
            let c = cur.get_untracked();
            let r = async {
                let v = api("POST", &base(), Some(json!({"id": c["id"], "name": c["name"], "body": body_to_save(&c), "channel": c["channel"], "pings": c["pings"].as_str().unwrap_or("none"), "webhook": c["webhook"]}))).await?;
                if same() {
                    cur.update(|c| { c["id"] = v["id"].clone(); c["missing"] = v["missing"].clone(); c["missing_answer"] = v["missing_answer"].clone(); c["version"] = v["version"].clone(); });
                    orig.set(snapshot(&c));
                }
                let id = v["id"].clone();
                // Discord answers 0 x 0 for a picture it could not load: said as an error, since
                // the slot shows nothing in Discord although the preview shows it.
                let done = match post {
                    Some(0) => {
                        let r = api("POST", &format!("{}/{id}/post", base()), Some(json!({}))).await?;
                        if same() {
                            cur.update(|c| { c["message"] = r["message"].clone(); c["message_channel"] = r["message_channel"].clone(); });
                        }
                        load_sends(id.clone());
                        if let Some(w) = r["warning"].as_str() {
                            return Err(format!("Sent, but {}{}", w[..1].to_lowercase(), &w[1..]));
                        }
                        "Sent. The new copy is in the Sent log.".to_string()
                    }
                    Some(send) => {
                        let r = api("POST", &format!("{}/{id}/sends/{send}/edit", base()), Some(json!({}))).await?;
                        load_sends(id.clone());
                        if let Some(w) = r["warning"].as_str() {
                            return Err(format!("Edited, but {}{}", w[..1].to_lowercase(), &w[1..]));
                        }
                        "That copy now shows this version.".to_string()
                    }
                    None => return Ok(if v["missing"].is_string() { "Saved. Not ready to send yet.".into() } else { "Saved.".into() }),
                };
                load_sends(id);
                Ok::<_, String>(done)
            }.await;
            if busy.try_set(None).is_some() {
                return;
            }
            match r {
                Ok(t) => msg.ok(t),
                Err(e) => msg.err(e),
            }
            load();
        })
    };
    let remove = move |id: i64, name: String| {
        ask(
            Ask::new(format!("Delete “{name}”?"))
                .text("The copies in its Sent log are deleted in Discord too.")
                .yes("Delete")
                .danger(),
            move |_| {
                spawn_local(async move {
                    match api("POST", &format!("{}/{id}/delete", base()), Some(json!({}))).await {
                        Ok(v) => {
                            open.set(false);
                            match v["left"].as_u64().unwrap_or(0) {
                                0 => msg.ok("Deleted."),
                                n => msg.err(format!("Deleted. {n} posted copies stay in Discord: Brim cannot reach their channel. Delete them in Discord.")),
                            }
                            load()
                        }
                        Err(e) => msg.err(e),
                    }
                })
            },
        )
    };
    let duplicate = move |m: Value| {
        spawn_local(async move {
            let name = format!("{} (copy)", m["name"].as_str().unwrap_or(""));
            let name: String = name.chars().take(100).collect();
            match api(
                "POST",
                &base(),
                Some(json!({"name": name, "body": m["body"], "channel": m["channel"]})),
            )
            .await
            {
                Ok(_) if m["pings"].as_str().is_some_and(|p| p != "none") => {
                    msg.ok(format!("Made “{name}”. Pings are off in the copy."))
                }
                Ok(_) => msg.ok(format!("Made “{name}”.")),
                Err(e) => msg.err(e),
            }
            load()
        })
    };
    let back = move || {
        crate::guard::leave(move || {
            open.set(false);
            load()
        })
    };
    crate::guard::guard(
        move || open.get() && (dirty.try_get().unwrap_or(false) || json_view.get() && jerr.with(Option::is_some)),
        move || busy.try_get().unwrap_or(None).is_some(),
        move || jerr.get().filter(|_| json_view.get()).map(|e| format!("The JSON has an error. {e}")),
        move || save(None),
    );
    // Sent log delete: Brim deletes that copy in Discord, then its row goes.
    let del_sent = move |send: i64| {
        ask(
            Ask::new("Delete this copy?")
                .text("Brim deletes it in Discord and removes it from the Sent log. The saved message stays.")
                .yes("Delete")
                .danger(),
            move |_| {
                let id = cur.with_untracked(|c| c["id"].clone());
                spawn_local(async move {
                    match api("POST", &format!("{}/{id}/sends/{send}/delete", base()), Some(json!({}))).await {
                        Ok(v) => msg.ok(v["done"].as_str().unwrap_or("Copy deleted.")),
                        Err(e) => msg.err(e),
                    }
                    load_sends(id);
                })
            },
        )
    };
    let jump = move |channel: &str, m: &str| {
        format!(
            "https://discord.com/channels/{}/{channel}/{m}",
            g.get_value()
        )
    };
    let chan_name = move |id: &str| {
        chans
            .with(|v| {
                v["channels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|c| c["id"] == id)
                    .map(|c| format!("#{}", c["name"].as_str().unwrap_or("?")))
                    // Still loading, or just made and not in the list yet: not "deleted".
                    .or_else(|| (v["loaded"] == true).then(|| "a deleted channel".into()))
            })
            .unwrap_or_else(|| "#…".into())
    };
    // Only what a message does is shown, each icon with a card that says what it means.
    let states = move |m: &Value| {
        let sent = m["message"].is_string();
        let u = m["uses"].clone();
        let uses = move |k: &str| u[k] == true;
        let missing = m["missing"].as_str().map(|w| {
            format!("Not ready: {w} It is saved, but it cannot be sent until this is fixed.")
        });
        view! {
            <span class="states">
                {sent.then(|| view! { <Tip text="Sent: Brim has posted this message. The Send tab shows where." icon="send" /> })}
                {uses("scheduled").then(|| view! { <Tip text="Scheduled: a schedule on the Automate tab posts it." icon="clock" /> })}
                {uses("answers").then(|| view! { <Tip text="Answers members: a trigger on the Automate tab answers members with it." icon="message" /> })}
                {uses("dms").then(|| view! { <Tip text="Sends DMs: a trigger answers members with it by DM." icon="mail" /> })}
                {uses("sticky").then(|| view! { <Tip text="Sticky: it stays the last message in a channel. The Automate tab shows where." icon="pin" /> })}
                {uses("issue").then(|| view! { <span class="dot need" aria-hidden="true"></span><Tip text="Needs a look: a schedule, trigger or sticky has a problem. Open Automate to see it." icon="warning" /> })}
                {missing.map(|w| view! { <span class="chip need">"Not ready"</span><Tip text=w icon="warning" /> })}
            </span>
        }
    };
    let list = move || {
        let needle = q.get().to_lowercase();
        let all = rows.get();
        let n = all.len();
        view! {
            <div class="intro"><p>"Build a message once, then let Brim post it. Edit it later and the posted copy changes too."</p><Count n=Signal::stored(n) key="messages" /></div>
            {(n > 0).then(|| view! { <div class="listtop">
                <input type="search" placeholder="Search messages" aria-label="search messages" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e)) />
                <AddButton label="Add new message" attr:disabled=move || !room(n, "messages") on:click=move |_| show(blank()) />
            </div> })}
            {(n == 0).then(|| view! { <Empty title="No messages yet" hint="Build your first message: a welcome, the rules or an announcement." label="Add new message" add=move || show(blank()) /> })}
            <div class="mlist">
                {all.into_iter().filter(|m| m["name"].as_str().unwrap_or("").to_lowercase().contains(&needle)).map(|m| {
                    let color = format!("#{:06x}", m["body"]["embeds"][0]["color"].as_u64().unwrap_or(0x4e5058));
                    let (id, name) = (m["id"].as_i64().unwrap_or(0), m["name"].as_str().unwrap_or("").to_string());
                    let (m1, m2, n2) = (m.clone(), m.clone(), name.clone());
                    view! {
                        <div class="card mrow">
                            <span class="sw8" style=format!("background:{color}")></span>
                            <button class="nm" on:click=move |_| show(m1.clone())><b>{name}</b></button>
                            {states(&m)}
                            <span class="acts">
                                <button class="ghost" title="Duplicate" aria-label="Duplicate" disabled={n >= 100} on:click=move |_| duplicate(m2.clone())>{crate::icons::icon("copy")}</button>
                                <button class="ghost" title="Delete" aria-label="Delete" on:click=move |_| remove(id, n2.clone())>{crate::icons::icon("trash")}</button>
                            </span>
                        </div>
                    }
                }).collect_view()}
            </div>
        }
    };
    let access = expect_context::<Access>();
    let send_tab = move || {
        let need = move || cur.with(|c| c["channel"].is_null());
        let why = move || {
            cur.with(|c| {
                if c["channel"].is_null() {
                    Some("Choose a channel first.".to_string())
                } else if c["webhook"].is_object()
                    && c["webhook"]["name"]
                        .as_str()
                        .is_none_or(|n| n.trim().is_empty())
                {
                    Some("Give the webhook a name first.".to_string())
                } else {
                    c["missing"].as_str().map(|w| format!("Not ready: {w}"))
                }
            })
        };
        view! {
            <div class="card send">
                // Where, Channel and Send as side by side (one line where the width allows).
                <div class="sendto">
                <div class="row"><label>"Where"</label>
                    <div class="seg" role="group" aria-label="where">
                        <button class="on">"Channel"</button>
                        <button disabled=true title="A DM only goes to the member who triggered a rule. It comes with trigger answers.">"DM"</button>
                    </div>
                    <Tip text="Send posts in a channel. A DM only goes to the member who triggered a rule, so DM is switched on in trigger answers, not here." icon="info" />
                </div>
                <div class="row"><label>"Channel"</label>
                    <button class="ghost chbtn" class:need=need aria-haspopup="dialog" on:click=move |_| picking.set(true)>
                        {move || cur.with(|c| c["channel"].as_str().map(chan_name)).unwrap_or_else(|| "Choose channel...".into())}
                    </button>
                </div>
                <div class="row"><label>"Send as"</label>
                    <div class="seg" role="group" aria-label="send as">
                        <button class:on=move || cur.with(|c| c["webhook"].is_null()) disabled=move || access.rank() < 3 on:click=move |_| cur.update(|c| c["webhook"] = Value::Null)>"Brim"</button>
                        <button class:on=move || cur.with(|c| !c["webhook"].is_null()) disabled=move || access.rank() < 3 on:click=move |_| cur.update(|c| if c["webhook"].is_null() { c["webhook"] = json!({"name": "", "avatar": null}) })>"Webhook"</button>
                    </div>
                    <Tip text=Signal::derive(move || if access.rank() < 3 { "Only the Owner, Extra Owners and Trusted Admins can send as a webhook, or change or send a message that does.".to_string() } else { "Webhook: the message shows the name and picture you choose (Discord still marks it APP). Brim makes one webhook per channel and needs Manage Webhooks there.".to_string() }) icon="info" />
                </div>
                </div>
                {move || (!cur.with(|c| c["webhook"].is_null())).then(|| webhook_fields(cur, msg, g.get_value()))}
                <div class="row"><label>"Pings"</label>
                    <div class="seg" role="group" aria-label="pings">
                        {[("none", "No pings"), ("named", "Users and roles it names"), ("all", "Also @everyone and @here")].map(|(k, l)| view! {
                            <button class:on=move || cur.with(|c| c["pings"].as_str().unwrap_or("none") == k)
                                disabled=move || access.rank() < 3
                                on:click=move |_| cur.update(|c| c["pings"] = json!(k))>{l}</button>
                        }).collect_view()}
                    </div>
                    <Tip text=Signal::derive(move || if access.rank() < 3 { "Only the Owner, Extra Owners and Trusted Admins can switch pings on, or change a message that pings.".to_string() } else { "Off by default. \"Users and roles it names\" pings the members and roles written in the text (not in embeds). Variables never add a ping.".to_string() }) icon="info" />
                </div>
                {move || why().map(|w| view! { <p class="err">{w}</p> })}
                <div class="row">
                    <button on:click=move |_| save(Some(0)) disabled=move || why().is_some() || busy.get().is_some()>{busy_label(move || busy.get() == Some(Some(0)), "Send")}</button>
                    <span class="muted small">"Each send posts a new copy. Edit a copy from the Sent log."</span>
                </div>
            </div>
            <div class="card sentlog">
                <h2>"Sent log"</h2>
                {move || {
                    let rows = sends.get();
                    if rows.is_empty() {
                        return view! { <p class="muted">"Not sent yet. Each send shows here, the last 100 are kept."</p> }.into_any();
                    }
                    view! { <ul>{rows.into_iter().map(|r| {
                        let ch = r["channel"].as_str().unwrap_or("").to_string();
                        let sid = r["id"].as_i64().filter(|&i| i > 0);
                        let link = r["posted"].as_str().map(|m| jump(&ch, m));
                        let at = crate::discord::time(r["at"].as_i64().unwrap_or(0), "f");
                        let edited = r["edited"].as_i64().map(|e| format!(" Edited {}.", crate::discord::time(e, "f")));
                        // Its webhook was deleted in Discord: only that webhook could edit it.
                        let gone = r["hook_gone"].is_object().then(|| format!(
                            "Brim's webhook for this copy was deleted in Discord on {}, so it can no longer be edited. Send a new copy, then delete this one with its trash button.",
                            crate::discord::time(r["hook_gone"]["at"].as_i64().unwrap_or(0), "f")
                        ));
                        // A trigger answer sent by DM: only the member sees it; it is not edited.
                        let dm = r["dm_user"].as_str().map(String::from);
                        let editable = gone.is_none() && dm.is_none();
                        let place = match &dm {
                            Some(u) => format!(" by DM to member {u}"),
                            None => format!(" in {}", chan_name(&ch)),
                        };
                        let link = if dm.is_some() { None } else { link };
                        // Send again where this copy was, with the message's own settings (the
                        // normal send, so its rights and waits apply). A deleted channel: pick one.
                        let again = gone.is_some().then(|| {
                            let deleted = chan_name(&ch) == "a deleted channel";
                            let ch2 = ch.clone();
                            if deleted {
                                view! { <div class="err small">"That channel was deleted: pick a new channel under Send, then click Send."</div> }.into_any()
                            } else {
                                view! { <button class="ghost" disabled=move || why().is_some() || busy.get().is_some() on:click=move |_| {
                                    if cur.with_untracked(|c| c["channel"].as_str() != Some(ch2.as_str())) {
                                        cur.update(|c| c["channel"] = json!(ch2.clone()));
                                    }
                                    save(Some(0))
                                }>"Send again here"</button> }.into_any()
                            }
                        });
                        view! { <li>
                            <div><b>{at}</b>{place}<div class="muted small">{r["detail"].as_str().unwrap_or("").to_string()}{edited}</div>{gone.map(|g| view! { <div class="err small">{g}</div> })}{r["warning"].as_str().map(|w| view! { <div class="warn small">{w.to_string()}</div> })}{again}</div>
                            <span class="sp"></span>
                            {link.clone().map(|u| view! { <a class="btn ghost" href=u target="_blank" rel="noopener">"Jump"</a> })}
                            <button class="ghost" disabled=move || !editable || link.is_none() || sid.is_none() || why().is_some() || busy.get().is_some() on:click=move |_| save(sid)>{busy_label(move || sid.is_some() && busy.get() == Some(sid), "Edit message")}</button>
                            {sid.map(|s| view! { <button class="danger" aria-label="delete this copy" title="Delete this copy" disabled=move || busy.get().is_some() on:click=move |_| del_sent(s)>{crate::icons::icon("trash")}</button> })}
                        </li> }
                    }).collect_view()}</ul> }.into_any()
                }}
            </div>
            {move || picking.get().then(|| channel_picker(chans, cur, picking, msg, load_chans, format!("{}/channels", base())))}
        }
    };
    let rule_target = RwSignal::new(None::<RwSignal<Value>>);
    let rule_picking = RwSignal::new(false);
    let automate_tab = move || {
        let access = expect_context::<Access>();
        let picker = move |r: RwSignal<Value>| {
            rule_target.set(Some(r));
            rule_picking.set(true)
        };
        view! {
            {crate::msgrules::automate_tab(
                base(),
                cur,
                dirty,
                Signal::derive(move || access.rank()),
                chan_name,
                picker,
                msg,
            )}
            // Sticky is automated sending too, so it sits here with schedules and triggers.
            {crate::msgsticky::sticky_tab(
                base(),
                cur,
                dirty,
                Signal::derive(move || access.rank()),
                chan_name,
                picker,
                msg,
            )}
            {crate::msgtags::tag_tab(base(), cur, dirty, Signal::derive(move || access.rank()), msg)}
            {move || rule_picking.get().then(|| rule_target.get().map(|r| channel_picker(chans, r, rule_picking, msg, load_chans, format!("{}/channels", base()))))}
        }
    };
    let builder = move || {
        view! {
            <div class="mbhead">
                <button class="ghost back" on:click=move |_| back()>"← Saved messages"</button>
                <input class="name" placeholder="Name this message" maxlength="100" aria-label="message name" class:need=move || cur.with(|c| c["name"].as_str().unwrap_or("").trim().is_empty())
                    prop:value=move || cur.with(|c| c["name"].as_str().unwrap_or("").to_string()) on:input=move |e| { let v = event_target_value(&e); cur.update(|c| c["name"] = json!(v)) } />
                {move || cur.with(|c| states(c))}
                <Show when=move || cur.with(|c| c["id"].is_i64())>
                    <button class="ghost" title="Delete" aria-label="Delete" on:click=move |_| { let c = cur.get_untracked(); remove(c["id"].as_i64().unwrap_or(0), c["name"].as_str().unwrap_or("").to_string()) }>{crate::icons::icon("trash")}</button>
                </Show>
            </div>
            <PageTabs tabs=vec![Tab::state("message", "Message").icon("edit"), Tab::state("automate", "Automate").icon("clock"), Tab::state("send", "Send").icon("send")]
                active=Signal::derive(move || tab.get().to_string())
                on_pick=Callback::new(move |k| { pop.set(None); parts.sub.set(None); answer.set(None); tab.set(k) }) />
            {move || match tab.get() {
                "send" => send_tab().into_any(),
                "automate" => automate_tab().into_any(),
                _ => split(editing, true, cur, message_tab, preview_tab),
            }}
            {pop_view}
            {crate::msgparts::sub_view(parts)}
            <Show when=move || dirty.get() || (json_view.get() && jerr.with(Option::is_some))>
                <div class="bar"><b>"Unsaved changes"</b>
                    <button class="ghost" on:click=move |_| { let o = orig.get_untracked(); cur.update(|c| { c["name"] = o[0].clone(); c["body"] = o[1].clone(); c["channel"] = o[2].clone(); c["pings"] = o[3].clone(); c["webhook"] = o[4].clone(); }); jerr.set(None); json_view.set(false) }>"Cancel"</button>
                    <button disabled=move || busy.get().is_some() on:click=move |_| save(None)>{busy_label(move || busy.get() == Some(None), "Save")}</button></div>
            </Show>
        }
    };
    view! {
        {move || if open.get() { builder().into_any() } else { list().into_any() }}
        <Toast msg />
    }
}

/// An upload served by this dashboard (plain http in a local setup).
fn own_media(u: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .is_some_and(|o| {
            u.strip_prefix(o.as_str())
                .is_some_and(|r| r.starts_with("/media/"))
        })
}

/// "Send as" a webhook: its name, and a picture from an https link or an upload.
fn webhook_fields(cur: RwSignal<Value>, msg: Msg, guild: String) -> impl IntoView {
    let file = NodeRef::<leptos::html::Input>::new();
    let busy = RwSignal::new(false);
    // Only Trusted Admins and up change "Send as"; the save refuses it for others too.
    let low = expect_context::<Access>().rank() < 3;
    let url = StoredValue::new(format!("/api/guilds/{guild}/embeds/media"));
    let upload = move |_| {
        let Some(f) = file
            .get_untracked()
            .and_then(|i| i.files())
            .and_then(|l| l.get(0))
        else {
            return;
        };
        if !matches!(
            f.type_().as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return msg.err("Only PNG, JPEG, GIF and WebP pictures can be used.");
        }
        if f.size() > 1024.0 * 1024.0 {
            return msg.err("The picture is larger than 1 MB.");
        }
        busy.set(true);
        let u = url.get_value();
        spawn_local(async move {
            let r = api_file("PUT", &u, f.into()).await;
            if busy.try_set(false).is_some() {
                return;
            }
            match r {
                Ok(v) => cur.update(|c| c["webhook"]["avatar"] = v["url"].clone()),
                Err(e) => msg.err(e),
            }
            if let Some(i) = file.get_untracked() {
                i.set_value("")
            }
        })
    };
    let name = move || cur.with(|c| c["webhook"]["name"].as_str().unwrap_or("").to_string());
    let avatar = move || cur.with(|c| c["webhook"]["avatar"].as_str().unwrap_or("").to_string());
    view! {
        <div class="hookf">
            <div class="row"><label>"Name"</label>
                <input aria-label="webhook name" maxlength="80" prop:disabled=low class:need=move || name().trim().is_empty() prop:value=name on:input=move |e| { let v = event_target_value(&e); cur.update(|c| c["webhook"]["name"] = json!(v)) } />
            </div>
            <div class="row"><label>"Picture"</label>
                {move || { let a = avatar(); (https(&a) || own_media(&a)).then(|| view! { <img class="hookav" src=a alt="" /> }) }}
                <input aria-label="webhook picture link" prop:disabled=low placeholder="https://… or upload" class:need=move || { let a = avatar(); !a.is_empty() && !https(&a) && !own_media(&a) } prop:value=avatar on:input=move |e| { let v = event_target_value(&e); cur.update(|c| c["webhook"]["avatar"] = if v.trim().is_empty() { Value::Null } else { json!(v) }) } />
                <input type="file" accept="image/png,image/jpeg,image/gif,image/webp" aria-label="webhook picture file" node_ref=file on:change=upload prop:disabled=move || low || busy.get() />
            </div>
            <p class="muted small">"Empty picture: Discord's default picture for webhooks. Uploads: PNG, JPEG, GIF or WebP up to 1 MB, cut to 512 × 512."</p>
        </div>
    }
}

/// The channel picker: a card over the blurred page, channels grouped by category in server order,
/// the ones Brim cannot post in greyed with the reason, and "+ New channel" for TA and up.
fn channel_picker(
    chans: RwSignal<Value>,
    cur: RwSignal<Value>,
    picking: RwSignal<bool>,
    msg: Msg,
    reload: impl Fn() + Copy + Send + Sync + 'static,
    url: String,
) -> impl IntoView {
    use crate::icons::icon;
    let q = RwSignal::new(String::new());
    let adding = RwSignal::new(false);
    let (name, parent, staff) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(false),
    );
    let url = StoredValue::new(url);
    let r = NodeRef::new();
    focus_on_open(r);
    let close = move || picking.set(false);
    let pick = move |id: String| {
        cur.update(|c| c["channel"] = json!(id));
        picking.set(false)
    };
    let cats = move || {
        chans.with(|v| {
            v["channels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["kind"] == "category")
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let groups = move || {
        let q = q.get().trim().to_lowercase();
        let hit = |c: &Value| {
            q.is_empty()
                || c["id"] == q.as_str()
                || c["name"]
                    .as_str()
                    .is_some_and(|n| n.to_lowercase().contains(&q))
        };
        let mut heads: Vec<(Option<Value>, Vec<Value>)> = vec![(None, vec![])];
        heads.extend(cats().into_iter().map(|c| (Some(c), vec![])));
        chans.with(|v| {
            for c in v["channels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["kind"] != "category" && hit(c))
            {
                let i = heads
                    .iter()
                    .position(|(h, _)| h.as_ref().is_some_and(|h| h["id"] == c["parent"]))
                    .unwrap_or(0);
                heads[i].1.push(c.clone());
            }
        });
        heads.retain(|(_, l)| !l.is_empty());
        heads
    };
    let row = move |c: Value| {
        let id = c["id"].as_str().unwrap_or("").to_string();
        let why = c["why"].as_str().map(str::to_string);
        let on = cur.with_untracked(|v| v["channel"] == id.as_str());
        let ico = if c["kind"] == "announcement" {
            "announcement"
        } else {
            "channel_text"
        };
        let off = why.is_some();
        view! {
            <button class="chrow" class:on=on disabled=off on:click=move |_| pick(id.clone())>
                {icon(ico)}<span class="nm">{c["name"].as_str().unwrap_or("?").to_string()}</span>
                {why.map(|w| view! { <span class="muted small">{w}</span> })}
            </button>
        }
    };
    let busy = RwSignal::new(false);
    let create = move || {
        let n = name.get_untracked().trim().to_string();
        if n.is_empty() {
            return msg.err("Type a name for the channel.");
        }
        let p = parent.get_untracked();
        let p = (!p.is_empty()).then_some(p);
        let body = json!({"name": n, "parent": p, "staff_only": staff.get_untracked()});
        let u = url.get_value();
        run_act(
            guard(busy, async move { api("POST", &u, Some(body)).await }),
            msg,
            step_up(),
            move |v| {
                msg.ok("Channel made.");
                if let Some(id) = v["id"].as_str() {
                    // Shown at once; the reload then brings its real place and post check.
                    let made =
                        json!({"id": id, "name": n, "kind": "text", "parent": p, "why": null});
                    chans.update(|c| {
                        if let Some(a) = c["channels"].as_array_mut() {
                            a.push(made)
                        }
                    });
                    pick(id.to_string())
                }
                reload();
            },
        )
    };
    view! {
        <div class="modal" node_ref=r role="dialog" aria-modal="true" aria-label="Choose a channel" on:keydown=move |e| dialog_keys(&e, close)>
            <div class="card chpick">
                <h2>{icon("channel_text")}"Choose a channel"</h2>
                <input aria-label="search channels" placeholder="Search a name or paste an ID" prop:value=q on:input=move |e| q.set(event_target_value(&e)) />
                <div class="chlist">
                    {move || {
                        let g = groups();
                        if g.is_empty() {
                            return view! { <p class="muted">"No channel matches."</p> }.into_any();
                        }
                        g.into_iter().map(|(h, list)| view! {
                            {h.map(|h| view! { <div class="chcat">{icon("category")}{h["name"].as_str().unwrap_or("?").to_uppercase()}</div> })}
                            {list.into_iter().map(row).collect_view()}
                        }).collect_view().into_any()
                    }}
                </div>
                <Show when=move || chans.with(|v| v["can_create"] == true)>
                    <Show when=move || adding.get() fallback=move || view! { <AddButton label="Add new channel" on:click=move |_| adding.set(true) /> }>
                        <div class="chnew">
                            <div class="row"><label>"Name"</label><input aria-label="new channel name" maxlength="100" prop:value=name on:input=move |e| name.set(event_target_value(&e)) /></div>
                            <div class="row"><label>"Category"</label>
                                <crate::form::ListPick items=Signal::derive(move || cats().into_iter().map(|c| (c["id"].as_str().unwrap_or("").to_string(), c["name"].as_str().unwrap_or("?").to_string())).collect()) get=parent set=Callback::new(move |v| parent.set(v)) none="No category" label="new channel category" />
                            </div>
                            <div class="row"><label>"Who can see it"</label>
                                <div class="seg" role="group" aria-label="who can see the new channel">
                                    <button class:on=move || !staff.get() on:click=move |_| staff.set(false)>"Everyone"</button>
                                    <button class:on=move || staff.get() on:click=move |_| staff.set(true)>"Only staff"</button>
                                </div>
                            </div>
                            <p class="muted small">"Made with Channel Setup's rules. Needs your 2FA code."</p>
                            <div class="acts"><button class="ghost" on:click=move |_| adding.set(false)>"Cancel"</button><button disabled=move || busy.get() on:click=move |_| create()>{busy_label(move || busy.get(), "Make channel")}</button></div>
                        </div>
                    </Show>
                </Show>
                <div class="acts"><button class="ghost" on:click=move |_| close()>"Close"</button></div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_buttons_follow_the_config() {
        let panels =
            json!([{"id": "help", "name": "Help"}, {"id": "old", "name": "Old", "enabled": false}]);
        let on = json!({"tickets": {"enabled": true, "panels": panels}, "verification": {"enabled": true, "mode": "Button"}});
        assert_eq!(
            ready_buttons(&on),
            [
                ("ticket:help".to_string(), "Open a ticket: Help".to_string()),
                ("rules".to_string(), "I accept the rules".to_string())
            ]
        );
        // Tickets off: no ticket button. A captcha, verification off or no mode saved (the default is a captcha): no rules button.
        for v in [
            json!({"enabled": true, "mode": "Captcha"}),
            json!({"enabled": false, "mode": "Button"}),
            json!({"enabled": true}),
        ] {
            assert!(ready_buttons(
                &json!({"tickets": {"enabled": false, "panels": panels}, "verification": v})
            )
            .is_empty());
        }
    }

    #[test]
    fn put_makes_and_prunes() {
        let mut c = json!({"body": {"embeds": [{}]}});
        put(&mut c, "/body/embeds/0/author/name", "a".into());
        assert_eq!(c["body"]["embeds"][0], json!({"author": {"name": "a"}}));
        put(&mut c, "/body/embeds/0/author/name", String::new());
        put(&mut c, "/body/embeds/0/title", String::new());
        assert_eq!(c["body"]["embeds"][0], json!({}));
        let mut c = json!({"body": {"embeds": [{"image": "x"}]}});
        put(&mut c, "/body/embeds/0/image/url", "u".into());
        assert_eq!(c["body"]["embeds"][0], json!({"image": {"url": "u"}}));
        // An index that is not there changes nothing; emptied text never removes the body.
        put(&mut c, "/body/embeds/3/title", "t".into());
        assert_eq!(c["body"]["embeds"].as_array().unwrap().len(), 1);
        let mut c = json!({"body": {"content": "x"}});
        put(&mut c, "/body/content", String::new());
        assert_eq!(c, json!({"body": {}}));
    }

    #[test]
    fn colour_only_embeds_are_left_out() {
        let c = json!({"body": {"content": "hi", "embeds": [{"color": 1}, {"color": 1, "title": "t"}]}});
        assert_eq!(
            body_to_save(&c),
            json!({"content": "hi", "embeds": [{"color": 1, "title": "t"}]})
        );
    }

    #[test]
    fn colours_and_calendar() {
        for c in [0x0d9df8, 0xff2445, 0x000000, 0xffffff, 0x9b7bff] {
            let (h, s, v) = rgb_to_hsv(c);
            assert_eq!(hsv_to_rgb(h, s, v), c, "{c:06x}");
        }
        assert_eq!(days_in(2026, 2), 28);
        assert_eq!(days_in(2028, 2), 29);
        assert_eq!(days_in(2100, 2), 28);
        assert_eq!(days_in(2000, 2), 29);
        assert_eq!(days_in(2026, 11), 30);
        // 1 November 2026 is a Sunday, 1 January 2024 a Monday.
        assert_eq!(first_weekday(2026, 11), 6);
        assert_eq!(first_weekday(2024, 1), 0);
        assert!(valid_hm("09:05") && !valid_hm("24:00") && !valid_hm("9:5"));
        assert!(https("https://a") && !https("https://") && !https("http://a"));
    }
}

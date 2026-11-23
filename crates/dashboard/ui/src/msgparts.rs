//! Message Builder parts typed into the message: variables, time tags and emojis. In the editor each
//! shows as a chip (`data-tok` holds its text); Brim fills variables and times in when it sends.
use crate::common::*;
use crate::embeds::{exec, valid_hm, Cal};
use crate::md::esc;
use leptos::{ev, portal::Portal, prelude::*, task::spawn_local};
use serde_json::{json, Value};
use std::cell::RefCell;
use wasm_bindgen::JsCast;

/// (token, name as Discord calls it, group, example). Role variables take a role id: `{role:<id>}`.
pub const VARS: [(&str, &str, &str, &str); 28] = [
    ("member", "Mention", "Member", "@Alex"),
    ("member.display_name", "Display Name", "Member", "Alex"),
    ("member.username", "Username", "Member", "alex"),
    ("member.id", "User ID", "Member", "123456789012345678"),
    (
        "member.avatar",
        "Avatar",
        "Member",
        "https://cdn.discordapp.com/embed/avatars/0.png",
    ),
    ("member.top_role", "Highest Role", "Member", "@Member"),
    ("member.joined_at", "Member Since", "Member", "2 months ago"),
    (
        "member.created_at",
        "Account Created",
        "Member",
        "3 years ago",
    ),
    ("server.name", "Server Name", "Server", "My Server"),
    ("server.id", "Server ID", "Server", "987654321098765432"),
    ("server.member_count", "Member Count", "Server", "1234"),
    ("server.boost_count", "Server Boosts", "Server", "7"),
    ("server.boost_level", "Boost Level", "Server", "Level 1"),
    ("server.owner", "Server Owner", "Server", "@Owner"),
    ("server.icon", "Server Icon", "Server", "/logo.svg"),
    (
        "server.created_at",
        "Server Created",
        "Server",
        "2 years ago",
    ),
    ("channel", "Channel Mention", "Channel", "#general"),
    ("channel.name", "Channel Name", "Channel", "general"),
    ("channel.id", "Channel ID", "Channel", "112233445566778899"),
    ("channel.topic", "Channel Topic", "Channel", "Say hi!"),
    ("channel.category", "Category", "Channel", "Community"),
    ("role", "Role Mention", "Role", "@Role"),
    ("role.member_count", "Members With Role", "Role", "12"),
    (
        "message.link",
        "Message Link",
        "Message",
        "https://discord.com/channels/…",
    ),
    ("trigger", "Matched Trigger", "Message", "hello"),
    ("date", "Date", "Date and time", ""),
    ("time", "Time", "Date and time", ""),
    ("weekday", "Day of the Week", "Date and time", ""),
];

/// (group, what its variables show).
const GROUPS: [(&str, &str); 6] = [
    ("Member", "The member who picked the menu option. Only in a menu answer."),
    ("Server", "This server."),
    ("Channel", "The channel the message is posted in."),
    ("Role", "The role you pick."),
    ("Message", "The message with the menu. Only in a menu answer."),
    ("Date and time", "When Brim sends it. Each member sees date and time in their own time zone; the day of the week is in UTC."),
];

/// (style, name) of Discord's time tags.
const STYLES: [(char, &str); 7] = [
    ('R', "Relative"),
    ('t', "Short time"),
    ('T', "Long time"),
    ('d', "Short date"),
    ('D', "Long date"),
    ('f', "Date and time"),
    ('F', "Full date and time"),
];

type Var = (&'static str, &'static str, &'static str, &'static str);

/// The group of a bot message's own variables.
const BOT_GROUP: (&str, &str) = (
    "Bot message",
    "Filled in by Brim when it sends this message.",
);

thread_local! {
    /// A bot message's variables, while its editor is open: they replace the builder's.
    static BOT: RefCell<Vec<Var>> = const { RefCell::new(Vec::new()) };
}

/// Opens (a list) or ends (empty) the bot message mode: (name, label, example) per variable.
/// shortcut: the texts are leaked, a few KB per opened message; intern them if pages stay open for days.
pub fn set_bot_vars(vars: &[(String, String, String)]) {
    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
    let v = vars
        .iter()
        .map(|(n, l, e)| (leak(n), leak(l), BOT_GROUP.0, leak(e)))
        .collect();
    BOT.with(|b| *b.borrow_mut() = v);
}

/// The one sentence about placeholders, with examples from this message's own variables; none
/// where the message has no variables.
pub fn placeholder_hint() -> Option<AnyView> {
    let names: Vec<String> = BOT.with(|b| b.borrow().iter().take(3).map(|v| format!("{{{}}}", v.0)).collect());
    let list = match names.as_slice() {
        [] => return None,
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        _ => return None,
    };
    Some(view! { <p class="muted small">{format!("Placeholders such as {list} show each post's own details: Brim fills them in every time it posts, so one design works every time. Press ")}<b>"{ }"</b>" to add one."</p> }.into_any())
}

fn bot_mode() -> bool {
    BOT.with(|b| !b.borrow().is_empty())
}

fn all_vars() -> Vec<Var> {
    BOT.with(|b| {
        if b.borrow().is_empty() {
            VARS.to_vec()
        } else {
            b.borrow().clone()
        }
    })
}

fn groups() -> Vec<(&'static str, &'static str)> {
    if bot_mode() {
        vec![BOT_GROUP]
    } else {
        GROUPS.to_vec()
    }
}

fn var(name: &str) -> Option<Var> {
    all_vars().into_iter().find(|v| v.0 == name)
}

/// A variable that needs a member who did something; the bot has one only for a menu answer.
pub fn member_only(name: &str) -> bool {
    var(name).is_some_and(|v| matches!(v.2, "Member" | "Message"))
}

/// A `{name}` or `{role:<id>}` at the start of `s`: (name, role id, length), as the bot reads them.
pub fn var_at(s: &str) -> Option<(&'static str, Option<&str>, usize)> {
    let end = s.find('}')?;
    let inner = s.strip_prefix('{')?.get(..end - 1)?;
    let (name, arg) = match inner.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (inner, None),
    };
    let v = var(name)?;
    let ok = match arg {
        None => v.2 != "Role",
        Some(a) => {
            v.2 == "Role" && !a.is_empty() && a.len() <= 20 && a.bytes().all(|b| b.is_ascii_digit())
        }
    };
    ok.then_some((v.0, arg, end + 1))
}

/// A `<t:unix:style>` or `<t:+seconds:style>` at the start of `s`: (seconds, counted from sending,
/// style, length).
pub fn time_at(s: &str) -> Option<(i64, bool, char, usize)> {
    let r = s.strip_prefix("<t:")?;
    let (plus, r) = match r.strip_prefix('+') {
        Some(r) => (true, r),
        None => (false, r),
    };
    let digits = r.bytes().take_while(u8::is_ascii_digit).count();
    // As the bot: at most a year after sending; a date within what a browser can show.
    let max = if plus {
        365 * 86_400
    } else {
        8_640_000_000_000
    };
    let n = r[..digits].parse().ok().filter(|n| *n <= max)?;
    let r = r[digits..].strip_prefix(':')?;
    let style = r.chars().next().filter(|c| "tTdDfFR".contains(*c))?;
    r[1..]
        .starts_with('>')
        .then_some((n, plus, style, 3 + plus as usize + digits + 3))
}

/// A custom emoji `<:name:id>` or `<a:name:id>` at the start of `s`: (animated, name, id, length).
pub fn emoji_at(s: &str) -> Option<(bool, &str, &str, usize)> {
    let r = s.strip_prefix('<')?;
    let (anim, r) = match r.strip_prefix("a:") {
        Some(r) => (true, r),
        None => (false, r.strip_prefix(':')?),
    };
    let (name, r) = r.split_once(':')?;
    let n = r.bytes().take_while(u8::is_ascii_digit).count();
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && n > 0
        && r[n..].starts_with('>');
    ok.then(|| (anim, name, &r[..n], s.len() - r.len() + n + 1))
}

pub fn emoji_url(id: &str, anim: bool) -> String {
    format!(
        "https://cdn.discordapp.com/emojis/{id}.{}",
        if anim { "gif" } else { "webp" }
    )
}

thread_local! {
    /// The server's roles (id, name), for the names on role chips.
    static ROLES: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// The text box and place a picked variable, time or emoji goes to, kept while a popup has the focus.
    static AT: RefCell<Option<(web_sys::HtmlElement, web_sys::Range)>> = const { RefCell::new(None) };
}

fn role_name(id: &str) -> String {
    ROLES.with(|r| {
        r.borrow()
            .iter()
            .find(|(i, _)| i == id)
            .map_or("unknown role".into(), |(_, n)| {
                n.trim_start_matches('@').to_string()
            })
    })
}

/// A time tag as members see it now.
fn time_label(n: i64, plus: bool, style: char) -> String {
    let secs = if plus {
        (js_sys::Date::now() / 1000.0) as i64 + n
    } else {
        n
    };
    crate::discord::time(secs.max(1), &style.to_string())
}

/// The chip for one token; other text comes back escaped.
pub fn chip(tok: &str) -> String {
    let attrs = format!("contenteditable=\"false\" data-tok=\"{}\"", esc(tok));
    if let Some((name, arg, _)) = var_at(tok) {
        let label = var(name).map_or(name, |v| v.1);
        let label = match arg {
            Some(id) => format!("{label}: @{}", role_name(id)),
            None => label.into(),
        };
        let red = if member_only(name) { " mem" } else { "" };
        return format!("<span class=\"vchip{red}\" {attrs} title=\"{}\">{}</span>", esc(&label), esc(&label));
    }
    if let Some((n, plus, style, _)) = time_at(tok) {
        return format!(
            "<span class=\"vchip ts\" {attrs}>🕒 {}</span>",
            esc(&time_label(n, plus, style))
        );
    }
    if let Some((anim, name, id, _)) = emoji_at(tok) {
        return format!(
            // An empty alt: a deleted emoji's name would push the line apart in the editor.
            "<img class=\"emo\" {attrs} alt=\"\" title=\":{}:\" src=\"{}\">",
            esc(name),
            emoji_url(id, anim)
        );
    }
    esc(tok)
}

/// Text as the editor shows it: escaped, with variables, time tags and custom emojis as chips.
pub fn chips(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        let t = &s[i..];
        let len = match c {
            '{' => var_at(t).map(|x| x.2),
            '<' => time_at(t).map(|x| x.3).or_else(|| emoji_at(t).map(|x| x.3)),
            _ => None,
        };
        match len {
            Some(l) => {
                out.push_str(&chip(&t[..l]));
                i += l;
            }
            None => {
                out.push_str(&esc(&c.to_string()));
                i += c.len_utf8();
            }
        }
    }
    out
}

fn example(name: &str, arg: Option<&str>) -> String {
    if bot_mode() {
        return var(name).map_or(String::new(), |v| v.3.replace("\\n", "\n"));
    }
    match (name, arg) {
        ("date", _) => crate::discord::time(0, "D"),
        ("time", _) => crate::discord::time(0, "t"),
        ("weekday", _) => crate::discord::time(0, "F")
            .split(',')
            .next()
            .unwrap_or("")
            .to_string(),
        ("role", Some(id)) => format!("@{}", role_name(id)),
        _ => var(name).map_or(String::new(), |v| v.3.to_string()),
    }
}

/// The message with example values in place of the variables, as the preview shows it.
pub fn examples(v: &Value) -> Value {
    match v {
        Value::String(s) => {
            let mut out = String::with_capacity(s.len());
            let mut rest = s.as_str();
            while let Some(i) = rest.find('{') {
                out.push_str(&rest[..i]);
                match var_at(&rest[i..]) {
                    Some((name, arg, len)) => {
                        out.push_str(&example(name, arg));
                        rest = &rest[i + len..];
                    }
                    None => {
                        out.push('{');
                        rest = &rest[i + 1..];
                    }
                }
            }
            out.push_str(rest);
            json!(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(examples).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, x)| (k.clone(), examples(x))).collect())
        }
        x => x.clone(),
    }
}

/// The focused text box of the message, if any.
pub fn focused_ed() -> Option<web_sys::HtmlElement> {
    document()
        .active_element()
        .filter(|e| e.class_list().contains("ed"))
        .and_then(|e| e.dyn_into().ok())
}

/// Keeps where a pick goes: the cursor in `el` (or the focused box), else the end of `el`. With
/// `node`, that chip is what a pick replaces. False when there is no box.
pub fn keep(el: Option<web_sys::HtmlElement>, node: Option<&web_sys::Node>) -> bool {
    let Some(el) = el.or_else(focused_ed) else {
        return false;
    };
    let inside = |r: &web_sys::Range| {
        r.common_ancestor_container()
            .is_ok_and(|n| el.contains(Some(&n)))
    };
    let sel = window()
        .get_selection()
        .ok()
        .flatten()
        .filter(|s| s.range_count() > 0)
        .and_then(|s| s.get_range_at(0).ok())
        .filter(|r| node.is_none() && inside(r));
    let r = match (sel, document().create_range()) {
        (Some(r), _) => r,
        (None, Ok(r)) => {
            match node {
                Some(n) => {
                    let _ = r.select_node(n);
                }
                None => {
                    let _ = r.select_node_contents(&el);
                    r.collapse_with_to_start(false);
                }
            }
            r
        }
        _ => return false,
    };
    AT.with(|a| *a.borrow_mut() = Some((el, r)));
    true
}

/// Where the kept place is on screen.
fn kept_rect() -> Option<web_sys::DomRect> {
    AT.with(|a| {
        a.borrow().as_ref().map(|(el, r)| {
            let rr = r.get_bounding_client_rect();
            if rr.height() > 0.0 {
                rr
            } else {
                el.get_bounding_client_rect()
            }
        })
    })
}

fn kept_answer() -> bool {
    AT.with(|a| {
        a.borrow()
            .as_ref()
            .is_some_and(|(e, _)| e.has_attribute("data-answer"))
    })
}

/// Puts the kept place back as the selection, in its box.
fn restore() -> Option<web_sys::HtmlElement> {
    let (el, r) = AT.with(|a| a.borrow_mut().take())?;
    let _ = el.focus();
    if let Some(sel) = window().get_selection().ok().flatten() {
        let _ = sel.remove_all_ranges();
        let _ = sel.add_range(&r);
    }
    Some(el)
}

/// Puts `html` at the kept place (in place of a chip being changed; after removing the typed `{`
/// when `brace`); empty removes the chip. Undo works and the box's input event saves it.
fn insert(html: &str, brace: bool) {
    if restore().is_none() {
        return;
    }
    if brace {
        exec("delete", "");
    }
    if html.is_empty() {
        exec("delete", "");
    } else {
        exec("insertHTML", html);
    }
}

/// A popup's place on the page for a control at `left` whose bottom is at `bottom` (window
/// coordinates); it never runs off the right edge.
pub fn place(left: f64, bottom: f64) -> (f64, f64) {
    let (sx, sy) = (
        window().scroll_x().unwrap_or(0.0),
        window().scroll_y().unwrap_or(0.0),
    );
    let vw = window()
        .inner_width()
        .ok()
        .and_then(|w| w.as_f64())
        .unwrap_or(1024.0);
    // Window coordinates are real pixels, the popup's `left`/`top` are zoomed ones (the screen scale).
    let z = crate::common::screen_zoom();
    (
        (left + sx).min(vw - 316.0 * z + sx).max(8.0 * z + sx) / z,
        (bottom + sy) / z + 6.0,
    )
}

/// Near the bottom of the window a popup opens upwards, so it is never cut off.
pub fn fit(el: NodeRef<leptos::html::Div>, top: f64) {
    let Some(e) = el.get() else { return };
    let r = e.get_bounding_client_rect();
    let vh = window()
        .inner_height()
        .ok()
        .and_then(|h| h.as_f64())
        .unwrap_or(800.0);
    if r.bottom() > vh - 8.0 && r.height() < vh {
        let z = crate::common::screen_zoom();
        let sy = window().scroll_y().unwrap_or(0.0);
        let _ = web_sys::HtmlElement::style(&e).set_property(
            "top",
            &format!("{}px", (top - (r.bottom() - vh + 8.0) / z).max((sy + 8.0) / z)),
        );
    }
}

/// Where a picked emoji goes: the text box with the cursor, a button, a menu option.
#[derive(Clone, Copy, PartialEq)]
pub enum Dest {
    Text,
    Button(usize, usize),
    Opt(usize, usize),
}

impl Dest {
    fn ptr(self) -> Option<String> {
        match self {
            Dest::Text => None,
            Dest::Button(r, b) => Some(format!("/body/components/{r}/components/{b}")),
            Dest::Opt(r, o) => Some(format!("/body/components/{r}/components/0/options/{o}")),
        }
    }
}

/// The small popups over the editor: the variable picker (after a typed `{`), a variable chip, the
/// time tag panel (changing that chip), the emoji picker.
#[derive(Clone, PartialEq)]
pub enum Sub {
    Vars(bool),
    Var(String),
    Time(Option<String>),
    Emoji(Dest),
}

#[derive(Clone, Copy)]
pub struct Parts {
    pub cur: RwSignal<Value>,
    pub sub: RwSignal<Option<(Sub, f64, f64)>>,
    pub roles: RwSignal<Vec<(String, String)>>,
    /// The ready-made buttons this server can use now: (action, what it does).
    pub ready: RwSignal<Vec<(String, String)>>,
    guild: StoredValue<String>,
    custom: RwSignal<Option<Value>>,
    standard: RwSignal<Option<Value>>,
    recent: RwSignal<Vec<Value>>,
    /// Counts undo and redo steps: every text box draws itself again, the focused one too.
    pub redraw: RwSignal<u32>,
}

impl Parts {
    pub fn new(cur: RwSignal<Value>, guild: StoredValue<String>) -> Parts {
        let p = Parts {
            cur,
            sub: RwSignal::new(None),
            roles: RwSignal::new(vec![]),
            ready: RwSignal::new(vec![]),
            guild,
            custom: RwSignal::new(None),
            standard: RwSignal::new(None),
            recent: RwSignal::new(vec![]),
            redraw: RwSignal::new(0),
        };
        Effect::new(move |_| ROLES.with(|r| *r.borrow_mut() = p.roles.get()));
        p
    }

    /// Opens `s` at the kept place (`at` None) or under the element `at`.
    pub fn open(self, s: Sub, at: Option<&web_sys::Element>) {
        let r = match at {
            Some(e) => Some(e.get_bounding_client_rect()),
            None => kept_rect(),
        };
        if let Some(r) = r {
            let (l, t) = place(r.left(), r.bottom());
            self.sub.set(Some((s, l, t)));
        }
    }

    /// Closes the popup and puts the cursor back where it was.
    pub fn close(self) {
        self.sub.set(None);
        restore();
    }

    /// The editor box handles `{` and clicks on chips with this.
    pub fn on_brace(self) {
        request_animation_frame(move || {
            if keep(None, None) {
                self.open(Sub::Vars(true), None)
            }
        });
    }

    /// A click on a chip in `el` opens what it is: a variable or a time tag.
    pub fn chip_click(self, el: web_sys::HtmlElement, e: &ev::MouseEvent) {
        let Some(c) = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|t| t.closest("[data-tok]").ok().flatten())
        else {
            return;
        };
        let tok = c.get_attribute("data-tok").unwrap_or_default();
        let s = if var_at(&tok).is_some() {
            Sub::Var(tok)
        } else if time_at(&tok).is_some() {
            Sub::Time(Some(tok))
        } else {
            return;
        };
        if keep(Some(el), Some(&c)) {
            self.open(s, Some(&c));
        }
    }
}

/// The popup layer over the editor and its popups.
pub fn sub_view(p: Parts) -> impl IntoView {
    move || {
        p.sub.get().map(|(s, left, top)| {
            let el = NodeRef::<leptos::html::Div>::new();
            Effect::new(move |_| {
                p.cur.track();
                fit(el, top)
            });
            let body = move |s: Sub| match s {
                Sub::Vars(brace) => vars_body(p, brace),
                Sub::Var(tok) => var_body(p, tok),
                Sub::Time(old) => time_body(p, old),
                Sub::Emoji(d) => emoji_body(p, d),
            };
            view! {
                <Portal>
                    <div class="popbg sub" on:click=move |_| p.close()></div>
                    <div node_ref=el class="pop subpop" role="dialog" style=format!("left:{left}px;top:{top}px")>{body(s.clone())}</div>
                </Portal>
            }
        })
    }
}

fn head(p: Parts, icon: &'static str, title: String) -> impl IntoView {
    view! { <h3>{crate::icons::icon(icon)}{title}<span class="sp"></span><button class="ghost" aria-label="Close" on:click=move |_| p.close()>"✕"</button></h3> }
}

/// Focuses a popup's search box once it is on the page.
fn focus_soon(r: NodeRef<leptos::html::Input>) {
    request_animation_frame(move || {
        if let Some(i) = r.get_untracked() {
            let _ = i.focus();
        }
    });
}

fn vars_body(p: Parts, brace: bool) -> AnyView {
    let answer = kept_answer();
    let q = RwSignal::new(String::new());
    let role_for = RwSignal::new(None::<&'static str>);
    let search = NodeRef::<leptos::html::Input>::new();
    focus_soon(search);
    let pick = move |tok: String| {
        p.sub.set(None);
        insert(&chip(&tok), brace);
    };
    let list = move || {
        let n = q.get().to_lowercase();
        let vars = all_vars();
        groups().into_iter().map(|(g, what)| {
            let items: Vec<Var> = vars.iter().copied().filter(|v| v.2 == g && (n.is_empty() || v.0.contains(&n) || v.1.to_lowercase().contains(&n) || g.to_lowercase().contains(&n))).collect();
            (!items.is_empty()).then(|| view! {
                <small title=what>{g}</small>
                {items.into_iter().map(|(tok, name, _, _)| {
                    let why = if tok == "trigger" { Some("Comes with trigger rules.") } else if member_only(tok) && !answer { Some("Only in a menu answer, where there is a member who picked it.") } else { None };
                    let shown = if g == "Role" { format!("{{{tok}:…}}") } else { format!("{{{tok}}}") };
                    view! { <button class="vitem" disabled=why.is_some() title=why on:click=move |_| if g == "Role" { role_for.set(Some(tok)) } else { pick(format!("{{{tok}}}")) }>
                        <b>{name}</b><code>{shown}</code><span>{why.map_or_else(|| example(tok, None), String::from)}</span></button> }
                }).collect_view()}
            })
        }).collect_view()
    };
    view! {
        {head(p, "code", "Variables".into())}
        <input node_ref=search placeholder="Search variables" aria-label="search variables" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e)) />
        {move || role_for.get().map(|tok| view! {
            <div class="row"><crate::form::ListPick items=p.roles get=Signal::derive(String::new) set=Callback::new(move |id: String| if !id.is_empty() { pick(format!("{{{tok}:{id}}}")) }) none="Pick the role..." label="role" /></div>
        })}
        <div class="vlist">{list}</div>
        <p class="muted small">"Brim fills these in when it sends the message."</p>
    }.into_any()
}

fn var_body(p: Parts, tok: String) -> AnyView {
    let Some((name, arg, _)) = var_at(&tok) else {
        return ().into_any();
    };
    let v = var(name).unwrap_or(("", "", "", ""));
    let what = groups()
        .into_iter()
        .find(|g| g.0 == v.2)
        .map_or("", |g| g.1);
    let ex = example(name, arg);
    let arg = arg.map(String::from);
    view! {
        {head(p, "code", v.1.into())}
        <p class="muted small">{what}" Example: "{ex}</p>
        {arg.map(|id| view! { <div class="row"><label>"Role"</label><crate::form::ListPick items=p.roles get=Signal::derive(move || id.clone()) set=Callback::new(move |r: String| { p.sub.set(None); insert(&chip(&format!("{{{name}:{r}}}")), false) }) none="" label="role" /></div> })}
        <div class="row"><button class="ghost" on:click=move |_| { p.sub.set(None); insert("", false) }>"Remove"</button><span class="sp"></span>
            <button class="ghost" on:click=move |_| p.open(Sub::Vars(false), None)>"Change"</button></div>
    }.into_any()
}

fn time_body(p: Parts, old: Option<String>) -> AnyView {
    let (n0, plus0) = old
        .as_deref()
        .and_then(time_at)
        .map_or((7200, true), |(n, plus, ..)| (n, plus));
    let mode = RwSignal::new(if plus0 { "after" } else { "fixed" });
    let after = RwSignal::new(if plus0 { n0 } else { 7200 });
    let unit = RwSignal::new(if n0 % 86_400 == 0 {
        86_400
    } else if n0 % 3600 == 0 {
        3600
    } else {
        60
    });
    let d = js_sys::Date::new_0();
    if !plus0 {
        d.set_time(n0 as f64 * 1000.0);
    }
    let ym = RwSignal::new((d.get_full_year() as i32, d.get_month() + 1));
    let day = RwSignal::new((d.get_full_year() as i32, d.get_month() + 1, d.get_date()));
    let hm = RwSignal::new(format!("{:02}:{:02}", d.get_hours(), d.get_minutes()));
    // (seconds, counted from sending) for the chosen time.
    let at = move || {
        if mode.get() == "after" {
            return (after.get().clamp(1, 365 * 86_400), true);
        }
        let (y, m, dd) = day.get();
        let (h, mi) = hm.get().split_once(':').map_or((18, 0), |(h, m)| {
            (h.parse().unwrap_or(18), m.parse().unwrap_or(0))
        });
        let t =
            js_sys::Date::new_with_year_month_day_hr_min(y as u32, m as i32 - 1, dd as i32, h, mi);
        ((t.get_time() / 1000.0) as i64, false)
    };
    let pick = move |style: char| {
        let (n, plus) = at();
        p.sub.set(None);
        insert(
            &chip(&format!("<t:{}{n}:{style}>", if plus { "+" } else { "" })),
            false,
        );
    };
    view! {
        {head(p, "clock", "Time".into())}
        <div class="seg wrap">{[("after", "After sending"), ("fixed", "Date and time")].map(|(k, l)| view! {
            <button class:on=move || mode.get() == k on:click=move |_| mode.set(k)>{l}</button> }).collect_view()}</div>
        {move || if mode.get() == "after" { view! {
            <div class="row"><input class="num" type="number" min="1" aria-label="how long after" prop:value=move || (after.get() / unit.get()).to_string()
                on:input=move |e| after.set(event_target_value(&e).parse::<i64>().unwrap_or(1).max(1) * unit.get_untracked()) />
                <select aria-label="unit" on:change=move |e| { let n = after.get_untracked() / unit.get_untracked(); unit.set(event_target_value(&e).parse().unwrap_or(3600)); after.set(n * unit.get_untracked()) }>
                    {[(60, "minutes"), (3600, "hours"), (86_400, "days")].map(|(v, l)| view! { <option value=v.to_string() prop:selected=move || unit.get() == v>{l}</option> }).collect_view()}
                </select></div>
            <p class="muted small">"Counted from when Brim posts it. At most a year."</p>
        }.into_any() } else { view! {
            <Cal ym picked=Signal::derive(move || Some(day.get())) pick=move |dd| { let (y, m) = ym.get_untracked(); day.set((y, m, dd)) } />
            <div class="row"><span class="muted small">"Time"</span><input class="hm" maxlength="5" aria-label="time" prop:value=move || hm.get()
                class:need=move || !valid_hm(&hm.get()) on:input=move |e| { let v = event_target_value(&e); if valid_hm(&v) { hm.set(v) } } /></div>
        }.into_any() }}
        <small class="muted">"How it looks (each member sees their own time zone)"</small>
        <div class="vlist">{STYLES.map(|(s, name)| view! { <button class="vitem" on:click=move |_| pick(s)><b>{name}</b><span>{move || { let (n, plus) = at(); time_label(n, plus, s) }}</span></button> }).collect_view()}</div>
    }.into_any()
}

fn emoji_body(p: Parts, d: Dest) -> AnyView {
    // A failed list is kept as {"error"} to show it, and asked again the next time the picker opens.
    let missing = |s: RwSignal<Option<Value>>| {
        s.with_untracked(|v| v.as_ref().is_none_or(|v| v.get("error").is_some()))
    };
    if missing(p.custom) {
        let path = format!("/api/guilds/{}/embeds/emojis", p.guild.get_value());
        spawn_local(async move {
            let v = api("GET", &path, None)
                .await
                .unwrap_or_else(|e| json!({"error": e}));
            p.custom.set(Some(v));
        });
    }
    if missing(p.standard) {
        spawn_local(async move {
            let v = match gloo_net::http::Request::get("/emoji.json").send().await {
                Ok(r) if r.ok() => r.json::<Value>().await.unwrap_or_else(
                    |e| json!({"error": format!("The standard emoji list could not be read: {e}")}),
                ),
                Ok(r) => {
                    json!({"error": format!("The standard emoji list could not be loaded: the server answered {} {}.", r.status(), r.status_text())})
                }
                Err(_) => {
                    json!({"error": "The standard emoji list could not be loaded: the dashboard server did not answer."})
                }
            };
            p.standard.set(Some(v));
        });
    }
    let error = |s: RwSignal<Option<Value>>| {
        s.with(|v| {
            v.as_ref()
                .and_then(|v| v["error"].as_str().map(String::from))
        })
    };
    let tab = RwSignal::new("server");
    let q = RwSignal::new(String::new());
    let search = NodeRef::<leptos::html::Input>::new();
    focus_soon(search);
    let pick = move |e: Value| {
        p.recent.update(|r| {
            r.retain(|x| *x != e);
            r.insert(0, e.clone());
            r.truncate(16);
        });
        p.sub.set(None);
        match d.ptr() {
            None => {
                let name = e["name"].as_str().unwrap_or("");
                let html = match e["id"].as_str() {
                    Some(id) => chip(&format!(
                        "<{}:{name}:{id}>",
                        if e["animated"] == true { "a" } else { "" }
                    )),
                    None => esc(name),
                };
                insert(&html, false)
            }
            Some(ptr) => {
                AT.with(|a| a.borrow_mut().take());
                p.cur.update(|c| {
                    if let Some(o) = c.pointer_mut(&ptr).and_then(Value::as_object_mut) {
                        o.insert("emoji".into(), e);
                    }
                })
            }
        }
    };
    let cell = move |e: Value| {
        let face = match e["id"].as_str() {
            Some(id) => view! { <img class="emo" alt=format!(":{}:", e["name"].as_str().unwrap_or("")) src=emoji_url(id, e["animated"] == true) /> }.into_any(),
            None => view! { <span>{e["name"].as_str().unwrap_or("").to_string()}</span> }.into_any(),
        };
        let label = e["label"]
            .as_str()
            .or(e["name"].as_str())
            .unwrap_or("")
            .to_string();
        let v = json!({"id": e["id"], "name": e["name"], "animated": e["animated"]});
        let v = match v["id"].is_null() {
            true => json!({"name": e["name"]}),
            false => v,
        };
        view! { <button class="ecell" title=label.clone() aria-label=label on:click=move |_| pick(v.clone())>{face}</button> }
    };
    let custom = move |key: &str| {
        let n = q.get().to_lowercase();
        let list: Vec<Value> = p.custom.with(|c| {
            c.as_ref()
                .and_then(|c| c[key].as_array())
                .into_iter()
                .flatten()
                .filter(|e| {
                    e["name"]
                        .as_str()
                        .is_some_and(|x| x.to_lowercase().contains(&n))
                })
                .cloned()
                .collect()
        });
        if p.custom.with(Option::is_none) {
            return view! { <p class="small"><Working /></p> }.into_any();
        }
        if let Some(e) = error(p.custom) {
            return view! { <p class="small err">{e}</p> }.into_any();
        }
        if list.is_empty() {
            let t = if key == "server" {
                "This server has no emojis of its own."
            } else {
                "Brim has no emojis of its own."
            };
            return view! { <p class="muted small">{t}</p> }.into_any();
        }
        view! { <div class="egrid">{list.into_iter().map(cell).collect_view()}</div> }.into_any()
    };
    let standard = move || {
        let n = q.get().to_lowercase();
        let Some(all) = p.standard.get() else {
            return view! { <p class="small"><Working /></p> }.into_any();
        };
        if let Some(e) = error(p.standard) {
            return view! { <p class="small err">{e}</p> }.into_any();
        }
        let groups = all.as_array().cloned().unwrap_or_default();
        let as_emoji = |x: &Value| json!({"name": x[0], "label": x[1]});
        let matches = |x: &Value| {
            n.is_empty()
                || x[1].as_str().is_some_and(|l| l.contains(&n))
                || x[2].as_str().is_some_and(|t| t.contains(&n))
        };
        view! {
            {(n.is_empty() && !p.recent.with(Vec::is_empty)).then(|| view! { <small>"Recent"</small><div class="egrid">{p.recent.get().into_iter().map(cell).collect_view()}</div> })}
            {groups.into_iter().map(|g| {
                let list: Vec<Value> = g[1].as_array().into_iter().flatten().filter(|x| matches(x)).map(as_emoji).collect();
                (!list.is_empty()).then(|| view! { <small>{g[0].as_str().unwrap_or("").to_string()}</small><div class="egrid">{list.into_iter().map(cell).collect_view()}</div> })
            }).collect_view()}
        }.into_any()
    };
    let has = d.ptr().is_some_and(|ptr| {
        p.cur
            .with_untracked(|c| c.pointer(&ptr).is_some_and(|o| !o["emoji"].is_null()))
    });
    view! {
        {head(p, "emoji", "Emoji".into())}
        <div class="seg wrap">{[("server", "This server"), ("bot", "Brim"), ("std", "Standard")].map(|(k, l)| view! {
            <button class:on=move || tab.get() == k on:click=move |_| tab.set(k)>{l}</button> }).collect_view()}</div>
        <input node_ref=search placeholder="Search emojis" aria-label="search emojis" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e)) />
        <div class="elist">{move || match tab.get() { "std" => standard(), k => custom(k) }}</div>
        {has.then(|| view! { <div class="row"><button class="ghost" on:click=move |_| { p.sub.set(None); p.cur.update(|c| { if let Some(o) = d.ptr().and_then(|ptr| c.pointer_mut(&ptr)).and_then(Value::as_object_mut) { o.remove("emoji"); } }) }>"Remove emoji"</button></div> })}
    }.into_any()
}

/// A button's or option's emoji in the editor; a grey emoji icon when there is none.
pub fn face(e: Option<Value>) -> AnyView {
    match e.filter(|e| e["name"].is_string()) {
        Some(e) => match e["id"].as_str() {
            Some(id) => {
                view! { <img class="emo" alt="" src=emoji_url(id, e["animated"] == true) /> }
                    .into_any()
            }
            None => {
                view! { <span>{e["name"].as_str().unwrap_or("").to_string()}</span> }.into_any()
            }
        },
        None => view! { <span class="noemo">{crate::icons::icon("emoji")}</span> }.into_any(),
    }
}

/// The toolbar's Variables, Time and Emoji buttons. Each acts on the box with the cursor, else on
/// the first text box of the card or popup the toolbar is in.
pub fn tool_buttons(p: Parts) -> impl IntoView {
    let go = move |e: ev::MouseEvent, s: Sub| {
        e.prevent_default();
        let t = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok());
        let first = t
            .as_ref()
            .and_then(|t| t.closest(".pop, .mcard").ok().flatten())
            .and_then(|c| c.query_selector(".ed[data-md]").ok().flatten())
            .and_then(|e| e.dyn_into().ok());
        if keep(None, None) || keep(first, None) {
            p.open(s, t.as_ref())
        }
    };
    view! {
        <button class="ghost" title="Variables" aria-label="Variables" on:mousedown=move |e| go(e, Sub::Vars(false))>"{ }"</button>
        <button class="ghost" title="Time" aria-label="Time" on:mousedown=move |e| go(e, Sub::Time(None))>{crate::icons::icon("clock")}</button>
        <button class="ghost" title="Emoji" aria-label="Emoji" on:mousedown=move |e| go(e, Sub::Emoji(Dest::Text))>{crate::icons::icon("emoji")}</button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tokens_as_the_bot_does() {
        assert_eq!(var_at("{server.name} x"), Some(("server.name", None, 13)));
        assert_eq!(var_at("{role:12}"), Some(("role", Some("12"), 9)));
        assert_eq!(var_at("{role}"), None);
        assert_eq!(var_at("{server.name:1}"), None);
        assert_eq!(var_at("{nope}"), None);
        assert_eq!(time_at("<t:+3600:R> x"), Some((3600, true, 'R', 11)));
        assert_eq!(time_at("<t:17:F>"), Some((17, false, 'F', 8)));
        assert_eq!(time_at("<t:+x:R>"), None);
        assert_eq!(time_at("<t:+99999999999:R>"), None);
        assert_eq!(time_at("<t:1:Q>"), None);
        assert_eq!(emoji_at("<:ok:42>!"), Some((false, "ok", "42", 8)));
        assert_eq!(emoji_at("<a:ok_2:42>"), Some((true, "ok_2", "42", 11)));
        assert_eq!(emoji_at("<:x:>"), None);
        assert_eq!(emoji_at("<@42>"), None);
        assert!(member_only("member.id") && member_only("message.link") && !member_only("channel"));
        assert_eq!(VARS.len(), 28);
    }

    #[test]
    fn examples_fill_known_variables_only() {
        let v = examples(&json!({"a": "Hi {channel} {x} {", "b": ["{server.name}"], "c": 1}));
        assert_eq!(
            v,
            json!({"a": "Hi #general {x} {", "b": ["My Server"], "c": 1})
        );
    }

    #[test]
    fn bot_message_variables_replace_the_builders() {
        set_bot_vars(&[
            ("reason".into(), "Reason".into(), "Spam".into()),
            ("time".into(), "Time left".into(), "10 minutes".into()),
        ]);
        assert_eq!(var_at("{reason}"), Some(("reason", None, 8)));
        assert_eq!(var_at("{server.name}"), None);
        assert_eq!(
            examples(&json!("{reason} {time}")),
            json!("Spam 10 minutes")
        );
        set_bot_vars(&[]);
        assert_eq!(var_at("{reason}"), None);
        assert!(var_at("{server.name}").is_some());
    }
}

/// The list frame of the schedule, trigger and sticky cards: the open message's `path` rows, the
/// editor's draft, busy and the rank lock. Opening another message reloads the list, and a list
/// that lands after that is dropped.
#[derive(Clone, Copy)]
pub struct RuleList {
    pub embed: Signal<Option<i64>>,
    /// Why a rule cannot be on yet: unsaved changes (it sends the saved message), or the
    /// message's `missing` reason.
    pub ready_why: Signal<Option<String>>,
    pub rows: RwSignal<Vec<Value>>,
    pub editing: RwSignal<Option<RwSignal<Value>>>,
    // Made here: a signal made in a click handler belongs to the list, and a list redraw would
    // drop it while the editor still reads it.
    draft: RwSignal<Value>,
    pub busy: RwSignal<bool>,
    pub locked: Signal<bool>,
    base: StoredValue<String>,
    path: &'static str,
    msg: Msg,
}

impl RuleList {
    pub fn new(base: String, path: &'static str, missing: &'static str, cur: RwSignal<Value>, dirty: Memo<bool>, rank: Signal<u8>, msg: Msg) -> Self {
        let embed = Signal::derive(move || cur.with(|c| c["id"].as_i64()));
        let ready_why = Signal::derive(move || {
            if dirty.get() {
                Some("save or cancel your changes to the message first".to_string())
            } else {
                cur.with(|c| c[missing].as_str().map(String::from))
            }
        });
        let l = RuleList {
            embed,
            ready_why,
            rows: RwSignal::new(vec![]),
            editing: RwSignal::new(None),
            draft: RwSignal::new(Value::Null),
            busy: RwSignal::new(false),
            locked: Signal::derive(move || rank.get() < 3),
            base: StoredValue::new(base),
            path,
            msg,
        };
        Effect::new(move |_| {
            embed.track();
            l.load()
        });
        l
    }

    pub fn open(self, v: Value) {
        self.draft.set(v);
        self.editing.set(Some(self.draft))
    }

    pub fn url(self) -> Option<String> {
        self.embed.get_untracked().map(|id| format!("{}/{id}/{}", self.base.get_value(), self.path))
    }

    pub fn load(self) {
        let Some(u) = self.url() else { return self.rows.set(vec![]) };
        let asked = self.embed.get_untracked();
        spawn_local(async move {
            let r = api("GET", &u, None).await;
            // Another message was opened meanwhile, or the page left.
            if self.embed.try_get_untracked() != Some(asked) {
                return;
            }
            match r {
                Ok(v) => self.rows.set(v.as_array().cloned().unwrap_or_default()),
                Err(e) => self.msg.err(e),
            }
        })
    }
}

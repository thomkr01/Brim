//! Discord look-alike messages for the wizard previews and the Message Builder. The bot's own
//! messages come from `/api/previews`: the bot builds them at startup with the code that sends
//! them, so the previews match Discord.
use crate::md::{esc, inline, md};
use leptos::prelude::*;
use serde_json::Value;

thread_local! {
    static SAMPLES: ArcRwSignal<Value> = {
        let s = ArcRwSignal::new(Value::Null);
        #[cfg(target_arch = "wasm32")]
        {
            let s = s.clone();
            leptos::task::spawn_local(async move {
                // Retry with a growing pause: a slow or failed first call must not leave the front page empty.
                for secs in [0u32, 1, 2, 4, 8, 15] {
                    if secs > 0 { pause(secs * 1000).await; }
                    if let Ok(v) = crate::common::api("GET", "/api/previews", None).await { s.set(v); break }
                }
            });
        }
        s
    };
}

#[cfg(target_arch = "wasm32")]
async fn pause(ms: u32) {
    let p = js_sys::Promise::new(&mut |ok, _| {
        if let Some(w) = web_sys::window() { let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&ok, ms as i32); }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}

/// One of the bot's sample messages; null until they have loaded or before the bot first started.
pub fn sample(key: &str) -> Value {
    SAMPLES.with(|s| s.with(|v| v[key].clone()))
}

/// The name behind a sample id: `kind` is users, channels or roles.
pub fn name(kind: &str, id: &str) -> Option<String> {
    SAMPLES.with(|s| s.with_untracked(|v| v["names"][kind][id].as_str().map(String::from)))
}

/// A `<t:unix:style>` timestamp as Discord shows it; 0 is now.
#[cfg(target_arch = "wasm32")]
pub fn time(secs: i64, style: &str) -> String {
    let d = if secs == 0 {
        js_sys::Date::new_0()
    } else {
        js_sys::Date::new(&(secs as f64 * 1000.0).into())
    };
    let o = js_sys::Object::new();
    let set = |k: &str, v: &str| {
        let _ = js_sys::Reflect::set(&o, &k.into(), &v.into());
    };
    if style == "R" {
        return relative(secs - (js_sys::Date::now() / 1000.0) as i64);
    }
    match style {
        "t" | "T" => {}
        "d" => {
            set("year", "numeric");
            set("month", "2-digit");
            set("day", "2-digit");
        }
        "F" => {
            set("weekday", "long");
            set("year", "numeric");
            set("month", "long");
            set("day", "numeric");
        }
        _ => {
            set("year", "numeric");
            set("month", "long");
            set("day", "numeric");
        }
    }
    if !matches!(style, "d" | "D") {
        set("hour", "numeric");
        set("minute", "2-digit");
    }
    if style == "T" {
        set("second", "2-digit");
    }
    String::from(d.to_locale_string("en-US", &o)).replace(" at ", " ")
}

/// Discord's relative time ("in 2 hours", "3 days ago") for `d` seconds from now.
pub fn relative(d: i64) -> String {
    let a = d.unsigned_abs() as f64;
    let n = |unit: f64| (a / unit).round() as u64;
    let t = match a {
        a if a < 45.0 => "a few seconds".to_string(),
        a if a < 90.0 => "a minute".into(),
        a if a < 45.0 * 60.0 => format!("{} minutes", n(60.0)),
        a if a < 90.0 * 60.0 => "an hour".into(),
        a if a < 22.0 * 3600.0 => format!("{} hours", n(3600.0)),
        a if a < 36.0 * 3600.0 => "a day".into(),
        a if a < 26.0 * 86_400.0 => format!("{} days", n(86_400.0)),
        a if a < 45.0 * 86_400.0 => "a month".into(),
        a if a < 320.0 * 86_400.0 => format!("{} months", n(30.0 * 86_400.0).max(2)),
        a if a < 548.0 * 86_400.0 => "a year".into(),
        _ => format!("{} years", n(365.0 * 86_400.0).max(2)),
    };
    if d >= 0 {
        format!("in {t}")
    } else {
        format!("{t} ago")
    }
}

/// Native unit tests have no browser clock.
#[cfg(not(target_arch = "wasm32"))]
pub fn time(secs: i64, style: &str) -> String {
    format!("{secs}:{style}")
}

/// "Today at 9:11 PM", the time beside a message's author.
fn now() -> String {
    format!("Today at {}", time(0, "t"))
}

/// Seconds since 1970 for an embed `timestamp`: "now", "+seconds" after posting, or a date.
#[cfg(target_arch = "wasm32")]
pub fn stamp_secs(t: &str) -> Option<i64> {
    let now = js_sys::Date::now() / 1000.0;
    let s = match t {
        "now" => now,
        _ => match t.strip_prefix('+') {
            Some(n) => now + n.parse::<f64>().ok()?,
            None => js_sys::Date::parse(t) / 1000.0,
        },
    };
    s.is_finite().then_some(s as i64)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn stamp_secs(t: &str) -> Option<i64> {
    t.strip_prefix('+').and_then(|n| n.parse().ok())
}

/// The footer time as Discord shows it: "Today at 18:00", else the date and the time.
pub fn stamp(t: &str) -> String {
    match stamp_secs(t) {
        Some(s) if time(s, "d") == time(0, "d") => format!("Today at {}", time(s, "t")),
        Some(s) => format!("{} {}", time(s, "d"), time(s, "t")),
        None => String::new(),
    }
}

/// Discord's field rows: inline fields share a row, up to 3 (2 next to a thumbnail); any other field takes a row.
pub fn field_rows(inline: &[bool], thumb: bool) -> Vec<Vec<usize>> {
    let max = if thumb { 2 } else { 3 };
    let (mut rows, mut cur) = (Vec::new(), Vec::new());
    for (i, &inl) in inline.iter().enumerate() {
        if !inl && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
        }
        cur.push(i);
        if !inl || cur.len() == max {
            rows.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// A member's message: grey avatar, name, text.
pub fn chat(author: &str, body: AnyView) -> AnyView {
    let first = author
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    view! { <div class="dmsg"><i class="user">{first}</i><div><b>{author.to_string()}</b>" "<small class="muted">{now()}</small><div class="blk">{body}</div></div></div> }.into_any()
}

fn button(b: &Value) -> AnyView {
    let style = match b["style"].as_u64() {
        Some(1) => "primary",
        Some(3) => "success",
        Some(4) => "danger",
        _ => "secondary",
    };
    let link = b["style"] == 5;
    let emoji = emoji(&b["emoji"]);
    // `data-url`: the Message Builder's preview opens it after asking, as Discord does.
    let url = b["url"].as_str().map(String::from);
    let id = b["custom_id"].as_str().map(String::from);
    view! { <span class=format!("dbtn blk {style}") data-url=url data-id=id><span inner_html=emoji />{b["label"].as_str().unwrap_or("").to_string()}{link.then_some(" ↗")}</span> }.into_any()
}

/// A button's or menu option's emoji as HTML: the image of a custom one, else the character.
fn emoji(e: &Value) -> Option<String> {
    let name = e["name"].as_str()?;
    Some(match e["id"].as_str() {
        Some(id) if id.bytes().all(|b| b.is_ascii_digit()) => format!(
            "<img class=\"emo\" alt=\":{}:\" src=\"{}\"> ",
            esc(name),
            crate::msgparts::emoji_url(id, e["animated"] == true)
        ),
        _ => crate::icons::fill_tags(&format!("{} ", esc(name))),
    })
}

/// A select menu; the Message Builder's preview opens it (`.open`) and answers a pick (`data-opt`).
fn menu(r: usize, m: &Value) -> AnyView {
    let ph = m["placeholder"]
        .as_str()
        .filter(|p| !p.is_empty())
        .unwrap_or("Make a selection")
        .to_string();
    let opts = m["options"].as_array().cloned().unwrap_or_default();
    view! {
        <div class="dmenu blk"><div class="dsel">{ph}<span class="caret">"⌄"</span></div>
            <div class="dopts">{opts.into_iter().enumerate().map(|(o, x)| view! {
                <div class="dopt" data-opt=format!("{r}:{o}")><span inner_html=emoji(&x["emoji"]) />
                    <div><b>{x["label"].as_str().unwrap_or("").to_string()}</b>
                    {x["description"].as_str().filter(|d| !d.is_empty()).map(|d| view! { <small class="muted">{d.to_string()}</small> })}</div></div>
            }).collect_view()}</div>
        </div>
    }.into_any()
}

fn embed(e: &Value) -> AnyView {
    let s = |p: &str| {
        p.split('.')
            .fold(e, |v, k| &v[k])
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    // Only https images load: another address would learn the viewer's IP (as the editor does).
    // A png data address is the bot's own captcha sample; it makes no request.
    let img = |p: &str| {
        Some(s(p))
            .filter(|u| u.starts_with("https://") || u.starts_with("data:image/png;base64,"))
            .unwrap_or_default()
    };
    // Discord's bar for an embed without a colour.
    let color = e["color"]
        .as_u64()
        .map_or("#1e1f22".into(), |c| format!("#{c:06x}"));
    let image = img("image.url");
    let footer = [s("footer.text"), stamp(&s("timestamp"))]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" • ");
    view! {
        // The thumbnail sits in the top right corner, as in Discord (the same layout as the editor).
        <div class="embed" class:th=!img("thumbnail.url").is_empty() style=format!("border-left-color:{color}")>
            {(!img("thumbnail.url").is_empty()).then(|| view! { <span class="thumbslot"><img class="thumbimg" src=img("thumbnail.url") alt="" /></span> })}
            {(!s("author.name").is_empty()).then(|| view! { <small class="author blk">
                {(!img("author.icon_url").is_empty()).then(|| view! { <img class="ico" src=img("author.icon_url") alt="" /> })}
                <b class:link=!s("author.url").is_empty() data-url=Some(s("author.url")).filter(|u| !u.is_empty())>{s("author.name")}</b></small> })}
            {(!s("title").is_empty()).then(|| view! { <b class="blk" class:link=!s("url").is_empty() data-url=Some(s("url")).filter(|u| !u.is_empty()) inner_html=crate::icons::fill_tags(&inline(&esc(&s("title")))) /> })}
            {(!s("description").is_empty()).then(|| view! { <div class="md" inner_html=md(&s("description")) /> })}
            {e["fields"].as_array().filter(|f| !f.is_empty()).map(|fs| {
                let inl: Vec<bool> = fs.iter().map(|f| f["inline"] == true).collect();
                view! { <div class="fields">{field_rows(&inl, !img("thumbnail.url").is_empty()).into_iter().flat_map(|r| {
                    let span = 12 / r.len();
                    r.into_iter().map(move |i| (i, span))
                }).map(|(i, span)| { let f = &fs[i]; view! {
                    <div class="blk" style=format!("grid-column:span {span}")><b inner_html=crate::icons::fill_tags(&inline(&esc(f["name"].as_str().unwrap_or("")))) /><div class="md" inner_html=md(f["value"].as_str().unwrap_or("")) /></div> } }).collect_view()}</div> }
            })}
            {(!image.is_empty()).then(|| view! { <img src=image alt="" /> })}
            {(!footer.is_empty()).then(|| view! { <small class="muted author blk">
                {(!img("footer.icon_url").is_empty()).then(|| view! { <img class="ico" src=img("footer.icon_url") alt="" /> })}{footer}</small> })}
        </div>
    }.into_any()
}

/// A message the bot sends: content, embeds, button rows; flag 64 marks it "only you can see this".
pub fn bot(m: &Value) -> AnyView {
    let content = m["content"].as_str().unwrap_or("");
    view! {
        <div class="dmsg">
            <img class="avatar" src="/logo.svg" alt="" />
            <div>
                <b>"Brim "</b><span class="chip">"APP"</span>" "<small class="muted">{now()}</small>
                {(!content.is_empty()).then(|| view! { <div class="md" inner_html=md(content) /> })}
                {m["embeds"].as_array().into_iter().flatten().map(embed).collect_view()}
                {m["components"].as_array().into_iter().flatten().enumerate().map(|(i, r)| view! { <div class="drow">{r["components"].as_array().into_iter().flatten().map(|c| if c["type"] == 3 { menu(i, c) } else { button(c) }).collect_view()}</div> }).collect_view()}
                {(m["flags"] == 64).then(|| view! { <small class="muted eph">"Only you can see this • "<span class="dismiss" data-dismiss="">"Dismiss message"</span></small> })}
            </div>
        </div>
    }.into_any()
}

/// The verification panel as a member sees it, and pressable: Verify and Help show what Discord
/// answers (only to the member) under the panel, as the bot sends it for `mode`. `captcha` is the
/// picture in the challenge when the page draws its own (else the bot's sample).
#[component]
pub fn VerifyPreview(#[prop(into)] mode: Signal<String>, #[prop(into, optional)] captcha: Signal<Option<String>>) -> impl IntoView {
    let pressed = RwSignal::new(None::<&'static str>);
    let click = move |e: leptos::ev::MouseEvent| {
        use wasm_bindgen::JsCast;
        let id = e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).and_then(|t| t.closest("[data-id]").ok().flatten()).and_then(|b| b.get_attribute("data-id"));
        match id.as_deref() {
            Some("verify:start") => pressed.set(Some("verify")),
            Some("verify:help") => pressed.set(Some("help")),
            _ => {}
        }
    };
    let reply = move || {
        let key = pressed.get()?;
        let mode = mode.get();
        let mut challenge = sample("verify_captcha");
        if let (Some(png), false) = (captcha.get(), challenge.is_null()) {
            challenge["embeds"][0]["image"]["url"] = Value::from(format!("data:image/png;base64,{png}"));
        }
        let note = |t: &'static str| view! { <p class="muted">{t}</p> };
        Some(if key == "help" {
            show("verify_help")
        } else {
            match mode.as_str() {
                "Web" => show("verify_web"),
                "Dm" => {
                    let mut dm = challenge;
                    dm["flags"] = Value::Null;
                    view! { {show("verify_dm")}{note("In DMs:")}{bot(&dm)} }.into_any()
                }
                "Button" => show("verify_button"),
                "SimpleCaptcha" => show("verify_simple"),
                _ => show_msg(challenge),
            }
        })
    };
    move || {
        if sample("verify_panel").is_null() {
            // The bot has not published its samples yet: just the picture, as before.
            return match captcha.get() {
                Some(png) => view! { <div class="dmsg"><img src=format!("data:image/png;base64,{png}") alt="captcha preview" /></div> }.into_any(),
                None => show("verify_panel"),
            };
        }
        view! {
            <div class="vprev" on:click=click>
                {show("verify_panel")}
                <p class="muted">{move || if pressed.get().is_some() { "Press Verify or Help again to see the other answer." } else { "Press Verify or Help, as in Discord." }}</p>
                {reply}
            </div>
        }.into_any()
    }
}

/// A sample by key, as the bot sends it.
pub fn show(key: &str) -> AnyView {
    show_msg(sample(key))
}

/// A sample message, or a note while the samples have not loaded.
pub fn show_msg(m: Value) -> AnyView {
    match m {
        Value::Null => {
            view! { <p class="muted">"The preview appears once Brim has started."</p> }
                .into_any()
        }
        m => bot(&m),
    }
}

/// Marks the preview's lines whose text is new since the last call, so a settings change shows
/// where it landed; `seen` holds the last texts and starts empty, so the first render stays still.
pub fn flash_changes(root: &web_sys::Element, seen: &mut Option<Vec<String>>) {
    let Ok(list) = root.query_selector_all(".blk, .md > div, .md > pre, p, .chip") else {
        return;
    };
    let els: Vec<web_sys::Element> = (0..list.length())
        .filter_map(|i| wasm_bindgen::JsCast::dyn_into(list.item(i)?).ok())
        .collect();
    let texts: Vec<String> = els
        .iter()
        .map(|e| e.text_content().unwrap_or_default())
        .collect();
    if let Some(old) = seen.as_ref() {
        for (e, t) in els.iter().zip(&texts) {
            if !old.contains(t) {
                e.set_class_name(&format!("{} chg", e.class_name()));
            }
        }
    }
    *seen = Some(texts);
}

#[cfg(test)]
mod tests {
    use super::{field_rows, relative};

    #[test]
    fn relative_times_read_as_in_discord() {
        assert_eq!(relative(10), "in a few seconds");
        assert_eq!(relative(7200), "in 2 hours");
        assert_eq!(relative(-3 * 86_400), "3 days ago");
        assert_eq!(relative(60), "in a minute");
        assert_eq!(relative(400 * 86_400), "in a year");
    }

    #[test]
    fn fields_sit_in_rows_as_in_discord() {
        let t = true;
        assert_eq!(
            field_rows(&[t, t, t, t], false),
            vec![vec![0, 1, 2], vec![3]]
        );
        assert_eq!(field_rows(&[t, t, t], true), vec![vec![0, 1], vec![2]]);
        assert_eq!(
            field_rows(&[t, false, t, t], false),
            vec![vec![0], vec![1], vec![2, 3]]
        );
        assert_eq!(field_rows(&[], false), Vec::<Vec<usize>>::new());
    }
}

#[cfg(test)]
#[path = "xss_tests.rs"]
mod xss_tests;

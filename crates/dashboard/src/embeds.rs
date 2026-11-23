//! Message Builder: saved messages (text, up to 10 embeds, up to 5 rows of link buttons) the bot posts as its own
//! message and later edits in place.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    lockdown::audit,
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use brim_core::msgvars;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

type Row = (
    i64,
    String,
    Value,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<Value>,
    String,
    Option<String>,
    i32,
    Value,
);

pub fn routes() -> Router<AppState> {
    let g = "/api/guilds/{guild}/embeds";
    Router::new()
        .route(g, get(list).post(save))
        .route(&format!("{g}/emojis"), get(emojis))
        .route(&format!("{g}/channels"), get(channels).post(new_channel))
        .route(&format!("{g}/{{id}}/post"), post(publish))
        .route(&format!("{g}/{{id}}/sends"), get(sends))
        .route(&format!("{g}/{{id}}/sends/{{send}}/edit"), post(edit_sent))
        .route(
            &format!("{g}/{{id}}/sends/{{send}}/delete"),
            post(delete_sent),
        )
        .route(&format!("{g}/{{id}}/delete"), post(delete))
}

/// The saved message of a row (text, embeds and rows).
pub(crate) const MSG: &str = "message";

/// The pings a send may use: only when Owner, Extra Owners or Trusted Admins saved the message last,
/// through this code (`saved_at = updated_at`: a save by an older image after a rollback moves
/// `updated_at` only, and so switches the pings off).
/// The message's version as the page last read it (its `updated_at` in microseconds): a schedule
/// save names it, so a Trusted Admin vouches only for the text they saw.
pub(crate) const VERSION: &str = "(extract(epoch from updated_at) * 1000000)::bigint::text";

pub(crate) const PINGS: &str =
    "case when saved_level >= 3 and saved_at = updated_at then pings else 'none' end";

fn level_num(l: Level) -> i16 {
    match l {
        Level::None => 0,
        Level::Viewer => 1,
        Level::Editor => 2,
        Level::Admin => 3,
        Level::Owner => 4,
    }
}

/// Saved messages per server.
pub(crate) const MAX_MESSAGES: i64 = 100;

fn chars(v: &Value) -> usize {
    v.as_str().map_or(0, |s| s.chars().count())
}

/// Text Discord shows: missing or a string; anything else Discord refuses at post time.
fn text(v: Option<&Value>, what: &str) -> Result<usize, String> {
    match v {
        None | Some(Value::Null) => Ok(0),
        Some(Value::String(s)) => Ok(s.chars().count()),
        Some(_) => Err(format!("{what} must be text")),
    }
}

fn embeds(m: &Value) -> &[Value] {
    m["embeds"].as_array().map_or(&[][..], Vec::as_slice)
}

fn rows(m: &Value) -> &[Value] {
    m["components"].as_array().map_or(&[][..], Vec::as_slice)
}

fn buttons(row: &Value) -> &[Value] {
    row["components"].as_array().map_or(&[][..], Vec::as_slice)
}

/// Longest "after sending" footer time, in seconds.
const MAX_AFTER: i64 = 365 * 86_400;

/// An embed's `timestamp`: "now" (when Brim posts it), "+seconds" after posting, or a fixed date.
fn timestamp_ok(t: &str) -> bool {
    t == "now"
        || t.strip_prefix('+')
            .and_then(|n| n.parse::<i64>().ok())
            .is_some_and(|n| (1..=MAX_AFTER).contains(&n))
        || chrono::DateTime::parse_from_rfc3339(t).is_ok()
}

/// The embed keys that hold a link.
const LINKS: [&str; 6] = [
    "/url",
    "/author/url",
    "/author/icon_url",
    "/footer/icon_url",
    "/image/url",
    "/thumbnail/url",
];

/// A link is text; a draft may hold one still being typed, but never another scheme
/// (`javascript:` would run in the dashboard's preview).
fn link_ok(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(u)) => u.split_once(':').is_none_or(|(scheme, _)| {
            let s = scheme.to_ascii_lowercase();
            s == "https"
                || s == "http"
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        }),
        Some(_) => false,
    }
}

/// A component emoji: a standard one (`name`) or a custom one (`id`, `name`, `animated`).
fn emoji_ok(v: Option<&Value>) -> bool {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return true;
    };
    v.as_object().is_some_and(|o| {
        o.keys()
            .all(|k| ["id", "name", "animated"].contains(&k.as_str()))
            && o.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| !n.is_empty() && n.chars().count() <= 64)
            && o.get("id").is_none_or(|i| {
                i.as_str()
                    .is_some_and(|i| i.len() <= 20 && i.bytes().all(|b| b.is_ascii_digit()))
            })
            && o.get("animated").is_none_or(Value::is_boolean)
    })
}

fn keys_ok(o: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match o.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("{k} is not allowed (only {})", allowed.join(", "))),
        None => Ok(()),
    }
}

/// What a ready-made button does (`action`): `rules` gives the verification roles, `ticket:<id>`
/// opens a ticket of that type. The id has the shape of a ticket type id.
fn action_ok(a: &str) -> bool {
    a == "rules"
        || a.strip_prefix("ticket:").is_some_and(|id| {
            (1..=32).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Whether the message has an "Open a ticket" button. Posting a ticket panel takes a Trusted Admin,
/// so saving, sending or editing a copy of such a message does too.
fn opens_tickets(m: &Value) -> bool {
    rows(m).iter().flat_map(buttons).any(|b| {
        b["action"]
            .as_str()
            .is_some_and(|a| a.starts_with("ticket:"))
    })
}

const TICKET_BUTTON_ADMIN: &str = "this message has an Open a ticket button: only the Owner, Extra Owners and Trusted Admins can save it, send it or edit its copies";

/// The `custom_id` Brim answers for a ready-made button. Never read from the saved JSON.
fn action_id(a: &str) -> String {
    match a.strip_prefix("ticket:") {
        Some(id) => format!("tk2:open:{id}"),
        None => brim_logging::embed::VERIFY_ACCEPT.into(),
    }
}

/// Rows of buttons or one select menu (type 3) whose options each carry the private answer
/// Brim gives (`answer`). A button is a link (type 2, style 5) or a ready-made one (`action`,
/// style 1 to 4). Brim makes every `custom_id` when it sends.
fn components_ok(m: &Value) -> Result<(), String> {
    if !m["components"].is_null() && !m["components"].is_array() {
        return Err("components must be a list".into());
    }
    if rows(m).len() > 5 {
        return Err("at most 5 rows of buttons and menus".into());
    }
    for (i, r) in rows(m).iter().enumerate() {
        let n = i + 1;
        if r["type"] != 1
            || !r["components"].is_array()
            || r.as_object().is_some_and(|o| o.len() != 2)
        {
            return Err(format!(
                "row {n} must be {{\"type\": 1, \"components\": [...]}}"
            ));
        }
        let items = buttons(r);
        if items.first().is_some_and(|c| c["type"] == 3) {
            let [menu] = items else {
                return Err(format!("row {n}: a menu fills its row alone"));
            };
            let Some(o) = menu.as_object() else {
                return Err(format!("row {n}: the menu must be an object"));
            };
            keys_ok(o, &["type", "placeholder", "options"]).map_err(|e| format!("row {n}: {e}"))?;
            if text(o.get("placeholder"), "the menu text")? > 150 {
                return Err(format!("row {n}: the menu text is at most 150 characters"));
            }
            let opts = menu["options"].as_array().map_or(&[][..], Vec::as_slice);
            if !(menu["options"].is_null() || menu["options"].is_array()) || opts.len() > 25 {
                return Err(format!("row {n}: a menu has a list of at most 25 options"));
            }
            for op in opts {
                let Some(o) = op.as_object() else {
                    return Err(format!("row {n}: an option must be an object"));
                };
                keys_ok(o, &["label", "description", "emoji", "answer"])
                    .map_err(|e| format!("row {n}: {e}"))?;
                if text(o.get("label"), "an option name")? > 100
                    || text(o.get("description"), "an option description")? > 100
                    || text(o.get("answer"), "an option answer")? > 2000
                {
                    return Err(format!(
                        "row {n}: option names and descriptions are at most 100 characters, answers 2000"
                    ));
                }
                if !emoji_ok(o.get("emoji")) {
                    return Err(format!("row {n}: an option emoji has the wrong shape"));
                }
            }
            continue;
        }
        if !(1..=5).contains(&items.len()) {
            return Err(format!("button row {n} needs 1 to 5 buttons"));
        }
        for b in items {
            let Some(o) = b.as_object() else {
                return Err(format!("button row {n}: a button must be an object"));
            };
            keys_ok(o, &["type", "style", "label", "url", "emoji", "action"])
                .map_err(|e| format!("button row {n}: {e}"))?;
            if let Some(a) = o.get("action") {
                let style = b["style"].as_u64().is_some_and(|s| (1..=4).contains(&s));
                if b["type"] != 2
                    || !style
                    || o.contains_key("url")
                    || !a.as_str().is_some_and(action_ok)
                {
                    return Err(format!(
                        "button row {n}: a ready-made button is type 2 with style 1 to 4, no link, and the action \"rules\" or \"ticket:\" plus a ticket type id"
                    ));
                }
            } else if b["type"] != 2 || b["style"] != 5 {
                return Err(format!(
                    "button row {n}: a button is a link (type 2, style 5) or a ready-made button with an action"
                ));
            }
            if !link_ok(b.get("url")) {
                return Err(format!(
                    "button row {n}: a link must be text that starts with https://"
                ));
            }
            if !emoji_ok(b.get("emoji")) {
                return Err(format!(
                    "button row {n}: a button emoji has the wrong shape"
                ));
            }
            if text(b.get("label"), "a button label")? > 80
                || text(b.get("url"), "a button link")? > 512
            {
                return Err(format!(
                    "button row {n}: labels are at most 80 characters, links 512"
                ));
            }
        }
    }
    Ok(())
}

/// Discord's limits for a whole message: past any of them Discord rejects it, so a save refuses it too.
/// A draft may still be unfinished (see [`missing`]).
/// Where a text in the message holds a C0 control character other than a line break or tab, as a
/// JSON path. The preview uses U+0001 as its own marker, and Discord shows none of them.
fn control_char(v: &Value, at: &str) -> Option<String> {
    match v {
        Value::String(s) => s
            .chars()
            .any(|c| c.is_ascii_control() && !matches!(c, '\n' | '\r' | '\t'))
            .then(|| {
                if at.is_empty() {
                    "the message".into()
                } else {
                    at[1..].to_string()
                }
            }),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, x)| control_char(x, &format!("{at}/{}", i + 1))),
        Value::Object(o) => o
            .iter()
            .find_map(|(k, x)| control_char(x, &format!("{at}/{k}"))),
        _ => None,
    }
}

pub(crate) fn limits(m: &Value) -> Result<(), String> {
    let Some(o) = m.as_object() else {
        return Err("the message must be an object".into());
    };
    if let Some(k) = o
        .keys()
        .find(|k| !["content", "embeds", "components"].contains(&k.as_str()))
    {
        return Err(format!("{k} is not allowed in a saved message"));
    }
    if let Some(at) = control_char(m, "") {
        return Err(format!(
            "{at} holds a control character (only line breaks and tabs are allowed); remove it and save again"
        ));
    }
    if !m["content"].is_null() && !m["content"].is_string() {
        return Err("content must be text".into());
    }
    if chars(&m["content"]) > 2000 {
        return Err("the text is longer than 2000 characters".into());
    }
    if !m["embeds"].is_null() && !m["embeds"].is_array() {
        return Err("embeds must be a list".into());
    }
    if embeds(m).len() > 10 {
        return Err("at most 10 embeds".into());
    }
    let mut total = 0;
    for (i, e) in embeds(m).iter().enumerate() {
        let n = i + 1;
        if !e.is_object() {
            return Err(format!("embed {n} must be an object"));
        }
        // The shapes Discord expects; a wrong one is refused here, not as a 400 at post time.
        for (k, ok) in [
            ("color", e["color"].is_null() || e["color"].is_u64()),
            ("fields", e["fields"].is_null() || e["fields"].is_array()),
            ("author", e["author"].is_null() || e["author"].is_object()),
            ("footer", e["footer"].is_null() || e["footer"].is_object()),
            ("image", e["image"].is_null() || e["image"].is_object()),
            (
                "thumbnail",
                e["thumbnail"].is_null() || e["thumbnail"].is_object(),
            ),
        ] {
            if !ok {
                return Err(format!("embed {n}: {k} has the wrong shape"));
            }
        }
        match &e["timestamp"] {
            Value::Null => {}
            Value::String(t) if timestamp_ok(t) => {}
            _ => {
                return Err(format!(
                    "embed {n}: timestamp must be \"now\", \"+seconds\" (up to a year) or a date like 2026-11-20T18:00:00Z"
                ))
            }
        }
        if let Some(k) = LINKS.iter().find(|k| !link_ok(e.pointer(k))) {
            return Err(format!(
                "embed {n}: {} must be text that starts with https://",
                &k[1..]
            ));
        }
        let fields = e["fields"].as_array().map_or(&[][..], Vec::as_slice);
        if fields.iter().any(|f| !f.is_object()) {
            return Err(format!("embed {n}: every field must be an object"));
        }
        if fields.len() > 25 {
            return Err(format!("embed {n}: at most 25 fields"));
        }
        for (key, max) in [
            ("/title", 256),
            ("/description", 4096),
            ("/author/name", 256),
            ("/footer/text", 2048),
        ] {
            let c = text(e.pointer(key), &format!("embed {n}: {}", &key[1..]))?;
            if c > max {
                return Err(format!(
                    "embed {n}: {} is longer than {max} characters",
                    &key[1..]
                ));
            }
            total += c;
        }
        for f in fields {
            let (a, b) = (
                text(f.get("name"), &format!("embed {n}: a field name"))?,
                text(f.get("value"), &format!("embed {n}: a field value"))?,
            );
            if a > 256 || b > 1024 {
                return Err(format!(
                    "embed {n}: field names are at most 256 characters, values 1024"
                ));
            }
            total += a + b;
        }
    }
    // A variable is counted at its longest filled-in value, so the message still fits once sent.
    let vars = msgvars::used(&serde_json::to_string(embeds(m)).unwrap_or_default()).len();
    if total + vars * msgvars::MAX_VALUE > 6000 {
        return Err(if vars == 0 {
            "the embeds have more than 6000 characters together".into()
        } else {
            format!("the embeds have more than 6000 characters together, counting each of the {vars} variables at its longest ({} characters)", msgvars::MAX_VALUE)
        });
    }
    components_ok(m)?;
    // Links and keys Discord ignores have no limit above; this keeps a saved message small.
    if serde_json::to_vec(m).map_or(0, |b| b.len()) > 64 * 1024 {
        return Err("the message is larger than 64 KB".into());
    }
    Ok(())
}

/// What a saved draft still needs before Discord takes it; `None` when it can be sent.
pub(crate) fn missing(m: &Value) -> Option<String> {
    missing_in(m, false)
}

/// [`missing`] for a trigger answer (`answer`), where there is a member who wrote: member
/// variables are allowed there.
pub(crate) fn missing_in(m: &Value, answer: bool) -> Option<String> {
    let blank = |v: &Value| v.as_str().is_none_or(|s| s.trim().is_empty());
    if m["content"].as_str().is_none_or(|c| c.trim().is_empty()) && embeds(m).is_empty() {
        return Some("The message is empty: add text or an embed.".into());
    }
    for (i, e) in embeds(m).iter().enumerate() {
        let fields = e["fields"].as_array().map_or(&[][..], Vec::as_slice);
        if fields
            .iter()
            .any(|f| blank(&f["name"]) || blank(&f["value"]))
        {
            return Some(format!(
                "Embed {}: every field needs a name and a value.",
                i + 1
            ));
        }
        let text = ["/title", "/description", "/author/name", "/footer/text"]
            .iter()
            .any(|k| e.pointer(k).is_some_and(|v| !blank(v)));
        let img = |k| e.pointer(k).is_some_and(|v| !blank(v));
        if !text && fields.is_empty() && !img("/image/url") && !img("/thumbnail/url") {
            return Some(format!("Embed {} is empty.", i + 1));
        }
        // Discord refuses a footer without text and an author without a name.
        if e["footer"].is_object() && blank(&e["footer"]["text"]) {
            return Some(format!(
                "Embed {}: the footer needs text when it has an icon.",
                i + 1
            ));
        }
        if e["author"].is_object() && blank(&e["author"]["name"]) {
            return Some(format!(
                "Embed {}: the author needs a name when it has an icon or a link.",
                i + 1
            ));
        }
        let bad_link = LINKS.iter().any(|k| {
            e.pointer(k).and_then(Value::as_str).is_some_and(|u| {
                // An image may be a link variable as a whole: `{guild.icon}`.
                let var = k.ends_with("icon_url")
                    || k.starts_with("/image")
                    || k.starts_with("/thumbnail");
                let var = var && msgvars::LINKS.iter().any(|v| u == format!("{{{v}}}"));
                !u.is_empty() && !https(u) && !var
            })
        });
        if bad_link {
            return Some(format!(
                "Embed {}: links and images must start with https://",
                i + 1
            ));
        }
    }
    let (menus, links): (Vec<&Value>, Vec<&Value>) = rows(m)
        .iter()
        .flat_map(buttons)
        .partition(|c| c["type"] == 3);
    if !links.iter().all(|b| {
        !blank(&b["label"]) && (b["action"].is_string() || b["url"].as_str().is_some_and(https))
    }) {
        return Some(
            "Every button needs a label, and a link button a link that starts with https://".into(),
        );
    }
    for menu in menus {
        let opts = menu["options"].as_array().map_or(&[][..], Vec::as_slice);
        if opts.is_empty() {
            return Some("A menu needs at least one option.".into());
        }
        if opts
            .iter()
            .any(|o| blank(&o["label"]) || blank(&o["answer"]))
        {
            return Some("Every menu option needs a name and an answer.".into());
        }
    }
    // A member variable needs a member: only a menu answer has one (the member who picked).
    let mut texts = vec![m["content"].as_str().unwrap_or("")];
    for e in embeds(m) {
        for k in ["/title", "/description", "/author/name", "/footer/text"]
            .iter()
            .chain(LINKS.iter())
        {
            texts.extend(e.pointer(k).and_then(Value::as_str));
        }
        for f in e["fields"].as_array().into_iter().flatten() {
            texts.extend(
                [f["name"].as_str(), f["value"].as_str()]
                    .into_iter()
                    .flatten(),
            );
        }
    }
    for c in rows(m).iter().flat_map(buttons) {
        texts.extend(
            [c["label"].as_str(), c["placeholder"].as_str()]
                .into_iter()
                .flatten(),
        );
        for o in c["options"].as_array().into_iter().flatten() {
            texts.extend(
                [o["label"].as_str(), o["description"].as_str()]
                    .into_iter()
                    .flatten(),
            );
        }
    }
    if let Some(v) = texts
        .iter()
        .flat_map(|t| msgvars::used(t))
        .find(|v| msgvars::MEMBER_ONLY.contains(v))
        .filter(|_| !answer)
    {
        return Some(format!(
            "{{{v}}} only works in a trigger answer or a menu answer, where there is a member who wrote or picked."
        ));
    }
    None
}

fn https(u: &str) -> bool {
    u.strip_prefix("https://")
        .is_some_and(|r| !r.trim().is_empty())
}

/// The footer time Discord gets: "now" and "+seconds" count from `now`, the moment of posting.
fn resolve_timestamp(e: &mut Value, now: chrono::DateTime<chrono::Utc>) {
    let at = match e["timestamp"].as_str() {
        Some("now") => now,
        Some(t) => match t.strip_prefix('+').and_then(|n| n.parse::<i64>().ok()) {
            Some(n) => now + chrono::Duration::seconds(n),
            None => return,
        },
        None => return,
    };
    e["timestamp"] = json!(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
}

/// Who a send may ping, from the author's own text before variables are filled, so a filled-in
/// value can never add a ping. `named`: the users and roles the text mentions; `all`: also
/// @everyone and @here. Anything else pings nobody.
/// Whether [`mentions`] lets the message ping anyone: only a Trusted Admin or higher sends such a
/// message, the same bar as saving it (`trusted_only`).
fn pings_members(allowed: &Value) -> bool {
    ["parse", "users", "roles"]
        .iter()
        .any(|k| allowed[k].as_array().is_some_and(|a| !a.is_empty()))
}

fn mentions(m: &Value, pings: &str) -> Value {
    if pings != "named" && pings != "all" {
        return json!({"parse": []});
    }
    let text = m["content"].as_str().unwrap_or("");
    let (mut users, mut roles) = (Vec::<String>::new(), Vec::<String>::new());
    for part in text.split("<@").skip(1) {
        let (role, rest) = match part.strip_prefix('&') {
            Some(r) => (true, r),
            None => (false, part.strip_prefix('!').unwrap_or(part)),
        };
        let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
        // A real snowflake only: Discord refuses the whole send for a bad id.
        if id.parse::<u64>().map_or(true, |n| n == 0) || !rest[id.len()..].starts_with('>') {
            continue;
        }
        let list = if role { &mut roles } else { &mut users };
        // Discord takes at most 100 of each.
        if !list.contains(&id) && list.len() < 100 {
            list.push(id)
        }
    }
    // Only when the author wrote it: a filled-in name like "@everyone" must not ping everyone.
    let wrote = text.contains("@everyone") || text.contains("@here");
    let parse: Vec<&str> = if pings == "all" && wrote {
        vec!["everyone"]
    } else {
        vec![]
    };
    json!({"parse": parse, "users": users, "roles": roles})
}

/// A DM has no server, so a ready-made button could do nothing there: a DM copy goes without them.
fn server_buttons_out(body: &mut Value) {
    let Some(rows) = body["components"].as_array_mut() else {
        return;
    };
    for r in rows.iter_mut() {
        if let Some(items) = r["components"].as_array_mut() {
            items.retain(|c| c["type"] != 2 || c["custom_id"].is_null());
        }
    }
    rows.retain(|r| !buttons(r).is_empty());
}

/// What Discord gets: variables and times filled in, empty text left out, no pings (the caller sets `allowed_mentions`), and
/// each menu given the `custom_id` the bot answers (`mb:<message>:<copy>:<row>`, `key` being
/// `<message>:<copy>`; option values are indexes).
/// `components` is always sent, so an edit in place also removes buttons taken off.
fn discord_body(m: &Value, key: &str, ctx: &msgvars::Ctx) -> Value {
    let now = chrono::DateTime::from_timestamp(ctx.now, 0).unwrap_or_else(chrono::Utc::now);
    // Filled text is cut to Discord's limit, since a long value must never stop a send.
    // `plain`: a part Discord shows without markdown, where an escaping backslash would show.
    let fill_as = |v: &mut Value, max: usize, plain: bool| {
        if let Some(t) = v.as_str() {
            let f = if plain {
                msgvars::fill_plain(t, ctx)
            } else {
                msgvars::fill(t, ctx)
            };
            *v = json!(f.chars().take(max).collect::<String>());
        }
    };
    let fill = |v: &mut Value, max: usize| fill_as(v, max, false);
    let embeds: Vec<Value> = embeds(m)
        .iter()
        .map(|e| {
            let mut e = e.clone();
            resolve_timestamp(&mut e, now);
            for (k, max, plain) in [
                ("/title", 256, false),
                ("/description", 4096, false),
                ("/author/name", 256, true),
                ("/footer/text", 2048, true),
            ] {
                if let Some(v) = e.pointer_mut(k) {
                    fill_as(v, max, plain)
                }
            }
            // A link is filled only when it is one link variable as a whole (an image slot); a
            // value inside a link would break it. One with no value (no server icon) is left out.
            for k in LINKS {
                let Some(v) = e.pointer_mut(k) else { continue };
                let whole = v
                    .as_str()
                    .and_then(|u| u.strip_prefix('{')?.strip_suffix('}'))
                    .is_some_and(|n| msgvars::LINKS.contains(&n));
                if whole {
                    fill(v, 2048);
                }
                if !v.as_str().is_some_and(|u| u.starts_with("https://")) {
                    let (parent, key) = k.rsplit_once('/').unwrap_or_default();
                    if let Some(o) = e.pointer_mut(parent).and_then(Value::as_object_mut) {
                        o.remove(key);
                    }
                    let gone = !parent.is_empty()
                        && e.pointer(parent)
                            .and_then(Value::as_object)
                            .is_some_and(|o| o.is_empty());
                    if gone {
                        e.as_object_mut().map(|o| o.remove(&parent[1..]));
                    }
                }
            }
            for f in e
                .get_mut("fields")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if let Some(v) = f.get_mut("name") {
                    fill(v, 256)
                }
                if let Some(v) = f.get_mut("value") {
                    fill(v, 1024)
                }
            }
            e
        })
        .collect();
    let components: Vec<Value> = rows(m)
        .iter()
        .enumerate()
        .map(|(r, row)| {
            let items: Vec<Value> = buttons(row)
                .iter()
                .map(|c| {
                    let mut c = c.clone();
                    if c["type"] != 3 {
                        if let Some(v) = c.get_mut("label") {
                            fill_as(v, 80, true)
                        }
                        // The id is ours alone: a stored one is never sent.
                        let action = c.as_object_mut().and_then(|o| {
                            o.remove("custom_id");
                            o.remove("action")
                        });
                        if let Some(a) = action.as_ref().and_then(Value::as_str) {
                            c["custom_id"] = json!(action_id(a));
                        }
                        return c;
                    }
                    let options: Vec<Value> = c["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(i, o)| {
                            let mut d = json!({"label": o["label"], "value": i.to_string()});
                            for k in ["description", "emoji"] {
                                if !o[k].is_null() {
                                    d[k] = o[k].clone();
                                }
                            }
                            d
                        })
                        .collect();
                    let mut d =
                        json!({"type": 3, "custom_id": format!("mb:{key}:{r}"), "options": options});
                    if let Some(p) = c["placeholder"].as_str().filter(|p| !p.is_empty()) {
                        d["placeholder"] = json!(p);
                    }
                    d
                })
                .collect();
            json!({"type": 1, "components": items})
        })
        .collect();
    let mut b =
        json!({"embeds": embeds, "components": components, "allowed_mentions": {"parse": []}});
    if chars(&m["content"]) > 0 {
        let t = msgvars::fill(m["content"].as_str().unwrap_or(""), ctx);
        b["content"] = json!(t.chars().take(2000).collect::<String>());
    }
    b
}

/// The values for the variables a message uses, read from Discord when it is sent. `channel` is
/// the channel it goes to, as Discord returned it.
async fn vars(
    st: &AppState,
    guild: i64,
    channel: &Value,
    msg: &Value,
    answer: Option<&Answer>,
) -> Result<msgvars::Ctx, ApiError> {
    let all = msg.to_string();
    let used = msgvars::used(&all);
    let mut c = msgvars::Ctx::now();
    if let Some(a) = answer {
        member_vars(st, guild, &mut c, a, &used).await?;
    }
    c.raw.insert(
        "channel",
        format!("<#{}>", channel["id"].as_str().unwrap_or("0")),
    );
    for (k, v) in [
        ("channel.name", "name"),
        ("channel.id", "id"),
        ("channel.topic", "topic"),
    ] {
        if let Some(x) = channel[v].as_str() {
            c.text.insert(k, x.into());
        }
    }
    if used.contains(&"channel.category") {
        if let Some(p) = channel["parent_id"].as_str() {
            if let Some(cat) = discord(st, Method::GET, &format!("/channels/{p}"), None).await? {
                c.text.insert(
                    "channel.category",
                    cat["name"].as_str().unwrap_or("").into(),
                );
            }
        }
    }
    if brim_core::msgvars::needs_guild(&used) {
        if let Some(g) = discord(
            st,
            Method::GET,
            &format!("/guilds/{guild}?with_counts=true"),
            None,
        )
        .await?
        {
            c.text
                .insert("guild.name", g["name"].as_str().unwrap_or("").into());
            c.text.insert("guild.id", guild.to_string());
            c.text.insert(
                "guild.member_count",
                g["approximate_member_count"].to_string(),
            );
            c.text.insert(
                "guild.premium_subscription_count",
                g["premium_subscription_count"]
                    .as_u64()
                    .unwrap_or(0)
                    .to_string(),
            );
            c.text.insert(
                "guild.premium_tier",
                match g["premium_tier"].as_u64().unwrap_or(0) {
                    0 => "No level".into(),
                    n => format!("Level {n}"),
                },
            );
            if let Some(o) = g["owner_id"].as_str() {
                c.raw.insert("guild.owner_id", format!("<@{o}>"));
            }
            if let Some(h) = g["icon"].as_str() {
                c.raw.insert(
                    "guild.icon",
                    format!("https://cdn.discordapp.com/icons/{guild}/{h}.png"),
                );
            }
            c.raw.insert(
                "guild.created_at",
                format!("<t:{}:R>", msgvars::snowflake_secs(guild as u64)),
            );
        }
    }
    if used.contains(&"role.member_count") {
        // shortcut: counts the first 10 000 members; page further (or ask the bot's cache) for bigger servers.
        let mut after = 0u64;
        for _ in 0..10 {
            let page = discord(
                st,
                Method::GET,
                &format!("/guilds/{guild}/members?limit=1000&after={after}"),
                None,
            )
            .await?
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
            for m in &page {
                for r in m["roles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r.as_str()?.parse().ok())
                {
                    *c.role_counts.entry(r).or_default() += 1;
                }
            }
            match page
                .last()
                .and_then(|m| m["user"]["id"].as_str()?.parse().ok())
            {
                Some(last) if page.len() == 1000 => after = last,
                _ => break,
            }
        }
    }
    Ok(c)
}

async fn list(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select id, name, {MSG}, channel_id::text, message_channel::text, message_id::text, pings, webhook, {VERSION},
             category_id::text, position,
             jsonb_build_object(
                 'scheduled', exists(select 1 from message_rules r where r.embed_id = embeds.id and r.enabled and r.kind = 'schedule'),
                 'answers', exists(select 1 from message_rules r where r.embed_id = embeds.id and r.enabled and r.kind = 'trigger'),
                 'dms', exists(select 1 from message_rules r where r.embed_id = embeds.id and r.enabled and r.kind = 'trigger' and r.spec->>'answer' = 'dm'),
                 'sticky', exists(select 1 from sticky_msgs k where k.embed_id = embeds.id and k.enabled),
                 'issue', exists(select 1 from message_rules r where r.embed_id = embeds.id and r.issue is not null)
                       or exists(select 1 from sticky_msgs k where k.embed_id = embeds.id and k.issue is not null))
         from embeds where guild_id = $1 order by position, lower(name), id"
    )))
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(
            |(id, name, body, channel, message_channel, message, pings, webhook, version, category, position, uses)| json!({
                "id": id, "name": name, "missing": missing(&body), "missing_answer": missing_in(&body, true), "body": body, "channel": channel,
                "message_channel": message_channel, "message": message, "pings": pings, "webhook": webhook,
                "version": version, "uses": uses, "category": category, "position": position,
            })
        )
        .collect::<Vec<_>>())))
}

const VIEW: u64 = 1 << 10;
const SEND: u64 = 1 << 11;
const EMBED: u64 = 1 << 14;

/// Why Brim cannot post a message in a channel, or None when it can: Discord's permission
/// order (server roles, then the channel's @everyone, role and member overwrites).
fn cannot_post(
    c: &Value,
    guild: &str,
    roles: &[Value],
    mine: &[String],
    me: &str,
) -> Option<&'static str> {
    let bits = |v: &Value| v.as_str().and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
    let perm_of = |id: &str| {
        roles
            .iter()
            .find(|r| r["id"] == id)
            .map_or(0, |r| bits(&r["permissions"]))
    };
    let mut p = perm_of(guild) | mine.iter().fold(0, |a, r| a | perm_of(r));
    if p & (1 << 3) == 0 {
        let ows = c["permission_overwrites"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let ow = |id: &str| {
            ows.iter()
                .find(|o| o["id"] == id)
                .map(|o| (bits(&o["allow"]), bits(&o["deny"])))
        };
        if let Some((a, d)) = ow(guild) {
            p = (p & !d) | a;
        }
        let (a, d) = mine
            .iter()
            .filter_map(|r| ow(r))
            .fold((0, 0), |(a, d), (x, y)| (a | x, d | y));
        p = (p & !d) | a;
        if let Some((a, d)) = ow(me) {
            p = (p & !d) | a;
        }
        if p & VIEW == 0 {
            return Some("Brim cannot see this channel (View Channel).");
        }
        if p & SEND == 0 {
            return Some("Brim cannot send messages here (Send Messages).");
        }
        if p & EMBED == 0 {
            return Some("Brim cannot post embeds here (Embed Links).");
        }
    }
    None
}

/// The Send tab's channel picker: text and announcement channels with their category, in server
/// order, each saying whether Brim can post there and why not.
async fn channels(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let list = |v: Option<Value>| v.and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let chans = list(discord(&st, Method::GET, &format!("/guilds/{guild}/channels"), None).await?);
    let roles = list(discord(&st, Method::GET, &format!("/guilds/{guild}/roles"), None).await?);
    let me = st.discord.client_id.clone();
    // Without Brim's own roles the check would grey channels it can post in: skip it then.
    let mine = st.discord.member_roles(guild, &me).await.ok();
    let g = guild.to_string();
    let mut out: Vec<Value> = chans
        .iter()
        .filter(|c| matches!(c["type"].as_u64(), Some(0 | 4 | 5)))
        .map(|c| {
            let kind = match c["type"].as_u64() {
                Some(4) => "category",
                Some(5) => "announcement",
                _ => "text",
            };
            let why = (kind != "category")
                .then_some(mine.as_ref())
                .flatten()
                .and_then(|mine| cannot_post(c, &g, &roles, mine, &me));
            json!({"id": c["id"], "name": c["name"], "kind": kind, "parent": c["parent_id"],
                   "position": c["position"], "why": why})
        })
        .collect();
    out.sort_by_key(|c| c["position"].as_i64().unwrap_or(0));
    Ok(Json(
        json!({"channels": out, "can_create": api::require(&st, &s, guild, Level::Admin, false).await.is_ok()}),
    ))
}

#[derive(Deserialize)]
struct NewChannel {
    name: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    staff_only: bool,
}

/// "+ New channel" in the picker: Owner, Extra Owners and Trusted Admins, with the 2FA step-up.
async fn new_channel(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<NewChannel>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::BadRequest(
            "a channel name is 1 to 100 characters".into(),
        ));
    }
    let id = crate::channels::create_one(
        st.clone(),
        s.clone(),
        guild,
        name.to_string(),
        b.parent,
        b.staff_only,
    )
    .await?;
    audit(
        &st,
        &s,
        guild,
        format!("made the channel <#{id}> from the Message Builder"),
    )
    .await?;
    Ok(Json(json!({"id": id})))
}

/// The emoji picker's custom emojis: this server's and Brim's own. Never other servers'.
/// This server's usable emojis and Brim's own, each as `{id, name, animated}`.
async fn emoji_lists(st: &AppState, guild: i64) -> Result<(Vec<Value>, Vec<Value>), ApiError> {
    let short = |e: &Value| json!({"id": e["id"], "name": e["name"], "animated": e["animated"].as_bool().unwrap_or(false)});
    let server = discord(st, Method::GET, &format!("/guilds/{guild}/emojis"), None)
        .await?
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    // Brim's own emojis are a nice extra: a failure leaves them out.
    let bot = st
        .discord
        .call(
            Method::GET,
            &format!("/applications/{}/emojis", st.discord.client_id),
            None,
        )
        .await
        .map(|v| v["items"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    Ok((
        server
            .iter()
            .filter(|e| e["available"] != false)
            .map(short)
            .collect(),
        bot.iter().map(short).collect(),
    ))
}

async fn emojis(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let (server, bot) = emoji_lists(&st, guild).await?;
    Ok(Json(json!({"server": server, "bot": bot})))
}

/// The custom emoji ids a message uses: `<:name:id>` / `<a:name:id>` in any text, and the emoji
/// of buttons and menu options.
fn custom_emojis(m: &Value) -> Vec<String> {
    let mut out = vec![];
    let mut texts = vec![];
    collect_strings(m, &mut texts);
    for t in texts {
        for (i, _) in t.match_indices('<') {
            let r = &t[i + 1..];
            let r = r.strip_prefix('a').unwrap_or(r);
            let Some(r) = r.strip_prefix(':') else {
                continue;
            };
            let Some((name, rest)) = r.split_once(':') else {
                continue;
            };
            let Some((id, _)) = rest.split_once('>') else {
                continue;
            };
            if !name.is_empty() && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
                out.push(id.to_string());
            }
        }
    }
    for row in rows(m) {
        for c in buttons(row) {
            let opts = c["options"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            for e in std::iter::once(&c["emoji"]).chain(opts.iter().map(|o| &o["emoji"])) {
                if let Some(id) = e["id"].as_str() {
                    out.push(id.to_string());
                }
            }
        }
    }
    out
}

fn collect_strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

#[derive(Deserialize)]
struct Save {
    id: Option<i64>,
    name: String,
    /// `{"content", "embeds", "components"}`.
    body: Option<Value>,
    /// One embed, as saved before the Message Builder (MCP `embed_save`).
    embed: Option<Value>,
    channel: Option<String>,
    /// "none", "named" or "all"; absent keeps the stored setting (MCP and older pages).
    pings: Option<String>,
    /// Send as: `{"name", "avatar"?}` for a webhook, null for Brim; absent keeps the stored one.
    #[serde(default, deserialize_with = "some_value")]
    webhook: Option<Value>,
}

/// Tells a JSON `null` (send as Brim) apart from a missing key (keep what is stored).
fn some_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// Creates or updates a draft; the Discord message only changes on the next post.
async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Save>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Editor, false).await?;
    if let Some(p) = b.pings.as_deref() {
        if !["none", "named", "all"].contains(&p) {
            return Err(ApiError::BadRequest(format!("unknown pings setting {p}")));
        }
    }
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::BadRequest(
            "name must be 1 to 100 characters".into(),
        ));
    }
    let body = match (b.body, b.embed) {
        (Some(m), _) => m,
        (None, Some(e)) => json!({"embeds": [e]}),
        (None, None) => return Err(ApiError::BadRequest("the message is missing".into())),
    };
    limits(&body).map_err(ApiError::BadRequest)?;
    if opens_tickets(&body) && level < Level::Admin {
        return Err(ApiError::Forbidden(TICKET_BUTTON_ADMIN));
    }
    let given = body;
    let mut body =
        json!({"content": given["content"].as_str().unwrap_or(""), "embeds": embeds(&given)});
    // Only when there are buttons or menus: a message without them has no `components` key.
    if !rows(&given).is_empty() {
        body["components"] = json!(rows(&given));
    }
    let channel = match b
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(c) => Some(
            c.parse::<i64>()
                .map_err(|_| ApiError::BadRequest("bad channel".into()))?,
        ),
        None => None,
    };
    let mut tx = st.db.begin().await?;
    // Read under a row lock, so a ping switched on a moment earlier cannot slip past the check.
    let stored: Option<(String, Option<Value>)> = match b.id {
        Some(id) => {
            sqlx::query_as(
                "select pings, webhook from embeds where id = $1 and guild_id = $2 for update",
            )
            .bind(id)
            .bind(guild)
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    let (stored_pings, stored_hook) = stored.unzip();
    let stored_hook = stored_hook.flatten();
    let pings = b
        .pings
        .clone()
        .or(stored_pings.clone())
        .unwrap_or_else(|| "none".into());
    let webhook = match &b.webhook {
        Some(w) => {
            crate::msghooks::clean(w, crate::msghooks::base(&st)).map_err(ApiError::BadRequest)?
        }
        None => stored_hook.clone().unwrap_or(Value::Null),
    };
    if level < Level::Admin {
        let pings = pings != "none" || stored_pings.as_deref().is_some_and(|p| p != "none");
        let hook = !webhook.is_null() || stored_hook.is_some();
        trusted_only(&mut tx, guild, b.id, pings, hook).await?;
    }
    let q = match b.id {
        Some(id) => sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "update embeds set name = $3, message = $4, channel_id = $5, updated_by = $6, updated_at = now(),
                 pings = $8, saved_level = $9, saved_at = now(), webhook = $10
             where id = $1 and guild_id = $2 returning id, {VERSION}"
        )))
        .bind(id),
        None => sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "insert into embeds (guild_id, name, message, channel_id, updated_by, pings, saved_level, saved_at, webhook, position)
             -- shortcut: two saves at the same moment can pass 100 by one; lock per server if that matters.
             -- After a manual order a new message goes last; in A to Z order (all 0) it stays 0.
             select $2, $3, $4, $5, $6, $8, $9, now(), $10,
                    (select case when max(position) > 0 then max(position) + 1 else 0 end from embeds where guild_id = $2)
             where (select count(*) from embeds where guild_id = $2) < $7
             returning id, {VERSION}"
        )))
        .bind(None::<i64>),
    };
    let id: Option<(i64, String)> = q
        .bind(guild)
        .bind(name)
        .bind(&body)
        .bind(channel)
        .bind(s.user_id)
        .bind(MAX_MESSAGES)
        .bind(&pings)
        .bind(level_num(level))
        .bind((!webhook.is_null()).then_some(&webhook))
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    match (id, b.id) {
        (Some((id, version)), _) => Ok(Json(
            json!({"id": id, "missing": missing(&body), "missing_answer": missing_in(&body, true), "version": version}),
        )),
        (None, Some(_)) => Err(ApiError::NotFound),
        (None, None) => Err(ApiError::BadRequest(format!(
            "this server already has {MAX_MESSAGES} saved messages: delete one first"
        ))),
    }
}

/// Calls Discord as the bot; `None` means 404, the statuses a user can fix become readable errors.
pub(crate) async fn discord(
    st: &AppState,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Option<Value>, ApiError> {
    if method == Method::GET {
        return Ok(st.discord.get_json(path).await?);
    }
    let r = st.discord.send(method, path, body).await?;
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or_default();
    match status {
        200..=299 => Ok(Some(v)),
        404 => Ok(None),
        403 => Err(ApiError::BadRequest(
            "Brim needs View Channel, Send Messages and Embed Links in that channel".into(),
        )),
        400 => Err(ApiError::BadRequest(format!(
            "Discord refused the message: {v}"
        ))),
        _ => Err(anyhow::anyhow!("discord {status}: {v}").into()),
    }
}

#[derive(Deserialize)]
struct Publish {
    /// Ignored: every send posts a new copy (older clients still send it).
    #[serde(default)]
    #[allow(dead_code)]
    repost: bool,
}

/// How many sends of one message the Sent log keeps.
const MAX_SENDS: i64 = 100;

/// The saved message, checked as ready to go out: not missing anything, only emojis of this
/// server or Brim, and `channel` a channel of this server (as Discord returned it).
/// Rights follow the message: one that pings, posts under another name or runs by itself is
/// changed (saved or deleted) only by TA and up. Call it below that level, inside the
/// transaction that holds the row lock.
async fn trusted_only(
    tx: &mut sqlx::PgConnection,
    guild: i64,
    id: Option<i64>,
    pings: bool,
    webhook: bool,
) -> Result<(), ApiError> {
    if pings {
        return Err(ApiError::Forbidden(
            "this message pings members: only the Owner, Extra Owners and Trusted Admins can change it",
        ));
    }
    if webhook {
        return Err(ApiError::Forbidden(
            "this message sends as a webhook: only the Owner, Extra Owners and Trusted Admins can change it",
        ));
    }
    // Plan 4d.1: read in the same transaction, under the row lock a rule save also takes.
    if let Some(id) = id {
        let rules: bool = sqlx::query_scalar(
            "select exists(select 1 from message_rules where embed_id = $1 and guild_id = $2)
                 or exists(select 1 from sticky_msgs where embed_id = $1 and guild_id = $2)
                 or exists(select 1 from embeds where id = $1 and guild_id = $2 and tag is not null)",
        )
        .bind(id)
        .bind(guild)
        .fetch_one(&mut *tx)
        .await?;
        if rules {
            return Err(ApiError::BadRequest(
                "this message runs by itself (schedule, trigger, sticky or tag): only the Owner, Extra Owners and Trusted Admins can change it".into(),
            ));
        }
    }
    Ok(())
}

/// A message checked for sending: its name, body, target channel (and Discord's view of it), the
/// `allowed_mentions` to use, and who it sends as (None: Brim).
pub(crate) struct Ready {
    pub(crate) name: String,
    pub(crate) msg: Value,
    pub(crate) channel: i64,
    pub(crate) ch: Value,
    pub(crate) pings: Value,
    pub(crate) webhook: Option<Value>,
}

pub(crate) async fn ready(
    st: &AppState,
    guild: i64,
    id: i64,
    channel: Option<i64>,
    answer: bool,
) -> Result<Ready, ApiError> {
    let (name, msg, saved, pings, webhook, trusted): (String, Value, Option<i64>, String, Option<Value>, bool) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select name, {MSG}, channel_id, {PINGS}, webhook, saved_level >= 3 and saved_at = updated_at
             from embeds where id = $1 and guild_id = $2"
        )))
        .bind(id)
        .bind(guild)
        .fetch_optional(&st.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(why) = missing_in(&msg, answer) {
        return Err(ApiError::BadRequest(why));
    }
    let channel = channel
        .or(saved)
        .ok_or_else(|| ApiError::BadRequest("Choose a channel first.".into()))?;
    // The bot is in many servers: never let an editor of this one post into another's channel.
    let ch = discord(st, Method::GET, &format!("/channels/{channel}"), None)
        .await?
        .unwrap_or_default();
    if ch["guild_id"].as_str().and_then(|g| g.parse().ok()) != Some(guild) {
        return Err(ApiError::BadRequest(
            "that channel is not in this server".into(),
        ));
    }
    // Only this server's emojis and Brim's own: never one seen in another server.
    let used = custom_emojis(&msg);
    if !used.is_empty() {
        let (server, bot) = emoji_lists(st, guild).await?;
        let ok = |id: &String| {
            server
                .iter()
                .chain(&bot)
                .any(|e| e["id"].as_str() == Some(id))
        };
        if let Some(bad) = used.iter().find(|id| !ok(id)) {
            return Err(ApiError::BadRequest(format!(
                "the emoji with id {bad} is not from this server or Brim: pick it again from the emoji button"
            )));
        }
    }
    // A webhook message an older image saved after a rollback (or a lower level, before the rule)
    // is not sent under that name until a Trusted Admin saves it again.
    if webhook.is_some() && !trusted {
        return Err(ApiError::BadRequest(
            "this message sends as a webhook: an Owner, Extra Owner or Trusted Admin must save it again first".into(),
        ));
    }
    let pings = mentions(&msg, &pings);
    Ok(Ready {
        name,
        msg,
        channel,
        ch,
        pings,
        webhook,
    })
}

/// Send: posts a new copy in the saved channel and adds it to the Sent log.
/// Why automated and lower-level sends wait: a lockdown, panic mode, or an anti-nuke case.
pub(crate) async fn emergency(st: &AppState, guild: i64) -> Result<Option<&'static str>, ApiError> {
    let (lock, panic, nuke): (bool, bool, bool) = sqlx::query_as(
        // shortcut: a case counts as open for 10 minutes after it starts (the bot's case window);
        // incidents keep no last-activity time. Add one if long cases need it.
        "select exists(select 1 from lock_snapshots where guild_id = $1 and state <> 'unlocking'),
                exists(select 1 from scheduled_jobs where guild_id = $1 and kind = 'panic_end'),
                exists(select 1 from nuke_incidents where guild_id = $1 and created_at > now() - interval '10 minutes')",
    )
    .bind(guild)
    .fetch_one(&st.db)
    .await?;
    Ok(if panic {
        Some("panic mode is on")
    } else if lock {
        Some("a lockdown is on")
    } else if nuke {
        Some("an anti-nuke case is open")
    } else {
        None
    })
}

/// During a lockdown, panic mode or an anti-nuke case only Owner, Extra Owners and Trusted Admins send.
async fn emergency_gate(st: &AppState, guild: i64, level: Level) -> Result<(), ApiError> {
    if level >= Level::Admin {
        return Ok(());
    }
    match emergency(st, guild).await? {
        Some(why) => Err(ApiError::BadRequest(format!(
            "{why}: only the Owner, Extra Owners and Trusted Admins can send or edit messages now"
        ))),
        None => Ok(()),
    }
}

/// Log, appeal, ticket and verification channels: Brim's own, never a place for a built message.
/// `up` is the channel's parent and, for a thread, the parent's parent (`parents`), so a thread
/// inside one of these channels, or inside a ticket channel, counts as well.
pub(crate) fn bot_channel(config: &Value, channel: i64, up: &[i64]) -> bool {
    let id = |v: &Value| v.as_str().and_then(|v| v.parse::<i64>().ok());
    let logs = serde_json::from_value::<brim_logging::LogConfig>(config["logging"].clone())
        .unwrap_or_default();
    let ours: Vec<i64> = logs
        .channels()
        .iter()
        .map(|c| c.get() as i64)
        .chain(id(&config["appeals"]["channel"]))
        .chain(id(&config["tickets"]["log_channel"]))
        .chain(id(&config["verification"]["channel"]))
        .collect();
    let tickets = crate::tickets::categories(config);
    ours.contains(&channel)
        || up
            .iter()
            .any(|p| ours.contains(p) || tickets.contains(&p.to_string().as_str()))
}

/// The parent of a channel (Discord's channel object) and, for a thread, the parent's parent.
pub(crate) async fn parents(st: &AppState, ch: &Value) -> Result<Vec<i64>, ApiError> {
    let parent = |c: &Value| c["parent_id"].as_str().and_then(|p| p.parse::<i64>().ok());
    let Some(p) = parent(ch) else {
        return Ok(vec![]);
    };
    // 10, 11 and 12 are the thread types.
    if !matches!(ch["type"].as_u64(), Some(10..=12)) {
        return Ok(vec![p]);
    }
    let up = discord(st, Method::GET, &format!("/channels/{p}"), None).await?;
    Ok([Some(p), up.as_ref().and_then(parent)]
        .into_iter()
        .flatten()
        .collect())
}

async fn publish(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(_): Json<Publish>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Editor, false).await?;
    emergency_gate(&st, guild, level).await?;
    let r = ready(&st, guild, id, None, false).await?;
    let (_, config) = crate::store::current(&st.db, guild).await?;
    if bot_channel(&config, r.channel, &parents(&st, &r.ch).await?) {
        return Err(ApiError::BadRequest(
            "that is a log, ticket, verification or appeal channel: Brim does not post built messages there".into(),
        ));
    }
    if r.webhook.is_some() && level < Level::Admin {
        return Err(ApiError::Forbidden(
            "this message sends as a webhook: only the Owner, Extra Owners and Trusted Admins can send it",
        ));
    }
    if pings_members(&r.pings) && level < Level::Admin {
        return Err(ApiError::Forbidden(
            "this message pings members: only the Owner, Extra Owners and Trusted Admins can send it",
        ));
    }
    if opens_tickets(&r.msg) && level < Level::Admin {
        return Err(ApiError::Forbidden(TICKET_BUTTON_ADMIN));
    }
    let (name, channel) = (r.name.clone(), r.channel);
    let by = Sender {
        user: s.user_id,
        who: format!("Sent by {}", s.username),
        rule: None,
        answer: None,
        sticky: false,
    };
    let (m, warning) = send_copy(&st, guild, id, r, by).await?.ok_or_else(|| {
        ApiError::BadRequest("too many sends this minute: try again in a minute".into())
    })?;
    audit(
        &st,
        &s,
        guild,
        format!("sent message **{name}** in <#{channel}>"),
    )
    .await?;
    Ok(Json(
        json!({"message_channel": channel.to_string(), "message": m.to_string(), "done": "sent", "warning": warning}),
    ))
}

/// Who a copy is sent for: a person (Send), a schedule or a trigger rule (no audit line; its rule
/// id instead).
pub(crate) struct Sender {
    pub user: i64,
    /// The Sent log's "how", e.g. "Sent by alex" or "Sent by the schedule “every day at 09:00”".
    pub who: String,
    pub rule: Option<i64>,
    /// A trigger rule's answer to a member's message.
    pub answer: Option<Answer>,
    /// A sticky's repost: not shown in the Sent log, its own per-minute cap.
    pub sticky: bool,
}

/// What a trigger answer needs besides the message: the member who wrote (as Discord's member
/// object), the word or pattern found, where it was written, and how to answer.
pub(crate) struct Answer {
    pub member: Value,
    pub matched: String,
    pub channel: i64,
    pub message: i64,
    /// Reply to the member's message (in the channel only).
    pub reply: bool,
    /// Answer by DM to the member instead of in the channel.
    pub dm: bool,
    /// Seconds after which Brim deletes the answer.
    pub delete_after: Option<i64>,
}

/// Trigger answers per server per minute (plan 4: their own budget).
pub(crate) const TRIGGER_CAP: i64 = 10;
/// DMs per server per hour (trigger answers and cooldown notes; plan 4, ban risk).
pub(crate) const DM_CAP: i64 = 30;

/// Member variables for a trigger answer, from Discord's member object.
async fn member_vars(
    st: &AppState,
    guild: i64,
    c: &mut msgvars::Ctx,
    a: &Answer,
    used: &[&str],
) -> Result<(), ApiError> {
    let m = &a.member;
    let u = &m["user"];
    let uid: u64 = u["id"].as_str().and_then(|i| i.parse().ok()).unwrap_or(0);
    c.raw.insert("member", format!("<@{uid}>"));
    c.text.insert("member.id", uid.to_string());
    let name = u["username"].as_str().unwrap_or("").to_string();
    c.text.insert(
        "member.display_name",
        m["nick"]
            .as_str()
            .or(u["global_name"].as_str())
            .map_or_else(|| name.clone(), String::from),
    );
    c.text.insert("member.username", name);
    let avatar = match (m["avatar"].as_str(), u["avatar"].as_str()) {
        (Some(h), _) => {
            format!("https://cdn.discordapp.com/guilds/{guild}/users/{uid}/avatars/{h}.png")
        }
        (None, Some(h)) => format!("https://cdn.discordapp.com/avatars/{uid}/{h}.png"),
        (None, None) => format!(
            "https://cdn.discordapp.com/embed/avatars/{}.png",
            (uid >> 22) % 6
        ),
    };
    c.raw.insert("member.avatar", avatar);
    c.raw.insert(
        "member.created_at",
        format!("<t:{}:R>", msgvars::snowflake_secs(uid)),
    );
    if let Some(j) = m["joined_at"]
        .as_str()
        .and_then(|j| chrono::DateTime::parse_from_rfc3339(j).ok())
    {
        c.raw
            .insert("member.joined_at", format!("<t:{}:R>", j.timestamp()));
    }
    c.raw.insert(
        "message.link",
        format!(
            "https://discord.com/channels/{guild}/{}/{}",
            a.channel, a.message
        ),
    );
    c.text.insert("msg_trigger", a.matched.clone());
    if used.contains(&"member.top_role") {
        let roles = discord(st, Method::GET, &format!("/guilds/{guild}/roles"), None)
            .await?
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let pos = |id: &str| {
            roles
                .iter()
                .find(|r| r["id"].as_str() == Some(id))
                .and_then(|r| r["position"].as_i64())
                .unwrap_or(0)
        };
        match m["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .max_by_key(|r| pos(r))
        {
            Some(r) => c.raw.insert("member.top_role", format!("<@&{r}>")),
            None => c.text.insert("member.top_role", "@everyone".into()),
        };
    }
    Ok(())
}

/// Posts one copy of a ready message and logs it in the Sent log: (message id, a warning about
/// pictures Discord could not load). A schedule's copy is refused (`Ok(None)`) past the per-minute
/// cap, counted in the database so a restart cannot reset it.
pub(crate) async fn send_copy(
    st: &AppState,
    guild: i64,
    id: i64,
    r: Ready,
    by: Sender,
) -> Result<Option<(i64, Option<String>)>, ApiError> {
    let Ready {
        msg,
        channel,
        ch,
        pings,
        webhook,
        ..
    } = r;
    let a = by.answer.as_ref();
    let dm_user: Option<i64> = a
        .filter(|a| a.dm)
        .and_then(|a| a.member["user"]["id"].as_str()?.parse().ok());
    // A DM cannot come from a webhook, and a trigger answer never pings (plan 4).
    let webhook = webhook.filter(|_| dm_user.is_none());
    // Nor does a sticky's repost (plan 4: it would ping on every repost).
    let pings = if a.is_some() || by.sticky {
        json!({"parse": []})
    } else {
        pings
    };
    let detail = match &webhook {
        Some(w) => format!(
            "{} as the webhook “{}”",
            by.who,
            w["name"].as_str().unwrap_or("")
        ),
        None => by.who.clone(),
    };
    let (source, cap) = match (a, by.rule) {
        _ if by.sticky => ("sticky", STICKY_CAP),
        (Some(_), _) => ("trigger", TRIGGER_CAP),
        (None, Some(_)) => ("schedule", SCHEDULE_CAP),
        (None, None) => ("manual", 0),
    };
    // A DM goes to the member's DM channel, made (or found) by Discord.
    let target = match dm_user {
        Some(u) => discord(
            st,
            Method::POST,
            "/users/@me/channels",
            Some(json!({"recipient_id": u.to_string()})),
        )
        .await?
        .and_then(|c| c["id"].as_str()?.parse::<i64>().ok())
        .ok_or_else(|| ApiError::BadRequest("Discord did not open a DM with that member".into()))?,
        None => channel,
    };
    // The log row comes first: its id goes into the menus' custom_id. An automated row is the
    // claim on its per-minute cap (and a DM's on the hourly DM cap).
    let mut tx = st.db.begin().await?;
    if source != "manual" {
        sqlx::query("select pg_advisory_xact_lock(hashtext('message_sends'), $1::int)")
            .bind((guild % i64::from(i32::MAX)) as i32)
            .execute(&mut *tx)
            .await?;
    }
    if dm_user.is_some() && !dm_claim(&mut tx, guild).await? {
        return Ok(None);
    }
    let send: Option<i64> = sqlx::query_scalar(
        "insert into message_sends (embed_id, guild_id, channel_id, body, sent_by, detail, source, rule_id, dm_user, delete_at)
         select $1, $2, $3, $4, $5, $6, $7, $8, $10, now() + make_interval(secs => $11)
         where $7 = 'manual' or (select count(*) from message_sends where guild_id = $2 and source = $7
                                 and sent_at > now() - interval '1 minute') < $9
         returning id",
    )
    .bind(id)
    .bind(guild)
    .bind(target)
    .bind(&msg)
    .bind(by.user)
    .bind(&detail)
    .bind(source)
    .bind(by.rule)
    .bind(cap)
    .bind(dm_user)
    .bind(a.and_then(|a| a.delete_after).map(|d| d as f64))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(send) = send else {
        return Ok(None);
    };
    tx.commit().await?;
    let posted = async {
        let mut body = discord_body(
            &msg,
            &format!("{id}:{send}"),
            &vars(st, guild, &ch, &msg, a).await?,
        );
        body["allowed_mentions"] = pings;
        if dm_user.is_some() {
            server_buttons_out(&mut body);
        }
        if let Some(w) = &webhook {
            let (m, hook, v) = crate::msghooks::post(st, guild, channel, w, body).await?;
            sqlx::query("update message_sends set webhook_id = $1 where id = $2")
                .bind(hook)
                .bind(send)
                .execute(&st.db)
                .await?;
            return Ok((m, v));
        }
        if let Some(a) = a.filter(|a| a.reply && !a.dm) {
            body["message_reference"] =
                json!({"message_id": a.message.to_string(), "fail_if_not_exists": true});
        }
        let v = discord(
            st,
            Method::POST,
            &format!("/channels/{target}/messages"),
            Some(body),
        )
        .await?
        .ok_or_else(|| ApiError::BadRequest("that channel no longer exists".into()))?;
        let m = v["id"]
            .as_str()
            .and_then(|m| m.parse::<i64>().ok())
            .ok_or_else(|| ApiError::from(anyhow::anyhow!("no message id")))?;
        Ok((m, v))
    }
    .await;
    let (m, answer) = match posted {
        Ok(m) => m,
        Err(e) => {
            sqlx::query("delete from message_sends where id = $1")
                .bind(send)
                .execute(&st.db)
                .await?;
            return Err(e);
        }
    };
    let mut warning = unloaded(&answer);
    // Discord answers before it has fetched a picture it has not seen yet (width 0), then loads
    // it a moment later: read the copy again before calling it not loaded.
    for wait in [2, 3] {
        if warning.is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        match discord(
            st,
            Method::GET,
            &format!("/channels/{target}/messages/{m}"),
            None,
        )
        .await
        {
            Ok(Some(v)) => warning = unloaded(&v),
            // Cannot read it back (no Read Message History, or deleted): keep the first answer.
            _ => break,
        }
    }
    // The warning stays on the copy's row in the Sent log (a toast closes too soon to act on it).
    sqlx::query("update message_sends set posted_id = $1, warning = $3 where id = $2")
        .bind(m)
        .bind(send)
        .bind(&warning)
        .execute(&st.db)
        .await?;
    // `message_*` and `published`: the newest copy in a channel, for the list's Sent mark and
    // older copies' menus (a DM is not one).
    if dm_user.is_none() && !by.sticky {
        sqlx::query("update embeds set message_channel = $1, message_id = $2, published = $3 where id = $4 and guild_id = $5")
            .bind(channel)
            .bind(m)
            .bind(&msg)
            .bind(id)
            .bind(guild)
            .execute(&st.db)
            .await?;
    }
    sqlx::query(
        "delete from message_sends where embed_id = $1 and guild_id = $3 and delete_at is null and source <> 'sticky' and id not in
         (select id from message_sends where embed_id = $1 and guild_id = $3 and source <> 'sticky' order by sent_at desc, id desc limit $2)",
    )
    .bind(id)
    .bind(MAX_SENDS)
    .bind(guild)
    .execute(&st.db)
    .await?;
    Ok(Some((m, warning)))
}

/// Claims one DM of the server's hourly budget (call under the `message_sends` advisory lock).
pub(crate) async fn dm_claim(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    guild: i64,
) -> Result<bool, ApiError> {
    let n: i64 = sqlx::query_scalar(
        "select count(*) from message_dms where guild_id = $1 and sent_at > now() - interval '1 hour'",
    )
    .bind(guild)
    .fetch_one(&mut **tx)
    .await?;
    if n >= DM_CAP {
        return Ok(false);
    }
    sqlx::query("insert into message_dms (guild_id) values ($1)")
        .bind(guild)
        .execute(&mut **tx)
        .await?;
    Ok(true)
}

/// Scheduled sends per server per minute (plan 4: separate from trigger answers and stickies).
pub(crate) const SCHEDULE_CAP: i64 = 10;
/// Sticky reposts per server per minute (plan 4).
pub(crate) const STICKY_CAP: i64 = 12;

/// Discord fetches a link's picture when the message is sent and answers with its size; 0 x 0
/// means it could not load it (a page instead of a picture, or a site that blocks Discord), and
/// the slot then shows nothing in Discord although a browser shows the picture. Author and footer
/// icons come back without a size, so only the thumbnail and the big image can be checked.
pub(crate) fn unloaded(answer: &Value) -> Option<String> {
    let mut gone = Vec::new();
    let embeds = answer["embeds"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (i, e) in embeds.iter().enumerate() {
        for (k, what) in [("thumbnail", "thumbnail"), ("image", "big image")] {
            if e[k].is_object() && e[k]["width"].as_u64().unwrap_or(0) == 0 {
                gone.push(if embeds.len() > 1 {
                    format!("the {what} of embed {}", i + 1)
                } else {
                    format!("the {what}")
                });
            }
        }
    }
    (!gone.is_empty()).then(|| {
        format!(
            "Discord could not load {}, so it does not show in Discord. Use a direct link to the picture itself (in the browser: right-click the picture > Copy image address), ending in .png, .jpg, .gif or .webp. Some sites block Discord from loading their pictures; upload it to Discord and copy its link instead.",
            gone.join(" and ")
        )
    })
}

/// The Sent log: the newest sends first.
async fn sends(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        i64,
        Option<i64>,
        String,
        chrono::DateTime<chrono::Utc>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<i64>,
        Option<String>,
        Option<i64>,
    )> = sqlx::query_as(
        "select id, channel_id, posted_id, detail, sent_at, edited_at, hook_gone_at, hook_gone_by, warning, dm_user from message_sends
             where embed_id = $1 and guild_id = $2 and source <> 'sticky' order by sent_at desc, id desc",
    )
    .bind(id)
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(sid, c, p, detail, at, edited, gone, by, warning, dm)| json!({
            "id": sid, "channel": c.to_string(), "posted": p.map(|p| p.to_string()), "detail": detail,
            "at": at.timestamp(), "edited": edited.map(|e| e.timestamp()),
            "hook_gone": gone.map(|g| json!({"at": g.timestamp(), "by": by.map(|b| b.to_string())})),
            "warning": warning, "dm_user": dm.map(|d| d.to_string()),
        }))
        .collect::<Vec<_>>())))
}

/// Sent log delete: Brim deletes that copy in Discord, then its row goes. The saved message
/// stays. A copy Brim cannot delete keeps its row, with the reason.
async fn delete_sent(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id, send)): Path<(i64, i64, i64)>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Editor, false).await?;
    emergency_gate(&st, guild, level).await?;
    #[allow(clippy::type_complexity)]
    let (channel, posted, hook, dm, name): (
        i64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        String,
    ) = sqlx::query_as(
        "select s.channel_id, s.posted_id, s.webhook_id, s.dm_user, e.name from message_sends s
         join embeds e on e.id = s.embed_id
         where s.id = $1 and s.embed_id = $2 and s.guild_id = $3 and s.source <> 'sticky'",
    )
    .bind(send)
    .bind(id)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if hook.is_some() && level < Level::Admin {
        return Err(ApiError::Forbidden(
            "a webhook copy can only be deleted by the Owner, Extra Owners and Trusted Admins",
        ));
    }
    let done = match posted {
        None => "It was already deleted in Discord, so only its row in the Sent log went.",
        Some(m) => {
            // A DM copy lives in the member's DM channel, not in the channel of the row.
            let at = match dm {
                Some(u) => discord(
                    &st,
                    Method::POST,
                    "/users/@me/channels",
                    Some(json!({"recipient_id": u.to_string()})),
                )
                .await
                .ok()
                .flatten()
                .and_then(|c| c["id"].as_str()?.parse::<i64>().ok()),
                None => Some(channel),
            };
            let r = match (at, hook) {
                // The member cannot be reached any more: nobody can delete that DM, so only the row goes.
                (None, _) => Ok(()),
                (Some(c), Some(h)) => crate::msghooks::delete(&st, guild, h, c, m).await,
                (Some(c), None) => discord(
                    &st,
                    Method::DELETE,
                    &format!("/channels/{c}/messages/{m}"),
                    None,
                )
                .await
                .map(|_| ()),
            };
            if let Err(e) = r {
                tracing::warn!(?e, send, "sent log: deleting a copy failed");
                return Err(ApiError::BadRequest(if dm.is_some() {
                    "Brim could not delete that DM in Discord. The row stays.".into()
                } else {
                    "Brim could not delete that copy in Discord: it needs View Channel in that channel, and Manage Messages too when the copy's webhook was deleted. The row stays.".into()
                }));
            }
            if dm.is_some() && at.is_none() {
                "Discord no longer opens a DM with that member, so Brim cannot delete it there. Only its row in the Sent log went."
            } else {
                "Copy deleted in Discord and removed from the Sent log."
            }
        }
    };
    sqlx::query("delete from message_sends where id = $1")
        .bind(send)
        .execute(&st.db)
        .await?;
    // A webhook whose last copy this was goes too, as on a message delete.
    if hook.is_some() {
        if let Err(e) = crate::msghooks::forget_unused(&st, guild).await {
            tracing::warn!("message builder: removing unused webhooks failed: {e:?}");
        }
    }
    // It was the newest copy: the list's Sent mark must not point at a deleted message.
    if let Some(m) = posted {
        sqlx::query("update embeds set message_channel = null, message_id = null where id = $1 and guild_id = $2 and message_id = $3")
            .bind(id)
            .bind(guild)
            .bind(m)
            .execute(&st.db)
            .await?;
    }
    audit(
        &st,
        &s,
        guild,
        match dm {
            Some(_) => format!("deleted a DM copy of message **{name}**"),
            None => format!("deleted a sent copy of message **{name}** in <#{channel}>"),
        },
    )
    .await?;
    Ok(Json(json!({"ok": true, "done": done})))
}

/// Edit message: puts the current version of the message into one copy from the Sent log.
async fn edit_sent(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id, send)): Path<(i64, i64, i64)>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Editor, false).await?;
    emergency_gate(&st, guild, level).await?;
    let (channel, posted, hook, gone): (i64, Option<i64>, Option<i64>, bool) = sqlx::query_as(
        "select channel_id, posted_id, webhook_id, hook_gone_at is not null from message_sends where id = $1 and embed_id = $2 and guild_id = $3",
    )
    .bind(send)
    .bind(id)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let posted = posted.ok_or(ApiError::NotFound)?;
    if gone {
        return Err(ApiError::BadRequest(
            "Brim's webhook for that copy was deleted in Discord, so the copy cannot be edited: send a new one and delete the old copy in Discord".into(),
        ));
    }
    if hook.is_some() && level < Level::Admin {
        return Err(ApiError::Forbidden(
            "a webhook copy can only be edited by the Owner, Extra Owners and Trusted Admins",
        ));
    }
    let Ready {
        name,
        msg,
        channel,
        ch,
        pings,
        ..
    } = ready(&st, guild, id, Some(channel), false).await?;
    if pings_members(&pings) && level < Level::Admin {
        return Err(ApiError::Forbidden(
            "this message pings members: only the Owner, Extra Owners and Trusted Admins can edit its copies",
        ));
    }
    if opens_tickets(&msg) && level < Level::Admin {
        return Err(ApiError::Forbidden(TICKET_BUTTON_ADMIN));
    }
    let (_, config) = crate::store::current(&st.db, guild).await?;
    if bot_channel(&config, channel, &parents(&st, &ch).await?) {
        return Err(ApiError::BadRequest(
            "that copy is in a log, ticket, verification or appeal channel now: Brim does not edit messages there".into(),
        ));
    }
    let mut body = discord_body(
        &msg,
        &format!("{id}:{send}"),
        &vars(&st, guild, &ch, &msg, None).await?,
    );
    body["allowed_mentions"] = pings;
    // An edit keeps what it does not name, so empty text clears text posted before.
    if body["content"].is_null() {
        body["content"] = json!("");
    }
    // A copy keeps the author it was posted with: Brim's copy is edited by Brim, a webhook
    // copy through its webhook (its name and picture stay as they were posted).
    let edited = match hook {
        Some(h) => crate::msghooks::edit(&st, guild, h, posted, body).await?,
        None => {
            discord(
                &st,
                Method::PATCH,
                &format!("/channels/{channel}/messages/{posted}"),
                Some(body),
            )
            .await?
        }
    };
    if edited.is_none() {
        // Gone on Discord: the row stays in the log, without Jump and Edit message.
        sqlx::query("update message_sends set posted_id = null where id = $1")
            .bind(send)
            .execute(&st.db)
            .await?;
        return Err(ApiError::BadRequest(
            "that copy was deleted on Discord: send a new one".into(),
        ));
    }
    let warning = edited.as_ref().and_then(unloaded);
    sqlx::query(
        "update message_sends set body = $1, edited_at = now(), warning = $3 where id = $2",
    )
    .bind(&msg)
    .bind(send)
    .bind(&warning)
    .execute(&st.db)
    .await?;
    audit(
        &st,
        &s,
        guild,
        format!("edited a sent copy of message **{name}** in <#{channel}>"),
    )
    .await?;
    Ok(Json(json!({"done": "edited", "warning": warning})))
}

/// Deletes the saved message and every copy Brim posted (already gone on Discord is fine).
async fn delete(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    let level = api::require(&st, &s, guild, Level::Editor, false).await?;
    emergency_gate(&st, guild, level).await?;
    let mut tx = st.db.begin().await?;
    // The same bar as save, read under the row lock a save and a rule save also take.
    let (pings, hook): (String, Option<Value>) = sqlx::query_as(
        "select pings, webhook from embeds where id = $1 and guild_id = $2 for update",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if level < Level::Admin {
        trusted_only(&mut tx, guild, Some(id), pings != "none", hook.is_some()).await?;
    }
    // Every copy in the Sent log goes too (the log rows go with the message).
    let copies: Vec<(i64, i64, Option<i64>)> = sqlx::query_as(
        "select channel_id, posted_id, webhook_id from message_sends
         where embed_id = $1 and guild_id = $2 and posted_id is not null",
    )
    .bind(id)
    .bind(guild)
    .fetch_all(&mut *tx)
    .await?;
    let (name, c, m): (String, Option<i64>, Option<i64>) = sqlx::query_as(
        "delete from embeds where id = $1 and guild_id = $2 returning name, message_channel, message_id",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    // The message is gone first; a copy Brim can no longer reach (no access, already deleted)
    // must not keep the message alive or stop the other copies from going.
    let newest = c
        .zip(m)
        .filter(|&(c, m)| !copies.iter().any(|&(x, y, _)| (x, y) == (c, m)))
        .map(|(c, m)| (c, m, None));
    let mut left = 0;
    for (c, m, hook) in copies.into_iter().chain(newest) {
        let r = match hook {
            Some(h) => crate::msghooks::delete(&st, guild, h, c, m)
                .await
                .map_err(|e| anyhow::anyhow!("{e:?}")),
            None => st
                .discord
                .call(Method::DELETE, &format!("/channels/{c}/messages/{m}"), None)
                .await
                .map(|_| ()),
        };
        if let Err(e) = r {
            if !format!("{e:#}").contains("404") {
                left += 1;
                tracing::warn!("message builder: copy {m} in {c} not deleted: {e:#}");
            }
        }
    }
    // Its webhooks and pictures go too once no other message uses them.
    if let Err(e) = crate::msghooks::forget_unused(&st, guild).await {
        tracing::warn!("message builder: removing unused webhooks failed: {e:?}");
    }
    crate::msghooks::gc(&mut *st.db.acquire().await?, guild).await?;
    audit(&st, &s, guild, format!("deleted message **{name}**")).await?;
    Ok(Json(json!({"ok": true, "left": left})))
}

#[cfg(test)]
mod tests {
    use super::{cannot_post, custom_emojis, discord_body, limits, missing};
    use brim_core::msgvars::Ctx;

    fn body(m: &serde_json::Value) -> serde_json::Value {
        discord_body(
            m,
            "9",
            &Ctx {
                now: 1_000_000,
                ..Default::default()
            },
        )
    }
    use serde_json::json;

    fn one(e: serde_json::Value) -> serde_json::Value {
        json!({"content": "", "embeds": [e]})
    }

    #[test]
    fn limits_refuse_control_characters() {
        let e = limits(&one(json!({"author": {"name": "<:\u{1}1\u{1}:42>"}}))).unwrap_err();
        assert!(
            e.contains("embeds/1/author/name holds a control character"),
            "{e}"
        );
        assert!(limits(&json!({"content": "a\u{7f}b"})).is_err());
        assert!(limits(&json!({"content": "line\r\nnext\tcol"})).is_ok());
    }

    #[test]
    fn limits_refuse_what_discord_refuses() {
        assert!(limits(&one(json!({"title": "hi"}))).is_ok());
        assert!(limits(&json!({"content": "hi"})).is_ok());
        assert!(limits(&json!("x")).is_err());
        assert!(limits(&json!({"content": "a".repeat(2001)})).is_err());
        assert!(limits(&json!({"embeds": vec![json!({"title": "a"}); 11]})).is_err());
        assert!(
            limits(&json!({"embeds": [{}], "allowed_mentions": {"parse": ["everyone"]}})).is_err(),
            "pings are never stored"
        );
        assert!(limits(&one(json!({"title": "a".repeat(257)}))).is_err());
        assert!(limits(&one(json!({"author": {"name": "a".repeat(257)}}))).is_err());
        assert!(limits(&one(
            json!({"fields": vec![json!({"name": "a", "value": "b"}); 26]})
        ))
        .is_err());
        assert!(limits(&one(json!({"image": "https://x/y.png"}))).is_err());
        assert!(limits(&one(json!({"color": "red"}))).is_err());
        // Each part within its own limit, but over 6000 together, also across two embeds.
        let big =
            one(json!({"description": "a".repeat(4096), "footer": {"text": "a".repeat(2000)}}));
        assert!(limits(&big).is_err());
        let two = json!({"embeds": [{"description": "a".repeat(3500)}, {"description": "a".repeat(3500)}]});
        assert!(limits(&two).is_err());
        // An unfinished draft is within the limits; it is only not ready.
        assert!(limits(&one(json!({"fields": [{"name": "a", "value": ""}]}))).is_ok());
        assert!(
            limits(&one(json!({"title": 5}))).is_err(),
            "Discord refuses a number as text"
        );
        assert!(limits(&one(json!({"url": "a".repeat(70_000)}))).is_err());
    }

    #[test]
    fn missing_says_what_a_draft_needs() {
        assert_eq!(missing(&one(json!({"title": "hi"}))), None);
        assert_eq!(missing(&json!({"content": "hi", "embeds": []})), None);
        assert_eq!(
            missing(&one(json!({"image": {"url": "https://x/y.png"}}))),
            None
        );
        assert!(missing(&json!({"content": "", "embeds": []}))
            .unwrap()
            .contains("empty"));
        assert!(missing(&one(json!({})))
            .unwrap()
            .contains("Embed 1 is empty"));
        assert!(missing(&one(
            json!({"title": "a", "fields": [{"name": "a", "value": ""}]})
        ))
        .unwrap()
        .contains("field"));
        let ws = one(json!({"title": "a", "fields": [{"name": "a", "value": "  "}]}));
        assert!(missing(&ws).is_some());
        assert!(missing(&one(json!({"image": {"url": ""}}))).is_some());
        assert!(missing(&json!({"content": "  ", "embeds": []})).is_some());
    }

    #[test]
    fn discord_gets_no_empty_text_and_no_pings() {
        let b = body(&one(json!({"title": "hi"})));
        assert!(b.get("content").is_none());
        assert_eq!(b["allowed_mentions"], json!({"parse": []}));
        assert_eq!(
            body(&json!({"content": "hey", "embeds": []}))["content"],
            "hey"
        );
    }

    #[test]
    fn link_buttons_and_footer_times() {
        let row = |b: serde_json::Value| json!({"content": "hi", "components": [{"type": 1, "components": [b]}]});
        let ok = json!({"type": 2, "style": 5, "label": "Rules", "url": "https://e.com"});
        assert!(limits(&row(ok.clone())).is_ok());
        assert_eq!(missing(&row(ok.clone())), None);
        // A ready-made button names what it does; Brim makes its id when it sends.
        let rules =
            json!({"type": 2, "style": 3, "label": "I accept the rules", "action": "rules"});
        let ticket = json!({"type": 2, "style": 1, "label": "Help", "action": "ticket:help-1"});
        assert!(limits(&row(rules.clone())).is_ok());
        assert!(limits(&row(ticket.clone())).is_ok());
        for bad in [
            json!({"type": 2, "style": 1, "label": "x", "action": "wl:add"}),
            json!({"type": 2, "style": 1, "label": "x", "action": "ticket:"}),
            json!({"type": 2, "style": 1, "label": "x", "action": "ticket:a:close"}),
            json!({"type": 2, "style": 1, "label": "x", "action": "ticket:UP"}),
            json!({"type": 2, "style": 1, "label": "x", "action": 5}),
            json!({"type": 2, "style": 5, "label": "x", "action": "rules"}),
            json!({"type": 2, "style": 1, "label": "x", "action": "rules", "url": "https://e.com"}),
            json!({"type": 2, "style": 1, "label": "x", "action": "rules", "custom_id": "undo:1:unban"}),
        ] {
            assert!(limits(&row(bad.clone())).is_err(), "{bad}");
        }
        assert!(missing(&row(
            json!({"type": 2, "style": 1, "label": "", "action": "rules"})
        ))
        .is_some());
        assert!(missing(&row(rules.clone())).is_none());
        let sent = body(
            &json!({"content": "hi", "components": [{"type": 1, "components": [rules, ticket, ok.clone()]}]}),
        );
        let b = &sent["components"][0]["components"];
        assert_eq!(b[0]["custom_id"], "verify:accept");
        assert_eq!(b[1]["custom_id"], "tk2:open:help-1");
        assert!(b[0]["action"].is_null() && b[2]["custom_id"].is_null());
        assert!(super::opens_tickets(&row(
            json!({"type": 2, "style": 1, "label": "x", "action": "ticket:help"})
        )));
        assert!(!super::opens_tickets(&row(
            json!({"type": 2, "style": 3, "label": "x", "action": "rules"})
        )));
        let stored = body(&row(
            json!({"type": 2, "style": 5, "label": "x", "url": "https://e.com", "custom_id": "undo:1:unban"}),
        ));
        assert!(stored["components"][0]["components"][0]["custom_id"].is_null());
        // A DM copy keeps the link and loses the buttons that need a server.
        let mut dm = sent.clone();
        super::server_buttons_out(&mut dm);
        assert_eq!(dm["components"][0]["components"], json!([ok]));
        let mut only = body(
            &json!({"content": "hi", "components": [{"type": 1, "components": [{"type": 2, "style": 1, "label": "x", "action": "rules"}]}]}),
        );
        super::server_buttons_out(&mut only);
        assert_eq!(only["components"], json!([]));
        // No other custom_id: it would need the bot to answer the click.
        assert!(limits(&row(
            json!({"type": 2, "style": 1, "label": "x", "custom_id": "a"})
        ))
        .is_err());
        assert!(limits(&row(
            json!({"type": 2, "style": 5, "label": "x", "url": "https://e.com", "disabled": true})
        ))
        .is_err());
        assert!(limits(&row(
            json!({"type": 2, "style": 5, "label": "a".repeat(81), "url": "https://e.com"})
        ))
        .is_err());
        assert!(limits(&json!({"components": [{"type": 1, "components": []}]})).is_err());
        assert!(
            limits(&json!({"components": vec![json!({"type": 1, "components": [ok]}); 6]}))
                .is_err()
        );
        // Another scheme is refused even in a draft: the preview would open it.
        assert!(limits(&row(
            json!({"type": 2, "style": 5, "label": "x", "url": "JavaScript:alert(1)"})
        ))
        .is_err());
        assert!(limits(&one(json!({"title": "a", "url": "javascript:alert(1)"}))).is_err());
        assert!(limits(&one(json!({"title": "a", "url": 5}))).is_err());
        assert!(limits(&one(json!({"title": "a", "fields": ["x"]}))).is_err());
        assert!(limits(&one(json!({"title": "a", "url": "https://x/a:b"}))).is_ok());
        assert!(missing(&one(
            json!({"title": "a", "footer": {"icon_url": "https://x/i.png"}})
        ))
        .unwrap()
        .contains("footer"));
        assert!(
            missing(&one(json!({"title": "a", "author": {"url": "https://x"}})))
                .unwrap()
                .contains("author")
        );
        // A button still being typed saves, but is not ready.
        let draft = row(json!({"type": 2, "style": 5, "label": "x", "url": "http://e.com"}));
        assert!(limits(&draft).is_ok());
        assert!(missing(&draft).unwrap().contains("https://"));
        assert!(
            missing(&one(json!({"title": "a", "image": {"url": "ftp://x"}})))
                .unwrap()
                .contains("https://")
        );
        for t in ["now", "+7200", "2026-11-20T18:00:00Z"] {
            assert!(
                limits(&one(json!({"title": "a", "timestamp": t}))).is_ok(),
                "{t}"
            );
        }
        for t in ["+0", "+999999999", "soon", "-5"] {
            assert!(
                limits(&one(json!({"title": "a", "timestamp": t}))).is_err(),
                "{t}"
            );
        }
        let b = body(
            &json!({"embeds": [{"title": "a", "timestamp": "+60"}, {"title": "b", "timestamp": "now"}]}),
        );
        let at = |i: usize| {
            chrono::DateTime::parse_from_rfc3339(b["embeds"][i]["timestamp"].as_str().unwrap())
                .unwrap()
        };
        assert_eq!((at(0) - at(1)).num_seconds(), 60);
        // Buttons taken off go away on an edit in place too.
        assert_eq!(b["components"], json!([]));
    }

    #[test]
    fn pings_only_what_the_text_names() {
        let m = json!({"content": "<@1> <@!2> <@&3> <@1> <@x> <@&4 {member} @everyone"});
        assert_eq!(super::mentions(&m, "none"), json!({"parse": []}));
        assert_eq!(super::mentions(&m, "bogus"), json!({"parse": []}));
        // Send and Edit refuse an Editor exactly when the stored setting is not "none".
        for (p, pings) in [
            ("none", false),
            ("bogus", false),
            ("named", true),
            ("all", true),
        ] {
            assert_eq!(super::pings_members(&super::mentions(&m, p)), pings, "{p}");
        }
        // "named" with nothing named pings nobody, so an Editor may send it.
        let quiet = super::mentions(&json!({"content": "hi"}), "named");
        assert!(!super::pings_members(&quiet));
        assert_eq!(
            super::mentions(&m, "named"),
            json!({"parse": [], "users": ["1", "2"], "roles": ["3"]})
        );
        assert_eq!(super::mentions(&m, "all")["parse"], json!(["everyone"]));
        // @everyone only when the author wrote it, never from a filled-in value; real ids only.
        let v = json!({"content": "Hi {guild.name} <@0> <@99999999999999999999999>"});
        assert_eq!(
            super::mentions(&v, "all"),
            json!({"parse": [], "users": [], "roles": []})
        );
    }

    #[test]
    fn knows_the_bots_own_channels() {
        let config = json!({
            "logging": {"fallback": "11", "routes": {}, "events": {}, "custom": [], "ignored_channels": []},
            "appeals": {"channel": "12"}, "tickets": {"log_channel": "13", "category": "20"},
            "verification": {"channel": "14"},
        });
        for c in [11, 12, 13, 14] {
            assert!(super::bot_channel(&config, c, &[]), "{c}");
        }
        assert!(super::bot_channel(&config, 99, &[20]), "a ticket channel");
        assert!(
            super::bot_channel(&config, 99, &[11]),
            "a thread in a log channel"
        );
        assert!(
            super::bot_channel(&config, 99, &[98, 20]),
            "a thread in a ticket channel"
        );
        assert!(!super::bot_channel(&config, 99, &[21]));
        assert!(!super::bot_channel(&config, 99, &[98, 21]));
        assert!(!super::bot_channel(&json!({}), 11, &[]));
    }

    #[test]
    fn knows_where_the_bot_can_post() {
        let roles = [
            json!({"id": "1", "permissions": (super::VIEW | super::SEND | super::EMBED).to_string()}),
            json!({"id": "2", "permissions": "0"}),
            json!({"id": "3", "permissions": "8"}),
        ];
        let mine = vec!["2".to_string()];
        let ch = |ows: serde_json::Value| json!({"permission_overwrites": ows});
        assert_eq!(cannot_post(&ch(json!([])), "1", &roles, &mine, "9"), None);
        let hidden = ch(json!([{"id": "1", "allow": "0", "deny": super::VIEW.to_string()}]));
        assert!(cannot_post(&hidden, "1", &roles, &mine, "9")
            .unwrap()
            .contains("see"));
        // A role overwrite lets the bot back in; a member overwrite wins over roles.
        let back = ch(
            json!([{"id": "1", "allow": "0", "deny": super::VIEW.to_string()}, {"id": "2", "allow": super::VIEW.to_string(), "deny": "0"}]),
        );
        assert_eq!(cannot_post(&back, "1", &roles, &mine, "9"), None);
        let muted = ch(json!([{"id": "9", "allow": "0", "deny": super::SEND.to_string()}]));
        assert!(cannot_post(&muted, "1", &roles, &mine, "9")
            .unwrap()
            .contains("Send"));
        // A member allow wins over a role deny.
        let mine_back = ch(
            json!([{"id": "2", "allow": "0", "deny": super::VIEW.to_string()}, {"id": "9", "allow": super::VIEW.to_string(), "deny": "0"}]),
        );
        assert_eq!(cannot_post(&mine_back, "1", &roles, &mine, "9"), None);
        // Administrator skips overwrites.
        assert_eq!(
            cannot_post(&hidden, "1", &roles, &["3".to_string()], "9"),
            None
        );
    }

    /// Discord answers 0 x 0 for a picture it could not load (seen live: a site that answers
    /// Discord's fetch with 403); the send then says which slot.
    #[test]
    fn says_which_picture_discord_could_not_load() {
        let ok = json!({"width": 256, "height": 256});
        let bad = json!({"width": 0, "height": 0});
        assert_eq!(
            crate::embeds::unloaded(&json!({"embeds": [{"thumbnail": ok, "image": ok}]})),
            None
        );
        assert_eq!(crate::embeds::unloaded(&json!({"id": "1"})), None);
        let w =
            crate::embeds::unloaded(&json!({"embeds": [{"thumbnail": bad, "image": ok}]})).unwrap();
        assert!(
            w.starts_with("Discord could not load the thumbnail,"),
            "{w}"
        );
        let w =
            crate::embeds::unloaded(&json!({"embeds": [{"title": "a"}, {"image": bad}]})).unwrap();
        assert!(
            w.starts_with("Discord could not load the big image of embed 2,"),
            "{w}"
        );
    }

    #[test]
    fn menus_and_variables() {
        let menu = |o: serde_json::Value| json!({"content": "hi", "components": [{"type": 1, "components": [{"type": 3, "placeholder": "Pick", "options": [o]}]}]});
        let ok = json!({"label": "Rules", "description": "d", "emoji": {"name": "📜"}, "answer": "Read {channel}, {member}"});
        assert!(limits(&menu(ok.clone())).is_ok());
        assert_eq!(
            missing(&menu(ok.clone())),
            None,
            "member variables are fine in an answer"
        );
        assert!(missing(&menu(json!({"label": "x", "answer": ""})))
            .unwrap()
            .contains("answer"));
        assert!(limits(&menu(json!({"label": "x", "answer": "a", "value": "evil"}))).is_err());
        assert!(limits(&menu(
            json!({"label": "x", "answer": "a", "emoji": {"id": "1x", "name": "a"}})
        ))
        .is_err());
        // A menu fills its row alone, and its custom_id is never taken from the saved JSON.
        assert!(limits(&json!({"components": [{"type": 1, "components": [{"type": 3, "options": []}, {"type": 3, "options": []}]}]})).is_err());
        assert!(limits(&json!({"components": [{"type": 1, "components": [{"type": 3, "custom_id": "wl:x", "options": []}]}]})).is_err());
        let b = body(&menu(ok));
        let sel = &b["components"][0]["components"][0];
        assert_eq!(sel["custom_id"], "mb:9:0");
        assert_eq!(
            sel["options"][0],
            json!({"label": "Rules", "value": "0", "description": "d", "emoji": {"name": "📜"}})
        );
        assert!(
            sel["options"][0].get("answer").is_none(),
            "the answer stays with Brim"
        );
        // Member variables need a member: not in a message sent to a channel.
        assert!(missing(&json!({"content": "Hi {member}"}))
            .unwrap()
            .contains("{member}"));
        assert_eq!(missing(&json!({"content": "Hi {guild.name}"})), None);
        assert_eq!(
            missing(&one(
                json!({"title": "a", "thumbnail": {"url": "{guild.icon}"}})
            )),
            None
        );
        assert!(missing(&one(json!({"title": "a", "url": "{guild.icon}"}))).is_some());
        // Times counted from sending, and variables, are filled in everywhere.
        let mut ctx = Ctx {
            now: 100,
            ..Default::default()
        };
        ctx.text.insert("guild.name", "*S*".into());
        let b = discord_body(
            &json!({"content": "{guild.name} <t:+60:R>", "embeds": [{"title": "{guild.name}", "fields": [{"name": "n", "value": "<t:+1:t>"}]}]}),
            "1",
            &ctx,
        );
        assert_eq!(b["content"], "\\*S\\* <t:160:R>");
        assert_eq!(b["embeds"][0]["title"], "\\*S\\*");
        assert_eq!(b["embeds"][0]["fields"][0]["value"], "<t:101:t>");
        // Footers show no markdown: no escaping backslash there. A variable inside a link stays as typed.
        let b = discord_body(
            &json!({"embeds": [{"title": "t", "url": "https://a.b/{guild.name}", "footer": {"text": "{guild.name}"}}]}),
            "1",
            &ctx,
        );
        assert_eq!(b["embeds"][0]["footer"]["text"], "*S*");
        assert_eq!(b["embeds"][0]["url"], "https://a.b/{guild.name}");
        let mut big = one(
            json!({"description": "x".repeat(4000), "fields": [{"name": "n", "value": "x".repeat(900)}, {"name": "n", "value": "x".repeat(900)}], "footer": {"text": "{channel.topic}"}}),
        );
        assert!(limits(&big).is_err(), "a variable counts at its longest");
        big["embeds"][0]["footer"]["text"] = json!("plain text here");
        assert!(limits(&big).is_ok());
        // No server icon: the image slot is left out, never sent as "{guild.icon}".
        let b = discord_body(
            &json!({"embeds": [{"title": "t", "thumbnail": {"url": "{guild.icon}"}, "footer": {"text": "f", "icon_url": "{guild.icon}"}}]}),
            "1",
            &ctx,
        );
        assert_eq!(
            b["embeds"][0],
            json!({"title": "t", "footer": {"text": "f"}})
        );
        let long = discord_body(
            &json!({"embeds": [{"title": "{role:1}".repeat(60)}]}),
            "1",
            &ctx,
        );
        assert_eq!(
            long["embeds"][0]["title"].as_str().unwrap().chars().count(),
            256
        );
        assert_eq!(
            custom_emojis(
                &json!({"content": "<:a:12> <a:b:34> <:c:x> <t:1:R>", "components": [{"type": 1, "components": [{"type": 3, "options": [{"label": "l", "emoji": {"id": "56", "name": "e"}}]}]}]})
            ),
            ["12", "34", "56"]
        );
    }

    /// Save, list and the 100 cap on a scratch Postgres (returns early without DATABASE_URL).
    #[tokio::test]
    async fn saves_drafts_and_one_embed_from_mcp() {
        use super::{list, save, Save};
        use axum::extract::{Path, State};
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_310_000_000).await else {
            return;
        };
        let call =
            |name: &str, body: Option<serde_json::Value>, embed: Option<serde_json::Value>| Save {
                id: None,
                name: name.into(),
                body,
                embed,
                channel: None,
                pings: None,
                webhook: None,
            };
        // A draft with an empty field saves and says what it still needs.
        let d = json!({"embeds": [{"title": "t", "fields": [{"name": "a", "value": ""}]}]});
        let r = save(
            State(st.clone()),
            s.clone(),
            Path(g),
            axum::Json(call("draft", Some(d), None)),
        )
        .await
        .unwrap()
        .0;
        assert!(r["missing"].as_str().unwrap().contains("field"));
        // MCP still sends one embed.
        let _ = save(
            State(st.clone()),
            s.clone(),
            Path(g),
            axum::Json(call("old", None, Some(json!({"title": "x"})))),
        )
        .await
        .unwrap();
        let rows = list(State(st.clone()), s.clone(), Path(g)).await.unwrap().0;
        let by = |n: &str| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == n)
                .unwrap()
                .clone()
        };
        assert!(by("draft")["missing"].is_string());
        assert_eq!(
            by("old")["body"],
            json!({"content": "", "embeds": [{"title": "x"}]})
        );
        assert!(by("old")["missing"].is_null());
        // At 100 saved messages a new one is refused with the reason.
        sqlx::query("insert into embeds (guild_id, name, message, updated_by) select $1, 'f' || n, '{}', 7 from generate_series(1, 98) n")
            .bind(g).execute(&st.db).await.unwrap();
        let e = save(
            State(st.clone()),
            s.clone(),
            Path(g),
            axum::Json(call("one more", Some(json!({"content": "hi"})), None)),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(e, crate::ApiError::BadRequest(ref m) if m.contains("100")),
            "{e:?}"
        );
        sqlx::query("delete from embeds where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
    }
}

//! The History "Diff vs current" box: one table row per change, with the labels of the settings pages.

use serde_json::Value;

/// One changed setting: its label, and its value in the older version and now.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub what: String,
    pub old: String,
    pub new: String,
}

/// "public_channel_id" -> "Public channel".
fn readable(s: &str) -> String {
    let s = s.replace(['/', '_', '.'], " ");
    let s = s.trim();
    let s = s
        .strip_suffix(" ids")
        .or_else(|| s.strip_suffix(" id"))
        .unwrap_or(s);
    let mut c = s.chars();
    c.next()
        .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// The schema field of `section` at `rest` (a dotted path), or the field that holds it, and the part left over.
fn field<'a>(schema: &'a Value, section: &str, rest: &str) -> Option<(&'a Value, Value, String)> {
    let mut best: Option<(&Value, Value, String)> = None;
    for sec in schema.as_array().into_iter().flatten() {
        for f in sec["fields"].as_array().into_iter().flatten() {
            if f["page"] == "wizard"
                || f["section"].as_str().or(sec["name"].as_str()) != Some(section)
            {
                continue;
            }
            let p = f["path"].as_str().unwrap_or_default();
            let left = if p == rest {
                ""
            } else if let Some(l) = rest.strip_prefix(p).and_then(|l| l.strip_prefix('.')) {
                l
            } else {
                continue;
            };
            if best.as_ref().is_none_or(|b| left.len() < b.2.len()) {
                best = Some((f, sec["title"].clone(), left.to_string()));
            }
        }
    }
    best
}

/// The section's title from the schema, else its name made readable.
fn section_title(schema: &Value, section: &str) -> String {
    schema
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["name"] == section)
        .and_then(|s| s["title"].as_str().map(String::from))
        .unwrap_or_else(|| readable(section))
}

/// The id list key for `items` and the word for an id it no longer has.
fn id_kind(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "channel" | "channels" => Some(("channels", "channel")),
        "category" | "categories" => Some(("categories", "category")),
        "role" | "roles" => Some(("roles", "role")),
        _ => None,
    }
}

/// A value as plain words: names for channel and role ids, on/off, "not set".
fn show(v: Option<&Value>, kind: &str, dc: &Value) -> String {
    let v = v.unwrap_or(&Value::Null);
    match v {
        Value::Null => return "not set".into(),
        Value::String(s) if s.is_empty() => return "not set".into(),
        Value::Bool(b) => return if *b { "on" } else { "off" }.into(),
        Value::Array(a) if a.is_empty() => return "none".into(),
        _ => {}
    }
    // Config keeps ids as strings of digits; a number under a "channels" key is a count, not an id.
    let is_id = |x: &Value| {
        x.as_str()
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    };
    let ids = is_id(v) || v.as_array().is_some_and(|a| a.iter().all(is_id));
    if let Some((key, word)) = id_kind(kind).filter(|_| ids) {
        let names = crate::form::items(dc, key);
        let loaded = dc[if key == "roles" { "roles" } else { "channels" }].is_array();
        let one = |x: &Value| {
            let id = match x {
                Value::String(s) => s.clone(),
                o => o.to_string(),
            };
            names.iter().find(|(i, _)| *i == id).map_or_else(
                || {
                    if loaded {
                        format!("deleted {word}")
                    } else {
                        format!("unknown {word}")
                    }
                },
                |(_, n)| n.clone(),
            )
        };
        return match v {
            Value::Array(a) => a.iter().map(one).collect::<Vec<_>>().join(", "),
            x => one(x),
        };
    }
    match v {
        Value::String(s) if kind == "choice" => crate::form::opt_label(s),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => a
            .iter()
            .map(|x| show(Some(x), kind, dc))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Array(a) => a
            .iter()
            .map(|x| show(Some(x), "", dc))
            .collect::<Vec<_>>()
            .join(" | "),
        // A list entry such as a ticket panel: each key with its value, ids as names.
        Value::Object(o) => o
            .iter()
            .map(|(k, x)| format!("{}: {}", readable(k), show(Some(x), key_kind(k), dc)))
            .collect::<Vec<_>>()
            .join("; "),
        o => o.to_string(),
    }
}

/// The kind of the value at `left` below field `f`: the field's own, a channel inside a route pick, else none.
fn kind_at<'a>(f: &'a Value, left: &str) -> &'a str {
    match f["kind"].as_str().unwrap_or_default() {
        k if left.is_empty() => k,
        "route_pick" if !left.contains('.') => "channel",
        _ => "",
    }
}

/// The kind a key name tells, for ids the schema does not show (such as the bot's own `public_channel_id`).
fn key_kind(key: &str) -> &'static str {
    let k = key.rsplit('.').next().unwrap_or(key);
    if k.contains("categor") {
        "category"
    } else if k.contains("channel") {
        "channel"
    } else if k.contains("role") {
        "role"
    } else {
        ""
    }
}

/// The kind of the value at `rest` in `section`: from the schema, else from the key name.
fn kind_of(schema: &Value, section: &str, rest: &str) -> String {
    match field(schema, section, rest) {
        Some((f, _, left)) if !kind_at(f, &left).is_empty() => kind_at(f, &left).to_string(),
        _ => key_kind(rest).to_string(),
    }
}

/// True when a value is an object or holds one: its keys can name ids.
fn has_object(v: &Value) -> bool {
    v.is_object() || v.as_array().is_some_and(|a| a.iter().any(has_object))
}

/// True when a change can hold a channel or role id, so the names list is needed.
pub fn needs_names(diff: &Value, schema: &Value) -> bool {
    diff.as_array().into_iter().flatten().any(|c| {
        let (sec, rest) = split(c["path"].as_str().unwrap_or_default());
        id_kind(&kind_of(schema, sec, &rest)).is_some()
            || has_object(&c["old"])
            || has_object(&c["new"])
    })
}

/// "/tickets/look/color" -> ("tickets", "look.color").
fn split(path: &str) -> (&str, String) {
    let p = path.trim_start_matches('/');
    let (sec, rest) = p.split_once('/').unwrap_or((p, ""));
    (sec, rest.replace('/', "."))
}

/// The server's diff (`[{path, old, new}]`) as plain rows.
pub fn lines(diff: &Value, schema: &Value, dc: &Value) -> Vec<Line> {
    diff.as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let (sec, rest) = split(c["path"].as_str().unwrap_or_default());
            let what = match field(schema, sec, &rest) {
                Some((f, title, left)) => {
                    let label = f["title"].as_str().map_or_else(
                        || readable(f["label"].as_str().unwrap_or(&rest)),
                        String::from,
                    );
                    let label = if left.is_empty() {
                        label
                    } else {
                        format!("{label}, {}", readable(&left))
                    };
                    format!(
                        "{}, {label}",
                        title.as_str().map_or_else(|| readable(sec), String::from)
                    )
                }
                None if rest.is_empty() => section_title(schema, sec),
                None => format!("{}, {}", section_title(schema, sec), readable(&rest)),
            };
            let kind = kind_of(schema, sec, &rest);
            Line {
                what,
                old: show(c.get("old"), &kind, dc),
                new: show(c.get("new"), &kind, dc),
            }
        })
        // Missing and null both read "not set": no line for a change nobody can see.
        .filter(|l| l.old != l.new)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!([{"name": "tickets", "title": "Tickets", "fields": [
            {"path": "enabled", "label": "enabled", "kind": "bool", "title": "Use tickets"},
            {"path": "category_id", "label": "category id", "kind": "category"},
            {"path": "log_channel", "label": "log channel", "kind": "channel", "title": "Ticket log channel"},
            {"path": "public_channel", "label": "public channel", "kind": "bool"},
            {"path": "ping_roles", "label": "ping roles", "kind": "roles"},
            {"path": "mode", "label": "mode", "kind": "choice"},
            {"path": "routes", "label": "routes", "kind": "route_pick", "title": "Routes"},
        ]}])
    }

    #[test]
    fn plain_lines_with_names() {
        let dc = json!({"channels": [{"id": "1", "name": "tickets", "kind": "category"}, {"id": "2", "name": "open-a-ticket", "kind": "text"}], "roles": [{"id": "5", "name": "Staff"}]});
        let d = json!([
            {"path": "/tickets/category_id", "old": null, "new": "1"},
            {"path": "/tickets/enabled", "old": false, "new": true},
            {"path": "/tickets/log_channel", "old": "9", "new": "2"},
            {"path": "/tickets/public_channel_id", "old": null, "new": "2"},
            {"path": "/tickets/public_channel", "old": true, "new": false},
            {"path": "/tickets/panels", "old": [], "new": [{"id": "help", "category": "1", "ping_roles": ["5"]}]},
            {"path": "/tickets/ping_roles", "old": [], "new": ["5", "7"]},
            {"path": "/tickets/mode", "old": "Dm", "new": null},
            {"path": "/automod/heat/max", "old": 3, "new": 4},
            {"path": "/automod/max_channels", "old": 3, "new": 4},
        ]);
        assert!(needs_names(&d, &schema()));
        let l = lines(&d, &schema(), &dc);
        let got: Vec<_> = l
            .iter()
            .map(|l| format!("{}: {} -> {}", l.what, l.old, l.new))
            .collect();
        assert_eq!(
            got,
            [
                "Tickets, Category: not set -> tickets",
                "Tickets, Use tickets: off -> on",
                "Tickets, Ticket log channel: deleted channel -> #open-a-ticket",
                "Tickets, Public channel: not set -> #open-a-ticket",
                "Tickets, Public channel: on -> off",
                "Tickets, Panels: none -> Category: tickets; Id: help; Ping roles: @Staff",
                "Tickets, Ping roles: none -> @Staff, deleted role",
                "Tickets, Mode: DMs -> not set",
                "Automod, Heat max: 3 -> 4",
                "Automod, Max channels: 3 -> 4",
            ]
        );
    }

    #[test]
    fn names_that_did_not_load_are_not_bare_ids() {
        let d = json!([{"path": "/tickets/public_channel_id", "old": null, "new": "2"}]);
        assert_eq!(lines(&d, &schema(), &json!({}))[0].new, "unknown channel");
        let route = json!([{"path": "/tickets/routes/ban", "old": "2", "new": null}]);
        assert!(needs_names(&route, &schema()));
        assert_eq!(
            lines(
                &route,
                &schema(),
                &json!({"channels": [{"id": "2", "name": "bans", "kind": "text"}]})
            )[0]
            .old,
            "#bans"
        );
        let same = json!([{"path": "/tickets/category_id", "new": null}]);
        assert!(lines(&same, &schema(), &json!({})).is_empty());
        assert!(!needs_names(
            &json!([{"path": "/tickets/enabled", "old": false, "new": true}]),
            &schema()
        ));
    }
}

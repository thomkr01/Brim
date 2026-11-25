//! The recommended security setup: one file of values (`ui/src/profile.json`) that the setup
//! wizard fills in for a new server and that the Overview row and its Fix compare a saved server
//! with. The bot's own defaults stay as they are (PITFALLS: an on default that acts on members is
//! never a changed default); a server gets these values only when its owner saves the wizard or
//! presses the Fix.

use crate::{access::Level, ApiError};
use brim_antinuke::security::{Check, Fix, Severity};
use serde_json::{json, Map, Value};
use std::{collections::HashMap, sync::OnceLock};

/// The [`Fix::setting`] of the profile row.
pub(crate) const FIX: &str = "security_profile";
/// The [`Fix::setting`] of the "Backups are off" row.
pub(crate) const BACKUPS_FIX: &str = "backups_on";

/// The values, `section.path` -> value.
pub(crate) fn wanted() -> &'static Map<String, Value> {
    static P: OnceLock<Map<String, Value>> = OnceLock::new();
    P.get_or_init(|| {
        serde_json::from_str(include_str!("../ui/src/profile.json")).expect("profile.json reads")
    })
}

/// A schema field: its default, the older field that held the value in other units, its title.
struct Field {
    default: Value,
    fallback: Option<(String, f64)>,
    title: String,
}

fn fields() -> &'static HashMap<String, Field> {
    static F: OnceLock<HashMap<String, Field>> = OnceLock::new();
    F.get_or_init(|| {
        let mut out = HashMap::new();
        for sec in crate::schema::schema() {
            for fd in sec["fields"].as_array().into_iter().flatten() {
                let (Some(section), Some(path)) = (
                    fd["section"].as_str().or(sec["name"].as_str()),
                    fd["path"].as_str(),
                ) else {
                    continue;
                };
                out.insert(
                    format!("{section}.{path}"),
                    Field {
                        default: fd["default"].clone(),
                        fallback: fd["fallback"]["path"]
                            .as_str()
                            .zip(fd["fallback"]["scale"].as_f64())
                            .map(|(p, s)| (p.to_string(), s)),
                        title: fd["title"].as_str().unwrap_or(path).to_string(),
                    },
                );
            }
        }
        out
    })
}

fn at<'a>(config: &'a Value, key: &str) -> &'a Value {
    key.split('.').fold(config, |v, k| &v[k])
}

/// What the bot uses for `key`: the saved value, else the older field in its units, else the default.
fn current(config: &Value, key: &str) -> Value {
    let saved = at(config, key);
    if !saved.is_null() {
        return saved.clone();
    }
    let f = fields().get(key);
    if let Some((old, scale)) = f.and_then(|f| f.fallback.as_ref()) {
        let parent = key.rsplit_once('.').map_or("", |(p, _)| p);
        if let Some(n) = at(config, &format!("{parent}.{old}")).as_f64() {
            return json!((n * scale) as i64);
        }
    }
    match f.map(|f| &f.default) {
        Some(d) if !d.is_null() => d.clone(),
        // A switch with no saved value and no default is off.
        _ if wanted().get(key).is_some_and(Value::is_boolean) => json!(false),
        _ => Value::Null,
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// One saved value that differs from the recommended one.
pub(crate) struct Change {
    pub key: &'static str,
    pub now: Value,
    pub wanted: &'static Value,
}

/// The differences for one server. Backups have their own row, so they are left out here.
pub(crate) fn changes(config: &Value) -> Vec<Change> {
    wanted()
        .iter()
        .filter(|(k, _)| k.as_str() != "backups.enabled")
        .filter_map(|(k, w)| {
            let now = current(config, k);
            (!same(&now, w)).then_some(Change {
                key: k.as_str(),
                now,
                wanted: w,
            })
        })
        .collect()
}

fn section_of(key: &str) -> &str {
    key.split('.').next().unwrap_or_default()
}

/// A number of seconds as short text.
fn span(secs: f64) -> String {
    let s = secs.round() as i64;
    match s {
        0 => "0 seconds".into(),
        s if s % 86_400 == 0 => format!("{} day(s)", s / 86_400),
        s if s % 3_600 == 0 => format!("{} hour(s)", s / 3_600),
        s if s % 60 == 0 => format!("{} minute(s)", s / 60),
        s => format!("{s} second(s)"),
    }
}

fn show(key: &str, v: &Value) -> String {
    match v {
        Value::Null => "not set".into(),
        Value::Bool(b) => if *b { "on" } else { "off" }.into(),
        Value::Number(n) => match n.as_f64() {
            Some(x) if key.ends_with("_ms") => span(x / 1000.0),
            Some(x) if key.ends_with("_secs") => span(x),
            _ => n.to_string(),
        },
        Value::String(s) if s.is_empty() => "strike system".into(),
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("{} step(s)", a.len()),
        Value::Object(_) => "set".into(),
    }
}

/// One line per change for the confirm: where it is, what it is now, what it becomes.
pub(crate) fn lines(list: &[Change]) -> Vec<String> {
    list.iter()
        .map(|c| {
            let title = fields().get(c.key).map_or(c.key, |f| f.title.as_str());
            let sec = crate::schema::schema()
                .iter()
                .find(|s| s["name"] == section_of(c.key))
                .and_then(|s| s["title"].as_str().map(String::from))
                .unwrap_or_else(|| section_of(c.key).to_string());
            format!(
                "{sec}: {title}: {} to {}",
                show(c.key, &c.now),
                show(c.key, c.wanted)
            )
        })
        .collect()
}

/// The profile row (None when the server already has every value) and the Backups row.
pub(crate) fn rows(config: &Value) -> Vec<Value> {
    let mut out = vec![];
    if config["backups"]["enabled"] != true {
        out.push(json!(Check {
            id: "backups_off".into(),
            severity: Severity::Warn,
            title: "Backups are off",
            detail: "Without a backup, Brim cannot put back the channels and roles an attacker deletes, and Panic mode has nothing to load. Backups stay inside Brim and are taken every 3 hours.".into(),
            fix: Some(Fix {
                label: "Turn on backups",
                role_id: None,
                channel_id: None,
                setting: Some(BACKUPS_FIX),
                move_under_bot: false,
            }),
            steps: vec![],
        }));
    }
    let list = changes(config);
    if !list.is_empty() {
        let mut row = json!(Check {
            id: "security_profile".into(),
            severity: Severity::Warn,
            title: "Security setup differs from the recommended one",
            detail: format!(
                "{} setting(s) differ from the recommended setup. Brim changes only these; everything else stays as you set it. The wizard shows the same values.",
                list.len()
            ),
            fix: Some(Fix {
                label: "Use the recommended setup",
                role_id: None,
                channel_id: None,
                setting: Some(FIX),
                move_under_bot: false,
            }),
            steps: vec![],
        });
        row["changes"] = json!(lines(&list));
        out.push(row);
    }
    out
}

/// Writes every recommended value into `config`. Each changed section needs its own write bar,
/// and a section turned on brings its module on (the bar of the module switch). Returns the
/// changed keys; the caller validates the sections and saves.
pub(crate) fn apply(config: &mut Value, level: Level) -> Result<Vec<&'static str>, ApiError> {
    let list = changes(config);
    let mut sections: Vec<&str> = list.iter().map(|c| section_of(c.key)).collect();
    sections.dedup();
    sections.sort_unstable();
    sections.dedup();
    for s in &sections {
        if level < crate::access::write_bar(s).0 {
            return Err(ApiError::Forbidden(
                "only the owner or an Extra Owner can use the recommended setup",
            ));
        }
    }
    let mut done = vec![];
    for c in &list {
        let mut cur = &mut *config;
        for k in c.key.split('.') {
            if !cur.is_object() {
                *cur = json!({});
            }
            cur = &mut cur[k];
        }
        *cur = c.wanted.clone();
        done.push(c.key);
        if c.key.ends_with(".enabled") && c.wanted == &json!(true) {
            if let Some(m) = brim_core::sections::module_of(section_of(c.key)) {
                if config["modules"][m] == false {
                    if level < crate::access::write_bar("modules").0 {
                        return Err(ApiError::Forbidden(
                            "only owners and trusted admins can turn this on",
                        ));
                    }
                    config["modules"][m] = json!(true);
                }
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recommended_value_names_a_real_setting() {
        for (k, w) in wanted() {
            let f = fields()
                .get(k)
                .unwrap_or_else(|| panic!("{k} is no setting"));
            // A saved value must read back: the type of the default is the type of the value.
            if !f.default.is_null() && !w.is_array() {
                assert_eq!(
                    f.default.is_string(),
                    w.is_string(),
                    "{k} has another type than its default"
                );
            }
            let (sec, path) = k.split_once('.').unwrap();
            let mut doc = json!({});
            let mut cur = &mut doc;
            let parts: Vec<_> = path.split('.').collect();
            for p in &parts[..parts.len() - 1] {
                cur = &mut cur[*p];
            }
            cur[parts[parts.len() - 1]] = w.clone();
            crate::api::validate(sec, &doc, &Value::Null)
                .unwrap_or_else(|e| panic!("{k} = {w} is refused: {e:?}"));
        }
    }

    #[test]
    fn the_profile_row_lists_only_what_differs() {
        let all = apply(&mut json!({}), Level::Owner).unwrap();
        assert!(all.len() > 40);
        let mut c = json!({});
        apply(&mut c, Level::Owner).unwrap();
        c["backups"]["enabled"] = json!(true);
        assert!(changes(&c).is_empty(), "applied once, nothing is left");
        assert!(rows(&c).is_empty());
        // One value changed back shows once, with what it is and what it becomes.
        c["verification"]["time_limit_ms"] = json!(120_000);
        c["joinraid"].as_object_mut().unwrap().remove("count");
        let l = lines(&changes(&c));
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].contains("2 minute(s) to 5 minute(s)"), "{}", l[0]);
        // A value saved in the older unit counts as saved: 7 days is not the 5 days wanted.
        let mut old = json!({"joingate": {"min_age_days": 7}});
        assert_eq!(
            current(&old, "joingate.min_age_override_ms"),
            json!(7 * 86_400_000)
        );
        old["joingate"]["min_age_override_ms"] = json!(432_000_000);
        assert_eq!(
            current(&old, "joingate.min_age_override_ms"),
            json!(432_000_000)
        );
    }

    #[test]
    fn a_missing_switch_is_off_and_a_lower_level_is_refused() {
        assert_eq!(current(&json!({}), "antinuke.enabled"), json!(false));
        assert!(apply(&mut json!({}), Level::Admin).is_err());
        let mut c = json!({"modules": {"server_joins": false}});
        apply(&mut c, Level::Owner).unwrap();
        assert_eq!(c["modules"]["server_joins"], json!(true));
        // A value equal to the bot's default is not written: it is not a difference.
        assert_eq!(c["joingate"]["unverified_bot"]["enabled"], json!(true));
        assert!(c["joingate"]["unverified_bot"]["action"].is_null());
    }

    #[test]
    fn backups_have_their_own_row() {
        let r = rows(&json!({"backups": {"enabled": false}}));
        assert!(r.iter().any(|r| r["id"] == "backups_off"));
        assert!(!changes(&json!({}))
            .iter()
            .any(|c| c.key == "backups.enabled"));
    }
}

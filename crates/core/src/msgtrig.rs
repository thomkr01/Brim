//! Message Builder trigger rules: a member's message in a watched channel that holds a word,
//! phrase or pattern gets the saved message as an answer. The spec, stored as JSON:
//! `{"channels": ["id", ...], "words": [{"text": "hi", "on": true}], "patterns": [{"text": "^a.c$",
//! "on": true}], "ignore_case": true, "whole_words": true, "answer": "channel" | "dm", "reply": true,
//! "cooldown": 30, "delete_after": null | seconds, "dm_on_cooldown": null | seconds,
//! "roles": {"mode": "skip" | "only", "ids": ["id", ...]}}`.
use regex::{RegexBuilder, RegexSet, RegexSetBuilder};
use serde_json::{json, Value};

pub const MAX_WORDS: usize = 50;
pub const MAX_PATTERNS: usize = 10;
pub const MAX_PATTERN_LEN: usize = 200;
pub const MAX_WORD_LEN: usize = 100;
pub const MAX_CHANNELS: usize = 25;
pub const MAX_ROLES: usize = 25;
/// Only the start of a long message is searched.
pub const SCAN_CHARS: usize = 2000;
pub const MIN_COOLDOWN: i64 = 10;
pub const MAX_COOLDOWN: i64 = 86_400;
pub const MIN_DELETE: i64 = 10;
pub const MAX_DELETE: i64 = 86_400;
pub const MIN_DM_COOLDOWN: i64 = 60;
pub const MAX_DM_COOLDOWN: i64 = 86_400;

/// One word, phrase or pattern of a rule.
#[derive(Clone, Debug)]
pub struct Trigger {
    /// As typed: shown in the Sent log and as `{trigger}`.
    pub text: String,
}

/// Compiled size caps: each pattern alone, and one set for all of a rule's words and patterns
/// (matched in one pass).
const SIZE_LIMIT: usize = 1 << 18;
const DFA_LIMIT: usize = 1 << 20;

/// A checked trigger rule, with its matchers compiled once.
#[derive(Clone, Debug)]
pub struct Spec {
    pub channels: Vec<u64>,
    pub triggers: Vec<Trigger>,
    set: RegexSet,
    pub dm: bool,
    pub reply: bool,
    pub cooldown: i64,
    pub delete_after: Option<i64>,
    pub dm_on_cooldown: Option<i64>,
    /// true: only members with one of `roles`; false: skip members with one of them.
    pub only: bool,
    pub roles: Vec<u64>,
    stored: Value,
}

fn ids(v: &Value, max: usize, what: &str) -> Result<Vec<u64>, String> {
    let list = v.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if list.len() > max {
        return Err(format!("at most {max} {what}"));
    }
    let mut out: Vec<u64> = list
        .iter()
        .map(|x| {
            x.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| x.as_u64())
                .ok_or_else(|| format!("a {what} id is not a number"))
        })
        .collect::<Result<_, _>>()?;
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn secs(v: &Value, min: i64, max: i64, what: &str) -> Result<Option<i64>, String> {
    if v.is_null() {
        return Ok(None);
    }
    let n = v
        .as_i64()
        .ok_or_else(|| format!("{what} must be a number of seconds"))?;
    if !(min..=max).contains(&n) {
        return Err(format!(
            "{what} is {min} seconds at the least and 24 hours at the most"
        ));
    }
    Ok(Some(n))
}

/// Checks a spec from the dashboard and compiles it. Turned-off words and patterns are kept in the
/// stored spec but never match.
pub fn parse(v: &Value) -> Result<Spec, String> {
    let channels = ids(&v["channels"], MAX_CHANNELS, "channels")?;
    if channels.is_empty() {
        return Err("choose at least one channel to watch".into());
    }
    let case = v["ignore_case"].as_bool().unwrap_or(true);
    let whole = v["whole_words"].as_bool().unwrap_or(true);
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    let (words, patterns) = (list("words"), list("patterns"));
    if words.len() > MAX_WORDS {
        return Err(format!("a rule has at most {MAX_WORDS} words"));
    }
    if patterns.len() > MAX_PATTERNS {
        return Err(format!("a rule has at most {MAX_PATTERNS} patterns"));
    }
    let build = |p: &str| {
        RegexBuilder::new(p)
            .case_insensitive(case)
            // Typed by staff: cap the compiled size (matching is linear time already).
            .size_limit(SIZE_LIMIT)
            .dfa_size_limit(DFA_LIMIT)
            .build()
    };
    let mut triggers = Vec::new();
    let mut sources = Vec::new();
    let mut clean = |l: &[Value], is_word: bool| -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for w in l {
            let text = w["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                continue;
            }
            let on = w["on"].as_bool().unwrap_or(true);
            let max = if is_word {
                MAX_WORD_LEN
            } else {
                MAX_PATTERN_LEN
            };
            if text.chars().count() > max {
                return Err(format!("“{text}” is longer than {max} characters"));
            }
            let pattern = if is_word {
                let e = regex::escape(&text);
                // A word boundary only where the word starts or ends with a letter or digit.
                let edge = |c: Option<char>| whole && c.is_some_and(|c| c.is_alphanumeric());
                format!(
                    "{}{e}{}",
                    if edge(text.chars().next()) { r"\b" } else { "" },
                    if edge(text.chars().last()) { r"\b" } else { "" }
                )
            } else {
                text.clone()
            };
            if !is_word {
                build(&pattern).map_err(|e| format!("the pattern “{text}” is not valid: {e}"))?;
            }
            if on {
                triggers.push(Trigger { text: text.clone() });
                sources.push(pattern);
            }
            out.push(json!({"text": text, "on": on}));
        }
        Ok(out)
    };
    let words = clean(&words, true)?;
    let patterns = clean(&patterns, false)?;
    if triggers.is_empty() {
        return Err("add a word or pattern that is on".into());
    }
    let set = RegexSetBuilder::new(&sources)
        .case_insensitive(case)
        .size_limit(SIZE_LIMIT * 4)
        .dfa_size_limit(DFA_LIMIT)
        .build()
        .map_err(|_| {
            "this rule's words and patterns are too big together: remove some".to_string()
        })?;
    let dm = match v["answer"].as_str() {
        Some("dm") => true,
        Some("channel") | None => false,
        Some(_) => return Err("answer in the channel or by DM".into()),
    };
    let reply = v["reply"].as_bool().unwrap_or(true);
    let cooldown = secs(&v["cooldown"], MIN_COOLDOWN, MAX_COOLDOWN, "the cooldown")?.unwrap_or(30);
    let delete_after = secs(&v["delete_after"], MIN_DELETE, MAX_DELETE, "delete after")?;
    let dm_on_cooldown = secs(
        &v["dm_on_cooldown"],
        MIN_DM_COOLDOWN,
        MAX_DM_COOLDOWN,
        "the cooldown DM wait",
    )?;
    let only = match v["roles"]["mode"].as_str() {
        Some("only") => true,
        Some("skip") | None => false,
        Some(_) => return Err("roles: skip or only".into()),
    };
    let roles = ids(&v["roles"]["ids"], MAX_ROLES, "roles")?;
    let stored = json!({
        "channels": channels.iter().map(u64::to_string).collect::<Vec<_>>(),
        "words": words, "patterns": patterns, "ignore_case": case, "whole_words": whole,
        "answer": if dm { "dm" } else { "channel" }, "reply": reply, "cooldown": cooldown,
        "delete_after": delete_after, "dm_on_cooldown": dm_on_cooldown,
        "roles": {"mode": if only { "only" } else { "skip" }, "ids": roles.iter().map(u64::to_string).collect::<Vec<_>>()},
    });
    Ok(Spec {
        channels,
        triggers,
        set,
        dm,
        reply,
        cooldown,
        delete_after,
        dm_on_cooldown,
        only,
        roles,
        stored,
    })
}

impl Spec {
    /// The spec as stored: cleaned, with defaults filled in.
    pub fn to_json(&self) -> Value {
        self.stored.clone()
    }

    /// The first trigger found in the start of `text`.
    pub fn find(&self, text: &str) -> Option<&Trigger> {
        let end = text
            .char_indices()
            .nth(SCAN_CHARS)
            .map_or(text.len(), |(i, _)| i);
        let text = &text[..end];
        self.set
            .matches(text)
            .iter()
            .next()
            .map(|i| &self.triggers[i])
    }

    /// Whether a member with `roles` may get an answer.
    pub fn member_ok(&self, roles: &[u64]) -> bool {
        let has = roles.iter().any(|r| self.roles.contains(r));
        if self.only {
            has
        } else {
            !has
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(v: Value) -> Spec {
        let mut base = json!({"channels": ["1"], "words": [{"text": "hello"}]});
        if let (Some(b), Some(v)) = (base.as_object_mut(), v.as_object()) {
            for (k, x) in v {
                b.insert(k.clone(), x.clone());
            }
        }
        parse(&base).unwrap()
    }

    #[test]
    fn words_match_whole_words_and_ignore_case() {
        let s = spec(json!({}));
        assert_eq!(
            s.find("Hello there").map(|t| t.text.as_str()),
            Some("hello")
        );
        assert!(s.find("othello").is_none());
        let s = spec(json!({"whole_words": false}));
        assert!(s.find("othello").is_some());
        let s = spec(json!({"ignore_case": false}));
        assert!(s.find("HELLO").is_none());
        // A phrase with signs is matched as typed, not as a pattern.
        let s = spec(json!({"words": [{"text": "a.b?"}]}));
        assert!(s.find("say a.b? now").is_some());
        assert!(s.find("say axb now").is_none());
    }

    #[test]
    fn patterns_off_words_and_limits() {
        let s = spec(
            json!({"words": [{"text": "x", "on": false}], "patterns": [{"text": r"^\d{3}$"}]}),
        );
        assert!(s.find("x").is_none());
        assert_eq!(s.find("123").map(|t| t.text.as_str()), Some(r"^\d{3}$"));
        assert_eq!(s.to_json()["words"][0]["on"], false);
        let bad = json!({"channels": ["1"], "patterns": [{"text": "("}]});
        assert!(parse(&bad).unwrap_err().contains("not valid"));
        let none = json!({"channels": ["1"], "words": [{"text": "x", "on": false}]});
        assert!(parse(&none).is_err());
        let no_channel = json!({"channels": [], "words": [{"text": "x"}]});
        assert!(parse(&no_channel).is_err());
        let fast = json!({"channels": ["1"], "words": [{"text": "x"}], "cooldown": 5});
        assert!(parse(&fast).is_err());
        let huge = json!({"channels": ["1"], "patterns": [{"text": "a{1000}{1000}"}]});
        assert!(parse(&huge).is_err());
        // Only the start of a long message is searched.
        let s = spec(json!({}));
        assert!(s
            .find(&format!("{} hello", "a".repeat(SCAN_CHARS)))
            .is_none());
    }

    #[test]
    fn roles_skip_or_only() {
        let s = spec(json!({"roles": {"mode": "skip", "ids": ["5"]}}));
        assert!(s.member_ok(&[1]));
        assert!(!s.member_ok(&[5]));
        let s = spec(json!({"roles": {"mode": "only", "ids": ["5"]}}));
        assert!(!s.member_ok(&[1]));
        assert!(s.member_ok(&[1, 5]));
        assert!(spec(json!({})).member_ok(&[]));
    }
}

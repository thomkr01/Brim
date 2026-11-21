//! Per-command settings of one server: config section `commands`, a map from a command's name
//! (as the command catalog lists it) to what the server changed. A command that is not in the map
//! runs as before. The dashboard validates with [`problem`]; the bot reads through [`of`], which
//! holds the same rules for a value that reached the database another way (a rollback).
use crate::permits::Level;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_ALIASES: usize = 5;
pub const MAX_COOLDOWN_SECS: u32 = 3600;
/// Shortest and longest wait before the bot deletes its answer.
pub const DELETE_SECS: (u32, u32) = (1, 300);
/// More entries than commands exist: only a cap on what one save may store.
const MAX_ENTRIES: usize = 300;

/// Commands staff need to end a lock, a quarantine or panic mode. They are never off, narrowed
/// or slowed, so a wrong setting cannot lock the staff out during an attack.
pub const SAFETY: &[&str] = &["lock", "unlock", "unquarantine", "anpanic"];

const FIELDS: &[&str] = &[
    "prefix",
    "slash",
    "aliases",
    "only",
    "delete_command",
    "delete_reply_secs",
    "reply",
    "cooldown_secs",
];

/// Narrows who may use a command below what its permit allows. It never widens: the command's own
/// permit check still runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Only {
    /// Everyone the permit allows.
    #[default]
    Permit,
    /// Trusted Admins, Extra Owners and the Owner.
    Ta,
    /// Extra Owners and the Owner.
    Eo,
    Owner,
}

impl Only {
    pub fn allows(self, level: &Level) -> bool {
        match self {
            Only::Permit => true,
            Only::Ta => level.immune(),
            Only::Eo => matches!(level, Level::Owner | Level::ExtraOwner),
            Only::Owner => *level == Level::Owner,
        }
    }

    /// For "Only ... can use it".
    pub fn who(self) -> &'static str {
        match self {
            Only::Permit => "members with the permit",
            Only::Ta => "the server owner, Extra Owners and Trusted Admins",
            Only::Eo => "the server owner and Extra Owners",
            Only::Owner => "the server owner",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct CmdCfg {
    /// The prefix form (`t!ban`) answers.
    pub prefix: bool,
    /// The slash form (`/ban`) answers.
    pub slash: bool,
    /// Extra prefix words for this command on this server.
    pub aliases: Vec<String>,
    pub only: Only,
    /// Delete the member's prefix command message.
    pub delete_command: bool,
    /// Delete the bot's answer to the prefix form after this many seconds; `None` leaves it to the
    /// server's own setting.
    pub delete_reply_secs: Option<u32>,
    /// Post the answer to the prefix form as a Discord reply to the command message.
    pub reply: bool,
    /// Seconds one member waits between two uses; 0 is no cooldown. Owner, Extra Owners and
    /// Trusted Admins never wait.
    pub cooldown_secs: u32,
}

impl Default for CmdCfg {
    fn default() -> Self {
        Self {
            prefix: true,
            slash: true,
            aliases: vec![],
            only: Only::Permit,
            delete_command: false,
            delete_reply_secs: None,
            reply: false,
            cooldown_secs: 0,
        }
    }
}

pub type CommandsCfg = BTreeMap<String, CmdCfg>;

/// The settings the bot acts on for command `name`, or `None` when the server changed nothing.
pub fn of(all: &CommandsCfg, name: &str) -> Option<CmdCfg> {
    let mut c = all.get(name)?.clone();
    if SAFETY.contains(&name) {
        (c.prefix, c.slash, c.only, c.cooldown_secs) = (true, true, Only::Permit, 0);
    }
    c.cooldown_secs = c.cooldown_secs.min(MAX_COOLDOWN_SECS);
    c.delete_reply_secs = c
        .delete_reply_secs
        .map(|s| s.clamp(DELETE_SECS.0, DELETE_SECS.1));
    Some(c)
}

/// A prefix word a server may add: 1-32 lower-case letters, digits, `-` or `_`.
pub fn alias_ok(a: &str) -> bool {
    (1..=32).contains(&a.len())
        && a.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Why the section `v` cannot be saved, in words for the person who saves it.
pub fn problem(v: &Value) -> Result<(), String> {
    let Some(map) = v.as_object() else {
        return Err("the command settings must be an object".into());
    };
    if map.len() > MAX_ENTRIES {
        return Err(format!("at most {MAX_ENTRIES} commands can have settings"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (name, c) in map {
        if !alias_ok(name) {
            return Err(format!("\"{name}\" is not a command name"));
        }
        if let Some(k) = c
            .as_object()
            .and_then(|o| o.keys().find(|k| !FIELDS.contains(&k.as_str())))
        {
            return Err(format!("{name}: unknown setting \"{k}\""));
        }
        let c: CmdCfg = serde_json::from_value(c.clone()).map_err(|e| format!("{name}: {e}"))?;
        if c.aliases.len() > MAX_ALIASES {
            return Err(format!("{name}: at most {MAX_ALIASES} aliases"));
        }
        for a in &c.aliases {
            if !alias_ok(a) {
                return Err(format!(
                    "{name}: the alias \"{a}\" must be 1-32 lower-case letters, digits, - or _"
                ));
            }
            if !seen.insert(a.clone()) {
                return Err(format!("the alias \"{a}\" is used twice"));
            }
        }
        if c.cooldown_secs > MAX_COOLDOWN_SECS {
            return Err(format!(
                "{name}: the cooldown is at most {MAX_COOLDOWN_SECS} seconds"
            ));
        }
        if c.delete_reply_secs
            .is_some_and(|s| !(DELETE_SECS.0..=DELETE_SECS.1).contains(&s))
        {
            return Err(format!(
                "{name}: the answer is deleted after {} to {} seconds",
                DELETE_SECS.0, DELETE_SECS.1
            ));
        }
        let limited = !c.prefix || !c.slash || c.only != Only::Permit || c.cooldown_secs > 0;
        if limited && SAFETY.contains(&name.as_str()) {
            return Err(format!(
                "{name} is a safety command: it cannot be turned off, narrowed or given a cooldown"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn narrowing_never_widens() {
        let custom = Level::Custom {
            rank: 1,
            perms: vec!["ban".into()],
            whitelist: vec![],
            limits: Default::default(),
        };
        let levels = [
            Level::None,
            custom,
            Level::TrustedAdmin,
            Level::ExtraOwner,
            Level::Owner,
        ];
        // Each step allows a subset of the step before, and the Owner always passes.
        let allowed = |o: Only| levels.iter().map(|l| o.allows(l)).collect::<Vec<_>>();
        assert_eq!(allowed(Only::Permit), [true; 5]);
        assert_eq!(allowed(Only::Ta), [false, false, true, true, true]);
        assert_eq!(allowed(Only::Eo), [false, false, false, true, true]);
        assert_eq!(allowed(Only::Owner), [false, false, false, false, true]);
    }

    #[test]
    fn saves_are_checked() {
        let ok = json!({"ban": {"prefix": false, "aliases": ["hammer"], "only": "ta", "cooldown_secs": 30, "delete_reply_secs": 10}});
        assert_eq!(problem(&ok), Ok(()));
        for bad in [
            json!([]),
            json!({"Ban": {}}),
            json!({"ban": {"prefx": false}}),
            json!({"ban": {"aliases": ["Has Space"]}}),
            json!({"ban": {"aliases": ["a", "b", "c", "d", "e", "f"]}}),
            json!({"ban": {"aliases": ["x"]}, "kick": {"aliases": ["x"]}}),
            json!({"ban": {"cooldown_secs": 3601}}),
            json!({"ban": {"delete_reply_secs": 0}}),
            json!({"ban": {"only": "everyone"}}),
            json!({"unlock": {"prefix": false}}),
            json!({"lock": {"only": "owner"}}),
            json!({"unquarantine": {"cooldown_secs": 5}}),
        ] {
            assert!(problem(&bad).is_err(), "{bad}");
        }
        // A safety command may still get an alias or the delete settings.
        assert_eq!(
            problem(&json!({"unlock": {"aliases": ["open"], "delete_command": true}})),
            Ok(())
        );
    }

    #[test]
    fn the_bot_reads_a_stored_value_by_the_same_rules() {
        let all: CommandsCfg = serde_json::from_value(json!({
            "unlock": {"prefix": false, "slash": false, "only": "owner", "cooldown_secs": 9},
            "ban": {"cooldown_secs": 999999, "delete_reply_secs": 0},
        }))
        .unwrap();
        let u = of(&all, "unlock").unwrap();
        assert!(u.prefix && u.slash && u.only == Only::Permit && u.cooldown_secs == 0);
        let b = of(&all, "ban").unwrap();
        assert_eq!(
            (b.cooldown_secs, b.delete_reply_secs),
            (MAX_COOLDOWN_SECS, Some(1))
        );
        assert!(of(&all, "kick").is_none());
    }
}

//! Prefix syntax: `ban @a @b 5d ?r reason ?dm`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub command: String,
    pub targets: Vec<u64>,
    /// Targets typed as `<@id>` mentions; the others in `targets` are raw ids.
    pub mentioned: Vec<u64>,
    /// Usernames or nicknames to look up, in order.
    pub names: Vec<String>,
    pub duration_s: Option<i64>,
    pub reason: Option<String>,
    /// `?t TYPE` that is not a duration (lock types such as `sc`); lock shortcuts like `slock` set it too.
    pub kind: Option<String>,
    /// Words after `?t TYPE` (purge `has`/`equals`/`starts` text, ids for `after`/`before`/`webhook`).
    pub input: Option<String>,
    /// `<@&id>` and `<#id>` mentions.
    pub roles: Vec<u64>,
    pub channels: Vec<u64>,
    pub flags: Flags,
    /// A duration over `duration::MAX_SECS` was typed; the command answers `duration::TOO_LONG`.
    pub too_long: bool,
}

impl Parsed {
    /// A target typed as a raw id or a name can hit the wrong member, a mention can't.
    pub fn unsure(&self) -> bool {
        !self.names.is_empty() || self.targets.iter().any(|t| !self.mentioned.contains(t))
    }
}

/// Command aliases: (words, canonical command, implied `?t` type).
pub const ALIASES: &[(&[&str], &str, Option<&str>)] = &[
    (&["b"], "ban", None),
    (&["hb"], "hackban", None),
    (&["k"], "kick", None),
    (
        &["mute", "silence", "m", "to", "tout", "t"],
        "timeout",
        None,
    ),
    (&["deltimeout", "unmute", "um"], "untimeout", None),
    (&["q"], "quarantine", None),
    (&["uq", "restore", "r"], "unquarantine", None),
    (&["wa"], "warn", None),
    (&["ub"], "unban", None),
    (&["lockdown", "ld"], "lock", None),
    (&["lockc", "clock"], "lock", Some("c")),
    (
        &["serverlock", "slock", "locks", "lockserver", "lockchannels"],
        "lock",
        Some("sc"),
    ),
    (&["ul"], "unlock", None),
    (&["lockupdate", "updatel", "locku"], "lock", Some("update")),
    (&["sweep", "clear", "p", "c"], "purge", None),
    (&["botclear", "bc"], "purge", Some("bots")),
    (&["san", "sanitise", "dehoist"], "sanitize", None),
    (&["sm"], "slowmode", None),
    (
        &["backup", "bcreate", "bsave", "btake", "createbackup"],
        "backups",
        Some("take"),
    ),
    (&["bload", "bapply", "loadbackup"], "backups", Some("load")),
    (
        &[
            "bdel",
            "bdelete",
            "deletebackup",
            "delbackup",
            "backupdelete",
        ],
        "backups",
        Some("delete"),
    ),
    (
        &["iclear", "imgclear", "ipurge", "imgp", "imgpurge", "ic"],
        "backups",
        Some("clear"),
    ),
];

pub(crate) fn alias(cmd: &str) -> (&str, Option<&str>) {
    ALIASES
        .iter()
        .find(|a| a.0.contains(&cmd))
        .map_or((cmd, None), |a| (a.1, a.2))
}

/// `?dm` tell the target, `?s` softban (ban + unban to purge messages), `?p` short reply,
/// `?h` hackban (bans already work on ids that left the server; the flag shows in the result),
/// `?na` / `?vc` kick only members without an avatar / in a voice channel.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Flags {
    pub dm: bool,
    pub soft: bool,
    pub compact: bool,
    /// `-blind` / `-b` on a lock: hide the channels too.
    #[serde(default)]
    pub blind: bool,
    #[serde(default)]
    pub hack: bool,
    #[serde(default)]
    pub no_avatar: bool,
    #[serde(default)]
    pub voice: bool,
    /// A ban or softban deletes the full 7 days of messages, whatever the server's purge days.
    #[serde(default)]
    pub full_purge: bool,
    /// The part of Brim that acts for the stats (`catch`); None: staff, or `brim` for the bot.
    #[serde(skip)]
    pub by: Option<&'static str>,
}

/// `s` is the message text with the prefix already stripped. Targets are mentions, raw ids, or names (words before the first `?flag`; with commas,
/// each comma-separated chunk is one name so multi-word names work).
pub fn parse(s: &str) -> Option<Parsed> {
    let mut it = s.split_whitespace().peekable();
    let word = it.next()?.to_lowercase();
    let (command, kind) = alias(&word);
    let mut p = Parsed {
        command: command.into(),
        kind: kind.map(Into::into),
        ..Default::default()
    };
    let mut name_words: Vec<&str> = vec![];
    let mut flagged = false;
    while let Some(tok) = it.next() {
        flagged |= tok.starts_with('?');
        match tok {
            "?r" => {
                let mut words = vec![];
                while let Some(t) =
                    it.next_if(|t| !matches!(*t, "?dm" | "?s" | "?h" | "?p" | "?t" | "?na" | "?vc"))
                {
                    words.push(t);
                }
                if !words.is_empty() {
                    p.reason = Some(words.join(" "));
                }
            }
            "?t" => {
                if let Some(v) = it.next_if(|t| !t.starts_with('?')) {
                    match brim_core::duration::parse(v) {
                        Some(d) => p.duration_s = Some(d),
                        None if brim_core::duration::too_long(v) => p.too_long = true,
                        None => {
                            p.kind = Some(v.to_lowercase());
                            let mut words = vec![];
                            while let Some(t) =
                                it.next_if(|t| !t.starts_with('?') && !t.starts_with('<'))
                            {
                                words.push(t);
                            }
                            p.input = Some(words.join(" ")).filter(|w| !w.is_empty());
                        }
                    }
                }
            }
            "?dm" => p.flags.dm = true,
            "?s" => p.flags.soft = true,
            "?p" => p.flags.compact = true,
            "?h" => p.flags.hack = true,
            "?na" => p.flags.no_avatar = true,
            "?vc" => p.flags.voice = true,
            "-blind" | "-b" if command == "lock" || command == "unlock" => p.flags.blind = true,
            _ if tok.starts_with("<@&") || tok.starts_with("<#") => {
                // Discord ids are never 0, and `Id::new(0)` panics.
                let id = tok
                    .trim_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u64>();
                if let Some(id) = id.ok().filter(|&id| id != 0) {
                    if tok.starts_with("<#") {
                        p.channels.push(id)
                    } else {
                        p.roles.push(id)
                    }
                }
            }
            _ => {
                let id = tok
                    .trim_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u64>()
                    .ok()
                    .filter(|&id| id != 0);
                if let (Some(id), true) = (
                    id,
                    tok.starts_with("<@") || tok.chars().all(|c| c.is_ascii_digit()),
                ) {
                    if tok.starts_with("<@") {
                        p.mentioned.push(id);
                    }
                    p.targets.push(id);
                } else if let Some(d) = brim_core::duration::parse(tok) {
                    p.duration_s = Some(d);
                } else if brim_core::duration::too_long(tok) {
                    p.too_long = true;
                } else if !flagged {
                    name_words.push(tok);
                }
            }
        }
    }
    let joined = name_words.join(" ");
    p.names = if joined.contains(',') {
        joined
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(String::from)
            .collect()
    } else {
        name_words.into_iter().map(String::from).collect()
    };
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets_duration_reason() {
        let p = parse("ban <@!111> 222 5d ?r spamming links ?dm").unwrap();
        assert_eq!(p.command, "ban");
        assert_eq!(p.targets, vec![111, 222]);
        assert_eq!(p.mentioned, vec![111]);
        assert!(p.unsure());
        assert!(!parse("ban <@111> ?r x").unwrap().unsure());
        assert!(parse("ban alice").unwrap().unsure());
        assert_eq!(p.duration_s, Some(432_000));
        assert_eq!(p.reason.as_deref(), Some("spamming links"));
        assert!(p.flags.dm);
    }

    #[test]
    fn id_zero_is_no_target() {
        // Discord ids are never 0, and `Id::new(0)` panics.
        let p = parse("ban 0 <@0> <@&0> <#0>").unwrap();
        assert!(p.targets.is_empty() && p.roles.is_empty() && p.channels.is_empty());
    }

    #[test]
    fn a_duration_over_the_cap_is_flagged_not_a_name() {
        let p = parse("ban <@5> 106751991167300d").unwrap();
        assert!(p.too_long && p.duration_s.is_none() && p.names.is_empty());
        let p = parse("lock ?t 400d").unwrap();
        assert!(p.too_long && p.kind.is_none());
        assert!(!parse("ban <@5> 365d").unwrap().too_long);
    }

    #[test]
    fn names_and_commas() {
        let p = parse("ban alice bob 5d ?r x").unwrap();
        assert_eq!(p.names, vec!["alice", "bob"]);
        let p = parse("kick john smith, <@5>, bob ?r why ok").unwrap();
        assert_eq!(p.names, vec!["john smith", "bob"]);
        assert_eq!(p.targets, vec![5]);
        assert!(parse("ban ?r not a name").unwrap().names.is_empty());
        let p = parse("warn sample_user#0").unwrap();
        assert_eq!(
            (p.names, p.targets),
            (vec!["sample_user#0".to_string()], vec![])
        );
    }

    #[test]
    fn softban_hackban_compact_flags() {
        let p = parse("ban 5 ?r spam ?s ?h ?p").unwrap();
        assert_eq!(p.reason.as_deref(), Some("spam"));
        assert_eq!(
            (p.flags.soft, p.flags.compact, p.flags.dm),
            (true, true, false)
        );
    }

    #[test]
    fn aliases_and_t_flag() {
        assert_eq!(parse("b 5").unwrap().command, "ban");
        assert_eq!(parse("hb 5").unwrap().command, "hackban");
        assert_eq!(parse("deltimeout 5").unwrap().command, "untimeout");
        assert_eq!(parse("r 5").unwrap().command, "unquarantine");
        let p = parse("slock").unwrap();
        assert_eq!(
            (p.command.as_str(), p.kind.as_deref()),
            ("lock", Some("sc"))
        );
        assert_eq!(parse("ld ?t K").unwrap().kind.as_deref(), Some("k"));
        let p = parse("bload 3").unwrap();
        assert_eq!(
            (p.command.as_str(), p.kind.as_deref(), p.targets.as_slice()),
            ("backups", Some("load"), &[3][..])
        );
        let p = parse("locku raid over").unwrap();
        assert_eq!(
            (p.command.as_str(), p.kind.as_deref()),
            ("lock", Some("update"))
        );
        let p = parse("t 5 ?t 15m ?r why").unwrap();
        assert_eq!(
            (p.duration_s, p.reason.as_deref()),
            (Some(900), Some("why"))
        );
        // `?t` ends a reason.
        let p = parse("m 5 ?r too loud ?t 1h").unwrap();
        assert_eq!(
            (p.duration_s, p.reason.as_deref()),
            (Some(3600), Some("too loud"))
        );
    }

    #[test]
    fn type_input_roles_channels() {
        let p = parse("purge 50 <@&7> ?t has free nitro ?r spam").unwrap();
        assert_eq!(
            (p.kind.as_deref(), p.input.as_deref()),
            (Some("has"), Some("free nitro"))
        );
        assert_eq!(
            (p.roles.as_slice(), p.targets.as_slice()),
            (&[7][..], &[50][..])
        );
        let p = parse("lock ?t c <#1> <#2>").unwrap();
        assert_eq!(
            (p.kind.as_deref(), p.input, p.channels),
            (Some("c"), None, vec![1, 2])
        );
    }

    #[test]
    fn blind_flag_on_locks() {
        assert!(parse("lock -blind").unwrap().flags.blind);
        assert!(parse("lock sc -b").unwrap().flags.blind);
        assert!(!parse("purge 5 -b").unwrap().flags.blind);
    }

    #[test]
    fn kick_filters_and_hackban_flag() {
        let p = parse("kick 5 ?na ?vc ?r spam").unwrap();
        assert!(p.flags.no_avatar && p.flags.voice && !p.flags.hack);
        assert_eq!(p.reason.as_deref(), Some("spam"));
        let p = parse("ban 5 ?r x ?na").unwrap();
        assert_eq!(p.reason.as_deref(), Some("x"));
        assert!(parse("ban 5 ?h").unwrap().flags.hack);
    }

    #[test]
    fn bare_command() {
        assert_eq!(parse("warn 5").unwrap().targets, vec![5]);
        assert!(parse("").is_none());
    }
}

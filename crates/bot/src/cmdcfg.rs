//! A server's own settings for one command (config section `commands`, the dashboard's Commands
//! page): prefix and slash form on or off, extra aliases, a narrower "who may use it", deleting
//! the command message or the answer, answering as a reply, and a cooldown.
//!
//! One gate per entry point ([`prefix`] from `events`, [`slash`] from `slash::handle`), so no
//! command can skip it. The gate only ever refuses: each command's own permit check stays where
//! it is and still runs after the gate. A command the server never changed costs one map lookup.
use crate::{app::App, general, http_err::best_effort};
use anyhow::Result;
use brim_core::{
    cmdcfg::{self, CmdCfg, CommandsCfg},
    permits::Level,
};
use std::{
    collections::{BTreeSet, HashMap},
    sync::LazyLock,
};
use twilight_model::{
    application::{
        command::CommandType,
        interaction::{application_command::CommandData, Interaction},
    },
    channel::Message,
    gateway::payload::incoming::MessageCreate,
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
        Id,
    },
};

/// What the running prefix command's settings ask of its answers.
#[derive(Clone, Copy, Default)]
pub struct Run {
    guild: Option<Id<GuildMarker>>,
    channel: Option<Id<ChannelMarker>>,
    reply_to: Option<Id<MessageMarker>>,
    delete_secs: Option<u32>,
    /// `events` deletes the command message once the command has run (its files may be relayed).
    pub delete_command: bool,
}

tokio::task_local! {
    /// Set by `events` around one prefix command, so every answer path can read it without a
    /// new argument through each command.
    pub static RUN: Run;
}

/// The settings, for an answer posted in the channel the command was used in.
fn here(channel: Id<ChannelMarker>) -> Option<Run> {
    RUN.try_with(|r| *r)
        .ok()
        .filter(|r| r.channel == Some(channel))
}

/// The command message, when the answer in `channel` goes out as a reply to it.
pub fn reply_to(channel: Id<ChannelMarker>) -> Option<Id<MessageMarker>> {
    here(channel)?.reply_to
}

/// Seconds after which the answer in `channel` is deleted, when the command has its own setting.
pub fn delete_secs(channel: Id<ChannelMarker>) -> Option<u32> {
    here(channel)?.delete_secs
}

/// Call with each answer a prefix command posted: queues its delete when the command asks for one.
pub async fn sent(
    app: &App,
    channel: Id<ChannelMarker>,
    answer: twilight_http::Response<Message>,
) -> Result<()> {
    let Some(r) = here(channel) else {
        return Ok(());
    };
    if let (Some(secs), Some(gid)) = (r.delete_secs, r.guild) {
        let id = answer.model().await?.id;
        crate::reply::delete_later(app, gid, channel, id, secs).await?;
    }
    Ok(())
}

/// Every prefix word the bot answers by itself. A server's alias never replaces one of these.
pub(crate) static RESERVED: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    general::words()
        .chain(
            crate::prefix::ALIASES
                .iter()
                .flat_map(|a| a.0.iter().copied()),
        )
        .chain(crate::tickets::words())
        .chain(crate::settings::words())
        .chain(crate::commands::KNOWN.iter().copied())
        .chain(["whitelist"])
        .collect()
});

/// The catalog name of the command a word runs; `in_ticket` picks the ticket meaning of the
/// words `r`, `c`, `s` and `whitelist`.
pub(crate) fn canonical(word: &str, in_ticket: bool) -> Option<&'static str> {
    match (in_ticket, word) {
        (true, "r") => return Some("reply"),
        (true, "c") => return Some("close"),
        (true, "s") => return Some("snippet"),
        (true, "whitelist") => return Some("unblock"),
        (_, "softban") => return Some("ban"),
        _ => {}
    }
    general::name_of(word).or_else(|| general::name_of(crate::prefix::alias(word).0))
}

/// Words a ticket reads its own way (`tickets::commands::TICKET_ONLY`).
const TICKET_WORDS: &[&str] = &["r", "c", "s", "whitelist"];

/// A second command the same text runs, so its settings count too: a narrowed command is not
/// reachable through a sibling word. `has` tells if the text holds a word.
fn also(name: &str, has: impl Fn(&str) -> bool) -> Option<&'static str> {
    match name {
        "hackban" => Some("ban"),
        "warns" => Some("cases"),
        "cases" if has("modify") => Some("editc"),
        "info" if ["-server", "-guild", "-g"].iter().any(|w| has(w)) => Some("serverinfo"),
        _ => None,
    }
}

/// The command whose alias on this server is `word`.
fn custom<'a>(all: &'a CommandsCfg, word: &str) -> Option<&'a str> {
    if RESERVED.contains(word) {
        return None;
    }
    all.iter()
        .find(|(_, c)| c.aliases.iter().any(|a| a == word))
        .map(|(name, _)| name.as_str())
}

/// When (ms) each (server, member, command) may run again. Past the cap only run-out entries
/// go, so one busy server never resets another's cooldowns.
fn wait(
    until: &mut HashMap<(u64, u64, &'static str), i64>,
    key: (u64, u64, &'static str),
    secs: u32,
    now: i64,
) -> Option<i64> {
    if until.len() > 10_000 {
        until.retain(|_, t| *t > now);
    }
    match until.get(&key) {
        Some(t) if *t > now => Some((*t - now + 999) / 1000),
        // Still full of running cooldowns: this use goes uncounted, the map never grows on.
        _ if until.len() > 10_000 => None,
        _ => {
            until.insert(key, now + i64::from(secs) * 1000);
            None
        }
    }
}

/// Why the server's settings stop this use, as the text for the member. `on` is the switch of the
/// form used and `shown` the command as typed (`t!ban`, `/ban`).
#[allow(clippy::too_many_arguments)]
fn refusal(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    name: &'static str,
    c: &CmdCfg,
    on: bool,
    level: &Level,
    shown: &str,
) -> Option<String> {
    if !on {
        return Some(format!("`{shown}` is turned off on this server."));
    }
    if !c.only.allows(level) {
        let who = c.only.who();
        return Some(format!("Only {who} can use `{shown}` on this server."));
    }
    if c.cooldown_secs > 0 && !level.immune() {
        let key = (gid.get(), user.get(), name);
        let now = crate::automod::now_ms();
        let left = wait(
            &mut app.cmd_waits.lock().unwrap(),
            key,
            c.cooldown_secs,
            now,
        );
        if let Some(left) = left {
            return Some(format!(
                "`{shown}` has a cooldown on this server. Try again in {left} s."
            ));
        }
    }
    None
}

/// Gate of every prefix command. `rest` is the message after the prefix. `None`: the server's
/// settings stopped the command (the member was told, unless they have no permit). Otherwise
/// the text to run, with a server alias swapped for its command, and what its answers follow.
pub async fn prefix(
    app: &App,
    m: &MessageCreate,
    gid: Id<GuildMarker>,
    cfg: &crate::app::GuildCfg,
    rest: Option<&str>,
) -> Result<Option<(Option<String>, Run)>> {
    let (Some(rest), false) = (rest, cfg.commands.is_empty()) else {
        return Ok(Some((rest.map(String::from), Run::default())));
    };
    let r = rest.trim_start();
    let (word, tail) = r.split_at(r.find(char::is_whitespace).unwrap_or(r.len()));
    let mut word = word.to_lowercase();
    let mut rest = rest.to_string();
    if let Some(name) = custom(&cfg.commands, &word) {
        rest = format!("{name}{tail}");
        word = name.to_string();
    }
    let pass = |rest| Ok(Some((Some(rest), Run::default())));
    let in_ticket = TICKET_WORDS.contains(&word.as_str())
        && crate::tickets::in_ticket(app, cfg, gid, m.channel_id).await?;
    let Some(name) = canonical(&word, in_ticket) else {
        return pass(rest);
    };
    let has = |w: &str| tail.split_whitespace().any(|t| t.eq_ignore_ascii_case(w));
    let sibling = also(name, has).and_then(|n| Some((n, cmdcfg::of(&cfg.commands, n)?)));
    let own = cmdcfg::of(&cfg.commands, name);
    if own.is_none() && sibling.is_none() {
        return pass(rest);
    }
    // The bot's silence rules come first (quarantined members, voice-channel chats): the
    // command's own path stays silent for them, and so does the gate.
    if crate::commands::ignored(app, m, gid, &rest).await? {
        return pass(rest);
    }
    let owner = app.guild_info(gid).await?.owner;
    let level = app.level(gid, owner, m.author.id).await?;
    // A member without a permit gets no answer: nothing to learn and nothing to spam with.
    let quiet = level == Level::None;
    for (name, c) in own
        .iter()
        .map(|c| (name, c))
        .chain(sibling.iter().map(|s| (s.0, &s.1)))
    {
        let shown = format!("{}{name}", cfg.misc.prefix);
        if let Some(no) = refusal(app, gid, m.author.id, name, c, c.prefix, &level, &shown) {
            if !quiet {
                crate::commands::say(app, m, &no).await?;
            }
            return Ok(None);
        }
    }
    let c = own.unwrap_or_default();
    // The same rule as "delete the moderation command messages": only a permit holder's message.
    let delete_command = c.delete_command && !quiet;
    let run = Run {
        guild: Some(gid),
        channel: Some(m.channel_id),
        // A reply to a message that is deleted right after shows "original message was deleted".
        reply_to: (c.reply && !delete_command).then_some(m.id),
        delete_secs: c.delete_reply_secs,
        delete_command,
    };
    Ok(Some((Some(rest), run)))
}

/// After the command ran: deletes the command message when its settings ask for that.
pub async fn done(app: &App, m: &MessageCreate, run: Run) {
    if run.delete_command {
        let r = app.http.delete_message(m.channel_id, m.id).await;
        best_effort(r, "delete command message");
    }
}

/// The prefix word of a slash command (after `slash::flatten`: `/ban remove` is unban) or of a
/// right-click entry ("Kick" is kick), so the settings of that command apply.
fn slash_word(flat: &str) -> String {
    match flat {
        "Sanitize & Dehoist" => "sanitize".into(),
        n if n.starts_with("Purge 100") => "purge".into(),
        n => n.to_lowercase(),
    }
}

/// Gate of every slash command and right-click command: the text to answer with when the
/// server's settings stop it. Runs after the interaction was acknowledged (it asks Discord for
/// the member).
pub async fn slash(app: &App, i: &Interaction, data: &CommandData) -> Result<Option<String>> {
    let (Some(gid), Some(user)) = (i.guild_id, i.author_id()) else {
        return Ok(None);
    };
    let cfg = app.cfg(gid.get()).await?;
    if cfg.commands.is_empty() {
        return Ok(None);
    }
    let word = slash_word(crate::slash::flatten(&data.name, &data.options).0);
    let Some(name) = canonical(&word, false) else {
        return Ok(None);
    };
    // `/cases modify` is `cases modify`, `/info server` is `info -server`.
    let sub = data.options.first().map(|o| o.name.as_str());
    let has = |w: &str| sub.is_some_and(|s| s == w.trim_start_matches('-'));
    let owner = app.guild_info(gid).await?.owner;
    let level = app.level(gid, owner, user).await?;
    for name in [Some(name), also(name, has)].into_iter().flatten() {
        let Some(c) = cmdcfg::of(&cfg.commands, name) else {
            continue;
        };
        let shown = match data.kind {
            CommandType::ChatInput => format!("/{name}"),
            _ => data.name.clone(),
        };
        let no = refusal(app, gid, user, name, &c, c.slash, &level, &shown);
        if no.is_some() {
            return Ok(no);
        }
    }
    Ok(None)
}

/// Gate of a button that runs command `name` (the Ban button of a report): the same settings
/// as the typed command. Off means both forms are off.
pub async fn button(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    level: &Level,
    name: &'static str,
) -> Result<Option<String>> {
    let cfg = app.cfg(gid.get()).await?;
    let Some(c) = cmdcfg::of(&cfg.commands, name) else {
        return Ok(None);
    };
    let on = c.prefix || c.slash;
    Ok(refusal(app, gid, user, name, &c, on, level, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::T;
    use serde_json::json;

    #[test]
    fn words_resolve_to_their_command() {
        assert_eq!(canonical("b", false), Some("ban"));
        assert_eq!(canonical("softban", false), Some("ban"));
        assert_eq!(canonical("slock", false), Some("lock"));
        assert_eq!(canonical("lockupdate", false), Some("lock"));
        assert_eq!(canonical("r", false), Some("unquarantine"));
        assert_eq!(canonical("r", true), Some("reply"));
        assert_eq!(canonical("c", true), Some("close"));
        assert_eq!(canonical("whitelist", true), Some("unblock"));
        assert_eq!(canonical("whitelist", false), Some("whitelist"));
        // A sibling word answers to the settings of the command it runs.
        let none = |_: &str| false;
        assert_eq!(also("hackban", none), Some("ban"));
        assert_eq!(also("warns", none), Some("cases"));
        assert_eq!(also("cases", none), None);
        assert_eq!(also("cases", |w| w == "modify"), Some("editc"));
        assert_eq!(also("info", |w| w == "-server"), Some("serverinfo"));
        for n in ["ban", "cases", "editc", "serverinfo"] {
            assert_eq!(canonical(n, false), Some(n));
        }
        assert_eq!(canonical("hello", false), None);
        // Slash subcommands and every right-click entry land on a command too.
        for (flat, name) in [
            ("unban", "unban"),
            ("lockupdate", "lock"),
            ("Ban", "ban"),
            ("Kick", "kick"),
            ("Timeout", "timeout"),
            ("Quarantine", "quarantine"),
            ("Sanitize & Dehoist", "sanitize"),
            ("Purge 100 From Here", "purge"),
            ("Purge 100 Until Here", "purge"),
            ("db", "dashboard"),
        ] {
            assert_eq!(canonical(&slash_word(flat), false), Some(name), "{flat}");
        }
        // No right-click entry is left without a command (Report Message is not a command).
        for d in crate::slash::definitions() {
            if d.kind != CommandType::ChatInput && d.name != "Report Message" {
                assert!(
                    canonical(&slash_word(&d.name), false).is_some(),
                    "{}",
                    d.name
                );
            }
        }
        // Every safety command is a real command, so its rule can never miss by a typo.
        for s in cmdcfg::SAFETY {
            assert_eq!(canonical(s, false), Some(*s));
        }
    }

    #[test]
    fn a_server_alias_never_replaces_a_built_in_word() {
        let all: CommandsCfg = serde_json::from_value(json!({
            "ban": {"aliases": ["hammer", "kick", "unlock", "slock", "r", "s", "jg", "verifysetup"]},
        }))
        .unwrap();
        assert_eq!(custom(&all, "hammer"), Some("ban"));
        for built_in in [
            "kick",
            "unlock",
            "slock",
            "r",
            "s",
            "jg",
            "softban",
            "verifysetup",
            "whitelist",
        ] {
            assert_eq!(custom(&all, built_in), None, "{built_in}");
        }
    }

    #[test]
    fn cooldown_counts_per_member_and_command() {
        let mut m = HashMap::new();
        assert_eq!(wait(&mut m, (1, 2, "ban"), 10, 0), None);
        assert_eq!(wait(&mut m, (1, 2, "ban"), 10, 1), Some(10));
        assert_eq!(wait(&mut m, (1, 2, "ban"), 10, 9_001), Some(1));
        assert_eq!(wait(&mut m, (1, 2, "kick"), 10, 1), None);
        assert_eq!(wait(&mut m, (1, 3, "ban"), 10, 1), None);
        assert_eq!(wait(&mut m, (1, 2, "ban"), 10, 10_000), None);
    }

    fn message(t: &T, id: u64, author: u64, content: &str) -> MessageCreate {
        let mut m = crate::automod::more_tests::msg(
            json!({"id": id.to_string(), "content": content,
            "author": {"id": author.to_string(), "username": "x", "discriminator": "0", "avatar": null}}),
        );
        m.guild_id = Some(t.g(0));
        MessageCreate {
            channel_type: None,
            message: m,
        }
    }

    /// The gate over the mock: off, narrowed, cooldown, alias, safety, and members without a permit.
    #[tokio::test]
    async fn the_prefix_gate_follows_the_settings() {
        let Some(t) = T::new(431).await else { return };
        let (owner, gid) = (t.owner(), t.g(0));
        let member = t.base * 10 + 1;
        t.set(|m| {
            m.members.insert((gid.get(), member), vec![]);
        });
        t.server_config(
            0,
            owner,
            json!({"commands": {
                "ban": {"prefix": false, "aliases": ["hammer"]},
                "kick": {"only": "owner", "aliases": ["boot", "unlock"]},
                "ping": {"cooldown_secs": 60, "reply": true, "delete_reply_secs": 5},
                "warn": {"delete_command": true, "reply": true},
                "unlock": {"prefix": false, "only": "owner", "cooldown_secs": 60},
            }}),
        )
        .await;
        let cfg = t.app.cfg(gid.get()).await.unwrap();
        let gate = |id: u64, who: u64, text: &'static str| {
            let (app, cfg, m) = (&t.app, &cfg, message(&t, id, who, text));
            async move {
                let rest = m.content.strip_prefix("t!");
                prefix(app, &m, gid, cfg, rest).await.unwrap()
            }
        };
        let posts = || t.calls("POST /channels/7/messages");
        // Off: the owner is told, a member without a permit gets silence; the alias is off too.
        assert!(gate(1, owner, "t!ban <@5>").await.is_none());
        assert_eq!(posts(), 1);
        assert!(gate(2, owner, "t!hammer <@5>").await.is_none());
        assert!(gate(3, member, "t!b <@5>").await.is_none());
        assert_eq!(posts(), 2);
        // An alias becomes its command and keeps the rest of the line; a built-in word stays itself.
        let (rest, _) = gate(4, owner, "t!boot <@5> ?r spam").await.unwrap();
        assert_eq!(rest.as_deref(), Some("kick <@5> ?r spam"));
        let (rest, _) = gate(5, owner, "t!unlock").await.unwrap();
        assert_eq!(
            rest.as_deref(),
            Some("unlock"),
            "a safety command is never off"
        );
        // A cooldown: the second use waits, per member; the owner never waits.
        let (_, run) = gate(6, member, "t!ping").await.unwrap();
        assert!(run.reply_to.is_some() && run.delete_secs == Some(5) && !run.delete_command);
        assert!(gate(7, member, "t!ping").await.is_none());
        assert!(gate(8, owner, "t!ping").await.is_some());
        assert!(gate(9, owner, "t!ping").await.is_some());
        // Deleting the command message: only for a permit holder, and then no reply reference.
        let (_, run) = gate(10, owner, "t!warn <@5>").await.unwrap();
        assert!(run.delete_command && run.reply_to.is_none());
        let (_, run) = gate(11, member, "t!warn <@5>").await.unwrap();
        assert!(!run.delete_command);
        // Untouched commands and plain messages pass as they are.
        let (rest, _) = gate(12, member, "t!about").await.unwrap();
        assert_eq!(rest.as_deref(), Some("about"));
        assert_eq!(posts(), 2, "nobody else was answered by the gate");
        t.cleanup().await;
    }
}

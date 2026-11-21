//! Reply-shaping helpers driven by `MiscCfg`: compact text, DMs, delayed deletes, message purges.
use anyhow::{Context, Result};
use brim_core::jobs;
use brim_logging::botmsg;
use brim_logging::embed::{duration, Log, ARROW, BLUE, GREEN, NO, YES};
use brim_logging::emoji::{DETAILS, MODERATOR, PANIC, REASON, UNSET, WARNING};
use brim_logging::LogKind;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;
use twilight_model::channel::message::{Component, Embed};
use twilight_model::id::{
    marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
    Id,
};

use crate::app::{now_secs, App};
use crate::http_err::{best_effort, gone};
use crate::prefix::Flags;

/// Compact replies are one line; the full form adds moderator and reason.
pub fn case_text(
    compact: bool,
    (case_no, kind, target): (i64, &str, u64),
    reason: Option<&str>,
    actor: u64,
) -> String {
    let mut s = format!("Case #{case_no}: {kind} <@{target}>");
    if !compact {
        s += format!("\nModerator: <@{actor}>").as_str();
        if let Some(r) = reason {
            s += format!("\nReason: {r}").as_str();
        }
    }
    if reason.is_none() {
        s += add_reason_hint(case_no).as_str();
    }
    s
}

/// Whether the member was told about the action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DmOutcome {
    /// No DM was asked for (server toggle off and no `?dm`).
    #[default]
    Off,
    Sent,
    /// Closed DMs, an error, or no answer within [`DM_WAIT`].
    Failed,
}

/// One target's outcome. `case` is set when the action happened; `text` is the plain reply
/// (`Case #N: ...` on success, the refusal or error otherwise).
#[derive(Debug, Default)]
pub struct Applied {
    pub target: Option<u64>,
    pub case: Option<i32>,
    pub text: String,
    pub dm: DmOutcome,
    /// The command already posted its result in the channel (a purge), so no reply repeats it.
    pub posted: bool,
}

impl From<String> for Applied {
    fn from(text: String) -> Self {
        Self {
            text,
            ..Default::default()
        }
    }
}

/// The server's edited versions of the moderation answers (`reply.mod.*`, plan D1). The default
/// is the code's own text, so a server that edited none gets exactly what it always got.
#[derive(Default)]
pub struct ModTexts {
    result: Option<Value>,
    total: Option<Value>,
    case: Option<Value>,
}

impl ModTexts {
    pub async fn load(app: &App, gid: Id<GuildMarker>) -> Self {
        let get = |k| stored_msg(app, gid, k);
        Self {
            result: get("reply.mod.result").await,
            total: get("reply.mod.total").await,
            case: get("reply.mod.case").await,
        }
    }

    #[cfg(test)]
    pub fn from_defaults() -> Self {
        let get = |k: &str| botmsg::find(k).map(|d| d.template.clone());
        Self {
            result: get("reply.mod.result"),
            total: get("reply.mod.total"),
            case: get("reply.mod.case"),
        }
    }

    #[cfg(test)]
    pub fn with(key: &str, v: Value) -> Self {
        let mut t = Self::default();
        match key {
            "reply.mod.result" => t.result = Some(v),
            "reply.mod.total" => t.total = Some(v),
            _ => t.case = Some(v),
        }
        t
    }

    /// The entry `key` filled with `vars`, when the server's edited version is in use.
    fn say(&self, key: &str, vars: &[(&str, &str)]) -> Option<(String, Vec<Embed>)> {
        let stored = match key {
            "reply.mod.result" => &self.result,
            "reply.mod.total" => &self.total,
            _ => &self.case,
        };
        let stored = stored.as_ref().filter(|s| botmsg::uses_stored(key, s))?;
        Some(parts(&botmsg::render(key, Some(stored), vars)))
    }
}

/// One reply for all targets. Compact mode turns several targets into count lines; failures stay listed.
pub fn summary(
    kind: &str,
    compact: bool,
    done: &[Applied],
    reason: Option<&str>,
    actor: u64,
) -> String {
    summary_with(&ModTexts::default(), kind, compact, done, reason, actor).0
}

/// [`summary`] with the server's edited texts: the count lines, and the line of each case.
pub fn summary_with(
    t: &ModTexts,
    kind: &str,
    compact: bool,
    done: &[Applied],
    reason: Option<&str>,
    actor: u64,
) -> (String, Vec<Embed>) {
    let (mut text, mut embeds) = if compact && done.len() > 1 {
        let (ok, failed): (Vec<_>, Vec<_>) = done.iter().partition(|a| a.case.is_some());
        let reason_line = reason.map_or(String::new(), |r| format!("{REASON} Reason: {r}"));
        let failures: Vec<&str> = failed.iter().map(|f| f.text.as_str()).collect();
        let (n_ok, n_failed, by) = (
            ok.len().to_string(),
            failed.len().to_string(),
            format!("<@{actor}>"),
        );
        let vars = [
            ("kind", kind),
            ("ok", n_ok.as_str()),
            ("failed", n_failed.as_str()),
            ("reason_line", reason_line.as_str()),
            ("moderator", by.as_str()),
            ("failures", &failures.join("\n")),
        ];
        t.say("reply.mod.total", &vars).unwrap_or_else(|| {
            let mut s = format!(
                "{YES} Successful {kind}s: `{}`\n{NO} Unsuccessful {kind}s: `{}`",
                ok.len(),
                failed.len()
            );
            if let Some(r) = reason {
                s += format!("\n{REASON} Reason: {r}").as_str();
            }
            s += format!("\n{MODERATOR} Moderator: <@{actor}>").as_str();
            (failures.iter().fold(s, |s, f| s + "\n" + *f), vec![])
        })
    } else {
        let mut embeds = Vec::new();
        let texts: Vec<String> = done
            .iter()
            .map(|a| match t.case_line(a) {
                Some((text, e)) => {
                    embeds.extend(e);
                    text
                }
                None => a.text.clone(),
            })
            .collect();
        (texts.join("\n"), embeds)
    };
    text.truncate(text.floor_char_boundary(2000));
    embeds.truncate(10);
    (text, embeds)
}

impl ModTexts {
    /// The edited line of a case a command made (`Case #12: ban <@5>` and what the bot adds below
    /// it), or `None` when the default stands or the text is not a case line.
    fn case_line(&self, a: &Applied) -> Option<(String, Vec<Embed>)> {
        let (case, target) = (a.case?, a.target?);
        let (first, rest) = a.text.split_once('\n').unwrap_or((a.text.as_str(), ""));
        let kind = first
            .strip_prefix(&format!("Case #{case}: "))?
            .strip_suffix(&format!(" <@{target}>"))?;
        let member = format!("<@{target}>");
        let vars = [
            ("case", case.to_string()),
            ("kind", kind.to_string()),
            ("member", member),
            ("rest", rest.to_string()),
        ];
        let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.say("reply.mod.case", &vars)
    }
}

/// "Add Reason" pointer for actions run without a reason.
pub fn add_reason_hint(case_no: i64) -> String {
    format!("\nAdd Reason: `/cases modify case:{case_no} reason:`")
}

/// Title word, plural for the Successful/Unsuccessful headings, and the line shown when nothing failed.
fn labels(kind: &str) -> (&'static str, &'static str, &'static str) {
    match kind {
        "ban" => ("Ban", "bans", "All members were banned!"),
        "softban" => ("Softban", "softbans", "All members were softbanned!"),
        "kick" => ("Kick", "kicks", "All members were kicked!"),
        "warn" => ("Warn", "warns", "All members were warned!"),
        "unban" => ("Unban", "unbans", "All members were unbanned!"),
        "untimeout" => (
            "Untimeout",
            "timeout removals",
            "All timeouts were removed!",
        ),
        "quarantine" => ("Quarantine", "quarantines", "All members were quarantined!"),
        "unquarantine" => (
            "Unquarantine",
            "quarantine removals",
            "All members were released from quarantine!",
        ),
        "verify" => ("Verify", "verifications", "All members were verified!"),
        "sanitize" => ("Sanitize", "renames", "All members were sanitized!"),
        "purge" => ("Purge", "purges", "Nothing failed!"),
        "slowmode" => ("Slowmode", "changes", "Nothing failed!"),
        _ => ("Timeout", "timeouts", "All members were timed out!"),
    }
}

/// `✅` when every DM that was tried arrived, `❎` when none did (or none was tried), counts when mixed.
fn dm_flag(done: &[Applied]) -> String {
    let sent = done.iter().filter(|a| a.dm == DmOutcome::Sent).count();
    let failed = done.iter().filter(|a| a.dm == DmOutcome::Failed).count();
    match (sent, failed) {
        (0, _) => UNSET.to_string(),
        (_, 0) => YES.to_string(),
        (s, f) => format!("{YES} {s} | {UNSET} {f}"),
    }
}

/// The Details lines of a result: the options that apply to `kind`, each ✅ when used.
pub fn details_lines(
    kind: &str,
    flags: Flags,
    duration_s: Option<i64>,
    done: &[Applied],
) -> Vec<(&'static str, String)> {
    let on = |b: bool| if b { YES } else { UNSET }.to_string();
    let dur = || match duration_s {
        Some(d) => format!("`{}` {YES}", duration(d)),
        None => UNSET.to_string(),
    };
    let dm = ("DM-Members", dm_flag(done));
    match kind {
        "ban" | "softban" => vec![
            // A softban unbans at once, so a typed duration does nothing.
            (
                "Duration",
                if kind == "softban" { on(false) } else { dur() },
            ),
            ("Soft Ban", on(kind == "softban")),
            ("Hack Ban", on(flags.hack)),
            dm,
        ],
        "kick" => vec![
            ("No-Avatar", on(flags.no_avatar)),
            ("Voice-Channels", on(flags.voice)),
            dm,
        ],
        "timeout" => vec![("Duration", dur()), dm],
        "warn" | "quarantine" | "unquarantine" | "untimeout" | "unban" => vec![dm],
        _ => vec![],
    }
}

/// "Timeout result:" and its kin (Reason, Moderator, Details, Successful and Unsuccessful
/// blocks), plus a pointer to add a reason when none was given.
pub fn result_embeds(
    kind: &str,
    done: &[Applied],
    shown: (Flags, Option<i64>),
    reason: Option<&str>,
    actor: u64,
) -> Vec<Embed> {
    result_embeds_with(&ModTexts::default(), kind, done, shown, reason, actor)
}

/// [`result_embeds`] with the server's edited version of the result (`reply.mod.result`).
pub fn result_embeds_with(
    t: &ModTexts,
    kind: &str,
    done: &[Applied],
    (flags, duration_s): (Flags, Option<i64>),
    reason: Option<&str>,
    actor: u64,
) -> Vec<Embed> {
    let (title, plural, all) = labels(kind);
    let (ok, failed): (Vec<_>, Vec<_>) = done.iter().partition(|a| a.case.is_some());
    // Nothing worked: the title says so (owner decision 11-15).
    let head = if ok.is_empty() && !failed.is_empty() {
        format!("{title} failed:")
    } else {
        format!("{title} result:")
    };
    let reason_text = reason.unwrap_or("No reason.");
    // The three blocks of lines under Reason and Moderator, kept apart so an edited version can
    // place each.
    let mut details = Vec::new();
    let lines = details_lines(kind, flags, duration_s, done);
    if !lines.is_empty() {
        details.push(format!("{DETAILS} **Details:**"));
        details.extend(lines.into_iter().map(|(k, v)| format!("{ARROW} {k}: {v}")));
    }
    let mut good = vec![format!("{YES} **Successful {plural}**")];
    for a in &ok {
        let first = a.text.lines().next().unwrap_or_default();
        good.push(match a.target {
            Some(id) => format!("{ARROW} <@{id}> [`{id}`]"),
            // Purge and slowmode have no member: show what happened.
            None => format!("{ARROW} {}", first.split_once(": ").map_or(first, |c| c.1)),
        });
        // Warn threshold notes; the embed already shows moderator and reason.
        good.extend(
            a.text
                .lines()
                .skip(1)
                .filter(|l| {
                    !["Moderator:", "Reason:", "Add Reason"]
                        .iter()
                        .any(|p| l.starts_with(p))
                })
                .map(String::from),
        );
    }
    let mut bad = vec![format!("{UNSET} **Unsuccessful {plural}**")];
    if failed.is_empty() {
        bad.push(all.into());
    }
    bad.extend(failed.iter().map(|a| format!("{ARROW} {}", a.text)));
    let by = format!("<@{actor}>");
    let (details, good, bad) = (details.join("\n"), good.join("\n"), bad.join("\n"));
    let vars = [
        ("title", head.as_str()),
        ("reason", reason_text),
        ("moderator", by.as_str()),
        ("details", details.as_str()),
        ("successful", good.as_str()),
        ("unsuccessful", bad.as_str()),
    ];
    let mut out = match t.say("reply.mod.result", &vars) {
        Some((_, e)) if !e.is_empty() => e,
        _ => {
            let mut result = Log::new(&head, BLUE)
                .text(&format!("{REASON} **Reason:** {reason_text}"))
                .text(&format!("{MODERATOR} **Moderator:** {by}"));
            for block in [&details, &good, &bad]
                .into_iter()
                .filter(|b| !b.is_empty())
            {
                for l in block.split('\n') {
                    result = result.text(l);
                }
            }
            vec![result.build()]
        }
    };
    if reason.is_none() && !ok.is_empty() {
        let mut add = Log::new("Add Reason", GREEN);
        for n in ok.iter().filter_map(|a| a.case) {
            add = add.text(&format!("{ARROW} `/cases modify (Case: {n})`"));
        }
        out.push(add.build());
    }
    // An edited result may hold up to 10 embeds: Discord takes no more in one message.
    out.truncate(10);
    out
}

/// How long a punishment waits for its DM before it goes ahead anyway.
pub const DM_WAIT: Duration = Duration::from_secs(3);

/// Runs the DM and the action in order. Punishments (`dm_first`) DM first, so a kicked or banned
/// member can still be reached, and act whatever the DM did. Removals act first and only DM once
/// the action worked. A DM that takes longer than [`DM_WAIT`] counts as failed.
pub async fn dm_and_act<T>(
    dm_first: bool,
    dm: Option<impl Future<Output = bool>>,
    act: impl Future<Output = Result<T>>,
) -> (DmOutcome, Result<T>) {
    let send = async move {
        match dm {
            None => DmOutcome::Off,
            Some(f) => match tokio::time::timeout(DM_WAIT, f).await {
                Ok(true) => DmOutcome::Sent,
                _ => DmOutcome::Failed,
            },
        }
    };
    if dm_first {
        let told = send.await;
        (told, act.await)
    } else {
        let done = act.await;
        let told = match done {
            Ok(_) => send.await,
            Err(_) => DmOutcome::Off,
        };
        (told, done)
    }
}

/// A punishment DM goes out when the server's switch is on or the moderator added `?dm`.
pub fn wants_dm(forced: bool, misc: &brim_core::sections::MiscCfg) -> bool {
    forced || misc.always_dm
}

/// Punishments DM before they act; their removals act first.
pub fn dm_first(kind: &str) -> bool {
    !matches!(kind, "unquarantine" | "untimeout" | "unban")
}

/// The key of a punishment DM template; an unknown kind is a ban, as before.
fn punish_key(kind: &str) -> String {
    let k = match kind {
        "warn" | "timeout" | "quarantine" | "kick" | "softban" | "tempban" | "unquarantine"
        | "untimeout" | "unban" => kind,
        _ => "ban",
    };
    format!("dm.punish.{k}")
}

/// The punishment DM's variables.
fn punish_vars<'a>(
    server: &'a str,
    reason: Option<&'a str>,
    moderator: Option<&'a str>,
    duration: &'a str,
    staff_line: &'a str,
    moderator_code: &'a str,
    appeal: &'a str,
) -> Vec<(&'static str, &'a str)> {
    vec![
        ("server", server),
        ("reason", reason.unwrap_or("No reason.")),
        ("moderator", moderator.unwrap_or("")),
        ("moderator_code", moderator_code),
        ("duration", duration),
        ("staff_line", staff_line),
        ("appeal", appeal),
    ]
}

/// A value in inline code style, or nothing for an empty one.
fn code(v: Option<&str>) -> String {
    v.filter(|v| !v.is_empty())
        .map_or(String::new(), |v| format!("`{}`", v.replace('`', "'")))
}

fn punish_duration(kind: &str, duration_s: Option<i64>) -> String {
    duration_s
        .filter(|_| matches!(kind, "timeout" | "tempban"))
        .map_or(String::new(), |d| format!("`{}`", duration(d)))
}

/// "You have been banned in Server!" with Reason, Moderator (None hides staff) and Duration, from
/// the default template.
pub fn dm_embed(
    kind: &str,
    server: &str,
    reason: Option<&str>,
    moderator: Option<&str>,
    duration_s: Option<i64>,
) -> Embed {
    let d = punish_duration(kind, duration_s);
    let m = botmsg::render(
        &punish_key(kind),
        None,
        &punish_vars(server, reason, moderator, &d, "", &code(moderator), ""),
    );
    parts(&m).1.remove(0)
}

pub(crate) use brim_logging::overlay::{bot_embed, parts};

/// A command answer that is its own entry of facts (plan D6, `botmsg::ANSWERS`): the server's
/// edited version of `key` or its default, filled with `vars`. A placeholder the answer did not
/// set is filled empty, so its line goes and no raw `{name}` reaches Discord.
pub async fn answer(app: &App, gid: Id<GuildMarker>, key: &str, vars: &[(&str, String)]) -> Embed {
    answer_from(key, stored_msg(app, gid, key).await.as_ref(), vars)
}

/// The pure part of [`answer`].
pub(crate) fn answer_from(key: &str, stored: Option<&Value>, vars: &[(&str, String)]) -> Embed {
    let d = botmsg::find(key).expect("known answer key");
    debug_assert!(
        vars.iter().all(|(k, _)| d.vars.contains(k)),
        "{key} has no such placeholder"
    );
    let all: Vec<(&str, &str)> = d
        .vars
        .iter()
        .map(|v| {
            let set = vars.iter().find(|(k, _)| k == v);
            (*v, set.map_or("", |(_, x)| x.as_str()))
        })
        .collect();
    bot_embed(key, stored, &all)
}

/// [`bot_embed`] with the server's copy read from the database.
pub async fn bot_embed_for(
    app: &App,
    gid: Id<GuildMarker>,
    key: &str,
    vars: &[(&str, &str)],
) -> Embed {
    bot_embed(key, stored_msg(app, gid, key).await.as_ref(), vars)
}

/// The embeds of a `{text}` frame message, for senders that build the request themselves: the
/// bot-wide copy when one is saved (an outside-server entry), else the default.
pub async fn frame_embeds(app: &App, key: &str, text: &str) -> Vec<Embed> {
    let stored = stored_row(app, 0, key).await;
    parts(&botmsg::render(key, stored.as_ref(), &[("text", text)])).1
}

tokio::task_local! {
    /// The command word of the prefix message being answered (set in `events`), so the reply
    /// helpers can find the command's frame.
    pub static WORD: (String, Id<GuildMarker>);
}

/// The Bot messages key of a command word: `cmd.<name>`. `mod_first` tries the moderation aliases
/// before the general ones (`r` is unquarantine there, reply in tickets).
pub fn command_key(word: &str, mod_first: bool) -> Option<String> {
    let w = word.to_lowercase();
    let ticket = (!mod_first).then(|| ticket_alias(&w)).flatten();
    let general = crate::general::lookup(&w).map(|c| c.0);
    let moderation = mod_first.then(|| crate::prefix::alias(&w).0);
    let names = if mod_first {
        [moderation, general, None]
    } else {
        [ticket, general, None]
    };
    let key = names
        .into_iter()
        .flatten()
        .map(|c| format!("cmd.{c}"))
        .find(|k| botmsg::find(k).is_some());
    key
}

/// Ticket command aliases that are not in the general list (`r` is unquarantine for moderators).
fn ticket_alias(w: &str) -> Option<&'static str> {
    Some(match w {
        "r" | "ar" | "areply" => "reply",
        "c" | "ac" | "aclose" => "close",
        "acloseall" => "closeall",
        "block" => "blacklist",
        "s" | "as" | "asnippet" => "snippet",
        "viewsnippets" | "snippetlist" | "vs" => "viewsnippet",
        "tnote" => "ticketnote",
        "mentionrole" => "pingrole",
        "logs" => "logging",
        "commandrequired" => "commandonly",
        "welcomemessage" | "greetmessage" => "greetingmessage",
        "goodbyemessage" | "closemessage" => "closingmessage",
        "advancedlogging" | "advancedlogs" => "loggingplus",
        _ => return None,
    })
}

/// The reply of the command `key` with the server's saved frame laid over it, or None when the
/// server has none (or the command is locked): the caller then sends the reply as built. Plain
/// text becomes one embed. Buttons and files stay the caller's.
pub async fn framed_reply(
    app: &App,
    gid: Id<GuildMarker>,
    key: &str,
    text: &str,
    embeds: &[Embed],
) -> Option<(String, Vec<Embed>)> {
    if botmsg::find(key).is_none_or(|d| d.locked) || (text.is_empty() && embeds.is_empty()) {
        return None;
    }
    let stored = stored_msg(app, gid, key).await?;
    lay(key, &stored, text, embeds)
}

/// The pure part of [`framed_reply`].
fn lay(key: &str, stored: &Value, text: &str, embeds: &[Embed]) -> Option<(String, Vec<Embed>)> {
    if embeds.is_empty() {
        let plain: Embed =
            serde_json::from_value(json!({"type": "rich", "description": text})).ok()?;
        return Some((
            String::new(),
            vec![crate::events::frame_over(key, stored, &plain)],
        ));
    }
    // The frame goes on the first embed only: its title and fields would repeat on every one.
    let mut laid = embeds.to_vec();
    laid[0] = crate::events::frame_over(key, stored, &laid[0]);
    let size = |e: &Embed| serde_json::to_string(e).map_or(0, |s| s.chars().count());
    // Discord counts all embeds of a message together.
    if laid.iter().map(size).sum::<usize>() > 5500 {
        return None;
    }
    Some((text.into(), laid))
}

/// [`framed_reply`] for the prefix command being answered (see [`WORD`]).
pub async fn command_reply(
    app: &App,
    mod_first: bool,
    text: &str,
    embeds: &[Embed],
) -> Option<(String, Vec<Embed>)> {
    let (word, gid) = WORD.try_with(Clone::clone).ok()?;
    let key = command_key(&word, mod_first)?;
    framed_reply(app, gid, &key, text, embeds).await
}

/// A short reply built from its entry (`reply.<area>.<name>`).
pub struct Said {
    pub text: String,
    pub embeds: Vec<Embed>,
    /// The server saved its own edited version: it is sent as it is. Without one, a command's
    /// reply still goes through that command's `cmd.<command>` frame, like the code-built text.
    pub edited: bool,
    /// A security text (group Security): never framed by `cmd.<command>`, which is not a security
    /// text, so a Trusted Admin could hide its facts in a spoiler there.
    pub security: bool,
}

/// The one way to send a short reply: the server's edited version of entry `key` or its default,
/// with `vars` filled. Channel sends, interaction replies and followups all send what it returns.
pub async fn say(app: &App, gid: Id<GuildMarker>, key: &str, vars: &[(&str, &str)]) -> Said {
    said_from(key, stored_msg(app, gid, key).await, vars)
}

/// [`say`] for a bot-wide entry (`botmsg::BOT_WIDE`, sent where no server is known): the one copy
/// under guild 0, edited by the bot owner.
pub async fn say_wide(app: &App, key: &str, vars: &[(&str, &str)]) -> Said {
    said_from(key, stored_row(app, 0, key).await, vars)
}

/// [`say_line`] for a bot-wide entry.
pub async fn say_wide_line(app: &App, key: &str, vars: &[(&str, &str)]) -> String {
    let text = say_wide(app, key, vars).await.text;
    if text.is_empty() {
        return parts(&botmsg::render(key, None, vars)).0;
    }
    text
}

pub(crate) fn said_from(key: &str, stored: Option<Value>, vars: &[(&str, &str)]) -> Said {
    let m = botmsg::render(key, stored.as_ref(), vars);
    // A security text whose edit lost a fact sends the default: then it is not an edit.
    let edited = stored.as_ref().is_some_and(|s| botmsg::uses_stored(key, s))
        && m != botmsg::render(key, None, vars);
    let (text, embeds) = parts(&m);
    Said {
        text,
        embeds,
        edited,
        security: botmsg::find(key).is_some_and(|d| d.security),
    }
}

impl From<String> for Said {
    /// Text the code built (not an entry): sent as it is, through the command frame.
    fn from(text: String) -> Self {
        Self {
            text,
            embeds: vec![],
            edited: false,
            security: false,
        }
    }
}

/// What a command parser answers for a command it does not know.
pub const UNKNOWN_COMMAND: &str = "Unknown command";

/// The reply for an error text a command parser returned: [`UNKNOWN_COMMAND`] has its own entry,
/// every other text goes in the wrong use entry as `{text}`.
pub async fn say_wrong(app: &App, gid: Id<GuildMarker>, text: &str) -> Said {
    match text {
        UNKNOWN_COMMAND => say(app, gid, "reply.error.unknown_command", &[]).await,
        t => say(app, gid, "reply.error.wrong_use", &[("text", t)]).await,
    }
}

/// [`say`] for a sentence that goes inside a bigger reply (a result line): only its text. An
/// edited version with no text gives the default sentence, so the line is never lost.
pub async fn say_line(app: &App, gid: Id<GuildMarker>, key: &str, vars: &[(&str, &str)]) -> String {
    let said = say(app, gid, key, vars).await;
    if said.text.is_empty() {
        return parts(&botmsg::render(key, None, vars)).0;
    }
    said.text
}

/// The server's own copy of a built-in message, if it saved one.
pub async fn stored_msg(app: &App, gid: Id<GuildMarker>, key: &str) -> Option<Value> {
    stored_row(app, gid.get() as i64, key).await
}

/// The copy saved under `guild` (0 for a bot-wide entry) for `key`.
pub async fn stored_row(app: &App, guild: i64, key: &str) -> Option<Value> {
    sqlx::query_scalar("SELECT message FROM bot_messages WHERE guild_id = $1 AND key = $2")
        .bind(guild)
        .bind(key)
        .fetch_optional(&app.pool)
        .await
        .ok()
        .flatten()
}

/// DMs `user` the built-in message `key`: the server's copy or the default, with `vars` filled.
/// `gid` None (a job with no server) always sends the default. Same limits as [`send_dm`].
pub async fn dm_msg(
    app: &App,
    gid: Option<Id<GuildMarker>>,
    user: Id<UserMarker>,
    key: &str,
    vars: &[(&str, &str)],
    rows: &[Component],
) -> bool {
    let stored = match gid {
        Some(g) if botmsg::find(key).is_some_and(|d| !d.locked) => {
            stored_row(app, botmsg::copy_guild(key, g.get() as i64), key).await
        }
        None if botmsg::BOT_WIDE.contains(&key) => stored_row(app, 0, key).await,
        _ => None,
    };
    let (text, embeds) = parts(&botmsg::render(key, stored.as_ref(), vars));
    send_dm(app, user, &text, &embeds, rows).await
}

/// The line a quarantine DM without a reason gets while tickets are on: tickets stay open to
/// quarantined members, so they can ask why.
fn reach_staff(kind: &str, reason: Option<&str>, tickets: bool) -> Option<&'static str> {
    (kind == "quarantine" && reason.is_none() && tickets)
        .then_some("You can DM Brim to reach this server's staff.")
}

/// An automatic quarantine (anti-nuke, a rate limit, the Raider List, a rejoin) tells the member
/// how to reach staff, while tickets are on. Queued as a job, so the DM never holds back the
/// quarantine or the revert after it; a raid wave queues none, so the bot stays off DM limits.
/// Callers skip join and verification quarantines, which get the verification prompt.
pub(crate) async fn queue_quarantine_dm(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) {
    let r = async {
        let cfg = app.cfg(gid.get()).await?;
        let now = crate::automod::now_ms();
        let voice_raid = app
            .voice_raids
            .lock()
            .unwrap()
            .get(&gid.get())
            .is_some_and(|r| r.active(now));
        // New tickets are refused during a raid or lock, so the DM would promise nothing.
        if !cfg.tickets.enabled || voice_raid || crate::lockdown::raid_or_lock(app, gid).await? {
            return anyhow::Ok(());
        }
        // One DM per member and server a day: the marker job runs (and ends) after 24 h.
        let g = gid.get() as i64;
        if jobs::of_user(&app.pool, g, QUARANTINE_DM_COOLDOWN, user.get())
            .await?
            .is_some()
        {
            return Ok(());
        }
        let job = json!({ "user": user.get() });
        jobs::enqueue(
            &app.pool,
            g,
            QUARANTINE_DM_COOLDOWN,
            job.clone(),
            now_secs() + 86_400,
        )
        .await?;
        jobs::enqueue(&app.pool, g, QUARANTINE_DM, job, now_secs()).await?;
        Ok(())
    };
    if let Err(e) = r.await {
        tracing::warn!(?e, "queueing the quarantine DM failed");
    }
}

/// Marks that a member got the quarantine DM in the last 24 hours; its run does nothing.
pub(crate) const QUARANTINE_DM_COOLDOWN: &str = "quarantine_dm_cooldown";
pub(crate) const QUARANTINE_DM: &str = "quarantine_dm";

/// The `quarantine_dm` job: only a member still in a real quarantine, and never a bot. A failed
/// DM is only logged, never retried.
pub(crate) async fn quarantine_dm_job(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let user = Id::new(p["user"].as_u64().context("job payload has no user")?);
    if crate::antinuke::quarantine_kind(app, gid, user).await? != Some(false)
        || crate::antinuke::is_bot(app, user).await
    {
        return Ok(());
    }
    let server = crate::verify::guild_name(app, gid).await;
    if !dm_msg(
        app,
        Some(gid),
        user,
        "dm.quarantine_notice",
        &[("server", &server)],
        &[],
    )
    .await
    {
        tracing::warn!(user = user.get(), "quarantine DM failed");
    }
    Ok(())
}

/// What a punishment DM says, see [`dm_embed`]. `moderator` is None when staff is hidden.
pub struct Notice<'a> {
    pub kind: &'a str,
    pub reason: Option<&'a str>,
    pub duration_s: Option<i64>,
    pub moderator: Option<Id<UserMarker>>,
    /// The appeal line under ban and timeout DMs.
    pub appeal: Option<String>,
}

/// Sends the punishment DM; true when it arrived.
pub async fn tell(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>, n: Notice<'_>) -> bool {
    let server = crate::verify::guild_name(app, gid).await;
    let moderator = match n.moderator {
        Some(m) => Some(match app.http.user(m).await {
            Ok(r) => r.model().await.map_or(format!("<@{m}>"), |u| u.name),
            Err(_) => format!("<@{m}>"),
        }),
        None => None,
    };
    let tickets = app.cfg(gid.get()).await.is_ok_and(|c| c.tickets.enabled);
    let staff_line = reach_staff(n.kind, n.reason, tickets).unwrap_or_default();
    let d = punish_duration(n.kind, n.duration_s);
    let key = punish_key(n.kind);
    let stored = stored_msg(app, gid, &key).await;
    // The appeal note (the server's message and the link) is a line of the embed, and the link
    // is also a button, for clients where one of them does not show.
    let appeal = n.appeal.as_deref().unwrap_or_default();
    let url = appeal
        .rsplit_once(' ')
        .map(|(_, u)| u)
        .filter(|u| u.starts_with("http"));
    let mc = code(moderator.as_deref());
    let vars = punish_vars(
        &server,
        n.reason,
        moderator.as_deref(),
        &d,
        staff_line,
        &mc,
        appeal,
    );
    let (text, embeds) = parts(&botmsg::render(&key, stored.as_ref(), &vars));
    let rows: Vec<Component> = url
        .map(|u| crate::verify::row(crate::welcome::link("Appeal", u.into())))
        .into_iter()
        .collect();
    send_dm(app, user, &text, &embeds, &rows).await
}

async fn send_dm(
    app: &App,
    user: Id<UserMarker>,
    text: &str,
    embeds: &[Embed],
    rows: &[Component],
) -> bool {
    let send = async {
        let ch = best_effort(app.http.create_private_channel(user).await, "open DM")?;
        let ch = ch.model().await.ok()?;
        best_effort(
            app.http
                .create_message(ch.id)
                .content(text)
                .embeds(embeds)
                .components(rows)
                .await,
            "send DM",
        )
    };
    match tokio::time::timeout(DM_WAIT, send).await {
        Ok(sent) => sent.is_some(),
        Err(_) => {
            // Rate limits during an attack can hold a DM this long; owner alerts must not vanish unseen.
            tracing::warn!(user = user.get(), "DM gave up after {DM_WAIT:?}");
            false
        }
    }
}

/// DMs the server owner that an attack started, so they hear about it even when nobody watches
/// the log channel. At most one DM per server per 10 minutes. A stopped nuke's report DM has its
/// own limit (`antinuke::dm_report`) and takes this slot too, so panic mode right after a nuke
/// sends no second DM, as before.
pub(crate) static ALERTED: Mutex<BTreeMap<u64, i64>> = Mutex::new(BTreeMap::new());

/// `kind`: the log the details went to, which the DM links.
pub async fn alert_owner(app: &App, gid: Id<GuildMarker>, what: &str, kind: LogKind) {
    if !alert_due(&mut ALERTED.lock().unwrap(), gid.get(), now_secs(), 600) {
        return;
    }
    let logs = app
        .cfg(gid.get())
        .await
        .ok()
        .and_then(|c| c.logging.target(kind, None))
        .map_or(String::new(), |ch| {
            format!("\nDetails: https://discord.com/channels/{gid}/{ch}")
        });
    let text = format!("{PANIC} **{what}** on your server. Brim is handling it.{logs}");
    dm_owner(app, gid, "dm.security", &text).await;
}

/// Tells the owner that settings could not be read and run on their defaults (a newer version
/// saved them, or they are damaged). Once a day per server.
pub async fn fallback_alert(
    app: &App,
    gid: Id<GuildMarker>,
    sections: &[&str],
    main_roles: &[Id<twilight_model::id::marker::RoleMarker>],
) {
    static LAST: Mutex<BTreeMap<u64, i64>> = Mutex::new(BTreeMap::new());
    if !alert_due(&mut LAST.lock().unwrap(), gid.get(), now_secs(), 86_400) {
        return;
    }
    let list = sections.join(", ");
    let name = md(&crate::verify::guild_name(app, gid).await);
    let text = format!(
        "{WARNING} Brim could not read these settings of **{name}** and uses their defaults until they are saved again: {list}. Defaults are off for most systems, Anti-nuke too. This happens after a rollback to an older Brim version; update it again, or open the dashboard and save these settings."
    );
    dm_owner(app, gid, "dm.security", &text).await;
    // Also in the log, so it is seen with the owner's DMs closed (S-h3 6). Read from the raw
    // config: the logging section may be one that failed.
    let ch: Option<Option<String>> = sqlx::query_scalar(
        "SELECT coalesce(config->'logging'->>'config_changes', config->'logging'->>'fallback')
         FROM guild_config WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(?e, "settings-not-read log: config lookup failed");
        None
    });
    let Some(ch) = ch
        .flatten()
        .and_then(|c| c.parse().ok())
        .and_then(Id::new_checked)
    else {
        return;
    };
    let log = Log::new("Settings Not Read", brim_logging::embed::RED)
        .line("Sections", &list)
        .line(
            "Effect",
            "Brim runs them on their defaults (off for most systems, Anti-nuke too) until they are saved again or Brim is updated.",
        )
        .time()
        .build();
    // Straight to Discord, not events::send: that reads the config, and this runs inside App::cfg.
    // Only to a channel @everyone cannot see: "Anti-nuke is off" helps a raider (R-h3 round 2, 4).
    // Without readable main roles the check sees only @everyone, so a members-only channel
    // would pass as private (R-h3 round 3, 1).
    if sections.contains(&"lockdown") {
        tracing::warn!(%gid, "settings-not-read log skipped: main roles unknown");
        return;
    }
    let private = async {
        let c = app.http.channel(ch).await?.model().await?;
        // The id comes from raw config that may be damaged: only a channel of this server.
        if c.guild_id != Some(gid) {
            return anyhow::Ok(false);
        }
        crate::events::public_with(app, c, main_roles)
            .await
            .map(|p| !p)
    };
    match private.await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(%gid, "settings-not-read log skipped: @everyone can read the channel");
            return;
        }
        Err(e) => {
            tracing::warn!(?e, "settings-not-read log: visibility check failed");
            return;
        }
    }
    if let Err(e) = app.http.create_message(ch).embeds(&[log]).await {
        tracing::warn!(?e, "settings-not-read log not sent");
    }
}

/// DMs the server owner right away, without [`alert_owner`]'s 10-minute limit.
pub async fn dm_owner(app: &App, gid: Id<GuildMarker>, key: &str, text: &str) {
    // The bot owner: after a rescue claim the Discord owner account may be in someone else's hands.
    match app.bot_owner(gid).await {
        Ok(owner) => {
            dm_msg(app, Some(gid), owner, key, &[("text", text)], &[]).await;
        }
        Err(e) => tracing::warn!(?e, "owner DM: guild lookup failed"),
    }
}

/// Whether `key` (a guild or channel) is due another alert, at most one per `every` seconds.
pub(crate) fn alert_due(last: &mut BTreeMap<u64, i64>, key: u64, now: i64, every: i64) -> bool {
    if last.get(&key).is_some_and(|t| now - t < every) {
        return false;
    }
    last.insert(key, now);
    true
}

/// Queued as a job so it survives restarts and never blocks the handler.
pub async fn delete_later(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
    msg: Id<MessageMarker>,
    secs: u32,
) -> Result<()> {
    let job = json!({"channel": channel.get(), "message": msg.get()});
    let at = now_secs() + i64::from(secs);
    jobs::enqueue(&app.pool, gid.get() as i64, "delete_message", job, at).await?;
    Ok(())
}

/// Scheduler arm for `delete_message`; the scheduler drops the job when the message is already gone.
pub async fn run_delete(app: &App, payload: &Value) -> Result<()> {
    let id = |k: &str| payload[k].as_u64().with_context(|| format!("no {k}"));
    app.http
        .delete_message(Id::new(id("channel")?), Id::new(id("message")?))
        .await?;
    Ok(())
}

/// Bulk delete `user`'s messages among the channel's last 100. Discord refuses messages older than 14 days.
pub async fn purge_user(app: &App, channel: Id<ChannelMarker>, user: Id<UserMarker>) -> Result<()> {
    let msgs = app.http.channel_messages(channel).limit(100).await?;
    let cutoff = now_secs() - 14 * 86_400 + 60;
    let ids: Vec<_> = msgs
        .models()
        .await?
        .into_iter()
        .filter(|m| m.author.id == user && m.timestamp.as_secs() > cutoff)
        .map(|m| m.id)
        .collect();
    delete_batch(app, channel, &ids).await
}

/// Deletes tracked (channel, message) pairs, one bulk delete per channel; too old ones are skipped.
pub async fn purge_ids(app: &App, msgs: &[(u64, u64)]) {
    for (c, ids) in batches(msgs, now_secs()) {
        if let Err(e) = delete_batch(app, Id::new(c), &ids).await {
            tracing::warn!(?e, channel = c, "purging tracked messages failed");
        }
    }
}

/// Groups messages younger than 14 days by channel, at most 100 per group (Discord's bulk delete cap).
fn batches(msgs: &[(u64, u64)], now: i64) -> Vec<(u64, Vec<Id<MessageMarker>>)> {
    let cutoff_ms = (now - 14 * 86_400 + 60) * 1000;
    let mut by_channel: HashMap<u64, Vec<Id<MessageMarker>>> = HashMap::new();
    for &(c, m) in msgs {
        // Snowflake timestamp: ms since the Discord epoch in the top bits.
        if ((m >> 22) + 1_420_070_400_000) as i64 > cutoff_ms {
            by_channel.entry(c).or_default().push(Id::new(m));
        }
    }
    by_channel
        .into_iter()
        .flat_map(|(c, ids)| {
            ids.chunks(100)
                .map(|ch| (c, ch.to_vec()))
                .collect::<Vec<_>>()
        })
        .collect()
}

async fn delete_batch(
    app: &App,
    channel: Id<ChannelMarker>,
    ids: &[Id<MessageMarker>],
) -> Result<()> {
    match ids {
        [] => {}
        // Already deleted (by the author or another moderator) is fine.
        [one] => match app.http.delete_message(channel, *one).await {
            Err(e) if !gone(&e) => return Err(e.into()),
            _ => {}
        },
        _ => {
            app.http.delete_messages(channel, ids).await?;
        }
    }
    Ok(())
}

/// User text for an embed line: one line, markdown and mentions shown as typed.
pub(crate) fn md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | '\r' => out.push(' '),
            '\\' | '[' | ']' | '*' | '_' | '~' | '|' | '>' | '<' | '`' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// Defers an ephemeral answer, runs the click, then sends its text. `None` from the click means
/// the button does not work where it was pressed.
pub(crate) async fn answer_click(
    app: &App,
    i: &twilight_model::application::interaction::Interaction,
    clicked: impl std::future::Future<Output = Option<Result<String>>>,
) -> Result<()> {
    let client = app.http.interaction(Id::new(app.me.get()));
    let defer = crate::verify::ephemeral(
        twilight_model::http::interaction::InteractionResponseType::DeferredChannelMessageWithSource,
        None,
    );
    client.create_response(i.id, &i.token, &defer).await?;
    let done = (clicked.await).unwrap_or_else(|| Ok("This button does not work here.".into()));
    let text = done.unwrap_or_else(|e| {
        tracing::warn!(?e, "button failed");
        format!("Something went wrong: {}", crate::http_err::why(&e))
    });
    client
        .create_followup(&i.token)
        .content(&text)
        .flags(twilight_model::channel::message::MessageFlags::EPHEMERAL)
        .allowed_mentions(Some(
            &twilight_model::channel::message::AllowedMentions::default(),
        ))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_replies_find_their_frame_and_wrap_plain_text() {
        assert_eq!(command_key("B", true).as_deref(), Some("cmd.ban"));
        assert_eq!(command_key("sweep", true).as_deref(), Some("cmd.purge"));
        assert_eq!(command_key("db", false).as_deref(), Some("cmd.dashboard"));
        assert_eq!(command_key("nothing", true), None);
        assert_eq!(command_key("r", false).as_deref(), Some("cmd.reply"));
        assert_eq!(command_key("c", false).as_deref(), Some("cmd.close"));
        assert_eq!(command_key("c", true).as_deref(), Some("cmd.purge"));
        let stored = json!({"embeds": [{"title": "{title}", "description": "{text}\nAsk staff.", "color": 7}]});
        let (text, e) = lay("cmd.ban", &stored, "Case #1: ban", &[]).unwrap();
        assert_eq!(text, "");
        assert_eq!(
            e[0].description.as_deref(),
            Some("Case #1: ban\nAsk staff.")
        );
        assert_eq!(e[0].color, Some(7));
        let (text, e) = lay("cmd.ban", &stored, "kept", &e).unwrap();
        assert_eq!((text.as_str(), e.len()), ("kept", 1));
    }

    use super::*;

    #[test]
    fn case_text_forms() {
        assert_eq!(
            case_text(true, (3, "ban", 5), Some("x"), 9),
            "Case #3: ban <@5>"
        );
        assert_eq!(
            case_text(false, (3, "ban", 5), Some("x"), 9),
            "Case #3: ban <@5>\nModerator: <@9>\nReason: x"
        );
        assert!(case_text(true, (3, "ban", 5), None, 9)
            .ends_with("\nAdd Reason: `/cases modify case:3 reason:`"));
    }

    fn ok(case: i32, target: u64, dm: DmOutcome) -> Applied {
        Applied {
            target: Some(target),
            case: Some(case),
            text: format!("Case #{case}: kick <@{target}>"),
            dm,
            posted: false,
        }
    }

    fn refused(target: u64) -> Applied {
        Applied {
            target: Some(target),
            ..format!("Cannot kick <@{target}>: they are immune").into()
        }
    }

    fn desc(e: &[Embed]) -> String {
        e[0].description.clone().unwrap()
    }

    #[test]
    fn compact_summary_counts() {
        let t = [ok(1, 5, DmOutcome::Off), refused(6)];
        assert_eq!(
            summary("kick", true, &t, Some("spam"), 9),
            "✅ Successful kicks: `1`\n❌ Unsuccessful kicks: `1`\n📄 Reason: spam\n👤 Moderator: <@9>\nCannot kick <@6>: they are immune"
        );
        assert_eq!(
            summary("kick", false, &t, None, 9),
            "Case #1: kick <@5>\nCannot kick <@6>: they are immune"
        );
        assert_eq!(
            summary("kick", true, &t[..1], None, 9),
            "Case #1: kick <@5>"
        );
    }

    #[test]
    fn result_lists_successful_and_unsuccessful() {
        let t = [ok(4, 5, DmOutcome::Sent), refused(6)];
        let e = result_embeds("timeout", &t, (Flags::default(), Some(300)), None, 9);
        assert_eq!(e[0].title.as_deref(), Some("Timeout result:"));
        // Nothing worked: the title says so.
        let none = result_embeds("warn", &t[1..], (Flags::default(), None), None, 9);
        assert_eq!(none[0].title.as_deref(), Some("Warn failed:"));
        let d = desc(&e);
        let succ = d.find("Successful timeouts").unwrap();
        let fail = d.find("Unsuccessful timeouts").unwrap();
        assert!(d.contains("No reason.") && d.contains("Moderator:** <@9>"));
        assert!(d.find("Details:").unwrap() < succ && succ < fail);
        assert!(d.contains("Duration: `5m` ✅") && d.contains("DM-Members: ✅"));
        assert!(d[succ..fail].contains("<@5> [`5`]"));
        assert!(d[fail..].contains("Cannot kick <@6>"));
        assert!(!d.contains("All members"));
        // Add Reason lists only the cases that were made.
        assert_eq!(e.len(), 2);
        assert!(desc(&e[1..]).contains("(Case: 4)"));
        let one = result_embeds("timeout", &t, (Flags::default(), Some(300)), Some("x"), 9);
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn nothing_failed_says_so() {
        let t = [ok(1, 5, DmOutcome::Off)];
        for (kind, all) in [
            ("ban", "All members were banned!"),
            ("kick", "All members were kicked!"),
            ("warn", "All members were warned!"),
            ("timeout", "All members were timed out!"),
            ("quarantine", "All members were quarantined!"),
            ("softban", "All members were softbanned!"),
            ("unban", "All members were unbanned!"),
        ] {
            assert!(
                desc(&result_embeds(kind, &t, Default::default(), Some("r"), 9)).contains(all),
                "{kind}"
            );
        }
    }

    fn flags(
        kind: &str,
        f: Flags,
        dur: Option<i64>,
        done: &[Applied],
    ) -> Vec<(&'static str, String)> {
        details_lines(kind, f, dur, done)
    }

    #[test]
    fn details_per_action() {
        let none = [ok(1, 5, DmOutcome::Off)];
        let names =
            |v: Vec<(&'static str, String)>| v.into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        assert_eq!(
            names(flags("ban", Flags::default(), None, &none)),
            ["Duration", "Soft Ban", "Hack Ban", "DM-Members"]
        );
        assert_eq!(
            names(flags("kick", Flags::default(), None, &none)),
            ["No-Avatar", "Voice-Channels", "DM-Members"]
        );
        assert_eq!(
            names(flags("timeout", Flags::default(), Some(60), &none)),
            ["Duration", "DM-Members"]
        );
        for k in ["warn", "quarantine", "unquarantine", "untimeout", "unban"] {
            assert_eq!(
                names(flags(k, Flags::default(), None, &none)),
                ["DM-Members"],
                "{k}"
            );
        }
        assert!(flags("purge", Flags::default(), None, &none).is_empty());
        // Each flag says whether it was used.
        let f = Flags {
            hack: true,
            no_avatar: true,
            ..Default::default()
        };
        assert_eq!(
            flags("softban", f, Some(86_400), &none),
            [
                ("Duration", "❎".to_string()),
                ("Soft Ban", "✅".into()),
                ("Hack Ban", "✅".into()),
                ("DM-Members", "❎".into())
            ]
        );
        assert_eq!(
            flags("kick", f, None, &none)[..2],
            [
                ("No-Avatar", "✅".to_string()),
                ("Voice-Channels", "❎".into())
            ]
        );
        assert_eq!(flags("ban", Flags::default(), None, &none)[0].1, "❎");
    }

    #[test]
    fn dm_members_tells_the_truth() {
        let dm = |d: &[DmOutcome]| {
            let done: Vec<_> = d.iter().map(|&o| ok(1, 5, o)).collect();
            dm_flag(&done)
        };
        assert_eq!(dm(&[DmOutcome::Sent, DmOutcome::Sent]), "✅");
        assert_eq!(dm(&[DmOutcome::Off]), "❎");
        assert_eq!(dm(&[DmOutcome::Failed]), "❎");
        assert_eq!(
            dm(&[DmOutcome::Sent, DmOutcome::Sent, DmOutcome::Failed]),
            "✅ 2 | ❎ 1"
        );
    }

    #[test]
    fn other_commands_get_result_embeds() {
        let t = [Applied {
            case: Some(5),
            text: "Case #5: Deleted 7 messages".into(),
            ..Default::default()
        }];
        let d = desc(&result_embeds(
            "purge",
            &t,
            Default::default(),
            Some("x"),
            9,
        ));
        assert!(d.contains("Successful purges") && d.contains("Deleted 7 messages"));
        assert!(!d.contains("Details:"));
        let t = [Applied {
            target: Some(5),
            case: Some(6),
            text: "Case #6: Renamed <@5> to `bob`".into(),
            ..Default::default()
        }];
        let d = desc(&result_embeds(
            "sanitize",
            &t,
            Default::default(),
            Some("x"),
            9,
        ));
        assert!(d.contains("<@5> [`5`]"));
    }

    /// Records the order of the DM and the action.
    async fn step(log: &Mutex<Vec<&'static str>>, what: &'static str) {
        log.lock().unwrap().push(what);
    }

    #[tokio::test]
    async fn punishments_dm_before_they_act() {
        for kind in [
            "warn",
            "timeout",
            "quarantine",
            "kick",
            "ban",
            "softban",
            "tempban",
        ] {
            assert!(dm_first(kind), "{kind}");
            let log = Mutex::new(vec![]);
            let dm = async {
                step(&log, "dm").await;
                true
            };
            let act = async {
                step(&log, "act").await;
                anyhow::Ok(())
            };
            let (told, r) = dm_and_act(dm_first(kind), Some(dm), act).await;
            assert!(r.is_ok());
            assert_eq!(
                (told, log.into_inner().unwrap()),
                (DmOutcome::Sent, vec!["dm", "act"])
            );
        }
        for kind in ["unquarantine", "untimeout", "unban"] {
            assert!(!dm_first(kind), "{kind}");
        }
    }

    #[tokio::test]
    async fn failed_dm_still_punishes() {
        let acted = Mutex::new(false);
        let act = async {
            *acted.lock().unwrap() = true;
            anyhow::Ok(())
        };
        let (told, r) = dm_and_act(true, Some(async { false }), act).await;
        assert!(r.is_ok() && *acted.lock().unwrap());
        assert_eq!(told, DmOutcome::Failed);
        let e = result_embeds("ban", &[ok(1, 5, told)], Default::default(), Some("r"), 9);
        assert!(desc(&e).contains("DM-Members: ❎"));
    }

    #[tokio::test(start_paused = true)]
    async fn hanging_dm_gives_up_after_three_seconds() {
        let start = tokio::time::Instant::now();
        let act = async { anyhow::Ok(tokio::time::Instant::now()) };
        let (told, r) = dm_and_act(true, Some(std::future::pending::<bool>()), act).await;
        assert_eq!(told, DmOutcome::Failed);
        assert_eq!(r.unwrap() - start, DM_WAIT);
        assert_eq!(DM_WAIT, Duration::from_secs(3));
    }

    #[tokio::test]
    async fn removals_act_first_and_skip_the_dm_when_they_fail() {
        let log = Mutex::new(vec![]);
        let dm = async {
            step(&log, "dm").await;
            false
        };
        let act = async {
            step(&log, "act").await;
            anyhow::Ok(())
        };
        // An unban DM usually fails (no shared server): the unban still happened, the result shows ❎.
        let (told, r) = dm_and_act(dm_first("unban"), Some(dm), act).await;
        assert!(r.is_ok());
        assert_eq!(
            (told, log.into_inner().unwrap()),
            (DmOutcome::Failed, vec!["act", "dm"])
        );
        let failed = async { Err::<(), _>(anyhow::anyhow!("no ban")) };
        let (told, r) = dm_and_act(false, Some(async { true }), failed).await;
        assert!(r.is_err());
        assert_eq!(told, DmOutcome::Off);
    }

    #[tokio::test]
    async fn toggle_off_sends_nothing() {
        let none: Option<std::future::Ready<bool>> = None;
        for first in [true, false] {
            let (told, _) = dm_and_act(first, none.clone(), async { anyhow::Ok(()) }).await;
            assert_eq!(told, DmOutcome::Off);
        }
    }

    #[test]
    fn one_switch_for_every_dm() {
        let mut misc = brim_core::sections::MiscCfg::default();
        assert!(misc.always_dm, "on by default");
        assert!(wants_dm(false, &misc));
        misc.always_dm = false;
        assert!(!wants_dm(false, &misc));
        assert!(wants_dm(true, &misc), "?dm still sends one");
    }

    #[test]
    fn a_quarantine_dm_without_a_reason_says_how_to_reach_staff() {
        assert!(reach_staff("quarantine", None, true).is_some());
        assert_eq!(reach_staff("quarantine", Some("raid"), true), None);
        assert_eq!(reach_staff("quarantine", None, false), None, "tickets off");
        assert_eq!(reach_staff("ban", None, true), None);
    }

    #[test]
    fn dm_embeds() {
        let e = dm_embed("warn", "WellBeing", None, Some("mod"), None);
        assert_eq!(
            e.title.as_deref(),
            Some("You have been warned in WellBeing!")
        );
        let d = e.description.unwrap();
        assert!(d.contains("**Reason:** No reason.") && d.contains("**Moderator:** `mod`"));
        assert!(!d.contains("Duration"));
        let e = dm_embed("timeout", "S", Some("spam"), None, Some(300));
        assert_eq!(e.title.as_deref(), Some("You have been timed out in S!"));
        let d = e.description.unwrap();
        assert!(d.contains("**Reason:** spam") && d.contains("**Duration:** `5m`"));
        assert!(!d.contains("Moderator"));
        for (k, t) in [
            ("quarantine", "You have been quarantined in S!"),
            ("kick", "You have been kicked in S!"),
            ("ban", "You have been banned in S!"),
            ("softban", "You have been softbanned in S!"),
            ("tempban", "You have been temporarily banned in S!"),
            ("unquarantine", "You have been unquarantined in S!"),
            ("untimeout", "Your timeout was removed in S!"),
            ("unban", "You have been unbanned in S!"),
        ] {
            assert_eq!(dm_embed(k, "S", None, None, None).title.as_deref(), Some(t));
        }
        // A ban shows no duration line, a tempban does.
        assert!(!dm_embed("ban", "S", None, None, Some(60))
            .description
            .unwrap()
            .contains("Duration"));
        assert!(dm_embed("tempban", "S", None, None, Some(60))
            .description
            .unwrap()
            .contains("`1m`"));
    }

    #[test]
    fn batches_cap_at_100_and_skip_old() {
        // A snowflake made "now" in ms since the Discord epoch.
        let now = 1_800_000_000;
        let fresh = ((now as u64 * 1000) - 1_420_070_400_000) << 22;
        let old = ((now as u64 - 15 * 86_400) * 1000 - 1_420_070_400_000) << 22;
        let mut msgs: Vec<(u64, u64)> = (0..150).map(|i| (1, fresh + i)).collect();
        msgs.push((1, old));
        msgs.push((2, fresh));
        let mut b = batches(&msgs, now);
        b.sort_by_key(|(c, ids)| (*c, std::cmp::Reverse(ids.len())));
        let sizes: Vec<_> = b.iter().map(|(c, ids)| (*c, ids.len())).collect();
        assert_eq!(sizes, [(1, 100), (1, 50), (2, 1)]);
    }

    #[test]
    fn owner_alert_waits_ten_minutes_per_server() {
        let mut last = BTreeMap::new();
        assert!(alert_due(&mut last, 1, 1000, 600));
        assert!(!alert_due(&mut last, 1, 1599, 600));
        assert!(alert_due(&mut last, 2, 1599, 600));
        assert!(alert_due(&mut last, 1, 1600, 600));
    }
}

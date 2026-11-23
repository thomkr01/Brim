//! Moderation commands beyond ban/kick/timeout/warn: unban, untimeout, unwarn, hackban,
//! quarantine, purge, slowmode, verify, sanitize and case viewing/editing. One implementation, prefix and slash parsers.
use anyhow::Result;
use brim_antinuke::Inverse;
use brim_core::{
    cases,
    notes::{self, NoteAction, NoteLog},
    permits,
};
use brim_logging::Log;
use serde_json::{json, Value};
use twilight_http::request::AuditLogReason;
use twilight_model::{
    channel::message::{AllowedMentions, Component, Embed},
    gateway::payload::incoming::MessageCreate,
    id::{
        marker::{ChannelMarker, GuildMarker, UserMarker},
        Id,
    },
};

use crate::{
    antinuke,
    app::App,
    commands::{self, gated, target_above_bot},
    confirm::{self, Action, Pending},
    prefix::{Flags, Parsed},
    purge,
    reply::{self, Applied, DmOutcome},
    settings, targets,
};

/// The clicked message of a purge menu: From Here purges it and the newer ones below it, Until Here
/// the older ones above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Anchor {
    From(u64),
    Until(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModCmd {
    Unban {
        user: u64,
    },
    Hackban {
        user: u64,
    },
    Untimeout {
        user: u64,
    },
    Unwarn {
        case: i32,
    },
    Quarantine {
        user: u64,
    },
    Unquarantine {
        user: u64,
    },
    Purge {
        count: usize,
        user: Option<u64>,
        /// Start at this message (message context menus) instead of the newest.
        from: Option<Anchor>,
        #[serde(default)]
        filters: Vec<crate::purge::Filter>,
        /// The command message: deleted too but not counted (owner decision 11-17).
        #[serde(default)]
        own: Vec<u64>,
        /// The count starts before this id: the command message, or a slash interaction.
        #[serde(default)]
        before: Option<u64>,
    },
    Slowmode {
        secs: u16,
        /// Empty means the current channel.
        #[serde(default)]
        channels: Vec<u64>,
    },
    CasesView {
        case: Option<i32>,
        user: Option<u64>,
        /// `modcases`: cases this moderator made.
        by: Option<u64>,
        /// `?t TYPE`: one case kind.
        kind: Option<String>,
    },
    CasesEdit {
        case: i32,
        reason: String,
    },
    /// `proof 12 <link or text>`, or with a file: one more proof on a case.
    ProofAdd {
        case: i32,
        text: String,
    },
    /// `closecase` / `opencase`: switch a case off or back on.
    CaseSet {
        case: i32,
        active: bool,
    },
    ClearWarns {
        user: u64,
    },
    Verify {
        user: u64,
    },
    Sanitize {
        user: u64,
    },
    NoteAdd {
        user: u64,
        text: String,
    },
    /// Every filter is optional; none lists the latest notes.
    NoteList {
        user: Option<u64>,
        id: Option<String>,
        by: Option<u64>,
    },
    NoteRemove {
        id: String,
        /// Only if the note is about this member.
        user: Option<u64>,
    },
    NoteModify {
        id: String,
        text: String,
    },
    /// Without filters: every note of the server.
    NoteSweep {
        user: Option<u64>,
        by: Option<u64>,
    },
}

/// Numbers below this are case numbers / counts, not snowflakes.
pub(crate) const SMALL: u64 = 1_000_000;
pub(crate) const MAX_SLOWMODE: u64 = 21_600;
const NOTE_MIN: usize = 5;
const NOTE_MAX: usize = 500;

impl ModCmd {
    /// `cases MEMBER` with no other filter pages through all of the member's cases.
    pub fn paged_user(&self) -> Option<u64> {
        match *self {
            ModCmd::CasesView {
                case: None,
                user: Some(u),
                by: None,
                kind: None,
            } => Some(u),
            _ => None,
        }
    }

    /// Permit name and rate-limit action. Unban/hackban share `ban`, untimeout shares `timeout`.
    pub(crate) fn perm(&self) -> &'static str {
        match self {
            Self::Unban { .. } | Self::Hackban { .. } => "ban",
            Self::Untimeout { .. } => "timeout",
            Self::Unwarn { .. } | Self::ClearWarns { .. } => "warn",
            Self::Quarantine { .. } | Self::Unquarantine { .. } => "quarantine",
            Self::Purge { .. } => "purge",
            Self::NoteList { .. } => "notes_view",
            Self::NoteAdd { .. }
            | Self::NoteModify { .. }
            | Self::NoteRemove { .. }
            | Self::NoteSweep { .. } => "notes_manage",
            Self::Slowmode { .. } => "slowmode",
            Self::CasesView { .. }
            | Self::CasesEdit { .. }
            | Self::CaseSet { .. }
            | Self::ProofAdd { .. } => "cases",
            Self::Verify { .. } => "verify",
            Self::Sanitize { .. } => "sanitize",
        }
    }

    /// Commands that lose data or are hard to undo ask for a Confirm click first (`MiscCfg.confirm`).
    pub fn destructive(&self) -> bool {
        matches!(
            self,
            Self::Purge { .. } | Self::Hackban { .. } | Self::NoteSweep { .. }
        )
    }

    /// Whether to wait for a Confirm click: a sweep of every note in the server always asks,
    /// other destructive commands only while `misc.confirm` is on.
    pub fn asks_confirm(&self, confirm_on: bool) -> bool {
        matches!(
            self,
            Self::NoteSweep {
                user: None,
                by: None
            }
        ) || (confirm_on && self.destructive())
    }

    /// A purge counts from before `id` (the command message or slash interaction) and, when
    /// `message`, deletes it too, uncounted.
    pub fn sent_at(&mut self, id: u64, message: bool) {
        if let Self::Purge { own, before, .. } = self {
            *before = Some(id);
            if message {
                own.push(id);
            }
        }
    }

    /// Completes "Really ...?" for the confirm prompt.
    pub fn describe(&self) -> String {
        match self {
            Self::Purge {
                count,
                user: Some(u),
                ..
            } => format!("purge {count} messages of <@{u}>"),
            Self::Purge { count, .. } => format!("purge {count} messages"),
            Self::Hackban { user } => format!("ban <@{user}>"),
            Self::NoteSweep { user: Some(u), .. } => format!("remove all notes of <@{u}>"),
            Self::NoteSweep { by: Some(m), .. } => format!("remove all notes written by <@{m}>"),
            Self::NoteSweep { .. } => "remove all notes".into(),
            _ => self.perm().into(),
        }
    }

    /// The result Details flags this command implies: a hackban shows Hack Ban ✅.
    pub(crate) fn flags(&self) -> Flags {
        Flags {
            hack: matches!(self, Self::Hackban { .. }),
            ..Flags::default()
        }
    }

    /// Quarantine, unquarantine and unban ask Yes/No when a member was typed as an id or a name.
    pub fn asks_when_unsure(&self) -> bool {
        matches!(
            self,
            Self::Quarantine { .. } | Self::Unquarantine { .. } | Self::Unban { .. }
        )
    }

    /// These take several members; the prefix runs one copy per member.
    pub(crate) fn with_user(&self, u: u64) -> Option<Self> {
        Some(match self {
            Self::Unban { .. } => Self::Unban { user: u },
            Self::Untimeout { .. } => Self::Untimeout { user: u },
            Self::Quarantine { .. } => Self::Quarantine { user: u },
            Self::Unquarantine { .. } => Self::Unquarantine { user: u },
            Self::Verify { .. } => Self::Verify { user: u },
            Self::Sanitize { .. } => Self::Sanitize { user: u },
            _ => return None,
        })
    }

    /// Key of the result embed (`reply::result_embeds`); None keeps plain text (views, notes, case edits).
    pub(crate) fn result_kind(&self) -> Option<&'static str> {
        Some(match self {
            Self::Hackban { .. } => "ban",
            Self::Unban { .. } => "unban",
            Self::Untimeout { .. } => "untimeout",
            Self::Quarantine { .. } => "quarantine",
            Self::Unquarantine { .. } => "unquarantine",
            Self::Verify { .. } => "verify",
            Self::Sanitize { .. } => "sanitize",
            Self::Purge { .. } => "purge",
            Self::Slowmode { .. } => "slowmode",
            Self::NoteAdd { .. } | Self::NoteList { id: Some(_), .. } => "note",
            _ => return None,
        })
    }

    /// The member the permit/hierarchy check applies to.
    pub(crate) fn target(&self) -> Option<u64> {
        match *self {
            Self::Unban { user }
            | Self::Hackban { user }
            | Self::Untimeout { user }
            | Self::ClearWarns { user }
            | Self::Quarantine { user }
            | Self::Unquarantine { user }
            // Verify can end a quarantine, so it gets the same checks as unquarantine.
            | Self::Verify { user }
            | Self::Sanitize { user }
            | Self::NoteAdd { user, .. }
            | Self::NoteList {
                user: Some(user), ..
            }
            | Self::NoteRemove {
                user: Some(user), ..
            }
            | Self::NoteSweep {
                user: Some(user), ..
            }
            // A member's messages: the same protection as ban (owner decision 11-17).
            | Self::Purge {
                user: Some(user), ..
            } => Some(user),
            _ => None,
        }
    }
}

/// Text and embeds for a finished command: result embeds, or count lines when compact.
pub(crate) fn reply_parts(
    t: &reply::ModTexts,
    kind: Option<&str>,
    compact: bool,
    done: &[Applied],
    shown: (Flags, Option<i64>),
    reason: Option<&str>,
    actor: u64,
) -> (String, Vec<Embed>) {
    let joined = || {
        done.iter()
            .map(|a| a.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    match kind {
        // A note card shows the note's text, so it goes to the moderator only (see `run_prefix`).
        Some("note") => match done.first() {
            Some(a) if !compact && a.text.starts_with("Note: ") => {
                (String::new(), vec![crate::modview::note_embed(&a.text)])
            }
            _ => (joined(), vec![]),
        },
        Some(k) if !compact => (
            String::new(),
            reply::result_embeds_with(t, k, done, shown, reason, actor),
        ),
        Some(k) => reply::summary_with(t, k, true, done, reason, actor),
        None => (joined(), vec![]),
    }
}

/// `resolved` is a member found by name when no id/mention was typed.
pub fn from_prefix(p: &Parsed, resolved: Option<u64>) -> Result<ModCmd, String> {
    let small = p.targets.iter().copied().find(|t| *t < SMALL);
    let user = p.targets.iter().copied().find(|t| *t >= SMALL).or(resolved);
    let need_user = || user.ok_or_else(|| "Give a user mention, id or name".to_string());
    let case = || {
        small
            .map(|n| n as i32)
            .ok_or_else(|| "Give a case number".to_string())
    };
    Ok(match p.command.as_str() {
        "unban" => ModCmd::Unban { user: need_user()? },
        "hackban" => ModCmd::Hackban { user: need_user()? },
        "untimeout" | "unmute" => ModCmd::Untimeout { user: need_user()? },
        "unwarn" => ModCmd::Unwarn { case: case()? },
        "clearwarns" => ModCmd::ClearWarns { user: need_user()? },
        "closecase" | "caseoff" | "opencase" | "caseon" => ModCmd::CaseSet {
            case: case()?,
            active: matches!(p.command.as_str(), "opencase" | "caseon"),
        },
        // `run_prefix` put the text as typed into `reason`; empty when the proof is a file.
        "proof" => ModCmd::ProofAdd {
            case: case()?,
            text: p.reason.clone().unwrap_or_default(),
        },
        // The words after the case number are the reason, no `?r` needed.
        "editc" | "editreason" => ModCmd::CasesEdit {
            case: case()?,
            reason: p
                .reason
                .clone()
                .or_else(|| Some(p.names.join(" ")).filter(|r| !r.is_empty()))
                .ok_or("Give the new reason")?,
        },
        "quarantine" => ModCmd::Quarantine { user: need_user()? },
        "unquarantine" => ModCmd::Unquarantine { user: need_user()? },
        "verify" => ModCmd::Verify { user: need_user()? },
        "sanitize" | "dehoist" => ModCmd::Sanitize { user: need_user()? },
        "purge" | "clear" => ModCmd::Purge {
            count: small
                .filter(|n| (1..=purge::MAX as u64).contains(n))
                .ok_or("Give a message count from 1 to 1000")? as usize,
            user,
            from: None,
            filters: p
                .kind
                .as_deref()
                .map(|k| purge::Filter::parse(k, p.input.as_deref()))
                .transpose()?
                .into_iter()
                .chain(p.roles.iter().map(|r| purge::Filter::Role(*r)))
                .collect(),
            own: vec![],
            before: None,
        },
        "slowmode" => {
            let channels: Vec<u64> = p
                .channels
                .iter()
                .copied()
                .chain(p.targets.iter().copied().filter(|t| *t >= SMALL))
                .collect();
            let off = p.names.iter().any(|n| n == "off") || !channels.is_empty();
            let secs = p
                .duration_s
                .map(|d| d as u64)
                .or(small)
                .or(off.then_some(0));
            ModCmd::Slowmode {
                secs: secs
                    .filter(|s| *s <= MAX_SLOWMODE)
                    .ok_or("Give a delay up to 6h (e.g. 10s) or `off`")?
                    as u16,
                channels,
            }
        }
        "note" | "notes" => {
            let (sub, text) = (p.names.first().map(String::as_str), p.reason.clone());
            match sub {
                Some("add") => ModCmd::NoteAdd {
                    user: need_user()?,
                    text: text.ok_or("Give the note with ?r")?,
                },
                Some("remove") => ModCmd::NoteRemove {
                    id: p
                        .names
                        .get(1)
                        .cloned()
                        .or(small.map(|n| n.to_string()))
                        .ok_or("Give a note id")?,
                    user,
                },
                Some("sweep") => ModCmd::NoteSweep {
                    user: Some(need_user()?),
                    by: None,
                },
                _ => ModCmd::NoteList {
                    user,
                    id: None,
                    by: None,
                },
            }
        }
        "cases" | "case" if p.names.first().is_some_and(|n| n == "modify") => ModCmd::CasesEdit {
            case: case()?,
            reason: p.reason.clone().ok_or("Give a new reason with ?r")?,
        },
        // No case and no user lists the guild's latest cases.
        "cases" | "case" => ModCmd::CasesView {
            case: small.map(|n| n as i32),
            user,
            by: None,
            kind: crate::modview::case_kind(p.kind.as_deref())?,
        },
        "warns" => ModCmd::CasesView {
            case: None,
            user: Some(need_user()?),
            by: None,
            kind: Some("warn".into()),
        },
        "modcases" => ModCmd::CasesView {
            case: None,
            user: None,
            by: Some(need_user()?),
            kind: crate::modview::case_kind(p.kind.as_deref())?,
        },
        _ => return Err(crate::reply::UNKNOWN_COMMAND.into()),
    })
}

/// The word after `word` in a message, lowercased as note ids are.
pub(crate) fn word_after(content: &str, word: &str) -> Option<String> {
    let mut words = content.split_whitespace();
    words.find(|w| w.eq_ignore_ascii_case(word))?;
    words.next().map(str::to_lowercase)
}

/// `editc` (a new reason) or `closecase` / `opencase` (the open state): the change, its history
/// row and its mod log entry. An edit that changes nothing writes no history and no log.
async fn case_edit(
    app: &App,
    gid: Id<GuildMarker>,
    actor_id: Id<UserMarker>,
    case: i32,
    reason: Option<String>,
    active: Option<bool>,
) -> Result<Applied> {
    if reason
        .as_ref()
        .is_some_and(|r| r.chars().count() > cases::CASE_TEXT_MAX)
    {
        return say(app, gid, "reply.case.reason_too_long", &[]).await;
    }
    let (g, by) = (gid.get() as i64, (actor_id.get() as i64, None));
    let new = reason.as_deref().map(Some);
    let was = cases::edit(&mut *app.pool.acquire().await?, g, case, new, active, by).await?;
    let Some(was) = was else {
        return no_case(app, gid, case).await;
    };
    stat(
        app,
        g,
        actor_id.get(),
        "case_edit",
        Some(was.user_id),
        Some(case),
    )
    .await;
    let case_s = case.to_string();
    let reason_change = reason.clone().filter(|r| was.reason.as_ref() != Some(r));
    let active_change = active.filter(|a| *a != was.active);
    if reason_change.is_some() || active_change.is_some() {
        let log = cases::CaseLog {
            case_no: case,
            by: actor_id.get(),
            reason: reason_change.map(|r| (was.reason, Some(r))),
            active: active_change,
            ..Default::default()
        };
        crate::proof::send_case_log(app, gid.get(), &log).await.ok();
    }
    match (reason, active) {
        (Some(_), _) => say(app, gid, "reply.case.reason_updated", &[("case", &case_s)]).await,
        (_, Some(true)) => say(app, gid, "reply.case.active_again", &[("case", &case_s)]).await,
        _ => plain(closed_text(app, gid, case).await),
    }
}

/// `proof CASE` with nothing after it: the case's proofs, oldest first, who added each and when.
async fn proof_list(app: &App, gid: Id<GuildMarker>, case: i32) -> Result<Applied> {
    let g = gid.get() as i64;
    let exists: Option<i32> =
        sqlx::query_scalar("SELECT case_no FROM cases WHERE guild_id = $1 AND case_no = $2")
            .bind(g)
            .bind(case)
            .fetch_optional(&app.pool)
            .await?;
    if exists.is_none() {
        return no_case(app, gid, case).await;
    }
    let rows: Vec<(String, bool, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT body, verified, added_by, ai_client, EXTRACT(EPOCH FROM added_at)::bigint
         FROM case_proofs WHERE guild_id = $1 AND case_no = $2 ORDER BY id",
    )
    .bind(g)
    .bind(case)
    .fetch_all(&app.pool)
    .await?;
    if rows.is_empty() {
        return say(
            app,
            gid,
            "reply.case.no_proof",
            &[("case", &case.to_string())],
        )
        .await;
    }
    let mut out = format!("Proofs of case #{case}:");
    for (i, (body, verified, by, ai, at)) in rows.iter().enumerate() {
        let who = ai.clone().unwrap_or_else(|| format!("<@{by}>"));
        let mark = if *verified { " (verified)" } else { "" };
        out.push_str(&format!("\n{}. {body}{mark}: {who}, <t:{at}:f>", i + 1));
    }
    plain(out)
}

/// `proof CASE [PROOF]`: the typed proof and the attached file are one proof each.
async fn proof_add(
    app: &App,
    gid: Id<GuildMarker>,
    actor_id: Id<UserMarker>,
    case: i32,
    (text, file): (&str, Option<&str>),
    subject: Option<i64>,
) -> Result<Applied> {
    if text.is_empty() && file.is_none() {
        return proof_list(app, gid, case).await;
    }
    if text.chars().count() > cases::CASE_TEXT_MAX {
        return say(app, gid, "reply.case.proof_too_long", &[]).await;
    }
    let (g, by) = (gid.get() as i64, (actor_id.get() as i64, None));
    let (case_s, max_s) = (case.to_string(), cases::PROOFS_MAX.to_string());
    let mut lines = vec![];
    for (body, is_file) in [(Some(text), false), (file, true)] {
        let Some(body) = body.filter(|b| !b.is_empty()) else {
            continue;
        };
        let kind = cases::proof_kind(body);
        match cases::add_proof(&app.pool, g, case, (kind, body), by, None).await? {
            cases::Added::NoCase => return no_case(app, gid, case).await,
            cases::Added::Full => {
                let vars = [("case", case_s.as_str()), ("count", &max_s)];
                lines.push(reply::say_line(app, gid, "reply.case.proofs_full", &vars).await);
                break;
            }
            cases::Added::Done => {}
        }
        let shown = if is_file {
            "A file (on this message)"
        } else {
            body
        };
        let log = crate::proof::case_log(&cases::CaseLog {
            case_no: case,
            by: actor_id.get(),
            proof_added: Some(shown.into()),
            ..Default::default()
        });
        // A file is kept on this mod log message, and the proof becomes that message's link.
        let file = is_file.then_some(body);
        // "proof" is no punishment kind, so this entry gets no undo buttons.
        let kept = commands::send_case_log(app, gid, (case, "proof"), log, &[], file).await;
        let vars = [("case", case_s.as_str())];
        lines.push(reply::say_line(app, gid, "reply.case.proof_added", &vars).await);
        lines.extend(kept);
    }
    stat(app, g, actor_id.get(), "case_edit", subject, Some(case)).await;
    plain(lines.join("\n"))
}

/// Everything after the first whole number in `content` (`proof 12 https://...` gives the link).
fn text_after_number(content: &str) -> Option<String> {
    let mut words = content.split_whitespace();
    words.find(|w| w.parse::<i32>().is_ok())?;
    let rest = words.collect::<Vec<_>>().join(" ");
    (!rest.is_empty()).then_some(rest)
}

/// Prefix entry for the commands above; the caller only routes known names here.
pub async fn run_prefix(app: &App, msg: &MessageCreate, p: Parsed) -> Result<()> {
    let Some(gid) = msg.guild_id else {
        return Ok(());
    };
    let owner = app.guild_info(gid).await?.owner;
    let level = app.level(gid, owner, msg.author.id).await?;
    if level == permits::Level::None && app.cfg(gid.get()).await?.misc.ignore_no_permit {
        return Ok(());
    }
    // `unban 123 spam links`: the words after an id are the reason, not names.
    let mut p = p;
    if p.command == "unban" && !p.targets.is_empty() && p.reason.is_none() {
        p.reason = Some(p.names.join(" ")).filter(|r| !r.is_empty());
        p.names.clear();
    }
    let multi = matches!(
        p.command.as_str(),
        "unban" | "untimeout" | "quarantine" | "unquarantine" | "verify" | "sanitize"
    );
    let name_word = |n: &&String| {
        !matches!(
            n.as_str(),
            "modify" | "view" | "off" | "add" | "remove" | "sweep" | "list"
        )
    };
    // `editc 3 new reason`: the words are the reason, not a member.
    let free_text = matches!(p.command.as_str(), "editc" | "editreason" | "proof");
    if p.command == "proof" {
        // The proof as typed: the parser lower-cases words and reads some as durations or flags.
        p.reason = text_after_number(&msg.content);
    }
    let mut users: Vec<u64> = p.targets.iter().copied().filter(|t| *t >= SMALL).collect();
    let mut failed = vec![];
    // `note remove q413p9`: the id is not a member name.
    let note_id = matches!(p.command.as_str(), "note" | "notes")
        && p.names.first().is_some_and(|n| n == "remove");
    if note_id {
        // The id as typed: the parser reads `12345m` as a duration and `012345` as the number 12345.
        if let Some(raw) = word_after(&msg.content, "remove") {
            p.names.insert(1, raw);
        }
    }
    let lookups: Vec<&String> = match (multi, free_text || note_id, users.is_empty()) {
        (true, ..) => p.names.iter().filter(name_word).collect(),
        (false, false, true) if p.targets.iter().all(|t| *t < SMALL) => {
            p.names.iter().find(name_word).into_iter().collect()
        }
        _ => vec![],
    };
    let mut bans = None;
    for n in lookups {
        let found = if p.command == "unban" {
            if bans.is_none() {
                bans = Some(targets::ban_names(app, gid).await?);
            }
            targets::resolve_banned(bans.as_deref().unwrap_or_default(), n)
        } else {
            targets::resolve(app, gid, n).await?
        };
        match found {
            Ok(id) => users.push(id.get()),
            Err(e) if multi => failed.push(e),
            Err(e) => return commands::say(app, msg, &e).await,
        }
    }
    let mut cmd = match from_prefix(&p, users.first().copied()) {
        Ok(c) => c,
        Err(e) => {
            let text = if failed.is_empty() {
                e
            } else {
                failed.join("\n")
            };
            return commands::say_said(app, msg, &crate::reply::say_wrong(app, gid, &text).await)
                .await;
        }
    };
    cmd.sent_at(msg.id.get(), true);
    let kind = cmd.result_kind();
    let shown = (
        Flags {
            hack: cmd.flags().hack,
            ..p.flags
        },
        None,
    );
    let private = matches!(cmd, ModCmd::NoteAdd { .. } | ModCmd::NoteList { .. });
    let added = matches!(cmd, ModCmd::NoteAdd { .. });
    let paged = cmd.paged_user();
    let action = Action::Ext {
        cmd,
        channel: msg.channel_id,
        reason: p.reason.clone(),
        // `proof 12` with a file: the file is the proof, kept on the mod log like a slash one.
        proof: (p.command == "proof")
            .then(|| msg.attachments.first().map(|a| a.url.clone()))
            .flatten(),
        users,
        unsure: p.unsure(),
        dm: p.flags.dm,
        reply: confirm::reply_of(msg),
    };
    let job = Pending::new(gid, msg.author.id, action);
    if job.needs_confirm(app.cfg(gid.get()).await?.misc.confirm) {
        if !failed.is_empty() {
            commands::say(app, msg, &failed.join("\n")).await?;
        }
        return confirm::ask_confirm(app, msg, job).await;
    }
    let mut texts = job.run(app, owner, 0).await?;
    // A purge posted its own note in the channel.
    if !texts.is_empty() && texts.iter().all(|t| t.posted) && failed.is_empty() {
        return Ok(());
    }
    texts.extend(failed.into_iter().map(Applied::from));
    let compact = app.cfg(gid.get()).await?.misc.compact_replies || p.flags.compact;
    let t = reply::ModTexts::load(app, gid).await;
    let (text, embeds) = reply_parts(
        &t,
        kind,
        compact,
        &texts,
        shown,
        p.reason.as_deref(),
        msg.author.id.get(),
    );
    if private {
        // Notes are for moderators only: the channel never sees their text.
        let framed = crate::reply::command_reply(app, true, &text, &embeds).await;
        let (text, embeds) = framed.as_ref().map_or((&text, &embeds), |f| (&f.0, &f.1));
        let sent = async {
            let ch = app
                .http
                .create_private_channel(msg.author.id)
                .await?
                .model()
                .await?;
            app.http
                .create_message(ch.id)
                .content(text)
                .embeds(embeds)
                .allowed_mentions(Some(&AllowedMentions::default()))
                .await?;
            anyhow::Ok(())
        };
        // A saved note must not read as a failure, or it is added twice.
        let saved = (texts.first())
            .filter(|_| added)
            .and_then(|a| a.text.strip_prefix("Note: "))
            .and_then(|r| r.split(' ').next());
        let (key, id) = match (sent.await, saved) {
            (Ok(()), _) => ("reply.note.dm_sent", ""),
            (Err(_), Some(id)) => ("reply.note.saved_no_dm", id),
            (Err(_), None) => ("reply.note.no_dm", ""),
        };
        let notice = reply::say_line(app, gid, key, &[("id", id)]).await;
        return commands::say(app, msg, &notice).await;
    }
    if let Some((e, row)) = cases_pager(app, gid, msg.author.id.get(), paged, &text).await? {
        return settings::send(app, msg, "", Some(e), vec![row]).await;
    }
    commands::say_embeds(app, msg, &text, &embeds).await
}

/// The first page and its buttons when a `cases MEMBER` reply spans several pages.
pub(crate) async fn cases_pager(
    app: &App,
    gid: Id<GuildMarker>,
    invoker: u64,
    user: Option<u64>,
    text: &str,
) -> Result<Option<(Embed, Component)>> {
    // A first page that starts with a case line was allowed; denials are plain text.
    let (Some(u), true) = (user, text.starts_with("`#")) else {
        return Ok(None);
    };
    let (e, pages) = crate::modview::cases_page(app, gid.get() as i64, u, 0).await?;
    Ok((pages > 1).then(|| {
        (
            e,
            settings::page_row(invoker, &format!("cases.{u}"), 0, pages),
        )
    }))
}

/// The reply to a closed case; the Close case button knows its press worked by it.
pub(crate) async fn closed_text(app: &App, gid: Id<GuildMarker>, case: i32) -> String {
    let case = case.to_string();
    reply::say_line(app, gid, "reply.case.closed", &[("case", &case)]).await
}

/// A short reply that is an entry (`reply.case.*`, `reply.note.*`).
async fn say(app: &App, gid: Id<GuildMarker>, key: &str, vars: &[(&str, &str)]) -> Result<Applied> {
    plain(reply::say_line(app, gid, key, vars).await)
}

async fn no_case(app: &App, gid: Id<GuildMarker>, case: i32) -> Result<Applied> {
    say(
        app,
        gid,
        "reply.case.no_case",
        &[("case", &case.to_string())],
    )
    .await
}

/// A reply that is no case: a refusal, a listing or a note.
fn plain(text: impl Into<String>) -> Result<Applied> {
    Ok(Applied::from(text.into()))
}

/// Permit check -> hierarchy -> rate limit -> action -> case. `dm` is the `?dm` flag: quarantine
/// and the removals DM the member when it or the server's DM toggle says so.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    app: &App,
    gid: Id<GuildMarker>,
    owner: Id<UserMarker>,
    (actor_id, actor): (Id<UserMarker>, &permits::Level),
    channel: Id<ChannelMarker>,
    cmd: ModCmd,
    (reason, proof): (Option<&str>, Option<&str>),
    dm: bool,
) -> Result<Applied> {
    let perm = cmd.perm();
    let target = cmd.target();
    if let Some(t) = cmd.target() {
        let target = Id::new(t);
        let mut level = app.level(gid, owner, target).await?;
        // Quarantine took the member's roles, so judge a release by the roles it gives back.
        if matches!(cmd, ModCmd::Unquarantine { .. } | ModCmd::Verify { .. }) {
            let saved: Option<(Vec<i64>, bool)> = sqlx::query_as(
                "SELECT saved_roles, on_join FROM quarantines WHERE guild_id=$1 AND user_id=$2",
            )
            .bind(gid.get() as i64)
            .bind(t as i64)
            .fetch_optional(&app.pool)
            .await?;
            if let Some((roles, on_join)) = saved {
                // Verify ends only verification's join quarantine; any other needs the quarantine permit.
                if matches!(cmd, ModCmd::Verify { .. }) && !on_join && !actor.has("quarantine") {
                    return plain(format!("Cannot verify <@{t}>: they are quarantined for another reason, which needs the `quarantine` permit"));
                }
                let (held, staff) =
                    crate::antinuke::held_staff(app, gid, owner, target, &roles).await?;
                // Staff caught by anti-nuke or a rate limit go free only through an Owner or Extra
                // Owner, as the quarantine hold allows (owner decision 11-17, option A).
                if staff && !matches!(actor, permits::Level::Owner | permits::Level::ExtraOwner) {
                    return plain(format!("Cannot release <@{t}>: their roles hold a permit or dangerous permissions, so only the owner or an Extra Owner can"));
                }
                if held.immune() {
                    level = held;
                }
            }
        }
        // Deleting messages needs no role order: a member above the bot can still be purged.
        let above =
            !matches!(cmd, ModCmd::Purge { .. }) && target_above_bot(app, gid, target).await?;
        let actor_n = actor_id.get() as i64;
        if let Err(d) = permits::check(actor, perm, actor_n, &level, t as i64, above) {
            return plain(commands::deny_text(perm, t, &level, &d));
        }
    } else if !actor.has(perm) {
        return plain(format!("Cannot use `{perm}`: no permit"));
    }
    // A case or note has its member as the target: nobody changes their own, or an owner's or
    // TA's; a sweep by author also spares an immune member. A purge of one member goes through
    // `target` and `permits::check` instead.
    let (subject, what): (Option<i64>, String) = match &cmd {
        ModCmd::Unwarn { case }
        | ModCmd::CasesEdit { case, .. }
        | ModCmd::CaseSet { case, .. }
        | ModCmd::ProofAdd { case, .. } => (
            // A case without a member is about nobody, so its moderator may change it.
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COALESCE({}, 0) FROM cases WHERE guild_id=$1 AND case_no=$2",
                cases::TARGET
            )))
            .bind(gid.get() as i64)
            .bind(case)
            .fetch_optional(&app.pool)
            .await?,
            format!("change case #{case}"),
        ),
        ModCmd::NoteRemove { id, .. } | ModCmd::NoteModify { id, .. } => (
            sqlx::query_scalar("SELECT user_id FROM notes WHERE guild_id=$1 AND short_id=$2")
                .bind(gid.get() as i64)
                .bind(id)
                .fetch_optional(&app.pool)
                .await?,
            format!("change note `{id}`"),
        ),
        ModCmd::NoteSweep { by: Some(b), .. } => {
            (Some(*b as i64), format!("remove the notes by <@{b}>"))
        }
        _ => (None, String::new()),
    };
    // A bare `proof CASE` only lists the proofs, as the case page does for anyone with `cases`.
    let listing =
        matches!(&cmd, ModCmd::ProofAdd { text, .. } if text.is_empty()) && proof.is_none();
    if let Some(m) = subject.filter(|m| *m > 0 && !listing) {
        let own = !matches!(cmd, ModCmd::NoteSweep { .. });
        if own && m as u64 == actor_id.get() {
            return plain(format!("You cannot {what}: it is about you"));
        }
        if !actor.immune() && app.level(gid, owner, Id::new(m as u64)).await?.immune() {
            return plain(format!("Cannot {what}: <@{m}> is immune"));
        }
    }
    if let Some(msg) = gated(app, gid, actor_id, actor, perm).await? {
        return plain(msg);
    }
    let g = gid.get() as i64;
    let reasonless = reason.is_none();
    let audit = commands::audit_reason(reason, actor_id);
    let cfg = app.cfg(gid.get()).await?;
    // The DM for quarantine and the removals, sent by `reply::dm_and_act` around the action.
    let notice = |kind, user: u64| {
        let n = reply::Notice {
            kind,
            reason,
            duration_s: None,
            moderator: (!cfg.misc.hide_staff_in_dms).then_some(actor_id),
            appeal: None,
        };
        reply::wants_dm(dm, &cfg.misc).then(|| reply::tell(app, gid, Id::new(user), n))
    };
    let mut told = DmOutcome::Off;
    // (case kind, case user, inverse, reason prefix, reply)
    let mut purged = None;
    let (kind, uid, inverse, note, reply): (&str, u64, Option<Value>, Option<String>, String) =
        match cmd {
            ModCmd::CasesView {
                case,
                user,
                by,
                kind,
            } => {
                return crate::modview::view_cases(app, g, case, user, by, kind, 0)
                    .await
                    .map(Applied::from)
            }
            ModCmd::NoteAdd { user, text } => {
                if !(NOTE_MIN..=NOTE_MAX).contains(&text.chars().count()) {
                    return say(app, gid, "reply.note.length", &[]).await;
                }
                let (id,): (String,) = sqlx::query_as(
                    "INSERT INTO notes (guild_id, user_id, author_id, text) VALUES ($1,$2,$3,$4) RETURNING short_id",
                )
                .bind(g)
                .bind(user as i64)
                .bind(actor_id.get() as i64)
                .bind(&text)
                .fetch_one(&app.pool)
                .await?;
                let log = NoteLog {
                    action: NoteAction::Add,
                    id: Some(id.clone()),
                    user: Some(user),
                    by: actor_id.get(),
                    text: Some(text.clone()),
                    ..NoteLog::default()
                };
                crate::modview::send_note_log(app, gid.get(), &log)
                    .await
                    .ok();
                return plain(crate::modview::note_text(
                    &id,
                    user,
                    actor_id.get(),
                    None,
                    &text,
                ));
            }
            ModCmd::NoteModify { id, text } => {
                if !(NOTE_MIN..=NOTE_MAX).contains(&text.chars().count()) {
                    return say(app, gid, "reply.note.length", &[]).await;
                }
                let actor = actor_id.get();
                let Some((user, old)) =
                    notes::edit(&app.pool, g, &id, &text, actor as i64, None).await?
                else {
                    return say(app, gid, "reply.note.not_found", &[("id", &id)]).await;
                };
                if old == text {
                    return say(app, gid, "reply.note.unchanged", &[("id", &id)]).await;
                }
                let log = NoteLog {
                    action: NoteAction::Modify,
                    id: Some(id.clone()),
                    user: Some(user as u64),
                    by: actor,
                    text: Some(text),
                    old: Some(old),
                    ..NoteLog::default()
                };
                crate::modview::send_note_log(app, gid.get(), &log)
                    .await
                    .ok();
                return say(app, gid, "reply.note.updated", &[("id", &id)]).await;
            }
            ModCmd::NoteList { user, id, by } => {
                return crate::modview::view_notes(app, g, user, id, by)
                    .await
                    .map(Applied::from)
            }
            ModCmd::NoteRemove { id, user } => {
                let gone: Option<(i64, String)> = sqlx::query_as(
                    "DELETE FROM notes WHERE guild_id=$1 AND short_id=$2 AND ($3::bigint IS NULL OR user_id=$3) RETURNING user_id, text",
                )
                .bind(g)
                .bind(&id)
                .bind(user.map(|u| u as i64))
                .fetch_optional(&app.pool)
                .await?;
                let Some((user, text)) = gone else {
                    return say(app, gid, "reply.note.not_found", &[("id", &id)]).await;
                };
                let log = NoteLog {
                    action: NoteAction::Remove,
                    id: Some(id.clone()),
                    user: Some(user as u64),
                    by: actor_id.get(),
                    text: Some(text),
                    ..NoteLog::default()
                };
                crate::modview::send_note_log(app, gid.get(), &log)
                    .await
                    .ok();
                return say(app, gid, "reply.note.removed", &[("id", &id)]).await;
            }
            ModCmd::NoteSweep { user, by } => {
                let n = sqlx::query(
                    "DELETE FROM notes WHERE guild_id=$1 AND ($2::bigint IS NULL OR user_id=$2) AND ($3::bigint IS NULL OR author_id=$3)",
                )
                .bind(g)
                .bind(user.map(|u| u as i64))
                .bind(by.map(|u| u as i64))
                .execute(&app.pool)
                .await?
                .rows_affected();
                let log = NoteLog {
                    action: NoteAction::Sweep,
                    user,
                    author: by,
                    by: actor_id.get(),
                    count: Some(n),
                    ..NoteLog::default()
                };
                crate::modview::send_note_log(app, gid.get(), &log)
                    .await
                    .ok();
                return say(app, gid, "reply.note.swept", &[("count", &n.to_string())]).await;
            }
            // Boxed: these futures stay out of this already large one (a debug test thread's stack).
            ModCmd::CasesEdit { case, reason } => {
                let reason: String = reason.chars().take(cases::CASE_TEXT_MAX).collect();
                return Box::pin(case_edit(app, gid, actor_id, case, Some(reason), None)).await;
            }
            ModCmd::CaseSet { case, active } => {
                // A warn case's `active` flag is its warn points: closing one is an unwarn and
                // reopening one a warn, so both need the permit perm those need.
                let kind: Option<String> =
                    sqlx::query_scalar("SELECT kind FROM cases WHERE guild_id=$1 AND case_no=$2")
                        .bind(g)
                        .bind(case)
                        .fetch_optional(&app.pool)
                        .await?;
                if kind.as_deref() == Some("warn") && !actor.has("warn") {
                    return plain(format!(
                        "Cannot change case #{case}: a warn case needs the `warn` permit"
                    ));
                }
                return Box::pin(case_edit(app, gid, actor_id, case, None, Some(active))).await;
            }
            ModCmd::ProofAdd { case, text } => {
                let subject = subject.filter(|m| *m > 0);
                return Box::pin(proof_add(app, gid, actor_id, case, (&text, proof), subject))
                    .await;
            }
            ModCmd::ClearWarns { user } => {
                cases::reset_warns(&app.pool, g, user as i64, actor_id.get() as i64).await?;
                stat(
                    app,
                    g,
                    actor_id.get(),
                    "warn_clear",
                    Some(user as i64),
                    None,
                )
                .await;
                return plain(format!("Cleared the warns of <@{user}>"));
            }
            ModCmd::Unban { user } => {
                let act = async {
                    Ok(app
                        .http
                        .delete_ban(gid, Id::new(user))
                        .reason(&audit)
                        .await?)
                };
                let (d, r) =
                    reply::dm_and_act(reply::dm_first("unban"), notice("unban", user), act).await;
                (told, _) = (d, r?);
                crate::scheduler::forget_jobs(app, gid, user, &["unban"], None).await;
                crate::raiders::lifted(app, gid, Id::new(user), actor_id).await;
                ("unban", user, None, None, format!("Unbanned <@{user}>"))
            }
            ModCmd::Hackban { user } => {
                let secs = cfg.misc.purge_secs();
                app.http
                    .create_ban(gid, Id::new(user))
                    .delete_message_seconds(secs)
                    .reason(&audit)
                    .await?;
                crate::scheduler::forget_jobs(app, gid, user, crate::scheduler::BANNED, None).await;
                let inv = json!({"op":"unban","user":user});
                (
                    "ban",
                    user,
                    Some(inv),
                    None,
                    format!("Banned <@{user}> by id"),
                )
            }
            ModCmd::Untimeout { user } => {
                let act = async {
                    Ok(app
                        .http
                        .update_guild_member(gid, Id::new(user))
                        .communication_disabled_until(None)
                        .reason(&audit)
                        .await?)
                };
                let (d, r) =
                    reply::dm_and_act(reply::dm_first("untimeout"), notice("untimeout", user), act)
                        .await;
                (told, _) = (d, r?);
                crate::scheduler::forget_jobs(
                    app,
                    gid,
                    user,
                    &["timeout_renew", "raid_punish"],
                    None,
                )
                .await;
                let text = format!("Removed timeout of <@{user}>");
                ("untimeout", user, None, None, text)
            }
            ModCmd::Unwarn { case } => {
                // The closed warn gets its history line; the unwarn itself is the new case below.
                let row: Option<(i64,)> = sqlx::query_as(
                    "WITH up AS (UPDATE cases SET active=false WHERE guild_id=$1 AND case_no=$2 AND kind='warn' AND active RETURNING user_id),
                          ed AS (INSERT INTO case_edits (guild_id, case_no, field, old_value, new_value, editor_id)
                                 SELECT $1, $2, 'active', 'open', 'closed', $3 FROM up)
                     SELECT user_id FROM up",
                )
                .bind(g)
                .bind(case)
                .bind(actor_id.get() as i64)
                .fetch_optional(&app.pool)
                .await?;
                let Some((uid,)) = row else {
                    return say(
                        app,
                        gid,
                        "reply.case.no_warn_case",
                        &[("case", &case.to_string())],
                    )
                    .await;
                };
                let text = format!("Deactivated warn #{case} of <@{uid}>");
                (
                    "unwarn",
                    uid as u64,
                    None,
                    Some(format!("warn #{case}")),
                    text,
                )
            }
            ModCmd::Quarantine { user } => {
                // Through contain, so a bot also loses its managed role's permissions.
                let act = antinuke::contain(app, gid, Id::new(user), None, antinuke::Kind::Real);
                let (d, r) = reply::dm_and_act(
                    reply::dm_first("quarantine"),
                    notice("quarantine", user),
                    act,
                )
                .await;
                told = d;
                let done = r?;
                if done.applied == antinuke::Applied::Kicked {
                    let text = format!("Kicked <@{user}>: the bot could not be quarantined");
                    ("kick", user, None, None, text)
                } else {
                    let inv = json!({"op":"unquarantine","user":user});
                    let text = format!("Quarantined <@{user}>");
                    ("quarantine", user, Some(inv), None, text)
                }
            }
            ModCmd::Unquarantine { user } => {
                let held: Option<(i64,)> = sqlx::query_as(
                    "SELECT 1::bigint FROM quarantines WHERE guild_id=$1 AND user_id=$2",
                )
                .bind(g)
                .bind(user as i64)
                .fetch_optional(&app.pool)
                .await?;
                if held.is_none() {
                    return plain(format!("<@{user}> is not quarantined"));
                }
                let inv = Inverse::Unquarantine { user };
                let act = antinuke::run_inverse(app, gid, &inv);
                let (d, r) = reply::dm_and_act(
                    reply::dm_first("unquarantine"),
                    notice("unquarantine", user),
                    act,
                )
                .await;
                (told, _) = (d, r?);
                crate::raiders::lifted(app, gid, Id::new(user), actor_id).await;
                let text = format!("Released <@{user}> from quarantine");
                ("unquarantine", user, None, None, text)
            }
            ModCmd::Verify { user } => {
                // Verify also ends a quarantine.
                let held: Option<(i64,)> = sqlx::query_as(
                    "SELECT 1::bigint FROM quarantines WHERE guild_id=$1 AND user_id=$2",
                )
                .bind(g)
                .bind(user as i64)
                .fetch_optional(&app.pool)
                .await?;
                if held.is_some() {
                    antinuke::run_inverse(app, gid, &Inverse::Unquarantine { user }).await?;
                }
                if cfg.verification.roles.is_empty() && held.is_none() {
                    return plain("Set the verification roles on the dashboard first");
                }
                crate::verify::grant(app, gid, Id::new(user), &cfg.verification).await?;
                ("verify", user, None, None, format!("Verified <@{user}>"))
            }
            ModCmd::Sanitize { user } => {
                let m = app
                    .http
                    .guild_member(gid, Id::new(user))
                    .await?
                    .model()
                    .await?;
                let name = m.nick.or(m.user.global_name).unwrap_or(m.user.name);
                let clean = crate::modview::sanitized(&name);
                if clean == name {
                    return plain(format!("<@{user}> already has a clean name"));
                }
                app.http
                    .update_guild_member(gid, Id::new(user))
                    .nick(Some(&clean))
                    .reason(&audit)
                    .await?;
                let note = format!("`{name}` -> `{clean}`");
                let text = format!("Renamed <@{user}> to `{clean}`");
                ("sanitize", user, None, Some(note), text)
            }
            ModCmd::Purge {
                count,
                user,
                from,
                filters,
                own,
                before,
            } => {
                // Log content stays, whoever asks (owner decision 11-15, the Owner included).
                let logs = app.cfg(gid.get()).await?.log_channels();
                // A thread in a log channel counts too.
                let parent = match logs.contains(&channel) {
                    true => None,
                    false => app.http.channel(channel).await?.model().await?.parent_id,
                };
                if logs.contains(&channel) || parent.is_some_and(|p| logs.contains(&p)) {
                    return plain("This is a log channel; its messages cannot be purged");
                }
                let done = match purge::run(
                    app,
                    gid,
                    channel,
                    (count, &own, before),
                    user,
                    &filters,
                    from,
                )
                .await
                {
                    Ok(d) => d,
                    // A purge needs no role order: a refusal is a channel permission.
                    Err(e) if crate::http_err::forbidden_any(&e) => {
                        let text = format!(
                            "I lack Manage Messages or Read Message History in <#{channel}>"
                        );
                        return plain(text);
                    }
                    Err(e) => return Err(e),
                };
                let what = purge::describe(user, &filters, from);
                if done.deleted == 0 {
                    let text = nothing_purged(done.too_old);
                    post_briefly(app, gid, channel, &text, &[]).await;
                    return Ok(Applied {
                        posted: true,
                        ..text.into()
                    });
                }
                // No member: the case has no target (0), not the moderator.
                let who = user.unwrap_or(0);
                let n = done.deleted;
                let note = format!("{n} messages in <#{channel}>");
                let mut text = format!("Deleted {n} messages in <#{channel}>. Filter: {what}");
                if done.too_old > 0 {
                    let old = done.too_old;
                    text.push_str(&format!("\n{old} more matched but are older than 14 days, which Discord does not let bots bulk delete."));
                }
                purged = Some((done, what));
                ("purge", who, None, Some(note), text)
            }
            ModCmd::Slowmode { secs, channels } => {
                let channels = match channels.as_slice() {
                    [] => vec![channel.get()],
                    c => c.to_vec(),
                };
                for c in &channels {
                    let ch = app.http.channel(Id::new(*c)).await?.model().await?;
                    // A raw id can name another server's channel; the bot must not act there.
                    anyhow::ensure!(
                        ch.guild_id == Some(gid),
                        "<#{c}> is not a channel of this server"
                    );
                }
                for c in &channels {
                    app.http
                        .update_channel(Id::new(*c))
                        .rate_limit_per_user(secs)
                        .reason(&audit)
                        .await?;
                    // Staff's value stays: a running auto slowmode must not revert it.
                    if let Err(e) = crate::slowmode::staff_set(app, gid.get(), *c, secs).await {
                        tracing::warn!(?e, channel = c, "ending the auto slowmode failed");
                    }
                }
                let list: Vec<_> = channels.iter().map(|c| format!("<#{c}>")).collect();
                let note = format!("{secs}s in {}", list.join(" "));
                (
                    "slowmode",
                    0,
                    None,
                    Some(note),
                    format!("Slowmode set to {secs}s"),
                )
            }
        };
    let reason = match (note, reason) {
        (Some(n), Some(r)) => Some(format!("{n}: {r}")),
        (n, r) => n.or(r.map(String::from)),
    };
    let case_no = cases::create(
        &app.pool,
        cases::NewCase {
            guild_id: g,
            kind,
            user_id: uid as i64,
            mod_id: actor_id.get() as i64,
            reason: reason.as_deref(),
            points: 0,
            duration_s: None,
            proof,
            inverse,
        },
    )
    .await?;
    let mut log = commands::mod_log_embed(
        (case_no, kind),
        (actor_id.get(), uid),
        None,
        reason.as_deref(),
    );
    let mut files: &[(String, String)] = &[];
    if let Some((done, what)) = &purged {
        log = purge_log(log, channel, done, what);
        files = &done.files;
    }
    let kept = commands::send_case_log(app, gid, (case_no, kind), log, files, proof).await;
    let mut hint = if reasonless {
        crate::reply::add_reason_hint(case_no.into())
    } else {
        String::new()
    };
    if let Some(k) = kept {
        hint += format!("\n{k}").as_str();
    }
    let applied = Applied {
        target,
        case: Some(case_no),
        text: format!("Case #{case_no}: {reply}{hint}"),
        dm: told,
        posted: purged.is_some(),
    };
    if purged.is_some() {
        // The channel sees who purged it; the mod log keeps the record.
        let compact = app.cfg(gid.get()).await?.misc.compact_replies;
        let t = reply::ModTexts::load(app, gid).await;
        let parts = reply_parts(
            &t,
            Some(kind),
            compact,
            std::slice::from_ref(&applied),
            (Flags::default(), None),
            reason.as_deref(),
            actor_id.get(),
        );
        post_briefly(app, gid, channel, &parts.0, &parts.1).await;
    }
    Ok(applied)
}

/// The reply when a purge deleted nothing, naming why.
fn nothing_purged(too_old: usize) -> String {
    match too_old {
        0 => "No messages matched, so nothing was deleted.".into(),
        n => format!("Nothing was deleted: the {n} messages that matched are older than 14 days, which Discord does not let bots bulk delete."),
    }
}

/// The purge details the mod log entry adds: where, how many and the filter (`send_log` adds the time).
pub(crate) fn purge_log(
    log: Log,
    channel: Id<ChannelMarker>,
    done: &purge::Purged,
    what: &str,
) -> Log {
    let (channel, deleted) = (format!("<#{channel}>"), format!("`{}`", done.deleted));
    let too_old = format!("`{}` (older than 14 days)", done.too_old);
    log.inline("Channel:", &channel)
        .inline("Deleted:", &deleted)
        .inline("Not deleted:", &too_old)
        .field("Filter:", what)
        .var("channel", &channel)
        .var("deleted", &deleted)
        .var("too_old", &too_old)
        .var("filter", what)
}

/// A purge's note in its channel, removed after 10 s (owner decision 11-17).
async fn post_briefly(
    app: &App,
    gid: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
    text: &str,
    embeds: &[Embed],
) {
    let framed = crate::reply::command_reply(app, true, text, embeds).await;
    let (text, embeds) = framed.as_ref().map_or((text, embeds), |f| (&f.0, &f.1));
    let sent = async {
        let m = app
            .http
            .create_message(channel)
            .content(text)
            .embeds(embeds)
            .allowed_mentions(Some(&AllowedMentions::default()))
            .await?
            .model()
            .await?;
        crate::reply::delete_later(app, gid, channel, m.id, 10).await
    };
    if let Err(e) = sent.await {
        tracing::warn!(?e, "posting the purge note failed");
    }
}

#[cfg(test)]
#[path = "modcmds_tests.rs"]
mod tests;

/// A command that rewrites case history counts in moderator stats, like the dashboard's edits.
/// Nothing is recorded when the case did not exist; a failed write is only logged.
async fn stat(app: &App, g: i64, by: u64, kind: &str, target: Option<i64>, case_no: Option<i32>) {
    if target.is_none() {
        return;
    }
    let r = async {
        cases::record(
            &mut *app.pool.acquire().await?,
            cases::ModAction {
                guild_id: g,
                mod_id: by as i64,
                kind,
                source: "bot",
                target_id: target,
                case_no,
                audit_id: None,
                ai_client: None,
            },
        )
        .await
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(?e, kind, "recording a case change for mod stats failed");
    }
}

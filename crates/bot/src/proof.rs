//! Case proof (review): the copy of a punished member's own message the bot stores as
//! verified proof, and the mod log entry of every case edit and proof change.
use anyhow::Result;
use brim_core::{
    cases::{self, CaseDelete, CaseLog, MessageCopy},
    notes::ai_label,
};
use brim_logging::{
    embed::{self, Log, GREEN, ORANGE, RED},
    LogKind,
};
use twilight_model::{
    channel::Message,
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
        Id,
    },
};

use crate::app::App;

/// What is kept of a message: its text, where it was, and the names of its files. The files
/// themselves are not copied.
pub(crate) fn copy_of(m: &Message) -> MessageCopy<'_> {
    MessageCopy {
        channel_id: m.channel_id.get() as i64,
        message_id: Some(m.id.get() as i64),
        text: &m.content,
        files: m.attachments.iter().map(|a| a.filename.clone()).collect(),
    }
}

/// Stores `copy` as verified proof of a case the bot just made. A failure is only logged: the
/// punishment and its case already stand.
pub(crate) async fn verified(
    app: &App,
    gid: Id<GuildMarker>,
    case_no: i32,
    copy: &MessageCopy<'_>,
) {
    if copy.text.trim().is_empty() && copy.files.is_empty() {
        return;
    }
    let by = (app.me.get() as i64, None);
    let r = cases::add_proof(
        &app.pool,
        gid.get() as i64,
        case_no,
        ("text", copy.text),
        by,
        Some(copy),
    )
    .await;
    if let Err(e) = r {
        tracing::warn!(?e, case_no, "storing the verified proof failed");
    }
}

/// The message a moderator's punishment replies to, read before the punishment runs: a ban
/// deletes the member's last messages, and after it there is nothing left to copy.
pub(crate) async fn replied(
    app: &App,
    (channel, message): (Id<ChannelMarker>, Id<MessageMarker>),
) -> Option<Message> {
    let fetched = async { anyhow::Ok(app.http.message(channel, message).await?.model().await?) };
    fetched
        .await
        .inspect_err(|e| tracing::warn!(?e, "reading the replied message for proof failed"))
        .ok()
}

/// The copy of `m` for a case about `target`: it is verified proof only when the punished member
/// wrote it. Another member's message is never stored on the case.
pub(crate) fn own_copy(m: &Message, target: Id<UserMarker>) -> Option<MessageCopy<'_>> {
    (m.author.id == target).then(|| copy_of(m))
}

/// The mod log entry of a case edit or proof change (command or dashboard).
pub(crate) fn case_log(c: &CaseLog) -> Log {
    let (title, color) = match c {
        CaseLog {
            proof_added: Some(_),
            ..
        } => ("Case Proof Added", GREEN),
        CaseLog {
            proof_removed: Some(_),
            ..
        } => ("Case Proof Removed", RED),
        _ => ("Case Updated", ORANGE),
    };
    let mut log = Log::new(title, color)
        .line("Case", &format!("`{}`", c.case_no))
        .line("Moderator", &embed::user(c.by))
        .var("case", &c.case_no.to_string())
        .var("moderator", &embed::user(c.by));
    // The values an edited version of the entry uses; empty ones drop their line.
    let (mut written_by, mut state, mut proof, mut removed, mut changes) = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    if let Some(ai) = &c.ai {
        written_by = ai_label(ai);
        log = log.line("Written By", &written_by);
    }
    if let Some(open) = c.active {
        state = if open { "Open again" } else { "Closed" }.into();
        log = log.line("State", &state);
    }
    if let Some(p) = &c.proof_added {
        proof = embed::clip(p, 1000);
        log = log.line("Proof", &proof);
    }
    if let Some(kind) = &c.proof_removed {
        removed = kind.clone();
        log = log.line("Removed", kind);
    }
    if let Some((old, new)) = &c.reason {
        changes = Log::new("", 0)
            .section("Reason")
            .diff(old.as_deref(), new.as_deref())
            .lines_text();
        log = log.text(&changes);
    }
    log.var("written_by", &written_by)
        .var("state", &state)
        .var("proof", &proof)
        .var("removed", &removed)
        .var("changes", &changes)
}

/// Sends [`case_log`] to the mod log.
pub(crate) async fn send_case_log(app: &App, gid: u64, c: &CaseLog) -> Result<()> {
    crate::events::send_log(app, gid, LogKind::ModCommand, case_log(c)).await
}

/// The mod log entry of a case a staff member deleted in the dashboard. The case's own reason is
/// shown; proofs and message copies are not.
pub(crate) fn case_delete_log(c: &CaseDelete) -> Log {
    // A case without a member (member 0: a purge of a channel, a slowmode) shows `None`.
    let member = match c.user {
        0 => "`None`".to_string(),
        u => embed::user(u),
    };
    let mut log = Log::new("Case Deleted", RED)
        .line("Case", &format!("`{}`", c.case_no))
        .line("Kind", &c.kind)
        .line("Member", &member)
        .user("Moderator", c.moderator)
        .user("Deleted By", c.by)
        .var("case", &c.case_no.to_string())
        .var("kind", &c.kind)
        .var("member", &member)
        .var("moderator", &embed::user(c.moderator))
        .var("deleted_by", &embed::user(c.by));
    let mut reason = String::new();
    if let Some(r) = c.reason.as_deref().filter(|r| !r.is_empty()) {
        reason = embed::clip(r, 1000);
        log = log.line("Reason", &reason);
    }
    log.var("reason", &reason)
}

/// Sends [`case_delete_log`] to the mod log.
pub(crate) async fn send_case_delete_log(app: &App, gid: u64, c: &CaseDelete) -> Result<()> {
    crate::events::send_log(app, gid, LogKind::ModCommand, case_delete_log(c)).await
}

/// One line for a retention sweep run that removed `count` cases older than `days` days.
pub(crate) async fn send_old_cases_log(app: &App, gid: u64, count: u64, days: i64) -> Result<()> {
    crate::events::send_log(app, gid, LogKind::ModCommand, old_cases_log(count, days)).await
}

pub(crate) fn old_cases_log(count: u64, days: i64) -> Log {
    Log::new("Old Cases Removed", RED)
        .line("Cases Removed", &count.to_string())
        .line("Older Than", &format!("{days} days"))
        .line("Reason", "The server's retention limit for cases")
        .var("count", &count.to_string())
        .var("days", &days.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        automod::more_tests::msg,
        modcmds::{self, ModCmd},
        test_support::T,
    };
    use brim_core::permits::Level;
    use serde_json::json;

    #[test]
    fn a_reply_is_proof_only_when_the_punished_member_wrote_it() {
        let m = msg(json!({"content": "scam", "attachments": []}));
        assert_eq!(own_copy(&m, Id::new(50)).map(|c| c.text), Some("scam"));
        assert!(
            own_copy(&m, Id::new(51)).is_none(),
            "another member's message"
        );
    }

    /// (kind, body, verified, added_by) of every proof in server 0.
    pub(crate) async fn proofs(t: &T) -> Vec<(String, String, bool, i64)> {
        sqlx::query_as("SELECT kind, body, verified, added_by FROM case_proofs WHERE guild_id = $1 ORDER BY id")
            .bind(t.g(0).get() as i64)
            .fetch_all(&t.app.pool)
            .await
            .unwrap()
    }

    /// `proof`, `editc` and `closecase` change the case, write its history and refuse a case about
    /// the moderator.
    #[tokio::test]
    async fn case_commands_keep_proofs_and_history() {
        let Some(t) = T::new(999).await else { return };
        let g = t.g(0).get() as i64;
        t.server_config(0, t.owner(), json!({})).await;
        t.exec("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, reason) VALUES ($1, 1, 'ban', 77, 9, 'old'), ($1, 2, 'warn', 9, 8, NULL)")
            .await;
        let level = Level::Custom {
            rank: 1,
            perms: vec!["cases".into()],
            whitelist: vec![],
            limits: Default::default(),
        };
        // Boxed: each call below would else put one copy of the large command future on the stack.
        let run = |cmd| {
            let (t, level) = (&t, &level);
            Box::pin(async move {
                let r = modcmds::run(
                    &t.app,
                    t.g(0),
                    Id::new(t.owner()),
                    (Id::new(9), level),
                    Id::new(7),
                    cmd,
                    (None, None),
                    false,
                );
                r.await.unwrap().text
            })
        };
        let add = |case, text: &str| ModCmd::ProofAdd {
            case,
            text: text.into(),
        };
        assert_eq!(
            run(add(1, "https://example.com/a.png")).await,
            "Proof added to case #1"
        );
        assert_eq!(
            run(add(1, "said it in voice")).await,
            "Proof added to case #1"
        );
        assert_eq!(run(add(5, "x")).await, "No case #5");
        // Nothing after the number lists the proofs, also of a case about the caller.
        let list = run(add(1, "")).await;
        assert!(list.starts_with("Proofs of case #1:"), "{list}");
        assert!(list.contains("https://example.com/a.png") && list.contains("said it in voice"));
        assert!(run(add(2, "")).await.contains("has no proof"));
        assert_eq!(run(add(5, "")).await, "No case #5");
        assert!(run(add(2, "x")).await.contains("it is about you"));
        assert_eq!(
            proofs(&t).await,
            [
                ("link".into(), "https://example.com/a.png".into(), false, 9),
                ("text".into(), "said it in voice".into(), false, 9)
            ]
        );
        let edit = |reason: &str| ModCmd::CasesEdit {
            case: 1,
            reason: reason.into(),
        };
        run(edit("new")).await;
        run(edit("new")).await;
        run(ModCmd::CaseSet {
            case: 1,
            active: false,
        })
        .await;
        run(ModCmd::CaseSet {
            case: 1,
            active: false,
        })
        .await;
        let edits: Vec<(String, Option<String>, Option<String>, i64)> = sqlx::query_as(
            "SELECT field, old_value, new_value, editor_id FROM case_edits WHERE guild_id = $1 ORDER BY id",
        )
        .bind(g)
        .fetch_all(&t.app.pool)
        .await
        .unwrap();
        let s = |x: &str| Some(x.to_string());
        assert_eq!(
            edits,
            [
                ("reason".into(), s("old"), s("new"), 9),
                ("active".into(), s("open"), s("closed"), 9)
            ]
        );
        t.cleanup().await;
    }

    #[test]
    fn case_delete_and_retention_logs_say_what_went() {
        let text = |l: Log| serde_json::to_string(&l.build()).unwrap();
        let c = CaseDelete {
            case_no: 4,
            kind: "ban".into(),
            user: 5,
            moderator: 6,
            by: 7,
            reason: Some("spam".into()),
        };
        let t = text(case_delete_log(&c));
        for want in ["Case Deleted", "`4`", "ban", "<@5>", "<@6>", "<@7>", "spam"] {
            assert!(t.contains(want), "{want}: {t}");
        }
        let t = text(case_delete_log(&CaseDelete { reason: None, ..c }));
        assert!(!t.contains("Reason"));
    }

    #[test]
    fn case_logs_show_the_change() {
        let text = |c: &CaseLog| serde_json::to_string(&case_log(c).build()).unwrap();
        let edited = text(&CaseLog {
            case_no: 4,
            by: 9,
            reason: Some((Some("old".into()), None)),
            active: Some(false),
            ..Default::default()
        });
        assert!(
            edited.contains("Case Updated")
                && edited.contains("- old")
                && edited.contains("Closed"),
            "{edited}"
        );
        let removed = text(&CaseLog {
            proof_removed: Some("verified message".into()),
            ..Default::default()
        });
        assert!(removed.contains("Case Proof Removed") && removed.contains("verified message"));
    }
}

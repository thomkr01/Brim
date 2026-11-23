//! D1: the moderation logs and answers read exactly as the code builds them until a server edits
//! the entry, and an edit keeps their facts.
use brim_core::{
    cases::{CaseDelete, CaseLog},
    notes::{NoteAction, NoteLog},
};
use brim_logging::{botmsg, embed::Log};
use serde_json::{json, Value};
use twilight_model::channel::message::Embed;

use crate::{
    commands::mod_log_embed,
    modcmds::purge_log,
    modview::note_log,
    prefix::Flags,
    proof::{case_delete_log, case_log, old_cases_log},
    purge::Purged,
    reply::{self, Applied, DmOutcome, ModTexts},
};

/// The default entry filled with the log's own values, laid beside the log as the code builds it.
pub(crate) fn same(key: &str, log: Log, colour: bool) {
    let log = log.time();
    let built = log.clone().build();
    let (_, extra) = log.meta();
    let mut vars = vec![
        ("title", built.title.as_deref().unwrap_or("")),
        ("text", built.description.as_deref().unwrap_or("")),
    ];
    vars.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let m = botmsg::render(key, None, &vars);
    let e = reply::parts(&m).1.remove(0);
    assert_eq!(e.title, built.title, "{key} title");
    assert_eq!(e.description, built.description, "{key} description");
    let f = |e: &Embed| {
        e.fields
            .iter()
            .map(|f| (f.name.clone(), f.value.clone(), f.inline))
            .collect::<Vec<_>>()
    };
    assert_eq!(f(&e), f(&built), "{key} fields");
    if colour {
        assert_eq!(e.color, built.color, "{key} colour");
    }
}

#[test]
fn every_moderation_log_default_reads_as_the_code_builds_it() {
    let case = "log.mod_command.case";
    same(
        case,
        mod_log_embed((12, "ban"), (5, 6), Some(3600), Some("spam")),
        false,
    );
    same(case, mod_log_embed((13, "kick"), (5, 6), None, None), false);
    let done = Purged {
        deleted: 12,
        too_old: 2,
        files: vec![],
    };
    let purge = purge_log(
        mod_log_embed((14, "purge"), (5, 6), None, Some("clean up")),
        twilight_model::id::Id::new(9),
        &done,
        "from <@6>",
    );
    same(case, purge, false);
    // A purge of a channel has no member: Target shows `None`, not the moderator.
    let channel = mod_log_embed((15, "purge"), (5, 0), None, None);
    same(case, channel.clone(), false);
    let target = channel.build().fields[3].value.clone();
    assert_eq!(target, format!("{} `None`", brim_logging::embed::ARROW));

    let cases = [
        CaseLog {
            case_no: 4,
            by: 5,
            reason: Some((Some("old".into()), Some("new".into()))),
            active: Some(false),
            ai: Some("client".into()),
            ..Default::default()
        },
        CaseLog {
            case_no: 4,
            by: 5,
            proof_added: Some("https://example.com/p.png".into()),
            ..Default::default()
        },
        CaseLog {
            case_no: 4,
            by: 5,
            proof_removed: Some("image".into()),
            ..Default::default()
        },
        CaseLog {
            case_no: 4,
            by: 5,
            reason: Some((None, Some("added".into()))),
            ..Default::default()
        },
    ];
    for c in &cases {
        let log = case_log(c);
        let title = log.clone().build().title.unwrap();
        same(
            &botmsg::log_type_key("log.mod_command", &title).unwrap(),
            log,
            true,
        );
    }
    for reason in [Some("spam".to_string()), None] {
        let c = CaseDelete {
            case_no: 7,
            kind: "ban".into(),
            user: 6,
            moderator: 5,
            by: 8,
            reason,
        };
        same("log.mod_command.case_deleted", case_delete_log(&c), true);
    }
    same(
        "log.mod_command.old_cases_removed",
        old_cases_log(5, 30),
        true,
    );

    let notes = [
        NoteLog {
            action: NoteAction::Add,
            id: Some("q413p9".into()),
            user: Some(6),
            by: 5,
            text: Some("keeps pinging".into()),
            ai: Some("client".into()),
            ..Default::default()
        },
        NoteLog {
            action: NoteAction::Modify,
            id: Some("q413p9".into()),
            user: Some(6),
            by: 5,
            old: Some("before".into()),
            text: Some("after".into()),
            author: Some(9),
            ..Default::default()
        },
        NoteLog {
            action: NoteAction::Remove,
            id: Some("q413p9".into()),
            user: Some(6),
            by: 5,
            ..Default::default()
        },
        NoteLog {
            action: NoteAction::Sweep,
            by: 5,
            count: Some(3),
            ..Default::default()
        },
    ];
    for n in &notes {
        let log = note_log(n);
        let title = log.clone().build().title.unwrap();
        same(
            &botmsg::log_type_key("log.mod_command", &title).unwrap(),
            log,
            true,
        );
    }
}

fn applied(case: i32, target: u64, extra: &str) -> Applied {
    Applied {
        target: Some(target),
        case: Some(case),
        text: format!("Case #{case}: ban <@{target}>{extra}"),
        dm: DmOutcome::Sent,
        posted: false,
    }
}

/// A texts holder whose three entries are the defaults, as a server that saved them untouched.
fn untouched() -> ModTexts {
    ModTexts::from_defaults()
}

#[test]
fn every_moderation_answer_default_reads_as_the_code_builds_it() {
    let done = [
        applied(1, 5, "\nWarning threshold reached"),
        applied(2, 6, ""),
        Applied::from("Cannot ban <@7>: they are immune".to_string()),
    ];
    let flags = Flags {
        hack: true,
        ..Default::default()
    };
    for reason in [Some("spam"), None] {
        for set in [&done[..], &done[1..2], &done[2..]] {
            let want = reply::result_embeds("ban", set, (flags, Some(60)), reason, 9);
            let got =
                reply::result_embeds_with(&untouched(), "ban", set, (flags, Some(60)), reason, 9);
            assert_eq!(json!(want), json!(got));
        }
        {
            let compact = true;
            let want = reply::summary_with(&ModTexts::default(), "ban", compact, &done, reason, 9);
            let got = reply::summary_with(&untouched(), "ban", compact, &done, reason, 9);
            assert_eq!(json!(want.0), json!(got.0));
            let one = &done[..1];
            let want = reply::summary_with(&ModTexts::default(), "ban", compact, one, reason, 9);
            let got = reply::summary_with(&untouched(), "ban", compact, one, reason, 9);
            assert_eq!(want.0, got.0);
        }
    }
}

#[test]
fn an_edited_answer_is_used_and_still_shows_who_failed() {
    let done = [
        applied(1, 5, ""),
        Applied::from("Cannot ban <@7>".to_string()),
    ];
    let edited = json!({"content": "", "embeds": [{"title": "{title} (edited)",
        "description": "{reason} / {moderator}\n{details}\n{successful}\n{unsuccessful}"}]});
    assert!(botmsg::missing_text("reply.mod.result", &edited).is_none());
    let t = ModTexts::with("reply.mod.result", edited);
    let e = reply::result_embeds_with(&t, "ban", &done, (Flags::default(), None), Some("r"), 9);
    assert_eq!(e[0].title.as_deref(), Some("Ban result: (edited)"));
    let d = e[0].description.clone().unwrap();
    assert!(d.contains("<@5>") && d.contains("Cannot ban <@7>") && d.starts_with("r / <@9>"));
    // An edit without the failures, the moderator or the reason is refused.
    for lost in [
        "{reason} {moderator} {details} {successful}",
        "{moderator} {details} {successful} {unsuccessful}",
    ] {
        let m = json!({"content": "", "embeds": [{"title": "{title}", "description": lost}]});
        assert!(
            botmsg::missing_text("reply.mod.result", &m).is_some(),
            "{lost}"
        );
    }
    let line = json!({"content": "Case {case} done: {kind} {member}\n{rest}", "embeds": []});
    let t = ModTexts::with("reply.mod.case", line);
    let s = reply::summary_with(&t, "ban", true, &done[..1], None, 9);
    assert_eq!(s.0, "Case 1 done: ban <@5>");
}

#[test]
fn an_edited_log_keeps_its_facts_and_old_frames_still_work() {
    let key = "log.mod_command.case";
    let d = botmsg::find(key).unwrap();
    assert!(d.strict() && d.details());
    assert!(botmsg::missing_text(key, &d.template).is_none());
    // The reason or the case number may not be dropped, nor hidden in a spoiler.
    let lost = json!({"content": "", "embeds": [{"fields": [
        {"name": "n", "value": "{case} {kind} {member} {moderator} {duration}"}]}]});
    assert!(botmsg::missing_text(key, &lost).is_some());
    let spoiler = json!({"content": "", "embeds": [{"description":
        "||{reason}|| {case} {kind} {member} {moderator} {duration}"}]});
    assert!(botmsg::missing_text(key, &spoiler).is_some());
    let own = json!({"content": "", "embeds": [{"description":
        "#{case}: {moderator} {kind} {member} for {duration}: {reason}"}]});
    assert!(botmsg::missing_text(key, &own).is_none());
    // A frame made before the entry had placeholders holds every fact in `{text}`.
    let frame = json!({"content": "", "embeds": [{"title": "{title}", "description": "{text}"}]});
    assert!(botmsg::missing_text("log.mod_command.case_deleted", &frame).is_none());
    // A field with an empty value is dropped, not sent.
    let m = botmsg::render(
        key,
        None,
        &[
            ("case", "1"),
            ("kind", "Ban"),
            ("member", "m"),
            ("moderator", "x"),
            ("duration", ""),
            ("reason", "r"),
            ("when", "t"),
            ("channel", ""),
            ("deleted", ""),
            ("too_old", ""),
            ("filter", ""),
        ],
    );
    let names: Vec<&str> = m["embeds"][0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 5, "{names:?}");
    let _: Value = m;
}

#[test]
fn review_fixes_hold() {
    // The action word is required in the short result.
    let lost =
        json!({"content": "{ok} {failed} {reason_line} {moderator} {failures}", "embeds": []});
    assert!(botmsg::missing_text("reply.mod.total", &lost).is_some());
    // A field whose name or value is empty goes, so a fact in a dropped field is not trusted.
    let key = "log.mod_command.case";
    let hidden = json!({"content": "", "embeds": [{"description": "{case} {kind} {member} {moderator} {duration}",
        "fields": [{"name": "Reason: {reason}", "value": "{filter}"}]}]});
    assert!(botmsg::missing_text(key, &hidden).is_none());
    let m = botmsg::render(
        key,
        Some(&hidden),
        &[
            ("case", "1"),
            ("kind", "Kick"),
            ("member", "m"),
            ("moderator", "x"),
            ("duration", ""),
            ("reason", "spam"),
            ("filter", ""),
            ("when", "t"),
            ("channel", ""),
            ("deleted", ""),
            ("too_old", ""),
            ("filter", ""),
        ],
    );
    // The default is sent: the edit lost the reason.
    assert!(m.to_string().contains("spam"));
    // An old frame with {text} in a field still passes, without hiding characters check.
    let frame =
        json!({"content": "", "embeds": [{"title": "{title}", "description": "{text}\u{200b}"}]});
    assert!(botmsg::missing_text("log.mod_command.case_deleted", &frame).is_none());
    // Too many embeds are cut to Discord's 10.
    let many = json!({"content": "", "embeds": (0..10).map(|_| json!({"title": "{title}",
        "description": "{reason} {moderator} {details} {successful} {unsuccessful}"})).collect::<Vec<_>>()});
    let done = [applied(1, 5, "")];
    let t = ModTexts::with("reply.mod.result", many);
    let e = reply::result_embeds_with(&t, "ban", &done, (Flags::default(), None), None, 9);
    assert_eq!(e.len(), 10);
}

/// D2: the server event logs the bot builds itself read as their default.
#[test]
fn every_bot_made_event_log_default_reads_as_the_code_builds_it() {
    let key = |k: &str| format!("log.{k}");
    same(
        &key("member.member_renamed"),
        crate::dehoist::renamed_log(6, "!alex", "alex", "Auto dehoist"),
        false,
    );
    for (case, now) in [(Some(4), 200), (None, 100_000)] {
        same(
            &key("member.timeout_ended"),
            crate::timeout_end::log(6, 100, 200, case, now),
            false,
        );
    }
    same(
        &key("user.user_avatar_changed"),
        crate::profile_log::avatar_log(6, Some("a"), Some("b")),
        false,
    );
    let old = ("alex".to_string(), None, None);
    let new = ("alexa".to_string(), Some("Alexa".to_string()), None);
    same(
        &key("user.user_name_changed"),
        crate::profile_log::name_log(6, &old, &new),
        false,
    );
    for k in [
        "member.member_renamed",
        "member.timeout_ended",
        "user.user_avatar_changed",
        "user.user_name_changed",
    ] {
        assert!(botmsg::find(&key(k)).unwrap().details(), "{k}");
    }
}

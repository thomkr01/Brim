//! D3: the automod, Discord AutoMod rule, Join Gate and verification logs read exactly as the code
//! builds them until a server edits the entry, and an edit keeps their facts.
use brim_automod::join::Action;
use brim_core::sections::FilterAction;
use brim_logging::{botmsg, embed::Log};
use serde_json::json;

use crate::{
    automod, joingate,
    mod_texts_tests::same,
    native,
    verify::{self, given},
};

/// The entry of `log`: its own, else the one its title names in `group`.
pub(crate) fn key(group: &str, log: &Log) -> String {
    let own = log.meta().0.map(String::from);
    let title = log.clone().build().title.unwrap_or_default();
    let k = own
        .or_else(|| botmsg::log_type_key(&format!("log.{group}"), &title))
        .unwrap_or_else(|| panic!("{title} has no entry"));
    assert!(botmsg::FACTS.contains(&k.as_str()), "{k} is not a D3 entry");
    k
}

pub(crate) fn check(group: &str, log: Log, colour: bool) {
    same(&key(group, &log), log, colour);
}

fn user(id: u64, name: &str) -> twilight_model::user::User {
    serde_json::from_value(
        json!({"id": id.to_string(), "username": name, "discriminator": "0", "avatar": null}),
    )
    .unwrap()
}

#[test]
fn every_d3_log_default_reads_as_the_code_builds_it() {
    let err = anyhow::anyhow!("Missing Permissions");
    for a in [
        FilterAction::Delete,
        FilterAction::Warn,
        FilterAction::Timeout,
        FilterAction::Kick,
        FilterAction::Ban,
    ] {
        let log = automod::filter_log("alex", a, "Use of blacklisted words", (7, "you all"), 6);
        check("auto_mod", log, true);
    }
    check(
        "auto_mod",
        automod::timeout_log("alex", "Spam", (6, 7), 60_000, 2),
        true,
    );
    check(
        "auto_mod",
        automod::strike_log("A member", "Spam", 6, 1),
        true,
    );
    let strike = automod::strike_log("alex", "Spam", 6, 1)
        .line("Channel", &brim_logging::embed::channel(7))
        .var("channel", &brim_logging::embed::channel(7));
    check("auto_mod", strike, true);
    check(
        "auto_mod",
        automod::cut_log(&["1".into(), "2".into()]),
        true,
    );
    check("auto_mod", automod::sync_failed_log(&err), true);
    check(
        "auto_mod",
        automod::warn_action_log(6, "Timed out for 1 hour"),
        true,
    );
    check("auto_mod", automod::action_failed_log(6, "ban", &err), true);
    check("auto_mod", native::not_approved_log(5), true);
    check("auto_mod", native::not_read_log(6, 5), true);
    check("auto_mod", native::no_permit_log(6, 5), true);
    check("auto_mod", native::report_log(6, 5, "bad\nwords"), true);
    check("member", joingate::invite_log(6, "abc", 8, None), true);
    check("member", joingate::invite_log(6, "abc", 8, Some(4)), true);
    let bot = joingate::Detail {
        filters: &["Account age", "Avatar"],
        is_bot: true,
        adder: Some(8),
    };
    let plain = joingate::Detail::default();
    check(
        "join_gate",
        joingate::gate_log(6, Action::Kick, "join gate", plain, None),
        true,
    );
    check(
        "join_gate",
        joingate::gate_log(
            6,
            Action::Ban,
            "join gate",
            bot,
            Some("Missing Permissions"),
        ),
        false,
    );
    check("verification", verify::alt_log(6, "<@8>, <@9>"), true);
    let u = user(6, "alex");
    check("verification", verify::passed_log(&u), true);
    check(
        "verification",
        verify::not_verified_log(&u, "Kick", ""),
        true,
    );
    check(
        "verification",
        verify::not_verified_log(&u, "Kick", " (their DMs are closed)"),
        true,
    );
    check("verification", given::role_taken_log(6, 8, None), true);
    check(
        "verification",
        given::role_taken_log(6, 8, Some(&err)),
        true,
    );
    for what in [given::BOT_TAKEN, given::OFFLINE_TAKEN, given::CHANNEL_TAKEN] {
        check("verification", given::bot_taken_log(6, 8, what), true);
    }
    check("verification", given::catch_up_cap_log(), true);
    check(
        "verification",
        given::take_back_failed_log(6, 8, &err),
        true,
    );
}

/// An edit names the facts, and one that drops a required fact is refused.
#[test]
fn a_d3_edit_fills_its_facts_and_keeps_them() {
    let k = "log.join_gate.join_gate";
    let ok = json!({"embeds": [{"title": "Gate", "description": "{member} {kind} {reason} {filters} {bot} {refused}"}]});
    assert!(botmsg::missing_text(k, &ok).is_none());
    let m = botmsg::render(
        k,
        Some(&ok),
        &[
            ("member", "M"),
            ("kind", "Kick"),
            ("reason", "R"),
            ("filters", ""),
            ("bot", ""),
            ("refused", ""),
        ],
    );
    assert_eq!(m["embeds"][0]["description"], "M Kick R   ");
    let bad = json!({"embeds": [{"title": "Gate", "description": "{member} {kind}"}]});
    assert!(botmsg::missing_text(k, &bad).is_some());
}

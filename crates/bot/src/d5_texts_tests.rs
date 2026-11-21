//! D5: the lockdown, auto slowmode, backup and scheduled action logs read exactly as the code
//! builds them until a server edits the entry.
use crate::{d3_texts_tests::check, lockdown, scheduler, slowmode};

#[test]
fn every_d5_log_default_reads_as_the_code_builds_it() {
    check("server", scheduler::created_log("a1b2c3"), true);
    let err = anyhow::anyhow!("Missing Permissions");
    check("mod_command", lockdown::state_log(false), true);
    check("mod_command", lockdown::state_log(true), true);
    check("server", lockdown::incomplete_log(3), true);
    check(
        "server",
        lockdown::startup_failed_log("verify_catch_up", 5),
        true,
    );
    check("server", scheduler::give_up("unban", &err, 8), true);
    check(
        "server",
        scheduler::dropped("unban", Some(6), "Nobody with a permit asked for it."),
        true,
    );
    check(
        "server",
        scheduler::dropped("unban", None, "Too old."),
        true,
    );
    check(
        "server",
        scheduler::loaded_log(4, "Restored 12 channels."),
        true,
    );
    check(
        "server",
        scheduler::loaded_log(4, "The server is locked."),
        true,
    );
    check("auto_mod", slowmode::on_log(7, 10, 80), true);
    check("auto_mod", slowmode::off_log(7, "`5s` again"), true);
    check(
        "auto_mod",
        slowmode::off_log(7, "left as staff set it"),
        true,
    );
    check(
        "auto_mod",
        slowmode::failed_log("Auto Slowmode Failed", 7, &err),
        true,
    );
    check(
        "auto_mod",
        slowmode::failed_log("Auto Slowmode Off Failed", 7, &err),
        true,
    );
}

/// The default answer entry filled with the answer's own values, beside the embed the code builds.
fn answer_same(embed: twilight_model::channel::message::Embed, (key, vars): &lockdown::Entry) {
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let m = brim_logging::botmsg::render(key, None, &vars);
    assert!(brim_logging::botmsg::FACTS.contains(key), "{key}");
    let e = crate::reply::parts(&m).1.remove(0);
    assert_eq!(e.title, embed.title, "{key} title");
    assert_eq!(e.description, embed.description, "{key} description");
    assert_eq!(e.color, embed.color, "{key} colour");
}

#[test]
fn every_lock_answer_default_reads_as_the_code_builds_it() {
    use brim_automod::lockdown::LockKind;
    use std::time::Duration;
    use twilight_model::id::Id;
    for lift in [false, true] {
        for k in [None, Some(&LockKind::Role)] {
            let (e, entry) = lockdown::loading_embed(k, lift);
            answer_same(e, &entry);
        }
        for k in [LockKind::Role, LockKind::Channel(5)] {
            let r = lockdown::finished(
                Some(&k),
                "Roles".into(),
                lift,
                Duration::from_micros(390_110),
                Id::new(9),
            );
            answer_same(r.embed, &r.entry);
        }
        let r = lockdown::fail(lift, "Nothing to unlock.".into());
        answer_same(r.embed, &r.entry);
    }
    for active in [vec![], vec![LockKind::Role, LockKind::Server]] {
        let (e, entry) = lockdown::overview_embed(&active, "t!");
        answer_same(e, &entry);
    }
}

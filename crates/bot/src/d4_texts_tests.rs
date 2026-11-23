//! D4: the security logs (anti-nuke, panic, join raid, permit sync, NSFW and webhook spam) read
//! exactly as the code built them before they had entries, and an edit cannot drop a fact.
//! Each `old_*` log is the code as it was before D4, kept only as the reference.
use brim_logging::{
    botmsg,
    embed::{self, user, Log, ALERT, ARROW, DONE, NO, ORANGE, RED, YES},
    emoji::{PANIC, QUARANTINED},
    overlay::security_log,
};
use serde_json::{json, Value};
use twilight_model::channel::message::Embed;

use crate::{antinuke, automod, joingate, mod_texts_tests::same, nsfw, panic, verify, webhooks};

type Parts = (
    Option<String>,
    Option<String>,
    Option<u32>,
    Vec<(String, String, bool)>,
);

fn parts(e: &Embed) -> Parts {
    let f = e
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.value.clone(), f.inline))
        .collect();
    (e.title.clone(), e.description.clone(), e.color, f)
}

/// The new log reads as the old one, and its entry's default (filled with its values) as both.
fn check(old: Log, new: Log) {
    let key = new.meta().0.expect("a D4 log names its entry").to_string();
    assert!(botmsg::SECURITY_LOGS.iter().any(|l| l.0 == key), "{key}");
    assert_eq!(parts(&old.build()), parts(&new.clone().build()), "{key}");
    same(&key, new, true);
}

/// A security log built from its entry, beside the old builder.
fn sec(old: Log, title: &str, key: &str, vars: &[(&str, &str)]) {
    check(old, security_log(title, key, vars));
}

#[test]
fn every_anti_nuke_log_reads_as_before() {
    let (by, member) = (user(5), user(6));
    let an = |k: &str| format!("log.anti_nuke.{k}");
    let a = |t: &str| Log::new("Anti Nuke", ALERT).text(t);
    let alert = format!("{NO} **This server's settings could not be read**");
    sec(
        a(&alert).line("Effect", "Dangerous actions are saved for `/revert` but not counted, so nobody is quarantined until the settings can be read again."),
        "Anti Nuke",
        &an("settings_unread"),
        &[("alert", &alert)],
    );
    for repeats in [
        "",
        "taken back 6 times since <t:1:R>, not logged one by one",
    ] {
        let mut old = Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .user("Member", 6)
            .text("**Gave a member a role that carries a permit**")
            .line("Roles", "<@&7>")
            .line("Action", "taken back: rule. Why, so nobody was punished");
        if !repeats.is_empty() {
            old = old.line("Repeats", repeats);
        }
        sec(
            old,
            "Anti Nuke",
            &an("permit_role_taken_back"),
            &[
                ("by", &by),
                ("member", &member),
                ("alert", "**Gave a member a role that carries a permit**"),
                ("roles", "<@&7>"),
                ("action", "taken back: rule. Why, so nobody was punished"),
                ("repeats", repeats),
            ],
        );
    }
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .user("Member", 6)
            .text("**Removed a permit role from a member listed in the permit**")
            .line("Permit", "Mods")
            .line("Roles", "<@&7>")
            .line("Action", "given back: x")
            .line("Repeats", "given back 6 times"),
        "Anti Nuke",
        &an("permit_role_given_back"),
        &[
            ("by", &by),
            ("member", &member),
            (
                "alert",
                "**Removed a permit role from a member listed in the permit**",
            ),
            ("permit", "Mods"),
            ("roles", "<@&7>"),
            ("action", "given back: x"),
            ("repeats", "given back 6 times"),
        ],
    );
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .user("Member", 6)
            .text("**Slowed down reverting a member's permit roles**")
            .line("Action", "what"),
        "Anti Nuke",
        &an("permit_roles_slowed"),
        &[
            ("by", &by),
            ("member", &member),
            ("alert", "**Slowed down reverting a member's permit roles**"),
            ("action", "what"),
        ],
    );
    let alert = format!("{NO} **Changed a locked target during a lockdown**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .text(&alert)
            .line("Target", "<@&7>")
            .line("Change", "set back"),
        "Anti Nuke",
        &an("locked_target_changed"),
        &[
            ("by", &by),
            ("alert", &alert),
            ("target", "<@&7>"),
            ("result", "set back"),
        ],
    );
    let alert = format!("{NO} **A managed role holds dangerous permissions**");
    sec(
        a(&alert).line("Role", "<@&7>").line("Effect", "Enforce permits cannot take a bot or integration role off. Remove the permissions from the role instead."),
        "Anti Nuke",
        &an("managed_role_dangerous"),
        &[("alert", &alert), ("role", "<@&7>")],
    );
    let alert = format!("{YES} **Dangerous roles removed**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Member", 6)
            .text(&alert)
            .line("Roles", "<@&7>")
            .line("Result", "removed")
            .line("Reason", "holds dangerous permissions without a permit"),
        "Anti Nuke",
        &an("dangerous_roles_removed"),
        &[
            ("member", &member),
            ("alert", &alert),
            ("roles", "<@&7>"),
            ("result", "removed"),
        ],
    );
    let alert = format!("{NO} **Quarantine role missing**");
    sec(
        a(&alert).line("Effect", "Quarantined members lose their roles but get no Quarantine role. Pick or make a new one in the anti-nuke settings."),
        "Anti Nuke",
        &an("quarantine_role_missing"),
        &[("alert", &alert)],
    );
    let q = format!("{YES} **Quarantined**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .line("CASE", "`12`")
            .user("Responsible", 5)
            .text(&q)
            .line("Reason", "why"),
        "Anti Nuke",
        &an("quarantined"),
        &[
            ("case", "`12`"),
            ("by", &by),
            ("alert", &q),
            ("reason", "why"),
        ],
    );
    let alert = format!("{YES} **Reverted their last hour**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .text(&alert)
            .line("Reason", "command rate limit exceeded")
            .line("Reverted", "`2/3`")
            .line("Kicked, re-invite", "None"),
        "Anti Nuke",
        &an("rate_limit_reverted"),
        &[
            ("by", &by),
            ("alert", &alert),
            ("reverted", "`2/3`"),
            ("reinvite", "None"),
        ],
    );
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Member", 6)
            .text(&q)
            .line("Reason", "Rejoined the server while quarantined"),
        "Anti Nuke",
        &an("quarantined_rejoined"),
        &[("member", &member), ("alert", &q)],
    );
    let alert = format!("{NO} **Could not take all channel access from a quarantined member**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Member", 6)
            .text(&alert)
            .line("Error", "boom")
            .line(
                "Effect",
                "Their own or a managed role's overwrite may still let them in there. Check it.",
            ),
        "Anti Nuke",
        &an("channel_access_not_taken"),
        &[("member", &member), ("alert", &alert), ("error", "boom")],
    );
    let alert = format!("{NO} **A quarantined member's channel overwrite was not saved**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Member", 6)
            .text(&alert)
            .line("Channel", "<#8>")
            .line("Effect", "It will not come back on release. Had none."),
        "Anti Nuke",
        &an("overwrite_not_saved"),
        &[
            ("member", &member),
            ("alert", &alert),
            ("channel", "<#8>"),
            ("effect", "It will not come back on release. Had none."),
        ],
    );
    let alert = "**A limit was reached only because their permits could not be read**";
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .text(alert)
            .line(
                "Action",
                "they are immune, so nothing was undone and nobody was punished",
            ),
        "Anti Nuke",
        &an("immune_permits_unread"),
        &[("by", &by), ("alert", alert)],
    );
    sec(
        Log::new("Anti Nuke", ALERT)
            .line("CASE", "not saved")
            .user("Responsible", 5)
            .text(&q)
            .line(
                "Next",
                "contained by a retry; the earlier report said they were not",
            ),
        "Anti Nuke",
        &an("contained_by_retry"),
        &[("case", "not saved"), ("by", &by), ("alert", &q)],
    );
    let alert = format!("{NO} **The revert of this nuke failed**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .text(&alert)
            .line("Reason", "boom")
            .line("Next", "It is tried again by itself; `/revert` also works"),
        "Anti Nuke",
        &an("revert_failed"),
        &[("by", &by), ("alert", &alert), ("reason", "boom")],
    );
    let alert = format!("{NO} **This server's settings could not be read during a nuke**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .text(&alert)
            .line("Reason", "boom")
            .line("Effect", "The nuker was contained and reverted with the last settings loaded, or the defaults. Check the revert; `/revert` can run it again."),
        "Anti Nuke",
        &an("settings_unread_in_nuke"),
        &[("by", &by), ("alert", &alert), ("error", "boom")],
    );
    let alert = "**Gave roles to a member who has not verified yet**";
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .user("Member", 6)
            .text(alert)
            .line("Roles", "<@&7>")
            .line("Action", "taken back: the member is held until they verify. Anti-nuke is off, so nobody was punished"),
        "Anti Nuke",
        &an("unverified_roles_taken"),
        &[
            ("by", &by),
            ("member", &member),
            ("alert", alert),
            ("roles", "<@&7>"),
        ],
    );
    let alert =
        format!("{NO} **Could not take back channel access given to a quarantined member**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Responsible", 5)
            .user("Member", 6)
            .text(&alert)
            .line("Channel", "<#8>")
            .line("Error", "boom")
            .line(
                "Effect",
                "The member may have access to this channel. Check its overwrites.",
            ),
        "Anti Nuke",
        &an("overwrite_not_taken_back"),
        &[
            ("by", &by),
            ("member", &member),
            ("alert", &alert),
            ("channel", "<#8>"),
            ("error", "boom"),
        ],
    );
    for member in [String::new(), user(6)] {
        let mut old = Log::new("Anti Nuke", ALERT).user("Responsible", 5);
        if !member.is_empty() {
            old = old.user("Member", 6);
        }
        sec(
            old.text("**deleted a channel**").line("Action", "undone"),
            "Anti Nuke",
            &an("change_undone"),
            &[
                ("by", &by),
                ("member", &member),
                ("alert", "**deleted a channel**"),
                ("action", "undone"),
            ],
        );
    }
    let alert = format!("{NO} **Could not take back roles given to a quarantined member**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Member", 6)
            .text(&alert)
            .line("Error", "boom")
            .line(
                "Effect",
                "The member may hold roles again. Check them and quarantine again if needed.",
            ),
        "Anti Nuke",
        &an("roles_not_taken_back"),
        &[("member", &member), ("alert", &alert), ("error", "boom")],
    );
    let alert = format!("{YES} **View Mod Stats role removed**");
    sec(
        Log::new("Anti Nuke", ALERT)
            .user("Member", 6)
            .user("Given by", 5)
            .text(&alert)
            .line("Roles", "<@&7>")
            .line(
                "Reason",
                "only owners can give roles that see moderator stats",
            ),
        "Anti Nuke",
        &an("mod_stats_role_removed"),
        &[
            ("member", &member),
            ("by", &by),
            ("alert", &alert),
            ("roles", "<@&7>"),
        ],
    );
    sec(
        Log::new("Anti Nuke", ALERT)
            .text("**A permit is on a role Discord hands out**")
            .line("Roles", "<@&7>")
            .line("Effect", "the permit is ignored for these roles: Discord gives them through Onboarding, a linked role or a server subscription, so anyone could get the permit. It counts again once the role is no longer handed out that way; else put the permit on a staff role that only staff hand out."),
        "Anti Nuke",
        &an("permit_on_handed_out_role"),
        &[
            ("alert", "**A permit is on a role Discord hands out**"),
            ("roles", "<@&7>"),
        ],
    );
    let why = "Brim could not read the audit log after a restart or reconnect";
    sec(
        Log::new("Anti Nuke", ALERT).text(&format!("**{why}.** Actions made while it was away were not all checked by anti-nuke. Check the audit log.")),
        "Anti Nuke",
        &an("audit_gap"),
        &[("reason", why)],
    );
}

#[test]
fn the_nuke_report_reads_as_before() {
    use antinuke::{Applied, Contained, RevertSummary};
    for (bot_role, (missing, stripped), failed) in [
        (None, (0, 0), vec![]),
        (
            Some(true),
            (2, 1),
            vec![brim_antinuke::Inverse::Unban { user: 9 }],
        ),
    ] {
        let done = Contained {
            applied: Applied::Quarantined,
            bot_role,
        };
        let sum = RevertSummary {
            ops: 3,
            done: vec![],
            failed: failed.clone(),
            reinvite: vec![9],
            recreated: 1,
            stripped,
            missing,
        };
        let new = antinuke::report_log((Some(7), "abc"), 5, done, "why", &sum);
        let action = if bot_role.is_some() {
            format!("{YES} **Quarantined**, bot role cleared")
        } else {
            format!("{YES} **Quarantined**")
        };
        let mut old = Log::new("Anti Nuke Report:", ALERT)
            .line("CASE", "`7` (nuke `abc`)")
            .user("Responsible", 5)
            .text(&action)
            .line("Reason", "why")
            .section("More Details:")
            .line("Action Applied", &YES.to_string());
        if let Some(c) = bot_role {
            old = old.line("Bot Role Cleansing", &if c { YES } else { NO }.to_string());
        }
        old = old
            .section("Revert Report:")
            .line("Reverted", &format!("`{}/3`", 3 - failed.len()))
            .line("Recreated", "`1`")
            .line("Kicked, re-invite", "<@9>");
        if !failed.is_empty() {
            old = old.line("Could not undo", &format!("\n- Unban {}", user(9)));
        }
        if missing > 0 {
            old = old.line("Not recreated", &format!("`{missing}` deleted channels or roles did not come back: no backup holds them. Turn on Backups so anti-nuke can recreate them."));
        }
        if stripped > 0 {
            old = old.line("Older backup", &format!("`{stripped}` recreated roles or channels came from an older backup and got no dangerous permissions (role permissions or channel overwrite allows). Give them back by hand if they should have them."));
        }
        check(old.clone(), new.clone());
        let w = "the case or the nuke record could not be saved; see the bot's logs";
        check(
            old.line("Not saved", w),
            new.fact("Not saved", "not_saved", w),
        );
    }
}

fn old_panic_report(case: Option<&(String, Value)>, b: &panic::Backup) -> Log {
    let mut log = Log::new("Panic Mode Report:", DONE);
    if let Some((id, data)) = case {
        let ids = |k: &str| -> Vec<u64> {
            data[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .collect()
        };
        let (who, failed) = (ids("quarantined"), ids("failed"));
        log = log
            .text(&format!("{ARROW} **CASE:** `{id}`"))
            .text(&format!(
                "{QUARANTINED} **Members Quarantined:** {} {YES} | {} {NO}",
                who.len(),
                failed.len()
            ));
        for (u, mark) in who
            .iter()
            .map(|u| (u, QUARANTINED))
            .chain(failed.iter().map(|u| (u, NO)))
        {
            let frags = data["fragments"][u.to_string()].as_u64().unwrap_or(0);
            log = log.text(&format!(
                "{ARROW} {} [{frags} fragments] {mark}",
                embed::user(*u)
            ));
        }
    }
    let log = log.section("Backup System Report:");
    let sum = match b {
        panic::Backup::Disabled => {
            return log.text(&format!("{NO} **Backup System is disabled!**"))
        }
        panic::Backup::None => return log.text(&format!("{NO} **There is no backup to load.**")),
        panic::Backup::NotAllowed(by) => {
            let who = by.filter(|b| *b > 0).map_or("Someone".into(), embed::user);
            return log.text(&format!(
                "{NO} **The recent backup was not loaded:** {who} started panic mode by hand, and only the owner or an Extra Owner can load a backup."
            ));
        }
        panic::Backup::Failed(e) => {
            return log
                .text(&format!(
                    "{NO} **Backup System failed to load the recent backup!**"
                ))
                .line("Error", e)
        }
        panic::Backup::Loaded(sum) => sum,
    };
    let col = |c: crate::backup::Counts| {
        format!(
            "Deleted: `{}`\nRecovered: `{}`\nEdited: `{}`",
            c.deleted, c.created, c.updated
        )
    };
    let log = match sum.failures() {
        None => log.text(&format!(
            "{YES} **Backup System loaded the recent backup successfully!**"
        )),
        Some(f) => log
            .text(&format!(
                "{NO} **Backup System loaded the recent backup, but some steps failed!**"
            ))
            .line("Failed", &f),
    };
    log.inline("Roles", &col(sum.roles))
        .inline("Channels", &col(sum.channels))
        .inline("Categories", &col(sum.categories))
        .inline("Webhooks", &format!("Deleted: `{}`", sum.webhooks_deleted))
        .inline(
            "Settings",
            // Restores never touch Brim's own settings.
            &format!(
                "{ARROW} Server: {}\n{ARROW} Brim: {NO}{}",
                if sum.settings { YES } else { NO },
                if sum.icon_lost {
                    format!("\n{ARROW} Server icon: {NO} no copy kept")
                } else {
                    String::new()
                }
            ),
        )
}

#[test]
fn every_panic_log_reads_as_before() {
    let case = (
        "a1b2".to_string(),
        json!({"quarantined": [5], "failed": [6], "fragments": {"5": 3}}),
    );
    let mut loaded = crate::backup::Summary {
        webhooks_deleted: 2,
        settings: true,
        icon_lost: true,
        ..Default::default()
    };
    loaded.roles.deleted = 1;
    let mut partly = loaded.clone();
    partly.failed = 1;
    partly.first_error = Some("Missing Permissions".into());
    for b in [
        panic::Backup::Disabled,
        panic::Backup::None,
        panic::Backup::NotAllowed(Some(5)),
        panic::Backup::NotAllowed(None),
        panic::Backup::Failed("boom".into()),
        panic::Backup::Loaded(loaded),
        panic::Backup::Loaded(partly),
    ] {
        for c in [None, Some(&case)] {
            check(old_panic_report(c, &b), panic::report(c, &b));
        }
    }
    check(
        Log::new("Heat Panic Mode Triggered!", ALERT).text(&format!(
            "{ARROW} 4 raiders will be timed out on every message for {}.",
            embed::duration(600)
        )),
        panic::heat_log(4, 600),
    );
    check(
        Log::new("Panic Mode Triggered! [Anti Nuke]", ALERT)
            .text(&format!(
                "{PANIC} **An abnormal behaviour has been detected!**"
            ))
            .text(&format!(
                "{ARROW} Brim will lockdown the server and run a scan."
            )),
        panic::triggered_log(),
    );
    check(
        Log::new("Panic Mode Lockdown", ALERT)
            .text(&format!("{YES} **The server has been locked down.**")),
        panic::lockdown_log(),
    );
    let ended =
        Log::new("Panic Mode Ended", DONE).text(&format!("{YES} **Panic mode has ended.**"));
    check(ended.clone(), panic::end_log(&json!({})));
    check(
        ended.user("Ended by", 5),
        panic::end_log(&json!({"ended_by": "5"})),
    );
    for reason in ["Mention spam", "Reached strike `3`"] {
        check(
            Log::new("Server Locked", ALERT)
                .text(&format!("{YES} **The server has been locked down.**"))
                .user("Member", 6)
                .line("Reason", reason),
            automod::locked_log(6, reason),
        );
    }
}

#[test]
fn every_join_raid_log_reads_as_before() {
    let flagged = [(1_234_567_890_123u64, "alex"), (2, "sam")];
    for url in [None, Some("https://example.com/r".to_string())] {
        let flags = flagged
            .iter()
            .map(|(u, n)| format!("`{} | {n}`", crate::invites::short_id(*u)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut old = Log::new("🕵 Join Raid Trigger!", ALERT)
            .text(&format!("{YES} Brim has identified a weird join pattern."))
            .text(&format!("{ARROW} Next."))
            .text(&format!(
                "{ARROW} A full report will be posted once the duration ends."
            ))
            .field("Initial Flags:", &flags);
        old = old.field("Code:", "`abc`");
        if let Some(u) = &url {
            old = old.text(&format!("{ARROW} [Every caught account]({u})"));
        }
        check(
            old,
            joingate::trigger_log(
                ("🕵 Join Raid Trigger!", "join"),
                "Next.",
                &flagged,
                "abc",
                url,
            ),
        );
    }
    for code in [None, Some("abc")] {
        let old = Log::new("🕵 Join Raid Report!", DONE).text(&format!(
            "{YES} `4` users have been caught by the join raid."
        ));
        let old = match code {
            Some(c) => old.text(&format!("{ARROW} `{c}`")),
            None => old,
        };
        check(old, joingate::raid_report_log(4, code));
    }
}

#[test]
fn every_permit_and_list_log_reads_as_before() {
    let an = |k: &str| format!("log.anti_nuke.{k}");
    sec(
        Log::new("Anti Nuke", ALERT)
            .text("**Permit roles taken from members not listed in the permit**")
            .line("Taken", "<@6>: <@&7>")
            .line("Action", "a role alone gives no permit, so nobody was punished. To give someone a permit, add them to it on the dashboard"),
        "Anti Nuke",
        &an("permit_roles_taken"),
        &[
            (
                "alert",
                "**Permit roles taken from members not listed in the permit**",
            ),
            ("taken", "<@6>: <@&7>"),
        ],
    );
    sec(
        Log::new("Permits", ORANGE)
            .text("**Permits now belong to members, not roles**")
            .text("Each holder of a permit role was listed in the permit (one permit each). From now on Brim gives a permit's roles to its listed members and takes them from everyone else; change who is listed on the dashboard.")
            .line("Listed", "<@6>: Mods"),
        "Permits",
        &an("permits_moved"),
        &[
            ("alert", "**Permits now belong to members, not roles**"),
            ("listed", "<@6>: Mods"),
        ],
    );
    let gone = "<@6> left the server: removed Mods\n<@7> left the server: removed Extra Owner";
    sec(
        Log::new("Permits", ORANGE).text(gone),
        "Permits",
        &an("permit_holders_left"),
        &[("gone", gone)],
    );
    sec(
        Log::new("🤖 Bot check failed", ORANGE).text("Could not check if <@6>, <@7> are bots, so they stay listed. Only users can be Extra Owner, Trusted Admin or on the quarantine whitelist; the check runs again at the next start."),
        "🤖 Bot check failed",
        &an("bot_check_failed"),
        &[("accounts", "<@6>, <@7>")],
    );
    sec(
        Log::new("🤖 Bots removed from user-only lists", ORANGE).text("Bot accounts were removed: <@6> (Extra Owner). Only users can be Extra Owner, Trusted Admin or on the quarantine whitelist. A bot permit gives a bot exactly the whitelist flags you pick: add the bot as a member of a permit on the dashboard's Permits page."),
        "🤖 Bots removed from user-only lists",
        &an("bots_removed"),
        &[("accounts", "<@6> (Extra Owner)")],
    );
}

#[test]
fn every_channel_alert_reads_as_before() {
    let v = |text: &str| Log::new("Verification Channel Deleted", ALERT).text(text);
    let d = verify::deleted::deleted_log;
    let base = [
        ("channel.name", "verify"),
        ("by", "<@5>"),
        ("made", "Brim made a new one: <#8>."),
    ];
    let deleted =
        "The verification channel #verify was deleted by <@5>. Brim made a new one: <#8>.";
    check(
        v("The verification channel #verify was deleted and Brim could not make a new one: boom"),
        d(
            "not_remade",
            &[("channel.name", "verify"), ("error", "boom")],
        ),
    );
    check(
        v("The verification channel #verify was deleted again after 3 new ones in 10 minutes. Brim stopped making new ones. Check who is deleting it."),
        d("deleted_again", &[("channel.name", "verify"), ("count", "3")]),
    );
    check(
        v("Brim made the verification channel <#8> again, but could not set it up: boom. Press Setup Channel on the dashboard."),
        d("not_set_up", &[("channel", "<#8>"), ("error", "boom")]),
    );
    check(
        v(&format!("{deleted} Brim could not read the permit level of <@5>, so nobody was punished. Check who this is.")),
        d("level_unread", &base),
    );
    check(
        v("The verification channel #verify was deleted by the server owner. Brim made a new one: <#8>."),
        d("deleted_by_owner", &base),
    );
    check(
        v(&format!("{deleted} <@5> is trusted, so only this message was sent. If they delete it again, they will be quarantined.")),
        d("deleted_by_trusted", &base),
    );
    check(
        v(&format!("{deleted} Brim could not quarantine them: boom")),
        d(
            "not_quarantined",
            &[base[0], base[1], base[2], ("error", "boom")],
        ),
    );
    let p = |title: &str, text: &str| Log::new(title, ALERT).text(text);
    sec(
        p(
            "Log Channel Deleted",
            "The log channel #logs was deleted by <@5>. Brim did not make a new one.",
        ),
        "Log Channel Deleted",
        "log.anti_nuke.protected_channel_deleted",
        &[
            ("channel_kind", "log channel"),
            ("channel.name", "logs"),
            ("by", "<@5>"),
            ("made", "Brim did not make a new one."),
        ],
    );
    sec(
        p("Log Channel Locked", "Brim locked the log channel #logs: only Brim may delete messages or the channel there now."),
        "Log Channel Locked",
        "log.anti_nuke.log_channel_locked",
        &[("channel.name", "logs")],
    );
    sec(
        p("Log Channel Lock Changed", "<@5> changed the permissions on the log channel #logs so others could delete messages or the channel. Brim put the lock back."),
        "Log Channel Lock Changed",
        "log.anti_nuke.log_channel_lock_changed",
        &[("by", "<@5>"), ("channel.name", "logs")],
    );
    sec(
        p("Log Messages Deleted", "<@5> deleted 2 message(s) in the log channel <#8>. Deleted messages cannot be brought back."),
        "Log Messages Deleted",
        "log.anti_nuke.log_messages_deleted",
        &[("by", "<@5>"), ("count", "2"), ("channel", "<#8>")],
    );
}

#[test]
fn the_nsfw_and_webhook_spam_logs_read_as_before() {
    let s = [0.01, 0.02, 0.03, 0.9, 0.04];
    let pct = |i: usize| format!("{} {:.0}%", nsfw::CLASSES[i], s[i] * 100.0);
    let old = Log::new("NSFW Image Found", ALERT)
        .user("Member", 6)
        .line("Channel", "<#8>")
        .line(
            "Score",
            &format!(
                "`{:.0}%` ({}, {}, {})",
                nsfw::score(&s) * 100.0,
                pct(3),
                pct(1),
                pct(4)
            ),
        )
        .line("Scan", "`12`");
    check(old, nsfw::found_log(6, 8, 12, &s));
    check(
        Log::new("Webhooks Deleted", RED)
            .line("Count", "`3`")
            .line("Reason", "Webhook spam"),
        webhooks::deleted_log(3),
    );
}

/// Every D4 entry is a security text whose default passes its own save check and needs every fact
/// it shows, as plain text, with `{title}` first.
#[test]
fn every_security_log_entry_guards_its_facts() {
    for (key, ..) in botmsg::SECURITY_LOGS {
        let d = botmsg::find(key).unwrap();
        assert!(d.security && d.strict() && d.details(), "{key}");
        assert_eq!(d.required[0], "title", "{key}");
        assert!(botmsg::missing_text(key, &d.template).is_none(), "{key}");
        assert!(
            botmsg::unknown_vars(key, &botmsg::to_public(key, &d.template)).is_empty(),
            "{key}"
        );
        let with = |r: &str, to: &str| -> Value {
            serde_json::from_str(&d.template.to_string().replace(&format!("{{{r}}}"), to)).unwrap()
        };
        for r in &d.required {
            let gone = with(r, "x");
            assert!(
                botmsg::missing_text(key, &gone).is_some(),
                "{key} without {{{r}}}"
            );
            let hidden = with(r, &format!("||{{{r}}}||"));
            assert!(
                botmsg::missing_text(key, &hidden).is_some(),
                "{key} with {{{r}}} in a spoiler"
            );
        }
    }
}

/// A frame around `{text}` is no way around the fact rules: no security entry of facts offers it.
#[test]
fn a_text_frame_is_refused() {
    for d in botmsg::defs().iter().filter(|d| d.security && d.details()) {
        assert!(!d.vars.contains(&"text"), "{}", d.key);
    }
    for (key, ..) in botmsg::SECURITY_LOGS {
        let frame = json!({"embeds": [{"title": "Routine check", "description": "||{text}||"}]});
        assert!(botmsg::missing_text(key, &frame).is_some(), "{key}");
        assert_eq!(botmsg::unknown_vars(key, &frame), ["text"], "{key}");
    }
}

/// An edited version sends its own words with every fact; a value the log did not set never shows
/// as a raw `{name}`.
#[test]
fn an_edit_keeps_the_facts_and_never_sends_a_placeholder() {
    let key = "log.anti_nuke.permit_role_taken_back";
    // No `{repeats}` value, as a sender that forgot it.
    let log = Log::new("Anti Nuke", ALERT)
        .entry(key)
        .user_fact("Responsible", "by", 5)
        .user_fact("Member", "member", 6)
        .fact_text("alert", "**Gave a role**")
        .fact("Roles", "roles", "<@&7>")
        .fact("Action", "action", "taken back")
        .time();
    let (built, vars) = log.parts();
    let copy = json!({"content": "", "embeds": [{"title": "🚨 {title}", "description": "{alert}\n{by} gave {member} {roles}: {action}. {repeats}\n{when}"}]});
    assert!(botmsg::missing_text(key, &copy).is_none());
    let e = brim_logging::overlay::over(key, &copy, &built, &vars, true);
    assert_eq!(e.title.as_deref(), Some("🚨 Anti Nuke"));
    let d = e.description.unwrap();
    assert!(
        d.contains("**Gave a role**") && d.contains(&user(5)) && d.contains(&user(6)),
        "{d}"
    );
    assert!(!d.contains('{'), "{d}");
}

/// Every security log key the bot names exists, so `security_log` never falls back.
#[test]
fn every_named_security_log_exists() {
    fn walk(dir: &std::path::Path, out: &mut String) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push_str(&std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    let mut src = String::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut src,
    );
    let mut n = 0;
    let keys = ["\"log.anti_nuke.", "\"log.panic.", "\"log.join_raid."]
        .iter()
        .flat_map(|p| src.split(p).skip(1).map(move |r| (p, r)))
        .filter_map(|(p, r)| r.split_once('"').map(|(k, _)| format!("{}{k}", &p[1..])));
    for part in keys {
        let part = part.as_str();
        let named = !part.ends_with('.') && !part.contains('{');
        if named {
            assert!(botmsg::SECURITY_LOGS.iter().any(|l| l.0 == part), "{part}");
            n += 1;
        }
    }
    assert!(n > 30, "{n}");
    for part in include_str!("verify/deleted.rs")
        .split("deleted_log(\"")
        .skip(1)
    {
        let kind = part.split('"').next().unwrap();
        let key = format!("log.anti_nuke.verification_channel_{kind}");
        assert!(botmsg::find(&key).is_some(), "{key}");
    }
}

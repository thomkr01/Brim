//! D6: the general command answers (`info`, `roleinfo`, `serverinfo`, `about`, `ping`, `invites`)
//! read exactly as the code built them before until a server edits the entry, and an edit keeps
//! their facts. Each `old` below is the code's former builder, kept here as the reference.
use brim_logging::{
    botmsg,
    embed::{self, Log, ARROW, BLUE, GREEN, NO, YES},
    emoji::{
        ANTI_NUKE, CPU, HEAT, JOIN_GATE, JOIN_RAID, LATENCY, ONLINE, QUARANTINED, RAM, RESOURCES,
        SERVERS, SIZE, UPTIME, VERIFICATION,
    },
};
use serde_json::{json, Value};
use twilight_model::channel::message::Embed;

use crate::{info::with_avatar, reply::answer_from};

type Vars = Vec<(&'static str, String)>;

fn v(pairs: &[(&'static str, &str)]) -> Vars {
    pairs.iter().map(|(k, x)| (*k, x.to_string())).collect()
}

fn json_of(e: &Embed) -> Value {
    serde_json::to_value(e).unwrap()
}

fn same(key: &str, vars: &Vars, old: Log) {
    let got = answer_from(key, None, vars);
    assert_eq!(json_of(&got), json_of(&old.build()), "{key}");
}

fn user() -> twilight_model::user::User {
    serde_json::from_value(
        json!({"id": "123456789012345678", "username": "alex", "discriminator": "0", "avatar": null}),
    )
    .unwrap()
}

#[test]
fn every_info_answer_default_reads_as_the_code_built_it() {
    let u = user();
    let head = || {
        Log::new("alex's Info:", BLUE)
            .thumbnail(&u)
            .user("User", 123456789012345678)
            .line("Created", "<t:1600000000:R>")
    };
    let mut base = v(&[
        ("member.username", "alex"),
        ("member", &embed::user(123456789012345678)),
        ("member.created_at", "<t:1600000000:R>"),
    ]);
    let user_same = |vars: &Vars, old: Log| {
        let got = with_avatar(answer_from("reply.info.user", None, vars), &u);
        assert_eq!(json_of(&got), json_of(&old.build()));
    };
    // Not in the server, seen by someone without staff rights, then by staff.
    let mut gone = base.clone();
    gone.push(("in_server", NO.to_string()));
    user_same(&gone, head().line("In Server", &NO.to_string()));
    gone.extend(v(&[
        ("left_timed_out", &YES.to_string()),
        ("left_quarantined", &NO.to_string()),
        ("banned", &YES.to_string()),
        ("is_bot", &NO.to_string()),
    ]));
    user_same(
        &gone,
        head()
            .line("In Server", &NO.to_string())
            .line("Left While Timed Out", &YES.to_string())
            .line("Left While Quarantined", &NO.to_string())
            .line("Banned", &YES.to_string())
            .line("Bot", &NO.to_string()),
    );
    // A member: without a join time, with one, and the full answer.
    user_same(&base, head());
    base.push(("member.joined_at", "<t:1700000000:R>".into()));
    let joined = || head().line("Joined", "<t:1700000000:R>");
    user_same(&base, joined());
    let fields: [(&str, &str, &str); 10] = [
        (
            "Suspicious Status:",
            "suspicious",
            "✅ No Avatar\n❌ Account Younger Than 7d",
        ),
        ("Whitelist:", "whitelist", "✅ Spam\n❌ Ping"),
        ("Warn Points:", "warn_points", "`3`"),
        ("Active Strikes:", "strikes", "`0`"),
        ("Current Heat:", "heat", "`12%`"),
        ("Dangerous Permissions:", "dangerous", "Ban Members"),
        ("Brim Permissions:", "permits", "Trusted Admin"),
        ("Account Accessories:", "accessories", "❌ Banner"),
        ("Roles:", "member_roles", "<@&5>, <@&6>"),
        ("Webhooks:", "webhooks", "❌"),
    ];
    base.extend(v(&[("color", "#3498db"), ("badges", "Verified Bot")]));
    let mut old = joined()
        .line("Color", "`#3498db`")
        .line("Badges", "Verified Bot");
    for (name, var, value) in fields {
        base.push((var, value.into()));
        old = old.inline(name, value);
    }
    user_same(&base, old);

    let role = v(&[
        ("role.name", "Helpers"),
        ("role", &embed::role(77)),
        ("role.created_at", "<t:1650000000:R>"),
        ("position", "4"),
        ("role_details", "❌ Managed\n✅ Mentionable"),
        ("whitelist", "✅ Spam"),
        ("dangerous", "❌"),
    ]);
    same(
        "reply.info.role",
        &role,
        Log::new("Helpers's Info:", BLUE)
            .line("Role", &embed::role(77))
            .line("Created", "<t:1650000000:R>")
            .line("Position", "`4`")
            .inline("Details:", "❌ Managed\n✅ Mentionable")
            .inline("Whitelist:", "✅ Spam")
            .inline("Dangerous Permissions:", "❌"),
    );

    let server = v(&[
        ("server.name", "My Server"),
        ("owner", &embed::user(9)),
        ("server.created_at", "<t:1500000000:R>"),
        ("members", "120"),
        ("role_count", "14"),
        ("channel_count", "30"),
        ("boosts", "3"),
        ("boost_tier", "1"),
        ("pages", "2"),
    ]);
    same(
        "reply.info.server",
        &server,
        Log::new("My Server's Info:", BLUE)
            .line("Owner", &embed::user(9))
            .line("Created", "<t:1500000000:R>")
            .line("Members", "`120`")
            .line("Roles", "`14`")
            .line("Channels", "`30`")
            .line("Boosts", "`3` (tier 1)")
            .footer("Page: 1/2"),
    );
    let list = format!("{ARROW} {}\n{ARROW} {}", embed::role(5), embed::role(6));
    same(
        "reply.info.server_roles",
        &v(&[
            ("server.name", "My Server"),
            ("role_list", &list),
            ("page_no", "2"),
            ("pages", "2"),
        ]),
        Log::new("My Server's Roles:", BLUE)
            .text(&format!("{ARROW} {}", embed::role(5)))
            .text(&format!("{ARROW} {}", embed::role(6)))
            .footer("Page: 2/2"),
    );
}

#[test]
fn about_and_ping_defaults_read_as_the_code_built_them() {
    same(
        "reply.about.pitch",
        &vec![],
        Log::new("About Brim:", BLUE)
            .text(&format!("{ARROW} Brim is a Discord security and moderation bot, written in Rust to be fast and light. It stops nukes, raids and spam on its own, undoes the damage, and gives your staff moderation commands that are quick to use."))
            .text(&format!("{ARROW} Every feature is free. There is no premium tier."))
            .footer("Page: 1/3"),
    );
    same(
        "reply.about.protects",
        &vec![],
        Log::new("About Brim [What it protects]:", BLUE)
            .text(&format!("{ANTI_NUKE} **Anti-nuke:** quarantines staff who mass ban, kick or delete, then reverts what they did"))
            .text(&format!("{HEAT} **Heat automod:** spam, links, scams, mentions, words and more add heat until the member is punished"))
            .text(&format!("{JOIN_GATE} **Join gate:** filters bots, young accounts and suspicious names the moment they join"))
            .text(&format!("{JOIN_RAID} **Join raid:** spots join waves and kicks or bans the raiders"))
            .text(&format!("{VERIFICATION} **Verification:** captcha, web or button checks before members get in"))
            .text(&format!("{QUARANTINED} **Lockdown, tickets, appeals, backups and logs:** all in the dashboard"))
            .footer("Page: 2/3"),
    );
    same(
        "reply.about.stats",
        &v(&[
            ("version", "0.1.0"),
            ("uptime", "3d"),
            ("command_count", "90"),
        ]),
        Log::new("About Brim [Nerdy Stats]:", BLUE)
            .line("Version", "`0.1.0`")
            .line("Uptime", "`3d`")
            .line("Language", "`Rust`")
            .line("Commands", "`90`")
            .line("Price", "Everything is free")
            .footer("Page: 3/3"),
    );
    let ping = v(&[
        ("latency", "measuring"),
        ("api", "120ms"),
        ("uptime", "2h"),
        ("ram", "35 MB"),
        ("cpu", "0.40%"),
        ("servers", "unknown"),
    ]);
    let shard = format!(
        "{LATENCY} **Latency:** `measuring` (API `120ms`)\n{UPTIME} **Uptime:** `2h`\n{RESOURCES} **Resources:**\n\u{2003}{RAM} RAM: `35 MB`\n\u{2003}{CPU} CPU: `0.40%`\n{SIZE} **Size:**\n\u{2003}{SERVERS} Servers: `unknown`"
    );
    same(
        "reply.ping.status",
        &ping,
        Log::new("Brim Status:", GREEN).inline(&format!("{ONLINE} Shard [0 -Current-]:"), &shard),
    );
}

#[test]
fn every_invites_answer_default_reads_as_the_code_built_it() {
    let notes = [
        "Invite tracking is off on this server, so new joins are not counted.",
        "2 joins in the last 7 days could not be tied to an invite.",
    ];
    for notes in [&notes[..], &[]] {
        let with_notes = |mut e: Log| {
            for n in notes {
                e = e.text(n);
            }
            e
        };
        let tracked = |mut vars: Vars| {
            vars.extend(v(&[
                ("since", "<t:1700000000:d>"),
                ("notes", &notes.join("\n")),
            ]));
            vars
        };
        let member = embed::user(6);
        same(
            "reply.invite.stats",
            &tracked(v(&[
                ("member", &member),
                ("invited", "5"),
                ("still_here", "3"),
                ("left", "2"),
            ])),
            with_notes(
                Log::new("Invites", BLUE)
                    .user("Member", 6)
                    .line("Invited", "`5`")
                    .line("Still here", "`3`")
                    .line("Left", "`2`")
                    .line("Tracking since", "<t:1700000000:d>"),
            ),
        );
        for rows in [
            vec![],
            vec![
                "`1.` <@6>: `5` (3 still here)",
                "`2.` <@7>: `1` (0 still here)",
            ],
        ] {
            let mut old = Log::new("Invite Leaderboard", BLUE);
            if rows.is_empty() {
                old = old.text("No invites counted yet");
            }
            for r in &rows {
                old = old.text(r);
            }
            let ranking = if rows.is_empty() {
                "No invites counted yet".to_string()
            } else {
                rows.join("\n")
            };
            same(
                "reply.invite.top",
                &tracked(v(&[
                    ("ranking", &ranking),
                    ("page_no", "1"),
                    ("pages", "1"),
                ])),
                with_notes(old.line("Tracking since", "<t:1700000000:d>")).footer("Page: 1/1"),
            );
        }
        same(
            "reply.invite.who_unseen",
            &tracked(v(&[("member", &member)])),
            with_notes(
                Log::new("Who Invited", BLUE)
                    .user("Member", 6)
                    .text("No join of this member was seen since tracking started")
                    .line("Tracking since", "<t:1700000000:d>"),
            ),
        );
        // The code only in a slash answer.
        for code in ["abc123", ""] {
            let mut old = Log::new("Who Invited", BLUE)
                .user("Member", 6)
                .line("Invited by", "Unknown")
                .line("First seen", "<t:1700000000:f>")
                .line("Method", "Unknown")
                .line("Still here", "No")
                .line("Joins seen", "`2`");
            if !code.is_empty() {
                old = old.line("Invite code", &format!("`{code}`"));
            }
            same(
                "reply.invite.who",
                &v(&[
                    ("member", &member),
                    ("inviter", "Unknown"),
                    ("first_seen", "<t:1700000000:f>"),
                    ("method", "Unknown"),
                    ("present", "No"),
                    ("joins", "2"),
                    ("code", code),
                    ("notes", &notes.join("\n")),
                ]),
                with_notes(old),
            );
        }
    }
    same(
        "reply.invite.status",
        &v(&[
            ("tracking", "On"),
            ("status", "Working since <t:1700000000:R>"),
            ("since", "No joins seen yet"),
            ("unknown", "0"),
        ]),
        Log::new("Invite Tracking", BLUE)
            .line("Tracking", "On")
            .line("Status", "Working since <t:1700000000:R>")
            .line("Tracking since", "No joins seen yet")
            .line("Unknown joins (7 days)", "`0`")
            .text("Brim needs the Manage Server permission to read the server's invites."),
    );
}

/// Every placeholder moved anywhere, none set: nothing raw reaches Discord, and a copy without
/// the required fact is refused and never sent.
#[test]
fn an_edit_can_move_every_placeholder_but_not_drop_a_fact() {
    for key in botmsg::ANSWERS {
        let d = botmsg::find(key).unwrap();
        assert!(d.details() && d.strict() && !d.security, "{key}");
        let all: Vec<String> = d.vars.iter().map(|n| format!("{{{n}}}")).collect();
        let moved = json!({"content": "", "embeds": [{
            "title": "Moved",
            "description": all.join(" and "),
            "footer": {"text": all.join(" ")},
        }]});
        assert_eq!(botmsg::missing_text(key, &moved), None, "{key}");
        let sent = json_of(&answer_from(key, Some(&moved), &[])).to_string();
        assert!(sent.contains("Moved"), "{key}: the edit is sent");
        for p in &all {
            assert!(!sent.contains(p.as_str()), "{key}: {p} reached Discord");
        }
        for r in &d.required {
            let kept: Vec<&String> = all.iter().filter(|p| **p != format!("{{{r}}}")).collect();
            let lost = json!({"content": "", "embeds": [{"title": "Lost", "description": format!("{kept:?}")}]});
            assert!(
                botmsg::missing_text(key, &lost).is_some(),
                "{key} without {r}"
            );
            let sent = json_of(&answer_from(key, Some(&lost), &[(*r, "fact".into())]));
            assert!(
                !sent.to_string().contains("Lost"),
                "{key}: the default is sent"
            );
        }
    }
}

#[test]
fn an_edited_member_info_keeps_its_values_and_the_avatar() {
    let u = user();
    let edited = json!({"content": "", "embeds": [{
        "title": "Who is {member.username}?",
        "description": "{member}, here since {member.joined_at}\n**Colour:** {color}",
        "fields": [{"name": "Heat", "value": "{heat}", "inline": false}],
    }]});
    assert_eq!(botmsg::missing_text("reply.info.user", &edited), None);
    // A member seen by someone without staff rights: no colour, no heat.
    let vars = v(&[
        ("member.username", "alex"),
        ("member", &embed::user(6)),
        ("member.joined_at", "<t:1700000000:R>"),
    ]);
    let e = with_avatar(answer_from("reply.info.user", Some(&edited), &vars), &u);
    assert_eq!(e.title.as_deref(), Some("Who is alex?"));
    assert_eq!(
        e.description.as_deref(),
        Some(format!("{}, here since <t:1700000000:R>", embed::user(6)).as_str())
    );
    assert!(e.fields.is_empty(), "an empty field goes");
    assert_eq!(e.thumbnail.map(|t| t.url), Some(embed::avatar(&u)));
}

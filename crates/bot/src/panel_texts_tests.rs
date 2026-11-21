//! The security command answers: the settings panels of `antinuke`, `ancaps`, `anpanic`,
//! `antinukerestore` and `joinraid`, and the whitelist menu and result, read exactly as the code
//! built them until a server edits the entry, and an edit keeps every value and row number.
use brim_logging::{
    botmsg,
    embed::{self, Log, GREEN, NO, ORANGE, YES},
    emoji::{MODERATOR, TARGET},
};
use serde_json::{json, Value};
use twilight_model::channel::message::Embed;

use crate::{app::GuildCfg, reply::answer_from, settings};

fn json_of(e: &Embed) -> Value {
    serde_json::to_value(e).unwrap()
}

fn cfgs() -> Vec<GuildCfg> {
    vec![
        GuildCfg::parse(&json!({})),
        GuildCfg::parse(&json!({
            "misc": {"prefix": "?"},
            "antinuke": {"enabled": true, "ticket_categories": ["5"]},
            "panic": {"executors": 0, "warned_roles": ["7", "8"]},
            "backups": {"restore": true},
            "joinraid": {"enabled": true, "flag_younger_than_ms": 86400000},
        })),
    ]
}

#[test]
fn every_security_panel_reads_as_the_code_built_it() {
    for (key, word, _, _, rows) in botmsg::PANELS {
        let m = settings::module(word).unwrap();
        let ours: Vec<(&str, &str)> = m.rows.iter().map(|r| (r.code, r.name)).collect();
        let theirs: Vec<(&str, &str)> = rows.iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(ours, theirs, "{key}");
        for cfg in cfgs() {
            let (k, vars) = settings::panel_vars(m, &cfg, 1).unwrap();
            assert_eq!(k, *key);
            // The editor's preview shows each row number as the panel does.
            for (name, value) in &vars {
                if rows.iter().any(|r| r.0 == *name) {
                    assert_eq!(botmsg::var_info(key, name).1, value, "{key} {name}");
                }
            }
            let (old, pages) = settings::panel(m, &cfg, 1, 0);
            assert_eq!(pages, 1, "{key}");
            let new = settings::with_prefix(answer_from(key, None, &vars), &cfg.misc.prefix);
            assert_eq!(json_of(&new), json_of(&old), "{key}");
        }
    }
    // The other panels stay built in code.
    for word in ["heat", "statics", "joingate", "misc"] {
        let m = settings::module(word).unwrap();
        assert!(settings::panel_vars(m, &cfgs()[0], 1).is_none(), "{word}");
    }
}

/// The code's former whitelist builders.
fn old_prompt(target: &str) -> Embed {
    Log::new("📋 Interactive Whitelist", ORANGE)
        .text(&format!("Target: {target}"))
        .build()
}

fn old_saved(lines: &[(bool, &str)], actor: u64, target: &str) -> Embed {
    let mut e = Log::new("📋 Interactive Whitelist", GREEN);
    for (on, label) in lines {
        e = e.text(&format!("{} {label}", if *on { YES } else { NO }));
    }
    e.inline(&format!("{MODERATOR} Executor"), &embed::user(actor))
        .inline(&format!("{TARGET} Target"), target)
        .build()
}

#[test]
fn whitelist_answers_read_as_the_code_built_them() {
    let target = embed::user(9);
    let got = answer_from(
        "reply.whitelist.prompt",
        None,
        &[("target", target.clone())],
    );
    assert_eq!(json_of(&got), json_of(&old_prompt(&target)));
    let lists = format!("{YES} Spam Whitelist\n{NO} Ping Whitelist");
    let vars = [
        ("lists", lists),
        ("moderator", embed::user(4)),
        ("target", target.clone()),
    ];
    let got = answer_from("reply.whitelist.saved", None, &vars);
    let old = old_saved(
        &[(true, "Spam Whitelist"), (false, "Ping Whitelist")],
        4,
        &target,
    );
    assert_eq!(json_of(&got), json_of(&old));
}

fn keys() -> Vec<&'static str> {
    let panels = [
        "reply.panel.antinuke",
        "reply.panel.ancaps",
        "reply.panel.anpanic",
        "reply.panel.antinukerestore",
        "reply.panel.joinraid",
    ];
    let ours: Vec<&str> = botmsg::PANELS.iter().map(|p| p.0).collect();
    assert_eq!(ours, panels);
    let mut k = panels.to_vec();
    k.extend(botmsg::WHITELIST_ANSWERS);
    k
}

/// Each is a security text of facts: no `{text}`, every value and row number required as plain
/// text, and the default passes its own save check.
#[test]
fn every_security_answer_guards_its_facts() {
    for key in keys() {
        let d = botmsg::find(key).unwrap();
        assert!(
            d.security && d.strict() && d.details() && !d.locked,
            "{key}"
        );
        assert!(!d.vars.contains(&"text"), "{key}");
        assert!(botmsg::missing_text(key, &d.template).is_none(), "{key}");
        assert!(botmsg::unknown_vars(key, &d.template).is_empty(), "{key}");
        let with = |r: &str, to: &str| -> Value {
            serde_json::from_str(&d.template.to_string().replace(&format!("{{{r}}}"), to)).unwrap()
        };
        for r in &d.required {
            assert!(
                botmsg::missing_text(key, &with(r, "x")).is_some(),
                "{key} without {{{r}}}"
            );
            let hidden = with(r, &format!("||{{{r}}}||"));
            assert!(
                botmsg::missing_text(key, &hidden).is_some(),
                "{key} {{{r}}} in a spoiler"
            );
        }
        let frame = json!({"embeds": [{"title": "Routine check", "description": "||{text}||"}]});
        assert!(botmsg::missing_text(key, &frame).is_some(), "{key}");
        assert_eq!(botmsg::unknown_vars(key, &frame), ["text"], "{key}");
    }
}

/// The owner may reword, reorder and put every row in one list; the values still arrive.
#[test]
fn a_panel_can_be_reworded_and_put_in_one_list() {
    let key = "reply.panel.ancaps";
    let rows = botmsg::PANELS.iter().find(|p| p.0 == key).unwrap().4;
    // One line per row, in reverse order, in its own words.
    let list: Vec<String> = rows
        .iter()
        .rev()
        .map(|(code, _, var, _)| match *var {
            "" => format!("**{{{code}}}**"),
            v => format!("{{{code}}} is {{{v}}}"),
        })
        .collect();
    let edit =
        json!({"content": "", "embeds": [{"title": "Caps", "description": list.join("\n")}]});
    assert_eq!(botmsg::missing_text(key, &edit), None);
    let m = settings::module("ancaps").unwrap();
    let cfg = &cfgs()[0];
    let (_, vars) = settings::panel_vars(m, cfg, 1).unwrap();
    let sent = json_of(&answer_from(key, Some(&edit), &vars));
    let text = sent["description"].as_str().unwrap();
    assert_eq!(sent["title"], "Caps");
    assert!(
        text.contains("**[7]**") && text.contains("[D] is 16%"),
        "{text}"
    );
    assert!(!text.contains('{'), "{text}");
    // Rename a row and move it first: kept.
    let panic = botmsg::find("reply.panel.anpanic").unwrap();
    let renamed: Value = serde_json::from_str(&panic.template.to_string().replace(
        "Lockdown Server On Trigger",
        "Lock the server when panic starts",
    ))
    .unwrap();
    assert_eq!(botmsg::missing_text("reply.panel.anpanic", &renamed), None);
    let (_, vars) = settings::panel_vars(settings::module("anpanic").unwrap(), cfg, 1).unwrap();
    let sent = json_of(&answer_from("reply.panel.anpanic", Some(&renamed), &vars)).to_string();
    assert!(
        sent.contains("[3] Lock the server when panic starts:"),
        "{sent}"
    );
}

/// No `{prefix}`: a server's prefix such as `||` could wrap a fact in a spoiler after the save
/// check. A copy too long for Discord loses fields, so the default goes out instead.
#[test]
fn no_prefix_placeholder_and_no_message_over_discords_limit() {
    for key in keys() {
        assert!(
            !botmsg::find(key).unwrap().vars.contains(&"prefix"),
            "{key}"
        );
    }
    let key = "reply.panel.antinuke";
    let d = botmsg::find(key).unwrap();
    let mut edit = d.template.clone();
    edit["embeds"][0]["title"] = "x".repeat(250).into();
    for f in edit["embeds"][0]["fields"].as_array_mut().unwrap() {
        let v = f["value"].as_str().unwrap().to_string();
        f["value"] = format!("{v} {}", "y".repeat(960)).into();
    }
    assert_eq!(botmsg::missing_text(key, &edit), None);
    let m = settings::module("antinuke").unwrap();
    let (_, vars) = settings::panel_vars(m, &cfgs()[0], 1).unwrap();
    let sent = json_of(&answer_from(key, Some(&edit), &vars));
    assert_eq!(sent["title"], "Anti Nuke Panel:", "the default is sent");
}

//! The settings command answers: every settings panel (the security ones of `antinuke`, `ancaps`,
//! `anpanic`, `antinukerestore` and `joinraid`, and `statics`, `heat`, `hfilters`, `joingate`,
//! `verification`, `misc` and `raiderlist`), the whitelist menu and result and the reply after a
//! setting changed, read exactly as the code built them until a server edits the entry, and an
//! edit keeps every value and row number.
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
            "automod": {"enabled": true, "strike": {"enabled": true}},
            "joingate": {"enabled": true},
            "verification": {"enabled": true, "roles": ["9"], "regen_ms": 12000},
            "raider_list": {"enabled": true},
            "lockdown": {"main_roles": ["3"]},
            "logging": {"fallback": "4"},
        })),
    ]
}

#[test]
fn every_panel_reads_as_the_code_built_it() {
    let all = botmsg::PANELS.iter().chain(botmsg::SETTINGS_PANELS);
    let words: Vec<&str> = all.clone().map(|p| p.1).collect();
    let modules: Vec<&str> = settings::words()
        .filter(|w| settings::module(w).is_some_and(|m| m.names[0] == *w))
        .collect();
    let mut sorted = words.clone();
    sorted.sort_unstable();
    let mut modules = modules;
    modules.sort_unstable();
    assert_eq!(sorted, modules, "every panel has an entry");
    for (key, word, _, _, rows) in all {
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
            let filled = settings::with_prefix(answer_from(key, None, &vars), &cfg.misc.prefix);
            let (_, pages) = settings::panel(m, &cfg, 1, 0);
            for page in 0..pages + 1 {
                let (old, n) = settings::panel(m, &cfg, 1, page);
                let (new, k) = settings::page_of(filled.clone(), m.per_page, page);
                assert_eq!(n, k, "{key}");
                assert_eq!(json_of(&new), json_of(&old), "{key} page {page}");
            }
        }
    }
}

/// A server's prefix shows in a footer that names a command, as the code wrote it.
#[test]
fn a_footer_names_the_servers_prefix() {
    let cfg = &cfgs()[1];
    let m = settings::module("heat").unwrap();
    let (key, vars) = settings::panel_vars(m, cfg, 1).unwrap();
    let e = settings::with_prefix(answer_from(key, None, &vars), &cfg.misc.prefix);
    assert_eq!(
        e.footer.unwrap().text,
        "Advanced Settings: Heat System Filters: ?hfilters"
    );
}

/// The heat filters panel keeps its pages of 4 cards, also when an edit renames the cards.
#[test]
fn an_edited_heat_filters_panel_keeps_its_pages() {
    let key = "reply.panel.hfilters";
    let d = botmsg::find(key).unwrap();
    let edit: Value = serde_json::from_str(
        &d.template
            .to_string()
            .replace("Normal Message Heat", "Every message"),
    )
    .unwrap();
    assert_eq!(botmsg::missing_text(key, &edit), None);
    let m = settings::module("hfilters").unwrap();
    let (_, vars) = settings::panel_vars(m, &cfgs()[0], 1).unwrap();
    let filled = answer_from(key, Some(&edit), &vars);
    let (first, pages) = settings::page_of(filled.clone(), m.per_page, 0);
    assert_eq!(pages, 4);
    assert_eq!(first.fields.len(), 4);
    assert!(first.fields[0].name.starts_with("[1] Every message"));
    assert_eq!(first.footer.unwrap().text, "Page: 1/4");
    let (last, _) = settings::page_of(filled, m.per_page, 9);
    assert_eq!(last.footer.unwrap().text, "Page: 4/4");
}

/// The code's former reply after a setting changed.
fn old_changed(code: &str, name: &str, value: &str) -> String {
    format!("{YES} [{}] {name}: {value}", code.to_uppercase())
}

#[test]
fn the_setting_changed_reply_reads_as_the_code_built_it() {
    for key in botmsg::CHANGED {
        let vars = [
            ("row", "[7B]"),
            ("setting", "Regular Strike Duration"),
            ("value", "1 hour"),
        ];
        let got = botmsg::render(key, None, &vars);
        assert_eq!(
            got["content"],
            old_changed("7b", "Regular Strike Duration", "1 hour"),
            "{key}"
        );
        assert_eq!(got["embeds"], json!([]), "{key}");
    }
    let d = botmsg::find("reply.settings.changed").unwrap();
    assert!(!d.security && d.strict());
    assert_eq!(d.required, ["row", "value"]);
    // Only the Owner and Extra Owners reword the reply for join raid and Trusted Admins.
    assert!(
        botmsg::find("reply.settings.changed_security")
            .unwrap()
            .security
    );
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
    k.push("reply.settings.changed_security");
    k
}

fn settings_keys() -> Vec<&'static str> {
    let panels = [
        "reply.panel.statics",
        "reply.panel.heat",
        "reply.panel.hfilters",
        "reply.panel.joingate",
        "reply.panel.verification",
        "reply.panel.misc",
        "reply.panel.raiderlist",
    ];
    let ours: Vec<&str> = botmsg::SETTINGS_PANELS.iter().map(|p| p.0).collect();
    assert_eq!(ours, panels);
    let mut k = panels.to_vec();
    k.push("reply.settings.changed");
    k
}

/// Each is a security text of facts: no `{text}`, every value and row number required as plain
/// text, and the default passes its own save check.
#[test]
fn every_settings_answer_guards_its_facts() {
    let security = keys();
    for key in security.iter().chain(&settings_keys()) {
        let d = botmsg::find(key).unwrap();
        assert_eq!(d.security, security.contains(key), "{key}");
        assert!(d.strict() && d.details() && !d.locked, "{key}");
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

/// No `{prefix}` in a security text: a server's prefix such as `||` could wrap a fact in a
/// spoiler after the save check. Only the misc panel has it, as the value of its Prefix setting.
/// A copy too long for Discord loses fields, so the default goes out instead.
#[test]
fn no_prefix_placeholder_and_no_message_over_discords_limit() {
    for key in keys().into_iter().chain(settings_keys()) {
        assert_eq!(
            botmsg::find(key).unwrap().vars.contains(&"prefix"),
            key == "reply.panel.misc",
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

/// A Trusted Admin cannot reword the answer to a change only the Owner and Extra Owners may make.
#[test]
fn a_security_or_owner_only_change_answers_in_the_security_copy() {
    let key = |word: &str, code: &str| {
        let m = settings::module(word).unwrap();
        let row = m.rows.iter().find(|r| r.code == code).unwrap();
        settings::changed_key(m, row)
    };
    assert_eq!(key("joinraid", "1"), "reply.settings.changed_security");
    assert_eq!(key("misc", "8"), "reply.settings.changed_security");
    assert_eq!(key("statics", "5"), "reply.settings.changed_security");
    assert_eq!(key("heat", "1"), "reply.settings.changed");
    assert_eq!(key("verification", "1"), "reply.settings.changed");
}

/// The settings error replies  read exactly as the code wrote them before the entries: every
/// row of every panel is refused the way the old text did.
#[test]
fn settings_errors_read_as_the_code_wrote_them() {
    use settings::{edit_for, Kind, Op};
    let s = |t: &str| t.to_string();
    let mut checked = 0;
    for word in settings::words() {
        let Some(m) = settings::module(word).filter(|m| m.names[0] == word) else {
            continue;
        };
        for row in m.rows {
            let (c, label) = (
                row.code,
                format!("[{}] {}", row.code.to_uppercase(), row.name),
            );
            let err = |op: Op, ids: &[u64]| edit_for(row, &op, ids, None).unwrap_err().text();
            let set = |v: &str| Op::Set(s(c), s(v));
            let mut want: Vec<(String, String)> = vec![];
            match row.kind {
                Kind::Dashboard | Kind::Users => want.push((
                    err(set("x"), &[]),
                    format!("{label} can only be changed on the dashboard"),
                )),
                Kind::Group => want.push((
                    err(set("x"), &[]),
                    format!("{label} has sub-settings, use {c}a, {c}b..."),
                )),
                Kind::Bool | Kind::Switch(..) => want.push((
                    err(set("x"), &[]),
                    format!("{label} is a switch: use `{c} ?on` or `{c} ?off`"),
                )),
                kind => {
                    want.push((
                        err(Op::Toggle(s(c), true), &[]),
                        format!("{label} is not a switch: use `VALUE ?set {c}`"),
                    ));
                    if !matches!(kind, Kind::Roles | Kind::Channels) {
                        want.push((
                            err(Op::Add(s(c), s("x")), &[]),
                            format!("{label} is not a list: use `VALUE ?set {c}`"),
                        ));
                    }
                    match kind {
                        Kind::Int(min, max) => {
                            want.push((
                                err(set("x"), &[]),
                                format!("`x` is not a valid value for {label}"),
                            ));
                            want.push((
                                err(set(&(max + 1).to_string()), &[]),
                                format!("{label} must be between {min} and {max}"),
                            ));
                        }
                        Kind::Secs | Kind::Ms => {
                            want.push((
                                err(set("999w"), &[]),
                                format!("{label}: {}", brim_core::duration::TOO_LONG),
                            ));
                            want.push((err(set("abc"), &[]), format!("`abc` is not a valid value for {label}. Give a number with a unit, like `15d`, `12h` or `30m`.")));
                        }
                        Kind::Age => {
                            want.push((
                                err(set("1.5"), &[]),
                                format!(
                                    "`1.5` is not a valid value for {label}. Give a whole number."
                                ),
                            ));
                            want.push((err(set("abc"), &[]), format!("`abc` is not a valid value for {label}. Give days, or a number with a unit, like `12h` or `30m`.")));
                            want.push((
                                err(set("1s"), &[]),
                                format!("{label} ranges from 5 seconds to 100 days"),
                            ));
                        }
                        Kind::Choice(opts) => {
                            let names: Vec<_> = opts.iter().map(|o| o.0).collect();
                            want.push((
                                err(set("zzz-none"), &[]),
                                format!("{label} takes one of: {}", names.join(", ")),
                            ));
                        }
                        Kind::Role | Kind::Channel => want
                            .push((err(set("x"), &[1, 2]), format!("{label} takes exactly one"))),
                        Kind::Text if row.path == "misc.prefix" => want.push((
                            err(set("a b"), &[]),
                            format!("{label} must be 1-32 characters with no spaces"),
                        )),
                        _ => {}
                    }
                }
            }
            for (got, old) in want {
                assert_eq!(got, old, "{word} {c}");
                checked += 1;
            }
        }
    }
    assert!(checked > 150, "{checked}");
}

/// Every settings error reply is an entry of facts: what went wrong is a key variable, no
/// `{text}`, and the Security copy reads as the settings copy and is a security text.
#[test]
fn settings_errors_keep_what_went_wrong() {
    let link = settings::dashboard_link(1);
    let vars = [
        ("row", "[3C]"),
        ("setting", "Minimum Age"),
        ("number", "3c"),
        ("typed", "abc"),
        ("min", "1"),
        ("max", "9"),
        ("options", "kick, ban"),
        ("reason", "why"),
        ("target", "<@&5>"),
        ("dashboard", link.as_str()),
    ];
    let text = |key: &str| botmsg::render(key, None, &vars)["content"].clone();
    for (key, _, _, required, _) in botmsg::SETTINGS_ERRORS {
        let copy = botmsg::security_copy(key);
        let (d, sec) = (botmsg::find(key).unwrap(), botmsg::find(&copy).unwrap());
        assert!(!d.security && sec.security, "{key}");
        assert_eq!(d.group, "Settings panels", "{key}");
        assert_eq!(text(key), text(&copy), "{key}");
        assert!(!required.is_empty() && d.required == *required, "{key}");
        for d in [d, sec] {
            assert!(!d.vars.contains(&"text"), "{}", d.key);
            let lost = json!({"content": "Something went wrong", "embeds": []});
            assert!(botmsg::missing_text(&d.key, &lost).is_some(), "{}", d.key);
        }
    }
    for (key, ..) in botmsg::SECURITY_ERRORS {
        let d = botmsg::find(key).unwrap();
        assert!(d.security && !d.vars.contains(&"text"), "{key}");
    }
    // The texts that are not built from a row, as the code wrote them.
    let old = [
        ("reply.settings.needs_2fa", format!("This change needs 2FA for this server. Open this server on the {link}, choose Confirm 2FA in the account menu, then try again within 10 minutes.")),
        ("reply.settings.needs_2fa_dashboard", format!("This setting needs 2FA, so change it on the {link}")),
        ("reply.settings.main_roles_owners", s("Anti-nuke monitors the main roles, so only the owner and Extra Owners can change them")),
        ("reply.settings.not_main_role", s("<@&5> cannot be a main role: why")),
        ("reply.settings.ta_usage", s("[10] Trusted Admins: use `USER ?add 10` or `USER ?remove 10`")),
        ("reply.settings.ta_owners", s("Only the owner and Extra Owners can change Trusted Admins")),
        ("reply.settings.ta_no_user", s("Give at least one user mention or ID")),
        ("reply.settings.not_saved", s("Not saved: why")),
        ("reply.settings.not_a_role", s("`abc` is not a role of this server")),
        ("reply.settings.not_a_channel", s("`abc` is not a channel of this server")),
        ("reply.settings.nothing_found", s("Nothing found for `abc`")),
        ("reply.settings.ambiguous", s("`abc` is ambiguous, use a mention or id")),
        ("reply.settings.not_your_panel", s("Only the person who opened this panel can use it.")),
    ];
    for (key, want) in old {
        assert_eq!(text(key), json!(want), "{key}");
    }
    fn s(t: &str) -> String {
        t.to_string()
    }
}

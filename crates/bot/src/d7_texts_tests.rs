//! D7: the ticket, report, appeal and golden channel logs read exactly as the code builds them
//! until a server edits the entry, and an edit keeps their facts.
use brim_logging::{botmsg, embed::Log, overlay};
use serde_json::json;
use twilight_model::id::Id;

use crate::{appeals, catch, d3_texts_tests::key, reports, tickets};

fn user(id: u64, name: &str) -> twilight_model::user::User {
    serde_json::from_value(
        json!({"id": id.to_string(), "username": name, "discriminator": "0", "avatar": null}),
    )
    .unwrap()
}

/// The default of the log's entry, filled with the log's own values, beside the log as built
/// (the log already carries its time line).
fn exact(group: &str, log: Log) {
    let k = key(group, &log);
    let (built, vars) = log.parts();
    let vars: Vec<(&str, &str)> = vars.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let e = overlay::bot_embed(&k, None, &vars);
    assert_eq!(e.title, built.title, "{k} title");
    assert_eq!(e.description, built.description, "{k} description");
    assert_eq!(e.color, built.color, "{k} colour");
    let f = |e: &twilight_model::channel::message::Embed| {
        e.fields
            .iter()
            .map(|f| (f.name.clone(), f.value.clone(), f.inline))
            .collect::<Vec<_>>()
    };
    assert_eq!(f(&e), f(&built), "{k} fields");
    if e.author.is_some() {
        let name =
            |e: &twilight_model::channel::message::Embed| e.author.as_ref().map(|a| a.name.clone());
        assert_eq!(name(&e), name(&built), "{k} author");
    }
}

#[test]
fn every_d7_log_default_reads_as_the_code_builds_it() {
    let (alex, sam) = (user(6, "alex"), user(5, "sam"));
    exact("ticket", tickets::new_ticket_log(&alex, None, None).time());
    exact(
        "ticket",
        tickets::new_ticket_log(&alex, Some("Support"), Some("Report")).time(),
    );
    let changes = ["- <@5> claimed it".to_string(), "- type set".into()];
    for (kind, changes) in [(None, &[][..]), (Some("Support"), &changes[..])] {
        let log = tickets::closed_log((&sam, "sam"), "Solved", Id::new(6), kind, changes);
        exact("ticket", log.time());
    }
    use tickets::{claimed_log, handed_log, type_log};
    exact("ticket", claimed_log(3, "<@5>").time());
    for slug in ["unclaimed", "reassigned"] {
        exact("ticket", handed_log(3, slug, Id::new(5), Id::new(6)).time());
    }
    exact(
        "ticket",
        type_log(3, "<@5>", Some("Report"), "Appeal").time(),
    );
    exact("ticket", type_log(3, "<@5>", None, "Appeal").time());
    let jump = "https://discord.com/channels/1/2/3";
    for content in ["", "buy | nitro"] {
        let log = reports::reported_log(&alex, (Id::new(5), "sam"), jump, content, "Scam");
        exact("report", log);
    }
    exact("report", reports::native_log(Id::new(6), Id::new(9), "bad"));
    exact(
        "appeal",
        appeals::not_accepted_log(4, 5, "Could not lift the punishment").time(),
    );
    exact("auto_mod", catch::test_log(6).time());
    exact(
        "auto_mod",
        catch::failed_log(6, "Missing Permissions").time(),
    );
}

/// An edit that moves the facts around shows the log's own values, and the ticket closer's
/// author line and footer stay as built.
#[test]
fn an_edited_d7_log_shows_its_values() {
    let sam = user(5, "sam");
    let log = tickets::closed_log((&sam, "sam"), "Solved", Id::new(6), None, &[])
        .footer("alex | 6")
        .time();
    let (built, vars) = log.parts();
    let stored = json!({"content": "", "embeds": [{"title": "Closed for {member}", "description": "Why: {reason} (by {staff})"}]});
    let k = "log.ticket.ticket_closed";
    assert!(botmsg::missing_text(k, &stored).is_none());
    let e = overlay::over(k, &stored, &built, &vars, true);
    assert_eq!(e.title.as_deref(), Some("Closed for <@6> *`6`*"));
    assert_eq!(e.description.as_deref(), Some("Why: Solved (by sam)"));
    assert_eq!(e.footer.map(|f| f.text).as_deref(), Some("alex | 6"));
    assert_eq!(e.author.map(|a| a.name).as_deref(), Some("sam"));
    // Dropping a required fact is refused, and such a copy is never laid over.
    let lost = json!({"content": "", "embeds": [{"title": "Closed", "description": "{reason}"}]});
    assert!(botmsg::missing_text(k, &lost).is_some());
}

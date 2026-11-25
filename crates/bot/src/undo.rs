//! Undo buttons on a case's mod log: Unban, Remove timeout, Unquarantine and Close case. A press
//! runs the command a moderator would type (`modcmds::run`), so it passes the same checks: the
//! presser's permit, the role order, immunity, the panic lock, the command rate limit and the
//! quarantine release guard. Unquarantine goes through anti-nuke's own inverse, as the command does.
use anyhow::Result;
use brim_core::permits::Level;
use twilight_model::{
    application::interaction::{Interaction, InteractionData},
    channel::message::{
        component::{ActionRow, ButtonStyle},
        embed::EmbedField,
        AllowedMentions, Component, MessageFlags,
    },
    http::interaction::{InteractionResponse, InteractionResponseType},
    id::{
        marker::{ChannelMarker, GuildMarker, UserMarker},
        Id,
    },
};

use crate::{
    app::App,
    modcmds::{self, ModCmd},
    reply::{self, Said},
    reports::Acting,
};

const PREFIX: &str = "undo:";
const CLOSE: &str = "close";
const ACTION_LOG: &str = "Action Log";

/// What lifts a case of this kind: (button action, label).
fn lift(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "ban" => Some(("unban", "Unban")),
        "timeout" => Some(("untimeout", "Remove timeout")),
        "quarantine" => Some(("unquarantine", "Unquarantine")),
        _ => None,
    }
}

/// The button row under the mod log of a punishment case; no row for other cases.
pub fn rows(case_no: i32, kind: &str) -> Vec<Component> {
    let punishment = ["ban", "softban", "kick", "timeout", "warn", "quarantine"];
    if !punishment.contains(&kind) {
        return vec![];
    }
    let button = |action: &str, label: &str| {
        let id = format!("{PREFIX}{case_no}:{action}");
        brim_logging::embed::button(label, &id, ButtonStyle::Secondary)
    };
    let components = lift(kind)
        .map(|(action, label)| button(action, label))
        .into_iter()
        .chain([button(CLOSE, "Close case")])
        .collect();
    vec![Component::ActionRow(ActionRow {
        id: None,
        components,
    })]
}

/// The command a button stands for. The member and the kind come from the stored case, never from
/// the button, so a button cannot be aimed at another member or another kind of case.
fn command(action: &str, case_no: i32, kind: &str, user: u64) -> Option<ModCmd> {
    if action == CLOSE {
        return Some(ModCmd::CaseSet {
            case: case_no,
            active: false,
        });
    }
    match lift(kind).filter(|(a, _)| *a == action)?.0 {
        "unban" => Some(ModCmd::Unban { user }),
        "untimeout" => Some(ModCmd::Untimeout { user }),
        _ => Some(ModCmd::Unquarantine { user }),
    }
}

/// The message's rows without the button `id`; a row left empty goes too.
fn without(rows: &[Component], id: &str) -> Vec<Component> {
    rows.iter()
        .filter_map(|r| match r {
            Component::ActionRow(row) => {
                let components: Vec<Component> = row
                    .components
                    .iter()
                    .filter(|c| !matches!(c, Component::Button(b) if b.custom_id.as_deref() == Some(id)))
                    .cloned()
                    .collect();
                (!components.is_empty()).then_some(Component::ActionRow(ActionRow {
                    id: None,
                    components,
                }))
            }
            other => Some(other.clone()),
        })
        .collect()
}

/// Runs the press: the reply, and whether the button's work is done.
async fn press(
    app: &App,
    gid: Id<GuildMarker>,
    (owner, actor_id, level): (Id<UserMarker>, Id<UserMarker>, &Level),
    channel: Id<ChannelMarker>,
    (case_no, action): (i32, &str),
) -> Result<(Said, bool)> {
    let case_s = case_no.to_string();
    let case: Option<(i64, String)> =
        sqlx::query_as("SELECT user_id, kind FROM cases WHERE guild_id=$1 AND case_no=$2")
            .bind(gid.get() as i64)
            .bind(case_no)
            .fetch_optional(&app.pool)
            .await?;
    let Some((user, kind)) = case else {
        let said = reply::say(app, gid, "reply.case.no_case", &[("case", &case_s)]).await;
        return Ok((said, false));
    };
    let Some(cmd) = command(action, case_no, &kind, user as u64) else {
        return Ok((
            reply::say(app, gid, "reply.undo.wrong_case", &[]).await,
            false,
        ));
    };
    // The button is the command: the server's settings for it count here too.
    let word = if action == CLOSE { "closecase" } else { action };
    if let Some(name) = crate::cmdcfg::canonical(word, false) {
        if let Some(no) = crate::cmdcfg::button(app, gid, actor_id, level, name).await? {
            return Ok((no.into(), false));
        }
    }
    // A lift names the member, so it would also lift a punishment made after this case. Asked
    // only of a presser who holds the command's permit; anyone else gets its refusal below.
    if action != CLOSE && level.has(cmd.perm()) {
        let g = gid.get() as i64;
        if let Some(why) = brim_core::cases::not_current(&app.pool, g, case_no).await? {
            let user = format!("<@{user}>");
            let vars = [("case", case_s.as_str()), ("user", &user), ("cause", &why)];
            let said = reply::say(app, gid, "reply.undo.not_current", &vars).await;
            return Ok((said, false));
        }
    }
    let reason = format!("Undo of case #{case_no}");
    let who = (actor_id, level);
    let why = (Some(reason.as_str()), None);
    // Boxed: the command's future is large, and every interaction handler would carry it.
    let done = Box::pin(modcmds::run(app, gid, owner, who, channel, cmd, why, false)).await?;
    // A lift opens its own case; a refusal opens none and closes nothing.
    let worked = done.case.is_some() || done.text == modcmds::closed_text(app, gid, case_no).await;
    Ok((done.text.into(), worked))
}

/// Handles a press on an undo button; `false` when the interaction is not one.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let Some(InteractionData::MessageComponent(c)) = &i.data else {
        return Ok(false);
    };
    let parsed = c
        .custom_id
        .strip_prefix(PREFIX)
        .and_then(|r| r.split_once(':'))
        .and_then(|(n, a)| Some((n.parse::<i32>().ok()?, a)));
    let Some((case_no, action)) = parsed else {
        return Ok(false);
    };
    let ids = (i.guild_id, i.author_id(), i.channel.as_ref().map(|c| c.id));
    let (Some(gid), Some(actor_id), Some(channel)) = ids else {
        return Ok(true);
    };
    let client = &app.http.interaction(Id::new(app.me.get()));
    let defer = InteractionResponse {
        kind: InteractionResponseType::DeferredUpdateMessage,
        data: None,
    };
    client.create_response(i.id, &i.token, &defer).await?;
    let say = |told: Said| async move {
        let mut followup = client.create_followup(&i.token);
        if !told.text.is_empty() {
            followup = followup.content(&told.text);
        }
        followup
            .embeds(&told.embeds)
            .flags(MessageFlags::EPHEMERAL)
            .allowed_mentions(Some(&AllowedMentions::default()))
            .await
            .map(drop)
    };
    // A member without a permit stops here: they reach no lookup about the case or its member,
    // and cannot hold the claim against a moderator.
    let read = async {
        let owner = app.guild_info(gid).await?.owner;
        anyhow::Ok((owner, app.level(gid, owner, actor_id).await?))
    };
    let (owner, level) = match read.await {
        Ok((_, Level::None)) => {
            say(reply::say(app, gid, "reply.undo.no_permit", &[]).await).await?;
            return Ok(true);
        }
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(?e, "undo button: reading the presser's level failed");
            say(went_wrong(app, gid, &e).await).await?;
            return Ok(true);
        }
    };
    // Claimed before acting, so two presses at once do not both act.
    let Some(_acting) = Acting::claim((gid.get(), case_no.into())) else {
        say(reply::say(app, gid, "reply.undo.busy", &[]).await).await?;
        return Ok(true);
    };
    let pressed = press(
        app,
        gid,
        (owner, actor_id, &level),
        channel,
        (case_no, action),
    )
    .await;
    let (text, worked) = match pressed {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(?e, action, "undo button failed");
            (went_wrong(app, gid, &e).await, false)
        }
    };
    if let (true, Some(m)) = (worked, i.message.as_ref()) {
        let label = match action {
            CLOSE => "Close case",
            a => lift_label(a),
        };
        let now = crate::app::now_secs();
        let line = format!("- <@{actor_id}> pressed **{label}** <t:{now}:R>");
        let mut embeds = m.embeds.clone();
        if let Some(e) = embeds.first_mut() {
            match e.fields.iter().position(|f| f.name == ACTION_LOG) {
                // A field holds 1024 characters; past that Discord would refuse the update.
                Some(k) if e.fields[k].value.len() + line.len() < 1024 => {
                    e.fields[k].value = format!("{}\n{line}", e.fields[k].value)
                }
                Some(_) => {}
                // Nor does it take a 26th field.
                None if e.fields.len() < 25 => e.fields.push(EmbedField {
                    inline: false,
                    name: ACTION_LOG.into(),
                    value: line,
                }),
                None => {}
            }
        }
        let rows = without(&m.components, &c.custom_id);
        let edit = client.update_response(&i.token).embeds(Some(&embeds));
        // The work is done either way: a refused edit must not swallow the answer below.
        if let Err(e) = edit.components(Some(&rows)).await {
            tracing::warn!(?e, "undo button: updating the mod log failed");
        }
    }
    say(text).await?;
    Ok(true)
}

async fn went_wrong(app: &App, gid: Id<GuildMarker>, e: &anyhow::Error) -> Said {
    let why = crate::http_err::why(e);
    reply::say(app, gid, "reply.undo.error", &[("error", &why)]).await
}

fn lift_label(action: &str) -> &'static str {
    ["ban", "timeout", "quarantine"]
        .iter()
        .filter_map(|k| lift(k))
        .find(|(a, _)| *a == action)
        .map_or("Undo", |(_, label)| label)
}

#[cfg(test)]
mod tests {
    use super::{command, rows, without, ModCmd};
    use twilight_model::channel::message::Component;

    fn ids(rows: &[Component]) -> Vec<String> {
        rows.iter()
            .flat_map(|r| match r {
                Component::ActionRow(row) => row.components.clone(),
                _ => vec![],
            })
            .filter_map(|c| match c {
                Component::Button(b) => b.custom_id,
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_punishment_gets_its_own_buttons() {
        assert_eq!(ids(&rows(7, "ban")), ["undo:7:unban", "undo:7:close"]);
        assert_eq!(
            ids(&rows(7, "timeout")),
            ["undo:7:untimeout", "undo:7:close"]
        );
        assert_eq!(
            ids(&rows(7, "quarantine")),
            ["undo:7:unquarantine", "undo:7:close"]
        );
        for kind in ["warn", "kick", "softban"] {
            assert_eq!(ids(&rows(7, kind)), ["undo:7:close"], "{kind}");
        }
        // A lift, a purge or a note has nothing to undo.
        for kind in ["unban", "untimeout", "unquarantine", "purge", "slowmode"] {
            assert!(rows(7, kind).is_empty(), "{kind}");
        }
    }

    #[test]
    fn a_button_only_lifts_the_kind_of_its_case() {
        assert!(matches!(
            command("unban", 7, "ban", 5),
            Some(ModCmd::Unban { user: 5 })
        ));
        assert!(matches!(
            command("untimeout", 7, "timeout", 5),
            Some(ModCmd::Untimeout { user: 5 })
        ));
        assert!(matches!(
            command("unquarantine", 7, "quarantine", 5),
            Some(ModCmd::Unquarantine { user: 5 })
        ));
        assert!(matches!(
            command("close", 7, "kick", 5),
            Some(ModCmd::CaseSet {
                case: 7,
                active: false
            })
        ));
        // A forged id: an unquarantine on a ban case, or an unknown action.
        assert!(command("unquarantine", 7, "ban", 5).is_none());
        assert!(command("unban", 7, "warn", 5).is_none());
        assert!(command("delete", 7, "ban", 5).is_none());
    }

    /// Runs only with a scratch Postgres: a press is the presser's own command, so it needs their
    /// permit, and a button aimed at another kind of case does nothing.
    #[tokio::test]
    async fn a_press_needs_the_permit_of_its_command() {
        use serde_json::json;
        let Some(t) = crate::test_support::T::new(959).await else {
            return;
        };
        t.server_config(0, t.owner(), json!({})).await;
        let (g, target) = (t.g(0), t.base * 10 + 3);
        let case = brim_core::cases::NewCase {
            guild_id: g.get() as i64,
            kind: "ban",
            user_id: target as i64,
            mod_id: 1,
            reason: None,
            points: 0,
            duration_s: None,
            proof: None,
            inverse: None,
            feature: "staff",
        };
        let n = brim_core::cases::create(&t.app.pool, case).await.unwrap();
        t.ban(0, target, "x");
        let press = |user: u64, action: &str| -> super::Interaction {
            let user = json!({"id": user.to_string(), "username": "m", "discriminator": "0", "avatar": null});
            serde_json::from_value(json!({
                "id": "5", "application_id": "1", "type": 3, "token": "tok", "version": 1,
                "guild_id": g.to_string(), "channel_id": "1",
                "channel": {"id": "1", "type": 0},
                "data": {"custom_id": format!("undo:{n}:{action}"), "component_type": 2},
                "member": {"user": user, "roles": [], "joined_at": "2020-01-01T00:00:00.000000+00:00",
                    "deaf": false, "mute": false, "flags": 0, "permissions": "0"},
                "message": {"id": "9", "channel_id": "1", "content": "",
                    "author": {"id": "1", "username": "bot", "discriminator": "0", "avatar": null},
                    "timestamp": "2026-01-01T00:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
                    "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
                    "embeds": [{"type": "rich", "fields": []}], "components": rows(n, "ban"),
                    "pinned": false, "type": 0},
                "entitlements": [], "authorizing_integration_owners": {}, "attachment_size_limit": 8388608
            }))
            .unwrap()
        };
        let active = || async {
            sqlx::query_scalar::<_, bool>(
                "SELECT active FROM cases WHERE guild_id=$1 AND case_no=$2",
            )
            .bind(g.get() as i64)
            .bind(n)
            .fetch_one(&t.app.pool)
            .await
            .unwrap()
        };
        // A member without a permit: nothing is lifted, nothing is closed, the log stays as it is.
        for action in ["unban", "close"] {
            super::handle(&t.app, &press(77, action)).await.unwrap();
        }
        assert!(t.banned(0, target).is_some());
        assert!(active().await);
        assert_eq!(t.calls(super::ACTION_LOG), 0);
        assert_eq!(t.calls("You need a permit"), 2);
        // The owner, with a button that does not fit a ban case: nothing happens either.
        super::handle(&t.app, &press(t.owner(), "unquarantine"))
            .await
            .unwrap();
        assert_eq!(t.calls("does not fit"), 1);
        // A later ban of the same member is not this case's to lift, also for the owner.
        let later = brim_core::cases::NewCase {
            guild_id: g.get() as i64,
            kind: "ban",
            user_id: target as i64,
            mod_id: 2,
            reason: None,
            points: 0,
            duration_s: None,
            proof: None,
            inverse: None,
            feature: "staff",
        };
        let later = brim_core::cases::create(&t.app.pool, later).await.unwrap();
        super::handle(&t.app, &press(t.owner(), "unban"))
            .await
            .unwrap();
        assert!(t.banned(0, target).is_some());
        assert_eq!(t.calls("Nothing was lifted"), 1);
        sqlx::query("DELETE FROM cases WHERE guild_id=$1 AND case_no=$2")
            .bind(g.get() as i64)
            .bind(later)
            .execute(&t.app.pool)
            .await
            .unwrap();
        // The owner's Unban lifts the ban and notes it on the log; Close case then closes the case.
        super::handle(&t.app, &press(t.owner(), "unban"))
            .await
            .unwrap();
        assert!(t.banned(0, target).is_none());
        assert_eq!(t.calls(super::ACTION_LOG), 1);
        assert!(active().await);
        super::handle(&t.app, &press(t.owner(), "close"))
            .await
            .unwrap();
        assert!(!active().await);
        t.cleanup().await;
    }

    #[test]
    fn a_used_button_leaves_and_an_empty_row_goes() {
        let left = without(&rows(7, "ban"), "undo:7:unban");
        assert_eq!(ids(&left), ["undo:7:close"]);
        assert!(without(&left, "undo:7:close").is_empty());
    }
}

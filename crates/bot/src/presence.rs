//! The bot's global status and activity from `bot_presence`, set on the dashboard.

use anyhow::Result;
use sqlx::{postgres::PgListener, PgPool};
use twilight_gateway::MessageSender;
use twilight_model::gateway::{
    payload::outgoing::{update_presence::UpdatePresencePayload, UpdatePresence},
    presence::{Activity, ActivityType, MinimalActivity, Status},
    OpCode,
};

type Row = (String, Option<String>, Option<String>);

/// Sends the saved presence. Discord forgets it at every new session, so this runs at each READY.
pub async fn apply(pool: &PgPool, sender: &MessageSender) -> Result<()> {
    // READY and a NOTIFY can run at once; without this an older read could be sent last.
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    let row: Option<Row> = sqlx::query_as("SELECT status, kind, text FROM bot_presence")
        .fetch_optional(pool)
        .await?;
    sender.command(&payload(row))?;
    Ok(())
}

/// Applies every saved change; never returns `Ok`, so the caller restarts it after an error.
pub async fn listen(pool: PgPool, sender: MessageSender) -> Result<()> {
    let mut l = PgListener::connect_with(&pool).await?;
    l.listen("bot_presence").await?;
    // A restarted listener missed what was saved while it was down.
    apply(&pool, &sender).await?;
    loop {
        // None: the connection dropped and reconnects; a change saved meanwhile was not announced.
        l.try_recv().await?;
        apply(&pool, &sender).await?;
    }
}

/// No row: online without an activity.
fn payload(row: Option<Row>) -> UpdatePresence {
    let (status, kind, text) = row.unwrap_or_else(|| ("online".into(), None, None));
    let status = match status.as_str() {
        "idle" => Status::Idle,
        "dnd" => Status::DoNotDisturb,
        "invisible" => Status::Invisible,
        _ => Status::Online,
    };
    let activity = kind.zip(text).map(|(kind, text)| {
        let kind = match kind.as_str() {
            "watching" => ActivityType::Watching,
            "listening" => ActivityType::Listening,
            "competing" => ActivityType::Competing,
            "custom" => ActivityType::Custom,
            _ => ActivityType::Playing,
        };
        // A bot's custom status shows `state`; Discord wants this fixed name with it.
        let custom = kind == ActivityType::Custom;
        let mut a = Activity::from(MinimalActivity {
            kind,
            name: if custom {
                "Custom Status".into()
            } else {
                text.clone()
            },
            url: None,
        });
        a.state = custom.then_some(text);
        a
    });
    // Built directly: `UpdatePresence::new` refuses an empty list, which Discord accepts as "none".
    UpdatePresence {
        d: UpdatePresencePayload {
            activities: activity.into_iter().collect(),
            afk: false,
            since: None,
            status,
        },
        op: OpCode::PresenceUpdate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn sent(status: &str, kind: Option<&str>, text: Option<&str>) -> Value {
        let row = (status.into(), kind.map(Into::into), text.map(Into::into));
        serde_json::to_value(payload(Some(row))).unwrap()
    }

    #[test]
    fn presence_payload_from_row() {
        let none = serde_json::to_value(payload(None)).unwrap();
        assert_eq!(
            none,
            json!({"op": 3, "d": {"activities": [], "afk": false, "since": null, "status": "online"}})
        );
        for s in ["online", "idle", "dnd", "invisible"] {
            let v = sent(s, None, None);
            assert_eq!(
                (&v["d"]["status"], &v["d"]["activities"]),
                (&json!(s), &json!([]))
            );
        }
        let watching = &sent("idle", Some("watching"), Some("the mods"))["d"]["activities"][0];
        assert_eq!(
            (&watching["type"], &watching["name"], watching.get("state")),
            (&json!(3), &json!("the mods"), None)
        );
        let custom = &sent("dnd", Some("custom"), Some("on duty"))["d"]["activities"][0];
        assert_eq!(
            (&custom["type"], &custom["name"], &custom["state"]),
            (&json!(4), &json!("Custom Status"), &json!("on duty"))
        );
    }
}

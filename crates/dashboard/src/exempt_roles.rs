//! Overview row and Fix for the roles older versions let exempt members from Auto Mod and the
//! golden channel. A role exempts nobody now (owner rule: trust is by user id and permits), so
//! until the Fix runs their members are checked like everyone else, the safe direction. The Fix
//! queues the bot's job `exempt_roles_move` (`crates/bot/src/exempt_roles.rs`), which pages every
//! member, so it holds on any server size.

use crate::{
    access::Level,
    api::{log_by, Res},
    auth::Sess,
    ApiError, AppState,
};
use axum::Json;
use brim_antinuke::{
    security::{Check, Fix, Severity},
    RoleImg,
};
use serde_json::{json, Value};

/// The [`Fix::setting`] of the row.
pub(crate) const FIX: &str = "exempt_roles";

/// The bot's job.
const JOB: &str = "exempt_roles_move";

/// The stored role lists, as (config path, where it exempted).
const LISTS: [(&str, &str); 5] = [
    ("/whitelist/roles", "Spam"),
    ("/whitelist/invites/roles", "Invite links"),
    ("/whitelist/mentions/roles", "Mentions"),
    ("/whitelist/everyone/roles", "@everyone"),
    ("/catch/roles", "Golden channel"),
];

/// Names shown in the row at most; the count covers everyone read.
const NAMES: usize = 10;

/// Each stored role with the places it exempted from, in stored order.
pub(crate) fn stored(config: &Value) -> Vec<(u64, Vec<&'static str>)> {
    let mut out: Vec<(u64, Vec<&'static str>)> = vec![];
    for (path, place) in LISTS {
        let ids = config.pointer(path).and_then(Value::as_array);
        for id in ids.into_iter().flatten() {
            // Text or number, as the bot reads both.
            let id = id
                .as_str()
                .and_then(|x| x.parse::<u64>().ok())
                .or(id.as_u64());
            let Some(id) = id else {
                continue;
            };
            match out.iter_mut().find(|(r, _)| *r == id) {
                Some((_, places)) => places.push(place),
                None => out.push((id, vec![place])),
            }
        }
    }
    out
}

async fn queued(st: &AppState, guild: i64) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(
        "select exists(select 1 from scheduled_jobs where guild_id = $1 and kind = $2)",
    )
    .bind(guild)
    .bind(JOB)
    .fetch_one(&st.db)
    .await?)
}

/// The row while roles are stored: the roles, where they exempted, and who holds them now.
pub(crate) async fn overview_check(
    st: &AppState,
    guild: i64,
    config: &Value,
    roles: &[RoleImg],
    level: Level,
) -> Result<Option<Check>, ApiError> {
    let saved = stored(config);
    if saved.is_empty() {
        return Ok(None);
    }
    let named: Vec<String> = saved
        .iter()
        .map(|(id, places)| {
            let name = roles.iter().find(|r| r.id == *id).map_or_else(
                || format!("a deleted role ({id})"),
                |r| format!("@{}", r.name),
            );
            format!("{name} ({})", places.join(", "))
        })
        .collect();
    let mut detail = format!(
        "A role no longer keeps members out of Auto Mod or the golden channel. Saved roles: {}.",
        named.join("; ")
    );
    if queued(st, guild).await? {
        detail.push_str(" Brim is moving their members now. The result goes to the security log.");
        return Ok(Some(row(detail, None)));
    }
    // Names only for those who can press the Fix (the bar of `ops::fix`); paging costs Discord
    // calls too, so a Viewer's load reads no members.
    if level < Level::Admin {
        detail.push_str(
            " Their members are checked by Auto Mod until a Trusted Admin or an owner moves them.",
        );
        return Ok(Some(row(detail, None)));
    }
    let ids: Vec<String> = saved.iter().map(|(id, _)| id.to_string()).collect();
    let (mut count, mut names) = (0u64, vec![]);
    let more = crate::ops::member_pages(st, guild, |m| {
        let holds = m["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r.as_str().is_some_and(|r| ids.iter().any(|i| i == r)));
        if m["user"]["bot"] == true || !holds {
            return;
        }
        count += 1;
        if names.len() < NAMES {
            let name = [
                &m["nick"],
                &m["user"]["global_name"],
                &m["user"]["username"],
            ]
            .into_iter()
            .find_map(|v| v.as_str())
            .unwrap_or("Unknown member");
            names.push(name.to_string());
        }
    })
    .await;
    match more {
        Ok(more) if count > 0 => {
            let at = if more { "At least " } else { "" };
            let rest = count.saturating_sub(names.len() as u64);
            let tail = if rest > 0 {
                format!(" and {rest} more")
            } else {
                String::new()
            };
            let who = if count == 1 {
                "member holds"
            } else {
                "members hold"
            };
            detail.push_str(&format!(
                " {at}{count} {who} them now: {}{tail}. Unless they are exempt by user ID already, Auto Mod checks them like everyone else until you move them.",
                names.join(", ")
            ));
        }
        Ok(_) => detail.push_str(" Nobody holds them now."),
        Err(_) => detail.push_str(" Brim could not read who holds them now."),
    }
    detail.push_str(
        " Move them: Brim adds each member by user ID to the permit Automod Immunity. A member who has another permit, and the golden channel's members, go on the Members list instead. Then the saved roles are cleared.",
    );
    Ok(Some(row(
        detail,
        Some(Fix {
            label: "Move to members",
            role_id: None,
            channel_id: None,
            setting: Some(FIX),
            move_under_bot: false,
        }),
    )))
}

fn row(detail: String, fix: Option<Fix>) -> Check {
    Check {
        id: "exempt_roles".into(),
        severity: Severity::Warn,
        title: "Exemptions by role",
        detail,
        fix,
        steps: vec![],
    }
}

/// Queues the move once; the bot reads the roles again under the config lock.
pub(crate) async fn fix(st: &AppState, s: &Sess, guild: i64, config: &Value) -> Res {
    if stored(config).is_empty() {
        return Err(ApiError::BadRequest(
            "No roles are saved as exemptions.".into(),
        ));
    }
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "insert into scheduled_jobs (guild_id, kind, payload, run_at)
         select $1, $2, $3, to_timestamp($4::float8) where not exists (
           select 1 from scheduled_jobs where guild_id = $1 and kind = $2)",
    )
    .bind(guild)
    .bind(JOB)
    .bind(json!({"by": s.user_id}))
    .bind(now as f64)
    .execute(&st.db)
    .await?;
    log_by(
        st,
        s,
        guild,
        config,
        None,
        "moved the members of the exempt roles to user IDs from the security score",
    )
    .await;
    Ok(Json(json!({"ok": true, "queued": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_joins_the_places_of_one_role() {
        let c = json!({"whitelist": {"roles": ["1"], "invites": {"roles": ["1", "2"]}}, "catch": {"roles": ["2", "x"]}});
        assert_eq!(
            stored(&c),
            vec![
                (1, vec!["Spam", "Invite links"]),
                (2, vec!["Invite links", "Golden channel"])
            ]
        );
        assert!(stored(&json!({})).is_empty());
    }
}

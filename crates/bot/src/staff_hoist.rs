//! A one-time pass per server that shows each staff role (the Admins and Moderators permit roles
//! and the level roles) as its own group in the member list (owner decision 2026-11-17). New and reused
//! roles get it from Auto Setup; this covers servers set up before. Roles at or above the bot
//! are left alone, and the bot's own edits never trip anti-nuke (it skips its own actions).

use crate::app::App;
use anyhow::Result;
use twilight_model::{
    guild::Role,
    id::{marker::GuildMarker, Id},
};

/// Runs once per server: the row is claimed before any edit, so a later start never undoes an
/// owner's choice. What it needs is read first, so a failed read leaves it for the next start.
pub(crate) async fn once(app: &App, gid: Id<GuildMarker>, roles: &[Role]) -> Result<()> {
    let g = gid.get() as i64;
    let staff = brim_core::permits::all_staff_roles(&app.pool, g).await?;
    // A server without staff roles yet is left for a later start: Auto Setup hoists new ones.
    if staff.is_empty() {
        return Ok(());
    }
    let info = app.guild_info(gid).await?;
    let claimed: Option<bool> = sqlx::query_scalar(
        "INSERT INTO staff_hoist_done (guild_id) VALUES ($1) ON CONFLICT DO NOTHING RETURNING true",
    )
    .bind(g)
    .fetch_optional(&app.pool)
    .await?;
    if claimed.is_none() {
        return Ok(());
    }
    let (mut tried, mut failed) = (0, 0);
    for r in roles
        .iter()
        .filter(|r| !r.hoist && staff.contains(&(r.id.get() as i64)))
    {
        if info.roles.get(&r.id).is_none_or(|p| *p >= info.bot_top) {
            continue;
        }
        tried += 1;
        if let Err(e) = app.http.update_role(gid, r.id).hoist(true).await {
            failed += 1;
            tracing::warn!(
                ?e,
                guild = g,
                role = r.id.get(),
                "showing a staff role separately failed"
            );
        }
    }
    // Every edit refused (no Manage Roles, a Discord outage): the next start tries again.
    if tried > 0 && failed == tried {
        sqlx::query("DELETE FROM staff_hoist_done WHERE guild_id = $1")
            .bind(g)
            .execute(&app.pool)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::test_support::{ME, T};
    use serde_json::json;
    use twilight_model::guild::Role;

    fn role(id: u64) -> Role {
        serde_json::from_value(
            json!({"id": id.to_string(), "name": "r", "color": 0, "colors": {"primary_color": 0}, "hoist": false,
            "managed": false, "mentionable": false, "permissions": "0", "position": 1, "flags": 0}),
        )
        .unwrap()
    }

    /// Staff roles below the bot are hoisted once; one above it is left; a refused pass is retried.
    #[tokio::test]
    async fn staff_roles_are_hoisted_once_below_the_bot() {
        let Some(t) = T::new(972).await else { return };
        for i in [0, 1] {
            t.server_config(i, t.owner(), json!({})).await;
            t.app.invalidate_guild(t.g(i));
            let g = t.g(i).get();
            t.set(|m| {
                m.roles.insert(
                    g,
                    vec![
                        (g, false, 0, 0),
                        (20, false, 0, 1),
                        (30, false, 0, 11),
                        (40, true, 8, 10),
                    ],
                );
                m.members.insert((g, ME), vec![40]);
            });
            sqlx::query("INSERT INTO level_roles (guild_id, level, role_id) VALUES ($1, 'owner', 20), ($1, 'trusted_admin', 30)")
                .bind(g as i64)
                .execute(&t.app.pool)
                .await
                .unwrap();
        }
        let (g, roles) = (t.g(0), [role(20), role(30)]);
        super::once(&t.app, g, &roles).await.unwrap();
        let at = |r: u64| format!("PATCH /guilds/{}/roles/{r}", g.get());
        assert_eq!(t.calls(&at(20)), 1);
        assert_eq!(t.calls(&at(30)), 0, "above the bot");
        // A later start does nothing, even when the owner un-hoisted the role since.
        super::once(&t.app, g, &roles).await.unwrap();
        assert_eq!(t.calls(&at(20)), 1);
        // Every edit refused: the server is not marked, so the next start tries again.
        t.set(|m| m.fail_role_edit = true);
        super::once(&t.app, t.g(1), &roles).await.unwrap();
        let done: i64 =
            sqlx::query_scalar("SELECT count(*) FROM staff_hoist_done WHERE guild_id = $1")
                .bind(t.g(1).get() as i64)
                .fetch_one(&t.app.pool)
                .await
                .unwrap();
        assert_eq!(done, 0);
        t.exec("DELETE FROM staff_hoist_done WHERE guild_id BETWEEN $1 AND $1 + 9")
            .await;
        t.cleanup().await;
    }
}

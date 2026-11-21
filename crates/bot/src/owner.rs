//! Server owners: `guild_owners` (Raider List counts owners by it) and the owner handover.
use anyhow::Result;
use serde_json::{json, Value};
use twilight_model::id::{
    marker::{GuildMarker, UserMarker},
    Id,
};

use crate::{app::App, http_err, reply, reply::md, verify};

/// The owner from Discord, not the 5-minute cache; None when the bot cannot see the server.
pub(crate) async fn live_owner(app: &App, gid: Id<GuildMarker>) -> Result<Option<Id<UserMarker>>> {
    match app.http.guild(gid).await {
        Ok(r) => Ok(Some(r.model().await?.owner_id)),
        Err(e) if http_err::no_access(&e) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// [`on_owner`] from a gateway event. A failure queues `owner_check`, which runs it again (each
/// failed try waits for the job lease, up to `jobs::MAX_ATTEMPTS`), so the reset is not left to the
/// next guild event (R-h3 round 2, 3). Meanwhile the dashboard keeps the owner-only writes closed.
pub async fn owner_seen(app: &App, gid: Id<GuildMarker>, owner: Id<UserMarker>) -> Result<()> {
    if let Err(e) = on_owner(app, gid, owner).await {
        tracing::warn!(?e, %gid, "owner change not processed, retrying as a job");
        sqlx::query(
            "INSERT INTO scheduled_jobs (guild_id, kind, payload, run_at)
             SELECT $1, 'owner_check', '{}', now() + interval '30 seconds'
             WHERE NOT EXISTS (SELECT 1 FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'owner_check')",
        )
        .bind(gid.get() as i64)
        .execute(&app.pool)
        .await?;
    }
    Ok(())
}

/// Job `owner_check`: reads the owner from Discord now (not the cache) and runs [`on_owner`].
pub async fn owner_check_job(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let owner = app.http.guild(gid).await?.model().await?.owner_id;
    on_owner(app, gid, owner).await
}

/// Keeps `guild_owners` current (GuildCreate, GuildUpdate). On a new owner  the Extra Owners
/// and Trusted Admins the previous owner set are removed, with the roles of their owner, full
/// immunity and dashboard-edit permits. What was removed is kept in `owner_handovers`, so the new
/// owner can keep it from the dashboard's Overview.
pub async fn on_owner(app: &App, gid: Id<GuildMarker>, discord: Id<UserMarker>) -> Result<()> {
    // One transaction under the config lock: a crash cannot record the new owner without the reset.
    let (mut tx, version, mut doc) = crate::settings::locked(app, gid).await?;
    let g = gid.get() as i64;
    // A rescue claim keeps the Owner level when Discord ownership moves, so the paused account
    // cannot end it by handing the server to an alt; it ends when ownership lands on the holder
    // (owner decision 2026-11-20). `guild_owners` keeps the bot owner, so a claim or a give-back reaches
    // the handover below like an ownership change.
    sqlx::query("DELETE FROM rescue_owners WHERE server_id = $1 AND user_id = $2")
        .bind(g)
        .bind(discord.get() as i64)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE rescue_owners SET discord_owner = $2 WHERE server_id = $1")
        .bind(g)
        .bind(discord.get() as i64)
        .execute(&mut *tx)
        .await?;
    let owner =
        Id::new(brim_core::permits::bot_owner(&mut *tx, g, discord.get() as i64).await? as u64);
    let old: Option<i64> = sqlx::query_scalar(
        "WITH old AS (SELECT owner_id FROM guild_owners WHERE guild_id = $1)
         INSERT INTO guild_owners (guild_id, owner_id) VALUES ($1, $2)
         ON CONFLICT (guild_id) DO UPDATE SET owner_id = $2, seen_at = now()
         RETURNING (SELECT owner_id FROM old)",
    )
    .bind(g)
    .bind(owner.get() as i64)
    .fetch_one(&mut *tx)
    .await?;
    let Some(old) = old.map(|o| Id::new(o as u64)).filter(|o| *o != owner) else {
        tx.commit().await?;
        return Ok(());
    };
    let eo: Vec<i64> =
        sqlx::query_scalar("DELETE FROM extra_owners WHERE guild_id = $1 RETURNING user_id")
            .bind(g)
            .fetch_all(&mut *tx)
            .await?;
    let ta: Vec<i64> =
        sqlx::query_scalar("DELETE FROM trusted_admins WHERE guild_id = $1 RETURNING user_id")
            .bind(g)
            .fetch_all(&mut *tx)
            .await?;
    // Guarded permits (owner-like, dashboard edit, any immunity flag but the filter ones) are
    // paused too; weaker ones stay on and are only listed.
    let all: Vec<(i64, Vec<String>, Vec<String>)> =
        sqlx::query_as("SELECT id, perms, whitelist FROM permits WHERE guild_id = $1")
            .bind(g)
            .fetch_all(&mut *tx)
            .await?;
    let strong: Vec<i64> = all
        .into_iter()
        .filter(|(_, p, w)| brim_core::permits::guarded(p, w))
        .map(|(id, ..)| id)
        .collect();
    // Once permits belong to user ids, their members are paused, and `permit_sync` takes the roles.
    let by_member = brim_core::permits::members_migrated(&mut *tx, g).await?;
    let (sql, key, held) = if by_member {
        (
            "DELETE FROM permit_members m USING permits p WHERE m.permit_id = p.id AND p.id = ANY($1)
             RETURNING p.name, m.user_id",
            "user",
            "permit_members",
        )
    } else {
        (
            "DELETE FROM permit_roles r USING permits p WHERE r.permit_id = p.id AND p.id = ANY($1)
             RETURNING p.name, r.role_id",
            "role",
            "permit_roles",
        )
    };
    let paused: Vec<(String, i64)> = sqlx::query_as(sql)
        .bind(&strong)
        .fetch_all(&mut *tx)
        .await?;
    // The quarantine whitelist exempts by user id, the same kind of trust as EO/TA.
    let qw: Vec<i64> = doc["antinuke"]["quarantine_whitelist"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| {
            u.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| u.as_i64())
        })
        .collect();
    if !qw.is_empty() {
        doc["antinuke"]["quarantine_whitelist"] = json!([]);
    }
    let other: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT name FROM permits p WHERE guild_id = $1
           AND EXISTS (SELECT 1 FROM {held} r WHERE r.permit_id = p.id)
         ORDER BY rank DESC, name"
    )))
    .bind(g)
    .fetch_all(&mut *tx)
    .await?;
    let paused_json: Vec<Value> = paused
        .iter()
        .map(|(permit, id)| json!({"permit": permit, key: id.to_string()}))
        .collect();
    // Always: the new owner gets the Owner role and the old one loses it (level roles).
    let now = crate::app::now_secs();
    brim_core::jobs::enqueue_once(&mut *tx, g, "permit_sync", now).await?;
    // The rescue key made before this change works 7 more days; then its job asks for a new one.
    let grace = brim_core::permits::RESCUE_GRACE_DAYS * 86_400 + 60;
    brim_core::jobs::enqueue(&mut *tx, g, "rescue_expired", json!({}), now + grace).await?;
    // Only a key made by the owner handing over gets the 7 days, once: a key made before the
    // previous change is expired or still inside that change's 7 days, so it goes now.
    sqlx::query(
        "DELETE FROM rescue_key k WHERE guild_id = $1
           AND EXISTS (SELECT 1 FROM owner_handovers h WHERE h.guild_id = k.guild_id AND h.at > k.created_at)",
    )
    .bind(g)
    .execute(&mut *tx)
    .await?;
    // Recorded on every change, even with nothing to reset: the dashboard refuses the previous
    // owner by it, and a rollback to before it keeps what the change paused. Back to the owner the
    // last change paused (a rescue give-back) and not kept yet: that owner's paused lists stay on
    // the Keep card next to this change's.
    sqlx::query(
        "INSERT INTO owner_handovers (guild_id, old_owner, new_owner, extra_owners, trusted_admins,
           paused_roles, other_permits, quarantine_whitelist, at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, clock_timestamp())
         ON CONFLICT (guild_id) DO UPDATE SET old_owner = $2, new_owner = $3,
           extra_owners = $4 || CASE WHEN owner_handovers.old_owner = $3 AND owner_handovers.kept_at IS NULL THEN owner_handovers.extra_owners ELSE '{}' END,
           trusted_admins = $5 || CASE WHEN owner_handovers.old_owner = $3 AND owner_handovers.kept_at IS NULL THEN owner_handovers.trusted_admins ELSE '{}' END,
           paused_roles = $6 || CASE WHEN owner_handovers.old_owner = $3 AND owner_handovers.kept_at IS NULL THEN owner_handovers.paused_roles ELSE '[]' END,
           other_permits = $7 || CASE WHEN owner_handovers.old_owner = $3 AND owner_handovers.kept_at IS NULL THEN owner_handovers.other_permits ELSE '{}' END,
           quarantine_whitelist = $8 || CASE WHEN owner_handovers.old_owner = $3 AND owner_handovers.kept_at IS NULL THEN owner_handovers.quarantine_whitelist ELSE '{}' END,
           at = clock_timestamp(), kept_at = NULL",
    )
    .bind(g)
    .bind(old.get() as i64)
    .bind(owner.get() as i64)
    .bind(&eo)
    .bind(&ta)
    .bind(json!(paused_json))
    .bind(&other)
    .bind(&qw)
    .execute(&mut *tx)
    .await?;
    // A config version of its own, so History shows it and a rollback sees the removal.
    crate::settings::append(app, tx, gid, version, &mut doc, app.me.get()).await?;
    // From here on nothing can fail: only cache drops and best-effort DMs.
    app.forget_cfg(gid.get());
    app.invalidate_guild(gid);
    let name = md(&verify::guild_name(app, gid).await);
    let paused: Vec<String> = paused.into_iter().map(|p| p.0).collect();
    let h = Handover {
        eo: &eo,
        ta: &ta,
        paused: &paused,
        other: &other,
        qw: &qw,
    };
    if let Some(text) = handover_text(&name, &h, gid) {
        reply::dm_msg(
            app,
            Some(gid),
            owner,
            "dm.owner_handover",
            &[("text", &text)],
            &[],
        )
        .await;
    }
    Ok(())
}

/// 7 days after an ownership change the rescue key made before it stopped working, so the owner
/// is asked for a new one (owner decision 11-17). Nothing when no key was made before the change, or a
/// later change started its own 7 days.
pub async fn rescue_expired_job(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let Some(owner) = rescue_reminder_to(&app.pool, gid.get() as i64).await? else {
        return Ok(());
    };
    let name = md(&verify::guild_name(app, gid).await);
    let page = match crate::general::dashboard() {
        Some(u) => format!("{}/#/{gid}/rescue", u.trim_end_matches('/')),
        None => "the dashboard's Rescue Key page".into(),
    };
    let vars = [("name", name.as_str()), ("page", page.as_str())];
    reply::dm_msg(
        app,
        Some(gid),
        Id::new(owner as u64),
        "dm.rescue_expired",
        &vars,
        &[],
    )
    .await;
    Ok(())
}

/// The new owner to remind, while the rescue key made before the last change is expired.
async fn rescue_reminder_to(pool: &sqlx::PgPool, guild: i64) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT new_owner FROM owner_handovers o WHERE guild_id = $1
           AND EXISTS (SELECT 1 FROM rescue_key k WHERE k.guild_id = o.guild_id AND {})",
        brim_core::permits::rescue_expired_sql()
    )))
    .bind(guild)
    .fetch_optional(pool)
    .await?)
}

/// What [`on_owner`] did at an ownership change, for the new owner's DM.
struct Handover<'a> {
    eo: &'a [i64],
    ta: &'a [i64],
    /// Names of the permits paused, once per role (or per member, once permits belong to user ids).
    paused: &'a [String],
    /// Weaker permits that stayed on.
    other: &'a [String],
    /// Users removed from the quarantine whitelist.
    qw: &'a [i64],
}

/// The new owner's DM after [`on_owner`], or None when it changed nothing worth telling.
fn handover_text(name: &str, h: &Handover, gid: Id<GuildMarker>) -> Option<String> {
    let mentions = |ids: &[i64]| {
        ids.iter()
            .map(|u| format!("<@{u}>"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut names: Vec<&str> = h.paused.iter().map(String::as_str).collect();
    names.sort();
    names.dedup();
    let mut done = vec![];
    if !h.eo.is_empty() {
        done.push(format!("removed the Extra Owner(s) {}", mentions(h.eo)));
    }
    if !h.ta.is_empty() {
        done.push(format!("removed the Trusted Admin(s) {}", mentions(h.ta)));
    }
    if !h.qw.is_empty() {
        done.push(format!(
            "removed {} from the quarantine whitelist",
            mentions(h.qw)
        ));
    }
    if !names.is_empty() {
        done.push(format!(
            "paused the permit(s) {} (owner, dashboard edit, or an anti-nuke or quarantine whitelist flag)",
            names.join(", ")
        ));
    }
    if done.is_empty() && h.other.is_empty() {
        return None;
    }
    let link = crate::settings::dashboard_link(gid.get());
    let mut text = format!("You are the new owner of **{name}**.");
    if !done.is_empty() {
        text.push_str(&format!(" To protect you, Brim {}.", done.join("; ")));
    }
    if !(h.eo.is_empty() && h.ta.is_empty() && h.qw.is_empty() && names.is_empty()) {
        text.push_str(&format!(" The previous owner set them. Keep only the ones you trust, one by one, on the Overview of the {link}."));
    }
    if !h.other.is_empty() {
        text.push_str(&format!(
            " These permits from the previous owner stay on: {}. Check who holds them.",
            h.other.join(", ")
        ));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::T;

    /// Owner decision 2026-11-20: a rescue claim reaches on_owner as a handover from the Discord owner to
    /// the claimer, who then holds the Owner level. Handing the server to another account (a thief's
    /// alt) keeps it; Discord ownership landing on the claimer ends it.
    #[tokio::test]
    async fn a_rescue_claim_moves_with_discord_ownership_until_it_lands_on_the_claimer() {
        let Some(t) = T::new(207).await else { return };
        let (o, new, later) = (t.owner(), t.base + 8, t.base + 6);
        t.server_config(0, o, json!({})).await;
        t.exec("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, $1 + 1)")
            .await;
        t.exec("INSERT INTO rescue_owners (server_id, user_id, discord_owner) VALUES ($1, $1 + 8, $1 + 9)")
            .await;
        on_owner(&t.app, t.g(0), Id::new(o)).await.unwrap();
        let g = t.g(0).get() as i64;
        let owners = |sql: &'static str| {
            sqlx::query_scalar::<_, i64>(sql)
                .bind(g)
                .fetch_one(&t.app.pool)
        };
        assert_eq!(
            owners("SELECT owner_id FROM guild_owners WHERE guild_id = $1")
                .await
                .unwrap(),
            new as i64
        );
        assert_eq!(
            owners("SELECT old_owner FROM owner_handovers WHERE guild_id = $1")
                .await
                .unwrap(),
            o as i64
        );
        assert_eq!(
            t.count("SELECT count(*) FROM extra_owners WHERE guild_id = $1")
                .await,
            0,
            "the paused account's Extra Owners are paused too"
        );
        let level = |u: u64| brim_core::permits::resolve(&t.app.pool, g, o as i64, u as i64, false);
        assert_eq!(level(new).await.unwrap(), brim_core::permits::Level::Owner);
        assert_eq!(level(o).await.unwrap(), brim_core::permits::Level::None);
        // The paused account hands the server to an alt: the claimer keeps the level.
        on_owner(&t.app, t.g(0), Id::new(later)).await.unwrap();
        assert_eq!(
            owners("SELECT discord_owner FROM rescue_owners WHERE server_id = $1")
                .await
                .unwrap(),
            later as i64
        );
        assert_eq!(
            owners("SELECT owner_id FROM guild_owners WHERE guild_id = $1")
                .await
                .unwrap(),
            new as i64
        );
        let alt = brim_core::permits::resolve(&t.app.pool, g, later as i64, later as i64, false);
        assert_eq!(alt.await.unwrap(), brim_core::permits::Level::None);
        // Discord ownership lands on the claimer: the claim has done its job.
        on_owner(&t.app, t.g(0), Id::new(new)).await.unwrap();
        assert_eq!(
            t.count("SELECT count(*) FROM rescue_owners WHERE server_id = $1")
                .await,
            0
        );
        assert_eq!(
            owners("SELECT owner_id FROM guild_owners WHERE guild_id = $1")
                .await
                .unwrap(),
            new as i64
        );
        t.cleanup().await;
    }

    #[tokio::test]
    async fn an_owner_change_stores_and_removes_the_old_staff() {
        let Some(t) = T::new(202).await else { return };
        let (o, new) = (t.owner(), t.base + 8);
        t.server_config(0, o, json!({})).await;
        t.exec("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, $1 + 1)")
            .await;
        t.exec("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $1 + 2)")
            .await;
        for (name, perms, wl, role) in [
            ("Admins", "{dashboard_edit}", "{}", 3),
            ("Immune", "{}", "{all}", 4),
            ("Mods", "{ban}", "{links}", 5),
            ("Nukers", "{}", "{wl_role_delete}", 6),
        ] {
            sqlx::query(
                "WITH p AS (INSERT INTO permits (guild_id, name, perms, whitelist) VALUES ($1, $2, $3::text[], $4::text[]) RETURNING id)
                 INSERT INTO permit_roles (permit_id, guild_id, role_id) SELECT id, $1, $1 + $5 FROM p",
            )
            .bind(t.base as i64)
            .bind(name)
            .bind(perms)
            .bind(wl)
            .bind(role as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        }
        t.exec("UPDATE guild_config SET config = jsonb_set(config, '{antinuke}', jsonb_build_object('quarantine_whitelist', jsonb_build_array(($1 + 7)::text))) WHERE guild_id = $1").await;
        t.set(|m| {
            m.owners.insert(t.g(0).get(), new);
        });
        on_owner(&t.app, t.g(0), Id::new(new)).await.unwrap();
        let cfg: Value = sqlx::query_scalar(
            "SELECT config FROM guild_config WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
        )
        .bind(t.g(0).get() as i64)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(cfg["antinuke"]["quarantine_whitelist"], json!([]));
        assert_eq!(
            cfg["permits"]["eo"],
            json!([]),
            "the version records the removal"
        );
        let staff = t
            .count("SELECT count(*) FROM extra_owners WHERE guild_id = $1")
            .await
            + t.count("SELECT count(*) FROM trusted_admins WHERE guild_id = $1")
                .await;
        assert_eq!(staff, 0);
        let b = t.base as i64;
        let (eo, ta, mut paused, other, qw): (Vec<i64>, Vec<i64>, Value, Vec<String>, Vec<i64>) =
            sqlx::query_as(
                "SELECT extra_owners, trusted_admins, paused_roles, other_permits, quarantine_whitelist
                 FROM owner_handovers WHERE guild_id = $1",
            )
            .bind(b)
            .fetch_one(&t.app.pool)
            .await
            .unwrap();
        assert_eq!((eo, ta), (vec![b + 1], vec![b + 2]));
        paused
            .as_array_mut()
            .unwrap()
            .sort_by_key(|p| p["role"].to_string());
        assert_eq!(
            paused,
            json!([{"permit": "Admins", "role": (b + 3).to_string()}, {"permit": "Immune", "role": (b + 4).to_string()},
                   {"permit": "Nukers", "role": (b + 6).to_string()}])
        );
        assert_eq!(qw, [b + 7]);
        assert_eq!(other, ["Mods"], "a weaker permit stays on");
        assert_eq!(
            t.count("SELECT count(*) FROM permit_roles WHERE guild_id = $1")
                .await,
            1
        );
        // Seeing the same owner again changes nothing.
        on_owner(&t.app, t.g(0), Id::new(new)).await.unwrap();
        assert_eq!(
            t.count("SELECT count(*) FROM owner_handovers WHERE guild_id = $1")
                .await,
            1
        );
        let day7 =
            "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'rescue_expired'
             AND run_at BETWEEN now() + interval '7 days' AND now() + interval '7 days 2 minutes'";
        assert_eq!(t.count(day7).await, 1, "one rescue key reminder, at day 7");
        t.cleanup().await;
    }

    /// Owner decision 11-17: only a key made by the owner who hands over gets the 7 days, once. A later
    /// change neither brings an expired key back nor gives an old key 7 more days.
    #[tokio::test]
    async fn a_second_owner_change_does_not_revive_or_extend_the_old_rescue_key() {
        let Some(t) = T::new(203).await else { return };
        let (o, b) = (t.owner(), t.base as i64);
        t.server_config(0, o, json!({})).await;
        let key = "SELECT count(*) FROM rescue_key WHERE guild_id = $1";
        for (handover_ago, owner) in [("8 days", 8), ("1 day", 9)] {
            // The key was made before the change, which is expired (8 days) or inside its 7 days.
            t.exec("INSERT INTO rescue_key (guild_id, key_hash, created_at) VALUES ($1, 'h', now() - interval '30 days')
                    ON CONFLICT (guild_id) DO UPDATE SET created_at = excluded.created_at").await;
            let at = format!("UPDATE owner_handovers SET at = now() - interval '{handover_ago}' WHERE guild_id = $1");
            sqlx::query(sqlx::AssertSqlSafe(at))
                .bind(b)
                .execute(&t.app.pool)
                .await
                .unwrap();
            if handover_ago == "8 days" {
                // The first loop has no handover row yet: make one.
                if t.count("SELECT count(*) FROM owner_handovers WHERE guild_id = $1")
                    .await
                    == 0
                {
                    t.exec("INSERT INTO owner_handovers (guild_id, old_owner, new_owner, at) VALUES ($1, 1, $1 + 7, now() - interval '8 days')").await;
                }
                assert_eq!(
                    rescue_reminder_to(&t.app.pool, b).await.unwrap(),
                    Some(b + 7)
                );
            }
            on_owner(&t.app, t.g(0), Id::new(t.base + owner))
                .await
                .unwrap();
            assert_eq!(
                t.count(key).await,
                0,
                "the key from before {handover_ago} ago is gone"
            );
            assert_eq!(rescue_reminder_to(&t.app.pool, b).await.unwrap(), None);
        }
        // A key the current owner made after the change gets its 7 days at the next change.
        t.exec("INSERT INTO rescue_key (guild_id, key_hash) VALUES ($1, 'h')")
            .await;
        on_owner(&t.app, t.g(0), Id::new(t.base + 10))
            .await
            .unwrap();
        assert_eq!(t.count(key).await, 1);
        t.exec("DELETE FROM rescue_key WHERE guild_id = $1").await;
        t.cleanup().await;
    }

    /// D47 once permits belong to user ids: a strong permit's members are paused, not its roles,
    /// and `permit_sync` is queued to take the roles off.
    #[tokio::test]
    async fn an_owner_change_pauses_strong_permits_by_member() {
        let Some(t) = T::new(605).await else { return };
        let (o, new) = (t.owner(), t.base + 8);
        t.server_config(0, o, json!({})).await;
        t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
            .await;
        for (name, perms, user) in [("Admins", "{dashboard_edit}", 3), ("Mods", "{ban}", 5)] {
            sqlx::query(
                "WITH p AS (INSERT INTO permits (guild_id, name, perms) VALUES ($1, $2, $3::text[]) RETURNING id),
                 r AS (INSERT INTO permit_roles (permit_id, guild_id, role_id) SELECT id, $1, $1 + $4 + 10 FROM p)
                 INSERT INTO permit_members (permit_id, guild_id, user_id) SELECT id, $1, $1 + $4 FROM p",
            )
            .bind(t.base as i64)
            .bind(name)
            .bind(perms)
            .bind(user as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        }
        t.set(|m| {
            m.owners.insert(t.g(0).get(), new);
        });
        on_owner(&t.app, t.g(0), Id::new(new)).await.unwrap();
        let b = t.base as i64;
        let (paused, other): (Value, Vec<String>) = sqlx::query_as(
            "SELECT paused_roles, other_permits FROM owner_handovers WHERE guild_id = $1",
        )
        .bind(b)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(
            paused,
            json!([{"permit": "Admins", "user": (b + 3).to_string()}])
        );
        assert_eq!(other, ["Mods"]);
        let left: Vec<i64> =
            sqlx::query_scalar("SELECT user_id FROM permit_members WHERE guild_id = $1")
                .bind(b)
                .fetch_all(&t.app.pool)
                .await
                .unwrap();
        assert_eq!(left, [b + 5], "the weaker permit keeps its member");
        assert_eq!(
            t.count("SELECT count(*) FROM permit_roles WHERE guild_id = $1")
                .await,
            2,
            "the roles stay on the permits"
        );
        assert_eq!(t.take("permit_sync").await.len(), 1);
        t.cleanup().await;
    }

    #[test]
    fn the_handover_dm_says_what_was_done() {
        let g = Id::new(5);
        let p = ["Admins".to_string(), "Admins".to_string()];
        let other = ["Mods".to_string()];
        let h = Handover {
            eo: &[11],
            ta: &[12, 13],
            paused: &p,
            other: &other,
            qw: &[14],
        };
        let t = handover_text("S", &h, g).unwrap();
        assert!(t.contains("Extra Owner(s) <@11>") && !t.contains("Ban Sync"));
        assert!(t.contains("Trusted Admin(s) <@12>, <@13>") && t.contains("permit(s) Admins ("));
        assert!(t.contains("one by one") && t.contains("stay on: Mods"));
        assert!(t.contains("removed <@14> from the quarantine whitelist"));
        let h = Handover {
            eo: &[],
            ta: &[],
            paused: &[],
            other: &[],
            qw: &[],
        };
        assert!(handover_text("S", &h, g).is_none(), "nothing to say");
    }

    /// D47: an owner change while the bot was out of the server is caught when it is added back.
    #[tokio::test]
    async fn an_owner_change_while_the_bot_was_out_is_caught_on_return() {
        let Some(t) = T::new(204).await else { return };
        let (o, new) = (t.owner(), t.base + 8);
        t.server_config(0, o, json!({})).await;
        t.exec("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, $1 + 1)")
            .await;
        t.set(|m| {
            m.owners.insert(t.g(0).get(), new);
        });
        on_owner(&t.app, t.g(0), Id::new(new)).await.unwrap();
        assert_eq!(
            t.count("SELECT count(*) FROM extra_owners WHERE guild_id = $1")
                .await,
            0
        );
        assert_eq!(
            t.count(
                "SELECT count(*) FROM owner_handovers WHERE guild_id = $1 AND old_owner = $1 + 9"
            )
            .await,
            1,
            "the kept guild_owners row named the previous owner"
        );
        t.cleanup().await;
    }
}

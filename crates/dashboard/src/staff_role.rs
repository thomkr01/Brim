//! The optional common Staff role (owner decisions 2026-11-23 and 2026-11-25): one switch, off by
//! default, on the setup wizard and the Permits page. On: Brim makes or takes a role, gives it to
//! every staff member by user id (`permit_sync`, level `staff`) and lets it into the Staff
//! channels next to the permit roles. It gives no rights. The row in `level_roles` is the only
//! state: it exists while the feature is on.

use crate::{
    access::Level,
    api::{log_by, require, Res},
    auth::Sess,
    store, ApiError, AppState,
};
use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use brim_core::permits::{self, STAFF};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

/// A new Staff role (decision 7): the Moderators blue, no permissions, not its own member list
/// group, not mentionable by members.
const COLOUR: u32 = 0x3498DB;

/// The alert lists that get the Staff role when they are empty (decision 4), as config paths.
const WARNED: [(&str, &str, &str); 2] = [
    ("panic", "warned_roles", "Panic Mode"),
    ("joinraid", "warned_roles", "Join Raid and Voice Raid"),
];

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/guilds/{guild}/staff-role", get(state).post(set))
}

/// The switch's state and what turning it off would change, read live.
/// With `?pick=<role>`: also how many members hold that role now and are not staff, as the next
/// sync takes it from them (`takes`, `more` when there are more members than were read).
async fn state(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(q): Query<Pick>,
) -> Res {
    let level = require(&st, &s, guild, Level::Admin, false).await?;
    if let Some(role) = q.pick.filter(|_| level >= Level::Owner) {
        let (takes, more) = takes(&st, guild, &role).await?;
        return Ok(Json(json!({"takes": takes, "more": more})));
    }
    let row = permits::staff_role(&st.db, guild).await?;
    let (_, config) = store::current(&st.db, guild).await?;
    let (role, made, alive, members, warned) = match row {
        None => (None, false, false, 0, vec![]),
        Some((r, made)) => {
            let (roles, _) = crate::ops::roles_and_bot_pos(&st, guild).await?;
            let alive = roles.iter().any(|x| x.id == r.to_string());
            (
                Some(r.to_string()),
                made,
                alive,
                holders(&st, guild).await?,
                warned_with(&config, r),
            )
        }
    };
    Ok(Json(json!({
        "on": role.is_some(), "role": role, "made": made, "alive": alive,
        "members": members, "warned": warned, "can_change": level >= Level::Owner,
    })))
}

#[derive(Deserialize)]
struct Pick {
    pick: Option<String>,
}

/// Members (not bots) that hold `role` and are not staff: a sync takes it from them.
async fn takes(st: &AppState, guild: i64, role: &str) -> Result<(u64, bool), ApiError> {
    let owner: Option<i64> =
        sqlx::query_scalar("select owner_id from guild_owners where guild_id = $1")
            .bind(guild)
            .fetch_optional(&st.db)
            .await?;
    let staff: std::collections::HashSet<i64> =
        permits::staff_users(&st.db, guild, owner.unwrap_or(0))
            .await?
            .into_iter()
            .collect();
    let mut n = 0;
    let more = crate::ops::member_pages(st, guild, |m| {
        let holds = m["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r.as_str() == Some(role));
        let user = m["user"]["id"].as_str().and_then(|u| u.parse::<i64>().ok());
        if holds && m["user"]["bot"] != true && !user.is_some_and(|u| staff.contains(&u)) {
            n += 1;
        }
    })
    .await?;
    Ok((n, more))
}

/// How many people Brim gives the Staff role (the role's holders after a sync).
async fn holders(st: &AppState, guild: i64) -> Result<i64, ApiError> {
    let owner: Option<i64> =
        sqlx::query_scalar("select owner_id from guild_owners where guild_id = $1")
            .bind(guild)
            .fetch_optional(&st.db)
            .await?;
    let Some((role, _)) = permits::staff_role(&st.db, guild).await? else {
        return Ok(0);
    };
    let mut users: Vec<i64> = permits::level_members(&st.db, guild, owner.unwrap_or(0))
        .await?
        .into_iter()
        .filter(|(u, r)| *r == role && *u != 0)
        .map(|(u, _)| u)
        .collect();
    users.dedup();
    Ok(users.len() as i64)
}

/// The alert lists (by name) that hold `role`.
fn warned_with(config: &Value, role: i64) -> Vec<&'static str> {
    let id = role.to_string();
    WARNED
        .iter()
        .filter(|(sec, key, _)| {
            config[sec][key]
                .as_array()
                .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(&id)))
        })
        .map(|(.., name)| *name)
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    on: bool,
    /// A role of the server to use; left out, Brim makes a new "Staff" role.
    #[serde(default)]
    role: Option<String>,
}

/// Turns the Staff role on or off. Only the Owner and Extra Owners, with a fresh 2FA (decision 8).
async fn set(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Body>,
) -> Res {
    require(&st, &s, guild, Level::Owner, true).await?;
    match b.on {
        true => on(&st, &s, guild, b.role.as_deref()).await,
        false => off(&st, &s, guild).await,
    }
}

/// Why `role` cannot be the Staff role, or None. Brim would give it to every staff member, so a
/// role with a dangerous permission, one Discord or a bot hands out, @everyone, a role at or above
/// Brim's top role and a role another setting uses are all refused.
async fn unfit(
    st: &AppState,
    guild: i64,
    role: &str,
    config: &Value,
) -> Result<Option<String>, ApiError> {
    let (roles, top) = crate::ops::roles_and_bot_pos(st, guild).await?;
    let Some(r) = roles.iter().find(|r| r.id == role) else {
        return Ok(Some("that role does not exist any more".into()));
    };
    if let Some(why) = r.unfit(guild, top) {
        return Ok(Some(why.into()));
    }
    // Off takes every overwrite of the role away, so it may hold only the ones Brim adds.
    let chans = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    let opens = chans.as_array().into_iter().flatten().any(|c| {
        let ows = c["permission_overwrites"].as_array().into_iter().flatten();
        ows.clone().any(|o| o["id"].as_str() == Some(role))
    });
    if opens {
        return Ok(Some(
            "it has channel permissions of its own, which turning the Staff role off would take away. Pick a role without any, or let Brim make a new role".into(),
        ));
    }
    // The Staff role gives no rights: it may hold only what @everyone has already.
    let everyone = roles
        .iter()
        .find(|e| e.id == guild.to_string())
        .map_or(0, |e| e.perms());
    if r.perms() & !everyone != 0 {
        return Ok(Some(
            "it has permissions of its own, and Brim would give them to every staff member. Pick a role without any, or let Brim make a new role".into(),
        ));
    }
    let unpermittable = st.discord.unpermittable_roles(guild).await.map_err(|e| {
        tracing::warn!(?e, guild, "staff role check failed");
        ApiError::BadRequest(
            "could not check the role with Discord, so nothing changed. Try again.".into(),
        )
    })?;
    if let Some(why) = unpermittable.get(role) {
        return Ok(Some((*why).into()));
    }
    // The alert and ping lists may name it already: that is what the role is for.
    let mut rest = config.clone();
    for (sec, key, _) in WARNED {
        rest[sec][key] = json!([]);
    }
    rest["tickets"]["ping_roles"] = json!([]);
    if let Some(page) = crate::channels::used_by(&rest, role) {
        return Ok(Some(format!("{page} uses it already")));
    }
    Ok(None)
}

async fn on(st: &AppState, s: &Sess, guild: i64, role: Option<&str>) -> Res {
    if permits::staff_role(&st.db, guild).await?.is_some() {
        return Err(ApiError::BadRequest("the Staff role is on already".into()));
    }
    let (_, config) = store::current(&st.db, guild).await?;
    let (role, made) = match role {
        Some(r) => {
            if let Some(why) = unfit(st, guild, r, &config).await? {
                return Err(ApiError::BadRequest(format!(
                    "<@&{r}> cannot be the Staff role: {why}."
                )));
            }
            (r.to_string(), false)
        }
        None => {
            let body = json!({"name": "Staff", "color": COLOUR, "hoist": false, "permissions": "0", "mentionable": false});
            (
                crate::sections::create(st, guild, None, "roles", body).await?,
                true,
            )
        }
    };
    let id: i64 = role
        .parse()
        .map_err(|_| ApiError::BadRequest("bad role id".into()))?;
    let saved = save_on(st, s, guild, id, made).await;
    if saved.is_err() && made {
        // Nothing points to the new role: do not leave it behind.
        let at = format!("/guilds/{guild}/roles/{role}");
        if let Err(e) = st.discord.call(Method::DELETE, &at, None).await {
            tracing::warn!(?e, guild, "deleting an unused Staff role failed");
        }
    }
    let version = saved?;
    // Best effort: the Overview row "Staff cannot use their channels" offers the same change.
    let channels = crate::channels::queue_staff_role(st, guild, s.user_id)
        .await
        .inspect_err(|e| {
            tracing::warn!(?e, guild, "queueing the Staff role channel changes failed")
        })
        .unwrap_or(0);
    Ok(Json(
        json!({"ok": true, "version": version, "role": role, "channels": channels}),
    ))
}

/// Writes the row and presets the empty alert lists, as one config version (it queues
/// `permit_sync`, since the permits copy's levels change).
async fn save_on(
    st: &AppState,
    s: &Sess,
    guild: i64,
    role: i64,
    made: bool,
) -> Result<i32, ApiError> {
    let mut tx = st.db.begin().await?;
    let (cur, mut config) = store::locked(&mut tx, guild).await?;
    sqlx::query("insert into level_roles (guild_id, level, role_id, made) values ($1, $2, $3, $4)")
        .bind(guild)
        .bind(STAFF)
        .bind(role)
        .bind(made)
        .execute(&mut *tx)
        .await
        .map_err(|_| {
            ApiError::BadRequest(
                "the Staff role is on already, or that role is a Trusted Admin or Owner role"
                    .into(),
            )
        })?;
    let mut preset = vec![];
    for (sec, key, name) in WARNED {
        if config[sec][key].as_array().is_none_or(Vec::is_empty) {
            config[sec][key] = json!([role.to_string()]);
            preset.push(name);
        }
    }
    let version = store::commit(&mut tx, guild, cur, &config, s.user_id).await?;
    tx.commit().await?;
    let what = match preset.is_empty() {
        true => format!("turned the Staff role <@&{role}> on"),
        false => format!(
            "turned the Staff role <@&{role}> on and added it to the alert pings of {}",
            preset.join(", ")
        ),
    };
    log_by(st, s, guild, &config, Some(version), &what).await;
    Ok(version)
}

/// Turns the Staff role off, safe at every step: the permit roles get their Staff channels first
/// (they always keep them; this only repairs), then the Staff role's overwrites go, then the row,
/// then the role (Brim's own) or its holders (a role the owner picked). A failure before the row
/// goes leaves the feature on, so the owner can try again; nothing closes a channel to staff.
async fn off(st: &AppState, s: &Sess, guild: i64) -> Res {
    let Some((role, made)) = permits::staff_role(&st.db, guild).await? else {
        return Ok(Json(json!({"ok": true})));
    };
    let (_, config) = store::current(&st.db, guild).await?;
    let live = crate::ops::live(st, guild, &config).await?;
    let others: Vec<u64> = permits::all_staff_roles(&st.db, guild)
        .await?
        .into_iter()
        .filter(|r| *r != role)
        .map(|r| r as u64)
        .collect();
    let permit_staff: Vec<u64> = permits::staff_roles(&st.db, guild)
        .await?
        .into_iter()
        .map(|r| r as u64)
        .collect();
    let keep = crate::channel_access::live_staff(&permit_staff, &live.image.roles);
    // With no permit role left to take its place, staff would lose every channel it opens.
    let opens = live.image.channels.iter().any(|c| {
        c.overwrites
            .iter()
            .any(|o| o.is_role && o.target == role as u64 && o.allow & (1 << 10) != 0)
    });
    if opens && keep.is_empty() {
        return Err(ApiError::BadRequest(
            "the Staff role is the only role that lets staff into their channels. Give the Admins or Moderators permit a role first (Permits page), then turn it off".into(),
        ));
    }
    // Every overwrite of the role goes: take a backup first, as the other permission fixes.
    let backup = crate::ops::backup(st, guild, &live).await?;
    // No Channel Setup push may let the role in again between the strip and the row's delete.
    let drafts = crate::channels::hold_pushes(st, guild).await?;
    let r = off_locked(st, s, guild, (role, made), &config, (&others, &keep)).await;
    crate::channels::release_pushes(drafts).await;
    let (version, stripped, cleared) = r?;
    log_by(
        st,
        s,
        guild,
        &config,
        Some(version),
        &format!("turned the Staff role <@&{role}> off ({stripped} channels changed)"),
    )
    .await;
    Ok(Json(
        json!({"ok": true, "version": version, "warned": cleared, "backup": backup}),
    ))
}

/// [`off`] while Channel Setup pushes wait: (version, channels changed, alert lists cleared).
async fn off_locked(
    st: &AppState,
    s: &Sess,
    guild: i64,
    (role, made): (i64, bool),
    config: &Value,
    (others, keep): (&[u64], &[u64]),
) -> Result<(i32, usize, Vec<&'static str>), ApiError> {
    crate::channel_access::staff_access(st, guild, config, others).await?;
    let stripped = crate::channel_access::strip_role(st, guild, role as u64, keep).await?;
    let mut tx = st.db.begin().await?;
    let (cur, mut config) = store::locked(&mut tx, guild).await?;
    let gone =
        sqlx::query("delete from level_roles where guild_id = $1 and level = $2 and role_id = $3")
            .bind(guild)
            .bind(STAFF)
            .bind(role)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if gone == 0 {
        // Turned off, or made again, by someone else meanwhile.
        return Err(ApiError::Conflict(cur));
    }
    permits::was_staff_role(&mut *tx, guild, role).await?;
    let id = role.to_string();
    let mut cleared = vec![];
    for (sec, key, name) in WARNED {
        if let Some(list) = config[sec][key].as_array_mut() {
            let n = list.len();
            list.retain(|v| v.as_str() != Some(&id));
            if list.len() < n {
                cleared.push(name);
            }
        }
    }
    let now = chrono::Utc::now().timestamp();
    if !made {
        brim_core::jobs::enqueue(&mut *tx, guild, "staff_role_off", json!({"role": id}), now)
            .await?;
    }
    let version = store::commit(&mut tx, guild, cur, &config, s.user_id).await?;
    tx.commit().await?;
    if made {
        let at = format!("/guilds/{guild}/roles/{role}");
        if let Err(e) = st.discord.call(Method::DELETE, &at, None).await {
            // It has no channel access any more; take it from its holders instead.
            tracing::warn!(?e, guild, "deleting the Staff role failed");
            brim_core::jobs::enqueue(&st.db, guild, "staff_role_off", json!({"role": id}), now)
                .await?;
        }
    }
    Ok((version, stripped, cleared))
}

/// The Overview row while the Staff role is on and its role is gone in Discord. Off: no row.
pub(crate) async fn overview_check(
    st: &AppState,
    guild: i64,
    roles: &[brim_antinuke::RoleImg],
) -> Result<Option<brim_antinuke::security::Check>, ApiError> {
    let Some((role, _)) = permits::staff_role(&st.db, guild).await? else {
        return Ok(None);
    };
    if roles.iter().any(|r| r.id == role as u64) {
        return Ok(None);
    }
    Ok(Some(brim_antinuke::security::Check {
        id: "staff_role_gone".into(),
        severity: brim_antinuke::security::Severity::Warn,
        title: "The Staff role is gone",
        detail: "The Staff role was deleted in Discord. Until it is back, Brim pings the staff permit roles. The fix makes the role again and gives it to all staff.".into(),
        fix: Some(brim_antinuke::security::Fix {
            label: "Make it again",
            role_id: None,
            channel_id: None,
            setting: Some(FIX),
            move_under_bot: false,
        }),
        steps: vec![],
    }))
}

/// "Staff", "Trusted Admin" or "Owner" for a `level_roles` level.
pub(crate) fn level_name(level: &str) -> &'static str {
    match level {
        permits::STAFF => "Staff",
        "trusted_admin" => "Trusted Admin",
        _ => "Owner",
    }
}

/// A setting saved before the check that has Brim give a level role to members (the quarantine
/// role, main roles, verification roles). `permit_sync` takes it back from everyone who is not
/// staff, so the setting and the sync fight. No Fix: the owner picks another role there.
pub(crate) async fn given_check(
    st: &AppState,
    guild: i64,
    config: &Value,
) -> Result<Option<brim_antinuke::security::Check>, ApiError> {
    let levels = permits::level_roles(&st.db, guild).await?;
    let mut hits: Vec<String> = vec![];
    for (section, title) in [
        ("antinuke", "Anti Nuke quarantine role"),
        ("lockdown", "Lockdown main roles"),
        ("verification", "Verification roles"),
    ] {
        let given = crate::api::given_roles(section, &config[section]);
        for (level, role) in &levels {
            if given.contains(&role.to_string()) {
                hits.push(format!("{title} (the {} role)", level_name(level)));
            }
        }
    }
    if hits.is_empty() {
        return Ok(None);
    }
    Ok(Some(brim_antinuke::security::Check {
        id: "level_role_given".into(),
        severity: brim_antinuke::security::Severity::Warn,
        title: "A staff-only role is given to members",
        detail: format!(
            "{}. Brim gives this role only to staff and takes it from everyone else, so these settings do not work. Pick another role there.",
            hits.join(", ")
        ),
        fix: None,
        steps: vec![],
    }))
}

/// Fix setting of [`overview_check`].
pub(crate) const FIX: &str = "staff_role";

/// The Fix: makes a new Staff role (decision 7) and points the row and the alert lists to it.
/// Owner and Extra Owners only, as every Staff role change.
pub(crate) async fn make_again(st: &AppState, s: &Sess, guild: i64, level: Level) -> Res {
    if level < Level::Owner {
        return Err(ApiError::Forbidden(
            "only the Owner or an Extra Owner can change the Staff role",
        ));
    }
    let Some((old, _)) = permits::staff_role(&st.db, guild).await? else {
        return Ok(Json(json!({"ok": true})));
    };
    let (roles, _) = crate::ops::roles_and_bot_pos(st, guild).await?;
    if roles.iter().any(|r| r.id == old.to_string()) {
        return Ok(Json(json!({"ok": true})));
    }
    let body = json!({"name": "Staff", "color": COLOUR, "hoist": false, "permissions": "0", "mentionable": false});
    let new: i64 = crate::sections::create(st, guild, None, "roles", body)
        .await?
        .parse()
        .map_err(|_| ApiError::BadRequest("discord returned a bad role id".into()))?;
    let mut tx = st.db.begin().await?;
    let (cur, mut config) = store::locked(&mut tx, guild).await?;
    let moved = sqlx::query("update level_roles set role_id = $3, made = true where guild_id = $1 and level = 'staff' and role_id = $2")
        .bind(guild)
        .bind(old)
        .bind(new)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if moved > 0 {
        permits::was_staff_role(&mut *tx, guild, old).await?;
    }
    if moved == 0 {
        // Turned off, or relinked by a restore, meanwhile: the new role is not needed.
        drop(tx);
        let at = format!("/guilds/{guild}/roles/{new}");
        if let Err(e) = st.discord.call(Method::DELETE, &at, None).await {
            tracing::warn!(?e, guild, "deleting an unused Staff role failed");
        }
        return Err(ApiError::Conflict(cur));
    }
    for (sec, key, _) in WARNED {
        for v in config[sec][key].as_array_mut().into_iter().flatten() {
            if v.as_str() == Some(&old.to_string()) {
                *v = json!(new.to_string());
            }
        }
    }
    let version = store::commit(&mut tx, guild, cur, &config, s.user_id).await?;
    tx.commit().await?;
    log_by(
        st,
        s,
        guild,
        &config,
        Some(version),
        &format!("made the Staff role again as <@&{new}>"),
    )
    .await;
    let channels = crate::channels::queue_staff_role(st, guild, s.user_id)
        .await
        .inspect_err(|e| {
            tracing::warn!(?e, guild, "queueing the Staff role channel changes failed")
        })
        .unwrap_or(0);
    Ok(Json(
        json!({"ok": true, "version": version, "channels": channels}),
    ))
}

#[cfg(test)]
mod tests {
    use crate::{auth::Sess, discord::Discord, AppState};
    use axum::{body::Body, http::Request, routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    /// No setting may have Brim give a level role to members (the quarantine role, main roles,
    /// verification roles): a new pick is refused, and one saved before shows on Overview.
    #[tokio::test]
    async fn a_level_role_is_never_given_to_members() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_710_000_000 + std::process::id() as i64;
        sqlx::query("insert into level_roles (guild_id, level, role_id) values ($1, 'staff', 1000), ($1, 'owner', 1001)")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        let st = AppState {
            db: db.clone(),
            discord: Discord {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                client_id: "7".into(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                // No Discord call may happen before the refusal.
                api: "http://127.0.0.1:9".into(),
                memo: Default::default(),
                limits: Default::default(),
            },
            secure: false,
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        };
        let pick = |v: Value| (v, json!({}));
        let (new, old) = pick(json!({"roles": ["5", "1000"]}));
        let e = crate::api::check_held_roles(&st, g, "verification", &new, &old).await;
        assert!(format!("{e:?}").contains("the Staff role"), "{e:?}");
        // The bot reads a number as an id too.
        let (new, old) = pick(json!({"main_roles": [1000]}));
        let e = crate::api::check_held_roles(&st, g, "lockdown", &new, &old).await;
        assert!(format!("{e:?}").contains("the Staff role"), "{e:?}");
        let (new, old) = pick(json!({"quarantine_role": "1001"}));
        let e = crate::api::check_held_roles(&st, g, "antinuke", &new, &old).await;
        assert!(format!("{e:?}").contains("the Owner role"), "{e:?}");
        // A value saved before is left alone on save, and Overview names it.
        let stored = json!({"roles": ["1000"]});
        crate::api::check_held_roles(&st, g, "verification", &stored, &stored)
            .await
            .unwrap();
        let row = super::given_check(&st, g, &json!({"verification": stored}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.id, "level_role_given");
        assert!(
            row.detail.contains("Verification roles (the Staff role)"),
            "{}",
            row.detail
        );
        assert!(
            super::given_check(&st, g, &json!({"verification": {"roles": ["5"]}}))
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query("delete from level_roles where guild_id = $1")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
    }

    /// Only the Owner with a fresh 2FA turns it on; a dangerous role is refused; on writes the row,
    /// presets the empty alert lists and queues the sync; a rollback keeps it; off takes the role's
    /// overwrites first, then the row, then deletes the role Brim made. Discord is a local mock.
    #[tokio::test]
    async fn on_and_off_in_a_safe_order() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_700_000_000 + std::process::id() as i64;
        let owner = 7_820_000_000 + std::process::id() as i64;
        let calls: Arc<Mutex<Vec<String>>> = Default::default();
        let made: Arc<Mutex<bool>> = Default::default();
        let (c1, c2, c3, c4, m1, m2) = (
            calls.clone(),
            calls.clone(),
            calls.clone(),
            calls.clone(),
            made.clone(),
            made.clone(),
        );
        let ta = owner + 1;
        let mock = Router::new()
            .route("/guilds/{g}", get(move || async move { Json(json!({"owner_id": owner.to_string(), "mfa_level": 1})) }))
            .route(
                "/guilds/{g}/roles",
                get(move || async move {
                    let mut r = vec![json!({"id": g.to_string(), "name": "@everyone", "permissions": "0", "position": 0}),
                        json!({"id": "5", "name": "Mods", "permissions": "0", "position": 1}),
                        json!({"id": "6", "name": "Boss", "permissions": "8", "position": 2}),
                        json!({"id": "9", "name": "Brim", "permissions": "8", "position": 5, "managed": true})];
                    if *m1.lock().unwrap() {
                        r.push(json!({"id": "1000", "name": "Staff", "permissions": "0", "position": 1}));
                    }
                    Json(json!(r))
                })
                .post(move |Json(b): Json<Value>| async move {
                    assert_eq!((b["permissions"].as_str(), b["hoist"].as_bool(), b["color"].as_u64()), (Some("0"), Some(false), Some(0x3498DB)));
                    *m2.lock().unwrap() = true;
                    c1.lock().unwrap().push("create role".into());
                    Json(json!({"id": "1000"}))
                }),
            )
            .route("/guilds/{g}/roles/{r}", axum::routing::delete(move || async move {
                c2.lock().unwrap().push("delete role".into());
                axum::http::StatusCode::NO_CONTENT
            }))
            .route("/guilds/{g}/onboarding", get(|| async { axum::http::StatusCode::NOT_FOUND }))
            .route(
                "/guilds/{g}/channels",
                get(|| async {
                    Json(json!([{"id": "300", "name": "mod-notes", "type": 0, "position": 0,
                        "permission_overwrites": [{"id": "1000", "type": 0, "allow": "1024", "deny": "0"}]}]))
                }),
            )
            .route("/users/@me", get(|| async { Json(json!({"id": "7"})) }))
            .route("/guilds/{g}/members", get(move || async move {
                Json(json!([{"user": {"id": "11"}, "roles": ["5"]}, {"user": {"id": ta.to_string()}, "roles": ["5"]},
                    {"user": {"id": "12", "bot": true}, "roles": ["5"]}, {"user": {"id": "13"}, "roles": []}]))
            }))
            .route("/guilds/{g}/members/{u}", get(|| async { Json(json!({"roles": ["9"]})) }))
            .route(
                "/channels/{c}/permissions/{t}",
                axum::routing::delete(move |axum::extract::Path((c, t)): axum::extract::Path<(String, String)>| async move {
                    c3.lock().unwrap().push(format!("strip {c} {t}"));
                    axum::http::StatusCode::NO_CONTENT
                })
                .put(move |axum::extract::Path((c, t)): axum::extract::Path<(String, String)>| async move {
                    c4.lock().unwrap().push(format!("let in {c} {t}"));
                    axum::http::StatusCode::NO_CONTENT
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, mock).await });
        let st = AppState {
            db: db.clone(),
            discord: Discord {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                client_id: "7".into(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                api,
                memo: Default::default(),
                limits: Default::default(),
            },
            secure: false,
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        };
        let send = |m: &str, user: i64, stepped_up: bool, body: Option<Value>| {
            let (m, q) = m.split_once(' ').unwrap_or((m, ""));
            let mut req = Request::builder()
                .method(m)
                .uri(format!("/api/guilds/{g}/staff-role{q}"))
                .header("content-type", "application/json")
                .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
                .unwrap();
            req.extensions_mut().insert(Sess {
                id: String::new(),
                user_id: user,
                username: "t".into(),
                access_token: String::new(),
                step_up_guild: Some(g),
                stepped_up,
                csrf: String::new(),
            });
            let app = crate::router(st.clone(), "/nonexistent");
            async move {
                let res = app.oneshot(req).await.unwrap();
                let code = res.status().as_u16();
                let b = axum::body::to_bytes(res.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (
                    code,
                    serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
                )
            }
        };
        sqlx::query("insert into trusted_admins (guild_id, user_id) values ($1, $2)")
            .bind(g)
            .bind(ta)
            .execute(&db)
            .await
            .unwrap();
        // A picked role: members who hold it and are not staff (not the Trusted Admin, not a bot).
        let (code, r) = send("GET ?pick=5", owner, false, None).await;
        assert_eq!(
            (code, r["takes"].as_u64(), r["more"].as_bool()),
            (200, Some(1), Some(false)),
            "{r}"
        );
        let on = Some(json!({"on": true}));
        let (code, _) = send("POST", ta, true, on.clone()).await;
        assert_eq!(code, 403, "a Trusted Admin cannot change it");
        let (code, r) = send("POST", owner, false, on.clone()).await;
        assert_eq!((code, r["error"].as_str()), (403, Some("step_up_required")));
        let (code, r) = send("POST", owner, true, Some(json!({"on": true, "role": "6"}))).await;
        assert_eq!(code, 400, "a role with Administrator is refused: {r}");
        let (code, r) = send("POST", owner, true, on.clone()).await;
        assert_eq!(code, 200, "{r}");
        let row = brim_core::permits::staff_role(&db, g).await.unwrap();
        let (_, config) = crate::store::current(&db, g).await.unwrap();
        let sync: i64 = sqlx::query_scalar(
            "select count(*) from scheduled_jobs where guild_id = $1 and kind = 'permit_sync'",
        )
        .bind(g)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row, Some((1000, true)));
        assert_eq!(config["panic"]["warned_roles"], json!(["1000"]));
        assert_eq!(config["permits"]["levels"]["staff"], "1000");
        assert_eq!(sync, 1);
        let (code, r) = send("GET", owner, false, None).await;
        assert_eq!(
            (code, r["on"].as_bool(), r["made"].as_bool()),
            (200, Some(true), Some(true)),
            "{r}"
        );
        // A rollback (or Auto Setup) writes the other level roles and keeps the Staff role.
        let mut c = db.acquire().await.unwrap();
        crate::store::write_levels(&mut c, g, &json!({}))
            .await
            .unwrap();
        drop(c);
        assert_eq!(
            brim_core::permits::staff_role(&db, g).await.unwrap(),
            Some((1000, true))
        );
        // No permit role could take its place: off is refused and nothing changes.
        let (code, r) = send("POST", owner, true, Some(json!({"on": false}))).await;
        assert_eq!(code, 400, "{r}");
        assert!(brim_core::permits::staff_role(&db, g)
            .await
            .unwrap()
            .is_some());
        for q in [
            "insert into permits (guild_id, name, perms) values ($1, 'Moderators', '{ban}')",
            "insert into permit_roles (permit_id, guild_id, role_id) select id, $1, 5 from permits where guild_id = $1 and name = 'Moderators'",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(q)).bind(g).execute(&db).await.unwrap();
        }
        let (code, r) = send("POST", owner, true, Some(json!({"on": false}))).await;
        assert_eq!(code, 200, "{r}");
        let (_, config) = crate::store::current(&db, g).await.unwrap();
        let row = brim_core::permits::staff_role(&db, g).await.unwrap();
        let former = brim_core::permits::former_staff_roles(&db, g)
            .await
            .unwrap();
        for t in [
            "guild_config",
            "level_roles",
            "trusted_admins",
            "scheduled_jobs",
            "config_audit",
            "channel_drafts",
            "permit_roles",
            "permits",
            "former_staff_roles",
        ] {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await;
        }
        assert_eq!(row, None);
        assert_eq!(
            former.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            vec![1000],
            "a restore gives it no access back"
        );
        assert_eq!(config["panic"]["warned_roles"], json!([]));
        let calls = calls.lock().unwrap().clone();
        let let_in = calls.iter().position(|c| c == "let in 300 5");
        let strip = calls.iter().position(|c| c == "strip 300 1000");
        let delete = calls.iter().position(|c| c == "delete role");
        assert!(
            let_in.is_some()
                && let_in < strip
                && strip.is_some()
                && delete.is_some()
                && strip < delete,
            "{calls:?}"
        );
    }
}

//! Permits belong to user ids (owner decision 2026-11-15). Brim gives a permit's roles to the members
//! listed in it and takes them from everyone else, and forgets who left the server.
use anyhow::Result;
use brim_core::permits::{self, Level};
use brim_logging::{overlay::security_log, Log, LogKind};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock, Mutex},
};
use twilight_http::request::AuditLogReason;
use twilight_model::{
    gateway::payload::incoming::MemberUpdate,
    guild::Member,
    id::{
        marker::{GuildMarker, RoleMarker, UserMarker},
        Id,
    },
};

use crate::{app::App, events::send_log};

/// Audit log reason of every permit role Brim gives.
pub(crate) const REASON: &str = "Permit: Brim gives this role to the permit's members";

static RUNS: LazyLock<Mutex<HashMap<u64, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

fn run_lock(gid: Id<GuildMarker>) -> Arc<tokio::sync::Mutex<()>> {
    let mut all = RUNS.lock().unwrap();
    all.retain(|_, l| Arc::strong_count(l) > 1);
    all.entry(gid.get()).or_default().clone()
}

/// GuildCreate and job `permit_sync` (the dashboard queues it after a permits save). Pages every
/// member first and changes nothing when the paging fails: a cut-short list would drop listed
/// people as gone. Moves role holders to user ids once, forgets listed people who are gone, gives
/// listed members their permit's roles and takes permit roles from everyone else. Nobody is punished.
pub async fn sync(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    // One run per guild: GuildCreate and a queued job can meet.
    let _one = run_lock(gid).lock_owned().await;
    let g = gid.get() as i64;
    let members = all_members(app, gid).await?;
    let info = app.guild_info(gid).await?;
    let mut changed = false;
    if !permits::members_migrated(&app.pool, g).await? {
        // A quarantined holder's roles are saved, not on them: their permit moves too (owner decision).
        let saved: HashMap<i64, Vec<i64>> = sqlx::query_as(
            "SELECT user_id, saved_roles FROM quarantines WHERE guild_id = $1 AND NOT on_join",
        )
        .bind(g)
        .fetch_all(&app.pool)
        .await?
        .into_iter()
        .collect();
        let holders: Vec<(i64, Vec<i64>)> = members
            .iter()
            .map(|m| {
                let u = m.user.id.get() as i64;
                let mut roles = ids(&m.roles);
                roles.extend(saved.get(&u).into_iter().flatten());
                (u, roles)
            })
            .collect();
        let (mut tx, version, mut doc) = crate::settings::locked(app, gid).await?;
        let moved = permits::migrate_members(&mut tx, g, &holders).await?;
        if moved.is_empty() {
            tx.commit().await?;
        } else {
            crate::settings::append(app, tx, gid, version, &mut doc, app.me.get()).await?;
            let list: Vec<String> = moved.iter().map(|(u, p)| format!("<@{u}>: {p}")).collect();
            let log = security_log(
                "Permits",
                "log.anti_nuke.permits_moved",
                &[
                    ("alert", "**Permits now belong to members, not roles**"),
                    ("listed", &capped(&list)),
                ],
            );
            warn_log(app, gid, log).await;
            changed = true;
        }
    }
    let here: HashSet<i64> = members.iter().map(|m| m.user.id.get() as i64).collect();
    let listed: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM permit_members WHERE guild_id = $1
         UNION SELECT user_id FROM extra_owners WHERE guild_id = $1
         UNION SELECT user_id FROM trusted_admins WHERE guild_id = $1",
    )
    .bind(g)
    .fetch_all(&app.pool)
    .await?;
    let owner = info.owner.get() as i64;
    let mut gone = vec![];
    for u in listed
        .into_iter()
        .filter(|u| !here.contains(u) && *u != owner)
    {
        // Someone who joined during the paging is not on it: only Discord's "not a member" forgets.
        let Some(id) = Id::new_checked(u as u64) else {
            continue;
        };
        match crate::http_err::member(&app.http, gid, id).await {
            Ok(None) => gone.push(u),
            Ok(Some(_)) => {}
            Err(e) => tracing::warn!(?e, user = u, "checking a listed member failed, kept"),
        }
    }
    if !gone.is_empty() {
        changed |= forget(app, gid, &gone).await?;
    }
    changed |= roles(app, gid, &info, &members).await?;
    if changed {
        app.forget_guild_levels(gid.get());
    }
    Ok(())
}

/// Gives listed members the roles of their permit and takes permit roles from everyone else.
/// Skips roles Brim cannot or must not change: managed ones, deleted ones, those at or above
/// its top role, and roles Discord hands out (`unpermittable_roles`). A quarantine holds its
/// member's roles, so a quarantined member gets nothing.
async fn roles(
    app: &App,
    gid: Id<GuildMarker>,
    info: &crate::hierarchy::GuildInfo,
    members: &[Member],
) -> Result<bool> {
    let g = gid.get() as i64;
    let listed = permits::members(&app.pool, g).await?;
    let mut own: HashMap<i64, Vec<i64>> = listed.into_iter().map(|(u, _, r)| (u, r)).collect();
    // Trusted Admins, Extra Owners and the owner get their level's role the same way.
    for (u, r) in permits::level_members(&app.pool, g, info.owner.get() as i64).await? {
        own.entry(u).or_default().push(r);
    }
    let permit_roles: HashSet<i64> = permits::permit_roles(&app.pool, g)
        .await?
        .into_iter()
        .collect();
    let held: HashSet<i64> =
        sqlx::query_scalar("SELECT user_id FROM quarantines WHERE guild_id = $1")
            .bind(g)
            .fetch_all(&app.pool)
            .await?
            .into_iter()
            .collect();
    let discord: HashSet<i64> =
        sqlx::query_scalar("SELECT role_id FROM unpermittable_roles WHERE guild_id = $1")
            .bind(g)
            .fetch_all(&app.pool)
            .await?
            .into_iter()
            .collect();
    let fits = |r: i64| {
        let id = Id::new(r as u64);
        !info.managed.contains(&id)
            && !discord.contains(&r)
            && info.roles.get(&id).is_some_and(|p| *p < info.bot_top)
    };
    let mut changed = false;
    let mut taken = vec![];
    'members: for m in members {
        let u = m.user.id.get() as i64;
        if held.contains(&u) {
            continue;
        }
        let has = ids(&m.roles);
        let mine: &[i64] = own.get(&u).map_or(&[], |r| r.as_slice());
        for &r in mine.iter().filter(|r| !has.contains(r) && fits(**r)) {
            // `held` is from before this run's REST calls: a quarantine since then holds too,
            // so the member is skipped as if held.
            if now_held(app, g, u).await? {
                continue 'members;
            }
            let give = app
                .http
                .add_guild_member_role(gid, m.user.id, Id::new(r as u64))
                .reason(REASON)
                .await;
            let given = give.is_ok();
            match give {
                Ok(_) => changed = true,
                Err(e) => tracing::warn!(?e, user = u, role = r, "giving a permit role failed"),
            }
            // A quarantine that began during the give: its saved roles miss this one, so take it.
            if given && now_held(app, g, u).await? {
                let back = app
                    .http
                    .remove_guild_member_role(gid, m.user.id, Id::new(r as u64))
                    .reason("Permit: the member was quarantined during the give")
                    .await;
                if let Err(e) = back {
                    tracing::warn!(?e, user = u, role = r, "taking back a permit role failed");
                }
                continue 'members;
            }
        }
        let extra: Vec<i64> = has
            .into_iter()
            .filter(|r| permit_roles.contains(r) && !mine.contains(r) && fits(*r))
            .collect();
        let mut gone = vec![];
        for r in extra {
            let take = app
                .http
                .remove_guild_member_role(gid, m.user.id, Id::new(r as u64))
                .reason("Permit: only the permit's members hold its roles")
                .await;
            match take {
                Ok(_) => gone.push(format!("<@&{r}>")),
                Err(e) => tracing::warn!(?e, user = u, role = r, "taking a permit role failed"),
            }
        }
        if !gone.is_empty() {
            taken.push(format!("<@{u}>: {}", gone.join(", ")));
        }
    }
    if !taken.is_empty() {
        let log = security_log(
            "Anti Nuke",
            "log.anti_nuke.permit_roles_taken",
            &[
                (
                    "alert",
                    "**Permit roles taken from members not listed in the permit**",
                ),
                ("taken", &capped(&taken)),
            ],
        );
        warn_log(app, gid, log).await;
    }
    Ok(changed || !taken.is_empty())
}

async fn now_held(app: &App, g: i64, u: i64) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM quarantines WHERE guild_id = $1 AND user_id = $2)",
    )
    .bind(g)
    .bind(u)
    .fetch_one(&app.pool)
    .await?)
}

/// MemberUpdate: a permit role the member is not listed for is taken, nobody punished. Catches a
/// give whose audit entry was missed; the audit guard logs, this only traces. A role of the
/// member's own permit is never taken, so this never undoes Brim's own gives.
pub async fn on_member_update(app: &App, m: &MemberUpdate) -> Result<()> {
    if m.roles.is_empty() {
        return Ok(());
    }
    let (g, u) = (m.guild_id.get() as i64, m.user.id.get() as i64);
    // Without the owner first (no lookup for most updates): it can only add roles to the list.
    let extra = permits::unentitled_roles(&app.pool, g, u, 0, &ids(&m.roles)).await?;
    if extra.is_empty() {
        return Ok(());
    }
    let info = app.guild_info(m.guild_id).await?;
    let owner = info.owner.get() as i64;
    let extra = permits::unentitled_roles(&app.pool, g, u, owner, &extra).await?;
    let extra: Vec<i64> = extra
        .into_iter()
        .filter(|r| {
            let id = Id::new(*r as u64);
            !info.managed.contains(&id) && info.roles.get(&id).is_some_and(|p| *p < info.bot_top)
        })
        .collect();
    // Only an attempted take uses the fight breaker's once-a-minute slot.
    if extra.is_empty() || crate::antinuke::permit_fight_stopped(m.guild_id.get(), m.user.id.get())
    {
        return Ok(());
    }
    for r in extra {
        let id = Id::new(r as u64);
        let take = app
            .http
            .remove_guild_member_role(m.guild_id, m.user.id, id)
            .reason("Permit: only the permit's members hold its roles")
            .await;
        match take {
            Ok(_) => tracing::info!(
                user = u,
                role = r,
                "took a permit role from a member not listed"
            ),
            Err(e) => tracing::warn!(?e, user = u, role = r, "taking a permit role failed"),
        }
    }
    Ok(())
}

/// How long [`left_level`] remembers a leaver's level: the kick or ban audit entry comes within it.
const LEFT_FOR_MS: i64 = 600_000;
/// (guild, user) -> the level a leaver held, and when they left (Unix milliseconds).
type Left = HashMap<(u64, u64), (Level, i64)>;
static LEFT: LazyLock<Mutex<Left>> = LazyLock::new(Default::default);

/// The level `user` held when they left in the last 10 minutes, for an audit entry made at
/// `entry_ms`: MemberRemove may forget a kicked or banned holder before anti-nuke reads the entry.
/// An entry more than 5 seconds after the leave did not cause it (a ban by id after a voluntary
/// leave): None. In memory: a restart forgets it.
pub(crate) fn left_level(
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    entry_ms: i64,
) -> Option<Level> {
    let all = LEFT.lock().unwrap();
    let (l, at) = all.get(&(gid.get(), user.get()))?;
    (crate::automod::now_ms() - at < LEFT_FOR_MS && entry_ms <= at + 5000).then(|| l.clone())
}

/// MemberAdd: a returning member is judged by their new level, not the one they left with.
pub(crate) fn forget_left(gid: Id<GuildMarker>, user: Id<UserMarker>) {
    LEFT.lock().unwrap().remove(&(gid.get(), user.get()));
}

/// MemberRemove (a kick or ban too): the user leaves every per-id trust list. Their level is kept
/// for [`left_level`] first.
pub async fn on_leave(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    bot: bool,
) -> Result<()> {
    let (g, u) = (gid.get() as i64, user.get() as i64);
    // Most leavers hold nothing: no config lock for them (a raid's mass kick).
    let listed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM permit_members WHERE guild_id = $1 AND user_id = $2)
             OR EXISTS (SELECT 1 FROM extra_owners WHERE guild_id = $1 AND user_id = $2)
             OR EXISTS (SELECT 1 FROM trusted_admins WHERE guild_id = $1 AND user_id = $2)",
    )
    .bind(g)
    .bind(u)
    .fetch_one(&app.pool)
    .await?;
    if !listed {
        return Ok(());
    }
    // A failed lookup must not keep the leaver listed: the record is best effort.
    let level = async {
        let owner = app.guild_info(gid).await?.owner.get() as i64;
        permits::resolve(&app.pool, g, owner, u, bot).await
    };
    match level.await {
        Ok(level) => {
            let now = crate::automod::now_ms();
            let mut all = LEFT.lock().unwrap();
            all.retain(|_, (_, at)| now - *at < LEFT_FOR_MS);
            all.insert((gid.get(), user.get()), (level, now));
        }
        Err(e) => tracing::warn!(?e, user = u, "keeping a leaver's level failed"),
    }
    if forget(app, gid, &[u]).await? {
        app.forget_level(gid.get(), user.get());
    }
    Ok(())
}

/// Takes `users` out of the permits, Extra Owners and Trusted Admins in one config version, and
/// logs it. Returns whether anything was removed.
async fn forget(app: &App, gid: Id<GuildMarker>, users: &[i64]) -> Result<bool> {
    let (mut tx, version, mut doc) = crate::settings::locked(app, gid).await?;
    let mut lines = vec![];
    for &u in users {
        let held = permits::forget_member(&mut tx, gid.get() as i64, u).await?;
        if !held.is_empty() {
            lines.push(format!(
                "<@{u}> left the server: removed {}",
                held.join(", ")
            ));
        }
    }
    if lines.is_empty() {
        tx.commit().await?;
        return Ok(false);
    }
    crate::settings::append(app, tx, gid, version, &mut doc, app.me.get()).await?;
    let log = security_log(
        "Permits",
        "log.anti_nuke.permit_holders_left",
        &[("gone", &capped(&lines))],
    );
    warn_log(app, gid, log).await;
    Ok(true)
}

/// The change is done and saved: a failed log is only a warning.
async fn warn_log(app: &App, gid: Id<GuildMarker>, log: Log) {
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "permit log failed");
    }
}

/// `lines` joined up to about 1500 characters, then "and N more": a log holds 4096 in all.
fn capped(lines: &[String]) -> String {
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if out.len() + l.len() > 1500 {
            return format!("{out}\nand {} more", lines.len() - i);
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(l);
    }
    out
}

fn ids(roles: &[Id<RoleMarker>]) -> Vec<i64> {
    roles.iter().map(|r| r.get() as i64).collect()
}

/// Every member, paged as in `sticky::sync`; an error anywhere fails the whole list.
/// Job `staff_role_off`: the owner turned the Staff role off and it was a role of theirs, so it
/// stays; Brim takes it from every member who holds it. Skipped when the role is a level or
/// permit role again (turned on again meanwhile): then `sync` owns it.
pub async fn staff_role_off(app: &App, gid: Id<GuildMarker>, role: u64) -> Result<()> {
    let g = gid.get() as i64;
    let Some(role) = Id::<RoleMarker>::new_checked(role) else {
        return Ok(());
    };
    let _one = run_lock(gid).lock_owned().await;
    if permits::permit_roles(&app.pool, g)
        .await?
        .contains(&(role.get() as i64))
    {
        return Ok(());
    }
    let info = app.guild_info(gid).await?;
    if info.roles.get(&role).is_none_or(|p| *p >= info.bot_top) {
        return Ok(());
    }
    for m in all_members(app, gid).await? {
        if !m.roles.contains(&role) {
            continue;
        }
        let take = app
            .http
            .remove_guild_member_role(gid, m.user.id, role)
            .reason("Staff role: turned off in the dashboard")
            .await;
        if let Err(e) = take {
            tracing::warn!(?e, user = m.user.id.get(), "taking the Staff role failed");
        }
    }
    Ok(())
}

pub(crate) async fn all_members(app: &App, gid: Id<GuildMarker>) -> Result<Vec<Member>> {
    let mut all = vec![];
    let mut after = None;
    loop {
        let req = app.http.guild_members(gid).limit(1000);
        let req = match after {
            Some(a) => req.after(a),
            None => req,
        };
        let page = req.await?.models().await?;
        let more = page.len() == 1000;
        after = page.last().map(|m| m.user.id);
        all.extend(page);
        if !more {
            return Ok(all);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_lists_are_capped() {
        let lines: Vec<String> = (0..200)
            .map(|i| format!("<@{i:018}>: Moderators"))
            .collect();
        let out = super::capped(&lines);
        assert!(out.len() < 1600, "{}", out.len());
        assert!(out.ends_with(" more"), "{out}");
        let n: usize = out.lines().last().unwrap()[4..]
            .trim_end_matches(" more")
            .parse()
            .unwrap();
        assert_eq!(out.lines().count() - 1 + n, 200);
        assert_eq!(super::capped(&lines[..2]), lines[..2].join("\n"));
    }
}

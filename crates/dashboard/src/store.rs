//! Versioned config and permit tables. Every write path goes through `commit`.
use crate::ApiError;
use brim_core::permits;
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// Version 0 with an empty config until the first save: nothing else creates a guild's first row.
pub async fn current(db: &PgPool, guild: i64) -> Result<(i32, Value), ApiError> {
    Ok(sqlx::query_as("select version, config from guild_config where guild_id = $1 order by version desc limit 1")
        .bind(guild).fetch_optional(db).await?.unwrap_or((0, json!({}))))
}

/// The newest version under the config lock, so a `commit` in the same transaction cannot conflict.
pub async fn locked(
    tx: &mut Transaction<'_, Postgres>,
    guild: i64,
) -> Result<(i32, Value), ApiError> {
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(guild)
        .execute(&mut **tx)
        .await?;
    Ok(sqlx::query_as("select version, config from guild_config where guild_id = $1 order by version desc limit 1")
        .bind(guild).fetch_optional(&mut **tx).await?.unwrap_or((0, json!({}))))
}

/// Records a change made to the permit tables alone as a new version, so rollbacks and stale saves see it.
pub async fn bump(
    tx: &mut Transaction<'_, Postgres>,
    guild: i64,
    user: i64,
) -> Result<i32, ApiError> {
    let (cur, config) = locked(tx, guild).await?;
    commit(tx, guild, cur, &config, user).await
}

/// Appends a new config version if `expected` is still the newest, then tells the bot.
/// The version's `permits` copy is always read from the live tables, never taken from `config`.
pub async fn commit(
    tx: &mut Transaction<'_, Postgres>,
    guild: i64,
    expected: i32,
    config: &Value,
    user: i64,
) -> Result<i32, ApiError> {
    let (cur, before) = locked(tx, guild).await?;
    if cur != expected {
        return Err(ApiError::Conflict(cur));
    }
    let mut config = config.clone();
    config["permits"] = read_permits(tx, guild).await?;
    // A changed EO/TA list or level role: the roles follow (`permit_sync`).
    if permits::level_lists_changed(&before["permits"], &config["permits"]) {
        let now = chrono::Utc::now().timestamp();
        brim_core::jobs::enqueue_once(&mut **tx, guild, "permit_sync", now).await?;
    }
    sqlx::query(
        "insert into guild_config (guild_id, version, config, changed_by) values ($1,$2,$3,$4)",
    )
    .bind(guild)
    .bind(cur + 1)
    .bind(&config)
    .bind(user)
    .execute(&mut **tx)
    .await?;
    sqlx::query("select pg_notify('config_changed', $1)")
        .bind(format!("{guild}:{}", cur + 1))
        .execute(&mut **tx)
        .await?;
    Ok(cur + 1)
}

fn ids(v: &Value) -> Vec<i64> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str()?.parse().ok())
        .collect()
}

pub(crate) fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(String::from))
        .collect()
}

pub async fn read_permits(c: &mut PgConnection, guild: i64) -> Result<Value, ApiError> {
    Ok(permits::snapshot(c, guild).await?)
}

/// Sets the level roles (Trusted Admin, Owner) to `levels` (`{"trusted_admin": "id", ...}`, the
/// `permits.levels` of a config version). Only Auto Setup and a rollback write them: a permits
/// save or an import never does, since naming any role here makes Brim take it from everyone
/// not on the level. A level left out loses its role; nothing happens when `levels` is no object
/// (a version from before level roles). Queues a permit sync when something changed.
pub async fn write_levels(
    c: &mut PgConnection,
    guild: i64,
    levels: &Value,
) -> Result<(), ApiError> {
    let Some(levels) = levels.as_object() else {
        return Ok(());
    };
    let before = permits::snapshot(c, guild).await?["levels"].clone();
    sqlx::query("delete from level_roles where guild_id = $1")
        .bind(guild)
        .execute(&mut *c)
        .await?;
    for level in permits::ROLE_LEVELS {
        let Some(role) = levels
            .get(level)
            .and_then(|r| r.as_str()?.parse::<i64>().ok())
        else {
            continue;
        };
        sqlx::query("insert into level_roles (guild_id, level, role_id) values ($1, $2, $3)")
            .bind(guild)
            .bind(level)
            .bind(role)
            .execute(&mut *c)
            .await
            .map_err(|_| {
                ApiError::BadRequest("one role cannot be the role of two levels".into())
            })?;
    }
    if permits::snapshot(c, guild).await?["levels"] != before {
        let now = chrono::Utc::now().timestamp();
        brim_core::jobs::enqueue_once(&mut *c, guild, "permit_sync", now).await?;
    }
    Ok(())
}

/// Who holds which permit and the roles Brim gives them: what the bot's `permit_sync` acts on.
fn held(p: &Value) -> Vec<Value> {
    let custom = p["custom"].as_array().into_iter().flatten();
    // The EO and TA lists too: they get the level roles.
    custom
        .map(|c| json!([c["name"], c["roles"], c["members"]]))
        .chain([p["eo"].clone(), p["ta"].clone()])
        .collect()
}

/// For a rollback or import: each permit in `target` keeps only the members it has in `cur` now,
/// so going back to an older version never adds a person (owner decision 2026-11-15). A version from
/// before member lists (no `members` key) keeps the current members. The same for the Extra
/// Owner and Trusted Admin lists.
pub fn keep_current_members(target: &mut Value, cur: &Value) {
    for k in ["eo", "ta"] {
        let old = target.get(k).map(strs);
        target[k] = json!(strs(&cur[k])
            .into_iter()
            .filter(|u| old.as_ref().is_none_or(|o| o.contains(u)))
            .collect::<Vec<_>>());
    }
    for c in target["custom"].as_array_mut().into_iter().flatten() {
        let now = cur["custom"].as_array().into_iter().flatten();
        let now: Vec<Value> = now
            .filter(|n| n["name"] == c["name"])
            .flat_map(|n| n["members"].as_array().cloned().unwrap_or_default())
            .collect();
        c["members"] = match c.get("members") {
            None => json!(now),
            Some(m) => {
                let m = strs(m);
                json!(now
                    .iter()
                    .filter(|u| m.iter().any(|x| **u == *x))
                    .collect::<Vec<_>>())
            }
        };
    }
}

/// Replaces the guild's EO/TA lists and custom permits with `p`. Permits are upserted by name; a
/// member who stays keeps `added_by`, a new one gets `actor`. A change to who holds a permit or its
/// roles queues the bot's `permit_sync`. Takes the config lock first: the bot's TA edit locks, then
/// writes, so the reverse order deadlocks.
pub async fn write_permits(
    c: &mut PgConnection,
    guild: i64,
    p: &Value,
    actor: i64,
) -> Result<(), ApiError> {
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(guild)
        .execute(&mut *c)
        .await?;
    let cur = read_permits(c, guild).await?;
    let before = held(&cur);
    // A permit sent without `members` or with `null` (an old UI tab, an older MCP client) keeps its members;
    // only an explicit list, `[]` too, changes them.
    let mut p = p.clone();
    for x in p["custom"].as_array_mut().into_iter().flatten() {
        if x.is_object() && x.get("members").is_none_or(Value::is_null) {
            let name = x["name"].as_str().map(str::trim);
            let mut now = cur["custom"].as_array().into_iter().flatten();
            let now = now.find(|n| n["name"].as_str() == name);
            x["members"] = now.map_or(json!([]), |n| n["members"].clone());
        }
    }
    let p = &p;
    let bad = |m: &str| ApiError::BadRequest(m.into());
    let custom = p["custom"]
        .as_array()
        .ok_or_else(|| bad("permits.custom must be a list"))?;
    // One permit per person, checked over the whole list before anything is written.
    let (mut users, mut of) = (vec![], vec![]);
    for x in custom {
        let name = x["name"].as_str().unwrap_or_default().trim();
        for m in x["members"].as_array().into_iter().flatten() {
            let id = m
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|id| *id > 0)
                .ok_or_else(|| ApiError::BadRequest(format!("{m} is not a user id")))?;
            match users.iter().position(|u| *u == id) {
                Some(i) if of[i] == name => {}
                Some(i) => {
                    return Err(ApiError::BadRequest(format!(
                    "<@{id}> is in the permits {} and {name}. A person can hold only one permit.",
                    of[i]
                )))
                }
                None => {
                    users.push(id);
                    of.push(name);
                }
            }
        }
    }
    // Members who leave or move first, so a move does not hit the one-permit-per-person key.
    sqlx::query(
        "delete from permit_members m using permits p where p.id = m.permit_id and m.guild_id = $1
         and (m.user_id, p.name) not in (select * from unnest($2::bigint[], $3::text[]))",
    )
    .bind(guild)
    .bind(&users)
    .bind(&of)
    .execute(&mut *c)
    .await?;
    let mut names = vec![];
    for x in custom {
        let name = x["name"]
            .as_str()
            .map(str::trim)
            .filter(|n| !n.is_empty() && n.len() <= 32)
            .ok_or_else(|| bad("permit needs a name of 1-32 chars"))?;
        if names.contains(&name) {
            return Err(bad("duplicate permit name"));
        }
        names.push(name);
        let perms = strs(&x["perms"]);
        let whitelist = strs(&x["whitelist"]);
        if let Some(k) = perms
            .iter()
            .find(|k| !permits::known(k, false))
            .or(whitelist.iter().find(|k| !permits::known(k, true)))
        {
            return Err(ApiError::BadRequest(format!("unknown permission `{k}`")));
        }
        let id: i64 = sqlx::query_scalar(
            "insert into permits (guild_id, name, rank, perms, limits, whitelist) values ($1,$2,$3,$4,$5,$6)
             on conflict (guild_id, name) do update set rank = $3, perms = $4, limits = $5, whitelist = $6 returning id")
            .bind(guild).bind(name).bind(x["rank"].as_i64().unwrap_or(0) as i32).bind(&perms)
            .bind(if x["limits"].is_object() { x["limits"].clone() } else { json!({}) }).bind(&whitelist)
            .fetch_one(&mut *c).await?;
        sqlx::query("delete from permit_roles where permit_id = $1")
            .bind(id)
            .execute(&mut *c)
            .await?;
        for r in ids(&x["roles"]) {
            sqlx::query("insert into permit_roles (permit_id, guild_id, role_id) values ($1,$2,$3) on conflict do nothing")
                .bind(id).bind(guild).bind(r).execute(&mut *c).await?;
        }
        for u in ids(&x["members"]) {
            sqlx::query("insert into permit_members (permit_id, guild_id, user_id, added_by) values ($1,$2,$3,$4) on conflict do nothing")
                .bind(id).bind(guild).bind(u).bind(actor).execute(&mut *c).await?;
        }
    }
    sqlx::query("delete from permits where guild_id = $1 and name <> all($2)")
        .bind(guild)
        .bind(&names)
        .execute(&mut *c)
        .await?;
    for (table, key) in [("extra_owners", "eo"), ("trusted_admins", "ta")] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {table} where guild_id = $1"
        )))
        .bind(guild)
        .execute(&mut *c)
        .await?;
        for u in ids(&p[key]) {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "insert into {table} (guild_id, user_id) values ($1,$2) on conflict do nothing"
            )))
            .bind(guild)
            .bind(u)
            .execute(&mut *c)
            .await?;
        }
    }
    if held(&read_permits(c, guild).await?) != before {
        let now = chrono::Utc::now().timestamp();
        brim_core::jobs::enqueue_once(&mut *c, guild, "permit_sync", now).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn versions_conflicts_and_permits() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_000_000_000 + (std::process::id() as i64);
        assert_eq!(current(&db, g).await.unwrap(), (0, json!({})));
        let mut tx = db.begin().await.unwrap();
        assert_eq!(commit(&mut tx, g, 0, &json!({"a": 1}), 7).await.unwrap(), 1);
        assert!(matches!(
            commit(&mut tx, g, 0, &json!({}), 7).await,
            Err(ApiError::Conflict(1))
        ));
        let p = json!({"eo": ["5"], "ta": [], "custom": [{"name": "Mod", "rank": 1, "perms": ["ban", "dashboard_view"], "limits": {"ban": {"per_min": 5}}, "whitelist": [], "roles": ["10", "11"], "members": []}]});
        write_permits(&mut tx, g, &p, 7).await.unwrap();
        assert_eq!(read_permits(&mut tx, g).await.unwrap(), p);
        write_permits(&mut tx, g, &json!({"eo": [], "ta": ["6"], "custom": []}), 7)
            .await
            .unwrap();
        assert_eq!(read_permits(&mut tx, g).await.unwrap()["custom"], json!([]));
        assert!(write_permits(
            &mut tx,
            g,
            &json!({"custom": [{"name": "X", "perms": ["BAD"]}]}),
            7
        )
        .await
        .is_err());
        tx.rollback().await.unwrap();
    }

    /// Owner decision 2026-11-15: members are saved by user id with who added them, one permit per person,
    /// and a change to members or roles queues the bot's `permit_sync`.
    #[tokio::test]
    async fn permit_members_by_user_id() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_903_000_000 + (std::process::id() as i64);
        let mut tx = db.begin().await.unwrap();
        let p = |a: &[&str], b: &[&str]| {
            json!({"eo": [], "ta": [], "custom": [
            {"name": "A", "perms": [], "roles": ["10"], "members": a},
            {"name": "B", "perms": [], "roles": [], "members": b}]})
        };
        async fn rows(c: &mut PgConnection, g: i64) -> Vec<(i64, Option<i64>)> {
            sqlx::query_as(
                "select user_id, added_by from permit_members where guild_id = $1 order by user_id",
            )
            .bind(g)
            .fetch_all(c)
            .await
            .unwrap()
        }
        // Takes the queued syncs, so each step counts its own.
        async fn jobs(c: &mut PgConnection, g: i64) -> i64 {
            sqlx::query_scalar(
                "with d as (delete from scheduled_jobs where guild_id = $1 and kind = 'permit_sync' returning 1)
                 select count(*) from d",
            )
            .bind(g)
            .fetch_one(c)
            .await
            .unwrap()
        }
        write_permits(&mut tx, g, &p(&["21", "22"], &[]), 7)
            .await
            .unwrap();
        assert_eq!(rows(&mut tx, g).await, [(21, Some(7)), (22, Some(7))]);
        assert_eq!(jobs(&mut tx, g).await, 1);
        // 21 stays (added_by kept), 22 moves to B and 23 is new: both by 8.
        write_permits(&mut tx, g, &p(&["21"], &["22", "23"]), 8)
            .await
            .unwrap();
        assert_eq!(
            rows(&mut tx, g).await,
            [(21, Some(7)), (22, Some(8)), (23, Some(8))]
        );
        let snap = read_permits(&mut tx, g).await.unwrap();
        assert_eq!(snap["custom"][1]["members"], json!(["22", "23"]));
        assert_eq!(jobs(&mut tx, g).await, 1);
        // A save that changes no member or role queues nothing.
        write_permits(&mut tx, g, &p(&["21"], &["22", "23"]), 8)
            .await
            .unwrap();
        assert_eq!(jobs(&mut tx, g).await, 0);
        // A permit sent without `members` keeps them; `[]` clears. Two changes queue one sync.
        let old_tab = json!({"eo": [], "ta": [], "custom": [
            {"name": "A", "perms": [], "roles": ["10"]},
            {"name": "B", "perms": [], "roles": [], "members": []}]});
        write_permits(&mut tx, g, &old_tab, 8).await.unwrap();
        assert_eq!(rows(&mut tx, g).await, [(21, Some(7))]);
        write_permits(&mut tx, g, &p(&["21"], &["22"]), 8)
            .await
            .unwrap();
        assert_eq!(jobs(&mut tx, g).await, 1);
        // `"members": null` keeps them too, as a missing key.
        let null = json!({"eo": [], "ta": [], "custom": [
            {"name": "A", "perms": [], "roles": ["10"], "members": null},
            {"name": "B", "perms": [], "roles": [], "members": ["22"]}]});
        write_permits(&mut tx, g, &null, 8).await.unwrap();
        assert_eq!(rows(&mut tx, g).await, [(21, Some(7)), (22, Some(8))]);
        for (bad, says) in [
            (p(&["21"], &["21"]), "<@21> is in the permits A and B"),
            (p(&["x"], &[]), "not a user id"),
            (p(&["-4"], &[]), "not a user id"),
        ] {
            let r = write_permits(&mut tx, g, &bad, 8).await;
            assert!(
                matches!(&r, Err(ApiError::BadRequest(m)) if m.contains(says)),
                "{r:?}"
            );
        }
        tx.rollback().await.unwrap();
    }

    /// A rollback or import keeps only members a permit has now, and a version without member
    /// lists keeps them all.
    #[test]
    fn keep_current_members_never_adds() {
        let cur = json!({"custom": [{"name": "A", "members": ["1", "2"]}, {"name": "B", "members": ["3"]}]});
        let mut old = json!({"custom": [
            {"name": "A", "members": ["2", "3", "4"]},
            {"name": "B"},
            {"name": "C", "members": ["1"]}]});
        keep_current_members(&mut old, &cur);
        let m: Vec<&Value> = old["custom"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| &c["members"])
            .collect();
        assert_eq!(m, [&json!(["2"]), &json!(["3"]), &json!([])]);
    }

    /// Extra Owners and Trusted Admins too: a rollback keeps only those listed now.
    #[test]
    fn keep_current_members_never_adds_extra_owners_or_trusted_admins() {
        let cur = json!({"eo": ["1"], "ta": ["5", "6"], "custom": []});
        let mut old = json!({"eo": ["1", "2"], "ta": ["6", "7"], "custom": []});
        keep_current_members(&mut old, &cur);
        assert_eq!((&old["eo"], &old["ta"]), (&json!(["1"]), &json!(["6"])));
    }

    /// A version's `permits` copy is the live tables, also for a save that carried a stale copy, and
    /// a change to the tables alone (rescue claim) gets a version of its own.
    #[tokio::test]
    async fn every_version_holds_the_live_permits() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_840_000_000 + (std::process::id() as i64);
        let mut tx = db.begin().await.unwrap();
        sqlx::query("insert into trusted_admins (guild_id, user_id) values ($1, 6)")
            .bind(g)
            .execute(&mut *tx)
            .await
            .unwrap();
        let stale = json!({"a": 1, "permits": {"eo": [], "ta": ["6", "9"], "custom": []}});
        assert_eq!(commit(&mut tx, g, 0, &stale, 7).await.unwrap(), 1);
        sqlx::query("insert into extra_owners (guild_id, user_id) values ($1, 5)")
            .bind(g)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(bump(&mut tx, g, 5).await.unwrap(), 2);
        let rows: Vec<(i32, Value)> = sqlx::query_as(
            "select version, config from guild_config where guild_id = $1 order by version",
        )
        .bind(g)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        let live = |eo: &[&str]| json!({"a": 1, "permits": {"eo": eo, "ta": ["6"], "custom": []}});
        assert_eq!(rows, vec![(1, live(&[])), (2, live(&["5"]))]);
        tx.rollback().await.unwrap();
    }

    /// A permit save running next to the bot's TA edit (config lock, then the table) must wait for it,
    /// not take the table rows first and deadlock on the lock in `commit`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn permit_save_waits_for_the_config_lock_instead_of_deadlocking() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_870_000_000 + (std::process::id() as i64);
        sqlx::query("insert into trusted_admins (guild_id, user_id) values ($1, 6)")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        let mut bot = db.begin().await.unwrap();
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(g)
            .execute(&mut *bot)
            .await
            .unwrap();
        let dash = {
            let db = db.clone();
            tokio::spawn(async move {
                let mut tx = db.begin().await.unwrap();
                write_permits(&mut tx, g, &json!({"eo": [], "ta": [], "custom": []}), 7).await?;
                commit(&mut tx, g, 0, &json!({}), 7).await?;
                tx.commit().await.map_err(ApiError::from)
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        sqlx::query("delete from trusted_admins where guild_id = $1 and user_id = 6")
            .bind(g)
            .execute(&mut *bot)
            .await
            .expect("the bot's write must not deadlock");
        bot.commit().await.unwrap();
        assert!(dash.await.unwrap().is_ok());
        for t in ["guild_config", "trusted_admins"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn history_keeps_newest_200_versions() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_300_000_000 + (std::process::id() as i64);
        let mut tx = db.begin().await.unwrap();
        for v in 0..205 {
            commit(&mut tx, g, v, &json!({}), 7).await.unwrap();
        }
        let (n, low): (i64, i32) =
            sqlx::query_as("select count(*), min(version) from guild_config where guild_id = $1")
                .bind(g)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!((n, low), (200, 6));
        tx.rollback().await.unwrap();
    }
}

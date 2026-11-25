//! Job `exempt_roles_move`: older versions let a role exempt its holders from Auto Mod and the
//! golden channel. A role exempts nobody now (owner rule: trust is by user id and permits), so the
//! Overview Fix moves today's holders once, by user id:
//! - Auto Mod whitelists: into the permit [`PERMIT`] (flag `automod`), at the bottom of the
//!   permit list. A member in another permit (one permit each) without Automod Immunity goes on
//!   the same tab's Members list instead.
//! - Golden channel: onto its Members list (a permit does not exempt from the golden channel).
//!
//! The role lists are emptied in the same config version, so History shows them and a rollback
//! brings them back.
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use brim_logging::{overlay::security_log, LogKind};
use serde_json::{json, Value};
use twilight_model::id::{marker::GuildMarker, Id};

use crate::{app::App, events::send_log};

pub const KIND: &str = "exempt_roles_move";

/// The permit the Fix fills (the catalog's name of the flag).
pub const PERMIT: &str = "Automod Immunity";

/// Whitelist tabs: config path of the tab ("" is the Spam tab at the top level).
const TABS: [&str; 4] = ["", "invites", "mentions", "everyone"];

/// A Members list grows to at most this many ids by the move: a whole config version must stay
/// small. Larger roles go into the permit (a table), which has no such limit.
pub const MAX_LIST: usize = 1000;

fn tab<'a>(doc: &'a Value, t: &str) -> &'a Value {
    if t.is_empty() {
        &doc["whitelist"]
    } else {
        &doc["whitelist"][t]
    }
}

fn tab_mut<'a>(doc: &'a mut Value, t: &str) -> &'a mut Value {
    if t.is_empty() {
        &mut doc["whitelist"]
    } else {
        &mut doc["whitelist"][t]
    }
}

fn id_list(v: &Value) -> Vec<u64> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().and_then(|s| s.parse().ok()).or(x.as_u64()))
        .collect()
}

/// The stored exempt roles: per Auto Mod tab, and the golden channel's.
pub fn stored(doc: &Value) -> (BTreeMap<&'static str, Vec<u64>>, Vec<u64>) {
    let tabs = TABS
        .iter()
        .map(|t| (*t, id_list(&tab(doc, t)["roles"])))
        .filter(|(_, r)| !r.is_empty())
        .collect();
    (tabs, id_list(&doc["catch"]["roles"]))
}

/// One member as the move sees them.
pub struct Holder {
    pub user: u64,
    pub roles: Vec<u64>,
    pub bot: bool,
}

/// What the move does. Members lists are keyed by tab ("catch" for the golden channel).
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub to_permit: Vec<u64>,
    pub to_lists: BTreeMap<&'static str, Vec<u64>>,
    /// Already exempt by id: Owner, Extra Owner, Trusted Admin, or a permit with Automod Immunity.
    pub already: usize,
    /// Over [`MAX_LIST`]: not moved.
    pub not_moved: Vec<u64>,
}

/// `permit_of`: each listed member's permit, with whether it has Automod Immunity. `immune`:
/// Owner, Extra Owners and Trusted Admins. `lists`: the Members lists as stored now. With no
/// usable [`PERMIT`] (`permit_ok` false), every member goes on the tab's Members list.
pub fn plan(
    members: &[Holder],
    tabs: &BTreeMap<&'static str, Vec<u64>>,
    catch: &[u64],
    immune: &HashSet<u64>,
    permit_of: &HashMap<u64, bool>,
    lists: &BTreeMap<&'static str, Vec<u64>>,
    permit_ok: bool,
) -> Plan {
    let mut p = Plan::default();
    let mut len: BTreeMap<&str, usize> = lists.iter().map(|(k, v)| (*k, v.len())).collect();
    let mut over = HashSet::new();
    let mut add = |p: &mut Plan, list: &'static str, user: u64| {
        if lists.get(list).is_some_and(|l| l.contains(&user)) {
            return;
        }
        let n = len.entry(list).or_default();
        if *n >= MAX_LIST {
            if over.insert(user) {
                p.not_moved.push(user);
            }
            return;
        }
        *n += 1;
        p.to_lists.entry(list).or_default().push(user);
    };
    for m in members.iter().filter(|m| !m.bot) {
        let holds = |roles: &[u64]| m.roles.iter().any(|r| roles.contains(r));
        if holds(catch) && !immune.contains(&m.user) {
            add(&mut p, "catch", m.user);
        }
        let in_tabs: Vec<&'static str> = tabs
            .iter()
            .filter(|(_, r)| holds(r))
            .map(|(t, _)| *t)
            .collect();
        if in_tabs.is_empty() {
            continue;
        }
        match permit_of.get(&m.user) {
            _ if immune.contains(&m.user) => p.already += 1,
            Some(true) => p.already += 1,
            None if permit_ok => p.to_permit.push(m.user),
            // One permit each: their own permit stays, the tab lists them by id.
            _ => in_tabs.iter().for_each(|t| add(&mut p, t, m.user)),
        }
    }
    p
}

pub async fn run(app: &App, gid: Id<GuildMarker>, payload: &Value) -> Result<()> {
    let by = payload["by"].as_u64().unwrap_or(app.me.get());
    let g = gid.get() as i64;
    // Paged before the lock: a big server takes a while, and the lock holds every settings write.
    let members = crate::permit_sync::all_members(app, gid).await?;
    let info = app.guild_info(gid).await?;
    // Before the move to user ids, a permit came from its role: `permit_of` below would miss it,
    // and the later move could not list a member twice. Tried again until `permit_sync` ran.
    anyhow::ensure!(
        brim_core::permits::members_migrated(&app.pool, g).await?,
        "permits are not moved to members yet"
    );
    let owner = brim_core::permits::bot_owner(&app.pool, g, info.owner.get() as i64).await? as u64;
    let (mut tx, version, mut doc) = crate::settings::locked(app, gid).await?;
    let (tabs, catch) = stored(&doc);
    if tabs.is_empty() && catch.is_empty() {
        tx.commit().await?;
        return Ok(());
    }
    let immune: HashSet<u64> = sqlx::query_scalar::<_, i64>(
        "SELECT user_id FROM extra_owners WHERE guild_id = $1
         UNION SELECT user_id FROM trusted_admins WHERE guild_id = $1",
    )
    .bind(g)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|u| u as u64)
    .chain([owner])
    .collect();
    let permit_of: HashMap<u64, bool> = sqlx::query_as::<_, (i64, bool)>(
        "SELECT m.user_id, p.whitelist && '{automod,all}' FROM permit_members m
         JOIN permits p ON p.id = m.permit_id WHERE m.guild_id = $1",
    )
    .bind(g)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(u, ok)| (u as u64, ok))
    .collect();
    // Only a permit that gives Automod Immunity and nothing else: a permit of that name with more
    // in it is the owner's own, and filling it would hand out its rights.
    let found: Option<(i64, bool)> = sqlx::query_as(
        "SELECT id, whitelist = '{automod}' AND perms = '{}' FROM permits WHERE guild_id = $1 AND name = $2",
    )
    .bind(g)
    .bind(PERMIT)
    .fetch_optional(&mut *tx)
    .await?;
    let mut lists: BTreeMap<&'static str, Vec<u64>> = TABS
        .iter()
        .map(|t| (*t, id_list(&tab(&doc, t)["users"])))
        .collect();
    lists.insert("catch", id_list(&doc["catch"]["users"]));
    let holders: Vec<Holder> = members
        .iter()
        .map(|m| Holder {
            user: m.user.id.get(),
            roles: m.roles.iter().map(|r| r.get()).collect(),
            bot: m.user.bot,
        })
        .collect();
    let permit_ok = found.is_none_or(|(_, ok)| ok);
    let p = plan(
        &holders, &tabs, &catch, &immune, &permit_of, &lists, permit_ok,
    );
    if !p.to_permit.is_empty() {
        let id = match found {
            Some((id, _)) => id,
            None => {
                // At the bottom of the list (the order is the hierarchy): staff permits outrank it.
                sqlx::query("UPDATE permits SET rank = rank + 1 WHERE guild_id = $1")
                    .bind(g)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query_scalar(
                    "INSERT INTO permits (guild_id, name, rank, perms, limits, whitelist)
                     VALUES ($1, $2, 0, '{}', '{}'::jsonb, '{automod}') RETURNING id",
                )
                .bind(g)
                .bind(PERMIT)
                .fetch_one(&mut *tx)
                .await?
            }
        };
        let users: Vec<i64> = p.to_permit.iter().map(|u| *u as i64).collect();
        // One statement for any number of members; one permit each stays the table's rule.
        sqlx::query(
            "INSERT INTO permit_members (permit_id, guild_id, user_id, added_by)
             SELECT $1, $2, u, $4 FROM unnest($3::bigint[]) u ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(g)
        .bind(&users)
        .bind(by as i64)
        .execute(&mut *tx)
        .await?;
    }
    for (list, users) in &p.to_lists {
        let at = if *list == "catch" {
            &mut doc["catch"]["users"]
        } else {
            &mut tab_mut(&mut doc, list)["users"]
        };
        if !at.is_array() {
            *at = json!([]);
        }
        let a = at.as_array_mut().expect("set above");
        a.extend(users.iter().map(|u| json!(u.to_string())));
    }
    for t in tabs.keys() {
        tab_mut(&mut doc, t)["roles"] = json!([]);
    }
    if !catch.is_empty() {
        doc["catch"]["roles"] = json!([]);
    }
    crate::settings::append(app, tx, gid, version, &mut doc, by).await?;
    app.forget_guild_levels(gid.get());
    // Each list names its members, so staff can check who got the exemption.
    let to_lists: Vec<u64> = p.to_lists.values().flatten().copied().collect();
    let log = security_log(
        "Permits",
        "log.anti_nuke.exempt_roles_moved",
        &[
            (
                "alert",
                "**Auto Mod exemptions moved from roles to members**",
            ),
            ("by", &format!("<@{by}>")),
            (
                "to_permit",
                &named(&p.to_permit, &format!("added to {PERMIT}")),
            ),
            ("to_lists", &named(&to_lists, "added to the Members lists")),
            ("already", &format!("{} already exempt", p.already)),
            (
                "not_moved",
                &named(
                    &p.not_moved,
                    &format!("not moved: a Members list holds at most {MAX_LIST} by this move"),
                ),
            ),
        ],
    );
    if let Err(e) = send_log(app, gid.get(), LogKind::AntiNuke, log).await {
        tracing::warn!(?e, "exempt roles log failed");
    }
    Ok(())
}

/// "N what: <@a>, <@b> and M more", short enough for three of them in one log.
fn named(users: &[u64], what: &str) -> String {
    const SHOWN: usize = 20;
    let mut out = format!("{} {what}", users.len());
    if !users.is_empty() {
        let ids: Vec<String> = users
            .iter()
            .take(SHOWN)
            .map(|u| format!("<@{u}>"))
            .collect();
        out.push_str(&format!(": {}", ids.join(", ")));
        if users.len() > SHOWN {
            out.push_str(&format!(" and {} more", users.len() - SHOWN));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(user: u64, roles: &[u64]) -> Holder {
        Holder {
            user,
            roles: roles.to_vec(),
            bot: false,
        }
    }

    #[test]
    fn stored_reads_every_tab_and_the_catch_channel() {
        let doc = json!({"whitelist": {"roles": ["1"], "invites": {"roles": ["2"]}, "mentions": {"roles": []}},
            "catch": {"roles": ["3"]}});
        let (tabs, catch) = stored(&doc);
        assert_eq!(tabs.get(""), Some(&vec![1]));
        assert_eq!(tabs.get("invites"), Some(&vec![2]));
        assert!(!tabs.contains_key("mentions"));
        assert_eq!(catch, [3]);
    }

    #[test]
    fn each_member_goes_where_their_exemption_holds() {
        let tabs = BTreeMap::from([("", vec![10]), ("invites", vec![11])]);
        let members = [
            h(1, &[10]),     // no permit: the permit
            h(2, &[11]),     // in a permit without the flag: the Members list
            h(3, &[10]),     // permit with the flag: already exempt
            h(4, &[10, 20]), // Trusted Admin: already exempt
            h(5, &[20]),     // no listed role
            h(6, &[12]),     // the golden channel's role
            Holder {
                user: 7,
                roles: vec![10],
                bot: true,
            },
        ];
        let immune = HashSet::from([4]);
        let permit_of = HashMap::from([(2, false), (3, true)]);
        let p = plan(
            &members,
            &tabs,
            &[12],
            &immune,
            &permit_of,
            &BTreeMap::new(),
            true,
        );
        assert_eq!(p.to_permit, [1]);
        assert_eq!(p.to_lists.get("invites"), Some(&vec![2]));
        assert_eq!(p.to_lists.get("catch"), Some(&vec![6]));
        assert_eq!(p.already, 2);
        assert!(p.not_moved.is_empty());
        // The owner's own permit of that name without the flag: the Members list, not the permit.
        let p = plan(
            &members,
            &tabs,
            &[],
            &immune,
            &permit_of,
            &BTreeMap::new(),
            false,
        );
        assert!(p.to_permit.is_empty());
        assert_eq!(p.to_lists.get(""), Some(&vec![1]));
    }

    #[test]
    fn named_lists_a_few_and_counts_the_rest() {
        assert_eq!(named(&[], "moved"), "0 moved");
        let many: Vec<u64> = (1..=25).collect();
        let s = named(&many, "moved");
        assert!(
            s.starts_with("25 moved: <@1>, ") && s.ends_with(" and 5 more"),
            "{s}"
        );
    }

    #[test]
    fn a_member_listed_already_is_not_added_twice() {
        let tabs = BTreeMap::from([("", vec![10])]);
        let lists = BTreeMap::from([("", vec![2])]);
        let p = plan(
            &[h(2, &[10])],
            &tabs,
            &[],
            &HashSet::new(),
            &HashMap::from([(2, false)]),
            &lists,
            true,
        );
        assert_eq!(p, Plan::default());
    }

    /// 100,000 members, half in the role: the permit takes all of them, the golden channel's
    /// Members list stops at its cap, and the plan stays fast.
    #[test]
    fn large_roles_hold() {
        let members: Vec<Holder> = (1..=100_000u64)
            .map(|u| h(u, if u % 2 == 0 { &[10, 12] } else { &[30] }))
            .collect();
        let tabs = BTreeMap::from([("", vec![10])]);
        let start = std::time::Instant::now();
        let p = plan(
            &members,
            &tabs,
            &[12],
            &HashSet::new(),
            &HashMap::new(),
            &BTreeMap::new(),
            true,
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(p.to_permit.len(), 50_000);
        assert_eq!(p.to_lists["catch"].len(), MAX_LIST);
        assert_eq!(p.not_moved.len(), 50_000 - MAX_LIST);
    }

    /// The whole job on a scratch database and the mock: 3,001 role holders over four member
    /// pages go into the permit, a member of another permit and the golden channel's role holders
    /// go on the Members lists, and the role lists are emptied in one new version.
    #[tokio::test]
    async fn the_job_moves_every_holder_by_id() {
        let Some(t) = crate::test_support::T::new(300).await else {
            return;
        };
        let (g, owner) = (t.g(0), t.owner());
        let gi = g.get() as i64;
        let cfg = json!({"whitelist": {"roles": ["50"], "invites": {"roles": ["51"]}}, "catch": {"roles": ["52"]}});
        t.server_config(0, owner, cfg).await;
        // Before permits moved to members the job waits: a permit still came from its role.
        assert!(run(&t.app, g, &json!({"by": owner})).await.is_err());
        sqlx::query("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
            .bind(gi)
            .execute(&t.app.pool)
            .await
            .unwrap();
        let mods: i64 = sqlx::query_scalar(
            "INSERT INTO permits (guild_id, name, rank, perms, limits, whitelist) VALUES ($1, 'Mods', 0, '{ban}', '{}', '{}') RETURNING id",
        )
        .bind(gi)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, 1003)",
        )
        .bind(mods)
        .bind(gi)
        .execute(&t.app.pool)
        .await
        .unwrap();
        t.set(|m| {
            m.members.insert((g.get(), 1002), vec![51, 52]);
            m.members.insert((g.get(), 1003), vec![50]);
            m.members.insert((g.get(), 1004), vec![60]);
            for u in 10_000..13_000 {
                m.members.insert((g.get(), u), vec![50]);
            }
        });
        run(&t.app, g, &json!({"by": owner})).await.unwrap();
        let ranks: Vec<(String, i32, i64)> = sqlx::query_as(
            "SELECT p.name, p.rank, (SELECT count(*) FROM permit_members m WHERE m.permit_id = p.id)
             FROM permits p WHERE p.guild_id = $1 ORDER BY p.rank",
        )
        .bind(gi)
        .fetch_all(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(
            ranks,
            [(PERMIT.to_string(), 0, 3001), ("Mods".to_string(), 1, 1)]
        );
        let doc: Value = sqlx::query_scalar(
            "SELECT config FROM guild_config WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
        )
        .bind(gi)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(doc["whitelist"]["roles"], json!([]));
        assert_eq!(doc["whitelist"]["invites"]["roles"], json!([]));
        assert_eq!(doc["catch"]["roles"], json!([]));
        assert_eq!(doc["whitelist"]["users"], json!(["1003"]));
        // 1002 had no permit: the permit covers the invites tab too.
        assert!(doc["whitelist"]["invites"]["users"].is_null());
        assert_eq!(doc["catch"]["users"], json!(["1002"]));
        // A second run finds no roles and changes nothing.
        run(&t.app, g, &json!({"by": owner})).await.unwrap();
        let versions: i64 =
            sqlx::query_scalar("SELECT count(*) FROM guild_config WHERE guild_id = $1")
                .bind(gi)
                .fetch_one(&t.app.pool)
                .await
                .unwrap();
        assert_eq!(versions, 2);
    }
}

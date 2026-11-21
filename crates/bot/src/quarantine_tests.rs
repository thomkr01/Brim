//! Behaviour tests of the quarantine state (D2/N1, N5) through entry
//! points that main had too, so each one also shows the bug on main. They run only when
//! DATABASE_URL points at a scratch Postgres; Discord is the mock in `test_support`.
use crate::test_support::{ME, T};
use serde_json::{json, Value};
use twilight_model::{
    channel::Channel,
    gateway::payload::incoming::{MemberAdd, MemberUpdate},
    guild::audit_log::AuditLogEntry,
    id::Id,
};

const Q: u64 = 500;
const ROLE: u64 = 501;
const BOOST: u64 = 502;
const VERIFIED: u64 = 503;
const AUTOROLE: u64 = 504;
const BOT_TOP: u64 = 520;
/// Log channel, known to be private.
const LOGS: u64 = 88;

fn logging() -> Value {
    json!({"routes": {"anti_nuke": LOGS.to_string(), "verification": LOGS.to_string(),
                      "join_gate": LOGS.to_string(), "mod_command": LOGS.to_string(), "server": LOGS.to_string()}})
}

/// Server `i` with `cfg` (plus the log routes), its roles in Discord, and the bot holding the top role.
async fn server(t: &T, i: u64, mut cfg: Value) {
    cfg["logging"] = logging();
    t.server_config(i, t.owner(), cfg).await;
    // server_config seeds an empty guild cache; the role tests need the real one.
    t.app.invalidate_guild(t.g(i));
    let g = t.g(i).get();
    t.set(|m| {
        let all = vec![
            (g, false, 0, 0),
            (Q, false, 0, 1),
            (ROLE, false, 0, 2),
            (BOOST, true, 0, 3),
            (VERIFIED, false, 0, 4),
            (AUTOROLE, false, 0, 5),
            (BOT_TOP, true, 8, 10),
        ];
        m.roles.insert(g, all);
        m.members.insert((g, ME), vec![BOT_TOP]);
    });
    let private = (false, std::time::Instant::now());
    t.app
        .log_public
        .lock()
        .unwrap()
        .insert((t.g(i), Id::new(LOGS)), private);
}

fn roles(t: &T, i: u64, u: u64) -> Option<Vec<u64>> {
    t.mock
        .lock()
        .unwrap()
        .members
        .get(&(t.g(i).get(), u))
        .cloned()
}

async fn saved(t: &T, i: u64, u: u64) -> Option<Vec<i64>> {
    sqlx::query_scalar("SELECT saved_roles FROM quarantines WHERE guild_id = $1 AND user_id = $2")
        .bind(t.g(i).get() as i64)
        .bind(u as i64)
        .fetch_optional(&t.app.pool)
        .await
        .unwrap()
}

async fn row(t: &T, i: u64, u: u64, saved: &[i64]) {
    sqlx::query("INSERT INTO quarantines (guild_id, user_id, saved_roles) VALUES ($1, $2, $3)")
        .bind(t.g(i).get() as i64)
        .bind(u as i64)
        .bind(saved)
        .execute(&t.app.pool)
        .await
        .unwrap();
}

fn web_verification() -> Value {
    json!({"enabled": true, "mode": "Web", "target": "Everyone", "on_fail": "Nothing",
           "roles": [VERIFIED.to_string()], "channel": "700"})
}

/// D1: a quarantine row whose member got their roles back by hand is not "already contained": a
/// re-armed nuker is contained again (and today's roles saved), not only reverted.
#[tokio::test]
async fn a_stale_quarantine_row_does_not_count_as_contained() {
    let Some(t) = T::new(150).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string(),
                    "limits": {"Ban": {"per_minute": 1, "per_hour": 1}}});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (stale, held) = (t.base * 10 + 1, t.base * 10 + 2);
    // Released by hand (a Quarantine Whitelist user): other roles than the saved ones, no Q.
    row(&t, 0, stale, &[ROLE as i64]).await;
    // Still held in Discord.
    row(&t, 0, held, &[ROLE as i64]).await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), stale), vec![AUTOROLE]);
        m.members.insert((t.g(0).get(), held), vec![Q]);
    });
    let cfg = t.app.cfg(t.g(0).get()).await.unwrap();
    for u in [stale, held] {
        let r = crate::antinuke::count(
            &t.app,
            &cfg,
            t.g(0),
            Id::new(u),
            brim_antinuke::Action::Ban,
            Some(9),
            &[],
            false,
            5,
        )
        .await;
        tracing::info!(?r, u, "count");
    }
    assert_eq!(roles(&t, 0, stale), Some(vec![Q]), "contained again");
    assert_eq!(
        saved(&t, 0, stale).await,
        Some(vec![AUTOROLE as i64]),
        "the stale saved list is replaced by today's roles"
    );
    let patch = |u| format!("PATCH /guilds/{}/members/{u}", t.g(0));
    assert_eq!(
        t.calls(&patch(held)),
        0,
        "a held member is not contained twice"
    );
    assert_eq!(saved(&t, 0, held).await, Some(vec![ROLE as i64]));
    t.cleanup().await;
}

/// D6: with the Quarantine role deleted in Discord, a member is still stripped (to the managed
/// roles Discord will not take off) and staff are told the role is missing.
#[tokio::test]
async fn a_deleted_quarantine_role_still_strips_the_member() {
    let Some(t) = T::new(151).await else { return };
    // 599 is not a role of the server any more.
    let an = json!({"enabled": true, "quarantine_role": "599"});
    server(&t, 0, json!({ "antinuke": an })).await;
    let u = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![ROLE, BOOST]);
    });
    let r = crate::antinuke::quarantine_actor(&t.app, t.g(0), Id::new(u), "test").await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(roles(&t, 0, u), Some(vec![BOOST]));
    assert_eq!(saved(&t, 0, u).await, Some(vec![ROLE as i64, BOOST as i64]));
    assert!(t.calls("Quarantine role missing") >= 1, "staff are told");
    t.cleanup().await;
}

/// D2: a Raider List quarantine of a joiner with no roles is a real quarantine: passing web
/// verification does not let them out.
#[tokio::test]
async fn a_raider_list_quarantine_of_a_roleless_joiner_cannot_be_verified_out() {
    let Some(t) = T::new(152).await else { return };
    let cfg = json!({
        "antinuke": {"quarantine_role": Q.to_string()},
        "raider_list": {"enabled": true, "action": "quarantine"},
        "joingate": {"enabled": true},
        "verification": web_verification(),
    });
    server(&t, 0, cfg).await;
    let u = t.base * 10 + 1;
    sqlx::query(
        "INSERT INTO raiders (user_id, status, category, reviewed_at, verified)
         VALUES ($1, 'confirmed', 'raid', now(), true)",
    )
    .bind(u as i64)
    .execute(&t.app.pool)
    .await
    .unwrap();
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![]);
    });
    let m: MemberAdd = serde_json::from_value(json!({
        "guild_id": t.g(0).to_string(), "roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00",
        "deaf": false, "mute": false, "flags": 0,
        "user": {"id": u.to_string(), "username": "raider", "discriminator": "0", "avatar": null},
    }))
    .unwrap();
    crate::joingate::on_join(&t.app, &m, false).await.unwrap();
    assert_eq!(roles(&t, 0, u), Some(vec![Q]), "the list quarantined them");
    let r = crate::verify::web_grant(&t.app, t.g(0), Id::new(u), &[]).await;
    assert!(r.is_err(), "verifying must be refused");
    assert_eq!(roles(&t, 0, u), Some(vec![Q]), "still quarantined");
    assert!(saved(&t, 0, u).await.is_some());
    sqlx::query("DELETE FROM raiders WHERE user_id = $1")
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.cleanup().await;
}

/// D2: an autorole landing before verification's join quarantine does not turn it into a real one.
#[tokio::test]
async fn a_join_quarantine_with_an_autorole_still_lifts_on_verify() {
    let Some(t) = T::new(153).await else { return };
    let mut v = web_verification();
    v["quarantine_on_join"] = json!(true);
    let cfg = json!({"antinuke": {"quarantine_role": Q.to_string()}, "verification": v});
    server(&t, 0, cfg).await;
    let u = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![AUTOROLE]);
    });
    crate::verify::on_join(&t.app, t.g(0), Id::new(u), false, false, None, None).await;
    assert_eq!(roles(&t, 0, u), Some(vec![Q]), "join quarantine");
    let r = crate::verify::web_grant(&t.app, t.g(0), Id::new(u), &[]).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(roles(&t, 0, u), Some(vec![VERIFIED]));
    assert_eq!(saved(&t, 0, u).await, None, "the join quarantine is over");
    t.cleanup().await;
}

/// Audit 11-17: "quarantine on join" alone does not replace the roles of a member who is in no
/// join quarantine; only "replace roles" does.
#[tokio::test]
async fn quarantine_on_join_keeps_the_roles_of_a_member_not_held() {
    let Some(t) = T::new(985).await else { return };
    let mut v = web_verification();
    v["quarantine_on_join"] = json!(true);
    let cfg = json!({"antinuke": {"quarantine_role": Q.to_string()}, "verification": v});
    server(&t, 0, cfg).await;
    let u = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![AUTOROLE]);
    });
    let r = crate::verify::web_grant(&t.app, t.g(0), Id::new(u), &[]).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(roles(&t, 0, u), Some(vec![AUTOROLE, VERIFIED]));
    t.cleanup().await;
}

/// Audit 11-17: a Trusted Admin who joins is immune, so verification does not quarantine them.
#[tokio::test]
async fn quarantine_on_join_skips_a_trusted_admin() {
    let Some(t) = T::new(986).await else { return };
    let mut v = web_verification();
    v["quarantine_on_join"] = json!(true);
    let cfg = json!({"antinuke": {"quarantine_role": Q.to_string()}, "verification": v});
    server(&t, 0, cfg).await;
    let u = t.base * 10 + 1;
    t.exec("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $1 * 10 + 1)")
        .await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![AUTOROLE]);
    });
    // `joingate::on_join` reads the TA's level from the join's roles and passes it.
    let must =
        crate::verify::on_join(&t.app, t.g(0), Id::new(u), false, false, None, Some(true)).await;
    assert!(!must);
    assert_eq!(roles(&t, 0, u), Some(vec![AUTOROLE]), "not quarantined");
    t.cleanup().await;
}

/// `purge N @member` protects a permit holder as ban does (owner decision 11-17).
#[tokio::test]
async fn purge_of_a_permit_holder_is_refused() {
    let Some(t) = T::new(987).await else { return };
    t.server_config(0, t.owner(), json!({})).await;
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1, $1, 'Staff', '{}', '{}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1, $1, 556)")
        .await;
    t.exec(
        "INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $1, $1 * 10 + 5)",
    )
    .await;
    let staff = t.base * 10 + 5;
    t.set(|m| {
        m.members.insert((t.g(0).get(), staff), vec![556]);
    });
    let me = brim_core::permits::Level::Custom {
        rank: 1,
        perms: vec!["purge".into()],
        whitelist: vec![],
        limits: Default::default(),
    };
    let cmd = crate::modcmds::ModCmd::Purge {
        count: 10,
        user: Some(staff),
        from: None,
        filters: vec![],
        own: vec![],
        before: None,
    };
    let owner = Id::new(t.owner());
    let r = crate::modcmds::run(
        &t.app,
        t.g(0),
        owner,
        (Id::new(9), &me),
        Id::new(LOGS),
        cmd,
        (None, None),
        false,
    );
    let text = r.await.unwrap().text;
    assert!(text.ends_with("they hold a permit"), "{text}");
    assert_eq!(t.calls("/messages"), 0, "nothing read or deleted");
    t.cleanup().await;
}

/// N1: turning "quarantine on join" off while someone waits does not strand them in quarantine.
#[tokio::test]
async fn turning_quarantine_on_join_off_does_not_strand_a_verifying_member() {
    let Some(t) = T::new(154).await else { return };
    let mut v = web_verification();
    v["quarantine_on_join"] = json!(true);
    let cfg = json!({"antinuke": {"quarantine_role": Q.to_string()}, "verification": v.clone()});
    server(&t, 0, cfg.clone()).await;
    let u = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![]);
    });
    crate::verify::on_join(&t.app, t.g(0), Id::new(u), false, false, None, None).await;
    assert_eq!(roles(&t, 0, u), Some(vec![Q]));
    let mut off = cfg;
    off["verification"]["quarantine_on_join"] = json!(false);
    off["logging"] = logging();
    sqlx::query("INSERT INTO guild_config (guild_id, version, config) VALUES ($1, 2, $2)")
        .bind(t.g(0).get() as i64)
        .bind(off)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.app.forget_cfg(t.g(0).get());
    let r = crate::verify::web_grant(&t.app, t.g(0), Id::new(u), &[]).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(roles(&t, 0, u), Some(vec![VERIFIED]), "out of quarantine");
    assert_eq!(saved(&t, 0, u).await, None);
    t.cleanup().await;
}

/// D7: a bot on the Raider List loses its managed role's permissions, which a quarantine alone
/// leaves it.
#[tokio::test]
async fn a_raider_list_quarantine_of_a_bot_clears_its_role() {
    let Some(t) = T::new(155).await else { return };
    let cfg = json!({
        "antinuke": {"quarantine_role": Q.to_string()},
        "raider_list": {"enabled": true, "action": "quarantine"},
        "joingate": {"enabled": true},
    });
    server(&t, 0, cfg).await;
    let (u, own) = (t.base * 10 + 1, 530);
    sqlx::query(
        "INSERT INTO raiders (user_id, status, category, reviewed_at, verified)
         VALUES ($1, 'confirmed', 'raid', now(), true)",
    )
    .bind(u as i64)
    .execute(&t.app.pool)
    .await
    .unwrap();
    t.set(|m| {
        m.bots.push(u);
        m.bot_roles.insert(own, u);
        m.roles
            .get_mut(&t.g(0).get())
            .unwrap()
            .push((own, true, 8, 6));
        m.members.insert((t.g(0).get(), u), vec![own]);
    });
    let m: MemberAdd = serde_json::from_value(json!({
        "guild_id": t.g(0).to_string(), "roles": [own.to_string()],
        "joined_at": "2026-01-01T00:00:00.000000+00:00", "deaf": false, "mute": false, "flags": 0,
        "user": {"id": u.to_string(), "username": "raidbot", "discriminator": "0", "avatar": null, "bot": true},
    }))
    .unwrap();
    crate::joingate::on_join(&t.app, &m, false).await.unwrap();
    let perms = t.mock.lock().unwrap().roles[&t.g(0).get()]
        .iter()
        .find(|r| r.0 == own)
        .unwrap()
        .2;
    assert_eq!(perms, 0, "the bot's own role lost its permissions");
    sqlx::query("DELETE FROM raiders WHERE user_id = $1")
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.cleanup().await;
}

/// D12: a failed undo (the role is above the bot) does not spare the actor.
#[tokio::test]
async fn a_failed_undo_still_quarantines_the_actor() {
    let Some(t) = T::new(156).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let actor = t.base * 10 + 1;
    t.set(|m| {
        m.fail_role_edit = true;
        m.members.insert((t.g(0).get(), actor), vec![ROLE]);
        m.roles.get_mut(&t.g(0).get()).unwrap()[2].2 = 8; // ROLE, as the actor left it
    });
    let e: AuditLogEntry = serde_json::from_value(json!({
        "id": "1", "action_type": 31, "guild_id": t.g(0).to_string(), "target_id": ROLE.to_string(),
        "user_id": actor.to_string(), "reason": null,
        "changes": [{"key": "permissions", "old_value": "0", "new_value": "8"}],
    }))
    .unwrap();
    let r = crate::antinuke::on_audit(&t.app, &e).await;
    tracing::info!(?r, "on_audit");
    assert!(t.calls(&format!("PATCH /guilds/{}/roles/{ROLE}", t.g(0))) >= 1);
    assert_eq!(roles(&t, 0, actor), Some(vec![Q]), "quarantined anyway");
    t.cleanup().await;
}

/// D18: a managed role Discord will not take off does not keep enforce_permits from the rest.
#[tokio::test]
async fn enforce_permits_skips_managed_roles() {
    let Some(t) = T::new(157).await else { return };
    server(&t, 0, json!({"misc": {"enforce_permits": true}})).await;
    // The first permit sync ran: before it, enforcement waits (nobody is listed yet).
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (u, managed, plain) = (t.base * 10 + 1, 510, 511);
    t.set(|m| {
        let all = m.roles.get_mut(&t.g(0).get()).unwrap();
        all.push((managed, true, 8, 6));
        all.push((plain, false, 8, 7));
        m.members.insert((t.g(0).get(), u), vec![managed, plain]);
    });
    let m: MemberUpdate = serde_json::from_value(json!({
        "guild_id": t.g(0).to_string(), "roles": [managed.to_string(), plain.to_string()],
        "joined_at": "2026-01-01T00:00:00.000000+00:00", "deaf": false, "mute": false,
        "pending": false, "avatar": null, "communication_disabled_until": null, "nick": null,
        "premium_since": null,
        "user": {"id": u.to_string(), "username": "u", "discriminator": "0", "avatar": null},
    }))
    .unwrap();
    let r = crate::antinuke::enforce_permits(&t.app, &m).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(
        roles(&t, 0, u),
        Some(vec![managed]),
        "the plain role is gone"
    );
    t.cleanup().await;
}

/// D55: enforce_quarantine gives the verification channel the shape the join quarantine reads
/// (and repairs an older overwrite), also a channel the dashboard made and has not saved yet while
/// verification is on and the saved channel is gone; other channels still get the full deny.
#[tokio::test]
async fn enforce_quarantine_gives_the_verification_channel_its_shape() {
    use brim_core::sections::{QUARANTINE_DENY, VERIFY_ALLOW, VERIFY_DENY};
    let Some(t) = T::new(158).await else { return };
    let q = json!({"quarantine_role": Q.to_string()});
    let misc = json!({"enforce_quarantine_perms": true});
    let mut unsaved = web_verification();
    unsaved.as_object_mut().unwrap().remove("channel");
    let mut dead = web_verification();
    dead["channel"] = json!("799"); // deleted in Discord: GET answers 404
    let mut off = web_verification();
    off["enabled"] = json!(false);
    off.as_object_mut().unwrap().remove("channel");
    for (i, v) in [web_verification(), unsaved, dead, off]
        .into_iter()
        .enumerate()
    {
        server(
            &t,
            i as u64,
            json!({"antinuke": q, "misc": misc, "verification": v}),
        )
        .await;
    }
    let chan = |i: u64, id: u64, allow: u64, deny: u64| -> Channel {
        let ow = json!([{"id": Q.to_string(), "type": 0, "allow": allow.to_string(), "deny": deny.to_string()}]);
        serde_json::from_value(json!({"id": id.to_string(), "type": 0, "guild_id": t.g(i).to_string(),
                                      "permission_overwrites": if allow | deny == 0 { json!([]) } else { ow }}))
        .unwrap()
    };
    let shape = |i, id| chan(i, id, VERIFY_ALLOW, VERIFY_DENY);
    for (i, c) in [
        (0, chan(0, 700, VERIFY_ALLOW, 2048)), // the saved one, with D55's old overwrite
        (0, chan(0, 701, 0, 0)),               // another channel
        (1, shape(1, 702)),                    // made by the dashboard, not saved yet
        (1, chan(1, 703, VERIFY_ALLOW | 2048, 0)),
        (2, shape(2, 704)), // the saved channel was deleted
        (3, shape(3, 705)), // verification is off
    ] {
        let r = crate::setup::enforce_quarantine(&t.app, &c).await;
        assert!(r.is_ok(), "{i}: {r:?}");
    }
    let put = |c: u64, deny: Option<u64>| {
        let m = t.mock.lock().unwrap();
        let to = format!("PUT /channels/{c}/permissions/{Q} ");
        let body = deny.map(|d| format!("\"deny\":\"{d}\""));
        m.log
            .iter()
            .filter(|l| l.starts_with(&to) && body.as_ref().is_none_or(|b| l.contains(b)))
            .count()
    };
    assert_eq!(
        put(700, Some(VERIFY_DENY)),
        1,
        "the saved one is repaired to the shape"
    );
    assert_eq!(
        (put(702, None), put(704, None)),
        (0, 0),
        "a new verification channel keeps it"
    );
    for c in [701, 703, 705] {
        assert_eq!(put(c, Some(QUARANTINE_DENY)), 1, "{c} gets the deny");
    }
    t.cleanup().await;
}

/// Round 2, item 1: a failed member read counts as not contained, so the nuker gets the full run.
#[tokio::test]
async fn a_failed_member_read_does_not_count_as_contained() {
    let Some(t) = T::new(161).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string(),
                    "limits": {"Ban": {"per_minute": 1, "per_hour": 1}}});
    server(&t, 0, json!({ "antinuke": an })).await;
    let u = t.base * 10 + 1;
    row(&t, 0, u, &[ROLE as i64]).await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![Q]);
        m.fail_member = true;
    });
    let cfg = t.app.cfg(t.g(0).get()).await.unwrap();
    let ban = brim_antinuke::Action::Ban;
    let r = crate::antinuke::count(
        &t.app,
        &cfg,
        t.g(0),
        Id::new(u),
        ban,
        Some(9),
        &[],
        false,
        5,
    )
    .await;
    tracing::info!(?r, "count");
    // The member read also leaves the level unread: reported, but no quarantine case.
    assert!(said(&t, "Anti Nuke Report"), "the full run reported");
    t.cleanup().await;
}

/// Round 2, item 2: a saved list stays when the member holds no other role than quarantine
/// leaves (none at all, or a recreated Quarantine role they lack), and when the other roles come
/// from a membership that began after the quarantine (a rejoin with autoroles).
#[tokio::test]
async fn a_second_quarantine_keeps_a_valid_saved_list() {
    let Some(t) = T::new(963).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"quarantine_role": Q.to_string()}}),
    )
    .await;
    let (none, boost, rejoined) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
    for u in [none, boost, rejoined] {
        row(&t, 0, u, &[ROLE as i64]).await;
    }
    // The mock's members joined in 2020: this quarantine is older than the membership.
    sqlx::query(
        "UPDATE quarantines SET created_at = '2019-01-01' WHERE guild_id = $1 AND user_id = $2",
    )
    .bind(t.g(0).get() as i64)
    .bind(rejoined as i64)
    .execute(&t.app.pool)
    .await
    .unwrap();
    t.set(|m| {
        m.members.insert((t.g(0).get(), none), vec![]);
        m.members.insert((t.g(0).get(), boost), vec![BOOST]);
        m.members.insert((t.g(0).get(), rejoined), vec![AUTOROLE]);
    });
    for u in [none, boost, rejoined] {
        let r = crate::antinuke::quarantine_actor(&t.app, t.g(0), Id::new(u), "test").await;
        assert!(r.is_ok(), "{u}: {r:?}");
        assert_eq!(saved(&t, 0, u).await, Some(vec![ROLE as i64]), "{u}");
    }
    t.cleanup().await;
}

/// Round 2, item 5: with verification off, a rejoin ends a join quarantine instead of holding the
/// member where nobody can verify out.
#[tokio::test]
async fn a_join_quarantine_ends_on_rejoin_when_verification_is_off() {
    let Some(t) = T::new(964).await else { return };
    let mut v = web_verification();
    v["enabled"] = json!(false);
    server(
        &t,
        0,
        json!({"antinuke": {"quarantine_role": Q.to_string()}, "verification": v}),
    )
    .await;
    let u = t.base * 10 + 1;
    row(&t, 0, u, &[]).await;
    sqlx::query("UPDATE quarantines SET on_join = true WHERE guild_id = $1")
        .bind(t.g(0).get() as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![]);
    });
    let r = crate::antinuke::requarantine_rejoin(&t.app, t.g(0), Id::new(u)).await;
    assert_eq!(r.unwrap(), crate::antinuke::Rejoin::Free);
    assert_eq!(saved(&t, 0, u).await, None);
    assert_eq!(roles(&t, 0, u), Some(vec![]));
    t.cleanup().await;
}

/// N5: a report log the bot cannot post (the nuker hid the channel) still DMs the owner.
#[tokio::test]
async fn a_failed_report_log_still_alerts_the_owner() {
    let Some(t) = T::new(159).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let actor = t.base * 10 + 1;
    t.set(|m| {
        m.fail_channels.push(LOGS);
        m.members.insert((t.g(0).get(), actor), vec![ROLE]);
    });
    let since = crate::app::now_secs() - 60;
    let r = crate::antinuke::revert_and_report(
        &t.app,
        t.g(0),
        Id::new(actor),
        "test",
        since,
        (true, true),
        Ok(false),
    )
    .await;
    tracing::info!(?r, "revert_and_report");
    assert!(
        t.calls(&format!("POST /channels/{LOGS}/messages")) >= 1,
        "the log was tried"
    );
    assert!(
        t.calls("POST /channels/77/messages") >= 1,
        "the owner got the DM"
    );
    t.cleanup().await;
}

/// Migration 3900: only rows with nothing saved (the old join quarantine) become join quarantines,
/// unless a quarantine case or Raider List quarantine since the row shows a real one.
#[tokio::test]
async fn migration_3900_marks_only_empty_saved_rows_as_join() {
    let Ok(url) = brim_core::db::test_url() else {
        return;
    };
    let pool = brim_core::db::connect(&url).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    // The table as it was before 3900, in a schema of its own that the rollback drops.
    for sql in [
        "CREATE SCHEMA m3900",
        "SET LOCAL search_path = m3900",
        "CREATE TABLE quarantines (guild_id BIGINT, user_id BIGINT, saved_roles BIGINT[] NOT NULL,
                                   created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        "CREATE TABLE cases (guild_id BIGINT, user_id BIGINT, kind TEXT, created_at TIMESTAMPTZ)",
        "CREATE TABLE joingate_actions (guild_id BIGINT, user_id BIGINT, action TEXT, ok BOOLEAN,
                                        created_at TIMESTAMPTZ)",
        "INSERT INTO quarantines VALUES (1, 1, '{}'), (1, 2, '{5}'), (1, 3, '{5,6}'), (1, 5, '{}'),
                                        (1, 6, '{}'), (1, 7, '{}')",
        // 5: a staff quarantine case; 6: a Raider List quarantine; 7: only an old, released case.
        "INSERT INTO cases VALUES (1, 5, 'quarantine', now()), (1, 7, 'quarantine', now() - interval '1 day')",
        "INSERT INTO joingate_actions VALUES (1, 6, 'Quarantine', true, now())",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    let up = include_str!("../../core/migrations/3900_quarantine_join.sql");
    sqlx::raw_sql(up).execute(&mut *tx).await.unwrap();
    let rows: Vec<(i64, bool)> =
        sqlx::query_as("SELECT user_id, on_join FROM quarantines ORDER BY user_id")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, true),
            (2, false),
            (3, false),
            (5, false),
            (6, false),
            (7, true)
        ]
    );
    // New rows default to a real quarantine.
    sqlx::query("INSERT INTO quarantines VALUES (1, 4, '{}')")
        .execute(&mut *tx)
        .await
        .unwrap();
    let new: bool = sqlx::query_scalar("SELECT on_join FROM quarantines WHERE user_id = 4")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(!new);
    tx.rollback().await.unwrap();
}

/// D61: a member who left during verification's join quarantine and rejoins is verifying, not
/// held: their sticky roles wait for verification instead of being dropped.
#[tokio::test]
async fn a_join_quarantine_rejoin_keeps_sticky_roles_for_verification() {
    let Some(t) = T::new(160).await else { return };
    let mut v = web_verification();
    v["quarantine_on_join"] = json!(true);
    let cfg = json!({"antinuke": {"quarantine_role": Q.to_string()}, "verification": v,
                     "misc": {"sticky_roles": true}});
    server(&t, 0, cfg).await;
    let (g, u) = (t.g(0).get() as i64, t.base * 10 + 1);
    // Left while in the join quarantine; their roles from before are saved.
    for sql in [
        "INSERT INTO quarantines (guild_id, user_id, saved_roles, on_join) VALUES ($1, $2, '{}', true)",
        "INSERT INTO member_roles (guild_id, user_id, roles, joined_at, left_at)
         VALUES ($1, $2, '{501}', '2025-01-01T00:00:00Z', now())",
    ] {
        sqlx::query(sql)
            .bind(g)
            .bind(u as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
    }
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![]);
    });
    let m: MemberAdd = serde_json::from_value(json!({
        "guild_id": t.g(0).to_string(), "roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00",
        "deaf": false, "mute": false, "flags": 0,
        "user": {"id": u.to_string(), "username": "back", "discriminator": "0", "avatar": null},
    }))
    .unwrap();
    crate::joingate::on_join(&t.app, &m, false).await.unwrap();
    assert_eq!(
        roles(&t, 0, u),
        Some(vec![Q]),
        "back in the join quarantine"
    );
    let r = crate::verify::web_grant(&t.app, t.g(0), Id::new(u), &[]).await;
    assert!(r.is_ok(), "{r:?}");
    let mut got = roles(&t, 0, u).unwrap();
    got.sort();
    assert_eq!(
        got,
        vec![ROLE, VERIFIED],
        "sticky roles came back after verifying"
    );
    sqlx::query("DELETE FROM member_roles WHERE guild_id = $1")
        .bind(g)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.cleanup().await;
}

/// D61: a join raid hit that keeps the member drops their saved sticky roles, as on main, so a
/// raider cannot get them back by verifying or rejoining; nothing is given back.
#[tokio::test]
async fn a_logged_raider_loses_saved_sticky_roles() {
    let Some(t) = T::new(84).await else { return };
    let raid = json!({"enabled": true, "count": 1, "span_ms": 60000, "suspicious_only": false,
                      "active_ms": 60000, "string_patterns": [], "flag_younger_than_ms": null,
                      "flag_no_avatar": true, "flag_spammer": false, "action": "Log"});
    let cfg = json!({"antinuke": {"quarantine_role": Q.to_string()}, "joinraid": raid,
                     "misc": {"sticky_roles": true}});
    server(&t, 0, cfg).await;
    let (g, u) = (t.g(0).get() as i64, t.base * 10 + 1);
    sqlx::query(
        "INSERT INTO member_roles (guild_id, user_id, roles, joined_at, left_at)
         VALUES ($1, $2, '{501}', '2025-01-01T00:00:00Z', now())",
    )
    .bind(g)
    .bind(u as i64)
    .execute(&t.app.pool)
    .await
    .unwrap();
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![]);
    });
    let m: MemberAdd = serde_json::from_value(json!({
        "guild_id": t.g(0).to_string(), "roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00",
        "deaf": false, "mute": false, "flags": 0,
        "user": {"id": u.to_string(), "username": "raider", "discriminator": "0", "avatar": null},
    }))
    .unwrap();
    crate::joingate::on_join(&t.app, &m, false).await.unwrap();
    let flagged: i64 = sqlx::query_scalar("SELECT count(*) FROM raid_log WHERE guild_id = $1")
        .bind(g)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
    assert_eq!(flagged, 1, "the join raid flagged them");
    assert_eq!(roles(&t, 0, u), Some(vec![]), "nothing given back");
    let kept: (Vec<i64>, bool) = sqlx::query_as(
        "SELECT roles, pending FROM member_roles WHERE guild_id = $1 AND user_id = $2",
    )
    .bind(g)
    .bind(u as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(kept, (vec![], false), "the saved list is dropped");
    for sql in [
        "DELETE FROM member_roles WHERE guild_id = $1",
        "DELETE FROM raid_log WHERE guild_id = $1",
    ] {
        sqlx::query(sql).bind(g).execute(&t.app.pool).await.unwrap();
    }
    t.cleanup().await;
}

/// Web verification and staff /verify work during a timeout: the pending sticky roles come back,
/// but the main roles wait for the timeout's end when the timeout takes them.
#[tokio::test]
async fn verifying_during_a_timeout_does_not_give_back_main_roles() {
    let Some(t) = T::new(85).await else { return };
    let cfg = json!({"misc": {"sticky_roles": true, "remove_main_roles_on_timeout": true},
                     "lockdown": {"main_roles": [ROLE.to_string()]}});
    server(&t, 0, cfg).await;
    let (g, u) = (t.g(0).get() as i64, t.base * 10 + 1);
    sqlx::query(
        "INSERT INTO member_roles (guild_id, user_id, roles, joined_at, pending)
         VALUES ($1, $2, '{501,504}', '2025-01-01T00:00:00Z', true)",
    )
    .bind(g)
    .bind(u as i64)
    .execute(&t.app.pool)
    .await
    .unwrap();
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![VERIFIED]);
        m.timeouts
            .insert((t.g(0).get(), u), "2099-01-01T00:00:00.000000+00:00".into());
    });
    crate::sticky::restore_pending(&t.app, t.g(0), Id::new(u))
        .await
        .unwrap();
    let mut got = roles(&t, 0, u).unwrap();
    got.sort();
    assert_eq!(
        got,
        vec![VERIFIED, AUTOROLE],
        "the main role waits for the timeout"
    );
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2")
            .bind(g)
            .bind(crate::timeout_roles::KIND)
            .fetch_one(&t.app.pool)
            .await
            .unwrap();
    assert_eq!(queued, 1, "queued for the timeout's end");
    sqlx::query("DELETE FROM member_roles WHERE guild_id = $1")
        .bind(g)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.cleanup().await;
}

/// D25: `verify` ends only verification's join quarantine (other quarantines need the quarantine
/// permit), and saved roles with dangerous permissions (Administrator) need an owner or TA.
#[tokio::test]
async fn verify_lifts_only_a_join_quarantine_and_admin_roles_need_staff() {
    let Some(t) = T::new(163).await else { return };
    let cfg =
        json!({"antinuke": {"quarantine_role": Q.to_string()}, "verification": web_verification()});
    server(&t, 0, cfg).await;
    const ADMIN: u64 = 505;
    let (real, admin) = (t.base * 10 + 1, t.base * 10 + 2);
    t.set(|m| {
        let g = t.g(0).get();
        m.roles.get_mut(&g).unwrap().push((ADMIN, false, 8, 6));
        m.members.insert((g, real), vec![Q]);
        m.members.insert((g, admin), vec![Q]);
    });
    row(&t, 0, real, &[ROLE as i64]).await;
    row(&t, 0, admin, &[ADMIN as i64]).await;
    let permit = |perms: &[&str]| brim_core::permits::Level::Custom {
        rank: 1,
        perms: perms.iter().map(|p| p.to_string()).collect(),
        whitelist: vec![],
        limits: Default::default(),
    };
    let run = |level: brim_core::permits::Level, cmd| {
        let t = &t;
        async move {
            let me = (Id::new(9), &level);
            let owner = Id::new(t.owner());
            // Boxed: `run` is a large future, and this test awaits it many times; on the test
            // thread's 2 MB stack in a debug build the inlined copies overflowed it.
            let r = Box::pin(crate::modcmds::run(
                &t.app,
                t.g(0),
                owner,
                me,
                Id::new(LOGS),
                cmd,
                (None, None),
                false,
            ));
            r.await.unwrap().text
        }
    };
    use crate::modcmds::ModCmd::{Unquarantine, Verify};
    let text = run(permit(&["verify"]), Verify { user: real }).await;
    assert!(text.starts_with("Cannot verify"), "{text}");
    assert_eq!(roles(&t, 0, real), Some(vec![Q]), "still quarantined");
    let text = run(permit(&["quarantine"]), Unquarantine { user: admin }).await;
    assert!(text.starts_with("Cannot release"), "{text}");
    assert_eq!(saved(&t, 0, admin).await, Some(vec![ADMIN as i64]));
    // During a role lock the Administrator role reads 0 live; the lock's saved value counts.
    const LOCKED: u64 = 506;
    let stripped = t.base * 10 + 3;
    t.set(|m| {
        let g = t.g(0).get();
        m.roles.get_mut(&g).unwrap().push((LOCKED, false, 0, 7));
        m.members.insert((g, stripped), vec![Q]);
    });
    t.app.invalidate_guild(t.g(0));
    row(&t, 0, stripped, &[LOCKED as i64]).await;
    let lock = brim_automod::lockdown::Snapshot {
        kind: Some(brim_automod::lockdown::LockKind::Role),
        role_perms: vec![(LOCKED, 8)],
        ..Default::default()
    };
    brim_automod::store::save_lock(&t.app.pool, t.g(0).get(), &lock)
        .await
        .unwrap();
    crate::lockdown::forget_locks(t.g(0).get());
    let text = run(permit(&["quarantine"]), Unquarantine { user: stripped }).await;
    sqlx::query("DELETE FROM lock_snapshots WHERE guild_id = $1")
        .bind(t.g(0).get() as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    crate::lockdown::forget_locks(t.g(0).get());
    assert!(text.starts_with("Cannot release"), "role lock: {text}");
    // With the quarantine permit, verify may end it.
    let text = run(permit(&["verify", "quarantine"]), Verify { user: real }).await;
    assert_eq!(saved(&t, 0, real).await, None, "{text}");
    t.cleanup().await;
}

/// An audit entry: `actor` added `role` to `target`.
fn role_added(t: &T, actor: u64, target: u64, role: u64) -> AuditLogEntry {
    serde_json::from_value(json!({
        "id": "2", "action_type": 25, "guild_id": t.g(0).to_string(), "target_id": target.to_string(),
        "user_id": actor.to_string(), "reason": null,
        "changes": [{"key": "$add", "new_value": [{"id": role.to_string(), "name": "r"}]}],
    }))
    .unwrap()
}

fn joined(t: &T, u: u64) -> MemberAdd {
    serde_json::from_value(json!({
        "guild_id": t.g(0).to_string(), "roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00",
        "deaf": false, "mute": false, "flags": 0,
        "user": {"id": u.to_string(), "username": "back", "discriminator": "0", "avatar": null},
    }))
    .unwrap()
}

/// D15: a quarantined member rejoins and an autorole bot gives them a role: the role goes, the
/// bot is not quarantined. A human doing the same still is.
#[tokio::test]
async fn an_autorole_bot_after_a_rejoin_quarantine_is_undone_not_punished() {
    let Some(t) = T::new(165).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (u, bot, human) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
    row(&t, 0, u, &[ROLE as i64]).await;
    t.set(|m| {
        m.bots.push(bot);
        m.members.insert((t.g(0).get(), u), vec![]);
    });
    crate::joingate::on_join(&t.app, &joined(&t, u), false)
        .await
        .unwrap();
    assert_eq!(roles(&t, 0, u), Some(vec![Q]), "quarantined again");
    for actor in [bot, human] {
        t.set(|m| {
            m.members.insert((t.g(0).get(), u), vec![Q, AUTOROLE]);
            m.members.insert((t.g(0).get(), actor), vec![ROLE]);
        });
        let e = role_added(&t, actor, u, AUTOROLE);
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        assert_eq!(roles(&t, 0, u), Some(vec![Q]), "the role is taken back");
    }
    assert_eq!(
        saved(&t, 0, bot).await,
        None,
        "the autorole bot was punished"
    );
    assert!(
        saved(&t, 0, human).await.is_some(),
        "a human is still held to the rule"
    );
    t.cleanup().await;
}

/// D15 review: the grace covers any number of bot changes in the window, but only plain roles.
/// An Administrator role or a permit role quarantines the bot as before.
#[tokio::test]
async fn the_rejoin_grace_covers_plain_roles_only() {
    let Some(t) = T::new(162).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    const ADMIN: u64 = 507;
    const PERMIT: u64 = 511;
    let [u, v, w, admin_bot, auto_bot, level_bot, permit_bot, unknown_bot] =
        [1, 2, 3, 4, 5, 6, 7, 8].map(|n| t.base * 10 + n);
    let bots = [admin_bot, auto_bot, level_bot, permit_bot, unknown_bot];
    t.set(|m| {
        let g = t.g(0).get();
        m.roles
            .get_mut(&g)
            .unwrap()
            .extend([(ADMIN, false, 8, 6), (PERMIT, false, 0, 7)]);
        m.bots.extend(bots);
        for x in [u, v, w].into_iter().chain(bots) {
            m.members.insert((g, x), vec![]);
        }
    });
    t.exec(
        "INSERT INTO permits (id, guild_id, name, perms) VALUES ($1 + 1, $1, 'Helpers', '{warn}')",
    )
    .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 511)")
        .await;
    for x in [u, v, w] {
        row(&t, 0, x, &[ROLE as i64]).await;
        crate::joingate::on_join(&t.app, &joined(&t, x), false)
            .await
            .unwrap();
    }
    let give = |actor: u64, target: u64, role: u64| {
        let t = &t;
        async move {
            t.set(|m| {
                m.members.insert((t.g(0).get(), target), vec![Q, role]);
            });
            let e = role_added(t, actor, target, role);
            crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        }
    };
    give(admin_bot, u, ADMIN).await;
    assert!(
        saved(&t, 0, admin_bot).await.is_some(),
        "Administrator is no autorole"
    );
    give(auto_bot, v, AUTOROLE).await;
    give(level_bot, v, AUTOROLE).await;
    give(auto_bot, v, AUTOROLE).await;
    for b in [auto_bot, level_bot] {
        assert_eq!(
            saved(&t, 0, b).await,
            None,
            "plain roles are graced, however many"
        );
    }
    give(permit_bot, w, PERMIT).await;
    assert!(
        saved(&t, 0, permit_bot).await.is_some(),
        "a permit role is no autorole"
    );
    // A role Discord does not list (created a moment ago, or the read failed) is not plain.
    give(unknown_bot, v, 599).await;
    assert!(
        saved(&t, 0, unknown_bot).await.is_some(),
        "an unknown role is no autorole"
    );
    t.cleanup().await;
}

/// D12b: an undo Discord refuses on a join-quarantined member punishes nobody, is tried once
/// more and then shown to staff.
#[tokio::test]
async fn a_failed_undo_on_a_join_quarantine_is_retried_and_logged() {
    let Some(t) = T::new(166).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (u, actor) = (t.base * 10 + 1, t.base * 10 + 2);
    sqlx::query("INSERT INTO quarantines (guild_id, user_id, saved_roles, on_join) VALUES ($1, $2, '{}', true)")
        .bind(t.g(0).get() as i64)
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.fail_member_edit = true;
        m.members.insert((t.g(0).get(), u), vec![Q, AUTOROLE]);
        m.members.insert((t.g(0).get(), actor), vec![]);
    });
    let e = role_added(&t, actor, u, AUTOROLE);
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    let patch = format!("PATCH /guilds/{}/members/{u}", t.g(0));
    assert_eq!(t.calls(&patch), 2, "tried twice");
    assert_eq!(t.calls("Could not take back roles"), 1, "staff were told");
    assert_eq!(saved(&t, 0, actor).await, None, "nobody is punished");
    t.cleanup().await;
}

/// D17: at startup a member with a quarantine row who joined after their last quarantine (a rejoin
/// while the bot was down) is quarantined again; one in the same membership is left as staff left
/// them, and one who is gone is skipped.
#[tokio::test]
async fn a_rejoin_while_the_bot_was_down_is_quarantined_at_startup() {
    let Some(t) = T::new(167).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"quarantine_role": Q.to_string()}}),
    )
    .await;
    let [back, freed, held_live, gone] = [1, 2, 3, 4].map(|n| t.base * 10 + n);
    for u in [back, freed, held_live, gone] {
        row(&t, 0, u, &[ROLE as i64]).await;
    }
    // The mock's members joined in 2020: before these rows, after a 2019 quarantine.
    let g = t.g(0).get() as i64;
    sqlx::query(
        "UPDATE quarantines SET created_at = '2019-01-01' WHERE guild_id = $1 AND user_id <> $2",
    )
    .bind(g)
    .bind(freed as i64)
    .execute(&t.app.pool)
    .await
    .unwrap();
    // Quarantined again live on its rejoin: the bot's own record is newer than the join.
    sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id) VALUES ($1, $2, 'quarantine', $3)")
        .bind(g)
        .bind(ME as i64)
        .bind(held_live as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        for u in [back, freed, held_live] {
            m.members.insert((t.g(0).get(), u), vec![ROLE]);
        }
    });
    crate::joingate::requarantine_missed(&t.app, t.g(0), &[], false, None)
        .await
        .unwrap();
    assert_eq!(roles(&t, 0, back), Some(vec![Q]), "rejoined while down");
    assert_eq!(roles(&t, 0, freed), Some(vec![ROLE]), "released by hand");
    assert_eq!(roles(&t, 0, held_live), Some(vec![ROLE]), "same membership");
    assert_eq!(
        saved(&t, 0, back).await,
        Some(vec![ROLE as i64]),
        "saved list kept"
    );
    // A second run (a restart's job next to GuildCreate) quarantines and logs nobody again.
    let logged = t.calls("Rejoined the server while quarantined");
    let edits = t.calls(&format!("PATCH /guilds/{}/members/{back}", t.g(0)));
    crate::joingate::requarantine_missed(&t.app, t.g(0), &[], false, None)
        .await
        .unwrap();
    assert_eq!(t.calls("Rejoined the server while quarantined"), logged);
    assert_eq!(
        t.calls(&format!("PATCH /guilds/{}/members/{back}", t.g(0))),
        edits
    );
    // During a raid the check waits in one job instead of reading members, at most 30 minutes.
    let raid = brim_automod::join::RaidDetector::restored(0, i64::MAX, String::new());
    t.app.raids.lock().unwrap().insert(t.g(0).get(), raid);
    let reads = t.calls(&format!("GET /guilds/{}/members", t.g(0)));
    for _ in 0..2 {
        crate::joingate::requarantine_missed(&t.app, t.g(0), &[], false, None)
            .await
            .unwrap();
    }
    assert_eq!(t.calls(&format!("GET /guilds/{}/members", t.g(0))), reads);
    // GuildCreate (no `first`) takes the first wait from the pending job, so the cap holds.
    sqlx::query("UPDATE scheduled_jobs SET payload = jsonb_build_object('first', $2::bigint) WHERE guild_id = $1 AND kind = 'requarantine_missed'")
        .bind(t.g(0).get() as i64)
        .bind(crate::app::now_secs() - 1801)
        .execute(&t.app.pool)
        .await
        .unwrap();
    crate::joingate::requarantine_missed(&t.app, t.g(0), &[], false, None)
        .await
        .unwrap();
    t.app.raids.lock().unwrap().clear();
    assert!(
        t.calls(&format!("GET /guilds/{}/members", t.g(0))) > reads,
        "ran anyway"
    );
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'requarantine_missed'",
    )
    .bind(t.g(0).get() as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
    t.cleanup().await;
}

/// D58: with Brim's verification on, a verification role another bot gives an unverified
/// member is taken back; nobody is punished, and a human giving it is left alone. With
/// "quarantine on join" the hold already did this (#170); this covers verification without it.
#[tokio::test]
async fn a_verification_role_from_another_bot_is_taken_back() {
    let Some(t) = T::new(168).await else { return };
    server(&t, 0, json!({"verification": web_verification()})).await;
    const MOD: u64 = 516;
    let [u, bot, human, staff] = [1, 2, 3, 4].map(|n| t.base * 10 + n);
    t.set(|m| {
        m.bots.push(bot);
        m.roles
            .get_mut(&t.g(0).get())
            .unwrap()
            .push((MOD, false, 0, 6));
        m.members.insert((t.g(0).get(), human), vec![]);
        m.members.insert((t.g(0).get(), staff), vec![MOD]);
    });
    t.exec(
        "INSERT INTO permits (id, guild_id, name, perms) VALUES ($1 + 1, $1, 'Mods', '{verify}')",
    )
    .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 516)")
        .await;
    t.exec("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1 + 1, $1, $1 * 10 + 4)")
        .await;
    // Another bot, and a human with only Discord's Manage Roles, lose it; a verify permit or the
    // owner keeps it (owner decision, 13:53).
    let owner = t.owner();
    for (actor, kept) in [
        (bot, vec![]),
        (human, vec![]),
        (staff, vec![VERIFIED]),
        (owner, vec![VERIFIED]),
    ] {
        t.set(|m| {
            m.members.insert((t.g(0).get(), u), vec![VERIFIED]);
        });
        let e = role_added(&t, actor, u, VERIFIED);
        crate::verify::given::on_audit(&t.app, &e).await.unwrap();
        assert_eq!(roles(&t, 0, u), Some(kept), "actor {actor}");
    }
    // The permit lookup fails (here the server read): the rule fails closed and staff are told.
    t.set(|m| {
        m.owners.remove(&t.g(0).get());
        m.members.insert((t.g(0).get(), u), vec![VERIFIED]);
    });
    t.app.invalidate_guild(t.g(0));
    crate::verify::given::on_audit(&t.app, &role_added(&t, staff, u, VERIFIED))
        .await
        .unwrap();
    assert_eq!(roles(&t, 0, u), Some(vec![]));
    assert_eq!(t.calls("could not be read"), 1);
    for x in [bot, human] {
        assert_eq!(saved(&t, 0, x).await, None, "nobody is punished");
    }
    assert_eq!(t.calls("given by another bot, was taken back"), 1);
    assert_eq!(t.calls("given without a verify permit, was taken back"), 2);
    t.cleanup().await;
}

/// D27: an automatic quarantine queues one DM telling the member how to reach staff, while tickets
/// are on; none during a raid, none for a bot, and the job skips a join quarantine.
#[tokio::test]
async fn an_automatic_quarantine_tells_the_member_how_to_reach_staff() {
    let Some(t) = T::new(169).await else { return };
    let cfg = json!({"antinuke": {"quarantine_role": Q.to_string()}, "tickets": {"enabled": true}});
    server(&t, 0, cfg).await;
    let [member, bot, raider] = [1, 2, 3].map(|n| t.base * 10 + n);
    t.set(|m| {
        m.bots.push(bot);
        for u in [member, bot, raider] {
            m.members.insert((t.g(0).get(), u), vec![ROLE]);
        }
    });
    let jobs = || async {
        sqlx::query_as::<_, (Value,)>(
            "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'quarantine_dm' ORDER BY id",
        )
        .bind(t.g(0).get() as i64)
        .fetch_all(&t.app.pool)
        .await
        .unwrap()
    };
    let why = "command rate limit exceeded";
    for u in [member, bot] {
        crate::antinuke::quarantine_actor(&t.app, t.g(0), Id::new(u), why)
            .await
            .unwrap();
    }
    // A raid wave queues no DMs.
    let raid = brim_automod::join::RaidDetector::restored(0, i64::MAX, String::new());
    t.app.raids.lock().unwrap().insert(t.g(0).get(), raid);
    crate::antinuke::quarantine_actor(&t.app, t.g(0), Id::new(raider), why)
        .await
        .unwrap();
    t.app.raids.lock().unwrap().clear();
    let queued = jobs().await;
    assert_eq!(queued.len(), 2, "{queued:?}");
    for (p,) in &queued {
        crate::reply::quarantine_dm_job(&t.app, t.g(0), p)
            .await
            .unwrap();
    }
    assert_eq!(
        t.calls("You can DM Brim to reach this server"),
        1,
        "the member, not the bot"
    );
    // A second quarantine within 24 h queues no second DM.
    crate::antinuke::quarantine_actor(&t.app, t.g(0), Id::new(member), why)
        .await
        .unwrap();
    assert_eq!(jobs().await.len(), 2, "one DM a day");
    // A join quarantine gets the verification prompt instead.
    sqlx::query("UPDATE quarantines SET on_join = true WHERE guild_id = $1 AND user_id = $2")
        .bind(t.g(0).get() as i64)
        .bind(member as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    crate::reply::quarantine_dm_job(&t.app, t.g(0), &json!({"user": member}))
        .await
        .unwrap();
    assert_eq!(t.calls("You can DM Brim to reach this server"), 1);
    t.cleanup().await;
}

/// D58 review: a role opening the verified channels counts as a verification role; two grants in
/// parallel are both taken back; a removal Discord refuses is tried again and then told to staff;
/// and a Quarantine Whitelist bot's grant to a join-quarantined member is taken back too.
#[tokio::test]
async fn other_bot_grants_that_skip_verification_are_all_taken_back() {
    let Some(t) = T::new(164).await else { return };
    const EXTRA: u64 = 508;
    const VERIFIED2: u64 = 509;
    const STUCK: u64 = 510;
    let mut v = web_verification();
    v["roles"] = json!([
        VERIFIED.to_string(),
        VERIFIED2.to_string(),
        STUCK.to_string()
    ]);
    let [u, w, x, y, bot] = [1, 2, 3, 4, 5].map(|n| t.base * 10 + n);
    let cfg = json!({"verification": v, "antinuke": {"enabled": true, "quarantine_role": Q.to_string(),
                     "quarantine_whitelist": [bot.to_string()]}});
    server(&t, 0, cfg).await;
    t.set(|m| {
        let g = t.g(0).get();
        let r = m.roles.get_mut(&g).unwrap();
        r.extend([
            (EXTRA, false, 0, 6),
            (VERIFIED2, false, 0, 7),
            (STUCK, true, 0, 8),
        ]);
        m.bots.push(bot);
        let ow =
            |role: u64| json!({"id": role.to_string(), "type": 0, "allow": "1024", "deny": "0"});
        m.channels.insert(
            g,
            json!([{"id": "701", "type": 0, "guild_id": g.to_string(), "name": "general",
            "position": 0, "permission_overwrites": [ow(VERIFIED), ow(EXTRA)]}]),
        );
        m.members.insert((g, u), vec![EXTRA]);
        m.members.insert((g, w), vec![VERIFIED, VERIFIED2]);
        m.members.insert((g, x), vec![STUCK]);
        m.members.insert((g, y), vec![Q, VERIFIED]);
    });
    let give = |target: u64, role: u64| role_added(&t, bot, target, role);
    // A plain role that opens the verified channel.
    crate::verify::given::on_audit(&t.app, &give(u, EXTRA))
        .await
        .unwrap();
    assert_eq!(roles(&t, 0, u), Some(vec![]));
    // Two verification roles in two calls at once: neither proves the other.
    let (a, b) = (give(w, VERIFIED), give(w, VERIFIED2));
    let (ra, rb) = tokio::join!(
        crate::verify::given::on_audit(&t.app, &a),
        crate::verify::given::on_audit(&t.app, &b)
    );
    ra.unwrap();
    rb.unwrap();
    assert_eq!(roles(&t, 0, w), Some(vec![]));
    // Discord refuses the removal (a managed role): tried twice, then staff hear of it.
    crate::verify::given::on_audit(&t.app, &give(x, STUCK))
        .await
        .unwrap();
    let del = format!("DELETE /guilds/{}/members/{x}/roles/{STUCK}", t.g(0));
    assert_eq!(t.calls(&del), 2);
    assert_eq!(t.calls("Could not take back a verification role"), 1);
    // The hold lets a Quarantine Whitelist bot through; this check does not.
    row(&t, 0, y, &[]).await;
    crate::verify::given::on_audit(&t.app, &give(y, VERIFIED))
        .await
        .unwrap();
    assert_eq!(roles(&t, 0, y), Some(vec![Q]));
    t.cleanup().await;
}

/// D58 round 3: a role opening the verified channels by base permission (the usual setup with
/// no overwrites) or Send Messages where @everyone only reads is taken back, and so is a bot's
/// member overwrite; a role opening nothing costs no member read. Grants made while Brim was
/// down are found at GuildCreate. With Quarantine Hold off, a held member is judged here.
#[tokio::test]
async fn bot_access_that_skips_verification_is_taken_back_by_base_overwrite_and_at_startup() {
    let Some(t) = T::new(174).await else { return };
    t.exec("INSERT INTO permits (id, guild_id, name, perms) VALUES ($1 + 3, $1 + 2, 'Mods', '{verify}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 3, $1 + 2, 516)")
        .await;
    t.exec("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1 + 3, $1 + 2, $1 * 100 + 12)")
        .await;
    const MEMBER: u64 = 512;
    const PINGS: u64 = 513;
    const TALK: u64 = 514;
    const SENDS: u64 = 515;
    let (view, send) = (1024u64, 2048u64);
    let [u, p, vfd, s1, s2, s3, a, b, c, z, bot, human, s4, d, e, nobody, s5, s6] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ]
    .map(|n| t.base * 100 + n);
    let an = json!({"enabled": true, "quarantine_role": Q.to_string(), "quarantine_hold": false});
    for i in 0..3 {
        server(
            &t,
            i,
            json!({"verification": web_verification(), "antinuke": an}),
        )
        .await;
    }
    let chan = |g: u64, id: &str, ows: Vec<Value>| {
        json!([{"id": id, "type": 0, "guild_id": g.to_string(), "name": "c", "position": 0,
                "permission_overwrites": ows}])
    };
    let ow = |id: u64, allow: u64, deny: u64| json!({"id": id.to_string(), "type": 0, "allow": allow.to_string(), "deny": deny.to_string()});
    t.set(|m| {
        m.bots.push(bot);
        // Server 0: @everyone has no View; the verified role has it in its base permissions.
        let g = t.g(0).get();
        let r = m.roles.get_mut(&g).unwrap();
        r.retain(|x| x.0 != VERIFIED);
        r.extend([(VERIFIED, false, view, 4), (MEMBER, false, view, 6), (PINGS, false, 0, 7)]);
        m.channels.insert(g, chan(g, "711", vec![]));
        for x in [u, p] {
            m.members.insert((g, x), vec![]);
        }
        m.members.insert((g, vfd), vec![VERIFIED]);
        // Server 1: @everyone reads but cannot write; verified members write.
        let g = t.g(1).get();
        let r = m.roles.get_mut(&g).unwrap();
        r.retain(|x| x.0 != g);
        r.extend([(g, false, view | send, 0), (TALK, false, 0, 6), (SENDS, false, send, 7)]);
        m.channels.insert(g, chan(g, "712", vec![ow(g, 0, send), ow(VERIFIED, send, 0), ow(TALK, send, 0)]));
        for x in [s1, s2, s3] {
            m.members.insert((g, x), vec![]);
        }
        // Server 2: grants made while Brim was down, newest first.
        let g = t.g(2).get();
        for x in [a, b, c] {
            m.members.insert((g, x), vec![VERIFIED]);
        }
        m.members.insert((g, z), vec![Q, VERIFIED]);
        let add = |id: u64, actor: u64, target: u64| {
            json!({"id": id.to_string(), "action_type": 25, "guild_id": g.to_string(),
                   "target_id": target.to_string(), "user_id": actor.to_string(), "reason": null,
                   "changes": [{"key": "$add", "new_value": [{"id": VERIFIED.to_string(), "name": "v"}]}]})
        };
        let mut taken = add(10, ME, d);
        taken["changes"][0]["key"] = json!("$remove");
        m.members.insert((g, d), vec![VERIFIED]);
        // `human` holds a verify permit, `nobody` only Discord's Manage Roles.
        m.roles.get_mut(&g).unwrap().push((516, false, 0, 6));
        m.members.insert((g, human), vec![516]);
        m.members.insert((g, nobody), vec![]);
        m.members.insert((g, e), vec![VERIFIED]);
        m.audit_by_guild.insert(g, vec![taken, add(11, nobody, e), add(9, ME, c), add(8, bot, a), add(7, human, b), add(6, bot, c), add(5, bot, d)]);
    });
    let give = |i: u64, target: u64, role: u64| {
        let mut e = role_added(&t, bot, target, role);
        e.guild_id = Some(t.g(i));
        e
    };
    let given = |e: AuditLogEntry| {
        let t = &t;
        async move { crate::verify::given::on_audit(&t.app, &e).await.unwrap() }
    };
    let reads = |i: u64, x: u64| t.calls(&format!("GET /guilds/{}/members/{x} ", t.g(i)));
    // Base View: an ordinary "Member" autorole verifies nobody.
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![MEMBER]);
    });
    given(give(0, u, MEMBER)).await;
    assert_eq!(roles(&t, 0, u), Some(vec![]));
    // A role that opens nothing: no member read at all.
    t.set(|m| {
        m.members.insert((t.g(0).get(), p), vec![PINGS]);
    });
    given(give(0, p, PINGS)).await;
    assert_eq!((roles(&t, 0, p), reads(0, p)), (Some(vec![PINGS]), 0));
    // A member verified long ago is read once, then remembered.
    for _ in 0..2 {
        t.set(|m| {
            m.members
                .insert((t.g(0).get(), vfd), vec![VERIFIED, MEMBER]);
        });
        given(give(0, vfd, MEMBER)).await;
    }
    assert_eq!(
        (roles(&t, 0, vfd), reads(0, vfd)),
        (Some(vec![VERIFIED, MEMBER]), 1)
    );
    // Staff un-verify them: they are no longer remembered as verified.
    let mut unverify = role_added(&t, human, vfd, VERIFIED);
    unverify.changes = serde_json::from_value(
        json!([{"key": "$remove", "new_value": [{"id": VERIFIED.to_string(), "name": "v"}]}]),
    )
    .unwrap();
    given(unverify).await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), vfd), vec![MEMBER]);
    });
    given(give(0, vfd, MEMBER)).await;
    assert_eq!(
        roles(&t, 0, vfd),
        Some(vec![]),
        "checked again after the un-verify"
    );
    // A human without a permit: a role opening the channels goes from an unverified member and
    // stays with a verified one.
    t.set(|m| {
        m.members.insert((t.g(0).get(), human), vec![]);
        m.members.insert((t.g(0).get(), u), vec![MEMBER]);
        m.members
            .insert((t.g(0).get(), vfd), vec![VERIFIED, MEMBER]);
    });
    let by_human = |target: u64| {
        let mut e = role_added(&t, human, target, MEMBER);
        e.guild_id = Some(t.g(0));
        e
    };
    given(by_human(u)).await;
    given(by_human(vfd)).await;
    assert_eq!(
        (roles(&t, 0, u), roles(&t, 0, vfd)),
        (Some(vec![]), Some(vec![VERIFIED, MEMBER]))
    );
    // Brim's own slowmode keeps the gate; its own lock edits only shorten its life.
    let own = |kind: u64| -> AuditLogEntry {
        serde_json::from_value(
            json!({"id": "5", "action_type": kind, "guild_id": t.g(0).to_string(),
            "target_id": "711", "user_id": ME.to_string(), "reason": null, "changes": []}),
        )
        .unwrap()
    };
    let before = crate::verify::given::cached_for(t.g(0));
    given(own(11)).await;
    assert_eq!(crate::verify::given::cached_for(t.g(0)), before, "slowmode");
    given(own(14)).await;
    assert!(
        crate::verify::given::cached_for(t.g(0))
            .is_some_and(|d| d < std::time::Duration::from_secs(60)),
        "lock edit"
    );
    // A role edited to open the verified channels counts at once: the edit drops the cached gate.
    t.set(|m| {
        let g = t.g(0).get();
        m.roles
            .get_mut(&g)
            .unwrap()
            .iter_mut()
            .filter(|r| r.0 == PINGS)
            .for_each(|r| r.2 = view);
    });
    let edit: AuditLogEntry = serde_json::from_value(json!({
        "id": "4", "action_type": 31, "guild_id": t.g(0).to_string(), "target_id": PINGS.to_string(),
        "user_id": human.to_string(), "reason": null, "changes": [],
    }))
    .unwrap();
    given(edit).await;
    given(give(0, p, PINGS)).await;
    assert_eq!(
        roles(&t, 0, p),
        Some(vec![]),
        "the edited role is taken back"
    );
    // Send only: a role allowed to write there goes, one whose base Send @everyone's deny blocks stays.
    t.set(|m| {
        m.members.insert((t.g(1).get(), s1), vec![TALK]);
        m.members.insert((t.g(1).get(), s2), vec![SENDS]);
    });
    given(give(1, s1, TALK)).await;
    given(give(1, s2, SENDS)).await;
    assert_eq!(
        (roles(&t, 1, s1), roles(&t, 1, s2)),
        (Some(vec![]), Some(vec![SENDS]))
    );
    // A bot's member overwrite opening that channel is removed.
    let over: AuditLogEntry = serde_json::from_value(json!({
        "id": "3", "action_type": 13, "guild_id": t.g(1).to_string(), "target_id": "712",
        "user_id": bot.to_string(), "reason": null, "options": {"id": s3.to_string(), "type": "1"},
        "changes": [{"key": "allow", "new_value": send.to_string()}],
    }))
    .unwrap();
    given(over).await;
    assert_eq!(
        t.calls(&format!("DELETE /channels/712/permissions/{s3}")),
        1
    );
    // A verification role and an overwrite at once: the bot's own role proves nothing, both go,
    // and the member is not remembered as verified.
    t.set(|m| {
        m.members.insert((t.g(1).get(), s4), vec![VERIFIED]);
    });
    let mut over4 = serde_json::to_value(role_added(&t, bot, s4, VERIFIED)).unwrap();
    over4["action_type"] = json!(13);
    over4["guild_id"] = json!(t.g(1).to_string());
    over4["target_id"] = json!("712");
    over4["options"] = json!({"id": s4.to_string(), "type": "1"});
    over4["changes"] = json!([{"key": "allow", "new_value": send.to_string()}]);
    let over4: AuditLogEntry = serde_json::from_value(over4).unwrap();
    // The overwrite handler goes first, while the bot's role is still on the member: the grant
    // starts once that handler's first Discord call reached the mock (no fixed sleep to lose).
    let before = t.calls("");
    tokio::join!(given(over4), async {
        for _ in 0..500 {
            if t.calls("") > before {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        given(give(1, s4, VERIFIED)).await
    });
    assert_eq!(roles(&t, 1, s4), Some(vec![]));
    assert_eq!(
        t.calls(&format!("DELETE /channels/712/permissions/{s4}")),
        1
    );
    t.set(|m| {
        m.members.insert((t.g(1).get(), s4), vec![VERIFIED]);
    });
    given(give(1, s4, VERIFIED)).await;
    assert_eq!(
        roles(&t, 1, s4),
        Some(vec![]),
        "a later grant is still checked"
    );
    // A channel lock also denies Send to the verified role: the gate judges the saved overwrite,
    // so the Send-only channel stays gated while the lock lasts.
    let g1 = t.g(1).get();
    t.set(|m| {
        m.channels.insert(
            g1,
            chan(
                g1,
                "712",
                vec![ow(g1, 0, send), ow(VERIFIED, 0, send), ow(TALK, send, 0)],
            ),
        );
        m.members.insert((g1, s5), vec![TALK]);
    });
    let lock = brim_automod::lockdown::Snapshot {
        kind: Some(brim_automod::lockdown::LockKind::Channel(712)),
        channel_denies: vec![(712, VERIFIED)],
        prior: vec![(712, VERIFIED, send, 0)],
        ..Default::default()
    };
    brim_automod::store::save_lock(&t.app.pool, g1, &lock)
        .await
        .unwrap();
    crate::lockdown::forget_locks(g1);
    // The lock's gateway event for the channel Brim marked only softens the gate.
    crate::verify::given::mark_own(t.g(1), 712);
    t.app.invalidate_object(t.g(1), 712);
    assert!(crate::verify::given::cached_for(t.g(1))
        .is_some_and(|d| d < std::time::Duration::from_secs(60)));
    // An event about another object inside the window drops it (S R6-1); so does any event once
    // the mark ran out (S R5-1).
    given(give(1, s5, SENDS)).await;
    t.app.invalidate_object(t.g(1), 799);
    assert_eq!(
        crate::verify::given::cached_for(t.g(1)),
        None,
        "another object"
    );
    given(give(1, s5, SENDS)).await;
    crate::verify::given::age_own_edits(t.g(1));
    t.app.invalidate_object(t.g(1), 712);
    assert_eq!(
        crate::verify::given::cached_for(t.g(1)),
        None,
        "mark ran out"
    );
    t.set(|m| {
        m.members.insert((g1, s5), vec![TALK]);
    });
    given(give(1, s5, TALK)).await;
    sqlx::query("DELETE FROM lock_snapshots WHERE guild_id = $1")
        .bind(g1 as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    crate::lockdown::forget_locks(g1);
    assert_eq!(
        roles(&t, 1, s5),
        Some(vec![]),
        "Send gate held during the lock"
    );
    // Panic lets @everyone send in a whitelisted category: the gate judges the saved overwrite.
    t.set(|m| {
        m.channels.insert(
            g1,
            chan(
                g1,
                "712",
                vec![ow(g1, send, 0), ow(VERIFIED, send, 0), ow(TALK, send, 0)],
            ),
        );
        m.members.insert((g1, s6), vec![TALK]);
    });
    let end = json!({"exempt": [{"channel": 712, "allow": 0, "deny": send, "had": true}]});
    brim_core::jobs::enqueue(
        &t.app.pool,
        g1 as i64,
        "panic_end",
        end,
        crate::app::now_secs() + 3600,
    )
    .await
    .unwrap();
    t.app.invalidate_guild(t.g(1));
    given(give(1, s6, TALK)).await;
    t.exec("DELETE FROM scheduled_jobs WHERE guild_id = $1 + 1")
        .await;
    assert_eq!(
        roles(&t, 1, s6),
        Some(vec![]),
        "Send gate held during panic"
    );
    // At GuildCreate: only a role whose newest grant came from another bot goes.
    // During a raid the catch-up waits in a job and reads nothing.
    let raid = brim_automod::join::RaidDetector::restored(0, i64::MAX, String::new());
    t.app.raids.lock().unwrap().insert(t.g(2).get(), raid);
    crate::verify::given::catch_up(&t.app, t.g(2), None)
        .await
        .unwrap();
    t.app.raids.lock().unwrap().clear();
    assert_eq!(t.calls(&format!("GET /guilds/{}/audit-logs", t.g(2))), 0);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'verify_catch_up'",
    )
    .bind(t.g(2).get() as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
    crate::verify::given::catch_up(&t.app, t.g(2), None)
        .await
        .unwrap();
    assert_eq!(reads(2, d), 0, "Brim took it back last: no member read");
    // A member read that fails is retried by a job.
    t.exec("DELETE FROM scheduled_jobs WHERE guild_id = $1 + 2")
        .await;
    t.set(|m| m.fail_member = true);
    crate::verify::given::catch_up(&t.app, t.g(2), None)
        .await
        .unwrap();
    t.set(|m| m.fail_member = false);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'verify_catch_up'",
    )
    .bind(t.g(2).get() as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "a skipped member is checked again");
    // A lasting failure stops after 5 tries and tells staff.
    t.exec("UPDATE scheduled_jobs SET payload = '{\"tries\": 4}' WHERE guild_id = $1 + 2")
        .await;
    t.set(|m| m.fail_member = true);
    crate::verify::given::catch_up(&t.app, t.g(2), None)
        .await
        .unwrap();
    t.set(|m| m.fail_member = false);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'verify_catch_up'",
    )
    .bind(t.g(2).get() as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(n, 0, "gave up");
    assert_eq!(t.calls("Gave up after 5 failed tries"), 1);
    assert_eq!(
        roles(&t, 2, a),
        Some(vec![]),
        "a bot verified them while Brim was down"
    );
    assert_eq!(
        roles(&t, 2, b),
        Some(vec![VERIFIED]),
        "staff with a verify permit gave it"
    );
    assert_eq!(
        roles(&t, 2, e),
        Some(vec![]),
        "a human without a permit gave it"
    );
    assert_eq!(roles(&t, 2, c), Some(vec![VERIFIED]), "Brim gave it last");
    // Quarantine Hold off: the hold leaves a held member's role, so this check takes it.
    row(&t, 2, z, &[]).await;
    given(give(2, z, VERIFIED)).await;
    assert_eq!(roles(&t, 2, z), Some(vec![Q]));
    assert_eq!(saved(&t, 2, bot).await, None, "nobody is punished");
    t.cleanup().await;
}

/// D12b: when the actor's permits cannot be read, a dangerous grant and a permit role grant are
/// undone, but nobody is quarantined.
#[tokio::test]
async fn an_unreadable_actor_is_undone_but_not_punished() {
    let Some(t) = T::new(106).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (g, actor, member) = (t.g(0), t.base * 10 + 1, t.base * 10 + 2);
    let (permit,): (i64,) = sqlx::query_as(
        "INSERT INTO permits (guild_id, name, rank, perms) VALUES ($1, 'Mods', 1, '{ban}') RETURNING id",
    )
    .bind(g.get() as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1, $2, $3)")
        .bind(permit)
        .bind(g.get() as i64)
        .bind(BOOST as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.members.insert((g.get(), actor), vec![ROLE]);
        m.members.insert((g.get(), member), vec![BOOST]);
        m.fail_member = true;
        m.roles.get_mut(&g.get()).unwrap()[2].2 = 8; // ROLE, as the actor left it
    });
    let entry = |action: u8, target: u64, changes: Value| -> AuditLogEntry {
        serde_json::from_value(json!({
            "id": "1", "action_type": action, "guild_id": g.to_string(), "target_id": target.to_string(),
            "user_id": actor.to_string(), "reason": null, "changes": changes,
        }))
        .unwrap()
    };
    let perms = json!([{"key": "permissions", "old_value": "0", "new_value": "8"}]);
    let _ = crate::antinuke::on_audit(&t.app, &entry(31, ROLE, perms)).await;
    assert!(
        t.calls(&format!("PATCH /guilds/{g}/roles/{ROLE}")) >= 1,
        "the grant is undone"
    );
    let added = json!([{"key": "$add", "new_value": [{"id": BOOST.to_string(), "name": "Mods"}]}]);
    let _ = crate::antinuke::on_audit(&t.app, &entry(25, member, added)).await;
    assert_eq!(
        t.calls(&format!(
            "DELETE /guilds/{g}/members/{member}/roles/{BOOST}"
        )),
        1,
        "the permit role is taken back"
    );
    t.set(|m| m.fail_member = false);
    assert_eq!(roles(&t, 0, actor), Some(vec![ROLE]), "not quarantined");
    sqlx::query("DELETE FROM permits WHERE guild_id = $1")
        .bind(g.get() as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.cleanup().await;
}

/// Owner decision 2026-11-13: with anti-nuke off, a permit role given by someone without a permit is taken
/// back, and nobody is punished.
#[tokio::test]
async fn anti_nuke_off_still_takes_back_a_permit_role() {
    let Some(t) = T::new(263).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"enabled": false, "quarantine_role": Q.to_string()}}),
    )
    .await;
    t.exec("INSERT INTO permits (id, guild_id, name, perms) VALUES ($1 + 1, $1, 'Mods', '{ban}')")
        .await;
    // 501 = ROLE
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 501)")
        .await;
    let (actor, member) = (t.base * 10 + 1, t.base * 10 + 2);
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![]);
        m.members.insert((t.g(0).get(), member), vec![ROLE]);
    });
    let e: AuditLogEntry = serde_json::from_value(json!({
        "id": "1", "action_type": 25, "guild_id": t.g(0).to_string(), "target_id": member.to_string(),
        "user_id": actor.to_string(), "reason": null,
        "changes": [{"key": "$add", "new_value": [{"id": ROLE.to_string(), "name": "Mods"}]}],
    }))
    .unwrap();
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(
        roles(&t, 0, member),
        Some(vec![]),
        "the permit role is taken back"
    );
    assert_eq!(
        roles(&t, 0, actor),
        Some(vec![]),
        "the actor is not quarantined"
    );
    assert_eq!(saved(&t, 0, actor).await, None);
    t.cleanup().await;
}

/// Owner decision 2026-11-13: a role with only weak permits is taken back too, with anti-nuke on and off,
/// and nobody is punished.
#[tokio::test]
async fn a_weak_permit_role_is_taken_back_unpunished() {
    let Some(t) = T::new(280).await else { return };
    for (i, on) in [(0, true), (1, false)] {
        let an = json!({"enabled": on, "quarantine_role": Q.to_string()});
        server(&t, i, json!({ "antinuke": an })).await;
    }
    t.exec("INSERT INTO permits (id, guild_id, name, perms) VALUES ($1 + 1, $1, 'Cases', '{cases}'), ($1 + 2, $1 + 1, 'Cases', '{cases}')")
        .await;
    // 501 = ROLE
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 501), ($1 + 2, $1 + 1, 501)")
        .await;
    let (actor, member) = (t.base * 10 + 1, t.base * 10 + 2);
    for i in [0, 1] {
        let g = t.g(i);
        t.set(|m| {
            m.members.insert((g.get(), actor), vec![]);
            m.members.insert((g.get(), member), vec![ROLE]);
        });
        let mut e = role_added(&t, actor, member, ROLE);
        e.guild_id = Some(g);
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        assert_eq!(roles(&t, i, member), Some(vec![]), "{i}: taken back");
        assert_eq!(roles(&t, i, actor), Some(vec![]), "{i}: not quarantined");
        assert_eq!(saved(&t, i, actor).await, None);
    }
    t.cleanup().await;
}

/// An audit entry: `actor` gave `role` Administrator.
fn admin_given(t: &T, actor: u64, role: u64) -> AuditLogEntry {
    serde_json::from_value(json!({
        "id": "3", "action_type": 31, "guild_id": t.g(0).to_string(), "target_id": role.to_string(),
        "user_id": actor.to_string(), "reason": null,
        "changes": [{"key": "permissions", "old_value": "0", "new_value": "8"}],
    }))
    .unwrap()
}

fn role_perms(t: &T, role: u64) -> u64 {
    t.mock.lock().unwrap().roles[&t.g(0).get()]
        .iter()
        .find(|r| r.0 == role)
        .unwrap()
        .2
}

/// Owner decision 2026-11-13 (review): with anti-nuke off, a quarantined member's role change is
/// undone, and the actor is not quarantined.
#[tokio::test]
async fn anti_nuke_off_still_holds_a_quarantined_member() {
    let Some(t) = T::new(281).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (u, actor) = (t.base * 10 + 1, t.base * 10 + 2);
    row(&t, 0, u, &[ROLE as i64]).await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![Q, AUTOROLE]);
        m.members.insert((t.g(0).get(), actor), vec![ROLE]);
    });
    let e = role_added(&t, actor, u, AUTOROLE);
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(roles(&t, 0, u), Some(vec![Q]), "the role is taken back");
    assert_eq!(roles(&t, 0, actor), Some(vec![ROLE]), "not quarantined");
    assert_eq!(saved(&t, 0, actor).await, None);
    t.cleanup().await;
}

/// Owner decision 2026-11-13 (review): with anti-nuke off, a change to a locked role is set back.
#[tokio::test]
async fn anti_nuke_off_still_sets_a_lock_back() {
    let Some(t) = T::new(282).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let actor = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![]);
        m.roles.get_mut(&t.g(0).get()).unwrap()[2].2 = 8; // ROLE, as the actor left it
    });
    let lock = brim_automod::lockdown::Snapshot {
        kind: Some(brim_automod::lockdown::LockKind::Role),
        role_perms: vec![(ROLE, 8)],
        ..Default::default()
    };
    brim_automod::store::save_lock(&t.app.pool, t.g(0).get(), &lock)
        .await
        .unwrap();
    crate::lockdown::forget_locks(t.g(0).get());
    let r = crate::antinuke::on_audit(&t.app, &admin_given(&t, actor, ROLE)).await;
    t.exec("DELETE FROM lock_snapshots WHERE guild_id = $1")
        .await;
    crate::lockdown::forget_locks(t.g(0).get());
    r.unwrap();
    assert_eq!(role_perms(&t, ROLE), 0, "set back to what the lock left");
    assert_eq!(roles(&t, 0, actor), Some(vec![]), "not quarantined");
    t.cleanup().await;
}

/// Owner decision 2026-11-13, answer C, and the owner 2026-11-15: with anti-nuke off (and strict mode off),
/// a dangerous grant is still undone, and the actor is not quarantined.
#[tokio::test]
async fn anti_nuke_off_undoes_a_dangerous_grant_unpunished() {
    let Some(t) = T::new(283).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let actor = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![ROLE]);
        m.roles.get_mut(&t.g(0).get()).unwrap()[2].2 = 8;
    });
    crate::antinuke::on_audit(&t.app, &admin_given(&t, actor, ROLE))
        .await
        .unwrap();
    assert_eq!(role_perms(&t, ROLE), 0, "the grant is undone");
    assert_eq!(roles(&t, 0, actor), Some(vec![ROLE]), "not quarantined");
    assert_eq!(saved(&t, 0, actor).await, None);
    t.cleanup().await;
}

/// Review 11-14 (E): with anti-nuke off, a kick of a quarantined member cannot be undone, so no
/// "undone" alert is logged.
#[tokio::test]
async fn anti_nuke_off_logs_no_undo_for_a_kick() {
    let Some(t) = T::new(284).await else { return };
    let an = json!({"enabled": false, "quarantine_hold": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (u, actor) = (t.base * 10 + 1, t.base * 10 + 2);
    row(&t, 0, u, &[]).await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![]);
    });
    let kick: AuditLogEntry = serde_json::from_value(json!({
        "id": "4", "action_type": 20, "guild_id": t.g(0).to_string(), "target_id": u.to_string(),
        "user_id": actor.to_string(), "reason": null, "changes": [],
    }))
    .unwrap();
    crate::antinuke::on_audit(&t.app, &kick).await.unwrap();
    assert_eq!(t.calls(&format!("POST /channels/{LOGS}/messages")), 0);
    // Control: the same kick with anti-nuke on is logged, so the 0 above is not an early return.
    let on = json!({"enabled": true, "quarantine_hold": true, "quarantine_role": Q.to_string()});
    server(&t, 1, json!({ "antinuke": on })).await;
    row(&t, 1, u, &[]).await;
    t.set(|m| {
        m.members.insert((t.g(1).get(), actor), vec![]);
    });
    let mut kick1 = serde_json::to_value(&kick).unwrap();
    kick1["id"] = json!("5");
    kick1["guild_id"] = json!(t.g(1).to_string());
    let kick1: AuditLogEntry = serde_json::from_value(kick1).unwrap();
    crate::antinuke::on_audit(&t.app, &kick1).await.unwrap();
    assert!(t.calls(&format!("POST /channels/{LOGS}/messages")) >= 1);
    t.cleanup().await;
}

/// Review 11-14 (F): with anti-nuke off, roles given to a join-quarantined member are taken back
/// and that is logged.
#[tokio::test]
async fn anti_nuke_off_logs_roles_taken_from_an_unverified_member() {
    let Some(t) = T::new(285).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (u, actor) = (t.base * 10 + 1, t.base * 10 + 2);
    sqlx::query("INSERT INTO quarantines (guild_id, user_id, saved_roles, on_join) VALUES ($1, $2, '{}', true)")
        .bind(t.g(0).get() as i64)
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.members.insert((t.g(0).get(), u), vec![Q, AUTOROLE]);
        m.members.insert((t.g(0).get(), actor), vec![]);
    });
    crate::antinuke::on_audit(&t.app, &role_added(&t, actor, u, AUTOROLE))
        .await
        .unwrap();
    assert_eq!(roles(&t, 0, u), Some(vec![Q]), "taken back");
    assert_eq!(t.calls(&format!("POST /channels/{LOGS}/messages")), 1);
    t.cleanup().await;
}

/// Review 11-14 (H): a late undo does not set back a role a later change already replaced; the
/// skip is logged.
#[tokio::test]
async fn a_late_undo_skips_a_role_changed_again() {
    let Some(t) = T::new(286).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let actor = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![]);
        m.roles.get_mut(&t.g(0).get()).unwrap()[2].2 = 4; // a later change made it Ban Members
    });
    crate::antinuke::on_audit(&t.app, &admin_given(&t, actor, ROLE))
        .await
        .unwrap();
    assert_eq!(role_perms(&t, ROLE), 4, "the later change stays");
    assert_eq!(t.calls(&format!("POST /channels/{LOGS}/messages")), 1);
    t.cleanup().await;
}

/// Q2 (owner decision 11-14): a person with a verify permit who gives a join-quarantined member the
/// verification role verifies them, as the button does, with anti-nuke on and off. Without the
/// permit the role is taken back as before.
#[tokio::test]
async fn a_verify_permit_verifies_a_join_quarantined_member_by_hand() {
    let Some(t) = T::new(745).await else { return };
    let mut v = web_verification();
    v["quarantine_on_join"] = json!(true);
    for (i, on) in [(0, true), (1, false)] {
        let an = json!({"enabled": on, "quarantine_role": Q.to_string()});
        server(&t, i, json!({"antinuke": an, "verification": v})).await;
    }
    t.exec("INSERT INTO permits (id, guild_id, name, perms) VALUES ($1 + 1, $1, 'Verifiers', '{verify}'), ($1 + 2, $1 + 1, 'Verifiers', '{verify}')")
        .await;
    // 501 = ROLE
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 501), ($1 + 2, $1 + 1, 501)")
        .await;
    t.exec("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1 + 1, $1, $1 * 10 + 1), ($1 + 2, $1 + 1, $1 * 10 + 1)")
        .await;
    let (staff, nobody, u, u2) = (
        t.base * 10 + 1,
        t.base * 10 + 2,
        t.base * 10 + 3,
        t.base * 10 + 4,
    );
    for i in [0, 1] {
        let g = t.g(i);
        for m in [u, u2] {
            sqlx::query("INSERT INTO quarantines (guild_id, user_id, saved_roles, on_join) VALUES ($1, $2, '{}', true)")
                .bind(g.get() as i64)
                .bind(m as i64)
                .execute(&t.app.pool)
                .await
                .unwrap();
        }
        t.set(|m| {
            m.members.insert((g.get(), staff), vec![ROLE]);
            m.members.insert((g.get(), nobody), vec![]);
            m.members.insert((g.get(), u), vec![Q, VERIFIED]);
            m.members.insert((g.get(), u2), vec![Q, VERIFIED]);
        });
        let mut e = role_added(&t, staff, u, VERIFIED);
        e.guild_id = Some(g);
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        assert_eq!(roles(&t, i, u), Some(vec![VERIFIED]), "{i}: verified");
        assert_eq!(
            saved(&t, i, u).await,
            None,
            "{i}: the join quarantine is over"
        );
        assert_eq!(
            saved(&t, i, staff).await,
            None,
            "{i}: staff not quarantined"
        );
        let mut e = role_added(&t, nobody, u2, VERIFIED);
        e.guild_id = Some(g);
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        assert_eq!(
            roles(&t, i, u2),
            Some(vec![Q]),
            "{i}: no permit, taken back"
        );
    }
    t.cleanup().await;
}

/// Q4 and Q5 (owner decision 11-14): a bot listed as Trusted Admin or on the quarantine whitelist gets
/// neither trust, only the permit on its own role; a linked role and an Onboarding role carry no
/// permit in any lookup once read (GuildCreate, GuildUpdate), and that is logged once.
#[tokio::test]
async fn a_listed_bot_gets_no_user_trust_and_a_linked_role_no_permit() {
    let Some(t) = T::new(746).await else { return };
    let (bot, user, linked) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
    let qw = json!([bot.to_string(), user.to_string()]);
    server(
        &t,
        0,
        json!({"antinuke": {"quarantine_role": Q.to_string(), "quarantine_whitelist": qw}}),
    )
    .await;
    let g = t.g(0);
    t.set(|m| {
        m.bots.push(bot);
        m.role_tags
            .insert(AUTOROLE, json!({"guild_connections": null}));
        m.members.insert((g.get(), bot), vec![ROLE]);
        m.members.insert((g.get(), user), vec![]);
        m.members.insert((g.get(), linked), vec![AUTOROLE]);
        m.members.insert((g.get(), linked + 1), vec![VERIFIED]);
        m.onboarding.insert(
            g.get(),
            json!({"prompts": [{"options": [{"role_ids": [VERIFIED.to_string()]}]}]}),
        );
    });
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $2), ($1, $3)")
        .bind(g.get() as i64)
        .bind(bot as i64)
        .bind(user as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    // 501 = ROLE (the bot's role), 504 = AUTOROLE (linked)
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1 + 1, $1, 'Bot', '{}', '{quarantine}'), ($1 + 2, $1, 'Linked', '{ban}', '{}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 501), ($1 + 2, $1, 504), ($1 + 2, $1, 503)")
        .await;
    // The bot is listed in its permit; nobody is listed in Linked.
    t.exec("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1 + 1, $1, $1 * 10 + 1)")
        .await;
    let owner = t.app.guild_info(g).await.unwrap().owner;
    let ignored = "A permit is on a role Discord hands out";
    assert!(
        !t.app
            .level(g, owner, Id::new(linked))
            .await
            .unwrap()
            .has("ban"),
        "a role alone gives no permit"
    );
    t.app.sync_unpermittable(g).await.unwrap();
    t.app.sync_unpermittable(g).await.unwrap();
    assert_eq!(t.calls(ignored), 1, "logged once");
    assert_eq!(
        t.app.level(g, owner, Id::new(linked + 1)).await.unwrap(),
        brim_core::permits::Level::None,
        "an Onboarding role"
    );
    let level = |u: u64| t.app.level(g, owner, Id::new(u));
    let b = level(bot).await.unwrap();
    assert!(!b.immune(), "a listed bot is no Trusted Admin");
    assert!(b.whitelisted("quarantine"), "its bot permit still counts");
    assert!(level(user).await.unwrap().immune(), "a listed user is");
    assert_eq!(
        level(linked).await.unwrap(),
        brim_core::permits::Level::None
    );
    let cfg = t.app.cfg(g.get()).await.unwrap();
    assert!(!t
        .app
        .quarantine_listed(&cfg, g, Id::new(bot))
        .await
        .unwrap());
    assert!(t
        .app
        .quarantine_listed(&cfg, g, Id::new(user))
        .await
        .unwrap());
    t.cleanup().await;
}

/// Permits `Mods` (rank 2, roles ROLE and AUTOROLE) and `Links` (weak, rank 1, role VERIFIED) on
/// server `i`, as `(mods, links)` ids.
async fn listed_permits(t: &T, i: u64) -> (i64, i64) {
    let g = t.g(i).get() as i64;
    let mut ids = vec![];
    for (name, rank, perms, wl, roles) in [
        ("Mods", 2, "{ban}", "{}", vec![ROLE, AUTOROLE]),
        ("Links", 1, "{}", "{links}", vec![VERIFIED]),
    ] {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO permits (guild_id, name, rank, perms, whitelist) VALUES ($1, $2, $3, $4::text[], $5::text[]) RETURNING id",
        )
        .bind(g)
        .bind(name)
        .bind(rank)
        .bind(perms)
        .bind(wl)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
        for r in roles {
            sqlx::query(
                "INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(g)
            .bind(r as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        }
        ids.push(id);
    }
    (ids[0], ids[1])
}

/// The permit each listed member of server `i` holds.
async fn listed(t: &T, i: u64) -> Vec<(i64, String)> {
    sqlx::query_as(
        "SELECT m.user_id, p.name FROM permit_members m JOIN permits p ON p.id = m.permit_id
         WHERE m.guild_id = $1 ORDER BY 1",
    )
    .bind(t.g(i).get() as i64)
    .fetch_all(&t.app.pool)
    .await
    .unwrap()
}

/// Owner decision 2026-11-15: the first sync lists every permit role holder by id (one permit each), forgets
/// listed people who are gone (never the owner), gives listed members their permit's roles and
/// takes permit roles from everyone else, nobody punished.
#[tokio::test]
async fn permit_sync_migrates_then_gives_and_takes_roles() {
    let Some(t) = T::new(600).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"enabled": true, "quarantine_role": Q.to_string()}}),
    )
    .await;
    listed_permits(&t, 0).await;
    let g = t.g(0);
    let (a, b, c, gone, held) = (
        t.base * 10 + 1,
        t.base * 10 + 2,
        t.base * 10 + 3,
        t.base * 10 + 4,
        t.base * 10 + 5,
    );
    t.set(|m| {
        m.members.insert((g.get(), a), vec![ROLE]);
        m.members.insert((g.get(), b), vec![VERIFIED, ROLE]);
        m.members.insert((g.get(), c), vec![VERIFIED]);
        m.members.insert((g.get(), held), vec![Q]);
    });
    sqlx::query("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, $2), ($1, $3)")
        .bind(g.get() as i64)
        .bind(gone as i64)
        .bind(t.owner() as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $2)")
        .bind(g.get() as i64)
        .bind(gone as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    crate::permit_sync::sync(&t.app, g).await.unwrap();
    let (a_, b_, c_) = (a as i64, b as i64, c as i64);
    assert_eq!(
        listed(&t, 0).await,
        [
            (a_, "Mods".into()),
            (b_, "Mods".into()),
            (c_, "Links".into())
        ]
    );
    assert!(
        brim_core::permits::members_migrated(&t.app.pool, g.get() as i64)
            .await
            .unwrap()
    );
    assert_eq!(
        roles(&t, 0, a),
        Some(vec![ROLE, AUTOROLE]),
        "given the permit's other role"
    );
    assert_eq!(
        roles(&t, 0, b),
        Some(vec![ROLE, AUTOROLE]),
        "one permit each: the weaker permit's role is taken"
    );
    assert_eq!(roles(&t, 0, c), Some(vec![VERIFIED]));
    let eo: Vec<i64> = sqlx::query_scalar("SELECT user_id FROM extra_owners WHERE guild_id = $1")
        .bind(g.get() as i64)
        .fetch_all(&t.app.pool)
        .await
        .unwrap();
    assert_eq!(
        eo,
        [t.owner() as i64],
        "the gone one is forgotten, the owner never"
    );
    let ta = t
        .count("SELECT count(*) FROM trusted_admins WHERE guild_id = $1")
        .await;
    assert_eq!(ta, 0);
    let members: Value = sqlx::query_scalar(
        "SELECT config->'permits'->'custom'->0->'members' FROM guild_config WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(g.get() as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(
        members,
        json!([a.to_string(), b.to_string()]),
        "a config version records it"
    );
    assert!(t.calls(&format!("POST /channels/{LOGS}/messages")) >= 3);
    assert_eq!(saved(&t, 0, a).await, None, "nobody is quarantined");
    // After the migration a permit role alone is taken; a quarantined member is left alone.
    t.set(|m| {
        m.members.insert((g.get(), held), vec![Q, ROLE]);
        m.members.insert((g.get(), gone), vec![ROLE, AUTOROLE]);
    });
    row(&t, 0, held, &[]).await;
    crate::permit_sync::sync(&t.app, g).await.unwrap();
    assert_eq!(
        roles(&t, 0, gone),
        Some(vec![]),
        "a role alone gives no permit"
    );
    assert_eq!(listed(&t, 0).await.len(), 3, "the migration runs once");
    t.cleanup().await;
}

/// Owner decision 2026-11-16: Trusted Admin has a role, and Owner one for the owner and every Extra Owner.
/// Brim gives them by user id and takes them from anyone else, like permit roles.
#[tokio::test]
async fn level_roles_follow_the_owner_eo_and_ta_lists() {
    let Some(t) = T::new(601).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"enabled": true, "quarantine_role": Q.to_string()}}),
    )
    .await;
    let (owner_role, ta_role) = (VERIFIED, AUTOROLE);
    let g = t.g(0);
    let gi = g.get() as i64;
    let (eo, ta, other) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
    t.set(|m| {
        m.members.insert((g.get(), t.owner()), vec![]);
        m.members.insert((g.get(), eo), vec![]);
        m.members.insert((g.get(), ta), vec![]);
        m.members
            .insert((g.get(), other), vec![owner_role, ta_role]);
    });
    sqlx::query("INSERT INTO level_roles (guild_id, level, role_id) VALUES ($1, 'owner', $2), ($1, 'trusted_admin', $3)")
        .bind(gi).bind(owner_role as i64).bind(ta_role as i64).execute(&t.app.pool).await.unwrap();
    sqlx::query("INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, $2)")
        .bind(gi)
        .bind(eo as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $2)")
        .bind(gi)
        .bind(ta as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    crate::permit_sync::sync(&t.app, g).await.unwrap();
    assert_eq!(roles(&t, 0, t.owner()), Some(vec![owner_role]), "the owner");
    assert_eq!(roles(&t, 0, eo), Some(vec![owner_role]), "an Extra Owner");
    assert_eq!(roles(&t, 0, ta), Some(vec![ta_role]), "a Trusted Admin");
    assert_eq!(
        roles(&t, 0, other),
        Some(vec![]),
        "a role alone gives nothing"
    );
    let all = brim_core::permits::permit_roles(&t.app.pool, gi)
        .await
        .unwrap();
    assert!(
        all.contains(&(owner_role as i64)) && all.contains(&(ta_role as i64)),
        "never handed out by verification or sticky roles"
    );
    t.cleanup().await;
}

/// Owner decision 2026-11-15: a member who leaves (a kick or ban too) loses their permit, Extra Owner and
/// Trusted Admin, in a config version, and the anti-nuke log says so.
#[tokio::test]
async fn a_leaver_loses_permit_extra_owner_and_trusted_admin() {
    let Some(t) = T::new(604).await else { return };
    server(&t, 0, json!({})).await;
    let (mods, _) = listed_permits(&t, 0).await;
    let (g, u) = (t.g(0), t.base * 10 + 1);
    for sql in [
        "INSERT INTO extra_owners (guild_id, user_id) VALUES ($1, $2)",
        "INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $2)",
    ] {
        sqlx::query(sql)
            .bind(g.get() as i64)
            .bind(u as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, $3)")
        .bind(mods)
        .bind(g.get() as i64)
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    let logs = format!("POST /channels/{LOGS}/messages");
    crate::permit_sync::on_leave(&t.app, g, Id::new(u), false)
        .await
        .unwrap();
    assert!(listed(&t, 0).await.is_empty());
    let staff = t
        .count("SELECT count(*) FROM extra_owners WHERE guild_id = $1")
        .await
        + t.count("SELECT count(*) FROM trusted_admins WHERE guild_id = $1")
            .await;
    assert_eq!(staff, 0);
    let p: Value = sqlx::query_scalar(
        "SELECT config->'permits' FROM guild_config WHERE guild_id = $1 AND version = 2",
    )
    .bind(g.get() as i64)
    .fetch_one(&t.app.pool)
    .await
    .unwrap();
    assert_eq!((&p["eo"], &p["ta"]), (&json!([]), &json!([])));
    let sent = t.mock.lock().unwrap().log.clone();
    assert!(sent.iter().any(|l| l.contains(&logs)
        && l.contains("left the server: removed Extra Owner, Trusted Admin, permit Mods")));
    // Leaving again holds nothing: no version, no log.
    crate::permit_sync::on_leave(&t.app, g, Id::new(u), false)
        .await
        .unwrap();
    assert_eq!(
        t.count("SELECT count(*) FROM guild_config WHERE guild_id = $1")
            .await,
        2
    );
    assert_eq!(t.calls(&logs), 1);
    t.cleanup().await;
}

fn role_entry(
    g: Id<twilight_model::id::marker::GuildMarker>,
    actor: u64,
    target: u64,
    key: &str,
    role: u64,
) -> AuditLogEntry {
    serde_json::from_value(json!({
        "id": "1", "action_type": 25, "guild_id": g.to_string(), "target_id": target.to_string(),
        "user_id": actor.to_string(), "reason": null,
        "changes": [{"key": key, "new_value": [{"id": role.to_string(), "name": "Mods"}]}],
    }))
    .unwrap()
}

/// Owner decision 2026-11-15: once permits belong to user ids, a permit role given to someone not listed
/// is taken back whoever gave it, a Trusted Admin too, and nobody is quarantined; a listed member
/// keeps it.
#[tokio::test]
async fn a_permit_role_given_to_someone_not_listed_is_taken_back_unpunished() {
    let Some(t) = T::new(602).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (mods, _) = listed_permits(&t, 0).await;
    let g = t.g(0);
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (ta, plain, member, mod_) = (
        t.base * 10 + 1,
        t.base * 10 + 2,
        t.base * 10 + 3,
        t.base * 10 + 4,
    );
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $2)")
        .bind(g.get() as i64)
        .bind(ta as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, $3)")
        .bind(mods)
        .bind(g.get() as i64)
        .bind(mod_ as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.members.insert((g.get(), ta), vec![]);
        m.members.insert((g.get(), plain), vec![AUTOROLE]);
        m.members.insert((g.get(), member), vec![ROLE]);
        m.members.insert((g.get(), mod_), vec![ROLE]);
    });
    for actor in [ta, plain] {
        t.set(|m| {
            m.members.insert((g.get(), member), vec![ROLE]);
        });
        let e = role_entry(g, actor, member, "$add", ROLE);
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        assert_eq!(
            roles(&t, 0, member),
            Some(vec![]),
            "taken back from {actor}'s grant"
        );
        assert_eq!(
            saved(&t, 0, actor).await,
            None,
            "{actor} is not quarantined"
        );
    }
    assert_eq!(roles(&t, 0, plain), Some(vec![AUTOROLE]));
    let e = role_entry(g, plain, mod_, "$add", ROLE);
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(
        roles(&t, 0, mod_),
        Some(vec![ROLE]),
        "a listed member keeps it"
    );
    t.cleanup().await;
}

/// Owner decision 2026-11-15: a permit role removed from a member listed in the permit is given back, and
/// the log names who removed it.
#[tokio::test]
async fn a_permit_role_removed_from_a_listed_member_is_given_back() {
    let Some(t) = T::new(603).await else { return };
    server(&t, 0, json!({"antinuke": {"enabled": false}})).await;
    let (mods, _) = listed_permits(&t, 0).await;
    let g = t.g(0);
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (actor, member, other) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
    sqlx::query("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, $3)")
        .bind(mods)
        .bind(g.get() as i64)
        .bind(member as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.members.insert((g.get(), member), vec![AUTOROLE]);
        m.members.insert((g.get(), other), vec![]);
    });
    let e = role_entry(g, actor, member, "$remove", ROLE);
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(roles(&t, 0, member), Some(vec![AUTOROLE, ROLE]));
    let sent = t.mock.lock().unwrap().log.clone();
    assert!(sent
        .iter()
        .any(|l| l.contains(&format!("POST /channels/{LOGS}/messages"))
            && l.contains(&actor.to_string())
            && l.contains("Mods")));
    // Not listed: nothing to give back.
    let e = role_entry(g, actor, other, "$remove", ROLE);
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(roles(&t, 0, other), Some(vec![]));
    t.cleanup().await;
}

/// Owner decision (2026-11-15): a staff member quarantined during the migration keeps their permit: the
/// saved roles count, a join quarantine's do not.
#[tokio::test]
async fn permit_sync_migrates_a_quarantined_holder_by_saved_roles() {
    let Some(t) = T::new(606).await else { return };
    server(&t, 0, json!({})).await;
    listed_permits(&t, 0).await;
    let g = t.g(0);
    let (held, joiner) = (t.base * 10 + 1, t.base * 10 + 2);
    t.set(|m| {
        m.members.insert((g.get(), held), vec![Q]);
        m.members.insert((g.get(), joiner), vec![Q]);
    });
    row(&t, 0, held, &[ROLE as i64]).await;
    row(&t, 0, joiner, &[ROLE as i64]).await;
    sqlx::query("UPDATE quarantines SET on_join = true WHERE guild_id = $1 AND user_id = $2")
        .bind(g.get() as i64)
        .bind(joiner as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    crate::permit_sync::sync(&t.app, g).await.unwrap();
    assert_eq!(listed(&t, 0).await, [(held as i64, "Mods".into())]);
    t.cleanup().await;
}

async fn list_in(t: &T, permit: i64, u: u64) {
    sqlx::query("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, $3)")
        .bind(permit)
        .bind(t.g(0).get() as i64)
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
}

/// A permit by id ends with the membership: a listed user not in the server has no level, and the
/// sync forgets them only on Discord's "not a member", never on a failed lookup.
#[tokio::test]
async fn a_listed_user_not_in_the_server_has_no_permit_and_a_failed_lookup_keeps_them() {
    let Some(t) = T::new(620).await else { return };
    server(&t, 0, json!({})).await;
    let (mods, _) = listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (g, u) = (t.g(0), t.base * 10 + 1);
    list_in(&t, mods, u).await;
    let owner = Id::new(t.owner());
    t.app.guild_info(g).await.unwrap(); // cached before member lookups fail
    let level = t.app.level(g, owner, Id::new(u)).await.unwrap();
    assert_eq!(level, brim_core::permits::Level::None);
    t.set(|m| {
        m.members.insert((g.get(), u), vec![]);
    });
    let level = t.app.level(g, owner, Id::new(u)).await.unwrap();
    assert!(matches!(level, brim_core::permits::Level::Custom { .. }));
    t.set(|m| {
        m.members.remove(&(g.get(), u));
        m.fail_member = true;
    });
    assert!(t.app.level(g, owner, Id::new(u)).await.is_err());
    crate::permit_sync::sync(&t.app, g).await.unwrap();
    assert_eq!(listed(&t, 0).await.len(), 1, "a failed lookup keeps them");
    t.set(|m| m.fail_member = false);
    crate::permit_sync::sync(&t.app, g).await.unwrap();
    assert!(listed(&t, 0).await.is_empty(), "not a member: forgotten");
    t.cleanup().await;
}

/// A missed audit entry: on a member update, a permit role the member is not listed for is taken,
/// a role of their own permit never, and a quarantined member is left alone.
#[tokio::test]
async fn a_member_update_takes_permit_roles_the_member_is_not_listed_for() {
    let Some(t) = T::new(621).await else { return };
    server(&t, 0, json!({})).await;
    let (mods, _) = listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let g = t.g(0).get();
    let (listed_, plain, held) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
    list_in(&t, mods, listed_).await;
    row(&t, 0, held, &[]).await;
    let all = vec![ROLE, AUTOROLE, VERIFIED];
    for u in [listed_, plain, held] {
        t.set(|m| {
            m.members.insert((g, u), all.clone());
        });
        let m: MemberUpdate = serde_json::from_value(json!({
            "guild_id": g.to_string(), "roles": all.iter().map(u64::to_string).collect::<Vec<_>>(),
            "joined_at": "2026-01-01T00:00:00.000000+00:00", "deaf": false, "mute": false,
            "pending": false, "avatar": null, "communication_disabled_until": null, "nick": null,
            "premium_since": null,
            "user": {"id": u.to_string(), "username": "u", "discriminator": "0", "avatar": null},
        }))
        .unwrap();
        crate::permit_sync::on_member_update(&t.app, &m)
            .await
            .unwrap();
    }
    assert_eq!(roles(&t, 0, listed_), Some(vec![ROLE, AUTOROLE]));
    assert_eq!(roles(&t, 0, plain), Some(vec![]));
    assert_eq!(roles(&t, 0, held), Some(all));
    assert_eq!(t.calls(&format!("POST /channels/{LOGS}/messages")), 0);
    t.cleanup().await;
}

/// A failed take-back is logged as failed; and when another bot keeps giving a member a permit
/// role, Brim reverts 5 times, then at most once a minute for the window, with one log and
/// one owner DM.
#[tokio::test]
async fn permit_role_reverts_report_failures_and_stop_a_fight() {
    let Some(t) = T::new(622).await else { return };
    server(&t, 0, json!({"antinuke": {"enabled": true}})).await;
    listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let g = t.g(0);
    let (other_bot, member, someone, gone) = (
        t.base * 10 + 1,
        t.base * 10 + 2,
        t.base * 10 + 3,
        t.base * 10 + 4,
    );
    let sent = || t.mock.lock().unwrap().log.clone();
    // `gone` is not in the mock, so the remove fails.
    let e = role_entry(g, someone, gone, "$add", ROLE);
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert!(
        sent().iter().any(|l| l.contains("failed, not taken back")),
        "{:?}",
        sent()
    );
    for i in 1..=7 {
        t.set(|m| {
            m.members.insert((g.get(), member), vec![ROLE]);
        });
        let e = role_entry(g, other_bot, member, "$add", ROLE);
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        let want = if i <= 5 { vec![] } else { vec![ROLE] };
        assert_eq!(roles(&t, 0, member), Some(want), "revert {i}");
    }
    let stops = sent()
        .iter()
        .filter(|l| l.contains("Slowed down reverting a member's permit roles"))
        .count();
    assert_eq!(stops, 1);
    let dms = sent()
        .iter()
        .filter(|l| l.contains("should be stopped"))
        .count();
    assert_eq!(dms, 2, "the log and the owner DM");
    let old_dm = sent()
        .iter()
        .filter(|l| l.contains("keeps giving out roles"))
        .count();
    assert_eq!(
        old_dm, 0,
        "in a migrated guild only the breaker DMs the owner"
    );
    assert!(!crate::antinuke::permit_fight_stopped(g.get(), someone));
    assert!(crate::antinuke::permit_fight_stopped(g.get(), member));
    t.cleanup().await;
}

/// An audit entry for a kick (20), ban (22) or timeout (24) of `target` by `actor`.
fn member_entry(t: &T, kind: u16, actor: u64, target: u64) -> AuditLogEntry {
    let changes = if kind == 24 {
        json!([{"key": "communication_disabled_until", "new_value": "2030-01-01T00:00:00+00:00"}])
    } else {
        json!([])
    };
    serde_json::from_value(json!({
        "id": "5", "action_type": kind, "guild_id": t.g(0).to_string(), "target_id": target.to_string(),
        "user_id": actor.to_string(), "reason": null, "changes": changes,
    }))
    .unwrap()
}

/// `users` listed in the permit `permit`, all in the mock with no roles.
async fn members_in(t: &T, permit: i64, users: &[u64]) {
    for &u in users {
        list_in(t, permit, u).await;
        t.set(|m| {
            m.members.insert((t.g(0).get(), u), vec![]);
        });
    }
}

/// Owner decision 2026-11-15: a kick by someone of the same level, done outside Brim, is a
/// breach: the actor is quarantined and the victim gets a one-use invite back by DM, also when
/// MemberRemove forgot the victim's permit before the audit entry came in.
#[tokio::test]
async fn an_equal_rank_kick_quarantines_the_actor_and_reinvites_the_victim() {
    let Some(t) = T::new(965).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(
        &t,
        0,
        json!({"antinuke": an, "verification": {"channel": "700"}}),
    )
    .await;
    let (mods, _) = listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (actor, victim) = (t.base * 10 + 1, t.base * 10 + 2);
    members_in(&t, mods, &[actor, victim]).await;
    t.set(|m| {
        m.members.remove(&(t.g(0).get(), victim));
        // The verification channel is one of this server's (checked before the invite).
        let g = t.g(0).get();
        m.channels.insert(g, json!([{"id": "700", "type": 0, "guild_id": g.to_string(), "name": "verify", "position": 0}]));
    });
    crate::permit_sync::on_leave(&t.app, t.g(0), Id::new(victim), false)
        .await
        .unwrap();
    assert!(!listed(&t, 0).await.iter().any(|l| l.0 == victim as i64));
    crate::antinuke::on_audit(&t.app, &member_entry(&t, 20, actor, victim))
        .await
        .unwrap();
    assert!(
        saved(&t, 0, actor).await.is_some(),
        "the actor is quarantined"
    );
    let sent = t.mock.lock().unwrap().log.clone();
    assert!(
        sent.iter()
            .any(|l| l.starts_with("POST /channels/700/invites")
                && l.contains("\"max_uses\":1")
                && l.contains("\"max_age\":86400")),
        "{sent:?}"
    );
    assert!(sent.iter().any(|l| l.contains("discord.gg/c")), "{sent:?}");
    assert!(
        sent.iter()
            .any(|l| l.contains(&format!("<@{victim}>")) && l.contains("same or a higher level")),
        "the report names why: {sent:?}"
    );
    t.cleanup().await;
}

/// H3: a ban by a lower rank is undone and the actor quarantined; the entry may come before
/// MemberRemove.
#[tokio::test]
async fn a_ban_by_a_lower_rank_is_undone() {
    let Some(t) = T::new(966).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (mods, links) = listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (actor, victim) = (t.base * 10 + 1, t.base * 10 + 2);
    members_in(&t, links, &[actor]).await;
    members_in(&t, mods, &[victim]).await;
    t.set(|m| {
        m.members.remove(&(t.g(0).get(), victim));
        m.bans.insert((t.g(0).get(), victim), "x".into());
        m.system_channels.insert(t.g(0).get(), 701);
    });
    crate::antinuke::on_audit(&t.app, &member_entry(&t, 22, actor, victim))
        .await
        .unwrap();
    assert!(
        !t.mock
            .lock()
            .unwrap()
            .bans
            .contains_key(&(t.g(0).get(), victim)),
        "unbanned"
    );
    assert!(saved(&t, 0, actor).await.is_some());
    assert_eq!(
        t.calls("POST /channels/701/invites"),
        1,
        "the system channel"
    );
    t.cleanup().await;
}

/// H3: a higher rank may kick, ban and time out a lower one: nobody is punished, nothing undone.
#[tokio::test]
async fn a_higher_rank_acting_is_not_punished() {
    let Some(t) = T::new(967).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (mods, links) = listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (actor, victim) = (t.base * 10 + 1, t.base * 10 + 2);
    members_in(&t, mods, &[actor]).await;
    members_in(&t, links, &[victim]).await;
    t.set(|m| {
        m.timeouts
            .insert((t.g(0).get(), victim), "2030-01-01T00:00:00+00:00".into());
    });
    crate::antinuke::on_audit(&t.app, &member_entry(&t, 24, actor, victim))
        .await
        .unwrap();
    assert!(saved(&t, 0, actor).await.is_none());
    assert!(t
        .mock
        .lock()
        .unwrap()
        .timeouts
        .contains_key(&(t.g(0).get(), victim)));
    assert_eq!(t.calls(&format!("POST /channels/{LOGS}/messages")), 0);
    t.cleanup().await;
}

/// H3 with anti-nuke off: an equal-rank timeout is removed and logged, nobody quarantined.
#[tokio::test]
async fn anti_nuke_off_undoes_an_equal_rank_timeout_without_punishment() {
    let Some(t) = T::new(953).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (mods, _) = listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (actor, victim) = (t.base * 10 + 1, t.base * 10 + 2);
    members_in(&t, mods, &[actor, victim]).await;
    t.set(|m| {
        m.timeouts
            .insert((t.g(0).get(), victim), "2030-01-01T00:00:00+00:00".into());
    });
    crate::antinuke::on_audit(&t.app, &member_entry(&t, 24, actor, victim))
        .await
        .unwrap();
    assert!(
        saved(&t, 0, actor).await.is_none(),
        "anti-nuke off punishes nobody"
    );
    assert!(
        !t.mock
            .lock()
            .unwrap()
            .timeouts
            .contains_key(&(t.g(0).get(), victim)),
        "timeout removed"
    );
    let sent = t.mock.lock().unwrap().log.clone();
    let logs = sent
        .iter()
        .filter(|l| l.starts_with(&format!("POST /channels/{LOGS}/messages")))
        .count();
    assert_eq!(logs, 1, "{sent:?}");
    assert!(sent.iter().any(|l| l.contains("Anti-nuke is off")));
    t.cleanup().await;
}

/// `member_entry` with a snowflake id made at `ms` (Unix milliseconds).
fn member_entry_at(t: &T, kind: u16, actor: u64, target: u64, ms: i64) -> AuditLogEntry {
    let mut e = member_entry(t, kind, actor, target);
    e.id = Id::new(((ms - 1_420_070_400_000) as u64) << 22);
    e
}

/// Whether any request names `text` (a log, a DM).
fn said(t: &T, text: &str) -> bool {
    t.mock.lock().unwrap().log.iter().any(|l| l.contains(text))
}

/// Mods permit holders `actor` and `victim`, in the mock, anti-nuke on.
async fn rank_server(t: &T, cfg: Value) -> (i64, i64, u64, u64) {
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    let mut c = json!({ "antinuke": an });
    if let (Some(c), Some(extra)) = (c.as_object_mut(), cfg.as_object()) {
        c.extend(extra.clone());
    }
    server(t, 0, c).await;
    let (mods, links) = listed_permits(t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (actor, victim) = (t.base * 10 + 1, t.base * 10 + 2);
    members_in(t, mods, &[actor, victim]).await;
    (mods, links, actor, victim)
}

/// R2: an equal-rank kick replayed after an outage of more than 5 minutes is not judged.
#[tokio::test]
async fn a_replayed_old_kick_is_not_judged_by_rank() {
    let Some(t) = T::new(954).await else { return };
    let (_, _, actor, victim) = rank_server(&t, json!({"verification": {"channel": "700"}})).await;
    let old = crate::automod::now_ms() - 6 * 60 * 1000;
    let e = member_entry_at(&t, 20, actor, victim, old);
    crate::antinuke::on_replayed(&t.app, &e).await.unwrap();
    assert!(saved(&t, 0, actor).await.is_none());
    assert_eq!(t.calls("POST /channels/700/invites"), 0);
    assert!(!said(&t, "same or a higher level"));
    t.cleanup().await;
}

/// R3: an action on oneself is no breach, and an actor who left is judged by the level they had.
#[tokio::test]
async fn rank_skips_self_and_judges_a_leaver_actor_by_the_left_level() {
    let Some(t) = T::new(955).await else { return };
    let (_, links, actor, _) = rank_server(&t, json!({})).await;
    t.set(|m| {
        m.timeouts
            .insert((t.g(0).get(), actor), "2030-01-01T00:00:00+00:00".into());
    });
    crate::antinuke::on_audit(&t.app, &member_entry(&t, 24, actor, actor))
        .await
        .unwrap();
    assert!(t
        .mock
        .lock()
        .unwrap()
        .timeouts
        .contains_key(&(t.g(0).get(), actor)));
    assert!(saved(&t, 0, actor).await.is_none());
    // A Mods holder kicks a Links holder, then leaves before the entry comes in.
    let low = t.base * 10 + 3;
    members_in(&t, links, &[low]).await;
    for u in [low, actor] {
        t.set(|m| {
            m.members.remove(&(t.g(0).get(), u));
        });
        crate::permit_sync::on_leave(&t.app, t.g(0), Id::new(u), false)
            .await
            .unwrap();
    }
    crate::antinuke::on_audit(&t.app, &member_entry(&t, 20, actor, low))
        .await
        .unwrap();
    assert!(!said(&t, "same or a higher level"));
    t.cleanup().await;
}

/// R4: a member who left by themselves and is then banned by id is no breach: the ban came well
/// after the leave.
#[tokio::test]
async fn a_ban_after_a_voluntary_leave_is_not_judged_by_the_left_level() {
    let Some(t) = T::new(956).await else { return };
    let (_, links, _, victim) = rank_server(&t, json!({})).await;
    let low = t.base * 10 + 3;
    members_in(&t, links, &[low]).await;
    t.set(|m| {
        m.members.remove(&(t.g(0).get(), victim));
        m.bans.insert((t.g(0).get(), victim), "x".into());
    });
    crate::permit_sync::on_leave(&t.app, t.g(0), Id::new(victim), false)
        .await
        .unwrap();
    let later = crate::automod::now_ms() + 6000;
    let e = member_entry_at(&t, 22, low, victim, later);
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert!(saved(&t, 0, low).await.is_none());
    assert!(t
        .mock
        .lock()
        .unwrap()
        .bans
        .contains_key(&(t.g(0).get(), victim)));
    assert!(!said(&t, "same or a higher level"));
    t.cleanup().await;
}

/// R5: no invite for a bot, none for a ban that stays, one per victim in 10 minutes, made on the
/// system channel when there is no verification channel.
#[tokio::test]
async fn reinvite_skips_bots_failed_unbans_and_repeats() {
    let Some(t) = T::new(957).await else { return };
    let (mods, _, actor, victim) = rank_server(&t, json!({})).await;
    let (bot, banned) = (t.base * 10 + 3, t.base * 10 + 4);
    members_in(&t, mods, &[bot, banned]).await;
    t.set(|m| {
        m.bots.push(bot);
        m.system_channels.insert(t.g(0).get(), 702);
        for u in [victim, bot, banned] {
            m.members.remove(&(t.g(0).get(), u));
        }
    });
    // `banned` is not banned in the mock, so the unban fails.
    for (kind, u) in [(20, bot), (22, banned), (20, victim), (20, victim)] {
        crate::antinuke::on_audit(&t.app, &member_entry(&t, kind, actor, u))
            .await
            .unwrap();
    }
    assert_eq!(t.calls("POST /channels/702/invites"), 1);
    let log = t.mock.lock().unwrap().log.clone();
    assert_eq!(log.iter().filter(|l| l.contains("/invites ")).count(), 1);
    assert_eq!(log.iter().filter(|l| l.contains("discord.gg/")).count(), 1);
    t.cleanup().await;
}

/// R6: an immune actor of the same level is undone, not quarantined, and the log says so.
#[tokio::test]
async fn an_immune_actor_of_the_same_level_is_undone_not_quarantined() {
    let Some(t) = T::new(958).await else { return };
    rank_server(&t, json!({})).await;
    t.exec("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $1 * 10 + 5), ($1, $1 * 10 + 6)")
        .await;
    let (ta, other) = (t.base * 10 + 5, t.base * 10 + 6);
    t.set(|m| {
        m.members.insert((t.g(0).get(), ta), vec![]);
        m.members.insert((t.g(0).get(), other), vec![]);
        m.timeouts
            .insert((t.g(0).get(), other), "2030-01-01T00:00:00+00:00".into());
    });
    crate::antinuke::on_audit(&t.app, &member_entry(&t, 24, ta, other))
        .await
        .unwrap();
    assert!(saved(&t, 0, ta).await.is_none());
    assert!(!t
        .mock
        .lock()
        .unwrap()
        .timeouts
        .contains_key(&(t.g(0).get(), other)));
    assert!(said(&t, "undone, the actor is immune"));
    t.cleanup().await;
}

/// A bot's own (managed) role, for the dangerous grant tests.
const BOT_ROLE: u64 = 530;

/// Server `i` with anti-nuke on and strict mode off, and `bot` holding its own role `BOT_ROLE`
/// with `perms`.
async fn bot_server(t: &T, i: u64, bot: u64, perms: u64) {
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(t, i, json!({ "antinuke": an })).await;
    let g = t.g(i).get();
    t.set(|m| {
        m.bots.push(bot);
        m.bot_roles.insert(BOT_ROLE, bot);
        m.roles
            .get_mut(&g)
            .unwrap()
            .push((BOT_ROLE, true, perms, 6));
        m.members.insert((g, bot), vec![BOT_ROLE]);
    });
}

fn perms_in(t: &T, i: u64, role: u64) -> u64 {
    t.mock.lock().unwrap().roles[&t.g(i).get()]
        .iter()
        .find(|r| r.0 == role)
        .unwrap()
        .2
}

/// Owner 2026-11-15: with strict mode off, giving a role Administrator is undone and punished
/// unless an Owner, Extra Owner or Trusted Admin did it; the `antinuke` whitelist flag exempts
/// nobody, a bot least of all.
#[tokio::test]
async fn a_dangerous_role_grant_needs_immunity_not_strict_mode() {
    let Some(t) = T::new(968).await else { return };
    let (plain, ta, flagged, bot) = (t.base * 10 + 1, 951, t.base * 10 + 3, t.base * 10 + 4);
    for i in 0..4 {
        bot_server(&t, i, bot, if i == 3 { 8 } else { 0 }).await;
    }
    t.exec("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1 + 1, 951)")
        .await;
    // 504 = AUTOROLE (the human's permit role), 530 = BOT_ROLE.
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1 + 1, $1 + 2, 'Wl', '{}', '{antinuke}'), ($1 + 2, $1 + 3, 'Wl', '{}', '{antinuke}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1 + 2, 504), ($1 + 2, $1 + 3, 530)")
        .await;
    for (i, actor) in [(0, plain), (1, ta), (2, flagged), (3, bot)] {
        let g = t.g(i).get();
        t.set(|m| {
            if actor != bot {
                m.members.insert((g, actor), vec![AUTOROLE]);
            }
            m.roles.get_mut(&g).unwrap()[2].2 = 8; // ROLE, as the actor left it
        });
        let mut e = admin_given(&t, actor, ROLE);
        e.guild_id = Some(t.g(i));
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    }
    assert_eq!(perms_in(&t, 0, ROLE), 0, "no permit: undone");
    assert_eq!(roles(&t, 0, plain), Some(vec![Q]), "and quarantined");
    assert_eq!(perms_in(&t, 1, ROLE), 8, "a Trusted Admin may");
    assert_eq!(roles(&t, 1, ta), Some(vec![AUTOROLE]));
    assert_eq!(
        perms_in(&t, 2, ROLE),
        0,
        "the antinuke flag exempts no human"
    );
    assert_eq!(roles(&t, 2, flagged), Some(vec![Q]));
    assert_eq!(perms_in(&t, 3, ROLE), 0, "nor a bot");
    assert_eq!(perms_in(&t, 3, BOT_ROLE), 0, "the bot is contained");
    assert_eq!(roles(&t, 3, bot), Some(vec![Q, BOT_ROLE]));
    t.exec("DELETE FROM permits WHERE guild_id BETWEEN $1 AND $1 + 9")
        .await;
    t.cleanup().await;
}

/// Owner 2026-11-15: a role with Administrator given to a bot is taken off, and a bot added with
/// Administrator on its own role loses it, unless an Owner, Extra Owner or Trusted Admin did it.
/// The bot role's RoleCreate entry is left to the BotAdd entry, so one bot add is handled once.
#[tokio::test]
async fn a_bot_gets_dangerous_permissions_only_from_immune_staff() {
    let Some(t) = T::new(952).await else { return };
    let (actor, bot) = (t.base * 10 + 1, t.base * 10 + 2);
    for (i, perms) in [(0, 0), (1, 8), (2, 8)] {
        bot_server(&t, i, bot, perms).await;
        let (g, owner) = (t.g(i).get(), t.owner());
        t.set(|m| {
            m.members.insert((g, actor), vec![]);
            m.members.insert((g, owner), vec![]);
        });
    }
    // A role with Administrator given to the bot.
    let g0 = t.g(0).get();
    t.set(|m| {
        m.roles.get_mut(&g0).unwrap()[2].2 = 8;
        m.members.insert((g0, bot), vec![BOT_ROLE, ROLE]);
    });
    let e: AuditLogEntry = serde_json::from_value(json!({
        "id": "5", "action_type": 25, "guild_id": g0.to_string(), "target_id": bot.to_string(),
        "user_id": actor.to_string(), "reason": null,
        "changes": [{"key": "$add", "new_value": [{"id": ROLE.to_string(), "name": "r"}]}],
    }))
    .unwrap();
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(roles(&t, 0, bot), Some(vec![BOT_ROLE]), "taken off the bot");
    assert_eq!(
        roles(&t, 0, actor),
        Some(vec![Q]),
        "the actor is quarantined"
    );
    // A bot added with Administrator: its role's RoleCreate entry (naming the bot or the adder),
    // then the BotAdd entry.
    for (i, adder) in [(1, actor), (2, t.owner())] {
        let g = t.g(i).to_string();
        for by in [bot, adder] {
            let e: AuditLogEntry = serde_json::from_value(json!({
                "id": "6", "action_type": 30, "guild_id": g, "target_id": BOT_ROLE.to_string(),
                "user_id": by.to_string(), "reason": null,
                "changes": [{"key": "permissions", "new_value": "8"}],
            }))
            .unwrap();
            crate::antinuke::on_audit(&t.app, &e).await.unwrap();
        }
        let e: AuditLogEntry = serde_json::from_value(json!({
            "id": "7", "action_type": 28, "guild_id": g, "target_id": bot.to_string(),
            "user_id": adder.to_string(), "reason": null, "changes": [],
        }))
        .unwrap();
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    }
    let edits = |i| t.calls(&format!("PATCH /guilds/{}/roles/{BOT_ROLE}", t.g(i)));
    assert_eq!(perms_in(&t, 1, BOT_ROLE), 0, "added by a member: cleared");
    assert_eq!(edits(1), 1, "once, on the BotAdd entry");
    assert_eq!(
        roles(&t, 1, actor),
        Some(vec![Q]),
        "the adder is quarantined"
    );
    // Owner decision 2026-11-15: an app added by anyone but an Owner, Extra Owner or Trusted Admin goes.
    assert_eq!(roles(&t, 1, bot), None, "the bot is removed");
    assert!(roles(&t, 2, bot).is_some(), "added by the owner: stays");
    assert_eq!(perms_in(&t, 2, BOT_ROLE), 8, "added by the owner: kept");
    assert_eq!(edits(2), 0);
    t.cleanup().await;
}

/// Owner 2026-11-15 (A): a channel overwrite that gains dangerous allow bits, for any role or
/// member, is undone unless an Owner, Extra Owner or Trusted Admin made it; bots get no exception.
#[tokio::test]
async fn a_dangerous_channel_overwrite_needs_immunity() {
    let Some(t) = T::new(960).await else { return };
    let (plain, ta, bot, x) = (t.base * 10 + 1, 961, t.base * 10 + 3, t.base * 10 + 4);
    const MANAGE_MESSAGES: u64 = 1 << 13;
    const MANAGE_CHANNELS: u64 = 1 << 4;
    let off = json!({"enabled": false, "quarantine_role": Q.to_string()});
    for i in 0..6 {
        match i {
            4 => server(&t, i, json!({ "antinuke": off })).await,
            _ => bot_server(&t, i, bot, 0).await,
        }
    }
    t.exec("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1 + 2, 961)")
        .await;
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1 + 1, $1 + 3, 'Wl', '{}', '{antinuke}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1 + 3, 530)")
        .await;
    // (server, actor, overwrite target, member overwrite, gained bit, live allow now)
    let cases = [
        (0, plain, ROLE, false, MANAGE_MESSAGES, MANAGE_MESSAGES),
        (1, plain, x, true, MANAGE_CHANNELS, MANAGE_CHANNELS),
        (2, ta, ROLE, false, MANAGE_MESSAGES, MANAGE_MESSAGES),
        (3, bot, ROLE, false, MANAGE_MESSAGES, MANAGE_MESSAGES),
        (4, plain, ROLE, false, MANAGE_MESSAGES, MANAGE_MESSAGES),
        (5, plain, ROLE, false, MANAGE_MESSAGES, 0), // changed again since
    ];
    for (i, actor, target, member, bit, live) in cases {
        let (g, c) = (t.g(i).get(), 800 + i);
        let kind = if member { 1 } else { 0 };
        t.set(|m| {
            if actor != bot {
                m.members.insert((g, actor), vec![AUTOROLE]);
            }
            m.overwrites.insert(
                c,
                vec![json!({"id": target.to_string(), "type": kind,
                "allow": live.to_string(), "deny": "1024"})],
            );
        });
        // Server 1 is a create (no old value), the rest are updates.
        let (action, change) = if i == 1 {
            (13, json!({"key": "allow", "new_value": bit.to_string()}))
        } else {
            (
                14,
                json!({"key": "allow", "old_value": "0", "new_value": bit.to_string()}),
            )
        };
        let e: AuditLogEntry = serde_json::from_value(json!({
            "id": "8", "action_type": action, "guild_id": g.to_string(), "target_id": c.to_string(),
            "user_id": actor.to_string(), "reason": null,
            "options": {"id": target.to_string(), "type": kind.to_string()},
            "changes": [change],
        }))
        .unwrap();
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    }
    let put =
        |i: u64, target: u64| t.calls(&format!("PUT /channels/{}/permissions/{target} ", 800 + i));
    assert_eq!(put(0, ROLE), 1, "a staff role's overwrite is set back");
    assert_eq!(
        t.calls(r#""allow":"0","deny":"1024""#),
        3,
        "keeping the deny (servers 0, 3, 4)"
    );
    assert_eq!(
        roles(&t, 0, plain),
        Some(vec![Q]),
        "and the actor quarantined"
    );
    assert_eq!(
        t.calls(&format!("DELETE /channels/801/permissions/{x}")),
        1,
        "a member overwrite is removed"
    );
    assert_eq!(roles(&t, 1, plain), Some(vec![Q]));
    assert_eq!(put(2, ROLE), 0, "a Trusted Admin may");
    assert_eq!(roles(&t, 2, ta), Some(vec![AUTOROLE]));
    assert_eq!(put(3, ROLE), 1, "the antinuke flag exempts no bot");
    assert_eq!(
        roles(&t, 3, bot),
        Some(vec![Q, BOT_ROLE]),
        "the bot is contained"
    );
    assert_eq!(put(4, ROLE), 1, "anti-nuke off: undone");
    assert_eq!(
        roles(&t, 4, plain),
        Some(vec![AUTOROLE]),
        "but not punished"
    );
    assert_eq!(put(5, ROLE), 0, "changed again since: left");
    t.exec("DELETE FROM permits WHERE guild_id BETWEEN $1 AND $1 + 9")
        .await;
    t.cleanup().await;
}

/// Review 11-15: with anti-nuke off, a role that is dangerous and carries a permit, given again
/// and again (a reaction-role bot), is taken back each time but logged once; a failed undo says so.
#[tokio::test]
async fn a_dangerous_take_back_is_logged_once_and_a_failed_undo_says_so() {
    let Some(t) = T::new(962).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (actor, u) = (t.base * 10 + 1, t.base * 10 + 2);
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1 + 1, $1, 'Mod', '{cases}', '{}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 501)")
        .await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![]);
        m.roles.get_mut(&t.g(0).get()).unwrap()[2].2 = 8;
    });
    for _ in 0..3 {
        t.set(|m| {
            m.members.insert((t.g(0).get(), u), vec![ROLE]);
        });
        crate::antinuke::on_audit(&t.app, &role_added(&t, actor, u, ROLE))
            .await
            .unwrap();
        assert_eq!(roles(&t, 0, u), Some(vec![]), "taken back each time");
    }
    assert_eq!(
        t.calls(&format!("POST /channels/{LOGS}/messages")),
        1,
        "logged once"
    );
    t.set(|m| m.fail_role_edit = true);
    crate::antinuke::on_audit(&t.app, &admin_given(&t, actor, ROLE))
        .await
        .unwrap();
    assert_eq!(t.calls("NOT undone"), 1);
    t.exec("DELETE FROM permits WHERE guild_id BETWEEN $1 AND $1 + 9")
        .await;
    t.cleanup().await;
}

/// Auto-dehoist renames a hoisted name once (its own echo does not rename again), and leaves a
/// plain name and the owner alone.
#[tokio::test]
async fn auto_dehoist_renames_a_hoisted_name_once_and_skips_the_owner() {
    let Some(t) = T::new(880).await else { return };
    server(&t, 0, json!({"misc": {"auto_dehoist": true}})).await;
    let user = |id: u64, name: &str| -> twilight_model::user::User {
        serde_json::from_value(json!({
            "id": id.to_string(), "username": name, "discriminator": "0", "avatar": null,
        }))
        .unwrap()
    };
    let (gid, u) = (t.g(0), t.base * 10 + 1);
    t.set(|m| {
        for id in [u, u + 1, t.owner()] {
            m.members.insert((gid.get(), id), vec![]);
        }
    });
    let renames = |id: u64| t.calls(&format!("PATCH /guilds/{gid}/members/{id}"));
    let hoisted = user(u, "!!!hoist");
    crate::dehoist::check(&t.app, gid, &hoisted, None, &[])
        .await
        .unwrap();
    assert_eq!(renames(u), 1);
    crate::dehoist::check(&t.app, gid, &hoisted, None, &[])
        .await
        .unwrap();
    assert_eq!(
        renames(u),
        1,
        "the echo of our own rename does not rename again"
    );
    let plain = user(u + 1, "plain");
    crate::dehoist::check(&t.app, gid, &plain, None, &[])
        .await
        .unwrap();
    assert_eq!(renames(u + 1), 0);
    let owner = user(t.owner(), "!!!boss");
    crate::dehoist::check(&t.app, gid, &owner, None, &[])
        .await
        .unwrap();
    assert_eq!(renames(t.owner()), 0, "the owner keeps their name");
    t.cleanup().await;
}

/// Final audit 2, review 1 and 7: a nuke run whose actor level cannot be read reverts, contains
/// nobody, opens no quarantine case, says so, and fails, so the guard job asks again. The retry
/// that reads the level contains the actor and tells staff, though the nuke was reported already.
#[tokio::test]
async fn a_nuke_run_with_an_unread_level_contains_nobody_until_the_retry() {
    let Some(t) = T::new(988).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let actor = t.base * 10 + 1;
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![ROLE]);
    });
    // Cached, so only the actor's member read fails.
    t.app.guild_info(t.g(0)).await.unwrap();
    t.set(|m| {
        m.fail_member = true;
    });
    // Something to revert: a ban the actor made.
    sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse, counted) VALUES ($1, $2, 'Ban', 7, $3, 1)")
        .bind(t.g(0).get() as i64)
        .bind(actor as i64)
        .bind(json!({"op": "unban", "user": 7}))
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.bans.insert((t.g(0).get(), 7), "x".into());
    });
    let since = crate::app::now_secs() - 60;
    let r =
        crate::antinuke::resume_nuke(&t.app, t.g(0), Id::new(actor), "test", since, false).await;
    assert!(r.is_err(), "the guard job must retry: {r:?}");
    assert_eq!(roles(&t, 0, actor), Some(vec![ROLE]), "not contained");
    assert!(
        !t.mock.lock().unwrap().bans.contains_key(&(t.g(0).get(), 7)),
        "reverted anyway"
    );
    assert!(saved(&t, 0, actor).await.is_none());
    assert!(said(&t, "level could not be read"));
    assert!(said(&t, "nobody was quarantined"), "no quarantine case");
    let cases: i64 = sqlx::query_scalar("SELECT count(*) FROM cases WHERE guild_id = $1")
        .bind(t.g(0).get() as i64)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
    assert_eq!(cases, 0);
    t.set(|m| m.fail_member = false);
    let r = crate::antinuke::resume_nuke(&t.app, t.g(0), Id::new(actor), "test", since, true).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(roles(&t, 0, actor), Some(vec![Q]), "contained by the retry");
    assert!(said(&t, "contained by a retry"));
    t.cleanup().await;
}

/// Review 2: an actor whose level reads as immune (counted only because a whitelist lookup
/// failed) is not reverted, gets no case, and is not reported.
#[tokio::test]
async fn a_nuke_run_against_an_immune_actor_does_nothing() {
    let Some(t) = T::new(990).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let owner = t.owner();
    sqlx::query("INSERT INTO action_log (guild_id, actor_id, action, target_id, inverse, counted) VALUES ($1, $2, 'Ban', 7, $3, 1)")
        .bind(t.g(0).get() as i64)
        .bind(owner as i64)
        .bind(json!({"op": "unban", "user": 7}))
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.bans.insert((t.g(0).get(), 7), "x".into());
    });
    let since = crate::app::now_secs() - 60;
    let r =
        crate::antinuke::resume_nuke(&t.app, t.g(0), Id::new(owner), "test", since, false).await;
    assert!(r.is_ok(), "{r:?}");
    assert!(
        t.mock.lock().unwrap().bans.contains_key(&(t.g(0).get(), 7)),
        "not reverted"
    );
    assert!(said(&t, "nothing was undone"));
    assert!(!said(&t, "Anti Nuke Report"));
    let cases: i64 = sqlx::query_scalar("SELECT count(*) FROM cases WHERE guild_id = $1")
        .bind(t.g(0).get() as i64)
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
    assert_eq!(cases, 0);
    t.cleanup().await;
}

/// Final audit 10: a ban of a permit holder by an actor whose level cannot be read is undone
/// and logged, and nobody is punished.
#[tokio::test]
async fn a_rank_breach_with_an_unread_actor_level_is_undone_without_punishment() {
    let Some(t) = T::new(989).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (mods, _) = listed_permits(&t, 0).await;
    t.exec("INSERT INTO permit_members_migrated (guild_id) VALUES ($1)")
        .await;
    let (actor, victim) = (t.base * 10 + 1, t.base * 10 + 2);
    members_in(&t, mods, &[victim]).await;
    t.set(|m| {
        m.members.remove(&(t.g(0).get(), victim));
        m.members.insert((t.g(0).get(), actor), vec![]);
        m.bans.insert((t.g(0).get(), victim), "x".into());
    });
    // Cached, so only the actor's member read fails.
    t.app.guild_info(t.g(0)).await.unwrap();
    t.set(|m| m.fail_member = true);
    let r = crate::antinuke::on_audit(&t.app, &member_entry(&t, 22, actor, victim)).await;
    tracing::info!(?r, "on_audit");
    assert!(
        !t.mock
            .lock()
            .unwrap()
            .bans
            .contains_key(&(t.g(0).get(), victim)),
        "unbanned"
    );
    assert!(saved(&t, 0, actor).await.is_none(), "not punished");
    assert!(said(&t, "a level could not be read"));
    t.cleanup().await;
}

/// Final review 1: a live run takes over the actor's pending guard job, also one a failed run
/// left leased, so the retry is due in 2 minutes, not when the lease ends.
#[tokio::test]
async fn a_live_nuke_run_takes_over_a_leased_guard_job() {
    let Some(t) = T::new(991).await else { return };
    let an = json!({"enabled": true, "quarantine_role": Q.to_string(),
                    "limits": {"Ban": {"per_minute": 1, "per_hour": 1}}});
    server(&t, 0, json!({ "antinuke": an })).await;
    let actor = t.base * 10 + 1;
    let g = t.g(0).get() as i64;
    sqlx::query("INSERT INTO scheduled_jobs (guild_id, kind, payload, run_at, locked_until) VALUES ($1, 'nuke_revert', $2, now(), now() + interval '900 seconds')")
        .bind(g)
        .bind(json!({"actor": actor, "why": "x", "since": 0}))
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.members.insert((t.g(0).get(), actor), vec![ROLE]);
        m.fail_member = true;
    });
    let cfg = t.app.cfg(t.g(0).get()).await.unwrap();
    let r = crate::antinuke::count(
        &t.app,
        &cfg,
        t.g(0),
        Id::new(actor),
        brim_antinuke::Action::Ban,
        Some(9),
        &[],
        false,
        5,
    )
    .await;
    tracing::info!(?r, "count");
    let jobs: Vec<(bool, bool)> = sqlx::query_as(
        "SELECT locked_until IS NULL, run_at > now() + interval '60 seconds' FROM scheduled_jobs
         WHERE guild_id = $1 AND kind = 'nuke_revert'",
    )
    .bind(g)
    .fetch_all(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(
        jobs,
        vec![(true, true)],
        "one guard job, lease dropped, due again soon"
    );
    t.cleanup().await;
}

/// Owner decision 11-17, option A: a permit's `quarantine` flag lets its holder give roles to a held
/// ordinary member, but a held member whose saved roles carry a permit goes free only through an
/// Owner or Extra Owner.
#[tokio::test]
async fn a_quarantine_flag_does_not_free_held_staff() {
    let Some(t) = T::new(992).await else { return };
    let an = json!({"enabled": false, "quarantine_role": Q.to_string()});
    server(&t, 0, json!({ "antinuke": an })).await;
    let (staff, member, actor) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
    // ROLE: the actor's permit with the quarantine flag; VERIFIED: a staff member's ban permit.
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1 + 1, $1, 'Helpers', '{}', '{quarantine}'), ($1 + 2, $1, 'Mods', '{ban}', '{}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 501), ($1 + 2, $1, 503)")
        .await;
    // The staff member is listed in Mods (a role alone gives nothing); the actor in Helpers.
    t.exec("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1 + 2, $1, $1 * 10 + 1), ($1 + 1, $1, $1 * 10 + 3)")
        .await;
    row(&t, 0, staff, &[VERIFIED as i64]).await;
    row(&t, 0, member, &[]).await;
    t.set(|m| {
        m.members.insert((t.g(0).get(), staff), vec![Q, AUTOROLE]);
        m.members.insert((t.g(0).get(), member), vec![Q, AUTOROLE]);
        m.members.insert((t.g(0).get(), actor), vec![ROLE]);
    });
    crate::antinuke::on_audit(&t.app, &role_added(&t, actor, staff, AUTOROLE))
        .await
        .unwrap();
    assert_eq!(roles(&t, 0, staff), Some(vec![Q]), "held staff stay held");
    crate::antinuke::on_audit(&t.app, &role_added(&t, actor, member, AUTOROLE))
        .await
        .unwrap();
    assert_eq!(
        roles(&t, 0, member),
        Some(vec![Q, AUTOROLE]),
        "an ordinary member may be given roles"
    );
    t.cleanup().await;
}

/// Reverting a role removal gives back only plain roles, and nothing to a member quarantined now.
#[tokio::test]
async fn a_revert_gives_back_no_staff_role_and_nothing_to_a_held_member() {
    let Some(t) = T::new(993).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"quarantine_role": Q.to_string()}}),
    )
    .await;
    let g = t.g(0);
    let (member, held) = (t.base * 10 + 1, t.base * 10 + 2);
    t.exec("INSERT INTO permits (id, guild_id, name, perms) VALUES ($1 + 1, $1, 'Mods', '{ban}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 503)")
        .await;
    row(&t, 0, held, &[]).await;
    t.set(|m| {
        m.members.insert((g.get(), member), vec![]);
        m.members.insert((g.get(), held), vec![Q]);
        m.roles.get_mut(&g.get()).unwrap()[2].2 = 8; // ROLE has Administrator
    });
    let give = |user: u64| brim_antinuke::Inverse::AddRoles {
        user,
        roles: vec![ROLE, VERIFIED, AUTOROLE],
    };
    crate::antinuke::run_inverse(&t.app, g, &give(member))
        .await
        .unwrap();
    let put = |u: u64, r: u64| t.calls(&format!("PUT /guilds/{g}/members/{u}/roles/{r}"));
    assert_eq!(put(member, AUTOROLE), 1, "a plain role comes back");
    assert_eq!(put(member, ROLE), 0, "not a dangerous one");
    assert_eq!(put(member, VERIFIED), 0, "not a permit role");
    crate::antinuke::run_inverse(&t.app, g, &give(held))
        .await
        .unwrap();
    assert_eq!(put(held, AUTOROLE), 0, "nothing to a quarantined member");
    t.cleanup().await;
}

/// A Trusted Admin may not hand out a role of a permit with the anti-nuke whitelist, also when
/// that role has dangerous permissions (which `guard_perms` spares for them); nobody is punished.
#[tokio::test]
async fn a_trusted_admin_cannot_give_a_dangerous_owner_only_permit_role() {
    let Some(t) = T::new(994).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"enabled": true, "quarantine_role": Q.to_string()}}),
    )
    .await;
    let g = t.g(0);
    let (ta, alt) = (t.base * 10 + 1, t.base * 10 + 2);
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1 + 1, $1, 'Admins', '{ban}', '{antinuke}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 1, $1, 501)")
        .await;
    sqlx::query("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1, $2)")
        .bind(g.get() as i64)
        .bind(ta as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    t.set(|m| {
        m.members.insert((g.get(), ta), vec![]);
        m.members.insert((g.get(), alt), vec![ROLE]);
        m.roles.get_mut(&g.get()).unwrap()[2].2 = 8; // ROLE has Administrator
    });
    crate::antinuke::on_audit(&t.app, &role_added(&t, ta, alt, ROLE))
        .await
        .unwrap();
    assert_eq!(
        t.calls(&format!("DELETE /guilds/{g}/members/{alt}/roles/{ROLE}")),
        1,
        "taken back"
    );
    assert_eq!(
        saved(&t, 0, ta).await,
        None,
        "the Trusted Admin is not quarantined"
    );
    t.cleanup().await;
}

/// Owner decision 2026-11-18: only an Owner or Extra Owner gives the quarantine role permissions; a Trusted
/// Admin's grant is undone, and they are not punished.
#[tokio::test]
async fn only_owners_give_the_quarantine_role_power() {
    let Some(t) = T::new(995).await else { return };
    server(
        &t,
        0,
        json!({"antinuke": {"enabled": true, "quarantine_role": Q.to_string()}}),
    )
    .await;
    let g = t.g(0);
    let (ta, eo) = (t.base * 10 + 1, t.base * 10 + 2);
    for (table, u) in [("trusted_admins", ta), ("extra_owners", eo)] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (guild_id, user_id) VALUES ($1, $2)"
        )))
        .bind(g.get() as i64)
        .bind(u as i64)
        .execute(&t.app.pool)
        .await
        .unwrap();
    }
    let q_admin = |t: &T| {
        t.set(|m| {
            m.members.insert((g.get(), ta), vec![]);
            m.members.insert((g.get(), eo), vec![]);
            let r = m.roles.get_mut(&g.get()).unwrap();
            r.iter_mut().find(|r| r.0 == Q).unwrap().2 = 8;
        })
    };
    q_admin(&t);
    crate::antinuke::on_audit(&t.app, &admin_given(&t, ta, Q))
        .await
        .unwrap();
    assert_eq!(role_perms(&t, Q), 0, "a Trusted Admin's grant is undone");
    assert_eq!(saved(&t, 0, ta).await, None, "and not punished");
    q_admin(&t);
    crate::antinuke::on_audit(&t.app, &admin_given(&t, eo, Q))
        .await
        .unwrap();
    assert_eq!(role_perms(&t, Q), 8, "an Extra Owner's stays");
    t.cleanup().await;
}

/// Owner decision 2026-11-20 (one list of 15): the dangerous Discord permissions added to the list (here
/// View Audit Log and Manage Nicknames) are guarded like the rest. A role gaining one is undone
/// and the actor quarantined, unless an Owner, Extra Owner or Trusted Admin or Brim itself
/// made the change; giving a member a role that holds one is taken back the same way. A role that
/// already holds one is left alone by an unrelated change to it.
#[tokio::test]
async fn the_added_dangerous_permissions_are_guarded_like_the_rest() {
    let Some(t) = T::new(945).await else { return };
    const VIEW_AUDIT_LOG: u64 = 1 << 7;
    const MANAGE_NICKNAMES: u64 = 1 << 27;
    let (plain, ta, member) = (t.base * 10 + 1, 9451, t.base * 10 + 3);
    for i in 0..7 {
        let an = json!({"enabled": true, "quarantine_role": Q.to_string()});
        server(&t, i, json!({ "antinuke": an })).await;
        let g = t.g(i).get();
        t.set(|m| {
            m.members.insert((g, plain), vec![]);
            m.members.insert((g, ta), vec![]);
            m.members.insert((g, member), vec![]);
        });
    }
    t.exec("INSERT INTO trusted_admins (guild_id, user_id) VALUES ($1 + 1, 9451)")
        .await;
    // Servers 0 to 2: ROLE gains View Audit Log, by a plain member, a Trusted Admin, Brim.
    for (i, actor) in [(0, plain), (1, ta), (2, ME)] {
        let g = t.g(i).get();
        t.set(|m| m.roles.get_mut(&g).unwrap()[2].2 = VIEW_AUDIT_LOG);
        let e: AuditLogEntry = serde_json::from_value(json!({
            "id": "3", "action_type": 31, "guild_id": g.to_string(), "target_id": ROLE.to_string(),
            "user_id": actor.to_string(), "reason": null,
            "changes": [{"key": "permissions", "old_value": "0", "new_value": VIEW_AUDIT_LOG.to_string()}],
        }))
        .unwrap();
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    }
    assert_eq!(perms_in(&t, 0, ROLE), 0, "a plain member: undone");
    assert_eq!(roles(&t, 0, plain), Some(vec![Q]), "and quarantined");
    assert_eq!(perms_in(&t, 1, ROLE), VIEW_AUDIT_LOG, "a Trusted Admin may");
    assert_eq!(roles(&t, 1, ta), Some(vec![]));
    assert_eq!(perms_in(&t, 2, ROLE), VIEW_AUDIT_LOG, "Brim's own change");
    // Server 3: a plain member gives a member ROLE, which holds Manage Nicknames.
    let g3 = t.g(3).get();
    t.set(|m| {
        m.roles.get_mut(&g3).unwrap()[2].2 = MANAGE_NICKNAMES;
        m.members.insert((g3, member), vec![ROLE]);
    });
    let e: AuditLogEntry = serde_json::from_value(json!({
        "id": "4", "action_type": 25, "guild_id": g3.to_string(), "target_id": member.to_string(),
        "user_id": plain.to_string(), "reason": null,
        "changes": [{"key": "$add", "new_value": [{"id": ROLE.to_string(), "name": "r"}]}],
    }))
    .unwrap();
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(roles(&t, 3, member), Some(vec![]), "the role is taken back");
    assert_eq!(
        roles(&t, 3, plain),
        Some(vec![Q]),
        "the giver is quarantined"
    );
    // Server 4: ROLE already holds Manage Nicknames; a plain member renames it. Nothing is undone.
    let g4 = t.g(4).get();
    t.set(|m| m.roles.get_mut(&g4).unwrap()[2].2 = MANAGE_NICKNAMES);
    let e: AuditLogEntry = serde_json::from_value(json!({
        "id": "5", "action_type": 31, "guild_id": g4.to_string(), "target_id": ROLE.to_string(),
        "user_id": plain.to_string(), "reason": null,
        "changes": [{"key": "name", "old_value": "a", "new_value": "b"}],
    }))
    .unwrap();
    crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    assert_eq!(perms_in(&t, 4, ROLE), MANAGE_NICKNAMES, "held before: kept");
    assert_eq!(roles(&t, 4, plain), Some(vec![]), "nobody punished");
    // Servers 5 and 6: Mention @everyone added to the Moderators permit role (AUTOROLE) is
    // allowed; added to another role (ROLE) it is undone.
    t.exec("INSERT INTO permits (id, guild_id, name, perms, whitelist) VALUES ($1 + 5, $1 + 5, 'Moderators', '{}', '{}'), ($1 + 6, $1 + 6, 'Moderators', '{}', '{}')")
        .await;
    t.exec("INSERT INTO permit_roles (permit_id, guild_id, role_id) VALUES ($1 + 5, $1 + 5, 504), ($1 + 6, $1 + 6, 504)")
        .await;
    const MENTION: u64 = 1 << 17;
    for (i, role) in [(5, AUTOROLE), (6, ROLE)] {
        let g = t.g(i).get();
        t.set(|m| {
            for r in m
                .roles
                .get_mut(&g)
                .unwrap()
                .iter_mut()
                .filter(|r| r.0 == role)
            {
                r.2 = MENTION;
            }
        });
        let e: AuditLogEntry = serde_json::from_value(json!({
            "id": "6", "action_type": 31, "guild_id": g.to_string(), "target_id": role.to_string(),
            "user_id": plain.to_string(), "reason": null,
            "changes": [{"key": "permissions", "old_value": "0", "new_value": MENTION.to_string()}],
        }))
        .unwrap();
        crate::antinuke::on_audit(&t.app, &e).await.unwrap();
    }
    assert_eq!(
        perms_in(&t, 5, AUTOROLE),
        MENTION,
        "a staff permit role keeps it"
    );
    assert_eq!(roles(&t, 5, plain), Some(vec![]));
    assert_eq!(perms_in(&t, 6, ROLE), 0, "another role: undone");
    assert_eq!(roles(&t, 6, plain), Some(vec![Q]));
    t.exec("DELETE FROM permits WHERE guild_id BETWEEN $1 AND $1 + 9")
        .await;
    t.cleanup().await;
}

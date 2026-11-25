use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    channel_access, schema, store, ApiError, AppState,
};
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use brim_antinuke::security::{log_overwrites, synced_children, LOG_LOCK};
use brim_logging::LogEvent;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/guilds/{guild}/schema", get(schema_route))
        .route("/api/guilds/{guild}/antinuke/coverage", get(coverage_route))
        .route("/api/guilds/{guild}/discord", get(discord_route))
        .route("/api/guilds/{guild}/members", get(members_route))
        .route("/api/guilds/{guild}/wizard/auto-setup", post(auto_setup))
        .route("/api/guilds/{guild}/wizard/done", post(wizard_done))
        .route(
            "/api/guilds/{guild}/logging/setup",
            get(log_setup_todo).post(log_setup),
        )
        .route(
            "/api/guilds/{guild}/wizard/verify-channel",
            post(verify_channel),
        )
        .route(
            "/api/guilds/{guild}/wizard/discord-settings",
            post(discord_settings),
        )
        .route(
            "/api/guilds/{guild}/verification/captcha-preview",
            post(captcha_preview),
        )
        .route("/api/guilds/{guild}/catch/setup", post(catch_channel))
        .route("/api/guilds/{guild}/tickets/setup", post(tickets_setup))
        .route(
            "/api/guilds/{guild}/tickets/log/{channel}/{message}",
            get(ticket_log),
        )
        .route(
            "/api/guilds/{guild}/tickets/panels/publish",
            post(crate::tickets::publish),
        )
        .route(
            "/api/guilds/{guild}/tickets/preview",
            post(crate::tickets::preview).layer(axum::extract::DefaultBodyLimit::max(256 * 1024)),
        )
        .route(
            "/api/guilds/{guild}/tickets/panels/posts",
            get(crate::tickets::posts),
        )
        .route(
            "/api/guilds/{guild}/tickets/panels/posts/{message}/delete",
            post(crate::tickets::unpost),
        )
        .route(
            "/api/guilds/{guild}/tickets/cases",
            get(crate::tickets::cases),
        )
        .route(
            "/api/guilds/{guild}/tickets/stats",
            get(crate::tickets::stats),
        )
}

async fn schema_route(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    Ok(Json(json!(schema::schema())))
}

/// The anti-nuke Rule coverage table; read like the settings it summarises (`get_config`).
async fn coverage_route(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let (_, config) = store::current(&st.db, guild).await?;
    let permits = store::read_permits(&mut *st.db.acquire().await?, guild).await?;
    Ok(Json(coverage(&config, &permits)))
}

/// One row per anti-nuke action: what the bot watches, at which limit, and which permits exempt it.
fn coverage(config: &Value, permits: &Value) -> Value {
    use brim_antinuke::Action;
    use brim_core::{permits::Level as Permit, sections::AntinukeCfg};
    // An unreadable section is read as the bot reads it: the defaults (off).
    let cfg: AntinukeCfg = serde_json::from_value(config["antinuke"].clone()).unwrap_or_default();
    let held: Vec<(Value, Permit)> = permits["custom"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let wl = store::strs(&c["whitelist"]);
            (
                c["name"].clone(),
                Permit::Custom {
                    rank: 0,
                    perms: vec![],
                    whitelist: wl,
                    limits: Default::default(),
                },
            )
        })
        .collect();
    let rows: Vec<Value> = Action::ALL
        .into_iter()
        .map(|a| {
            let key = format!("{a:?}");
            let (label, note) = match a {
                Action::Kick => ("Kicks", "Counts on the Kicks & Bans card."),
                Action::Prune => ("Member prunes", "No card: on with \"Detect and react to member prunes\"."),
                Action::RoleRemove => (
                    "Role removals from members",
                    "Heat only: counts when Panic Mode and its heat option are on.",
                ),
                Action::BotAdd => ("Bot additions", "No count. A bot whose own role has dangerous permissions is always stopped unless an Owner, Extra Owner or Trusted Admin adds it."),
                _ => (
                    schema::NUKE_CARDS.iter().find(|c| c.0 == key).map_or("", |c| c.1),
                    "",
                ),
            };
            let limit = cfg.limit(a);
            let caps = limit.filter(|_| !a.heat_only());
            let state = match limit {
                Some(_) => "on",
                None if a.opt_in() && !cfg.limits.contains_key(&a.card()) => "opt_in",
                None => "off",
            };
            let exempt: Vec<&Value> = held
                .iter()
                .filter(|(_, p)| p.whitelisted(a.whitelist_flag()))
                .map(|(n, _)| n)
                .collect();
            json!({
                "action": key, "label": label, "note": note, "state": state,
                "per_minute": caps.map(|l| l.0.per_minute), "per_hour": caps.map(|l| l.0.per_hour),
                "heat": limit.map(|l| l.1).filter(|h| *h > 0),
                "undo": a.undo(), "permits": exempt,
            })
        })
        .collect();
    json!({"enabled": cfg.enabled, "rows": rows})
}

async fn discord_route(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let list = |v: Value| v.as_array().cloned().unwrap_or_default();
    let channels = list(
        st.discord
            .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
            .await?,
    )
    .into_iter()
    .filter_map(|c| {
        let kind = match c["type"].as_i64()? {
            0 => "text",
            4 => "category",
            5 => "announcement",
            _ => return None,
        };
        Some(json!({"id": c["id"], "name": c["name"], "kind": kind}))
    })
    .collect::<Vec<_>>();
    let roles = list(st.discord.call(Method::GET, &format!("/guilds/{guild}/roles"), None).await?).into_iter()
        .map(|r| json!({"id": r["id"], "name": r["name"], "managed": r["managed"], "position": r["position"], "permissions": r["permissions"], "color": r["color"]}))
        .collect::<Vec<_>>();
    let info = st
        .discord
        .call(
            Method::GET,
            &format!("/guilds/{guild}?with_counts=true"),
            None,
        )
        .await?;
    // The wizard's score checks the bot's own roles.
    let bot_roles = st
        .discord
        .member_roles(guild, &st.discord.client_id)
        .await
        .unwrap_or_default();
    Ok(Json(
        json!({"channels": channels, "roles": roles, "members": info["approximate_member_count"], "bot_roles": bot_roles}),
    ))
}

#[derive(Deserialize)]
struct MemberQ {
    /// Name to search for.
    #[serde(default)]
    q: String,
    /// Or: comma-separated user ids to show by name.
    #[serde(default)]
    ids: String,
}

/// The user picker: members matching `q`, or the members behind `ids` (at most 25, unknown ids left out).
async fn members_route(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(m): Query<MemberQ>,
) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let found = if m.q.trim().is_empty() {
        let mut found = vec![];
        // Digits only: the id becomes part of the Discord path.
        let ids = m
            .ids
            .split(',')
            .filter(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()));
        for id in ids.take(25) {
            if let Ok(x) = st
                .discord
                .call(Method::GET, &format!("/guilds/{guild}/members/{id}"), None)
                .await
            {
                found.push(x);
            }
        }
        found
    } else {
        let q = crate::discord::urlencode(m.q.trim());
        st.discord
            .call(
                Method::GET,
                &format!("/guilds/{guild}/members/search?limit=10&query={q}"),
                None,
            )
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let list = found
        .iter()
        .filter(|m| m["user"]["id"].is_string())
        .map(|m| {
            let u = &m["user"];
            let name = m["nick"]
                .as_str()
                .or(u["global_name"].as_str())
                .or(u["username"].as_str());
            json!({"id": u["id"], "name": name.unwrap_or_default()})
        })
        .collect::<Vec<_>>();
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct Setup {
    expected_version: i32,
    /// Log channels to create besides the essential two; absent means only those two.
    logs: Option<Vec<String>>,
    /// Name of the main role (default "Member"): the made one, or the first main role renamed.
    #[serde(default)]
    member_name: Option<String>,
    /// Custom only: the optional Admin-only and Owner-only chats in the Staff category.
    #[serde(default)]
    admin_chat: bool,
    #[serde(default)]
    owner_chat: bool,
    /// #staff-chat and Staff Voice in the Staff category, for all staff; on unless turned off.
    #[serde(default = "yes")]
    staff_chat: bool,
}

fn yes() -> bool {
    true
}

/// Default staff permits, made with a role each when a server has none:
/// (name, rank, perms, whitelist). Admins are created first so they sit above Moderators.
const STAFF: [(&str, i32, &[&str], &[&str]); 2] = [
    (
        "Admins",
        2,
        &[
            "cases",
            "ban",
            "kick",
            "timeout",
            "warn",
            "slowmode",
            "purge",
            "sanitize",
            "reports",
            "tickets",
            "tags",
            "lock_channels",
            "lock_joins",
        ],
        &["automod", "reports", "words"],
    ),
    (
        "Moderators",
        1,
        &[
            "cases",
            "kick",
            "timeout",
            "warn",
            "slowmode",
            "purge",
            "sanitize",
            "reports",
            "tickets",
            "tags",
            "lock_channels",
        ],
        &["automod", "reports", "words"],
    ),
];

async fn auto_setup(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Setup>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let (cur, mut config) = store::current(&st.db, guild).await?;
    if cur != b.expected_version {
        return Err(ApiError::Conflict(cur)); // before any Discord side effect
    }
    let name = b
        .member_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if name.is_some_and(|n| n.chars().count() > 100) {
        return Err(ApiError::BadRequest(
            "the Member role name is longer than 100 characters".into(),
        ));
    }
    let start = config.clone();
    let role = quarantine_role(&st, guild, &config).await?;
    config["antinuke"]["quarantine_role"] = json!(role);
    let before = store::read_permits(&mut *st.db.acquire().await?, guild).await?;
    let no_custom = |p: &Value| p["custom"].as_array().is_none_or(Vec::is_empty);
    let staff = no_custom(&before);
    // Before the Member role: a role made later sits lower, so staff end up above Member.
    let made = staff_roles(&st, guild, &before, staff).await?;
    // Stored at once with a version: a later step that fails must not make a run again create a
    // second set, and rollbacks and stale saves see it.
    let mut tx = st.db.begin().await?;
    store::locked(&mut tx, guild).await?;
    let now = store::read_permits(&mut tx, guild).await?["levels"].clone();
    let levels = new_levels(&before["levels"], &now, &made.levels());
    if levels != now {
        store::write_levels(&mut tx, guild, &levels).await?;
        store::bump(&mut tx, guild, s.user_id).await?;
    }
    tx.commit().await?;
    let member = member_role_named(&st, guild, &mut config, name).await?;
    // Best effort: the setup goes on, and Overview offers the Fix again.
    let display = show_bot(&st, guild, &config)
        .await
        .inspect_err(|e| tracing::warn!(?e, guild, "showing Brim separately failed"))
        .ok();
    let roles_stay = order_roles(&st, guild, &made, &config, display.as_deref()).await;
    let logs = log_channels(&st, guild, &mut config, essentials_and(b.logs)).await?;
    // Held to the end: turning the Staff role off must not run between the read and the overwrites.
    let held = crate::channels::hold_shared(&st, guild).await?;
    let mut staff_ids: Vec<u64> = made
        .0
        .iter()
        .flatten()
        .filter_map(|r| r.parse().ok())
        .collect();
    // The Staff role, while on, opens the Staff channels too (next to the permit roles).
    if let Some((r, _)) = brim_core::permits::staff_role(&st.db, guild).await? {
        staff_ids.push(r as u64);
    }
    let pick = |on: bool, keys: &[&str]| -> Vec<u64> {
        keys.iter()
            .filter(|_| on)
            .filter_map(|k| made.of(k)?.parse().ok())
            .collect()
    };
    let chats = [
        (
            "admin-chat",
            pick(b.admin_chat, &["owner", "trusted_admin", "Admins"]),
        ),
        ("owner-chat", pick(b.owner_chat, &["owner"])),
    ];
    let mut chats = channel_access::staff_chats(&st, guild, &config, chats).await?;
    let mut lounge_failed = false;
    if b.staff_chat {
        // Best effort: a refused voice channel must not stop the setup half-way.
        match channel_access::staff_lounge(&st, guild, &config).await {
            Ok(made) => chats.extend(made),
            Err(e) => {
                tracing::warn!(?e, guild, "making the staff chat and voice channels failed");
                lounge_failed = true;
            }
        }
    }
    // After the chats: they may make the Staff category.
    channel_access::staff_access(&st, guild, &config, &staff_ids).await?;
    // Every channel move goes in one call at the end: Discord limits that route hard.
    let mut moves = vec![];
    let info = channel_access::information(&st, guild, &mut config, &staff_ids, &mut moves).await?;
    let sorted = channel_access::sort_loose(&st, guild, &config, &mut moves).await?;
    channel_access::verification_first(&config, &mut moves);
    let verify_failed = channel_access::verify_access(&st, guild, &config)
        .await
        .unwrap_or(1);
    if let Err(e) = ticket_log_to_logs(&st, guild, &config, &mut moves).await {
        tracing::warn!(?e, guild, "moving the ticket log into Logs failed");
    }
    let mut tx = st.db.begin().await?;
    // Read again after the Discord calls, so an EO/TA change made meanwhile is kept.
    let (now, mut latest) = store::locked(&mut tx, guild).await?;
    let mut permits = store::read_permits(&mut tx, guild).await?;
    // Default permits only while there are still none: ones saved meanwhile are kept.
    let fresh = staff && no_custom(&permits);
    if staff_permits(&mut permits, &made, fresh) {
        store::write_permits(&mut tx, guild, &permits, s.user_id).await?;
    }
    // A save made during the Discord calls is kept, and what the setup made gets a config.
    put_changes(&mut latest, &start, &config);
    let config = latest;
    let version = store::commit(&mut tx, guild, now, &config, s.user_id).await?;
    mark_setup_done(&mut *tx, guild, s.user_id).await?;
    tx.commit().await?;
    let quarantine_failed = quarantine_channels(&st, guild, &role, &config).await;
    // After the commit, best effort like the quarantine step: the setup is saved already.
    let catch_stays = match channel_access::move_catch(&st, guild, &config, &mut moves).await {
        Ok(why) => why,
        Err(e) => {
            tracing::warn!(?e, guild, "making the catch channel verified-only failed");
            Some("Brim could not read or change it".into())
        }
    };
    let moved = channel_access::send_moves(&st, guild, moves).await;
    let access = open_channels(&st, guild, &config, &staff_ids).await;
    crate::channels::release_pushes(held).await;
    api::log_by(&st, &s, guild, &config, Some(version), "ran auto-setup").await;
    Ok(Json(
        json!({"version": version, "created": {"quarantine_role": role, "member_role": member, "staff": staff, "logs": logs, "chats": chats, "information": info, "categories": sorted}, "catch_stays": catch_stays, "quarantine_failed": quarantine_failed, "channel_access": access,
            "steps": steps(&Report { roles_stay: roles_stay.as_deref(), quarantine_failed, lounge_failed, moved, catch_stays: catch_stays.as_deref(), verify_failed, access: &access })}),
    ))
}

/// What Auto Setup's result list needs to know.
struct Report<'a> {
    roles_stay: Option<&'a str>,
    quarantine_failed: Option<usize>,
    lounge_failed: bool,
    moved: bool,
    catch_stays: Option<&'a str>,
    verify_failed: usize,
    access: &'a Value,
}

/// Auto Setup's result, one entry per step the setup page lists (owner decision 2026-11-17: simple, and
/// a step that needs him says what happened and what to do): `{"step", "ok", "note"}`.
fn steps(r: &Report) -> Value {
    let manage = "Give Brim Manage Roles there, then run Auto Setup again.";
    let quarantine = match r.quarantine_failed {
        Some(0) => None,
        Some(n) => Some(format!(
            "{n} channels did not get the quarantine role. {manage}"
        )),
        None => Some("Brim could not list the channels. Run Auto Setup again.".into()),
    };
    let staff = r.lounge_failed.then(|| {
        "The staff chat and voice channels could not be made. Check that Brim may make channels, then run Auto Setup again.".to_string()
    });
    let mut info = vec![];
    if !r.moved {
        info.push(
            "Discord refused moving some channels into place. A category holds at most 50 channels: make room, then run Auto Setup again."
                .to_string(),
        );
    }
    if let Some(why) = r.catch_stays {
        info.push(format!(
            "The catch channel is still visible to members without a role: {why}."
        ));
    }
    let info = (!info.is_empty()).then(|| info.join(" "));
    let verify = (r.verify_failed > 0)
        .then(|| format!("Verified members still see the verification channel. {manage}"));
    let access = match (r.access["failed"].as_u64(), r.access["skipped"].as_str()) {
        (Some(0), _) => None,
        (Some(n), _) => Some(format!(
            "{n} channels did not get the Member-only permissions. {manage}"
        )),
        (None, Some(why)) => Some(format!(
            "Not changed: {why}. Fix that, then run Auto Setup again or use the fix in Overview."
        )),
        _ => None,
    };
    let rows = [
        ("Roles", r.roles_stay.map(|why| format!("The staff roles were not put above the other roles: {why}. Run Auto Setup again later."))),
        ("Quarantine", quarantine),
        ("Logs", None),
        ("Staff channels", staff),
        ("Information", info),
        ("Verification channel", verify),
        ("Member-only channels", access),
    ];
    Value::Array(
        rows.into_iter()
            .map(|(step, note)| json!({"step": step, "ok": note.is_none(), "note": note}))
            .collect(),
    )
}

/// The level roles to store: `now` (read under the config lock), with Auto Setup's role only for a
/// level nobody changed since `before` (read before its Discord calls), so an Owner's change made
/// meanwhile is kept.
fn new_levels(before: &Value, now: &Value, made: &Value) -> Value {
    let mut out = now.as_object().cloned().unwrap_or_default();
    for (level, role) in made.as_object().into_iter().flatten() {
        if before.get(level) == now.get(level) {
            out.insert(level.clone(), role.clone());
        }
    }
    Value::Object(out)
}

/// Sets on `latest` every field that differs from `before` to `after` (arrays whole), so Auto
/// Setup's changes go onto the newest version instead of failing on a save made meanwhile.
fn put_changes(latest: &mut Value, before: &Value, after: &Value) {
    for c in crate::diff::diff(before, after) {
        let keys: Vec<&str> = c.path.split('/').skip(1).collect();
        let Some((last, parents)) = keys.split_last() else {
            continue;
        };
        let mut v = &mut *latest;
        for k in parents {
            if !v[*k].is_object() {
                v[*k] = json!({});
            }
            v = &mut v[*k];
        }
        let Some(obj) = v.as_object_mut() else {
            continue;
        };
        match c.new {
            Some(n) => obj.insert(last.to_string(), n),
            None => obj.remove(*last),
        };
    }
}

/// Auto Setup's standard set on open channels, when verification is on and gives a main role.
/// A member without it loses the channels (owner decision 2026-11-17: not verified, no access): the
/// first time verification is on, the bot gives every member there the role first, and later the
/// bot's `verify_sweep` gives the rest the time limit. `{"failed": n}` when it ran,
/// `{"skipped": why}` when not.
async fn open_channels(st: &AppState, guild: i64, config: &Value, staff: &[u64]) -> Value {
    match channel_access::blocked(st, guild, config).await {
        Ok(None) => {}
        Ok(Some(why)) => return json!({"skipped": why}),
        Err(_) => return json!({"skipped": "Brim could not check who would lose access"}),
    }
    let info = channel_access::info_access(st, guild, config, staff)
        .await
        .unwrap_or(1);
    match channel_access::standard_access(st, guild, config).await {
        Some(n) => {
            // Only once the channels are Member-only.
            start_limits(st, guild).await;
            json!({"failed": n + info})
        }
        None => json!({"skipped": "Brim could not list the channels"}),
    }
}

/// Has the bot give members without the verified roles the time limit (`verify_sweep`), after
/// channels became Member-only. Best effort.
pub(crate) async fn start_limits(st: &AppState, guild: i64) {
    // Later, so the bot's first-time pass (every member there gets the role) runs first.
    let at = chrono::Utc::now().timestamp() + 600;
    if let Err(e) = brim_core::jobs::enqueue_once(&st.db, guild, "verify_sweep", at).await {
        tracing::warn!(?e, guild, "queueing the verification time limits failed");
    }
}

/// Auto Setup's staff roles, highest first: (permit or level, role name, colour). The Owner role
/// is for the server owner and every Extra Owner (owner decision 2026-11-16). Each holds only Mention
/// @everyone (Moderators and up may, owner decision 2026-11-16); a role gives no power by itself.
const STAFF_ROLES: [(&str, &str, u32); 4] = [
    ("owner", "Owner", 0xE74C3C),
    ("trusted_admin", "Trusted Admin", 0xE67E22),
    ("Admins", "Admins", 0xF1C40F),
    ("Moderators", "Moderators", 0x3498DB),
];
const MENTION_EVERYONE: u64 = 1 << 17;

/// The role id Auto Setup made or reused for each entry of [`STAFF_ROLES`], same order.
struct StaffRoles(Vec<Option<String>>);

impl StaffRoles {
    fn of(&self, key: &str) -> Option<&String> {
        let i = STAFF_ROLES.iter().position(|(k, ..)| *k == key)?;
        self.0[i].as_ref()
    }
    /// The `permits.levels` value for [`store::write_levels`].
    fn levels(&self) -> Value {
        let mut out = serde_json::Map::new();
        for level in brim_core::permits::ROLE_LEVELS {
            if let Some(id) = self.of(level) {
                out.insert(level.into(), json!(id));
            }
        }
        Value::Object(out)
    }
}

/// Makes or reuses the staff roles. Reused (owner decision 2026-11-16, "Reuse and update"): the level's
/// role, and the first role of the Admins or Moderators permit; each gets the colour and Mention
/// @everyone, keeping its other permissions (the security score still flags dangerous ones). A
/// permit role is made only for an Admins or Moderators permit that exists, or that Auto Setup
/// makes (`new_permits`: the server has no permits yet).
async fn staff_roles(
    st: &AppState,
    guild: i64,
    permits: &Value,
    new_permits: bool,
) -> Result<StaffRoles, ApiError> {
    let roles = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/roles"), None)
        .await?;
    let roles = roles.as_array().cloned().unwrap_or_default();
    let alive = |id: &str| roles.iter().find(|r| r["id"].as_str() == Some(id));
    let custom = permits["custom"].as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    for (key, name, colour) in STAFF_ROLES {
        let level = brim_core::permits::ROLE_LEVELS.contains(&key);
        let permit = custom.iter().find(|p| p["name"] == key);
        if !level && permit.is_none() && !new_permits {
            out.push(None);
            continue;
        }
        let existing = if level {
            permits["levels"][key].as_str().and_then(alive)
        } else {
            permit
                .and_then(|p| p["roles"].as_array())
                .into_iter()
                .flatten()
                .find_map(|r| r.as_str().and_then(alive))
        };
        let id = match existing {
            Some(r) => {
                let id = r["id"].as_str().unwrap_or_default().to_string();
                let perms = r["permissions"]
                    .as_str()
                    .and_then(|p| p.parse::<u64>().ok())
                    .unwrap_or(0);
                // Hoisted: each staff role is its own group in the member list (owner decision 2026-11-17).
                let body = json!({"color": colour, "hoist": true, "permissions": (perms | MENTION_EVERYONE).to_string()});
                // A role above the bot cannot be changed; it is still the staff role.
                let r = st
                    .discord
                    .call(
                        Method::PATCH,
                        &format!("/guilds/{guild}/roles/{id}"),
                        Some(body),
                    )
                    .await;
                if let Err(e) = r {
                    tracing::warn!(?e, guild, role = id, "updating a staff role failed");
                }
                id
            }
            None => {
                let body = json!({"name": name, "color": colour, "hoist": true, "permissions": MENTION_EVERYONE.to_string()});
                create(st, guild, None, "roles", body).await?
            }
        };
        out.push(Some(id));
    }
    Ok(StaffRoles(out))
}

/// Puts the roles below the bot in one order (owner decision 2026-11-17), top to bottom: Quarantine, the
/// Brim colour role, the staff roles (Owner highest), the other roles as they were, and the
/// first main role at the bottom, also a reused one. Staff sit above the other roles, so they can
/// moderate members who hold them. Best effort: Discord refuses a move to or past the bot's own
/// role, and the setup goes on. Returns why the roles stay as they were, for the Roles step.
async fn order_roles(
    st: &AppState,
    guild: i64,
    made: &StaffRoles,
    config: &Value,
    display: Option<&str>,
) -> Option<String> {
    let Ok((roles, top)) = crate::ops::roles_and_bot_pos(st, guild).await else {
        return Some("Brim could not read the roles from Discord".into());
    };
    let everyone = guild.to_string();
    // Highest first, as Discord lists them; a tie goes to the older (lower) id.
    let now: Vec<&str> = {
        let mut below: Vec<&crate::ops::Role> = roles
            .iter()
            .filter(|r| r.position < top && r.id != everyone)
            .collect();
        below.sort_by_key(|r| {
            (
                std::cmp::Reverse(r.position),
                r.id.parse::<u64>().unwrap_or(0),
            )
        });
        below.into_iter().map(|r| r.id.as_str()).collect()
    };
    let main = config["lockdown"]["main_roles"][0].as_str();
    let quarantine = config["antinuke"]["quarantine_role"].as_str();
    let mut want: Vec<&str> = vec![];
    for id in [quarantine, display]
        .into_iter()
        .flatten()
        .chain(made.0.iter().flatten().map(String::as_str))
    {
        if now.contains(&id) && !want.contains(&id) && Some(id) != main {
            want.push(id);
        }
    }
    let rest: Vec<&str> = now
        .iter()
        .copied()
        .filter(|id| !want.contains(id) && Some(*id) != main)
        .collect();
    want.extend(rest);
    want.extend(main.filter(|m| now.contains(m)));
    if want == now {
        return None;
    }
    // Positions that tie can leave too few slots under the bot; Discord would refuse it all.
    if want.len() as i64 >= top {
        tracing::warn!(
            guild,
            "too few role positions under the bot to order the roles"
        );
        return Some("there are too few role positions under Brim's role".into());
    }
    let body: Vec<Value> = want
        .iter()
        .rev()
        .enumerate()
        .map(|(i, id)| json!({"id": id, "position": i + 1}))
        .collect();
    let path = format!("/guilds/{guild}/roles");
    let why = match st
        .discord
        .send(Method::PATCH, &path, Some(json!(body)))
        .await
    {
        Ok(r) if r.status().is_success() => return None,
        Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
            "Discord is limiting changes right now".to_string()
        }
        // Discord's own reason: the status alone (a 400 on 11-17 and 11-18) does not say it.
        Ok(r) => {
            let s = r.status().as_u16();
            let body: String = r
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect();
            format!("Discord answered {s}: {body}")
        }
        Err(e) => format!("Discord did not answer: {e}"),
    };
    tracing::warn!(guild, why, "ordering the roles under the bot failed");
    Some(why)
}

/// Puts the staff roles on the Admins and Moderators permits: makes both permits when
/// `new_permits`, else adds the role to one that has none of it. Returns whether anything changed.
fn staff_permits(permits: &mut Value, made: &StaffRoles, new_permits: bool) -> bool {
    if new_permits {
        let custom: Vec<Value> = STAFF
            .iter()
            .map(|(name, rank, perms, whitelist)| {
                let roles: Vec<&String> = made.of(name).into_iter().collect();
                json!({"name": name, "rank": rank, "perms": perms, "whitelist": whitelist, "roles": roles})
            })
            .collect();
        permits["custom"] = json!(custom);
        return true;
    }
    let mut changed = false;
    for p in permits["custom"].as_array_mut().into_iter().flatten() {
        // Only these two: a permit named like a level must not get the level's role.
        let name = p["name"]
            .as_str()
            .filter(|n| matches!(*n, "Admins" | "Moderators"));
        let Some(id) = name.and_then(|n| made.of(n)).cloned() else {
            continue;
        };
        let roles = p["roles"].as_array().cloned().unwrap_or_default();
        if !roles.iter().any(|r| r.as_str() == Some(&id)) {
            let mut roles = roles;
            roles.push(json!(id));
            p["roles"] = json!(roles);
            changed = true;
        }
    }
    changed
}

/// Lockdown and verification need a main role; members get "Member" when they verify. Makes it
/// (or takes over a fit role with that name) and sets it, and the verification roles when empty,
/// in `config` when no main role is set. Ids of roles deleted in Discord are dropped first.
pub(crate) async fn member_role(
    st: &AppState,
    guild: i64,
    config: &mut Value,
) -> Result<Option<String>, ApiError> {
    member_role_named(st, guild, config, None).await
}

/// Brim's colour in the member list: the dashboard's main brand violet.
const BOT_COLOUR: u32 = 0x9B7BFF;
/// The Member role's colour (owner decision 2026-11-18), green next to the staff roles' red, orange,
/// yellow and blue. Given only to a Member role without a colour: a colour set by hand stays.
const MEMBER_COLOUR: u32 = 0x2ECC71;

/// Shows the bot as its own group in the member list in [`BOT_COLOUR`] (owner decision 2026-11-17).
/// Discord refuses a bot any change to its own role, so it gets a second role with its name and
/// no permissions: a member is listed under their highest hoisted role and coloured by their
/// highest coloured one. One it already holds is reused.
pub(crate) async fn show_bot(
    st: &AppState,
    guild: i64,
    config: &Value,
) -> Result<String, ApiError> {
    let me = st.discord.client_id.as_str();
    let path = format!("/guilds/{guild}/roles");
    let get_roles = || async {
        let r = st.discord.call(Method::GET, &path, None).await?;
        Ok::<_, ApiError>(r.as_array().cloned().unwrap_or_default())
    };
    let roles = get_roles().await?;
    let mine = st.discord.member_roles(guild, me).await?;
    let held = |r: &&Value| {
        r["id"]
            .as_str()
            .is_some_and(|id| mine.iter().any(|m| m == id))
    };
    let bot_role = roles.iter().filter(held).find(|r| r["managed"] == true);
    let name = bot_role
        .and_then(|r| r["name"].as_str())
        .unwrap_or("Brim")
        .to_string();
    let candidates: Vec<String> = roles
        .iter()
        .filter(held)
        .filter(|r| r["managed"] != true && r["name"] == name.as_str() && r["permissions"] == "0")
        .filter_map(|r| r["id"].as_str().map(String::from))
        .collect();
    // Never one a member holds too: it is lifted above staff, who could then not moderate them.
    // Members not all read, or the read failed: none is reused.
    // shortcut: above 20,000 members every run makes a new role, fine until such servers use it.
    let mut others = std::collections::HashSet::new();
    let more = match candidates.is_empty() {
        true => false,
        false => crate::ops::member_pages(st, guild, |m| {
            if m["user"]["id"].as_str() != Some(me) {
                let ids = m["roles"].as_array().into_iter().flatten();
                others.extend(ids.filter_map(|r| r.as_str().map(String::from)));
            }
        })
        .await
        .unwrap_or(true),
    };
    let reuse = candidates
        .into_iter()
        .find(|id| !more && !others.contains(id));
    let id = match reuse {
        Some(id) => {
            let at = format!("/guilds/{guild}/roles/{id}");
            let body = json!({"color": BOT_COLOUR, "hoist": true});
            st.discord.call(Method::PATCH, &at, Some(body)).await?;
            id
        }
        None => {
            let body =
                json!({"name": name, "color": BOT_COLOUR, "hoist": true, "permissions": "0"});
            let id = create(st, guild, None, "roles", body).await?;
            let at = format!("/guilds/{guild}/members/{me}/roles/{id}");
            if let Err(e) = st.discord.call(Method::PUT, &at, None).await {
                // A role the bot does not hold is never reused: removed, so a retry makes no second.
                let gone = format!("/guilds/{guild}/roles/{id}");
                let _ = st.discord.call(Method::DELETE, &gone, None).await;
                return Err(e.into());
            }
            id
        }
    };
    // Read again: a new role shifts the others up. It goes right under the bot's role, or under
    // the quarantine role, which must stay right under the bot. Best effort, like order_staff.
    let roles = get_roles().await?;
    let pos = |id: &str| {
        roles
            .iter()
            .find(|r| r["id"].as_str() == Some(id))
            .and_then(|r| r["position"].as_i64())
    };
    let top = bot_role
        .and_then(|r| r["id"].as_str())
        .and_then(pos)
        .unwrap_or(0);
    let above = own(&config["antinuke"]["quarantine_role"])
        .and_then(|q| pos(&q))
        .filter(|&q| q < top)
        .unwrap_or(top);
    let at = pos(&id).unwrap_or(1);
    // Moving up takes the slot under `above`; moving down takes its slot and pushes it up.
    let to = if at < above { above - 1 } else { above };
    if to >= 1 && at != to {
        let body = json!([{"id": id, "position": to}]);
        if let Err(e) = st.discord.call(Method::PATCH, &path, Some(body)).await {
            tracing::warn!(
                ?e,
                guild,
                "moving the Brim role under the bot's role failed"
            );
        }
    }
    Ok(id)
}

/// [`member_role`] with the name Auto Setup's Custom gave (owner decision 2026-11-16: changeable, "Member"
/// by default). With a name and a main role already set, the first main role is renamed.
async fn member_role_named(
    st: &AppState,
    guild: i64,
    config: &mut Value,
    name: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let empty = |v: &Value| v.as_array().is_none_or(Vec::is_empty);
    // Ids of roles deleted in Discord stay in the config: drop them first, or a saved but dead
    // main role stops the Member role from being made (owner decision 2026-11-17).
    let (roles, bot_pos) = crate::ops::roles_and_bot_pos(st, guild).await?;
    let alive = |id: &Value| roles.iter().any(|r| id.as_str() == Some(r.id.as_str()));
    let had_verify = !empty(&config["verification"]["roles"]);
    for (s, k) in [("lockdown", "main_roles"), ("verification", "roles")] {
        if let Some(ids) = config[s][k].as_array_mut() {
            ids.retain(alive);
        }
    }
    if let Some(main) = config["lockdown"]["main_roles"][0]
        .as_str()
        .map(String::from)
    {
        let role = roles.iter().find(|r| r.id == main);
        let mut body = json!({});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        // A Member role from an earlier run without a colour gets one; another main role keeps its look.
        let member = name.is_some() || role.is_some_and(|r| r.name == "Member");
        if member && role.is_some_and(|r| r.color == 0) {
            body["color"] = json!(MEMBER_COLOUR);
        }
        if body != json!({}) {
            let at = format!("/guilds/{guild}/roles/{main}");
            // A main role above the bot keeps its name; the rest of the setup still runs.
            if let Err(e) = st.discord.call(Method::PATCH, &at, Some(body)).await {
                tracing::warn!(?e, guild, role = main, "renaming the main role failed");
            }
        }
        // Verification with no role does nothing: one whose roles were all deleted gets the main role.
        if had_verify && empty(&config["verification"]["roles"]) {
            config["verification"]["roles"] = json!([main]);
        }
        return Ok(None);
    }
    let name = name.unwrap_or("Member");
    // A role with that name is taken over instead of made twice (owner decision 2026-11-17), when it is
    // fit: not @everyone, not managed, below the bot, without what a main role must not hold (the
    // @everyone rule of `stripped`), and not a permit or level role, as staff roles are.
    let permits = store::read_permits(&mut *st.db.acquire().await?, guild)
        .await?
        .to_string();
    let g = guild as u64;
    let named = roles.iter().filter(|r| {
        r.name == name
            && r.unfit(guild, bot_pos).is_none()
            && brim_antinuke::security::stripped(g, g, (&[], &[]), r.perms()) == r.perms()
            && !permits.contains(&format!("\"{}\"", r.id))
    });
    let id = match oldest_unused(named.map(|r| r.id.as_str()), config) {
        Some(id) => {
            if roles.iter().any(|r| r.id == id && r.color == 0) {
                let body = json!({"color": MEMBER_COLOUR});
                let at = format!("/guilds/{guild}/roles/{id}");
                if let Err(e) = st.discord.call(Method::PATCH, &at, Some(body)).await {
                    tracing::warn!(?e, guild, role = id, "colouring the Member role failed");
                }
            }
            id.to_string()
        }
        None => {
            let body = json!({"name": name, "color": MEMBER_COLOUR});
            create(st, guild, None, "roles", body).await?
        }
    };
    config["lockdown"]["main_roles"] = json!([id]);
    if empty(&config["verification"]["roles"]) {
        config["verification"]["roles"] = json!([id]);
    }
    Ok(Some(id))
}

/// A sample captcha drawn from the (unsaved) settings on the Verification page, with the bot's
/// own drawing code. Only the image goes back, never its answer.
async fn captcha_preview(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let cfg: brim_automod::join::CaptchaCfg = serde_json::from_value(b)
        .map_err(|e| ApiError::BadRequest(format!("captcha settings: {e}")))?;
    let png = tokio::task::spawn_blocking(move || brim_automod::captcha::new_captcha(&cfg).1)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    use base64::Engine as _;
    Ok(Json(
        json!({"png": base64::engine::general_purpose::STANDARD.encode(png)}),
    ))
}

/// Records that the server went through setup, so Overview stops asking for the wizard.
async fn mark_setup_done(
    db: impl sqlx::PgExecutor<'_>,
    guild: i64,
    user: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "insert into guild_setup_done (guild_id, done_by) values ($1, $2) on conflict do nothing",
    )
    .bind(guild)
    .bind(user)
    .execute(db)
    .await?;
    Ok(())
}

/// The wizard's last step: setup is done, also when Auto Setup was not used.
async fn wizard_done(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    mark_setup_done(&st.db, guild, s.user_id).await?;
    let (_, config) = store::current(&st.db, guild).await?;
    let text = format!(
        "{} (<@{}>) finished the setup wizard",
        s.username, s.user_id
    );
    api::log_change(&st, guild, s.user_id, &config, None, text).await;
    Ok(Json(json!({"ok": true})))
}

/// The wizard's Discord settings card: the three server settings the Overview rows check, set
/// to the same safer values (`setting_fix`), and only where Discord has a weaker one now. A
/// stricter value the owner chose stays.
async fn discord_settings(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(_): Json<Setup>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let (_, config) = store::current(&st.db, guild).await?;
    let now = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}"), None)
        .await?;
    let n = |k: &str| now[k].as_u64().unwrap_or(0);
    let mut body = serde_json::Map::new();
    for (setting, weaker) in [
        ("verification_level", n("verification_level") < 2),
        ("content_filter", n("explicit_content_filter") < 2),
        ("notifications", n("default_message_notifications") == 0),
    ] {
        if let (true, Some((key, value))) = (weaker, brim_antinuke::security::setting_fix(setting))
        {
            body.insert(key.into(), json!(value));
        }
    }
    if body.is_empty() {
        return Ok(Json(json!({"ok": true})));
    }
    let keys: Vec<String> = body.keys().cloned().collect();
    st.discord
        .call(
            Method::PATCH,
            &format!("/guilds/{guild}"),
            Some(Value::Object(body)),
        )
        .await?;
    let text = format!(
        "{} (<@{}>) set Discord's {} from the setup wizard",
        s.username,
        s.user_id,
        keys.join(", ")
    );
    api::log_change(&st, guild, s.user_id, &config, None, text).await;
    Ok(Json(json!({"ok": true})))
}

/// "Setup channel": a #verify-here channel only unverified members see, holding the
/// verify button. An already set verification channel is reused.
async fn verify_channel(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Setup>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let (cur, mut config) = store::current(&st.db, guild).await?;
    if cur != b.expected_version {
        return Err(ApiError::Conflict(cur));
    }
    // @everyone reads but cannot write; verified members (the verification roles) stop seeing it.
    let mut overwrites = vec![
        json!({"id": guild.to_string(), "type": 0, "allow": "66560", "deny": "2048"}),
        json!({"id": st.discord.client_id, "type": 1, "allow": "19456"}),
    ];
    // The bot falls back to the main roles when no verification roles are set.
    let roles = |s: &str, k: &str| config[s][k].as_array().filter(|a| !a.is_empty()).cloned();
    let given = roles("verification", "roles")
        .or_else(|| roles("lockdown", "main_roles"))
        .unwrap_or_default();
    for r in &given {
        overwrites.push(json!({"id": r, "type": 0, "deny": "1024"}));
    }
    // Members in the join quarantine read it too; the bot's enforce_quarantine knows this shape.
    if let Some(q) = own(&config["antinuke"]["quarantine_role"]) {
        overwrites.push(verification_overwrite(&q));
    }
    let existing = own(&config["verification"]["channel"]);
    let mut taken = None;
    if existing.is_none() {
        let channels = st
            .discord
            .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
            .await?;
        taken = unused_channel(&channels, &config, "verify-here");
    }
    // A taken-over #verify-here gets the overwrites a new one would; its messages stay.
    if let Some(id) = &taken {
        st.discord
            .call(
                Method::PATCH,
                &format!("/channels/{id}"),
                Some(json!({"permission_overwrites": overwrites})),
            )
            .await?;
    }
    let channel = create(
        &st,
        guild,
        existing.clone().or(taken),
        "channels",
        // First in the list, above every category (owner decision 2026-11-17).
        json!({"name": "verify-here", "type": 0, "position": 0, "permission_overwrites": overwrites}),
    )
    .await?;
    config["verification"]["channel"] = json!(channel);
    let mut moves = vec![];
    channel_access::verification_first(&config, &mut moves);
    // Only its place in the list; a refusal is logged and the channel works where it is.
    channel_access::send_moves(&st, guild, moves).await;
    // A reused channel set up before today's verification roles still shows to verified members.
    if existing.is_some() {
        if let Err(e) = channel_access::verify_access(&st, guild, &config).await {
            tracing::warn!(
                ?e,
                guild,
                "hiding the verification channel from verified members failed"
            );
        }
    }
    // The same panel as the bot's `verifysetup`, so both look alike in Discord.
    let server = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}"), None)
        .await?;
    let stored = crate::botmsgs::stored(&st.db, guild, "verify.panel").await?;
    let (embed, buttons) = brim_logging::embed::verify_panel(
        server["name"].as_str().unwrap_or("the server"),
        stored.as_ref(),
    );
    let mut tx = st.db.begin().await?;
    let version = store::commit(&mut tx, guild, b.expected_version, &config, s.user_id).await?;
    tx.commit().await?;
    api::log_by(
        &st,
        &s,
        guild,
        &config,
        Some(version),
        "set up the verification channel",
    )
    .await;
    reshape_verification(&st, &channel, &config).await?;
    // The set channel already holds the panel; a new or taken-over one gets it.
    if existing.as_deref() != Some(channel.as_str()) {
        st.discord
            .call(
                Method::POST,
                &format!("/channels/{channel}/messages"),
                Some(json!({"embeds": [embed], "components": [buttons]})),
            )
            .await?;
    }
    // The channel stays, but nobody can pass until there is a role to hand out.
    let warning = given.is_empty().then_some(NO_VERIFY_ROLE);
    Ok(Json(
        json!({"version": version, "channel": channel, "warning": warning}),
    ))
}

const NO_VERIFY_ROLE: &str = "The verification channel is ready, but members cannot pass yet: there is no role to give them. Pick a main role under Misc or the verification roles under Verification, then save.";

const CATCH_WARNING: &str =
    "Do not write here. Writing = instant ban. Account hacked? Change your password.";

/// Makes the catch channel (`config`'s one is reused while it exists) and turns it on. Returns
/// the channel when it is new, for [`pin_catch_notice`]; also used by the Overview fix for a
/// deleted catch channel (2026-11-18).
pub(crate) async fn make_catch(
    st: &AppState,
    guild: i64,
    config: &mut Value,
) -> Result<Option<String>, ApiError> {
    let existing = own(&config["catch"]["channel"]);
    let fresh = existing.is_none();
    // At the bottom of Information when Auto Setup made it: only verified members see it there.
    let body = match channel_access::new_catch_place(st, guild, config).await? {
        Some((parent, overwrites)) => {
            json!({"name": "do-not-write", "type": 0, "parent_id": parent, "topic": CATCH_WARNING, "permission_overwrites": overwrites})
        }
        None => {
            // @everyone: View, Send, History; no reactions, embeds or files. The bot also
            // deletes and pins.
            let overwrites = json!([
                {"id": guild.to_string(), "type": 0, "allow": "68608", "deny": "49216"},
                {"id": st.discord.client_id, "type": 1, "allow": "93184"}
            ]);
            json!({"name": "do-not-write", "type": 0, "position": 0, "topic": CATCH_WARNING, "permission_overwrites": overwrites})
        }
    };
    let channel = create(st, guild, existing.clone(), "channels", body).await?;
    config["catch"]["channel"] = json!(channel);
    config["catch"]["enabled"] = json!(true);
    // The catch channel runs only with Moderation on; this route has that switch's bar.
    if config["modules"]["moderation"] == false {
        config["modules"]["moderation"] = json!(true);
    }
    if fresh {
        config["catch"]["keep_top"] = json!(true);
    }
    Ok((existing.as_deref() != Some(channel.as_str())).then_some(channel))
}

/// Posts and pins the "Do not write here" warning in a new catch channel.
pub(crate) async fn pin_catch_notice(st: &AppState, channel: &str) -> Result<(), ApiError> {
    let m = st
        .discord
        .call(
            Method::POST,
            &format!("/channels/{channel}/messages"),
            Some(json!({"embeds": [{"title": "Do not write here", "color": 0xE71B1B, "description": CATCH_WARNING}]})),
        )
        .await?;
    if let Some(id) = m["id"].as_str() {
        let pin = format!("/channels/{channel}/messages/pins/{id}");
        st.discord.call(Method::PUT, &pin, None).await?;
    }
    Ok(())
}

/// The catch channel: at the bottom of the Information category when Auto Setup made one, else
/// on top of the list (uncategorized channels sit above every category); members may only read
/// and write plain text. Turns the catch channel on; an already set
/// channel is reused.
async fn catch_channel(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Setup>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let (cur, mut config) = store::current(&st.db, guild).await?;
    if cur != b.expected_version {
        return Err(ApiError::Conflict(cur));
    }
    let made = make_catch(&st, guild, &mut config).await?;
    let mut tx = st.db.begin().await?;
    let version = store::commit(&mut tx, guild, b.expected_version, &config, s.user_id).await?;
    tx.commit().await?;
    api::log_by(
        &st,
        &s,
        guild,
        &config,
        Some(version),
        "set up the catch channel",
    )
    .await;
    if let Some(ch) = &made {
        pin_catch_notice(&st, ch).await?;
    }
    let channel = &config["catch"]["channel"];
    Ok(Json(json!({"version": version, "channel": channel})))
}

/// One click: a private "Tickets" category with #ticket-logs. Channels already set are reused.
async fn tickets_setup(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Setup>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let (cur, mut config) = store::current(&st.db, guild).await?;
    if cur != b.expected_version {
        return Err(ApiError::Conflict(cur));
    }
    let log = ticket_channels(&st, guild, &mut config).await?;
    let mut tx = st.db.begin().await?;
    let version = store::commit(&mut tx, guild, b.expected_version, &config, s.user_id).await?;
    tx.commit().await?;
    api::log_by(&st, &s, guild, &config, Some(version), "set up tickets").await;
    Ok(Json(json!({"version": version, "channel": log})))
}

/// Makes the Tickets category and #ticket-logs (each only when unset) and turns Tickets on in
/// `config`. Returns the log channel.
pub(crate) async fn ticket_channels(
    st: &AppState,
    guild: i64,
    config: &mut Value,
) -> Result<String, ApiError> {
    // Staff are the roles of permits with Manage Tickets (owner decision 11-15: permits only).
    let staff: Vec<String> = brim_core::permits::roles_with(&st.db, guild, "tickets")
        .await?
        .iter()
        .map(i64::to_string)
        .collect();
    // The bot reads the history for the log and posts files there.
    let mut hidden = vec![
        json!({"id": guild.to_string(), "type": 0, "deny": "1024"}),
        json!({"id": st.discord.client_id, "type": 1, "allow": "117760"}),
    ];
    hidden.extend(
        staff
            .iter()
            .map(|r| json!({"id": r, "type": 0, "allow": "117760"})),
    );
    let t = config["tickets"].clone();
    let category = create(
        st,
        guild,
        own(&t["category"]),
        "channels",
        json!({"name": "Tickets", "type": 4, "permission_overwrites": hidden}),
    )
    .await?;
    // The log goes with the other logs, in the one shared Logs category, made when missing (owner decision 11-15).
    let (parent, follow) = if own(&t["log_channel"]).is_some() {
        (None, false)
    } else {
        let channels = st
            .discord
            .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
            .await?;
        let main: Vec<u64> = config["lockdown"]["main_roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str()?.parse().ok())
            .collect();
        let logs = self::category(
            st,
            guild,
            &channels,
            brim_antinuke::security::LOGS_CATEGORY,
            &main,
        )
        .await?;
        let img = crate::recovery::live_image(guild, &json!([]), &channels);
        let ids: Vec<u64> = staff.iter().filter_map(|r| r.parse().ok()).collect();
        let follow = img
            .channels
            .iter()
            .find(|c| c.id.to_string() == logs)
            .is_some_and(|c| brim_antinuke::security::ticket_log_follows(c, &ids));
        (Some(logs), follow)
    };
    let parent = parent.unwrap_or_else(|| category.clone());
    // No overwrites given: it takes the Logs category's, lock included (owner decision 2026-11-18).
    let body = match follow {
        true => json!({"name": "ticket-logs", "type": 0, "parent_id": parent}),
        false => {
            json!({"name": "ticket-logs", "type": 0, "parent_id": parent, "permission_overwrites": locked(&hidden, guild, &st.discord.client_id)})
        }
    };
    let log = create(st, guild, own(&t["log_channel"]), "channels", body).await?;
    if !config["tickets"].is_object() {
        config["tickets"] = json!({});
    }
    let tickets = &mut config["tickets"];
    tickets["enabled"] = json!(true);
    tickets["category"] = json!(category);
    tickets["log_channel"] = json!(log);
    // One set up before the rule sits elsewhere, often in Tickets. Best effort.
    let mut moves = vec![];
    match ticket_log_to_logs(st, guild, config, &mut moves).await {
        // A refusal is logged; the Overview row "Ticket log not with the other logs" shows it.
        Ok(()) => {
            channel_access::send_moves(st, guild, moves).await;
        }
        Err(e) => tracing::warn!(?e, guild, "moving the ticket log into Logs failed"),
    }
    Ok(log)
}

/// Moves the ticket log channel into the Logs category, made when missing, with the other log
/// channels (owner decision 2026-11-17). It keeps its own overwrites, which only staff let in.
pub(crate) async fn ticket_log_to_logs(
    st: &AppState,
    guild: i64,
    config: &Value,
    moves: &mut Vec<Value>,
) -> Result<(), ApiError> {
    let Some(log) = own(&config["tickets"]["log_channel"]) else {
        return Ok(());
    };
    let channels = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    let img = crate::recovery::live_image(guild, &json!([]), &channels);
    let log_id = log.parse().ok();
    if brim_antinuke::security::ticket_log_check(&img.channels, log_id).is_none() {
        return Ok(());
    }
    let main: Vec<u64> = config["lockdown"]["main_roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str()?.parse().ok())
        .collect();
    let logs = category(
        st,
        guild,
        &channels,
        brim_antinuke::security::LOGS_CATEGORY,
        &main,
    )
    .await?;
    moves.push(json!({"id": log, "parent_id": logs, "lock_permissions": false}));
    Ok(())
}

/// Discord's upload limit for the bot; a ticket log file is never larger.
const TRANSCRIPT_MAX: u64 = 25 << 20;

/// The conversation file of a closed ticket, read from the ticket log message that holds it.
async fn ticket_log(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, channel, message)): Path<(i64, u64, u64)>,
) -> Res {
    // T-02: a transcript is private ticket content, so Viewer is not enough.
    api::staff(&st, &s, guild, &["tickets"]).await?;
    // Only a transcript recorded for this server, or a channel this server ever had as its
    // ticket log (older files), so no other bot file is served.
    let was_log: bool = sqlx::query_scalar(
        "select exists(select 1 from ticket_transcripts
                       where guild_id = $1 and log_channel_id = $3 and log_message_id = $4)
             or exists(select 1 from guild_config
                       where guild_id = $1 and config #>> '{tickets,log_channel}' = $2)",
    )
    .bind(guild)
    .bind(channel.to_string())
    .bind(channel as i64)
    .bind(message as i64)
    .fetch_one(&st.db)
    .await?;
    if !was_log {
        return Err(ApiError::NotFound);
    }
    // The bot token reads every server's channels, so only this server's files are served.
    let ch = st
        .discord
        .call(Method::GET, &format!("/channels/{channel}"), None)
        .await
        .map_err(|_| ApiError::NotFound)?;
    if ch["guild_id"].as_str() != Some(guild.to_string().as_str()) {
        return Err(ApiError::NotFound);
    }
    let m = st
        .discord
        .call(
            Method::GET,
            &format!("/channels/{channel}/messages/{message}"),
            None,
        )
        .await
        .map_err(|_| ApiError::NotFound)?;
    if m["author"]["id"].as_str() != Some(st.discord.client_id.as_str()) {
        return Err(ApiError::NotFound);
    }
    let file = m["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| {
            a["filename"]
                .as_str()
                .is_some_and(|f| f.starts_with("ticket_log_"))
        })
        .ok_or(ApiError::NotFound)?;
    // Fetched with the dashboard's client, so only Discord's file host and a size Discord upholds.
    let cdn = |u: &str| u.starts_with("https://cdn.discordapp.com/");
    let url = file["url"]
        .as_str()
        .filter(|u| cdn(u))
        .filter(|_| file["size"].as_u64().is_some_and(|n| n <= TRANSCRIPT_MAX))
        .ok_or(ApiError::NotFound)?;
    let r = st.discord.http.get(url).send().await?.error_for_status()?;
    // The shared client follows redirects, so the final address must still be Discord's.
    if !cdn(r.url().as_str()) {
        return Err(ApiError::NotFound);
    }
    let body = brim_automod::sites::read_capped(r, TRANSCRIPT_MAX as usize).await?;
    let text = String::from_utf8_lossy(&body);
    Ok(Json(json!({"file": file["filename"], "text": text})))
}

fn at<'a>(config: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(config, |v, k| &v[*k])
}

fn not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<reqwest::Error>().and_then(|e| e.status())
        == Some(reqwest::StatusCode::NOT_FOUND)
}

pub(crate) fn own(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(String::from)
}

/// Of `ids`, the oldest (lowest snowflake) that `config` does not use. A setup takes it over
/// instead of making a second one with the same name, and never deletes it: it can be the
/// server's own channel or role with years of history (owner decision 11-17).
fn oldest_unused<'a>(ids: impl IntoIterator<Item = &'a str>, config: &Value) -> Option<&'a str> {
    let used = config.to_string();
    ids.into_iter()
        .filter(|id| !used.contains(&format!("\"{id}\"")))
        .min_by_key(|id| id.parse::<u64>().unwrap_or(u64::MAX))
}

/// The oldest text channel named `name` that `config` does not use ([`oldest_unused`]).
/// `brim-logs` also takes a `tommybot-logs` channel, made before the bot was renamed.
fn unused_channel(channels: &Value, config: &Value, name: &str) -> Option<String> {
    let old = (name == "brim-logs").then_some("tommybot-logs");
    let named = channels
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == 0 && (c["name"] == name || old.is_some_and(|o| c["name"] == o)))
        .filter_map(|c| c["id"].as_str());
    oldest_unused(named, config).map(String::from)
}

/// The quarantine role. A picked role is kept and stripped of every permission. With none picked,
/// the oldest unused role named Quarantine that no member holds is taken over the same way, and a
/// new one is made only when there is none. Other roles named Quarantine are left alone. A role that cannot be one
/// (@everyone, managed, not below the bot) is never used.
pub(crate) async fn quarantine_role(
    st: &AppState,
    guild: i64,
    config: &Value,
) -> Result<String, ApiError> {
    let (roles, bot_pos) = crate::ops::roles_and_bot_pos(st, guild).await?;
    let fit = |r: &crate::ops::Role| r.unfit(guild, bot_pos).is_none();
    // A picked role missing from the list is handled below as deleted in Discord.
    let picked = own(&config["antinuke"]["quarantine_role"])
        .filter(|id| roles.iter().find(|r| r.id == *id).is_none_or(fit));
    let named: Vec<&str> = roles
        .iter()
        .filter(|r| r.name == "Quarantine" && fit(r))
        .map(|r| r.id.as_str())
        .collect();
    // Only a role nobody holds: a held one would lock its members out of every channel with no
    // quarantine row, case or release. Not every member read: none is taken over.
    let held = match named.is_empty() {
        true => Some(vec![]),
        false => crate::ops::held_roles(st, guild).await?,
    };
    let free = named
        .into_iter()
        .filter(|id| held.as_ref().is_some_and(|h| !h.iter().any(|x| x == id)));
    let reuse = oldest_unused(free, config).map(String::from);
    for id in picked.into_iter().chain(reuse) {
        let r = st
            .discord
            .call(
                Method::PATCH,
                &format!("/guilds/{guild}/roles/{id}"),
                Some(json!({"permissions": "0"})),
            )
            .await;
        // A saved role someone deleted in Discord is made again below instead of failing the setup.
        match r {
            Ok(_) => return Ok(id),
            Err(e) if !not_found(&e) => return Err(e.into()),
            Err(_) => {}
        }
    }
    create(
        st,
        guild,
        None,
        "roles",
        json!({"name": "Quarantine", "permissions": "0"}),
    )
    .await
}

/// Returns `existing`, or creates the role or channel on Discord and returns its id.
pub(crate) async fn create(
    st: &AppState,
    guild: i64,
    existing: Option<String>,
    path: &str,
    body: Value,
) -> Result<String, ApiError> {
    if let Some(id) = existing {
        let at = match path {
            "roles" => format!("/guilds/{guild}/roles/{id}"),
            _ => format!("/channels/{id}"),
        };
        // Reuse it only while it still exists; one deleted in Discord is made again.
        match st.discord.call(Method::GET, &at, None).await {
            Ok(_) => return Ok(id),
            Err(e) if !not_found(&e) => return Err(e.into()),
            Err(_) => {}
        }
    }
    let r = st
        .discord
        .call(Method::POST, &format!("/guilds/{guild}/{path}"), Some(body))
        .await?;
    Ok(r["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("discord returned no id"))?
        .to_string())
}

/// Log types, each with its own channel: (config path, channel name).
const LOG_CHANNELS: [(&[&str], &str); 11] = [
    (&["logging", "fallback"], "brim-logs"),
    (&["logging", "routes", "mod_command"], "mod-logs"),
    (&["logging", "routes", "report"], "reports"),
    (&["appeals", "channel"], "appeals"),
    (&["logging", "routes", "auto_mod"], "automod-logs"),
    (&["logging", "routes", "anti_nuke"], "antinuke-logs"),
    (&["logging", "routes", "verification"], "verification-logs"),
    (&["logging", "routes", "join_gate"], "joingate-logs"),
    (&["logging", "routes", "join_raid"], "joinraid-logs"),
    (&["logging", "routes", "panic"], "panic-logs"),
    (&["logging", "routes", "server"], "server-logs"),
];

/// One click: the essential log channels plus the named ones (a channel per log type, "discord-logs"
/// for every Discord event without one), in private "Logs" and "Staff" categories. Set channels are kept.
async fn log_setup(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Setup>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let (cur, mut config) = store::current(&st.db, guild).await?;
    if cur != b.expected_version {
        return Err(ApiError::Conflict(cur)); // before any Discord side effect
    }
    let created = log_channels(&st, guild, &mut config, essentials_and(b.logs)).await?;
    if created.is_empty() {
        return Err(ApiError::BadRequest(
            "every log type already has a channel".into(),
        ));
    }
    let mut tx = st.db.begin().await?;
    let version = store::commit(&mut tx, guild, b.expected_version, &config, s.user_id).await?;
    tx.commit().await?;
    api::log_by(
        &st,
        &s,
        guild,
        &config,
        Some(version),
        "created the log channels",
    )
    .await;
    Ok(Json(json!({"version": version, "created": created})))
}

/// Once the channel is saved: the bot's enforce_quarantine may have denied it before it knew the
/// channel (with verification still off nothing marks a new one), so the shape goes back on.
async fn reshape_verification(
    st: &AppState,
    channel: &str,
    config: &Value,
) -> Result<(), ApiError> {
    if let Some(q) = own(&config["antinuke"]["quarantine_role"]) {
        let path = format!("/channels/{channel}/permissions/{q}");
        st.discord
            .call(Method::PUT, &path, Some(verification_overwrite(&q)))
            .await?;
    }
    Ok(())
}

/// The quarantine role's overwrite in the verification channel (`verification_shape`).
fn verification_overwrite(role: &str) -> Value {
    use brim_core::sections::{VERIFY_ALLOW, VERIFY_DENY};
    json!({"id": role, "type": 0, "allow": VERIFY_ALLOW.to_string(), "deny": VERIFY_DENY.to_string()})
}

/// Sets the quarantine overwrite on every channel (the verification channel gets the shape the
/// join quarantine can read) and returns how many it could not set (the bot lacks Manage Roles
/// there); `None` when the channels could not be listed. Runs after the commit, so a failure here
/// loses no setup.
pub(crate) async fn quarantine_channels(
    st: &AppState,
    guild: i64,
    role: &str,
    config: &Value,
) -> Option<usize> {
    let channels = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await
        .ok()?;
    let body = json!({"type": 0, "deny": brim_core::sections::QUARANTINE_DENY.to_string()});
    let mut failed = 0;
    let verification = own(&config["verification"]["channel"]);
    for id in channels.as_array()?.iter().filter_map(|c| c["id"].as_str()) {
        let path = format!("/channels/{id}/permissions/{role}");
        let body = match verification.as_deref() == Some(id) {
            true => verification_overwrite(role),
            false => body.clone(),
        };
        if st
            .discord
            .call(Method::PUT, &path, Some(body))
            .await
            .is_err()
        {
            failed += 1;
        }
    }
    Some(failed)
}

/// Essential logs, always created: the main log channel and the mod log.
const ESSENTIAL: [&str; 2] = ["brim-logs", "mod-logs"];

/// The [`ESSENTIAL`] log channels plus the named ones.
fn essentials_and(only: Option<Vec<String>>) -> impl Fn(&str) -> bool {
    move |name| ESSENTIAL.contains(&name) || only.iter().flatten().any(|n| n == name)
}

/// The one log channel set at `path` (`logging.fallback` or a route), made as Auto Setup makes it.
pub(crate) async fn log_channel(
    st: &AppState,
    guild: i64,
    config: &mut Value,
    path: &[&str],
) -> Result<Vec<&'static str>, ApiError> {
    let name = LOG_CHANNELS
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, n)| *n)
        .ok_or_else(|| ApiError::BadRequest("unknown log channel".into()))?;
    // An emptied field counts as unset, as the security score reads it.
    if let Some(v) = config.pointer_mut(&format!("/{}", path.join("/"))) {
        if v == "" {
            *v = Value::Null;
        }
    }
    log_channels(st, guild, config, |n| n == name).await
}

/// Staff-facing channels, kept in their own "Staff" category instead of "Logs".
const STAFF_CHANNELS: [&str; 2] = ["reports", "appeals"];

/// The overwrite PUTs, as (channel, target, body), that make the reused category `id` and its
/// synced channels private; none when it already is. The children get the same overwrites so they stay synced.
fn privatize(
    guild: i64,
    channels: &Value,
    id: &str,
    bot: &str,
    main: &[u64],
) -> Vec<(u64, u64, Value)> {
    let img = crate::recovery::live_image(guild, &json!([]), channels);
    let Some(cat) = img.channels.iter().find(|c| c.id.to_string() == id) else {
        return vec![];
    };
    let ows = log_overwrites(cat, guild as u64, bot.parse().unwrap_or_default(), main);
    std::iter::once(cat)
        .chain(synced_children(&img.channels, cat))
        .flat_map(|c| {
            ows.iter()
                .map(|o| (c.id, o.target, crate::ops::overwrite_body(o)))
        })
        .collect()
}

/// Log channel overwrites (`ows`, as Discord JSON) with the lock: @everyone denied
/// [`LOG_LOCK`] and every other allow of it dropped, the bot let in with it.
fn locked(ows: &[Value], guild: i64, bot: &str) -> Vec<Value> {
    let bits = |o: &Value, k: &str| {
        o[k].as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    ows.iter()
        .map(|o| {
            let (mut allow, mut deny) = (bits(o, "allow"), bits(o, "deny"));
            if o["id"] == bot {
                allow |= LOG_LOCK;
            } else {
                allow &= !LOG_LOCK;
            }
            if o["id"].as_str() == Some(guild.to_string().as_str()) {
                deny |= LOG_LOCK;
            }
            json!({"id": o["id"], "type": o["type"], "allow": allow.to_string(), "deny": deny.to_string()})
        })
        .collect()
}

fn private_category(guild: i64, bot: &str) -> Vec<Value> {
    let bot_id = bot.parse().unwrap_or(0);
    brim_antinuke::security::private_category(guild as u64, bot_id)
        .into_iter()
        .map(|o| {
            let id = if o.target == bot_id { bot.to_string() } else { o.target.to_string() };
            json!({"id": id, "type": if o.is_role { 0 } else { 1 }, "allow": o.allow.to_string(), "deny": o.deny.to_string()})
        })
        .collect()
}

/// The private category `name`: the existing one, so a later run adds to it, or a new one.
/// Hidden from @everyone (its role id is the guild id) and the main roles; the bot (its user id
/// is the app id) keeps View Channel, Send Messages and Embed Links. Staff see it through their
/// roles. A reused category that anyone can see is made private first.
pub(crate) async fn category(
    st: &AppState,
    guild: i64,
    channels: &Value,
    name: &str,
    main: &[u64],
) -> Result<String, ApiError> {
    let existing = channels
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|c| (c["type"] == 4 && c["name"] == name).then(|| own(&c["id"]))?);
    if let Some(id) = &existing {
        for (ch, target, body) in privatize(guild, channels, id, &st.discord.client_id, main) {
            st.discord
                .call(
                    Method::PUT,
                    &format!("/channels/{ch}/permissions/{target}"),
                    Some(body),
                )
                .await?;
        }
    }
    create(
        st,
        guild,
        existing,
        "channels",
        json!({"name": name, "type": 4, "permission_overwrites": private_category(guild, &st.discord.client_id)}),
    )
    .await
}

/// Creates the missing log channels under "Logs" (reports and appeals under "Staff"), and
/// sets them in `config`. `wanted` picks the channel names to make ("discord-logs" for the
/// Discord events); Auto Setup makes just the [`ESSENTIAL`] ones. An unused text channel with the
/// same name is taken over instead of made. Returns the names of the channels it set; nothing
/// when all have one.
/// The log channels `wanted` that have no channel set, and the Discord events with none.
fn log_gaps(
    config: &Value,
    wanted: impl Fn(&str) -> bool,
) -> (Vec<(&'static [&'static str], &'static str)>, Vec<String>) {
    let missing: Vec<_> = LOG_CHANNELS
        .into_iter()
        .filter(|(p, name)| at(config, p).is_null() && wanted(name))
        .collect();
    let unset: Vec<String> = if wanted("discord-logs") {
        LogEvent::ALL
            .iter()
            // Votes and event sign-ups stay off until a person picks a channel for them.
            .filter(|e| !e.quiet())
            .filter_map(|e| serde_json::to_value(e).ok()?.as_str().map(String::from))
            .filter(|e| config["logging"]["events"][e].is_null())
            .collect()
    } else {
        vec![]
    };
    (missing, unset)
}

/// Whether "Create a channel per log type" would make anything, so the page can hide the button.
async fn log_setup_todo(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let (_, config) = store::current(&st.db, guild).await?;
    let (missing, unset) = log_gaps(&config, |_| true);
    Ok(Json(
        json!({"todo": !missing.is_empty() || !unset.is_empty()}),
    ))
}

pub(crate) async fn log_channels(
    st: &AppState,
    guild: i64,
    config: &mut Value,
    wanted: impl Fn(&str) -> bool,
) -> Result<Vec<&'static str>, ApiError> {
    let (missing, unset) = log_gaps(config, wanted);
    if unset.is_empty() && missing.is_empty() {
        return Ok(vec![]);
    }
    let channels = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    // `None` is the shared discord-logs channel for every unset Discord event.
    let todo: Vec<(Option<&[&str]>, &str)> = missing
        .into_iter()
        .map(|(path, name)| (Some(path), name))
        .chain((!unset.is_empty()).then_some((None, "discord-logs")))
        .collect();
    let category_of = |name: &str| {
        if STAFF_CHANNELS.contains(&name) {
            "Staff"
        } else {
            "Logs"
        }
    };
    let main: Vec<u64> = config["lockdown"]["main_roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str()?.parse().ok())
        .collect();
    let mut parents = vec![];
    for cat in ["Logs", "Staff"] {
        if todo.iter().any(|(_, name)| category_of(name) == cat) {
            parents.push((cat, category(st, guild, &channels, cat, &main).await?));
        }
    }
    let mut created = vec![];
    for (path, name) in todo {
        let parent = parents
            .iter()
            .find(|(c, _)| *c == category_of(name))
            .map(|(_, id)| id);
        let id = match unused_channel(&channels, config, name) {
            // Taken over: moved into the category with exactly the overwrites a new channel there
            // gets (the category's, read now), so no old allow keeps someone reading the logs.
            // Its messages stay.
            Some(id) => {
                let ows = match parent {
                    Some(p) => {
                        let cat = st
                            .discord
                            .call(Method::GET, &format!("/channels/{p}"), None)
                            .await?;
                        cat["permission_overwrites"].clone()
                    }
                    None => json!([]),
                };
                let body = json!({"parent_id": parent, "permission_overwrites": ows});
                st.discord
                    .call(Method::PATCH, &format!("/channels/{id}"), Some(body))
                    .await?;
                id
            }
            None => {
                let body = json!({"name": name, "type": 0, "parent_id": parent});
                create(st, guild, None, "channels", body).await?
            }
        };
        match path.and_then(|p| p.split_last()) {
            Some((last, parents)) => {
                parents.iter().fold(&mut *config, |v, k| &mut v[*k])[*last] = json!(id)
            }
            None => {
                for e in &unset {
                    config["logging"]["events"][e] = json!(id);
                }
            }
        }
        created.push(name);
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_gaps_empty_only_when_every_channel_is_set() {
        use serde_json::json;
        let mut c = json!({});
        assert!(!super::log_gaps(&c, |n| n != "discord-logs").0.is_empty());
        for (p, _) in super::LOG_CHANNELS {
            let mut v = &mut c;
            for k in p {
                v = &mut v[*k];
            }
            *v = json!("1");
        }
        let (missing, unset) = super::log_gaps(&c, |n| n != "discord-logs");
        assert!(missing.is_empty() && unset.is_empty());
        assert!(!super::log_gaps(&c, |_| true).1.is_empty());
    }

    #[test]
    fn log_channels_are_made_locked() {
        let ows = super::locked(
            &[
                json!({"id": "1", "type": 0, "deny": "1024"}),
                json!({"id": "7", "type": 1, "allow": "19456"}),
                json!({"id": "5", "type": 0, "allow": "8192"}),
            ],
            1,
            "7",
        );
        let lock = brim_antinuke::security::LOG_LOCK;
        assert_eq!(ows[0]["deny"], json!((1024 | lock).to_string()));
        assert_eq!(ows[1]["allow"], json!((19456 | lock).to_string()));
        assert_eq!(ows[2]["allow"], json!("0"));
    }

    use super::*;

    #[test]
    fn coverage_rows() {
        let config = json!({"antinuke": {"enabled": true, "limits": {"Ban": {"per_minute": 2}, "Unban": {}}}});
        let permits = json!({"custom": [
            {"name": "Mods", "whitelist": ["wl_kick_ban"]},
            {"name": "Bots", "whitelist": ["antinuke"]},
            {"name": "Chat", "whitelist": ["automod"]},
        ]});
        let c = coverage(&config, &permits);
        let rows = c["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 29);
        let row = |a: &str| rows.iter().find(|r| r["action"] == a).unwrap();
        // Kick reads the Ban card and its per-action flag.
        assert_eq!(row("Kick")["state"], "on");
        assert_eq!(row("Kick")["per_minute"], 2);
        assert_eq!(row("Kick")["per_hour"], 15);
        assert_eq!(row("Kick")["undo"], "no");
        assert_eq!(row("Timeout")["undo"], "no");
        assert_eq!(row("ChannelDelete")["undo"], "backup");
        assert_eq!(row("Ban")["undo"], "yes");
        assert_eq!(row("Ban")["permits"], json!(["Mods", "Bots"]));
        // Opt-in: a card saved is watched, one never saved is not set up.
        assert_eq!(row("Unban")["state"], "on");
        assert_eq!(row("Timeout")["state"], "opt_in");
        assert_eq!(row("Timeout")["per_minute"], Value::Null);
        // The blanket flag covers every row, a per-action flag only its own.
        assert!(rows
            .iter()
            .all(|r| r["permits"].as_array().unwrap().contains(&json!("Bots"))));
        assert_eq!(row("RoleDelete")["permits"], json!(["Bots"]));
        assert_eq!(row("Prune")["state"], "off");
        assert_eq!(row("BotAdd")["state"], "off");
        assert_eq!(row("ChannelDelete")["heat"], 25);
        assert_eq!(c["enabled"], true);
        // Unreadable section: the bot's defaults (off).
        assert_eq!(
            coverage(&json!({"antinuke": {"enabled": "yes"}}), &json!({}))["enabled"],
            false
        );
    }

    /// An [`AppState`] whose Discord API is `app`, with no database.
    async fn mock_state(app: axum::Router) -> AppState {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        AppState {
            db: sqlx::postgres::PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(200))
                .connect_lazy("postgres://x@127.0.0.1:1/x")
                .unwrap(),
            discord: crate::discord::Discord {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                client_id: String::new(),
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
        }
    }

    #[tokio::test]
    async fn show_bot_gives_the_bot_a_coloured_hoisted_role_under_quarantine() {
        use axum::routing::{get, patch, put};
        // `held`: the bot's roles. Returns every changing call as "METHOD path" -> body.
        async fn run(held: Value) -> Vec<(String, Value)> {
            let seen: crate::Shared<String, Value> = Default::default();
            let (a, b, c, d) = (seen.clone(), seen.clone(), seen.clone(), seen.clone());
            let roles = json!([
                {"id": "1", "name": "@everyone", "position": 0, "permissions": "0"},
                {"id": "9", "name": "Brim", "position": 6, "managed": true, "permissions": "8"},
                {"id": "8", "name": "Quarantine", "position": 5, "permissions": "0"},
                {"id": "60", "name": "Brim", "position": 1, "permissions": "0"},
                {"id": "61", "name": "Brim", "position": 2, "permissions": "8"},
                {"id": "62", "name": "Brim", "position": 3, "permissions": "0"}
            ]);
            let app = axum::Router::new()
                .route(
                    "/guilds/1/roles",
                    get(move || async move { Json(roles) })
                        .patch(move |Json(v): Json<Value>| async move {
                            a.lock().unwrap().insert("PATCH roles".into(), v);
                            Json(json!([]))
                        })
                        .post(move |Json(v): Json<Value>| async move {
                            d.lock().unwrap().insert("POST".into(), v);
                            Json(json!({"id": "70"}))
                        }),
                )
                .route(
                    "/guilds/1/members",
                    get(|| async {
                        Json(json!([
                            {"user": {"id": "77"}, "roles": ["60", "62"]},
                            {"user": {"id": "5"}, "roles": ["62"]}
                        ]))
                    }),
                )
                .route(
                    "/guilds/1/members/77",
                    get(move || async move { Json(json!({"roles": held})) }),
                )
                .route(
                    "/guilds/1/roles/{id}",
                    patch(
                        move |Path(id): Path<String>, Json(v): Json<Value>| async move {
                            b.lock().unwrap().insert(format!("PATCH {id}"), v);
                            Json(json!({}))
                        },
                    ),
                )
                .route(
                    "/guilds/1/members/77/roles/{id}",
                    put(move |Path(id): Path<String>| async move {
                        c.lock().unwrap().insert(format!("PUT {id}"), json!(null));
                        axum::http::StatusCode::NO_CONTENT
                    }),
                );
            let mut st = mock_state(app).await;
            st.discord.client_id = "77".into();
            let config = json!({"antinuke": {"quarantine_role": "8"}});
            show_bot(&st, 1, &config).await.unwrap();
            let mut v: Vec<(String, Value)> = seen.lock().unwrap().clone().into_iter().collect();
            v.sort_by(|x, y| x.0.cmp(&y.0));
            v
        }
        // No display role yet: one is made with no permissions, given to the bot and moved right
        // under the quarantine role. 61 has permissions, so it is never taken over.
        let calls = run(json!(["9", "61"])).await;
        assert_eq!(
            calls,
            [
                (
                    "PATCH roles".to_string(),
                    json!([{"id": "70", "position": 4}])
                ),
                (
                    "POST".to_string(),
                    json!({"name": "Brim", "color": 0x9B7BFF, "hoist": true, "permissions": "0"})
                ),
                ("PUT 70".to_string(), json!(null)),
            ]
        );
        // The one only it holds is reused and only coloured, shown separately and moved; 62 is
        // held by a member too, so it is left alone.
        let calls = run(json!(["9", "62", "60"])).await;
        assert_eq!(
            calls,
            [
                (
                    "PATCH 60".to_string(),
                    json!({"color": 0x9B7BFF, "hoist": true})
                ),
                (
                    "PATCH roles".to_string(),
                    json!([{"id": "60", "position": 4}])
                ),
            ]
        );
    }

    #[tokio::test]
    async fn order_roles_puts_quarantine_display_staff_others_then_member() {
        let seen: crate::Shared<String, Value> = Default::default();
        let s2 = seen.clone();
        // Today, top down: bot 9, other 40, member 50, staff 60 and 61, display 70, quarantine 80.
        let roles = json!([
            {"id": "1", "position": 0, "permissions": "0"},
            {"id": "9", "position": 8, "permissions": "0", "managed": true},
            {"id": "40", "position": 7, "permissions": "0"},
            {"id": "50", "position": 6, "permissions": "0"},
            {"id": "60", "position": 5, "permissions": "0"},
            {"id": "61", "position": 4, "permissions": "0"},
            {"id": "70", "position": 3, "permissions": "0"},
            {"id": "80", "position": 2, "permissions": "0"},
            {"id": "99", "position": 9, "permissions": "0"}
        ]);
        let app = axum::Router::new()
            .route(
                "/guilds/1/roles",
                axum::routing::get(move || async move { Json(roles) }).patch(
                    move |Json(b): Json<Value>| async move {
                        let mut s = s2.lock().unwrap();
                        let n = s.len();
                        s.insert(n.to_string(), b);
                        Json(json!([]))
                    },
                ),
            )
            .route(
                "/users/@me",
                axum::routing::get(|| async { Json(json!({"id": "77"})) }),
            )
            .route(
                "/guilds/1/members/77",
                axum::routing::get(|| async { Json(json!({"roles": ["9"]})) }),
            );
        let st = mock_state(app).await;
        // 61 is Owner, 60 Moderators (STAFF_ROLES is highest first).
        let made = StaffRoles(vec![Some("61".into()), None, None, Some("60".into())]);
        let config =
            json!({"lockdown": {"main_roles": ["50"]}, "antinuke": {"quarantine_role": "80"}});
        assert_eq!(order_roles(&st, 1, &made, &config, Some("70")).await, None);
        let seen = seen.lock().unwrap();
        // 99 above the bot is left alone.
        assert_eq!(
            seen["0"],
            json!([
                {"id": "50", "position": 1},
                {"id": "40", "position": 2},
                {"id": "60", "position": 3},
                {"id": "61", "position": 4},
                {"id": "70", "position": 5},
                {"id": "80", "position": 6}
            ])
        );
        assert_eq!(seen.len(), 1);
    }

    #[tokio::test]
    async fn quarantine_role_takes_over_the_oldest_fit_unheld_role_and_deletes_none() {
        let seen: crate::Shared<String, Value> = Default::default();
        let s2 = seen.clone();
        let roles = json!([
            {"id": "1", "name": "@everyone", "position": 0, "permissions": "0"},
            {"id": "9", "name": "Brim", "position": 5, "managed": true, "permissions": "8"},
            {"id": "20", "name": "Quarantine", "position": 6, "permissions": "0"},
            {"id": "30", "name": "Quarantine", "position": 1, "managed": true, "permissions": "0"},
            {"id": "50", "name": "Quarantine", "position": 3, "permissions": "8"},
            {"id": "40", "name": "Quarantine", "position": 2, "permissions": "8"}
        ]);
        let app = axum::Router::new()
            .route(
                "/guilds/1/roles",
                axum::routing::get(move || async move { Json(roles) }),
            )
            .route(
                "/users/@me",
                axum::routing::get(|| async { Json(json!({"id": "77"})) }),
            )
            .route(
                "/guilds/1/members",
                axum::routing::get(|| async {
                    Json(json!([{"user": {"id": "5"}, "roles": ["40"]}]))
                }),
            )
            .route(
                "/guilds/1/members/77",
                axum::routing::get(|| async { Json(json!({"roles": ["9"]})) }),
            )
            .route(
                "/guilds/1/roles/{id}",
                axum::routing::patch(move |Path(id): Path<String>| async move {
                    s2.lock().unwrap().insert(id, json!(true));
                    Json(json!({}))
                }),
            );
        let st = mock_state(app).await;
        // 20 is above the bot, 30 is managed and a member holds 40: 50 is the oldest that can be one.
        let role = quarantine_role(&st, 1, &json!({})).await.unwrap();
        assert_eq!(role, "50");
        let patched: Vec<String> = seen.lock().unwrap().keys().cloned().collect();
        assert_eq!(patched, ["50"], "only the taken-over role is changed");
    }

    #[tokio::test]
    async fn a_saved_verification_channel_gets_the_shape_back() {
        use brim_core::sections::{VERIFY_ALLOW, VERIFY_DENY};
        let seen: crate::Shared<String, Value> = Default::default();
        let s2 = seen.clone();
        let app = axum::Router::new().route(
            "/channels/700/permissions/500",
            axum::routing::put(move |Json(b): Json<Value>| async move {
                s2.lock().unwrap().insert("body".into(), b);
                ""
            }),
        );
        let st = mock_state(app).await;
        reshape_verification(&st, "700", &json!({})).await.unwrap();
        assert!(
            seen.lock().unwrap().is_empty(),
            "no quarantine role, nothing to do"
        );
        let config = json!({"antinuke": {"quarantine_role": "500"}});
        reshape_verification(&st, "700", &config).await.unwrap();
        let body = seen.lock().unwrap()["body"].clone();
        assert_eq!(body["allow"], json!(VERIFY_ALLOW.to_string()));
        assert_eq!(body["deny"], json!(VERIFY_DENY.to_string()));
    }

    /// A recorded transcript opens the gate even when no config version named its channel.
    #[tokio::test]
    async fn ticket_log_accepts_a_recorded_transcript() {
        use crate::tickets::tests::{cleanup, mock};
        let Some((mut st, s, g)) = crate::api::g3_handler_tests::setup(9_940_000_000).await else {
            return;
        };
        let (api, seen) = mock(g).await;
        st.discord.api = api;
        let read = || ticket_log(State(st.clone()), s.clone(), Path((g, 801, g as u64)));
        // The message id is the guild id, unique per test run (log_message_id is UNIQUE).
        assert!(matches!(read().await, Err(ApiError::NotFound)));
        assert!(
            seen.lock().unwrap().is_empty(),
            "Discord asked before the gate"
        );
        // The same message recorded for another server does not count.
        for (guild, number) in [(g + 1, 1), (g, 1)] {
            sqlx::query("insert into tickets (guild_id, number, user_id) values ($1, $2, 55)")
                .bind(guild)
                .bind(number)
                .execute(&st.db)
                .await
                .unwrap();
        }
        sqlx::query(
            "insert into ticket_transcripts (guild_id, number, log_channel_id, log_message_id, complete)
             values ($1, 1, 801, $2, true)",
        )
        .bind(g + 1)
        .bind(g)
        .execute(&st.db)
        .await
        .unwrap();
        assert!(matches!(read().await, Err(ApiError::NotFound)));
        assert!(
            seen.lock().unwrap().is_empty(),
            "another server's transcript"
        );
        sqlx::query("update ticket_transcripts set guild_id = $1 where guild_id = $2")
            .bind(g)
            .bind(g + 1)
            .execute(&st.db)
            .await
            .unwrap();
        // Past the gate: the mock has no message route, so the read still ends in 404.
        assert!(matches!(read().await, Err(ApiError::NotFound)));
        assert!(seen.lock().unwrap().contains_key("GET 801"));
        cleanup(&st, g).await;
        cleanup(&st, g + 1).await;
    }

    #[test]
    fn log_channel_paths_are_real_log_routes() {
        let mut config = json!({});
        for (i, (path, _)) in LOG_CHANNELS.iter().enumerate() {
            let (last, parents) = path.split_last().unwrap();
            parents.iter().fold(&mut config, |v, k| &mut v[*k])[*last] = json!((i + 1).to_string());
        }
        let log: brim_logging::LogConfig =
            serde_json::from_value(config["logging"].clone()).unwrap();
        assert!(log.fallback.is_some());
        assert_eq!(log.routes.len(), 9);
        assert!(at(&config, &["appeals", "channel"]).is_string());
    }

    #[test]
    fn a_reused_public_category_and_its_synced_channels_turn_private() {
        let public = json!([
            {"id": "10", "name": "Logs", "type": 4, "permission_overwrites": [{"id": "99", "type": 0, "allow": "1024", "deny": "0"}]},
            {"id": "11", "name": "mod-logs", "type": 0, "parent_id": "10", "permission_overwrites": [{"id": "99", "type": 0, "allow": "1024", "deny": "0"}]},
            {"id": "12", "name": "chat", "type": 0, "parent_id": "10", "permission_overwrites": []}
        ]);
        let puts = privatize(1, &public, "10", "7", &[99]);
        let hit: Vec<_> = puts
            .iter()
            .map(|(c, t, b)| (*c, *t, b["deny"].as_str().unwrap()))
            .collect();
        // @everyone and the main role 99 lose View (@everyone also gets the delete lock), the bot is let in; the synced child follows, chat (not synced) does not.
        assert_eq!(
            hit,
            [
                (10, 1, "9232"),
                (10, 99, "1024"),
                (10, 7, "0"),
                (11, 1, "9232"),
                (11, 99, "1024"),
                (11, 7, "0")
            ]
        );
        // Private but not locked: only the lock goes on.
        let private = json!([{"id": "10", "name": "Logs", "type": 4, "permission_overwrites": [{"id": "1", "type": 0, "allow": "0", "deny": "1024"}]}]);
        assert_eq!(privatize(1, &private, "10", "7", &[99]).len(), 2);
        let locked = json!([{"id": "10", "name": "Logs", "type": 4, "permission_overwrites": [
            {"id": "1", "type": 0, "allow": "0", "deny": "9232"},
            {"id": "7", "type": 1, "allow": "27664", "deny": "0"}
        ]}]);
        assert!(privatize(1, &locked, "10", "7", &[99]).is_empty());
    }

    #[test]
    fn new_levels_keep_a_level_role_changed_during_the_run() {
        let before = json!({"trusted_admin": "1"});
        // The Owner removed the TA role and set an Owner role meanwhile.
        let now = json!({"owner": "8"});
        let made = json!({"trusted_admin": "1", "owner": "9"});
        assert_eq!(
            super::new_levels(&before, &now, &made),
            json!({"owner": "8"})
        );
        // Nothing changed meanwhile: the setup's roles go in.
        let made = json!({"trusted_admin": "2", "owner": "9"});
        assert_eq!(
            super::new_levels(&before, &before, &made),
            json!({"trusted_admin": "2", "owner": "9"})
        );
    }

    #[test]
    fn put_changes_keeps_a_save_made_meanwhile() {
        let before = json!({"antinuke": {"quarantine_role": "1"}, "misc": {"x": 1}});
        let after = json!({"antinuke": {"quarantine_role": "2"}, "misc": {"x": 1}, "logging": {"fallback": "9"}});
        let mut latest =
            json!({"antinuke": {"quarantine_role": "1", "punish": true}, "misc": {"x": 5}});
        super::put_changes(&mut latest, &before, &after);
        assert_eq!(
            latest,
            json!({"antinuke": {"quarantine_role": "2", "punish": true}, "misc": {"x": 5}, "logging": {"fallback": "9"}})
        );
    }

    #[test]
    fn unused_channel_takes_the_oldest_unused_text_channel() {
        let channels = json!([
            {"id": "30", "name": "reports", "type": 0},
            {"id": "2", "name": "reports", "type": 0},
            {"id": "9", "name": "reports", "type": 0},
            {"id": "3", "name": "reports", "type": 4},
            {"id": "4", "name": "appeals", "type": 0}
        ]);
        let config = json!({"logging": {"events": {"ban": "2"}}});
        assert_eq!(
            unused_channel(&channels, &config, "reports").as_deref(),
            Some("9")
        );
        assert_eq!(unused_channel(&channels, &config, "panic-logs"), None);
        let old = json!([{"id": "5", "name": "tommybot-logs", "type": 0}]);
        assert_eq!(
            unused_channel(&old, &json!({}), "brim-logs").as_deref(),
            Some("5")
        );
        let config = json!({"misc": {"quarantine_role": "6"}});
        assert_eq!(oldest_unused(["6", "50", "7"], &config), Some("7"));
        assert_eq!(oldest_unused(["6"], &config), None);
    }
}

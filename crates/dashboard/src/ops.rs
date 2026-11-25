use crate::{
    access::Level,
    api::{log_by, log_change, require, Res},
    auth::{self, Sess},
    ApiError, AppState,
};
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use brim_antinuke::{
    security::{
        checks, expression_targets, log_overwrites, move_under_bot, overwrite_targets, setting_fix,
        strip_targets, stripped, stripped_overwrite, synced_children, Check, Fix, GuildSafety,
        Setup, Severity, CATCH_CHANNEL, EVERYONE_TO_MAIN, EXPRESSIONS, EXPRESSION_PERMS,
        LOG_FALLBACK, MEMBER_ROLE, MODULES, MOD_LOG, PRIVATE, QUARANTINE_CHANNELS, QUARANTINE_ROLE,
        STANDARD_ACCESS,
    },
    Inverse, RoleImg,
};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

type ActionRow = (
    i64,
    i64,
    String,
    Option<i64>,
    Option<Value>,
    bool,
    chrono::DateTime<chrono::Utc>,
);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/guilds/{guild}/score", get(score))
        .route("/api/guilds/{guild}/score/fix", post(fix))
        .route(
            "/api/guilds/{guild}/enforce-permits",
            get(enforce_permits_preview),
        )
        .route("/api/guilds/{guild}/actions", get(actions))
        .route("/api/guilds/{guild}/revert", post(revert))
        .route("/api/guilds/{guild}/rescue", get(rescue_status))
        .route(
            "/api/guilds/{guild}/rescue/regenerate",
            post(rescue_regenerate),
        )
        .route("/api/rescue/claim", post(rescue_claim))
        .route(
            "/api/guilds/{guild}/rescue/give-back",
            post(rescue_give_back),
        )
}

#[derive(Deserialize, Clone)]
pub(crate) struct Role {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub position: i64,
    #[serde(default)]
    pub managed: bool,
    permissions: String,
    #[serde(default)]
    pub color: u32,
}

impl Role {
    pub fn perms(&self) -> u64 {
        self.permissions.parse().unwrap_or(0)
    }

    /// [`brim_core::roles::unfit`]; `position` is already a rank.
    pub fn unfit(&self, guild: i64, bot_pos: i64) -> Option<&'static str> {
        let id = self.id.parse().unwrap_or(0);
        brim_core::roles::unfit(guild as u64, id, self.managed, self.position, bot_pos)
    }
}

/// Positions are ranks in Discord's order (ties broken by id), as in `recovery::live_image`.
async fn roles_of(st: &AppState, guild: i64) -> Result<Vec<Role>, ApiError> {
    let mut roles: Vec<Role> = serde_json::from_value(
        st.discord
            .call(Method::GET, &format!("/guilds/{guild}/roles"), None)
            .await?,
    )
    .map_err(anyhow::Error::from)?;
    let rank = brim_core::roles::ranks(
        roles
            .iter()
            .map(|r| (r.id.parse().unwrap_or(0), r.position)),
        guild as u64,
    );
    for r in &mut roles {
        r.position = rank[&r.id.parse().unwrap_or(0)];
    }
    Ok(roles)
}

/// Guild roles and the position of the bot's top role.
pub(crate) async fn roles_and_bot_pos(
    st: &AppState,
    guild: i64,
) -> Result<(Vec<Role>, i64), ApiError> {
    let (roles, mine) = roles_and_mine(st, guild).await?;
    let bot_pos = top_of(&roles, &mine);
    Ok((roles, bot_pos))
}

/// Highest position among the roles with ids in `mine` (0 = @everyone only).
fn top_of(roles: &[Role], mine: &[String]) -> i64 {
    roles
        .iter()
        .filter(|r| mine.contains(&r.id))
        .map(|r| r.position)
        .max()
        .unwrap_or(0)
}

/// The server's roles and the ids of the bot's own roles.
async fn roles_and_mine(st: &AppState, guild: i64) -> Result<(Vec<Role>, Vec<String>), ApiError> {
    let roles = roles_of(st, guild).await?;
    let me = st.discord.call(Method::GET, "/users/@me", None).await?;
    let mine = st
        .discord
        .member_roles(guild, me["id"].as_str().unwrap_or_default())
        .await?;
    Ok((roles, mine))
}

/// What the security score reads from Discord, fetched once for the check and its fixes.
pub(crate) struct Live {
    pub safety: GuildSafety,
    pub setup: Setup,
    pub bot_pos: i64,
    pub image: brim_antinuke::Image,
    /// Raw role and channel JSON, kept for the backup a fix takes first.
    pub roles: Value,
    pub channels: Value,
    /// Raw guild JSON (`GET /guilds/{id}`), for the server settings of a saved layout.
    pub guild: Value,
}

pub(crate) async fn live(st: &AppState, guild: i64, config: &Value) -> Result<Live, ApiError> {
    let get = |path: String| async move { st.discord.call(Method::GET, &path, None).await };
    let (g, roles, channels, me) = tokio::try_join!(
        get(format!("/guilds/{guild}")),
        get(format!("/guilds/{guild}/roles")),
        get(format!("/guilds/{guild}/channels")),
        get("/users/@me".into()),
    )?;
    let me = me["id"].as_str().unwrap_or_default();
    let mine = st.discord.member_roles(guild, me).await?;
    let image = crate::recovery::live_image(guild, &roles, &channels);
    let bot_pos = image
        .roles
        .iter()
        .filter(|r| mine.contains(&r.id.to_string()))
        .map(|r| r.position)
        .max()
        .unwrap_or(0);
    let extra: Vec<i64> =
        sqlx::query_scalar("select user_id from extra_owners where guild_id = $1")
            .bind(guild)
            .fetch_all(&st.db)
            .await?;
    let id = |v: &Value| v.as_str().and_then(|x| x.parse::<u64>().ok());
    let n = |k: &str| g[k].as_u64().unwrap_or(0) as u8;
    Ok(Live {
        safety: GuildSafety {
            id: guild as u64,
            mfa: n("mfa_level"),
            verification: n("verification_level"),
            content_filter: n("explicit_content_filter"),
            notifications: n("default_message_notifications"),
        },
        setup: Setup {
            quarantine_role: id(&config["antinuke"]["quarantine_role"]),
            log_fallback: config["logging"]["fallback"]
                .as_str()
                .is_some_and(|f| !f.is_empty()),
            unwhitelisted_staff: brim_core::permits::unwhitelisted_staff(&st.db, guild).await?,
            bot_roles: mine.iter().filter_map(|r| r.parse().ok()).collect(),
            mod_log: config["logging"]["routes"]["mod_command"].is_string(),
            // Every module defaults to off, so a missing switch reads as off.
            modules: MODULES
                // A part is off while its module is off (`modules`, a missing switch is on).
                .map(|(name, k, _)| {
                    let module = brim_core::sections::module_of(k)
                        .is_none_or(|m| config["modules"][m] != false);
                    (name, config[k]["enabled"] == true && module)
                })
                .to_vec(),
            main_roles: config["lockdown"]["main_roles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(id)
                .collect(),
            mod_roles: brim_core::permits::mod_roles(&st.db, guild)
                .await?
                .into_iter()
                .map(|r| r as u64)
                .collect(),
            trusted: extra
                .into_iter()
                .map(|u| u as u64)
                .chain(id(&g["owner_id"]))
                .chain(me.parse().ok())
                .collect(),
            log_channels: log_channel_ids(config),
            bot: me.parse().unwrap_or_default(),
            verification_channel: id(&config["verification"]["channel"]),
            verification_roles: config["verification"]["roles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(id)
                .collect(),
            staff_roles: brim_core::permits::all_staff_roles(&st.db, guild)
                .await?
                .into_iter()
                .map(|r| r as u64)
                .collect(),
        },
        bot_pos,
        image,
        roles,
        channels,
        guild: g,
    })
}

async fn score(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    let level = require(&st, &s, guild, Level::Viewer, false).await?;
    let (_, config) = crate::store::current(&st.db, guild).await?;
    let l = live(&st, guild, &config).await?;
    let marked: bool =
        sqlx::query_scalar("select exists(select 1 from guild_setup_done where guild_id = $1)")
            .bind(guild)
            .fetch_one(&st.db)
            .await?;
    // A server set up before the marker existed counts as set up when the basics are there.
    let setup_done =
        marked || (l.setup.quarantine_role.is_some() && !l.setup.main_roles.is_empty());
    let mut rows = checks(
        &l.safety,
        &l.image.roles,
        &l.image.channels,
        l.bot_pos,
        &l.setup,
    );
    rows.extend(crate::channels::overview_check(&st, guild, &config, &l).await?);
    rows.extend(crate::staff_role::overview_check(&st, guild, &l.image.roles).await?);
    rows.extend(crate::staff_role::given_check(&st, guild, &config).await?);
    rows.extend(
        crate::exempt_roles::overview_check(&st, guild, &config, &l.image.roles, level).await?,
    );
    rows.extend(crate::msghooks::overview_check(&st, guild, &l.image.channels).await?);
    rows.extend(crate::feeds::overview_check(&st, guild, &l.image.channels).await?);
    // Its rows name paused and kept permit holders, which Viewers do not see (security review).
    if level > Level::Viewer {
        rows.extend(handover_check(&st, guild).await?);
    }
    let staff = brim_core::permits::all_staff_roles(&st.db, guild).await?;
    let staff_u = crate::channel_access::live_staff(
        &staff.iter().map(|&r| r as u64).collect::<Vec<_>>(),
        &l.image.roles,
    );
    rows.extend(crate::channel_access::staff_access_check(
        &l.image.channels,
        &staff_u,
        &log_channel_ids(&config),
    ));
    rows.extend(brim_antinuke::security::staff_hoist_check(
        &l.image.roles,
        &brim_core::permits::grouped_staff_roles(&st.db, guild).await?,
        l.bot_pos,
    ));
    rows.extend(brim_antinuke::security::catch_gone_check(
        &l.image.channels,
        config["catch"]["channel"]
            .as_str()
            .and_then(|x| x.parse().ok()),
        config["catch"]["enabled"] == true && config["modules"]["moderation"] != false,
    ));
    rows.extend(brim_antinuke::security::ticket_log_check(
        &l.image.channels,
        config["tickets"]["log_channel"]
            .as_str()
            .and_then(|x| x.parse().ok()),
    ));
    rows.extend(brim_antinuke::security::bot_hoist_check(
        &l.image.roles,
        &l.setup.bot_roles,
    ));
    rows.extend(brim_antinuke::security::staff_mention_note(
        &l.image.roles,
        &staff,
    ));
    // The standard set hides channels from members without the main role: shown only while
    // verification hands that role out to new members.
    rows.extend(crate::channel_access::overview_check(
        &st,
        guild,
        &l.image.channels,
        &l.image.roles,
        &config,
    ));
    rows.extend(crate::lockdown::paused_rule_checks(&st, guild).await?);
    // D48 (S-h3 6): a section the bot cannot read runs on defaults, Anti-nuke off among them.
    let unread = crate::api::unread_sections(&config);
    if !unread.is_empty() {
        rows.push(Check {
            id: "sections_unread".into(),
            severity: Severity::Bad,
            title: "Settings not read",
            detail: format!(
                "Brim could not read these saved settings and runs them on their defaults (off for most systems, Anti-nuke too): {}. A newer Brim version saved them.",
                unread.join(", ")
            ),
            fix: None,
            steps: brim_antinuke::security::steps("sections_unread"),
        });
    }
    // D70b: old zero values are left as they are, but the owner should see them.
    let old = brim_core::limits::stored_outside(&config);
    if !old.is_empty() {
        rows.push(Check {
            id: "stored_out_of_range".into(),
            severity: Severity::Warn,
            title: "Settings outside their range",
            detail: format!(
                "Saved before the limits: {}. A 0 window or count switches that protection off; a value marked \"used as\" still works at that value.",
                old.join(", ")
            ),
            fix: Some(Fix {
                label: "Move into range",
                role_id: None,
                channel_id: None,
                setting: Some(CLAMP),
                move_under_bot: false,
            }),
            steps: vec![],
        });
    }
    // Owner decision 2026-11-22: suspicious accounts, and members whose appeal was accepted in the
    // last week, get a little more heat.
    let automod = &config["automod"];
    // Its heat counts only with Anti Spam on (on unless switched off).
    if automod["enabled"] == true
        && automod["anti_spam"] != false
        && automod["filters"]["suspicion"]["enabled"] != true
    {
        rows.push(Check {
            id: "suspicion_off".into(),
            severity: Severity::Warn,
            title: "Suspicious account filter off",
            detail: "Accounts with no avatar, very new accounts and members whose appeal was accepted in the last 7 days get no extra automod heat.".into(),
            fix: Some(Fix {
                label: "Turn on",
                role_id: None,
                channel_id: None,
                setting: Some(SUSPICION),
                move_under_bot: false,
            }),
            steps: vec![],
        });
    }
    rows.extend(enforce_permits_check(&config));
    let mut rows = json!(rows);
    rows.as_array_mut()
        .into_iter()
        .for_each(|r| r.extend(crate::security_setup::rows(&config)));
    // Move into range's confirm names each value and where it goes.
    if let Some(r) = rows
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|r| r["fix"]["setting"] == CLAMP)
    {
        let mut c = config.clone();
        let moves: Vec<String> = crate::access::SECTIONS
            .iter()
            .filter_map(|k| Some(brim_core::limits::clamp(k, c.get_mut(*k)?, &Value::Null)))
            .flatten()
            .collect();
        r["moves"] = json!(moves);
    }
    // Move to main roles and the standard set ask first how many members they lock out.
    // Read once for both rows: the member list is up to 20 pages.
    let mut lose = None;
    for r in rows.as_array_mut().into_iter().flatten().filter(|r| {
        r["fix"]["setting"] == EVERYONE_TO_MAIN || r["fix"]["setting"] == STANDARD_ACCESS
    }) {
        if lose.is_none() {
            lose = Some(members_without(&st, guild, &l.setup.main_roles).await);
        }
        if let Some(Ok((n, more))) = &lose {
            r["members_lose"] = json!(n);
            r["members_lose_more"] = json!(more);
        }
    }
    Ok(Json(json!({"rows": rows, "setup_done": setup_done})))
}

/// Every channel a log goes to: the log routes, appeals and the ticket log.
pub(crate) fn log_channel_ids(config: &Value) -> Vec<u64> {
    let id = |v: &Value| v.as_str().and_then(|x| x.parse::<u64>().ok());
    serde_json::from_value::<brim_logging::LogConfig>(config["logging"].clone())
        .unwrap_or_default()
        .channels()
        .iter()
        .map(|c| c.get())
        .chain(id(&config["appeals"]["channel"]))
        .chain(id(&config["tickets"]["log_channel"]))
        .collect()
}

/// Pages of 1000 members [`members_without`] reads at most.
const MEMBER_PAGES: usize = 20;

/// How many members (not bots) hold none of `main`, and whether there may be more than counted.
pub(crate) async fn members_without(
    st: &AppState,
    guild: i64,
    main: &[u64],
) -> Result<(u64, bool), ApiError> {
    let mut n = 0;
    let more = member_pages(st, guild, |m| {
        let holds = m["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str()?.parse::<u64>().ok())
            .any(|r| main.contains(&r));
        if m["user"]["bot"] != true && !holds {
            n += 1;
        }
    })
    .await?;
    Ok((n, more))
}

/// Every role id some member holds; `None` when there are more members than [`MEMBER_PAGES`] read.
pub(crate) async fn held_roles(st: &AppState, guild: i64) -> Result<Option<Vec<String>>, ApiError> {
    let mut held = std::collections::BTreeSet::new();
    let more = member_pages(st, guild, |m| {
        let ids = m["roles"].as_array().into_iter().flatten();
        held.extend(ids.filter_map(|r| r.as_str().map(String::from)));
    })
    .await?;
    Ok((!more).then(|| held.into_iter().collect()))
}

/// Calls `each` for every member on up to [`MEMBER_PAGES`] pages; `true` when there are more.
pub(crate) async fn member_pages(
    st: &AppState,
    guild: i64,
    mut each: impl FnMut(&Value),
) -> Result<bool, ApiError> {
    let mut after = "0".to_string();
    for _ in 0..MEMBER_PAGES {
        let page = st
            .discord
            .call(
                Method::GET,
                &format!("/guilds/{guild}/members?limit=1000&after={after}"),
                None,
            )
            .await?;
        let page = page.as_array().cloned().unwrap_or_default();
        page.iter().for_each(&mut each);
        match page.last().and_then(|m| m["user"]["id"].as_str()) {
            Some(id) if page.len() == 1000 => after = id.to_string(),
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// The [`Fix::setting`] that moves every stored number outside its range to the nearest end.
const CLAMP: &str = "clamp_stored";

/// The [`Fix::setting`] that turns on the automod's Suspicious account filter.
const SUSPICION: &str = "enable_suspicion";

/// The [`Fix::setting`] that turns on Remove dangerous roles from members without permits.
const ENFORCE_PERMITS: &str = "enforce_permits";

/// The Overview row while Remove dangerous roles is off (also with no value saved): offered
/// here with the list of who loses roles, never turned on by itself (`MiscCfg::enforce_permits`).
fn enforce_permits_check(config: &Value) -> Option<Check> {
    (config["misc"]["enforce_permits"] != true).then(|| Check {
        id: "enforce_permits_off".into(),
        severity: Severity::Warn,
        title: "Dangerous roles kept without a permit",
        detail: "Members without a permit keep the roles with dangerous permissions they hold now. Brim only takes back such a role when someone gives it.".into(),
        fix: Some(Fix {
            label: "Remove dangerous roles",
            role_id: None,
            channel_id: None,
            setting: Some(ENFORCE_PERMITS),
            move_under_bot: false,
        }),
        steps: vec![],
    })
}

/// The members (not bots) who lose roles once `misc.enforce_permits` is on: no permit, Owner,
/// Extra Owner or Trusted Admin, and a role with dangerous permissions below Brim that is not
/// managed. The same rule as the bot's `antinuke::enforce_permits`. Names only, at most 25.
async fn enforce_permits_preview(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
) -> Res {
    require(&st, &s, guild, crate::access::write_bar("misc").0, false).await?;
    let g = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}"), None)
        .await?;
    let owner = g["owner_id"]
        .as_str()
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    let holders = brim_core::permits::holders(&st.db, guild, owner).await?;
    let (roles, bot_pos) = roles_and_bot_pos(&st, guild).await?;
    // A role lock strips the bits it holds: judge by what the role had before, as the bot does
    // (`lockdown::unlocked_info`), since it is dangerous again once the lock ends.
    let locks: Vec<(sqlx::types::Json<brim_automod::lockdown::Snapshot>,)> =
        sqlx::query_as("select snapshot from lock_snapshots where guild_id = $1")
            .bind(guild)
            .fetch_all(&st.db)
            .await?;
    let saved = |id: &str| {
        locks
            .iter()
            .flat_map(|(l,)| &l.0.role_perms)
            .filter(|(r, _)| r.to_string() == id)
            .fold(0, |a, (_, p)| a | p)
    };
    let dangerous: std::collections::HashMap<&str, &str> = roles
        .iter()
        .filter(|r| {
            !r.managed
                && r.position < bot_pos
                && brim_antinuke::has_dangerous(r.perms() | saved(&r.id))
        })
        .map(|r| (r.id.as_str(), r.name.as_str()))
        .collect();
    let (mut count, mut members) = (0u64, vec![]);
    let more = member_pages(&st, guild, |m| {
        let id = m["user"]["id"].as_str().and_then(|x| x.parse::<i64>().ok());
        if m["user"]["bot"] == true || id.is_none_or(|u| holders.contains(&u)) {
            return;
        }
        let lose: Vec<String> = m["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| dangerous.get(r.as_str()?))
            .map(|n| format!("@{n}"))
            .collect();
        if lose.is_empty() {
            return;
        }
        count += 1;
        if members.len() < 25 {
            let name = [
                &m["nick"],
                &m["user"]["global_name"],
                &m["user"]["username"],
            ]
            .into_iter()
            .find_map(|v| v.as_str())
            .unwrap_or("Unknown member");
            members.push(json!({"name": name, "roles": lose}));
        }
    })
    .await?;
    Ok(Json(
        json!({"count": count, "more": more, "members": members}),
    ))
}

/// The [`Fix::setting`] that gives back one thing the previous owner set (`role_id` names it:
/// `eo:<user>`, `ta:<user>`, `member:<user>:<permit>`, `role:<role>:<permit>`), or with `role_id` "done" closes the card.
const KEEP_STAFF: &str = "keep_previous_staff";

/// "name (id)" for a user, or the id alone when Discord does not answer.
async fn user_label(st: &AppState, id: i64) -> String {
    match st.discord.user(id).await.ok().flatten() {
        Some(u) if u["username"].is_string() => {
            format!("{} ({id})", u["username"].as_str().unwrap_or_default())
        }
        _ => id.to_string(),
    }
}

async fn role_label(st: &AppState, guild: i64, role: &str) -> String {
    match st.discord.role_name(guild, role).await.ok().flatten() {
        Some(n) => format!("@{n} ({role})"),
        None => role.to_string(),
    }
}

fn keep(entry: String) -> Option<Fix> {
    Some(Fix {
        label: "Keep",
        role_id: Some(entry),
        channel_id: None,
        setting: Some(KEEP_STAFF),
        move_under_bot: false,
    })
}

/// The weaker permits from before the last ownership change that still have members, as
/// "name (<@user>, ...)": they stayed on, so the card lists them.
pub(crate) async fn weaker_permits(st: &AppState, guild: i64) -> Result<Vec<String>, ApiError> {
    let held: Vec<(String, Vec<String>)> = sqlx::query_as(
        "select p.name, array_agg(r.user_id::text order by r.user_id) from owner_handovers h
         join permits p on p.guild_id = h.guild_id and p.name = any(h.other_permits)
         join permit_members r on r.permit_id = p.id
         where h.guild_id = $1 group by p.name, p.rank order by p.rank desc, p.name",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let mut on = vec![];
    for (name, ids) in held {
        let labels: Vec<String> = ids.iter().map(|r| format!("<@{r}>")).collect();
        on.push(format!("{name} ({})", labels.join(", ")));
    }
    Ok(on)
}

/// After an ownership change the bot paused the previous owner's EO/TA and strong permit members
/// (older rows: roles): one Overview row per paused person or role, kept one by one.
async fn handover_check(st: &AppState, guild: i64) -> Result<Vec<Check>, ApiError> {
    type Row = (i64, Vec<i64>, Vec<i64>, Value, Vec<String>, Vec<i64>);
    let row: Option<Row> = sqlx::query_as(
        "select old_owner, extra_owners, trusted_admins, paused_roles, other_permits, quarantine_whitelist
         from owner_handovers where guild_id = $1 and kept_at is null",
    )
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let Some((old, eo, ta, roles, other, qw)) = row else {
        return Ok(vec![]);
    };
    // (kind, id, permit): "member" for a paused person, "role" for an entry from before 2026-11-15.
    let roles: Vec<(&str, String, String)> = roles
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let (kind, id) = match r["user"].as_str() {
                Some(u) => ("member", u),
                None => ("role", r["role"].as_str()?),
            };
            Some((kind, id.to_string(), r["permit"].as_str()?.to_string()))
        })
        .collect();
    if eo.is_empty() && ta.is_empty() && qw.is_empty() && roles.is_empty() && other.is_empty() {
        return Ok(vec![]);
    }
    let on = weaker_permits(st, guild).await?;
    let old_label = user_label(st, old).await;
    let mut detail = format!(
        "When this server got a new owner, Brim paused what the previous owner, {old_label}, set. Each row below is one person or role: keep only the ones you trust. Only the server owner can keep them."
    );
    if !on.is_empty() {
        detail.push_str(&format!(
            " These permits from before the change stay on: {}.",
            on.join("; ")
        ));
    }
    let mut rows = vec![Check {
        id: "owner_handover".into(),
        severity: Severity::Warn,
        title: "Ownership changed",
        detail,
        fix: Some(Fix {
            label: "Dismiss, keep nothing more",
            role_id: Some("done".into()),
            channel_id: None,
            setting: Some(KEEP_STAFF),
            move_under_bot: false,
        }),
        steps: vec![],
    }];
    let mark = |id: i64| {
        if id == old {
            ". This is the previous owner."
        } else {
            ""
        }
    };
    for (kind, title, ids) in [
        ("eo", "Paused Extra Owner", &eo),
        ("ta", "Paused Trusted Admin", &ta),
        ("qw", "Paused quarantine whitelist user", &qw),
    ] {
        for &id in ids {
            rows.push(Check {
                id: format!("owner_handover:{kind}:{id}"),
                severity: Severity::Warn,
                title,
                detail: format!("{}{}", user_label(st, id).await, mark(id)),
                fix: keep(format!("{kind}:{id}")),
                steps: vec![],
            });
        }
    }
    for (kind, id, permit) in roles {
        let (title, who) = match kind {
            "member" => ("Paused permit member", format!("<@{id}>")),
            _ => ("Paused permit role", role_label(st, guild, &id).await),
        };
        rows.push(Check {
            id: format!("owner_handover:{kind}:{id}:{permit}"),
            severity: Severity::Warn,
            title,
            detail: format!("{who} in the permit {permit}"),
            fix: keep(format!("{kind}:{id}:{permit}")),
            steps: vec![],
        });
    }
    Ok(rows)
}

/// Gives back one paused entry stored at the ownership change, as a config version; "done"
/// closes the card and leaves the rest removed.
async fn keep_staff(st: &AppState, s: &Sess, guild: i64, c: &Value, entry: Option<&str>) -> Res {
    if !crate::api::is_discord_owner(st, s, guild).await? {
        return Err(ApiError::Forbidden(
            "only the server owner can keep the previous owner's staff",
        ));
    }
    let entry = entry.ok_or_else(|| ApiError::BadRequest("name what to keep".into()))?;
    let mut tx = st.db.begin().await?;
    // The config lock first, as the bot's on_owner takes it: the other order deadlocks.
    let (cur, mut config) = crate::store::locked(&mut tx, guild).await?;
    type Paused = (Vec<i64>, Vec<i64>, Value, Vec<String>, Vec<i64>);
    let row: Option<Paused> = sqlx::query_as(
        "select extra_owners, trusted_admins, paused_roles, other_permits, quarantine_whitelist
         from owner_handovers where guild_id = $1 and new_owner = $2 and kept_at is null",
    )
    .bind(guild)
    .bind(s.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (mut eo, mut ta, roles, other, mut qw) = row.ok_or(ApiError::NotFound)?;
    let mut roles: Vec<Value> = roles.as_array().cloned().unwrap_or_default();
    let done = entry == "done";
    let what = if done {
        None
    } else {
        let (kind, rest) = entry.split_once(':').ok_or(ApiError::NotFound)?;
        let (list, table, label) = match kind {
            "eo" => (Some(&mut eo), "extra_owners", "Extra Owner"),
            "ta" => (Some(&mut ta), "trusted_admins", "Trusted Admin"),
            "qw" => (Some(&mut qw), "", "quarantine whitelist user"),
            "role" | "member" => (None, "", ""),
            _ => return Err(ApiError::NotFound),
        };
        if let Some(list) = list {
            let id: i64 = rest.parse().map_err(|_| ApiError::NotFound)?;
            if !list.contains(&id) {
                return Err(ApiError::NotFound);
            }
            // A bot is refused here as in any save of these lists: only users (owner decision 11-13).
            let w = match kind {
                "qw" => json!({"antinuke": {"quarantine_whitelist": [rest]}}),
                k => json!({"permits": {k: [rest]}}),
            };
            crate::api::users_only(st, &w, &json!({})).await?;
            list.retain(|u| *u != id);
            if table.is_empty() {
                // The quarantine whitelist lives in the config, so this keep is a config change.
                let w = &mut config["antinuke"]["quarantine_whitelist"];
                if !w.is_array() {
                    *w = json!([]);
                }
                if let Some(a) = w
                    .as_array_mut()
                    .filter(|a| !a.contains(&json!(id.to_string())))
                {
                    a.push(json!(id.to_string()));
                }
            } else {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "insert into {table} (guild_id, user_id) values ($1, $2) on conflict do nothing"
                )))
                .bind(guild)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            Some(format!("the {label} <@{id}>"))
        } else if kind == "member" {
            let (user, permit) = rest.split_once(':').ok_or(ApiError::NotFound)?;
            let at = roles
                .iter()
                .position(|r| r["user"] == user && r["permit"] == permit)
                .ok_or(ApiError::NotFound)?;
            roles.remove(at);
            let user: i64 = user.parse().map_err(|_| ApiError::NotFound)?;
            let other: Option<String> = sqlx::query_scalar(
                "select p.name from permit_members m join permits p on p.id = m.permit_id
                 where m.guild_id = $1 and m.user_id = $2",
            )
            .bind(guild)
            .bind(user)
            .fetch_optional(&mut *tx)
            .await?;
            match other {
                Some(o) if o == permit => {}
                Some(o) => {
                    return Err(ApiError::BadRequest(format!(
                        "<@{user}> is in the permit {o} now. A person can hold only one permit."
                    )))
                }
                None => {
                    let n = sqlx::query(
                        "insert into permit_members (permit_id, guild_id, user_id, added_by)
                         select id, $1, $3, $4 from permits where guild_id = $1 and name = $2",
                    )
                    .bind(guild)
                    .bind(permit)
                    .bind(user)
                    .bind(s.user_id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                    if n == 0 {
                        return Err(ApiError::BadRequest(format!(
                            "the permit {permit} was deleted since"
                        )));
                    }
                    // Someone who left since the handover is not listed again.
                    let added = json!({"custom": [{"members": [user.to_string()]}]});
                    crate::api::members_ok(st, guild, &added, &json!({})).await?;
                }
            }
            let now = chrono::Utc::now().timestamp();
            brim_core::jobs::enqueue_once(&mut *tx, guild, "permit_sync", now).await?;
            Some(format!("<@{user}> in the permit {permit}"))
        } else {
            let (role, permit) = rest.split_once(':').ok_or(ApiError::NotFound)?;
            let at = roles
                .iter()
                .position(|r| r["role"] == role && r["permit"] == permit)
                .ok_or(ApiError::NotFound)?;
            roles.remove(at);
            // A role Discord hands out (Onboarding, linked, subscription) cannot get its permit back.
            let added = json!({"custom": [{"roles": [role]}]});
            crate::api::permit_roles_ok(st, guild, &added, &json!({})).await?;
            let role: i64 = role.parse().map_err(|_| ApiError::NotFound)?;
            let n = sqlx::query(
                "insert into permit_roles (permit_id, guild_id, role_id)
                 select id, $1, $3 from permits where guild_id = $1 and name = $2 on conflict do nothing",
            )
            .bind(guild)
            .bind(permit)
            .bind(role)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if n == 0
                && !sqlx::query_scalar::<_, bool>(
                    "select exists(select 1 from permits where guild_id = $1 and name = $2)",
                )
                .bind(guild)
                .bind(permit)
                .fetch_one(&mut *tx)
                .await?
            {
                return Err(ApiError::BadRequest(format!(
                    "the permit {permit} was deleted since"
                )));
            }
            let now = chrono::Utc::now().timestamp();
            brim_core::jobs::enqueue_once(&mut *tx, guild, "permit_sync", now).await?;
            Some(format!("the role <@&{role}> in the permit {permit}"))
        }
    };
    let closed = done
        || (eo.is_empty()
            && ta.is_empty()
            && qw.is_empty()
            && roles.is_empty()
            && other.is_empty());
    sqlx::query(
        "update owner_handovers set extra_owners = $2, trusted_admins = $3, paused_roles = $4,
           quarantine_whitelist = $6, kept_at = case when $5 then now() end where guild_id = $1",
    )
    .bind(guild)
    .bind(&eo)
    .bind(&ta)
    .bind(json!(roles))
    .bind(closed)
    .bind(&qw)
    .execute(&mut *tx)
    .await?;
    let Some(what) = what else {
        tx.commit().await?;
        return Ok(Json(json!({"ok": true})));
    };
    let version = crate::store::commit(&mut tx, guild, cur, &config, s.user_id).await?;
    tx.commit().await?;
    log_by(
        st,
        s,
        guild,
        c,
        Some(version),
        &format!("kept the previous owner's {what}"),
    )
    .await;
    Ok(Json(json!({"ok": true, "version": version})))
}

#[derive(Deserialize)]
struct FixBody {
    role_id: Option<String>,
    channel_id: Option<String>,
    setting: Option<String>,
    #[serde(default)]
    move_under_bot: bool,
    /// The recommended setup: what the page showed (`security_setup::digest`).
    shown: Option<String>,
}

/// Role and overwrite strips take a backup first, so Backups can put the old permissions back.
async fn fix(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<FixBody>,
) -> Res {
    let level = require(&st, &s, guild, Level::Admin, true).await?;
    let (cur, c) = crate::store::current(&st.db, guild).await?;
    if let Some(r) = setup_fix(
        &st,
        &s,
        guild,
        level,
        cur,
        &c,
        b.setting.as_deref(),
        b.shown.as_deref(),
    )
    .await?
    {
        return Ok(r);
    }
    if b.setting.as_deref() == Some(crate::exempt_roles::FIX) {
        return crate::exempt_roles::fix(&st, &s, guild, &c).await;
    }
    if b.setting.as_deref() == Some(crate::staff_role::FIX) {
        return crate::staff_role::make_again(&st, &s, guild, level).await;
    }
    if b.setting.as_deref() == Some(QUARANTINE_CHANNELS) {
        let role = c["antinuke"]["quarantine_role"]
            .as_str()
            .ok_or_else(|| ApiError::BadRequest("no quarantine role".into()))?;
        // A stored role deleted in Discord would fail every write.
        let alive = live(&st, guild, &c)
            .await?
            .image
            .roles
            .iter()
            .any(|r| Some(r.id) == role.parse().ok());
        if !alive {
            return Err(ApiError::BadRequest(
                "The quarantine role was deleted in Discord. Use the quarantine role fix first to set up a new one.".into(),
            ));
        }
        // Only adds the deny; Backups is not needed to undo a stricter quarantine.
        let failed = crate::sections::quarantine_channels(&st, guild, role, &c)
            .await
            .ok_or_else(|| {
                ApiError::BadRequest("Brim could not read the server's channels from Discord. Try again in a minute.".into())
            })?;
        log_by(&st, &s, guild, &c, None, &format!("denied the quarantine role in every channel from the security score ({failed} failed)"))
        .await;
        return Ok(Json(json!({"ok": true, "failed": failed})));
    }
    if b.setting.as_deref() == Some(crate::channel_access::STAFF_ACCESS) {
        // Only adds access for the staff roles: no backup needed.
        let staff: Vec<u64> = brim_core::permits::all_staff_roles(&st.db, guild)
            .await?
            .into_iter()
            .map(|r| r as u64)
            .collect();
        crate::channel_access::staff_access(&st, guild, &c, &staff).await?;
        log_by(
            &st,
            &s,
            guild,
            &c,
            None,
            "let the staff roles into the Logs and Staff categories from the security score",
        )
        .await;
        return Ok(Json(json!({"ok": true})));
    }
    if b.setting.as_deref() == Some(brim_antinuke::security::TICKET_LOG_PLACE) {
        // Only moves the channel; it keeps its overwrites, so no backup is needed.
        let mut moves = vec![];
        crate::sections::ticket_log_to_logs(&st, guild, &c, &mut moves).await?;
        if !crate::channel_access::send_moves(&st, guild, moves).await {
            return Err(ApiError::BadRequest(
                "Discord refused moving the channel. A category holds at most 50 channels: make room, then try again".into(),
            ));
        }
        log_by(
            &st,
            &s,
            guild,
            &c,
            None,
            "moved the ticket log into the Logs category from the security score",
        )
        .await;
        return Ok(Json(json!({"ok": true})));
    }
    if b.setting.as_deref() == Some(brim_antinuke::security::BOT_HOIST) {
        // Adds a role with no permissions: no backup needed.
        crate::sections::show_bot(&st, guild, &c).await?;
        log_by(
            &st,
            &s,
            guild,
            &c,
            None,
            "showed Brim separately in the member list from the security score",
        )
        .await;
        return Ok(Json(json!({"ok": true})));
    }
    if b.setting.as_deref() == Some(brim_antinuke::security::STAFF_HOIST) {
        // Only the member-list grouping changes, no permission: no backup needed.
        let l = live(&st, guild, &c).await?;
        let staff = brim_core::permits::grouped_staff_roles(&st.db, guild).await?;
        let mut failed = 0;
        for r in brim_antinuke::security::staff_to_hoist(&l.image.roles, &staff, l.bot_pos) {
            let at = format!("/guilds/{guild}/roles/{}", r.id);
            if st
                .discord
                .call(Method::PATCH, &at, Some(json!({"hoist": true})))
                .await
                .is_err()
            {
                failed += 1;
            }
        }
        log_by(&st, &s, guild, &c, None, &format!("showed the staff roles separately in the member list from the security score ({failed} failed)"))
        .await;
        return Ok(Json(json!({"ok": true, "failed": failed})));
    }
    if b.setting.as_deref() == Some(STANDARD_ACCESS) {
        // Checked here too: the page may be old. Its confirm named who loses access.
        if let Some(why) = crate::channel_access::blocked(&st, guild, &c).await? {
            return Err(ApiError::BadRequest(why));
        }
        // Only adds overwrites; the backup lets Backups put the old ones back.
        let l = live(&st, guild, &c).await?;
        // A main role deleted in Discord would leave the channels to staff only.
        if !l
            .setup
            .main_roles
            .iter()
            .all(|m| l.image.roles.iter().any(|r| r.id == *m))
        {
            return Err(ApiError::BadRequest(
                "a main role was deleted in Discord: use Create Member role first".into(),
            ));
        }
        backup(&st, guild, &l).await?;
        let mut failed = crate::channel_access::standard_access(&st, guild, &c)
            .await
            .ok_or_else(|| {
                ApiError::BadRequest("Brim could not read the server's channels from Discord. Try again in a minute.".into())
            })?;
        crate::sections::start_limits(&st, guild).await;
        // The same row lists Information and the catch channel while unverified members see
        // them; the confirm named who loses access, so the members check is not repeated.
        // Errors only count: the log line below must follow the changes made already.
        let staff: Vec<u64> = brim_core::permits::all_staff_roles(&st.db, guild)
            .await?
            .into_iter()
            .map(|r| r as u64)
            .collect();
        failed += crate::channel_access::info_access(&st, guild, &c, &staff)
            .await
            .unwrap_or(1);
        failed += crate::channel_access::verify_access(&st, guild, &c)
            .await
            .unwrap_or(1);
        let mut moves = vec![];
        let catch_stays = match crate::channel_access::move_catch(&st, guild, &c, &mut moves).await
        {
            Ok(why) => why,
            Err(_) => Some("moving the catch channel failed".into()),
        };
        let catch_stays = match crate::channel_access::send_moves(&st, guild, moves).await {
            true => catch_stays,
            false => Some("Discord refused moving the catch channel. A category holds at most 50 channels: make room, then try again".into()),
        };
        log_by(&st, &s, guild, &c, None, &format!("gave the open channels, Information, the catch and the verification channel the standard permissions from the security score ({failed} failed)"))
        .await;
        return Ok(Json(
            json!({"ok": true, "failed": failed, "catch_stays": catch_stays}),
        ));
    }
    if b.setting.as_deref() == Some(KEEP_STAFF) {
        return keep_staff(&st, &s, guild, &c, b.role_id.as_deref()).await;
    }
    if b.setting.as_deref() == Some(PRIVATE) {
        return make_private(&st, &s, guild, &c, b.channel_id.as_deref()).await;
    }
    // Strips roles like a role fix, so it takes the backup below.
    let expressions = b.setting.as_deref() == Some(EXPRESSIONS);
    if let Some(setting) = b.setting.filter(|_| !expressions) {
        let (key, value) =
            setting_fix(&setting).ok_or_else(|| ApiError::BadRequest("unknown setting".into()))?;
        st.discord
            .call(
                Method::PATCH,
                &format!("/guilds/{guild}"),
                Some(json!({ key: value })),
            )
            .await?;
        log_by(
            &st,
            &s,
            guild,
            &c,
            None,
            &format!("set `{key}` to `{value}` from the security score"),
        )
        .await;
        return Ok(Json(json!({"ok": true})));
    }
    let target = match b.role_id {
        Some(t) if !expressions => t,
        None if expressions => String::new(),
        _ => return Err(ApiError::BadRequest("nothing to fix".into())),
    };
    let l = live(&st, guild, &c).await?;
    let (g, roles, channels) = (guild as u64, &l.image.roles, &l.image.channels);
    let backup = || backup(&st, guild, &l);
    if b.move_under_bot {
        let moves = target
            .parse()
            .map(|t| move_under_bot(roles, t, l.bot_pos, &l.setup))
            .unwrap_or_default();
        let Some(q) = moves
            .first()
            .and_then(|(id, _)| roles.iter().find(|r| r.id == *id))
        else {
            return Err(ApiError::BadRequest(
                "only the quarantine role can be moved, and only when a role sits between it and the bot".into(),
            ));
        };
        let backup = backup().await?;
        let body: Vec<Value> = moves
            .iter()
            .map(|(id, p)| json!({"id": id.to_string(), "position": p}))
            .collect();
        st.discord
            .call(
                Method::PATCH,
                &format!("/guilds/{guild}/roles"),
                Some(json!(body)),
            )
            .await?;
        log_by(
            &st,
            &s,
            guild,
            &c,
            None,
            &format!(
                "moved role `{}` right below the bot from the security score, {}",
                q.name,
                crate::recovery::backup_named(backup)
            ),
        )
        .await;
        return Ok(Json(json!({"ok": true, "backup": backup})));
    }
    let all = target == "*";
    let flagged = overwrite_targets(channels, roles, g, &l.setup);
    let overwrites: Vec<_> = flagged
        .iter()
        .filter(|(ch, o)| {
            !expressions
                && (all
                    || (b.channel_id.as_deref() == Some(&ch.id.to_string())
                        && o.target.to_string() == target))
        })
        .collect();
    let role_targets: Vec<&RoleImg> = if expressions {
        expression_targets(roles, g, l.bot_pos, &l.setup)
    } else if all {
        strip_targets(
            roles,
            g,
            l.bot_pos,
            (&l.setup.main_roles, &l.setup.staff_roles),
        )
    } else if b.channel_id.is_some() {
        if overwrites.is_empty() {
            return Err(ApiError::NotFound);
        }
        vec![]
    } else {
        let r = roles
            .iter()
            .find(|r| r.id.to_string() == target)
            .ok_or(ApiError::NotFound)?;
        if r.managed {
            return Err(ApiError::BadRequest(
                "managed roles cannot be changed here".into(),
            ));
        }
        vec![r]
    };
    let backup = backup().await?;
    // Keep going past a failed call (e.g. a private channel the bot cannot see), so everything
    // that can be fixed is, and the log lists both.
    let (mut done, mut failed) = (vec![], vec![]);
    for r in &role_targets {
        let res = st
            .discord
            .call(
                Method::PATCH,
                &format!("/guilds/{guild}/roles/{}", r.id),
                Some(json!({"permissions": if expressions {
                    r.permissions & !EXPRESSION_PERMS
                } else {
                    stripped(r.id, g, (&l.setup.main_roles, &l.setup.staff_roles), r.permissions)
                }
                .to_string()})),
            )
            .await;
        let line = format!("role `{}`", r.name);
        match res {
            Ok(_) => done.push(line),
            Err(e) => failed.push(format!("{line} ({e})")),
        }
    }
    // A category fix also goes to its synced children, so they stay synced.
    for (ch, o) in &overwrites {
        let fixed = stripped_overwrite(o, g, &l.setup);
        let who = match (o.target == g, o.is_role) {
            (true, _) => "@everyone".into(),
            (_, true) => format!("<@&{}>", o.target),
            _ => format!("<@{}>", o.target),
        };
        for id in std::iter::once(ch.id).chain(synced_children(channels, ch).iter().map(|x| x.id)) {
            let res = st
                .discord
                .call(
                    Method::PUT,
                    &format!("/channels/{id}/permissions/{}", o.target),
                    Some(overwrite_body(&fixed)),
                )
                .await;
            let line = format!("<#{id}> for {who}");
            match res {
                Ok(_) => done.push(line),
                Err(e) => failed.push(format!("{line} ({e})")),
            }
        }
    }
    let mut text = format!(
        "{} (<@{}>) stripped {} permissions from {}, {}",
        s.username,
        s.user_id,
        if expressions {
            "emoji, sticker, soundboard and event"
        } else {
            "dangerous"
        },
        if done.is_empty() {
            "nothing".into()
        } else {
            done.join(", ")
        },
        crate::recovery::backup_named(backup)
    );
    if !failed.is_empty() {
        text.push_str(&format!("\nFailed: {}", failed.join(", ")));
    }
    log_change(&st, guild, s.user_id, &c, None, text).await;
    if !failed.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "Fixed {}, but could not fix: {}.{}",
            done.len(),
            failed.join(", "),
            backup.map_or(String::new(), |b| format!(
                " Backup #{b} holds the state before."
            ))
        )));
    }
    Ok(Json(json!({"ok": true, "backup": backup})))
}

/// A backup of the live roles and channels, so Backups can put a fix's changes back. None while
/// the server is locked or in panic mode: that state must not become the newest backup.
pub(crate) async fn backup(st: &AppState, guild: i64, l: &Live) -> Result<Option<i64>, ApiError> {
    if crate::recovery::no_backup_now(st, guild).await? {
        return Ok(None);
    }
    Ok(Some(
        sqlx::query_scalar("INSERT INTO backups (guild_id, image) VALUES ($1, $2) RETURNING id")
            .bind(guild)
            .bind(sqlx::types::Json(crate::recovery::live_image(
                guild,
                &l.roles,
                &l.channels,
            )))
            .fetch_one(&st.db)
            .await?,
    ))
}

/// Saves a fix's config as a version (refused when it changed since `cur`) and logs it.
async fn save_fix(
    st: &AppState,
    s: &Sess,
    guild: i64,
    cur: i32,
    config: &Value,
    what: &str,
) -> Result<i32, ApiError> {
    let mut tx = st.db.begin().await?;
    let version = crate::store::commit(&mut tx, guild, cur, config, s.user_id).await?;
    tx.commit().await?;
    log_by(
        st,
        s,
        guild,
        config,
        Some(version),
        &format!("{what} from the security score"),
    )
    .await;
    Ok(version)
}

/// The fixes that do what Auto Setup does, or change the config; `None` for any other setting.
#[allow(clippy::too_many_arguments)]
async fn setup_fix(
    st: &AppState,
    s: &Sess,
    guild: i64,
    level: Level,
    cur: i32,
    c: &Value,
    setting: Option<&str>,
    shown: Option<&str>,
) -> Result<Option<Json<Value>>, ApiError> {
    let Some(setting) = setting else {
        return Ok(None);
    };
    let mut config = c.clone();
    let done = |v: Value| Ok(Some(Json(v)));
    match setting {
        QUARANTINE_ROLE => {
            // A picked or taken-over role loses its permissions: back it up first.
            let backup = backup(st, guild, &live(st, guild, &config).await?).await?;
            let role = crate::sections::quarantine_role(st, guild, &config).await?;
            config["antinuke"]["quarantine_role"] = json!(role);
            let version =
                save_fix(st, s, guild, cur, &config, "set up the quarantine role").await?;
            let failed = crate::sections::quarantine_channels(st, guild, &role, &config).await;
            done(json!({"ok": true, "version": version, "failed": failed, "backup": backup}))
        }
        CATCH_CHANNEL => {
            // Makes a new channel only: nothing to back up.
            let made = crate::sections::make_catch(st, guild, &mut config).await?;
            let version =
                save_fix(st, s, guild, cur, &config, "made the catch channel again").await?;
            if let Some(ch) = made {
                crate::sections::pin_catch_notice(st, &ch).await?;
            }
            done(json!({"ok": true, "version": version}))
        }
        MEMBER_ROLE => {
            // Auto Setup's level: if Auto Setup may do it, a Trusted Admin may (owner decision 11-15).
            let before = config.clone();
            let what = match crate::sections::member_role(st, guild, &mut config).await? {
                Some(_) => "set the Member role as the main role",
                None if config == before => return done(json!({"ok": true})),
                None => "set the verification roles to the main roles",
            };
            let version = save_fix(st, s, guild, cur, &config, what).await?;
            done(json!({"ok": true, "version": version}))
        }
        MOD_LOG | LOG_FALLBACK => {
            let path: &[&str] = match setting {
                MOD_LOG => &["logging", "routes", "mod_command"],
                _ => &["logging", "fallback"],
            };
            let made = crate::sections::log_channel(st, guild, &mut config, path).await?;
            let Some(name) = made.first() else {
                return done(json!({"ok": true}));
            };
            let what = format!("created the log channel #{name}");
            let version = save_fix(st, s, guild, cur, &config, &what).await?;
            done(json!({"ok": true, "version": version}))
        }
        EVERYONE_TO_MAIN => {
            // A lock's snapshot holds the roles' permissions and puts them back on unlock.
            let locked: bool = sqlx::query_scalar(
                "select exists(select 1 from lock_snapshots where guild_id = $1)",
            )
            .bind(guild)
            .fetch_one(&st.db)
            .await?;
            if locked {
                return Err(ApiError::BadRequest(
                    "a lockdown is active: end it first".into(),
                ));
            }
            let l = live(st, guild, &config).await?;
            if l.setup.main_roles.is_empty() {
                return Err(ApiError::BadRequest(
                    "no main roles are set: set them first, for example with Create Member role"
                        .into(),
                ));
            }
            let g = guild as u64;
            let every = l
                .image
                .roles
                .iter()
                .find(|r| r.id == g)
                .map_or(0, |r| r.permissions);
            // Dangerous bits are stripped, never handed to every member through the main roles.
            if stripped(g, g, (&[], &[]), every) != every {
                return Err(ApiError::BadRequest(
                    "@everyone holds dangerous permissions: strip them first".into(),
                ));
            }
            // A main role deleted in Discord stays in the saved list: skip it, move to the rest.
            let mains: Vec<&RoleImg> = l
                .image
                .roles
                .iter()
                .filter(|r| l.setup.main_roles.contains(&r.id))
                .collect();
            let deleted = l.setup.main_roles.len() - mains.len();
            if mains.is_empty() {
                return Err(ApiError::BadRequest(
                    "none of the main roles exist any more: use Create Member role, or pick them under Lockdown > Main roles"
                        .into(),
                ));
            }
            // Every role must be changeable before the first change, so none is half done.
            if let Some(r) = mains.iter().find(|r| r.managed || r.position >= l.bot_pos) {
                return Err(ApiError::BadRequest(format!(
                    "the main role {} is not below the bot's top role, so the bot cannot change it",
                    r.name
                )));
            }
            let backup = backup(st, guild, &l).await?;
            let mut calls: Vec<(String, String, u64)> = mains
                .iter()
                .map(|r| {
                    (
                        r.id.to_string(),
                        format!("`{}`", r.name),
                        r.permissions | every,
                    )
                })
                .collect();
            calls.push((guild.to_string(), "@everyone".into(), 0));
            let (mut changed, mut failed) = (vec![], None);
            for (id, name, perms) in calls {
                let r = st
                    .discord
                    .call(
                        Method::PATCH,
                        &format!("/guilds/{guild}/roles/{id}"),
                        Some(json!({"permissions": perms.to_string()})),
                    )
                    .await;
                match r {
                    Ok(_) => changed.push(name),
                    Err(e) => {
                        failed = Some(format!("{name} ({e})"));
                        break;
                    }
                }
            }
            let mut text = format!(
                "{} (<@{}>) moved the permissions of @everyone to the main roles from the security score, {}. Changed: {}",
                s.username,
                s.user_id,
                crate::recovery::backup_named(backup),
                if changed.is_empty() { "nothing".into() } else { changed.join(", ") }
            );
            if deleted > 0 {
                text.push_str(&format!(
                    "\nSkipped {deleted} saved main role(s) deleted in Discord"
                ));
            }
            if let Some(f) = &failed {
                text.push_str(&format!("\nFailed: {f}"));
            }
            log_change(st, guild, s.user_id, &config, None, text).await;
            if let Some(f) = failed {
                return Err(ApiError::BadRequest(format!(
                    "Changed {}, but could not change {f}.{}",
                    if changed.is_empty() {
                        "nothing".into()
                    } else {
                        changed.join(", ")
                    },
                    backup.map_or(String::new(), |b| format!(
                        " Backup #{b} holds the state before."
                    ))
                )));
            }
            done(json!({"ok": true, "backup": backup}))
        }
        SUSPICION => {
            if level < crate::access::write_bar("automod").0 {
                return Err(ApiError::Forbidden("you cannot change the automod"));
            }
            let Some(a) = config.get_mut("automod").and_then(Value::as_object_mut) else {
                return Err(ApiError::BadRequest("the automod is not set up yet".into()));
            };
            let f = a.entry("filters").or_insert(json!({}));
            if !f.is_object() {
                *f = json!({});
            }
            let on = &mut f["suspicion"];
            if !on.is_object() {
                *on = json!({});
            }
            on["enabled"] = json!(true);
            crate::api::validate("automod", &config["automod"], &c["automod"])?;
            let version = save_fix(
                st,
                s,
                guild,
                cur,
                &config,
                "turned on the Suspicious account filter",
            )
            .await?;
            done(json!({"ok": true, "version": version}))
        }
        ENFORCE_PERMITS => {
            if level < crate::access::write_bar("misc").0 {
                return Err(ApiError::Forbidden("you cannot change Miscellaneous"));
            }
            if config.get("misc").is_some_and(|m| !m.is_object()) {
                return Err(ApiError::BadRequest(
                    "the Miscellaneous settings could not be read".into(),
                ));
            }
            config["misc"]["enforce_permits"] = json!(true);
            crate::api::validate("misc", &config["misc"], &c["misc"])?;
            let version = save_fix(
                st,
                s,
                guild,
                cur,
                &config,
                "turned on Remove dangerous roles from members without permits",
            )
            .await?;
            done(json!({"ok": true, "version": version}))
        }
        crate::security_setup::BACKUPS_FIX => {
            if level < crate::access::write_bar("backups").0 {
                return Err(ApiError::Forbidden("you cannot change the backups"));
            }
            config["backups"]["enabled"] = json!(true);
            crate::api::validate("backups", &config["backups"], &c["backups"])?;
            let version = save_fix(st, s, guild, cur, &config, "turned on backups").await?;
            done(json!({"ok": true, "version": version}))
        }
        crate::security_setup::FIX => {
            let keys = crate::security_setup::apply(&mut config, level, shown)?;
            if keys.is_empty() {
                return done(json!({"ok": true}));
            }
            for k in crate::access::SECTIONS {
                if config[*k] != c[*k] {
                    crate::api::validate(k, &config[*k], &c[*k])?;
                }
            }
            if let Some(e) = brim_automod::join::web_mode_error(&config) {
                return Err(ApiError::BadRequest(e.into()));
            }
            let what = format!(
                "used the recommended security setup ({} settings)",
                keys.len()
            );
            let version = save_fix(st, s, guild, cur, &config, &what).await?;
            done(json!({"ok": true, "version": version, "changed": keys}))
        }
        CLAMP => {
            let mut moved = vec![];
            for k in crate::access::SECTIONS {
                let Some(v) = config.get_mut(*k) else {
                    continue;
                };
                let m = brim_core::limits::clamp(k, v, &Value::Null);
                if m.is_empty() {
                    continue;
                }
                if level < crate::access::write_bar(k).0 {
                    return Err(ApiError::Forbidden(
                        "only owners can change some of these settings",
                    ));
                }
                crate::api::validate(k, v, &c[*k])?;
                moved.extend(m);
            }
            if moved.is_empty() {
                return done(json!({"ok": true}));
            }
            let what = format!("moved into range: {}", moved.join(", "));
            let version = save_fix(st, s, guild, cur, &config, &what).await?;
            done(json!({"ok": true, "version": version, "clamped": moved}))
        }
        _ => {
            let Some((name, section, _)) = MODULES.iter().find(|m| m.2 == setting) else {
                return Ok(None);
            };
            if level < crate::access::write_bar(section).0 {
                return Err(ApiError::Forbidden("only owners can turn this on"));
            }
            config[*section]["enabled"] = json!(true);
            // Its module goes on too, or the fix would change nothing (same bar: Trusted Admin).
            if let Some(m) = brim_core::sections::module_of(section) {
                if config["modules"][m] == false {
                    if level < crate::access::write_bar("modules").0 {
                        return Err(ApiError::Forbidden(
                            "only owners and trusted admins can turn this on",
                        ));
                    }
                    config["modules"][m] = json!(true);
                }
            }
            crate::api::validate(section, &config[*section], &c[*section])?;
            if let Some(e) = brim_automod::join::web_mode_error(&config) {
                return Err(ApiError::BadRequest(e.into()));
            }
            let what = format!("turned on {name}");
            let version = save_fix(st, s, guild, cur, &config, &what).await?;
            done(json!({"ok": true, "version": version}))
        }
    }
}

/// The body of Discord's edit-channel-permissions call.
pub(crate) fn overwrite_body(o: &brim_antinuke::OverwriteImg) -> Value {
    json!({"type": if o.is_role { 0 } else { 1 }, "allow": o.allow.to_string(), "deny": o.deny.to_string()})
}

/// Hides a public log channel (@everyone and the main roles lose View Channel, the bot keeps
/// access) and locks it: only Brim has Manage Messages and Manage Channels there.
async fn make_private(
    st: &AppState,
    s: &Sess,
    guild: i64,
    config: &Value,
    channel: Option<&str>,
) -> Res {
    let l = live(st, guild, config).await?;
    let ch = l
        .image
        .channels
        .iter()
        .find(|x| {
            Some(x.id.to_string().as_str()) == channel && l.setup.log_channels.contains(&x.id)
        })
        .ok_or(ApiError::NotFound)?;
    // A backup first, so Backups can put the old overwrites back.
    let backup = backup(st, guild, &l).await?;
    let bot = st.discord.client_id.parse().unwrap_or_default();
    let ows = log_overwrites(ch, guild as u64, bot, &l.setup.main_roles);
    for o in ows {
        st.discord
            .call(
                Method::PUT,
                &format!("/channels/{}/permissions/{}", ch.id, o.target),
                Some(overwrite_body(&o)),
            )
            .await?;
    }
    log_by(st, s, guild, config, None, &format!("made the log channel <#{}> private and locked against deletes from the security score, {}", ch.id, crate::recovery::backup_named(backup)))
    .await;
    Ok(Json(json!({"ok": true, "backup": backup})))
}

#[derive(Deserialize)]
struct ActionsQ {
    actor: Option<i64>,
    hours: Option<i32>,
}

async fn actions(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(q): Query<ActionsQ>,
) -> Res {
    require(&st, &s, guild, Level::Editor, false).await?;
    let rows: Vec<ActionRow> = sqlx::query_as(
        "select id, actor_id, action, target_id, inverse, reverted, created_at from action_log
         where guild_id = $1 and ($2::bigint is null or actor_id = $2)
           and ($3::int is null or created_at > now() - make_interval(hours => $3))
         order by created_at desc, id desc limit 200",
    )
    .bind(guild)
    .bind(q.actor)
    .bind(q.hours)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows.iter().map(|(id, actor, action, target, inverse, reverted, at)| json!({
        "id": id, "actor_id": actor.to_string(), "action": action, "target_id": target.map(|t| t.to_string()),
        "inverse": inverse, "reverted": reverted, "at": at})).collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct RevertBody {
    ids: Vec<i64>,
    /// Also undo what an Owner, Extra Owner or Trusted Admin did; only owners may.
    #[serde(default)]
    owners: bool,
}

/// Ok(false): a server name or icon revert skipped, since the server no longer has the value the
/// entry set (renamed again since).
async fn run_inverse(st: &AppState, guild: i64, inv: &Inverse) -> anyhow::Result<bool> {
    let live = match inv {
        Inverse::GuildName { from: Some(_), .. } | Inverse::GuildIcon { from: Some(_), .. } => {
            st.discord
                .call(Method::GET, &format!("/guilds/{guild}"), None)
                .await?
        }
        _ => Value::Null,
    };
    let still = match inv {
        Inverse::GuildName { from, .. } => {
            brim_antinuke::still_set(from, &live["name"].as_str().unwrap_or_default().into())
        }
        Inverse::GuildIcon { from, .. } => {
            brim_antinuke::still_set(from, &live["icon"].as_str().map(String::from))
        }
        _ => true,
    };
    if !still {
        return Ok(false);
    }
    let (m, path, body) = match inv {
        // The old icon is downloaded first, so it cannot be a plain request.
        Inverse::GuildIcon { hash, .. } => {
            let icon = match hash {
                Some(h) => Some(
                    brim_automod::discord_api::stored_icon(
                        &st.db,
                        &st.discord.http,
                        guild as u64,
                        h,
                    )
                    .await?,
                ),
                None => None,
            };
            (
                Method::PATCH,
                format!("/guilds/{guild}"),
                Some(json!({ "icon": icon })),
            )
        }
        // Discord refuses a bot's delete of an app's integration; kicking its bot removes it.
        Inverse::DeleteIntegration { id } => {
            let list = st
                .discord
                .call(Method::GET, &format!("/guilds/{guild}/integrations"), None)
                .await?;
            let id_of = |v: &Value| v.as_str().and_then(|s| s.parse::<u64>().ok());
            let list =
                list.as_array().into_iter().flatten().filter_map(|i| {
                    Some((id_of(&i["id"])?, id_of(&i["application"]["bot"]["id"])))
                });
            match brim_antinuke::app_removal(*id, list) {
                brim_antinuke::AppRemoval::Gone => return Ok(true),
                // Never Brim itself (its bot id is the app's client id).
                brim_antinuke::AppRemoval::KickBot(bot)
                    if bot.to_string() == st.discord.client_id =>
                {
                    return Ok(true)
                }
                brim_antinuke::AppRemoval::KickBot(bot) => {
                    (Method::DELETE, format!("/guilds/{guild}/members/{bot}"), None)
                }
                brim_antinuke::AppRemoval::Manual => anyhow::bail!(
                    "this app has no bot user, so only a person can remove it in Server Settings > Integrations"
                ),
            }
        }
        // One call per role: the member keeps the roles they got since.
        // Never to a member quarantined now, and never a role with dangerous permissions or a
        // permit (as the bot's revert): those are left to staff.
        Inverse::AddRoles { user, roles } => {
            let wide: Vec<i64> = roles.iter().map(|r| *r as i64).collect();
            let skip: Vec<i64> = sqlx::query_scalar(
                "select r from unnest($2::bigint[]) r where exists (select 1 from quarantines where guild_id = $1 and user_id = $3)
                   or exists (select 1 from permit_roles p where p.guild_id = $1 and p.role_id = r)
                   or exists (select 1 from level_roles l where l.guild_id = $1 and l.role_id = r)",
            )
            .bind(guild)
            .bind(&wide)
            .bind(*user as i64)
            .fetch_all(&st.db)
            .await?;
            let live = roles_of(st, guild)
                .await
                .map_err(|e| anyhow::anyhow!("reading the server roles failed: {e:?}"))?;
            // A role lock strips the bits it holds: judge by what the role had before (as the bot).
            let locks: Vec<(sqlx::types::Json<brim_automod::lockdown::Snapshot>,)> =
                sqlx::query_as("select snapshot from lock_snapshots where guild_id = $1")
                    .bind(guild)
                    .fetch_all(&st.db)
                    .await?;
            let risky = |r: u64| {
                live.iter().any(|x| {
                    x.id == r.to_string()
                        && brim_antinuke::has_dangerous(x.permissions.parse().unwrap_or(u64::MAX))
                }) || locks.iter().any(|(l,)| {
                    l.0.role_perms
                        .iter()
                        .any(|(id, p)| *id == r && brim_antinuke::has_dangerous(*p))
                })
            };
            let mut last = None;
            for r in roles
                .iter()
                .filter(|r| !skip.contains(&(**r as i64)) && !risky(**r))
            {
                let path = format!("/guilds/{guild}/members/{user}/roles/{r}");
                // One deleted role must not keep the others from coming back.
                if let Err(e) = st.discord.call(Method::PUT, &path, None).await {
                    last = Some(e);
                }
            }
            if let Some(e) = last {
                return Err(e);
            }
            return Ok(true);
        }
        _ => inverse_request(guild, inv),
    };
    // An appeal's punishment comes back as it was: a temp ban with its unban, a timeout
    // over 28 days with its renewal. One that would have ended by now is not redone.
    let now = chrono::Utc::now().timestamp();
    let follow = match *inv {
        Inverse::Ban { until: Some(t), .. } | Inverse::Timeout { until: t, .. } if t <= now => {
            return Ok(true)
        }
        Inverse::Ban {
            user,
            until: Some(t),
        } => Some(("unban", json!({"user": user}), t)),
        Inverse::Timeout { user, until } if until > now + MAX_TIMEOUT_S => Some((
            "timeout_renew",
            json!({"user": user, "until_secs": until}),
            now + MAX_TIMEOUT_S - 86_400,
        )),
        _ => None,
    };
    let job = match &follow {
        Some((kind, p, at)) => {
            Some(brim_core::jobs::enqueue(&st.db, guild, kind, p.clone(), *at).await?)
        }
        None => None,
    };
    if let Err(e) = st.discord.call(m, &path, body).await {
        if let Some(id) = job {
            sqlx::query("delete from scheduled_jobs where id = $1")
                .bind(id)
                .execute(&st.db)
                .await?;
        }
        return Err(e);
    }
    if let (Some((kind, p, _)), Some(id)) = (follow, job) {
        let user = p["user"].as_u64().unwrap_or_default();
        brim_core::jobs::cancel_user(&st.db, guild, &[kind], user, Some(id)).await?;
    }
    Ok(true)
}

/// Discord's longest timeout; a longer one is renewed by the bot's `timeout_renew` job.
const MAX_TIMEOUT_S: i64 = 28 * 86_400;

/// The Discord call that undoes `inv`.
fn inverse_request(guild: i64, inv: &Inverse) -> (Method, String, Option<Value>) {
    match inv {
        Inverse::Unban { user } => (Method::DELETE, format!("/guilds/{guild}/bans/{user}"), None),
        Inverse::Ban { user, .. } => (Method::PUT, format!("/guilds/{guild}/bans/{user}"), None),
        Inverse::Timeout { user, until } => {
            let until = (*until).min(chrono::Utc::now().timestamp() + MAX_TIMEOUT_S);
            (
                Method::PATCH,
                format!("/guilds/{guild}/members/{user}"),
                Some(
                    json!({"communication_disabled_until": chrono::DateTime::from_timestamp(until, 0).map(|t| t.to_rfc3339())}),
                ),
            )
        }
        Inverse::DeleteChannel { id } => (Method::DELETE, format!("/channels/{id}"), None),
        Inverse::DeleteRole { id } => (Method::DELETE, format!("/guilds/{guild}/roles/{id}"), None),
        Inverse::DeleteWebhook { id } => (Method::DELETE, format!("/webhooks/{id}"), None),
        Inverse::RestoreRoles { user, roles } => (
            Method::PATCH,
            format!("/guilds/{guild}/members/{user}"),
            Some(json!({"roles": roles.iter().map(u64::to_string).collect::<Vec<_>>()})),
        ),
        Inverse::RolePerms { id, perms } => (
            Method::PATCH,
            format!("/guilds/{guild}/roles/{id}"),
            Some(json!({"permissions": perms.to_string()})),
        ),
        // Not reached from run_inverse, which removes the app's bot instead (Discord refuses this).
        Inverse::DeleteIntegration { id } => (
            Method::DELETE,
            format!("/guilds/{guild}/integrations/{id}"),
            None,
        ),
        Inverse::DeleteEmoji { id } => {
            (Method::DELETE, format!("/guilds/{guild}/emojis/{id}"), None)
        }
        Inverse::DeleteSticker { id } => (
            Method::DELETE,
            format!("/guilds/{guild}/stickers/{id}"),
            None,
        ),
        Inverse::DeleteScheduledEvent { id } => (
            Method::DELETE,
            format!("/guilds/{guild}/scheduled-events/{id}"),
            None,
        ),
        Inverse::GuildName { name, .. } => (
            Method::PATCH,
            format!("/guilds/{guild}"),
            Some(json!({ "name": name })),
        ),
        Inverse::Unquarantine { .. }
        | Inverse::Reinvite { .. }
        | Inverse::GuildIcon { .. }
        | Inverse::AddRoles { .. } => {
            unreachable!("handled by the caller")
        }
    }
}

async fn revert(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<RevertBody>,
) -> Res {
    let level = require(&st, &s, guild, Level::Admin, true).await?;
    // Newest first so later changes are undone before the ones they depend on.
    let rows: Vec<(i64, i64, Value, Option<i32>)> = sqlx::query_as(
        "select id, actor_id, inverse, case_no from action_log where guild_id = $1 and id = any($2) and not reverted and inverse is not null
         order by created_at desc, id desc")
        .bind(guild).bind(&b.ids).fetch_all(&st.db).await?;
    let mut immune: Vec<i64> = sqlx::query_scalar(
        "select user_id from extra_owners where guild_id = $1 union select user_id from trusted_admins where guild_id = $1",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    immune.extend(crate::api::bot_owner(&st, guild, false).await?);
    // Brim's own rows (automatic bans, cleared bot roles) count as an owner's.
    immune.extend(st.discord.bot_id().await?.parse::<i64>().ok());
    let (mut done, mut failed, mut skipped, mut reinvite, mut protected) =
        (0, vec![], vec![], vec![], vec![]);
    for (id, actor, raw, case_no) in rows {
        // An owner's or TA's actions are undone only when an owner confirms it.
        if immune.contains(&actor) && !(b.owners && level >= Level::Owner) {
            protected.push(id);
            continue;
        }
        let inv: Inverse = match serde_json::from_value(raw) {
            Ok(i) => i,
            Err(e) => {
                failed.push(json!({"id": id, "error": e.to_string()}));
                continue;
            }
        };
        // An old row must not lift a ban made after it: left for staff, as a rename is. A later
        // row covers anti-nuke's rows, which have no case; the case knows when its ban ran out.
        if let Inverse::Unban { user } = &inv {
            let later: bool = sqlx::query_scalar(
                "select exists(select 1 from action_log where guild_id = $1 and target_id = $2 and id > $3
                 and lower(action) in ('ban', 'softban', 'unban'))",
            )
            .bind(guild)
            .bind(*user as i64)
            .bind(id)
            .fetch_one(&st.db)
            .await?;
            let ended = match case_no {
                Some(n) => brim_core::cases::not_current(&st.db, guild, n)
                    .await?
                    .is_some(),
                None => false,
            };
            if later || ended {
                skipped.push(id);
                continue;
            }
        }
        match &inv {
            // Only the bot writes RolePerms (a nuking bot's cleared role): putting it back frees it.
            Inverse::Unquarantine { .. } | Inverse::RolePerms { .. } => {
                skipped.push(id);
                continue;
            }
            Inverse::Reinvite { user } => reinvite.push(user.to_string()),
            _ => match run_inverse(&st, guild, &inv).await {
                Ok(true) => {}
                // Renamed again since: left for staff, not marked reverted.
                Ok(false) => {
                    skipped.push(id);
                    continue;
                }
                Err(e) => {
                    failed.push(json!({"id": id, "error": e.to_string()}));
                    continue;
                }
            },
        }
        // The revert unbans as the bot, so the bot cannot tell staff lifted a Raider List ban.
        if let Inverse::Unban { user } = inv {
            brim_core::raiders::allow(&st.db, guild, user as i64, s.user_id).await?;
            brim_core::jobs::cancel_user(&st.db, guild, &["unban"], user, None).await?;
        }
        sqlx::query("update action_log set reverted = true where id = $1")
            .bind(id)
            .execute(&st.db)
            .await?;
        done += 1;
    }
    {
        let (_, c) = crate::store::current(&st.db, guild).await?;
        log_by(
            &st,
            &s,
            guild,
            &c,
            None,
            &format!("reverted {done} action(s)"),
        )
        .await;
    }
    Ok(Json(
        json!({"done": done, "failed": failed, "skipped": skipped, "reinvite": reinvite,
            "protected": protected, "can_include": level >= Level::Owner}),
    ))
}

pub(crate) fn hash(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The rescue authenticator secret XOR a pad made from the key, which only the holder has; the
/// row keeps only the key's hash, so a database read alone cannot make codes (PRIVACY.md).
fn rescue_pad(key: &str, i: usize) -> u8 {
    Sha256::digest(format!("rescue-totp:{}:{key}", i / 32).as_bytes())[i % 32]
}

fn seal(key: &str, secret: &str) -> String {
    let hex: String = secret
        .bytes()
        .enumerate()
        .map(|(i, b)| format!("{:02x}", b ^ rescue_pad(key, i)))
        .collect();
    format!("enc:{hex}")
}

/// A row made before the secret was sealed holds it in plain text.
pub(crate) fn unseal(key: &str, stored: &str) -> Result<String, ApiError> {
    let Some(hex) = stored.strip_prefix("enc:") else {
        return Ok(stored.to_string());
    };
    let bad = || anyhow::anyhow!("the stored rescue authenticator secret is damaged");
    let b = (0..hex.len() / 2)
        .map(|i| {
            u8::from_str_radix(hex.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16)
                .map(|b| b ^ rescue_pad(key, i))
                .map_err(|_| bad())
        })
        .collect::<Result<Vec<u8>, _>>()?;
    Ok(String::from_utf8(b).map_err(|_| bad())?)
}

async fn rescue_status(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Owner, false).await?;
    // The key makes its holder an Extra Owner, so an Extra Owner must not mint one for later.
    if !crate::api::is_discord_owner(&st, &s, guild).await? {
        return Err(ApiError::Forbidden(
            "only the server owner can manage the rescue key",
        ));
    }
    let key: Option<(bool, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {}, totp_secret is null from rescue_key k where guild_id = $1",
        brim_core::permits::rescue_expired_sql()
    )))
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let opens = rescue_opens(&st.db, guild).await?;
    // The paused server owner account, while this user holds the Owner level by a claim.
    let paused: Option<i64> = sqlx::query_scalar(
        "select discord_owner from rescue_owners where server_id = $1 and user_id = $2",
    )
    .bind(guild)
    .bind(s.user_id)
    .fetch_optional(&st.db)
    .await?;
    Ok(Json(json!({
        "paused_owner": paused.map(|u| u.to_string()),
        "has_key": key == Some((false, false)),
        "expired": key.is_some_and(|k| k.0),
        // A key from before the authenticator: refused since security review (owner decision 2026-11-18).
        "no_code": key.is_some_and(|k| !k.0 && k.1),
        "opens_at": opens.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()),
    })))
}

/// While the key made before the last ownership change still works (its 7 days): when a new key
/// can be made. Nobody, not even the new owner, replaces it before then (owner decision 11-17).
async fn rescue_opens(
    db: impl sqlx::PgExecutor<'_>,
    guild: i64,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    Ok(sqlx::query_scalar(
        "select h.at + make_interval(days => $2) from owner_handovers h join rescue_key k using (guild_id)
         where h.guild_id = $1 and h.at > k.created_at and h.at > now() - make_interval(days => $2)
           and k.totp_secret is not null",
    )
    .bind(guild)
    .bind(brim_core::permits::RESCUE_GRACE_DAYS as i32)
    .fetch_optional(db)
    .await?)
}

/// Whether the physical owner should be asked to make a new rescue key: the old one expired, or it
/// is from before the authenticator code and no longer works.
pub(crate) async fn rescue_expired(db: &sqlx::PgPool, guild: i64) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select exists(select 1 from rescue_key k where guild_id = $1 and ({} or totp_secret is null))",
        brim_core::permits::rescue_expired_sql()
    )))
    .bind(guild)
    .fetch_one(db)
    .await?)
}

async fn rescue_regenerate(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Owner, true).await?;
    // Under the config lock `on_owner` takes: checked after a handover that commits first, and a
    // handover after it sees this key as the previous owner's.
    let mut tx = st.db.begin().await?;
    crate::store::locked(&mut tx, guild).await?;
    // The key makes its holder an Extra Owner, so an Extra Owner must not mint one for later.
    if !crate::api::is_discord_owner(&st, &s, guild).await? {
        return Err(ApiError::Forbidden(
            "only the server owner can manage the rescue key",
        ));
    }
    if let Some(at) = rescue_opens(&mut *tx, guild).await? {
        return Err(ApiError::BadRequest(format!(
            "the server changed owner less than 7 days ago, so the rescue key made before that keeps working until {} and no new key can be made before then",
            at.format("%Y-%m-%d %H:%M UTC")
        )));
    }
    let key = auth::token();
    let secret = crate::otp::new_secret();
    sqlx::query(
        "insert into rescue_key (guild_id, key_hash, totp_secret) values ($1, $2, $3)
                 on conflict (guild_id) do update set key_hash = $2, totp_secret = $3, created_at = now(), last_step = null",
    )
    .bind(guild)
    .bind(hash(&key))
    .bind(seal(&key, &secret))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let uri = crate::otp::uri(&secret, "Brim Rescue", &format!("server {guild}"))?;
    Ok(Json(json!({"key": key, "uri": uri})))
}

/// The claimer hands Brim's Owner level back to the server owner account (owner decision 2026-11-20).
/// Its rescue key goes too: the key is the claimer's, and the owner makes a new one.
async fn rescue_give_back(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Owner, true).await?;
    let mut tx = st.db.begin().await?;
    // The config lock before the row, as the claim and the bot's on_owner.
    crate::store::locked(&mut tx, guild).await?;
    let back: Option<i64> = sqlx::query_scalar(
        "delete from rescue_owners where server_id = $1 and user_id = $2 returning discord_owner",
    )
    .bind(guild)
    .bind(s.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(owner) = back else {
        return Err(ApiError::BadRequest(
            "you do not hold Brim's Owner level here by a rescue claim".into(),
        ));
    };
    sqlx::query("delete from rescue_key where guild_id = $1")
        .bind(guild)
        .execute(&mut *tx)
        .await?;
    owner_check_now(&mut tx, guild).await?;
    tx.commit().await?;
    let (_, config) = crate::store::current(&st.db, guild).await?;
    let text = format!(
        "{} (<@{}>) gave Brim's Owner level back to the server owner account <@{owner}>",
        s.username, s.user_id
    );
    log_change(&st, guild, s.user_id, &config, None, text).await;
    Ok(Json(json!({"ok": true})))
}

/// The rescue key, plus a current 6-digit code from its authenticator.
#[derive(Deserialize)]
struct Claim {
    guild_id: String,
    key: Option<String>,
    code: Option<String>,
}

/// Claim tries per user in 10 minutes; Secure my account presses are counted apart.
const MAX_CLAIMS: i64 = 5;
/// Wrong authenticator codes one rescue key takes in an hour (owner decision 2026-11-18).
pub(crate) const MAX_CODE_FAILS: i32 = 10;

/// At most `MAX_CLAIMS` tries of `kind` per user in 10 minutes, counted in the database so a
/// dashboard restart does not reset them.
pub(crate) async fn within_limit(
    db: &sqlx::PgPool,
    kind: &str,
    user: i64,
) -> Result<bool, ApiError> {
    let mut tx = db.begin().await?;
    let n = crate::auth::tries(&mut tx, (kind, user), 10, true).await?;
    tx.commit().await?;
    crate::auth::prune_tries(db).await;
    Ok(n <= MAX_CLAIMS)
}

async fn claim_key(
    db: &sqlx::PgPool,
    (guild, discord_owner): (i64, i64),
    user: i64,
    (key, code): (Option<&str>, Option<&str>),
) -> Result<(), ApiError> {
    let mut tx = db.begin().await?;
    // The config lock before any row lock, the order the bot's on_owner uses.
    crate::store::locked(&mut tx, guild).await?;
    // Both are needed: the key alone (a leaked note) or the phone alone is not enough.
    let row: Option<(Option<String>, bool, bool, Option<i64>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select totp_secret, {}, code_fails >= $3 and code_fails_since > now() - interval '1 hour', last_step
         from rescue_key k where guild_id = $1 and key_hash = $2 for update",
        brim_core::permits::rescue_expired_sql()
    )))
    .bind(guild)
    .bind(hash(key.unwrap_or_default()))
    .bind(MAX_CODE_FAILS)
    .fetch_optional(&mut *tx)
    .await?;
    // A key made before an ownership change stops working 7 days after it (owner decision 11-17).
    if row.as_ref().is_some_and(|r| r.1) {
        return Err(ApiError::Forbidden(
            "this rescue key expired: the server changed owner more than 7 days ago",
        ));
    }
    let Some((secret, _, waiting, last_step)) = row else {
        return Err(ApiError::Forbidden("invalid rescue key or code"));
    };
    // Keys made before the authenticator existed have no secret: refused (owner decision 2026-11-18).
    let Some(secret) = secret else {
        return Err(ApiError::Forbidden(
            "this rescue key is from before rescue keys needed an authenticator code, so it no longer works: the server owner must make a new one",
        ));
    };
    // Counted on the key: only someone holding it can use up its tries (security review t-ga-rescue-guessing).
    if waiting {
        return Err(ApiError::TooMany(
            "Too many wrong authenticator codes for this rescue key. Try again in an hour.".into(),
        ));
    }
    // A right code deletes the key below; `last_step` refuses a code a Secure my account press used.
    let step = match code {
        Some(c) => crate::otp::step(&unseal(key.unwrap_or_default(), &secret)?, c)?,
        None => None,
    };
    if !step.is_some_and(|n| last_step.is_none_or(|l| (n as i64) > l)) {
        count_code_fail(&mut tx, guild).await?;
        tx.commit().await?;
        return Err(ApiError::Forbidden("invalid rescue key or code"));
    }
    // The delete is the single-use gate: the row lock makes a concurrent claim wait and then miss.
    sqlx::query("delete from rescue_key where guild_id = $1")
        .bind(guild)
        .execute(&mut *tx)
        .await?;
    // The claimer takes the bot's Owner level from the Discord owner account, which is paused until
    // the claimer gives it back; a new Discord owner ends it (owner decision 2026-11-20). The Discord owner
    // claiming with its own account takes the bot back.
    if user == discord_owner {
        sqlx::query("delete from rescue_owners where server_id = $1")
            .bind(guild)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "insert into rescue_owners (server_id, user_id, discord_owner) values ($1, $2, $3)
             on conflict (server_id) do update set user_id = $2, discord_owner = $3, at = now()",
        )
        .bind(guild)
        .bind(user)
        .bind(discord_owner)
        .execute(&mut *tx)
        .await?;
    }
    // Without a recorded owner, on_owner would take this for a first sight and skip the handover.
    sqlx::query(
        "insert into guild_owners (guild_id, owner_id) values ($1, $2) on conflict do nothing",
    )
    .bind(guild)
    .bind(discord_owner)
    .execute(&mut *tx)
    .await?;
    owner_check_now(&mut tx, guild).await?;
    tx.commit().await?;
    Ok(())
}

/// A wrong (or already used) rescue authenticator code, counted on the key for a claim and a Secure
/// my account press alike: `MAX_CODE_FAILS` in an hour and the key's code waits.
pub(crate) async fn count_code_fail(
    tx: &mut sqlx::PgConnection,
    guild: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "update rescue_key set
           code_fails = case when code_fails_since > now() - interval '1 hour' then code_fails + 1 else 1 end,
           code_fails_since = case when code_fails_since > now() - interval '1 hour' then code_fails_since else now() end
         where guild_id = $1",
    )
    .bind(guild)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Queues the bot's `owner_check`, which runs the ownership handover  for a claim or a
/// give-back: it pauses the Extra Owners, Trusted Admins and strong permits of the previous holder
/// (the new one can keep them on Overview) and moves the Owner level role.
async fn owner_check_now(tx: &mut sqlx::PgConnection, guild: i64) -> Result<(), ApiError> {
    sqlx::query(
        "insert into scheduled_jobs (guild_id, kind, payload, run_at)
         select $1, 'owner_check', '{}', now()
         where not exists (select 1 from scheduled_jobs where guild_id = $1 and kind = 'owner_check'
           and (locked_until is null or locked_until < now()))",
    )
    .bind(guild)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Counts a failed claim for the server. Some(n) when this failure starts a new hour, which is posted:
/// n is the number of failures of the hour before that were not posted. Nothing is refused by it.
async fn claim_failed(db: &sqlx::PgPool, guild: i64) -> Result<Option<i32>, ApiError> {
    let (fails, before): (i32, i32) = sqlx::query_as(
        "with old as (select fails from rescue_claim_fails where guild_id = $1)
         insert into rescue_claim_fails (guild_id, fails, since) values ($1, 1, now())
         on conflict (guild_id) do update set
           fails = case when rescue_claim_fails.since > now() - interval '1 hour' then rescue_claim_fails.fails + 1 else 1 end,
           since = case when rescue_claim_fails.since > now() - interval '1 hour' then rescue_claim_fails.since else now() end
         returning fails, coalesce((select fails from old), 1)",
    )
    .bind(guild)
    .fetch_one(db)
    .await?;
    Ok((fails == 1).then_some(before - 1))
}

async fn rescue_claim(State(st): State<AppState>, s: Sess, Json(b): Json<Claim>) -> Res {
    let guild: i64 = b
        .guild_id
        .trim()
        .parse()
        .map_err(|_| ApiError::BadRequest("bad guild id".into()))?;
    if !within_limit(&st.db, "claim", s.user_id).await? {
        return Err(ApiError::Forbidden(
            "too many attempts, try again in 10 minutes",
        ));
    }
    // No membership needed: the account may be new (owner decision 2026-11-20). Anyone can try, so a server
    // without Brim gets the wrong-key answer, is not counted and posts nothing; guessing the
    // 32-byte key is hopeless, and only the key's holder can use up its code tries.
    // An error too: Discord may answer 403 for a server Brim is not in.
    if !matches!(st.discord.guild_name(guild).await, Ok(Some(_))) {
        return Err(ApiError::Forbidden("invalid rescue key or code"));
    }
    let Ok(discord_owner) = st.discord.guild_owner_fresh(guild).await?.parse::<i64>() else {
        return Err(ApiError::BadRequest(
            "Discord did not name the server's owner. Try again; the rescue key was not used."
                .into(),
        ));
    };
    let (_, config) = crate::store::current(&st.db, guild).await?;
    let r = claim_key(
        &st.db,
        (guild, discord_owner),
        s.user_id,
        (
            b.key.as_deref().map(str::trim),
            b.code.as_deref().map(str::trim),
        ),
    )
    .await;
    let who = format!("{} (<@{}>)", s.username, s.user_id);
    let text = match &r {
        Ok(()) if s.user_id == discord_owner => Some(format!("{who}, the server owner, used the rescue key and holds Brim's Owner level again")),
        Ok(()) => Some(format!("{who} used the rescue key and now holds Brim's Owner level in this server. The server owner account <@{discord_owner}> has no Brim control until it is given back.")),
        Err(ApiError::Internal(_)) => None,
        Err(_) => {
            tracing::warn!(guild, user = s.user_id, "failed rescue claim");
            // A failed count must not hide the claim's own answer.
            let posted = claim_failed(&st.db, guild).await.unwrap_or_else(|e| {
                tracing::warn!(?e, guild, "failed rescue claim not counted");
                None
            });
            posted.map(|earlier| {
                let before = match earlier {
                    0 => String::new(),
                    1 => " 1 more failed claim came in the hour before this one.".into(),
                    n => format!(" {n} more failed claims came in the hour before this one."),
                };
                format!("{who} failed a rescue claim.{before} Further failed claims in the next hour are counted and shown with the next post.")
            })
        }
    };
    // One post per server per hour for failed claims, so they cannot bury the other log lines.
    if let Some(text) = text {
        log_change(&st, guild, s.user_id, &config, None, text).await;
    }
    r?;
    let invite = let_in(&st, guild, s.user_id).await;
    Ok(Json(json!({"ok": true, "invite": invite})))
}

/// After a claim: lifts a ban on the account and, when it is not a member, makes a one-use invite
/// for it (24 h). Best effort: the claim stands either way. Verification and the join gate let the
/// bot owner through, as any owner.
async fn let_in(st: &AppState, guild: i64, user: i64) -> Option<String> {
    let path = format!("/guilds/{guild}/bans/{user}");
    match st.discord.send(reqwest::Method::DELETE, &path, None).await {
        // 404: not banned.
        Ok(r) if r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND => {}
        Ok(r) => tracing::warn!(status = %r.status(), guild, "rescue claim: unban failed"),
        Err(e) => tracing::warn!(?e, guild, "rescue claim: unban failed"),
    }
    if st
        .discord
        .is_member_fresh(guild, user)
        .await
        .unwrap_or(false)
    {
        return None;
    }
    let g = st
        .discord
        .call(reqwest::Method::GET, &format!("/guilds/{guild}"), None)
        .await
        .ok()?;
    let mut tries: Vec<String> = ["system_channel_id", "rules_channel_id"]
        .iter()
        .filter_map(|k| g[*k].as_str().map(String::from))
        .collect();
    let chans = st
        .discord
        .call(
            reqwest::Method::GET,
            &format!("/guilds/{guild}/channels"),
            None,
        )
        .await
        .unwrap_or_default();
    let mut text: Vec<(i64, &str)> = chans
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == 0)
        .filter_map(|c| Some((c["position"].as_i64().unwrap_or(0), c["id"].as_str()?)))
        .collect();
    text.sort();
    for (_, id) in text {
        if !tries.iter().any(|t| t == id) {
            tries.push(id.to_string());
        }
    }
    for ch in tries.iter().take(4) {
        let body = json!({"max_age": 86_400, "max_uses": 1, "unique": true});
        let path = format!("/channels/{ch}/invites");
        match st
            .discord
            .call(reqwest::Method::POST, &path, Some(body))
            .await
        {
            Ok(v) => {
                if let Some(code) = v["code"].as_str() {
                    return Some(format!("https://discord.gg/{code}"));
                }
            }
            Err(e) => tracing::info!(?e, guild, "rescue claim: no invite in this channel"),
        }
    }
    tracing::warn!(guild, "rescue claim: no invite could be made");
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn bot_role_permissions_are_restored_on_the_role() {
        let (m, path, body) = inverse_request(5, &Inverse::RolePerms { id: 7, perms: 8 });
        assert_eq!((m, path.as_str()), (Method::PATCH, "/guilds/5/roles/7"));
        assert_eq!(body, Some(json!({"permissions": "8"})));
    }

    /// A ban row is left alone when a later row banned or unbanned the same member.
    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn an_old_ban_row_does_not_lift_a_later_ban() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_894_200_000).await else {
            return;
        };
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
        st.discord.memo.lock().unwrap().insert(
            "/users/@me".into(),
            (std::time::Instant::now(), Some(json!({"id": "1"}))),
        );
        // Anti-nuke's row (no case) for member 5, a moderator's later ban of 5, and a ban of 6.
        let ids: Vec<i64> = sqlx::query_scalar(
            "insert into action_log (guild_id, actor_id, action, target_id, inverse) values
             ($1, 40, 'Ban', 5, '{\"op\": \"unban\", \"user\": 5}'), ($1, 41, 'ban', 5, '{\"op\": \"unban\", \"user\": 5}'),
             ($1, 41, 'ban', 6, '{\"op\": \"unban\", \"user\": 6}') returning id",
        )
        .bind(g)
        .fetch_all(&st.db)
        .await
        .unwrap();
        let body = RevertBody {
            ids: vec![ids[0], ids[2]],
            owners: false,
        };
        let Json(v) = revert(State(st.clone()), s, Path(g), Json(body))
            .await
            .unwrap();
        assert_eq!(v["skipped"], json!([ids[0]]), "{v}");
        // The ban of 6 is the newest: it is tried (and fails here, with no Discord to call).
        assert_eq!(v["failed"][0]["id"], json!(ids[2]), "{v}");
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
    }

    #[tokio::test]
    async fn icon_revert_clears_a_new_icon_and_refuses_a_bad_hash() {
        let seen: crate::Shared<String, Value> = Default::default();
        let s2 = seen.clone();
        let app = axum::Router::new().route(
            "/guilds/5",
            axum::routing::patch(move |Json(b): Json<Value>| async move {
                s2.lock().unwrap().insert("body".into(), b);
                "{}"
            })
            .get(|| async { r#"{"name":"Now","icon":null}"# }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let st = AppState {
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
        };
        let clear = Inverse::GuildIcon {
            hash: None,
            from: None,
        };
        assert!(run_inverse(&st, 5, &clear).await.unwrap());
        assert_eq!(seen.lock().unwrap()["body"], json!({"icon": null}));
        let bad = Inverse::GuildIcon {
            hash: Some("../../users/@me".into()),
            from: None,
        };
        assert!(run_inverse(&st, 5, &bad).await.is_err());
        // Compare-and-set: renamed again since the entry, so the old name is not put back.
        let name = |from: &str| Inverse::GuildName {
            name: "Home".into(),
            from: Some(from.into()),
        };
        assert!(!run_inverse(&st, 5, &name("Nuked")).await.unwrap());
        assert!(run_inverse(&st, 5, &name("Now")).await.unwrap());
        assert_eq!(seen.lock().unwrap()["body"], json!({"name": "Home"}));
        let icon = Inverse::GuildIcon {
            hash: None,
            from: Some(Some("0123456789abcdef0123456789abcdef".into())),
        };
        assert!(!run_inverse(&st, 5, &icon).await.unwrap());
    }

    #[test]
    fn new_inverses_call_the_matching_routes() {
        for (inv, m, path, body) in [
            (
                Inverse::DeleteIntegration { id: 7 },
                Method::DELETE,
                "/guilds/5/integrations/7",
                None,
            ),
            (
                Inverse::DeleteEmoji { id: 7 },
                Method::DELETE,
                "/guilds/5/emojis/7",
                None,
            ),
            (
                Inverse::DeleteSticker { id: 7 },
                Method::DELETE,
                "/guilds/5/stickers/7",
                None,
            ),
            (
                Inverse::DeleteScheduledEvent { id: 7 },
                Method::DELETE,
                "/guilds/5/scheduled-events/7",
                None,
            ),
            (
                Inverse::GuildName {
                    name: "Home".into(),
                    from: None,
                },
                Method::PATCH,
                "/guilds/5",
                Some(json!({"name": "Home"})),
            ),
        ] {
            assert_eq!(inverse_request(5, &inv), (m, path.to_string(), body));
        }
    }

    /// D47: the new owner sees each paused person and role on Overview and keeps them one by one.
    #[tokio::test]
    async fn the_new_owner_keeps_the_previous_staff_one_by_one() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_894_100_000).await else {
            return;
        };
        let run = |sql: &'static str| sqlx::query(sql).bind(g).execute(&st.db);
        for t in [
            "owner_handovers",
            "extra_owners",
            "trusted_admins",
            "guild_config",
            "permits",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        }
        assert!(handover_check(&st, g).await.unwrap().is_empty());
        run("insert into permits (guild_id, name, perms) values ($1, 'Admins', '{dashboard_edit}'), ($1, 'Mods', '{ban}')").await.unwrap();
        run("insert into permit_roles (permit_id, guild_id, role_id) select id, $1, 21 from permits where guild_id = $1 and name = 'Mods'").await.unwrap();
        run("insert into permit_members (permit_id, guild_id, user_id) select id, $1, 21 from permits where guild_id = $1 and name = 'Mods'").await.unwrap();
        run("insert into owner_handovers (guild_id, old_owner, new_owner, extra_owners, trusted_admins, paused_roles, other_permits, quarantine_whitelist)
             values ($1, 3, 7, '{3}', '{12, 13}', '[{\"permit\": \"Admins\", \"role\": \"20\"}]', '{Mods}', '{14}')").await.unwrap();
        let rows = handover_check(&st, g).await.unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "owner_handover",
                "owner_handover:eo:3",
                "owner_handover:ta:12",
                "owner_handover:ta:13",
                "owner_handover:qw:14",
                "owner_handover:role:20:Admins"
            ]
        );
        assert!(
            !rows[0].detail.contains("Ban Sync")
                && rows[0].detail.contains("stay on: Mods (<@21>)")
        );
        assert!(
            rows[1].detail.contains("This is the previous owner"),
            "{}",
            rows[1].detail
        );
        assert!(!rows[2].detail.contains("previous owner"));
        let c = json!({});
        crate::api::g3_handler_tests::seed_user(&st, 12, false);
        crate::api::g3_handler_tests::seed_user(&st, 14, false);
        crate::api::g3_handler_tests::seed_user(&st, 13, true);
        // Role 20 comes back: a plain role, no Onboarding.
        {
            let mut m = st.discord.memo.lock().unwrap();
            let now = std::time::Instant::now();
            m.insert(format!("/guilds/{g}/roles"), (now, Some(json!([]))));
            m.insert(format!("/guilds/{g}/onboarding"), (now, None));
        }
        let keep = |e: &'static str| keep_staff(&st, &s, g, &c, Some(e));
        let _ = keep("ta:12").await.unwrap();
        // The kept permit role reaches the bot (its keyword rule exemptions) as a config change.
        let mut l = sqlx::postgres::PgListener::connect_with(&st.db)
            .await
            .unwrap();
        l.listen("config_changed").await.unwrap();
        let _ = keep("role:20:Admins").await.unwrap();
        let heard = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let n = l.recv().await.unwrap();
                if n.payload().starts_with(&format!("{g}:")) {
                    break;
                }
            }
        });
        assert!(heard.await.is_ok(), "config_changed sent");
        let _ = keep("qw:14").await.unwrap();
        let qw: Value = sqlx::query_scalar("select config->'antinuke'->'quarantine_whitelist' from guild_config where guild_id = $1 order by version desc limit 1")
            .bind(g).fetch_one(&st.db).await.unwrap();
        assert_eq!(qw, json!(["14"]), "kept into the config");
        assert!(
            matches!(keep("ta:12").await, Err(ApiError::NotFound)),
            "only once"
        );
        assert!(
            matches!(keep("eo:99").await, Err(ApiError::NotFound)),
            "only a paused one"
        );
        let n: i64 = sqlx::query_scalar("select (select count(*) from extra_owners where guild_id = $1) + (select count(*) from trusted_admins where guild_id = $1)")
            .bind(g).fetch_one(&st.db).await.unwrap();
        assert_eq!(n, 1, "only the kept TA");
        let p: Value = sqlx::query_scalar("select config->'permits' from guild_config where guild_id = $1 order by version desc limit 1")
            .bind(g).fetch_one(&st.db).await.unwrap();
        assert_eq!(p["ta"], json!(["12"]), "kept as a config version");
        assert!(p["custom"].to_string().contains("\"20\""), "{p}");
        let ids: Vec<String> = handover_check(&st, g)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(
            ids,
            [
                "owner_handover",
                "owner_handover:eo:3",
                "owner_handover:ta:13"
            ]
        );
        // Someone other than the Discord owner (user 7) is refused.
        let other = Sess {
            user_id: 8,
            ..s.clone()
        };
        assert!(matches!(
            keep_staff(&st, &other, g, &json!({}), Some("ta:13")).await,
            Err(ApiError::Forbidden(_))
        ));
        // 13 is a bot: keeping it as TA is refused, so it stays paused (owner decision 11-13).
        let r = keep("ta:13").await;
        assert!(
            matches!(&r, Err(ApiError::BadRequest(m)) if m.contains("member of a permit")),
            "{r:?}"
        );
        let _ = keep("done").await.unwrap();
        assert!(
            handover_check(&st, g).await.unwrap().is_empty(),
            "dismissed"
        );
        let n: i64 = sqlx::query_scalar("select count(*) from extra_owners where guild_id = $1")
            .bind(g)
            .fetch_one(&st.db)
            .await
            .unwrap();
        assert_eq!(n, 0, "dismissing keeps nothing more");
        for t in [
            "owner_handovers",
            "extra_owners",
            "trusted_admins",
            "guild_config",
            "permits",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        }
    }

    /// Owner decision 2026-11-15: the bot pauses strong permits by member; Keep lists the person back
    /// into the permit unless the permit is gone or they hold another one now.
    #[tokio::test]
    async fn the_new_owner_keeps_a_paused_permit_member() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_905_000_000).await else {
            return;
        };
        let wipe = || async {
            for t in [
                "owner_handovers",
                "guild_config",
                "permits",
                "permit_members_migrated",
                "scheduled_jobs",
            ] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "delete from {t} where guild_id = $1"
                )))
                .bind(g)
                .execute(&st.db)
                .await
                .unwrap();
            }
        };
        wipe().await;
        for sql in [
            "insert into permit_members_migrated (guild_id) values ($1)",
            "insert into permits (guild_id, name, perms) values ($1, 'Admins', '{ban}'), ($1, 'Mods', '{warn}')",
            "insert into permit_members (permit_id, guild_id, user_id) select id, $1, u from permits, unnest('{41, 43}'::bigint[]) u where guild_id = $1 and name = 'Mods'",
            "insert into owner_handovers (guild_id, old_owner, new_owner, paused_roles, other_permits)
             values ($1, 3, 7, '[{\"permit\": \"Admins\", \"user\": \"40\"}, {\"permit\": \"Admins\", \"user\": \"41\"}, {\"permit\": \"Gone\", \"user\": \"42\"}, {\"permit\": \"Admins\", \"user\": \"44\"}]', '{Mods}')",
        ] {
            sqlx::query(sql).bind(g).execute(&st.db).await.unwrap();
        }
        // 40 is in the server; 44 left since the handover.
        st.discord.memo.lock().unwrap().extend([
            (
                format!("/guilds/{g}/members/40"),
                (Instant::now(), Some(json!({"roles": []}))),
            ),
            (format!("/guilds/{g}/members/44"), (Instant::now(), None)),
        ]);
        let rows = handover_check(&st, g).await.unwrap();
        assert!(
            rows[0].detail.contains("stay on: Mods (<@41>, <@43>)"),
            "{}",
            rows[0].detail
        );
        assert_eq!(
            (rows[1].id.as_str(), rows[1].title, rows[1].detail.as_str()),
            (
                "owner_handover:member:40:Admins",
                "Paused permit member",
                "<@40> in the permit Admins"
            )
        );
        let c = json!({});
        let keep = |e: &'static str| keep_staff(&st, &s, g, &c, Some(e));
        let _ = keep("member:40:Admins").await.unwrap();
        let by: Option<i64> = sqlx::query_scalar(
            "select added_by from permit_members m join permits p on p.id = m.permit_id
             where m.guild_id = $1 and m.user_id = 40 and p.name = 'Admins'",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert_eq!(by, Some(7));
        let jobs: i64 = sqlx::query_scalar(
            "select count(*) from scheduled_jobs where guild_id = $1 and kind = 'permit_sync'",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert_eq!(jobs, 1);
        for (e, says) in [
            ("member:41:Admins", "in the permit Mods now"),
            ("member:42:Gone", "deleted since"),
            ("member:44:Admins", "not a member of this server"),
        ] {
            let r = keep(e).await;
            assert!(
                matches!(&r, Err(ApiError::BadRequest(m)) if m.contains(says)),
                "{e}: {r:?}"
            );
        }
        let p: Value = sqlx::query_scalar("select config->'permits' from guild_config where guild_id = $1 order by version desc limit 1")
            .bind(g).fetch_one(&st.db).await.unwrap();
        assert!(p["custom"][0]["members"] == json!(["40"]), "{p}");
        wipe().await;
    }

    /// A Discord stand-in for the score fixes: answers the reads `live` makes and records each
    /// PATCH as "path" -> body.
    async fn mock_discord(st: &mut AppState, g: i64, roles: Value) -> crate::Shared<String, Value> {
        let seen: crate::Shared<String, Value> = Default::default();
        let s2 = seen.clone();
        let app = axum::Router::new().fallback(
            move |m: axum::http::Method, uri: axum::http::Uri, body: String| {
                let (s2, roles) = (s2.clone(), roles.clone());
                async move {
                    let path = uri.path().to_string();
                    if m == axum::http::Method::PATCH {
                        let b = serde_json::from_str(&body).unwrap_or_default();
                        s2.lock().unwrap().insert(path, b);
                        return "{}".to_string();
                    }
                    if m == axum::http::Method::POST && path.ends_with("/roles") {
                        return r#"{"id": "77"}"#.into();
                    }
                    match path.rsplit('/').next() {
                        Some("roles") => roles.to_string(),
                        Some("channels") => "[]".into(),
                        Some("@me") => r#"{"id": "1"}"#.into(),
                        Some("members") => json!([
                            {"user": {"id": "2"}, "roles": []},
                            {"user": {"id": "3", "bot": true}, "roles": []},
                            {"user": {"id": "4"}, "roles": ["50"]},
                        ])
                        .to_string(),
                        _ if path.contains("/members/") => r#"{"roles": ["60"]}"#.into(),
                        _ => json!({"id": g.to_string(), "owner_id": "7"}).to_string(),
                    }
                }
            },
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        st.discord.api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        seen
    }

    async fn wipe(st: &AppState, g: i64) {
        for t in [
            "guild_config",
            "backups",
            "guild_setup_done",
            "lock_snapshots",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        }
    }

    fn body(setting: &str) -> Json<FixBody> {
        Json(FixBody {
            shown: None,
            role_id: None,
            channel_id: None,
            setting: Some(setting.into()),
            move_under_bot: false,
        })
    }

    /// Remove dangerous roles: offered (never forced) while not saved on, the confirm lists who
    /// loses which role (a Role lock's saved bits count), and the Fix writes it on.
    #[tokio::test]
    async fn dangerous_role_removal_is_offered_listed_and_fixed() {
        let Some((mut st, s, g)) = crate::api::g3_handler_tests::setup(9_901_200_000).await else {
            return;
        };
        let roles = json!([
            {"id": g.to_string(), "name": "@everyone", "permissions": "1024", "position": 0},
            // Administrator taken off by a Role lock: the lock's snapshot holds it.
            {"id": "50", "name": "Admin", "permissions": "1024", "position": 1},
            {"id": "60", "name": "Brim", "permissions": "8", "position": 2, "managed": true},
        ]);
        mock_discord(&mut st, g, roles).await;
        wipe(&st, g).await;
        sqlx::query("delete from trusted_admins where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        // No saved version: off, and the row offers it.
        let (v, c) = crate::store::current(&st.db, g).await.unwrap();
        assert_eq!(v, 0);
        assert!(enforce_permits_check(&c).is_some());
        let c = json!({"misc": {"prefix": "t!"}});
        sqlx::query("insert into guild_config (guild_id, version, config) values ($1, 1, $2)")
            .bind(g)
            .bind(&c)
            .execute(&st.db)
            .await
            .unwrap();
        let row = enforce_permits_check(&c).unwrap();
        assert_eq!(row.fix.as_ref().unwrap().setting, Some(ENFORCE_PERMITS));
        assert!(enforce_permits_check(&json!({"misc": {"enforce_permits": false}})).is_some());
        let p = enforce_permits_preview(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        assert_eq!(p["count"], 0, "no lock: role 50 is not dangerous now {p}");
        let lock = json!({"kind": null, "role_perms": [[50, 8]], "channel_denies": [], "join_lock": false, "invites_paused": false});
        sqlx::query(
            "insert into lock_snapshots (guild_id, kind, snapshot) values ($1, 'role', $2)",
        )
        .bind(g)
        .bind(&lock)
        .execute(&st.db)
        .await
        .unwrap();
        // Member 4 holds Admin below Brim with no permit; 2 holds nothing, 3 is a bot.
        let p = enforce_permits_preview(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        assert_eq!(p["count"], 1, "{p}");
        assert_eq!(p["members"][0]["roles"], json!(["@Admin"]), "{p}");
        // A Trusted Admin keeps the role, so nobody is listed.
        sqlx::query("insert into trusted_admins (guild_id, user_id) values ($1, 4)")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        let p = enforce_permits_preview(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        assert_eq!(p["count"], 0, "{p}");
        sqlx::query("delete from lock_snapshots where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        let r = fix(State(st.clone()), s.clone(), Path(g), body(ENFORCE_PERMITS))
            .await
            .unwrap();
        assert_eq!(r.0["version"], 2, "{}", r.0);
        let (_, c) = crate::store::current(&st.db, g).await.unwrap();
        assert_eq!(c["misc"], json!({"prefix": "t!", "enforce_permits": true}));
        assert!(enforce_permits_check(&c).is_none());
        sqlx::query("delete from trusted_admins where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        wipe(&st, g).await;
    }

    /// Exemptions by role: the row names each saved role and who holds it, the Fix queues the
    /// bot's move once, and the row then says the move runs.
    #[tokio::test]
    async fn exempt_roles_row_and_fix() {
        let Some((mut st, s, g)) = crate::api::g3_handler_tests::setup(9_901_300_000).await else {
            return;
        };
        let roles = json!([
            {"id": g.to_string(), "name": "@everyone", "permissions": "1024", "position": 0},
            {"id": "50", "name": "Helpers", "permissions": "0", "position": 1},
            {"id": "60", "name": "Brim", "permissions": "8", "position": 2, "managed": true},
        ]);
        mock_discord(&mut st, g, roles).await;
        wipe(&st, g).await;
        let clear = || async {
            sqlx::query("delete from scheduled_jobs where guild_id = $1")
                .bind(g)
                .execute(&st.db)
                .await
                .unwrap();
        };
        clear().await;
        let c = json!({"whitelist": {"roles": ["50"], "invites": {"roles": ["50"]}}, "catch": {"roles": ["99"]}});
        sqlx::query("insert into guild_config (guild_id, version, config) values ($1, 1, $2)")
            .bind(g)
            .bind(&c)
            .execute(&st.db)
            .await
            .unwrap();
        let row = |v: &Value| {
            v["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == "exempt_roles")
                .cloned()
        };
        let v = score(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        let r = row(&v).expect("the row is shown");
        let d = r["detail"].as_str().unwrap();
        assert!(d.contains("@Helpers (Spam, Invite links)"), "{d}");
        assert!(d.contains("a deleted role (99) (Catch channel)"), "{d}");
        // Member 4 holds Helpers; 3 is a bot.
        assert!(d.contains("1 member holds them now"), "{d}");
        assert_eq!(r["fix"]["setting"], crate::exempt_roles::FIX);
        for _ in 0..2 {
            let f = fix(
                State(st.clone()),
                s.clone(),
                Path(g),
                body(crate::exempt_roles::FIX),
            )
            .await
            .unwrap();
            assert_eq!(f.0["queued"], true);
        }
        let jobs: Vec<Value> = sqlx::query_scalar(
            "select payload from scheduled_jobs where guild_id = $1 and kind = 'exempt_roles_move'",
        )
        .bind(g)
        .fetch_all(&st.db)
        .await
        .unwrap();
        assert_eq!(jobs, [json!({"by": s.user_id})], "queued once");
        let v = score(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        let r = row(&v).unwrap();
        assert!(
            r["fix"].is_null() && r["detail"].as_str().unwrap().contains("moving"),
            "{r}"
        );
        clear().await;
        wipe(&st, g).await;
        let v = score(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        assert!(row(&v).is_none(), "no saved roles, no row");
    }

    /// The recommended setup: a server with no saved values gets the profile row and the Backups
    /// row, the Fix writes every difference once, and both rows are gone afterwards.
    #[tokio::test]
    async fn security_setup_row_and_fix() {
        let Some((mut st, s, g)) = crate::api::g3_handler_tests::setup(9_901_400_000).await else {
            return;
        };
        let roles = json!([
            {"id": g.to_string(), "name": "@everyone", "permissions": "1024", "position": 0},
            {"id": "60", "name": "Brim", "permissions": "8", "position": 1, "managed": true},
        ]);
        mock_discord(&mut st, g, roles).await;
        wipe(&st, g).await;
        sqlx::query("insert into guild_config (guild_id, version, config) values ($1, 1, $2)")
            .bind(g)
            .bind(json!({"verification": {"time_limit_ms": 120000}}))
            .execute(&st.db)
            .await
            .unwrap();
        let rows = |v: &Value, id: &str| {
            v["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
                .cloned()
        };
        let v = score(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        let r = rows(&v, "security_profile").expect("the row is shown");
        assert_eq!(r["fix"]["setting"], crate::security_setup::FIX);
        let lines = r["changes"].as_array().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.as_str().unwrap().contains("2 minute(s) to 5 minute(s)")),
            "{lines:?}"
        );
        assert!(rows(&v, "backups_off").is_some());
        let f = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body(crate::security_setup::BACKUPS_FIX),
        )
        .await
        .unwrap();
        assert_eq!(f.0["ok"], true);
        let f = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body(crate::security_setup::FIX),
        )
        .await
        .unwrap();
        assert!(f.0["changed"].as_array().unwrap().len() > 30, "{}", f.0);
        let (_, c) = crate::store::current(&st.db, g).await.unwrap();
        assert_eq!(c["verification"]["time_limit_ms"], 300_000);
        assert_eq!(c["backups"]["enabled"], true);
        assert_eq!(c["antinuke"]["enabled"], true);
        assert_eq!(c["automod"]["filters"]["bad_sites"]["action"], "timeout");
        assert_eq!(c["misc"]["warn_actions"].as_array().unwrap().len(), 2);
        let v = score(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap()
            .0;
        assert!(rows(&v, "security_profile").is_none() && rows(&v, "backups_off").is_none());
        // Pressed again with nothing left to change: no new version.
        let f = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body(crate::security_setup::FIX),
        )
        .await
        .unwrap();
        assert!(f.0["changed"].is_null());
        wipe(&st, g).await;
    }

    /// Move to main roles: each main role gets @everyone's bits, @everyone ends with none, and
    /// a backup is taken first.
    #[tokio::test]
    async fn everyone_permissions_move_to_the_main_roles() {
        let Some((mut st, s, g)) = crate::api::g3_handler_tests::setup(9_901_000_000).await else {
            return;
        };
        let roles = json!([
            {"id": g.to_string(), "name": "@everyone", "permissions": "1024", "position": 0},
            {"id": "50", "name": "Member", "permissions": "2048", "position": 1},
            {"id": "60", "name": "Brim", "permissions": "8", "position": 2, "managed": true},
        ]);
        let seen = mock_discord(&mut st, g, roles).await;
        wipe(&st, g).await;
        // No main roles: refused, nothing is made.
        let r = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body(EVERYONE_TO_MAIN),
        )
        .await;
        assert!(matches!(r, Err(ApiError::BadRequest(m)) if m.contains("no main roles")));
        // The confirm names the members who hold no main role: 2, not the bot 3 or 4 on role 50.
        assert_eq!(members_without(&st, g, &[50]).await.unwrap(), (1, false));
        // Role 99 was deleted in Discord: alone it is refused with where to pick main roles.
        let c = json!({"lockdown": {"main_roles": ["99"]}});
        sqlx::query("insert into guild_config (guild_id, version, config) values ($1, 1, $2)")
            .bind(g)
            .bind(&c)
            .execute(&st.db)
            .await
            .unwrap();
        // Refused during a lock, whose snapshot would put the old permissions back.
        let lock =
            "insert into lock_snapshots (guild_id, kind, snapshot) values ($1, 'role', '{}')";
        sqlx::query(lock).bind(g).execute(&st.db).await.unwrap();
        let r = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body(EVERYONE_TO_MAIN),
        )
        .await;
        assert!(matches!(r, Err(ApiError::BadRequest(m)) if m.contains("lockdown")));
        sqlx::query("delete from lock_snapshots where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        let r = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body(EVERYONE_TO_MAIN),
        )
        .await;
        assert!(matches!(r, Err(ApiError::BadRequest(m)) if m.contains("Lockdown > Main roles")));
        // Beside a live main role it is skipped.
        sqlx::query("update guild_config set config = $2 where guild_id = $1")
            .bind(g)
            .bind(json!({"lockdown": {"main_roles": ["50", "99"]}}))
            .execute(&st.db)
            .await
            .unwrap();
        assert!(seen.lock().unwrap().is_empty(), "nothing changed yet");
        let r = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body(EVERYONE_TO_MAIN),
        )
        .await
        .unwrap();
        assert!(r.0["backup"].is_i64(), "{}", r.0);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen[&format!("/guilds/{g}/roles/50")],
            json!({"permissions": "3072"})
        );
        assert_eq!(
            seen[&format!("/guilds/{g}/roles/{g}")],
            json!({"permissions": "0"})
        );
        // A row with dangerous bits is a strip, never a move.
        let roles = json!([{"id": g.to_string(), "permissions": "8", "position": 0}]);
        let _ = mock_discord(&mut st, g, roles).await;
        assert!(matches!(
            fix(
                State(st.clone()),
                s.clone(),
                Path(g),
                body(EVERYONE_TO_MAIN)
            )
            .await,
            Err(ApiError::BadRequest(_))
        ));
        wipe(&st, g).await;
    }

    /// A saved main role deleted in Discord does not stop Create Member role (owner decision 2026-11-17).
    #[tokio::test]
    async fn member_role_replaces_deleted_main_roles() {
        let Some((mut st, s, g)) = crate::api::g3_handler_tests::setup(9_901_500_000).await else {
            return;
        };
        let at = |st: AppState, c: Value| async move {
            wipe(&st, g).await;
            let q = "insert into guild_config (guild_id, version, config) values ($1, 1, $2)";
            sqlx::query(q)
                .bind(g)
                .bind(&c)
                .execute(&st.db)
                .await
                .unwrap();
        };
        let run = |st: AppState| {
            let s = s.clone();
            async move {
                let _ = fix(State(st.clone()), s, Path(g), body(MEMBER_ROLE))
                    .await
                    .unwrap();
                crate::store::current(&st.db, g).await.unwrap().1
            }
        };
        let dead = json!({"lockdown": {"main_roles": ["99"]}, "verification": {"roles": ["99"]}});
        // No role named Member: one is made (the mock gives it id 77).
        let only_everyone = json!([{"id": g.to_string(), "permissions": "0", "position": 0}]);
        let _ = mock_discord(&mut st, g, only_everyone).await;
        at(st.clone(), dead.clone()).await;
        let c = run(st.clone()).await;
        assert_eq!(c["lockdown"]["main_roles"], json!(["77"]));
        assert_eq!(c["verification"]["roles"], json!(["77"]));
        // A fit role named Member is taken over; one that may mention @everyone never.
        let roles = json!([
            {"id": g.to_string(), "permissions": "0", "position": 0},
            {"id": "30", "name": "Member", "permissions": "131072", "position": 1},
            {"id": "40", "name": "Member", "permissions": "1024", "position": 1},
            {"id": "60", "name": "Brim", "permissions": "8", "position": 2, "managed": true},
        ]);
        let seen = mock_discord(&mut st, g, roles).await;
        at(st.clone(), dead.clone()).await;
        let c = run(st.clone()).await;
        assert_eq!(c["lockdown"]["main_roles"], json!(["40"]));
        assert_eq!(c["verification"]["roles"], json!(["40"]));
        // A taken-over Member role without a colour gets the green; one with a colour keeps it.
        assert_eq!(
            seen.lock().unwrap()[&format!("/guilds/{g}/roles/40")]["color"],
            0x2ECC71
        );
        let coloured = json!([
            {"id": g.to_string(), "permissions": "0", "position": 0},
            {"id": "40", "name": "Member", "permissions": "1024", "position": 1, "color": 255},
            {"id": "60", "name": "Brim", "permissions": "8", "position": 2, "managed": true},
        ]);
        let seen = mock_discord(&mut st, g, coloured).await;
        at(st.clone(), dead).await;
        let _ = run(st.clone()).await;
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
        // A live main role with deleted verification roles: verification gets the main role.
        at(
            st.clone(),
            json!({"lockdown": {"main_roles": ["40"]}, "verification": {"roles": ["98"]}}),
        )
        .await;
        let c = run(st.clone()).await;
        assert_eq!(c["lockdown"]["main_roles"], json!(["40"]));
        assert_eq!(c["verification"]["roles"], json!(["40"]));
        wipe(&st, g).await;
    }

    /// Turn on saves `enabled` as a config version, and keeps each section's own write bar.
    #[tokio::test]
    async fn turn_on_saves_the_switch_as_a_version() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_902_000_000).await else {
            return;
        };
        wipe(&st, g).await;
        let r = fix(
            State(st.clone()),
            s.clone(),
            Path(g),
            body("enable_joingate"),
        )
        .await
        .unwrap();
        let (v, c) = crate::store::current(&st.db, g).await.unwrap();
        assert_eq!(
            (r.0["version"].as_i64(), c["joingate"]["enabled"].clone()),
            (Some(v as i64), json!(true))
        );
        // A Trusted Admin cannot switch Anti-nuke, as on its page.
        let r = setup_fix(
            &st,
            &s,
            g,
            Level::Admin,
            v,
            &c,
            Some("enable_antinuke"),
            None,
        )
        .await;
        assert!(matches!(r, Err(ApiError::Forbidden(_))));
        assert!(setup_fix(
            &st,
            &s,
            g,
            Level::Owner,
            v,
            &c,
            Some("verification_level"),
            None
        )
        .await
        .unwrap()
        .is_none());
        // Move into range: an old 0 goes to the lowest allowed value.
        let mut c = c;
        c["panic"]["window_secs"] = json!(0);
        let r = setup_fix(&st, &s, g, Level::Owner, v, &c, Some(CLAMP), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.0["clamped"], json!(["panic.window_secs: 0 -> 1"]));
        // The Suspicious account filter goes on, also with no filters saved yet.
        let (v, mut c) = crate::store::current(&st.db, g).await.unwrap();
        c["automod"] = json!({"enabled": true});
        let _ = setup_fix(&st, &s, g, Level::Owner, v, &c, Some(SUSPICION), None)
            .await
            .unwrap()
            .unwrap();
        let (_, c) = crate::store::current(&st.db, g).await.unwrap();
        assert_eq!(c["automod"]["filters"]["suspicion"]["enabled"], json!(true));
        wipe(&st, g).await;
    }

    #[test]
    fn a_long_timeout_is_restored_in_a_28_day_chunk() {
        let far = chrono::Utc::now().timestamp() + 90 * 86_400;
        let (_, _, body) = inverse_request(
            5,
            &Inverse::Timeout {
                user: 7,
                until: far,
            },
        );
        let set = body.unwrap()["communication_disabled_until"]
            .as_str()
            .unwrap()
            .to_string();
        let set = chrono::DateTime::parse_from_rfc3339(&set)
            .unwrap()
            .timestamp();
        assert!((set - (far - 62 * 86_400)).abs() < 30);
    }

    #[tokio::test]
    async fn a_claim_for_a_server_without_the_bot_counts_nothing() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_300_000_000 + std::process::id() as i64;
        sqlx::query("insert into rescue_key (guild_id, key_hash) values ($1, $2) on conflict (guild_id) do update set key_hash = $2")
            .bind(g).bind(hash("k")).execute(&db).await.unwrap();
        // Discord answers 404: no such server, or Brim is not in it. Anyone may try now, so
        // nothing is counted or posted, and the answer is the wrong-key one.
        let app =
            axum::Router::new().fallback(|| async { (axum::http::StatusCode::NOT_FOUND, "{}") });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let st = AppState {
            db: db.clone(),
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
        };
        let s = Sess {
            id: String::new(),
            user_id: g,
            username: "x".into(),
            access_token: String::new(),
            stepped_up: false,
            step_up_guild: None,
            csrf: String::new(),
        };
        let claim = Claim {
            guild_id: g.to_string(),
            key: Some("k".into()),
            code: None,
        };
        let r = rescue_claim(State(st), s, Json(claim)).await;
        assert!(matches!(
            r,
            Err(ApiError::Forbidden("invalid rescue key or code"))
        ));
        let kept: bool =
            sqlx::query_scalar("select exists(select 1 from rescue_key where guild_id = $1)")
                .bind(g)
                .fetch_one(&db)
                .await
                .unwrap();
        let eo: bool =
            sqlx::query_scalar("select exists(select 1 from rescue_owners where server_id = $1) or exists(select 1 from rescue_claim_fails where guild_id = $1)")
                .bind(g)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(kept && !eo);
        sqlx::query("delete from rescue_key where guild_id = $1")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn claim_needs_key_and_code_once() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_200_000_000 + std::process::id() as i64;
        let secret = crate::otp::new_secret();
        let sealed = seal("k", &secret);
        assert!(!sealed.contains(&secret));
        assert_eq!(unseal("k", &sealed).unwrap(), secret);
        assert_ne!(unseal("j", &sealed).unwrap_or_default(), secret);
        sqlx::query("insert into rescue_key (guild_id, key_hash, totp_secret) values ($1, $2, $3) on conflict (guild_id) do update set key_hash = $2, totp_secret = $3")
            .bind(g).bind(hash("k")).bind(&sealed).execute(&db).await.unwrap();
        let code = crate::otp::current(&secret);
        assert!(claim_key(&db, (g, 7), 6, (None, Some(&code)))
            .await
            .is_err());
        assert!(claim_key(&db, (g, 7), 6, (Some("k"), None)).await.is_err());
        // A code a Secure my account press used is refused too.
        let step = crate::otp::step(&secret, &code).unwrap().unwrap() as i64;
        sqlx::query("update rescue_key set last_step = $2 where guild_id = $1")
            .bind(g)
            .bind(step)
            .execute(&db)
            .await
            .unwrap();
        assert!(claim_key(&db, (g, 7), 6, (Some("k"), Some(&code)))
            .await
            .is_err());
        sqlx::query("update rescue_key set last_step = $2 where guild_id = $1")
            .bind(g)
            .bind(step - 2)
            .execute(&db)
            .await
            .unwrap();
        claim_key(&db, (g, 7), 6, (Some("k"), Some(&code)))
            .await
            .unwrap();
        assert!(claim_key(&db, (g, 7), 6, (Some("k"), Some(&code)))
            .await
            .is_err());
        sqlx::query("delete from rescue_owners where server_id = $1")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
    }

    /// Not removed by purge_guild on purpose (its column is `server_id`).
    async fn drop_claim(db: &sqlx::PgPool, g: i64) {
        sqlx::query("delete from rescue_owners where server_id = $1")
            .bind(g)
            .execute(db)
            .await
            .unwrap();
    }

    /// A rescue key "k" with an authenticator; returns a current code.
    async fn add_key(db: &sqlx::PgPool, g: i64) -> String {
        let secret = crate::otp::new_secret();
        sqlx::query("insert into rescue_key (guild_id, key_hash, totp_secret) values ($1, $2, $3) on conflict (guild_id) do update set key_hash = $2, totp_secret = $3, code_fails = 0")
            .bind(g).bind(hash("k")).bind(seal("k", &secret)).execute(db).await.unwrap();
        crate::otp::current(&secret)
    }

    #[tokio::test]
    async fn claim_is_single_use() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_100_000_000 + std::process::id() as i64;
        let code = add_key(&db, g).await;
        assert!(matches!(
            claim_key(&db, (g, 7), 5, (Some("bad"), Some(&code))).await,
            Err(ApiError::Forbidden(_))
        ));
        claim_key(&db, (g, 7), 5, (Some("k"), Some(&code)))
            .await
            .unwrap();
        assert!(matches!(
            claim_key(&db, (g, 7), 5, (Some("k"), Some(&code))).await,
            Err(ApiError::Forbidden(_))
        ));
        let n: i64 = sqlx::query_scalar(
            "select count(*) from rescue_owners where server_id = $1 and user_id = 5 and discord_owner = 7",
        )
        .bind(g)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(n, 1);
        drop_claim(&db, g).await;
        for t in ["scheduled_jobs"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }

    /// security review: wrong codes count on the key (10 an hour, then it waits, also for the right
    /// code); a wrong key counts nothing; a key without an authenticator is refused; failed claims
    /// are posted once per hour per server with the count of the hour before.
    #[tokio::test]
    async fn wrong_codes_wait_on_the_key_and_old_keys_are_refused() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_103_000_000 + std::process::id() as i64;
        let code = add_key(&db, g).await;
        let wrong = if code == "000000" { "111111" } else { "000000" };
        for _ in 0..20 {
            let r = claim_key(&db, (g, 7), 5, (Some("bad"), Some(wrong))).await;
            assert!(matches!(r, Err(ApiError::Forbidden(_))));
        }
        for _ in 0..MAX_CODE_FAILS {
            let r = claim_key(&db, (g, 7), 5, (Some("k"), Some(wrong))).await;
            assert!(matches!(r, Err(ApiError::Forbidden(_))), "{r:?}");
        }
        let r = claim_key(&db, (g, 7), 5, (Some("k"), Some(&code))).await;
        assert!(matches!(r, Err(ApiError::TooMany(_))), "{r:?}");
        sqlx::query("update rescue_key set code_fails_since = now() - interval '61 minutes' where guild_id = $1")
            .bind(g).execute(&db).await.unwrap();
        claim_key(&db, (g, 7), 5, (Some("k"), Some(&code)))
            .await
            .unwrap();

        sqlx::query("insert into rescue_key (guild_id, key_hash) values ($1, $2) on conflict (guild_id) do update set key_hash = $2, totp_secret = null")
            .bind(g).bind(hash("k")).execute(&db).await.unwrap();
        let r = claim_key(&db, (g, 7), 6, (Some("k"), None)).await;
        assert!(matches!(r, Err(ApiError::Forbidden(_))), "{r:?}");
        assert!(
            rescue_expired(&db, g).await.unwrap(),
            "the owner is asked for a new key"
        );

        assert_eq!(claim_failed(&db, g).await.unwrap(), Some(0));
        assert_eq!(claim_failed(&db, g).await.unwrap(), None);
        assert_eq!(claim_failed(&db, g).await.unwrap(), None);
        sqlx::query("update rescue_claim_fails set since = now() - interval '61 minutes' where guild_id = $1")
            .bind(g).execute(&db).await.unwrap();
        assert_eq!(claim_failed(&db, g).await.unwrap(), Some(2));
        drop_claim(&db, g).await;
        for t in ["rescue_key", "rescue_claim_fails", "scheduled_jobs"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }

    /// Owner decision 2026-11-20: a claim moves Brim's Owner level to the claimer; the Discord owner
    /// account then has no level; a new Discord owner keeps it;
    /// the bot is told (owner_check) to run the handover; give-back and a claim by the Discord
    /// owner return it.
    #[tokio::test]
    async fn a_claim_moves_the_bot_owner_until_given_back() {
        use brim_core::permits::{bot_owner, resolve, Level as L};
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_101_000_000).await else {
            return;
        };
        let db = st.db.clone();
        let code = add_key(&db, g).await;
        claim_key(&db, (g, 7), 5, (Some("k"), Some(&code)))
            .await
            .unwrap();
        assert_eq!(bot_owner(&db, g, 7).await.unwrap(), 5);
        assert_eq!(
            bot_owner(&db, g, 8).await.unwrap(),
            5,
            "a new Discord owner keeps it"
        );
        assert_eq!(resolve(&db, g, 7, 5, false).await.unwrap(), L::Owner);
        assert_eq!(resolve(&db, g, 7, 7, false).await.unwrap(), L::None);
        let queued: bool = sqlx::query_scalar(
            "select exists(select 1 from scheduled_jobs where guild_id = $1 and kind = 'owner_check')",
        )
        .bind(g)
        .fetch_one(&db)
        .await
        .unwrap();
        assert!(queued);
        // The paused Discord owner account (the session in setup) is no owner on the dashboard.
        assert!(!crate::api::is_discord_owner(&st, &s, g).await.unwrap());
        let r = rescue_give_back(State(st.clone()), s.clone(), Path(g)).await;
        assert!(r.is_err(), "only the claimer gives it back");
        let claimer = Sess {
            user_id: 5,
            ..s.clone()
        };
        // As the bot's on_owner after the claim (the claim seeded the Discord owner).
        sqlx::query("update guild_owners set owner_id = 5 where guild_id = $1")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        assert!(crate::api::is_discord_owner(&st, &claimer, g)
            .await
            .unwrap());
        let status = rescue_status(State(st.clone()), claimer.clone(), Path(g))
            .await
            .unwrap();
        assert_eq!(status.0["paused_owner"], "7");
        add_key(&db, g).await;
        let _ = rescue_give_back(State(st.clone()), claimer, Path(g))
            .await
            .unwrap();
        assert_eq!(bot_owner(&db, g, 7).await.unwrap(), 7);
        let key: bool =
            sqlx::query_scalar("select exists(select 1 from rescue_key where guild_id = $1)")
                .bind(g)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(!key, "the claimer's key goes with it");
        // The Discord owner using its own key takes the bot back too.
        let code = add_key(&db, g).await;
        claim_key(&db, (g, 7), 5, (Some("k"), Some(&code)))
            .await
            .unwrap();
        let code = add_key(&db, g).await;
        claim_key(&db, (g, 7), 7, (Some("k"), Some(&code)))
            .await
            .unwrap();
        assert_eq!(bot_owner(&db, g, 7).await.unwrap(), 7);
        drop_claim(&db, g).await;
        brim_core::db::purge_guild(&db, g).await.unwrap();
    }

    /// Owner decision 11-17: after an ownership change the old key works 7 days and nobody makes a new one;
    /// after that the key is refused and the physical owner is asked for a new one.
    #[tokio::test]
    async fn the_old_rescue_key_works_7_days_after_a_handover() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_102_000_000).await else {
            return;
        };
        let db = st.db.clone();
        let handover = |ago: &'static str| {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "insert into owner_handovers (guild_id, old_owner, new_owner, at) values ($1, 1, 7, now() - interval '{ago}')
                 on conflict (guild_id) do update set at = excluded.at"
            )))
            .bind(g)
            .execute(&db)
        };
        sqlx::query("insert into guild_owners (guild_id, owner_id) values ($1, 7) on conflict (guild_id) do update set owner_id = 7")
            .bind(g).execute(&db).await.unwrap();
        let secret = crate::otp::new_secret();
        sqlx::query("insert into rescue_key (guild_id, key_hash, totp_secret, created_at) values ($1, $2, $3, now() - interval '30 days') on conflict (guild_id) do update set key_hash = $2, totp_secret = $3, created_at = excluded.created_at")
            .bind(g).bind(hash("k")).bind(seal("k", &secret)).execute(&db).await.unwrap();
        handover("8 days").await.unwrap();
        assert!(rescue_opens(&db, g).await.unwrap().is_none());
        assert!(rescue_expired(&db, g).await.unwrap());
        assert!(matches!(
            claim_key(&db, (g, 7), 5, (Some("k"), None)).await,
            Err(ApiError::Forbidden(m)) if m.contains("expired")
        ));
        handover("1 day").await.unwrap();
        let opens = rescue_opens(&db, g).await.unwrap().unwrap();
        let left = (opens - chrono::Utc::now()).num_hours();
        assert!((5 * 24 - 1..=6 * 24 + 1).contains(&left), "{opens}");
        assert!(!rescue_expired(&db, g).await.unwrap());
        // Inside the 7 days nobody makes a new key, and the refusal names the date.
        let r = rescue_regenerate(State(st.clone()), s.clone(), Path(g)).await;
        assert!(
            matches!(&r, Err(ApiError::BadRequest(m)) if m.contains("until")),
            "{:?}",
            r.err()
        );
        claim_key(
            &db,
            (g, 7),
            7,
            (Some("k"), Some(&crate::otp::current(&secret))),
        )
        .await
        .unwrap();
        // A key made after the handover is the new owner's: no grace, no expiry, no prompt.
        let _key = rescue_regenerate(State(st.clone()), s.clone(), Path(g))
            .await
            .unwrap();
        assert!(rescue_opens(&db, g).await.unwrap().is_none());
        assert!(!rescue_expired(&db, g).await.unwrap());
        drop_claim(&db, g).await;
        for t in [
            "guild_config",
            "owner_handovers",
            "rescue_key",
            "guild_owners",
            "scheduled_jobs",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }
}

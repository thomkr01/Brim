//! Permit engine (spec section 2): who may run which mod action against whom. Pure, no I/O.
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::models::UserId;

/// Higher variants outrank lower ones; `Custom` ranks by its permit rank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    None,
    Custom {
        rank: i32,
        perms: Vec<String>,
        /// Systems this permit is exempt from: "antinuke", "automod", "words", "quarantine" or "all".
        whitelist: Vec<String>,
        /// Per-action command rate limits, keyed by action name.
        limits: HashMap<String, Limit>,
    },
    TrustedAdmin,
    ExtraOwner,
    Owner,
}

impl Level {
    fn order(&self) -> (u8, i32) {
        match self {
            Level::None => (0, 0),
            Level::Custom { rank, .. } => (1, *rank),
            Level::TrustedAdmin => (2, 0),
            Level::ExtraOwner => (3, 0),
            Level::Owner => (4, 0),
        }
    }

    /// Strictly higher: Owner > Extra Owner > Trusted Admin > permit by rank > none. Nobody acts
    /// against the same or a higher level (owner decision 2026-11-15).
    pub fn outranks(&self, other: &Level) -> bool {
        self.order() > other.order()
    }

    /// Owner, EO and TA are immune to every bot action and bypass all systems.
    pub fn immune(&self) -> bool {
        self.order().0 >= 2
    }

    pub fn has(&self, perm: &str) -> bool {
        match self {
            Level::None => false,
            Level::Custom { perms, .. } => perms
                .iter()
                .any(|p| p == perm || implied_by(perm).contains(&p.as_str())),
            _ => true,
        }
    }

    pub fn whitelisted(&self, flag: &str) -> bool {
        match self {
            Level::None => false,
            Level::Custom { whitelist, .. } => whitelist.iter().any(|w| {
                // The per-action `wl_` flags are narrower forms of the older blanket `antinuke` flag.
                w == flag || w == "all" || (flag.starts_with("wl_") && w == "antinuke")
            }),
            _ => true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Denied {
    NoPermit,
    TargetProtected,
    TargetAboveBot,
    SelfTarget,
}

/// Permits that predate a command keep working: holders of these older permits also get the new one.
fn implied_by(perm: &str) -> &'static [&'static str] {
    match perm {
        "lockdown" => &["lock_channels", "lock_staff_roles", "lock_joins"],
        "quarantine" => &["kick", "ban"],
        "purge" | "slowmode" => &["timeout", "kick", "ban"],
        "cases" => &["warn", "timeout", "kick", "ban"],
        "notes_view" => &["notes_manage"],
        _ => &[],
    }
}

/// Admins and Moderators permits without the `automod` (or `all`) whitelist flag: their members
/// are staff the automod can punish. Owner, EO and TA are always whitelisted.
pub async fn unwhitelisted_staff(pool: &sqlx::PgPool, guild_id: i64) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT name FROM permits WHERE guild_id = $1 AND name IN ('Admins', 'Moderators')
         AND NOT (whitelist && ARRAY['automod', 'all']) ORDER BY name",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
}

/// Role ids of the default staff permits (Admins, Moderators): who Channel Setup lets into limited channels.
pub async fn staff_roles(pool: &sqlx::PgPool, guild_id: i64) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT DISTINCT r.role_id FROM permits p JOIN permit_roles r ON r.permit_id = p.id
         WHERE p.guild_id = $1 AND p.name IN ('Admins', 'Moderators') ORDER BY 1",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
}

/// The staff roles: those of the Admins and Moderators permits and of the levels (Trusted Admin,
/// Owner). The security score notes their Mention @everyone without counting it.
pub async fn all_staff_roles(pool: &sqlx::PgPool, guild_id: i64) -> sqlx::Result<Vec<i64>> {
    let mut all = staff_roles(pool, guild_id).await?;
    all.extend(
        level_roles(pool, guild_id)
            .await?
            .into_iter()
            .map(|(_, r)| r),
    );
    all.sort_unstable();
    all.dedup();
    Ok(all)
}

/// Role ids on the Moderators permit and not on Admins: the security score holds them to member rules.
pub async fn mod_roles(pool: &sqlx::PgPool, guild_id: i64) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT DISTINCT r.role_id FROM permits p JOIN permit_roles r ON r.permit_id = p.id
         WHERE p.guild_id = $1 AND p.name = 'Moderators' AND NOT EXISTS (
           SELECT 1 FROM permits a JOIN permit_roles ar ON ar.permit_id = a.id
           WHERE a.guild_id = $1 AND a.name = 'Admins' AND ar.role_id = r.role_id)
         ORDER BY 1",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
}

/// One toggle in the dashboard's permit editor. `flag` entries live in a permit's whitelist, the
/// rest in its perms. `locked` ones are shown but never stored.
pub struct PermDef {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub flag: bool,
    pub locked: bool,
}

pub struct PermSection {
    pub id: &'static str,
    pub title: &'static str,
    pub items: &'static [PermDef],
}

const fn perm(key: &'static str, label: &'static str, help: &'static str) -> PermDef {
    PermDef {
        key,
        label,
        help,
        flag: false,
        locked: false,
    }
}

const fn flag(key: &'static str, label: &'static str, help: &'static str) -> PermDef {
    PermDef {
        key,
        label,
        help,
        flag: true,
        locked: false,
    }
}

/// Everything a permit can hold, in the order the editor shows it (five-section layout).
pub const CATALOG: &[PermSection] = &[
    PermSection {
        id: "immunity",
        title: "Immunity",
        items: &[
            PermDef {
                key: "owner",
                label: "Owner",
                help: "Anyone with this can do ANYTHING and is IMMUNE from anything",
                flag: false,
                locked: true,
            },
            flag(
                "all",
                "Full Immunity",
                "Entirely immune, can't change settings, total whitelist",
            ),
            flag(
                "automod",
                "Automod Immunity",
                "Safe from the automod, but not other things",
            ),
            perm(
                "dashboard_view",
                "Dashboard Access",
                "See this server's dashboard",
            ),
            perm(
                "dashboard_edit",
                "Dashboard Edit",
                "Change settings on this server's dashboard",
            ),
            flag(
                "reports",
                "Report Immunity",
                "Safe from being reported by server members",
            ),
        ],
    },
    PermSection {
        id: "moderation",
        title: "Moderation",
        items: &[
            perm("cases", "Manage Cases", "View and edit cases"),
            perm(
                "mod_stats",
                "View Mod Stats",
                "See how many actions each staff member took. Only owners can change who has this",
            ),
            perm(
                "invite_view",
                "View Invites",
                "See who invited a member and the invite tracking status",
            ),
            perm("ban", "Ban Command", "Ban, unban and hackban"),
            perm(
                "kick",
                "Kick Command",
                "Anyone with this can kick members without limitations",
            ),
            perm("timeout", "Timeout Command", "Timeout and untimeout"),
            perm(
                "quarantine",
                "Quarantine Command",
                "Quarantine and unquarantine",
            ),
            perm("warn", "Warn Command", "Warn and unwarn"),
            perm(
                "slowmode",
                "Slowmode Command",
                "Anyone with this can set slowmode without limitations",
            ),
            perm(
                "purge",
                "Purge Command",
                "Anyone with this can purge messages without limitations",
            ),
            perm("verify", "Verify Command", "Manually verify members"),
            perm("sanitize", "Sanitize Command", "Sanitize/Dehoist"),
            perm("reports", "Manage Reports", "View and act on reports"),
            perm("backups", "Backups", "Take/Load/Delete backups"),
            perm(
                "notes_view",
                "View Notes",
                "Anyone with this can view member notes",
            ),
            perm(
                "notes_manage",
                "Manage Notes",
                "View/edit/remove/sweep notes",
            ),
            perm(
                "appeals",
                "Review Appeals",
                "Anyone with this can accept or deny appeals",
            ),
            perm("revert", "Revert Center", "Undo anti-nuke actions"),
            perm("tickets", "Manage Tickets", "See and close every ticket"),
            perm(
                "tags",
                "Post Tags",
                "Post a saved message by its tag with /tag",
            ),
            perm(
                "giveaways",
                "Manage Giveaways",
                "Start, end, cancel, reroll and redraw giveaways",
            ),
        ],
    },
    PermSection {
        id: "lockdown",
        title: "Lockdown",
        items: &[
            perm(
                "lock_channels",
                "Lock Server Channels",
                "Anyone with this can lock and unlock server channels",
            ),
            perm(
                "lock_staff_roles",
                "Lock Staff Roles",
                "Lock/Unlock all dangerous roles",
            ),
            perm(
                "lock_joins",
                "Lock Server Joins",
                "Kick/Ban new members joining",
            ),
            perm(
                "lock_updates",
                "Make Lockdown Updates",
                "Make updates in the lock-updates channel",
            ),
        ],
    },
    PermSection {
        id: "whitelist",
        title: "Whitelist",
        items: &[
            flag(
                "wl_kick_ban",
                "Kick/Ban Whitelist",
                "Anyone with this can kick and ban without limitations",
            ),
            flag(
                "wl_channel_create",
                "Channel Creations Whitelist",
                "Anyone with this can create channels without limitations",
            ),
            flag(
                "wl_channel_delete",
                "Channel Deletions Whitelist",
                "Anyone with this can delete channels without limitations",
            ),
            flag(
                "wl_role_create",
                "Role Creations Whitelist",
                "Anyone with this can create roles without limitations",
            ),
            flag(
                "wl_role_delete",
                "Role Deletions Whitelist",
                "Anyone with this can delete roles without limitations",
            ),
            flag(
                "wl_webhook_create",
                "Webhook Creations Whitelist",
                "Anyone with this can create webhooks without limitations",
            ),
            flag(
                "quarantine",
                "Hold Whitelist",
                "Never quarantined by the quarantine hold",
            ),
            flag("words", "Profanity Whitelist", "Bypass the words blacklist"),
            flag("links", "Link Whitelist", "Bypass the links blacklist"),
        ],
    },
];

/// Days a rescue key made before an ownership change keeps working (owner decision 11-17). In that time
/// nobody makes a new key; after it the key is refused and the new owner is asked for a new one.
pub const RESCUE_GRACE_DAYS: i64 = 7;

/// SQL, true when the rescue key row `k` was made before the last ownership change and that change
/// is more than [`RESCUE_GRACE_DAYS`] old: the key no longer works.
pub fn rescue_expired_sql() -> String {
    format!(
        "exists(select 1 from owner_handovers h where h.guild_id = k.guild_id
         and h.at > k.created_at and h.at <= now() - make_interval(days => {RESCUE_GRACE_DAYS}))"
    )
}

/// Perms that make a permit's holders owners or dashboard editors.
pub const HANDOVER_PERMS: &[&str] = &["owner", "dashboard_edit"];

/// Whether a permit is guarded: an ownership change pauses its roles, and a rollback never makes
/// a role guarded again. Strong by default, so a new flag is covered without a change here.
pub fn guarded<S: AsRef<str>>(perms: &[S], whitelist: &[S]) -> bool {
    perms.iter().any(|p| HANDOVER_PERMS.contains(&p.as_ref()))
        || whitelist
            .iter()
            .any(|w| !WEAK_WHITELIST.contains(&w.as_ref()))
}

/// Keys older setups may already hold; saving them stays allowed.
const LEGACY: &[&str] = &["lockdown", "antinuke"];

/// Whether `key` may be stored as a perm (`flag` false) or a whitelist flag (`flag` true).
pub fn known(key: &str, flag: bool) -> bool {
    LEGACY.contains(&key)
        || CATALOG
            .iter()
            .flat_map(|s| s.items)
            .any(|d| d.key == key && d.flag == flag && d.key != "owner")
}

/// `target_above_bot`: target's top role is not below the bot's top role (Discord hierarchy).
pub fn check(
    actor: &Level,
    perm: &str,
    actor_id: UserId,
    target: &Level,
    target_id: UserId,
    target_above_bot: bool,
) -> Result<(), Denied> {
    if !actor.has(perm) {
        return Err(Denied::NoPermit);
    }
    if actor_id == target_id {
        return Err(Denied::SelfTarget);
    }
    // Any permit holder is protected, except from a quarantine by a higher rank (owner decision 2026-11-15).
    if target.immune()
        || (*target != Level::None && (perm != "quarantine" || !actor.outranks(target)))
    {
        return Err(Denied::TargetProtected);
    }
    if target_above_bot {
        return Err(Denied::TargetAboveBot);
    }
    Ok(())
}

/// Per-permit command rate limit (spec 2.5.1). Sliding windows over recorded timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct Limit {
    pub per_min: u32,
    pub per_hour: u32,
}

#[derive(Default)]
pub struct RateLimiter {
    /// Keyed by guild too: one mod's uses in one server never count in another.
    hits: HashMap<(i64, UserId, String), VecDeque<Instant>>,
}

impl RateLimiter {
    /// Records the use and returns `false` when it exceeds the limit (caller quarantines the actor).
    pub fn allow(
        &mut self,
        guild: i64,
        actor: UserId,
        action: &str,
        limit: Limit,
        now: Instant,
    ) -> bool {
        let q = self
            .hits
            .entry((guild, actor, action.to_owned()))
            .or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(3600))
        {
            q.pop_front();
        }
        q.push_back(now);
        let last_min = q
            .iter()
            .filter(|t| now.duration_since(**t) < Duration::from_secs(60))
            .count();
        last_min as u32 <= limit.per_min && q.len() as u32 <= limit.per_hour
    }
}

impl RateLimiter {
    /// Puts back a use from before a restart (N3). Call oldest first.
    pub fn seed(&mut self, guild: i64, actor: UserId, action: &str, at: Instant) {
        self.hits
            .entry((guild, actor, action.to_owned()))
            .or_default()
            .push_back(at);
    }
}

/// The limit a command use counts against: the permit's own for `action`, else `fallback`
/// (anti-nuke's limit for the same action). None: unlimited (no permit, or immune).
pub fn command_limit(level: &Level, action: &str, fallback: Option<Limit>) -> Option<Limit> {
    match level {
        Level::Custom { limits, .. } => limits.get(action).copied().or(fallback),
        _ => None,
    }
}

/// Whether `actor` may run `action` now; `false` means the caller quarantines the actor.
pub fn command_allowed(
    rl: &mut RateLimiter,
    guild: i64,
    actor: UserId,
    level: &Level,
    action: &str,
    now: Instant,
) -> bool {
    command_limit(level, action, None).is_none_or(|l| rl.allow(guild, actor, action, l, now))
}

/// The live EO/TA lists and custom permits as one document: the `permits` copy in each config version.
pub async fn snapshot(c: &mut sqlx::PgConnection, guild: i64) -> sqlx::Result<serde_json::Value> {
    let users = |table: &str| {
        format!("select user_id::text from {table} where guild_id = $1 order by user_id")
    };
    let eo: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(users("extra_owners")))
        .bind(guild)
        .fetch_all(&mut *c)
        .await?;
    let ta: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(users("trusted_admins")))
        .bind(guild)
        .fetch_all(&mut *c)
        .await?;
    type Row = (
        i64,
        String,
        i32,
        Vec<String>,
        serde_json::Value,
        Vec<String>,
    );
    let rows: Vec<Row> = sqlx::query_as("select id, name, rank, perms, limits, whitelist from permits where guild_id = $1 order by rank desc, id")
        .bind(guild)
        .fetch_all(&mut *c)
        .await?;
    let mut custom = vec![];
    for (id, name, rank, perms, limits, whitelist) in rows {
        let roles: Vec<String> = sqlx::query_scalar(
            "select role_id::text from permit_roles where permit_id = $1 order by role_id",
        )
        .bind(id)
        .fetch_all(&mut *c)
        .await?;
        let members: Vec<String> = sqlx::query_scalar(
            "select user_id::text from permit_members where permit_id = $1 order by user_id",
        )
        .bind(id)
        .fetch_all(&mut *c)
        .await?;
        custom.push(serde_json::json!({"name": name, "rank": rank, "perms": perms, "limits": limits, "whitelist": whitelist, "roles": roles, "members": members}));
    }
    let levels: Vec<(String, String)> = sqlx::query_as(
        "select level, role_id::text from level_roles where guild_id = $1 order by level",
    )
    .bind(guild)
    .fetch_all(&mut *c)
    .await?;
    let levels: serde_json::Map<String, serde_json::Value> =
        levels.into_iter().map(|(l, r)| (l, r.into())).collect();
    let mut doc = serde_json::json!({"eo": eo, "ta": ta, "custom": custom});
    // Left out while no level has a role, so older copies still compare equal.
    if !levels.is_empty() {
        doc["levels"] = levels.into();
    }
    Ok(doc)
}

/// Whether a new permits copy changes who gets a level role: the Extra Owner or Trusted Admin
/// list, or the level roles themselves. Then `permit_sync` must run, or a removed Trusted Admin
/// keeps the role until the next restart (security pass, 11-17).
pub fn level_lists_changed(old: &serde_json::Value, new: &serde_json::Value) -> bool {
    ["eo", "ta", "levels"]
        .iter()
        .any(|k| old.get(*k) != new.get(*k))
}

/// The levels that have a role of their own (`level_roles`): Trusted Admin, and Owner for the
/// server owner and every Extra Owner.
pub const ROLE_LEVELS: [&str; 2] = ["trusted_admin", "owner"];

/// The roles of the levels that `user` is on, with `owner` the server owner: the Trusted Admin
/// role for a Trusted Admin, the Owner role for the owner and an Extra Owner. Placeholders: $1
/// guild, $2 user, $3 Discord owner (mapped to the bot owner, [`bot_owner`]).
const LEVEL_ROLES_OF: &str = "SELECT l.role_id FROM level_roles l WHERE l.guild_id = $1 AND (
    (l.level = 'trusted_admin' AND EXISTS (SELECT 1 FROM trusted_admins t WHERE t.guild_id = $1 AND t.user_id = $2))
    OR (l.level = 'owner' AND ($2 = coalesce((SELECT o.user_id FROM rescue_owners o WHERE o.server_id = $1), $3)
      OR EXISTS (SELECT 1 FROM extra_owners e WHERE e.guild_id = $1 AND e.user_id = $2))))";

/// (level, role) of the guild's level roles.
pub async fn level_roles(pool: &sqlx::PgPool, guild_id: i64) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as("SELECT level, role_id FROM level_roles WHERE guild_id = $1 ORDER BY level")
        .bind(guild_id)
        .fetch_all(pool)
        .await
}

/// (user, role) for everyone a level role belongs to: Trusted Admins, Extra Owners and the bot
/// owner (`owner` is the Discord owner, mapped by [`bot_owner`]).
pub async fn level_members(
    pool: &sqlx::PgPool,
    guild_id: i64,
    owner: i64,
) -> sqlx::Result<Vec<(i64, i64)>> {
    sqlx::query_as(
        "SELECT t.user_id, l.role_id FROM level_roles l JOIN trusted_admins t ON t.guild_id = l.guild_id
         WHERE l.guild_id = $1 AND l.level = 'trusted_admin'
         UNION SELECT u.user_id, l.role_id FROM level_roles l
           JOIN (SELECT user_id FROM extra_owners WHERE guild_id = $1
             UNION SELECT coalesce((SELECT o.user_id FROM rescue_owners o WHERE o.server_id = $1), $2)) u ON true
         WHERE l.guild_id = $1 AND l.level = 'owner' ORDER BY 1, 2",
    )
    .bind(guild_id)
    .bind(owner)
    .fetch_all(pool)
    .await
}

/// Every role tied to one of the guild's permits or levels (`level_roles`); never handed out by
/// verification or sticky roles.
pub async fn permit_roles(pool: &sqlx::PgPool, guild_id: i64) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT r.role_id FROM permit_roles r JOIN permits p ON p.id = r.permit_id WHERE p.guild_id = $1
         UNION SELECT role_id FROM level_roles WHERE guild_id = $1",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
}

/// The permits whose role anyone may hand out: they give no moderation, dashboard,
/// lockdown, backup, revert or anti-nuke rights. Every other permit's role is guarded: handing it
/// out takes the role back and quarantines the actor unless they are Owner, Extra Owner or
/// Trusted Admin. A permit key not listed here (a new one too) is guarded.
pub const WEAK_PERMS: &[&str] = &[
    "cases",
    "mod_stats",
    "reports",
    "notes_view",
    "notes_manage",
    "tickets",
];
/// Whitelist flags of the same kind. Not `automod`: an automod-immune role lets raid accounts skip spam heat and slowmode (owner decision).
pub const WEAK_WHITELIST: &[&str] = &["words", "links", "reports"];

/// Roles of the guild's permits that hold `perm` (staff for tickets).
pub async fn roles_with(pool: &sqlx::PgPool, guild_id: i64, perm: &str) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT DISTINCT r.role_id FROM permit_roles r JOIN permits p ON p.id = r.permit_id
         WHERE p.guild_id = $1 AND $2 = ANY(p.perms)",
    )
    .bind(guild_id)
    .bind(perm)
    .fetch_all(pool)
    .await
}

/// The member's permit, as (rank, perms, whitelist, limits). Only one permit counts, never a
/// union of several; the bot and the dashboard both read it here. A permit belongs to a user
/// id (`permit_members`, owner decision 2026-11-15) and nothing else: holding a permit's role gives nothing
/// (owner decision 2026-11-20, the role path is gone, also before a guild's [`migrate_members`] ran).
/// A quarantined user holds none.
pub async fn top_permit(
    pool: &sqlx::PgPool,
    guild_id: i64,
    user_id: UserId,
) -> sqlx::Result<Option<(i32, Vec<String>, Vec<String>, serde_json::Value)>> {
    sqlx::query_as(
        "SELECT p.rank, p.perms, p.whitelist, p.limits FROM permits p
         WHERE p.guild_id = $1
           AND EXISTS (SELECT 1 FROM permit_members pm WHERE pm.permit_id = p.id AND pm.user_id = $2)
           -- A quarantine strips the roles and the permit with them: it still does.
           AND NOT EXISTS (SELECT 1 FROM quarantines q WHERE q.guild_id = $1 AND q.user_id = $2)
         ORDER BY p.rank DESC, p.id LIMIT 1",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// Whether the bot moved the guild's permit role holders into `permit_members` yet.
pub async fn members_migrated<'e>(
    db: impl sqlx::PgExecutor<'e>,
    guild_id: i64,
) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM permit_members_migrated WHERE guild_id = $1)")
        .bind(guild_id)
        .fetch_one(db)
        .await
}

/// Moves the guild's role-based permits to user ids, once (owner decision): each member in `holders`
/// (user id, role ids) gets the permit the role lookup gave them. Returns (user, permit name) for
/// the log; empty when the guild was moved before. `holders` must be the complete member list.
/// Takes the caller's transaction, so the config version of the move commits with it.
pub async fn migrate_members(
    tx: &mut sqlx::PgConnection,
    guild_id: i64,
    holders: &[(i64, Vec<i64>)],
) -> sqlx::Result<Vec<(i64, String)>> {
    let first: bool = sqlx::query_scalar(
        "INSERT INTO permit_members_migrated (guild_id) VALUES ($1) ON CONFLICT DO NOTHING RETURNING true",
    )
    .bind(guild_id)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(false);
    if !first {
        return Ok(vec![]);
    }
    // One read up front, so only members holding a permit role cost a query (100k-member servers).
    let permit_roles: std::collections::HashSet<i64> = sqlx::query_scalar(
        "SELECT r.role_id FROM permit_roles r JOIN permits p ON p.id = r.permit_id WHERE p.guild_id = $1",
    )
    .bind(guild_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let mut moved = vec![];
    for (user, roles) in holders
        .iter()
        .filter(|(_, r)| r.iter().any(|r| permit_roles.contains(r)))
    {
        let top: Option<(i64, String)> = sqlx::query_as(
            "SELECT p.id, p.name FROM permits p JOIN permit_roles r ON r.permit_id = p.id
             WHERE p.guild_id = $1 AND r.role_id = ANY($2) AND NOT EXISTS (
               SELECT 1 FROM unpermittable_roles u WHERE u.guild_id = $1 AND u.role_id = r.role_id)
             ORDER BY p.rank DESC, p.id LIMIT 1",
        )
        .bind(guild_id)
        .bind(roles)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id, name)) = top {
            let n = sqlx::query(
                "INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .bind(guild_id)
            .bind(user)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            // Already listed (in this or another permit): not moved by this run.
            if n > 0 {
                moved.push((*user, name));
            }
        }
    }
    Ok(moved)
}

/// The roles Brim gives `user`: those of the permit they are listed in, and the roles of the
/// levels they are on (`owner` is the server owner).
pub async fn member_roles(
    pool: &sqlx::PgPool,
    guild_id: i64,
    user_id: i64,
    owner: i64,
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT r.role_id FROM permit_members m JOIN permit_roles r ON r.permit_id = m.permit_id
         WHERE m.guild_id = $1 AND m.user_id = $2 UNION {LEVEL_ROLES_OF} ORDER BY 1"
    )))
    .bind(guild_id)
    .bind(user_id)
    .bind(owner)
    .fetch_all(pool)
    .await
}

/// The permit roles among `roles` that `user_id` is not entitled to, in a migrated guild: not
/// quarantined, not a role of their own permit, not one Discord hands out. One query, for every
/// member update.
pub async fn unentitled_roles(
    pool: &sqlx::PgPool,
    guild_id: i64,
    user_id: i64,
    owner: i64,
    roles: &[i64],
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT r.role_id FROM (SELECT guild_id, role_id FROM permit_roles
             UNION SELECT guild_id, role_id FROM level_roles) r
         WHERE r.guild_id = $1 AND r.role_id = ANY($4)
           AND EXISTS (SELECT 1 FROM permit_members_migrated g WHERE g.guild_id = $1)
           AND NOT EXISTS (SELECT 1 FROM quarantines q WHERE q.guild_id = $1 AND q.user_id = $2)
           AND NOT EXISTS (SELECT 1 FROM unpermittable_roles u WHERE u.guild_id = $1 AND u.role_id = r.role_id)
           AND r.role_id NOT IN (SELECT o.role_id FROM permit_members m
             JOIN permit_roles o ON o.permit_id = m.permit_id WHERE m.guild_id = $1 AND m.user_id = $2)
           AND r.role_id NOT IN ({LEVEL_ROLES_OF})
         ORDER BY 1"
    )))
    .bind(guild_id)
    .bind(user_id)
    .bind(owner)
    .bind(roles)
    .fetch_all(pool)
    .await
}

/// Every listed member of the guild's permits with the roles Brim gives them.
pub async fn members(
    pool: &sqlx::PgPool,
    guild_id: i64,
) -> sqlx::Result<Vec<(i64, String, Vec<i64>)>> {
    sqlx::query_as(
        "SELECT m.user_id, p.name, COALESCE(array_agg(r.role_id) FILTER (WHERE r.role_id IS NOT NULL), '{}')
         FROM permit_members m JOIN permits p ON p.id = m.permit_id
         LEFT JOIN permit_roles r ON r.permit_id = m.permit_id
         WHERE m.guild_id = $1 GROUP BY m.user_id, p.name ORDER BY 1",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
}

/// Takes a user who left out of every per-id trust list: their permit, Extra Owner and Trusted
/// Admin (owner decision 2026-11-15). Returns what they held, for the log, e.g. `["Trusted Admin", "permit Mods"]`.
pub async fn forget_member(
    c: &mut sqlx::PgConnection,
    guild_id: i64,
    user_id: i64,
) -> sqlx::Result<Vec<String>> {
    let mut held = vec![];
    for (table, label) in [
        ("extra_owners", "Extra Owner"),
        ("trusted_admins", "Trusted Admin"),
    ] {
        let gone = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE guild_id = $1 AND user_id = $2"
        )))
        .bind(guild_id)
        .bind(user_id)
        .execute(&mut *c)
        .await?;
        if gone.rows_affected() > 0 {
            held.push(label.to_string());
        }
    }
    let names: Vec<String> = sqlx::query_scalar(
        "DELETE FROM permit_members m USING permits p WHERE p.id = m.permit_id
         AND m.guild_id = $1 AND m.user_id = $2 RETURNING p.name",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_all(&mut *c)
    .await?;
    held.extend(names.into_iter().map(|n| format!("permit {n}")));
    Ok(held)
}

/// `unpermittable_roles.source` for linked, subscription and purchase roles (their tags).
pub const FROM_TAGS: &str = "tags";
/// `unpermittable_roles.source` for roles an Onboarding prompt gives.
pub const FROM_ONBOARDING: &str = "onboarding";

/// Replaces the guild's roles from `source` that Discord hands out by itself (owner decision): every
/// permit lookup ([`top_permit`]) skips them.
pub async fn set_unpermittable(
    pool: &sqlx::PgPool,
    guild_id: i64,
    source: &str,
    roles: &[i64],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "DELETE FROM unpermittable_roles WHERE guild_id = $1 AND source = $2 AND role_id <> ALL($3)",
    )
    .bind(guild_id)
    .bind(source)
    .bind(roles)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO unpermittable_roles (guild_id, role_id, source)
         SELECT $1, unnest($3::bigint[]), $2 ON CONFLICT DO NOTHING",
    )
    .bind(guild_id)
    .bind(source)
    .bind(roles)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Stored unpermittable roles that carry a permit and were not reported yet, marked reported now.
pub async fn unreported_unpermittable(
    pool: &sqlx::PgPool,
    guild_id: i64,
) -> sqlx::Result<Vec<i64>> {
    let mut ids: Vec<i64> = sqlx::query_scalar(
        "UPDATE unpermittable_roles u SET reported = true
         WHERE guild_id = $1 AND NOT reported AND EXISTS (
           SELECT 1 FROM permit_roles r JOIN permits p ON p.id = r.permit_id
           WHERE p.guild_id = $1 AND r.role_id = u.role_id)
         RETURNING role_id",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await?;
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// The roles an Onboarding prompt gives, from Discord's `GET /guilds/{id}/onboarding` JSON.
pub fn onboarding_roles(v: &serde_json::Value) -> Vec<i64> {
    let prompts = v["prompts"].as_array().into_iter().flatten();
    let options = prompts.filter_map(|p| p["options"].as_array()).flatten();
    let ids = options.filter_map(|o| o["role_ids"].as_array()).flatten();
    ids.filter_map(|id| id.as_str()?.parse().ok()).collect()
}

/// The account that holds Brim's Owner level, given the Discord owner: the account that
/// claimed the rescue key, else the Discord owner. The Discord owner then has no level of its own.
/// It moves with Discord ownership (a thief with the old account cannot end it by handing the server
/// to an alt); only a give-back, or Discord ownership landing on the holder, ends it (owner decision 2026-11-20).
pub async fn bot_owner(
    db: impl sqlx::PgExecutor<'_>,
    guild_id: i64,
    discord_owner: i64,
) -> sqlx::Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT user_id FROM rescue_owners WHERE server_id = $1")
            .bind(guild_id)
            .fetch_optional(db)
            .await?
            .unwrap_or(discord_owner),
    )
}

/// Resolves a member's level from the DB. A `bot` account is never Extra Owner or Trusted Admin, even
/// if listed: only users (owner decision, Q4 11-14); a bot permit (the bot's id in `permit_members`) still counts. The guild owner is never a bot.
pub async fn resolve(
    pool: &sqlx::PgPool,
    guild_id: i64,
    owner_id: UserId,
    user_id: UserId,
    bot: bool,
) -> anyhow::Result<Level> {
    if user_id == bot_owner(pool, guild_id, owner_id).await? {
        return Ok(Level::Owner);
    }
    if bot {
        return Ok(custom(top_permit(pool, guild_id, user_id).await?));
    }
    let (eo,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM extra_owners WHERE guild_id=$1 AND user_id=$2)",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if eo {
        return Ok(Level::ExtraOwner);
    }
    let (ta,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM trusted_admins WHERE guild_id=$1 AND user_id=$2)",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if ta {
        return Ok(Level::TrustedAdmin);
    }
    Ok(custom(top_permit(pool, guild_id, user_id).await?))
}

/// A [`top_permit`] row as its [`Level`].
fn custom(row: Option<(i32, Vec<String>, Vec<String>, serde_json::Value)>) -> Level {
    row.map_or(Level::None, |(rank, perms, whitelist, limits)| {
        // Malformed entries are dropped one by one so a bad action does not void the rest.
        let limits = limits
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), serde_json::from_value(v.clone()).ok()?)))
            .collect();
        Level::Custom {
            rank,
            perms,
            whitelist,
            limits,
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn catalog_keys_are_unique_per_kind_and_known() {
        let mut seen = std::collections::HashSet::new();
        for d in CATALOG.iter().flat_map(|s| s.items) {
            assert!(seen.insert((d.key, d.flag)), "duplicate {}", d.key);
            assert_eq!(known(d.key, d.flag), d.key != "owner");
        }
        assert!(known("antinuke", true) && !known("made_up", false) && !known("ban", true));
    }

    #[test]
    fn only_filter_flags_are_weak() {
        let g = |p: &[&str], w: &[&str]| guarded(p, w);
        assert!(g(&["dashboard_edit"], &[]) && g(&["owner"], &[]));
        for f in [
            "all",
            "antinuke",
            "quarantine",
            "wl_role_delete",
            "automod",
            "a_flag_from_later",
        ] {
            assert!(g(&[], &[f]), "{f}");
        }
        assert!(!g(&["ban", "warn"], &["words", "links", "reports"]));
        // Every catalog flag that is not listed weak is guarded.
        for d in CATALOG.iter().flat_map(|s| s.items).filter(|d| d.flag) {
            assert_eq!(
                g(&[], &[d.key]),
                !WEAK_WHITELIST.contains(&d.key),
                "{}",
                d.key
            );
        }
    }

    #[test]
    fn per_action_whitelist_is_narrower_than_antinuke() {
        let w = |f: &[&str]| Level::Custom {
            rank: 1,
            perms: vec![],
            whitelist: f.iter().map(|x| x.to_string()).collect(),
            limits: Default::default(),
        };
        assert!(w(&["wl_kick_ban"]).whitelisted("wl_kick_ban"));
        assert!(!w(&["wl_kick_ban"]).whitelisted("wl_role_delete"));
        assert!(w(&["antinuke"]).whitelisted("wl_role_delete"));
        let locker = Level::Custom {
            rank: 1,
            perms: vec!["lock_joins".into()],
            whitelist: vec![],
            limits: Default::default(),
        };
        assert!(locker.has("lockdown"));
    }

    #[test]
    fn older_permits_imply_the_new_commands() {
        let permit = |perms: &[&str]| Level::Custom {
            rank: 1,
            perms: perms.iter().map(|p| p.to_string()).collect(),
            whitelist: vec![],
            limits: Default::default(),
        };
        assert!(permit(&["timeout"]).has("purge"));
        assert!(permit(&["ban"]).has("quarantine"));
        assert!(!permit(&["warn"]).has("quarantine"));
        assert!(!permit(&["warn"]).has("purge"));
        assert!(!permit(&[]).has("cases"));
    }

    use super::*;

    fn custom(rank: i32, perms: &[&str]) -> Level {
        Level::Custom {
            rank,
            perms: perms.iter().map(|s| s.to_string()).collect(),
            whitelist: vec![],
            limits: HashMap::new(),
        }
    }

    #[test]
    fn permit_checks() {
        let mod_ = custom(1, &["ban"]);
        assert_eq!(
            check(&Level::None, "ban", 1, &Level::None, 2, false),
            Err(Denied::NoPermit)
        );
        assert_eq!(
            check(&mod_, "kick", 1, &Level::None, 2, false),
            Err(Denied::NoPermit)
        );
        assert_eq!(check(&mod_, "ban", 1, &Level::None, 2, false), Ok(()));
        assert_eq!(
            check(&mod_, "ban", 1, &Level::TrustedAdmin, 2, false),
            Err(Denied::TargetProtected)
        );
        assert_eq!(
            check(&mod_, "ban", 1, &custom(1, &[]), 2, false),
            Err(Denied::TargetProtected)
        );
        assert_eq!(
            check(&custom(2, &["ban"]), "ban", 1, &custom(1, &[]), 2, false),
            Err(Denied::TargetProtected)
        );
        // Quarantine of a permit holder only from a higher rank (owner decision 2026-11-15).
        let q = custom(1, &["quarantine"]);
        assert_eq!(
            check(&q, "quarantine", 1, &custom(1, &[]), 2, false),
            Err(Denied::TargetProtected)
        );
        assert_eq!(
            check(&q, "quarantine", 1, &custom(2, &[]), 2, false),
            Err(Denied::TargetProtected)
        );
        assert_eq!(
            check(
                &custom(2, &["quarantine"]),
                "quarantine",
                1,
                &custom(1, &[]),
                2,
                false
            ),
            Ok(())
        );
        assert_eq!(check(&q, "quarantine", 1, &Level::None, 2, false), Ok(()));
        assert_eq!(
            check(&q, "quarantine", 1, &Level::TrustedAdmin, 2, false),
            Err(Denied::TargetProtected)
        );
        assert_eq!(
            check(&mod_, "ban", 1, &Level::None, 2, true),
            Err(Denied::TargetAboveBot)
        );
        assert_eq!(
            check(&Level::Owner, "ban", 1, &Level::None, 1, false),
            Err(Denied::SelfTarget)
        );
    }

    /// Owner > Extra Owner > Trusted Admin > permit by rank > none; equal is not outranked.
    #[test]
    fn levels_outrank_only_lower_ones() {
        let order = [
            Level::None,
            custom(1, &[]),
            custom(2, &[]),
            Level::TrustedAdmin,
            Level::ExtraOwner,
            Level::Owner,
        ];
        for (i, a) in order.iter().enumerate() {
            for (j, b) in order.iter().enumerate() {
                assert_eq!(a.outranks(b), i > j, "{a:?} vs {b:?}");
            }
        }
        assert!(!custom(2, &["ban"]).outranks(&custom(2, &[])), "equal rank");
    }

    #[test]
    fn rate_limit_windows() {
        let mut rl = RateLimiter::default();
        let lim = Limit {
            per_min: 2,
            per_hour: 3,
        };
        let t0 = Instant::now();
        assert!(rl.allow(7, 1, "ban", lim, t0));
        assert!(rl.allow(7, 1, "ban", lim, t0));
        assert!(!rl.allow(7, 1, "ban", lim, t0)); // 3rd within the minute
        assert!(rl.allow(7, 2, "ban", lim, t0)); // other actor unaffected
        assert!(rl.allow(8, 1, "ban", lim, t0)); // the same mod in another server unaffected
        let later = t0 + Duration::from_secs(61);
        assert!(!rl.allow(7, 1, "ban", lim, later)); // 4th in the hour breaches per_hour
    }

    #[test]
    fn whitelist_flags() {
        let w = |f: &[&str]| Level::Custom {
            rank: 1,
            perms: vec![],
            limits: HashMap::new(),
            whitelist: f.iter().map(|s| s.to_string()).collect(),
        };
        assert!(Level::Owner.whitelisted("antinuke"));
        assert!(!Level::None.whitelisted("antinuke"));
        assert!(w(&["antinuke"]).whitelisted("antinuke"));
        assert!(!w(&["antinuke"]).whitelisted("automod"));
        assert!(w(&["all"]).whitelisted("quarantine"));
    }

    #[test]
    fn command_allowed_applies_limits() {
        let lvl = Level::Custom {
            rank: 1,
            perms: vec![],
            whitelist: vec![],
            limits: HashMap::from([(
                "ban".into(),
                Limit {
                    per_min: 1,
                    per_hour: 5,
                },
            )]),
        };
        let mut rl = RateLimiter::default();
        let t = Instant::now();
        assert!(command_allowed(&mut rl, 7, 1, &lvl, "ban", t));
        assert!(!command_allowed(&mut rl, 7, 1, &lvl, "ban", t));
        assert!(command_allowed(&mut rl, 7, 1, &lvl, "kick", t)); // no limit configured
        assert!(command_allowed(
            &mut rl,
            7,
            1,
            &Level::TrustedAdmin,
            "ban",
            t
        ));
        // Without its own limit, a permit takes the fallback (anti-nuke's); immune never.
        let nuke = Limit {
            per_min: 4,
            per_hour: 14,
        };
        assert_eq!(command_limit(&lvl, "kick", Some(nuke)), Some(nuke));
        assert_eq!(command_limit(&lvl, "ban", Some(nuke)).unwrap().per_min, 1);
        assert_eq!(command_limit(&Level::Owner, "ban", Some(nuke)), None);
        // A use from before a restart still counts.
        let mut rl = RateLimiter::default();
        rl.seed(7, 1, "ban", t);
        assert!(!command_allowed(&mut rl, 7, 1, &lvl, "ban", t));
    }

    #[tokio::test]
    async fn resolve_reads_whitelist_and_limits() {
        let Ok(url) = crate::db::test_url() else {
            return;
        };
        let pool = crate::db::connect(&url).await.unwrap();
        let gid = 8_000_000_000 + std::process::id() as i64;
        let (pid,): (i64,) = sqlx::query_as(
            "INSERT INTO permits (guild_id, name, rank, perms, whitelist, limits)
             VALUES ($1,'t',3,'{ban}','{antinuke}','{\"ban\":{\"per_min\":5,\"per_hour\":20},\"bad\":1}') RETURNING id",
        )
        .bind(gid)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO permit_members (permit_id, guild_id, user_id) VALUES ($1,$2,2)")
            .bind(pid)
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
        let got = resolve(&pool, gid, 1, 2, false).await;
        sqlx::query("DELETE FROM permits WHERE guild_id=$1")
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
        let Level::Custom {
            rank,
            whitelist,
            limits,
            ..
        } = got.unwrap()
        else {
            panic!("not custom")
        };
        assert_eq!((rank, whitelist), (3, vec!["antinuke".to_string()]));
        assert_eq!(limits.len(), 1);
        assert_eq!(limits["ban"].per_hour, 20);
    }
}

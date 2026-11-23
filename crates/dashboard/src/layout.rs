//! Layout Guard (plan server-setup-plan-2026-11-18): the saved server layout and the list of
//! differences between it and Discord; a Channel Setup push saves its changes as a new layout
//! version, and History rolls the layout back. Nothing here changes Discord or puts anything
//! back: a roll back changes only what the layout says is correct.

use crate::{
    access::Level,
    api::{log_by, require, Res},
    auth::Sess,
    channels::{desired, normalized, Ctx, Draft, World},
    ops, store, ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use brim_antinuke::{
    map_overwrites, plan, same_overwrites,
    security::{checks, Severity},
    ChannelImg, Image, RoleImg, Settings, CATEGORY, DANGEROUS_PERMS,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const AGAIN: ApiError = ApiError::Forbidden(
    "a server layout is already saved; only the Owner or an Extra Owner can save it again",
);

/// Versions kept per server (plan 2.2).
const KEEP: i32 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/guilds/{guild}/layout", get(get_layout))
        .route("/api/guilds/{guild}/layout/save", post(save))
        .route("/api/guilds/{guild}/layout/versions", get(versions))
        .route(
            "/api/guilds/{guild}/layout/{version}/rollback",
            post(rollback),
        )
}

/// One way Discord differs from the saved layout.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Difference {
    /// "role", "category", "channel" or "server".
    pub what: &'static str,
    pub name: String,
    /// What differs, in plain words.
    pub change: String,
    /// The Discord id the audit log names: the live object, or the saved one when it is gone.
    #[serde(serialize_with = "as_text")]
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

fn as_text<S: serde::Serializer>(n: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&n.to_string())
}

fn diff(what: &'static str, name: &str, change: String, id: u64) -> Difference {
    Difference {
        what,
        name: name.into(),
        change,
        id,
        by: None,
        at: None,
    }
}

const GONE: &str = "Deleted in Discord";
const NEW: &str = "Made in Discord, not in the layout";
const NEW_BOT: &str =
    "Bot role with dangerous Discord permissions, added after the layout was saved";

fn changed(fields: &[&str]) -> String {
    format!("Changed in Discord: {}", fields.join(", "))
}

/// Ids whose relative order differs between `saved` and `live` (both lists of the same ids, each
/// in its own order): everything outside the longest run kept in the same order, so one moved
/// item is one difference, not every item it passed.
fn moved(saved: &[u64], live: &[u64]) -> HashSet<u64> {
    let at: HashMap<u64, usize> = saved.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let seq: Vec<usize> = live.iter().filter_map(|id| at.get(id).copied()).collect();
    // Longest increasing subsequence, O(n log n), with back links to rebuild it.
    let (mut tails, mut prev): (Vec<usize>, Vec<Option<usize>>) = (vec![], vec![None; seq.len()]);
    for i in 0..seq.len() {
        let k = tails.partition_point(|&t| seq[t] < seq[i]);
        prev[i] = k.checked_sub(1).map(|j| tails[j]);
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut keep = HashSet::new();
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        keep.insert(saved[seq[i]]);
        cur = prev[i];
    }
    saved
        .iter()
        .filter(|id| !keep.contains(id) && live.contains(id))
        .copied()
        .collect()
}

/// Every difference between the saved layout and the live server. Managed (bot and integration)
/// roles come and go with their bot (plan 4.2), so only their Discord permissions count, and a new
/// one only when it has dangerous Discord permissions.
pub fn differences(saved: &Image, live: &Image) -> Vec<Difference> {
    // `plan` matches saved objects to live ones (by id, then name) exactly as a restore would.
    let mut map = plan(saved, live, &HashSet::new(), false).id_map;
    // `plan` leaves managed roles out; without them every overwrite for a bot role reads as gone.
    for r in saved.roles.iter().filter(|r| r.managed) {
        if live.roles.iter().any(|l| l.id == r.id) {
            map.insert(r.id, r.id);
        }
    }
    let used: HashSet<u64> = map.values().copied().collect();
    let mut out = vec![];

    let live_role = |id: u64| live.roles.iter().find(|l| l.id == id);
    let pairs: Vec<(&RoleImg, &RoleImg)> = saved
        .roles
        .iter()
        .filter(|r| !r.managed)
        .filter_map(|r| Some((r, map.get(&r.id).and_then(|l| live_role(*l))?)))
        .filter(|(_, l)| !l.managed)
        .collect();
    let order = |key: fn(&(&RoleImg, &RoleImg)) -> (i64, u64)| {
        let mut v: Vec<_> = pairs.iter().filter(|(r, _)| r.id != saved.guild).collect();
        v.sort_by_key(|p| key(p));
        v.into_iter().map(|(_, l)| l.id).collect::<Vec<_>>()
    };
    let roles_moved = moved(
        &order(|(r, _)| (-r.position, r.id)),
        &order(|(_, l)| (-l.position, l.id)),
    );
    for r in &saved.roles {
        let Some(l) = map.get(&r.id).and_then(|l| live_role(*l)) else {
            if !r.managed {
                out.push(diff("role", &r.name, GONE.into(), r.id));
            }
            continue;
        };
        if r.managed {
            if r.permissions != l.permissions {
                out.push(diff(
                    "role",
                    &l.name,
                    changed(&["Discord permissions"]),
                    l.id,
                ));
            }
            continue;
        }
        let mut f = vec![];
        if r.name != l.name {
            f.push("name");
        }
        if r.color != l.color {
            f.push("colour");
        }
        if r.permissions != l.permissions {
            f.push("Discord permissions");
        }
        if r.hoist != l.hoist {
            f.push("shown separately");
        }
        if r.mentionable != l.mentionable {
            f.push("mentionable");
        }
        if roles_moved.contains(&l.id) {
            f.push("place in the role order");
        }
        if !f.is_empty() {
            out.push(diff("role", &l.name, changed(&f), l.id));
        }
    }
    for l in &live.roles {
        if l.id == live.guild || used.contains(&l.id) {
            continue;
        }
        if !l.managed {
            out.push(diff("role", &l.name, NEW.into(), l.id));
        } else if l.permissions & DANGEROUS_PERMS != 0 {
            out.push(diff("role", &l.name, NEW_BOT.into(), l.id));
        }
    }

    let what = |c: &ChannelImg| {
        if c.kind == CATEGORY {
            "category"
        } else {
            "channel"
        }
    };
    let live_chan = |id: u64| live.channels.iter().find(|l| l.id == id);
    // Order counts among channels that are in the same category in both, per category.
    let mut groups: HashMap<Option<u64>, (Vec<&ChannelImg>, Vec<&ChannelImg>)> = HashMap::new();
    for c in &saved.channels {
        let Some(l) = map.get(&c.id).and_then(|l| live_chan(*l)) else {
            out.push(diff(what(c), &c.name, GONE.into(), c.id));
            continue;
        };
        let parent = c.parent.map(|p| map.get(&p).copied().unwrap_or(p));
        let mut f = vec![];
        if c.name != l.name {
            f.push("name");
        }
        if c.topic != l.topic {
            f.push("topic");
        }
        if c.nsfw != l.nsfw {
            f.push("age-restricted");
        }
        if c.kind != l.kind {
            f.push("type");
        }
        if parent != l.parent {
            f.push("category");
        } else {
            let g = groups.entry(l.parent).or_default();
            g.0.push(c);
            g.1.push(l);
        }
        let ows = map_overwrites(&c.overwrites, &map);
        if ows.len() != c.overwrites.len() || !same_overwrites(&ows, &l.overwrites) {
            f.push("who can see or write");
        }
        if !f.is_empty() {
            out.push(diff(what(c), &l.name, changed(&f), l.id));
        }
    }
    let mut chans_moved = HashSet::new();
    for (s, l) in groups.values_mut() {
        let key = |c: &&ChannelImg| {
            (
                c.kind == CATEGORY,
                matches!(c.kind, 2 | 13),
                c.position,
                c.id,
            )
        };
        s.sort_by_key(key);
        l.sort_by_key(key);
        let ids = |v: &[&ChannelImg], live_ids: bool| -> Vec<u64> {
            v.iter()
                .map(|c| if live_ids { c.id } else { map[&c.id] })
                .collect()
        };
        chans_moved.extend(moved(&ids(s, false), &ids(l, true)));
    }
    for id in chans_moved {
        let l = live_chan(id).expect("grouped from live");
        // Joined to the row the channel already has, so one channel is one row.
        match out.iter_mut().find(|d| d.id == id && d.what != "role") {
            Some(d) => d.change.push_str(", place in the list"),
            None => out.push(diff(what(l), &l.name, changed(&["place in the list"]), id)),
        }
    }
    for l in &live.channels {
        if !used.contains(&l.id) {
            out.push(diff(what(l), &l.name, NEW.into(), l.id));
        }
    }

    if let (Some(s), Some(l)) = (&saved.settings, &live.settings) {
        let ch = |c: Option<u64>| c.map(|c| map.get(&c).copied().unwrap_or(c));
        let f: Vec<&str> = [
            (s.name != l.name, "server name"),
            (s.icon != l.icon, "icon"),
            (s.verification != l.verification, "verification level"),
            (s.notifications != l.notifications, "default notifications"),
            (s.content_filter != l.content_filter, "content filter"),
            (ch(s.afk_channel) != l.afk_channel, "AFK channel"),
            (s.afk_timeout != l.afk_timeout, "AFK timeout"),
            (
                ch(s.system_channel) != l.system_channel,
                "system messages channel",
            ),
            (s.locale != l.locale, "language"),
        ]
        .into_iter()
        .filter_map(|(d, name)| d.then_some(name))
        .collect();
        if !f.is_empty() {
            out.push(diff("server", &l.name, changed(&f), live.guild));
        }
    }
    out
}

/// The server settings a layout keeps, from Discord's guild JSON.
pub(crate) fn settings_of(g: &Value) -> Settings {
    let id = |k: &str| g[k].as_str().and_then(|x| x.parse().ok());
    let n = |k: &str| g[k].as_u64().unwrap_or(0);
    Settings {
        name: g["name"].as_str().unwrap_or_default().into(),
        verification: n("verification_level") as u8,
        notifications: n("default_message_notifications") as u8,
        content_filter: n("explicit_content_filter") as u8,
        afk_channel: id("afk_channel_id"),
        afk_timeout: n("afk_timeout"),
        system_channel: id("system_channel_id"),
        locale: g["preferred_locale"].as_str().unwrap_or_default().into(),
        icon: g["icon"].as_str().map(String::from),
    }
}

/// The ticket channels among `ids`, from Brim's own ticket records (never from a name or a
/// category, so nobody can hide a channel from the guard): `open` are the channels of tickets
/// not closed yet, which are temporary and never part of a layout; `all` also holds the channels
/// of closed tickets, so a layout saved with a ticket channel in it does not miss it once it closes.
/// One query by the channel ids the page already has, however many tickets are open.
pub(crate) async fn ticket_channels(
    db: &sqlx::PgPool,
    guild: i64,
    ids: impl Iterator<Item = u64>,
) -> sqlx::Result<Tickets> {
    let ids: Vec<i64> = ids.map(|i| i as i64).collect();
    let rows: Vec<(i64, bool)> = sqlx::query_as(
        "SELECT channel_id, status <> 'closed' FROM tickets
         WHERE guild_id = $1 AND channel_id = ANY($2)",
    )
    .bind(guild)
    .bind(&ids)
    .fetch_all(db)
    .await?;
    Ok(Tickets {
        open: rows.iter().filter(|r| r.1).map(|r| r.0 as u64).collect(),
        all: rows.iter().map(|r| r.0 as u64).collect(),
    })
}

/// See [`ticket_channels`].
#[derive(Default)]
pub(crate) struct Tickets {
    pub open: HashSet<u64>,
    pub all: HashSet<u64>,
}

/// `img` without the channels in `ids`.
pub(crate) fn without(mut img: Image, ids: &HashSet<u64>) -> Image {
    img.channels.retain(|c| !ids.contains(&c.id));
    img
}

/// The live server as a layout image: ranked roles, index channel positions, server settings.
fn image_of(l: &ops::Live) -> Image {
    Image {
        name: l.guild["name"].as_str().unwrap_or_default().into(),
        settings: Some(settings_of(&l.guild)),
        channels: normalized(l.image.channels.clone()),
        webhooks: None,
        ..l.image.clone()
    }
}

/// Discord snowflake to its creation time in Unix milliseconds.
fn snowflake_ms(id: u64) -> i64 {
    ((id >> 22) + 1_420_070_400_000) as i64
}

/// Adds who made each difference and when, from the newest audit log entry for that object made
/// after the layout was saved. An entry from before the save is not the cause, so it is not shown.
/// shortcut: a changed object names its newest editor, who may not be the one who made the
/// difference; L6 must not judge anyone by `by`, it reads the audit entry of each event itself.
fn attribute(out: &mut [Difference], audit: &Value, since_ms: i64, bot: u64) {
    let names: HashMap<&str, String> = audit["users"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| {
            let name = u["global_name"].as_str().or(u["username"].as_str())?;
            Some((u["id"].as_str()?, name.to_string()))
        })
        .collect();
    let entries = audit["audit_log_entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for d in out {
        let target = d.id.to_string();
        // The list is newest first.
        let hit = entries.iter().find(|e| {
            let at = e["id"]
                .as_str()
                .and_then(|i| i.parse().ok())
                .map(snowflake_ms);
            // A made or deleted object is named after the entry that made or deleted it.
            let kind = e["action_type"].as_u64().unwrap_or(0);
            let fits = match d.change.as_str() {
                NEW | NEW_BOT => matches!(kind, 10 | 28 | 30),
                GONE => matches!(kind, 12 | 32),
                _ => true,
            };
            e["target_id"].as_str() == Some(&target) && fits && at.is_some_and(|t| t > since_ms)
        });
        let Some(e) = hit else { continue };
        let user = e["user_id"].as_str().unwrap_or_default();
        d.by = Some(if user == bot.to_string() {
            "Brim".into()
        } else {
            match names.get(user) {
                Some(n) => format!("{n} ({user})"),
                None => user.to_string(),
            }
        });
        let ms = e["id"]
            .as_str()
            .and_then(|i| i.parse().ok())
            .map(snowflake_ms);
        d.at = ms
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string());
    }
}

type Row = (
    i32,
    sqlx::types::Json<Image>,
    chrono::DateTime<chrono::Utc>,
    i64,
);

pub(crate) async fn newest(st: &AppState, guild: i64) -> sqlx::Result<Option<Row>> {
    sqlx::query_as(
        "SELECT version, image, created_at, created_by FROM server_layouts
         WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(guild)
    .fetch_optional(&st.db)
    .await
}

async fn view(st: &AppState, guild: i64, level: Level) -> Res {
    let can_save_again = level >= Level::Owner;
    let Some((version, sqlx::types::Json(saved), at, by)) = newest(st, guild).await? else {
        return Ok(Json(
            json!({"layout": null, "can_save_again": can_save_again}),
        ));
    };
    let (_, config) = store::current(&st.db, guild).await?;
    let l = ops::live(st, guild, &config).await?;
    let live = image_of(&l);
    let ids = saved.channels.iter().chain(&live.channels).map(|c| c.id);
    let t = ticket_channels(&st.db, guild, ids).await?;
    let mut out = differences(&without(saved, &t.all), &without(live, &t.open));
    if !out.is_empty() {
        // shortcut: the newest 100 entries only; a busy server's older changes show no name.
        // Needs View Audit Log; without it the list still shows, with no names.
        let path = format!("/guilds/{guild}/audit-logs?limit=100");
        match st.discord.call(Method::GET, &path, None).await {
            Ok(a) => attribute(&mut out, &a, at.timestamp_millis(), l.setup.bot),
            Err(e) => tracing::warn!("layout: audit log read failed: {e:#}"),
        }
    }
    Ok(Json(json!({
        "layout": {
            "version": version,
            "saved_at": at.format("%Y-%m-%d %H:%M UTC").to_string(),
            "saved_by": by.to_string(),
        },
        "differences": out,
        "can_save_again": can_save_again,
    })))
}

async fn get_layout(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    let level = require(&st, &s, guild, Level::Admin, false).await?;
    view(&st, guild, level).await
}

/// "Save current server as layout". The first save needs a TA (Trusted Admin); saving again over
/// an existing layout adopts whatever Discord holds now, so it needs the Owner or an EO (Extra
/// Owner). Both need 2FA. A server with a serious security problem is not saved as correct.
async fn save(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    let level = require(&st, &s, guild, Level::Admin, true).await?;
    // A clear answer before any Discord read; the INSERT below is the real guard.
    if level < Level::Owner && newest(&st, guild).await?.is_some() {
        return Err(AGAIN);
    }
    let (_, config) = store::current(&st.db, guild).await?;
    let l = ops::live(&st, guild, &config).await?;
    let blocking = blocking(
        &st,
        guild,
        &config,
        &l,
        &l.safety,
        l.bot_pos,
        &l.image.roles,
        &l.image.channels,
    )
    .await?;
    if !blocking.is_empty() {
        return Ok(Json(json!({"saved": false, "blocking": blocking})));
    }
    let image = image_of(&l);
    let t = ticket_channels(&st.db, guild, image.channels.iter().map(|c| c.id)).await?;
    let image = without(image, &t.open);
    // Version and the Owner rule in one statement, so two saves cannot both be "the first".
    let version: Option<i32> = sqlx::query_scalar(
        "INSERT INTO server_layouts (guild_id, version, image, source, created_by)
         SELECT $1, coalesce(max(version), 0) + 1, $2, 'import', $3 FROM server_layouts
         WHERE guild_id = $1 HAVING $4 OR count(*) = 0
         RETURNING version",
    )
    .bind(guild)
    .bind(sqlx::types::Json(image))
    .bind(s.user_id)
    .bind(level >= Level::Owner)
    .fetch_optional(&st.db)
    .await?;
    let Some(version) = version else {
        return Err(AGAIN);
    };
    prune(&st, guild, version).await?;
    log_by(
        &st,
        &s,
        guild,
        &config,
        None,
        &format!("saved the current server as server layout v{version} (Layout Guard)."),
    )
    .await;
    // Just saved from this read, so nothing differs: no second read of Discord.
    Ok(Json(json!({
        "saved": true,
        "layout": {"version": version, "saved_at": chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string(), "saved_by": s.user_id.to_string()},
        "differences": [],
        "can_save_again": level >= Level::Owner,
    })))
}

/// The serious problems that stop roles and channels from being saved as the correct layout, read
/// with the live server's settings and staff roles.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn blocking(
    st: &AppState,
    guild: i64,
    config: &Value,
    l: &ops::Live,
    safety: &brim_antinuke::security::GuildSafety,
    bot_pos: i64,
    roles: &[RoleImg],
    channels: &[ChannelImg],
) -> Result<Vec<brim_antinuke::security::Check>, ApiError> {
    let staff: Vec<u64> = brim_core::permits::all_staff_roles(&st.db, guild)
        .await?
        .into_iter()
        .map(|r| r as u64)
        .collect();
    let mut out: Vec<_> = checks(safety, roles, channels, bot_pos, &l.setup)
        .into_iter()
        .filter(|c| c.severity == Severity::Bad)
        .collect();
    // A staff or log channel members can see is the first thing a guard must not take as correct.
    out.extend(crate::channel_access::staff_access_check(
        channels,
        &crate::channel_access::live_staff(&staff, roles),
        &ops::log_channel_ids(config),
    ));
    Ok(out)
}

/// The live server's safety numbers with the verification level and content filter of `s`.
pub(crate) fn safety_with(
    l: &ops::Live,
    s: Option<&Settings>,
) -> brim_antinuke::security::GuildSafety {
    brim_antinuke::security::GuildSafety {
        id: l.safety.id,
        mfa: l.safety.mfa,
        notifications: l.safety.notifications,
        verification: s.map_or(l.safety.verification, |s| s.verification),
        content_filter: s.map_or(l.safety.content_filter, |s| s.content_filter),
    }
}

/// Keeps the newest `KEEP` versions.
async fn prune(st: &AppState, guild: i64, version: i32) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM server_layouts WHERE guild_id = $1 AND version <= $2")
        .bind(guild)
        .bind(version - KEEP)
        .execute(&st.db)
        .await
        .map(drop)
}

/// Saves `image` as the next version, only while `expected` is still the newest one: a change
/// made on top of an older version must not hide a newer one. `None` when it was not the newest.
async fn record(
    st: &AppState,
    guild: i64,
    image: &Image,
    source: &str,
    user: i64,
    note: &str,
    expected: i32,
) -> sqlx::Result<Option<i32>> {
    let note: String = note.chars().take(200).collect();
    let version: Option<i32> = sqlx::query_scalar(
        "INSERT INTO server_layouts (guild_id, version, image, source, created_by, note)
         SELECT $1, max(version) + 1, $2, $3, $4, $5 FROM server_layouts
         WHERE guild_id = $1 HAVING max(version) = $6
         ON CONFLICT DO NOTHING RETURNING version",
    )
    .bind(guild)
    .bind(sqlx::types::Json(image))
    .bind(source)
    .bind(user)
    .bind(note)
    .bind(expected)
    .fetch_optional(&st.db)
    .await?;
    if let Some(v) = version {
        prune(st, guild, v).await?;
    }
    Ok(version)
}

/// The saved layout with a pushed draft on top: the layout's own channels (moved onto the live ids
/// `plan` matches them to) take exactly the draft's edits, and the channels the push made take
/// their new ids. Discord's own differences on other channels stay differences: nothing made in
/// Discord is adopted. A live channel the draft edits that the layout lacks (made by a push that
/// stopped halfway, or in Discord) is taken in with its category, since the push was reviewed with
/// it; the caller checks the result with [`blocking`]. `Ok(None)` when the channels do not change,
/// an error when a made channel has no id.
pub(crate) fn with_draft(
    saved: &Image,
    live: &Image,
    draft: &Draft,
    ctx: &Ctx,
    made: &HashMap<String, String>,
) -> Result<Option<Image>, &'static str> {
    let map = plan(saved, live, &HashSet::new(), false).id_map;
    let own: HashSet<u64> = saved.channels.iter().map(|c| c.id).collect();
    let m = |id: u64| match own.contains(&id) {
        true => map.get(&id).copied().unwrap_or(id),
        false => id,
    };
    let mut base: Vec<ChannelImg> = saved
        .channels
        .iter()
        .map(|c| ChannelImg {
            id: m(c.id),
            parent: c.parent.map(m),
            ..c.clone()
        })
        .collect();
    let before = normalized(base.clone());
    let mut want: Vec<u64> = draft
        .items
        .iter()
        .flat_map(|(k, it)| {
            [
                Some(k.as_str()),
                it.parent.as_ref().and_then(|p| p.as_deref()),
            ]
        })
        .flatten()
        .filter_map(|k| k.parse().ok())
        .collect();
    while let Some(id) = want.pop() {
        if base.iter().any(|c| c.id == id) {
            continue;
        }
        if let Some(c) = live.channels.iter().find(|c| c.id == id) {
            want.extend(c.parent);
            base.push(c.clone());
        }
    }
    let nodes = desired(&normalized(base), draft, ctx);
    let stand_in: HashMap<u64, u64> = nodes
        .iter()
        .filter(|n| n.new && !n.deleted)
        .map(|n| Some((n.img.id, made.get(&n.id)?.parse().ok()?)))
        .collect::<Option<_>>()
        .ok_or("a channel the push made has no id")?;
    let real = |id: u64| stand_in.get(&id).copied().unwrap_or(id);
    let roles = roles_with(saved, live, &map, draft, ctx, made)?;
    let gone: HashSet<u64> = saved
        .roles
        .iter()
        .map(|r| map.get(&r.id).copied().unwrap_or(r.id))
        .filter(|id| !roles.iter().any(|r| r.id == *id))
        .collect();
    let mut channels = normalized(
        nodes
            .into_iter()
            .filter(|n| !n.deleted)
            .map(|n| ChannelImg {
                id: real(n.img.id),
                parent: n.img.parent.map(real),
                ..n.img
            })
            .collect(),
    );
    // Discord drops the overwrites of a deleted role.
    for c in &mut channels {
        c.overwrites
            .retain(|o| !(o.is_role && gone.contains(&o.target)));
    }
    // A channel the draft deletes is no longer the AFK or system messages channel.
    let kept = |id: Option<u64>| id.map(m).filter(|i| channels.iter().any(|c| c.id == *i));
    let settings = saved.settings.clone().map(|mut s| {
        s.afk_channel = kept(s.afk_channel);
        s.system_channel = kept(s.system_channel);
        if let Some(d) = &draft.server {
            s = crate::roles::want_settings(&s, d);
            if let Some(h) = made.get(crate::channels::ICON) {
                s.icon = (!h.is_empty()).then(|| h.clone());
            }
        }
        s
    });
    if channels == before && roles == saved.roles && settings == saved.settings {
        return Ok(None);
    }
    Ok(Some(Image {
        channels,
        roles,
        settings,
        ..saved.clone()
    }))
}

/// The saved roles with the pushed role edits, new roles and order on top. Roles the draft does not
/// name are not taken in from Discord. Without role edits the saved roles stay exactly as they are.
fn roles_with(
    saved: &Image,
    live: &Image,
    map: &HashMap<u64, u64>,
    draft: &Draft,
    ctx: &Ctx,
    made: &HashMap<String, String>,
) -> Result<Vec<RoleImg>, &'static str> {
    if draft.roles.is_empty() && draft.role_order.is_none() {
        return Ok(saved.roles.clone());
    }
    let mut base: Vec<RoleImg> = saved
        .roles
        .iter()
        .map(|r| RoleImg {
            id: map.get(&r.id).copied().unwrap_or(r.id),
            ..r.clone()
        })
        .collect();
    for k in draft.roles.keys() {
        let Ok(id) = k.parse::<u64>() else { continue };
        if !base.iter().any(|r| r.id == id) {
            base.extend(live.roles.iter().find(|r| r.id == id).cloned());
        }
    }
    let rc = crate::roles::RoleCtx { ctx, permit: &[] };
    // shortcut: a saved role deleted in Discord since sorts with the roles above Brim, as its
    // live place is unknown; L9's restore puts it back where Discord has room.
    crate::roles::desired(&base, draft, &rc)
        .into_iter()
        .filter(|n| !n.deleted)
        .map(|n| {
            let id = match n.new {
                true => made
                    .get(&n.id)
                    .and_then(|i| i.parse().ok())
                    .ok_or("a role the push made has no id")?,
                false => n.img.id,
            };
            Ok(RoleImg { id, ..n.img })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|mut v| {
            // Top first: renumber without the gaps of deleted roles.
            let n = v.len() as i64;
            for (i, r) in v.iter_mut().enumerate() {
                r.position = if r.id == saved.guild {
                    0
                } else {
                    n - 1 - i as i64
                };
            }
            v
        })
}

/// What a push did to the server layout.
pub(crate) enum Recorded {
    /// No layout is saved, or the push changed nothing in it.
    None,
    Saved(i32),
    /// The layout changed while the push ran; the push is not in it.
    Skipped,
    /// The layout with the push has a serious problem, so it is not saved as correct.
    Unsafe(Vec<String>),
    Failed,
}

impl Recorded {
    pub(crate) fn reply(&self) -> Value {
        match self {
            Recorded::None => Value::Null,
            Recorded::Saved(v) => json!({"version": v}),
            Recorded::Skipped => json!({"skipped": true}),
            Recorded::Unsafe(t) => json!({"unsafe": t}),
            Recorded::Failed => json!({"failed": true}),
        }
    }

    pub(crate) fn log_text(&self) -> String {
        match self {
            Recorded::None => String::new(),
            Recorded::Saved(v) => format!(", saved as server layout v{v}"),
            Recorded::Skipped => {
                ", not saved in the server layout (it changed during the push)".into()
            }
            Recorded::Unsafe(t) => format!(
                ", not saved in the server layout (serious problem: {})",
                t.join("; ")
            ),
            Recorded::Failed => ", the server layout could not be saved".into(),
        }
    }
}

/// The newest layout version, read inside the push lock before the push runs.
pub(crate) async fn base(st: &AppState, guild: i64) -> sqlx::Result<Option<(i32, Image)>> {
    Ok(newest(st, guild)
        .await?
        .map(|(v, sqlx::types::Json(img), ..)| (v, img)))
}

/// What a push read before it ran.
pub(crate) struct Push<'a> {
    pub w: &'a World,
    pub l: &'a ops::Live,
    /// [`base`], or `None` without a saved layout.
    pub base: Option<(i32, Image)>,
    /// Draft key -> id of each channel the push made.
    pub made: &'a HashMap<String, String>,
}

/// After a push that finished: saves the layout it read first with the pushed draft on top as a
/// new version, unless another version came in meanwhile or the result has a serious problem.
pub(crate) async fn after_push(
    st: &AppState,
    p: Push<'_>,
    draft: &Draft,
    user: i64,
    note: &str,
) -> Recorded {
    let guild = p.w.ctx.guild as i64;
    let Some((version, saved)) = p.base else {
        return Recorded::None;
    };
    // Ticket channels stay out: the live ones cannot be taken in, and an older layout that holds one
    // drops it.
    let saved = match ticket_channels(&st.db, guild, saved.channels.iter().map(|c| c.id)).await {
        Ok(t) => without(saved, &t.all),
        Err(e) => {
            tracing::warn!("layout: ticket read after a push in {guild} failed: {e}");
            return Recorded::Failed;
        }
    };
    let live = Image {
        guild: p.w.ctx.guild,
        roles: p.w.ctx.roles.clone(),
        channels: p.w.channels.clone(),
        ..Default::default()
    };
    let live = without(live, &p.w.tickets);
    let image = match with_draft(&saved, &live, draft, &p.w.ctx, p.made) {
        Ok(Some(i)) => i,
        Ok(None) => return Recorded::None,
        Err(e) => {
            tracing::warn!("layout: push in {guild}: {e}");
            return Recorded::Failed;
        }
    };
    // Only a problem the push brings stops it: one the saved layout already has (anti-nuke off,
    // no quarantine role) is the Overview's, and must not keep every push out of the layout.
    let c = &p.w.config;
    let (sa, sb) = (
        safety_with(p.l, image.settings.as_ref()),
        safety_with(p.l, saved.settings.as_ref()),
    );
    // Made roles renumber the list: Brim's top is read from the image it is checked with.
    let ctx = &p.w.ctx;
    let (ba, bb) = (
        crate::roles::bot_top(&image.roles, ctx, p.l.bot_pos),
        crate::roles::bot_top(&saved.roles, ctx, p.l.bot_pos),
    );
    let after = blocking(st, guild, c, p.l, &sa, ba, &image.roles, &image.channels).await;
    let before = blocking(st, guild, c, p.l, &sb, bb, &saved.roles, &saved.channels).await;
    let (after, before) = match (after, before) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!("layout: check after a push in {guild} failed: {e:?}");
            return Recorded::Failed;
        }
    };
    let new: Vec<String> = after
        .into_iter()
        .filter(|c| !before.iter().any(|b| b.id == c.id))
        .map(|c| c.title.to_string())
        .collect();
    if !new.is_empty() {
        return Recorded::Unsafe(new);
    }
    match record(st, guild, &image, "dashboard push", user, note, version).await {
        Ok(Some(v)) => Recorded::Saved(v),
        Ok(None) => Recorded::Skipped,
        Err(e) => {
            tracing::warn!("layout: save after a push in {guild} failed: {e}");
            Recorded::Failed
        }
    }
}

type VersionRow = (
    i32,
    String,
    i64,
    chrono::DateTime<chrono::Utc>,
    Option<String>,
);

/// The layout versions, newest first, for History: who, when and what made each one. A note can
/// name channels, so it needs the level that reads Channel Setup.
async fn versions(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    let level = require(&st, &s, guild, Level::Admin, false).await?;
    let rows: Vec<VersionRow> = sqlx::query_as(
        "SELECT version, source, created_by, created_at, note FROM server_layouts
             WHERE guild_id = $1 ORDER BY version DESC",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let list: Vec<Value> = rows
        .into_iter()
        .map(|(v, source, by, at, note)| {
            json!({"version": v, "source": source, "by": by.to_string(),
                   "at": at.format("%Y-%m-%d %H:%M UTC").to_string(), "note": note})
        })
        .collect();
    Ok(Json(
        json!({"versions": list, "can_roll_back": level >= Level::Owner}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackBody {
    /// The newest version the user looked at.
    expected_version: i32,
}

/// Makes an older layout version current again, as a new version (history stays append-only).
/// Like saving again it changes what counts as correct, so it needs the Owner or an Extra Owner
/// with 2FA, and an old version with a serious problem is refused. Discord is not changed.
async fn rollback(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, version)): Path<(i64, i32)>,
    Json(b): Json<RollbackBody>,
) -> Res {
    require(&st, &s, guild, Level::Owner, true).await?;
    const STALE: ApiError =
        ApiError::Busy("the server layout changed since you looked at it; reload, then roll back");
    let newest = newest(&st, guild).await?.map(|r| r.0);
    if newest != Some(b.expected_version) {
        return Err(STALE);
    }
    if version == b.expected_version {
        return Err(ApiError::BadRequest(
            "that version is already the current server layout".into(),
        ));
    }
    let image: Option<sqlx::types::Json<Image>> =
        sqlx::query_scalar("SELECT image FROM server_layouts WHERE guild_id = $1 AND version = $2")
            .bind(guild)
            .bind(version)
            .fetch_optional(&st.db)
            .await?;
    let Some(sqlx::types::Json(image)) = image else {
        return Err(ApiError::NotFound);
    };
    let (_, config) = store::current(&st.db, guild).await?;
    let l = ops::live(&st, guild, &config).await?;
    let blocking = blocking(
        &st,
        guild,
        &config,
        &l,
        &l.safety,
        l.bot_pos,
        &image.roles,
        &image.channels,
    )
    .await?;
    if !blocking.is_empty() {
        return Ok(Json(json!({"saved": false, "blocking": blocking})));
    }
    // An older version can hold a ticket channel: it is not taken back into the layout.
    let t = ticket_channels(&st.db, guild, image.channels.iter().map(|c| c.id)).await?;
    let image = without(image, &t.all);
    let note = format!("Rolled back to v{version}");
    let Some(new) = record(
        &st,
        guild,
        &image,
        "rollback",
        s.user_id,
        &note,
        b.expected_version,
    )
    .await?
    else {
        return Err(STALE);
    };
    log_by(&st, &s, guild, &config, None, &format!("rolled the server layout back to v{version}, saved as v{new} (Layout Guard). Discord is not changed."))
    .await;
    Ok(Json(json!({"saved": true, "version": new})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use brim_antinuke::OverwriteImg;

    fn role(id: u64, name: &str, position: i64) -> RoleImg {
        RoleImg {
            id,
            name: name.into(),
            color: 0,
            permissions: 0,
            position,
            hoist: false,
            mentionable: false,
            managed: false,
        }
    }

    fn ch(id: u64, name: &str, kind: u8, parent: Option<u64>, position: i64) -> ChannelImg {
        ChannelImg {
            id,
            name: name.into(),
            kind,
            parent,
            position,
            topic: None,
            nsfw: false,
            overwrites: vec![],
        }
    }

    fn server() -> Image {
        Image {
            guild: 1,
            name: "S".into(),
            roles: vec![
                role(1, "@everyone", 0),
                role(10, "Admins", 3),
                role(11, "Moderators", 2),
                role(12, "Member", 1),
            ],
            channels: vec![
                ch(20, "Staff", CATEGORY, None, 0),
                ch(21, "staff-chat", 0, Some(20), 0),
                ch(22, "admin-chat", 0, Some(20), 1),
                ch(23, "mod-notes", 0, Some(20), 2),
                ch(26, "mod-log", 0, Some(20), 3),
                ch(24, "general", 0, None, 0),
            ],
            settings: Some(Settings {
                name: "S".into(),
                system_channel: Some(24),
                ..Default::default()
            }),
            webhooks: None,
        }
    }

    fn changes(saved: &Image, live: &Image) -> Vec<(String, String)> {
        let mut v: Vec<_> = differences(saved, live)
            .into_iter()
            .map(|d| (d.name, d.change))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn ticket_channels_are_no_difference_open_or_closed() {
        let saved = server();
        let mut live = server();
        live.channels.push(ch(90, "ticket-1", 0, Some(20), 4));
        let open = HashSet::from([90]);
        assert!(differences(&saved, &without(live.clone(), &open)).is_empty());
        // A layout saved with an open ticket in it: the ticket closes and its channel goes.
        let mut old = server();
        old.channels.push(ch(90, "ticket-1", 0, Some(20), 4));
        assert!(differences(&without(old.clone(), &open), &server()).is_empty());
        // The ticket is closed but its channel was not deleted: it counts like any other channel.
        assert_eq!(
            changes(&without(old, &open), &live),
            [("ticket-1".into(), NEW.into())]
        );
    }

    #[tokio::test]
    async fn ticket_channels_come_from_the_ticket_records() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_800_000_000 + std::process::id() as i64;
        let c = g * 10;
        sqlx::query(
            "INSERT INTO tickets (guild_id, number, channel_id, user_id, status, closed_at)
             VALUES ($1, 1, $2, 5, 'open', NULL), ($1, 2, $2 + 1, 5, 'closed', now()),
                    ($1, 3, $2 + 2, 6, 'closing', NULL)",
        )
        .bind(g)
        .bind(c)
        .execute(&db)
        .await
        .unwrap();
        let ids = [c, c + 1, c + 2, c + 3].map(|i| i as u64);
        let t = ticket_channels(&db, g, ids.into_iter()).await.unwrap();
        sqlx::query("DELETE FROM tickets WHERE guild_id = $1")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        let c = c as u64;
        assert_eq!(t.open, HashSet::from([c, c + 2]));
        assert_eq!(t.all, HashSet::from([c, c + 1, c + 2]));
        // Another server's ticket record does not count here.
        let none = ticket_channels(&db, g + 1, ids.into_iter()).await.unwrap();
        assert!(none.all.is_empty());
    }

    #[test]
    fn the_same_server_has_no_differences() {
        assert!(differences(&server(), &server()).is_empty());
    }

    #[test]
    fn opening_a_staff_channel_and_everyone_perms_are_differences() {
        let mut live = server();
        live.channels[1].overwrites.push(OverwriteImg {
            target: 1,
            is_role: true,
            allow: 1024,
            deny: 0,
        });
        live.roles[0].permissions = 8;
        assert_eq!(
            changes(&server(), &live),
            vec![
                (
                    "@everyone".into(),
                    "Changed in Discord: Discord permissions".into()
                ),
                (
                    "staff-chat".into(),
                    "Changed in Discord: who can see or write".into()
                ),
            ]
        );
    }

    #[test]
    fn made_deleted_renamed_and_moved_objects_are_named() {
        let mut live = server();
        live.roles.retain(|r| r.id != 11);
        live.roles.push(role(13, "Hacker", 4));
        live.channels[5].name = "chat".into();
        live.channels[3].parent = None;
        live.channels.push(ch(25, "spam", 0, None, 1));
        assert_eq!(
            changes(&server(), &live),
            vec![
                ("Hacker".into(), NEW.into()),
                ("Moderators".into(), GONE.into()),
                ("chat".into(), "Changed in Discord: name".into()),
                ("mod-notes".into(), "Changed in Discord: category".into()),
                ("spam".into(), NEW.into()),
            ]
        );
    }

    #[test]
    fn one_moved_item_is_one_difference() {
        let mut live = server();
        // Member moved to the top: Admins and Moderators only shift down, they did not move.
        live.roles[3].position = 4;
        // admin-chat moved to the end of Staff.
        live.channels[2].position = 4;
        assert_eq!(
            changes(&server(), &live),
            vec![
                (
                    "Member".into(),
                    "Changed in Discord: place in the role order".into()
                ),
                (
                    "admin-chat".into(),
                    "Changed in Discord: place in the list".into()
                ),
            ]
        );
    }

    #[test]
    fn server_settings_compare_through_the_id_map_and_managed_roles_are_skipped() {
        let mut live = server();
        // general deleted and made again with a new id: the system channel still points at it.
        live.channels[5].id = 99;
        live.settings.as_mut().unwrap().system_channel = Some(99);
        live.settings.as_mut().unwrap().verification = 0;
        let mut bot = role(50, "SomeBot", 5);
        bot.managed = true;
        live.roles.push(bot);
        assert!(differences(&server(), &live).is_empty());
        live.settings.as_mut().unwrap().verification = 2;
        live.settings.as_mut().unwrap().name = "T".into();
        assert_eq!(
            changes(&server(), &live),
            vec![(
                "T".into(),
                "Changed in Discord: server name, verification level".into()
            )]
        );
    }

    #[test]
    fn bot_role_overwrites_match_and_a_new_dangerous_bot_role_shows() {
        let mut saved = server();
        let mut bot = role(50, "Brim", 5);
        bot.managed = true;
        saved.roles.push(bot.clone());
        saved.channels[1].overwrites.push(OverwriteImg {
            target: 50,
            is_role: true,
            allow: 1024,
            deny: 0,
        });
        let mut live = saved.clone();
        assert!(differences(&saved, &live).is_empty());
        let mut evil = role(51, "EvilBot", 6);
        (evil.managed, evil.permissions) = (true, 8);
        let mut calm = role(52, "MusicBot", 6);
        calm.managed = true;
        live.roles.extend([evil, calm]);
        live.roles[4].permissions = 8;
        assert_eq!(
            changes(&saved, &live),
            vec![
                (
                    "Brim".into(),
                    "Changed in Discord: Discord permissions".into()
                ),
                ("EvilBot".into(), NEW_BOT.into()),
            ]
        );
    }

    #[test]
    fn moved_keeps_the_longest_run() {
        assert!(moved(&[1, 2, 3], &[1, 2, 3]).is_empty());
        assert_eq!(moved(&[1, 2, 3, 4], &[4, 1, 2, 3]), HashSet::from([4]));
        assert_eq!(moved(&[1, 2, 3], &[1, 3]), HashSet::new());
    }

    #[test]
    fn only_audit_entries_after_the_save_are_named() {
        let since = snowflake_ms(1 << 40);
        let mut d = vec![
            diff("channel", "a", "x".into(), 5),
            diff("channel", "b", "x".into(), 6),
        ];
        let audit = json!({
            "users": [{"id": "7", "username": "eve", "global_name": null}],
            "audit_log_entries": [
                {"id": (2u64 << 40).to_string(), "target_id": "5", "user_id": "7"},
                {"id": (1u64 << 39).to_string(), "target_id": "6", "user_id": "7"},
            ]
        });
        attribute(&mut d, &audit, since, 9);
        assert_eq!(d[0].by.as_deref(), Some("eve (7)"));
        assert!(d[0].at.is_some());
        assert_eq!(d[1].by, None);
    }

    fn ctx(live: &Image) -> Ctx {
        Ctx {
            guild: 1,
            bot: 7,
            staff: vec![11],
            levels: vec![],
            admins: vec![11],
            owners: vec![],
            locked: false,
            roles: live.roles.clone(),
            setup: brim_antinuke::security::Setup {
                quarantine_role: None,
                log_fallback: true,
                unwhitelisted_staff: vec![],
                bot_roles: vec![],
                mod_log: true,
                modules: vec![],
                main_roles: vec![],
                mod_roles: vec![],
                trusted: vec![7],
                log_channels: vec![],
                bot: 7,
                verification_channel: None,
                verification_roles: vec![],
                staff_roles: vec![],
            },
            bot_pos: 10,
        }
    }

    fn draft(v: Value) -> Draft {
        serde_json::from_value(json!({ "items": v })).unwrap()
    }

    fn chan<'a>(img: &'a Image, name: &str) -> &'a ChannelImg {
        img.channels.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn a_push_lands_in_the_layout_with_the_new_ids() {
        let saved = server();
        let live = saved.clone();
        let d = draft(json!({
            "24": {"name": "chat"},
            "26": {"delete": true},
            "new-0": {"kind": 4, "name": "Fun"},
            "new-1": {"kind": 0, "name": "memes", "parent": "new-0"},
        }));
        let made = HashMap::from([
            ("new-0".into(), "900".into()),
            ("new-1".into(), "901".into()),
        ]);
        let img = with_draft(&saved, &live, &d, &ctx(&live), &made)
            .unwrap()
            .unwrap();
        assert_eq!(chan(&img, "chat").id, 24);
        assert!(!img.channels.iter().any(|c| c.id == 26));
        assert_eq!(chan(&img, "Fun").id, 900);
        assert_eq!(chan(&img, "memes").id, 901);
        assert_eq!(chan(&img, "memes").parent, Some(900));
        // The push's own changes are no difference afterwards.
        let mut after = live.clone();
        after.channels = img.channels.clone();
        assert!(differences(&img, &after).is_empty());
        // A made channel without its new id is not saved half right.
        assert!(with_draft(&saved, &live, &d, &ctx(&live), &HashMap::new()).is_err());
    }

    #[test]
    fn discord_changes_on_other_channels_are_not_adopted() {
        let saved = server();
        let mut live = server();
        // Opened to everyone in Discord, and general deleted and made again with a new id.
        live.channels[1].overwrites.push(OverwriteImg {
            target: 1,
            is_role: true,
            allow: 1024,
            deny: 0,
        });
        live.channels[5].id = 99;
        live.settings.as_mut().unwrap().system_channel = Some(99);
        let d = draft(json!({"99": {"name": "lobby"}}));
        let img = with_draft(&saved, &live, &d, &ctx(&live), &HashMap::new())
            .unwrap()
            .unwrap();
        assert!(chan(&img, "staff-chat").overwrites.is_empty());
        // The remade channel is the same layout channel, now under its live id.
        assert_eq!(chan(&img, "lobby").id, 99);
        assert_eq!(img.channels.len(), saved.channels.len());
        assert_eq!(img.settings.as_ref().unwrap().system_channel, Some(99));
        assert_eq!(
            changes(&img, &{
                let mut l = live.clone();
                l.channels[5].name = "lobby".into();
                l
            }),
            vec![(
                "staff-chat".into(),
                "Changed in Discord: who can see or write".into()
            )]
        );
    }

    #[test]
    fn a_discord_made_channel_the_push_edits_comes_in_with_its_category() {
        let saved = server();
        let mut live = server();
        live.channels.push(ch(30, "Games", CATEGORY, None, 1));
        live.channels.push(ch(31, "chess", 0, Some(30), 0));
        live.channels.push(ch(32, "spam", 0, None, 1));
        let d = draft(json!({"31": {"name": "chess-club"}}));
        let img = with_draft(&saved, &live, &d, &ctx(&live), &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(chan(&img, "chess-club").parent, Some(30));
        assert!(img.channels.iter().any(|c| c.id == 30));
        assert!(!img.channels.iter().any(|c| c.id == 32));
    }

    #[test]
    fn a_deleted_system_channel_leaves_the_settings() {
        let saved = server();
        let d = draft(json!({"24": {"delete": true}}));
        let img = with_draft(&saved, &saved, &d, &ctx(&saved), &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(img.settings.unwrap().system_channel, None);
    }

    #[test]
    fn a_draft_that_changes_nothing_saves_no_version() {
        let saved = server();
        let d = draft(json!({"24": {"name": "general"}}));
        assert_eq!(
            with_draft(&saved, &saved, &d, &ctx(&saved), &HashMap::new()),
            Ok(None)
        );
    }

    #[test]
    fn a_role_and_server_push_lands_in_the_layout() {
        let mut saved = server();
        saved.channels[0].overwrites.push(OverwriteImg {
            target: 12,
            is_role: true,
            allow: 0,
            deny: 1024,
        });
        let d: Draft = serde_json::from_value(json!({
            "roles": {"11": {"name": "Mods", "color": 255}, "12": {"delete": true}, "new-1": {"name": "Fans"}},
            "role_order": ["new-1", "10", "11"],
            "server": {"name": "Home", "afk_timeout": 900},
        }))
        .unwrap();
        let made = HashMap::from([("new-1".into(), "900".into())]);
        let img = with_draft(&saved, &saved, &d, &ctx(&saved), &made)
            .unwrap()
            .unwrap();
        let names: Vec<(&str, i64)> = img
            .roles
            .iter()
            .map(|r| (r.name.as_str(), r.position))
            .collect();
        assert_eq!(
            names,
            [("Fans", 3), ("Admins", 2), ("Mods", 1), ("@everyone", 0)]
        );
        assert_eq!(img.roles[0].id, 900);
        // The deleted role's overwrite goes with it, as in Discord.
        assert!(img.channels[0].overwrites.is_empty());
        let s = img.settings.clone().unwrap();
        assert_eq!(
            (s.name.as_str(), s.afk_timeout, s.system_channel),
            ("Home", 900, Some(24))
        );
        // What the push made is no difference afterwards.
        let after = Image {
            channels: img.channels.clone(),
            roles: img.roles.clone(),
            ..img.clone()
        };
        assert!(differences(&img, &after).is_empty());
        // A new role without its id is not saved half right.
        assert!(with_draft(&saved, &saved, &d, &ctx(&saved), &HashMap::new()).is_err());
        // A channel-only draft leaves the saved roles exactly as they are.
        let d = draft(json!({"24": {"name": "chat"}}));
        let img = with_draft(&saved, &saved, &d, &ctx(&saved), &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(img.roles, saved.roles);
    }
}

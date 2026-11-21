//! Layout Guard L4: the Roles and Server tabs of Channel Setup (plan 3.3, 3.4). The draft keeps
//! role and server setting edits next to the channel items; Push runs them with the channel steps
//! under the same lock. Rules the server holds, whatever the page sends: no role at or above
//! Brim's top role is touched; @everyone, the quarantine role and the bot's roles are not
//! edited; managed roles only move; the quarantine role stays the highest role Brim can move;
//! dangerous Discord permissions are never set here (Brim does that with permits); a role that
//! a permit, a setting or a channel draft uses is not deleted.
use crate::channels::{Ctx, Draft, Step};
use brim_antinuke::{ChannelImg, RoleImg, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// "Member basics" (plan 3.3): View Channels, Send Messages, Read History, Add Reactions, Connect,
/// Speak, Use Voice Activity, Change Nickname, Use Application Commands, Create Public Threads,
/// Send in Threads.
pub const BASICS: u64 = (1 << 10)
    | (1 << 11)
    | (1 << 16)
    | (1 << 6)
    | (1 << 20)
    | (1 << 21)
    | (1 << 25)
    | (1 << 26)
    | (1 << 31)
    | (1 << 35)
    | (1 << 38);
/// "Helper tools": Create Invite, Video, Embed Links, Attach Files, External Emoji, Create Private
/// Threads, External Stickers, Activities, Soundboard, External Sounds, Voice Messages, Polls,
/// External Apps. None is a moderation right or in anti-nuke's dangerous list.
pub const HELPERS: u64 = 1
    | (1 << 9)
    | (1 << 14)
    | (1 << 15)
    | (1 << 18)
    | (1 << 36)
    | (1 << 37)
    | (1 << 39)
    | (1 << 42)
    | (1 << 45)
    | (1 << 46)
    | (1 << 49)
    | (1 << 50);
/// The only Discord permissions the editor sets; every other bit keeps its live value.
pub const EDITABLE: u64 = BASICS | HELPERS;
/// Send Messages and Send in Threads: a lock owns them while it is on.
const SEND: u64 = (1 << 11) | (1 << 38);

/// The bits the editor sets now: during a lock, panic mode or a nuke revert not Send (the lock's
/// own denies must not be undone by a role edit, PITFALLS.md).
pub fn editable(ctx: &Ctx) -> u64 {
    if ctx.locked {
        EDITABLE & !SEND
    } else {
        EDITABLE
    }
}
/// New roles one draft may make.
const MAX_NEW: usize = 25;
/// Discord's AFK timeouts, in seconds.
const AFK: [u64; 5] = [60, 300, 900, 1800, 3600];
/// The longest icon data URI kept in a draft (about 500 KB of image).
const MAX_ICON: usize = 700_000;
/// The smallest icon taken, in pixels.
const ICON_MIN: (u32, u32) = (64, 64);

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct RoleItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hoist: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mentionable: Option<bool>,
    /// The whole wanted value; only `EDITABLE` bits may differ from live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub delete: bool,
}

/// Server settings edits (plan 3.4). Channel ids are live ids; `Some(None)` clears.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ServerItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A `data:image/...;base64,` URI, or `null` to remove the icon.
    #[serde(
        deserialize_with = "crate::channels::some",
        skip_serializing_if = "Option::is_none"
    )]
    pub icon: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_filter: Option<u8>,
    #[serde(
        deserialize_with = "crate::channels::some",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_channel: Option<Option<String>>,
    #[serde(
        deserialize_with = "crate::channels::some",
        skip_serializing_if = "Option::is_none"
    )]
    pub afk_channel: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub afk_timeout: Option<u64>,
}

impl ServerItem {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A role as it will be after Push.
#[derive(Serialize, Clone, Debug)]
pub struct RoleNode {
    pub id: String,
    pub name: String,
    pub color: u32,
    pub permissions: u64,
    pub hoist: bool,
    pub mentionable: bool,
    pub managed: bool,
    pub new: bool,
    pub deleted: bool,
    pub changed: bool,
    pub moved: bool,
    /// Staff permit role, Bot, Quarantine, Verified or Custom (plan 3.3); none for @everyone.
    pub tag: Option<&'static str>,
    /// Why the role cannot be edited; `None` when it can.
    pub locked: Option<&'static str>,
    /// It can be dragged in the order.
    pub movable: bool,
    #[serde(skip)]
    pub(crate) img: RoleImg,
}

/// The roles the role rules need besides `Ctx`.
pub struct RoleCtx<'a> {
    pub ctx: &'a Ctx,
    /// Roles a permit or a level (Trusted Admin, Owner) uses.
    pub permit: &'a [u64],
}

const ABOVE: &str = "Brim's top role is not above this role, so Discord does not let Brim change or move it. Move Brim's role higher in Discord first.";
const EVERYONE: &str =
    "@everyone is every member: Brim keeps it as it is, the channel levels build on it.";
const QUARANTINE: &str =
    "The quarantine role: Brim keeps it right below its own role, with its own permissions.";
const BOT: &str = "A bot's role: its bot sets it. It can be moved, not edited.";
const MINE: &str = "Brim's own role: Brim needs it as it is.";

/// Below Brim's top role, by the live order. A new role is movable.
fn movable(id: u64, ctx: &Ctx) -> bool {
    id != ctx.guild
        && ctx
            .roles
            .iter()
            .any(|r| r.id == id && r.position < ctx.bot_pos)
}

/// Why `r` cannot be edited (name, colour, permissions, delete).
fn lock_of(r: &RoleImg, ctx: &Ctx) -> Option<&'static str> {
    if r.id == ctx.guild {
        Some(EVERYONE)
    } else if ctx.setup.bot_roles.contains(&r.id) {
        Some(MINE)
    } else if !movable(r.id, ctx) {
        Some(ABOVE)
    } else if ctx.setup.quarantine_role == Some(r.id) {
        Some(QUARANTINE)
    } else if r.managed {
        Some(BOT)
    } else {
        None
    }
}

fn tag_of(r: &RoleImg, rc: &RoleCtx) -> Option<&'static str> {
    let s = &rc.ctx.setup;
    if r.id == rc.ctx.guild {
        None
    } else if s.quarantine_role == Some(r.id) {
        Some("Quarantine")
    } else if r.managed || s.bot_roles.contains(&r.id) {
        Some("Bot")
    } else if rc.permit.contains(&r.id) || rc.ctx.staff.contains(&r.id) {
        Some("Staff permit role")
    } else if s.main_roles.contains(&r.id) || s.verification_roles.contains(&r.id) {
        Some("Verified")
    } else {
        Some("Custom")
    }
}

/// Top first: the live movable roles in their live order.
fn live_order(live: &[RoleImg], ctx: &Ctx) -> Vec<String> {
    let mut v: Vec<&RoleImg> = live.iter().filter(|r| movable(r.id, ctx)).collect();
    v.sort_by_key(|r| std::cmp::Reverse(r.position));
    v.iter().map(|r| r.id.to_string()).collect()
}

/// The movable roles top first, as the draft wants them: its `role_order`, with roles it does not
/// list at their live place and new roles it does not list at the bottom. A changed order lifts the
/// quarantine role to the top (the score's rule: right below the bot); without one the live order
/// stays, and the Overview's Fix moves it.
pub fn order(live: &[RoleImg], draft: &Draft, ctx: &Ctx) -> Vec<String> {
    let live_ids = live_order(live, ctx);
    let new: Vec<String> = draft
        .roles
        .keys()
        .filter(|k| crate::channels::is_new(k))
        .cloned()
        .collect();
    let known: HashSet<&String> = live_ids.iter().chain(&new).collect();
    let mut out: Vec<String> = vec![];
    for k in draft.role_order.iter().flatten() {
        if known.contains(k) && !out.contains(k) {
            out.push(k.clone());
        }
    }
    // An unlisted live role goes right below the live role above it, or first.
    for (i, k) in live_ids.iter().enumerate() {
        if out.contains(k) {
            continue;
        }
        let at = live_ids[..i]
            .iter()
            .rev()
            .find_map(|above| out.iter().position(|x| x == above))
            .map_or(0, |p| p + 1);
        out.insert(at, k.clone());
    }
    for k in new {
        if !out.contains(&k) {
            out.push(k);
        }
    }
    if let Some(q) = ctx
        .setup
        .quarantine_role
        .filter(|_| draft.role_order.is_some())
        .map(|q| q.to_string())
    {
        if let Some(p) = out.iter().position(|k| *k == q) {
            let q = out.remove(p);
            out.insert(0, q);
        }
    }
    out
}

/// Every role as it will be after Push, top first, with new roles under stand-in ids.
pub fn desired(live: &[RoleImg], draft: &Draft, rc: &RoleCtx) -> Vec<RoleNode> {
    let ctx = rc.ctx;
    let wanted = order(live, draft, ctx);
    let before = live_order(live, ctx);
    let mut top: Vec<&RoleImg> = live
        .iter()
        .filter(|r| r.id != ctx.guild && !movable(r.id, ctx))
        .collect();
    top.sort_by_key(|r| std::cmp::Reverse(r.position));
    let by_id: HashMap<String, &RoleImg> = live.iter().map(|r| (r.id.to_string(), r)).collect();
    let mut keys: Vec<String> = top.iter().map(|r| r.id.to_string()).collect();
    keys.extend(wanted.iter().cloned());
    keys.extend(
        live.iter()
            .find(|r| r.id == ctx.guild)
            .map(|r| r.id.to_string()),
    );
    let n = keys.len() as i64;
    // Where a live role sits among the live roles after the draft: only a changed order moves it.
    let rel: Vec<&String> = wanted.iter().filter(|x| before.contains(x)).collect();
    let rel_at = |k: &String| rel.iter().position(|x| *x == k);
    // New roles get stand-in ids from just below channels' stand-ins, far from any snowflake.
    let mut fresh = 0u64;
    keys.iter()
        .enumerate()
        .map(|(i, k)| {
            let it = draft.roles.get(k).cloned().unwrap_or_default();
            let (base, new) = match by_id.get(k) {
                Some(r) => ((*r).clone(), false),
                None => {
                    fresh += 1;
                    let img = RoleImg {
                        id: u64::MAX / 2 - fresh,
                        name: String::new(),
                        color: 0,
                        permissions: 0,
                        position: 0,
                        hoist: false,
                        mentionable: false,
                        managed: false,
                    };
                    (img, true)
                }
            };
            let locked = (!new).then(|| lock_of(&base, ctx)).flatten();
            let mut img = base.clone();
            if locked.is_none() {
                img.name = it.name.clone().unwrap_or(img.name);
                img.color = it.color.unwrap_or(img.color);
                img.hoist = it.hoist.unwrap_or(img.hoist);
                img.mentionable = it.mentionable.unwrap_or(img.mentionable);
                if let Some(p) = it.permissions {
                    let e = editable(ctx);
                    img.permissions = (base.permissions & !e) | (p & e);
                }
            }
            img.position = if img.id == ctx.guild {
                0
            } else {
                n - 1 - i as i64
            };
            let deleted = it.delete && locked.is_none();
            let moved = !new && rel_at(k) != before.iter().position(|x| x == k);
            RoleNode {
                id: k.clone(),
                name: img.name.clone(),
                color: img.color,
                permissions: img.permissions,
                hoist: img.hoist,
                mentionable: img.mentionable,
                managed: img.managed,
                new,
                deleted,
                changed: new
                    || deleted
                    || moved
                    || (
                        &img.name,
                        img.color,
                        img.permissions,
                        img.hoist,
                        img.mentionable,
                    ) != (
                        &base.name,
                        base.color,
                        base.permissions,
                        base.hoist,
                        base.mentionable,
                    ),
                moved,
                tag: if new {
                    Some("Custom")
                } else {
                    tag_of(&base, rc)
                },
                locked,
                movable: new || movable(base.id, ctx),
                img,
            }
        })
        .collect()
}

/// Brim's top position in `roles` (ranks as `desired` numbers them), or `fallback`.
pub fn bot_top(roles: &[RoleImg], ctx: &Ctx, fallback: i64) -> i64 {
    roles
        .iter()
        .filter(|r| ctx.setup.bot_roles.contains(&r.id))
        .map(|r| r.position)
        .max()
        .unwrap_or(fallback)
}

/// A lower verification level or content filter than live, below what the score calls safe.
/// The score marks these as warnings only, so the serious-problem check alone lets them through.
pub fn weaker(live: &Settings, want: &Settings) -> Option<&'static str> {
    if want.verification < live.verification && want.verification < 2 {
        Some("a verification level below Medium lets brand new accounts post at once")
    } else if want.content_filter < live.content_filter {
        Some("a weaker content filter scans less media")
    } else {
        None
    }
}

/// Drops edits of roles deleted in Discord since.
pub fn prune(draft: &mut Draft, live: &[RoleImg]) {
    let ids: HashSet<String> = live.iter().map(|r| r.id.to_string()).collect();
    draft
        .roles
        .retain(|k, _| crate::channels::is_new(k) || ids.contains(k));
    let keys: HashSet<String> = draft.roles.keys().cloned().collect();
    if let Some(o) = &mut draft.role_order {
        o.retain(|k| ids.contains(k) || keys.contains(k));
    }
}

/// Checks the role and server parts of a draft against the live server and normalizes names.
pub fn validate(
    draft: &mut Draft,
    channels: &[ChannelImg],
    config: &Value,
    rc: &RoleCtx,
) -> Result<(), String> {
    let ctx = rc.ctx;
    let by_id: HashMap<String, &RoleImg> =
        ctx.roles.iter().map(|r| (r.id.to_string(), r)).collect();
    let new = draft
        .roles
        .keys()
        .filter(|k| crate::channels::is_new(k))
        .count();
    if new > MAX_NEW {
        return Err(format!("at most {MAX_NEW} new roles in one push"));
    }
    if let Some(k) = draft.roles.keys().find(|k| draft.items.contains_key(*k)) {
        return Err(format!(
            "{k}: used for a channel and a role; reload the page"
        ));
    }
    // Roles a channel of the draft is limited to or sets in the Advanced grid.
    let in_channels: HashSet<&String> = draft
        .items
        .values()
        .flat_map(|it| {
            it.access
                .iter()
                .flat_map(|a| a.roles.iter())
                .chain(it.grid.keys())
        })
        .collect();
    for (key, it) in &mut draft.roles {
        let live = by_id.get(key).copied();
        let label = live.map_or("the new role".to_string(), |r| format!("role {}", r.name));
        if live.is_none() && !crate::channels::is_new(key) {
            return Err(format!("{key}: not a role of this server"));
        }
        let edits = it.name.is_some()
            || it.color.is_some()
            || it.hoist.is_some()
            || it.mentionable.is_some()
            || it.permissions.is_some()
            || it.delete;
        if let Some(why) = live.and_then(|r| lock_of(r, ctx)) {
            if edits {
                return Err(format!("{label}: {why}"));
            }
        }
        match &it.name {
            Some(n) => {
                let n = n.trim().to_string();
                if !(1..=100).contains(&n.chars().count()) {
                    return Err(format!("{label}: a name needs 1-100 characters"));
                }
                if n == "@everyone" || n == "@here" {
                    return Err(format!("{label}: Discord does not allow that name"));
                }
                it.name = Some(n);
            }
            None if live.is_none() => return Err("a new role needs a name".into()),
            None => {}
        }
        if it.color.is_some_and(|c| c > 0xFF_FFFF) {
            return Err(format!("{label}: a colour is #000000 to #FFFFFF"));
        }
        if let Some(p) = it.permissions {
            let now = live.map_or(0, |r| r.permissions);
            if (p ^ now) & SEND & !editable(ctx) != 0 {
                return Err(format!(
                    "{label}: a lock is on, so Send Messages stays as it is until it ends"
                ));
            }
            if (p ^ now) & !EDITABLE != 0 {
                return Err(format!(
                    "{label}: dangerous Discord permissions are not set here. Brim does this with permits instead"
                ));
            }
        }
        if it.delete {
            if live.is_none() {
                return Err("a new role is removed from the draft, not deleted".into());
            }
            if rc.permit.contains(&key.parse().unwrap_or(0)) {
                return Err(format!(
                    "{label}: a permit uses it. Take it off the permit first"
                ));
            }
            if let Some(by) = crate::channels::used_by(config, key) {
                return Err(format!("{label}: {by} uses it. Change that setting first"));
            }
            if in_channels.contains(key) {
                return Err(format!(
                    "{label}: a channel in this draft uses it. Change that channel first"
                ));
            }
        }
    }
    if let Some(o) = &draft.role_order {
        if o.len() > by_id.len() + MAX_NEW {
            return Err("the role order lists too many roles".into());
        }
        for k in o {
            let ok = match by_id.get(k) {
                Some(r) => movable(r.id, ctx),
                None => draft.roles.contains_key(k) && crate::channels::is_new(k),
            };
            if !ok {
                return Err(format!(
                    "{k}: only roles below Brim's top role can be moved, and not @everyone"
                ));
            }
        }
    }
    if let Some(s) = &mut draft.server {
        server(s, channels)?;
    }
    Ok(())
}

fn server(s: &mut ServerItem, channels: &[ChannelImg]) -> Result<(), String> {
    if let Some(n) = &s.name {
        let n = n.trim().to_string();
        if !(2..=100).contains(&n.chars().count()) {
            return Err("a server name needs 2-100 characters".into());
        }
        s.name = Some(n);
    }
    if let Some(Some(i)) = &s.icon {
        let data = ["png", "jpeg"]
            .iter()
            .find_map(|t| i.strip_prefix(&format!("data:image/{t};base64,")));
        let ok = data.is_some_and(|d| {
            !d.is_empty()
                && i.len() <= MAX_ICON
                && d.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        });
        if !ok {
            return Err("the icon must be a PNG or JPEG of at most 500 KB".into());
        }
    }
    if s.verification.is_some_and(|v| v > 4) {
        return Err("the verification level is 0-4".into());
    }
    if s.content_filter.is_some_and(|v| v > 2) {
        return Err("the content filter is 0-2".into());
    }
    if s.afk_timeout.is_some_and(|t| !AFK.contains(&t)) {
        return Err("the AFK timeout is 1, 5, 15, 30 or 60 minutes".into());
    }
    let kind = |id: &str| {
        channels
            .iter()
            .find(|c| c.id.to_string() == id)
            .map(|c| c.kind)
    };
    if let Some(Some(c)) = &s.system_channel {
        if kind(c) != Some(0) {
            return Err("the system messages channel must be a text channel of this server".into());
        }
    }
    if let Some(Some(c)) = &s.afk_channel {
        if kind(c) != Some(2) {
            return Err("the AFK channel must be a voice channel of this server".into());
        }
    }
    Ok(())
}

/// Decodes a new icon and encodes it again, so only plain image data reaches Discord (as the bot
/// profile does). `unchanged` is the icon the stored draft already holds, cleaned before.
pub async fn clean_icon(s: &mut ServerItem, unchanged: Option<&str>) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let Some(Some(uri)) = &s.icon else {
        return Ok(());
    };
    if unchanged == Some(uri.as_str()) {
        return Ok(());
    }
    let bad = || "the icon must be a PNG or JPEG of at most 500 KB".to_string();
    let b64 = uri.split_once(";base64,").map(|x| x.1).ok_or_else(bad)?;
    if uri.len() > MAX_ICON {
        return Err(bad());
    }
    let bytes = STANDARD.decode(b64).map_err(|_| bad())?;
    let slot = crate::profile::DECODE
        .acquire()
        .await
        .map_err(|_| "try again in a moment".to_string())?;
    let (mime, out) = tokio::task::spawn_blocking(move || {
        (crate::profile::reencode(&bytes, ICON_MIN), drop(slot)).0
    })
    .await
    .map_err(|_| bad())??;
    let clean = format!("data:{mime};base64,{}", STANDARD.encode(out));
    if clean.len() > MAX_ICON {
        return Err(bad());
    }
    s.icon = Some(Some(clean));
    Ok(())
}

/// A server setting channel the draft deletes stops the push: Discord would clear it silently.
pub fn server_uses_deleted(draft: &Draft) -> Option<String> {
    let s = draft.server.as_ref()?;
    [&s.system_channel, &s.afk_channel]
        .into_iter()
        .filter_map(|c| c.clone().flatten())
        .find(|c| draft.items.get(c).is_some_and(|it| it.delete))
}

/// The settings as the draft wants them; `icon` keeps the live hash unless the draft sets one.
pub fn want_settings(live: &Settings, s: &ServerItem) -> Settings {
    let id = |c: &Option<Option<String>>, now: Option<u64>| match c {
        Some(c) => c.as_deref().and_then(|x| x.parse().ok()),
        None => now,
    };
    Settings {
        name: s.name.clone().unwrap_or_else(|| live.name.clone()),
        verification: s.verification.unwrap_or(live.verification),
        content_filter: s.content_filter.unwrap_or(live.content_filter),
        system_channel: id(&s.system_channel, live.system_channel),
        afk_channel: id(&s.afk_channel, live.afk_channel),
        afk_timeout: s.afk_timeout.unwrap_or(live.afk_timeout),
        ..live.clone()
    }
}

/// The PATCH body for the guild: only what differs from live. `None` when nothing does.
pub fn server_body(live: &Settings, s: &ServerItem) -> Option<Value> {
    let want = want_settings(live, s);
    let mut b = Map::new();
    let text = |id: Option<u64>| id.map(|i| i.to_string());
    if want.name != live.name {
        b.insert("name".into(), json!(want.name));
    }
    if want.verification != live.verification {
        b.insert("verification_level".into(), json!(want.verification));
    }
    if want.content_filter != live.content_filter {
        b.insert("explicit_content_filter".into(), json!(want.content_filter));
    }
    if want.system_channel != live.system_channel {
        b.insert("system_channel_id".into(), json!(text(want.system_channel)));
    }
    if want.afk_channel != live.afk_channel {
        b.insert("afk_channel_id".into(), json!(text(want.afk_channel)));
    }
    if want.afk_timeout != live.afk_timeout {
        b.insert("afk_timeout".into(), json!(want.afk_timeout));
    }
    match &s.icon {
        Some(None) if live.icon.is_some() => {
            b.insert("icon".into(), Value::Null);
        }
        Some(Some(uri)) => {
            b.insert("icon".into(), json!(uri));
        }
        _ => {}
    }
    (!b.is_empty()).then_some(Value::Object(b))
}

/// The role body Discord takes for a create or an edit: the fields of `to` that differ from `from`.
pub fn role_body(from: Option<&RoleImg>, to: &RoleImg) -> Option<Value> {
    let mut b = Map::new();
    if from.is_none_or(|f| f.name != to.name) {
        b.insert("name".into(), json!(to.name));
    }
    if from.is_none_or(|f| f.color != to.color) {
        b.insert("color".into(), json!(to.color));
    }
    if from.is_none_or(|f| f.permissions != to.permissions) {
        b.insert("permissions".into(), json!(to.permissions.to_string()));
    }
    if from.is_none_or(|f| f.hoist != to.hoist) {
        b.insert("hoist".into(), json!(to.hoist));
    }
    if from.is_none_or(|f| f.mentionable != to.mentionable) {
        b.insert("mentionable".into(), json!(to.mentionable));
    }
    (!b.is_empty()).then_some(Value::Object(b))
}

/// The role and server steps of a push: (before the channel steps, after them). Creates, edits,
/// the order and the server settings go first; deletes last, after no channel step needs the role.
pub fn plan(
    live: &[RoleImg],
    nodes: &[RoleNode],
    ctx: &Ctx,
    server: Option<Value>,
) -> (Vec<Step>, Vec<Step>) {
    let by_id: HashMap<String, &RoleImg> = live.iter().map(|r| (r.id.to_string(), r)).collect();
    let mut pre = vec![];
    for n in nodes.iter().filter(|n| n.new && !n.deleted) {
        if let Some(body) = role_body(None, &n.img) {
            pre.push(Step::RoleCreate {
                key: n.id.clone(),
                body,
            });
        }
    }
    for n in nodes
        .iter()
        .filter(|n| !n.new && !n.deleted && n.locked.is_none())
    {
        let Some(l) = by_id.get(&n.id) else { continue };
        if let Some(body) = role_body(Some(l), &n.img) {
            pre.push(Step::RoleEdit { id: l.id, body });
        }
    }
    let seq: Vec<&RoleNode> = nodes.iter().filter(|n| n.movable).collect();
    let kept: Vec<String> = seq
        .iter()
        .filter(|n| !n.new)
        .map(|n| n.id.clone())
        .collect();
    let new_last = seq.iter().skip_while(|n| !n.new).all(|n| n.new);
    // New roles land at the bottom: only another order needs the bulk call.
    if kept != live_order(live, ctx) || !new_last {
        let m = seq.len() as i64;
        pre.push(Step::RoleOrder(
            seq.iter()
                .enumerate()
                .map(|(i, n)| (n.id.clone(), m - i as i64))
                .collect(),
        ));
    }
    pre.extend(server.map(Step::Server));
    let post = nodes
        .iter()
        .filter(|n| n.deleted && !n.new)
        .filter_map(|n| by_id.get(&n.id))
        .map(|r| Step::RoleDelete { id: r.id })
        .collect();
    (pre, post)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::tests::ctx as chan_ctx;

    const ADMIN: u64 = 1 << 3;

    fn role(id: u64, position: i64, permissions: u64) -> RoleImg {
        RoleImg {
            id,
            name: format!("r{id}"),
            color: 0,
            permissions,
            position,
            hoist: false,
            mentionable: false,
            managed: false,
        }
    }

    /// @everyone (1), the bot's role 70 on top, roles 61 > 62 > 63 below it, 80 above the bot,
    /// quarantine role 60 in the middle.
    fn ctx() -> Ctx {
        let mut c = chan_ctx();
        c.roles = vec![
            role(1, 0, 0),
            role(63, 1, 0),
            role(60, 2, 0),
            role(62, 3, 0),
            role(61, 4, ADMIN),
            role(70, 5, ADMIN),
            role(80, 6, 0),
        ];
        c.bot_pos = 5;
        c.setup.bot_roles = vec![70];
        c.setup.quarantine_role = Some(60);
        c
    }

    fn draft(v: Value) -> Draft {
        serde_json::from_value(v).unwrap()
    }

    fn rc(c: &Ctx) -> RoleCtx<'_> {
        RoleCtx {
            ctx: c,
            permit: &[62],
        }
    }

    fn ids(nodes: &[RoleNode]) -> Vec<&str> {
        nodes.iter().map(|n| n.id.as_str()).collect()
    }

    #[test]
    fn the_quarantine_role_stays_on_top_and_unlisted_roles_keep_their_place() {
        let c = ctx();
        // Without an order the live order stays; a changed one lifts the quarantine role on top.
        assert_eq!(
            order(&c.roles, &Draft::default(), &c),
            ["61", "62", "60", "63"]
        );
        // The page puts 63 first and leaves 62 out: 62 stays right below the role above it live.
        let d =
            draft(json!({"role_order": ["63", "61", "60"], "roles": {"new-1": {"name": "Fans"}}}));
        assert_eq!(order(&c.roles, &d, &c), ["60", "63", "61", "62", "new-1"]);
        let nodes = desired(&c.roles, &d, &rc(&c));
        assert_eq!(
            ids(&nodes),
            ["80", "70", "60", "63", "61", "62", "new-1", "1"]
        );
        // Ranks are top down; @everyone stays 0.
        let pos: Vec<i64> = nodes.iter().map(|n| n.img.position).collect();
        assert_eq!(pos, [7, 6, 5, 4, 3, 2, 1, 0]);
        assert!(nodes.iter().find(|n| n.id == "63").unwrap().moved);
        assert!(nodes.iter().find(|n| n.id == "new-1").unwrap().new);
        // New roles renumber the list: the checks read Brim's top from it, so the quarantine
        // role right below it is not read as above it (it was with the live position).
        let imgs: Vec<RoleImg> = nodes.iter().map(|n| n.img.clone()).collect();
        let top = bot_top(&imgs, &c, c.bot_pos);
        let q = imgs.iter().find(|r| r.id == 60).unwrap().position;
        assert_eq!((top, q), (6, 5));
    }

    #[test]
    fn locked_roles_are_refused_and_only_harmless_bits_change() {
        let c = ctx();
        let (cfg, ch) = (json!({}), vec![]);
        for (id, why) in [
            ("80", ABOVE),
            ("70", MINE),
            ("60", QUARANTINE),
            ("1", EVERYONE),
        ] {
            let mut d = draft(json!({"roles": {id: {"name": "x"}}}));
            let e = validate(&mut d, &ch, &cfg, &rc(&c)).unwrap_err();
            assert!(e.ends_with(why), "{id}: {e}");
        }
        // Above the bot or @everyone: not in the order either.
        for id in ["80", "70", "1"] {
            let mut d = draft(json!({"role_order": [id]}));
            assert!(validate(&mut d, &ch, &cfg, &rc(&c)).is_err(), "{id}");
        }
        // Taking Administrator away or giving it: refused. Attach Files: fine, Admin stays.
        let mut d = draft(json!({"roles": {"61": {"permissions": 0}}}));
        assert!(validate(&mut d, &ch, &cfg, &rc(&c))
            .unwrap_err()
            .contains("permits"));
        let mut d = draft(json!({"roles": {"63": {"permissions": ADMIN}}}));
        assert!(validate(&mut d, &ch, &cfg, &rc(&c)).is_err());
        let mut d = draft(json!({"roles": {"61": {"permissions": ADMIN | (1 << 15)}}}));
        validate(&mut d, &ch, &cfg, &rc(&c)).unwrap();
        let n = desired(&c.roles, &d, &rc(&c));
        let r = n.iter().find(|n| n.id == "61").unwrap();
        assert_eq!(r.permissions, ADMIN | (1 << 15));
        assert!(r.changed);
        // A crafted node never takes dangerous bits through `desired` either.
        let d = draft(json!({"roles": {"63": {"permissions": ADMIN | (1 << 15)}}}));
        let n = desired(&c.roles, &d, &rc(&c));
        assert_eq!(
            n.iter().find(|n| n.id == "63").unwrap().permissions,
            1 << 15
        );
        // During a lock Send is the lock's: refused, and `desired` keeps the live bit.
        let mut cl = ctx();
        cl.locked = true;
        let mut d = draft(json!({"roles": {"63": {"permissions": 1 << 11}}}));
        assert!(validate(&mut d, &ch, &cfg, &rc(&cl))
            .unwrap_err()
            .contains("lock"));
        let n = desired(&cl.roles, &d, &rc(&cl));
        assert_eq!(n.iter().find(|n| n.id == "63").unwrap().permissions, 0);
        // A managed role below the bot moves but is not edited.
        let mut c2 = ctx();
        c2.roles[1].managed = true;
        let mut d = draft(json!({"roles": {"63": {"color": 5}}}));
        assert!(validate(&mut d, &ch, &cfg, &rc(&c2))
            .unwrap_err()
            .ends_with(BOT));
        let mut d = draft(json!({"role_order": ["63"]}));
        validate(&mut d, &ch, &cfg, &rc(&c2)).unwrap();
    }

    #[test]
    fn a_used_role_is_not_deleted() {
        let c = ctx();
        let ch = vec![];
        // A permit's role.
        let mut d = draft(json!({"roles": {"62": {"delete": true}}}));
        let e = validate(&mut d, &ch, &json!({}), &rc(&c)).unwrap_err();
        assert!(e.contains("permit"), "{e}");
        // A setting's role.
        let mut d = draft(json!({"roles": {"63": {"delete": true}}}));
        let cfg = json!({"verification": {"role": "63"}});
        assert!(validate(&mut d, &ch, &cfg, &rc(&c))
            .unwrap_err()
            .contains("uses it"));
        // A channel of the same draft limited to it.
        let mut d = draft(json!({"roles": {"63": {"delete": true}},
            "items": {"9": {"access": {"roles": ["63"]}}}}));
        assert!(validate(&mut d, &ch, &json!({}), &rc(&c))
            .unwrap_err()
            .contains("channel"));
        let mut d = draft(json!({"roles": {"63": {"delete": true}}}));
        validate(&mut d, &ch, &json!({}), &rc(&c)).unwrap();
        assert!(desired(&c.roles, &d, &rc(&c))
            .iter()
            .any(|n| n.id == "63" && n.deleted));
    }

    #[test]
    fn server_settings_are_checked_and_only_changes_are_sent() {
        let ch = vec![
            ChannelImg {
                id: 5,
                name: "general".into(),
                kind: 0,
                parent: None,
                position: 0,
                topic: None,
                nsfw: false,
                overwrites: vec![],
            },
            ChannelImg {
                id: 6,
                name: "afk".into(),
                kind: 2,
                parent: None,
                position: 1,
                topic: None,
                nsfw: false,
                overwrites: vec![],
            },
        ];
        let bad = [
            json!({"name": "x"}),
            json!({"verification": 5}),
            json!({"afk_timeout": 61}),
            json!({"afk_channel": "5"}),
            json!({"system_channel": "6"}),
            json!({"icon": "data:image/svg+xml;base64,PHN2Zz4="}),
            json!({"icon": "data:image/png;base64,<script>"}),
            json!({"unknown": 1}),
        ];
        for b in bad {
            let d: Result<ServerItem, _> = serde_json::from_value(b.clone());
            let Ok(mut s) = d else { continue };
            assert!(server(&mut s, &ch).is_err(), "{b}");
        }
        let mut s: ServerItem = serde_json::from_value(
            json!({"name": " Home ", "afk_channel": "6", "system_channel": null, "icon": "data:image/png;base64,iVBORw0KGgo="}),
        )
        .unwrap();
        server(&mut s, &ch).unwrap();
        let live = Settings {
            name: "Home".into(),
            system_channel: Some(5),
            afk_timeout: 300,
            ..Default::default()
        };
        let b = server_body(&live, &s).unwrap();
        assert_eq!(
            b,
            json!({"afk_channel_id": "6", "system_channel_id": null, "icon": "data:image/png;base64,iVBORw0KGgo="})
        );
        assert!(server_body(&live, &ServerItem::default()).is_none());
        let low = Settings {
            verification: 1,
            ..live.clone()
        };
        let high = Settings {
            verification: 3,
            content_filter: 2,
            ..live.clone()
        };
        assert!(weaker(&high, &low).is_some());
        assert!(weaker(&low, &high).is_none());
        let mid = Settings {
            verification: 2,
            content_filter: 2,
            ..live.clone()
        };
        assert!(weaker(&high, &mid).is_none());
        // A deleted channel cannot stay the AFK channel.
        let d = draft(json!({"server": {"afk_channel": "6"}, "items": {"6": {"delete": true}}}));
        assert_eq!(server_uses_deleted(&d).as_deref(), Some("6"));
    }

    #[test]
    fn plan_sends_only_what_changed_deletes_last() {
        let c = ctx();
        let none = |d: &Draft| plan(&c.roles, &desired(&c.roles, d, &rc(&c)), &c, None);
        // An empty draft sends nothing, also with the quarantine role low (that is Fix's).
        let (pre, post) = none(&Draft::default());
        assert!(pre.is_empty() && post.is_empty(), "{pre:?}");
        // Any order change lifts it: here the page only swaps 62 and 63.
        let (pre, _) = none(&draft(json!({"role_order": ["61", "60", "63", "62"]})));
        assert!(
            matches!(pre.as_slice(), [Step::RoleOrder(l)] if l[0] == ("60".into(), 4) && l[3] == ("62".into(), 1)),
            "{pre:?}"
        );
        let mut c2 = ctx();
        c2.roles[2].position = 4;
        c2.roles[4].position = 2;
        let d = Draft::default();
        let (pre, post) = plan(&c2.roles, &desired(&c2.roles, &d, &rc(&c2)), &c2, None);
        assert!(pre.is_empty() && post.is_empty(), "{pre:?}");
        // A new role at the bottom needs no order call; an edit sends only its field.
        let d = draft(
            json!({"roles": {"new-1": {"name": "Fans"}, "63": {"hoist": true}, "62": {"delete": true}}}),
        );
        let (pre, post) = plan(
            &c2.roles,
            &desired(&c2.roles, &d, &rc(&c2)),
            &c2,
            Some(json!({"name": "S"})),
        );
        assert_eq!(
            pre,
            [
                Step::RoleCreate {
                    key: "new-1".into(),
                    body: json!({"name": "Fans", "color": 0, "permissions": "0", "hoist": false, "mentionable": false})
                },
                Step::RoleEdit {
                    id: 63,
                    body: json!({"hoist": true})
                },
                Step::Server(json!({"name": "S"})),
            ]
        );
        assert_eq!(post, [Step::RoleDelete { id: 62 }]);
    }
}

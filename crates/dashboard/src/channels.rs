//! Channel Setup: a per-guild draft of channel changes (create, rename, move, delete, who can see
//! it). Nothing reaches Discord until Push, which checks the result with the security score's
//! rules, takes a backup and then runs the steps.
use crate::{
    access::Level,
    api::{log_by, log_change, require, Res},
    auth::Sess,
    ops::{self, overwrite_body},
    recovery::live_image,
    store, ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use brim_antinuke::{
    security::{self, private_overwrites, stripped_overwrite, synced, GuildSafety, Setup},
    ChannelImg, OverwriteImg, RoleImg, CATEGORY,
};
use brim_core::sections::QUARANTINE_DENY;
use reqwest::Method;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const VIEW: u64 = 1 << 10;
const SEND: u64 = 1 << 11;
/// Create public and private threads, send in threads: what a read-only channel denies members.
const THREADS: u64 = (1 << 35) | (1 << 36) | (1 << 38);
/// The Discord permissions the Advanced grid sets (plan 3.2, question 4: no dangerous ones): View,
/// Send, Read History, Connect, Speak, Attach Files, Embed Links, Add Reactions, External Emoji.
pub const HARMLESS: u64 =
    VIEW | SEND | (1 << 16) | (1 << 20) | (1 << 21) | (1 << 15) | (1 << 14) | (1 << 6) | (1 << 18);
const MAX_ITEMS: usize = 500;
/// Roles or strip targets one item may name; Discord allows 100 overwrites per channel.
const MAX_LIST: usize = 100;
/// Text, voice, category, announcement.
const KINDS: [u8; 4] = [0, 2, CATEGORY, 5];
/// The `made` key of the server icon's new hash after a push.
pub(crate) const ICON: &str = "server-icon";

/// The unpushed changes, keyed by a live channel id or `new-<n>` for a channel made in the draft.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    #[serde(default)]
    pub items: BTreeMap<String, Item>,
    /// Lowest `new-<n>` number not yet used: a push raises it past every key it created, so a stale
    /// PUT cannot bring a created key back and create it twice. Kept by the server, the UI's is ignored.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub next: u64,
    /// Role edits (Roles tab), keyed by a live role id or `new-<n>` from the same numbers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, crate::roles::RoleItem>,
    /// The movable roles top first, when the Roles tab changed the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_order: Option<Vec<String>>,
    /// Server settings edits (Server tab).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<crate::roles::ServerItem>,
}

impl Draft {
    /// No change in any tab.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
            && self.roles.is_empty()
            && self.role_order.is_none()
            && self.server.as_ref().is_none_or(|s| s.is_empty())
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The number of a `new-<n>` key.
fn new_num(key: &str) -> Option<u64> {
    key.strip_prefix("new-")?.parse().ok()
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Item {
    /// Only for a new channel: 0 text, 2 voice, 4 category, 5 announcement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `null` = no category; a live id or `new-<n>`. Left out = unchanged.
    #[serde(deserialize_with = "some", skip_serializing_if = "Option::is_none")]
    pub parent: Option<Option<String>>,
    /// Order inside the parent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    /// Left out = unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
    /// Drop the channel's own overwrites and take its category's.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reset: bool,
    /// Keep the channel's own overwrites after a move, also when it followed its old category
    /// ("Permissions after the move: Keep its own"); it becomes an exception.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keep: bool,
    /// Overwrite targets whose dangerous allows are removed (the Fix of an issue).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub strip: Vec<String>,
    /// The Advanced grid: role id -> [allow, deny] of the `HARMLESS` permissions, set last.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub grid: BTreeMap<String, [u64; 2]>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub delete: bool,
}

/// Keeps an explicit `null` apart from a missing field.
pub(crate) fn some<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// Who can see a channel.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Access {
    #[serde(default)]
    pub roles: Vec<String>,
    /// Staff (the Admins and Moderators permit roles) see a limited channel too.
    #[serde(default = "yes")]
    pub staff: bool,
    /// Only `roles` and staff see it. Left out: limited when `roles` is set, so an empty list is
    /// Everyone; `true` with no roles is staff only.
    #[serde(default)]
    pub limited: Option<bool>,
    /// Open to members, who read but do not write or start threads; staff write (Information).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// A limited channel for one staff tier instead of all staff: "admins" (the admin-chat rule:
    /// Admins permit, Trusted Admin and Owner roles) or "owner" (the Owner role, owner-chat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<String>,
}

fn yes() -> bool {
    true
}

impl Access {
    fn is_limited(&self) -> bool {
        self.limited.unwrap_or(!self.roles.is_empty())
    }
}

/// What the draft rules need besides the channels.
pub struct Ctx {
    pub guild: u64,
    /// The bot's member id, let into limited channels.
    pub bot: u64,
    pub staff: Vec<u64>,
    /// The Owner and Trusted Admin level roles: staff too, for the badges only.
    pub levels: Vec<u64>,
    /// "Admins only": the Admins permit roles and the level roles.
    pub admins: Vec<u64>,
    /// "Owner only": the Owner level role.
    pub owners: Vec<u64>,
    /// A lock, panic mode or a nuke revert is on: its Send denies are not read or changed as
    /// "Read-only", or picking Public would unlock a locked channel.
    pub locked: bool,
    pub roles: Vec<RoleImg>,
    pub setup: Setup,
    pub bot_pos: i64,
}

impl Ctx {
    /// Roles a user picks: not @everyone, staff, the bot's, quarantine, a main or a managed role.
    fn pickable(&self, role: u64) -> bool {
        role != self.guild
            && !self.staff.contains(&role)
            && !self.setup.bot_roles.contains(&role)
            && self.setup.quarantine_role != Some(role)
            && !self.setup.main_roles.contains(&role)
            && !self.roles.iter().any(|r| r.id == role && r.managed)
    }

    /// Roles the Advanced grid may set: any role of the server except the quarantine role, the
    /// bot's and managed roles, whose overwrites Brim or their bot keep.
    pub(crate) fn grid_role(&self, role: u64) -> bool {
        self.roles.iter().any(|r| r.id == role && !r.managed)
            && self.setup.quarantine_role != Some(role)
            && !self.setup.bot_roles.contains(&role)
    }
}

/// A channel as it will be after Push, with what changed.
#[derive(Serialize, Clone, Debug)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub kind: u8,
    pub parent: Option<String>,
    pub position: i64,
    pub new: bool,
    pub deleted: bool,
    pub renamed: bool,
    pub moved: bool,
    /// Its overwrites differ from its category's.
    pub exception: bool,
    pub access: Access,
    /// The badge on the row: "Unverified", "Staff" or "Members read only"; none for a channel
    /// members use (owner decision 2026-11-18).
    pub tag: Option<&'static str>,
    /// The custom roles a limited channel is limited to, shown instead of `tag`.
    pub tag_roles: Vec<String>,
    /// The Brim feature whose channel this is (logs, verification ...): its permissions stay
    /// its own when it moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<&'static str>,
    /// "Ticket" or "Lockdown" for an open ticket's channel or the lock's `#server-locked`: shown,
    /// but its name, place and existence are not layout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<&'static str>,
    /// Role overwrites as the Advanced grid shows them: [role id, allow, deny], `HARMLESS` bits only.
    pub grid: Vec<(String, u64, u64)>,
    pub issues: Vec<Issue>,
    /// The desired channel; a new one has a stand-in id.
    #[serde(skip)]
    pub(crate) img: ChannelImg,
    #[serde(skip)]
    changed: bool,
    /// A live channel that takes its category's overwrites because it is synced in live.
    #[serde(skip)]
    follows: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Issue {
    /// The node id.
    pub channel: String,
    pub id: String,
    pub title: String,
    pub detail: String,
    pub fix: Option<DraftFix>,
    /// On a member overwrite: it stops Push, but the tree's red pulse counts only role overwrites.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub member: bool,
}

/// The draft edit Fix makes on the issue's channel.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DraftFix {
    /// Add this target to the item's `strip`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip: Option<String>,
    /// Set the item's `access` to this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
}

/// Inserts the overwrite when missing, edits it, and drops one the edit leaves empty.
fn edit(
    ows: &mut Vec<OverwriteImg>,
    target: u64,
    is_role: bool,
    f: impl FnOnce(&mut OverwriteImg),
) {
    let at = ows
        .iter()
        .position(|o| o.target == target && o.is_role == is_role);
    let i = at.unwrap_or_else(|| {
        ows.push(OverwriteImg {
            target,
            is_role,
            allow: 0,
            deny: 0,
        });
        ows.len() - 1
    });
    let before = ows[i].clone();
    f(&mut ows[i]);
    if ows[i].allow == 0 && ows[i].deny == 0 && (at.is_none() || ows[i] != before) {
        ows.remove(i);
    }
}

/// `ows` with `a` applied. Only View Channel bits change, and only on @everyone, the main roles,
/// the bot, staff and picked roles: overwrites other features own stay as they are.
fn apply(c: &ChannelImg, ows: Vec<OverwriteImg>, a: &Access, ctx: &Ctx) -> Vec<OverwriteImg> {
    let mut ows = ows;
    let main = &ctx.setup.main_roles;
    if !a.is_limited() {
        // The verification channel stays open to @everyone with the main roles kept out, or
        // nobody could verify (review, 11-18).
        if ctx.setup.verification_channel == Some(c.id) {
            edit(&mut ows, ctx.guild, true, |o| {
                o.allow &= !VIEW;
                o.deny &= !VIEW;
            });
            return ows;
        }
        // With a main role, open means the standard set: the main roles see it and @everyone
        // (not verified yet) does not, so opening a channel never skips verification.
        let hidden = !main.is_empty();
        for &r in main {
            edit(&mut ows, r, true, |o| {
                o.deny &= !VIEW;
                if hidden {
                    o.allow |= VIEW;
                }
            });
        }
        edit(&mut ows, ctx.guild, true, |o| {
            o.allow &= !VIEW;
            o.deny = if hidden {
                o.deny | VIEW
            } else {
                o.deny & !VIEW
            };
        });
        // Read-only: members (the main roles, or @everyone without one) read but do not write.
        // Public clears @everyone's write deny too, or a channel made before the main role stays
        // read-only. During a lock its Send denies are the lock's, so nothing here touches them.
        let mut members = main.clone();
        if !a.read_only || main.is_empty() {
            members.push(ctx.guild);
        }
        for r in members.into_iter().filter(|_| !ctx.locked) {
            edit(&mut ows, r, true, |o| {
                if a.read_only {
                    o.allow &= !(SEND | THREADS);
                    o.deny |= SEND | THREADS;
                } else {
                    o.deny &= !(SEND | THREADS);
                }
            });
        }
        if a.read_only && !ctx.locked {
            for &r in ctx.staff.iter().chain(&ctx.levels) {
                edit(&mut ows, r, true, |o| {
                    o.allow |= VIEW | SEND;
                    o.deny &= !(VIEW | SEND);
                });
            }
        }
        // @everyone goes last, so Push writes the main role allows before the deny and a
        // refused allow never leaves the channel hidden from members (PITFALLS, 11-18).
        if let Some(i) = ows.iter().position(|o| o.is_role && o.target == ctx.guild) {
            let e = ows.remove(i);
            ows.push(e);
        }
        return ows;
    }
    let img = ChannelImg {
        overwrites: ows.clone(),
        ..c.clone()
    };
    for o in private_overwrites(&img, ctx.guild, ctx.bot, main) {
        edit(&mut ows, o.target, o.is_role, |x| *x = o.clone());
    }
    // Also on a channel that was hidden already: the bot member's View marks it as limited on
    // purpose, so the standard set never opens it once its roles are deleted (owner decision 11-18).
    edit(&mut ows, ctx.bot, false, |o| {
        o.allow |= VIEW;
        o.deny &= !VIEW;
    });
    let picked: Vec<u64> = a.roles.iter().filter_map(|r| r.parse().ok()).collect();
    let dropped: Vec<u64> = ows
        .iter()
        .filter(|o| o.is_role && o.allow & VIEW != 0 && ctx.pickable(o.target))
        .map(|o| o.target)
        .filter(|t| !picked.contains(t))
        .collect();
    for r in dropped {
        edit(&mut ows, r, true, |o| o.allow &= !VIEW);
    }
    let let_in = |o: &mut OverwriteImg| {
        o.allow |= VIEW;
        o.deny &= !VIEW;
    };
    for &r in &picked {
        edit(&mut ows, r, true, let_in);
    }
    let tier: Option<&[u64]> = match a.only.as_deref() {
        Some("admins") => Some(&ctx.admins),
        Some("owner") => Some(&ctx.owners),
        _ => None,
    };
    for &r in &ctx.staff {
        if a.staff && tier.is_none_or(|t| t.contains(&r)) {
            edit(&mut ows, r, true, let_in);
        } else if !picked.contains(&r) {
            edit(&mut ows, r, true, |o| o.allow &= !VIEW);
        }
    }
    if let Some(t) = tier {
        for &r in t {
            edit(&mut ows, r, true, let_in);
        }
        // A Trusted Admin role is not let into an Owner-only channel.
        for &r in ctx
            .levels
            .iter()
            .filter(|r| !t.contains(r) && !picked.contains(r))
        {
            edit(&mut ows, r, true, |o| o.allow &= !VIEW);
        }
    }
    ows
}

/// The row's badge and the custom roles it shows (owner decision 2026-11-18): the verification channel
/// "Unverified", a limited one its custom roles or else "Staff", a members' channel where the
/// main roles cannot send "Members read only" (Information), any other members' channel none.
fn tag_of(c: &ChannelImg, a: &Access, ctx: &Ctx) -> (Option<&'static str>, Vec<String>) {
    if ctx.setup.verification_channel == Some(c.id) {
        return (Some("Unverified"), vec![]);
    }
    if a.is_limited() {
        let custom: Vec<String> = a
            .roles
            .iter()
            .filter(|r| r.parse().is_ok_and(|r: u64| !ctx.levels.contains(&r)))
            .cloned()
            .collect();
        return match (custom.is_empty(), a.only.as_deref()) {
            (true, Some("admins")) => (Some("Admins"), vec![]),
            (true, Some("owner")) => (Some("Owner"), vec![]),
            (true, _) => (Some("Staff"), vec![]),
            (false, _) => (None, custom),
        };
    }
    let ow = |t: u64| c.overwrites.iter().find(|o| o.is_role && o.target == t);
    let denied = |t: u64| ow(t).is_some_and(|o| o.deny & SEND != 0);
    let main = &ctx.setup.main_roles;
    let members = if main.is_empty() {
        vec![ctx.guild]
    } else {
        main.clone()
    };
    let read_only = members
        .iter()
        .all(|&r| ow(r).is_none_or(|o| o.allow & SEND == 0) && (denied(r) || denied(ctx.guild)));
    (read_only.then_some("Members read only"), vec![])
}

/// Who can see a channel with these overwrites.
fn access_of(ows: &[OverwriteImg], ctx: &Ctx) -> Access {
    let ow = |t: u64| ows.iter().find(|o| o.is_role && o.target == t);
    let sees = |t: u64| ow(t).is_some_and(|o| o.allow & VIEW != 0);
    // Hidden from @everyone with a main role let in is a members' channel (verification).
    let limited = ow(ctx.guild).is_some_and(|o| o.deny & VIEW != 0)
        && !ctx.setup.main_roles.iter().any(|&r| sees(r));
    let staff = !limited || ctx.staff.iter().all(|&r| sees(r));
    // One tier sees it and no staff or level role outside that tier does.
    let tier_only = |t: &[u64]| {
        !t.is_empty()
            && t.iter().all(|&r| sees(r))
            && !ctx
                .staff
                .iter()
                .chain(&ctx.levels)
                .any(|r| !t.contains(r) && sees(*r))
    };
    let only = match limited && !staff {
        true if tier_only(&ctx.owners) => Some("owner".into()),
        true if tier_only(&ctx.admins) => Some("admins".into()),
        _ => None,
    };
    let denied = |t: u64| ow(t).is_some_and(|o| o.deny & SEND != 0);
    let members = match ctx.setup.main_roles.is_empty() {
        true => vec![ctx.guild],
        false => ctx.setup.main_roles.clone(),
    };
    let read_only = !limited
        && !ctx.locked
        && members.iter().all(|&r| {
            ow(r).is_none_or(|o| o.allow & SEND == 0) && (denied(r) || denied(ctx.guild))
        });
    Access {
        roles: ows
            .iter()
            .filter(|o| limited && o.is_role && o.allow & VIEW != 0 && ctx.pickable(o.target))
            .filter(|o| !ctx.levels.contains(&o.target))
            .map(|o| o.target.to_string())
            .collect(),
        staff,
        limited: Some(limited),
        read_only,
        only,
    }
}

pub(crate) fn is_new(key: &str) -> bool {
    key.strip_prefix("new-")
        .is_some_and(|n| (1..=9).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Drops edits of channels deleted in Discord since, and moves into categories that are gone.
pub fn prune(draft: &mut Draft, live: &[ChannelImg]) {
    let ids: HashSet<String> = live.iter().map(|c| c.id.to_string()).collect();
    draft.items.retain(|k, _| is_new(k) || ids.contains(k));
    let keys: HashSet<String> = draft.items.keys().cloned().collect();
    for it in draft.items.values_mut() {
        if let Some(Some(p)) = &it.parent {
            if !ids.contains(p) && !keys.contains(p) {
                it.parent = None;
            }
        }
    }
}

/// Discord's rules for a text channel name: lowercase, no spaces.
fn channel_name(name: &str, kind: u8) -> String {
    let name = name.trim();
    if matches!(kind, 0 | 5) {
        name.split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase()
    } else {
        name.into()
    }
}

/// Checks a draft from the UI against the live server and normalizes its names.
pub fn validate(draft: &mut Draft, live: &[ChannelImg], ctx: &Ctx) -> Result<(), String> {
    if draft.items.len() > MAX_ITEMS {
        return Err(format!("at most {MAX_ITEMS} changes"));
    }
    let by_id: HashMap<String, &ChannelImg> = live.iter().map(|c| (c.id.to_string(), c)).collect();
    let kinds: HashMap<String, u8> = draft
        .items
        .iter()
        .filter_map(|(k, it)| Some((k.clone(), it.kind?)))
        .chain(by_id.iter().map(|(k, c)| (k.clone(), c.kind)))
        .collect();
    for (key, it) in &mut draft.items {
        let live_c = by_id.get(key);
        let kind = match (live_c, it.kind) {
            (Some(_), Some(_)) => return Err(format!("{key}: only a new channel has a kind")),
            (Some(c), None) => c.kind,
            (None, Some(k)) if is_new(key) && KINDS.contains(&k) => k,
            (None, _) => return Err(format!("{key}: not a channel of this server")),
        };
        match &it.name {
            Some(n) => {
                let n = channel_name(n, kind);
                if !(1..=100).contains(&n.chars().count()) {
                    return Err(format!("{key}: a name needs 1-100 characters"));
                }
                it.name = Some(n);
            }
            None if live_c.is_none() => return Err(format!("{key}: a new channel needs a name")),
            None => {}
        }
        if let Some(Some(p)) = &it.parent {
            if kind == CATEGORY {
                return Err(format!("{key}: a category cannot be in a category"));
            }
            if kinds.get(p) != Some(&CATEGORY) {
                return Err(format!("{key}: {p} is not a category"));
            }
        }
        if it.position.is_some_and(|p| !(0..=10_000).contains(&p)) {
            return Err(format!("{key}: position must be 0-10000"));
        }
        let digits =
            |s: &String| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit());
        if it.strip.len() > MAX_LIST || !it.strip.iter().all(digits) {
            return Err(format!("{key}: strip takes up to {MAX_LIST} ids"));
        }
        if it.grid.len() > MAX_LIST {
            return Err(format!("{key}: the grid takes up to {MAX_LIST} roles"));
        }
        if !it.grid.is_empty()
            && ctx
                .setup
                .verification_channel
                .is_some_and(|v| v.to_string() == *key)
        {
            return Err(format!(
                "{key}: the verification channel keeps the permissions verification needs"
            ));
        }
        for (t, [allow, deny]) in &it.grid {
            let role = t.parse::<u64>().ok().filter(|r| ctx.grid_role(*r));
            if role == Some(ctx.guild) && allow & VIEW != 0 && !ctx.setup.main_roles.is_empty() {
                return Err(format!("{key}: @everyone cannot be let in by the grid: members who have not verified would see it. Use Public"));
            }
            if role.is_none() || (allow | deny) & !HARMLESS != 0 || allow & deny != 0 {
                return Err(format!(
                    "{key}: the grid takes roles of this server and only View, Send, Read History, Connect, Speak, Attach Files, Embed Links, Add Reactions and External Emoji"
                ));
            }
        }
        if let Some(a) = &it.access {
            let tier_empty = match a.only.as_deref() {
                None => false,
                Some("admins") => ctx.admins.is_empty(),
                Some("owner") => ctx.owners.is_empty(),
                Some(_) => return Err(format!("{key}: only is \"admins\" or \"owner\"")),
            };
            if tier_empty {
                return Err(format!("{key}: no role is set for that level yet. Set the roles of the permits and levels first"));
            }
            if (a.only.is_some() && !a.is_limited()) || (a.read_only && a.is_limited()) {
                return Err(format!("{key}: read-only is for a channel members see, Admins and Owner only for a limited one"));
            }
            // Quarantine, bot, main, managed roles and @everyone are not given access by hand.
            let known = |r: &String| {
                r.parse::<u64>()
                    .is_ok_and(|r| ctx.pickable(r) && ctx.roles.iter().any(|x| x.id == r))
            };
            if a.roles.len() > MAX_LIST || !a.roles.iter().all(known) {
                return Err(format!(
                    "{key}: access takes up to {MAX_LIST} roles of this server that you can pick"
                ));
            }
        }
    }
    for (key, it) in &draft.items {
        let cat = kinds.get(key) == Some(&CATEGORY);
        if !(it.delete && cat) {
            continue;
        }
        let kept_child = live
            .iter()
            .map(|c| (c.id.to_string(), c.parent.map(|p| p.to_string())))
            .chain(
                draft
                    .items
                    .keys()
                    .filter(|k| is_new(k))
                    .map(|k| (k.clone(), None)),
            )
            .any(|(k, live_parent)| {
                let item = draft.items.get(&k);
                let parent = item.and_then(|i| i.parent.clone()).unwrap_or(live_parent);
                parent.as_ref() == Some(key) && !item.is_some_and(|i| i.delete)
            });
        if kept_child {
            return Err("move or delete its channels first".into());
        }
    }
    Ok(())
}

/// Channels the draft hides from staff that live shows them, by a channel's own access, a move
/// or reset into such a category, or a new channel in one. Only owners may do that. Without staff
/// roles every limited channel hides from staff, so newly limiting one needs an owner too.
pub fn unstaffed(nodes: &[Node], live: &[ChannelImg], ctx: &Ctx) -> HashSet<String> {
    // Admins only and Owner only hide it from part of staff too.
    let hides =
        |a: &Access| a.is_limited() && (!a.staff || ctx.staff.is_empty() || a.only.is_some());
    // A staff role's own View deny (the Advanced grid) hides it from them too.
    let denies = |ows: &[OverwriteImg]| -> Vec<u64> {
        ows.iter()
            .filter(|o| o.is_role && o.deny & VIEW != 0 && ctx.staff.contains(&o.target))
            .map(|o| o.target)
            .collect()
    };
    nodes
        .iter()
        .filter(|n| !n.deleted)
        .filter(|n| {
            let live = live.iter().find(|c| c.id.to_string() == n.id);
            let newly_hidden =
                hides(&n.access) && live.is_none_or(|c| !hides(&access_of(&c.overwrites, ctx)));
            let before = live.map(|c| denies(&c.overwrites)).unwrap_or_default();
            newly_hidden
                || denies(&n.img.overwrites)
                    .iter()
                    .any(|r| !before.contains(r))
        })
        .map(|n| n.id.clone())
        .collect()
}

/// The refusal when `unstaffed` finds something and the user is no owner.
fn unstaff_refusal(ctx: &Ctx) -> ApiError {
    ApiError::Forbidden(if ctx.staff.is_empty() {
        "only owners can hide a channel from staff. Set roles for the Admins and Moderators permits first"
    } else {
        "only owners can hide a channel from staff"
    })
}

/// Positions as the index inside the parent, the order the UI edits. Discord's own positions have
/// gaps and ties; text-like channels sort above voice ones in a category.
pub fn normalized(mut live: Vec<ChannelImg>) -> Vec<ChannelImg> {
    let voice = |c: &ChannelImg| matches!(c.kind, 2 | 13);
    live.sort_by_key(|c| (c.parent, c.kind == CATEGORY, voice(c), c.position, c.id));
    let mut at: HashMap<(Option<u64>, bool), i64> = HashMap::new();
    for c in &mut live {
        let i = at.entry((c.parent, c.kind == CATEGORY)).or_default();
        c.position = *i;
        *i += 1;
    }
    live
}

/// The draft applied to the live channels. Category changes reach the channels synced with the
/// category; a channel with its own overwrites (an exception) keeps them.
pub fn desired(live: &[ChannelImg], draft: &Draft, ctx: &Ctx) -> Vec<Node> {
    let none = Item::default();
    let new_keys: Vec<&String> = draft
        .items
        .iter()
        .filter(|(k, it)| is_new(k) && !it.delete)
        .map(|(k, _)| k)
        .collect();
    // New channels get stand-in ids from the top of the range, far from any snowflake.
    let id_of: HashMap<String, u64> = live
        .iter()
        .map(|c| (c.id.to_string(), c.id))
        .chain(
            new_keys
                .iter()
                .enumerate()
                .map(|(i, k)| ((*k).clone(), u64::MAX - i as u64)),
        )
        .collect();
    let next = live.iter().map(|c| c.position).max().unwrap_or(0) + 1;
    let mut rows: Vec<(String, Option<&ChannelImg>, &Item)> = live
        .iter()
        .map(|c| {
            let key = c.id.to_string();
            let it = draft.items.get(&key).unwrap_or(&none);
            (key, Some(c), it)
        })
        .chain(
            new_keys
                .iter()
                .map(|k| ((*k).clone(), None, &draft.items[*k])),
        )
        .collect();
    let quarantine = |mut ows: Vec<OverwriteImg>| {
        if let Some(q) = ctx.setup.quarantine_role {
            if !ows.iter().any(|o| o.is_role && o.target == q) {
                ows.push(OverwriteImg {
                    target: q,
                    is_role: true,
                    allow: 0,
                    deny: QUARANTINE_DENY,
                });
            }
        }
        ows
    };
    // Categories first, so their channels can follow them.
    let kind_of = |c: Option<&ChannelImg>, it: &Item| c.map_or(it.kind.unwrap_or(0), |c| c.kind);
    rows.sort_by_key(|(_, c, it)| kind_of(*c, it) != CATEGORY);
    let mut done: HashMap<String, Vec<OverwriteImg>> = HashMap::new();
    let mut nodes = vec![];
    for (key, c, it) in rows {
        let kind = kind_of(c, it);
        let name = it
            .name
            .clone()
            .or(c.map(|c| c.name.clone()))
            .unwrap_or_default();
        let parent = match (&it.parent, c) {
            (Some(p), _) => p.clone(),
            (None, c) => c.and_then(|c| c.parent).map(|p| p.to_string()),
        };
        let position = it.position.or(c.map(|c| c.position)).unwrap_or(next);
        let parent_ows = parent.as_ref().and_then(|p| done.get(p)).cloned();
        let live_cat = c
            .and_then(|c| c.parent)
            .and_then(|p| live.iter().find(|x| x.id == p));
        let follows = c.is_some_and(|c| live_cat.is_some_and(|cat| synced(c, cat)));
        let follows = follows && parent_ows.is_some() && !it.keep;
        let base = match (c, parent_ows) {
            (None, p) => quarantine(p.unwrap_or_default()),
            (Some(_), Some(p)) if it.reset || follows => p,
            (Some(c), _) => c.overwrites.clone(),
        };
        let img = ChannelImg {
            id: id_of[&key],
            name: name.clone(),
            kind,
            parent: parent.as_ref().and_then(|p| id_of.get(p).copied()),
            position,
            topic: c.and_then(|c| c.topic.clone()),
            nsfw: c.is_some_and(|c| c.nsfw),
            overwrites: base,
        };
        let mut ows = match &it.access {
            Some(a) => apply(&img, img.overwrites.clone(), a, ctx),
            None => img.overwrites.clone(),
        };
        // The grid goes before the quarantine and verification shapes, which always win.
        for (t, [allow, deny]) in &it.grid {
            if let Ok(t) = t.parse() {
                edit(&mut ows, t, true, |o| {
                    o.allow = (o.allow & !HARMLESS) | allow;
                    o.deny = (o.deny & !HARMLESS) | deny;
                });
            }
        }

        // Taking a category's overwrites (reset, move, follow) must not drop the quarantine deny.
        let q = ctx.setup.quarantine_role;
        let held = c.and_then(|c| {
            c.overwrites
                .iter()
                .find(|o| o.is_role && Some(o.target) == q)
        });
        if let Some(o) = held.filter(|o| !ows.iter().any(|x| x.is_role && x.target == o.target)) {
            ows.push(o.clone());
        }
        // The verification channel keeps the shape the join quarantine reads, whatever it takes.
        if let Some(q) = q.filter(|_| ctx.setup.verification_channel == Some(id_of[&key])) {
            ows.retain(|o| !(o.is_role && o.target == q));
            ows.push(OverwriteImg {
                target: q,
                is_role: true,
                allow: brim_core::sections::VERIFY_ALLOW,
                deny: brim_core::sections::VERIFY_DENY,
            });
        }
        for o in ows.iter_mut() {
            if it.strip.contains(&o.target.to_string()) {
                *o = stripped_overwrite(o, ctx.guild, &ctx.setup);
            }
        }
        let mut img = ChannelImg {
            overwrites: ows,
            ..img
        };
        let renamed = c.is_some_and(|c| c.name != name);
        let moved =
            c.is_some_and(|c| c.parent.map(|p| p.to_string()) != parent || c.position != position);
        let changed = c.is_none_or(|c| !synced(&img, c)) || it.delete || renamed || moved;
        // A log channel the draft changes goes out locked, whatever the draft gave it: nobody
        // but Brim deletes there. One the draft leaves alone stays as it is (the bot locks it).
        if changed
            && id_of
                .get(&key)
                .is_some_and(|id| ctx.setup.log_channels.contains(id))
        {
            for o in security::lock_overwrites(&img, ctx.guild, ctx.setup.bot) {
                edit(&mut img.overwrites, o.target, o.is_role, |x| *x = o.clone());
            }
        }
        done.insert(key.clone(), img.overwrites.clone());
        let access = access_of(&img.overwrites, ctx);
        let (tag, tag_roles) = tag_of(&img, &access, ctx);
        nodes.push(Node {
            id: key,
            name,
            kind,
            parent,
            position,
            new: c.is_none(),
            deleted: it.delete,
            renamed,
            moved,
            exception: false,
            access,
            tag,
            tag_roles,
            fixed: None,
            temp: None,
            grid: img
                .overwrites
                .iter()
                .filter(|o| o.is_role && (o.allow | o.deny) & HARMLESS != 0)
                .map(|o| (o.target.to_string(), o.allow & HARMLESS, o.deny & HARMLESS))
                .collect(),
            issues: vec![],
            img,
            changed,
            follows,
        });
    }
    let cats: HashMap<String, ChannelImg> = nodes
        .iter()
        .filter(|n| n.kind == CATEGORY)
        .map(|n| (n.id.clone(), n.img.clone()))
        .collect();
    for n in &mut nodes {
        n.exception = n
            .parent
            .as_ref()
            .and_then(|p| cats.get(p))
            .is_some_and(|cat| !synced(&n.img, cat));
    }
    // The order Discord shows: channels without a category on top, then each category and its channels.
    let pos: HashMap<String, i64> = nodes.iter().map(|n| (n.id.clone(), n.position)).collect();
    nodes.sort_by_key(|n| {
        let group = match (&n.parent, n.kind == CATEGORY) {
            (_, true) => (1, n.position, n.id.clone()),
            (Some(p), _) => (1, pos.get(p).copied().unwrap_or(0), p.clone()),
            (None, _) => (0, 0, String::new()),
        };
        (group, n.kind != CATEGORY, n.position, n.id.clone())
    });
    nodes
}

/// The section of `config` that uses channel `id`, by its page title.
pub(crate) fn used_by(config: &Value, id: &str) -> Option<String> {
    let needle = format!("\"{id}\"");
    let (key, _) = config.as_object()?.iter().find(|(k, v)| {
        // Old role-profile data is ignored since Channel Setup replaced it.
        k.as_str() != "channel_profiles" && v.to_string().contains(&needle)
    })?;
    let title = crate::schema::schema()
        .into_iter()
        .find(|s| s["name"] == key.as_str())
        .and_then(|s| s["title"].as_str().map(String::from));
    Some(title.unwrap_or_else(|| key.clone()))
}

/// What makes the draft unsafe: the security score's channel rows on the desired channels (role
/// overwrites only), plus deleting a channel the config still uses.
/// The issues of what the draft changes (owner decision 2026-11-11): a channel the draft touches, or an
/// issue the draft brings. Problems that are already on the server are the Overview's to show.
pub fn draft_issues(live: &[ChannelImg], nodes: &[Node], ctx: &Ctx, config: &Value) -> Vec<Issue> {
    let before: HashSet<String> = issues(&desired(live, &Draft::default(), ctx), ctx, config)
        .into_iter()
        .map(|i| i.id)
        .collect();
    let changed: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.changed)
        .map(|n| n.id.as_str())
        .collect();
    issues(nodes, ctx, config)
        .into_iter()
        .filter(|i| changed.contains(i.channel.as_str()) || !before.contains(&i.id))
        .collect()
}

pub fn issues(nodes: &[Node], ctx: &Ctx, config: &Value) -> Vec<Issue> {
    let kept: Vec<&Node> = nodes.iter().filter(|n| !n.deleted).collect();
    let key: HashMap<u64, &str> = kept.iter().map(|n| (n.img.id, n.id.as_str())).collect();
    let imgs: Vec<ChannelImg> = kept.iter().map(|n| n.img.clone()).collect();
    // Only the channel rows are kept, so the server settings here do not matter.
    let g = GuildSafety {
        id: ctx.guild,
        mfa: 1,
        verification: 2,
        content_filter: 2,
        notifications: 1,
    };
    let mut out = vec![];
    for row in security::checks(&g, &ctx.roles, &imgs, ctx.bot_pos, &ctx.setup) {
        let mut member = false;
        let (channel, id, fix) = if let Some(rest) = row.id.strip_prefix("overwrite_") {
            let Some((c, t)) = rest.split_once('_') else {
                continue;
            };
            let (Ok(c), Ok(t)) = (c.parse::<u64>(), t.parse::<u64>()) else {
                continue;
            };
            member = imgs
                .iter()
                .find(|x| x.id == c)
                .is_some_and(|x| x.overwrites.iter().any(|o| o.target == t && !o.is_role));
            let fix = DraftFix {
                strip: Some(t.to_string()),
                access: None,
            };
            (key[&c], format!("overwrite_{}_{t}", key[&c]), Some(fix))
        } else if let Some(c) = row.id.strip_prefix("log_channel_public_") {
            let Some(k) = c.parse::<u64>().ok().and_then(|c| key.get(&c)) else {
                continue;
            };
            let fix = row.fix.as_ref().map(|_| DraftFix {
                strip: None,
                access: Some(Access {
                    roles: vec![],
                    staff: true,
                    limited: Some(true),
                    ..Default::default()
                }),
            });
            (*k, format!("log_channel_public_{k}"), fix)
        } else {
            continue;
        };
        out.push(Issue {
            channel: channel.into(),
            id,
            title: row.title.into(),
            detail: row.detail,
            fix,
            member,
        });
    }
    for n in nodes.iter().filter(|n| n.deleted && !n.new) {
        if let Some(by) = used_by(config, &n.id) {
            let what = if n.kind == CATEGORY {
                format!("Category {}", n.name)
            } else {
                format!("#{}", n.name)
            };
            out.push(Issue {
                channel: n.id.clone(),
                id: format!("in_use_{}", n.id),
                title: "Channel is in use".into(),
                detail: format!("{what} is used by {by}; pick another channel first."),
                fix: None,
                member: false,
            });
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub id: u64,
    pub position: i64,
    /// `Some(None)` = out of its category; a live id or a `new-<n>` key.
    pub parent: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Create {
        key: String,
        kind: u8,
        name: String,
        parent: Option<String>,
        position: i64,
        overwrites: Vec<OverwriteImg>,
    },
    Rename {
        id: u64,
        name: String,
    },
    /// One bulk call for every moved channel.
    Positions(Vec<Move>),
    Overwrite {
        channel: u64,
        ow: OverwriteImg,
    },
    DropOverwrite {
        channel: u64,
        target: u64,
    },
    Delete {
        id: u64,
    },
    /// A new role (draft key) with Discord's role body.
    RoleCreate {
        key: String,
        body: Value,
    },
    RoleEdit {
        id: u64,
        body: Value,
    },
    /// The movable roles with their new positions: live ids or `new-<n>` keys.
    RoleOrder(Vec<(String, i64)>),
    RoleDelete {
        id: u64,
    },
    /// The guild PATCH body of the Server tab.
    Server(Value),
}

/// The Discord calls that turn `live` into `nodes`, in a safe order: categories, channels, names,
/// positions, overwrites, deletes last. Only differences, so a second run resumes a failed one.
pub fn plan(live: &[ChannelImg], nodes: &[Node]) -> Vec<Step> {
    let by_id: HashMap<String, &ChannelImg> = live.iter().map(|c| (c.id.to_string(), c)).collect();
    let mut steps = vec![];
    let mut fresh: Vec<&Node> = nodes.iter().filter(|n| n.new && !n.deleted).collect();
    fresh.sort_by_key(|n| n.kind != CATEGORY);
    steps.extend(fresh.iter().map(|n| Step::Create {
        key: n.id.clone(),
        kind: n.kind,
        name: n.name.clone(),
        parent: n.parent.clone(),
        position: n.position,
        overwrites: n.img.overwrites.clone(),
    }));
    let kept: Vec<(&Node, &ChannelImg)> = nodes
        .iter()
        .filter(|n| !n.new && !n.deleted)
        .filter_map(|n| Some((n, *by_id.get(&n.id)?)))
        .collect();
    for (n, c) in &kept {
        if n.renamed {
            steps.push(Step::Rename {
                id: c.id,
                name: n.name.clone(),
            });
        }
    }
    // Positions are indexes inside a parent, so every sibling of a moved channel is sent too.
    let group = |n: &Node| (n.parent.clone(), n.kind == CATEGORY);
    // A new channel takes an index position, so its kept siblings need index positions too.
    let touched: HashSet<_> = kept
        .iter()
        .filter(|(n, _)| n.moved)
        .map(|(n, _)| group(n))
        .chain(fresh.iter().map(|n| group(n)))
        .collect();
    let moves: Vec<Move> = kept
        .iter()
        .filter(|(n, _)| n.moved || touched.contains(&group(n)))
        .map(|(n, c)| Move {
            id: c.id,
            position: n.position,
            parent: (c.parent.map(|p| p.to_string()) != n.parent).then(|| n.parent.clone()),
        })
        .collect();
    if !moves.is_empty() {
        steps.push(Step::Positions(moves));
    }
    for (n, c) in &kept {
        let same =
            |a: &OverwriteImg, b: &OverwriteImg| a.target == b.target && a.is_role == b.is_role;
        for o in &n.img.overwrites {
            if !c.overwrites.contains(o) {
                steps.push(Step::Overwrite {
                    channel: c.id,
                    ow: o.clone(),
                });
            }
        }
        for o in &c.overwrites {
            if !n.img.overwrites.iter().any(|x| same(x, o)) {
                steps.push(Step::DropOverwrite {
                    channel: c.id,
                    target: o.target,
                });
            }
        }
    }
    let mut gone: Vec<&Node> = nodes.iter().filter(|n| n.deleted && !n.new).collect();
    gone.sort_by_key(|n| n.kind == CATEGORY);
    steps.extend(
        gone.iter()
            .filter_map(|n| by_id.get(&n.id))
            .map(|c| Step::Delete { id: c.id }),
    );
    steps
}

/// After `key` was created as `id`: the item becomes a live one, and channels moving into it point
/// at it. A made role does the same in the role edits and the order.
fn remap(draft: &mut Draft, key: &str, id: &str) {
    if let Some(mut it) = draft.items.remove(key) {
        it.kind = None;
        draft.items.insert(id.into(), it);
    }
    if let Some(it) = draft.roles.remove(key) {
        // Kept under its id, so the layout takes the made role in also when a later step fails.
        draft.roles.insert(id.into(), it);
        for k in draft.role_order.iter_mut().flatten() {
            if k == key {
                *k = id.into();
            }
        }
    }
    for it in draft.items.values_mut() {
        if it
            .parent
            .as_ref()
            .is_some_and(|p| p.as_deref() == Some(key))
        {
            it.parent = Some(Some(id.into()));
        }
    }
}

/// Writes "follow the category" into the draft for every channel that follows it only because it
/// is synced in live. Push saves this first: once the category is pushed, a child whose step
/// failed is no longer synced in live and would otherwise read as an exception on the next push.
fn pin_follows(draft: &mut Draft, nodes: &[Node]) {
    for n in nodes.iter().filter(|n| n.follows && !n.deleted) {
        draft.items.entry(n.id.clone()).or_default().reset = true;
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/guilds/{guild}/channel-setup",
            get(get_draft).put(put_draft).delete(discard),
        )
        .route("/api/guilds/{guild}/channel-setup/push", post(push))
}

/// The live server as the draft rules see it.
pub(crate) struct World {
    pub(crate) config: Value,
    pub(crate) ctx: Ctx,
    pub(crate) channels: Vec<ChannelImg>,
    /// Roles a permit or a level uses: tagged, never deleted.
    pub(crate) permit: Vec<u64>,
    /// The live server settings.
    pub(crate) settings: brim_antinuke::Settings,
    /// Raw Discord JSON for the backup Push takes first.
    raw: (Value, Value),
    /// Channels of open tickets and of a held lock, by Brim's own records: not part of the layout.
    pub(crate) temp: crate::layout::Temp,
}

impl World {
    pub(crate) fn rc(&self) -> crate::roles::RoleCtx<'_> {
        crate::roles::RoleCtx {
            ctx: &self.ctx,
            permit: &self.permit,
        }
    }
}

async fn world(st: &AppState, guild: i64) -> Result<World, ApiError> {
    Ok(world_live(st, guild).await?.0)
}

/// [`world`] with the live read it came from, for the layout checks after a push.
async fn world_live(st: &AppState, guild: i64) -> Result<(World, ops::Live), ApiError> {
    let (_, config) = store::current(&st.db, guild).await?;
    let l = ops::live(st, guild, &config).await?;
    Ok((world_of(st, guild, config, &l).await?, l))
}

/// [`world`] from a [`ops::Live`] already read with `config`.
async fn world_of(
    st: &AppState,
    guild: i64,
    config: Value,
    l: &ops::Live,
) -> Result<World, ApiError> {
    let staff: Vec<u64> = brim_core::permits::staff_roles(&st.db, guild)
        .await?
        .into_iter()
        .map(|r| r as u64)
        .collect();
    let level_roles = brim_core::permits::level_roles(&st.db, guild).await?;
    let levels: Vec<u64> = level_roles.iter().map(|(_, r)| *r as u64).collect();
    // Only roles that still exist: a deleted Owner role must not be the one let in.
    let alive = |r: &u64| l.image.roles.iter().any(|x| x.id == *r);
    let owners = level_roles
        .iter()
        .filter(|(l, _)| l == "owner")
        .map(|(_, r)| *r as u64)
        .filter(alive)
        .collect();
    // Admins and Moderators share `staff`; the Moderators-only roles are the rest.
    let mut admins: Vec<u64> = staff
        .iter()
        .copied()
        .filter(|r: &u64| !l.setup.mod_roles.contains(r))
        .chain(levels.iter().copied())
        .filter(alive)
        .collect();
    admins.sort_unstable();
    admins.dedup();
    Ok(World {
        config,
        ctx: Ctx {
            guild: guild as u64,
            bot: st.discord.client_id.parse().unwrap_or_default(),
            staff,
            levels,
            admins,
            owners,
            locked: crate::recovery::no_backup_now(st, guild).await?,
            roles: l.image.roles.clone(),
            setup: l.setup.clone(),
            bot_pos: l.bot_pos,
        },
        channels: normalized(l.image.channels.clone()),
        permit: brim_core::permits::permit_roles(&st.db, guild)
            .await?
            .into_iter()
            .map(|r| r as u64)
            .collect(),
        settings: crate::layout::settings_of(&l.guild),
        raw: (l.roles.clone(), l.channels.clone()),
        temp: crate::layout::temp_channels(&st.db, guild, l.image.channels.iter().map(|c| c.id))
            .await?,
    })
}

/// The stored draft and its version (`updated_at`, empty without a row).
async fn load(st: &AppState, guild: i64) -> Result<(Draft, String), ApiError> {
    let row: Option<(Value, String)> =
        sqlx::query_as("SELECT draft, updated_at::text FROM channel_drafts WHERE guild_id = $1")
            .bind(guild)
            .fetch_optional(&st.db)
            .await?;
    Ok(row
        .map(|(v, t)| (serde_json::from_value(v).unwrap_or_default(), t))
        .unwrap_or_default())
}

/// Stores the draft and returns its new version; an empty one without a `next` is deleted.
async fn save(st: &AppState, guild: i64, draft: &Draft, user: i64) -> sqlx::Result<String> {
    if draft.is_empty() && draft.next == 0 {
        sqlx::query("DELETE FROM channel_drafts WHERE guild_id = $1")
            .bind(guild)
            .execute(&st.db)
            .await?;
        return Ok(String::new());
    }
    sqlx::query_scalar(
        "INSERT INTO channel_drafts (guild_id, draft, updated_by) VALUES ($1, $2, $3)
         ON CONFLICT (guild_id) DO UPDATE SET draft = $2, updated_by = $3, updated_at = now()
         RETURNING updated_at::text",
    )
    .bind(guild)
    .bind(sqlx::types::Json(draft))
    .bind(user)
    .fetch_one(&st.db)
    .await
}

/// The guild's draft lock, held until the transaction ends (also when it is dropped or the process
/// dies). Push holds it exclusively for its whole run; PUT and DELETE share it while they save, so
/// they are refused during a push instead of overwriting the draft it writes back.
/// Every holder ends it with `release` before answering: a dropped transaction only queues its
/// ROLLBACK, so the lock outlives the request and the next try-lock is refused (PITFALLS.md).
async fn lock(
    st: &AppState,
    guild: i64,
    push: bool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, ApiError> {
    let mut tx = st.db.begin().await?;
    let sql = if push {
        "SELECT pg_try_advisory_xact_lock(hashtextextended('channel_drafts:' || $1, 0))"
    } else {
        "SELECT pg_try_advisory_xact_lock_shared(hashtextextended('channel_drafts:' || $1, 0))"
    };
    let ok: bool = sqlx::query_scalar(sql)
        .bind(guild)
        .fetch_one(&mut *tx)
        .await?;
    if !ok {
        return Err(ApiError::Busy(if push {
            "a push is running or the draft is being saved; try again in a moment"
        } else {
            "a push is running; wait for it to finish, then reload"
        }));
    }
    Ok(tx)
}

async fn release(tx: sqlx::Transaction<'static, sqlx::Postgres>) {
    if let Err(e) = tx.rollback().await {
        tracing::warn!("channel draft lock release failed: {e}");
    }
}

/// The Overview's row for an unpushed draft that Push would refuse, by Channel Setup's own rules.
pub(crate) async fn overview_check(
    st: &AppState,
    guild: i64,
    config: &Value,
    l: &ops::Live,
) -> Result<Option<security::Check>, ApiError> {
    let (draft, _) = load(st, guild).await?;
    if draft.items.is_empty() {
        return Ok(None);
    }
    // The score's own read: Discord is not asked twice.
    let w = world_of(st, guild, config.clone(), l).await?;
    let tree = desired(&w.channels, &draft, &w.ctx);
    let n = draft_issues(&w.channels, &tree, &w.ctx, &w.config).len();
    Ok((n > 0).then(|| security::Check {
        id: "channel_setup_draft".into(),
        severity: security::Severity::Warn,
        title: "Channel Setup draft is not safe",
        detail: format!("{n} problem(s) in the unpushed Channel Setup draft, so Push refuses it."),
        fix: None,
        steps: security::steps("channel_setup_draft"),
    }))
}

/// The feature that owns channel `id` and keeps its permissions fixed (plan 3.2a): log channels,
/// the appeal, verification and catch channels, and the channels in a ticket category.
fn fixed_by(config: &Value, id: &str, parent: Option<&str>) -> Option<&'static str> {
    let is = |v: &Value| v.as_str() == Some(id);
    let logs = serde_json::from_value::<brim_logging::LogConfig>(config["logging"].clone())
        .unwrap_or_default();
    if is(&config["tickets"]["log_channel"])
        || logs.channels().iter().any(|c| c.get().to_string() == id)
    {
        Some("Logging")
    } else if is(&config["verification"]["channel"]) {
        Some("Verification")
    } else if is(&config["appeals"]["channel"]) {
        Some("Appeals")
    } else if is(&config["catch"]["channel"]) {
        Some("Catch")
    } else if parent.is_some_and(|p| crate::tickets::categories(config).contains(&p)) {
        Some("Tickets")
    } else {
        None
    }
}

/// [`fixed_by`] with the live category, so a draft move out of the ticket category does not
/// free a ticket channel.
fn fixed_of(w: &World, id: &str) -> Option<&'static str> {
    let live = w.channels.iter().find(|c| c.id.to_string() == id);
    let parent = live.and_then(|c| c.parent).map(|p| p.to_string());
    fixed_by(&w.config, id, parent.as_deref())
}

/// A feature's channel keeps its own permissions: a move keeps them and a reset is dropped. An open
/// ticket's channel and the lock's channel are temporary: a draft cannot rename, move or delete them.
fn hold_fixed(draft: &mut Draft, w: &World) {
    for (k, it) in draft.items.iter_mut() {
        if k.parse().is_ok_and(|id| w.temp.open.contains(&id)) {
            (it.name, it.parent, it.position, it.delete) = (None, None, None, false);
        }
        if fixed_of(w, k).is_some() {
            it.reset = false;
            it.keep |= it.parent.is_some();
        }
    }
}

/// The tree with its issues, as every route answers.
fn view(w: &World, draft: &Draft, level: Level, version: &str) -> (Vec<Node>, Vec<Issue>, Value) {
    let mut tree = desired(&w.channels, draft, &w.ctx);
    for n in &mut tree {
        n.fixed = fixed_of(w, &n.id);
        n.temp = match n.img.id {
            _ if n.new => None,
            id if w.temp.lock.contains(&id) => Some("Lockdown"),
            id if w.temp.open.contains(&id) => Some("Ticket"),
            _ => None,
        };
    }
    let issues = draft_issues(&w.channels, &tree, &w.ctx, &w.config);
    for n in &mut tree {
        // Member overwrites stop Push but do not make the row pulse.
        n.issues = issues
            .iter()
            .filter(|i| i.channel == n.id && !i.member)
            .cloned()
            .collect();
    }
    let pickable: Vec<String> = w
        .ctx
        .roles
        .iter()
        .filter(|r| w.ctx.pickable(r.id))
        .map(|r| r.id.to_string())
        .collect();
    let roles = crate::roles::desired(&w.ctx.roles, draft, &w.rc());
    let server = draft.server.clone().unwrap_or_default();
    let server_changed = crate::roles::server_body(&w.settings, &server).is_some();
    let reply = json!({
        "draft": draft,
        "version": version,
        "tree": tree,
        "issues": issues,
        "changes": tree.iter().filter(|n| n.changed).count()
            + roles.iter().filter(|n| n.changed).count()
            + usize::from(server_changed),
        "roles": roles,
        "server": {"live": w.settings, "want": crate::roles::want_settings(&w.settings, &server), "changed": server_changed},
        "staff_roles": w.ctx.staff.iter().map(u64::to_string).collect::<Vec<_>>(),
        "pickable_roles": pickable,
        "can_unstaff": level >= Level::Owner,
        "grid_roles": w.ctx.roles.iter().filter(|r| w.ctx.grid_role(r.id)).map(|r| r.id.to_string()).collect::<Vec<_>>(),
        // Which of "Admins only" and "Owner only" have roles to let in.
        "tiers": {"admins": !w.ctx.admins.is_empty(), "owner": !w.ctx.owners.is_empty()},
    });
    (tree, issues, reply)
}

async fn get_draft(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    let level = require(&st, &s, guild, Level::Admin, false).await?;
    let w = world(&st, guild).await?;
    let (mut draft, version) = load(&st, guild).await?;
    prune(&mut draft, &w.channels);
    crate::roles::prune(&mut draft, &w.ctx.roles);
    Ok(Json(view(&w, &draft, level, &version).2))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutBody {
    draft: Draft,
}

async fn put_draft(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    // Access first, so someone without it learns nothing about the draft's shape.
    let level = require(&st, &s, guild, Level::Owner, false).await?;
    let mut draft = serde_json::from_value::<PutBody>(b)
        .map_err(|e| ApiError::BadRequest(format!("invalid draft: {e}")))?
        .draft;
    let lock = lock(&st, guild, false).await?;
    let r: Res = async {
        let w = world(&st, guild).await?;
        prune(&mut draft, &w.channels);
        crate::roles::prune(&mut draft, &w.ctx.roles);
        hold_fixed(&mut draft, &w);
        // The draft's own key means nothing to the person who typed the name.
        validate(&mut draft, &w.channels, &w.ctx)
            .and_then(|_| crate::roles::validate(&mut draft, &w.channels, &w.config, &w.rc()))
            .map_err(|e| ApiError::BadRequest(e.replace("new-0: ", "")))?;
        let (mut before, _) = load(&st, guild).await?;
        prune(&mut before, &w.channels);
        draft.next = before.next;
        let old_icon = before
            .server
            .as_ref()
            .and_then(|s| s.icon.clone())
            .flatten();
        if let Some(sv) = &mut draft.server {
            crate::roles::clean_icon(sv, old_icon.as_deref())
                .await
                .map_err(ApiError::BadRequest)?;
        }
        let reused = draft.items.keys().chain(draft.roles.keys()).any(|k| {
            !before.items.contains_key(k)
                && !before.roles.contains_key(k)
                && new_num(k).is_some_and(|n| n < before.next)
        });
        if reused {
            return Err(ApiError::Busy(
                "this draft is older than the last push; reload the page",
            ));
        }
        // What an owner already put in the draft may stay while someone else edits the rest.
        let hidden = |d: &Draft| unstaffed(&desired(&w.channels, d, &w.ctx), &w.channels, &w.ctx);
        if level < Level::Owner && !hidden(&draft).is_subset(&hidden(&before)) {
            return Err(unstaff_refusal(&w.ctx));
        }
        let version = save(&st, guild, &draft, s.user_id).await?;
        Ok(Json(view(&w, &draft, level, &version).2))
    }
    .await;
    release(lock).await;
    r
}

async fn discard(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    let level = require(&st, &s, guild, Level::Owner, false).await?;
    let lock = lock(&st, guild, false).await?;
    let r: Res = async {
        let empty = Draft {
            next: load(&st, guild).await?.0.next,
            ..Default::default()
        };
        let version = save(&st, guild, &empty, s.user_id).await?;
        let w = world(&st, guild).await?;
        Ok(Json(view(&w, &empty, level, &version).2))
    }
    .await;
    release(lock).await;
    r
}

async fn push(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let level = require(&st, &s, guild, Level::Owner, true).await?;
    let lock = lock(&st, guild, true).await?;
    // A task, so the push finishes and releases the lock also when this request is dropped.
    tokio::spawn(async move {
        let r = push_locked(st, s, guild, b, level).await;
        release(lock).await;
        r
    })
    .await?
}

async fn push_locked(
    st: AppState,
    s: Sess,
    guild: i64,
    b: Value,
    level: Level,
) -> Result<Response, ApiError> {
    let (w, l) = world_live(&st, guild).await?;
    // Read before anything runs: the push's layout version is built on this one, or not saved.
    let base = crate::layout::base(&st, guild).await?;
    let (mut draft, version) = load(&st, guild).await?;
    // Push runs exactly the draft the user reviewed.
    if b["version"].as_str() != Some(version.as_str()) {
        return Err(ApiError::Busy(
            "the draft changed since you looked at it; check it again, then push",
        ));
    }
    prune(&mut draft, &w.channels);
    crate::roles::prune(&mut draft, &w.ctx.roles);
    hold_fixed(&mut draft, &w);
    validate(&mut draft, &w.channels, &w.ctx)
        .and_then(|_| crate::roles::validate(&mut draft, &w.channels, &w.config, &w.rc()))
        .map_err(ApiError::BadRequest)?;
    if let Some(c) = crate::roles::server_uses_deleted(&draft) {
        return Err(ApiError::BadRequest(format!(
            "<#{c}> is deleted in this draft: pick another AFK or system messages channel first"
        )));
    }
    let (tree, issues, _) = view(&w, &draft, level, &version);
    let role_nodes = crate::roles::desired(&w.ctx.roles, &draft, &w.rc());
    let server = draft
        .server
        .as_ref()
        .and_then(|s| crate::roles::server_body(&w.settings, s));
    // A role or server change must not bring a serious problem (the quarantine role below
    // another one) or a weaker server; the channel part is checked by `issues`.
    if role_nodes.iter().any(|n| n.changed) || server.is_some() {
        let want =
            crate::roles::want_settings(&w.settings, &draft.server.clone().unwrap_or_default());
        if let Some(why) = crate::roles::weaker(&w.settings, &want) {
            return Err(ApiError::BadRequest(format!(
                "this push would make the server less safe: {why}"
            )));
        }
        let safety = crate::layout::safety_with(&l, Some(&want));
        let roles: Vec<RoleImg> = role_nodes
            .iter()
            .filter(|n| !n.deleted)
            .map(|n| n.img.clone())
            .collect();
        let c = &w.config;
        // New roles renumber the list, Brim's role too.
        let top = crate::roles::bot_top(&roles, &w.ctx, l.bot_pos);
        let after =
            crate::layout::blocking(&st, guild, c, &l, &safety, top, &roles, &w.channels).await?;
        let before = crate::layout::blocking(
            &st,
            guild,
            c,
            &l,
            &l.safety,
            l.bot_pos,
            &w.ctx.roles,
            &w.channels,
        )
        .await?;
        let new: Vec<&str> = after
            .iter()
            .filter(|a| !before.iter().any(|b| b.id == a.id))
            .map(|a| a.title)
            .collect();
        if !new.is_empty() {
            return Err(ApiError::BadRequest(format!(
                "this push would bring a serious problem: {}",
                new.join("; ")
            )));
        }
    }
    if level < Level::Owner && !unstaffed(&tree, &w.channels, &w.ctx).is_empty() {
        return Err(unstaff_refusal(&w.ctx));
    }
    if !issues.is_empty() {
        let body = json!({"error": "the draft is not safe yet", "issues": issues});
        return Ok((StatusCode::CONFLICT, Json(body)).into_response());
    }
    let (pre, post) = crate::roles::plan(&w.ctx.roles, &role_nodes, &w.ctx, server);
    let steps: Vec<Step> = pre
        .into_iter()
        .chain(plan(&w.channels, &tree))
        .chain(post)
        .collect();
    if steps.is_empty() {
        let empty = Draft {
            next: draft.next,
            ..Default::default()
        };
        save(&st, guild, &empty, s.user_id).await?;
        return Ok(Json(json!({"done": 0, "failed": [], "backup": null})).into_response());
    }
    // Not while locked or in panic mode: that state must not become the newest backup.
    let backup: Option<i64> = match crate::recovery::no_backup_now(&st, guild).await? {
        true => None,
        false => Some(
            sqlx::query_scalar(
                "INSERT INTO backups (guild_id, image) VALUES ($1, $2) RETURNING id",
            )
            .bind(guild)
            .bind(sqlx::types::Json(live_image(guild, &w.raw.0, &w.raw.1)))
            .fetch_one(&st.db)
            .await?,
        ),
    };
    let named = crate::recovery::backup_named(backup);
    // The layout takes the draft as the user made it, before Push pins live followers.
    let reviewed = draft.clone();
    pin_follows(&mut draft, &tree);
    save(&st, guild, &draft, s.user_id).await?;
    let who = format!("{} (<@{}>)", s.username, s.user_id);
    let summary = outline(&steps, &tree);
    log_change(
        &st,
        guild,
        s.user_id,
        &w.config,
        None,
        format!("{who} started a channel setup push: {summary}, {named}"),
    )
    .await;
    let n = steps.len();
    let (done, failed, made) = run_steps(st.clone(), guild, s.user_id, draft, steps).await;
    // A push that stopped halfway keeps its draft; the next push finishes it and saves the layout.
    let layout = match failed.is_empty() {
        true => {
            let at = crate::layout::Push {
                w: &w,
                l: &l,
                base,
                made: &made,
            };
            crate::layout::after_push(&st, at, &reviewed, s.user_id, &summary).await
        }
        false => crate::layout::Recorded::None,
    };
    log_change(
        &st,
        guild,
        s.user_id,
        &w.config,
        None,
        format!(
            "{who} pushed the channel setup: {done} of {n} step(s) done, {} failed, {named}{}",
            failed.len(),
            layout.log_text()
        ),
    )
    .await;
    Ok(
        Json(json!({"done": done, "failed": failed, "backup": backup, "layout": layout.reply()}))
            .into_response(),
    )
}

/// One new text channel made outside a draft (the Message Builder's "+ New channel"), with Channel
/// Setup's own rules: the same overwrites (quarantine deny, staff visibility, the bot let in), the
/// same safety check, and the push lock so it never runs beside a push. The caller checks the level.
pub(crate) async fn create_one(
    st: AppState,
    s: Sess,
    guild: i64,
    name: String,
    parent: Option<String>,
    staff_only: bool,
) -> Result<String, ApiError> {
    let lock = lock(&st, guild, true).await?;
    // A task, so the create finishes, is logged and releases the lock also when this request is dropped.
    tokio::spawn(async move {
        let r = create_locked(&st, &s, guild, &name, parent, staff_only).await;
        release(lock).await;
        r
    })
    .await?
}

async fn create_locked(
    st: &AppState,
    s: &Sess,
    guild: i64,
    name: &str,
    parent: Option<String>,
    staff_only: bool,
) -> Result<String, ApiError> {
    {
        // Under the push lock, so two creates at once cannot both pass the caps.
        let (hour, day): (i64, i64) = sqlx::query_as(
            "select count(*) filter (where made_at > now() - interval '1 hour'), count(*)
             from message_new_channels where guild_id = $1 and made_at > now() - interval '1 day'",
        )
        .bind(guild)
        .fetch_one(&st.db)
        .await?;
        if hour >= 5 || day >= 25 {
            return Err(ApiError::BadRequest(format!(
                "this server made {} channels from the Message Builder in the last {}: the limit is {}. Make it in Channel Setup, or wait",
                if hour >= 5 { hour } else { day },
                if hour >= 5 { "hour" } else { "day" },
                if hour >= 5 { 5 } else { 25 }
            )));
        }
        let (w, l) = world_live(st, guild).await?;
        let base = crate::layout::base(st, guild).await?;
        if let Some(p) = parent.as_deref().and_then(|p| p.parse::<u64>().ok()) {
            let ours = crate::tickets::categories(&w.config).contains(&p.to_string().as_str())
                || w.channels.iter().any(|c| {
                    c.parent == Some(p) && crate::embeds::bot_channel(&w.config, c.id as i64, &[])
                });
            if ours {
                return Err(ApiError::BadRequest(
                    "that category holds Brim's log, ticket, verification or appeal channels: pick another one".into(),
                ));
            }
        }
        let mut draft = Draft::default();
        draft.items.insert(
            "new-0".into(),
            Item {
                kind: Some(0),
                name: Some(name.into()),
                parent: Some(parent),
                access: Some(Access {
                    roles: vec![],
                    staff: true,
                    limited: Some(staff_only),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        // The draft's own key means nothing to the person who typed the name.
        validate(&mut draft, &w.channels, &w.ctx)
            .map_err(|e| ApiError::BadRequest(e.replace("new-0: ", "")))?;
        let tree = desired(&w.channels, &draft, &w.ctx);
        let issues = draft_issues(&w.channels, &tree, &w.ctx, &w.config);
        if let Some(i) = issues.first() {
            return Err(ApiError::BadRequest(format!(
                "Channel Setup's rules refuse this channel: {}. {}",
                i.title, i.detail
            )));
        }
        let mut made = HashMap::new();
        for step in plan(&w.channels, &tree) {
            match run(st, guild, &step, &made).await {
                Ok(Some((key, id))) => {
                    made.insert(key, id);
                }
                Ok(None) => {}
                // Once the channel exists, a later step (its place in the list) failing must not
                // read as "not made": a retry would make a second one.
                Err(e) if made.contains_key("new-0") => {
                    tracing::warn!("message builder new channel: {}: {e:#}", what(&step));
                    break;
                }
                Err(e) => return Err(ApiError::BadRequest(format!("{}: {e:#}", what(&step)))),
            }
        }
        let id = made
            .get("new-0")
            .cloned()
            .ok_or_else(|| ApiError::from(anyhow::anyhow!("the channel was not created")))?;
        let note = format!("Message Builder: new channel #{name}");
        let at = crate::layout::Push {
            w: &w,
            l: &l,
            base,
            made: &made,
        };
        let layout = crate::layout::after_push(st, at, &draft, s.user_id, &note).await;
        sqlx::query(
            "insert into message_new_channels (guild_id, channel_id, made_by) values ($1, $2, $3)",
        )
        .bind(guild)
        .bind(id.parse::<i64>().unwrap_or_default())
        .bind(s.user_id)
        .execute(&st.db)
        .await?;
        log_by(
            st,
            s,
            guild,
            &w.config,
            None,
            &format!(
                "made the channel <#{id}> from the Message Builder ({}){}",
                if staff_only { "only staff" } else { "everyone" },
                layout.log_text()
            ),
        )
        .await;
        Ok(id)
    }
}

/// Counts of what the steps do, with the names of the channels they delete.
fn outline(steps: &[Step], tree: &[Node]) -> String {
    let count = |f: fn(&Step) -> bool| steps.iter().filter(|s| f(s)).count();
    let moves: usize = steps
        .iter()
        .map(|s| match s {
            Step::Positions(m) => m.len(),
            _ => 0,
        })
        .sum();
    let gone: Vec<String> = steps
        .iter()
        .filter_map(|s| match s {
            Step::Delete { id } => tree
                .iter()
                .find(|n| n.img.id == *id)
                .map(|n| n.name.clone()),
            _ => None,
        })
        .collect();
    let roles = match count(|s| {
        matches!(
            s,
            Step::RoleCreate { .. } | Step::RoleEdit { .. } | Step::RoleDelete { .. }
        )
    }) + count(|s| matches!(s, Step::RoleOrder(_)))
    {
        0 => String::new(),
        n => format!(", {n} role change(s)"),
    };
    format!(
        "{} create(s), {} rename(s), {moves} move(s), {} permission change(s), {} delete(s){}{roles}{}",
        count(|s| matches!(s, Step::Create { .. })),
        count(|s| matches!(s, Step::Rename { .. })),
        count(|s| matches!(s, Step::Overwrite { .. } | Step::DropOverwrite { .. })),
        gone.len(),
        if gone.is_empty() {
            String::new()
        } else {
            format!(" ({})", gone.join(", "))
        },
        if count(|s| matches!(s, Step::Server(_))) > 0 {
            ", server settings"
        } else {
            ""
        }
    )
}

/// Runs the steps in Discord and stops at the first failure: the plan is diff-based, so the next
/// push resumes there. Created ids are written back into the draft as they happen.
/// Returns the steps done, the failures and the channels made (draft key -> new id).
async fn run_steps(
    st: AppState,
    guild: i64,
    user: i64,
    mut draft: Draft,
    steps: Vec<Step>,
) -> (usize, Vec<String>, HashMap<String, String>) {
    let (mut done, mut failed) = (0, vec![]);
    let mut made: HashMap<String, String> = HashMap::new();
    for step in &steps {
        match run(&st, guild, step, &made).await {
            Ok(Some((key, id))) => {
                remap(&mut draft, &key, &id);
                if let Some(n) = new_num(&key) {
                    draft.next = draft.next.max(n + 1);
                }
                made.insert(key, id);
                done += 1;
                // Without the new id in the store, the next push would create it again.
                if let Err(e) = save(&st, guild, &draft, user).await {
                    failed.push(format!("save the draft after {}: {e}", what(step)));
                    break;
                }
            }
            Ok(None) => done += 1,
            Err(e) => {
                failed.push(format!("{}: {e:#}", what(step)));
                break;
            }
        }
    }
    if failed.is_empty() {
        draft = Draft {
            next: draft.next,
            ..Default::default()
        };
    }
    if let Err(e) = save(&st, guild, &draft, user).await {
        failed.push(format!("save the draft: {e}"));
    }
    (done, failed, made)
}

fn what(step: &Step) -> String {
    match step {
        Step::Create { name, .. } => format!("create {name}"),
        Step::Rename { id, name } => format!("rename <#{id}> to {name}"),
        Step::Positions(m) => format!("move {} channel(s)", m.len()),
        Step::Overwrite { channel, ow } => format!("permissions of {} on <#{channel}>", ow.target),
        Step::DropOverwrite { channel, target } => {
            format!("remove permissions of {target} on <#{channel}>")
        }
        Step::Delete { id } => format!("delete <#{id}>"),
        Step::RoleCreate { body, .. } => format!("create role {}", body["name"]),
        Step::RoleEdit { id, .. } => format!("edit role <@&{id}>"),
        Step::RoleOrder(l) => format!("move {} role(s)", l.len()),
        Step::RoleDelete { id } => format!("delete role <@&{id}>"),
        Step::Server(_) => "server settings".into(),
    }
}

/// One step in Discord; a create returns (draft key, new id).
async fn run(
    st: &AppState,
    guild: i64,
    step: &Step,
    made: &HashMap<String, String>,
) -> anyhow::Result<Option<(String, String)>> {
    let resolve = |k: &str| -> anyhow::Result<String> {
        if is_new(k) {
            made.get(k)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("its category was not created"))
        } else {
            Ok(k.into())
        }
    };
    let call = |m: Method, path: String, body: Option<Value>| async move {
        st.discord.call(m, &path, body).await
    };
    match step {
        Step::Create {
            key,
            kind,
            name,
            parent,
            position,
            overwrites,
        } => {
            let parent = parent.as_deref().map(resolve).transpose()?;
            let ows: Vec<Value> = overwrites
                .iter()
                .map(|o| {
                    let mut b = overwrite_body(o);
                    b["id"] = json!(o.target.to_string());
                    b
                })
                .collect();
            let r = call(
                Method::POST,
                format!("/guilds/{guild}/channels"),
                Some(json!({"name": name, "type": kind, "parent_id": parent, "position": position, "permission_overwrites": ows})),
            )
            .await?;
            let id = r["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("discord returned no id"))?;
            return Ok(Some((key.clone(), id.into())));
        }
        Step::Rename { id, name } => {
            call(
                Method::PATCH,
                format!("/channels/{id}"),
                Some(json!({"name": name})),
            )
            .await?;
        }
        Step::Positions(moves) => {
            let mut body = vec![];
            for m in moves {
                let mut e = json!({"id": m.id.to_string(), "position": m.position});
                if let Some(p) = &m.parent {
                    e["parent_id"] = json!(p.as_deref().map(resolve).transpose()?);
                }
                body.push(e);
            }
            call(
                Method::PATCH,
                format!("/guilds/{guild}/channels"),
                Some(json!(body)),
            )
            .await?;
        }
        Step::Overwrite { channel, ow } => {
            call(
                Method::PUT,
                format!("/channels/{channel}/permissions/{}", ow.target),
                Some(overwrite_body(ow)),
            )
            .await?;
        }
        Step::DropOverwrite { channel, target } => {
            call(
                Method::DELETE,
                format!("/channels/{channel}/permissions/{target}"),
                None,
            )
            .await?;
        }
        Step::Delete { id } => {
            call(Method::DELETE, format!("/channels/{id}"), None).await?;
        }
        Step::RoleCreate { key, body } => {
            let r = call(
                Method::POST,
                format!("/guilds/{guild}/roles"),
                Some(body.clone()),
            )
            .await?;
            let id = r["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("discord returned no id"))?;
            return Ok(Some((key.clone(), id.into())));
        }
        Step::RoleEdit { id, body } => {
            call(
                Method::PATCH,
                format!("/guilds/{guild}/roles/{id}"),
                Some(body.clone()),
            )
            .await?;
        }
        Step::RoleOrder(list) => {
            let mut body = vec![];
            for (k, position) in list {
                body.push(json!({"id": resolve(k)?, "position": position}));
            }
            call(
                Method::PATCH,
                format!("/guilds/{guild}/roles"),
                Some(json!(body)),
            )
            .await?;
        }
        Step::RoleDelete { id } => {
            call(Method::DELETE, format!("/guilds/{guild}/roles/{id}"), None).await?;
        }
        Step::Server(body) => {
            let g = call(
                Method::PATCH,
                format!("/guilds/{guild}"),
                Some(body.clone()),
            )
            .await?;
            // The layout keeps the icon's new hash. Discord answers 200 with no hash for image
            // data it cannot use (PITFALLS.md), so a new icon without one is a failure.
            if let Some(icon) = body.get("icon") {
                let hash = g["icon"].as_str().unwrap_or_default();
                if icon.is_string() && hash.is_empty() {
                    anyhow::bail!("Discord did not take the icon");
                }
                return Ok(Some((ICON.into(), hash.into())));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const G: u64 = 1;
    const STAFF: u64 = 50;
    const MAIN: u64 = 99;
    const Q: u64 = 60;
    const MANAGE_CHANNELS: u64 = 0x10;

    fn role(id: u64, permissions: u64) -> RoleImg {
        RoleImg {
            id,
            name: format!("r{id}"),
            color: 0,
            permissions,
            position: 1,
            hoist: false,
            mentionable: false,
            managed: false,
        }
    }

    fn ow(target: u64, allow: u64, deny: u64) -> OverwriteImg {
        OverwriteImg {
            target,
            is_role: true,
            allow,
            deny,
        }
    }

    fn ch(id: u64, kind: u8, parent: Option<u64>, overwrites: Vec<OverwriteImg>) -> ChannelImg {
        ChannelImg {
            id,
            name: format!("c{id}"),
            kind,
            parent,
            position: id as i64,
            topic: None,
            nsfw: false,
            overwrites,
        }
    }

    pub(crate) fn ctx() -> Ctx {
        Ctx {
            guild: G,
            bot: 7,
            staff: vec![STAFF],
            levels: vec![],
            admins: vec![STAFF],
            owners: vec![],
            locked: false,
            roles: [G, 5, 6, STAFF, MAIN, Q]
                .map(|r| role(r, if r == G { VIEW } else { 0 }))
                .to_vec(),
            setup: Setup {
                quarantine_role: Some(Q),
                log_fallback: true,
                unwhitelisted_staff: vec![],
                bot_roles: vec![],
                mod_log: true,
                modules: vec![],
                main_roles: vec![MAIN],
                mod_roles: vec![],
                trusted: vec![7],
                log_channels: vec![13],
                bot: 7,
                verification_channel: None,
                verification_roles: vec![],
                staff_roles: vec![],
            },
            bot_pos: 10,
        }
    }

    /// 10 a category with synced 11 and exception 12; 20 a category with the quarantine deny;
    /// 13 a public log channel.
    fn live() -> Vec<ChannelImg> {
        vec![
            ch(10, CATEGORY, None, vec![]),
            ch(11, 0, Some(10), vec![]),
            ch(12, 0, Some(10), vec![ow(5, VIEW, 0)]),
            ch(20, CATEGORY, None, vec![ow(Q, 0, QUARANTINE_DENY)]),
            ch(13, 0, None, vec![]),
        ]
    }

    fn draft(v: Value) -> Draft {
        serde_json::from_value(json!({ "items": v })).unwrap()
    }

    fn node<'a>(nodes: &'a [Node], id: &str) -> &'a Node {
        nodes.iter().find(|n| n.id == id).unwrap()
    }

    #[test]
    fn draft_json_keeps_null_parent_apart_and_refuses_unknown_fields() {
        let d = draft(json!({"11": {"parent": null}, "12": {"name": "x"}}));
        assert_eq!(d.items["11"].parent, Some(None));
        assert_eq!(d.items["12"].parent, None);
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            json!({"items": {"11": {"parent": null}, "12": {"name": "x"}}})
        );
        assert!(serde_json::from_value::<Draft>(json!({"items": {"11": {"perms": 8}}})).is_err());
    }

    #[test]
    fn a_moved_channel_follows_its_new_category_unless_it_keeps_its_own() {
        let c = ctx();
        let follows = desired(&live(), &draft(json!({"11": {"parent": "20"}})), &c);
        assert_eq!(
            node(&follows, "11").img.overwrites,
            vec![ow(Q, 0, QUARANTINE_DENY)]
        );
        assert!(!node(&follows, "11").exception);
        let kept = desired(
            &live(),
            &draft(json!({"11": {"parent": "20", "keep": true}})),
            &c,
        );
        // It keeps what it had (nothing), not the new category's overwrites.
        assert!(node(&kept, "11").exception);
        assert!(node(&kept, "11").img.overwrites.is_empty());
    }

    #[test]
    fn a_feature_channel_keeps_its_permissions_whatever_the_draft_says() {
        let w = World {
            config: json!({"tickets": {"category": "10"}}),
            ctx: ctx(),
            channels: live(),
            permit: vec![],
            settings: Default::default(),
            raw: (json!([]), json!([])),
            temp: Default::default(),
        };
        let mut d = draft(json!({"11": {"parent": "20", "reset": true}, "13": {"reset": true}}));
        hold_fixed(&mut d, &w);
        // 11 is in the ticket category in Discord: it moves but keeps its own.
        assert!(d.items["11"].keep && !d.items["11"].reset);
        assert!(d.items["13"].reset, "not a feature's channel");
    }

    #[test]
    fn an_open_tickets_channel_is_not_renamed_moved_or_deleted_by_a_draft() {
        let w = World {
            config: json!({}),
            ctx: ctx(),
            channels: live(),
            permit: vec![],
            settings: Default::default(),
            raw: (json!([]), json!([])),
            temp: crate::layout::Temp {
                open: HashSet::from([11]),
                ..Default::default()
            },
        };
        let access = json!({"roles": [], "staff": true, "limited": true});
        let mut d = draft(json!({
            "11": {"name": "x", "parent": "20", "position": 3, "delete": true, "access": access},
            "13": {"name": "y"}
        }));
        hold_fixed(&mut d, &w);
        let t = &d.items["11"];
        assert!(t.name.is_none() && t.parent.is_none() && t.position.is_none() && !t.delete);
        assert!(t.access.is_some(), "who can see it stays editable");
        assert_eq!(
            d.items["13"].name.as_deref(),
            Some("y"),
            "not a ticket channel"
        );
    }

    #[test]
    fn read_only_admins_only_and_the_grid() {
        let mut c = ctx();
        // 13 read-only: members (the main role) read, do not write; staff write.
        let n = desired(
            &live(),
            &draft(json!({"13": {"access": {"read_only": true}}})),
            &c,
        );
        let ows = &node(&n, "13").img.overwrites;
        let of = |t: u64| ows.iter().find(|o| o.target == t).cloned().unwrap();
        assert_eq!(of(MAIN).deny & (SEND | THREADS), SEND | THREADS);
        assert_eq!(of(STAFF).allow & SEND, SEND);
        assert!(node(&n, "13").access.read_only);
        assert_eq!(node(&n, "13").tag, Some("Members read only"));
        // Public again takes the members' write deny away.
        let back = desired(
            &n.iter()
                .filter(|x| !x.new)
                .map(|x| x.img.clone())
                .collect::<Vec<_>>(),
            &draft(json!({"13": {"access": {}}})),
            &c,
        );
        assert!(!node(&back, "13").access.read_only);
        // Admins only: a Moderators-only role (6) is kept out.
        c.staff = vec![STAFF, 6];
        c.setup.mod_roles = vec![6];
        let a = desired(
            &live(),
            &draft(json!({"13": {"access": {"limited": true, "only": "admins"}}})),
            &c,
        );
        let ows = &node(&a, "13").img.overwrites;
        assert!(ows.iter().any(|o| o.target == STAFF && o.allow & VIEW != 0));
        assert!(!ows.iter().any(|o| o.target == 6 && o.allow & VIEW != 0));
        assert_eq!(node(&a, "13").access.only.as_deref(), Some("admins"));
        assert_eq!(node(&a, "13").tag, Some("Admins"));
        assert!(
            unstaffed(&a, &live(), &c).contains("13"),
            "Admins only needs an owner"
        );
        // Owner only needs an Owner role; a grid takes harmless bits of known roles only.
        let mut bad = draft(json!({"13": {"access": {"limited": true, "only": "owner"}}}));
        assert!(validate(&mut bad, &live(), &c).is_err());
        let mut bad = draft(json!({"13": {"grid": {"5": [8, 0]}}}));
        assert!(
            validate(&mut bad, &live(), &c).is_err(),
            "Administrator is not in the grid"
        );
        let mut ok = draft(json!({"13": {"grid": {"5": [SEND, VIEW]}}}));
        assert!(validate(&mut ok, &live(), &c).is_ok());
        let g = desired(&live(), &ok, &c);
        let o = node(&g, "13")
            .img
            .overwrites
            .iter()
            .find(|o| o.target == 5)
            .cloned()
            .unwrap();
        assert_eq!((o.allow, o.deny), (SEND, VIEW));
        assert_eq!(node(&g, "13").grid, vec![("5".to_string(), SEND, VIEW)]);
        // The grid cannot set the quarantine role, let @everyone in past verification, or touch
        // the verification channel.
        for bad in [
            json!({"13": {"grid": {Q.to_string(): [VIEW, 0]}}}),
            json!({"13": {"grid": {G.to_string(): [VIEW, 0]}}}),
        ] {
            assert!(validate(&mut draft(bad), &live(), &c).is_err());
        }
        c.setup.verification_channel = Some(13);
        assert!(validate(&mut ok.clone(), &live(), &c).is_err());
        c.setup.verification_channel = None;
        // A staff role's View deny in the grid counts as hiding the channel from staff.
        let hide = draft(json!({"13": {"grid": {STAFF.to_string(): [0, VIEW]}}}));
        assert!(unstaffed(&desired(&live(), &hide, &c), &live(), &c).contains("13"));
        // During a lock, Public leaves the members' Send deny alone.
        let mut l = live();
        let i = l.iter().position(|x| x.id == 13).unwrap();
        l[i].overwrites.push(ow(MAIN, 0, SEND));
        c.locked = true;
        let n = desired(&l, &draft(json!({"13": {"access": {}}})), &c);
        let o = node(&n, "13")
            .img
            .overwrites
            .iter()
            .find(|o| o.target == MAIN)
            .cloned()
            .unwrap();
        assert_eq!(o.deny & SEND, SEND);
    }

    #[test]
    fn feature_channels_are_fixed() {
        let cfg = json!({"verification": {"channel": "13"}, "tickets": {"category": "20", "panels": [{"category": "21"}, {"category": null}]}, "catch": {"channel": "9"}});
        assert_eq!(fixed_by(&cfg, "13", None), Some("Verification"));
        assert_eq!(fixed_by(&cfg, "11", Some("20")), Some("Tickets"));
        assert_eq!(fixed_by(&cfg, "9", None), Some("Catch"));
        assert_eq!(fixed_by(&cfg, "11", Some("10")), None);
        // A panel's own category holds ticket channels too.
        assert_eq!(fixed_by(&cfg, "11", Some("21")), Some("Tickets"));
    }

    #[test]
    fn category_change_skips_exceptions_and_reset_follows_again() {
        let (live, ctx) = (live(), ctx());
        let d = draft(json!({"10": {"access": {"roles": ["6"]}}}));
        let n = desired(&live, &d, &ctx);
        let cat = node(&n, "10");
        assert_eq!(
            (
                cat.access.is_limited(),
                cat.access.roles.clone(),
                cat.access.staff
            ),
            (true, vec!["6".to_string()], true)
        );
        assert_eq!(node(&n, "11").img.overwrites, cat.img.overwrites);
        assert!(!node(&n, "11").exception);
        let ex = node(&n, "12");
        assert!(ex.exception && !ex.access.is_limited());
        assert_eq!(ex.img.overwrites, live[2].overwrites);
        // Reset to category: the exception takes the category's overwrites.
        let d = draft(json!({"10": {"access": {"roles": ["6"]}}, "12": {"reset": true}}));
        let n = desired(&live, &d, &ctx);
        assert_eq!(node(&n, "12").img.overwrites, node(&n, "10").img.overwrites);
        assert!(!node(&n, "12").exception);
        // Back to Everyone: the main role (99) sees it, @everyone (not verified) still does not,
        // other bits and targets stay (owner decision 11-18: opening never skips verification).
        let mut locked = live.clone();
        locked[0].overwrites = vec![ow(G, 0, VIEW | 0x800), ow(MAIN, 0, VIEW), ow(6, VIEW, 0)];
        let n = desired(
            &locked,
            &draft(json!({"10": {"access": {"roles": []}}})),
            &ctx,
        );
        assert_eq!(
            node(&n, "10").img.overwrites,
            // Public clears @everyone's Send deny too (or it would still read as read-only).
            vec![ow(MAIN, VIEW, 0), ow(6, VIEW, 0), ow(G, 0, VIEW)]
        );
    }

    /// Owner decision 2026-11-18: a members' channel on a server with verification (@everyone denied,
    /// the main role let in) has no badge; Information reads "Members read only", the
    /// verification channel "Unverified", a staff-only one "Staff", a custom role its name.
    #[test]
    fn badges_tell_who_uses_a_channel() {
        let mut ctx = ctx();
        ctx.setup.verification_channel = Some(34);
        ctx.levels = vec![5];
        let live = vec![
            ch(30, 0, None, vec![ow(G, 0, VIEW), ow(MAIN, VIEW, 0)]),
            ch(
                31,
                0,
                None,
                vec![ow(G, 0, VIEW), ow(MAIN, VIEW | (1 << 16), SEND)],
            ),
            ch(
                32,
                0,
                None,
                vec![ow(G, 0, VIEW), ow(MAIN, 0, VIEW), ow(5, VIEW, 0)],
            ),
            ch(
                33,
                0,
                None,
                vec![
                    ow(G, 0, VIEW),
                    ow(MAIN, 0, VIEW),
                    ow(5, VIEW, 0),
                    ow(6, VIEW, 0),
                ],
            ),
            ch(34, 0, None, vec![ow(MAIN, 0, VIEW)]),
        ];
        let n = desired(&live, &draft(json!({})), &ctx);
        let tag = |id| (node(&n, id).tag, node(&n, id).tag_roles.clone());
        assert!(!node(&n, "30").access.is_limited());
        assert_eq!(tag("30"), (None, vec![]));
        assert_eq!(tag("31"), (Some("Members read only"), vec![]));
        assert_eq!(tag("32"), (Some("Staff"), vec![]));
        assert_eq!(tag("33"), (None, vec!["6".to_string()]));
        assert_eq!(tag("34"), (Some("Unverified"), vec![]));
        // "Everyone" on the verification channel keeps it open to @everyone and the main role
        // out; on another channel the main role allow comes before @everyone's deny.
        let d = draft(
            json!({"34": {"access": {"roles": [], "limited": false}}, "32": {"access": {"roles": [], "limited": false}}}),
        );
        let n = desired(&live, &d, &ctx);
        let v = &node(&n, "34").img.overwrites;
        assert!(
            v.iter().any(|o| o.target == MAIN && o.deny & VIEW != 0),
            "{v:?}"
        );
        assert!(
            !v.iter().any(|o| o.target == G && o.deny & VIEW != 0),
            "{v:?}"
        );
        let o = &node(&n, "32").img.overwrites;
        let at = |t| o.iter().position(|x| x.target == t).unwrap();
        assert!(
            at(MAIN) < at(G) && o[at(MAIN)].allow & VIEW != 0 && o[at(G)].deny & VIEW != 0,
            "{o:?}"
        );
    }

    #[test]
    fn new_channel_inherits_and_gets_the_quarantine_deny() {
        let (live, ctx) = (live(), ctx());
        let mut d = draft(json!({
            "new-1": {"kind": 0, "name": "My  Chat", "parent": "10"},
            "new-2": {"kind": 0, "name": "x", "parent": "20"},
            "new-3": {"kind": 2, "name": "Voice Room"},
        }));
        validate(&mut d, &live, &ctx).unwrap();
        assert_eq!(d.items["new-1"].name.as_deref(), Some("my-chat"));
        assert_eq!(d.items["new-3"].name.as_deref(), Some("Voice Room"));
        let n = desired(&live, &d, &ctx);
        let qdeny = ow(Q, 0, QUARANTINE_DENY);
        assert_eq!(node(&n, "new-1").img.overwrites, vec![qdeny.clone()]);
        assert!(node(&n, "new-1").exception, "10 lacks the quarantine deny");
        assert!(!node(&n, "new-2").exception && node(&n, "new-2").new);
        assert_eq!(node(&n, "new-3").img.overwrites, vec![qdeny]);
    }

    #[test]
    fn the_verification_channel_keeps_its_shape_when_it_follows_a_category() {
        use brim_core::sections::{VERIFY_ALLOW, VERIFY_DENY};
        let (live, mut ctx) = (live(), ctx());
        ctx.setup.verification_channel = Some(11);
        let d = draft(json!({"11": {"parent": "20"}, "12": {"parent": "20"}}));
        let n = desired(&live, &d, &ctx);
        assert_eq!(
            node(&n, "11").img.overwrites,
            vec![ow(Q, VERIFY_ALLOW, VERIFY_DENY)]
        );
        let other = &node(&n, "12").img.overwrites;
        assert!(
            !other.iter().any(|o| o.allow == VERIFY_ALLOW),
            "only the verification channel"
        );
    }

    #[test]
    fn a_synced_channel_follows_its_new_category_an_exception_keeps_its_own() {
        let (live, ctx) = (live(), ctx());
        let d = draft(json!({"11": {"parent": "20"}, "12": {"parent": "20"}}));
        let n = desired(&live, &d, &ctx);
        assert_eq!(node(&n, "11").img.overwrites, live[3].overwrites);
        assert!(node(&n, "11").moved && !node(&n, "11").exception);
        assert_eq!(node(&n, "12").img.overwrites, live[2].overwrites);
        assert!(node(&n, "12").exception);
    }

    #[test]
    fn staff_off_needs_an_owner_unless_it_is_off_already() {
        let (mut live, ctx) = (live(), ctx());
        let hidden = |d: Value, live: &[ChannelImg]| {
            let mut v: Vec<String> = unstaffed(&desired(live, &draft(d), &ctx), live, &ctx)
                .into_iter()
                .collect();
            v.sort();
            v
        };
        let off = json!({"access": {"roles": [], "limited": true, "staff": false}});
        // The category and its synced channel 11; exception 12 keeps staff.
        assert_eq!(hidden(json!({"10": off.clone()}), &live), ["10", "11"]);
        let new = json!({"kind": 0, "name": "x", "access": off["access"]});
        assert_eq!(hidden(json!({"new-1": new}), &live), ["new-1"]);
        let on = json!({"access": {"roles": ["6"], "staff": true}});
        assert!(hidden(json!({"10": on}), &live).is_empty());
        // Already hidden from staff in Discord: no owner needed, but moving 13 into it is.
        live[0].overwrites = vec![ow(G, 0, VIEW)];
        live[1].overwrites = live[0].overwrites.clone();
        assert!(hidden(json!({"10": off}), &live).is_empty());
        assert_eq!(
            hidden(json!({"13": {"parent": "10", "reset": true}}), &live),
            ["13"]
        );
    }

    #[test]
    fn deleting_a_category_with_channels_or_a_used_channel_is_refused() {
        let (live, ctx) = (live(), ctx());
        let mut d = draft(json!({"10": {"delete": true}}));
        let e = validate(&mut d, &live, &ctx).unwrap_err();
        assert_eq!(e, "move or delete its channels first");
        let mut d =
            draft(json!({"10": {"delete": true}, "11": {"delete": true}, "12": {"parent": null}}));
        validate(&mut d, &live, &ctx).unwrap();
        let mut d = draft(json!({"new-1": {"kind": 0, "name": "x", "parent": "11"}}));
        assert!(validate(&mut d, &live, &ctx).is_err(), "11 is no category");
        let mut d = draft(json!({"11": {"access": {"roles": ["999"]}}}));
        assert!(validate(&mut d, &live, &ctx).is_err(), "not a role here");
        let mut d = draft(json!({"11": {"kind": 0}}));
        assert!(
            validate(&mut d, &live, &ctx).is_err(),
            "kind is for new ones"
        );
        // The config's log channel cannot go.
        let config = json!({"logging": {"fallback": "11"}});
        let n = desired(&live, &draft(json!({"11": {"delete": true}})), &ctx);
        let i = issues(&n, &ctx, &config);
        let used = i.iter().find(|i| i.channel == "11").unwrap();
        assert_eq!(
            used.detail,
            "#c11 is used by Logging; pick another channel first."
        );
        assert!(used.fix.is_none());
    }

    #[test]
    fn push_only_stops_for_what_the_draft_changes() {
        let (live, ctx) = (live(), ctx());
        // #13 is a public log channel already; changing something else does not stop Push.
        let d = draft(json!({"11": {"name": "chat"}}));
        assert!(draft_issues(&live, &desired(&live, &d, &ctx), &ctx, &json!({})).is_empty());
        // Touching #13 does.
        let d = draft(json!({"13": {"name": "logs"}}));
        let i = draft_issues(&live, &desired(&live, &d, &ctx), &ctx, &json!({}));
        let ids: Vec<_> = i.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["log_channel_public_13"]);
    }

    #[test]
    fn a_limited_category_is_no_overview_problem() {
        // What Channel Setup writes for "only these roles" must pass the Overview's own rules.
        let (live, ctx) = (live(), ctx());
        let d = draft(json!({"10": {"access": {"roles": ["6"], "staff": true}}}));
        let n = desired(&live, &d, &ctx);
        let imgs: Vec<ChannelImg> = n.iter().map(|n| n.img.clone()).collect();
        let g = GuildSafety {
            id: ctx.guild,
            mfa: 1,
            verification: 2,
            content_filter: 2,
            notifications: 1,
        };
        let rows = security::checks(&g, &ctx.roles, &imgs, ctx.bot_pos, &ctx.setup);
        assert!(!rows.iter().any(|r| r.id.starts_with("overwrite_10")
            || r.id.starts_with("overwrite_11")
            || r.id.starts_with("overwrite_12")));
    }

    /// A channel hidden by hand before Channel Setup limited it still gets the bot member's View:
    /// with its staff role deleted in Discord it would otherwise read as hidden by a failed run,
    /// and Overview Fix would open it to members (owner decision 11-18).
    #[test]
    fn limiting_an_already_hidden_channel_marks_it_with_the_bot() {
        let ctx = ctx();
        let live = vec![ch(30, 0, None, vec![ow(G, 0, VIEW), ow(STAFF, VIEW, 0)])];
        let d = draft(json!({"30": {"access": {"roles": [], "limited": true, "staff": true}}}));
        let n = desired(&live, &d, &ctx);
        let bot = node(&n, "30")
            .img
            .overwrites
            .iter()
            .find(|o| !o.is_role && o.target == ctx.bot)
            .cloned();
        assert!(
            bot.is_some_and(|o| o.allow & VIEW != 0),
            "{:?}",
            node(&n, "30").img.overwrites
        );
    }

    #[test]
    fn a_public_log_channel_is_an_issue_until_fixed() {
        let (live, ctx) = (live(), ctx());
        let n = desired(&live, &Draft::default(), &ctx);
        let i = issues(&n, &ctx, &json!({}));
        assert_eq!(i.len(), 1);
        assert_eq!(i[0].id, "log_channel_public_13");
        let fix = i[0].fix.clone().unwrap().access.unwrap();
        assert_eq!(
            fix,
            Access {
                roles: vec![],
                staff: true,
                limited: Some(true),
                ..Default::default()
            }
        );
        let item = Item {
            access: Some(fix),
            ..Default::default()
        };
        let d = Draft {
            items: [("13".to_string(), item)].into(),
            ..Default::default()
        };
        let n = desired(&live, &d, &ctx);
        assert!(issues(&n, &ctx, &json!({})).is_empty());
        let a = &node(&n, "13").access;
        assert!(a.is_limited() && a.staff && a.roles.is_empty());
    }

    #[test]
    fn a_dangerous_overwrite_is_an_issue_a_member_one_only_stops_push() {
        let (mut live, mut ctx) = (live(), ctx());
        ctx.setup.log_channels.clear();
        live.push(ch(14, 0, None, vec![ow(5, MANAGE_CHANNELS | VIEW, 0)]));
        let member = OverwriteImg {
            is_role: false,
            ..ow(8, MANAGE_CHANNELS, 0)
        };
        live.push(ch(15, 0, None, vec![member]));
        let n = desired(&live, &Draft::default(), &ctx);
        let i = issues(&n, &ctx, &json!({}));
        assert_eq!(i.len(), 2);
        assert_eq!(
            (i[0].id.as_str(), i[0].channel.as_str(), i[0].member),
            ("overwrite_14_5", "14", false)
        );
        assert_eq!(i[0].fix.as_ref().unwrap().strip.as_deref(), Some("5"));
        // The member overwrite stops Push too; only the pulse (view) leaves it out.
        assert_eq!((i[1].id.as_str(), i[1].member), ("overwrite_15_8", true));
        let n = desired(
            &live,
            &draft(json!({"14": {"strip": ["5"]}, "15": {"strip": ["8"]}})),
            &ctx,
        );
        assert!(issues(&n, &ctx, &json!({})).is_empty());
        assert_eq!(node(&n, "14").img.overwrites, vec![ow(5, VIEW, 0)]);
    }

    #[test]
    fn a_swap_sends_index_positions_for_the_whole_category() {
        let ctx = ctx();
        let mut raw = live();
        raw[1].position = 40; // Discord positions have gaps
        raw[2].position = 41;
        let live = normalized(raw);
        let pos = |id: u64| live.iter().find(|c| c.id == id).unwrap().position;
        assert_eq!(
            (pos(11), pos(12), pos(10), pos(20), pos(13)),
            (0, 1, 0, 1, 0)
        );
        let n = desired(
            &live,
            &draft(json!({"12": {"position": 0}, "11": {"position": 1}})),
            &ctx,
        );
        let Some(Step::Positions(m)) = plan(&live, &n)
            .into_iter()
            .find(|s| matches!(s, Step::Positions(_)))
        else {
            panic!("no positions step");
        };
        let mut got: Vec<_> = m.iter().map(|m| (m.id, m.position)).collect();
        got.sort();
        assert_eq!(got, [(11, 1), (12, 0)]);
        assert!(plan(&live, &desired(&live, &Draft::default(), &ctx)).is_empty());
    }

    #[test]
    fn a_resumed_push_still_moves_a_child_whose_category_went_through() {
        let (live, ctx) = (live(), ctx());
        let mut d = draft(json!({"10": {"access": {"roles": ["6"]}}}));
        let n = desired(&live, &d, &ctx);
        pin_follows(&mut d, &n);
        assert!(d.items["11"].reset, "11 follows 10 in live");
        assert!(!d.items.contains_key("12"), "12 is an exception");
        // The category's overwrites were pushed, 11's step failed.
        let mut half = live.clone();
        half[0].overwrites = node(&n, "10").img.overwrites.clone();
        let rest = plan(&half, &desired(&half, &d, &ctx));
        assert!(
            rest.iter()
                .any(|s| matches!(s, Step::Overwrite { channel: 11, .. })),
            "{rest:?}"
        );
        assert!(rest.iter().all(|s| !matches!(
            s,
            Step::Overwrite {
                channel: 10 | 12,
                ..
            }
        )));
    }

    #[test]
    fn a_new_channel_gives_its_siblings_index_positions() {
        let ctx = ctx();
        let mut raw = live();
        raw[1].position = 40;
        raw[2].position = 41;
        let live = normalized(raw);
        let d = draft(json!({"new-1": {"kind": 0, "name": "x", "parent": "10", "position": 2}}));
        let Some(Step::Positions(m)) = plan(&live, &desired(&live, &d, &ctx))
            .into_iter()
            .find(|s| matches!(s, Step::Positions(_)))
        else {
            panic!("no positions step");
        };
        let mut got: Vec<_> = m.iter().map(|m| (m.id, m.position)).collect();
        got.sort();
        assert_eq!(got, [(11, 0), (12, 1)]);
    }

    #[test]
    fn without_staff_roles_newly_limiting_needs_an_owner() {
        let (mut live, mut ctx) = (live(), ctx());
        ctx.staff.clear();
        let hidden =
            |d: Value, live: &[ChannelImg]| unstaffed(&desired(live, &draft(d), &ctx), live, &ctx);
        let on = json!({"access": {"roles": ["6"], "staff": true}});
        assert!(hidden(json!({"13": on.clone()}), &live).contains("13"));
        assert!(hidden(json!({"13": {"name": "x"}}), &live).is_empty());
        // Limited in Discord already: not new.
        live[4].overwrites = vec![ow(G, 0, VIEW)];
        assert!(hidden(json!({"13": on}), &live).is_empty());
        match unstaff_refusal(&ctx) {
            ApiError::Forbidden(m) => {
                assert!(m.contains("Set roles for the Admins and Moderators"))
            }
            _ => panic!(),
        }
    }

    #[test]
    fn quarantine_bot_main_and_managed_roles_cannot_be_picked_and_the_deny_stays() {
        let (mut live, mut ctx) = (live(), ctx());
        ctx.setup.bot_roles = vec![5];
        ctx.roles.push(RoleImg {
            managed: true,
            ..role(8, 0)
        });
        for r in [Q, 5, G, 8, MAIN] {
            let mut d = draft(json!({"11": {"access": {"roles": [r.to_string()]}}}));
            assert!(validate(&mut d, &live, &ctx).is_err(), "role {r}");
        }
        let mut d = draft(json!({"11": {"access": {"roles": ["6"]}}}));
        validate(&mut d, &live, &ctx).unwrap();
        // 14 has the deny as an exception in 10, 21 is synced with 20: reset and move keep it.
        let qdeny = ow(Q, 0, QUARANTINE_DENY);
        live.push(ch(14, 0, Some(10), vec![qdeny.clone()]));
        live.push(ch(21, 0, Some(20), vec![qdeny.clone()]));
        let d = draft(json!({"14": {"reset": true}, "21": {"parent": "10"}}));
        let n = desired(&live, &d, &ctx);
        assert_eq!(node(&n, "14").img.overwrites, vec![qdeny.clone()]);
        assert_eq!(node(&n, "21").img.overwrites, vec![qdeny]);
    }

    #[test]
    fn plan_runs_in_order_and_is_empty_once_applied() {
        let (live, ctx) = (live(), ctx());
        let mut d = draft(json!({
            "new-1": {"kind": 4, "name": "Events"},
            "new-2": {"kind": 0, "name": "news", "parent": "new-1"},
            "10": {"access": {"roles": ["6"]}},
            "11": {"name": "renamed"},
            "12": {"parent": "new-1", "position": 0},
            "13": {"delete": true},
        }));
        validate(&mut d, &live, &ctx).unwrap();
        let n = desired(&live, &d, &ctx);
        let steps = plan(&live, &n);
        let rank = |s: &Step| match s {
            Step::Create { kind, .. } if *kind == CATEGORY => 0,
            Step::Create { .. } => 1,
            Step::Rename { .. } => 2,
            Step::Positions(_) => 3,
            Step::Overwrite { .. } | Step::DropOverwrite { .. } => 4,
            Step::Delete { .. } => 5,
            _ => 6,
        };
        let order: Vec<_> = steps.iter().map(rank).collect();
        assert!(order.windows(2).all(|w| w[0] <= w[1]), "{order:?}");
        assert_eq!(order.iter().filter(|k| **k == 5).count(), 1);
        let moved = Move {
            id: 12,
            position: 0,
            parent: Some(Some("new-1".into())),
        };
        // New channels give their groups index positions too (here the categories).
        assert!(steps
            .iter()
            .any(|s| matches!(s, Step::Positions(m) if m.contains(&moved))));
        // Only the creates went through: the rest is still planned, nothing is created twice.
        let made: HashMap<u64, u64> = n
            .iter()
            .filter(|x| x.new)
            .enumerate()
            .map(|(i, x)| (x.img.id, 1000 + i as u64))
            .collect();
        let mut d2 = d.clone();
        for x in n.iter().filter(|x| x.new) {
            remap(&mut d2, &x.id, &made[&x.img.id].to_string());
        }
        let as_live = |x: &Node| {
            let mut c = x.img.clone();
            c.id = *made.get(&c.id).unwrap_or(&c.id);
            c.parent = c.parent.map(|p| *made.get(&p).unwrap_or(&p));
            c
        };
        let mut half = live.clone();
        half.extend(n.iter().filter(|x| x.new).map(as_live));
        let rest = plan(&half, &desired(&half, &d2, &ctx));
        assert!(rest.iter().all(|s| !matches!(s, Step::Create { .. })));
        assert_eq!(rest.len(), steps.len() - 2);
        // Everything went through: nothing is left.
        let all: Vec<ChannelImg> = n.iter().filter(|x| !x.deleted).map(as_live).collect();
        assert!(plan(&all, &desired(&all, &d2, &ctx)).is_empty());
    }
}

#[cfg(test)]
mod db_tests {
    use crate::{auth::Sess, discord::Discord, AppState};
    use axum::{body::Body, http::Request, routing::get, Json, Router};
    use brim_antinuke::{ChannelImg, OverwriteImg};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    fn ow(target: u64, allow: u64) -> OverwriteImg {
        OverwriteImg {
            target,
            is_role: true,
            allow,
            deny: 0,
        }
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. Discord is a local mock.
    #[tokio::test]
    async fn draft_is_saved_discarded_and_pushed_only_when_safe() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_600_000_000 + std::process::id() as i64;
        let owner = 7_810_000_000 + std::process::id() as i64;
        let calls: Arc<Mutex<Vec<String>>> = Default::default();
        let log = |c: &Arc<Mutex<Vec<String>>>, s: String| c.lock().unwrap().push(s);
        let (c1, c2, c3, c4) = (calls.clone(), calls.clone(), calls.clone(), calls.clone());
        // A role overwrite that grants Manage Channels: Push must refuse until it is stripped.
        let channels = json!([{"id": "300", "name": "chat", "type": 0, "position": 0,
            "permission_overwrites": [{"id": "5", "type": 0, "allow": "16", "deny": "0"}]}]);
        let mock = Router::new()
            .route(
                "/guilds/{g}",
                get(move || async move { Json(json!({"owner_id": owner.to_string(), "mfa_level": 1})) }),
            )
            .route(
                "/guilds/{g}/roles",
                get(move || async move {
                    Json(json!([{"id": g.to_string(), "name": "@everyone", "permissions": "1024", "position": 0},
                        {"id": "5", "name": "Mods", "permissions": "0", "position": 1}]))
                }),
            )
            .route(
                "/guilds/{g}/channels",
                get(move || async move { Json(channels) }).post(move |Json(b): Json<Value>| async move {
                    log(&c1, format!("create {}", b["name"]));
                    Json(json!({"id": "301"}))
                }).patch(move |Json(b): Json<Value>| async move {
                    log(&c3, format!("positions {b}"));
                    Json(json!({}))
                }),
            )
            .route("/users/@me", get(|| async { Json(json!({"id": "7"})) }))
            .route(
                "/channels/{c}",
                axum::routing::patch(move |Json(b): Json<Value>| async move {
                    // "b" stands in for a rename Discord refuses.
                    if b["name"] == "b" {
                        return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                    }
                    log(&c4, format!("rename {}", b["name"]));
                    Ok(Json(json!({})))
                }),
            )
            .route(
                "/guilds/{g}/members/{u}",
                get(|| async { Json(json!({"roles": []})) }),
            )
            .route(
                "/channels/{c}/permissions/{t}",
                axum::routing::put(move |axum::extract::Path((c, t)): axum::extract::Path<(String, String)>, Json(b): Json<Value>| async move {
                    log(&c2, format!("put {c} {t} {}", b["allow"]));
                    Json(json!({}))
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
        let send = |m: &str, path: &str, user: i64, stepped_up: bool, body: Option<Value>| {
            let mut req = Request::builder()
                .method(m)
                .uri(format!("/api/guilds/{g}{path}"))
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
        let stored = || async {
            sqlx::query_scalar::<_, Value>("select draft from channel_drafts where guild_id = $1")
                .bind(g)
                .fetch_optional(&db)
                .await
                .unwrap()
        };
        let (code, r) = send("GET", "/channel-setup", owner, false, None).await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(
            (r["changes"].as_i64(), r["can_unstaff"].as_bool()),
            (Some(0), Some(true))
        );
        assert_eq!(
            (&r["version"], &r["pickable_roles"]),
            (&json!(""), &json!(["5"]))
        );
        // An issue already on the server is the Overview's; it shows here once the draft touches it.
        assert_eq!(r["issues"], json!([]), "{r}");

        let new = json!({"draft": {"items": {"new-1": {"kind": 0, "name": "News Room"}, "300": {"name": "lobby"}}}});
        let (code, r) = send("PUT", "/channel-setup", owner, false, Some(new)).await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(r["changes"], 2);
        assert_eq!(r["issues"][0]["id"], "overwrite_300_5");
        assert_eq!(r["tree"][0]["issues"][0]["fix"], json!({"strip": "5"}));
        assert_eq!(r["draft"]["items"]["new-1"]["name"], "news-room");
        assert!(stored().await.is_some());
        let bad = json!({"draft": {"items": {"300": {"perms": 8}}}});
        assert_eq!(
            send("PUT", "/channel-setup", owner, false, Some(bad))
                .await
                .0,
            400
        );
        let other = json!({"draft": {"items": {}}});
        assert_eq!(
            send("PUT", "/channel-setup", owner + 1, false, Some(other))
                .await
                .0,
            403
        );

        let (code, r) = send("POST", "/channel-setup/push", owner, false, Some(json!({}))).await;
        assert_eq!((code, r["error"].as_str()), (403, Some("step_up_required")));
        let (code, r) = send("POST", "/channel-setup/push", owner, true, Some(json!({}))).await;
        assert_eq!(code, 409, "no version: {r}");
        let v = send("GET", "/channel-setup", owner, false, None).await.1["version"].clone();
        let (code, r) = send(
            "POST",
            "/channel-setup/push",
            owner,
            true,
            Some(json!({ "version": v })),
        )
        .await;
        assert_eq!(code, 409, "{r}");
        assert_eq!(r["issues"][0]["id"], "overwrite_300_5");
        assert!(calls.lock().unwrap().is_empty(), "nothing ran");

        let fixed = json!({"draft": {"items": {"new-1": {"kind": 0, "name": "news"}, "300": {"name": "lobby", "strip": ["5"]}}}});
        let (code, r) = send("PUT", "/channel-setup", owner, false, Some(fixed)).await;
        assert_eq!(code, 200, "{r}");
        // A push of a version the user did not review is refused.
        let (code, _) = send(
            "POST",
            "/channel-setup/push",
            owner,
            true,
            Some(json!({ "version": v })),
        )
        .await;
        assert_eq!(code, 409);
        // A saved server layout: the push lands in it as v2.
        let saved = brim_antinuke::Image {
            guild: g as u64,
            channels: vec![ChannelImg {
                id: 300,
                name: "chat".into(),
                kind: 0,
                parent: None,
                position: 0,
                topic: None,
                nsfw: false,
                overwrites: vec![ow(5, 16)],
            }],
            ..Default::default()
        };
        sqlx::query("insert into server_layouts (guild_id, version, image, source, created_by) values ($1, 1, $2, 'import', $3)")
            .bind(g)
            .bind(sqlx::types::Json(&saved))
            .bind(owner)
            .execute(&db)
            .await
            .unwrap();
        let push = Some(json!({ "version": r["version"] }));
        let (code, r) = send("POST", "/channel-setup/push", owner, true, push).await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(
            (r["done"].as_i64(), r["failed"].as_array().map(Vec::len)),
            (Some(4), Some(0))
        );
        assert!(r["backup"].is_i64());
        assert_eq!(r["layout"], json!({"version": 2}), "{r}");
        let layout = || async {
            sqlx::query_as::<_, (i32, sqlx::types::Json<brim_antinuke::Image>, String)>(
                "select version, image, source from server_layouts where guild_id = $1 order by version desc limit 1",
            )
            .bind(g)
            .fetch_one(&db)
            .await
            .unwrap()
        };
        let (_, sqlx::types::Json(img), source) = layout().await;
        assert_eq!(source, "dashboard push");
        let names: Vec<(u64, &str, Vec<OverwriteImg>)> = img
            .channels
            .iter()
            .map(|c| (c.id, c.name.as_str(), c.overwrites.clone()))
            .collect();
        assert_eq!(
            names,
            [(300, "lobby", vec![ow(5, 0)]), (301, "news", vec![])],
            "renamed, stripped and made, with the new id"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "create \"news\"",
                "rename \"lobby\"",
                "positions [{\"id\":\"300\",\"position\":0}]",
                "put 300 5 \"0\""
            ]
        );
        assert_eq!(
            stored().await,
            Some(json!({"items": {}, "next": 2})),
            "a full push empties the draft and keeps the used keys"
        );
        // A stale tab sends new-1 again: it was created, so it is refused.
        let stale = json!({"draft": {"items": {"new-1": {"kind": 0, "name": "news"}}}});
        assert_eq!(
            send("PUT", "/channel-setup", owner, false, Some(stale))
                .await
                .0,
            409
        );

        // The rename fails: the push stops there.
        calls.lock().unwrap().clear();
        let two = json!({"draft": {"items": {"new-2": {"kind": 0, "name": "a"}, "300": {"name": "b", "strip": ["5"]}}}});
        let (code, r) = send("PUT", "/channel-setup", owner, false, Some(two)).await;
        assert_eq!(code, 200, "{r}");
        let push = Some(json!({ "version": r["version"] }));
        let (code, r) = send("POST", "/channel-setup/push", owner, true, push).await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(
            (r["done"].as_i64(), r["failed"].as_array().map(Vec::len)),
            (Some(1), Some(1))
        );
        assert_eq!(
            *calls.lock().unwrap(),
            ["create \"a\""],
            "no step after the failure"
        );
        let left = stored().await.unwrap();
        assert_eq!(
            (&left["next"], left["items"].get("301").is_some()),
            (&json!(3), true)
        );
        assert_eq!(
            r["layout"],
            Value::Null,
            "a push that stopped saves no layout"
        );
        assert_eq!(layout().await.0, 2);
        // History lists the versions; only the Owner or an EO rolls back, and not onto the newest.
        let (code, r) = send("GET", "/layout/versions", owner, false, None).await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(
            (
                r["versions"][0]["version"].as_i64(),
                r["versions"][1]["source"].as_str(),
                r["can_roll_back"].as_bool()
            ),
            (Some(2), Some("import"), Some(true))
        );
        let back = |v: i64| Some(json!({"expected_version": v}));
        assert_eq!(
            send("POST", "/layout/1/rollback", owner + 1, true, back(2))
                .await
                .0,
            403
        );
        assert_eq!(
            send("POST", "/layout/2/rollback", owner, true, back(2))
                .await
                .0,
            400
        );
        assert_eq!(
            send("POST", "/layout/1/rollback", owner, true, back(1))
                .await
                .0,
            409
        );
        // v1 holds the role overwrite that grants Manage Channels: not saved as correct.
        let (code, r) = send("POST", "/layout/1/rollback", owner, true, back(2)).await;
        assert_eq!((code, &r["saved"]), (200, &json!(false)), "{r}");
        assert_eq!(layout().await.0, 2);

        // While a push holds the lock, PUT and DELETE are refused.
        let mut tx = db.begin().await.unwrap();
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('channel_drafts:' || $1, 0))")
            .bind(g)
            .execute(&mut *tx)
            .await
            .unwrap();
        let empty = json!({"draft": {"items": {}}});
        assert_eq!(
            send("PUT", "/channel-setup", owner, false, Some(empty))
                .await
                .0,
            409
        );
        assert_eq!(
            send("DELETE", "/channel-setup", owner, false, None).await.0,
            409
        );
        tx.rollback().await.unwrap();

        let again = json!({"draft": {"items": {"new-3": {"kind": 4, "name": "x"}}}});
        assert_eq!(
            send("PUT", "/channel-setup", owner, false, Some(again))
                .await
                .0,
            200
        );
        let (code, r) = send("DELETE", "/channel-setup", owner, false, None).await;
        assert_eq!((code, &r["draft"]), (200, &json!({"items": {}, "next": 3})));
        for t in ["backups", "channel_drafts", "server_layouts"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }

    /// Roles and Server tabs end to end on a scratch Postgres (DATABASE_URL), Discord mocked.
    #[tokio::test]
    async fn roles_and_server_settings_push_in_order_and_locked_roles_stay() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_700_000_000 + std::process::id() as i64;
        let owner = 7_900_000_000 + std::process::id() as i64;
        let calls: Arc<Mutex<Vec<String>>> = Default::default();
        let log = |c: &Arc<Mutex<Vec<String>>>, s: String| c.lock().unwrap().push(s);
        let (c1, c2, c3, c4, c5) = (
            calls.clone(),
            calls.clone(),
            calls.clone(),
            calls.clone(),
            calls.clone(),
        );
        // 9 is above Brim's role 8; 5 and 6 are below it.
        let roles = json!([{"id": g.to_string(), "name": "@everyone", "permissions": "1024", "position": 0},
            {"id": "5", "name": "Mods", "permissions": "8", "position": 1},
            {"id": "6", "name": "Fans", "permissions": "0", "position": 2},
            {"id": "8", "name": "Brim", "permissions": "8", "position": 3, "managed": true},
            {"id": "9", "name": "Owner", "permissions": "8", "position": 4}]);
        let channels = json!([{"id": "300", "name": "chat", "type": 0, "position": 0, "permission_overwrites": []}]);
        use axum::routing::{delete, patch};
        let mock = Router::new()
            .route(
                "/guilds/{g}",
                get(move || async move { Json(json!({"owner_id": owner.to_string(), "mfa_level": 1, "name": "Home", "verification_level": 2, "afk_timeout": 300})) })
                    .patch(move |Json(b): Json<Value>| async move {
                        log(&c5, format!("server {b}"));
                        Json(json!({"icon": null}))
                    }),
            )
            .route(
                "/guilds/{g}/roles",
                get(move || async move { Json(roles) })
                    .post(move |Json(b): Json<Value>| async move {
                        log(&c1, format!("create {}", b["name"]));
                        Json(json!({"id": "700"}))
                    })
                    .patch(move |Json(b): Json<Value>| async move {
                        log(&c2, format!("order {b}"));
                        Json(json!([]))
                    }),
            )
            .route(
                "/guilds/{g}/roles/{r}",
                patch(move |axum::extract::Path((_, r)): axum::extract::Path<(String, String)>, Json(b): Json<Value>| async move {
                    log(&c3, format!("edit {r} {b}"));
                    Json(json!({}))
                })
                .merge(delete(move |axum::extract::Path((_, r)): axum::extract::Path<(String, String)>| async move {
                    log(&c4, format!("delete {r}"));
                    axum::http::StatusCode::NO_CONTENT
                })),
            )
            .route("/guilds/{g}/channels", get(move || async move { Json(channels) }))
            .route("/users/@me", get(|| async { Json(json!({"id": "7"})) }))
            .route(
                "/guilds/{g}/members/{u}",
                get(|| async { Json(json!({"roles": ["8"]})) }),
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
        let send = |m: &str, path: &str, stepped_up: bool, body: Option<Value>| {
            let mut req = Request::builder()
                .method(m)
                .uri(format!("/api/guilds/{g}{path}"))
                .header("content-type", "application/json")
                .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
                .unwrap();
            req.extensions_mut().insert(Sess {
                id: String::new(),
                user_id: owner,
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
        let (code, r) = send("GET", "/channel-setup", false, None).await;
        assert_eq!(code, 200, "{r}");
        let ids: Vec<&str> = r["roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["9", "8", "6", "5", &g.to_string()]);
        assert!(r["roles"][0]["locked"].is_string() && r["roles"][0]["movable"] == false);
        assert_eq!(r["roles"][1]["tag"], "Bot");
        // Locked roles and dangerous Discord permissions are refused, whatever the page sends.
        for bad in [
            json!({"roles": {"9": {"name": "x"}}}),
            json!({"roles": {"8": {"color": 1}}}),
            json!({"roles": {g.to_string(): {"permissions": 0}}}),
            json!({"roles": {"6": {"permissions": 8}}}),
            json!({"roles": {"5": {"permissions": 0}}}),
            json!({"role_order": ["9", "6", "5"]}),
            json!({"server": {"verification": 9}}),
            json!({"server": {"afk_channel": "300"}}),
        ] {
            let (code, r) = send(
                "PUT",
                "/channel-setup",
                false,
                Some(json!({"draft": bad.clone()})),
            )
            .await;
            assert_eq!(code, 400, "{bad} {r}");
        }
        let d = json!({"draft": {
            "roles": {"5": {"name": "Moderators", "permissions": 8 | (1 << 15)}, "6": {"delete": true}, "new-1": {"name": "Helpers"}},
            "role_order": ["new-1", "5", "6"],
            "server": {"name": "Home Base"},
        }});
        let (code, r) = send("PUT", "/channel-setup", false, Some(d)).await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(r["changes"], 4, "{r}");
        // Push needs the 2FA step-up, like every Channel Setup push.
        let v = json!({"version": r["version"]});
        assert_eq!(
            send("POST", "/channel-setup/push", false, Some(v.clone()))
                .await
                .0,
            403
        );
        let (code, r) = send("POST", "/channel-setup/push", true, Some(v)).await;
        assert_eq!((code, &r["failed"]), (200, &json!([])), "{r}");
        let got = calls.lock().unwrap().clone();
        assert_eq!(
            got,
            [
                "create \"Helpers\"".to_string(),
                format!(
                    "edit 5 {}",
                    json!({"name": "Moderators", "permissions": (8u64 | (1 << 15)).to_string()})
                ),
                format!(
                    "order {}",
                    json!([{"id": "700", "position": 3}, {"id": "5", "position": 2}, {"id": "6", "position": 1}])
                ),
                format!("server {}", json!({"name": "Home Base"})),
                "delete 6".to_string(),
            ]
        );
        // The draft is empty again, its numbers kept.
        let (_, r) = send("GET", "/channel-setup", false, None).await;
        assert_eq!(r["draft"], json!({"items": {}, "next": 2}));
        for t in [
            "backups",
            "channel_drafts",
            "server_layouts",
            "dashboard_audit",
        ] {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await;
        }
    }
}

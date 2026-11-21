//! Auto Setup's channel access (owner decision 2026-11-16): staff see the Logs and Staff categories,
//! the optional Admin-only and Owner-only chats, the Information category (members read, staff
//! write) with the catch channel at its bottom, and the standard set on every open channel.
//! Every step only adds what is missing; it never takes an overwrite away.
use crate::{
    sections::{category, own},
    ApiError, AppState,
};
use brim_antinuke::{
    security::{
        role_overwrite, standard_access as plan, standard_access_check, synced_children, Check,
        Standard, LOGS_CATEGORY,
    },
    ChannelImg, OverwriteImg, RoleImg, CATEGORY,
};
use reqwest::Method;
use serde_json::{json, Value};

const VIEW: u64 = 1 << 10;
const SEND: u64 = 1 << 11;
const HISTORY: u64 = 1 << 16;
const MENTION_EVERYONE: u64 = 1 << 17;
/// Add Reactions, Embed Links, Attach Files: what nobody gets in the catch channel.
const CATCH_DENY: u64 = 49216;
/// Create Public Threads, Create Private Threads, Send Messages in Threads.
const THREADS: u64 = (1 << 35) | (1 << 36) | (1 << 38);
/// View Channel, Send Messages, Embed Links: what the bot needs to post.
const BOT: u64 = 19456;
/// The bot in the catch channel: [`BOT`] plus Manage Messages and Read Message History.
const BOT_CATCH: u64 = 93184;

/// The category for updates and announcements; members read it, staff write.
pub(crate) const INFORMATION: &str = "Information";
/// The category Auto Setup keeps the staff-facing channels in: #staff-chat and Staff Voice for
/// all staff, and the optional admin-chat and owner-chat with their own access (owner decision 2026-11-18).
const STAFF_CATEGORY: &str = "Staff";
/// The old name of the staff chat and voice category (11-17 to 11-18): still recognised, and
/// renamed to [`STAFF_CATEGORY`] while a server has no Staff category.
const STAFF_CHAT_CATEGORY: &str = "Staff Chat";
/// Connect and Speak, for Staff Voice.
const VOICE_TALK: u64 = (1 << 20) | (1 << 21);
/// Discord's own names for a new server's categories (owner decision 2026-11-17).
const TEXT_CATEGORY: &str = "Text Channels";
const VOICE_CATEGORY: &str = "Voice Channels";
const VOICE: u8 = 2;
const STAGE: u8 = 13;

fn ids(v: &Value) -> Vec<u64> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str()?.parse().ok())
        .collect()
}

fn main_roles(config: &Value) -> Vec<u64> {
    ids(&config["lockdown"]["main_roles"])
}

fn id_of(v: &Value) -> Option<u64> {
    own(v)?.parse().ok()
}

async fn roles(st: &AppState, guild: i64) -> Result<Vec<RoleImg>, ApiError> {
    let list = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/roles"), None)
        .await?;
    Ok(crate::recovery::live_image(guild, &list, &json!([])).roles)
}

/// Of `ids`, the roles Discord still has. Brim stores role ids (permits, levels, settings),
/// and a write for one deleted in Discord fails with 404 (the test server, 2026-11-18).
fn alive(ids: &[u64], roles: &[RoleImg]) -> Vec<u64> {
    ids.iter()
        .copied()
        .filter(|id| roles.iter().any(|r| r.id == *id))
        .collect()
}

/// The staff roles that need channel access: alive, and without Administrator (those see every
/// channel already). The Overview row and every fix use this one list.
pub(crate) fn live_staff(staff: &[u64], roles: &[RoleImg]) -> Vec<u64> {
    staff
        .iter()
        .copied()
        .filter(|id| {
            roles
                .iter()
                .any(|r| r.id == *id && r.permissions & ADMINISTRATOR == 0)
        })
        .collect()
}

const ADMINISTRATOR: u64 = 1 << 3;

async fn channels(st: &AppState, guild: i64) -> Result<Vec<ChannelImg>, ApiError> {
    let list = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    Ok(crate::recovery::live_image(guild, &json!([]), &list).channels)
}

/// Managed (bot) roles: the standard set keeps their access, so no bot loses a channel.
fn bot_roles(roles: &[RoleImg]) -> Vec<u64> {
    roles.iter().filter(|r| r.managed).map(|r| r.id).collect()
}

/// Why the standard set may not run, from the settings alone: it hides open channels from
/// everyone without a main role, so new members must get one when they verify.
pub(crate) fn config_gate(config: &Value) -> Option<&'static str> {
    let main = main_roles(config);
    let given = ids(&config["verification"]["roles"]);
    if config["verification"]["enabled"] != true {
        Some("verification is off, so new members would get no role that lets them in")
    } else if main.is_empty() {
        Some("no main role is set")
    } else if given.is_empty() || !given.iter().all(|r| main.contains(r)) {
        Some("verification does not give a main role, so verified members would not get in")
    } else {
        None
    }
}

/// Why the standard set may not run now: [`config_gate`], an active lockdown (its unlock would
/// undo half of it). Members without the verified role do not block it: they verify or get the
/// time limit (owner decision 2026-11-17).
pub(crate) async fn blocked(
    st: &AppState,
    guild: i64,
    config: &Value,
) -> Result<Option<String>, ApiError> {
    if let Some(why) = config_gate(config) {
        return Ok(Some(why.into()));
    }
    let locked: bool =
        sqlx::query_scalar("select exists(select 1 from lock_snapshots where guild_id = $1)")
            .bind(guild)
            .fetch_one(&st.db)
            .await?;
    if locked {
        return Ok(Some("a lockdown is active: end it first".into()));
    }
    Ok(None)
}

/// Writes one overwrite. A refusal says what Discord refused and why, in plain words.
async fn put(st: &AppState, channel: u64, o: &OverwriteImg) -> Result<(), ApiError> {
    let path = format!("/channels/{channel}/permissions/{}", o.target);
    let body = Some(crate::ops::overwrite_body(o));
    let Err(e) = st.discord.call(Method::PUT, &path, body).await else {
        return Ok(());
    };
    tracing::warn!(
        ?e,
        channel,
        target = o.target,
        "setting a channel permission failed"
    );
    let status = e.downcast_ref::<reqwest::Error>().and_then(|e| e.status());
    let why = match status.map(|s| s.as_u16()) {
        Some(404) => {
            "that channel or role no longer exists in Discord. Reload the page and try again".into()
        }
        Some(403) => "Brim lacks Manage Roles in that channel, or a permission it would give that Brim does not have itself".into(),
        Some(429) => "Discord is limiting changes right now. Try again in a minute".into(),
        Some(s) => format!("Discord answered with error {s}"),
        None => "Discord did not answer. Try again in a minute".into(),
    };
    let who = if o.is_role { "role" } else { "member" };
    Err(ApiError::BadRequest(format!(
        "Discord refused the permissions of {who} {} in channel {channel}: {why}.",
        o.target
    )))
}

/// The overwrite of `target` on `c` with the `allow` bits added that it neither allows nor
/// denies yet; None when nothing is missing.
fn with_allow(c: &ChannelImg, target: u64, allow: u64) -> Option<OverwriteImg> {
    let o = role_overwrite(c, target);
    let add = allow & !(o.allow | o.deny);
    (add != 0).then_some(OverwriteImg {
        allow: o.allow | add,
        ..o
    })
}

fn role(target: u64, allow: u64, deny: u64) -> Value {
    json!({"id": target.to_string(), "type": 0, "allow": allow.to_string(), "deny": deny.to_string()})
}

fn bot(st: &AppState, allow: u64) -> Value {
    json!({"id": st.discord.client_id, "type": 1, "allow": allow.to_string()})
}

/// Each change [`staff_access`] makes, as (channel, overwrite): every staff role gets `allow`
/// on the bot's Logs (View, History) and Staff (also Send) categories, their synced channels and
/// the log channels in them. The Admin-only and Owner-only chats keep their own access.
fn staff_plan(chans: &[ChannelImg], staff: &[u64], logs: &[u64]) -> Vec<(u64, OverwriteImg)> {
    let mut out = vec![];
    for (name, allow) in [
        (LOGS_CATEGORY, VIEW | HISTORY),
        (STAFF_CATEGORY, VIEW | SEND | HISTORY | VOICE_TALK),
        (STAFF_CHAT_CATEGORY, VIEW | SEND | HISTORY | VOICE_TALK),
    ] {
        for cat in chans
            .iter()
            .filter(|c| c.kind == CATEGORY && c.name == name)
        {
            let synced = synced_children(chans, cat);
            let inside = chans.iter().filter(|c| {
                synced.iter().any(|s| s.id == c.id)
                    || (c.parent == Some(cat.id) && logs.contains(&c.id))
            });
            for c in std::iter::once(cat).chain(inside) {
                out.extend(
                    staff
                        .iter()
                        .filter_map(|&r| with_allow(c, r, allow))
                        .map(|o| (c.id, o)),
                );
            }
        }
    }
    out
}

/// Lets every staff role into the Logs and Staff categories (owner decision 2026-11-16: "by default
/// every staff role sees all staff chats and log channels").
pub(crate) async fn staff_access(
    st: &AppState,
    guild: i64,
    config: &Value,
    staff: &[u64],
) -> Result<(), ApiError> {
    let chans = channels(st, guild).await?;
    let staff = &live_staff(staff, &roles(st, guild).await?);
    let logs = crate::ops::log_channel_ids(config);
    // Past a refused write, so one bad channel does not keep staff out of the rest.
    let mut last = Ok(());
    for (c, o) in staff_plan(&chans, staff, &logs) {
        if let Err(e) = put(st, c, &o).await {
            last = Err(e);
        }
    }
    last
}

/// The id of the category named `name` in Discord's channel list.
fn category_id(list: &Value, name: &str) -> Option<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .find(|c| c["type"] == 4 && c["name"] == name)
        .and_then(|c| crate::sections::own(&c["id"]))
}

/// The Staff category, made or reused by [`category`]. A server set up with the old "Staff Chat"
/// category and no Staff one gets it renamed first (owner decision 2026-11-18), so nothing is made twice.
/// Renaming changes no overwrites: admin-chat and owner-chat keep their own.
async fn staff_category(
    st: &AppState,
    guild: i64,
    list: &mut Value,
    main: &[u64],
) -> Result<String, ApiError> {
    if category_id(list, STAFF_CATEGORY).is_none() {
        if let Some(old) = category_id(list, STAFF_CHAT_CATEGORY) {
            let body = json!({"name": STAFF_CATEGORY});
            st.discord
                .call(Method::PATCH, &format!("/channels/{old}"), Some(body))
                .await?;
            for c in list.as_array_mut().into_iter().flatten() {
                if c["id"] == old {
                    c["name"] = json!(STAFF_CATEGORY);
                }
            }
        }
    }
    category(st, guild, list, STAFF_CATEGORY, main).await
}

/// #staff-chat and "Staff Voice" in the Staff category (owner decision 2026-11-17, one category since
/// 11-18): private, and [`staff_access`] lets every staff role in. Channels made here are synced
/// with it. Returns the names of what it made.
pub(crate) async fn staff_lounge(
    st: &AppState,
    guild: i64,
    config: &Value,
) -> Result<Vec<&'static str>, ApiError> {
    let mut list = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    let main = main_roles(config);
    let had = category_id(&list, STAFF_CATEGORY).or(category_id(&list, STAFF_CHAT_CATEGORY));
    let parent = staff_category(st, guild, &mut list, &main).await?;
    let old = category_id(&list, STAFF_CHAT_CATEGORY);
    let mut made = match had {
        Some(_) => vec![],
        None => vec![STAFF_CATEGORY],
    };
    for (name, kind) in [("staff-chat", 0), ("Staff Voice", VOICE)] {
        // One left in an old Staff Chat category next to a Staff one counts: no second copy.
        let exists = list.as_array().into_iter().flatten().any(|c| {
            let p = c["parent_id"].as_str();
            c["name"] == name
                && c["type"] == kind
                && (p == Some(&parent) || (p.is_some() && p == old.as_deref()))
        });
        if !exists {
            // No overwrites given: it takes the category's, so it is synced.
            let body = json!({"name": name, "type": kind, "parent_id": parent});
            crate::sections::create(st, guild, None, "channels", body).await?;
            made.push(name);
        }
    }
    Ok(made)
}

/// The Overview row when a staff role cannot use the Logs or Staff category (or an old Staff Chat one), or a
/// synced or log channel in it ([`staff_plan`]). Its Fix runs [`staff_access`].
pub(crate) fn staff_access_check(
    chans: &[ChannelImg],
    staff: &[u64],
    logs: &[u64],
) -> Option<Check> {
    let mut ids: Vec<u64> = staff_plan(chans, staff, logs)
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    ids.dedup();
    let names: Vec<String> = ids
        .iter()
        .filter_map(|id| chans.iter().find(|c| c.id == *id))
        .map(|c| match c.kind {
            CATEGORY => format!("category {}", c.name),
            _ => format!("#{}", c.name),
        })
        .collect();
    (!names.is_empty()).then(|| Check {
        id: "staff_access".into(),
        severity: brim_antinuke::security::Severity::Warn,
        title: "Staff cannot use their channels",
        detail: format!(
            "Some staff roles cannot see or use: {}. The fix only adds access for the staff roles.",
            names.join(", ")
        ),
        fix: Some(brim_antinuke::security::Fix {
            label: "Fix",
            role_id: None,
            channel_id: None,
            setting: Some(STAFF_ACCESS),
            move_under_bot: false,
        }),
        steps: vec![],
    })
}

/// Fix setting for [`staff_access_check`].
pub(crate) const STAFF_ACCESS: &str = "staff_access";

/// The optional chats in the Staff category: "admin-chat" for Admins and up, "owner-chat" for
/// the Owner role (the server owner and Extra Owners). One that exists is kept as it is.
pub(crate) async fn staff_chats(
    st: &AppState,
    guild: i64,
    config: &Value,
    wanted: [(&'static str, Vec<u64>); 2],
) -> Result<Vec<&'static str>, ApiError> {
    let wanted: Vec<_> = wanted.into_iter().filter(|(_, r)| !r.is_empty()).collect();
    if wanted.is_empty() {
        return Ok(vec![]);
    }
    let mut list = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    // Listed raw: `category` reads the Discord JSON.
    let main = main_roles(config);
    let parent = staff_category(st, guild, &mut list, &main).await?;
    let mut made = vec![];
    for (name, roles) in wanted {
        let exists = list
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["name"] == name && c["parent_id"].as_str() == Some(parent.as_str()));
        if exists {
            continue;
        }
        let mut ows = vec![role(guild as u64, 0, VIEW), bot(st, BOT)];
        ows.extend(roles.iter().map(|&r| role(r, VIEW | SEND | HISTORY, 0)));
        let body =
            json!({"name": name, "type": 0, "parent_id": parent, "permission_overwrites": ows});
        crate::sections::create(st, guild, None, "channels", body).await?;
        made.push(name);
    }
    Ok(made)
}

/// The Information category's overwrites: members (the main roles) read but do not write or
/// start threads, staff write and may mention @everyone, unverified members do not see it.
/// Information's overwrites. `open`: [`blocked`] says hiding it from members without a main role
/// would lock them out, so @everyone reads it (no write) until the standard set runs.
fn information_overwrites(
    st: &AppState,
    guild: i64,
    main: &[u64],
    staff: &[u64],
    open: bool,
) -> Vec<Value> {
    let everyone = match open {
        true => role(guild as u64, 0, SEND | THREADS),
        false => role(guild as u64, 0, VIEW),
    };
    let mut ows = vec![everyone, bot(st, BOT)];
    ows.extend(
        main.iter()
            .map(|&r| role(r, VIEW | HISTORY, SEND | THREADS)),
    );
    ows.extend(
        staff
            .iter()
            .map(|&r| role(r, VIEW | SEND | HISTORY | MENTION_EVERYONE, 0)),
    );
    ows
}

/// Makes or reuses the Information category with #announcements and #rules (owner decision
/// 2026-11-16) and sets it as the catch channel's place in `config`. A reused category gets the
/// staff roles' access added. Returns the names of the channels it created.
pub(crate) async fn information(
    st: &AppState,
    guild: i64,
    config: &mut Value,
    staff: &[u64],
    moves: &mut Vec<Value>,
) -> Result<Vec<&'static str>, ApiError> {
    let chans = channels(st, guild).await?;
    let staff = &live_staff(staff, &roles(st, guild).await?);
    let main = main_roles(config);
    let set = id_of(&config["catch"]["category"]);
    let existing = chans.iter().find(|c| {
        c.kind == CATEGORY && (Some(c.id) == set || (set.is_none() && c.name == INFORMATION))
    });
    let mut made = vec![];
    let cat = match existing {
        Some(c) => {
            // The synced channels too, so #announcements and #rules stay synced.
            let synced = synced_children(&chans, c);
            for ch in std::iter::once(c).chain(synced) {
                for o in staff
                    .iter()
                    .filter_map(|&r| with_allow(ch, r, VIEW | SEND | HISTORY | MENTION_EVERYONE))
                {
                    put(st, ch.id, &o).await?;
                }
            }
            // A reused one moves to the top too (owner decision 2026-11-17).
            moves.push(json!({"id": c.id.to_string(), "position": 0}));
            c.id.to_string()
        }
        None => {
            let open = blocked(st, guild, config).await?.is_some();
            let ows = information_overwrites(st, guild, &main, staff, open);
            // At the top of the list, below the channels without a category (owner decision 2026-11-17).
            let body = json!({"name": INFORMATION, "type": 4, "position": 0, "permission_overwrites": ows});
            made.push(INFORMATION);
            crate::sections::create(st, guild, None, "channels", body).await?
        }
    };
    for name in ["announcements", "rules"] {
        let here = chans
            .iter()
            .any(|c| c.name == name && c.parent.map(|p| p.to_string()) == Some(cat.clone()));
        if !here {
            // No overwrites given: a new channel takes its category's, so it is synced.
            let body = json!({"name": name, "type": 0, "parent_id": cat});
            crate::sections::create(st, guild, None, "channels", body).await?;
            made.push(name);
        }
    }
    config["catch"]["category"] = json!(cat);
    Ok(made)
}

/// Puts every channel without a category into "Text Channels" or, voice and stage, "Voice
/// Channels" (each reused by name or made), so only the verification channel sits above the
/// categories (owner decision 2026-11-17). The catch channel goes to Information instead. They keep
/// their overwrites; the standard set makes them verified-only when it may run. Returns the
/// categories it made.
pub(crate) async fn sort_loose(
    st: &AppState,
    guild: i64,
    config: &Value,
    moves: &mut Vec<Value>,
) -> Result<Vec<&'static str>, ApiError> {
    let chans = channels(st, guild).await?;
    let stay = [
        id_of(&config["verification"]["channel"]),
        id_of(&config["catch"]["channel"]),
        // Moved into Logs by `ticket_log_to_logs`.
        id_of(&config["tickets"]["log_channel"]),
    ];
    let mut made = vec![];
    for (voice, name) in [(false, TEXT_CATEGORY), (true, VOICE_CATEGORY)] {
        let loose: Vec<u64> = chans
            .iter()
            .filter(|c| c.parent.is_none() && c.kind != CATEGORY)
            .filter(|c| matches!(c.kind, VOICE | STAGE) == voice && !stay.contains(&Some(c.id)))
            .map(|c| c.id)
            .collect();
        if loose.is_empty() {
            continue;
        }
        let reuse = chans
            .iter()
            .filter(|c| c.kind == CATEGORY && c.name == name)
            .min_by_key(|c| c.id);
        let cat = match reuse {
            Some(c) => c.id.to_string(),
            None => {
                made.push(name);
                let body = json!({"name": name, "type": CATEGORY});
                crate::sections::create(st, guild, None, "channels", body).await?
            }
        };
        moves.extend(
            loose.iter().map(
                |id| json!({"id": id.to_string(), "parent_id": cat, "lock_permissions": false}),
            ),
        );
    }
    Ok(made)
}

/// The changes that make an Information channel verified-only and read-only: the main roles
/// and the bot roles (Brim's among them, so it never loses its own channel) get View and
/// History, the main roles lose Send and threads unless they allow them or @everyone denies
/// them already, then @everyone loses View, last, so a refused write never leaves it hidden from
/// everyone. Also when @everyone is kept out already: a category made while another main role
/// was set lets no current member in (the test server, 2026-11-17). Empty without a main role.
fn info_overwrites(c: &ChannelImg, guild: u64, main: &[u64], bots: &[u64]) -> Vec<OverwriteImg> {
    if main.is_empty() {
        return vec![];
    }
    let everyone = role_overwrite(c, guild);
    let mut out = vec![];
    for &r in main.iter().chain(bots) {
        let o = role_overwrite(c, r);
        let mute = match main.contains(&r) {
            true => (SEND | THREADS) & !(o.allow | everyone.deny),
            false => 0,
        };
        let allow = (o.allow | VIEW | HISTORY) & !mute;
        let deny = (o.deny & !(VIEW | HISTORY)) | mute;
        if (allow, deny) != (o.allow, o.deny) {
            out.push(OverwriteImg { allow, deny, ..o });
        }
    }
    if everyone.deny & VIEW == 0 {
        out.push(OverwriteImg {
            allow: everyone.allow & !VIEW,
            deny: everyone.deny | VIEW,
            ..everyone
        });
    }
    out
}

/// The roles verification gives; the bot falls back to the main roles when none are set.
fn given_roles(config: &Value) -> Vec<u64> {
    match ids(&config["verification"]["roles"]) {
        v if v.is_empty() => main_roles(config),
        v => v,
    }
}

/// The changes that hide the verification channel from verified members (owner decision 2026-11-17):
/// each role verification gives is denied View. A channel set up before that role existed has
/// no such overwrite.
fn verify_overwrites(c: &ChannelImg, guild: u64, given: &[u64]) -> Vec<OverwriteImg> {
    given
        .iter()
        // @everyone's own id would hide it from the members who must verify.
        .filter(|&&r| r != guild)
        .map(|&r| role_overwrite(c, r))
        .filter(|o| o.deny & VIEW == 0)
        .map(|o| OverwriteImg {
            allow: o.allow & !VIEW,
            deny: o.deny | VIEW,
            ..o
        })
        .collect()
}

/// Applies [`verify_overwrites`] to the verification channel. Returns how many overwrites it
/// could not set.
pub(crate) async fn verify_access(
    st: &AppState,
    guild: i64,
    config: &Value,
) -> Result<usize, ApiError> {
    // Only while verification gives a main role: then the roles it gives mean verified.
    let Some(id) =
        id_of(&config["verification"]["channel"]).filter(|_| config_gate(config).is_none())
    else {
        return Ok(0);
    };
    let chans = channels(st, guild).await?;
    let Some(c) = chans.iter().find(|c| c.id == id) else {
        return Ok(0);
    };
    let given = alive(&given_roles(config), &roles(st, guild).await?);
    let mut failed = 0;
    for o in verify_overwrites(c, guild as u64, &given) {
        if put(st, id, &o).await.is_err() {
            failed += 1;
        }
    }
    Ok(failed)
}

/// Information and every channel in it but the catch channel, which [`move_catch`] handles.
fn info_channels<'a>(chans: &'a [ChannelImg], cat: u64, config: &Value) -> Vec<&'a ChannelImg> {
    let catch = id_of(&config["catch"]["channel"]);
    chans
        .iter()
        .filter(|c| {
            (c.id == cat && c.kind == CATEGORY) || (c.parent == Some(cat) && Some(c.id) != catch)
        })
        .collect()
}

/// Makes Information verified-only ([`info_overwrites`]), for one made open while [`blocked`].
/// Callers check [`blocked`] first. Returns how many overwrites it could not set.
pub(crate) async fn info_access(
    st: &AppState,
    guild: i64,
    config: &Value,
    staff: &[u64],
) -> Result<usize, ApiError> {
    let Some(cat) = id_of(&config["catch"]["category"]) else {
        return Ok(0);
    };
    let get = |p: String| async move { st.discord.call(Method::GET, &p, None).await };
    let (roles, list) = (
        get(format!("/guilds/{guild}/roles")).await?,
        get(format!("/guilds/{guild}/channels")).await?,
    );
    let img = crate::recovery::live_image(guild, &roles, &list);
    let (main, bots) = (
        alive(&main_roles(config), &img.roles),
        bot_roles(&img.roles),
    );
    let staff = live_staff(staff, &img.roles);
    let mut failed = 0;
    for ch in info_channels(&img.channels, cat, config) {
        // Staff first: the main roles lose Send here, and staff hold a main role too.
        let staff_in = staff
            .iter()
            .filter_map(|&r| with_allow(ch, r, VIEW | SEND | HISTORY | MENTION_EVERYONE));
        // A refused staff write (Brim may lack Mention @everyone there) must not keep the
        // members out of Information, so only the member and bot writes stop the channel.
        for o in staff_in {
            if put(st, ch.id, &o).await.is_err() {
                failed += 1;
            }
        }
        for o in info_overwrites(ch, guild as u64, &main, &bots) {
            if put(st, ch.id, &o).await.is_err() {
                failed += 1;
                // @everyone's deny comes last: skip it when a role could not be let in.
                break;
            }
        }
    }
    Ok(failed)
}

/// Sends every channel move of a run as one `PATCH /guilds/{guild}/channels`: Discord limits
/// that route hard, and one call per move made Auto Setup wait on rate limits and drop moves
/// (the test server, 11-17). Best effort: the setup goes on when Discord refuses. Returns
/// whether the moves were made.
pub(crate) async fn send_moves(st: &AppState, guild: i64, moves: Vec<Value>) -> bool {
    if moves.is_empty() {
        return true;
    }
    let path = format!("/guilds/{guild}/channels");
    if patch(st, &path, guild, json!(moves)).await {
        return true;
    }
    if moves.len() == 1 {
        return false;
    }
    // Discord refuses the whole list for one bad entry (a full category holds 50 channels),
    // so each move is tried alone and the others still land.
    let mut all = true;
    for m in moves {
        all &= patch(st, &path, guild, json!([m])).await;
    }
    all
}

async fn patch(st: &AppState, path: &str, guild: i64, list: Value) -> bool {
    let r = st.discord.call(Method::PATCH, path, Some(list)).await;
    if let Err(e) = &r {
        tracing::warn!(?e, guild, "moving channels failed");
    }
    r.is_ok()
}

/// The verification channel goes first in the list, out of any category (owner decision 2026-11-17).
/// It keeps its own overwrites.
pub(crate) fn verification_first(config: &Value, moves: &mut Vec<Value>) {
    if let Some(id) = config["verification"]["channel"].as_str() {
        moves.push(json!({"id": id, "parent_id": null, "position": 0, "lock_permissions": false}));
    }
}

/// The catch channel's overwrites inside Information: only verified members (the main roles)
/// see it, and they may write there, so a hacked account that spams every channel is caught.
/// Merged into `c`'s overwrites when it exists; @everyone's is the bot's own and is replaced.
fn catch_overwrites(
    st: &AppState,
    guild: i64,
    main: &[u64],
    c: Option<&ChannelImg>,
) -> Vec<OverwriteImg> {
    let old = |id| c.map(|c| role_overwrite(c, id));
    let merge = |id, allow: u64, deny: u64| {
        let o = old(id).unwrap_or(OverwriteImg {
            target: id,
            is_role: true,
            allow: 0,
            deny: 0,
        });
        OverwriteImg {
            allow: o.allow | allow,
            deny: (o.deny | deny) & !allow,
            ..o
        }
    };
    let me: u64 = st.discord.client_id.parse().unwrap_or_default();
    let bot = c
        .and_then(|c| {
            c.overwrites
                .iter()
                .find(|o| !o.is_role && o.target == me)
                .cloned()
        })
        .unwrap_or(OverwriteImg {
            target: me,
            is_role: false,
            allow: 0,
            deny: 0,
        });
    let mut ows = vec![
        OverwriteImg {
            target: guild as u64,
            is_role: true,
            allow: 0,
            deny: VIEW | CATCH_DENY,
        },
        OverwriteImg {
            allow: bot.allow | BOT_CATCH,
            deny: bot.deny & !BOT_CATCH,
            ..bot
        },
    ];
    ows.extend(
        main.iter()
            .map(|&r| merge(r, VIEW | SEND | HISTORY, CATCH_DENY)),
    );
    ows
}

/// Whether the catch channel is hidden from @everyone, as [`catch_overwrites`] makes it.
fn catch_hidden(c: &ChannelImg, guild: i64) -> bool {
    role_overwrite(c, guild as u64).deny & VIEW != 0
}

/// The parent and overwrites that make a new catch channel at the bottom of Information, when
/// it exists and [`config_gate`] allows hiding it from members without a main role.
pub(crate) async fn new_catch_place(
    st: &AppState,
    guild: i64,
    config: &Value,
) -> Result<Option<(String, Vec<Value>)>, ApiError> {
    let Some(cat) = id_of(&config["catch"]["category"]).filter(|_| config_gate(config).is_none())
    else {
        return Ok(None);
    };
    let chans = channels(st, guild).await?;
    if !chans.iter().any(|c| c.id == cat && c.kind == CATEGORY) {
        return Ok(None);
    }
    let ows = catch_overwrites(st, guild, &main_roles(config), None)
        .iter()
        .map(|o| {
            let mut b = crate::ops::overwrite_body(o);
            b["id"] = json!(o.target.to_string());
            b
        })
        .collect();
    Ok(Some((cat.to_string(), ows)))
}

/// Moves the set catch channel to the bottom of Information and gives it the member write
/// access (verified members only, owner decision 2026-11-17). Runs after the commit that stores the
/// category, so the bot's keeper does not move it back to the top. The access only when
/// [`blocked`] allows it; otherwise it moves with its own overwrites. Returns why its
/// access stayed, None when done or nothing to do.
pub(crate) async fn move_catch(
    st: &AppState,
    guild: i64,
    config: &Value,
    moves: &mut Vec<Value>,
) -> Result<Option<String>, ApiError> {
    let (Some(ch), Some(cat)) = (
        id_of(&config["catch"]["channel"]),
        id_of(&config["catch"]["category"]),
    ) else {
        return Ok(None);
    };
    let chans = channels(st, guild).await?;
    let Some(me) = chans.iter().find(|c| c.id == ch) else {
        return Ok(None);
    };
    let placed = me.parent == Some(cat);
    if placed && catch_hidden(me, guild) {
        // Done already; the bot keeps it at the bottom.
        return Ok(None);
    }
    // Blocked, it still goes to Information (only the verification channel stays on top,
    // Owner decision 2026-11-17) but keeps its own overwrites, so members without a role still see it.
    let why = blocked(st, guild, config).await?;
    if why.is_none() {
        for o in catch_overwrites(st, guild, &main_roles(config), Some(me)) {
            put(st, ch, &o).await?;
        }
    }
    if placed {
        return Ok(why);
    }
    let spot = |c: &ChannelImg| (c.id, c.parent, c.position);
    let others: Vec<_> = chans.iter().map(spot).collect();
    if let Some(pos) = brim_core::sections::bottom_position(&others, spot(me), cat) {
        moves.push(json!({"id": ch.to_string(), "parent_id": cat.to_string(), "position": pos, "lock_permissions": false}));
    }
    Ok(why)
}

/// Channels with their own access set by Brim: logs, verification, catch, Information and
/// tickets. [`plan`] leaves them, and the channels inside a listed category, alone.
fn own_access(config: &Value) -> Vec<u64> {
    let mut skip = crate::ops::log_channel_ids(config);
    skip.extend(
        [
            &config["verification"]["channel"],
            &config["catch"]["channel"],
            &config["catch"]["category"],
            &config["tickets"]["category"],
        ]
        .into_iter()
        .filter_map(id_of),
    );
    skip
}

fn find_verify<'a>(chans: &'a [ChannelImg], config: &Value) -> Option<&'a ChannelImg> {
    let id = id_of(&config["verification"]["channel"])?;
    chans.iter().find(|c| c.id == id)
}

/// The bot and the quarantine role: their overwrites do not make a channel one with its own access.
fn known(st: &AppState, config: &Value) -> Vec<u64> {
    st.discord
        .client_id
        .parse()
        .ok()
        .into_iter()
        .chain(id_of(&config["antinuke"]["quarantine_role"]))
        .collect()
}

/// The Overview row for open channels without the standard set, and for Information and the
/// catch channel while unverified members still see them (owner decision 2026-11-17: Overview catches a
/// channel with wrong permissions). None while [`config_gate`] says it may not run, or a main
/// role was deleted in Discord (the Main roles row asks for that first).
pub(crate) fn overview_check(
    st: &AppState,
    guild: i64,
    chans: &[ChannelImg],
    roles: &[RoleImg],
    config: &Value,
) -> Option<Check> {
    let (main, bots, known, skip) = (
        main_roles(config),
        bot_roles(roles),
        known(st, config),
        own_access(config),
    );
    if config_gate(config).is_some() || !main.iter().all(|m| roles.iter().any(|r| r.id == *m)) {
        return None;
    }
    let find = |k: &str| {
        let id = id_of(&config["catch"][k])?;
        chans.iter().find(|c| c.id == id)
    };
    let mut more = vec![];
    // Only with Information there: the catch channel goes nowhere without it.
    if let Some(info) = find("category").filter(|c| c.kind == CATEGORY) {
        more.extend(
            info_channels(chans, info.id, config)
                .into_iter()
                .filter(|c| !info_overwrites(c, guild as u64, &main, &bots).is_empty())
                .map(|c| c.id),
        );
        if let Some(c) = find("channel") {
            if c.parent != Some(info.id) || !catch_hidden(c, guild) {
                more.push(c.id);
            }
        }
    }
    if let Some(c) = find_verify(chans, config) {
        if !verify_overwrites(c, guild as u64, &given_roles(config)).is_empty() {
            more.push(c.id);
        }
    }
    standard_access_check(
        chans,
        &Standard {
            guild: guild as u64,
            main_roles: &main,
            bot_roles: &bots,
            known: &known,
            skip: &skip,
        },
        &more,
    )
}

/// Gives every open channel the standard set: @everyone cannot view, the main roles (and the
/// bot roles) view, send and read history. Returns how many overwrites it could not set (the
/// bot lacks Manage Roles there); `None` when the channels could not be listed. Callers check
/// [`blocked`] first. Runs after a commit, so a failure here loses no setup.
pub(crate) async fn standard_access(st: &AppState, guild: i64, config: &Value) -> Option<usize> {
    let get = |p: String| async move { st.discord.call(Method::GET, &p, None).await.ok() };
    let (roles, list) = (
        get(format!("/guilds/{guild}/roles")).await?,
        get(format!("/guilds/{guild}/channels")).await?,
    );
    let img = crate::recovery::live_image(guild, &roles, &list);
    let (main, bots, known, skip) = (
        main_roles(config),
        bot_roles(&img.roles),
        known(st, config),
        own_access(config),
    );
    let changes = plan(
        &img.channels,
        &Standard {
            guild: guild as u64,
            main_roles: &main,
            bot_roles: &bots,
            known: &known,
            skip: &skip,
        },
    );
    let mut failed = 0;
    let mut refused = None;
    for (c, o) in changes {
        // @everyone's deny comes last on each channel: skip it when a role could not be let in,
        // or the channel ends hidden from members and later reads as private (owner decision, 11-18).
        let everyone = o.is_role && o.target == guild as u64;
        if (everyone && refused == Some(c)) || put(st, c, &o).await.is_err() {
            failed += 1;
            refused = Some(c);
        }
    }
    Some(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(
        id: u64,
        kind: u8,
        parent: Option<u64>,
        name: &str,
        ows: Vec<OverwriteImg>,
    ) -> ChannelImg {
        ChannelImg {
            id,
            name: name.into(),
            kind,
            parent,
            position: 0,
            topic: None,
            nsfw: false,
            overwrites: ows,
        }
    }

    #[test]
    fn staff_get_the_logs_and_staff_categories_but_not_the_own_chats() {
        let hidden = OverwriteImg {
            target: 1,
            is_role: true,
            allow: 0,
            deny: VIEW,
        };
        let admin = OverwriteImg {
            target: 30,
            is_role: true,
            allow: VIEW | SEND | HISTORY,
            deny: 0,
        };
        let chans = [
            chan(10, CATEGORY, None, "Logs", vec![hidden.clone()]),
            chan(11, 0, Some(10), "mod-logs", vec![hidden.clone()]),
            // A log channel that is not synced still gets staff in.
            chan(12, 0, Some(10), "brim-logs", vec![]),
            chan(20, CATEGORY, None, "Staff", vec![hidden.clone()]),
            chan(21, 0, Some(20), "admin-chat", vec![hidden.clone(), admin]),
            chan(40, CATEGORY, None, "General", vec![]),
            chan(50, CATEGORY, None, "Staff Chat", vec![hidden.clone()]),
            chan(51, VOICE, Some(50), "Staff Voice", vec![hidden.clone()]),
        ];
        let got: Vec<(u64, u64, u64)> = staff_plan(&chans, &[30, 31], &[12])
            .into_iter()
            .map(|(c, o)| (c, o.target, o.allow))
            .collect();
        let logs = VIEW | HISTORY;
        let staff = VIEW | SEND | HISTORY;
        assert_eq!(
            got,
            [
                (10, 30, logs),
                (10, 31, logs),
                (11, 30, logs),
                (11, 31, logs),
                (12, 30, logs),
                (12, 31, logs),
                // Staff Voice lives in Staff since 11-18; admin-chat (21) keeps its own access.
                (20, 30, staff | VOICE_TALK),
                (20, 31, staff | VOICE_TALK),
                (50, 30, staff | VOICE_TALK),
                (50, 31, staff | VOICE_TALK),
                (51, 30, staff | VOICE_TALK),
                (51, 31, staff | VOICE_TALK),
            ]
        );
        let row = staff_access_check(&chans, &[30, 31], &[12]).unwrap();
        assert_eq!(row.detail, "Some staff roles cannot see or use: category Logs, #mod-logs, #brim-logs, category Staff, category Staff Chat, #Staff Voice. The fix only adds access for the staff roles.");
        assert!(staff_access_check(&chans, &[], &[12]).is_none());
    }

    /// A staff role deleted in Discord but still stored (a permit or level role) gets no write:
    /// Discord answers 404 for it, and Information then never got its member access (the owner's
    /// server, 2026-11-18).
    #[tokio::test]
    async fn a_deleted_staff_role_gets_no_write() {
        let (st, seen) = mock(json!([
            {"id": "10", "type": 4, "name": "Logs", "position": 0,
             "permission_overwrites": [{"id": "1", "type": 0, "allow": "0", "deny": "1024"}]},
            {"id": "900", "type": 4, "name": "Information", "position": 1,
             "permission_overwrites": []},
        ]))
        .await;
        let config = json!({"lockdown": {"main_roles": ["2", "77"]}, "catch": {"category": "900"},
            "verification": {"enabled": true, "roles": ["2", "77"], "channel": "10"}});
        staff_access(&st, 1, &config, &[30, 77]).await.unwrap();
        // A refused staff write (59) still lets the members into Information.
        assert_eq!(
            info_access(&st, 1, &config, &[59, 30, 77]).await.unwrap(),
            1
        );
        verify_access(&st, 1, &config).await.unwrap();
        let writes: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert!(writes.contains(&"PUT /channels/10/permissions/30".to_string()));
        assert!(writes.contains(&"PUT /channels/900/permissions/2".to_string()));
        assert!(!writes.iter().any(|p| p.ends_with("/77")), "{writes:?}");
    }

    /// A refused main role write keeps @everyone's View on that channel: else the members lose
    /// it until the next Overview Fix or Auto Setup gives the main role its access.
    #[tokio::test]
    async fn a_refused_member_write_does_not_hide_the_channel() {
        let open = |id: &str| json!({"id": id, "type": 0, "name": id, "position": 0, "permission_overwrites": []});
        let (st, seen) = mock(json!([open("20"), open("21")])).await;
        let config = json!({"lockdown": {"main_roles": ["2", "59"]}});
        assert_eq!(standard_access(&st, 1, &config).await, Some(4));
        let writes: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert!(writes.contains(&"PUT /channels/20/permissions/2".to_string()));
        assert!(
            !writes.iter().any(|p| p.ends_with("/permissions/1")),
            "{writes:?}"
        );
    }

    /// Owner decision 2026-11-18: an old "Staff Chat" category is renamed to Staff, and its chat and voice
    /// channels are not made again.
    #[tokio::test]
    async fn an_old_staff_chat_category_is_renamed_to_staff() {
        let c = |id: &str, kind: u8, parent: Option<&str>, name: &str| json!({"id": id, "type": kind, "name": name, "parent_id": parent, "position": 0, "permission_overwrites": []});
        let (st, seen) = mock(json!([
            c("50", CATEGORY, None, "Staff Chat"),
            c("51", 0, Some("50"), "staff-chat"),
            c("52", VOICE, Some("50"), "Staff Voice"),
        ]))
        .await;
        let made = staff_lounge(&st, 1, &json!({})).await.unwrap();
        assert!(made.is_empty(), "{made:?}");
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter()
                .any(|(p, b)| p == "PATCH /channels/50" && b["name"] == "Staff"),
            "{seen:?}"
        );
        assert!(!seen.iter().any(|(p, _)| p.starts_with("POST")), "{seen:?}");
    }

    /// A Discord stand-in: lists `channels`, answers a POST with id 900, 901... and records
    /// every write as (method path, body).
    #[test]
    fn information_goes_verified_only_once_and_keeps_staff() {
        let staff = OverwriteImg {
            target: 30,
            is_role: true,
            allow: VIEW | SEND,
            deny: 0,
        };
        let read_only = OverwriteImg {
            target: 1,
            is_role: true,
            allow: 0,
            deny: SEND | THREADS,
        };
        let open = chan(900, CATEGORY, None, "Information", vec![read_only, staff]);
        let got: Vec<(u64, u64, u64)> = info_overwrites(&open, 1, &[2], &[8])
            .into_iter()
            .map(|o| (o.target, o.allow, o.deny))
            .collect();
        // The main and bot roles first, @everyone last.
        let want = [
            (2, VIEW | HISTORY, 0),
            (8, VIEW | HISTORY, 0),
            (1, 0, SEND | THREADS | VIEW),
        ];
        assert_eq!(got, want);
        // Hidden already but made for a main role since replaced (the test server): the main
        // role is let in, read-only; @everyone stays as it is.
        let hidden = chan(900, CATEGORY, None, "Information", vec![got_ow(1, 0, VIEW)]);
        let got: Vec<(u64, u64, u64)> = info_overwrites(&hidden, 1, &[2], &[])
            .into_iter()
            .map(|o| (o.target, o.allow, o.deny))
            .collect();
        assert_eq!(got, [(2, VIEW | HISTORY, SEND | THREADS)]);
        // Set up right, or no main role to let in: nothing to change.
        let right = chan(
            900,
            CATEGORY,
            None,
            "Information",
            vec![
                got_ow(1, 0, VIEW),
                got_ow(2, VIEW | HISTORY, SEND | THREADS),
            ],
        );
        assert!(info_overwrites(&right, 1, &[2], &[]).is_empty());
        assert!(info_overwrites(&open, 1, &[], &[8]).is_empty());
        // A main role allowed to write keeps it.
        let chatty = chan(
            900,
            CATEGORY,
            None,
            "Information",
            vec![got_ow(1, 0, VIEW), got_ow(2, SEND, 0)],
        );
        let got = info_overwrites(&chatty, 1, &[2], &[]);
        assert_eq!(
            (got[0].allow, got[0].deny),
            (SEND | VIEW | HISTORY, THREADS)
        );
    }

    #[test]
    fn verified_members_lose_the_verification_channel() {
        // 2 is given by verification and still sees it; 3 is denied already.
        let c = chan(
            5,
            0,
            None,
            "verify-here",
            vec![got_ow(2, VIEW, 0), got_ow(3, 0, VIEW)],
        );
        let got: Vec<(u64, u64, u64)> = verify_overwrites(&c, 1, &[1, 2, 3, 4])
            .into_iter()
            .map(|o| (o.target, o.allow, o.deny))
            .collect();
        assert_eq!(got, [(2, 0, VIEW), (4, 0, VIEW)]);
        let config = json!({"lockdown": {"main_roles": ["7"]}});
        assert_eq!(given_roles(&config), [7]);
        let config = json!({"lockdown": {"main_roles": ["7"]}, "verification": {"roles": ["8"]}});
        assert_eq!(given_roles(&config), [8]);
    }

    fn got_ow(target: u64, allow: u64, deny: u64) -> OverwriteImg {
        OverwriteImg {
            target,
            is_role: true,
            allow,
            deny,
        }
    }

    #[tokio::test]
    async fn information_is_made_open_while_verification_gives_no_main_role() {
        let (st, seen) = mock(json!([])).await;
        let mut config =
            json!({"lockdown": {"main_roles": ["2"]}, "verification": {"enabled": false}});
        information(&st, 1, &mut config, &[], &mut vec![])
            .await
            .unwrap();
        let writes = seen.lock().unwrap().clone();
        let ows = writes[0].1["permission_overwrites"]
            .as_array()
            .unwrap()
            .clone();
        let everyone = ows.iter().find(|o| o["id"] == "1").unwrap();
        // Members without a role still read it, but cannot write.
        assert_eq!(everyone["deny"], (SEND | THREADS).to_string());
    }

    #[tokio::test]
    async fn loose_channels_go_into_text_and_voice_categories() {
        let (st, seen) = mock(json!([
            {"id": "5", "type": 0, "name": "verify-here", "position": 0, "permission_overwrites": []},
            {"id": "6", "type": 0, "name": "general", "position": 1, "permission_overwrites": []},
            {"id": "7", "type": 2, "name": "General", "position": 0, "permission_overwrites": []},
            {"id": "8", "type": 15, "name": "forum", "position": 2, "permission_overwrites": []},
            {"id": "70", "type": 0, "name": "do-not-write", "position": 0, "permission_overwrites": []},
            {"id": "40", "type": 4, "name": "Text Channels", "position": 3, "permission_overwrites": []},
            {"id": "41", "type": 0, "name": "chat", "parent_id": "40", "position": 0, "permission_overwrites": []},
        ]))
        .await;
        let config = json!({"verification": {"channel": "5"}, "catch": {"channel": "70"}});
        let mut moves = vec![];
        let made = sort_loose(&st, 1, &config, &mut moves).await.unwrap();
        assert!(send_moves(&st, 1, moves).await);
        // Text Channels is reused; Voice Channels is made.
        assert_eq!(made, ["Voice Channels"]);
        let writes = seen.lock().unwrap().clone();
        let ids = |b: &Value| {
            b.as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    (
                        e["id"].as_str().unwrap().to_string(),
                        e["parent_id"].clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        // Voice Channels is made first; then every move goes in one call.
        assert_eq!(
            writes[0],
            (
                "POST /guilds/1/channels".into(),
                json!({"name": "Voice Channels", "type": 4})
            )
        );
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[1].0, "PATCH /guilds/1/channels");
        // The verification and catch channels stay out; the rest keep their overwrites.
        assert_eq!(
            ids(&writes[1].1),
            [
                ("6".into(), json!("40")),
                ("8".into(), json!("40")),
                ("7".into(), json!("900"))
            ]
        );
        assert_eq!(writes[1].1[0]["lock_permissions"], false);
    }

    async fn mock(
        channels: Value,
    ) -> (
        AppState,
        std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    ) {
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>> = Default::default();
        let s2 = seen.clone();
        let app = axum::Router::new().fallback(
            move |m: axum::http::Method, uri: axum::http::Uri, body: String| {
                let (s2, channels) = (s2.clone(), channels.clone());
                async move {
                    if m == axum::http::Method::GET {
                        // Every member holds the main role 2.
                        let body = match uri.path().rsplit('/').next() {
                            Some("members") => {
                                json!([{"user": {"id": "3"}, "roles": ["2"]}]).to_string()
                            }
                            // Roles 1 to 60 exist; 77 was deleted in Discord.
                            Some("roles") => Value::from_iter((1..=60).map(|i| {
                                json!({"id": i.to_string(), "name": format!("r{i}"), "permissions": "0", "position": i})
                            }))
                            .to_string(),
                            _ => channels.to_string(),
                        };
                        return (axum::http::StatusCode::OK, body);
                    }
                    let mut seen = s2.lock().unwrap();
                    let b = serde_json::from_str(&body).unwrap_or_default();
                    seen.push((format!("{m} {}", uri.path()), b));
                    // Role 59's writes are refused, as for a permission Brim lacks.
                    if uri.path().ends_with("/permissions/59") {
                        return (axum::http::StatusCode::FORBIDDEN, String::new());
                    }
                    (
                        axum::http::StatusCode::OK,
                        json!({"id": (899 + seen.len()).to_string()}).to_string(),
                    )
                }
            },
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let st = AppState {
            db: sqlx::PgPool::connect_lazy(&brim_core::db::test_url().unwrap()).unwrap(),
            discord: crate::discord::Discord {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                client_id: "9".into(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                api,
                memo: Default::default(),
                limits: Default::default(),
            },
            secure: false,
            totp: Default::default(),
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        };
        (st, seen)
    }

    /// Information: members read, staff write; the two channels take its overwrites; the catch
    /// channel then moves to its bottom with member write access.
    #[tokio::test]
    async fn information_is_made_and_the_catch_channel_moves_to_its_bottom() {
        let (st, seen) = mock(json!([
            {"id": "70", "type": 0, "name": "do-not-write", "position": 0, "permission_overwrites": []},
        ]))
        .await;
        let mut config = json!({"lockdown": {"main_roles": ["2"]}, "catch": {"channel": "70"}, "verification": {"enabled": true, "roles": ["2"]}});
        let made = information(&st, 1, &mut config, &[30], &mut vec![])
            .await
            .unwrap();
        assert_eq!(made, ["Information", "announcements", "rules"]);
        assert_eq!(config["catch"]["category"], "900");
        let writes = seen.lock().unwrap().clone();
        let ows = &writes[0].1["permission_overwrites"];
        let of = |id: &str| {
            ows.as_array()
                .unwrap()
                .iter()
                .find(|o| o["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(of("1")["deny"], VIEW.to_string());
        assert_eq!(of("2")["allow"], (VIEW | HISTORY).to_string());
        assert_eq!(of("2")["deny"], (SEND | THREADS).to_string());
        assert_eq!(
            of("30")["allow"],
            (VIEW | SEND | HISTORY | MENTION_EVERYONE).to_string()
        );
        assert_eq!(
            writes[1].1,
            json!({"name": "announcements", "type": 0, "parent_id": "900"})
        );
        // Moving needs the category in the channel list.
        let (st, seen) = mock(json!([
            {"id": "70", "type": 0, "name": "do-not-write", "position": 0, "permission_overwrites": []},
            {"id": "900", "type": 4, "name": "Information", "position": 1, "permission_overwrites": []},
            {"id": "901", "type": 0, "name": "announcements", "parent_id": "900", "position": 0, "permission_overwrites": []},
            {"id": "902", "type": 0, "name": "rules", "parent_id": "900", "position": 1, "permission_overwrites": []},
        ]))
        .await;
        // Verification off: it moves
        // into Information with its own overwrites and says why.
        let mut off = config.clone();
        off["verification"]["enabled"] = json!(false);
        let mut moves = vec![];
        assert!(move_catch(&st, 1, &off, &mut moves)
            .await
            .unwrap()
            .is_some());
        assert!(send_moves(&st, 1, moves).await);
        let moved: Vec<String> = seen.lock().unwrap().drain(..).map(|(p, _)| p).collect();
        assert_eq!(moved, ["PATCH /guilds/1/channels"]);
        let mut moves = vec![];
        assert_eq!(move_catch(&st, 1, &config, &mut moves).await.unwrap(), None);
        assert!(send_moves(&st, 1, moves).await);
        let writes = seen.lock().unwrap().clone();
        let put = |t: &str| {
            writes
                .iter()
                .find(|(p, _)| p == &format!("PUT /channels/70/permissions/{t}"))
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(put("1")["deny"], (VIEW | CATCH_DENY).to_string());
        assert_eq!(put("1")["allow"], "0");
        assert_eq!(put("2")["allow"], (VIEW | SEND | HISTORY).to_string());
        let (path, body) = writes.last().unwrap();
        assert_eq!(path, "PATCH /guilds/1/channels");
        assert_eq!(body[0]["parent_id"], "900");
        assert_eq!(body[0]["position"], 2);
        // A second run finds Information by its id and makes nothing new.
        let made = information(&st, 1, &mut config, &[], &mut vec![])
            .await
            .unwrap();
        assert!(made.is_empty(), "{made:?}");
    }
}

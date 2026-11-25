//! Security score: risky Discord settings a raider or nuker would use. Shared by `/security` and
//! the dashboard Overview, which runs the `fix` of a row with one click.
use crate::{ChannelImg, OverwriteImg, RoleImg, CATEGORY};
use serde::Serialize;

/// The one list of dangerous Discord permissions; staff permit roles may hold Mention @everyone.
const DANGEROUS: u64 = crate::DANGEROUS_PERMS;
const ADMINISTRATOR: u64 = 0x8;
const VIEW: u64 = 1 << 10;
/// View Channel, Send Messages and Embed Links: what the bot needs in a log channel.
const BOT_LOG: u64 = VIEW | (1 << 11) | (1 << 14);

/// Discord's numbers from the guild object.
pub struct GuildSafety {
    pub id: u64,
    pub mfa: u8,
    pub verification: u8,
    pub content_filter: u8,
    pub notifications: u8,
}

/// What the check needs from Brim's own config.
#[derive(Clone)]
pub struct Setup {
    pub quarantine_role: Option<u64>,
    pub log_fallback: bool,
    /// Staff permits (Admins, Moderators) without the `automod` whitelist flag.
    pub unwhitelisted_staff: Vec<String>,
    /// The bot's own roles.
    pub bot_roles: Vec<u64>,
    pub mod_log: bool,
    /// (module name, switched on) for the protection modules.
    pub modules: Vec<(&'static str, bool)>,
    /// Lockdown main roles; their channel overwrites are held to the @everyone rules.
    pub main_roles: Vec<u64>,
    /// Roles on the Moderators permit and not on Admins.
    pub mod_roles: Vec<u64>,
    /// Members whose own channel overwrites may grant anything: owner, extra owners, the bot.
    pub trusted: Vec<u64>,
    /// Every channel a log goes to.
    pub log_channels: Vec<u64>,
    /// Brim's user id: the only one a log channel lets delete messages ([`LOG_LOCK`]).
    pub bot: u64,
    /// Members in verification's join quarantine must read it: its quarantine overwrite has
    /// [`crate::verification_shape`] instead of the full deny.
    pub verification_channel: Option<u64>,
    /// The roles verification gives; empty means verification is off.
    pub verification_roles: Vec<u64>,
    /// The staff permits' roles (Moderators and up): they may hold Mention @everyone.
    pub staff_roles: Vec<u64>,
}

/// Ordered from fine to worst.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Ok,
    Warn,
    Bad,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Fix {
    pub label: &'static str,
    /// A role id, or `*` for every role [`strip_targets`] picks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    /// With `role_id`: the overwrite on this channel (and its synced children) whose target is
    /// `role_id`, a role or member id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// A guild setting [`setting_fix`] knows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting: Option<&'static str>,
    /// With `role_id`: move that role right below the bot's top role.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub move_under_bot: bool,
}

#[derive(Serialize, Debug)]
pub struct Check {
    pub id: String,
    pub severity: Severity,
    pub title: &'static str,
    pub detail: String,
    pub fix: Option<Fix>,
    /// How to fix it by hand when the bot cannot: short steps in Discord or on another page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
}

fn strip(role: impl ToString) -> Option<Fix> {
    Some(Fix {
        label: "Strip permissions",
        role_id: Some(role.to_string()),
        channel_id: None,
        setting: None,
        move_under_bot: false,
    })
}

fn set(setting: &'static str) -> Option<Fix> {
    act("Fix", setting)
}

fn act(label: &'static str, setting: &'static str) -> Option<Fix> {
    Some(Fix {
        label,
        role_id: None,
        channel_id: None,
        setting: Some(setting),
        move_under_bot: false,
    })
}

/// The [`Fix::setting`] that hides a log channel with [`private_overwrites`].
pub const PRIVATE: &str = "private_channel";

/// The [`Fix::setting`] that puts the quarantine deny on every channel, as Auto Setup does.
pub const QUARANTINE_CHANNELS: &str = "quarantine_channels";

/// The [`Fix::setting`] that makes the quarantine role as Auto Setup does, and denies it everywhere.
pub const QUARANTINE_ROLE: &str = "quarantine_role";

/// The [`Fix::setting`] that makes a Member role as Auto Setup does and sets it as the main role.
pub const MEMBER_ROLE: &str = "member_role";

/// The [`Fix::setting`]s that make the mod log or the fallback log channel as Auto Setup does.
pub const MOD_LOG: &str = "mod_log_channel";
pub const LOG_FALLBACK: &str = "fallback_log_channel";

/// The [`Fix::setting`] that adds @everyone's permissions to each main role and clears @everyone.
pub const EVERYONE_TO_MAIN: &str = "everyone_to_main";

/// The protection modules: (name, config section, the [`Fix::setting`] that switches it on).
pub const MODULES: [(&str, &str, &str); 4] = [
    ("Anti-nuke", "antinuke", "enable_antinuke"),
    ("Join gate", "joingate", "enable_joingate"),
    ("Join raid", "joinraid", "enable_joinraid"),
    ("Verification", "verification", "enable_verification"),
];

/// The [`Fix::setting`] that takes [`EXPRESSION_PERMS`] from every role [`expression_targets`] picks.
pub const EXPRESSIONS: &str = "expression_perms";

/// Manage Expressions, Manage Events, Create Expressions and Create Events: Admins only.
pub const EXPRESSION_PERMS: u64 = (1 << 30) | (1 << 33) | (1 << 43) | (1 << 44);

/// Roles that can create or manage emojis, stickers, soundboard sounds or events but should not,
/// and that the bot can change: @everyone, the main roles and Moderator-only roles, not managed,
/// below the bot.
pub fn expression_targets<'a>(
    roles: &'a [RoleImg],
    guild: u64,
    bot_top: i64,
    setup: &Setup,
) -> Vec<&'a RoleImg> {
    roles
        .iter()
        .filter(|r| {
            (r.id == guild || setup.main_roles.contains(&r.id) || setup.mod_roles.contains(&r.id))
                && !r.managed
                && r.position < bot_top
                && r.permissions & EXPRESSION_PERMS != 0
        })
        .collect()
}

/// The safer value a setting fix writes: (Discord field, value).
pub fn setting_fix(setting: &str) -> Option<(&'static str, u8)> {
    match setting {
        "verification_level" => Some(("verification_level", 2)),
        "content_filter" => Some(("explicit_content_filter", 2)),
        "notifications" => Some(("default_message_notifications", 1)),
        _ => None,
    }
}

/// What a role must not hold: [`DANGEROUS`], or [`crate::STAFF_DANGEROUS`] for a staff permit
/// role that is not @everyone or a main role (every member has those).
fn role_mask(role_id: u64, guild: u64, main_roles: &[u64], staff_roles: &[u64]) -> u64 {
    if role_id != guild && !main_roles.contains(&role_id) && staff_roles.contains(&role_id) {
        crate::STAFF_DANGEROUS
    } else {
        DANGEROUS
    }
}

/// The permissions a role keeps after a strip fix.
pub fn stripped(
    role_id: u64,
    guild: u64,
    (main_roles, staff_roles): (&[u64], &[u64]),
    perms: u64,
) -> u64 {
    perms & !role_mask(role_id, guild, main_roles, staff_roles)
}

/// Names of the [`DANGEROUS`] bits a channel overwrite can hold (Manage Roles is called Manage
/// Permissions there).
fn names(bits: u64) -> String {
    const NAMES: [(u64, &str); 7] = [
        (0x10, "Manage Channels"),
        (0x1000_0000, "Manage Permissions"),
        (0x2000_0000, "Manage Webhooks"),
        (1 << 13, "Manage Messages"),
        (1 << 17, "Mention @everyone, @here, and All Roles"),
        (1 << 33, "Manage Events"),
        (1 << 34, "Manage Threads and Posts"),
    ];
    NAMES
        .iter()
        .filter(|(b, _)| bits & b != 0)
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(", ")
}

fn overwrite_mask(o: &OverwriteImg, guild: u64, setup: &Setup) -> u64 {
    match (
        o.is_role,
        o.target == guild || setup.main_roles.contains(&o.target),
    ) {
        (false, _) if setup.trusted.contains(&o.target) => 0,
        (true, false) if setup.staff_roles.contains(&o.target) => crate::STAFF_DANGEROUS,
        _ => DANGEROUS,
    }
}

/// A channel is synced when it has its category's overwrites, in any order.
pub fn synced(c: &ChannelImg, cat: &ChannelImg) -> bool {
    c.overwrites.len() == cat.overwrites.len()
        && c.overwrites.iter().all(|o| cat.overwrites.contains(o))
}

/// Children of `cat` that are synced with it, so a category fix keeps them synced.
pub fn synced_children<'a>(channels: &'a [ChannelImg], cat: &ChannelImg) -> Vec<&'a ChannelImg> {
    channels
        .iter()
        .filter(|c| c.parent == Some(cat.id) && synced(c, cat))
        .collect()
}

/// Channel and category overwrites that grant dangerous permissions to anyone but managed
/// (bot) roles and trusted members. Synced children are left out: their category's row covers
/// them. Private channels count like any other.
pub fn overwrite_targets<'a>(
    channels: &'a [ChannelImg],
    roles: &[RoleImg],
    guild: u64,
    setup: &Setup,
) -> Vec<(&'a ChannelImg, &'a OverwriteImg)> {
    let managed = |id| roles.iter().any(|r| r.id == id && r.managed);
    channels
        .iter()
        .filter(|c| {
            c.parent
                .and_then(|p| channels.iter().find(|x| x.id == p))
                .is_none_or(|cat| !synced(c, cat))
        })
        .flat_map(|c| c.overwrites.iter().map(move |o| (c, o)))
        .filter(|(_, o)| {
            !(o.is_role && managed(o.target)) && o.allow & overwrite_mask(o, guild, setup) != 0
        })
        .collect()
}

/// The overwrite after an overwrite fix: dangerous allows dropped, deny kept.
pub fn stripped_overwrite(o: &OverwriteImg, guild: u64, setup: &Setup) -> OverwriteImg {
    OverwriteImg {
        allow: o.allow & !overwrite_mask(o, guild, setup),
        ..o.clone()
    }
}

/// Discord's role order: higher position first, the lower id first on a tie.
fn rank(r: &RoleImg) -> (i64, std::cmp::Reverse<u64>) {
    (r.position, std::cmp::Reverse(r.id))
}

/// Roles ranked between `q` and the bot's top role, highest first (by Discord's order, as
/// positions can tie).
pub fn between_bot_and<'a>(
    roles: &'a [RoleImg],
    q: &RoleImg,
    bot_top: i64,
    bot_roles: &[u64],
) -> Vec<&'a RoleImg> {
    let bot = roles
        .iter()
        .filter(|r| bot_roles.contains(&r.id))
        .map(rank)
        .max()
        // Without the bot's role list, a role tied with the bot counts as below it.
        .unwrap_or((bot_top, std::cmp::Reverse(0)));
    let mut v: Vec<&RoleImg> = roles
        .iter()
        .filter(|r| rank(r) > rank(q) && rank(r) < bot)
        .collect();
    v.sort_by_key(|r| std::cmp::Reverse(rank(r)));
    v
}

/// New `(role, position)` pairs that put the quarantine role right below the bot: the roles in
/// between and it trade positions, so no role outside that block moves. Empty for any other role,
/// so the fix cannot lift a staff role up the list.
pub fn move_under_bot(
    roles: &[RoleImg],
    target: u64,
    bot_top: i64,
    setup: &Setup,
) -> Vec<(u64, i64)> {
    let Some(q) = roles
        .iter()
        .find(|r| r.id == target && setup.quarantine_role == Some(target))
    else {
        return vec![];
    };
    let mid = between_bot_and(roles, q, bot_top, &setup.bot_roles);
    let Some(top) = mid.first() else {
        return vec![];
    };
    // A role tied with the bot's position may not take that position itself.
    let below = mid.iter().skip(1).map(|r| r.position).chain([q.position]);
    std::iter::once((q.id, top.position.min(bot_top - 1)))
        .chain(mid.iter().map(|r| r.id).zip(below))
        .collect()
}

/// Whether @everyone, or a main role every member gets, can view `c` (Administrator sees all).
pub fn is_public(c: &ChannelImg, roles: &[RoleImg], guild: u64, main_roles: &[u64]) -> bool {
    let perms = |id| {
        roles
            .iter()
            .find(|r| r.id == id)
            .map_or(0, |r| r.permissions)
    };
    let ow = |id| {
        c.overwrites
            .iter()
            .find(|o| o.is_role && o.target == id)
            .map_or((0, 0), |o| (o.allow, o.deny))
    };
    let sees = |base: u64, ows: &[(u64, u64)]| {
        base & ADMINISTRATOR != 0 || ows.iter().fold(base, |p, (a, d)| (p & !d) | a) & VIEW != 0
    };
    sees(perms(guild), &[ow(guild)])
        || main_roles
            .iter()
            .any(|&r| sees(perms(guild) | perms(r), &[ow(guild), ow(r)]))
}

/// The overwrites that make `c` private: @everyone and the main roles are denied View Channel
/// (their other bits kept), the bot member gets View, Send and Embed Links. Empty when @everyone
/// is already denied and no main role is let back in.
pub fn private_overwrites(
    c: &ChannelImg,
    guild: u64,
    bot: u64,
    main_roles: &[u64],
) -> Vec<OverwriteImg> {
    let find = |id, is_role| {
        c.overwrites
            .iter()
            .find(|o| o.target == id && o.is_role == is_role)
            .cloned()
            .unwrap_or(OverwriteImg {
                target: id,
                is_role,
                allow: 0,
                deny: 0,
            })
    };
    let everyone = find(guild, true);
    let mains: Vec<_> = main_roles.iter().map(|&r| find(r, true)).collect();
    if everyone.deny & VIEW != 0 && mains.iter().all(|o| o.allow & VIEW == 0) {
        return vec![];
    }
    let hide = |o: OverwriteImg| OverwriteImg {
        allow: o.allow & !VIEW,
        deny: o.deny | VIEW,
        ..o
    };
    let me = find(bot, false);
    std::iter::once(hide(everyone))
        .chain(mains.into_iter().map(hide))
        .chain([OverwriteImg {
            allow: me.allow | BOT_LOG,
            deny: me.deny & !BOT_LOG,
            ..me
        }])
        .collect()
}

/// What nobody but Brim may have on a log channel (owner decision 11-15): Manage Messages (delete
/// or purge) and Manage Channels (delete the channel). @everyone's deny takes a role's grant away
/// there; Administrator and the server owner ignore overwrites, so for them it is alert only.
pub const LOG_LOCK: u64 = (1 << 13) | (1 << 4);

/// The category the bot keeps its log channels in. One name and one shape for the dashboard and
/// the bot, so neither makes a second one (owner decision 11-15).
pub const LOGS_CATEGORY: &str = "Logs";

/// Whether a new #ticket-logs takes the Logs category's overwrites (owner decision 2026-11-18: it
/// follows Logs by default): only when every ticket staff role already sees the category, so
/// nobody who reads the ticket log today loses it. Otherwise it keeps its own staff overwrites.
pub fn ticket_log_follows(logs: &ChannelImg, ticket_staff: &[u64]) -> bool {
    ticket_staff.iter().all(|&r| {
        logs.overwrites
            .iter()
            .any(|o| o.is_role && o.target == r && o.allow & (1 << 10) != 0)
    })
}

/// A new private bot category ("Logs", "Staff"): hidden from @everyone, which is also denied
/// [`LOG_LOCK`]; Brim sees it, posts and keeps the lock rights. Staff see it through roles.
pub fn private_category(guild: u64, bot: u64) -> Vec<OverwriteImg> {
    vec![
        OverwriteImg {
            target: guild,
            is_role: true,
            allow: 0,
            deny: (1 << 10) | LOG_LOCK,
        },
        OverwriteImg {
            target: bot,
            is_role: false,
            allow: 19456 | LOG_LOCK,
            deny: 0,
        },
    ]
}

/// The overwrite changes that lock log channel `c`: Brim's member overwrite with it (it
/// deletes report messages and edits the channel), first, so the bot never locks itself out;
/// every other overwrite without it; @everyone denied [`LOG_LOCK`]. Empty when `c` is locked.
pub fn lock_overwrites(c: &ChannelImg, guild: u64, bot: u64) -> Vec<OverwriteImg> {
    let me = |o: &OverwriteImg| !o.is_role && o.target == bot;
    let everyone = |o: &OverwriteImg| o.is_role && o.target == guild;
    let find = |f: &dyn Fn(&OverwriteImg) -> bool, target, is_role| {
        c.overwrites
            .iter()
            .find(|o| f(o))
            .cloned()
            .unwrap_or(OverwriteImg {
                target,
                is_role,
                allow: 0,
                deny: 0,
            })
    };
    let mut out = vec![];
    let mine = find(&me, bot, false);
    let want = BOT_LOG | LOG_LOCK;
    if mine.allow & want != want || mine.deny & want != 0 {
        out.push(OverwriteImg {
            allow: mine.allow | want,
            deny: mine.deny & !want,
            ..mine
        });
    }
    out.extend(
        c.overwrites
            .iter()
            .filter(|o| !me(o) && !everyone(o) && o.allow & LOG_LOCK != 0)
            .map(|o| OverwriteImg {
                allow: o.allow & !LOG_LOCK,
                ..o.clone()
            }),
    );
    let all = find(&everyone, guild, true);
    if all.deny & LOG_LOCK != LOG_LOCK || all.allow & LOG_LOCK != 0 {
        out.push(OverwriteImg {
            allow: all.allow & !LOG_LOCK,
            deny: all.deny | LOG_LOCK,
            ..all
        });
    }
    out
}

/// [`private_overwrites`] and then [`lock_overwrites`] on top: what a log channel needs.
pub fn log_overwrites(
    c: &ChannelImg,
    guild: u64,
    bot: u64,
    main_roles: &[u64],
) -> Vec<OverwriteImg> {
    let mut ows = private_overwrites(c, guild, bot, main_roles);
    let mut img = c.clone();
    for o in &ows {
        img.overwrites
            .retain(|x| (x.target, x.is_role) != (o.target, o.is_role));
        img.overwrites.push(o.clone());
    }
    for o in lock_overwrites(&img, guild, bot) {
        match ows
            .iter_mut()
            .find(|x| (x.target, x.is_role) == (o.target, o.is_role))
        {
            Some(x) => *x = o,
            None => ows.push(o),
        }
    }
    ows
}

/// Roles the bot can safely strip in one go: not managed, not @everyone, below the bot.
pub fn strip_targets<'a>(
    roles: &'a [RoleImg],
    guild: u64,
    bot_top: i64,
    (main_roles, staff_roles): (&[u64], &[u64]),
) -> Vec<&'a RoleImg> {
    roles
        .iter()
        .filter(|r| {
            !r.managed
                && r.id != guild
                && r.position < bot_top
                && r.permissions & role_mask(r.id, guild, main_roles, staff_roles) != 0
        })
        .collect()
}

pub fn checks(
    g: &GuildSafety,
    roles: &[RoleImg],
    channels: &[ChannelImg],
    bot_top: i64,
    setup: &Setup,
) -> Vec<Check> {
    let mut out = vec![];
    let mut row = |id: &str, severity, title, detail: String, fix| {
        out.push(Check {
            id: id.into(),
            severity,
            title,
            detail,
            fix,
            steps: vec![],
        })
    };
    let quarantine = setup
        .quarantine_role
        .and_then(|q| roles.iter().find(|r| r.id == q));
    match (setup.quarantine_role, quarantine) {
        (None, _) => row(
            "quarantine_unset",
            Severity::Bad,
            "No quarantine role",
            "Anti-nuke has no role to quarantine offenders into.".into(),
            act("Create role", QUARANTINE_ROLE),
        ),
        (Some(_), None) => row(
            "quarantine_missing",
            Severity::Bad,
            "Quarantine role missing",
            "The configured quarantine role no longer exists.".into(),
            act("Create role", QUARANTINE_ROLE),
        ),
        (Some(_), Some(q)) => {
            if q.position >= bot_top {
                row(
                    "quarantine_above_bot",
                    Severity::Bad,
                    "Quarantine role above the bot",
                    format!(
                        "{} is not below the bot's top role, so the bot cannot assign it.",
                        q.name
                    ),
                    None,
                );
            } else if !between_bot_and(roles, q, bot_top, &setup.bot_roles).is_empty() {
                row(
                    "quarantine_not_right_under",
                    Severity::Warn,
                    "Quarantine role not right below the bot",
                    format!(
                        "{} is not right below the bot's top role. A role in between could give quarantined members permissions.",
                        q.name
                    ),
                    Some(Fix {
                        label: "Move role",
                        role_id: Some(q.id.to_string()),
                        channel_id: None,
                        setting: None,
                        move_under_bot: true,
                    }),
                );
            }
            if q.permissions & DANGEROUS != 0 {
                row(
                    "quarantine_perms",
                    Severity::Bad,
                    "Quarantine role is too powerful",
                    format!("{} grants dangerous permissions.", q.name),
                    strip(q.id),
                );
            }
        }
    }
    let everyone = roles
        .iter()
        .find(|r| r.id == g.id)
        .map_or(0, |r| r.permissions);
    // A role deleted in Discord stays in the config: only live ones count.
    let live = |ids: &[u64]| ids.iter().any(|m| roles.iter().any(|r| r.id == *m));
    let mains = live(&setup.main_roles);
    let verify_gone = !setup.verification_roles.is_empty() && !live(&setup.verification_roles);
    if everyone & DANGEROUS != 0 {
        row(
            "everyone_perms",
            Severity::Bad,
            "@everyone holds dangerous permissions",
            "Every member, raid accounts too, can ban, manage, mention @everyone or delete messages.".into(),
            strip(g.id),
        );
    } else if everyone != 0 {
        row(
            "everyone_perms",
            Severity::Warn,
            "@everyone holds permissions",
            "Move them to your main member role, so new and unverified accounts start with nothing.".into(),
            // Only onto main roles that exist: the fix never makes one (owner decision 11-15).
            match mains {
                false => None,
                true => act("Move to main roles", EVERYONE_TO_MAIN),
            },
        );
    } else {
        row(
            "everyone_perms",
            Severity::Ok,
            "@everyone is safe",
            "New accounts get no permissions until a role gives them some.".into(),
            None,
        );
    }
    let expressions = expression_targets(roles, g.id, bot_top, setup);
    if expressions.is_empty() {
        row(
            "expression_perms",
            Severity::Ok,
            "Emojis, stickers, sounds and events",
            "Members and Moderators cannot create or manage emojis, stickers, soundboard sounds or events.".into(),
            None,
        );
    } else {
        let names: Vec<&str> = expressions.iter().map(|r| r.name.as_str()).collect();
        row(
            "expression_perms",
            Severity::Warn,
            "Members can create emojis, stickers, sounds or events",
            format!(
                "{} can create or manage emojis, stickers, soundboard sounds or events. Only Admins should.",
                names.join(", ")
            ),
            Some(Fix {
                label: "Strip permissions",
                role_id: None,
                channel_id: None,
                setting: Some(EXPRESSIONS),
                move_under_bot: false,
            }),
        );
    }
    // (good, text when good, text when not) for the setup rows below.
    let pick = |good: bool, yes: &str, no: String| {
        (
            if good { Severity::Ok } else { Severity::Warn },
            if good { yes.into() } else { no },
        )
    };
    let bot = roles
        .iter()
        .filter(|r| setup.bot_roles.contains(&r.id))
        .fold(0, |p, r| p | r.permissions);
    let (sev, text) = pick(
        bot == ADMINISTRATOR,
        "The bot's roles grant Administrator and nothing else.",
        "Give the bot's role Administrator and nothing else, so its access is one clear switch."
            .into(),
    );
    row("bot_admin_only", sev, "Bot role permissions", text, None);
    // An empty staff permit is no warning: people are added to a permit only when needed (owner decision 11-16).
    let staff = &setup.unwhitelisted_staff;
    let (sev, text) = pick(
        staff.is_empty(),
        "Staff are whitelisted from the automod through their permit.",
        match staff.len() {
            1 => format!("The {} permit has no Automod Immunity whitelist flag, so the automod can punish its members.", staff[0]),
            _ => format!("The {} permits have no Automod Immunity whitelist flag, so the automod can punish their members.", staff.join(" and ")),
        },
    );
    row("whitelist", sev, "Automod whitelist", text, None);
    let (sev, text) = pick(
        setup.mod_log,
        "Staff actions are recorded as cases in the mod log channel.",
        "Pick a mod log channel so staff actions are recorded.".into(),
    );
    let f = if setup.mod_log {
        None
    } else {
        act("Create channel", MOD_LOG)
    };
    row("mod_log", sev, "Mod log channel", text, f);
    let (sev, text) = pick(
        mains && !verify_gone,
        "Lockdown and verification know your member roles.",
        match (setup.main_roles.is_empty(), mains) {
            (true, _) => "Set your main member roles so lockdowns and verification work.",
            (false, false) => "Your saved main roles were deleted in Discord, so lockdowns and verification have no member role.",
            (false, true) => "Your verification roles were deleted in Discord, so members who verify get no role.",
        }
        .into(),
    );
    let f = match (mains, verify_gone) {
        (false, _) => act("Create Member role", MEMBER_ROLE),
        (true, true) => act("Use main roles", MEMBER_ROLE),
        (true, false) => None,
    };
    row("main_roles", sev, "Main roles", text, f);
    for (name, on) in &setup.modules {
        let (mut sev, text) = pick(
            *on,
            "Switched on.",
            format!("{name} is off, so it does not protect the server."),
        );
        // Without anti-nuke nothing stops a staff account that goes rogue.
        if *name == "Anti-nuke" && !on {
            sev = Severity::Bad;
        }
        let f = MODULES
            .iter()
            .find(|m| m.0 == *name && !on)
            .and_then(|m| act("Turn on", m.2));
        row(
            &format!("module_{}", name.to_lowercase().replace(' ', "_")),
            sev,
            *name,
            text,
            f,
        );
    }
    let above: Vec<&str> = roles
        .iter()
        .filter(|r| r.position > bot_top && !r.managed && r.id != g.id)
        .map(|r| r.name.as_str())
        .collect();
    if above.is_empty() {
        row(
            "bot_position",
            Severity::Ok,
            "Bot role is near the top",
            "No manageable roles sit above the bot.".into(),
            None,
        );
    } else {
        row(
            "bot_position",
            Severity::Bad,
            "Roles above the bot",
            format!(
                "The bot cannot act on members holding: {}.",
                above.join(", ")
            ),
            None,
        );
    }
    // Any role, bot roles included, counts: a raider's bot sitting above ours wins the race.
    let higher = roles
        .iter()
        .filter(|r| r.position > bot_top && r.id != g.id)
        .count();
    let (sev, text) = match higher {
        0 => (Severity::Ok, "No role sits above it.".to_string()),
        1..8 => (
            Severity::Ok,
            if higher == 1 {
                "Only 1 role sits above it.".to_string()
            } else {
                format!("Only {higher} roles sit above it.")
            },
        ),
        _ => (
            Severity::Warn,
            format!("{higher} roles sit above it. Move it up so a raider's bot cannot outrank it."),
        ),
    };
    row("bot_top_eight", sev, "Bot role among the top 8", text, None);
    for r in roles.iter().filter(|r| {
        !r.managed
            && r.id != g.id
            && r.permissions & role_mask(r.id, g.id, &setup.main_roles, &setup.staff_roles) != 0
            && quarantine.is_none_or(|q| q.id != r.id)
    }) {
        let mask = role_mask(r.id, g.id, &setup.main_roles, &setup.staff_roles);
        let detail = format!(
            "{} holds: {}.",
            r.name,
            crate::dangerous_names(r.permissions & mask).join(", ")
        );
        row(
            &format!("role_{}", r.id),
            Severity::Bad,
            "Role holds dangerous permissions",
            detail,
            strip(r.id),
        );
    }
    let overwrites = overwrite_targets(channels, roles, g.id, setup);
    for (c, o) in &overwrites {
        let who = if o.target == g.id {
            "@everyone".into()
        } else if o.is_role {
            roles
                .iter()
                .find(|r| r.id == o.target)
                .map_or(format!("role {}", o.target), |r| r.name.clone())
        } else {
            format!("member {}", o.target)
        };
        let place = match synced_children(channels, c).len() {
            0 if c.kind == CATEGORY => format!("Category {}", c.name),
            0 => format!("#{}", c.name),
            n => format!("Category {} and its {n} synced channel(s)", c.name),
        };
        row(
            &format!("overwrite_{}_{}", c.id, o.target),
            Severity::Bad,
            "Channel grants dangerous permissions",
            format!(
                "{place} gives {who}: {}.",
                names(o.allow & overwrite_mask(o, g.id, setup))
            ),
            Some(Fix {
                label: "Strip permissions",
                role_id: Some(o.target.to_string()),
                channel_id: Some(c.id.to_string()),
                setting: None,
                move_under_bot: false,
            }),
        );
    }
    let strippable = strip_targets(
        roles,
        g.id,
        bot_top,
        (&setup.main_roles, &setup.staff_roles),
    )
    .len()
        + overwrites.len();
    if strippable > 1 {
        row(
            "roles_bulk",
            Severity::Warn,
            "Remove dangerous permissions everywhere",
            format!(
                "{strippable} roles or channel overwrites can administer, ban, kick, time out or manage the server."
            ),
            Some(Fix {
                label: "Strip all",
                role_id: Some("*".into()),
                channel_id: None,
                setting: None,
                move_under_bot: false,
            }),
        );
    }
    // Auto Setup and Channel Setup deny the quarantine role View Channel everywhere; a synced
    // channel is covered by its category.
    if let Some(q) = quarantine {
        let open: Vec<&str> = channels
            .iter()
            .filter(|c| {
                c.parent
                    .and_then(|p| channels.iter().find(|x| x.id == p))
                    .is_none_or(|cat| !synced(c, cat))
            })
            .filter(|c| {
                !c.overwrites.iter().any(|o| {
                    o.is_role
                        && o.target == q.id
                        && match Some(c.id) == setup.verification_channel {
                            true => crate::verification_shape(o.allow, o.deny),
                            false => o.deny & VIEW != 0,
                        }
                })
            })
            .map(|c| c.name.as_str())
            .collect();
        if !open.is_empty() {
            row(
                "quarantine_channels",
                Severity::Bad,
                "Quarantined members can see channels",
                format!(
                    "{} is not denied View Channel in {} channel(s): {}{}.",
                    q.name,
                    open.len(),
                    open[..open.len().min(10)].join(", "),
                    if open.len() > 10 { ", ..." } else { "" }
                ),
                set(QUARANTINE_CHANNELS),
            );
        }
    }
    if !setup.log_fallback {
        row(
            "log_fallback",
            Severity::Warn,
            "No fallback log channel",
            "Events without a dedicated channel are dropped.".into(),
            act("Create channel", LOG_FALLBACK),
        );
    }
    for c in channels
        .iter()
        .filter(|c| setup.log_channels.contains(&c.id))
        .filter(|c| is_public(c, roles, g.id, &setup.main_roles))
    {
        // Overwrites cannot hide a channel from Administrator; that needs the role changed.
        let fixable = !private_overwrites(c, g.id, 0, &setup.main_roles).is_empty();
        row(
            &format!("log_channel_public_{}", c.id),
            Severity::Bad,
            "Log channel is public",
            format!(
                "Every member can read #{}, and logs show deleted messages and staff actions. Brim does not post logs there until it is private.{}",
                c.name,
                if fixable { "" } else { " @everyone or a main role has Administrator." }
            ),
            fixable.then(|| Fix {
                label: "Make private",
                role_id: None,
                channel_id: Some(c.id.to_string()),
                setting: Some(PRIVATE),
                move_under_bot: false,
            }),
        );
    }
    for c in channels
        .iter()
        .filter(|c| setup.log_channels.contains(&c.id))
        .filter(|c| !lock_overwrites(c, g.id, setup.bot).is_empty())
    {
        row(
            &format!("log_channel_unlocked_{}", c.id),
            Severity::Bad,
            "Log channel can be purged",
            format!(
                "Someone besides Brim can delete messages in #{} or delete the channel. Brim denies Manage Messages and Manage Channels there for everyone; the server owner and Administrator still can, and Brim reports it.",
                c.name
            ),
            Some(Fix {
                label: "Lock",
                role_id: None,
                channel_id: Some(c.id.to_string()),
                setting: Some(PRIVATE),
                move_under_bot: false,
            }),
        );
    }
    let fix = |good: bool, s| if good { None } else { set(s) };
    // Each with a text for the good state, so a green row never reads as a to-do.
    for (id, good, title, yes, no) in [
        (
            "verification_level",
            g.verification >= 2,
            "Server verification level",
            "Medium or higher, so brand new accounts cannot post at once.",
            "Set it to at least Medium so brand new accounts cannot post at once.",
        ),
        (
            "content_filter",
            g.content_filter == 2,
            "Explicit media content filter",
            "Media from all members is scanned.",
            "Set it to scan media from all members.",
        ),
        (
            "notifications",
            g.notifications != 0,
            "Default notifications",
            "Mentions only, so a spam raid does not notify every member.",
            "Set them to mentions only, so a spam raid does not notify every member for every message.",
        ),
        (
            "mfa",
            g.mfa != 0,
            "2FA for moderators",
            "Moderators need 2FA, so a stolen password alone cannot ban or delete.",
            "Moderators do not need 2FA, so a stolen password alone can ban or delete.",
        ),
    ] {
        let (sev, text) = pick(good, yes, no.into());
        // 2FA is the owner's own account setting; the bot cannot switch it.
        let f = if id == "mfa" { None } else { fix(good, id) };
        row(id, sev, title, text, f);
    }
    for c in out
        .iter_mut()
        .filter(|c| c.severity != Severity::Ok && c.fix.is_none())
    {
        c.steps = steps(&c.id);
    }
    out
}

/// How to fix a row by hand, for the rows the bot cannot fix itself; empty for the others.
pub fn steps(id: &str) -> Vec<String> {
    let id = if id.starts_with("log_channel_public_") {
        "log_channel_public"
    } else if id.starts_with("message_webhook_gone_") {
        "message_webhook_gone"
    } else if id.starts_with("feed_broken_") {
        "feed_broken"
    } else {
        id
    };
    let s: &[&str] = match id {
        "bot_position" => &[
            "Open Server Settings > Roles.",
            "Drag the Brim role above the roles named here.",
            "Click Save Changes.",
        ],
        "bot_top_eight" => &[
            "Open Server Settings > Roles.",
            "Drag the Brim role up until 7 or fewer roles are above it.",
            "Click Save Changes.",
        ],
        "quarantine_above_bot" => &[
            "Open Server Settings > Roles.",
            "Drag the quarantine role below the Brim role.",
            "Click Save Changes.",
        ],
        "bot_admin_only" => &[
            "Open Server Settings > Roles and click the Brim role.",
            "Open Permissions and turn on Administrator.",
            "Turn off every other permission, then click Save Changes.",
        ],
        "mfa" => &[
            "The server owner opens Server Settings > Safety Setup.",
            "Turn on \"Require 2FA for moderator actions\".",
        ],
        "everyone_perms" => &[
            "Set your main roles first, for example with Create Member role on the Main roles row.",
            "Then click Move to main roles here.",
        ],
        "whitelist" => &[
            "Open the Permits page.",
            "Turn on Automod Immunity on each permit named here, then save.",
        ],
        "sections_unread" => &[
            "Update Brim again.",
            "Or open each page named here and save it.",
        ],
        "message_webhook_gone" => &[
            "Open the message (the link here opens its Send tab).",
            "In its Sent log, click Send again here (or pick a new channel if that one was deleted, then Send).",
            "In Discord, delete the old copy (it can no longer be edited).",
        ],
        "feed_broken" => &[
            "Fix the cause named here: give Brim Manage Webhooks in that channel, pick another channel, or correct the feed address.",
            "Open the Feeds page (the link here).",
            "Click Send latest in the feed's menu.",
        ],
        "channel_setup_draft" => &[
            "Open the Channel Setup page.",
            "Fix the problems it marks.",
            "Click Push.",
        ],
        "log_channel_public" => &[
            "Open Server Settings > Roles.",
            "Turn off Administrator on @everyone and your main roles.",
            "Click Make private here.",
        ],
        _ => &[],
    };
    s.iter().map(|x| x.to_string()).collect()
}

/// Share of checks that pass, 0-100. A note ([`NOTE`] id) is shown but not counted.
pub fn score(checks: &[Check]) -> usize {
    let checks: Vec<&Check> = checks.iter().filter(|c| !c.id.starts_with(NOTE)).collect();
    let ok = checks.iter().filter(|c| c.severity == Severity::Ok).count();
    (ok * 100).checked_div(checks.len()).unwrap_or(0)
}

/// Id prefix of a row that only informs: [`score`] skips it.
pub const NOTE: &str = "note_";

/// Staff roles may mention @everyone (owner decision 2026-11-16): shown so the owner sees it, "but not
/// worry about it". None when no staff role holds it.
/// Fix setting: show each staff role as its own group in the member list.
pub const STAFF_HOIST: &str = "staff_hoist";

/// The Overview row for staff roles below the bot that are not shown as their own group in the
/// member list (owner decision 2026-11-17); roles at or above the bot are out of its reach.
pub fn staff_hoist_check(roles: &[RoleImg], staff: &[i64], bot_top: i64) -> Option<Check> {
    let names: Vec<&str> = staff_to_hoist(roles, staff, bot_top)
        .map(|r| r.name.as_str())
        .collect();
    (!names.is_empty()).then(|| Check {
        id: "staff_hoist".into(),
        severity: Severity::Warn,
        title: "Staff roles not shown separately",
        detail: format!(
            "{} not shown as their own group in the member list, so members cannot see who is staff.",
            match names.len() {
                1 => format!("{} is", names[0]),
                _ => format!("{} are", names.join(", ")),
            }
        ),
        fix: act("Show separately", STAFF_HOIST),
        steps: vec![],
    })
}

/// The staff roles [`staff_hoist_check`] lists and its Fix changes.
pub fn staff_to_hoist<'a>(
    roles: &'a [RoleImg],
    staff: &'a [i64],
    bot_top: i64,
) -> impl Iterator<Item = &'a RoleImg> {
    roles
        .iter()
        .filter(move |r| staff.contains(&(r.id as i64)) && !r.hoist && r.position < bot_top)
}

/// Fix setting: give the bot its own display role, so it shows as its own group in its colour.
pub const BOT_HOIST: &str = "bot_hoist";

/// The Overview row when the bot is not its own group in the member list or has no name colour
/// (owner decision 2026-11-17). Discord lists a member under their highest hoisted role and colours them
/// by their highest coloured role; a bot cannot edit its own role, so the Fix adds a second one.
pub fn bot_hoist_check(roles: &[RoleImg], bot_roles: &[u64]) -> Option<Check> {
    let mine = || roles.iter().filter(|r| bot_roles.contains(&r.id));
    let shown = mine().any(|r| r.hoist);
    let coloured = mine().any(|r| r.color != 0);
    let what = match (shown, coloured) {
        (true, true) => return None,
        (false, false) => "not shown as its own group in the member list and has no name colour",
        (false, true) => "not shown as its own group in the member list",
        (true, false) => "has no name colour",
    };
    Some(Check {
        id: "bot_hoist".into(),
        severity: Severity::Warn,
        title: "Brim not shown separately",
        detail: format!("Brim is {what}, so members cannot easily see it is the bot."),
        fix: act("Show separately", BOT_HOIST),
        steps: vec![],
    })
}

/// Fix setting: move the ticket log channel into the Logs category.
pub const TICKET_LOG_PLACE: &str = "ticket_log_place";

/// The Overview row when the ticket log channel is not in the Logs category with the other log
/// channels (owner decision 2026-11-17: "ticket logs should be in staff logs category").
pub fn ticket_log_check(channels: &[ChannelImg], log: Option<u64>) -> Option<Check> {
    let c = channels.iter().find(|c| Some(c.id) == log)?;
    let in_logs = channels
        .iter()
        .any(|p| Some(p.id) == c.parent && p.kind == CATEGORY && p.name == LOGS_CATEGORY);
    (!in_logs).then(|| Check {
        id: "ticket_log_place".into(),
        severity: Severity::Warn,
        title: "Ticket log not with the other logs",
        detail: format!(
            "#{} is not in the {LOGS_CATEGORY} category, where staff find the other log channels.",
            c.name
        ),
        fix: act("Move it", TICKET_LOG_PLACE),
        steps: vec![],
    })
}

/// Fix setting for [`catch_gone_check`].
pub const CATCH_CHANNEL: &str = "catch_channel";

/// The golden channel is on but was deleted in Discord (test server, 2026-11-18): nobody is
/// caught there, and each save tried to post its warning into the missing channel.
pub fn catch_gone_check(channels: &[ChannelImg], catch: Option<u64>, on: bool) -> Option<Check> {
    let id = catch.filter(|_| on)?;
    (!channels.iter().any(|c| c.id == id)).then(|| Check {
        id: "catch_gone".into(),
        severity: Severity::Warn,
        title: "The golden channel was deleted",
        detail: "The golden channel no longer exists in Discord, so nobody is caught there. The fix makes a new #golden-channel where Auto Setup puts it.".into(),
        fix: act("Make it again", CATCH_CHANNEL),
        steps: vec![],
    })
}

/// Fix setting for [`catch_files_check`].
pub const CATCH_FILES: &str = "catch_files";

/// Attach Files.
const ATTACH: u64 = 1 << 15;

/// The golden channel denies Attach Files to a role (Brim made it that way before 2026-11-25): a
/// spam bot that posts only a picture cannot write there, so it is never caught.
pub fn catch_files_check(channels: &[ChannelImg], catch: Option<u64>, on: bool) -> Option<Check> {
    let id = catch.filter(|_| on)?;
    let c = channels.iter().find(|c| c.id == id)?;
    let blocked = c
        .overwrites
        .iter()
        .any(|o| o.is_role && o.deny & ATTACH != 0);
    blocked.then(|| Check {
        id: "catch_files".into(),
        severity: Severity::Warn,
        title: "The golden channel blocks files",
        detail: format!("#{} does not allow files, so a spam bot that posts only a picture cannot write there and is not caught. The fix allows files there. Brim deletes every post there anyway.", c.name),
        fix: act("Allow files", CATCH_FILES),
        steps: vec![],
    })
}

pub fn staff_mention_note(roles: &[RoleImg], staff: &[i64]) -> Option<Check> {
    let names: Vec<&str> = roles
        .iter()
        .filter(|r| staff.contains(&(r.id as i64)) && r.permissions & (1 << 17) != 0)
        .map(|r| r.name.as_str())
        .collect();
    (!names.is_empty()).then(|| Check {
        id: format!("{NOTE}staff_mentions"),
        severity: Severity::Ok,
        title: "Staff can mention @everyone",
        detail: format!(
            "{} may ping @everyone and @here. This is on purpose for staff, and their pings add no extra automod heat.",
            names.join(", ")
        ),
        fix: None,
        steps: vec![],
    })
}

/// The [`Fix::setting`] that gives every open channel [`standard_overwrites`], as Auto Setup does.
pub const STANDARD_ACCESS: &str = "standard_access";

/// View Channel, Send Messages and Read Message History: what a main role gets in an open channel.
pub const MEMBER_ACCESS: u64 = VIEW | (1 << 11) | (1 << 16);

/// What the standard set needs to know about a server.
pub struct Standard<'a> {
    pub guild: u64,
    /// Lockdown main roles: verified members.
    pub main_roles: &'a [u64],
    /// Managed (bot) roles: they keep what @everyone gave them, so other bots and Brim do
    /// not lose a channel when @everyone is denied View.
    pub bot_roles: &'a [u64],
    /// Overwrite targets that do not make a channel one with its own access: the bot member and
    /// the quarantine role.
    pub known: &'a [u64],
    /// Channels with their own access set by Brim (logs, verification, catch, tickets,
    /// Information); a channel inside a skipped category is skipped too.
    pub skip: &'a [u64],
}

/// The role overwrite of `id` on `c`, or an empty one.
pub fn role_overwrite(c: &ChannelImg, id: u64) -> OverwriteImg {
    c.overwrites
        .iter()
        .find(|o| o.is_role && o.target == id)
        .cloned()
        .unwrap_or(OverwriteImg {
            target: id,
            is_role: true,
            allow: 0,
            deny: 0,
        })
}

/// The standard set for an open channel or category (owner decision 2026-11-16): @everyone cannot view,
/// each main role (and each bot role) can view, send and read history. Only what is missing is
/// added: a bit the role already allows or denies, or that @everyone denies besides View (a
/// read-only channel), stays as it is. Empty for a channel with its own access, which Channel
/// Setup handles: an overwrite for anyone but @everyone, a main or bot role or `known`,
/// @everyone let in on purpose, a main role kept out, or a private channel (@everyone denied,
/// no main role let in, and someone else let in: the Brim member in Channel Setup's limited
/// channels and Brim's Logs, Staff and Staff Chat layouts, the quarantine role in a jail).
/// Owner decision 2026-11-18: a channel is public unless Channel Setup or a Brim layout limited it,
/// so one that lets in only bot roles (an earlier run wrote the @everyone deny and lost the main
/// role's allow to a 429) gets the set. Empty without a main role.
pub fn standard_overwrites(c: &ChannelImg, s: &Standard) -> Vec<OverwriteImg> {
    let ours = |o: &OverwriteImg| {
        s.known.contains(&o.target)
            || (o.is_role
                && (o.target == s.guild
                    || s.main_roles.contains(&o.target)
                    || s.bot_roles.contains(&o.target)))
    };
    let everyone = role_overwrite(c, s.guild);
    let mains: Vec<_> = s.main_roles.iter().map(|&r| role_overwrite(c, r)).collect();
    let let_in =
        |o: &OverwriteImg| o.allow & VIEW != 0 && !(o.is_role && s.bot_roles.contains(&o.target));
    let private = everyone.deny & VIEW != 0
        && mains.iter().all(|o| o.allow & VIEW == 0)
        && c.overwrites.iter().any(let_in);
    if s.main_roles.is_empty()
        || private
        || everyone.allow & VIEW != 0
        || mains.iter().any(|o| o.deny & VIEW != 0)
        || !c.overwrites.iter().all(ours)
    {
        return vec![];
    }
    let kept = everyone.deny & !VIEW;
    let mut out = vec![];
    let bots = s.bot_roles.iter().map(|&r| role_overwrite(c, r));
    for o in mains.into_iter().chain(bots) {
        let add = MEMBER_ACCESS & !(o.allow | o.deny | kept);
        if add != 0 {
            out.push(OverwriteImg {
                allow: o.allow | add,
                ..o
            });
        }
    }
    // Last: a refused role write must not leave the channel hidden from everyone.
    if everyone.deny & VIEW == 0 {
        out.push(OverwriteImg {
            deny: everyone.deny | VIEW,
            ..everyone
        });
    }
    out
}

/// Every [`standard_overwrites`] change, as (channel, overwrite), categories first.
pub fn standard_access(channels: &[ChannelImg], s: &Standard) -> Vec<(u64, OverwriteImg)> {
    let skipped =
        |c: &ChannelImg| s.skip.contains(&c.id) || c.parent.is_some_and(|p| s.skip.contains(&p));
    let mut list: Vec<&ChannelImg> = channels.iter().filter(|c| !skipped(c)).collect();
    list.sort_by_key(|c| c.kind != CATEGORY);
    list.into_iter()
        .flat_map(|c| {
            standard_overwrites(c, s)
                .into_iter()
                .map(move |o| (c.id, o))
        })
        .collect()
}

/// The Overview row for channels that miss the standard set, and `more` (Brim's own channels
/// that let unverified members in); None when every one has it.
pub fn standard_access_check(channels: &[ChannelImg], s: &Standard, more: &[u64]) -> Option<Check> {
    let mut ids: Vec<u64> = standard_access(channels, s)
        .into_iter()
        .map(|(c, _)| c)
        .chain(more.iter().copied())
        .collect();
    ids.dedup();
    let names: Vec<String> = ids
        .iter()
        .filter_map(|id| channels.iter().find(|c| c.id == *id))
        .map(|c| match c.kind {
            CATEGORY => format!("category {}", c.name),
            _ => format!("#{}", c.name),
        })
        .collect();
    (!names.is_empty()).then(|| Check {
        id: "channel_access".into(),
        severity: Severity::Warn,
        title: "Channels without the standard permissions",
        detail: format!(
            "{} open channel(s) let members who have not verified see them, or do not let the main role view and write: {}{}. The fix only adds what is missing and never removes an overwrite; channels with their own access are left to Channel Setup.",
            names.len(),
            names[..names.len().min(10)].join(", "),
            if names.len() > 10 { ", ..." } else { "" }
        ),
        fix: Some(Fix {
            label: "Fix",
            role_id: None,
            channel_id: None,
            setting: Some(STANDARD_ACCESS),
            move_under_bot: false,
        }),
        steps: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(id: u64, position: i64, managed: bool, permissions: u64) -> RoleImg {
        RoleImg {
            id,
            name: format!("r{id}"),
            color: 0,
            permissions,
            position,
            hoist: false,
            mentionable: false,
            managed,
        }
    }

    const SAFE: GuildSafety = GuildSafety {
        id: 1,
        mfa: 1,
        verification: 2,
        content_filter: 2,
        notifications: 1,
    };

    fn setup(quarantine_role: Option<u64>, log_fallback: bool) -> Setup {
        Setup {
            quarantine_role,
            log_fallback,
            unwhitelisted_staff: vec![],
            bot_roles: vec![],
            mod_log: true,
            main_roles: vec![99],
            mod_roles: vec![],
            modules: vec![],
            trusted: vec![],
            log_channels: vec![],
            bot: 7,
            verification_channel: None,
            verification_roles: vec![],
            staff_roles: vec![5],
        }
    }

    fn sev<'a>(rows: &'a [Check], id: &str) -> Option<&'a Check> {
        rows.iter().find(|r| r.id == id)
    }

    #[test]
    fn server_settings_checks() {
        let setup = setup(None, true);
        let rows = checks(&SAFE, &[role(99, 1, false, 0)], &[], 0, &setup);
        for id in [
            "verification_level",
            "content_filter",
            "notifications",
            "mfa",
        ] {
            assert_eq!(sev(&rows, id).unwrap().severity, Severity::Ok, "{id}");
        }
        let risky = GuildSafety {
            id: 1,
            mfa: 0,
            verification: 1,
            content_filter: 0,
            notifications: 0,
        };
        let rows = checks(&risky, &[], &[], 0, &setup);
        for id in [
            "verification_level",
            "content_filter",
            "notifications",
            "mfa",
        ] {
            assert_eq!(sev(&rows, id).unwrap().severity, Severity::Warn, "{id}");
        }
        assert_eq!(
            sev(&rows, "verification_level").unwrap().fix,
            set("verification_level")
        );
        assert_eq!(sev(&rows, "mfa").unwrap().fix, None);
        assert!(sev(&rows, "mfa").unwrap().steps[0].contains("Safety Setup"));
        for s in ["verification_level", "content_filter", "notifications"] {
            assert!(setting_fix(s).is_some());
        }
    }

    #[test]
    fn score_flags_dangerous_roles_and_missing_fallback() {
        let roles = [
            role(1, 0, false, 0),
            role(2, 5, false, 0x8),
            role(3, 6, true, 0x8),
            role(4, 1, false, 0x400),
            role(9, 3, false, 0x1),
            role(5, 2, false, 1 << 13),
            role(6, 2, false, 1 << 40),
        ];
        let rows = checks(&SAFE, &roles, &[], 4, &setup(Some(9), false));
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"role_2") && !ids.contains(&"role_3") && !ids.contains(&"role_4"));
        // Manage Messages and Timeout count too.
        assert!(ids.contains(&"role_5") && ids.contains(&"role_6"));
        assert!(ids.contains(&"log_fallback") && ids.contains(&"bot_position"));
        // Members holding a role above the bot are immune to it.
        assert_eq!(sev(&rows, "bot_position").unwrap().severity, Severity::Bad);
        assert_eq!(sev(&rows, "role_2").unwrap().fix, strip(2));
        let rows = checks(&SAFE, &roles, &[], 2, &setup(Some(9), true));
        assert!(sev(&rows, "quarantine_above_bot").is_some());
    }

    #[test]
    fn everyone_with_mass_mention_is_flagged_and_strippable() {
        let roles = [role(1, 0, false, (1 << 17) | 0x400)];
        let rows = checks(&SAFE, &roles, &[], 4, &setup(None, true));
        let row = sev(&rows, "everyone_perms").unwrap();
        assert_eq!(row.severity, Severity::Bad);
        assert_eq!(row.fix, strip(1));
        assert_eq!(stripped(1, 1, (&[], &[]), (1 << 17) | 0x400), 0x400);
        assert_eq!(
            stripped(2, 1, (&[], &[2]), (1 << 17) | 0x8),
            1 << 17,
            "a staff role"
        );
        assert_eq!(stripped(2, 1, (&[], &[]), (1 << 17) | 0x8), 0);
        assert_eq!(
            stripped(2, 1, (&[2], &[2]), (1 << 17) | 0x8),
            0,
            "a main role"
        );
        // Mention @everyone is flagged on any role but a staff permit role (5 in `setup`).
        let roles = [
            role(1, 0, false, 0),
            role(99, 1, false, 1 << 17),
            role(5, 1, false, 1 << 17),
            role(6, 1, false, 1 << 17),
        ];
        let rows = checks(&SAFE, &roles, &[], 4, &setup(None, true));
        assert_eq!(sev(&rows, "role_99").unwrap().severity, Severity::Bad);
        assert!(sev(&rows, "role_5").is_none());
        assert_eq!(
            sev(&rows, "role_6").unwrap().detail,
            "r6 holds: Mention @everyone, @here, and All Roles."
        );
        let ids: Vec<u64> = strip_targets(&roles, 1, 4, (&[99], &[5]))
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, [99, 6]);
    }

    #[test]
    fn bulk_strip_skips_managed_everyone_and_roles_above_bot() {
        let roles = [
            role(1, 0, false, 0x8),
            role(2, 2, false, 0x8),
            role(3, 3, true, 0x8),
            role(4, 4, false, 0x8),
            role(5, 1, false, 0x400),
        ];
        let ids: Vec<u64> = strip_targets(&roles, 1, 4, (&[], &[]))
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, [2]);
    }

    #[test]
    fn expression_row_flags_everyone_main_and_mod_roles_only() {
        const CREATE: u64 = (1 << 43) | (1 << 44);
        let roles = [
            role(1, 0, false, CREATE | 0x400),
            role(99, 1, false, 1 << 30),
            role(7, 2, false, 1 << 33),
            role(8, 3, false, CREATE),
            role(10, 2, true, CREATE),
            role(11, 6, false, CREATE),
            role(12, 1, false, CREATE),
        ];
        let setup = Setup {
            // 8 is an Admin role (on Moderators too, so not in mod_roles); 10 managed, 11 above
            // the bot; 12 holds no permit.
            mod_roles: vec![7, 10, 11],
            main_roles: vec![99, 11],
            ..setup(None, true)
        };
        let ids: Vec<u64> = expression_targets(&roles, 1, 5, &setup)
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, [1, 99, 7]);
        let rows = checks(&SAFE, &roles, &[], 5, &setup);
        let row = sev(&rows, "expression_perms").unwrap();
        assert_eq!(row.severity, Severity::Warn);
        assert_eq!(row.detail.split(" can ").next(), Some("r1, r99, r7"));
        assert_eq!(row.fix.as_ref().unwrap().setting, Some(EXPRESSIONS));
        // The fix keeps every other bit.
        assert_eq!((CREATE | 0x400 | 0x8) & !EXPRESSION_PERMS, 0x408);
        let clean = [role(1, 0, false, 0x400), role(11, 6, false, CREATE)];
        let rows = checks(&SAFE, &clean, &[], 5, &setup);
        assert_eq!(
            sev(&rows, "expression_perms").unwrap().severity,
            Severity::Ok
        );
    }

    #[test]
    fn staff_mention_note_is_shown_but_not_scored() {
        let mut staff = role(7, 3, false, 1 << 17);
        staff.name = "Moderators".into();
        let roles = [staff, role(8, 2, false, 1 << 17)];
        let note = staff_mention_note(&roles, &[7]).unwrap();
        assert!(
            note.detail.starts_with("Moderators may ping"),
            "{}",
            note.detail
        );
        assert!(
            staff_mention_note(&roles, &[9]).is_none(),
            "no staff role holds it"
        );
        let bad = Check {
            id: "x".into(),
            severity: Severity::Bad,
            title: "",
            detail: String::new(),
            fix: None,
            steps: vec![],
        };
        assert_eq!(score(&[bad, note]), 0, "the note does not count as a pass");
    }

    #[test]
    fn open_channels_get_only_the_missing_standard_bits() {
        let o = |target, allow, deny| OverwriteImg {
            target,
            is_role: true,
            allow,
            deny,
        };
        // Guild 1, main role 2, quarantine role 5, bot member 9, a staff role 7.
        let channels = [
            chan(10, CATEGORY, None, vec![]),
            chan(11, 0, Some(10), vec![o(5, 0, VIEW)]),
            // Read-only for members: Send stays denied, History is added.
            chan(12, 0, None, vec![o(1, 0, VIEW), o(2, VIEW, 1 << 11)]),
            // Own access, public on purpose, kept out, skipped: all left alone.
            chan(13, 0, None, vec![o(7, VIEW, 0)]),
            chan(14, 0, None, vec![o(1, VIEW, 0)]),
            // Hidden from every member, nobody let in: opened (owner decision 2026-11-18).
            chan(15, 0, None, vec![o(1, 0, VIEW)]),
            chan(16, 0, None, vec![o(2, 0, VIEW)]),
            chan(17, CATEGORY, None, vec![]),
            chan(18, 0, Some(17), vec![]),
            // Already standard.
            chan(19, 0, None, vec![o(1, 0, VIEW), o(2, MEMBER_ACCESS, 0)]),
        ];
        let std = |main: &'static [u64], bots: &'static [u64]| Standard {
            guild: 1,
            main_roles: main,
            bot_roles: bots,
            known: &[5, 9],
            skip: &[17],
        };
        let plan = standard_access(&channels, &std(&[2], &[]));
        let got: Vec<(u64, u64, u64, u64)> = plan
            .iter()
            .map(|(c, o)| (*c, o.target, o.allow, o.deny))
            .collect();
        assert_eq!(
            got,
            [
                (10, 2, MEMBER_ACCESS, 0),
                (10, 1, 0, VIEW),
                (11, 2, MEMBER_ACCESS, 0),
                (11, 1, 0, VIEW),
                (12, 2, VIEW | (1 << 16), 1 << 11),
                (15, 2, MEMBER_ACCESS, 0),
            ]
        );
        assert!(standard_access(&channels, &std(&[], &[])).is_empty());
        let row = standard_access_check(&channels, &std(&[2], &[]), &[]).unwrap();
        assert_eq!(row.severity, Severity::Warn);
        assert!(
            row.detail.starts_with("4 open channel(s)"),
            "{}",
            row.detail
        );
        assert!(row.detail.contains("category c10, #c11, #c12, #c15"));
        assert_eq!(row.fix.unwrap().setting, Some(STANDARD_ACCESS));
        // A channel Channel Setup gave a specific role (77) is left alone (owner decision 2026-11-17),
        // and Brim's own channels passed in `more` join the row.
        let custom = [chan(40, 0, None, vec![o(77, VIEW, 0)])];
        assert!(standard_access(&custom, &std(&[2], &[])).is_empty());
        assert!(standard_access_check(&custom, &std(&[2], &[]), &[]).is_none());
        let row = standard_access_check(&custom, &std(&[2], &[]), &[40]).unwrap();
        assert!(row.detail.contains("#c40"), "{}", row.detail);
        // @everyone denied Send (read-only): Members get no Send. A bot role (8) keeps its
        // access, also where it already had an overwrite of its own.
        let read_only = [
            chan(30, 0, None, vec![o(1, 0, 1 << 11)]),
            chan(31, 0, None, vec![o(8, 1 << 11, 0)]),
        ];
        let got: Vec<(u64, u64, u64, u64)> = standard_access(&read_only, &std(&[2], &[8]))
            .iter()
            .map(|(c, o)| (*c, o.target, o.allow, o.deny))
            .collect();
        assert_eq!(
            got,
            [
                (30, 2, VIEW | (1 << 16), 0),
                (30, 8, VIEW | (1 << 16), 0),
                (30, 1, 0, VIEW | (1 << 11)),
                (31, 2, MEMBER_ACCESS, 0),
                (31, 8, MEMBER_ACCESS, 0),
                (31, 1, 0, VIEW),
            ]
        );
    }

    /// Owner decision 2026-11-18: #general was left with only @everyone denied (the Member allow lost
    /// to a 429) and read as private, so neither Overview nor Fix saw it. A channel that lets in
    /// only bot roles opens; Channel Setup's limited channels and Brim's layouts do not.
    #[test]
    fn a_channel_hidden_from_every_member_opens_but_private_layouts_stay() {
        let o = |target, is_role, allow, deny| OverwriteImg {
            target,
            is_role,
            allow,
            deny,
        };
        let me = |allow| o(9, false, allow, 0);
        // Guild 1, main role 2, quarantine role 5, bot member 9, bot role 8, staff role 7.
        let channels = [
            // Half done: @everyone denied, a bot role let in, the quarantine role kept out.
            chan(
                50,
                0,
                None,
                vec![
                    o(1, true, 0, VIEW),
                    o(8, true, MEMBER_ACCESS, 0),
                    o(5, true, 0, VIEW),
                ],
            ),
            // A main role overwrite without View (Send only) lets nobody in.
            chan(
                51,
                0,
                None,
                vec![o(1, true, 0, VIEW), o(2, true, 1 << 11, 0)],
            ),
            // Channel Setup, limited to staff only: main role denied, the bot member let in.
            chan(
                52,
                0,
                None,
                vec![o(1, true, 0, VIEW), o(2, true, 0, VIEW), me(VIEW)],
            ),
            // The Logs category shape (`private_category`): only the bot member let in.
            chan(
                53,
                CATEGORY,
                None,
                vec![o(1, true, 0, VIEW | LOG_LOCK), me(19456 | LOG_LOCK)],
            ),
            // Limited to a staff role or one member: own access.
            chan(54, 0, None, vec![o(1, true, 0, VIEW), o(7, true, VIEW, 0)]),
            chan(
                55,
                0,
                None,
                vec![o(1, true, 0, VIEW), o(66, false, VIEW, 0)],
            ),
            // A jail channel: the quarantine role let in.
            chan(56, 0, None, vec![o(1, true, 0, VIEW), o(5, true, VIEW, 0)]),
        ];
        let s = Standard {
            guild: 1,
            main_roles: &[2],
            bot_roles: &[8],
            known: &[5, 9],
            skip: &[],
        };
        let got: Vec<(u64, u64, u64)> = standard_access(&channels, &s)
            .iter()
            .map(|(c, o)| (*c, o.target, o.allow))
            .collect();
        assert_eq!(
            got,
            [
                (50, 2, MEMBER_ACCESS),
                (51, 2, MEMBER_ACCESS),
                (51, 8, MEMBER_ACCESS)
            ]
        );
        let row = standard_access_check(&channels, &s, &[]).unwrap();
        assert!(row.detail.contains("#c50, #c51"), "{}", row.detail);
        assert!(!row.detail.contains("c52"), "{}", row.detail);
    }

    #[test]
    fn a_deleted_catch_channel_gets_a_row_only_while_catch_is_on() {
        let chans = [chan(70, 0, None, vec![])];
        assert!(catch_gone_check(&chans, Some(70), true).is_none());
        assert!(catch_gone_check(&chans, None, true).is_none());
        assert!(catch_gone_check(&chans, Some(71), false).is_none());
        let row = catch_gone_check(&chans, Some(71), true).unwrap();
        assert_eq!(row.fix.unwrap().setting, Some(CATCH_CHANNEL));
    }

    #[test]
    fn a_catch_channel_that_blocks_files_gets_a_row() {
        let ow = |deny| OverwriteImg {
            target: 1,
            is_role: true,
            allow: 0,
            deny,
        };
        let chans = [
            chan(70, 0, None, vec![ow(ATTACH | 64)]),
            chan(71, 0, None, vec![ow(64)]),
        ];
        let row = catch_files_check(&chans, Some(70), true).unwrap();
        assert_eq!(row.fix.unwrap().setting, Some(CATCH_FILES));
        assert!(catch_files_check(&chans, Some(70), false).is_none());
        assert!(catch_files_check(&chans, Some(71), true).is_none());
        assert!(catch_files_check(&chans, Some(72), true).is_none());
    }

    #[test]
    fn a_ticket_log_follows_logs_only_when_every_ticket_role_sees_it() {
        let o = |target, allow| OverwriteImg {
            target,
            is_role: true,
            allow,
            deny: 0,
        };
        let logs = chan(
            10,
            CATEGORY,
            None,
            vec![o(1, 0), o(30, VIEW), o(31, VIEW | 1 << 16)],
        );
        assert!(ticket_log_follows(&logs, &[30, 31]));
        assert!(ticket_log_follows(&logs, &[]));
        // A custom role with Manage Tickets that cannot see Logs keeps its own access.
        assert!(!ticket_log_follows(&logs, &[30, 40]));
        assert!(!ticket_log_follows(&logs, &[1]));
    }

    #[test]
    fn staff_roles_not_shown_separately_get_a_fix() {
        let hoisted = RoleImg {
            hoist: true,
            ..role(3, 2, false, 0)
        };
        // 2 is staff and not shown separately; 3 is shown; 4 is not staff; 9 is above the bot.
        let roles = [
            role(2, 1, false, 0),
            hoisted,
            role(4, 1, false, 0),
            role(9, 6, false, 0),
        ];
        let row = staff_hoist_check(&roles, &[2, 3, 9], 5).unwrap();
        assert_eq!(row.detail, "r2 is not shown as their own group in the member list, so members cannot see who is staff.");
        assert_eq!(row.fix, act("Show separately", STAFF_HOIST));
        assert!(staff_hoist_check(&roles, &[3, 9], 5).is_none());
    }

    #[test]
    fn a_ticket_log_outside_logs_gets_a_fix() {
        let chan = |id, kind, parent, name: &str| ChannelImg {
            id,
            name: name.into(),
            kind,
            parent,
            position: 0,
            topic: None,
            nsfw: false,
            overwrites: vec![],
        };
        let chans = [
            chan(1, CATEGORY, None, "Tickets"),
            chan(2, CATEGORY, None, LOGS_CATEGORY),
            chan(3, 0, Some(1), "ticket-logs"),
            chan(4, 0, Some(2), "ticket-logs-2"),
        ];
        let row = ticket_log_check(&chans, Some(3)).unwrap();
        assert_eq!(
            row.detail,
            "#ticket-logs is not in the Logs category, where staff find the other log channels."
        );
        assert_eq!(row.fix, act("Move it", TICKET_LOG_PLACE));
        assert!(ticket_log_check(&chans, Some(4)).is_none());
        assert!(ticket_log_check(&chans, None).is_none());
    }

    #[test]
    fn bot_without_a_hoisted_coloured_role_gets_a_fix() {
        let mut shown = role(3, 2, false, 0);
        shown.hoist = true;
        let mut coloured = role(4, 1, false, 0);
        coloured.color = 0x9B7BFF;
        let roles = [role(2, 5, true, 8), shown, coloured];
        let row = bot_hoist_check(&roles, &[2]).unwrap();
        assert_eq!(row.detail, "Brim is not shown as its own group in the member list and has no name colour, so members cannot easily see it is the bot.");
        assert_eq!(row.fix, act("Show separately", BOT_HOIST));
        assert!(bot_hoist_check(&roles, &[2, 3])
            .unwrap()
            .detail
            .contains("has no name colour"));
        assert!(bot_hoist_check(&roles, &[2, 3, 4]).is_none());
    }

    #[test]
    fn score_is_the_share_of_passing_checks() {
        let setup = setup(None, true);
        let rows = checks(&SAFE, &[role(99, 1, false, 0)], &[], 5, &setup);
        // Only the missing quarantine role and the bot role (none here) fail. By id, so a new
        // check row does not break this test.
        let failing: Vec<String> = rows
            .iter()
            .filter(|r| r.severity != Severity::Ok && !r.id.starts_with(NOTE))
            .map(|r| r.id.to_string())
            .collect();
        assert_eq!(failing, ["quarantine_unset", "bot_admin_only"]);
        assert_eq!(
            sev(&rows, "quarantine_unset").unwrap().severity,
            Severity::Bad
        );
        let counted = rows.iter().filter(|r| !r.id.starts_with(NOTE)).count();
        assert_eq!(score(&rows), (counted - failing.len()) * 100 / counted);
        assert_eq!(
            sev(&rows, "quarantine_unset").unwrap().fix,
            act("Create role", QUARANTINE_ROLE)
        );
        assert_eq!(score(&[]), 0);
    }

    #[test]
    fn quarantine_must_sit_right_below_the_bot() {
        let row = |roles: &[RoleImg]| {
            let mut s = setup(Some(9), true);
            s.bot_roles = vec![50];
            let rows = checks(&SAFE, roles, &[], 5, &s);
            (
                sev(&rows, "quarantine_not_right_under").map(|c| (c.severity, c.fix.is_some())),
                sev(&rows, "quarantine_above_bot").is_some(),
            )
        };
        let bot = role(50, 5, true, 0x8);
        // Right below: no row.
        assert_eq!(
            row(&[bot.clone(), role(9, 4, false, 0), role(2, 3, false, 0)]),
            (None, false)
        );
        // One role in between: a warning with a move fix, not the "above" row.
        assert_eq!(
            row(&[bot.clone(), role(2, 4, false, 0), role(9, 3, false, 0)]),
            (Some((Severity::Warn, true)), false)
        );
        // Tied positions follow Discord's order: the lower id ranks higher.
        assert_eq!(
            row(&[bot.clone(), role(9, 4, false, 0), role(20, 4, false, 0)]),
            (None, false)
        );
        assert_eq!(
            row(&[bot.clone(), role(9, 4, false, 0), role(3, 4, false, 0)]),
            (Some((Severity::Warn, true)), false)
        );
        let roles = [
            bot.clone(),
            role(2, 4, false, 0),
            role(3, 3, false, 0),
            role(9, 2, false, 0),
            role(4, 1, false, 0),
        ];
        let mut s = setup(Some(9), true);
        s.bot_roles = vec![50];
        assert_eq!(
            move_under_bot(&roles, 9, 5, &s),
            vec![(9, 4), (2, 3), (3, 2)]
        );
        // Only the quarantine role can be moved, and only when it is not already in place.
        assert!(move_under_bot(&roles, 3, 5, &s).is_empty());
        s.quarantine_role = Some(2);
        assert!(move_under_bot(&roles, 2, 5, &s).is_empty());
        // A role tied with the bot: the quarantine role goes just below the bot's position.
        let tied = [bot.clone(), role(60, 5, false, 0), role(9, 2, false, 0)];
        s.quarantine_role = Some(9);
        assert_eq!(move_under_bot(&tied, 9, 5, &s), vec![(9, 4), (60, 2)]);
        // Above the bot: only the "above" row.
        assert_eq!(row(&[bot, role(9, 6, false, 0)]), (None, true));
        let f = Fix {
            label: "Move role",
            role_id: Some("9".into()),
            channel_id: None,
            setting: None,
            move_under_bot: true,
        };
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            serde_json::json!({"label": "Move role", "role_id": "9", "move_under_bot": true})
        );
    }

    #[test]
    fn bot_role_must_be_among_the_top_eight() {
        let at = |above: u64| {
            let roles: Vec<RoleImg> = (0..above)
                .map(|i| role(10 + i, 2 + i as i64, true, 0))
                .collect();
            sev(
                &checks(&SAFE, &roles, &[], 1, &setup(None, true)),
                "bot_top_eight",
            )
            .unwrap()
            .severity
        };
        assert_eq!(at(7), Severity::Ok);
        assert_eq!(at(8), Severity::Warn);
        let detail = |above: u64| {
            let roles: Vec<RoleImg> = (0..above)
                .map(|i| role(10 + i, 2 + i as i64, true, 0))
                .collect();
            sev(
                &checks(&SAFE, &roles, &[], 1, &setup(None, true)),
                "bot_top_eight",
            )
            .unwrap()
            .detail
            .clone()
        };
        // A passing row reads as fine, not as a problem.
        assert!(detail(3).starts_with("Only 3 "), "{}", detail(3));
    }

    #[test]
    fn setup_rows_follow_permits_whitelist_and_bot_role() {
        let roles = [role(1, 0, false, 0), role(7, 5, true, ADMINISTRATOR)];
        let good = Setup {
            bot_roles: vec![7],
            ..setup(None, true)
        };
        let rows = checks(&SAFE, &roles, &[], 5, &good);
        for id in ["bot_admin_only", "whitelist", "everyone_perms"] {
            assert_eq!(sev(&rows, id).unwrap().severity, Severity::Ok, "{id}");
        }
        let mut roles = vec![
            role(1, 0, false, 0x400),
            role(7, 5, true, ADMINISTRATOR | 0x400),
        ];
        let bad = Setup {
            unwhitelisted_staff: vec!["Admins".into(), "Moderators".into()],
            mod_log: false,
            main_roles: vec![],
            modules: vec![("Anti-nuke", false), ("Join gate", false)],
            ..good
        };
        let rows = checks(&SAFE, &roles, &[], 5, &bad);
        for id in [
            "bot_admin_only",
            "whitelist",
            "everyone_perms",
            "mod_log",
            "main_roles",
            "module_join_gate",
        ] {
            assert_eq!(sev(&rows, id).unwrap().severity, Severity::Warn, "{id}");
        }
        // Serious ones are bad: no anti-nuke at all.
        assert_eq!(
            sev(&rows, "module_anti-nuke").unwrap().severity,
            Severity::Bad
        );
        assert!(sev(&rows, "whitelist")
            .unwrap()
            .detail
            .starts_with("The Admins and Moderators permits have no Automod Immunity"));
        // No staff permit row: an empty Admins or Moderators permit is intended.
        assert!(sev(&rows, "staff_permits").is_none());
        // Each warning gets the fix Auto Setup would do, or steps to do it by hand.
        let setting = |id| {
            sev(&rows, id)
                .unwrap()
                .fix
                .as_ref()
                .map(|f| (f.label, f.setting))
        };
        // No main roles: nothing to move onto, so steps instead of a fix.
        assert_eq!(setting("everyone_perms"), None);
        assert!(!sev(&rows, "everyone_perms").unwrap().steps.is_empty());
        let with_main = Setup {
            main_roles: vec![99],
            ..setup(None, true)
        };
        // Role 99 was deleted in Discord: no move, and Create Member role is offered.
        let gone = checks(&SAFE, &roles, &[], 5, &with_main);
        assert_eq!(sev(&gone, "everyone_perms").unwrap().fix, None);
        let row = sev(&gone, "main_roles").unwrap();
        assert_eq!(row.severity, Severity::Warn);
        assert!(row.detail.starts_with("Your saved main roles were deleted"));
        assert_eq!(row.fix, act("Create Member role", MEMBER_ROLE));
        roles.push(role(99, 1, false, 0));
        let moved = checks(&SAFE, &roles, &[], 5, &with_main);
        assert_eq!(
            sev(&moved, "everyone_perms").unwrap().fix,
            act("Move to main roles", EVERYONE_TO_MAIN)
        );
        assert!(sev(&moved, "main_roles").unwrap().fix.is_none());
        // A deleted verification role beside a live main role: verification hands out the main role.
        let dead_verify = Setup {
            verification_roles: vec![98],
            ..with_main.clone()
        };
        let dv = checks(&SAFE, &roles, &[], 5, &dead_verify);
        let row = sev(&dv, "main_roles").unwrap();
        assert!(row
            .detail
            .starts_with("Your verification roles were deleted"));
        assert_eq!(row.fix, act("Use main roles", MEMBER_ROLE));
        assert_eq!(setting("mod_log"), Some(("Create channel", Some(MOD_LOG))));
        assert_eq!(
            setting("main_roles"),
            Some(("Create Member role", Some(MEMBER_ROLE)))
        );
        assert_eq!(
            setting("module_anti-nuke"),
            Some(("Turn on", Some("enable_antinuke")))
        );
        for id in ["bot_admin_only", "whitelist"] {
            let r = sev(&rows, id).unwrap();
            assert!(r.fix.is_none() && !r.steps.is_empty(), "{id}");
        }
        // Rows with a fix, and passing rows, have no steps.
        assert!(sev(&rows, "mod_log").unwrap().steps.is_empty());
        let rows = checks(&SAFE, &roles, &[], 5, &setup(None, true));
        assert!(rows
            .iter()
            .filter(|r| r.severity == Severity::Ok)
            .all(|r| r.steps.is_empty() && r.fix.is_none()));
        let v = serde_json::to_value(sev(&rows, "whitelist").unwrap()).unwrap();
        assert!(v.get("steps").is_none(), "left out when empty");
    }

    #[test]
    fn rows_serialize_for_the_dashboard() {
        let setup = setup(None, true);
        let v =
            serde_json::to_value(checks(&SAFE, &[role(1, 0, false, 0x8)], &[], 0, &setup)).unwrap();
        let everyone = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "everyone_perms")
            .unwrap();
        assert_eq!(everyone["severity"], "bad");
        assert_eq!(
            everyone["fix"],
            serde_json::json!({"label": "Strip permissions", "role_id": "1"})
        );
    }

    fn chan(id: u64, kind: u8, parent: Option<u64>, overwrites: Vec<OverwriteImg>) -> ChannelImg {
        ChannelImg {
            id,
            name: format!("c{id}"),
            kind,
            parent,
            position: 0,
            topic: None,
            nsfw: false,
            overwrites,
        }
    }

    fn ow(target: u64, is_role: bool, allow: u64) -> OverwriteImg {
        OverwriteImg {
            target,
            is_role,
            allow,
            deny: 0x800,
        }
    }

    #[test]
    fn channels_without_the_quarantine_deny_are_one_row() {
        let roles = [role(1, 0, false, 0), role(5, 1, false, 0)];
        let deny = OverwriteImg {
            target: 5,
            is_role: true,
            allow: 0,
            deny: VIEW | 0x800,
        };
        let channels = [
            chan(10, CATEGORY, None, vec![deny.clone()]),
            chan(11, 0, Some(10), vec![deny.clone()]), // synced, covered
            chan(12, 0, Some(10), vec![]),             // own overwrites, open
            chan(13, 0, None, vec![]),                 // open
        ];
        let setup = Setup {
            quarantine_role: Some(5),
            ..setup(None, true)
        };
        let rows = checks(&SAFE, &roles, &channels, 4, &setup);
        let r = sev(&rows, "quarantine_channels").unwrap();
        assert_eq!(r.severity, Severity::Bad);
        assert!(r.detail.contains("2 channel(s): c12, c13"), "{}", r.detail);
        assert_eq!(r.fix.as_ref().unwrap().setting, Some(QUARANTINE_CHANNELS));
        // The verification channel needs the shape the join quarantine reads, not the full deny.
        let verifying = Setup {
            verification_channel: Some(13),
            verification_roles: vec![],
            ..setup
        };
        let shaped = OverwriteImg {
            target: 5,
            is_role: true,
            allow: crate::VERIFY_ALLOW,
            deny: crate::VERIFY_DENY,
        };
        let with = |o: &OverwriteImg| {
            [
                chan(12, 0, None, vec![]),
                chan(13, 0, None, vec![o.clone()]),
            ]
        };
        let open = |o| {
            let rows = checks(&SAFE, &roles, &with(o), 4, &verifying);
            sev(&rows, "quarantine_channels").unwrap().detail.clone()
        };
        assert!(open(&shaped).contains("1 channel(s): c12"));
        assert!(open(&deny).contains("2 channel(s): c12, c13"));
        let setup = verifying;
        let all = [
            chan(10, CATEGORY, None, vec![deny.clone()]),
            chan(11, 0, Some(10), vec![deny]),
        ];
        assert!(sev(
            &checks(&SAFE, &roles, &all, 4, &setup),
            "quarantine_channels"
        )
        .is_none());
    }

    #[test]
    fn overwrites_on_private_categories_and_channels_are_flagged() {
        let roles = [
            role(1, 0, false, 0),
            role(2, 1, false, 0),
            role(3, 2, true, 0),
        ];
        // Category 10 opens to role 2 with Manage Channels; 11 is synced, 12 has its own overwrites.
        let private = vec![ow(1, true, 0), ow(2, true, 0x400 | 0x10)];
        let channels = [
            chan(10, CATEGORY, None, private.clone()),
            chan(11, 0, Some(10), private.clone()),
            chan(
                12,
                0,
                Some(10),
                vec![ow(1, true, 1 << 17), ow(7, false, 0x2000_0000)],
            ),
            chan(
                13,
                0,
                None,
                vec![
                    ow(3, true, 0x10),
                    ow(8, false, 0x10),
                    ow(2, true, 0x400 | (1 << 17)),
                ],
            ),
        ];
        let setup = Setup {
            main_roles: vec![],
            trusted: vec![8],
            ..setup(None, true)
        };
        let hits: Vec<(u64, u64)> = overwrite_targets(&channels, &roles, 1, &setup)
            .iter()
            .map(|(c, o)| (c.id, o.target))
            .collect();
        // Synced 11, the managed role 3 and trusted member 8 are not hits; mass mention for role 2 is.
        assert_eq!(hits, [(10, 2), (12, 1), (12, 7), (13, 2)]);
        // On a staff permit role, mass mention alone is allowed.
        let staff = Setup {
            staff_roles: vec![2],
            ..setup.clone()
        };
        let hits: Vec<(u64, u64)> = overwrite_targets(&channels, &roles, 1, &staff)
            .iter()
            .map(|(c, o)| (c.id, o.target))
            .collect();
        assert_eq!(hits, [(10, 2), (12, 1), (12, 7)]);
        assert_eq!(synced_children(&channels, &channels[0]).len(), 1);
        let rows = checks(&SAFE, &roles, &channels, 4, &setup);
        let r = sev(&rows, "overwrite_10_2").unwrap();
        assert_eq!(r.severity, Severity::Bad);
        assert!(
            r.detail.contains("Category c10 and its 1 synced")
                && r.detail.contains("Manage Channels")
        );
        assert_eq!(
            serde_json::to_value(&r.fix).unwrap(),
            serde_json::json!({"label": "Strip permissions", "role_id": "2", "channel_id": "10"})
        );
        assert!(sev(&rows, "roles_bulk").unwrap().detail.starts_with("4 "));
        let fixed = stripped_overwrite(&channels[0].overwrites[1], 1, &setup);
        assert_eq!((fixed.allow, fixed.deny), (0x400, 0x800));
        // Main roles are held to the @everyone rules.
        let main = Setup {
            main_roles: vec![2],
            ..setup
        };
        assert!(overwrite_targets(&channels, &roles, 1, &main)
            .iter()
            .any(|(c, o)| (c.id, o.target) == (13, 2)));
    }

    #[test]
    fn public_log_channels_are_flagged_and_made_private() {
        const EVERYONE: u64 = 1;
        let roles = [role(EVERYONE, 0, false, VIEW), role(99, 1, false, 0)];
        let hidden = |deny| OverwriteImg {
            target: EVERYONE,
            is_role: true,
            allow: 0,
            deny,
        };
        let channels = [
            chan(20, 0, None, vec![]),
            chan(21, 0, None, vec![hidden(VIEW)]),
            // Hidden from @everyone, but the main role 99 is let back in.
            chan(22, 0, None, vec![hidden(VIEW), ow(99, true, VIEW)]),
            // Not a log channel.
            chan(23, 0, None, vec![]),
        ];
        let setup = Setup {
            log_channels: vec![20, 21, 22],
            ..setup(None, true)
        };
        let rows = checks(&SAFE, &roles, &channels, 4, &setup);
        let flagged: Vec<_> = rows
            .iter()
            .filter(|r| r.id.starts_with("log_channel_public_"))
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(flagged, ["log_channel_public_20", "log_channel_public_22"]);
        let r = sev(&rows, "log_channel_public_20").unwrap();
        assert_eq!(r.severity, Severity::Bad);
        assert_eq!(
            serde_json::to_value(&r.fix).unwrap(),
            serde_json::json!({"label": "Make private", "channel_id": "20", "setting": PRIVATE})
        );
        // Administrator sees through overwrites: flagged, but no fix button.
        let admin = [role(EVERYONE, 0, false, ADMINISTRATOR)];
        let rows = checks(&SAFE, &admin, &channels, 4, &setup);
        let r = sev(&rows, "log_channel_public_21").unwrap();
        assert!(r.fix.is_none() && r.detail.contains("Administrator"));
        assert!(r.steps[1].contains("Administrator"), "{:?}", r.steps);
        // An @everyone without View and no overwrites is private already.
        let closed = [role(EVERYONE, 0, false, 0)];
        assert!(!is_public(&channels[0], &closed, EVERYONE, &[]));
        // The fix: @everyone and main roles denied View, the bot let in; nothing once private.
        let fix = private_overwrites(&channels[2], EVERYONE, 7, &[99]);
        assert_eq!(
            fix.iter()
                .map(|o| (o.target, o.allow, o.deny))
                .collect::<Vec<_>>(),
            [(EVERYONE, 0, VIEW), (99, 0, 0x800 | VIEW), (7, BOT_LOG, 0)]
        );
        let mut fixed = channels[2].clone();
        fixed.overwrites = fix;
        assert!(!is_public(&fixed, &roles, EVERYONE, &[99]));
        assert!(private_overwrites(&fixed, EVERYONE, 7, &[99]).is_empty());
        assert!(private_overwrites(&channels[1], EVERYONE, 7, &[99]).is_empty());
    }

    #[test]
    fn log_channels_are_locked_against_deletes() {
        const MM: u64 = 1 << 13;
        const EVERYONE: u64 = 1;
        let hidden = OverwriteImg {
            target: EVERYONE,
            is_role: true,
            allow: 0,
            deny: VIEW,
        };
        let c = chan(20, 0, None, vec![hidden, ow(99, true, MM | VIEW)]);
        let lock = lock_overwrites(&c, EVERYONE, 7);
        assert_eq!(
            lock.iter()
                .map(|o| (o.target, o.allow, o.deny))
                .collect::<Vec<_>>(),
            [
                (7, BOT_LOG | LOG_LOCK, 0),
                (99, VIEW, 0x800),
                (EVERYONE, 0, VIEW | LOG_LOCK)
            ]
        );
        let mut locked = c.clone();
        locked.overwrites = lock;
        assert!(lock_overwrites(&locked, EVERYONE, 7).is_empty());
        // The bot's own allow gone while @everyone is still denied: that alone is unlocked.
        let mut lost = locked.clone();
        lost.overwrites.retain(|o| o.target != 7);
        assert_eq!(lock_overwrites(&lost, EVERYONE, 7).len(), 1);
        let setup = Setup {
            log_channels: vec![20],
            ..setup(None, true)
        };
        let rows = checks(&SAFE, &[role(EVERYONE, 0, false, 0)], &[c], 4, &setup);
        assert!(sev(&rows, "log_channel_unlocked_20").is_some());
        let rows = checks(&SAFE, &[role(EVERYONE, 0, false, 0)], &[locked], 4, &setup);
        assert!(sev(&rows, "log_channel_unlocked_20").is_none());
    }
}

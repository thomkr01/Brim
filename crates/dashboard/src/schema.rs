//! Typed form fields per config section, served to the dashboard UI.
use brim_antinuke::Action as NukeAction;
use brim_automod::{
    heat::HeatCfg,
    join::{Action as GateAction, Charset, Filter as GateFilter, GateCfg},
    strike::StrikeCfg,
};
use brim_core::{limits, sections::FILTERS_OFF_BY_DEFAULT};
use brim_logging::LogEvent;
use serde_json::{json, Value};

const LOG_KINDS: &[&str] = &[
    "message",
    "member",
    "role",
    "channel",
    "server",
    "invite",
    "webhook",
    "emoji",
    "sticker",
    "scheduled_event",
    "thread",
    "report",
    "auto_mod",
    "auto_mod_rule",
    "anti_nuke",
    "verification",
    "join_gate",
    "join_raid",
    "panic",
    "mod_command",
    "appeal",
    "voice",
    "poll",
    "user",
];

fn f(path: &str, kind: &str) -> Value {
    json!({"path": path, "label": path.replace(['.', '_'], " "), "kind": kind})
}

/// `f` placed in a named card instead of the one its path prefix would give.
fn fg(group: &str, path: &str, kind: &str) -> Value {
    let mut v = f(path, kind);
    v["group"] = json!(group);
    v
}

/// Shows `default` for a field the stored config leaves out (matches the Rust `Default`).
fn dv(mut v: Value, default: impl Into<Value>) -> Value {
    v["default"] = default.into();
    v
}

/// Puts a field on a dashboard page of its section (no page: the General page).
fn pg(mut v: Value, page: &str) -> Value {
    v["page"] = json!(page);
    v
}

/// Overrides the label the field shows (default: the last path segment).
fn ti(mut v: Value, title: &str) -> Value {
    v["title"] = json!(title);
    v
}

const RETENTION_HELP: &str = "Older records are deleted from Brim's database within an hour; logs already posted in Discord stay. 0 keeps them until staff delete them; otherwise at least 30 days. The newest case and ticket and the current config always stay. Changing this needs the owner or an Extra Owner and 2FA.";

const SPAM_FLAG_HELP: &str = "Discord sets this flag on accounts it thinks are spammers. Discord does not document it, can change it without notice, and it can be wrong.";

/// Help text, shown behind the field's info icon.
fn hp(mut v: Value, help: &str) -> Value {
    v["help"] = json!(help);
    v
}

/// A warning shown under the field, in full (not behind the info icon).
fn wn(mut v: Value, warn: &str) -> Value {
    v["warn"] = json!(warn);
    v
}

/// A field stored in another section, shown on this section's page (saved with it).
fn sx(section: &str, mut v: Value) -> Value {
    v["section"] = json!(section);
    v
}

/// Verification modes that use the captcha card (Instant Access only during a join raid, so it rings red).
const IMAGE_MODES: &[&str] = &["Captcha", "Dm"];
/// Verification modes that show the helpful-notes switch (hidden for Website and Instant Access).
const NOTE_MODES: &[&str] = &["Captcha", "Dm", "SimpleCaptcha"];

/// `key` "only" hides the field and "used" rings its card red unless the value at `path` is one of `vals`.
fn unless(mut v: Value, key: &str, path: &str, vals: &[&str]) -> Value {
    v[key] = json!({"path": path, "in": vals});
    v
}

/// Hides the field unless the switch at `path` is on.
fn only_when_on(mut v: Value, path: &str) -> Value {
    v["only"] = json!({"path": path, "in": [true]});
    v
}

/// Log types as one channel picker each (true) or as cards added through a Customize dialog (false).
const LOG_TYPE_PICKERS: bool = false;

/// The card a Discord log event is listed under (also the category on the Customize list).
fn event_group(key: &str) -> &'static str {
    match key.split('_').next().unwrap_or_default() {
        "channel" | "channels" => "Channels",
        "message" => "Messages",
        "member" | "bot" | "user" => "Members",
        "role" => "Roles",
        "invite" => "Invites",
        "webhook" => "Webhooks",
        "emoji" => "Emojis",
        "sticker" => "Stickers",
        "thread" => "Threads",
        "auto" => "AutoMod rules",
        "poll" => "Polls",
        "stage" => "Stages",
        "boost" => "Server",
        "app" => "Apps",
        "guild" if key == "guild_update" => "Server",
        "guild" => "Scheduled events",
        _ => "Other",
    }
}

/// The category a Brim log type is listed under on the Customize list.
fn kind_group(k: &str) -> &'static str {
    match k {
        "auto_mod" | "auto_mod_rule" | "anti_nuke" | "verification" | "join_gate" | "join_raid"
        | "panic" => "Protection",
        "report" | "appeal" => "Moderation",
        _ => "Server events",
    }
}

/// "auto_mod" -> "Auto Mod".
fn title_case(k: &str) -> String {
    k.split('_')
        .map(|w| w[..1].to_uppercase() + &w[1..])
        .collect::<Vec<_>>()
        .join(" ")
}

/// A Customize dialog writing `{key: channel}` into the object at `path`: pick one option (or tick
/// several when `multi`), then one channel.
fn route_pick(group: &str, path: &str, opts: Vec<Value>, multi: bool) -> Value {
    let mut v = fg(group, path, "route_pick");
    v["options"] = json!(opts);
    v["multi"] = json!(multi);
    v
}

/// A choice field with its options; "" is the unset option.
fn choice(group: &str, path: &str, opts: &[&str]) -> Value {
    let mut v = fg(group, path, "choice");
    v["options"] = json!(opts);
    v
}

/// Ticks several of `opts`; stored as an array of the ticked ones.
fn multi(group: &str, path: &str, opts: &[&str]) -> Value {
    let mut v = fg(group, path, "multi");
    v["options"] = json!(opts);
    v
}

/// A heat weight with its default and the "N messages before a timeout" helper.
fn heat_field(group: &str, w: &str, page: &str, noun: &str) -> Value {
    let heat = serde_json::to_value(HeatCfg::default()).unwrap_or_default();
    let mut v = pg(
        dv(fg(group, &format!("heat.{w}"), "float"), heat[w].clone()),
        page,
    );
    v["helper"] = json!({
        "noun": noun,
        "max": heat["max"],
        "decay": heat["decay_per_sec"],
        "strike_ms": StrikeCfg::default().strike_timeout_ms,
    });
    // 0 up to the max heat (`limits::float_range`); the form reads the max from the section.
    v["min"] = json!(0);
    v["max_path"] = json!("heat.max");
    v["max"] = heat["max"].clone();
    v
}

/// (filter key, dashboard page, card title).
const FILTERS: &[(&str, &str, &str)] = &[
    ("regular", "spam_text", "Regular message"),
    ("similar", "spam_text", "Similar message"),
    ("emojis", "spam_text", "Emojis"),
    ("newlines", "spam_text", "New lines"),
    ("chars_upper", "spam_text", "Characters (uppercase)"),
    ("chars_lower", "spam_text", "Characters (lowercase)"),
    ("zalgo", "spam_text", "Zalgo"),
    ("suspicion", "spam_text", "Suspicion"),
    ("inactive_channel", "spam_text", "Inactive channel"),
    ("invites", "invites", "Invite links"),
    ("bad_sites", "malicious", "Bad websites"),
    ("scam_links", "malicious", "Scam links and login QR codes"),
    ("nsfw_links", "nsfw", "NSFW links"),
    ("nsfw_images", "nsfw", "NSFW images"),
    ("words.filter", "words", "Word blacklist"),
    ("links", "links", "Link blacklist"),
    ("mentions", "spam_mentions", "General"),
    ("attachments", "spam_attach", "General"),
];
/// Heat weights shown on the card of the filter they feed, with the helper's noun.
const HEAT_FOR: &[(&str, &str, &str)] = &[
    ("regular", "base", "messages"),
    ("similar", "similarity", "similar messages"),
    ("emojis", "per_emoji", "emojis"),
    ("newlines", "per_newline", "new lines"),
    ("chars_upper", "per_upper", "uppercase characters"),
    ("chars_lower", "per_lower", "lowercase characters"),
    ("chars_lower", "per_char", "characters past the free ones"),
    ("zalgo", "zalgo", "zalgo messages"),
    ("suspicion", "suspicious_account", "messages"),
    ("inactive_channel", "inactive_channel", "messages"),
    ("invites", "invite", "invites"),
    ("scam_links", "malicious", "scam links"),
    ("nsfw_links", "nsfw", "NSFW links"),
    ("nsfw_images", "nsfw", "NSFW images"),
    ("words.filter", "per_blacklisted_word", "blacklisted words"),
    ("links", "blacklisted_link", "blacklisted links"),
];
/// Mention and attachment kinds: (filter key, page, card title, heat weight, helper noun).
/// Their action lives on the tab's General card.
const SUBS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "mention_users",
        "spam_mentions",
        "@user mentions",
        "per_mention",
        "mentions",
    ),
    (
        "mention_roles",
        "spam_mentions",
        "@role mentions",
        "per_role_mention",
        "role mentions",
    ),
    (
        "mention_everyone",
        "spam_mentions",
        "@everyone mentions",
        "per_everyone",
        "@everyone mentions",
    ),
    ("embeds", "spam_attach", "Embeds", "per_embed", "embeds"),
    ("images", "spam_attach", "Images", "per_image", "images"),
    ("files", "spam_attach", "Files", "per_file", "files"),
    ("attach_links", "spam_attach", "Links", "per_link", "links"),
    (
        "stickers",
        "spam_attach",
        "Stickers",
        "per_sticker",
        "stickers",
    ),
];
/// "" leaves the decision to the strike system.
const FILTER_ACTIONS: &[&str] = &["delete", "warn", "timeout", "kick", "ban", ""];
/// Anti-nuke filter cards: (action, card title). Kicks count on the Ban card.
pub(crate) const NUKE_CARDS: &[(&str, &str)] = &[
    ("Ban", "Kicks & Bans"),
    ("RoleCreate", "Role creations"),
    ("RoleDelete", "Role deletions"),
    ("ChannelCreate", "Channel creations"),
    ("ChannelDelete", "Channel deletions"),
    ("ThreadDelete", "Thread & forum post deletions"),
    ("WebhookCreate", "Webhook creations"),
    ("WebhookDelete", "Webhook deletions"),
    ("Timeout", "Member timeouts"),
    ("RoleRemove", "Role removals from members"),
    ("ChannelUpdate", "Channel updates"),
    ("RoleUpdate", "Role renames"),
    ("EmojiDelete", "Emoji deletions"),
    ("StickerDelete", "Sticker deletions"),
    ("EmojiCreate", "Emoji creations"),
    ("StickerCreate", "Sticker creations"),
    ("SoundboardDelete", "Soundboard deletions"),
    ("ScheduledEventCreate", "Event creations"),
    ("IntegrationCreate", "App additions"),
    ("IntegrationDelete", "App removals"),
    ("GuildUpdate", "Server name & icon changes"),
    ("VoiceDisconnect", "Voice disconnects"),
    ("VoiceMove", "Voice moves"),
    ("AutoModRuleDelete", "AutoMod rule deletions"),
    ("AutoModRuleUpdate", "AutoMod rule updates"),
    ("Unban", "Unbans"),
];
const GATE_ACTIONS: &[&str] = &["Log", "Warn", "Timeout", "Kick", "Ban"];
/// Only a name can be fixed by renaming, so only the username filter offers Rename.
const NAME_ACTIONS: &[&str] = &["Log", "Rename", "Warn", "Timeout", "Kick", "Ban"];

/// Typed form fields per section; paths mirror the serde types validated in `api::validate`.
/// Plain-language hover text for settings that are hard to read from their name: (section, path or
/// last path part, text). Shown by the info icon next to the field title.
const TIPS: &[(&str, &str, &str)] = &[
    ("joingate", "require_all", "Normally one matching filter is enough to stop a new member. With this on, a member is only stopped when ALL the filters you turned on match."),
    ("joingate", "exclude_trusted_bots", "Bots you or an Extra Owner add skip the checks. Bots added by anyone else are still checked."),
    ("joingate", "unauthorized_bot.enabled", "Someone without permission adds a bot to the server. Brim removes the bot right away."),
    ("joingate", "min_age_override_ms", "New accounts are often made for raids. Accounts younger than this are stopped at the door."),
    ("automod", "heat.max", "Every message adds \"heat\" to a member. When the heat reaches this number, the member gets a strike."),
    ("automod", "heat.decay_per_sec", "How fast heat cools down again. Higher means members can chat faster before they get a strike."),
    ("automod", "strike.multiplier_pct", "Each new strike in a short time makes the next timeout longer by this much. 200 % means it doubles."),
    ("automod", "strike.cap", "After this many strikes the member gets the longer CAP timeout instead of the normal one."),
    ("automod", "strike.reoffend_window_ms", "Strikes inside this time count together. After it, the count starts over."),
    ("automod", "strike.strike_expiry_ms", "How long a strike is remembered before it is forgotten."),
    ("automod", "heat.webhook_multiplier", "Messages from webhooks get this much more heat, because spam bots often use webhooks."),
    ("panic", "executors", "If this many accounts are quarantined in a short time, Brim treats it as an attack and starts Panic Mode. 0 turns this off."),
    ("panic", "heat_executors", "Same, but counted from members Auto Mod punished."),
    ("antinuke", "per_minute", "How many times one account may do this action (for example ban or delete a channel) before Brim stops it."),
    ("antinuke", "per_hour", "How many times one account may do this action (for example ban or delete a channel) before Brim stops it."),
    ("antinuke", "heat", "How much of the limit one action uses. 100 % means one action counts as reaching the limit."),
    ("joinraid", "voice.count", "How many different members must join a voice channel within the join history time before Brim treats it as a voice raid. One member joining again and again counts once."),
    ("joinraid", "voice.span_ms", "The time window Brim counts voice joins in. Moving between voice channels is not a join."),
    ("joinraid", "count", "How many members must join within the join history time before Brim treats it as a raid."),
    ("joinraid", "span_ms", "The time window Brim counts joins in. If the minimum trigger is reached within it, it is a raid."),
    ("joinraid", "id_flag_min", "Raid accounts are often made at the same moment. Brim looks for many joins whose accounts were created close together."),
    ("lockdown", "main_roles", "The roles normal members have. Lockdown takes the chat permissions away from these roles."),
    ("whitelist", "partner_channels", "Channels where invite links from other servers are allowed."),
    ("raider_list", "enabled", "The Raider List is a list of raider accounts Brim caught across many servers. The bot owner checks every entry before it is used. Your server sees only the category and a short note, never other servers' evidence."),
    ("raider_list", "action", "What Brim does when a listed account joins. Alert posts it in the mod log with Ban, Kick and Quarantine buttons."),
    ("raider_list", "lift_on_removal", "When the bot owner takes someone off the list, for example after an appeal, Brim lifts the ban it made here."),
    ("raider_list", "no_collect", "Normally Brim also learns about raiders from what it catches here. Turn this on to keep this server out of that."),
];

pub fn schema() -> Vec<Value> {
    let mut log = vec![
        ti(pg(f("fallback", "channel"), "brim_essential"), "Brim logs"),
        ti(
            pg(
                fg("Channels", "routes.mod_command", "channel"),
                "brim_essential",
            ),
            "Mod logs",
        ),
        pg(
            fg("Ignored", "ignored_channels", "channels"),
            "brim_customize",
        ),
        pg(fg("Ignored", "ignored_roles", "roles"), "brim_customize"),
        pg(fg("Ignored", "ignored_users", "users"), "brim_customize"),
        ti(
            hp(
                pg(fg("Ignored", "ignore_embeds", "bool"), "brim_customize"),
                "Edited and deleted messages that have embeds are not logged.",
            ),
            "Ignore messages with embeds",
        ),
    ];
    let kinds = LOG_KINDS.iter().filter(|k| **k != "mod_command").map(|k| {
        let help = match *k {
            "report" => "Message reports by your members will go to this channel where you and your moderators can interact and deal with them.",
            "auto_mod" => "All automod actions from Brim will be sent to this channel.",
            "anti_nuke" => "All antinuke actions from Brim will be sent to this channel.",
            "verification" => "All verification results from Brim will be sent to this channel.",
            "join_gate" => "Join Gate logs will go to this channel.",
            "join_raid" => "Join raid triggers and results will be sent to this channel.",
            "panic" => "Panic mode triggers and results will be sent to this channel.",
            "appeal" => "New ban and timeout appeals will be sent to this channel.",
            "voice" => "Voice joins, leaves and moves. Only logged once you pick a channel here.",
            "user" => "Members' own avatar, username and display name changes. To show the old value, Brim stores every member's last seen name and avatar while these logs are on (it reads the member list once a day), and deletes it when the member leaves, or 90 days after these logs are turned off.",
            _ => "Logs of this type will go to this channel instead of the Brim logs channel.",
        };
        (*k, help)
    });
    if LOG_TYPE_PICKERS {
        log.extend(kinds.map(|(k, help)| {
            pg(
                hp(fg("Log types", &format!("routes.{k}"), "channel"), help),
                "brim_customize",
            )
        }));
    } else {
        let opts: Vec<Value> = kinds
            .map(|(k, help)| json!({"key": k, "title": title_case(k), "help": help, "group": kind_group(k)}))
            .collect();
        log.push(pg(
            ti(
                route_pick("Log types", "routes", opts, false),
                "Custom log channels",
            ),
            "brim_customize",
        ));
    }
    let events: Vec<Value> = LogEvent::ALL
        .iter()
        .filter_map(|e| serde_json::to_value(e).ok()?.as_str().map(String::from))
        .map(|k| json!({"key": k, "title": title_case(&k), "group": event_group(&k)}))
        .collect();
    log.push(pg(
        ti(
            route_pick("Customize", "events", events, true),
            "Send several events to one channel",
        ),
        "discord_essential",
    ));
    log.extend(LogEvent::ALL.iter().map(|e| {
        let name = serde_json::to_value(e).unwrap_or(Value::Null);
        let key = name.as_str().unwrap_or_default();
        let group = event_group(key);
        pg(
            fg(group, &format!("events.{key}"), "channel"),
            "discord_essential",
        )
    }));
    let mut custom = fg("Logging types", "custom", "customs");
    custom["options"] = json!(LogEvent::ALL
        .iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect::<Vec<_>>());
    log.push(pg(custom, "discord_customize"));
    let mut nuke = vec![
        ti(fg("Status", "enabled", "bool"), "Use the antinuke module"),
        ti(fg("Status", "prune", "bool"), "Detect and react to member prunes"),
        ti(dv(fg("Status", "quarantine_hold", "bool"), true), "Punish anyone who touches a quarantined member"),
        ti(fg("Status", "vanity_guard", "bool"), "Have Brim guard your server's vanity url"),
        ti(dv(fg("Auto quarantine", "auto_quarantine", "bool"), true), "Use the auto quarantine module"),
        hp(
            ti(fg("Auto quarantine", "quarantine_role", "role"), "Quarantine role"),
            "Leave it empty and Auto Setup makes @Quarantine for you, deleting old Quarantine roles. Pick a role and Auto Setup removes all of its permissions and denies it in every channel.",
        ),
        ti(fg("Auto quarantine", "monitor_public_roles", "bool"), "Monitor public roles"),
        hp(
            ti(fg("Auto quarantine", "quarantine_whitelist", "users"), "Quarantine whitelist"),
            "Members that may change the roles of quarantined members.",
        ),
        hp(
            ti(fg("Ticket categories", "ticket_categories", "categories"), "Ticket categories"),
            "Channels a bot (a ticket bot) creates or deletes in these categories never count. Members always count, and so does a channel moved in from elsewhere in the last hour.",
        ),
    ];
    for (a, title) in NUKE_CARDS {
        let action = NukeAction::ALL.into_iter().find(|x| format!("{x:?}") == *a);
        let (m, h, heat) = action
            .and_then(NukeAction::default_limit)
            .unwrap_or_default();
        let card = *title;
        let at = |k: &str, kind: &str, title: &str| {
            ti(
                pg(fg(card, &format!("limits.{a}.{k}"), kind), "limits"),
                title,
            )
        };
        let opt_in = action.is_some_and(NukeAction::opt_in);
        let mut first = dv(at("enabled", "bool", "Monitor this action"), !opt_in);
        if *a == "Ban" {
            first = hp(first, "We absolutely recommend not giving your staff members native Discord Ban/Kick permissions, force them to use the bot's kick and ban commands that are supervised by the antinuke.");
        }
        if *a == "WebhookCreate" {
            first = hp(
                first,
                "Tracking webhooks created by people is very important.",
            );
        }
        nuke.push(first);
        // A heat-only card has no caps to set.
        if !action.is_some_and(NukeAction::heat_only) {
            nuke.extend([
                dv(at("per_minute", "int", "Minute limit"), m),
                dv(at("per_hour", "int", "Hour limit"), h),
            ]);
        }
        nuke.push(hp(
            dv(at("heat", "int", "Heat per action"), heat),
            "Only counts when Panic Mode and its heat option are on.",
        ));
    }
    let mut auto = vec![
        ti(fg("Status", "enabled", "bool"), "Use the automod module"),
        ti(dv(fg("Status", "anti_spam", "bool"), true), "Use the anti spam"),
        ti(dv(fg("Status", "monitor_webhooks", "bool"), true), "Monitor unwhitelisted webhooks"),
        // Display only: the heat system is the one mode, the stored value is ignored.
        ti(dv(choice("Status", "mode", &["Heat System"]), "Heat System"), "AutoMod mode"),
        ti(dv(fg("Auto timeouts", "strike.enabled", "bool"), true), "Use the auto timeouts"),
        ti(dv(fg("Auto timeouts", "strike.strike_timeout_ms", "int"), 60_000), "Regular strike duration"),
        ti(dv(fg("Auto timeouts", "strike.cap_timeout_ms", "int"), 660_000), "CAP strike duration"),
        ti(dv(fg("Auto timeouts", "strike.cap", "int"), 3), "Strikes before the CAP strike"),
        ti(dv(fg("Auto timeouts", "strike.reoffend_window_ms", "int"), 3_600_000), "Reoffend window"),
        ti(dv(fg("Auto timeouts", "strike.strike_expiry_ms", "int"), 3_600_000), "Strike expiry"),
        ti(dv(fg("Auto timeouts", "strike.multiply", "bool"), false), "Multiply timeouts"),
        ti(dv(fg("Auto timeouts", "strike.multiplier_pct", "int"), 200), "Multiplier percentage"),
        ti(dv(fg("Heat settings", "heat.max", "float"), 100.0), "Max heat"),
        ti(dv(fg("Heat settings", "heat.decay_per_sec", "pct"), HeatCfg::default().decay_per_sec), "Heat degradation (every second)"),
        dv(fg("Heat settings", "heat.webhook_multiplier", "float"), 1.5),
        hp(
            ti(dv(fg("Heat settings", "strike.reset_heat_on_timeout", "bool"), false), "Set current heat to 0 after punishment"),
            "The filter that generated the most heat is picked as the reason, and its action is used.",
        ),
        ti(fg("Auto lockdown", "auto_lockdown.enabled", "bool"), "Use the auto lockdown module"),
        ti(dv(fg("Mentions lockdown", "auto_lockdown.mentions", "bool"), true), "Use the mentions auto lockdown trigger"),
        hp(
            ti(dv(fg("Mentions lockdown", "auto_lockdown.mention_count", "int"), 50), "Trigger count (mentions)"),
            "While Anti Spam and its mentions filter are on, each member counts for about a third of this at most (17 of 50), so from 3 up it takes at least three members: the filter punishes one member's mass mention. Members the filter skips (whitelisted, or the filter off) count in full.",
        ),
        ti(dv(fg("Mentions lockdown", "auto_lockdown.mention_window_ms", "int"), 3_000), "Trigger count history"),
        hp(
            ti(fg("Strikes lockdown", "auto_lockdown.strikes", "int"), "Lock at this strike number"),
            "Empty: off.",
        ),
        sx(
            "joinraid",
            hp(
                ti(choice("JoinRaid lockdown", "lock", &["", "Server", "Blind", "Role", "All"]), "Lock when a join raid starts"),
                "Needs Use the auto lockdown module above to be on.",
            ),
        ),
        hp(
            ti(fg("Auto slowmode", "auto_slowmode.enabled", "bool"), "Use the auto slowmode"),
            "Gives a flooding channel a slowmode and puts the old one back when it is calm. Needs Anti Spam on and Manage Channels in the channel. Threads, spam-whitelisted and locked channels are skipped.",
        ),
        hp(
            ti(dv(fg("Auto slowmode", "auto_slowmode.channel_pct", "int"), 300), "Trigger (% of max heat)"),
            "All messages in the channel add their heat to one pool. Each member adds at most max heat, so 300 needs at least three members, and the lowest value, 200, needs two. Bots, webhooks and members with server-wide Manage Messages, Manage Channels or Administrator add nothing, as Discord lets them skip slowmode.",
        ),
        hp(
            ti(dv(fg("Auto slowmode", "auto_slowmode.slowmode_secs", "int"), 5), "Slowmode (seconds)"),
            "1 to 120 seconds: it is only meant to calm a flood for a moment. A slowmode staff set that is as high or higher is left alone.",
        ),
        ti(dv(fg("Auto slowmode", "auto_slowmode.min_on_secs", "int"), 60), "Minimum time on"),
        hp(
            ti(dv(fg("Auto slowmode", "auto_slowmode.calm_secs", "int"), 120), "Calm time"),
            "Off once the pool stayed under half the trigger this long. Changing the slowmode by hand ends it, unless you set exactly the auto value in Discord.",
        ),
    ];
    for (k, page, title) in FILTERS {
        let mut action = pg(
            ti(
                choice(title, &format!("filters.{k}.action"), FILTER_ACTIONS),
                "Trigger action",
            ),
            page,
        );
        action["none_label"] = json!("Strike system");
        let use_it = if *k == "mentions" {
            "Consider mentions found in messages"
        } else if *k == "attachments" {
            "Consider attachments found in messages"
        } else {
            "Use this filter"
        };
        auto.extend([
            pg(
                dv(
                    ti(fg(title, &format!("filters.{k}.enabled"), "bool"), use_it),
                    !FILTERS_OFF_BY_DEFAULT.contains(k),
                ),
                page,
            ),
            action,
            hp(
                pg(
                    ti(
                        fg(title, &format!("filters.{k}.duration_secs"), "int"),
                        "Action duration",
                    ),
                    page,
                ),
                "Empty uses the default timeout from Miscellaneous.",
            ),
        ]);
        auto.extend(
            HEAT_FOR
                .iter()
                .filter(|(f, ..)| f == k)
                .map(|(_, h, noun)| ti(heat_field(title, h, page, noun), "Heat added")),
        );
    }
    for (k, page, title, w, noun) in SUBS {
        auto.extend([
            pg(
                dv(
                    ti(
                        fg(title, &format!("filters.{k}.enabled"), "bool"),
                        "Use this filter",
                    ),
                    true,
                ),
                page,
            ),
            ti(heat_field(title, w, page, noun), "Heat added"),
        ]);
    }
    auto.extend([
        pg(
            dv(
                ti(
                    fg("Similar message", "heat.similarity_floor", "pct"),
                    "Similarity ratio",
                ),
                0.8,
            ),
            "spam_text",
        ),
        pg(
            dv(
                ti(
                    fg("Characters (lowercase)", "heat.free_chars", "int"),
                    "Free characters",
                ),
                200,
            ),
            "spam_text",
        ),
        pg(
            dv(
                ti(
                    fg("Inactive channel", "filters.inactive_messages", "int"),
                    "Inactive messages",
                ),
                10,
            ),
            "spam_text",
        ),
        pg(
            dv(
                ti(
                    fg("Inactive channel", "filters.inactive_hours", "int"),
                    "Quiet hours",
                ),
                1,
            ),
            "spam_text",
        ),
        pg(
            dv(
                ti(
                    fg("General", "filters.mention_hour_cap", "int"),
                    "Hour cap (pings)",
                ),
                20,
            ),
            "spam_mentions",
        ),
        pg(
            ti(fg("NSFW links", "filters.nsfw_crawl_shorteners", "bool"), "Follow short links before checking"),
            "nsfw",
        ),
        pg(
            hp(
                dv(
                    ti(
                        fg("NSFW images", "filters.nsfw_threshold", "int"),
                        "Score to count as NSFW (%)",
                    ),
                    80,
                ),
                "Brim checks posted images, link previews and GIFs with its own model, on its own server; images are never stored or sent anywhere. Higher means fewer false hits but more missed images. Age-restricted channels are skipped.",
            ),
            "nsfw",
        ),
        pg(
            ti(
                fg("Word blacklist", "filters.words.strict", "list"),
                "Strict words",
            ),
            "words",
        ),
        pg(
            ti(
                fg("Word blacklist", "filters.words.wildcard", "list"),
                "Wildcard words",
            ),
            "words",
        ),
        pg(
            hp(
                ti(
                    fg("Word blacklist", "filters.words.regex", "lines"),
                    "Regex patterns",
                ),
                "One pattern per line, case-insensitive. Each matching pattern counts as one word.",
            ),
            "words",
        ),
        pg(
            hp(
                ti(
                    fg("Word blacklist", "filters.words.premade_profane", "bool"),
                    "Use premade list of profane words",
                ),
                "A short English list, matched as whole words. It is strict: common swear words also hit in harmless uses, such as film and game titles.",
            ),
            "words",
        ),
        pg(
            hp(
                ti(
                    fg("Word blacklist", "filters.words.premade_vulgar", "bool"),
                    "Use premade list of vulgar words",
                ),
                "A short English list, matched as whole words. It is strict: some words also have a harmless meaning, such as a bird or a Latin phrase.",
            ),
            "words",
        ),
        pg(
            hp(
                ti(
                    fg("Word blacklist", "filters.words.premade_racist", "bool"),
                    "Use premade list of racist words",
                ),
                "This list is empty: Brim ships no slurs, so turning it on blocks nothing. Add the words your community wants blocked to Strict words or Wildcard words above.",
            ),
            "words",
        ),
        pg(
            ti(
                fg(
                    "Word blacklist",
                    "filters.words.use_discord_automod",
                    "bool",
                ),
                "Use Discord's AutoMod keyword blacklist instead",
            ),
            "words",
        ),
        pg(
            ti(
                fg("Link blacklist", "filters.blacklisted_domains", "list"),
                "Links",
            ),
            "links",
        ),
    ]);
    let mut gate = vec![
        ti(fg("General", "enabled", "bool"), "Use the joingate module"),
        ti(
            fg("General", "dm_punished", "bool"),
            "DM members when punished",
        ),
        ti(
            fg("General", "require_all", "bool"),
            "Act only when every enabled filter matches",
        ),
        hp(
            ti(
                dv(fg("General", "exclude_trusted_bots", "bool"), false),
                "Exclude bots added by the owner or an Extra Owner",
            ),
            "Bots added by the owner or an Extra Owner skip the account filters. Trusted Admins and other admins do not count. If one of those accounts is taken over, a bot added through it is let in. Keep this off unless you need it.",
        ),
    ];
    // (key, card, status label, filter whose default action the select shows).
    for (k, card, status, filter) in [
        (
            "unauthorized_bot",
            "Bot additions",
            "Target bots added by unauthorized members",
            GateFilter::UnauthorizedBot,
        ),
        (
            "unverified_bot",
            "Unverified bot additions",
            "Target Discord unverified bots added by anyone",
            GateFilter::UnverifiedBot,
        ),
        (
            "no_avatar",
            "No avatar",
            "Target accounts that have no profile pictures",
            GateFilter::NoAvatar,
        ),
        (
            "suspicious",
            "Suspicious account",
            "Target accounts Brim deems suspicious",
            GateFilter::Suspicious,
        ),
        (
            "invite_in_name",
            "Advertising name",
            "Target accounts with invite links in usernames",
            GateFilter::InviteInName,
        ),
        (
            "account_age",
            "Account age",
            "Target accounts younger than the minimum account age",
            GateFilter::AccountAge,
        ),
        (
            "username_filter",
            "Username",
            "Target accounts whose username matches your list",
            GateFilter::UsernameFilter,
        ),
        (
            "spammer",
            "Suspected spammer",
            "Target accounts Discord flags as likely spammers",
            GateFilter::SuspectedSpammer,
        ),
    ] {
        let name = filter == GateFilter::UsernameFilter;
        let on = ti(fg(card, &format!("{k}.enabled"), "bool"), status);
        gate.extend([
            if filter == GateFilter::SuspectedSpammer {
                hp(on, SPAM_FLAG_HELP)
            } else {
                on
            },
            hp(
                ti(
                    dv(
                        choice(
                            card,
                            &format!("{k}.action"),
                            if name { NAME_ACTIONS } else { GATE_ACTIONS },
                        ),
                        match filter.default_action() {
                            GateAction::Timeout(_) => "Timeout".to_string(),
                            a => format!("{a:?}"),
                        },
                    ),
                    "Trigger action",
                ),
                if name {
                    "Rename sets the nickname to Moderated Nickname. Kick is Discord's native kick feature, Timeout its native timeout."
                } else {
                    "Kick is Discord's native kick feature, Timeout its native timeout."
                },
            ),
        ]);
    }
    gate.extend([
        {
            // One duration with a unit; an unset value shows the days setting it replaces.
            let days = GateCfg::default().min_age_days;
            let mut v = ti(
                dv(
                    fg("Account age", "min_age_override_ms", "int"),
                    i64::from(days) * 86_400_000,
                ),
                "Minimum account age",
            );
            v["fallback"] = json!({"path": "min_age_days", "scale": 86_400_000});
            v
        },
        ti(
            dv(fg("Account age", "expose_min_age", "bool"), true),
            "Tell punished members the minimum age in the DM",
        ),
        ti(
            fg("Username", "username_post_join", "bool"),
            "Enable this filter POST join too",
        ),
        hp(
            ti(fg("Username", "username_strict", "list"), "Strict words"),
            "Exact match, case insensitive.",
        ),
        hp(
            ti(fg("Username", "username_terms", "list"), "Wildcard words"),
            "Contains; * matches anything. Names are sanitized before matching.",
        ),
    ]);
    let raid = vec![
        ti(fg("General", "enabled", "bool"), "Use the join raid module"),
        ti(dv(f("count", "int"), 10), "Minimum trigger (accounts)"),
        ti(dv(f("span_ms", "int"), 3 * 3_600_000), "Join history"),
        ti(f("suspicious_only", "bool"), "Suspicious accounts only"),
        ti(dv(f("active_ms", "int"), 1_800_000), "Trigger duration"),
        ti(f("string_patterns", "list"), "Flag names matching"),
        ti(
            f("flag_younger_than_ms", "int"),
            "Flag accounts younger than",
        ),
        ti(
            f("flag_no_avatar", "bool"),
            "Flag accounts without an avatar",
        ),
        hp(
            ti(
                f("flag_spammer", "bool"),
                "Flag accounts Discord marks as likely spammers",
            ),
            SPAM_FLAG_HELP,
        ),
        ti(
            fg("ID flag", "id_flag_min", "int"),
            "Minimum matches (empty turns it off)",
        ),
        ti(
            dv(
                choice(
                    "ID flag",
                    "id_granularity",
                    &["Adaptive", "Hour", "Day", "Month"],
                ),
                "Adaptive",
            ),
            "Granularity",
        ),
        // Same as RaidCfg::default; the field is not optional, so emptying it must not save null.
        ti(
            dv(fg("ID flag", "id_margin_ms", "int"), 60 * 60_000),
            "Margin",
        ),
        dv(
            choice(
                "Response",
                "action",
                &["Log", "Warn", "Timeout", "Kick", "Ban"],
            ),
            "Ban",
        ),
        // D63: joingate only locks while Auto Mod's Auto lockdown switch is on.
        hp(
            choice("Response", "lock", &["", "Server", "Blind", "Role", "All"]),
            "Only works while Auto lockdown is on under Auto Mod (Use the auto lockdown module). With it off, a join raid does not lock.",
        ),
        fg("Response", "warned_roles", "roles"),
        hp(
            ti(fg("Voice Raid", "voice.enabled", "bool"), "Use the voice raid trigger"),
            "Once it triggers, everyone who joins voice is disconnected and gets the action below until the trigger duration ends. Staff with any permit, bots and members on the Auto Mod spam whitelist are never flagged.",
        ),
        ti(dv(fg("Voice Raid", "voice.count", "int"), 8), "Minimum trigger (members)"),
        ti(dv(fg("Voice Raid", "voice.span_ms", "int"), 10_000), "Join history"),
        ti(dv(fg("Voice Raid", "voice.active_ms", "int"), 300_000), "Trigger duration"),
        hp(
            ti(dv(choice("Voice Raid", "voice.action", GATE_ACTIONS), "Timeout"), "Action"),
            "A timeout here lasts 10 minutes.",
        ),
    ];
    // "heat" is the Auto Mod page, "nuke" the Anti Nuke page; the fields without a page are shared.
    let panic = vec![
        ti(
            pg(dv(fg("Trigger", "executors", "int"), 0), "nuke"),
            // D70: 0 switches anti-nuke's Panic Mode trigger off on purpose; say so on the field.
            "Quarantined accounts to trip (0 = off)",
        ),
        ti(
            pg(fg("Trigger", "heat_algorithm", "bool"), "heat"),
            "Panic on heat",
        ),
        ti(
            pg(dv(fg("Trigger", "heat_executors", "int"), 3), "heat"),
            "Punished accounts to trip",
        ),
        ti(
            // "pct" like Auto Mod's decay: a float field steps by 1, and its range is 0.001-0.99.
            pg(dv(fg("Trigger", "heat_decay", "pct"), 0.01), "nuke"),
            "Heat degradation (every second)",
        ),
        dv(fg("Trigger", "window_secs", "int"), 600),
        dv(fg("Response", "duration_secs", "int"), 60),
        ti(
            dv(fg("Response", "lock_mod_commands", "bool"), true),
            "Lock moderation commands",
        ),
        ti(
            dv(fg("Response", "unlock_when_ending", "bool"), true),
            "Unlock the server when it ends",
        ),
        ti(
            dv(fg("Response", "lock_on_trigger", "bool"), true),
            "Lock the server on trigger",
        ),
        fg("Response", "warned_roles", "roles"),
        fg("Response", "whitelisted_categories", "categories"),
    ];
    let mut backups = vec![
        ti(
            dv(f("enabled", "bool"), false),
            "Backups status: allow backups to be made manually and automatically",
        ),
        ti(dv(f("auto", "bool"), true), "Automatic backups"),
        dv(f("max_images", "int"), 10),
        dv(f("interval_hours", "int"), 3),
    ];
    for (path, title) in [
        ("restore", "Restore after a nuke"),
        ("restore_roles", "Restore deleted roles"),
        ("delete_roles", "Delete created roles"),
        ("restore_channels", "Restore deleted channels"),
        ("delete_channels", "Delete created channels"),
        ("delete_webhooks", "Delete created webhooks"),
    ] {
        backups.push(ti(dv(fg("Restore", path, "bool"), true), title));
    }
    let mut whitelist = vec![];
    for (tab, prefix) in [
        ("", ""),
        ("invites", "invites."),
        ("mentions", "mentions."),
        ("everyone", "everyone."),
    ] {
        for (key, card, kind) in [
            ("users", "Members", "users"),
            ("roles", "Roles", "roles"),
            ("channels", "Channels", "channels"),
            ("categories", "Categories", "categories"),
            ("webhooks", "Webhooks", "list"),
        ] {
            let v = ti(fg(card, &format!("{prefix}{key}"), kind), card);
            whitelist.push(if tab.is_empty() { v } else { pg(v, tab) });
        }
    }
    // The wizard's whitelist step also holds these anti-nuke lists; page "wizard" keeps them off the whitelist tabs.
    for (path, kind, title) in [
        ("ticket_categories", "categories", "Categories"),
        ("quarantine_whitelist", "users", "Users"),
    ] {
        whitelist.push(sx("antinuke", pg(ti(f(path, kind), title), "wizard")));
    }
    let misc = vec![
        ti(dv(fg("Commands", "prefix", "text"), "t!"), "Prefix"),
        ti(dv(fg("Commands", "use_slash", "bool"), true), "Use slash commands"),
        hp(
            ti(fg("Commands", "ignore_no_permit", "bool"), "Ignore commands from members without permits"),
            "Commands from members without any permit get no answer.",
        ),
        hp(
            ti(fg("Replies", "compact_replies", "bool"), "Compact replies"),
            "Brim's message replies use small, compact embeds.",
        ),
        hp(
            ti(fg("Replies", "delete_replies_secs", "int"), "Delete auto messages after"),
            "Brim's automatic messages are deleted after this time.",
        ),
        hp(
            ti(dv(fg("Replies", "ban_appeal_message", "text"), ""), "Ban appeal message"),
            "Shown to banned members with the appeal link.",
        ),
        hp(
            ti(fg("Members", "sticky_roles", "bool"), "Give roles back on rejoin"),
            "Members get their roles back when they rejoin. Roles with dangerous permissions, permit roles, bot roles and roles above Brim are never given back. A member who has to verify gets them after verifying.",
        ),
        only_when_on(ti(fg("Members", "sticky_never", "roles"), "Roles never given back"), "misc.sticky_roles"),
        hp(
            ti(fg("Members", "auto_dehoist", "bool"), "Remove symbols from the start of names"),
            "Done on join and on every name change. Only the owner and extra owners are skipped.",
        ),
        hp(
            ti(fg("Members", "enforce_permits", "bool"), "Remove dangerous roles from members without permits"),
            "Roles with dangerous permissions are taken from members who have no permit.",
        ),
        hp(
            ti(dv(fg("Members", "enforce_quarantine_perms", "bool"), true), "Always enforce Quarantine role permissions"),
            "Brim keeps the right Quarantine role permissions in every channel.",
        ),
        sx(
            "logging",
            hp(
                ti(fg("Key roles and channels", "fallback", "channel"), "Logs channel"),
                "All logs that have no channel of their own go here.",
            ),
        ),
        sx(
            "logging",
            hp(
                ti(
                    fg("Key roles and channels", "routes.mod_command", "channel"),
                    "Modlogs channel",
                ),
                "Moderation actions done by staff with the bot go here as cases.",
            ),
        ),
        sx(
            "antinuke",
            hp(
                ti(fg("Key roles and channels", "quarantine_role", "role"), "Quarantine role"),
                "Quarantined members are moved to this role.",
            ),
        ),
        sx(
            "lockdown",
            hp(
                ti(fg("Key roles and channels", "main_roles", "roles"), "Main roles"),
                "The roles your members have; lockdown and verification use them.",
            ),
        ),
        sx(
            "whitelist",
            hp(
                ti(fg("Key roles and channels", "partner_channels", "channels"), "Partnering channels"),
                "Spam, @everyone pings and invites are allowed here.",
            ),
        ),
        hp(
            ti(dv(fg("Moderation commands", "confirm", "bool"), true), "Ask for a Yes/No confirmation when the target is given as an ID or a name"),
            "A @mention is never asked about. Only the moderator who ran the command can press Yes or No, and the question closes after 10 seconds. Purge, hackbans and note sweeps ask too.",
        ),
        hp(
            ti(dv(fg("Moderation commands", "always_dm", "bool"), true), "DM members when they are punished or when a punishment is removed"),
            "Punishment DMs are sent before the action, removal DMs (unquarantine, untimeout, unban) once it worked. If the member has closed DMs, the action still happens and the result shows DM-Members ❎. Unban DMs usually fail because the member no longer shares a server with Brim. Off: only ?dm sends one.",
        ),
        ti(fg("Moderation commands", "delete_invocation", "bool"), "Always delete the moderation execution messages for regular prefix commands"),
        ti(fg("Moderation commands", "hide_staff_in_dms", "bool"), "Hide the staff member responsible when DMing the punished member"),
        hp(
            ti(
                dv(fg("Moderation commands", "default_timeout_secs", "int"), 0),
                "Default timeout duration",
            ),
            "0 means 28 days",
        ),
        ti(dv(fg("Moderation commands", "ban_purge_days", "int"), 7), "Days purged on bans"),
        ti(dv(fg("Moderation commands", "remove_main_roles_on_timeout", "bool"), true), "Remove the main roles from the target on timeout"),
        ti(dv(fg("Auto moderation", "show_reason_on_auto", "bool"), true), "Display the reason behind the auto punishment in the message Brim sends"),
        ti(fg("Auto moderation", "purge_last_100", "bool"), "Always purge the messages sent by the punished member found in the recent 100 messages"),
        ti(dv(fg("Auto warn actions", "auto_warn_actions", "bool"), true), "Issue an action once a member reaches certain points"),
        ti(dv(fg("Auto warn actions", "warn_base_points", "int"), 5), "Base points"),
        ti(fg("Auto warn actions", "warn_actions", "warn_actions"), "Custom actions"),
        hp(
            ti(dv(fg("Invites", "invite_tracking", "bool"), true), "Track which invite each member joined with"),
            "Needed for /invites. Brim needs the Manage Server permission to read invites.",
        ),
        hp(
            ti(dv(fg("Invites", "invites_public_top", "bool"), true), "Anyone can see the invite leaderboard"),
            "Off: only members with a permit can use /invites top.",
        ),
        hp(
            ti(dv(fg("Invites", "invite_max_age_secs", "int"), 86_400), "Longest invite link lifetime in seconds"),
            "Default and maximum for links made with /invites create. 0 means members may also pick never; at most 604800 (7 days).",
        ),
        hp(
            ti(dv(fg("Invites", "invite_max_uses", "int"), 0), "Most uses per invite link"),
            "Default and maximum for links made with /invites create. 0 means members may also pick no limit; at most 100.",
        ),
        sx(
            "retention",
            hp(
                ti(dv(fg("Data kept", "cases_days", "int"), 0), "Cases kept for (days)"),
                RETENTION_HELP,
            ),
        ),
        sx(
            "retention",
            hp(
                ti(dv(fg("Data kept", "proofs_days", "int"), 365), "Verified proof kept for (days)"),
                "The copies of members' messages that Brim stores on a case as verified proof. 0 keeps them as long as the case; otherwise at least 30 days. Proof that staff add stays with its case. Changing this needs the owner or an Extra Owner and 2FA.",
            ),
        ),
        sx(
            "retention",
            hp(
                ti(dv(fg("Data kept", "notes_days", "int"), 0), "Staff notes kept for (days)"),
                RETENTION_HELP,
            ),
        ),
        sx(
            "retention",
            hp(
                ti(dv(fg("Data kept", "reports_days", "int"), 365), "Handled reports kept for (days)"),
                RETENTION_HELP,
            ),
        ),
        sx(
            "retention",
            hp(
                ti(dv(fg("Data kept", "appeals_days", "int"), 365), "Decided appeals kept for (days)"),
                RETENTION_HELP,
            ),
        ),
        sx(
            "retention",
            hp(
                ti(dv(fg("Data kept", "tickets_days", "int"), 365), "Closed tickets kept for (days)"),
                RETENTION_HELP,
            ),
        ),
        sx(
            "retention",
            hp(
                ti(dv(fg("Data kept", "config_history_days", "int"), 0), "Config history kept for (days)"),
                RETENTION_HELP,
            ),
        ),
    ];
    let verification = vec![
        ti(
            fg("General", "enabled", "bool"),
            "Use the verification module",
        ),
        hp(
            ti(
                fg("General", "quarantine_on_join", "bool"),
                "Quarantine user on join (not recommended)",
            ),
            "Legacy: every joiner sits in quarantine until they verify.",
        ),
        hp(
            ti(
                dv(fg("General", "replace_roles", "bool"), false),
                "Set autoroles instead of adding them",
            ),
            "On a pass the member's roles are replaced by the verification roles.",
        ),
        dv(
            ti(
                choice("General", "target", &["Everyone", "SuspiciousOnly"]),
                "Verification targets",
            ),
            "Everyone",
        ),
        hp(
            dv(
                ti(
                    choice(
                        "General",
                        "on_fail",
                        &["Nothing", "Kick", "Ban", "Quarantine"],
                    ),
                    "Fail action",
                ),
                "Kick",
            ),
            "Kick, Ban and Quarantine run when the verification duration ends after the join.",
        ),
        hp(
            ti(
                dv(fg("General", "audit_reason", "bool"), true),
                "Add a reason to the audit log when the time limit runs out",
            ),
            "Kicks, bans and quarantines at the time limit show \"Did not verify within the time limit\" in the server's audit log.",
        ),
        dv(
            ti(
                choice(
                    "General",
                    "mode",
                    &["Captcha", "Web", "Dm", "SimpleCaptcha", "Button"],
                ),
                "Verification mode",
            ),
            "Captcha",
        ),
        unless(
            ti(
                dv(fg("General", "show_notes", "bool"), true),
                "Display helpful notes alongside the captcha",
            ),
            "only",
            "verification.mode",
            NOTE_MODES,
        ),
        unless(
            hp(
                ti(
                    dv(fg("General", "block_vpn", "bool"), false),
                    "Block VPNs and proxies",
                ),
                "Members on Tor, a VPN or a proxy are asked to turn it off before they can verify. Their IP is checked against free public lists on this server; the check stores it nowhere and sends it nowhere. (With Turnstile keys set, Cloudflare's captcha on the page sees the visitor's IP, as on any site that uses it.)",
            ),
            "only",
            "verification.mode",
            &["Web"],
        ),
        only_when_on(
            hp(
                ti(
                    dv(fg("General", "block_hosting", "bool"), true),
                    "Also block hosting networks",
                ),
                "Cloud and server providers (AWS, Google Cloud, Hetzner, OVH and others). Most VPNs and proxies run there. Someone on a work VPN may be refused until they turn it off.",
            ),
            "verification.block_vpn",
        ),
        ti(
            fg("General", "regen_ms", "int"),
            "New captcha image every (milliseconds, empty is off)",
        ),
        hp(
            dv(
                ti(
                    fg("General", "time_limit_ms", "int"),
                    "Verification duration",
                ),
                120_000,
            ),
            "The time limit pauses while a member is timed out.",
        ),
        hp(
            ti(fg("General", "channel", "channel"), "Verification channel"),
            "If the verification channel is deleted, Brim makes a new one (up to 3 in 10 minutes), tells the owner, and quarantines whoever deleted it. Owners, Extra Owners and Trusted Admins only get a warning the first time.",
        ),
        ti(fg("General", "roles", "roles"), "Verification roles"),
        unless(
            ti(
                dv(fg("Captcha", "captcha.height", "int"), 100),
                "Captcha height",
            ),
            "used",
            "verification.mode",
            IMAGE_MODES,
        ),
        ti(
            dv(fg("Captcha", "captcha.width", "int"), 300),
            "Captcha width",
        ),
        ti(
            dv(fg("Captcha", "captcha.font_size", "int"), 40),
            "Font size",
        ),
        ti(
            dv(fg("Captcha", "captcha.font_color", "color"), "#32cf7e"),
            "Font color",
        ),
        ti(
            dv(fg("Captcha", "captcha.length", "int"), 6),
            "Captcha answer length",
        ),
        ti(
            dv(choice("Captcha", "captcha.charset", &Charset::NAMES), "AZ"),
            "Captcha characters",
        ),
        ti(
            dv(fg("Captcha", "captcha.trace", "bool"), true),
            "Show a colored trace that follows the captcha characters",
        ),
        ti(
            dv(fg("Captcha", "captcha.mixed", "bool"), true),
            "Add a mixed character under each captcha character",
        ),
        ti(
            dv(fg("Captcha", "captcha.spread", "bool"), true),
            "Show small spread-around characters to trick scripts",
        ),
        ti(
            dv(fg("Captcha", "captcha.decoy_color", "color"), "#646566"),
            "Decoy color",
        ),
    ];
    let tickets =
        vec![
        ti(fg("General", "enabled", "bool"), "Members can DM Brim to open a ticket"),
        hp(
            ti(fg("General", "category", "category"), "Ticket category"),
            "Owners, Extra Owners, Trusted Admins and permits with Manage Tickets see and answer every ticket.",
        ),
        hp(
            ti(fg("General", "log_channel", "channel"), "Ticket log channel"),
            "Without a log channel, a closed ticket is deleted with no copy of the conversation.",
        ),
        ti(
            dv(fg("General", "full_log", "bool"), true),
            "Full conversation in the ticket log",
        ),
        hp(
            wn(ti(dv(fg("General", "public_channel", "bool"), true), "Public ticket channel"), "Turning this off deletes the channel."),
            "Brim keeps #open-a-ticket in the ticket category with the panel message. Verified members only see it and press the button; nobody can write or react there.",
        ),
        ti(fg("General", "ping_roles", "roles"), "Roles pinged on a new ticket"),
        ti(fg("General", "ping_here", "bool"), "Ping @here on a new ticket"),
        hp(
            ti(dv(fg("General", "ping_staff", "bool"), true), "Ping staff in a new panel ticket"),
            "Pings the roles of the Moderators permit and every higher permit with Manage Tickets.",
        ),
        hp(
            ti(dv(fg("Messages", "greeting", "text"), brim_core::sections::DEFAULT_GREETING), "Greeting message"),
            "Sent to the member when a ticket opens, also in a panel ticket whose panel has no greeting of its own. Empty sends none. Tags: {username} {userid} {usermention} {ticket_type}",
        ),
        ti(dv(fg("Messages", "closing", "text"), ""), "Closing message"),
        ti(fg("Replies", "anonymous", "bool"), "Anonymous replies by default"),
        ti(
            fg("Replies", "command_only", "bool"),
            "Only the reply command reaches the member",
        ),
        ti(fg("New tickets", "paused", "bool"), "Turn off new tickets"),
        ti(
            dv(fg("New tickets", "pause_reason", "text"), ""),
            "Reason shown to members",
        ),
        {
            let mut v = ti(fg("Blacklist", "blocked", "users"), "Members who cannot open tickets");
            v["cap"] = json!(brim_core::sections::MAX_BLOCKED);
            v
        },
        {
            let mut v = hp(
                ti(dv(fg("Auto-close", "auto_close_hours", "int"), 0), "Close a ticket after this many hours without a message"),
                "One setting for the whole server, for DM and panel tickets. 0 = off. A new message from the member or staff stops the close.",
            );
            // The range `api::validate` enforces (at most 30 days).
            (v["min"], v["max"]) = (json!(0), json!(720));
            v
        },
        hp(
            ti(
                dv(fg("General", "types", "list"), json!(["Report", "Appeal", "Question", "Other"])),
                "Ticket types",
            ),
            "The member picks one when the ticket opens; if they skip it, staff pick at claim. Staff can change it any time, and every change goes to the ticket log. At most 25.",
        ),
        hp(
            ti(fg("General", "button_per_type", "bool"), "One button per ticket type"),
            "On: a panel with no ticket type of its own shows one button for each type above (two types or more), and the member picks the type by pressing it. Off: one button per panel. A type name is the button label, so at most 80 characters. Save, and Brim edits the posted message.",
        ),
        {
            let mut v = ti(fg("Panels", "panels", "panels"), "Ticket panels");
            // The button card's defaults, so the UI keeps no copy of them.
            // With every default ticket type; the page leaves off one the server removed.
            v["panel_default"] = brim_core::sections::default_panels();
            hp(v,
            "Each panel is a button that opens a private ticket channel for the member and staff. The first one is the button in #open-a-ticket. To post the panels in another channel too, save, then use Publish.",
            )
        },
        hp(
            {
                let mut v = ti(fg("Snippets", "snippets", "snippets"), "Saved replies");
                v["cap"] = json!(brim_core::sections::MAX_SNIPPETS);
                v
            },
            "Sent with the snippet command in a ticket. Tags: {username} {userid} {usermention} {ticket_type}",
        ),
    ];
    let appeals = vec![
        ti(fg("General", "enabled", "bool"), "Appeals System Status"),
        ti(fg("General", "channel", "channel"), "Appeals channel"),
        fg("Appeal Form", "fields", "form"),
    ];
    [
        ("logging", "Logging", log),
        ("antinuke", "Anti-nuke", nuke),
        ("automod", "Automod", auto),
        ("joingate", "Join gate", gate),
        ("joinraid", "Join raid", raid),
        (
            "lockdown",
            "Lockdown",
            vec![
                f("main_roles", "roles"),
                ti(
                    dv(fg("Lock scope", "channels", "bool"), true),
                    "Lock server channels",
                ),
                ti(
                    dv(fg("Lock scope", "roles", "bool"), false),
                    "Lock staff roles",
                ),
                ti(
                    dv(fg("Lock scope", "joins", "bool"), false),
                    "Lock server joins",
                ),
                ti(fg("Lock scope", "updates", "bool"), "Make lockdown updates"),
                ti(
                    dv(fg("Lock scope", "pause_invites", "bool"), true),
                    "Pause invites and member DMs when kicking new members",
                ),
                ti(
                    fg("Lock scope", "voice", "bool"),
                    "Lock voice too (connect, speak, soundboard, stream)",
                ),
            ],
        ),
        (
            "catch",
            "Catch channel",
            vec![
                ti(fg("General", "enabled", "bool"), "Use the catch channel"),
                hp(
                    ti(fg("General", "channel", "channel"), "Catch channel"),
                    "Anyone without a permit who writes here is punished. Hacked accounts spam every channel, so they land here first.",
                ),
                hp(
                    dv(
                        ti(
                            choice("General", "action", &["Ban", "Softban", "Kick", "Timeout", "Log"]),
                            "Action",
                        ),
                        "Ban",
                    ),
                    "Log is a test mode: it only logs. Bans delete messages for the days set under Miscellaneous.",
                ),
                ti(fg("General", "dm", "bool"), "DM the member first, with the appeal link"),
                ti(fg("General", "keep_top", "bool"), "Keep the channel in place (top of the server, or the bottom of Information)"),
                ti(fg("Whitelist", "roles", "roles"), "Whitelisted roles"),
                ti(fg("Whitelist", "users", "users"), "Whitelisted members"),
            ],
        ),
        ("panic", "Panic mode", panic),
        ("backups", "Backups", backups),
        ("appeals", "Appeals", appeals),
        ("verification", "Verification", verification),
        ("tickets", "Tickets", tickets),
        ("whitelist", "Whitelist", whitelist),
        ("misc", "Miscellaneous", misc),
        (
            "raider_list",
            "Raider List",
            vec![
                ti(fg("General", "enabled", "bool"), "Use the Raider List"),
                ti(
                    dv(
                        choice("General", "action", &["alert", "quarantine", "kick", "ban"]),
                        "ban",
                    ),
                    "When a listed account joins",
                ),
                ti(
                    dv(
                        multi("General", "categories", &["nuke", "raid", "scam", "spam"]),
                        json!(["nuke", "raid", "scam", "spam"]),
                    ),
                    "Act on these categories",
                ),
                hp(
                    ti(
                        dv(fg("General", "scan_members", "bool"), true),
                        "Check members already in the server",
                    ),
                    "Only alerts in the mod log, it never acts on members who are already here.",
                ),
                ti(
                    dv(fg("General", "lift_on_removal", "bool"), true),
                    "Unban here when someone is taken off the list",
                ),
                ti(
                    fg("General", "no_collect", "bool"),
                    "Do not send what Brim catches here to the list",
                ),
            ],
        ),
    ]
    .into_iter()
    .map(|(n, t, mut fields)| {
        // The form keeps numbers inside the same ranges `api::validate` enforces.
        for v in &mut fields {
            let sec = v["section"].as_str().unwrap_or(n).to_string();
            if let Some((.., tip)) = TIPS.iter().find(|(ts, tp, _)| {
                *ts == sec && v["path"].as_str().is_some_and(|p| p == *tp || p.ends_with(&format!(".{tp}")))
            }) {
                v["tip"] = json!(tip);
            }
            if let Some((lo, hi)) = v["path"].as_str().and_then(|p| limits::for_path(&sec, p)) {
                v["min"] = json!(lo);
                v["max"] = json!(hi);
            }
            if let Some((lo, hi)) = v["path"].as_str().and_then(|p| limits::float_for_path(&sec, p)) {
                v["min"] = json!(lo);
                v["max"] = json!(hi);
            }
            if v["kind"] == "bool" && v["help"].is_null() && v["tip"].is_null() {
                let (p, card) = (v["path"].as_str().unwrap_or(""), v["group"].as_str().unwrap_or(""));
                if let Some(h) = bool_help(&sec, p, card) {
                    v["help"] = json!(h);
                }
            }
        }
        json!({"name": n, "title": t, "fields": fields})
    })
    .collect()
}

/// The grey line under a toggle that has no help text of its own (review). Cards with
/// many rows (Anti Nuke limits, Auto Mod filters, Join Gate filters) share one line per kind.
fn bool_help(sec: &str, path: &str, card: &str) -> Option<String> {
    let lower = card.to_lowercase();
    if sec == "antinuke" && path == "limits.RoleRemove.enabled" {
        return Some("Counts role removals per account. They only add heat for Panic Mode.".into());
    }
    if sec == "antinuke" && path.starts_with("limits.") {
        return Some(format!(
            "Counts {lower} per account. Past the minute or hour limit, the action is undone and the account is quarantined (with Auto quarantine on)."
        ));
    }
    // These only add heat; the action sits on the General card (bot automod.rs heat weights).
    const HEAT_ONLY: &[&str] = &[
        "mention_users",
        "mention_roles",
        "mention_everyone",
        "embeds",
        "images",
        "files",
        "attach_links",
        "stickers",
    ];
    if sec == "automod"
        && HEAT_ONLY
            .iter()
            .any(|k| path == format!("filters.{k}.enabled"))
    {
        return Some("Off: this adds no heat. The action is set on the General card.".into());
    }
    if sec == "automod"
        && path.starts_with("filters.")
        && path.ends_with(".enabled")
        && !path.starts_with("filters.mentions.")
        && !path.starts_with("filters.attachments.")
    {
        return Some(
            "Off: messages are not checked for this. On: a match gets the action below.".into(),
        );
    }
    if sec == "joingate" && path.ends_with(".enabled") {
        return Some("On: a new member that matches gets this filter's action.".into());
    }
    Some(match (sec, path) {
        ("antinuke", "enabled") => "Off: nobody is counted or punished. Changes only permit holders may make (dangerous permissions, the vanity URL) are still undone.",
        ("antinuke", "auto_quarantine") => "An account past a limit is quarantined. A nuking bot stays in the server: its own role loses all permissions and it is quarantined; it is kicked only if that fails. Off: a reached limit is only logged, undone and counted towards Panic Mode.",
        ("antinuke", "monitor_public_roles") => "Only the Owner and Extra Owners change the main roles while this is on. Dangerous permissions on any role, member, bot or channel overwrite are always guarded: only the Owner, an Extra Owner or a Trusted Admin gives them, and anyone else's change is undone (with Anti Nuke on, they are quarantined too).",
        ("antinuke", "prune") => "Stops an account that prunes members, like any other dangerous action.",
        ("antinuke", "quarantine_hold") => "Changing a quarantined member's roles, nickname or timeout, or kicking or banning them, counts as an attack.",
        ("antinuke", "vanity_guard") => "Whoever changes the vanity URL is quarantined, and the old code is put back.",
        ("automod", "enabled") => "Off: Auto Mod checks no messages on this server.",
        ("automod", "anti_spam") => "Off: only the link and word filters run.",
        ("automod", "monitor_webhooks") => "Off: messages from webhooks are left alone.",
        ("automod", "strike.enabled") => "Each time a member's heat reaches the max, they get a strike, and strikes give timeouts. Off: strikes are counted, but nobody is timed out.",
        ("automod", "strike.multiply") => "A cap timeout that follows another within the reoffend window is longer, by the multiplier, up to 28 days.",
        ("automod", "auto_lockdown.enabled") => "Locks the server by itself when an attack trigger below is reached.",
        ("automod", "auto_lockdown.mentions") => "Locks the server when members send that many mentions in a short time.",
        ("automod", "filters.mentions.enabled") => "Mentions in a message count towards the mention filters.",
        ("automod", "filters.attachments.enabled") => "Attachments in a message count towards the attachment filters.",
        ("automod", "filters.nsfw_crawl_shorteners") => "Follows short links, like bit.ly, before checking. Already done when NSFW links, Invite links or Bad websites is on.",
        ("automod", "filters.words.use_discord_automod") => "Keeps the words in a Discord AutoMod rule, so Discord blocks the message before anyone sees it.",
        ("joingate", "enabled") => "Off: new members are not checked when they join.",
        ("joingate", "dm_punished") => "DMs the member before a warn, timeout, kick or ban. The reason shows only if \"Display the reason\" (Miscellaneous) is on.",
        ("joingate", "expose_min_age") => "Off: the DM does not say how old an account must be.",
        ("joingate", "username_post_join") => "Also checks a member's name again when they change their name or nickname.",
        ("joinraid", "enabled") => "Off: Brim does not look for join raids.",
        ("joinraid", "suspicious_only") => "Only suspicious accounts count towards the minimum trigger.",
        ("joinraid", "flag_no_avatar") => "During a raid, joiners without a profile picture get the raid action.",
        ("lockdown", "channels") => "A server lock takes the chat permissions from your main roles in every channel.",
        ("lockdown", "roles") => "A server lock takes the dangerous permissions from every role below Brim.",
        ("lockdown", "joins") => "A server lock bans members who join while it is on.",
        ("lockdown", "updates") => "Posts each lock and unlock in the mod command log.",
        ("lockdown", "pause_invites") => "While a lock kicks new members, Discord invites and member DMs are paused too.",
        ("lockdown", "voice") => "Main roles also cannot join, speak, use the soundboard or stream in voice channels.",
        ("catch", "enabled") => "Hacked accounts post in every channel. Anyone who writes in the catch channel is punished, except the Owner, Extra Owners, Trusted Admins, whitelisted members and bots.",
        ("catch", "dm") => "The member gets a DM before the punishment, with the appeal link for bans and timeouts.",
        ("catch", "keep_top") => "Moves the channel back when someone drags it somewhere else.",
        ("panic", "heat_algorithm") => "Panic Mode also starts on heat: Auto Mod heat and Anti Nuke heat per action.",
        ("panic", "lock_mod_commands") => "During Panic Mode, only the Owner, Extra Owners and Trusted Admins can use moderation commands.",
        ("panic", "unlock_when_ending") => "When Panic Mode ends, Brim unlocks the server again. Off: staff unlock it.",
        ("panic", "lock_on_trigger") => "Locks the server while the attack is contained.",
        ("backups", "enabled") => "Off: no backups are made, by hand or on a schedule.",
        ("backups", "auto") => "Makes a backup by itself, every number of hours you set.",
        ("backups", "restore") => "After an attack, Brim puts the server back from the latest backup. The options below pick what it changes.",
        ("backups", "restore_roles") => "Makes the roles the attacker deleted again.",
        ("backups", "delete_roles") => "Deletes roles made since the backup.",
        ("backups", "restore_channels") => "Makes the channels the attacker deleted again.",
        ("backups", "delete_channels") => "Deletes channels made since the backup.",
        ("backups", "delete_webhooks") => "Deletes webhooks made since the backup. Message Builder webhooks stay.",
        ("appeals", "enabled") => "Punished members can ask for the punishment to be lifted, with the link in their DM.",
        ("verification", "enabled") => "Off: new members get full access without a check.",
        ("verification", "show_notes") => "Adds short tips under the captcha: type the traced characters, ignore the decoys, case does not matter.",
        ("verification", "captcha.trace") => "Harder for scripts to read, and easy for people to follow.",
        ("verification", "captcha.mixed") => "Harder for scripts to read.",
        ("verification", "captcha.spread") => "Decoy characters that people ignore and scripts read.",
        ("tickets", "enabled") => "Members DM Brim, and staff answer in a private ticket channel.",
        ("tickets", "full_log") => "The close log has the whole conversation as a file.",
        ("tickets", "ping_here") => "Pings @here: online members who can see the ticket channel.",
        ("tickets", "anonymous") => "Staff replies do not show the staff member's name, unless they choose to.",
        ("tickets", "command_only") => "Only the reply command reaches the member. Other messages in the ticket stay staff talk.",
        ("tickets", "paused") => "No new tickets open. Members see the reason you give.",
        ("misc", "use_slash") => "Off: only the Owner and Extra Owners can use Brim's slash commands.",
        ("misc", "delete_invocation") => "Deletes the staff member's prefix command message after it runs.",
        ("misc", "hide_staff_in_dms") => "The DM to the punished member does not name the staff member.",
        ("misc", "remove_main_roles_on_timeout") => "Takes the main roles away while the member is timed out.",
        ("misc", "show_reason_on_auto") => "Off: the message says only that the member was punished.",
        ("misc", "purge_last_100") => "Deletes the member's messages among the last 100 when Auto Mod punishes them.",
        ("misc", "auto_warn_actions") => "Warns give points. At the points you set below, Brim times out, kicks or bans.",
        _ => return None,
    }.into())
}

/// Ids held in `v`: a string or number, or an array of them.
fn flat(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if !s.is_empty() => out.push(s.clone()),
        Value::Number(n) => out.push(n.to_string()),
        Value::Array(a) => a.iter().for_each(|x| flat(x, out)),
        _ => {}
    }
}

/// Channel and role ids the schema's fields of `section` hold in `v`, as (channels, roles).
pub fn ids(section: &str, v: &Value) -> (Vec<String>, Vec<String>) {
    let (mut ch, mut ro) = (vec![], vec![]);
    for sec in schema() {
        for fd in sec["fields"].as_array().into_iter().flatten() {
            if fd["section"].as_str().or(sec["name"].as_str()) != Some(section) {
                continue;
            }
            let path = format!(
                "/{}",
                fd["path"].as_str().unwrap_or_default().replace('.', "/")
            );
            let at = v.pointer(&path).unwrap_or(&Value::Null);
            let items = || at.as_array().into_iter().flatten();
            match fd["kind"].as_str().unwrap_or_default() {
                "channel" | "channels" | "category" | "categories" => flat(at, &mut ch),
                "role" | "roles" => flat(at, &mut ro),
                "customs" => items().for_each(|c| flat(&c["channel"], &mut ch)),
                "panels" => items().for_each(|p| {
                    flat(&p["category"], &mut ch);
                    flat(&p["ping_roles"], &mut ro)
                }),
                "route_pick" => at
                    .as_object()
                    .into_iter()
                    .flatten()
                    .for_each(|(_, c)| flat(c, &mut ch)),
                _ => {}
            }
        }
    }
    (ch, ro)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_toggle_has_a_help_line() {
        for s in super::schema() {
            for f in s["fields"].as_array().unwrap() {
                if f["kind"] == "bool" {
                    assert!(
                        f["help"].is_string() || f["tip"].is_string(),
                        "{}: {} has no help",
                        s["name"],
                        f["path"]
                    );
                    assert!(
                        f["title"].is_string(),
                        "{}: {} has no title",
                        s["name"],
                        f["path"]
                    );
                }
            }
        }
    }

    use super::*;
    use crate::api;

    #[test]
    fn limited_numbers_carry_their_range() {
        let all = schema();
        let field = |sec: &str, path: &str| {
            all.iter()
                .find(|s| s["name"] == sec)
                .and_then(|s| {
                    s["fields"]
                        .as_array()?
                        .iter()
                        .find(|f| f["path"] == path)
                        .cloned()
                })
                .unwrap()
        };
        let hour = field("antinuke", "limits.Ban.per_hour");
        assert_eq!((&hour["min"], &hour["max"]), (&json!(2), &json!(500)));
        let heat = field("antinuke", "limits.Unban.heat");
        assert_eq!((&heat["min"], &heat["max"]), (&json!(1), &json!(100)));
        let age = field("joingate", "min_age_override_ms");
        assert_eq!(
            (&age["min"], &age["max"]),
            (&json!(5_000), &json!(8_640_000_000_i64))
        );
        assert!(field("antinuke", "enabled")["min"].is_null());
    }

    #[test]
    fn gate_actions_show_defaults() {
        let gate = schema()
            .into_iter()
            .find(|s| s["name"] == "joingate")
            .unwrap();
        let default = |p: &str| {
            gate["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["path"] == p)
                .unwrap()["default"]
                .clone()
        };
        assert_eq!(default("username_filter.action"), "Ban");
        assert_eq!(default("invite_in_name.action"), "Timeout");
        assert_eq!(default("no_avatar.action"), "Kick");
        assert_eq!(default("spammer.action"), "Kick");
    }

    #[test]
    fn ids_follow_field_kinds() {
        let v = json!({"fallback": "1", "routes": {"report": 2}, "events": {"member_kick": ""},
            "custom": [{"channel": "3", "events": []}], "ignored_channels": ["4"], "other": "9"});
        let (ch, ro) = ids("logging", &v);
        let mut ch = ch;
        ch.sort();
        ch.dedup();
        assert_eq!(ch, ["1", "2", "3", "4"]);
        assert!(ro.is_empty());
        let v = json!({"panels": [{"category": "5", "ping_roles": ["6"]}]});
        assert_eq!(ids("tickets", &v), (vec!["5".into()], vec!["6".into()]));
    }

    /// A valid value for a schema field, by its kind.
    fn sample(fd: &Value) -> Value {
        match fd["kind"].as_str().unwrap() {
            "bool" => json!(true),
            "int" => fd.get("min").cloned().unwrap_or(json!(5)),
            "float" | "pct" => json!(0.5),
            "text" | "color" => json!("x"),
            "list" => json!(["1"]),
            "choice" => fd["options"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o.as_str() != Some(""))
                .unwrap()
                .clone(),
            "channel" | "role" | "category" => json!("1"),
            "form" => json!([{"label": "x"}]),
            "panels" => json!([{"id": "help", "name": "Help", "fields": [{"label": "x"}]}]),
            "customs" => json!([{"events": [fd["options"][0]], "channel": "1"}]),
            "route_pick" => json!({fd["options"][0]["key"].as_str().unwrap(): "1"}),
            "warn_actions" => json!([{"points": 5, "action": "timeout", "secs": 60}]),
            "snippets" => json!([{"name": "x", "content": "y"}]),
            "multi" => json!([fd["options"][0]]),
            _ => json!(["1"]),
        }
    }

    /// D70. Every number the form fills in by default (what the wizard and an emptied field save)
    /// passes the save check, so the new ranges never refuse the dashboard's own values.
    #[test]
    fn every_default_passes_the_range_check() {
        let mut bad = vec![];
        for sec in schema() {
            for fd in sec["fields"].as_array().unwrap() {
                let (Some(path), d) = (fd["path"].as_str(), &fd["default"]) else {
                    continue;
                };
                if !d.is_number() {
                    continue;
                }
                let name = fd["section"].as_str().or(sec["name"].as_str()).unwrap();
                let mut doc = json!({});
                let parts: Vec<_> = path.split('.').collect();
                let mut cur = &mut doc;
                for p in &parts[..parts.len() - 1] {
                    cur = &mut cur[*p];
                }
                cur[parts[parts.len() - 1]] = d.clone();
                if let Err(e) = limits::check(name, &doc, &Value::Null) {
                    bad.push(format!("{name}.{path}={d}: {e}"));
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn every_schema_path_deserializes() {
        let mut objs: std::collections::BTreeMap<String, Value> = Default::default();
        for sec in schema() {
            for fd in sec["fields"].as_array().unwrap() {
                let sample = sample(fd);
                let name = fd["section"].as_str().or(sec["name"].as_str()).unwrap();
                let mut cur = objs.entry(name.to_string()).or_insert_with(|| json!({}));
                let parts: Vec<_> = fd["path"].as_str().unwrap().split('.').collect();
                for p in &parts[..parts.len() - 1] {
                    cur = &mut cur[*p];
                }
                cur[parts[parts.len() - 1]] = sample;
            }
        }
        for (name, obj) in objs {
            api::validate(&name, &obj, &Value::Null)
                .unwrap_or_else(|e| panic!("{name} schema does not deserialize: {e:?}"));
        }
    }

    /// The form saves an emptied number field as its default, or null when it has none,
    /// so a field without a default must be optional in the config, or saving fails.
    #[test]
    fn number_fields_without_default_accept_null() {
        let mut bad = vec![];
        for sec in schema() {
            for fd in sec["fields"].as_array().unwrap() {
                if !matches!(fd["kind"].as_str(), Some("int" | "float" | "pct"))
                    || !fd["default"].is_null()
                {
                    continue;
                }
                let name = fd["section"].as_str().or(sec["name"].as_str()).unwrap();
                let mut obj = json!({});
                let mut cur = &mut obj;
                let parts: Vec<_> = fd["path"].as_str().unwrap().split('.').collect();
                for p in &parts[..parts.len() - 1] {
                    cur = &mut cur[*p];
                }
                cur[parts[parts.len() - 1]] = Value::Null;
                if let Err(e) = api::validate(name, &obj, &Value::Null) {
                    bad.push(format!("{name}.{}: {e:?}", fd["path"].as_str().unwrap()));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "number fields that cannot be emptied:\n{}",
            bad.join("\n")
        );
    }

    /// A section saved with only some fields (a server that never set the rest) still loads:
    /// every config struct needs `#[serde(default)]`, like Join Raid lacked (`suspicious_only`).
    #[test]
    fn one_field_alone_deserializes() {
        let mut bad = vec![];
        for sec in schema() {
            for fd in sec["fields"].as_array().unwrap() {
                let name = fd["section"].as_str().or(sec["name"].as_str()).unwrap();
                let mut obj = json!({});
                let mut cur = &mut obj;
                let parts: Vec<_> = fd["path"].as_str().unwrap().split('.').collect();
                for p in &parts[..parts.len() - 1] {
                    cur = &mut cur[*p];
                }
                cur[parts[parts.len() - 1]] = sample(fd);
                if let Err(e) = api::validate(name, &obj, &Value::Null) {
                    bad.push(format!("{name}.{}: {e:?}", fd["path"].as_str().unwrap()));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "sections that need every field:\n{}",
            bad.join("\n")
        );
    }

    #[test]
    fn every_tip_finds_its_field() {
        let all = schema();
        for (sec, path, _) in TIPS {
            let found = all
                .iter()
                .flat_map(|s| {
                    let n = s["name"].clone();
                    s["fields"].as_array().unwrap().iter().map(move |f| {
                        (
                            f["section"]
                                .as_str()
                                .map(str::to_string)
                                .or(n.as_str().map(str::to_string)),
                            f.clone(),
                        )
                    })
                })
                .any(|(s, f)| {
                    s.as_deref() == Some(*sec)
                        && !f["tip"].is_null()
                        && f["path"]
                            .as_str()
                            .is_some_and(|p| p == *path || p.ends_with(&format!(".{path}")))
                });
            assert!(found, "tip for {sec}.{path} matches no field");
        }
    }
}

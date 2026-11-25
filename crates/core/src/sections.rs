//! Typed per-guild config sections shared by the bot (reads them) and the dashboard (validates writes).
use brim_antinuke::{Action as NukeAction, Limit};
use brim_automod::{
    heat::{AutoSlowmodeCfg, HeatCfg},
    strike::StrikeCfg,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use twilight_model::id::{
    marker::{ChannelMarker, RoleMarker, UserMarker, WebhookMarker},
    Id,
};

/// Unknown keys are ignored: saved configs and exports still hold the removed `strict_mode`,
/// `strict_member_roles` and `monitor_channel_perms` (11-15), so no `deny_unknown_fields`.
#[derive(Deserialize)]
#[serde(default)]
pub struct AntinukeCfg {
    /// Master switch; off until a saved config turns it on.
    pub enabled: bool,
    /// Per action; actions left out use [`NukeAction::default_limit`]. An action this version
    /// does not know is skipped, not the whole section.
    #[serde(deserialize_with = "brim_logging::lenient::map")]
    pub limits: HashMap<NukeAction, LimitCfg>,
    /// Quarantined members are moved to this role (none: all roles removed).
    pub quarantine_role: Option<Id<RoleMarker>>,
    /// Watch member prunes.
    pub prune: bool,
    /// Punish anyone who touches a quarantined member (roles, nickname, timeout, kick, ban).
    pub quarantine_hold: bool,
    /// Quarantine whoever changes the vanity URL and put the old code back.
    pub vanity_guard: bool,
    /// Off: a reached limit is only logged, reverted and counted towards panic mode.
    pub auto_quarantine: bool,
    /// The main roles change only with the owner's bar while on. Their dangerous permissions
    /// are always guarded since 11-15, as every role's.
    pub monitor_public_roles: bool,
    /// May change a quarantined member's roles.
    pub quarantine_whitelist: Vec<Id<UserMarker>>,
    /// Channels a bot creates or deletes in these categories never count (ticket bots), unless the
    /// channel moved in from elsewhere in the last hour.
    pub ticket_categories: Vec<Id<ChannelMarker>>,
}

impl Default for AntinukeCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            limits: HashMap::new(),
            quarantine_role: None,
            prune: false,
            quarantine_hold: true,
            vanity_guard: false,
            auto_quarantine: true,
            monitor_public_roles: false,
            quarantine_whitelist: vec![],
            ticket_categories: vec![],
        }
    }
}

/// One anti-nuke filter card; unset numbers take the action's defaults.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(default)]
pub struct LimitCfg {
    pub enabled: bool,
    pub per_minute: Option<u32>,
    pub per_hour: Option<u32>,
    /// Heat % per action towards panic mode's heat algorithm.
    pub heat: Option<u32>,
}

impl Default for LimitCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            per_minute: None,
            per_hour: None,
            heat: None,
        }
    }
}

impl LimitCfg {
    /// Minute cap, hour cap and heat % of card `a`, unset ones taking the defaults. Values saved
    /// before the limits existed are pulled inside them, and a saved heat of 0 means the default.
    pub fn values(&self, a: NukeAction) -> Option<(u32, u32, u32)> {
        use crate::limits::{NUKE_HEAT, NUKE_HOUR, NUKE_MINUTE};
        let fit = |v: Option<u32>, (lo, hi): (i64, i64)| v.map(|v| v.clamp(lo as u32, hi as u32));
        let d = a.default_limit();
        Some((
            fit(self.per_minute, NUKE_MINUTE).or(d.map(|d| d.0))?,
            fit(self.per_hour, NUKE_HOUR).or(d.map(|d| d.1))?,
            fit(self.heat.filter(|h| *h > 0), NUKE_HEAT)
                .or(d.map(|d| d.2))
                .unwrap_or(0),
        ))
    }
}

impl AntinukeCfg {
    /// The watched limit and heat % of `a`, None when it is not watched.
    pub fn limit(&self, a: NukeAction) -> Option<(Limit, u32)> {
        if (a == NukeAction::Prune && !self.prune)
            || (a.opt_in() && !self.limits.contains_key(&a.card()))
        {
            return None;
        }
        let c = self.limits.get(&a.card()).copied().unwrap_or_default();
        let (per_minute, per_hour, heat) = c.values(a)?;
        c.enabled.then_some((
            Limit {
                per_minute,
                per_hour,
            },
            heat,
        ))
    }

    /// The cards with a minute and hour cap; a heat-only card counts in `count_at` alone.
    pub fn watched(&self) -> HashMap<NukeAction, Limit> {
        NukeAction::ALL
            .into_iter()
            .filter(|a| !a.heat_only())
            .filter_map(|a| self.limit(a).map(|(l, _)| (a, l)))
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(default)]
pub struct AutomodCfg {
    pub enabled: bool,
    /// Off: only the link and word filters run.
    pub anti_spam: bool,
    /// Off: messages from webhooks are left alone.
    pub monitor_webhooks: bool,
    #[serde(deserialize_with = "heat_cfg")]
    pub heat: HeatCfg,
    pub strike: StrikeCfg,
    pub auto_lockdown: AutoLockdownCfg,
    #[serde(deserialize_with = "slowmode_cfg")]
    pub auto_slowmode: AutoSlowmodeCfg,
    pub filters: FiltersCfg,
}

/// A new prefix: 1-32 characters without whitespace (like the prefix panel).
pub fn prefix_ok(p: &str) -> bool {
    (1..=32).contains(&p.chars().count()) && !p.contains(char::is_whitespace)
}

fn prefix<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let p = String::deserialize(d)?;
    // Only a blank prefix falls back: "" made every message a command. A stored "tb " still works.
    Ok(if p.trim().is_empty() { "t!".into() } else { p })
}

/// Max heat and decay inside their limits, so heat never turns NaN or stops cooling.
fn heat_cfg<'de, D: serde::Deserializer<'de>>(d: D) -> Result<HeatCfg, D::Error> {
    HeatCfg::deserialize(d).map(HeatCfg::clamped)
}

/// Auto slowmode inside its limits. A value of the wrong type (say -1) gives this card its defaults,
/// not the whole automod section.
fn slowmode_cfg<'de, D: serde::Deserializer<'de>>(d: D) -> Result<AutoSlowmodeCfg, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value::<AutoSlowmodeCfg>(v)
        .unwrap_or_default()
        .clamped())
}

impl Default for AutomodCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            anti_spam: true,
            monitor_webhooks: true,
            heat: HeatCfg::default(),
            strike: StrikeCfg::default(),
            auto_lockdown: AutoLockdownCfg::default(),
            auto_slowmode: AutoSlowmodeCfg::default(),
            filters: FiltersCfg::default(),
        }
    }
}

/// Locks the server on mention spam, a strike count or (with `joinraid.lock`) a join raid.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct AutoLockdownCfg {
    pub enabled: bool,
    pub mentions: bool,
    /// That many mentions in the window lock the server.
    pub mention_count: usize,
    pub mention_window_ms: i64,
    /// Lock when a member reaches this strike number.
    pub strikes: Option<u32>,
}

impl Default for AutoLockdownCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            mentions: true,
            mention_count: 50,
            mention_window_ms: 3_000,
            strikes: None,
        }
    }
}

/// What a tripped filter does; the heat weights stay in [`HeatCfg`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterAction {
    #[default]
    Delete,
    Warn,
    Timeout,
    Kick,
    Ban,
}

/// One automod filter. `action` None leaves the decision to the strike system.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct FilterCfg {
    pub enabled: bool,
    pub action: Option<FilterAction>,
    /// Timeout or ban length; ignored by the other actions.
    pub duration_secs: Option<i64>,
}

impl Default for FilterCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            action: None,
            duration_secs: None,
        }
    }
}

/// Per-filter switches (Filters page). Defaults in `FILTERS_OFF_BY_DEFAULT`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct FiltersCfg {
    pub regular: FilterCfg,
    pub similar: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub emojis: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub newlines: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub chars_upper: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub chars_lower: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub zalgo: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub suspicion: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub inactive_channel: FilterCfg,
    /// Messages after a quiet spell that count as the inactive-channel burst.
    pub inactive_messages: u32,
    /// Hours without a message before a channel counts as inactive.
    pub inactive_hours: u32,
    /// Mentions tab: `mentions` holds the action, the rest only switch their heat.
    pub mentions: FilterCfg,
    pub mention_users: FilterCfg,
    pub mention_roles: FilterCfg,
    pub mention_everyone: FilterCfg,
    /// Mentions a member may send per hour before the mentions filter trips.
    pub mention_hour_cap: usize,
    /// Attachments tab, same split as mentions.
    pub attachments: FilterCfg,
    pub embeds: FilterCfg,
    pub images: FilterCfg,
    pub files: FilterCfg,
    pub attach_links: FilterCfg,
    pub stickers: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub invites: FilterCfg,
    /// Phishing and scam sites.
    #[serde(deserialize_with = "off_filter")]
    pub bad_sites: FilterCfg,
    /// Fake Nitro and Steam links.
    #[serde(deserialize_with = "off_filter")]
    pub scam_links: FilterCfg,
    #[serde(deserialize_with = "off_filter")]
    pub nsfw_links: FilterCfg,
    /// Images checked by the self-hosted NSFW model (NSFW card).
    #[serde(deserialize_with = "off_filter")]
    pub nsfw_images: FilterCfg,
    /// Score in percent from which an image counts as NSFW (`limits::NSFW_THRESHOLD`).
    pub nsfw_threshold: u32,
    /// Follow link shorteners before checking.
    pub nsfw_crawl_shorteners: bool,
    pub words: WordsCfg,
    #[serde(deserialize_with = "off_filter")]
    pub links: FilterCfg,
    /// Domains for the link blacklist.
    pub blacklisted_domains: Vec<String>,
}

impl Default for FiltersCfg {
    fn default() -> Self {
        let on = FilterCfg::default;
        let off = || FilterCfg {
            enabled: false,
            ..FilterCfg::default()
        };
        Self {
            regular: on(),
            similar: on(),
            emojis: off(),
            newlines: off(),
            chars_upper: off(),
            chars_lower: off(),
            zalgo: off(),
            suspicion: off(),
            inactive_channel: off(),
            inactive_messages: 10,
            inactive_hours: 1,
            mentions: on(),
            mention_users: on(),
            mention_roles: on(),
            mention_everyone: on(),
            mention_hour_cap: 20,
            attachments: on(),
            embeds: on(),
            images: on(),
            files: on(),
            attach_links: on(),
            stickers: on(),
            invites: off(),
            bad_sites: off(),
            scam_links: off(),
            nsfw_links: off(),
            nsfw_images: off(),
            nsfw_threshold: 80,
            nsfw_crawl_shorteners: false,
            words: WordsCfg::default(),
            links: off(),
            blacklisted_domains: vec![],
        }
    }
}

/// A filter that starts off: a stored object without `enabled` (say only an action) stays off.
fn off_filter<'de, D: serde::Deserializer<'de>>(d: D) -> Result<FilterCfg, D::Error> {
    let mut v = serde_json::Value::deserialize(d)?;
    if let Some(o) = v.as_object_mut() {
        o.entry("enabled").or_insert(false.into());
    }
    serde_json::from_value(v).map_err(serde::de::Error::custom)
}

/// The filters `FiltersCfg::default` starts off (config keys, `words.filter` for the word blacklist),
/// for the dashboard's defaults. A test keeps the two in step.
pub const FILTERS_OFF_BY_DEFAULT: &[&str] = &[
    "emojis",
    "newlines",
    "chars_upper",
    "chars_lower",
    "zalgo",
    "suspicion",
    "inactive_channel",
    "invites",
    "bad_sites",
    "scam_links",
    "nsfw_links",
    "nsfw_images",
    "words.filter",
    "links",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct WordsCfg {
    #[serde(deserialize_with = "off_filter")]
    pub filter: FilterCfg,
    /// Matched as whole words.
    pub strict: Vec<String>,
    /// Matched anywhere in a word.
    pub wildcard: Vec<String>,
    /// Regular expressions, case-insensitive, matched anywhere in the message.
    pub regex: Patterns,
    pub premade_profane: bool,
    pub premade_vulgar: bool,
    pub premade_racist: bool,
    /// Keep the words in a native Discord AutoMod rule instead.
    pub use_discord_automod: bool,
}

impl Default for WordsCfg {
    fn default() -> Self {
        Self {
            // Word blacklist starts off and warns.
            filter: FilterCfg {
                enabled: false,
                action: Some(FilterAction::Warn),
                ..FilterCfg::default()
            },
            strict: vec![],
            wildcard: vec![],
            regex: Patterns::default(),
            premade_profane: false,
            premade_vulgar: false,
            premade_racist: false,
            use_discord_automod: false,
        }
    }
}

/// Regex word patterns, compiled once when the config loads, so a bad one fails the dashboard save.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(try_from = "Vec<String>")]
pub struct Patterns {
    list: Vec<String>,
    set: regex::RegexSet,
}

impl TryFrom<Vec<String>> for Patterns {
    type Error = regex::Error;

    fn try_from(list: Vec<String>) -> Result<Self, Self::Error> {
        let set = regex::RegexSetBuilder::new(&list)
            .case_insensitive(true)
            // Admin-supplied patterns: cap the compiled size (matching is linear time already).
            .size_limit(1 << 20)
            .build()?;
        Ok(Self { list, set })
    }
}

impl Patterns {
    /// Number of patterns that match `text`.
    pub fn hits(&self, text: &str) -> usize {
        self.set.matches(text).iter().count()
    }

    pub fn as_slice(&self) -> &[String] {
        &self.list
    }
}

/// Automod whitelists, one per tab. The top-level lists are the Spam tab, which skips automod entirely.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct WhitelistCfg {
    #[serde(flatten)]
    pub spam: WhitelistList,
    pub invites: WhitelistList,
    /// @user and @role mentions.
    pub mentions: WhitelistList,
    /// @everyone and @here.
    pub everyone: WhitelistList,
    /// Partnering channels static: spam, @everyone and invites are allowed there.
    pub partner_channels: Vec<Id<ChannelMarker>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct WhitelistList {
    pub users: Vec<Id<UserMarker>>,
    /// Stored by older versions; never read. A role exempts nobody: trust is by user id and
    /// permits (owner rule). The Overview row `exempt_roles` moves their members once.
    pub roles: Vec<Id<RoleMarker>>,
    pub channels: Vec<Id<ChannelMarker>>,
    pub categories: Vec<Id<ChannelMarker>>,
    pub webhooks: Vec<Id<WebhookMarker>>,
}

impl WhitelistList {
    /// A message whose author, channel, category or webhook is listed. Roles are not read.
    pub fn covers(
        &self,
        user: Id<UserMarker>,
        channel: Id<ChannelMarker>,
        category: Option<Id<ChannelMarker>>,
        webhook: Option<Id<WebhookMarker>>,
    ) -> bool {
        self.users.contains(&user)
            || self.channels.contains(&channel)
            || category.is_some_and(|c| self.categories.contains(&c))
            || webhook.is_some_and(|w| self.webhooks.contains(&w))
    }
}

/// The first entry in `v` that the bot's lenient readers would skip (an unknown key or enum
/// value, or a bad value under a known key), as `path: entry`, unless `old` (the stored
/// section) holds it unchanged. Writes refuse it, so only data a newer version wrote is skipped,
/// and an older version can still save a section that holds such data (R-h3 2).
pub fn unreadable_entry(section: &str, v: &Value, old: &Value) -> Option<String> {
    use brim_automod::discord_api::BotAction;
    use brim_logging::{LogEvent, LogKind};
    use serde::de::DeserializeOwned;
    fn bad<T: DeserializeOwned>(x: &Value) -> bool {
        serde_json::from_value::<T>(x.clone()).is_err()
    }
    fn items(a: &Value) -> Vec<Value> {
        a.as_array().cloned().unwrap_or_default()
    }
    /// A map entry whose key or value does not parse and is not stored as it is.
    fn entry<K: DeserializeOwned, V: DeserializeOwned>(
        path: &str,
        m: &Value,
        old: &Value,
    ) -> Option<String> {
        m.as_object()?
            .iter()
            .find(|(k, x)| {
                (bad::<K>(&Value::String((*k).clone())) || bad::<V>(x))
                    && old.get(k.as_str()) != Some(x)
            })
            .map(|(k, x)| format!("{path}: {k} = {x}"))
    }
    /// A list item that does not parse and is not in the stored list.
    fn item<T: DeserializeOwned>(path: &str, l: &Value, old: &Value) -> Option<String> {
        let old = items(old);
        items(l)
            .into_iter()
            .find(|x| bad::<T>(x) && !old.contains(x))
            .map(|x| format!("{path}: {x}"))
    }
    match section {
        "antinuke" => entry::<NukeAction, LimitCfg>("limits", &v["limits"], &old["limits"]),
        "logging" => {
            let old_custom = Value::Array(
                items(&old["custom"])
                    .iter()
                    .flat_map(|c| items(&c["events"]))
                    .collect(),
            );
            entry::<LogKind, Id<ChannelMarker>>("routes", &v["routes"], &old["routes"])
                .or_else(|| {
                    entry::<LogEvent, Id<ChannelMarker>>("events", &v["events"], &old["events"])
                })
                .or_else(|| {
                    items(&v["custom"])
                        .iter()
                        .find_map(|c| item::<LogEvent>("custom events", &c["events"], &old_custom))
                })
        }
        "raider_list" => item::<RaiderCategory>("categories", &v["categories"], &old["categories"]),
        "discord_automod" => v["actions"].as_object().and_then(|m| {
            m.iter().find_map(|(rule, a)| {
                let was = &old["actions"][rule];
                if !a.is_array() {
                    return (was != a).then(|| format!("actions: {rule} = {a}"));
                }
                item::<BotAction>(&format!("actions of {rule}"), a, was)
            })
        }),
        _ => None,
    }
}

pub const MAX_APPEAL_FIELDS: usize = 6;

/// Sections the dashboard saves only after a TOTP step-up, so prefix panels may not edit them.
pub const SENSITIVE: &[&str] = &[
    "antinuke",
    "whitelist",
    "panic",
    "backups",
    "catch",
    "raider_list",
    "retention",
    "commands",
    "modules",
];

#[derive(Clone, Debug, Deserialize)]
pub struct AppealField {
    pub label: String,
    /// Multi-line answer.
    #[serde(default)]
    pub long: bool,
    #[serde(default = "yes")]
    pub required: bool,
    /// Non-empty makes it a choice question: single choice, or multi choice with `multi`.
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub multi: bool,
    /// Text length limits; the maximum defaults to 1000.
    #[serde(default)]
    pub min: Option<usize>,
    #[serde(default)]
    pub max: Option<usize>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct TicketsCfg {
    /// Members can DM the bot to open a ticket with this server.
    pub enabled: bool,
    /// Where ticket channels are created.
    pub category: Option<Id<ChannelMarker>>,
    /// Gets "New Ticket" and "Ticket Closed" logs.
    pub log_channel: Option<Id<ChannelMarker>>,
    /// The close log carries the whole conversation as a file.
    pub full_log: bool,
    /// Pinged in a new ticket; the @everyone role (the guild id) pings everyone.
    pub ping_roles: Vec<Id<RoleMarker>>,
    pub ping_here: bool,
    /// A new panel ticket pings the permit roles of Moderators and higher permits with Manage Tickets.
    pub ping_staff: bool,
    /// With the Staff role on: ping only the permit roles with Manage Tickets (the ping without
    /// the Staff role), not the Staff role.
    pub ping_staff_tickets_only: bool,
    /// Sent to the member when a ticket opens / closes; `{member.username}` `{member.id}` `{member}`.
    /// A panel ticket whose panel has no greeting gets `greeting` too; empty sends none.
    pub greeting: String,
    pub closing: String,
    /// Staff replies hide their name by default.
    pub anonymous: bool,
    /// Only `reply` reaches the member; other messages stay staff talk.
    pub command_only: bool,
    /// No new tickets; the reason is shown to members ("" = none given).
    pub paused: bool,
    pub pause_reason: String,
    /// Cannot open tickets or get replies.
    pub blocked: Vec<Id<UserMarker>>,
    pub snippets: Vec<Snippet>,
    /// Ticket types members open with a button in a server channel (Tickets 2.0).
    pub panels: Vec<TicketPanel>,
    /// Hours without a member or staff message before a ticket closes itself; 0 = never.
    pub auto_close_hours: u32,
    /// Ticket types: the member picks one when a ticket opens, or staff at claim.
    pub types: Vec<String>,
    /// Brim keeps #open-a-ticket in the ticket category with the panel message: members
    /// only see it and press the button. Off deletes it.
    pub public_channel: bool,
    /// The channel Brim made for `public_channel`. Only the bot writes it; a dashboard save
    /// keeps the stored value.
    pub public_channel_id: Option<Id<ChannelMarker>>,
    /// A panel with no preset type shows one button per type in `types`; the member picks the
    /// type by pressing it. Off: one button per panel.
    pub button_per_type: bool,
}

/// Most panels a server can have, and most answers a panel's form asks (one Discord modal).
pub const MAX_PANELS: usize = 12;
pub const MAX_PANEL_FIELDS: usize = 5;

/// A panel: one kind of ticket that opens a private channel the member and staff share.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct TicketPanel {
    /// Stable id in button ids and in `tickets.panel`: 1-32 of a-z, 0-9 and `-`.
    pub id: String,
    /// The button or menu label.
    pub name: String,
    pub enabled: bool,
    /// One emoji shown on the button, or empty.
    pub emoji: String,
    /// Button color: primary, secondary, success or danger.
    pub style: String,
    /// Shown under the name in the menu and in the panel message.
    pub description: String,
    /// Where its tickets go; the ticket category when unset.
    pub category: Option<Id<ChannelMarker>>,
    /// Tickets of this panel one member may have open at once (1-5).
    pub per_member: u8,
    /// Asked in a form before the ticket opens; the answers go only to the ticket channel.
    pub fields: Vec<AppealField>,
    /// Posted in the new ticket; `{member.username}` `{member.id}` `{member}`.
    pub greeting: String,
    /// Roles pinged in a new ticket; the bot skips @everyone (the server id).
    pub ping_roles: Vec<Id<RoleMarker>>,
    /// The ticket type its tickets start with (one of `tickets.types`); empty = the member picks.
    pub kind: String,
}

impl Default for TicketPanel {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            emoji: String::new(),
            style: "primary".into(),
            description: String::new(),
            category: None,
            per_member: 1,
            fields: vec![],
            greeting: String::new(),
            ping_roles: vec![],
            kind: String::new(),
        }
    }
}

/// Most ticket types: one Discord select menu.
pub const MAX_TICKET_TYPES: usize = 25;

/// Ticket snippets per server at most (the same as other per-server lists of 100).
pub const MAX_SNIPPETS: usize = 100;
/// Ticket blacklist entries per server at most.
pub const MAX_BLOCKED: usize = 1000;

/// Why a tickets section adds a snippet or a blacklist entry past its cap, or `None`. Only a new
/// entry (a snippet name or a user id `old` does not hold) is refused: lists already above the cap
/// keep working and can be edited or shortened. Callers run it on the locked config (bot) or in
/// the save's validation (dashboard), so the count and the write see the same version.
pub fn tickets_cap_problem(new: &Value, old: &Value) -> Option<String> {
    let (list, count, cap) = tickets_cap_hit(new, old)?;
    let what = match list {
        "snippets" => "snippets",
        _ => "blacklisted users",
    };
    Some(format!(
        "This server has {count} {what}, the most allowed is {cap}. Remove one first."
    ))
}

/// [`tickets_cap_problem`] as values: the list (`snippets` or `blocked`), its count and its cap.
/// The bot sends them in its own entry.
pub fn tickets_cap_hit(new: &Value, old: &Value) -> Option<(&'static str, usize, usize)> {
    let names = |v: &Value, key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| match key {
                "snippets" => x["name"].as_str().unwrap_or_default().to_string(),
                _ => x.as_str().map_or_else(|| x.to_string(), String::from),
            })
            .collect()
    };
    for (key, cap) in [("snippets", MAX_SNIPPETS), ("blocked", MAX_BLOCKED)] {
        let (now, before) = (names(new, key), names(old, key));
        if now.len() > cap && now.iter().any(|n| !before.contains(n)) {
            return Some((key, before.len().max(cap), cap));
        }
    }
    None
}

/// Most buttons on one panel message (Discord: 5 rows of 5) and options in one menu.
pub const MAX_PANEL_BUTTONS: usize = 25;

/// Why the ticket types cannot be saved, or `None`: 1-100 characters each (80 with `per_type`,
/// the button label limit), no repeats, a panel's preset type must be one of them, and with
/// `per_type` the buttons of the enabled panels must fit one panel message.
pub fn types_problem(types: &[String], panels: &[TicketPanel], per_type: bool) -> Option<String> {
    if types.len() > MAX_TICKET_TYPES {
        return Some(format!("at most {MAX_TICKET_TYPES} ticket types"));
    }
    let max = if per_type { 80 } else { 100 };
    let mut seen = std::collections::HashSet::new();
    for t in types {
        if !(1..=max).contains(&t.trim().chars().count()) || t.trim() != t {
            return Some(format!(
                "a ticket type needs 1-{max} characters, without spaces at the ends"
            ));
        }
        if !seen.insert(t.to_lowercase()) {
            return Some(format!("ticket type \"{t}\" is listed twice"));
        }
    }
    if let Some(p) = panels
        .iter()
        .find(|p| !p.kind.is_empty() && !types.contains(&p.kind))
    {
        return Some(format!(
            "panel \"{}\": its ticket type \"{}\" is not in the list",
            p.name, p.kind
        ));
    }
    let buttons: usize = if per_type {
        panels
            .iter()
            .filter(|p| p.enabled)
            .map(|p| button_types(p, types, true).len())
            .sum()
    } else {
        0
    };
    (buttons > MAX_PANEL_BUTTONS).then(|| {
        format!(
            "the panel message would have {buttons} buttons, the most is {MAX_PANEL_BUTTONS}: remove {} ticket types or panels",
            buttons - MAX_PANEL_BUTTONS
        )
    })
}

/// The label of the one default button (owner decision 2026-11-22), also the name of the default
/// panel and of a new panel.
pub const DEFAULT_BUTTON: &str = "Create ticket";

/// A stable short hash of a ticket type name for a button id (`tk2:open:<panel>:<hash>`), so a
/// button posted before the type list changed is refused.
pub fn type_hash(kind: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in kind.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// The buttons `p` shows: one per type when `per_type` is on, the panel has no preset type and
/// the server lists two or more types; else one with no type (`""`).
pub fn button_types<'a>(p: &TicketPanel, types: &'a [String], per_type: bool) -> Vec<&'a str> {
    if per_type && p.kind.is_empty() && types.len() >= 2 {
        types.iter().map(String::as_str).collect()
    } else {
        vec![""]
    }
}

/// The panel Brim adds when the public ticket channel needs one and the server has none (owner
/// decision 2026-11-22): one `DEFAULT_BUTTON`. It keeps the id `support`, so buttons posted before
/// still open tickets.
pub fn default_panels() -> Value {
    serde_json::json!([
        {"id": "support", "name": DEFAULT_BUTTON, "enabled": true, "style": "primary", "emoji": "{e:ticket}"},
    ])
}

/// The panel message text the Tickets tab wrote with the default panel until 4055 moved the
/// panel message to the `ticket.panel` entry (Default messages): 4055 leaves a server still on it
/// with the new default look. Earlier migrations check against it.
pub const DEFAULT_PANEL_TEXT: &str = "{e:ticket} Need help from staff? Press a button below.
{e:lock} Brim opens a private channel that only you and the staff can see.
{e:note} **Have ready:** names, message links or screenshots.
{e:clock} One ticket per issue. Staff answer as soon as they can.";

/// The greeting a new ticket sends the member until the server writes its own (owner decision
/// 2026-11-21). Migration 4055 wrote it where the greeting was empty.
pub const DEFAULT_GREETING: &str = "Hi {member}, thanks for reaching out. Tell us what you need, with as much detail as you can, and staff will answer here soon.";

/// The panel message: the `ticket.panel` entry (`look`, the server's edited version, else the
/// default) with a line per panel that has a description as `{text}`, and a button per panel or
/// one menu. With `per_type` (the switch on) a panel with no preset type shows a button or menu
/// option per entry of `types` (`button_types`). The custom ids are the ones
/// `bot/src/tickets/panel.rs` reads.
pub fn panel_message(
    panels: &[&TicketPanel],
    menu: bool,
    look: Option<&Value>,
    types: &[String],
    per_type: bool,
) -> Value {
    use brim_logging::emoji;
    use serde_json::json;
    let text: Vec<String> = panels
        .iter()
        .filter(|p| !p.description.is_empty())
        .map(|p| format!("**{}**\n{}", p.name, p.description))
        .collect();
    let vars = [("text", text.join("\n\n"))];
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut m = brim_logging::botmsg::render("ticket.panel", look, &vars);
    // An edited version with only text has no embed to carry the buttons' message: the default.
    if m["embeds"].as_array().is_none_or(Vec::is_empty) {
        m = brim_logging::botmsg::render("ticket.panel", None, &vars);
    }
    // Brim emoji tags (`{e:name}`) work in the entry and in panel descriptions; an empty `{text}`
    // leaves blank lines at the end.
    for e in m["embeds"].as_array_mut().into_iter().flatten() {
        for p in ["/title", "/description", "/footer/text"] {
            if let Some(v) = e.pointer_mut(p) {
                if let Some(t) = v.as_str() {
                    *v = json!(emoji::fill(t).trim());
                }
            }
        }
    }
    // (panel, type or "") per button; the type is part of the id and the label.
    let items: Vec<(&TicketPanel, &str)> = panels
        .iter()
        .flat_map(|p| {
            button_types(p, types, per_type)
                .into_iter()
                .map(move |k| (*p, k))
        })
        .collect();
    let id_of = |p: &TicketPanel, k: &str| {
        if k.is_empty() {
            p.id.clone()
        } else {
            format!("{}:{}", p.id, type_hash(k))
        }
    };
    let rows: Vec<Value> = if menu {
        let options: Vec<Value> = items
            .iter()
            .map(|(p, k)| {
                let name = if k.is_empty() { &p.name } else { *k };
                let mut o = json!({"label": name, "value": id_of(p, k)});
                if !p.description.is_empty() {
                    o["description"] = json!(p.description.chars().take(100).collect::<String>());
                }
                o
            })
            .collect();
        vec![json!({"type": 1, "components": [
            {"type": 3, "custom_id": "tk2:menu", "options": options}
        ]})]
    } else {
        items
            .chunks(5)
            .map(|row| {
                let buttons: Vec<Value> = row
                    .iter()
                    .map(|(p, k)| {
                        let style = match p.style.as_str() {
                            "secondary" => 2,
                            "success" => 3,
                            "danger" => 4,
                            _ => 1,
                        };
                        let name = if k.is_empty() { &p.name } else { *k };
                        let mut b = json!({"type": 2, "style": style, "label": name,
                            "custom_id": format!("tk2:open:{}", id_of(p, k))});
                        // A Brim emoji that is not uploaded leaves the button without one.
                        match emoji::tag_name(&p.emoji) {
                            Some(n) => {
                                if let Some(id) = emoji::id_of(n) {
                                    b["emoji"] = json!({"id": id.to_string(), "name": n});
                                }
                            }
                            None if !p.emoji.is_empty() => b["emoji"] = json!({"name": p.emoji}),
                            None => {}
                        }
                        b
                    })
                    .collect();
                json!({"type": 1, "components": buttons})
            })
            .collect()
    };
    json!({
        "content": m["content"],
        "embeds": m["embeds"].clone(),
        "components": rows,
        "allowed_mentions": {"parse": []},
    })
}

/// Why a panel list cannot be saved, or `None`. Discord's own limits plus Brim's caps.
pub fn panels_problem(panels: &[TicketPanel]) -> Option<String> {
    if panels.len() > MAX_PANELS {
        return Some(format!("at most {MAX_PANELS} ticket panels"));
    }
    let mut ids = std::collections::HashSet::new();
    for p in panels {
        let n = &p.name;
        let id_ok = (1..=32).contains(&p.id.len())
            && p.id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !id_ok || !ids.insert(p.id.as_str()) {
            return Some(format!(
                "panel \"{n}\" needs a unique id of 1-32 characters: a-z, 0-9 and -"
            ));
        }
        if !(1..=80).contains(&n.chars().count()) {
            return Some("a panel needs a name of 1-80 characters".into());
        }
        if p.description.chars().count() > 100 || p.greeting.chars().count() > 2000 {
            return Some(format!(
                "panel \"{n}\": description up to 100 and greeting up to 2000 characters"
            ));
        }
        // shortcut: refuses text, :names: and server emoji but not every non-emoji symbol; use a
        // Unicode emoji table if Discord still refuses posts.
        let e = &p.emoji;
        let emoji_ok = e.is_empty()
            || brim_logging::emoji::tag_name(e).is_some()
            || (e.chars().count() <= 8
                && !e.is_ascii()
                && e.chars().all(|c| {
                    !c.is_whitespace()
                        && (!c.is_ascii() || c.is_ascii_digit() || c == '#' || c == '*')
                }));
        if !emoji_ok {
            return Some(format!(
                "panel \"{n}\": the emoji must be one standard emoji or a Brim emoji like {{e:ticket}} (server emoji are not supported)"
            ));
        }
        if !["primary", "secondary", "success", "danger"].contains(&p.style.as_str()) {
            return Some(format!("panel \"{n}\": unknown button color"));
        }
        if !(1..=5).contains(&p.per_member) {
            return Some(format!("panel \"{n}\": 1-5 open tickets per member"));
        }
        if p.ping_roles.len() > 10 {
            return Some(format!("panel \"{n}\": at most 10 ping roles"));
        }
        if p.fields.len() > MAX_PANEL_FIELDS {
            return Some(format!(
                "panel \"{n}\": at most {MAX_PANEL_FIELDS} form fields"
            ));
        }
        for f in &p.fields {
            let max = f.max.unwrap_or(1000);
            let label_ok = (1..=45).contains(&f.label.chars().count());
            if !label_ok
                || !f.options.is_empty()
                || max == 0
                || max > 1024
                || f.min.unwrap_or(0) > max
            {
                return Some(format!(
                    "panel \"{n}\": each form field needs a label of 1-45 characters, no choices, and a length of at most 1024"
                ));
            }
        }
    }
    None
}

#[derive(Clone, Debug, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub author: Option<Id<UserMarker>>,
}

impl Default for TicketsCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            category: None,
            log_channel: None,
            full_log: true,
            ping_roles: vec![],
            ping_here: false,
            ping_staff: true,
            ping_staff_tickets_only: false,
            greeting: DEFAULT_GREETING.into(),
            closing: String::new(),
            anonymous: false,
            command_only: false,
            paused: false,
            pause_reason: String::new(),
            blocked: vec![],
            snippets: vec![],
            panels: vec![],
            auto_close_hours: 0,
            types: ["Report", "Appeal", "Question", "Other"]
                .map(String::from)
                .to_vec(),
            public_channel: true,
            public_channel_id: None,
            button_per_type: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct AppealsCfg {
    pub enabled: bool,
    /// At most [`MAX_APPEAL_FIELDS`]; the bot ignores the rest.
    pub fields: Vec<AppealField>,
    /// Where new appeals are posted for staff.
    pub channel: Option<Id<ChannelMarker>>,
}

impl Default for AppealsCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            fields: vec![AppealField {
                label: "Appeal Message".into(),
                long: true,
                required: true,
                options: vec![],
                multi: false,
                min: None,
                max: None,
            }],
            channel: None,
        }
    }
}

/// Miscellaneous page.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MiscCfg {
    /// A blank prefix (from import or MCP) reads as `t!`.
    #[serde(deserialize_with = "prefix")]
    pub prefix: String,
    pub use_slash: bool,
    /// Short embeds instead of full ones.
    pub compact_replies: bool,
    /// Delete the bot's own replies after this many seconds.
    pub delete_replies_secs: Option<u32>,
    pub enforce_quarantine_perms: bool,
    /// Ignore commands from members without a permit.
    pub ignore_no_permit: bool,
    /// Take dangerous-permission roles from members who have no permit. Off until a save turns it
    /// on: the wizard switches it on for the owner and Overview offers it, and both list who loses
    /// roles first, so no server starts removing roles unseen.
    pub enforce_permits: bool,
    /// Shown instead of the default appeal line in ban and timeout DMs.
    pub ban_appeal_message: String,
    pub confirm: bool,
    pub always_dm: bool,
    /// Delete the staff member's prefix command message.
    pub delete_invocation: bool,
    pub hide_staff_in_dms: bool,
    pub default_timeout_secs: u64,
    pub ban_purge_days: u8,
    pub show_reason_on_auto: bool,
    /// Delete the member's last 100 messages when automod punishes them.
    pub purge_last_100: bool,
    /// No effect: staff are only ever owners, Extra Owners, Trusted Admins and permit holders,
    /// never a role's Discord permissions. Still read so old configs parse; not shown anywhere.
    pub staff_only_permitted: bool,
    pub warn_base_points: i32,
    /// Apply `warn_actions` once a member's warn points reach them.
    pub auto_warn_actions: bool,
    pub warn_actions: Vec<crate::warn::Threshold>,
    /// Take the main roles away while a member is timed out.
    pub remove_main_roles_on_timeout: bool,
    /// Give a member's roles back when they rejoin (safe roles only, see `bot::sticky`).
    pub sticky_roles: bool,
    /// Roles sticky roles never gives back.
    pub sticky_never: Vec<Id<RoleMarker>>,
    /// Strip hoisting symbols from the front of names on join and on every name change.
    pub auto_dehoist: bool,
    /// Invite Tracking: track which invite each joiner used (needs Manage Server).
    pub invite_tracking: bool,
    /// Anyone may see `/invites top`; off: only members with a permit.
    pub invites_public_top: bool,
    /// Default and longest life of a `/invites create` link (0 = never, and members may pick any; Discord's max is 7 days).
    pub invite_max_age_secs: u32,
    /// Default and most uses of a `/invites create` link (0 = no limit, and members may pick any; Discord's max is 100).
    pub invite_max_uses: u32,
}

impl Default for MiscCfg {
    fn default() -> Self {
        Self {
            prefix: "t!".into(),
            use_slash: true,
            compact_replies: false,
            delete_replies_secs: None,
            enforce_quarantine_perms: true,
            ignore_no_permit: false,
            enforce_permits: false,
            ban_appeal_message: String::new(),
            confirm: true,
            always_dm: true,
            delete_invocation: false,
            hide_staff_in_dms: false,
            default_timeout_secs: 0,
            ban_purge_days: 7,
            show_reason_on_auto: true,
            purge_last_100: false,
            staff_only_permitted: false,
            warn_base_points: 5,
            auto_warn_actions: true,
            warn_actions: vec![],
            remove_main_roles_on_timeout: true,
            sticky_roles: false,
            sticky_never: vec![],
            auto_dehoist: false,
            invite_tracking: true,
            invites_public_top: true,
            invite_max_age_secs: 86_400,
            invite_max_uses: 0,
        }
    }
}

impl MiscCfg {
    /// Discord allows timeouts up to 28 days; 0 means the maximum ("permanent").
    pub fn timeout_secs(&self) -> u64 {
        match self.default_timeout_secs {
            0 => 28 * 86_400,
            n => n.min(28 * 86_400),
        }
    }

    /// Discord allows purging up to 7 days of messages on a ban.
    pub fn purge_secs(&self) -> u32 {
        u32::from(self.ban_purge_days.min(7)) * 86_400
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct LockdownCfg {
    /// Roles that lose Send Messages in a server lock; empty means @everyone.
    pub main_roles: Vec<Id<RoleMarker>>,
}

/// The whole `verification` section as the bot reads it; the dashboard validates saves with it too.
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct VerificationCfg {
    pub enabled: bool,
    /// Where `verifysetup` posts the button.
    pub channel: Option<Id<ChannelMarker>>,
    /// Roles handed out on a pass.
    pub roles: Vec<Id<RoleMarker>>,
    #[serde(flatten)]
    pub verify: brim_automod::join::VerifyCfg,
}

/// Panic mode: trips when `executors` distinct accounts are quarantined within `window_secs`.
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct PanicCfg {
    /// 0 turns panic mode off.
    pub executors: usize,
    pub window_secs: u64,
    pub duration_secs: u64,
    pub lock_mod_commands: bool,
    pub warned_roles: Vec<Id<RoleMarker>>,
    pub unlock_when_ending: bool,
    /// Also trip on automod heat, not only on anti-nuke quarantines.
    pub heat_algorithm: bool,
    /// Distinct accounts that must hit the heat limit within `window_secs` to trip it.
    pub heat_executors: usize,
    /// Fraction of a staff member's anti-nuke heat lost per second (0.01 = 1%/s).
    pub heat_decay: f64,
    /// Categories panic mode leaves unlocked.
    pub whitelisted_categories: Vec<Id<ChannelMarker>>,
    pub lock_on_trigger: bool,
}

impl Default for PanicCfg {
    fn default() -> Self {
        Self {
            executors: 0,
            window_secs: 600,
            duration_secs: 60,
            lock_mod_commands: true,
            warned_roles: vec![],
            unlock_when_ending: true,
            heat_algorithm: false,
            heat_executors: 3,
            heat_decay: 0.01,
            whitelisted_categories: vec![],
            lock_on_trigger: true,
        }
    }
}

pub use brim_antinuke::{verification_shape, QUARANTINE_DENY, VERIFY_ALLOW, VERIFY_DENY};

/// Server backups: off until a guild turns on `enabled`; then the 3-hour auto backup runs. The rest are the
/// restore filters (`anr`), used by anti-nuke reverts and panic mode, never by `/backup load`.
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct BackupsCfg {
    /// Allows taking backups at all, manual and automatic.
    pub enabled: bool,
    /// Backups kept per guild; older ones are dropped on save.
    pub max_images: i64,
    /// Hours between automatic backups.
    pub interval_hours: u64,
    pub auto: bool,
    pub restore: bool,
    pub restore_roles: bool,
    pub delete_roles: bool,
    pub restore_channels: bool,
    pub delete_channels: bool,
    pub delete_webhooks: bool,
}

impl Default for BackupsCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            max_images: 10,
            interval_hours: 3,
            auto: true,
            restore: true,
            restore_roles: true,
            delete_roles: true,
            restore_channels: true,
            delete_channels: true,
            delete_webhooks: true,
        }
    }
}

/// How many days each kind of record is kept in a server (security review, owner decision 2026-11-17):
/// 0 keeps it until staff delete it. The bot's hourly sweep (`bot::retention`) deletes older rows.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RetentionCfg {
    pub cases_days: u32,
    /// Verified proof only: the copies of members' messages the bot stored on cases.
    pub proofs_days: u32,
    pub notes_days: u32,
    pub reports_days: u32,
    pub appeals_days: u32,
    pub tickets_days: u32,
    /// Older versions go; the current one and the newest 200 cap stay as they are.
    pub config_history_days: u32,
    /// Decided and collected form responses, from the decision (or the send); open ones stay.
    pub forms_days: u32,
}

impl Default for RetentionCfg {
    fn default() -> Self {
        Self {
            cases_days: 0,
            // Message text, like a report's excerpt.
            proofs_days: 365,
            notes_days: 0,
            reports_days: 365,
            appeals_days: 365,
            tickets_days: 365,
            config_history_days: 0,
            forms_days: 365,
        }
    }
}

/// The shortest limit a server may set, so a typo of 1 does not wipe its history within the hour.
pub const RETENTION_MIN_DAYS: u32 = 30;

impl RetentionCfg {
    /// (field, days) of every limit, for the range check and the sweep.
    pub fn days(&self) -> [(&'static str, u32); 8] {
        [
            ("cases_days", self.cases_days),
            ("proofs_days", self.proofs_days),
            ("notes_days", self.notes_days),
            ("reports_days", self.reports_days),
            ("appeals_days", self.appeals_days),
            ("tickets_days", self.tickets_days),
            ("config_history_days", self.config_history_days),
            ("forms_days", self.forms_days),
        ]
    }

    /// A limit from 1 to 29 days is refused: 0 (keep) or at least [`RETENTION_MIN_DAYS`].
    pub fn check(&self) -> Result<(), String> {
        match self
            .days()
            .iter()
            .find(|(_, d)| (1..RETENTION_MIN_DAYS).contains(d))
        {
            Some((k, _)) => Err(format!(
                "{k} must be 0 (keep) or at least {RETENTION_MIN_DAYS} days"
            )),
            None => Ok(()),
        }
    }
}

/// What the catch channel does to a writer; `Log` is the test mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub enum CatchAction {
    #[default]
    Ban,
    Softban,
    Kick,
    Timeout,
    Log,
}

/// Master switches of the modules without one of their own. A missing switch is on, so a server
/// keeps what it does today. Off keeps every setting below it; the bot only stops acting on them.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ModulesCfg {
    /// Join Gate, Join Raid (with voice raids), Verification and the Raider List.
    pub server_joins: bool,
    /// The moderation commands (not lock, unlock and unquarantine), member reports and the catch
    /// channel. Anti Nuke and Auto Mod still punish and record cases.
    pub moderation: bool,
    /// Triggers, sticky messages, "every N messages" and clock schedules, tags, feeds and giveaways.
    pub messages: bool,
    /// Event logs. Security and moderation logs still post (`LogKind::ignorable`).
    pub logging: bool,
}

impl Default for ModulesCfg {
    fn default() -> Self {
        Self {
            server_joins: true,
            moderation: true,
            messages: true,
            logging: true,
        }
    }
}

/// The module a part's `enabled` switch belongs to, for a part that has a module switch.
pub fn module_of(section: &str) -> Option<&'static str> {
    match section {
        "joingate" | "joinraid" | "verification" | "raider_list" => Some("server_joins"),
        "catch" => Some("moderation"),
        "giveaways" => Some("messages"),
        _ => None,
    }
}

/// The module a write must turn on with its part: the part goes from off (`before`) to on (`after`)
/// while its module is off. The dashboard and the bot's settings panel both use it.
pub fn module_to_turn_on(
    section: &str,
    before: &Value,
    after: &Value,
    modules: &Value,
) -> Option<&'static str> {
    module_of(section)
        .filter(|m| before["enabled"] != true && after["enabled"] == true && modules[*m] == false)
}

/// Honeypot channel: hacked accounts spam every channel, so anyone writing here is punished.
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct CatchCfg {
    pub enabled: bool,
    pub channel: Option<Id<ChannelMarker>>,
    pub action: CatchAction,
    /// DM the member first (with the appeal link for bans and timeouts).
    pub dm: bool,
    /// Move the channel back into place when it is dragged (set by the dashboard's setup): the
    /// top of the server, or the bottom of `category` when that is set.
    pub keep_top: bool,
    /// The Information category Auto Setup made; the channel is kept at its bottom (owner decision
    /// 2026-11-16).
    pub category: Option<Id<ChannelMarker>>,
    /// Stored by older versions; never read (see [`WhitelistList::roles`]).
    pub roles: Vec<Id<RoleMarker>>,
    /// Never punished, besides bots, the Owner, Extra Owners and Trusted Admins.
    pub users: Vec<Id<UserMarker>>,
}

/// Giveaways (part of the Messages module): the switch and what a new giveaway starts with.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GiveawaysCfg {
    pub enabled: bool,
    pub button_label: String,
    pub button_emoji: String,
    /// Members with one of these roles never enter a giveaway made with the defaults.
    pub blocked_roles: Vec<Id<RoleMarker>>,
    pub min_account_age_days: u32,
    pub min_member_hours: u32,
    /// Members a join raid caught cannot enter.
    pub block_raid_joiners: bool,
    /// Hours a winner has to claim the prize.
    pub claim_hours: u32,
    /// The sponsor's profile picture in the top right of the default giveaway message. Off until
    /// the server turns it on (owner decision 2026-11-23).
    pub sponsor_picture: bool,
    /// The picture at the bottom of the default giveaway message: `a`, `b`, `c` or `none`.
    pub banner: String,
    /// Running giveaways at once, up to `giveaways::MAX_RUNNING`.
    pub max_running: u32,
}

impl Default for GiveawaysCfg {
    fn default() -> Self {
        Self {
            // Off until the server turns it on (owner decision 2026-11-23).
            enabled: false,
            button_label: "Enter".into(),
            button_emoji: "{e:giveaway_enter}".into(),
            blocked_roles: vec![],
            min_account_age_days: 7,
            min_member_hours: 0,
            block_raid_joiners: false,
            claim_hours: 48,
            sponsor_picture: false,
            banner: "a".into(),
            max_running: crate::giveaways::MAX_RUNNING as u32,
        }
    }
}

/// The position that puts channel `me` (id, parent, position) below every other channel of
/// category `cat` in `others`; None when it is there already. Discord orders by position, then
/// id. Shared by the bot's catch channel keeper and the dashboard's Auto Setup.
pub fn bottom_position(
    others: &[(u64, Option<u64>, i64)],
    me: (u64, Option<u64>, i64),
    cat: u64,
) -> Option<u64> {
    let key = |(id, _, pos): (u64, Option<u64>, i64)| (pos, id);
    let last = others
        .iter()
        .filter(|(id, parent, _)| *parent == Some(cat) && *id != me.0)
        .map(|s| key(*s))
        .max();
    let placed = me.1 == Some(cat) && last.is_none_or(|l| l < key(me));
    (!placed).then(|| last.map_or(0, |(p, _)| p.max(0) as u64 + 1))
}

/// Raider List: acts on confirmed raider accounts when they join. Off until saved on.
#[derive(Deserialize)]
#[serde(default)]
pub struct RaiderListCfg {
    pub enabled: bool,
    pub action: RaiderAction,
    #[serde(deserialize_with = "brim_logging::lenient::list")]
    pub categories: Vec<RaiderCategory>,
    /// What to do for listed users already in the server: only alert, or nothing.
    pub scan_members: bool,
    /// Unban here when the bot owner removes someone from the list.
    pub lift_on_removal: bool,
    /// Do not send this server's detections to the list.
    pub no_collect: bool,
}

impl Default for RaiderListCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            action: RaiderAction::Ban,
            categories: RaiderCategory::ALL.to_vec(),
            scan_members: true,
            lift_on_removal: true,
            no_collect: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RaiderAction {
    Alert,
    Quarantine,
    Kick,
    #[default]
    Ban,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RaiderCategory {
    Nuke,
    Raid,
    Scam,
    Spam,
}

impl RaiderCategory {
    pub const ALL: [RaiderCategory; 4] = [Self::Nuke, Self::Raid, Self::Scam, Self::Spam];
}

/// Live ids a backup restore must never delete: what the bot made or set up and a nuke does not
/// make (N2): the quarantine role, the log channels, the ticket and panel categories, log channel and open
/// tickets' channels, anti-nuke's ticket categories, the verification channel and roles, and the
/// permit roles (Auto Setup's roles among them). They can be newer than the backup. A fixed list:
/// any other config field (most are Editor level) must not shield a nuker's role or channel.
/// Shared by the bot's restore and the dashboard's preview.
pub async fn restore_keep(
    pool: &sqlx::PgPool,
    guild: i64,
    config: &serde_json::Value,
) -> sqlx::Result<std::collections::HashSet<u64>> {
    let mut keep = config_ids(config);
    let open: Vec<i64> = sqlx::query_scalar(
        "SELECT channel_id FROM tickets WHERE guild_id = $1 AND closed_at IS NULL AND channel_id IS NOT NULL",
    )
    .bind(guild)
    .fetch_all(pool)
    .await?;
    let permits = crate::permits::permit_roles(pool, guild).await?;
    keep.extend(open.into_iter().chain(permits).map(|i| i as u64));
    Ok(keep)
}

/// The config fields `restore_keep` keeps: an id, a list of ids, or a map's values.
const KEEP_FIELDS: [&str; 14] = [
    "/antinuke/quarantine_role",
    "/appeals/channel",
    "/catch/channel",
    "/catch/category",
    "/antinuke/ticket_categories",
    "/logging/fallback",
    "/logging/routes",
    "/logging/events",
    "/logging/custom",
    "/tickets/category",
    "/tickets/log_channel",
    "/tickets/public_channel_id",
    "/verification/channel",
    "/verification/roles",
];

fn config_ids(config: &serde_json::Value) -> std::collections::HashSet<u64> {
    use serde_json::Value as V;
    let id = |v: &V| v.as_str().and_then(|s| s.parse::<u64>().ok());
    let mut out = std::collections::HashSet::new();
    for f in KEEP_FIELDS {
        match config.pointer(f) {
            Some(V::Array(a)) => out.extend(a.iter().filter_map(|v| {
                // A custom log is an object with its channel.
                id(v).or_else(|| v.get("channel").and_then(id))
            })),
            Some(V::Object(o)) => out.extend(o.values().filter_map(id)),
            Some(v) => out.extend(id(v)),
            None => {}
        }
    }
    // Each ticket panel's own category.
    if let Some(V::Array(panels)) = config.pointer("/tickets/panels") {
        out.extend(panels.iter().filter_map(|p| p.get("category").and_then(id)));
    }
    out
}

#[cfg(test)]
mod unreadable_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_see_what_reads_skip() {
        let none = Value::Null;
        let an = json!({"limits": {"Ban": {}, "Future": {}}});
        assert_eq!(
            unreadable_entry("antinuke", &an, &none).as_deref(),
            Some("limits: Future = {}")
        );
        // R-h3 5: a bad value under a known key is refused too, not dropped.
        let an = json!({"limits": {"Ban": {"enabled": "no"}}});
        assert!(unreadable_entry("antinuke", &an, &none).is_some());
        let log = json!({"routes": {"panic": "not an id"}});
        assert_eq!(
            unreadable_entry("logging", &log, &none).as_deref(),
            Some("routes: panic = \"not an id\"")
        );
        let log =
            json!({"routes": {"panic": "1"}, "custom": [{"events": ["nope"], "channel": "2"}]});
        assert_eq!(
            unreadable_entry("logging", &log, &none).as_deref(),
            Some("custom events: \"nope\"")
        );
        assert!(unreadable_entry("raider_list", &json!({"categories": ["nuke"]}), &none).is_none());
        let da = json!({"actions": {"1": ["zap"]}});
        assert!(unreadable_entry("discord_automod", &da, &none).is_some());
        assert!(
            unreadable_entry("discord_automod", &json!({"actions": {"1": 5}}), &none).is_some()
        );
        // The lenient read keeps the rest of the section.
        let a: AntinukeCfg =
            serde_json::from_value(json!({"enabled": true, "limits": {"Future": {}}})).unwrap();
        assert!(a.enabled && a.limits.is_empty());
        // S-h3 5: one unknown Discord AutoMod action drops only itself, not its rule's Ban.
        let d: brim_automod::discord_api::DiscordAutomodCfg =
            serde_json::from_value(json!({"actions": {"1": ["zap", "ban"], "2": "x"}})).unwrap();
        assert_eq!(d.actions.len(), 1);
        assert_eq!(d.actions["1"], [brim_automod::discord_api::BotAction::Ban]);
    }

    /// R-h3 2: after a downgrade the stored newer entry does not block a save of the rest.
    #[test]
    fn a_stored_newer_entry_does_not_block_a_save() {
        let old = json!({"limits": {"Future": {"x": 1}}});
        let new = json!({"enabled": true, "limits": {"Future": {"x": 1}, "Ban": {}}});
        assert!(unreadable_entry("antinuke", &new, &old).is_none());
        let changed = json!({"limits": {"Future": {"x": 2}}});
        assert!(
            unreadable_entry("antinuke", &changed, &old).is_some(),
            "changed, so refused"
        );
        let old = json!({"custom": [{"events": ["new_event"], "channel": "2"}], "routes": {"later": "3"}});
        let new = json!({"custom": [{"events": ["new_event", "message_delete"], "channel": "2"}], "routes": {"later": "3"}});
        assert!(unreadable_entry("logging", &new, &old).is_none());
        let old = json!({"categories": ["nuke", "future"]});
        assert!(
            unreadable_entry("raider_list", &json!({"categories": ["future"]}), &old).is_none()
        );
        assert!(unreadable_entry("raider_list", &json!({"categories": ["other"]}), &old).is_some());
        let old = json!({"actions": {"1": ["zap"]}});
        assert!(unreadable_entry(
            "discord_automod",
            &json!({"actions": {"1": ["zap", "ban"]}}),
            &old
        )
        .is_none());
        assert!(
            unreadable_entry("discord_automod", &json!({"actions": {"2": ["zap"]}}), &old)
                .is_some()
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ticket_lists_refuse_only_new_entries_past_the_cap() {
        use serde_json::json;
        let snips = |n: usize| json!({"snippets": (0..n).map(|i| json!({"name": format!("s{i}")})).collect::<Vec<_>>()});
        assert_eq!(tickets_cap_problem(&snips(100), &snips(99)), None);
        assert!(tickets_cap_problem(&snips(101), &snips(100))
            .unwrap()
            .contains("100 snippets, the most allowed is 100"));
        // Stored above the cap: kept, edited and shortened freely.
        assert_eq!(tickets_cap_problem(&snips(120), &snips(120)), None);
        assert_eq!(tickets_cap_problem(&snips(110), &snips(120)), None);
        let ids = |n: u64| json!({"blocked": (0..n).map(|i| i.to_string()).collect::<Vec<_>>()});
        assert!(tickets_cap_problem(&ids(1001), &ids(1000)).is_some());
        assert_eq!(tickets_cap_problem(&ids(1000), &json!({})), None);
    }

    #[test]
    fn only_a_blank_prefix_reads_as_the_default() {
        let read = |p: &str| {
            serde_json::from_value::<MiscCfg>(json!({ "prefix": p }))
                .unwrap()
                .prefix
        };
        assert_eq!(
            (read(""), read("  "), read("tb "), read("?")),
            ("t!".into(), "t!".into(), "tb ".into(), "?".into())
        );
    }

    #[test]
    fn ticket_types_are_checked() {
        let ok: Vec<String> = TicketsCfg::default().types;
        assert_eq!(types_problem(&ok, &[], false), None);
        assert!(
            types_problem(&["A".into(), "a".into()], &[], false).is_some(),
            "repeats"
        );
        assert!(
            types_problem(&[" A".into()], &[], false).is_some(),
            "spaces at the ends"
        );
        let many: Vec<String> = (0..26).map(|i| i.to_string()).collect();
        assert!(types_problem(&many, &[], false).is_some(), "at most 25");
        let p: TicketPanel =
            serde_json::from_value(serde_json::json!({"id": "a", "name": "A", "kind": "Bug"}))
                .unwrap();
        assert!(
            types_problem(&ok, &[p], false).is_some(),
            "a preset must be listed"
        );
    }

    use super::*;

    #[test]
    fn a_part_turned_on_names_its_off_module() {
        let off = serde_json::json!({"server_joins": false});
        let (no, yes) = (
            serde_json::json!({"enabled": false}),
            serde_json::json!({"enabled": true}),
        );
        assert_eq!(
            module_to_turn_on("joingate", &no, &yes, &off),
            Some("server_joins")
        );
        // Already on (a settings save), module on or missing, a part without a module: nothing.
        assert_eq!(module_to_turn_on("joingate", &yes, &yes, &off), None);
        assert_eq!(
            module_to_turn_on("verification", &no, &yes, &serde_json::json!({})),
            None
        );
        assert_eq!(module_to_turn_on("automod", &no, &yes, &off), None);
        let mod_off = serde_json::json!({"moderation": false});
        assert_eq!(
            module_to_turn_on("catch", &no, &yes, &mod_off),
            Some("moderation")
        );
        assert_eq!(module_to_turn_on("raider_list", &no, &no, &off), None);
    }

    #[test]
    fn a_restore_keeps_only_the_named_config_ids() {
        let keep = config_ids(&json!({
            "antinuke": {"quarantine_role": "11", "ticket_categories": ["12"]},
            "logging": {"fallback": "13", "routes": {"mod_command": "14"}, "events": {"message_delete": "15"},
                        "custom": [{"channel": "16", "events": []}]},
            "tickets": {"category": "17", "log_channel": "18",
                        "panels": [{"id": "help", "category": "22", "ping_roles": ["23"]}]},
            "verification": {"channel": "19", "roles": ["20"]},
            "catch": {"channel": "24", "category": "25"},
            // Any other field, even an id list, shields nothing.
            "automod": {"exempt_roles": ["1570002257523511317"]},
            "sticky": {"roles": ["21"]},
            "misc": {"timeout": "600"}
        }));
        assert_eq!(keep, (11..=20).chain([22, 24, 25]).collect());
    }
    use serde_json::{json, Value};

    #[test]
    fn a_panel_emoji_is_one_standard_emoji() {
        let with = |e: &str| {
            let p: TicketPanel =
                serde_json::from_value(json!({"id": "help", "name": "Help", "emoji": e})).unwrap();
            panels_problem(&[p]).is_none()
        };
        for ok in ["", "🎫", "👍🏽", "1️⃣", "#️⃣"] {
            assert!(with(ok), "{ok}");
        }
        for bad in ["help", ":ticket:", "<:help:123>", "🎫 x", "12"] {
            assert!(!with(bad), "{bad}");
        }
    }

    #[test]
    fn raider_list_is_off_until_saved_on() {
        let c: RaiderListCfg = serde_json::from_value(json!({})).unwrap();
        assert!(!c.enabled && c.scan_members && c.lift_on_removal && !c.no_collect);
        assert_eq!(c.action, RaiderAction::Ban);
        assert_eq!(c.categories, RaiderCategory::ALL);
    }

    #[test]
    fn regex_words_match_and_bad_ones_are_refused() {
        let w: WordsCfg =
            serde_json::from_value(json!({"regex": ["fr[e3]+ n[i1]tro", "a{1,3}b"]})).unwrap();
        assert_eq!(w.regex.hits("FREE NITRO here, aab"), 2);
        assert_eq!(w.regex.hits("hello"), 0);
        assert_eq!(WordsCfg::default().regex.hits("anything"), 0);
        assert!(serde_json::from_value::<WordsCfg>(json!({"regex": ["(unclosed"]})).is_err());
    }

    #[test]
    fn saved_caps_outside_the_limits_are_pulled_inside() {
        let c: AntinukeCfg = serde_json::from_value(
            json!({"limits": {"Ban": {"per_minute": 99, "per_hour": 1000, "heat": 0}}}),
        )
        .unwrap();
        let (l, heat) = c.limit(NukeAction::Ban).unwrap();
        assert_eq!((l.per_minute, l.per_hour, heat), (50, 500, 20));
        let c: AntinukeCfg =
            serde_json::from_value(json!({"limits": {"Ban": {"per_hour": 1, "heat": 101}}}))
                .unwrap();
        let (l, heat) = c.limit(NukeAction::Kick).unwrap();
        assert_eq!((l.per_minute, l.per_hour, heat), (5, 2, 100));
        // Prune is no card: its built-in 1/1 stays.
        let c: AntinukeCfg = serde_json::from_value(json!({"prune": true})).unwrap();
        assert_eq!(c.limit(NukeAction::Prune).unwrap().0.per_minute, 1);
    }

    #[test]
    fn the_catch_channel_goes_to_the_bottom_of_its_category() {
        let id = |n: u64| n;
        let (cat, other) = (10, 20);
        let siblings = [
            (id(1), Some(cat), 0),
            (id(2), Some(cat), 3),
            (id(3), Some(other), 9),
        ];
        // Dragged out, or above a sibling: below the last one (position 3).
        assert_eq!(bottom_position(&siblings, (id(5), None, 0), cat), Some(4));
        assert_eq!(
            bottom_position(&siblings, (id(5), Some(cat), 1), cat),
            Some(4)
        );
        // Last already (a tie goes to the higher id): no move, so no loop.
        assert_eq!(bottom_position(&siblings, (id(5), Some(cat), 4), cat), None);
        assert_eq!(bottom_position(&siblings, (id(5), Some(cat), 3), cat), None);
        assert_eq!(bottom_position(&[], (id(5), None, 0), cat), Some(0));
    }

    #[test]
    fn catch_is_off_and_bans_by_default() {
        let c: CatchCfg = serde_json::from_value(json!({})).unwrap();
        assert!(!c.enabled && c.channel.is_none() && c.action == CatchAction::Ban);
        let c: CatchCfg = serde_json::from_value(json!({"action": "Log"})).unwrap();
        assert_eq!(c.action, CatchAction::Log);
    }

    #[test]
    fn auto_slowmode_is_clamped_and_a_bad_value_keeps_the_section() {
        let am = |v: serde_json::Value| -> AutomodCfg {
            serde_json::from_value(json!({"enabled": true, "auto_slowmode": v})).unwrap()
        };
        let c = am(
            json!({"enabled": true, "channel_pct": 0, "slowmode_secs": 0,
            "min_on_secs": u32::MAX, "calm_secs": 0}),
        )
        .auto_slowmode;
        assert!(c.enabled);
        assert_eq!(
            (c.channel_pct, c.slowmode_secs, c.min_on_secs, c.calm_secs),
            (200, 1, 3_600, 10)
        );
        let c = am(json!({"enabled": true, "slowmode_secs": 99_999})).auto_slowmode;
        assert_eq!((c.slowmode_secs, c.channel_pct), (120, 300));
        // -1 does not fit a u32: the card falls back to its defaults, automod stays on.
        let bad = am(json!({"enabled": true, "calm_secs": -1}));
        assert!(bad.enabled && !bad.auto_slowmode.enabled);
        assert_eq!(bad.auto_slowmode.calm_secs, 120);
        let none: AutomodCfg = serde_json::from_value(json!({})).unwrap();
        assert!(!none.auto_slowmode.enabled);
        assert_eq!(none.auto_slowmode.slowmode_secs, 5);
    }

    #[test]
    fn heat_is_clamped_when_read() {
        let am: AutomodCfg =
            serde_json::from_value(json!({"heat": {"max": 0, "decay_per_sec": 4, "base": 3}}))
                .unwrap();
        assert_eq!(
            (am.heat.max, am.heat.decay_per_sec, am.heat.base),
            (1.0, 0.99, 1.0),
            "a weight stops at the max heat"
        );
        let am: AutomodCfg = serde_json::from_value(json!({})).unwrap();
        assert_eq!((am.heat.max, am.heat.decay_per_sec), (100.0, 0.08));
    }

    #[test]
    fn panic_lasts_a_minute_by_default() {
        assert_eq!(PanicCfg::default().duration_secs, 60);
        let p: PanicCfg = serde_json::from_value(json!({})).unwrap();
        assert_eq!(p.duration_secs, 60);
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres: the stored old default 600
    /// moves to 60, other durations stay.
    #[tokio::test]
    async fn migration_moves_only_the_old_panic_default() {
        let Ok(url) = crate::db::test_url() else {
            return;
        };
        let pool = crate::db::connect(&url).await.unwrap();
        let g = 7_100_000_000 + std::process::id() as i64 * 10;
        let put = |gid: i64, secs: i64| {
            sqlx::query("INSERT INTO guild_config (guild_id, version, config) VALUES ($1, 1, $2)")
                .bind(gid)
                .bind(json!({"panic": {"duration_secs": secs}, "misc": {"prefix": "t!"}}))
                .execute(&pool)
        };
        put(g, 600).await.unwrap();
        put(g + 1, 300).await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/2200_panic_duration_60s.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let read = |gid: i64| {
            sqlx::query_scalar::<_, Value>("SELECT config FROM guild_config WHERE guild_id = $1")
                .bind(gid)
                .fetch_one(&pool)
        };
        let moved = read(g).await.unwrap();
        assert_eq!(moved["panic"]["duration_secs"], 60);
        assert_eq!(moved["misc"]["prefix"], "t!");
        assert_eq!(read(g + 1).await.unwrap()["panic"]["duration_secs"], 300);
        sqlx::query("DELETE FROM guild_config WHERE guild_id IN ($1, $2)")
            .bind(g)
            .bind(g + 1)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[test]
    fn antinuke_is_off_until_saved_on() {
        assert!(!AntinukeCfg::default().enabled);
        let off: AntinukeCfg = serde_json::from_value(json!({"quarantine_role": "7"})).unwrap();
        assert!(!off.enabled);
        let on: AntinukeCfg = serde_json::from_value(json!({"enabled": true})).unwrap();
        assert!(on.enabled);
    }

    #[test]
    fn off_filters_stay_off_until_enabled() {
        let get = |f: &FiltersCfg, k: &str| match k {
            "emojis" => f.emojis.enabled,
            "newlines" => f.newlines.enabled,
            "chars_upper" => f.chars_upper.enabled,
            "chars_lower" => f.chars_lower.enabled,
            "zalgo" => f.zalgo.enabled,
            "suspicion" => f.suspicion.enabled,
            "inactive_channel" => f.inactive_channel.enabled,
            "invites" => f.invites.enabled,
            "bad_sites" => f.bad_sites.enabled,
            "scam_links" => f.scam_links.enabled,
            "nsfw_links" => f.nsfw_links.enabled,
            "nsfw_images" => f.nsfw_images.enabled,
            "words.filter" => f.words.filter.enabled,
            "links" => f.links.enabled,
            _ => unreachable!("{k}"),
        };
        for k in FILTERS_OFF_BY_DEFAULT {
            let at = |v: Value| match k.split_once('.') {
                Some((a, b)) => json!({a: {b: v}}),
                None => json!({*k: v}),
            };
            assert!(!get(&FiltersCfg::default(), k), "{k}");
            let partial: FiltersCfg = serde_json::from_value(at(json!({"action": "ban"}))).unwrap();
            assert!(!get(&partial, k), "{k}");
            let on: FiltersCfg = serde_json::from_value(at(json!({"enabled": true}))).unwrap();
            assert!(get(&on, k), "{k}");
        }
        let d = FiltersCfg::default();
        assert!(
            d.regular.enabled && d.similar.enabled && d.mentions.enabled && d.attachments.enabled
        );
    }

    #[test]
    fn backups_are_off_until_saved_on() {
        let d = BackupsCfg::default();
        assert!(!d.enabled && d.auto);
        let on: BackupsCfg = serde_json::from_value(json!({"enabled": true})).unwrap();
        assert!(on.enabled && on.auto);
    }

    #[test]
    fn misc_defaults_fill_missing_fields_and_clamp() {
        let m: MiscCfg = serde_json::from_value(
            json!({"default_timeout_secs": 99_999_999, "ban_purge_days": 30}),
        )
        .unwrap();
        assert_eq!(m.prefix, "t!");
        assert!(m.use_slash && m.auto_warn_actions);
        assert_eq!(m.timeout_secs(), 28 * 86_400);
        let zero: MiscCfg = serde_json::from_value(json!({"default_timeout_secs": 0})).unwrap();
        assert_eq!(zero.timeout_secs(), 28 * 86_400);
        assert_eq!(m.purge_secs(), 7 * 86_400);
    }

    #[test]
    fn filters_default_on_and_accept_an_action() {
        let a: AutomodCfg = serde_json::from_value(
            json!({"filters": {"zalgo": {"enabled": false}, "invites": {"action": "timeout", "duration_secs": 600}}}),
        )
        .unwrap();
        assert!(a.filters.regular.enabled && !a.filters.zalgo.enabled);
        assert_eq!(a.filters.invites.action, Some(FilterAction::Timeout));
    }

    #[test]
    fn appeals_start_with_one_message_field() {
        let a = AppealsCfg::default();
        assert_eq!(a.fields.len(), 1);
        assert!(a.fields.len() <= MAX_APPEAL_FIELDS);
    }

    #[test]
    fn whitelist_tabs_keep_the_old_lists_as_spam() {
        let w: WhitelistCfg = serde_json::from_value(
            json!({"users": ["1"], "webhooks": ["2"], "invites": {"channels": ["3"]}}),
        )
        .unwrap();
        let (u, c) = (Id::new(1), Id::new(3));
        assert!(w.spam.covers(u, Id::new(9), None, None));
        assert!(w.invites.covers(Id::new(5), c, None, None));
        assert!(!w.mentions.covers(u, c, None, Some(Id::new(2))));
        assert!(w.spam.covers(Id::new(5), c, None, Some(Id::new(2))));
    }

    #[test]
    fn a_listed_role_exempts_nobody() {
        let w: WhitelistCfg = serde_json::from_value(json!({"roles": ["7"]})).unwrap();
        assert_eq!(w.spam.roles, [Id::new(7)], "the stored ids are kept");
        assert!(!w.spam.covers(Id::new(5), Id::new(9), None, None));
    }

    #[test]
    fn a_button_per_type_only_for_a_panel_without_a_preset_type() {
        let types: Vec<String> = ["Report", "Appeal", "Other"].map(String::from).to_vec();
        let any: TicketPanel =
            serde_json::from_value(json!({"id": "help", "name": "Create ticket"})).unwrap();
        let set: TicketPanel =
            serde_json::from_value(json!({"id": "rep", "name": "Report", "kind": "Report"}))
                .unwrap();
        let ids = |m: &Value| -> Vec<String> {
            m["components"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|r| r["components"].as_array().unwrap().clone())
                .map(|b| b["custom_id"].as_str().unwrap().to_string())
                .collect()
        };
        let m = panel_message(&[&any, &set], false, None, &types, true);
        let h = type_hash("Appeal");
        assert_eq!(
            ids(&m),
            [
                format!("tk2:open:help:{}", type_hash("Report")),
                format!("tk2:open:help:{h}"),
                format!("tk2:open:help:{}", type_hash("Other")),
                "tk2:open:rep".to_string()
            ]
        );
        assert_eq!(m["components"][0]["components"][1]["label"], "Appeal");
        // Switch off, or fewer than two types: the panel's own button.
        let off = panel_message(&[&any], false, None, &types, false);
        assert_eq!(ids(&off), ["tk2:open:help"]);
        let one = panel_message(&[&any], false, None, &types[..1], true);
        assert_eq!(ids(&one), ["tk2:open:help"]);
        // The menu lists the same, as `<panel>:<hash>`.
        let menu = panel_message(&[&any], true, None, &types, true);
        assert_eq!(
            menu["components"][0]["components"][0]["options"][1]["value"],
            format!("help:{h}")
        );
        // 24 types and a second panel: 25 buttons fit, 26 do not; 80-character labels only.
        let many: Vec<String> = (0..24).map(|i| format!("T{i}")).collect();
        let two = [any.clone(), set.clone()];
        assert_eq!(types_problem(&many, &two[..1], true), None);
        assert!(types_problem(&many, &[any.clone(), any.clone()], true).is_some());
        let long = vec!["x".repeat(81), "y".into()];
        assert!(types_problem(&long, &[], true).is_some());
        assert!(types_problem(&long, &[], false).is_none());
    }

    #[test]
    fn the_default_panel_message_is_the_text_and_one_button() {
        let types = TicketsCfg::default().types;
        let ps: Vec<TicketPanel> = serde_json::from_value(default_panels()).unwrap();
        assert!(panels_problem(&ps).is_none());
        assert!(types_problem(&types, &ps, true).is_none());
        let refs: Vec<&TicketPanel> = ps.iter().collect();
        let m = panel_message(&refs, false, None, &types, false);
        // No description: no line for the panel under the default text.
        let d = m["embeds"][0]["description"].as_str().unwrap();
        assert!(d.starts_with("Need a hand") && !d.contains("{text}") && !d.contains("{e:"));
        assert!(m["embeds"][0]["footer"]["text"].is_string());
        let labels: Vec<&Value> = m["components"][0]["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| &b["label"])
            .collect();
        assert_eq!(labels, [DEFAULT_BUTTON]);
        assert_eq!(ps[0].id, "support");
        // The Brim emoji is not uploaded in tests: the button has none.
        assert!(m["components"][0]["components"][0]["emoji"].is_null());
        // A panel description is a line under the text; a server's own look replaces the default.
        let mut with = ps.clone();
        with[0].description = "Questions {e:ticket}".into();
        let refs: Vec<&TicketPanel> = with.iter().collect();
        let own = json!({"embeds": [{"title": "Help", "description": "{text}"}]});
        let m = panel_message(&refs, false, Some(&own), &types, false);
        assert_eq!(m["embeds"][0]["title"], "Help");
        let d = m["embeds"][0]["description"].as_str().unwrap();
        assert!(d.starts_with("**Create ticket**\nQuestions") && !d.contains("{e:"));
        // Migration 4033 wrote the same text (with the old name) and panels; 4049 moves that
        // untouched text to this one.
        let old = DEFAULT_PANEL_TEXT.replace("Brim", "TommyBot");
        let sql = include_str!("../migrations/4033_ticket_panel_default.sql");
        assert!(sql.contains(&format!("to_jsonb('{old}'::text)")));
        let rename = include_str!("../migrations/4051_panel_text_brim.sql");
        assert!(rename.contains(&format!("to_jsonb('{DEFAULT_PANEL_TEXT}'::text)")));
        assert!(rename.contains(&format!("= '{old}';")));
        // 4055 recognises both untouched texts and writes the default greeting.
        let moved = include_str!("../migrations/4055_ticket_embed_defaults.sql");
        assert!(
            moved.contains(&format!("'{DEFAULT_PANEL_TEXT}'"))
                && moved.contains(&format!("'{old}'"))
        );
        // It wrote the old variable name, which 4066 renames.
        let old_greeting = DEFAULT_GREETING.replace("{member}", "{usermention}");
        assert!(moved.contains(&format!("to_jsonb('{old_greeting}'::text)")));
        let renamed = include_str!("../migrations/4066_variable_names.sql");
        assert!(renamed.contains("'{usermention}', '{member}'"));
        assert!(moved.contains(&format!("'color', {}", brim_logging::embed::BLUE)));
    }

    #[test]
    fn antinuke_limits_fall_back_to_defaults() {
        let c: AntinukeCfg = serde_json::from_value(json!({"limits": {
            "Ban": {"per_minute": 2}, "RoleCreate": {"enabled": false}, "BotAdd": {"per_minute": 1, "per_hour": 2},
            "Timeout": {"enabled": true}
        }}))
        .unwrap();
        let ban = c.limit(NukeAction::Ban).unwrap();
        assert_eq!((ban.0.per_minute, ban.0.per_hour, ban.1), (2, 15, 20));
        assert!(c.limit(NukeAction::RoleCreate).is_none());
        assert!(
            c.limit(NukeAction::Prune).is_none(),
            "prunes need the toggle"
        );
        // Opt-in actions are watched only once configured, then with their defaults.
        assert_eq!(c.limit(NukeAction::Timeout).unwrap().0.per_minute, 5);
        assert!(c.limit(NukeAction::Unban).is_none());
        // App additions are watched by default; the other new cards are opt-in.
        assert_eq!(
            c.limit(NukeAction::IntegrationCreate).unwrap().0.per_minute,
            2
        );
        assert_eq!(c.watched().len(), 10);
        for a in [
            NukeAction::IntegrationDelete,
            NukeAction::EmojiCreate,
            NukeAction::StickerCreate,
            NukeAction::SoundboardDelete,
            NukeAction::ScheduledEventCreate,
            NukeAction::GuildUpdate,
            NukeAction::VoiceDisconnect,
            NukeAction::VoiceMove,
            NukeAction::ThreadDelete,
            NukeAction::RoleRemove,
        ] {
            assert!(c.limit(a).is_none(), "{a:?}");
            let on: AntinukeCfg =
                serde_json::from_value(json!({"limits": {format!("{a:?}"): {}}})).unwrap();
            assert!(on.limit(a).is_some(), "{a:?}");
        }
        // Role removal counts by heat alone: no minute or hour window.
        let on: AntinukeCfg =
            serde_json::from_value(json!({"limits": {"RoleRemove": {}}})).unwrap();
        assert_eq!(on.limit(NukeAction::RoleRemove).unwrap().1, 15);
        assert!(!on.watched().contains_key(&NukeAction::RoleRemove));
    }
}

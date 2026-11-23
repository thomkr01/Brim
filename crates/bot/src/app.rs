//! Shared bot state and the per-guild config cache. The dashboard appends a `guild_config` row and
//! sends `NOTIFY config_changed <guild>:<version>`; we drop that guild's cached copy and reload lazily.
use crate::verify::VerificationCfg;
use anyhow::Result;
use brim_antinuke::{AntiNuke, NukeCase, PanicTracker};
use brim_automod::discord_api::{Api, DiscordAutomodCfg};
use brim_automod::{
    heat::{ChannelActivity, ChannelHeat, HeatState, UserHistory},
    join::{GateCfg, RaidCfg, RaidDetector},
    lockdown::{LockdownPrefs, MentionLock, MentionWatch},
};
use brim_core::{
    permits::{self, Level, RateLimiter},
    sections::{
        AntinukeCfg, AppealsCfg, AutomodCfg, BackupsCfg, CatchCfg, GiveawaysCfg, LockdownCfg,
        MiscCfg, PanicCfg, RaiderListCfg, TicketsCfg, WhitelistCfg,
    },
};
use brim_logging::{LogConfig, Logger};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::{postgres::PgListener, PgPool};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use twilight_http::Client;
use twilight_model::id::{
    marker::{ChannelMarker, GuildMarker, RoleMarker, UserMarker},
    Id,
};

#[derive(Default)]
pub struct GuildCfg {
    pub logging: LogConfig,
    pub antinuke: AntinukeCfg,
    pub automod: AutomodCfg,
    pub joingate: GateCfg,
    pub joinraid: Option<RaidCfg>,
    pub lockdown: LockdownCfg,
    pub lockdown_prefs: LockdownPrefs,
    pub discord_automod: DiscordAutomodCfg,
    pub panic: PanicCfg,
    pub backups: BackupsCfg,
    pub misc: MiscCfg,
    pub appeals: AppealsCfg,
    pub whitelist: WhitelistCfg,
    pub verification: VerificationCfg,
    pub tickets: TicketsCfg,
    pub catch: CatchCfg,
    pub raider_list: RaiderListCfg,
    pub giveaways: GiveawaysCfg,
    pub modules: brim_core::sections::ModulesCfg,
    /// Per-command settings (`cmdcfg`); empty on a server that changed none.
    pub commands: brim_core::cmdcfg::CommandsCfg,
    /// Sections that failed to parse and run on their defaults; the owner is told.
    pub fallbacks: Vec<&'static str>,
}

/// One dashboard section; a section that fails to parse falls back to its default and is noted.
fn section<T: DeserializeOwned + Default>(
    cfg: &Value,
    name: &'static str,
    fallbacks: &mut Vec<&'static str>,
) -> T {
    match cfg.get(name).cloned().map(serde_json::from_value) {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            tracing::warn!(section = name, %e, "bad config section, using defaults");
            if !fallbacks.contains(&name) {
                fallbacks.push(name);
            }
            T::default()
        }
        None => T::default(),
    }
}

/// Strict parse of one section for writes from Discord: a bad value is refused, not defaulted.
/// `old` is the stored section: an entry the bot skips is refused only when it changed.
pub fn check_section(name: &str, v: &Value, old: &Value) -> Result<(), String> {
    fn t<T: DeserializeOwned>(v: &Value) -> Result<(), String> {
        T::deserialize(v).map(drop).map_err(|e| e.to_string())
    }
    if let Some(e) = brim_core::sections::unreadable_entry(name, v, old) {
        return Err(format!("unknown entry {e}"));
    }
    match name {
        "logging" => t::<LogConfig>(v),
        "antinuke" => t::<AntinukeCfg>(v),
        "automod" => t::<AutomodCfg>(v),
        "joingate" => t::<GateCfg>(v),
        "joinraid" => t::<RaidCfg>(v),
        "lockdown" => t::<LockdownCfg>(v).and(t::<LockdownPrefs>(v)),
        "panic" => t::<PanicCfg>(v),
        "backups" => t::<BackupsCfg>(v),
        "misc" => t::<MiscCfg>(v),
        "whitelist" => t::<WhitelistCfg>(v),
        "verification" => t::<VerificationCfg>(v),
        "tickets" => t::<TicketsCfg>(v),
        "raider_list" => t::<RaiderListCfg>(v),
        "giveaways" => t::<GiveawaysCfg>(v),
        "modules" => t::<brim_core::sections::ModulesCfg>(v),
        "commands" => t::<brim_core::cmdcfg::CommandsCfg>(v),
        _ => Ok(()),
    }
}

impl GuildCfg {
    /// Every channel Brim logs to: the logging routes, the appeals and the ticket log channel.
    /// Nobody purges these through Brim, and a deleted one is made again (`protected`).
    pub fn log_channels(&self) -> std::collections::BTreeSet<Id<ChannelMarker>> {
        let mut all = self.logging.channels();
        all.extend(self.appeals.channel);
        all.extend(self.tickets.log_channel);
        all
    }

    pub(crate) fn parse(cfg: &Value) -> Self {
        let mut f = vec![];
        let lockdown: LockdownCfg = section(cfg, "lockdown", &mut f);
        let mut verification: VerificationCfg = section(cfg, "verification", &mut f);
        // Verified members get the Statics main roles unless verification names its own.
        if verification.roles.is_empty() {
            verification.roles = lockdown.main_roles.clone();
        }
        let mut c = Self {
            logging: section(cfg, "logging", &mut f),
            antinuke: section(cfg, "antinuke", &mut f),
            automod: section(cfg, "automod", &mut f),
            joingate: section(cfg, "joingate", &mut f),
            joinraid: section(cfg, "joinraid", &mut f),
            lockdown,
            lockdown_prefs: section(cfg, "lockdown", &mut f),
            discord_automod: section(cfg, "discord_automod", &mut f),
            panic: section(cfg, "panic", &mut f),
            backups: section(cfg, "backups", &mut f),
            misc: section(cfg, "misc", &mut f),
            appeals: section(cfg, "appeals", &mut f),
            whitelist: section(cfg, "whitelist", &mut f),
            verification,
            tickets: section(cfg, "tickets", &mut f),
            catch: section(cfg, "catch", &mut f),
            raider_list: section(cfg, "raider_list", &mut f),
            giveaways: section(cfg, "giveaways", &mut f),
            modules: section(cfg, "modules", &mut f),
            commands: section(cfg, "commands", &mut f),
            fallbacks: vec![],
        };
        c.fallbacks = f;
        // A module switched off turns its parts off here, so every handler sees them off; the
        // stored settings are kept for when it is on again.
        if !c.modules.server_joins {
            c.joingate.enabled = false;
            if let Some(r) = c.joinraid.as_mut() {
                r.enabled = false;
                r.voice.enabled = false;
            }
            c.verification.enabled = false;
            c.raider_list.enabled = false;
        }
        c.logging.off = !c.modules.logging;
        if !c.modules.messages {
            c.giveaways.enabled = false;
        }
        // Moderation off: its commands are refused by the command gates (`cmdcfg::module_off`).
        if !c.modules.moderation {
            c.catch.enabled = false;
        }
        c
    }
}

/// A pending captcha or sum: the answer, its expiry (ms) and the wrong answers so far.
pub type Pending = (String, i64, u8);

/// Log actors' roles and when they were read: [`App::log_roles`].
pub type LogRoles = HashMap<(Id<GuildMarker>, Id<UserMarker>), (Vec<Id<RoleMarker>>, Instant)>;

/// A log channel of one server: the key of [`App::log_public`].
pub type LogChannel = (
    Id<GuildMarker>,
    Id<twilight_model::id::marker::ChannelMarker>,
);

pub struct App {
    pub http: Client,
    /// Own client and rate-limit bucket for backup restore and panic mode.
    pub restore_http: Client,
    pub pool: PgPool,
    pub api: Api,
    pub me: Id<UserMarker>,
    /// The bot application's owner (the team owner for a team app); the Raider List never acts on them.
    pub bot_owner: Option<Id<UserMarker>>,
    cfgs: RwLock<CfgCache>,
    pub logger: Mutex<Logger>,
    /// Whether @everyone can read a log channel of a server, and when that was checked.
    pub log_public: Mutex<HashMap<LogChannel, (bool, std::time::Instant)>>,
    /// A log author's or executor's roles for the role ignore list, and when they were read.
    pub log_roles: Mutex<LogRoles>,
    pub antinuke: Mutex<AntiNuke>,
    pub rate_limiter: Mutex<RateLimiter>,
    pub heat: Mutex<HashMap<(u64, u64), (HeatState, UserHistory)>>,
    pub crowd: Mutex<brim_automod::heat::Crowd>,
    /// Phishing and NSFW domain lists, refreshed in the background.
    pub sites: brim_automod::sites::Sites,
    pub raids: Mutex<HashMap<u64, RaidDetector>>,
    /// Pending image-captcha answers, their expiry (ms) and wrong answers so far per (guild, user);
    /// lost on restart, the member just clicks again.
    pub captchas: Mutex<HashMap<(u64, u64), Pending>>,
    /// The invite tracker's per-guild actors, status and join results (`invites.rs`).
    pub tracker: Mutex<crate::invites::Tracker>,
    pub mentions: Mutex<HashMap<u64, MentionLock>>,
    /// Mentions per (guild, user) for the mentions filter's hour cap.
    pub pings: Mutex<HashMap<(u64, u64), MentionWatch>>,
    /// Last message per channel, for the inactive channel filter.
    pub channels: Mutex<HashMap<u64, ChannelActivity>>,
    /// Pooled heat per channel for auto slowmode; its own lock, never taken under `heat`.
    pub slow: Mutex<HashMap<u64, ChannelHeat>>,
    /// Channels with an `auto_slowmode` row, so a channel update needs no query otherwise.
    pub slow_active: Mutex<HashSet<u64>>,
    /// Discord's spammer flag per (guild, user) and when it was read (ms), for the verify button.
    pub spam_flags: Mutex<HashMap<(u64, u64), (bool, i64)>>,
    /// Name pair last punished by the post-join username check, so unrelated member updates do not repeat it.
    pub punished_names: Mutex<HashMap<(u64, u64), String>>,
    /// Nick auto-dehoist last set per (guild, member), so an update with the old name renames once.
    pub dehoisted: Mutex<HashMap<(u64, u64), String>>,
    /// Last public command per (guild, member), in ms.
    pub cmd_cooldown: Mutex<HashMap<(u64, u64), i64>>,
    /// When (ms) a (guild, member, command) may run again, for a command's own cooldown (`cmdcfg`).
    pub cmd_waits: Mutex<HashMap<(u64, u64, &'static str), i64>>,
    /// Anti-nuke heat per (guild, staff member) for panic mode's heat algorithm.
    pub nuke_heat: Mutex<HashMap<(u64, u64), HeatState>>,
    /// The open nuke case per guild and when it last grew; a long quiet gap starts a new case.
    pub nukes: Mutex<HashMap<u64, (NukeCase, Instant)>>,
    /// Anti-nuke quarantines and automod heat trips are counted apart, each with its own threshold.
    pub panic: Mutex<PanicTracker>,
    pub panic_heat: Mutex<PanicTracker>,
    pub panic_runs: Mutex<HashMap<u64, PanicRun>>,
    /// Heat panic: accounts that tripped it and when it ends (unix secs). Saved in `heat_panic`.
    pub heat_raiders: Mutex<HashMap<u64, (HashSet<u64>, i64)>>,
    pub(crate) guilds: Mutex<HashMap<Id<GuildMarker>, Arc<crate::hierarchy::GuildInfo>>>,
    /// Category (None: no category) of each channel created or deleted, until its audit entry arrives.
    pub channel_parents: Mutex<HashMap<u64, Option<u64>>>,
    /// Permit levels [`App::whitelisted`] read in the last [`LEVEL_TTL`].
    pub(crate) levels: Mutex<LevelCache>,
    /// Newest audit entry id seen per guild, so a new gateway session can fetch what it missed.
    pub last_audit: Mutex<HashMap<u64, u64>>,
    /// The last 1000 audit entry ids seen per guild: live and catch-up handle each once.
    pub audit_recent: Mutex<HashMap<u64, std::collections::VecDeque<u64>>>,
    /// Newest handled audit id per guild not yet written to `audit_seen` (`welcome::flush_audit`).
    pub audit_done: Mutex<HashMap<u64, u64>>,
    /// `last_audit` as it stood when `Ready` came in: where the catch-up starts.
    pub audit_floor: Mutex<HashMap<u64, u64>>,
    /// Audit ids being handled now: `flush_audit` saves nothing past the oldest.
    pub audit_inflight: Mutex<HashMap<u64, std::collections::BTreeMap<u64, Instant>>>,
    /// Start of a gap whose catch-up has not succeeded: saved and re-read until it does.
    pub audit_pinned: Mutex<HashMap<u64, u64>>,
    /// Each channel's category as last seen, and when it last moved to another.
    pub category_moves: Mutex<HashMap<u64, (Option<u64>, crate::hierarchy::CategoryMove)>>,
    /// Nukers whose quarantine and revert is running; true when more of their actions came in
    /// meanwhile, so the run reverts once more before it ends.
    pub nuke_runs: Mutex<crate::antinuke::NukeRuns>,
    /// Lock and unlock of one guild run one at a time, so a second lock never saves locked
    /// permissions as its snapshot.
    pub lock_ops: Mutex<HashMap<u64, Arc<tokio::sync::Mutex<()>>>>,
    /// Recent reactions per (guild, member), for reaction spam.
    pub reactions: Mutex<HashMap<(u64, u64), brim_automod::window::Window>>,
    /// Thread -> parent channel; see [`App::channel_scope`].
    pub thread_parents: Mutex<HashMap<Id<ChannelMarker>, Id<ChannelMarker>>>,
    /// Sticky role adds per guild in the current minute: (minute, count).
    pub sticky_budget: Mutex<HashMap<u64, (i64, u32)>>,
    /// Guilds watched for merged voice disconnect and move entries; see `voice_nuke`.
    pub voice_audit: Mutex<crate::voice_nuke::State>,
    /// Voice join raid state per guild; idle guilds are dropped on each voice join.
    pub voice_raids: Mutex<HashMap<u64, brim_automod::voice::VoiceRaidDetector>>,
    /// NSFW image scan queue, model and score cache.
    pub nsfw: crate::nsfw::Scanner,
    /// (guild, member) running `/invites create` now: one at a time, so the link caps hold.
    pub invite_claims: Mutex<HashSet<(u64, u64)>>,
}

/// How long a whitelist lookup is reused. Short: a role change clears it anyway.
const LEVEL_TTL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct LevelCache {
    map: HashMap<(u64, u64), (Level, Instant)>,
    /// Moves on every forget, so a lookup started before it is not cached.
    generation: u64,
}

/// Parsed configs per guild. A load that read the database before a newer save was announced must not
/// cache what it read, so every forget moves `generation` and a load only caches under the one it started with.
#[derive(Default)]
struct CfgCache {
    map: HashMap<u64, Arc<GuildCfg>>,
    generation: u64,
    /// The last config loaded per guild, kept through `forget`: `App::cfg_or_last` falls back to it.
    last: HashMap<u64, Arc<GuildCfg>>,
}

impl CfgCache {
    fn insert(&mut self, guild: u64, generation: u64, cfg: Arc<GuildCfg>) {
        // A stale read must not replace a newer value in `last` either.
        if generation == self.generation {
            self.last.insert(guild, cfg.clone());
            self.map.insert(guild, cfg);
        }
    }

    /// One guild, or all with None.
    fn forget(&mut self, guild: Option<u64>) {
        self.generation += 1;
        match guild {
            Some(g) => drop(self.map.remove(&g)),
            None => self.map.clear(),
        }
    }
}

/// Active panic mode for one guild. Its end is a `panic_end` job, so `panic::restore` rebuilds this at boot.
pub struct PanicRun {
    pub lock_mod_commands: bool,
    /// Non-zero while `panic::start` runs (its token): it takes panic's locks itself, so
    /// `panic::relock` waits.
    pub starting: u64,
}

/// Anyone may run the public commands, so each member gets one per this many ms.
const CMD_COOLDOWN_MS: i64 = 3_000;

impl App {
    /// True (and the clock restarts) when the member's last public command is old enough.
    pub fn cooled_down(&self, guild: u64, user: u64) -> bool {
        cool(
            &mut self.cmd_cooldown.lock().unwrap(),
            (guild, user),
            crate::automod::now_ms(),
        )
    }

    pub fn new(
        http: Client,
        restore_http: Client,
        pool: PgPool,
        api: Api,
        me: Id<UserMarker>,
    ) -> Self {
        Self {
            http,
            restore_http,
            pool,
            api,
            me,
            bot_owner: None,
            cfgs: Default::default(),
            logger: Mutex::new(Logger::new(me)),
            log_public: Default::default(),
            log_roles: Default::default(),
            antinuke: Mutex::new(AntiNuke::new(HashMap::new())),
            rate_limiter: Default::default(),
            heat: Default::default(),
            sites: Default::default(),
            raids: Default::default(),
            captchas: Default::default(),
            tracker: Default::default(),
            mentions: Default::default(),
            pings: Default::default(),
            channels: Default::default(),
            slow: Default::default(),
            slow_active: Default::default(),
            spam_flags: Default::default(),
            punished_names: Default::default(),
            dehoisted: Default::default(),
            cmd_cooldown: Default::default(),
            cmd_waits: Default::default(),
            crowd: Default::default(),
            nuke_heat: Default::default(),
            nukes: Default::default(),
            panic: Mutex::new(PanicTracker::new(Duration::from_secs(600), 0)),
            panic_heat: Mutex::new(PanicTracker::new(Duration::from_secs(600), 0)),
            panic_runs: Default::default(),
            heat_raiders: Default::default(),
            guilds: Default::default(),
            channel_parents: Default::default(),
            category_moves: Default::default(),
            last_audit: Default::default(),
            audit_recent: Default::default(),
            audit_done: Default::default(),
            audit_floor: Default::default(),
            audit_inflight: Default::default(),
            audit_pinned: Default::default(),
            levels: Default::default(),
            nuke_runs: Default::default(),
            lock_ops: Default::default(),
            thread_parents: Default::default(),
            reactions: Default::default(),
            sticky_budget: Default::default(),
            voice_audit: Mutex::new(crate::voice_nuke::State::new(crate::automod::now_ms())),
            voice_raids: Mutex::new(HashMap::new()),
            nsfw: Default::default(),
            invite_claims: Default::default(),
        }
    }

    /// Permit level of a member; a user who is not in the server has none. A bot is never EO or
    /// TA. A permit belongs to a user id; a role gives none ([`permits::top_permit`]).
    pub async fn level(
        &self,
        guild: Id<GuildMarker>,
        owner: Id<UserMarker>,
        user: Id<UserMarker>,
    ) -> Result<Level> {
        let member = crate::http_err::member(&self.http, guild, user).await?;
        // A permit, EO or TA listed by id must not outlive the person leaving (as the dashboard).
        if member.is_none() && user != owner {
            return Ok(Level::None);
        }
        let bot = member.as_ref().is_some_and(|m| m.user.bot);
        permits::resolve(
            &self.pool,
            guild.get() as i64,
            owner.get() as i64,
            user.get() as i64,
            bot,
        )
        .await
    }

    /// On the quarantine whitelist and not a bot: only users are (owner decision 11-13). A bot listed
    /// there from before gets no exemption; its bot permit's `quarantine` flag still counts.
    pub async fn quarantine_listed(
        &self,
        cfg: &GuildCfg,
        guild: Id<GuildMarker>,
        user: Id<UserMarker>,
    ) -> Result<bool> {
        if !cfg.antinuke.quarantine_whitelist.contains(&user) {
            return Ok(false);
        }
        let member = crate::http_err::member(&self.http, guild, user).await?;
        Ok(!member.is_some_and(|m| m.user.bot))
    }

    pub fn mod_commands_locked(&self, guild: u64) -> bool {
        self.panic_runs
            .lock()
            .unwrap()
            .get(&guild)
            .is_some_and(|r| r.lock_mod_commands)
    }

    /// The account that holds Brim's Owner level ([`permits::bot_owner`]). `GuildInfo::owner`
    /// is the Discord owner: use this for trust and for the owner's DMs.
    pub async fn bot_owner(&self, guild: Id<GuildMarker>) -> Result<Id<UserMarker>> {
        let discord = self.guild_info(guild).await?.owner;
        let o = permits::bot_owner(&self.pool, guild.get() as i64, discord.get() as i64).await?;
        Ok(Id::new(o as u64))
    }

    /// Owner, Extra Owner or Trusted Admin: untouchable by every bot system.
    pub async fn is_immune(&self, guild: Id<GuildMarker>, user: Id<UserMarker>) -> Result<bool> {
        let owner = self.guild_info(guild).await?.owner;
        Ok(self.level(guild, owner, user).await?.immune())
    }

    /// Exempt from one system via the permit whitelist (immune levels are exempt from all). A nuke
    /// asks this for every audit entry, so the level is kept for [`LEVEL_TTL`]: a member
    /// update clears it, and a read that raced that clear is not kept.
    pub async fn whitelisted(
        &self,
        guild: Id<GuildMarker>,
        user: Id<UserMarker>,
        flag: &str,
    ) -> Result<bool> {
        Ok(self
            .cached_level(guild, user, None)
            .await?
            .whitelisted(flag))
    }

    /// The level that [`App::whitelisted`] keeps. `roles`: the member's roles and bot flag as an
    /// event gave them, read without a member fetch on a miss.
    pub async fn cached_level(
        &self,
        guild: Id<GuildMarker>,
        user: Id<UserMarker>,
        roles: Option<(&[i64], bool)>,
    ) -> Result<Level> {
        let key = (guild.get(), user.get());
        let (hit, generation) = {
            let c = self.levels.lock().unwrap();
            let hit = c.map.get(&key).filter(|e| e.1.elapsed() < LEVEL_TTL);
            (hit.map(|e| e.0.clone()), c.generation)
        };
        let level = match hit {
            Some(l) => l,
            None => {
                let owner = self.guild_info(guild).await?.owner;
                let l = match roles {
                    Some((_, bot)) => {
                        let (g, o, u) = (guild.get() as i64, owner.get() as i64, user.get() as i64);
                        permits::resolve(&self.pool, g, o, u, bot).await?
                    }
                    None => self.level(guild, owner, user).await?,
                };
                let mut c = self.levels.lock().unwrap();
                if c.generation == generation {
                    // shortcut: cleared whole past the cap, it only costs lookups.
                    if c.map.len() > 10_000 {
                        c.map.clear();
                    }
                    c.map.insert(key, (l.clone(), Instant::now()));
                }
                l
            }
        };
        Ok(level)
    }

    /// A member's roles (and so their level) changed: drop the cached level.
    pub fn forget_level(&self, guild: u64, user: u64) {
        let mut c = self.levels.lock().unwrap();
        c.generation += 1;
        c.map.remove(&(guild, user));
    }

    /// The guild's permits changed: drop every cached level of it.
    pub fn forget_guild_levels(&self, guild: u64) {
        let mut c = self.levels.lock().unwrap();
        c.generation += 1;
        c.map.retain(|k, _| k.0 != guild);
    }

    pub async fn cfg(&self, guild: u64) -> Result<Arc<GuildCfg>> {
        let generation = {
            let c = self.cfgs.read().unwrap();
            if let Some(cfg) = c.map.get(&guild) {
                return Ok(cfg.clone());
            }
            c.generation
        };
        let row: Option<(Value,)> = sqlx::query_as(
            "SELECT config FROM guild_config WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
        )
        .bind(guild as i64)
        .fetch_optional(&self.pool)
        .await?;
        let cfg = Arc::new(row.map_or_else(GuildCfg::default, |(v,)| GuildCfg::parse(&v)));
        self.cfgs
            .write()
            .unwrap()
            .insert(guild, generation, cfg.clone());
        // After the insert, so the alert's own config reads hit the cache.
        if !cfg.fallbacks.is_empty() {
            let alert = crate::reply::fallback_alert(
                self,
                Id::new(guild),
                &cfg.fallbacks,
                &cfg.lockdown.main_roles,
            );
            let _ = tokio::time::timeout(crate::reply::DM_WAIT, alert).await;
        }
        Ok(cfg)
    }

    /// [`Self::cfg`], or on a read error the last config loaded, else (after one retry) Err with
    /// the defaults and the error. An audit entry must not go unguarded and uncounted because the
    /// database was slow (final audit 7).
    pub async fn cfg_or_last(
        &self,
        guild: u64,
    ) -> std::result::Result<Arc<GuildCfg>, (Arc<GuildCfg>, anyhow::Error)> {
        let e = match self.cfg(guild).await {
            Ok(c) => return Ok(c),
            Err(e) => e,
        };
        if let Some(c) = self.cfgs.read().unwrap().last.get(&guild).cloned() {
            tracing::warn!(
                ?e,
                guild,
                "reading the config failed, using the last one loaded"
            );
            return Ok(c);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.cfg(guild).await.map_err(|e| {
            tracing::warn!(
                ?e,
                guild,
                "reading the config failed twice, using the defaults"
            );
            (Arc::default(), e)
        })
    }

    /// Drops a guild's cached config after a write from Discord, before the NOTIFY round trip.
    pub fn forget_cfg(&self, guild: u64) {
        self.cfgs.write().unwrap().forget(Some(guild));
    }

    /// Reloads configs when the dashboard saves. Reconnects by itself; a gap only delays a reload.
    pub async fn listen_config(self: Arc<Self>) -> Result<()> {
        let mut l = PgListener::connect_with(&self.pool).await?;
        l.listen_all(["config_changed", "ticket_panel"]).await?;
        // A restarted listener missed what was saved while it was down.
        self.cfgs.write().unwrap().forget(None);
        // The newest version whose save work ran, per guild. Any database session may NOTIFY, so
        // that work (rule sync, ticket category, Raider List scan) runs only for a version that is
        // saved and newer (security review s02:t-cn-notify-unauth). Dropping the cached copy is cheap
        // and always happens.
        // Seeded with what is saved now, so a notice for an old version does not pass once after
        // each restart.
        let mut seen: HashMap<u64, i32> = sqlx::query_as::<_, (i64, i32)>(
            "SELECT guild_id, max(version) FROM guild_config GROUP BY guild_id",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(g, v)| (g as u64, v))
        .collect();
        loop {
            match l.try_recv().await? {
                // A server's Default messages changed: the ticket panel message follows. Only a
                // re-render of what is saved, so any session may send it.
                Some(n) if n.channel() == "ticket_panel" => {
                    if let Some(gid) = n.payload().parse().ok().and_then(Id::new_checked) {
                        let app = self.clone();
                        tokio::spawn(async move {
                            if let Err(e) = crate::tickets::public::ensure(&app, gid).await {
                                tracing::warn!(?e, %gid, "updating the ticket panel message failed");
                            }
                        });
                    }
                }
                Some(n) => {
                    // `<guild>` or `<guild>:<saved version>`.
                    let (g, version) = match n.payload().split_once(':') {
                        Some((g, v)) => (g, v.parse::<i32>().ok()),
                        None => (n.payload(), None),
                    };
                    if let Ok(g) = g.parse::<u64>() {
                        self.cfgs.write().unwrap().forget(Some(g));
                        // Unread: the save work runs, as before this check.
                        let version = match newest_version(&self.pool, g).await {
                            Ok(newest) => match fresh(&mut seen, g, version, newest) {
                                Some(v) => Some(v),
                                None => continue,
                            },
                            Err(e) => {
                                tracing::warn!(?e, g, "reading the saved config version failed");
                                version
                            }
                        };
                        let app = self.clone();
                        tokio::spawn(
                            async move { crate::automod::sync_discord_rule(&app, g).await },
                        );
                        if let Some(gid) = Id::new_checked(g) {
                            tokio::spawn(on_saved(self.clone(), gid, version));
                        }
                    }
                }
                // The connection dropped and reconnects on the next call: changes saved meanwhile
                // were never announced, so every cached config is read again.
                None => self.cfgs.write().unwrap().forget(None),
            }
        }
    }
}

/// The guild's newest saved config version, None when it has none.
async fn newest_version(pool: &PgPool, guild: u64) -> Result<Option<i32>> {
    Ok(
        sqlx::query_scalar("SELECT max(version) FROM guild_config WHERE guild_id = $1")
            .bind(guild as i64)
            .fetch_one(pool)
            .await?,
    )
}

/// The version whose save work should run for a notice naming `version` (None: the newest), or
/// None for a version not saved or already handled. Versions are saved in order, so one at or
/// below the newest is saved. A removed server starts again at 1: a remembered version above the
/// newest is forgotten.
fn fresh(
    seen: &mut HashMap<u64, i32>,
    g: u64,
    version: Option<i32>,
    newest: Option<i32>,
) -> Option<i32> {
    let newest = newest?;
    if seen.get(&g).is_some_and(|s| *s > newest) {
        seen.remove(&g);
    }
    let v = version.unwrap_or(newest);
    if v > newest || v < 1 || seen.get(&g).is_some_and(|s| v <= *s) {
        return None;
    }
    seen.insert(g, v);
    Some(v)
}

/// Setup that follows from a saved setting: Tickets turned on gets its category, a catch channel
/// gets its warning, and the Raider List turned on scans the members.
async fn on_saved(app: Arc<App>, gid: Id<GuildMarker>, version: Option<i32>) {
    if let Err(e) = crate::raiders::on_saved(&app, gid, version).await {
        tracing::warn!(?e, %gid, "queueing the Raider List scan failed");
    }
    // With the public channel off, the category is made when a ticket opens (owner decision 11-15).
    let public = app
        .cfg(gid.get())
        .await
        .is_ok_and(|c| c.tickets.public_channel);
    if public {
        if let Err(e) = crate::tickets::ensure_category(&app, gid).await {
            tracing::warn!(?e, %gid, "making the ticket category failed");
        }
    }
    if let Err(e) = crate::tickets::public::ensure(&app, gid).await {
        tracing::warn!(?e, %gid, "keeping the public ticket channel failed");
    }
    if let Err(e) = crate::catch::post_notice(&app, gid).await {
        tracing::warn!(?e, %gid, "posting the catch channel warning failed");
    }
    // Last: on a big server this gives roles to many members.
    if let Err(e) = crate::verify::on_saved(&app, gid).await {
        tracing::warn!(?e, %gid, "giving existing members the verified role failed");
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brim_antinuke::Action as NukeAction;
    use serde_json::json;

    #[test]
    fn parses_sections_and_survives_bad_ones() {
        let c = GuildCfg::parse(&json!({
            "antinuke": {"limits": {"Ban": {"per_minute": 3, "per_hour": 10}}, "quarantine_role": "7"},
            "automod": {"enabled": true, "auto_lockdown": {"mention_count": 5}},
            "logging": {"fallback": "42"},
            "joingate": "not an object",
            "lockdown": {"main_roles": ["9"]}
        }));
        assert_eq!(c.antinuke.limit(NukeAction::Ban).unwrap().0.per_minute, 3);
        assert_eq!(c.antinuke.quarantine_role, Some(Id::new(7)));
        assert!(c.automod.enabled && c.automod.auto_lockdown.mention_count == 5);
        assert_eq!(c.logging.fallback, Some(Id::new(42)));
        assert_eq!(c.lockdown.main_roles, vec![Id::new(9)]);
        assert!(!GuildCfg::parse(&json!({})).automod.enabled);
        assert_eq!(c.fallbacks, ["joingate"]);
    }

    /// A module switched off turns its parts off for the bot; a missing switch is on.
    #[test]
    fn a_module_off_turns_its_parts_off() {
        let on = json!({
            "joingate": {"enabled": true}, "verification": {"enabled": true},
            "raider_list": {"enabled": true},
            "joinraid": {"enabled": true, "count": 5, "span_ms": 1000, "suspicious_only": false, "active_ms": 1000, "voice": {"enabled": true}},
            "logging": {"fallback": "42"},
        });
        let c = GuildCfg::parse(&on);
        assert!(c.joingate.enabled && c.verification.enabled && c.raider_list.enabled);
        assert!(c
            .joinraid
            .as_ref()
            .is_some_and(|r| r.enabled && r.voice.enabled));
        assert!(!c.logging.off && c.modules.messages);
        let mut off = on.clone();
        off["modules"] = json!({"server_joins": false, "logging": false, "messages": false});
        let c = GuildCfg::parse(&off);
        assert!(!c.joingate.enabled && !c.verification.enabled && !c.raider_list.enabled);
        assert!(c
            .joinraid
            .as_ref()
            .is_some_and(|r| !r.enabled && !r.voice.enabled));
        assert!(c.logging.off && !c.modules.messages);
        // Moderation off: its commands are off, the safety commands and Backups are not.
        let mut m = on.clone();
        m["catch"] = json!({"enabled": true});
        let c = GuildCfg::parse(&m);
        assert!(c.catch.enabled && !crate::cmdcfg::module_off(&c, "ban"));
        m["modules"] = json!({"moderation": false});
        let c = GuildCfg::parse(&m);
        let off = |n| crate::cmdcfg::module_off(&c, n);
        assert!(!c.catch.enabled && off("ban") && off("purge"));
        assert!(
            !off("lock")
                && !off("unban")
                && !off("raider")
                && !off("backups")
                && c.joingate.enabled
        );
        assert!(c.fallbacks.is_empty());
    }

    /// D48: a key a newer version wrote drops only that entry; a broken section is noted.
    #[test]
    fn an_unknown_key_keeps_the_section_on() {
        let c = GuildCfg::parse(&json!({
            "antinuke": {"enabled": true, "limits": {"Ban": {"per_minute": 2}, "SoundboardNuke": {"per_minute": 1}}},
            "logging": {"routes": {"anti_nuke": "5", "future_kind": "6"}},
            "raider_list": {"enabled": true, "categories": ["nuke", "future"]},
        }));
        assert!(c.antinuke.enabled);
        assert_eq!(c.antinuke.limit(NukeAction::Ban).unwrap().0.per_minute, 2);
        assert_eq!(c.logging.routes.len(), 1);
        assert!(c.raider_list.enabled && c.raider_list.categories.len() == 1);
        assert!(c.fallbacks.is_empty());
        let bad = GuildCfg::parse(&json!({"antinuke": {"enabled": "yes"}}));
        assert!(!bad.antinuke.enabled);
        assert_eq!(bad.fallbacks, ["antinuke"]);
    }

    /// D70. The bot's own defaults (what an empty section means) sit inside every new range.
    #[test]
    fn defaults_fit_the_ranges() {
        use brim_core::limits::for_path;
        let c = GuildCfg::parse(&json!({}));
        let r = RaidCfg::default();
        let a = &c.automod;
        let vals: Vec<(&str, &str, i64)> = vec![
            ("automod", "strike.cap", a.strike.cap as i64),
            (
                "automod",
                "strike.multiplier_pct",
                a.strike.multiplier_pct as i64,
            ),
            (
                "automod",
                "strike.strike_timeout_ms",
                a.strike.strike_timeout_ms,
            ),
            ("automod", "strike.cap_timeout_ms", a.strike.cap_timeout_ms),
            (
                "automod",
                "auto_lockdown.mention_count",
                a.auto_lockdown.mention_count as i64,
            ),
            (
                "automod",
                "auto_lockdown.mention_window_ms",
                a.auto_lockdown.mention_window_ms,
            ),
            (
                "automod",
                "filters.mention_hour_cap",
                a.filters.mention_hour_cap as i64,
            ),
            ("panic", "window_secs", c.panic.window_secs as i64),
            ("panic", "heat_executors", c.panic.heat_executors as i64),
            ("joinraid", "count", r.count as i64),
            ("joinraid", "span_ms", r.span_ms),
            ("joinraid", "id_flag_min", r.id_flag_min.unwrap() as i64),
            ("misc", "ban_purge_days", c.misc.ban_purge_days as i64),
        ];
        for (s, p, v) in vals {
            let (lo, hi) = for_path(s, p).unwrap_or_else(|| panic!("{s}.{p} has no range"));
            assert!(
                (lo..=hi).contains(&v),
                "{s}.{p} default {v} outside {lo}..={hi}"
            );
        }
        for t in &c.misc.warn_actions {
            let (lo, hi) = for_path("misc", "warn_actions.0.points").unwrap();
            assert!((lo..=hi).contains(&(t.points as i64)));
        }
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. Loads race saves (insert, then
    /// forget as after a NOTIFY); once both settle, the config served must be the newest saved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_load_racing_a_save_never_caches_the_older_version() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let pool = brim_core::db::connect(&url).await.unwrap();
        let app = Arc::new(App::new(
            twilight_http::Client::new(String::new()),
            twilight_http::Client::new(String::new()),
            pool,
            brim_automod::discord_api::Api::new(""),
            Id::new(1),
        ));
        let g = 9_870_000_000 + std::process::id() as i64;
        let mut stale = 0;
        for v in 1..=100 {
            let a = app.clone();
            app.forget_cfg(g as u64); // nothing cached: the loads below read the database
                                      // Several loads in flight, so one reads the old version while the save commits.
            let loads: Vec<_> = (0..8)
                .map(|_| {
                    let a = a.clone();
                    tokio::spawn(async move { a.cfg(g as u64).await.unwrap() })
                })
                .collect();
            tokio::task::yield_now().await;
            sqlx::query(// Padded on the server: a load reading a version is slow, writing one is quick.
                "insert into guild_config (guild_id, version, config) values ($1, $2, $3 || jsonb_build_object('pad', repeat('x', 4000000)))")
                .bind(g)
                .bind(v)
                .bind(json!({"misc": {"prefix": format!("p{v}")}}))
                .execute(&app.pool)
                .await
                .unwrap();
            app.forget_cfg(g as u64);
            for l in loads {
                l.await.unwrap();
            }
            if app.cfg(g as u64).await.unwrap().misc.prefix != format!("p{v}") {
                stale += 1;
                app.forget_cfg(g as u64);
            }
        }
        sqlx::query("delete from guild_config where guild_id = $1")
            .bind(g)
            .execute(&app.pool)
            .await
            .unwrap();
        assert_eq!(stale, 0, "an older version stayed cached after a save");
    }

    #[test]
    fn a_load_started_before_a_forget_is_not_cached() {
        let mut c = CfgCache::default();
        let started = c.generation;
        c.forget(Some(2)); // a NOTIFY for another save lands while the load awaits the database
        c.insert(1, started, Default::default());
        assert!(c.map.is_empty());
        c.insert(1, c.generation, Default::default());
        assert!(c.map.contains_key(&1));
    }

    #[test]
    fn a_forgotten_config_stays_as_the_last_one_loaded() {
        let mut c = CfgCache::default();
        c.insert(1, c.generation, Default::default());
        c.forget(None);
        assert!(c.map.is_empty());
        assert!(
            c.last.contains_key(&1),
            "a DB error after a forget has no config to count with"
        );
        // A read started before a save's forget lands after the newer read.
        let started = c.generation;
        c.forget(Some(1));
        let newer = Arc::new(GuildCfg::default());
        c.insert(1, c.generation, newer.clone());
        c.insert(1, started, Arc::default());
        assert!(
            Arc::ptr_eq(&c.last[&1], &newer),
            "a stale read replaced the newer config"
        );
    }
}

fn cool(last: &mut HashMap<(u64, u64), i64>, key: (u64, u64), now: i64) -> bool {
    if last.len() > 10_000 {
        last.retain(|_, t| now - *t < CMD_COOLDOWN_MS);
    }
    if last.get(&key).is_some_and(|t| now - t < CMD_COOLDOWN_MS) {
        return false;
    }
    last.insert(key, now);
    true
}

#[cfg(test)]
mod cooldown_tests {
    #[test]
    fn one_public_command_per_window() {
        let mut m = Default::default();
        assert!(super::cool(&mut m, (1, 2), 0));
        assert!(!super::cool(&mut m, (1, 2), 2_999));
        assert!(super::cool(&mut m, (1, 3), 10));
        assert!(super::cool(&mut m, (1, 2), 3_000));
    }

    /// A config notice runs the save work only for a saved, newer version (security review
    /// s02:t-cn-notify-unauth).
    #[test]
    fn notices_run_save_work_once_per_saved_version() {
        let mut seen = std::collections::HashMap::new();
        let f = |s: &mut std::collections::HashMap<u64, i32>, v, newest| {
            crate::app::fresh(s, 1, v, newest)
        };
        assert_eq!(f(&mut seen, Some(1), None), None, "nothing saved");
        assert_eq!(f(&mut seen, Some(2), Some(2)), Some(2));
        assert_eq!(f(&mut seen, Some(2), Some(2)), None, "a repeat");
        assert_eq!(f(&mut seen, Some(1), Some(2)), None, "an older version");
        assert_eq!(
            f(&mut seen, Some(9), Some(2)),
            None,
            "a version nobody saved"
        );
        assert_eq!(
            f(&mut seen, None, Some(3)),
            Some(3),
            "no version: the newest"
        );
        // Purged and set up again: numbers start at 1.
        assert_eq!(f(&mut seen, Some(1), Some(1)), Some(1));
    }
}

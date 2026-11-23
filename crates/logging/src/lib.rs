//! Event logging. The bot passes every gateway event of a guild to [`Logger::handle`] with that
//! guild's [`LogConfig`] and sends the returned [`LogMessage`]s. Send them with allowed mentions
//! set to none, since logged user text must not ping anyone.

pub mod botmsg;
mod cache;
mod config;
pub mod embed;
pub mod emoji;
pub mod lenient;
pub mod overlay;
mod render;

use cache::{CachedMessage, MessageCache};
pub use config::{CustomLog, LogConfig, LogEvent, LogKind};
pub use embed::Log;
pub use render::{perm_names, GO_CHANNEL, GO_MESSAGE, GO_POLL};
use twilight_model::{
    channel::{Channel, Message},
    poll::Poll,
};

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};
use twilight_model::{
    channel::message::{Embed, MessageFlags},
    gateway::event::Event,
    guild::{
        audit_log::{AuditLogEntry, AuditLogEventType},
        Guild,
    },
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, ScheduledEventMarker, UserMarker},
        Id,
    },
};

/// How long a moderator's message delete is kept to name the deleter of a gateway delete.
const DELETER_WINDOW: Duration = Duration::from_secs(60);
/// Caps the audit delete and removal lists.
const MAX_RECENT: usize = 1000;
/// A delete log waits this long so its audit entry, which can arrive after the delete, is seen.
const HOLD: Duration = Duration::from_secs(10);
/// How many polls, scheduled events and finished-message marks the logger keeps in memory.
const MAX_KEPT: usize = 2000;
/// Active threads kept to tell an archive from a thread the bot never saw open.
const MAX_THREADS: usize = 200_000;
/// A kick or ban audit entry suppresses the member leave for the same user within this window.
const REMOVAL_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq)]
pub struct LogMessage {
    /// The server whose log this is: the sender checks that `channel_id` is in it.
    pub guild: Id<GuildMarker>,
    pub channel_id: Id<ChannelMarker>,
    pub kind: LogKind,
    pub embed: Embed,
    /// Link button under the log ("Go to Message").
    pub link: Option<render::Link>,
    /// File name and text: the cached content of a bulk delete. Discord stores it, the bot keeps nothing.
    pub file: Option<(String, String)>,
    /// The message author, the moderator who deleted it, or the audit executor: the bot checks
    /// them against the ignore lists (the deleter is only known when a held delete is sent).
    pub actors: Vec<Id<UserMarker>>,
    /// Values an edited version of the log's entry fills as `{name}` (`Log::var`).
    pub vars: Vec<(String, String)>,
}

pub(crate) struct Entry {
    kind: LogKind,
    event: Option<LogEvent>,
    channel: Option<Id<ChannelMarker>>,
    /// Unbuilt, so its values (`Log::var`) reach the sender with it.
    embed: Log,
    link: Option<render::Link>,
    /// The message author, the audit executor, or the member a join or voice log is about.
    actor: Option<Id<UserMarker>>,
}

/// One audit log message delete: `executor` deleted a message by `target` in `channel`.
struct AuditDelete {
    at: Instant,
    executor: Id<UserMarker>,
    target: Id<UserMarker>,
    channel: Id<ChannelMarker>,
}

/// A message delete waiting for its audit entry.
struct HeldDelete {
    at: Instant,
    guild: Id<GuildMarker>,
    log: Id<ChannelMarker>,
    id: Id<MessageMarker>,
    channel: Id<ChannelMarker>,
    cached: Option<CachedMessage>,
    /// Set when the message was a poll the logger saw.
    poll: Option<render::PollInfo>,
}

/// A member leave waiting to see whether a staff kick or ban audit entry explains it.
struct HeldLeave {
    at: Instant,
    log: Id<ChannelMarker>,
    guild: Id<GuildMarker>,
    user: Id<UserMarker>,
    embed: Log,
}

/// A thread archive waiting to see whether an audit entry (a person archived it) explains it.
struct HeldArchive {
    at: Instant,
    guild: Id<GuildMarker>,
    log: Id<ChannelMarker>,
    thread: Id<ChannelMarker>,
    embed: Log,
    link: Option<render::Link>,
}

pub struct Logger {
    /// The bot's own user id. Its kicks and bans show only as member leaves.
    me: Id<UserMarker>,
    cache: MessageCache,
    deletes: VecDeque<AuditDelete>,
    /// Users removed by a staff kick or ban, from the audit log, per guild: a kick in one
    /// server must not hide a leave from another.
    removals: VecDeque<(Instant, Id<GuildMarker>, Id<UserMarker>)>,
    held: VecDeque<HeldDelete>,
    leaves: VecDeque<HeldLeave>,
    /// The voice channel each member is in, to tell joins, leaves and moves apart.
    voice: HashMap<(Id<GuildMarker>, Id<UserMarker>), Id<ChannelMarker>>,
    /// Recent polls, for the question of a delete and the answers of a vote. Memory only.
    polls: HashMap<(Id<GuildMarker>, Id<MessageMarker>), render::PollInfo>,
    poll_order: VecDeque<(Id<GuildMarker>, Id<MessageMarker>)>,
    /// Polls already logged as ended, so a later update of the same message does not log it again.
    ended: VecDeque<Id<MessageMarker>>,
    /// Scheduled event names, to name the event of a sign-up.
    events: HashMap<Id<ScheduledEventMarker>, (Id<GuildMarker>, String)>,
    event_order: VecDeque<Id<ScheduledEventMarker>>,
    /// The boost level of each guild before its latest update.
    tiers: HashMap<Id<GuildMarker>, u8>,
    /// Threads seen open: one that turns archived was archived while we watched.
    active_threads: HashSet<Id<ChannelMarker>>,
    archives: VecDeque<HeldArchive>,
    /// Threads a person changed (audit log), to tell their archive from the automatic one.
    thread_audits: VecDeque<(Instant, Id<ChannelMarker>)>,
}

impl Logger {
    pub fn new(me: Id<UserMarker>) -> Self {
        Self {
            me,
            cache: MessageCache::default(),
            deletes: VecDeque::new(),
            removals: VecDeque::new(),
            held: VecDeque::new(),
            leaves: VecDeque::new(),
            voice: HashMap::new(),
            polls: HashMap::new(),
            poll_order: VecDeque::new(),
            ended: VecDeque::new(),
            event_order: VecDeque::new(),
            events: HashMap::new(),
            tiers: HashMap::new(),
            active_threads: HashSet::new(),
            archives: VecDeque::new(),
            thread_audits: VecDeque::new(),
        }
    }

    /// Learns what a guild has now (from `GuildCreate`): its boost level, scheduled event names
    /// and open threads, so later changes can be told from the first sight.
    pub fn seed(&mut self, g: &Guild) {
        self.tiers.insert(g.id, u8::from(g.premium_tier));
        self.events.retain(|_, (guild, _)| *guild != g.id);
        for e in &g.guild_scheduled_events {
            self.remember_event(g.id, e.id, &e.name);
        }
        for t in g.threads.iter().filter(|t| !archived(t)) {
            self.note_thread(t.id, false);
        }
    }

    fn remember_event(&mut self, guild: Id<GuildMarker>, id: Id<ScheduledEventMarker>, name: &str) {
        if self.events.insert(id, (guild, name.into())).is_none() {
            self.event_order.push_back(id);
        }
        // Oldest first, so a finished event makes room for a new one.
        while self.events.len() > MAX_KEPT {
            if let Some(old) = self.event_order.pop_front() {
                self.events.remove(&old);
            }
        }
    }

    /// `archived`: the thread is archived now, so it is no longer open. Returns whether it was open.
    fn note_thread(&mut self, id: Id<ChannelMarker>, archived: bool) -> bool {
        if archived {
            self.active_threads.remove(&id)
        } else {
            if self.active_threads.len() < MAX_THREADS {
                self.active_threads.insert(id);
            }
            false
        }
    }

    fn remember_poll(&mut self, guild: Id<GuildMarker>, msg: &Message, poll: &Poll) {
        let key = (guild, msg.id);
        if self
            .polls
            .insert(key, render::PollInfo::of(poll, msg.author.id))
            .is_none()
        {
            self.poll_order.push_back(key);
        }
        while self.poll_order.len() > MAX_KEPT {
            if let Some(old) = self.poll_order.pop_front() {
                self.polls.remove(&old);
            }
        }
    }

    /// The voice channel the logger last saw the member in.
    pub fn voice_channel(
        &self,
        guild: Id<GuildMarker>,
        user: Id<UserMarker>,
    ) -> Option<Id<ChannelMarker>> {
        self.voice.get(&(guild, user)).copied()
    }

    /// Records a member's voice channel and returns the one before. The bot calls it before any
    /// await, since two voice updates of one member run in their own tasks, then logs the update
    /// with [`Logger::handle_voice`].
    pub fn voice_update(
        &mut self,
        guild: Id<GuildMarker>,
        user: Id<UserMarker>,
        new: Option<Id<ChannelMarker>>,
    ) -> Option<Id<ChannelMarker>> {
        match new {
            Some(c) => self.voice.insert((guild, user), c),
            None => self.voice.remove(&(guild, user)),
        }
    }

    /// `handle` for a voice update already recorded with [`Logger::voice_update`].
    pub fn handle_voice(
        &mut self,
        guild: Id<GuildMarker>,
        cfg: &LogConfig,
        user: Id<UserMarker>,
        (old, new): (Option<Id<ChannelMarker>>, Option<Id<ChannelMarker>>),
        now: Instant,
    ) -> Vec<LogMessage> {
        let mut out = self.flush(now);
        out.extend(voice_entry(user, old, new).and_then(|e| route(guild, cfg, e)));
        #[cfg(test)]
        reads_as_default(&out);
        out
    }

    /// Replaces a guild's voice state with the members in voice now (from `GuildCreate`), so a
    /// move after a restart is not read as a join.
    pub fn seed_voice(
        &mut self,
        guild: Id<GuildMarker>,
        members: impl IntoIterator<Item = (Id<UserMarker>, Id<ChannelMarker>)>,
    ) {
        self.voice.retain(|k, _| k.0 != guild);
        self.voice
            .extend(members.into_iter().map(|(u, c)| ((guild, u), c)));
    }

    /// Returns the logs to send for one event. Call it for every event of a guild. Messages are
    /// cached only while a message edit or delete log is set: no log would use the text.
    pub fn handle(
        &mut self,
        guild: Id<GuildMarker>,
        cfg: &LogConfig,
        event: &Event,
        now: Instant,
    ) -> Vec<LogMessage> {
        let out = self.handle_event(guild, cfg, event, now);
        #[cfg(test)]
        reads_as_default(&out);
        out
    }

    fn handle_event(
        &mut self,
        guild: Id<GuildMarker>,
        cfg: &LogConfig,
        event: &Event,
        now: Instant,
    ) -> Vec<LogMessage> {
        let mut out = self.flush(now);
        if let Event::MessageDelete(m) = event {
            let Some(guild) = m.guild_id else {
                return out;
            };
            let cached = self.cache.remove(guild, m.id, now);
            let poll = self.polls.remove(&(guild, m.id));
            if let Some(log) = cfg.target(LogKind::Message, Some(LogEvent::MessageDelete)) {
                let skip = cached.as_ref().is_some_and(|c| {
                    cfg.ignores_user(LogKind::Message, Some(c.author))
                        || (cfg.ignore_embeds && c.embeds > 0)
                }) || poll
                    .as_ref()
                    .is_some_and(|p| cfg.ignores_user(LogKind::Message, Some(p.author)));
                if !skip && !ignored(cfg, Some(m.channel_id)) {
                    let held = HeldDelete {
                        at: now,
                        guild,
                        log,
                        id: m.id,
                        channel: m.channel_id,
                        cached,
                        poll,
                    };
                    // Without the cached author no audit entry can name a deleter, so waiting is pointless.
                    // A delete flood sends the oldest early instead of growing the queue.
                    if held.cached.is_none() && held.poll.is_none() {
                        out.push(self.release(held, now));
                    } else {
                        if self.held.len() >= MAX_RECENT {
                            let oldest = self.held.pop_front().expect("queue is full");
                            out.push(self.release(oldest, now));
                        }
                        self.held.push_back(held);
                    }
                }
            }
            return out;
        }
        if let Event::MessageDeleteBulk(m) = event {
            let Some(guild) = m.guild_id else {
                return out;
            };
            let mut ids = m.ids.clone();
            ids.sort();
            // Each line is "author id: content", oldest first; uncached messages are skipped.
            for id in &ids {
                self.polls.remove(&(guild, *id));
            }
            let lines: Vec<String> = ids
                .iter()
                .filter_map(|id| self.cache.remove(guild, *id, now))
                .map(|c| format!("{}: {}\n", c.author, c.text()))
                .collect();
            if let Some(log) = cfg.target(LogKind::Message, Some(LogEvent::MessageDeleteBulk)) {
                if !ignored(cfg, Some(m.channel_id)) {
                    let name = ids
                        .first()
                        .map(|i| render::bulk_file(*i))
                        .unwrap_or_default();
                    let (embed, vars) =
                        render::bulk_delete(m.channel_id, m.ids.len(), lines.len(), &name).parts();
                    out.push(LogMessage {
                        vars,
                        guild,
                        channel_id: log,
                        kind: LogKind::Message,
                        embed,
                        link: None,
                        file: (!lines.is_empty()).then(|| (name, lines.concat())),
                        actors: vec![],
                    });
                }
            }
            return out;
        }
        if let Event::MemberRemove(m) = event {
            if !self.removed_by_audit(m.guild_id, m.user.id, now)
                && !cfg.ignores_user(LogKind::Member, Some(m.user.id))
            {
                if let Some(log) = cfg.target(LogKind::Member, None) {
                    self.leaves.push_back(HeldLeave {
                        at: now,
                        log,
                        guild: m.guild_id,
                        user: m.user.id,
                        embed: render::member_remove(&m.user),
                    });
                }
            }
            return out;
        }
        let keep = [
            LogEvent::MessageUpdate,
            LogEvent::MessageDelete,
            LogEvent::MessageDeleteBulk,
            LogEvent::MessagePublish,
        ]
        .into_iter()
        .any(|e| cfg.target(LogKind::Message, Some(e)).is_some());
        out.extend(
            self.entry(event, cfg, keep, now)
                .and_then(|e| route(guild, cfg, e)),
        );
        out
    }

    /// Returns the held delete logs that are due. Called from `handle`, and by the bot on a timer
    /// so a quiet guild still gets its logs.
    pub fn flush(&mut self, now: Instant) -> Vec<LogMessage> {
        let out = self.flush_due(now);
        #[cfg(test)]
        reads_as_default(&out);
        out
    }

    fn flush_due(&mut self, now: Instant) -> Vec<LogMessage> {
        self.cache.prune_all(now);
        let mut out = Vec::new();
        while self
            .held
            .front()
            .is_some_and(|h| now.duration_since(h.at) >= HOLD)
        {
            let Some(h) = self.held.pop_front() else {
                break;
            };
            out.push(self.release(h, now));
        }
        // The audit entry of a kick or ban can arrive after the leave; if it did, the leave is dropped.
        while self
            .leaves
            .front()
            .is_some_and(|h| now.duration_since(h.at) >= HOLD)
        {
            let Some(h) = self.leaves.pop_front() else {
                break;
            };
            if !self
                .removals
                .iter()
                .any(|(at, g, u)| *g == h.guild && *u == h.user && *at >= h.at)
            {
                out.push(LogMessage {
                    vars: h.embed.meta().1.to_vec(),
                    guild: h.guild,
                    channel_id: h.log,
                    kind: LogKind::Member,
                    embed: h.embed.build(),
                    link: None,
                    file: None,
                    actors: vec![h.user],
                });
            }
        }
        while self
            .archives
            .front()
            .is_some_and(|h| now.duration_since(h.at) >= HOLD)
        {
            let Some(h) = self.archives.pop_front() else {
                break;
            };
            let by_person = self
                .thread_audits
                .iter()
                .any(|(at, t)| *t == h.thread && *at + HOLD >= h.at);
            if !by_person {
                out.push(LogMessage {
                    vars: h.embed.meta().1.to_vec(),
                    guild: h.guild,
                    channel_id: h.log,
                    kind: LogKind::Thread,
                    embed: h.embed.build(),
                    link: h.link,
                    file: None,
                    actors: vec![],
                });
            }
        }
        while self
            .thread_audits
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= HOLD * 3)
        {
            self.thread_audits.pop_front();
        }
        out
    }

    fn release(&mut self, h: HeldDelete, now: Instant) -> LogMessage {
        let author = h
            .cached
            .as_ref()
            .map(|c| c.author)
            .or(h.poll.as_ref().map(|p| p.author));
        let deleter = author.and_then(|a| self.deleter(a, h.channel, h.at, now));
        let poll = h.poll.as_ref().map(|p| (p.question.as_str(), p.author));
        let (embed, vars) =
            render::message_delete(h.id, h.channel, h.cached.as_ref(), deleter, poll).parts();
        LogMessage {
            vars,
            guild: h.guild,
            channel_id: h.log,
            kind: LogKind::Message,
            embed,
            link: None,
            file: None,
            actors: author.into_iter().chain(deleter).collect(),
        }
    }

    /// `keep`: whether messages go into the cache.
    fn entry(&mut self, event: &Event, cfg: &LogConfig, keep: bool, now: Instant) -> Option<Entry> {
        match event {
            Event::MessageCreate(m) => {
                if let Some(guild) = m.guild_id.filter(|_| keep) {
                    let mut cached = CachedMessage::new(
                        m.author.id,
                        m.content.clone(),
                        files(&m.attachments),
                        m.embeds.len(),
                        now,
                    );
                    cached.published = published(&m.message);
                    self.cache.insert(guild, m.id, cached, now);
                }
                let (guild, poll) = (m.guild_id?, m.poll.as_ref()?);
                self.remember_poll(guild, &m.message, poll);
                let (embed, link) = render::poll_create(&m.message, poll);
                Some(Entry {
                    kind: LogKind::Poll,
                    event: Some(LogEvent::PollCreate),
                    channel: Some(m.channel_id),
                    embed,
                    link,
                    actor: Some(m.author.id),
                })
            }
            Event::MessageUpdate(m) => {
                let msg = &m.0;
                let guild = msg.guild_id?;
                let old = self.cache.get(guild, msg.id, now).cloned();
                let mut cached = CachedMessage::new(
                    msg.author.id,
                    msg.content.clone(),
                    files(&msg.attachments),
                    msg.embeds.len(),
                    now,
                );
                cached.published = published(msg);
                if keep {
                    self.cache.insert(guild, msg.id, cached, now);
                } else {
                    // A copy kept from before the log was off is stale now.
                    self.cache.remove(guild, msg.id, now);
                }
                if let Some(poll) = &msg.poll {
                    // A poll's updates carry no text change: only its end is logged, never as an edit.
                    let ended = poll.results.as_ref().is_some_and(|r| r.is_finalized);
                    if !ended || !first_time(&mut self.ended, msg.id) {
                        return None;
                    }
                    let (embed, link) = render::poll_end(msg, poll);
                    return Some(Entry {
                        kind: LogKind::Poll,
                        event: Some(LogEvent::PollEnd),
                        channel: Some(msg.channel_id),
                        embed,
                        link,
                        actor: Some(msg.author.id),
                    });
                }
                // Newly published: the copy we hold was not, or (no copy) we have not logged it yet.
                // Only with a copy that says it was not published: without one (expired, restart, log
                // off) an edit of an old announcement would read as a publish and hide its edit log.
                if published(msg) && old.as_ref().is_some_and(|o| !o.published) {
                    let (embed, link) = render::message_publish(msg);
                    return Some(Entry {
                        kind: LogKind::Message,
                        event: Some(LogEvent::MessagePublish),
                        channel: Some(msg.channel_id),
                        embed,
                        link,
                        actor: Some(msg.author.id),
                    });
                }
                // Embed-only updates (link previews) keep the content and are not worth a log.
                if old.as_ref().is_some_and(|c| c.content == msg.content) {
                    return None;
                }
                let embeds = !msg.embeds.is_empty() || old.as_ref().is_some_and(|c| c.embeds > 0);
                if embeds && cfg.ignore_embeds {
                    return None;
                }
                let old = old.map(|c| c.text());
                let (embed, link) = render::message_edit(old.as_deref(), msg);
                Some(Entry {
                    kind: LogKind::Message,
                    event: Some(LogEvent::MessageUpdate),
                    channel: Some(msg.channel_id),
                    embed,
                    link,
                    actor: Some(msg.author.id),
                })
            }
            Event::MemberAdd(m) => Some(Entry {
                kind: LogKind::Member,
                event: None,
                channel: None,
                embed: render::member_add(&m.member.user),
                link: None,
                actor: Some(m.member.user.id),
            }),
            Event::GuildAuditLogEntryCreate(e) => self.audit(&e.0, now),
            Event::MessagePollVoteAdd(v) => self.vote(
                cfg,
                true,
                v.guild_id?,
                (v.channel_id, v.message_id),
                (v.user_id, v.answer_id),
            ),
            Event::MessagePollVoteRemove(v) => self.vote(
                cfg,
                false,
                v.guild_id?,
                (v.channel_id, v.message_id),
                (v.user_id, v.answer_id),
            ),
            Event::GuildScheduledEventCreate(e) => {
                self.remember_event(e.guild_id, e.id, &e.name);
                None
            }
            Event::GuildScheduledEventUpdate(e) => {
                self.remember_event(e.guild_id, e.id, &e.name);
                None
            }
            Event::GuildScheduledEventDelete(e) => {
                self.events.remove(&e.id);
                None
            }
            Event::GuildScheduledEventUserAdd(v) => {
                self.signup(cfg, true, v.guild_scheduled_event_id, v.user_id)
            }
            Event::GuildScheduledEventUserRemove(v) => {
                self.signup(cfg, false, v.guild_scheduled_event_id, v.user_id)
            }
            Event::GuildUpdate(g) => {
                let new = u8::from(g.premium_tier);
                // The first sight of a guild has nothing to compare with.
                let old = self.tiers.insert(g.id, new).filter(|o| *o != new)?;
                Some(Entry {
                    kind: LogKind::Server,
                    event: Some(LogEvent::BoostLevelChange),
                    channel: None,
                    embed: render::boost_level(old, new, g.premium_subscription_count),
                    link: None,
                    actor: None,
                })
            }
            Event::ThreadCreate(t) => {
                self.note_thread(t.id, archived(t));
                None
            }
            Event::ThreadListSync(s) => {
                for t in &s.threads {
                    self.note_thread(t.id, archived(t));
                }
                None
            }
            Event::ThreadDelete(t) => {
                self.active_threads.remove(&t.id);
                None
            }
            Event::ThreadUpdate(t) => {
                self.thread_archived(cfg, t, now);
                None
            }
            Event::VoiceStateUpdate(v) => {
                let (guild, user, new) = (v.0.guild_id?, v.0.user_id, v.0.channel_id);
                let old = self.voice_update(guild, user, new);
                voice_entry(user, old, new)
            }
            _ => None,
        }
    }

    fn audit(&mut self, e: &AuditLogEntry, now: Instant) -> Option<Entry> {
        match e.action_type {
            AuditLogEventType::MemberKick | AuditLogEventType::MemberBanAdd
                if e.user_id == Some(self.me) =>
            {
                None
            }
            AuditLogEventType::MemberKick | AuditLogEventType::MemberBanAdd => {
                if let (Some(guild), Some(user)) = (e.guild_id, e.target_id) {
                    while self
                        .removals
                        .front()
                        .is_some_and(|(at, _, _)| now.duration_since(*at) >= REMOVAL_WINDOW)
                        || self.removals.len() >= MAX_RECENT
                    {
                        self.removals.pop_front();
                    }
                    self.removals.push_back((now, guild, user.cast()));
                }
                render::audit(e)
            }
            AuditLogEventType::MessageDelete => {
                self.remember_delete(e, now);
                None
            }
            AuditLogEventType::ThreadUpdate => {
                if let Some(t) = e.target_id {
                    if self.thread_audits.len() >= MAX_RECENT {
                        self.thread_audits.pop_front();
                    }
                    self.thread_audits.push_back((now, t.cast()));
                }
                render::audit(e)
            }
            _ => render::audit(e),
        }
    }

    /// A poll vote, named with the poll's question and answer when the poll was seen.
    fn vote(
        &self,
        cfg: &LogConfig,
        add: bool,
        guild: Id<GuildMarker>,
        (channel, message): (Id<ChannelMarker>, Id<MessageMarker>),
        (user, answer): (Id<UserMarker>, u8),
    ) -> Option<Entry> {
        let event = if add {
            LogEvent::PollVoteAdd
        } else {
            LogEvent::PollVoteRemove
        };
        // Votes are many: nothing is built unless a channel is picked for them.
        cfg.target(LogKind::Poll, Some(event))?;
        let info = self.polls.get(&(guild, message));
        let (embed, link) =
            render::poll_vote(add, (user, channel, message, answer), guild.get(), info);
        Some(Entry {
            kind: LogKind::Poll,
            event: Some(event),
            channel: Some(channel),
            embed,
            link,
            actor: Some(user),
        })
    }

    /// A scheduled event sign-up, named when the event was seen.
    fn signup(
        &self,
        cfg: &LogConfig,
        add: bool,
        event: Id<ScheduledEventMarker>,
        user: Id<UserMarker>,
    ) -> Option<Entry> {
        let kind = if add {
            LogEvent::GuildScheduledEventSubscribe
        } else {
            LogEvent::GuildScheduledEventUnsubscribe
        };
        cfg.target(LogKind::ScheduledEvent, Some(kind))?;
        let name = self.events.get(&event).map(|e| e.1.as_str());
        Some(Entry {
            kind: LogKind::ScheduledEvent,
            event: Some(kind),
            channel: None,
            embed: render::event_subscribe(add, user, event.get(), name),
            link: None,
            actor: Some(user),
        })
    }

    /// A thread that turned archived while open waits for an audit entry that a person did it;
    /// with none within `HOLD` it was the inactivity timer.
    fn thread_archived(&mut self, cfg: &LogConfig, t: &Channel, now: Instant) {
        if !self.note_thread(t.id, archived(t)) {
            return;
        }
        let Some(guild) = t.guild_id else { return };
        let Some(log) = cfg.target(LogKind::Thread, Some(LogEvent::ThreadAutoArchive)) else {
            return;
        };
        if ignored(cfg, Some(t.id)) || ignored(cfg, t.parent_id) {
            return;
        }
        if self.archives.len() >= MAX_RECENT {
            self.archives.pop_front();
        }
        let (embed, link) = render::thread_auto_archive(t);
        self.archives.push_back(HeldArchive {
            at: now,
            guild,
            log,
            thread: t.id,
            embed,
            link,
        });
    }

    /// Audit entries carry the executor but no content. The gateway delete comes with the
    /// content but no executor, so the two are joined on author and channel.
    fn remember_delete(&mut self, e: &AuditLogEntry, now: Instant) {
        let (Some(executor), Some(target), Some(channel)) = (
            e.user_id,
            e.target_id,
            e.options.as_ref().and_then(|o| o.channel_id),
        ) else {
            return;
        };
        while self.deletes.len() >= MAX_RECENT
            || self
                .deletes
                .front()
                .is_some_and(|d| now.duration_since(d.at) >= DELETER_WINDOW)
        {
            self.deletes.pop_front();
        }
        self.deletes.push_back(AuditDelete {
            at: now,
            executor,
            target: target.cast(),
            channel,
        });
    }

    fn removed_by_audit(&self, guild: Id<GuildMarker>, user: Id<UserMarker>, now: Instant) -> bool {
        self.removals
            .iter()
            .any(|(at, g, u)| *g == guild && *u == user && now.duration_since(*at) < REMOVAL_WINDOW)
    }

    /// The moderator whose audit entry explains a delete seen at `deleted`. An entry names one
    /// delete only, and only one seen before it arrived or within `HOLD` after: a later delete
    /// by the author in that channel is their own and has no audit entry.
    fn deleter(
        &mut self,
        author: Id<UserMarker>,
        channel: Id<ChannelMarker>,
        deleted: Instant,
        now: Instant,
    ) -> Option<Id<UserMarker>> {
        let i = self.deletes.iter().rposition(|d| {
            d.target == author
                && d.channel == channel
                && now.duration_since(d.at) < DELETER_WINDOW
                && deleted <= d.at + HOLD
        })?;
        self.deletes.remove(i).map(|d| d.executor)
    }
}

fn archived(t: &Channel) -> bool {
    t.thread_metadata.as_ref().is_some_and(|m| m.archived)
}

fn published(m: &Message) -> bool {
    m.flags
        .is_some_and(|f| f.contains(MessageFlags::CROSSPOSTED))
}

/// Notes `id` in a bounded list and says whether it was new.
fn first_time(seen: &mut VecDeque<Id<MessageMarker>>, id: Id<MessageMarker>) -> bool {
    if seen.contains(&id) {
        return false;
    }
    if seen.len() >= MAX_KEPT {
        seen.pop_front();
    }
    seen.push_back(id);
    true
}

fn files(a: &[twilight_model::channel::Attachment]) -> Vec<String> {
    a.iter().map(|a| a.filename.clone()).collect()
}

fn voice_entry(
    user: Id<UserMarker>,
    old: Option<Id<ChannelMarker>>,
    new: Option<Id<ChannelMarker>>,
) -> Option<Entry> {
    Some(Entry {
        kind: LogKind::Voice,
        event: None,
        channel: new.or(old),
        embed: render::voice(user, old, new)?,
        link: None,
        actor: Some(user),
    })
}

/// Every test that runs the logger checks each log of facts it makes: the entry's default, filled
/// with the log's values, reads exactly as the log the code built.
#[cfg(test)]
fn reads_as_default(out: &[LogMessage]) {
    for m in out {
        let kind = serde_json::to_value(m.kind).unwrap();
        let title = m.embed.title.clone().unwrap_or_default();
        let group = format!("log.{}", kind.as_str().unwrap());
        let Some(key) = botmsg::log_type_key(&group, &title) else {
            continue;
        };
        if !botmsg::find(&key).is_some_and(|d| d.details()) {
            continue;
        }
        let mut vars = vec![
            ("title", title.as_str()),
            ("text", m.embed.description.as_deref().unwrap_or("")),
        ];
        vars.extend(m.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let r = botmsg::render(&key, None, &vars);
        let e = &r["embeds"][0];
        assert_eq!(e["title"].as_str(), Some(title.as_str()), "{key}");
        assert_eq!(
            e["description"].as_str(),
            m.embed.description.as_deref(),
            "{key}"
        );
    }
}

/// The entry as a message for its log channel, unless it has none or the channel or user is ignored.
fn route(guild: Id<GuildMarker>, cfg: &LogConfig, entry: Entry) -> Option<LogMessage> {
    let channel_id = cfg.target(entry.kind, entry.event)?;
    let skip = ignored(cfg, entry.channel) || cfg.ignores_user(entry.kind, entry.actor);
    let (embed, vars) = entry.embed.parts();
    (!skip).then_some(LogMessage {
        guild,
        channel_id,
        kind: entry.kind,
        embed,
        link: entry.link,
        file: None,
        actors: entry.actor.into_iter().collect(),
        vars,
    })
}

/// Whether a log for this channel is on the ignore list.
fn ignored(cfg: &LogConfig, channel: Option<Id<ChannelMarker>>) -> bool {
    channel.is_some_and(|c| cfg.ignored_channels.contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use twilight_model::{
        gateway::payload::incoming::GuildAuditLogEntryCreate, guild::audit_log::AuditLogEntry,
    };

    const G: Id<GuildMarker> = Id::new(9);

    #[test]
    fn audit_entry_routes_to_role_log_with_changes() {
        let entry: AuditLogEntry = serde_json::from_value(serde_json::json!({
            "action_type": 31,
            "id": "1",
            "user_id": "10",
            "target_id": "20",
            "reason": "cleanup",
            "changes": [{ "key": "name", "old_value": "mods", "new_value": "staff" }]
        }))
        .unwrap();
        let event = Event::GuildAuditLogEntryCreate(Box::new(GuildAuditLogEntryCreate(entry)));

        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let out = Logger::new(Id::new(1)).handle(G, &cfg, &event, Instant::now());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].channel_id, Id::new(5));
        assert_eq!(out[0].kind, LogKind::Role);
        let e = &out[0].embed;
        assert_eq!(e.title.as_deref(), Some("Role Update"));
        let d = e.description.as_deref().unwrap();
        assert!(
            d.starts_with("▸ **Responsible:** <@10> *`10`*\n▸ **Target:** <@&20> *`20`*\n▸ <t:")
        );
        assert!(d.contains("▸ **Reason:** cleanup\n**Changes**\n▸ **Name:**\n```diff\n- mods\n```\n```diff\n+ staff\n```"));
    }

    #[test]
    fn messages_are_cached_only_with_a_message_log() {
        let m: twilight_model::gateway::payload::incoming::MessageCreate = serde_json::from_value(serde_json::json!({
            "id": "5", "channel_id": "3", "guild_id": "9", "content": "hi", "timestamp": "2026-11-17T00:00:00+00:00",
            "author": {"id": "7", "username": "a", "discriminator": "0", "avatar": null},
            "edited_timestamp": null, "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
            "attachments": [], "embeds": [], "pinned": false, "type": 0
        }))
        .unwrap();
        let ev = Event::MessageCreate(Box::new(m));
        let now = Instant::now();
        let on = LogConfig {
            events: std::collections::HashMap::from([(LogEvent::MessageDelete, Id::new(2))]),
            ..Default::default()
        };
        let mut logger = Logger::new(Id::new(1));
        for (cfg, cached) in [(LogConfig::default(), false), (on, true)] {
            logger.handle(G, &cfg, &ev, now);
            assert_eq!(
                logger.cache.get(Id::new(9), Id::new(5), now).is_some(),
                cached
            );
        }
        // An edit while the log is off drops the copy, which would be stale when it is on again.
        let Event::MessageCreate(m) = ev else {
            unreachable!()
        };
        let edit = Event::MessageUpdate(Box::new(
            twilight_model::gateway::payload::incoming::MessageUpdate(m.message),
        ));
        logger.handle(G, &LogConfig::default(), &edit, now);
        assert!(logger.cache.get(Id::new(9), Id::new(5), now).is_none());
    }

    #[test]
    fn unrouted_log_is_dropped() {
        let entry: AuditLogEntry = serde_json::from_value(serde_json::json!({
            "action_type": 22, "id": "1", "user_id": "10", "target_id": "20", "changes": []
        }))
        .unwrap();
        let event = Event::GuildAuditLogEntryCreate(Box::new(GuildAuditLogEntryCreate(entry)));
        let cfg = LogConfig::default();
        assert!(Logger::new(Id::new(1))
            .handle(G, &cfg, &event, Instant::now())
            .is_empty());
    }

    #[test]
    fn voice_join_move_leave_are_logged_mute_is_not() {
        let cfg = LogConfig {
            fallback: Some(Id::new(1)),
            routes: std::collections::HashMap::from([(LogKind::Voice, Id::new(2))]),
            ..Default::default()
        };
        let ev = |channel: Option<&str>| {
            Event::VoiceStateUpdate(Box::new(
                serde_json::from_value(serde_json::json!({
                    "guild_id": "5", "user_id": "7", "channel_id": channel, "session_id": "s",
                    "deaf": false, "mute": false, "self_deaf": false, "self_mute": true,
                    "self_video": false, "suppress": false, "request_to_speak_timestamp": null
                }))
                .unwrap(),
            ))
        };
        let mut l = Logger::new(Id::new(1));
        let mut titles = vec![];
        for c in [Some("10"), Some("10"), Some("11"), None, None] {
            for m in l.handle(G, &cfg, &ev(c), Instant::now()) {
                assert_eq!(m.channel_id, Id::new(2));
                titles.push(m.embed.title.unwrap());
            }
        }
        assert_eq!(titles, ["Voice Join", "Voice Move", "Voice Leave"]);
        // Without its own route, voice never reaches the fallback channel.
        let fallback_only = LogConfig {
            fallback: Some(Id::new(1)),
            ..Default::default()
        };
        assert!(Logger::new(Id::new(1))
            .handle(G, &fallback_only, &ev(Some("10")), Instant::now())
            .is_empty());
    }

    #[test]
    fn seeded_voice_state_reads_a_move_as_a_move() {
        let cfg = LogConfig {
            routes: std::collections::HashMap::from([(LogKind::Voice, Id::new(2))]),
            ..Default::default()
        };
        let mut l = Logger::new(Id::new(1));
        l.seed_voice(
            Id::new(5),
            [(Id::new(7), Id::new(10)), (Id::new(8), Id::new(10))],
        );
        assert_eq!(l.voice_channel(Id::new(5), Id::new(7)), Some(Id::new(10)));
        let t = l.handle(G, &cfg, &voice_ev(7, Some("11")), Instant::now());
        assert_eq!(t[0].embed.title.as_deref(), Some("Voice Move"));
        // A new snapshot drops members who left meanwhile.
        l.seed_voice(Id::new(5), [(Id::new(7), Id::new(11))]);
        assert_eq!(l.voice_channel(Id::new(5), Id::new(8)), None);
    }

    #[test]
    fn a_join_and_a_move_in_either_order_read_as_one_join() {
        // As `events.rs` does: record the update before any await, log it after.
        let cfg = LogConfig {
            routes: std::collections::HashMap::from([(LogKind::Voice, Id::new(2))]),
            ..Default::default()
        };
        let joins = |order: &[Option<u64>]| {
            let mut l = Logger::new(Id::new(1));
            let mut n = 0;
            let recorded: Vec<_> = order
                .iter()
                .map(|c| {
                    let new = c.map(Id::new);
                    let old = l.voice_update(Id::new(5), Id::new(7), new);
                    n += usize::from(old.is_none() && new.is_some());
                    (old, new)
                })
                .collect();
            // The logs may be sent in any order; each still names its own move.
            let titles: Vec<_> = recorded
                .into_iter()
                .rev()
                .flat_map(|m| l.handle_voice(G, &cfg, Id::new(7), m, Instant::now()))
                .map(|m| m.embed.title.unwrap())
                .collect();
            (n, titles)
        };
        assert_eq!(
            joins(&[Some(10), Some(11)]),
            (1, vec!["Voice Move".into(), "Voice Join".into()])
        );
        assert_eq!(joins(&[Some(11), Some(10)]).0, 1);
    }

    fn voice_ev(user: u64, channel: Option<&str>) -> Event {
        Event::VoiceStateUpdate(Box::new(
            serde_json::from_value(serde_json::json!({
                "guild_id": "5", "user_id": user.to_string(), "channel_id": channel,
                "session_id": "s", "deaf": false, "mute": false, "self_deaf": false,
                "self_mute": false, "self_video": false, "suppress": false,
                "request_to_speak_timestamp": null
            }))
            .unwrap(),
        ))
    }

    fn audit_event(action: u16, executor: u64) -> Event {
        let entry: AuditLogEntry = serde_json::from_value(serde_json::json!({
            "action_type": action, "id": "1", "user_id": executor.to_string(), "guild_id": "9",
            "target_id": "20", "options": { "channel_id": "30" }, "changes": []
        }))
        .unwrap();
        Event::GuildAuditLogEntryCreate(Box::new(GuildAuditLogEntryCreate(entry)))
    }

    #[test]
    fn bot_kick_is_not_logged_twice() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let now = Instant::now();
        assert!(Logger::new(Id::new(1))
            .handle(G, &cfg, &audit_event(20, 1), now)
            .is_empty());
        assert_eq!(
            Logger::new(Id::new(1))
                .handle(G, &cfg, &audit_event(20, 10), now)
                .len(),
            1
        );
    }

    #[test]
    fn ignore_lists_drop_logs_of_ignored_users_and_embed_messages() {
        let cfg = LogConfig {
            fallback: Some(Id::new(2)),
            ignored_users: [Id::new(10), Id::new(7), Id::new(20)].into(),
            ignore_embeds: true,
            ..Default::default()
        };
        let now = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        // Audit log: the executor is ignored; another executor is logged and named as actor.
        assert!(logger.handle(G, &cfg, &audit_event(31, 10), now).is_empty());
        let out = logger.handle(G, &cfg, &audit_event(31, 11), now);
        assert_eq!(out[0].actors, vec![Id::new(11)]);
        // Message delete: the cached author is ignored, or the message had an embed.
        let delete = |id| {
            Event::MessageDelete(twilight_model::gateway::payload::incoming::MessageDelete {
                channel_id: Id::new(30),
                guild_id: Some(G),
                id: Id::new(id),
            })
        };
        for (id, author, embeds) in [(1, 7, 0), (2, 8, 1), (3, 8, 0)] {
            let c = CachedMessage::new(Id::new(author), "hi".into(), vec![], embeds, now);
            logger.cache.insert(G, Id::new(id), c, now);
            logger.handle(G, &cfg, &delete(id), now);
        }
        let out = logger.flush(now + HOLD);
        assert_eq!(
            out.len(),
            1,
            "only the plain message of an author not listed"
        );
        assert_eq!(out[0].actors, vec![Id::new(8)]);
        // A member leave of an ignored user is not held.
        logger.handle(G, &cfg, &leave(), now);
        assert!(logger.leaves.is_empty());
    }

    #[test]
    fn ignore_lists_never_hide_security_or_mod_logs() {
        use LogKind::*;
        for k in [
            Report,
            AutoMod,
            AutoModRule,
            AntiNuke,
            Verification,
            JoinGate,
            JoinRaid,
            Panic,
            ModCommand,
            Appeal,
        ] {
            assert!(!k.ignorable(), "{k:?}");
            let cfg = LogConfig {
                ignored_users: [Id::new(10)].into(),
                ..Default::default()
            };
            assert!(!cfg.ignores_user(k, Some(Id::new(10))));
        }
        assert!(Message.ignorable() && Member.ignorable() && Voice.ignorable());
    }

    #[test]
    fn uncached_delete_is_logged_at_once() {
        let cfg = LogConfig {
            fallback: Some(Id::new(2)),
            ..Default::default()
        };
        let delete =
            Event::MessageDelete(twilight_model::gateway::payload::incoming::MessageDelete {
                channel_id: Id::new(30),
                guild_id: Some(Id::new(5)),
                id: Id::new(7),
            });
        let mut logger = Logger::new(Id::new(1));
        let now = Instant::now();
        assert_eq!(logger.handle(G, &cfg, &delete, now).len(), 1);
        assert!(logger.held.is_empty());
    }

    #[test]
    fn moderator_delete_names_the_deleter_within_window() {
        let mut logger = Logger::new(Id::new(1));
        let now = Instant::now();
        logger.handle(G, &LogConfig::default(), &audit_event(72, 10), now);
        assert_eq!(logger.deleter(Id::new(20), Id::new(31), now, now), None);
        assert_eq!(logger.deleter(Id::new(21), Id::new(30), now, now), None);
        assert_eq!(
            logger.deleter(Id::new(20), Id::new(30), now, now + DELETER_WINDOW),
            None
        );
        // A delete seen long after the entry is the author's own.
        let late = now + HOLD + Duration::from_secs(1);
        assert_eq!(logger.deleter(Id::new(20), Id::new(30), late, late), None);
        assert_eq!(
            logger.deleter(Id::new(20), Id::new(30), now, now),
            Some(Id::new(10))
        );
        // The entry named one delete; the author's next delete there is not blamed on it.
        assert_eq!(logger.deleter(Id::new(20), Id::new(30), now, now), None);
    }

    fn user(id: u64) -> twilight_model::user::User {
        serde_json::from_value(serde_json::json!({
            "id": id.to_string(), "username": "u", "discriminator": 0, "avatar": null, "bot": false
        }))
        .unwrap()
    }

    #[test]
    fn app_removal_keeps_kicks_and_rule_logs_have_their_own_kind() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let now = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        let out = logger.handle(G, &cfg, &audit_event(82, 10), now);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, LogKind::Member);
        assert_eq!(out[0].embed.title.as_deref(), Some("App Remove"));
        // A kick is never hidden: a moderator may remove an app and kick a raider right after.
        assert_eq!(logger.handle(G, &cfg, &audit_event(20, 10), now).len(), 1);
        for (action, title) in [
            (140, "Discord AutoMod Rule Created"),
            (141, "Discord AutoMod Rule Updated"),
            (142, "Discord AutoMod Rule Deleted"),
        ] {
            let out = logger.handle(G, &cfg, &audit_event(action, 10), now);
            assert_eq!(out[0].kind, LogKind::AutoModRule);
            assert_eq!(out[0].embed.title.as_deref(), Some(title));
        }
        let out = logger.handle(G, &cfg, &audit_event(121, 10), now);
        assert_eq!(out[0].kind, LogKind::Server);
        assert_eq!(
            out[0].embed.title.as_deref(),
            Some("App Command Permission Update")
        );
        // An ignored executor hides an app removal (a member log) but never a rule change.
        let cfg = LogConfig {
            ignored_users: [Id::new(10)].into(),
            ..cfg
        };
        assert!(logger.handle(G, &cfg, &audit_event(82, 10), now).is_empty());
        assert_eq!(logger.handle(G, &cfg, &audit_event(140, 10), now).len(), 1);
    }

    #[test]
    fn staff_kick_logs_once_as_audit_entry() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let now = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        assert_eq!(logger.handle(G, &cfg, &audit_event(20, 10), now).len(), 1);
        assert!(logger.handle(G, &cfg, &leave(), now).is_empty());
        assert!(logger.flush(now + HOLD).is_empty());
    }

    fn leave() -> Event {
        Event::MemberRemove(twilight_model::gateway::payload::incoming::MemberRemove {
            guild_id: Id::new(9),
            user: user(20),
        })
    }

    #[test]
    fn leave_before_audit_entry_logs_once() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let t0 = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        assert!(logger.handle(G, &cfg, &leave(), t0).is_empty());
        let late = t0 + Duration::from_secs(2);
        assert_eq!(logger.handle(G, &cfg, &audit_event(20, 10), late).len(), 1);
        assert!(logger.flush(t0 + HOLD).is_empty());
    }

    #[test]
    fn plain_leave_logs_after_hold() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let t0 = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        assert!(logger.handle(G, &cfg, &leave(), t0).is_empty());
        assert_eq!(logger.flush(t0 + HOLD).len(), 1);
    }

    #[test]
    fn kick_in_one_guild_keeps_a_leave_from_another() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let t0 = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        logger.handle(G, &cfg, &audit_event(20, 10), t0); // kick in guild 9
        let other = Event::MemberRemove(twilight_model::gateway::payload::incoming::MemberRemove {
            guild_id: Id::new(8),
            user: user(20),
        });
        assert!(logger.handle(G, &cfg, &other, t0).is_empty());
        assert_eq!(logger.flush(t0 + HOLD).len(), 1);
    }

    #[test]
    fn bulk_delete_attaches_cached_content_oldest_first() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let now = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        for (id, text) in [(7, "first"), (8, "second")] {
            let c = CachedMessage::new(Id::new(20), text.into(), vec![], 0, now);
            logger.cache.insert(Id::new(9), Id::new(id), c, now);
        }
        let bulk = |ids: Vec<u64>| {
            Event::MessageDeleteBulk(
                twilight_model::gateway::payload::incoming::MessageDeleteBulk {
                    channel_id: Id::new(3),
                    guild_id: Some(Id::new(9)),
                    ids: ids.into_iter().map(Id::new).collect(),
                },
            )
        };
        let out = logger.handle(G, &cfg, &bulk(vec![8, 99, 7]), now);
        let (name, text) = out[0].file.clone().unwrap();
        assert_eq!(
            (name.as_str(), text.as_str()),
            ("bulk-7.txt", "20: first\n20: second\n")
        );
        assert!(format!("{:?}", out[0].embed).contains("`2/3` saved in `bulk-7.txt` below"));
        assert_eq!(render::bulk_file(Id::new(36 * 36 + 35)), "bulk-10z.txt");
        // Already removed from the cache, so a repeat has nothing to attach.
        assert_eq!(logger.handle(G, &cfg, &bulk(vec![7]), now)[0].file, None);
    }

    #[test]
    fn delete_waits_for_late_audit_entry() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let t0 = Instant::now();
        let mut logger = Logger::new(Id::new(1));
        logger.cache.insert(
            Id::new(9),
            Id::new(7),
            CachedMessage::new(Id::new(20), "hi".into(), vec!["cat.png".into()], 0, t0),
            t0,
        );
        let delete =
            Event::MessageDelete(twilight_model::gateway::payload::incoming::MessageDelete {
                channel_id: Id::new(30),
                guild_id: Some(Id::new(9)),
                id: Id::new(7),
            });
        assert!(logger.handle(G, &cfg, &delete, t0).is_empty());
        let late = t0 + Duration::from_secs(3);
        assert!(logger
            .handle(G, &cfg, &audit_event(72, 10), late)
            .is_empty());
        let out = logger.flush(t0 + HOLD);
        assert_eq!(out.len(), 1);
        let d = out[0].embed.description.as_deref().unwrap();
        assert!(
            d.starts_with(
                "▸ **Responsible:** <@10> *`10`*\n▸ **Author:** <@20> *`20`*\n▸ **Channel:** <#30>"
            ),
            "{d}"
        );
        assert!(
            d.ends_with("```diff\n- hi\n```\n▸ **Attachments:** cat.png"),
            "{d}"
        );
        assert!(logger.flush(t0 + HOLD).is_empty());
    }

    #[test]
    fn custom_type_takes_its_event_channel() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            custom: vec![CustomLog {
                events: vec![LogEvent::MemberKick],
                channel: Id::new(6),
            }],
            ..Default::default()
        };
        let out = Logger::new(Id::new(1)).handle(G, &cfg, &audit_event(20, 10), Instant::now());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].channel_id, Id::new(6));
    }

    // --- Polls, events, stages, boost level and thread archives (L8, L11) ---

    fn message(extra: serde_json::Value) -> twilight_model::channel::Message {
        let mut v = serde_json::json!({
            "id": "5", "channel_id": "3", "guild_id": "9", "content": "", "timestamp": "2026-11-17T00:00:00+00:00",
            "author": {"id": "7", "username": "a", "discriminator": "0", "avatar": null},
            "edited_timestamp": null, "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
            "attachments": [], "embeds": [], "pinned": false, "type": 0
        });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    fn poll_json(finalized: bool) -> serde_json::Value {
        serde_json::json!({"poll": {
            "question": {"text": "Pizza?"}, "allow_multiselect": false, "layout_type": 1,
            "expiry": "2026-11-20T00:00:00+00:00",
            "answers": [
                {"answer_id": 1, "poll_media": {"text": "Yes"}},
                {"answer_id": 2, "poll_media": {"text": "No"}}
            ],
            "results": {"is_finalized": finalized, "answer_counts": [{"id": 1, "count": 3, "me_voted": false}]}
        }})
    }

    fn created(m: twilight_model::channel::Message) -> Event {
        Event::MessageCreate(Box::new(
            twilight_model::gateway::payload::incoming::MessageCreate {
                channel_type: None,
                message: m,
            },
        ))
    }

    fn updated(m: twilight_model::channel::Message) -> Event {
        Event::MessageUpdate(Box::new(
            twilight_model::gateway::payload::incoming::MessageUpdate(m),
        ))
    }

    fn vote(add: bool) -> Event {
        let v = serde_json::json!({"answer_id": 2, "channel_id": "3", "guild_id": "9", "message_id": "5", "user_id": "8"});
        if add {
            Event::MessagePollVoteAdd(serde_json::from_value(v).unwrap())
        } else {
            Event::MessagePollVoteRemove(serde_json::from_value(v).unwrap())
        }
    }

    #[test]
    fn poll_is_logged_when_made_ended_and_deleted() {
        let cfg = LogConfig {
            fallback: Some(Id::new(1)),
            ..Default::default()
        };
        let (mut logger, now) = (Logger::new(Id::new(1)), Instant::now());
        let out = logger.handle(G, &cfg, &created(message(poll_json(false))), now);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, LogKind::Poll);
        assert_eq!(out[0].embed.title.as_deref(), Some("Poll Created"));
        let text = out[0].embed.description.as_deref().unwrap();
        assert!(text.contains("Pizza?") && text.contains("1. Yes") && text.contains("2. No"));
        assert_eq!(out[0].actors, [Id::new(7)]);
        // A vote count update is no edit and no end.
        assert!(logger
            .handle(G, &cfg, &updated(message(poll_json(false))), now)
            .is_empty());
        // The end is logged once with the counts, however often the message is updated.
        let out = logger.handle(G, &cfg, &updated(message(poll_json(true))), now);
        assert_eq!(out[0].embed.title.as_deref(), Some("Poll Ended"));
        assert!(out[0]
            .embed
            .description
            .as_deref()
            .unwrap()
            .contains("1. Yes: **3**"));
        assert!(logger
            .handle(G, &cfg, &updated(message(poll_json(true))), now)
            .is_empty());
        // The delete names the question although no message text was cached, and the author.
        let del = Event::MessageDelete(
            serde_json::from_value(
                serde_json::json!({"id": "5", "channel_id": "3", "guild_id": "9"}),
            )
            .unwrap(),
        );
        assert!(logger.handle(G, &cfg, &del, now).is_empty());
        let out = logger.flush(now + HOLD);
        assert_eq!(out[0].embed.title.as_deref(), Some("Poll Deleted"));
        assert!(out[0]
            .embed
            .description
            .as_deref()
            .unwrap()
            .contains("Pizza?"));
        assert_eq!(out[0].actors, [Id::new(7)]);
        // An ignored author hides the poll delete like any message delete.
        logger.handle(G, &cfg, &created(message(poll_json(false))), now);
        let ignoring = LogConfig {
            ignored_users: [Id::new(7)].into(),
            ..cfg
        };
        logger.handle(G, &ignoring, &del, now);
        assert!(logger.flush(now + HOLD).is_empty());
    }

    #[test]
    fn poll_votes_are_off_until_their_own_channel_is_picked() {
        let mut cfg = LogConfig {
            fallback: Some(Id::new(1)),
            routes: [(LogKind::Poll, Id::new(2))].into(),
            ..Default::default()
        };
        let (mut logger, now) = (Logger::new(Id::new(1)), Instant::now());
        logger.handle(G, &cfg, &created(message(poll_json(false))), now);
        assert!(logger.handle(G, &cfg, &vote(true), now).is_empty());
        cfg.events.insert(LogEvent::PollVoteAdd, Id::new(4));
        let out = logger.handle(G, &cfg, &vote(true), now);
        assert_eq!(out[0].channel_id, Id::new(4));
        assert_eq!(out[0].embed.title.as_deref(), Some("Poll Vote Added"));
        let text = out[0].embed.description.as_deref().unwrap();
        assert!(text.contains("<@8>") && text.contains("Pizza?") && text.contains("No"));
        // Taking a vote back has its own event, still off.
        assert!(logger.handle(G, &cfg, &vote(false), now).is_empty());
        // A poll the logger never saw shows the answer number.
        let out = Logger::new(Id::new(1)).handle(G, &cfg, &vote(true), now);
        assert!(out[0]
            .embed
            .description
            .as_deref()
            .unwrap()
            .contains("Answer 2"));
        // The ignore lists hide a voter.
        cfg.ignored_users.insert(Id::new(8));
        assert!(logger.handle(G, &cfg, &vote(true), now).is_empty());
    }

    #[test]
    fn a_message_is_logged_once_when_published() {
        let cfg = LogConfig {
            fallback: Some(Id::new(1)),
            events: [(LogEvent::MessageUpdate, Id::new(1))].into(),
            ..Default::default()
        };
        let (mut logger, now) = (Logger::new(Id::new(1)), Instant::now());
        logger.handle(
            G,
            &cfg,
            &created(message(serde_json::json!({"content": "news"}))),
            now,
        );
        let sent = message(serde_json::json!({"content": "news", "flags": 1}));
        let out = logger.handle(G, &cfg, &updated(sent.clone()), now);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].embed.title.as_deref(), Some("Message Published"));
        assert!(logger.handle(G, &cfg, &updated(sent), now).is_empty());
        // Without a copy (restart, expired) an update of an announcement is an edit, never a publish.
        let mut fresh = Logger::new(Id::new(1));
        let other = message(serde_json::json!({"id": "6", "flags": 1}));
        let out = fresh.handle(G, &cfg, &updated(other), now);
        assert!(out
            .iter()
            .all(|m| m.embed.title.as_deref() != Some("Message Published")));
    }

    #[test]
    fn event_sign_ups_name_the_event_and_need_their_own_channel() {
        let mut cfg = LogConfig {
            fallback: Some(Id::new(1)),
            routes: [(LogKind::ScheduledEvent, Id::new(2))].into(),
            ..Default::default()
        };
        let (mut logger, now) = (Logger::new(Id::new(1)), Instant::now());
        let up = |add: bool| {
            let v = serde_json::json!({"guild_id": "9", "guild_scheduled_event_id": "40", "user_id": "8"});
            if add {
                Event::GuildScheduledEventUserAdd(serde_json::from_value(v).unwrap())
            } else {
                Event::GuildScheduledEventUserRemove(serde_json::from_value(v).unwrap())
            }
        };
        assert!(logger.handle(G, &cfg, &up(true), now).is_empty());
        cfg.events
            .insert(LogEvent::GuildScheduledEventSubscribe, Id::new(3));
        let out = logger.handle(G, &cfg, &up(true), now);
        assert_eq!(out[0].channel_id, Id::new(3));
        assert_eq!(out[0].embed.title.as_deref(), Some("Event Subscribed"));
        assert!(out[0]
            .embed
            .description
            .as_deref()
            .unwrap()
            .contains("`40`"));
        logger.remember_event(G, Id::new(40), "Movie night");
        let out = logger.handle(G, &cfg, &up(true), now);
        assert!(out[0]
            .embed
            .description
            .as_deref()
            .unwrap()
            .contains("Movie night"));
        assert!(logger.handle(G, &cfg, &up(false), now).is_empty());
    }

    #[test]
    fn a_stage_has_start_update_and_end_logs_in_the_channel_group() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let (mut logger, now) = (Logger::new(Id::new(1)), Instant::now());
        for (action, title, event) in [
            (83, "Stage Start", LogEvent::StageStart),
            (84, "Stage Update", LogEvent::StageUpdate),
            (85, "Stage End", LogEvent::StageEnd),
        ] {
            let out = logger.handle(G, &cfg, &audit_event(action, 10), now);
            assert_eq!(out[0].kind, LogKind::Channel);
            assert_eq!(out[0].embed.title.as_deref(), Some(title));
            assert_eq!(
                LogEvent::from_action(AuditLogEventType::from(action)),
                Some(event)
            );
            // The target is a stage instance, not a channel: the Channel line names the stage.
            let text = out[0].embed.description.as_deref().unwrap();
            assert!(text.contains("<#30>") && !text.contains("Target"));
        }
    }

    fn guild_update(tier: u8) -> Event {
        let g = serde_json::json!({
            "afk_channel_id": null, "afk_timeout": 300, "application_id": null, "banner": null,
            "default_message_notifications": 0, "description": null, "discovery_splash": null,
            "emojis": [], "explicit_content_filter": 0, "features": [], "icon": null, "id": "9",
            "mfa_level": 0, "name": "S", "nsfw_level": 0, "owner_id": "7", "preferred_locale": "en-US",
            "premium_progress_bar_enabled": false, "premium_subscription_count": 7, "premium_tier": tier,
            "public_updates_channel_id": null, "roles": [], "rules_channel_id": null, "splash": null,
            "system_channel_flags": 0, "system_channel_id": null, "verification_level": 0,
            "vanity_url_code": null
        });
        Event::GuildUpdate(Box::new(serde_json::from_value(g).unwrap()))
    }

    #[test]
    fn only_a_changed_boost_level_is_logged() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let (mut logger, now) = (Logger::new(Id::new(1)), Instant::now());
        // The first sight has nothing to compare with; an update without a change logs nothing.
        assert!(logger.handle(G, &cfg, &guild_update(1), now).is_empty());
        assert!(logger.handle(G, &cfg, &guild_update(1), now).is_empty());
        let out = logger.handle(G, &cfg, &guild_update(2), now);
        assert_eq!(out[0].kind, LogKind::Server);
        assert_eq!(out[0].embed.title.as_deref(), Some("Boost Level Changed"));
        assert!(out[0]
            .embed
            .description
            .as_deref()
            .unwrap()
            .contains("1 → 2"));
    }

    fn thread(archived: bool) -> twilight_model::channel::Channel {
        serde_json::from_value(serde_json::json!({
            "id": "50", "guild_id": "9", "parent_id": "3", "type": 11, "name": "talk",
            "thread_metadata": {"archived": archived, "auto_archive_duration": 60,
                "archive_timestamp": "2026-11-19T00:00:00+00:00", "locked": false}
        }))
        .unwrap()
    }

    fn thread_event(archived: bool, create: bool) -> Event {
        let t = thread(archived);
        if create {
            Event::ThreadCreate(Box::new(
                twilight_model::gateway::payload::incoming::ThreadCreate(t),
            ))
        } else {
            Event::ThreadUpdate(Box::new(
                twilight_model::gateway::payload::incoming::ThreadUpdate(t),
            ))
        }
    }

    #[test]
    fn a_thread_archived_with_no_audit_entry_is_logged_as_automatic() {
        let cfg = LogConfig {
            fallback: Some(Id::new(5)),
            ..Default::default()
        };
        let now = Instant::now();
        let run = |person: bool| {
            let mut logger = Logger::new(Id::new(1));
            logger.handle(G, &cfg, &thread_event(false, true), now);
            assert!(logger
                .handle(G, &cfg, &thread_event(true, false), now)
                .is_empty());
            if person {
                // The audit entry of a person's archive (thread update, target 50).
                let entry: AuditLogEntry = serde_json::from_value(serde_json::json!({
                    "action_type": 111, "id": "1", "user_id": "10", "guild_id": "9", "target_id": "50", "changes": []
                })).unwrap();
                let ev = Event::GuildAuditLogEntryCreate(Box::new(GuildAuditLogEntryCreate(entry)));
                assert_eq!(logger.handle(G, &cfg, &ev, now).len(), 1);
            }
            assert!(logger.flush(now + HOLD / 2).is_empty());
            let out = logger.flush(now + HOLD);
            // The same archived thread updating again is not a new archive.
            logger.handle(G, &cfg, &thread_event(true, false), now + HOLD);
            (out, logger.flush(now + HOLD * 3))
        };
        let (out, later) = run(false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, LogKind::Thread);
        assert_eq!(
            out[0].embed.title.as_deref(),
            Some("Thread Archived Automatically")
        );
        assert!(later.is_empty());
        assert!(run(true).0.is_empty());
        // A thread never seen open (archived before this run) is not logged.
        let mut logger = Logger::new(Id::new(1));
        logger.handle(G, &cfg, &thread_event(true, false), now);
        assert!(logger.flush(now + HOLD).is_empty());
    }
}

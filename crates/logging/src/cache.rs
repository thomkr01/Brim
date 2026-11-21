use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};
use twilight_model::id::{
    marker::{GuildMarker, MessageMarker, UserMarker},
    Id,
};

/// Discord does not send deleted or old message content, so we keep recent messages ourselves.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Per guild (about 15 MB at full size). Oldest entries go first once this is reached.
pub const CAPACITY: usize = 50_000;
/// Per guild: a flood of long messages reaches this before CAPACITY (about 4k messages of 4,000 bytes).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// All guilds together, so memory does not grow with the number of servers (300 at MAX_BYTES
/// would be about 5 GB). Over it, the guild holding the most loses its oldest messages first.
pub const MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;
/// Map, queue and struct memory of one entry, counted with its text so many short messages
/// cannot pass the byte caps unseen.
const OVERHEAD: usize = 160;

#[derive(Clone, Debug)]
pub struct CachedMessage {
    pub author: Id<UserMarker>,
    pub content: String,
    /// File names of the message's attachments.
    pub attachments: Vec<String>,
    /// How many embeds it had (a count only: its size is fixed, so the byte budget skips it).
    pub embeds: usize,
    /// Whether the message was published (crossposted) when it was cached.
    pub published: bool,
    inserted: Instant,
}

impl CachedMessage {
    /// The content as logs show it: the text, then "(1 embed)" when it had embeds, so an
    /// embed-only message is not an empty "-" line.
    pub fn text(&self) -> String {
        shown(&self.content, self.embeds)
    }

    fn size(&self) -> usize {
        OVERHEAD + self.content.len() + self.attachments.iter().map(String::len).sum::<usize>()
    }

    pub fn new(
        author: Id<UserMarker>,
        content: String,
        attachments: Vec<String>,
        embeds: usize,
        now: Instant,
    ) -> Self {
        Self {
            author,
            content,
            attachments,
            embeds,
            published: false,
            inserted: now,
        }
    }
}

/// See [`CachedMessage::text`]; for a message that is not cached.
pub fn shown(content: &str, embeds: usize) -> String {
    let note = match embeds {
        0 => return content.into(),
        1 => "(1 embed)".to_string(),
        n => format!("({n} embeds)"),
    };
    if content.is_empty() {
        note
    } else {
        format!("{content}\n{note}")
    }
}

#[derive(Default)]
struct GuildMessages {
    map: HashMap<Id<MessageMarker>, CachedMessage>,
    /// Insertion order, used for eviction. An id can appear more than once after an edit.
    order: VecDeque<(Instant, Id<MessageMarker>)>,
    /// Text bytes held in `map`.
    bytes: usize,
}

pub struct MessageCache {
    guilds: HashMap<Id<GuildMarker>, GuildMessages>,
    /// The `bytes` of all guilds.
    bytes: usize,
    max_bytes: usize,
}

impl Default for MessageCache {
    fn default() -> Self {
        Self {
            guilds: HashMap::new(),
            bytes: 0,
            max_bytes: MAX_TOTAL_BYTES,
        }
    }
}

impl MessageCache {
    pub fn insert(
        &mut self,
        guild: Id<GuildMarker>,
        id: Id<MessageMarker>,
        msg: CachedMessage,
        now: Instant,
    ) {
        let g = self.guilds.entry(guild).or_default();
        let before = g.bytes;
        g.order.push_back((msg.inserted, id));
        g.bytes += msg.size();
        if let Some(old) = g.map.insert(id, msg) {
            g.bytes -= old.size();
        }
        g.prune(now);
        self.bytes = self.bytes + g.bytes - before;
        while self.bytes > self.max_bytes {
            // shortcut: a scan of all guilds, only while over the cap; a heap if this gets slow.
            let Some(g) = self.guilds.values_mut().max_by_key(|g| g.bytes) else {
                break;
            };
            let freed = g.evict_oldest();
            if freed == 0 {
                break;
            }
            self.bytes -= freed;
        }
    }

    /// Expired entries are treated as missing, even before they are pruned.
    pub fn get(
        &self,
        guild: Id<GuildMarker>,
        id: Id<MessageMarker>,
        now: Instant,
    ) -> Option<&CachedMessage> {
        self.guilds
            .get(&guild)?
            .map
            .get(&id)
            .filter(|m| now.duration_since(m.inserted) < TTL)
    }

    /// Like `get`, an expired entry is missing: its content is past the 24 hours PRIVACY.md promises.
    pub fn remove(
        &mut self,
        guild: Id<GuildMarker>,
        id: Id<MessageMarker>,
        now: Instant,
    ) -> Option<CachedMessage> {
        let g = self.guilds.get_mut(&guild)?;
        let m = g.map.remove(&id)?;
        g.bytes -= m.size();
        self.bytes -= m.size();
        (now.duration_since(m.inserted) < TTL).then_some(m)
    }

    /// Drops expired messages of every guild, and guilds with none left (a quiet guild, or one
    /// the bot left, is never pruned by `insert`).
    pub fn prune_all(&mut self, now: Instant) {
        let mut freed = 0;
        self.guilds.retain(|_, g| {
            let before = g.bytes;
            g.prune(now);
            freed += before - g.bytes;
            !g.map.is_empty()
        });
        self.bytes -= freed;
    }
}

impl GuildMessages {
    /// Drops the oldest live message and returns its size (0 when none is left).
    fn evict_oldest(&mut self) -> usize {
        while let Some((at, id)) = self.order.pop_front() {
            if self.map.get(&id).is_some_and(|m| m.inserted == at) {
                if let Some(m) = self.map.remove(&id) {
                    self.bytes -= m.size();
                    return m.size();
                }
            }
        }
        0
    }

    fn prune(&mut self, now: Instant) {
        while let Some(&(at, id)) = self.order.front() {
            let expired = now.duration_since(at) >= TTL;
            if !expired && self.order.len() <= CAPACITY && self.bytes <= MAX_BYTES {
                break;
            }
            self.order.pop_front();
            // Skip stale order entries left behind by an edit: the live copy has a newer timestamp.
            if self.map.get(&id).is_some_and(|m| m.inserted == at) {
                if let Some(m) = self.map.remove(&id) {
                    self.bytes -= m.size();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(now: Instant) -> CachedMessage {
        CachedMessage::new(Id::new(9), "hi".into(), vec![], 0, now)
    }

    #[test]
    fn text_shows_embeds() {
        assert_eq!(shown("hi", 0), "hi");
        assert_eq!(shown("", 1), "(1 embed)");
        assert_eq!(shown("hi", 2), "hi\n(2 embeds)");
    }

    #[test]
    fn expires_after_ttl() {
        let t0 = Instant::now();
        let mut c = MessageCache::default();
        c.insert(Id::new(1), Id::new(2), msg(t0), t0);
        assert!(c
            .get(Id::new(1), Id::new(2), t0 + TTL - Duration::from_secs(1))
            .is_some());
        assert!(c.get(Id::new(1), Id::new(2), t0 + TTL).is_none());
    }

    #[test]
    fn expired_is_not_removed_and_quiet_guilds_go() {
        let t0 = Instant::now();
        let mut c = MessageCache::default();
        c.insert(Id::new(1), Id::new(2), msg(t0), t0);
        c.insert(Id::new(1), Id::new(3), msg(t0), t0);
        assert!(c.remove(Id::new(1), Id::new(2), t0 + TTL).is_none());
        c.prune_all(t0 + TTL - Duration::from_secs(1));
        assert!(c.guilds.contains_key(&Id::new(1)));
        c.prune_all(t0 + TTL);
        assert!(c.guilds.is_empty());
    }

    #[test]
    fn evicts_oldest_over_capacity() {
        let t0 = Instant::now();
        let mut c = MessageCache::default();
        for i in 0..=CAPACITY as u64 {
            c.insert(Id::new(1), Id::new(i + 1), msg(t0), t0);
        }
        assert!(c.get(Id::new(1), Id::new(1), t0).is_none());
        assert!(c
            .get(Id::new(1), Id::new(CAPACITY as u64 + 1), t0)
            .is_some());
    }

    #[test]
    fn evicts_oldest_over_byte_budget() {
        let t0 = Instant::now();
        let mut c = MessageCache::default();
        let big = || CachedMessage::new(Id::new(9), "x".repeat(4000), vec![], 0, t0);
        let n = (MAX_BYTES / 4000) as u64 + 1;
        for i in 1..=n {
            c.insert(Id::new(1), Id::new(i), big(), t0);
        }
        assert!(c.get(Id::new(1), Id::new(1), t0).is_none());
        assert!(c.get(Id::new(1), Id::new(n), t0).is_some());
        let g = &c.guilds[&Id::new(1)];
        assert!(g.bytes <= MAX_BYTES);
        assert_eq!(
            g.bytes,
            g.map.values().map(CachedMessage::size).sum::<usize>()
        );
    }

    #[test]
    fn total_cap_takes_from_the_biggest_guild() {
        let t0 = Instant::now();
        let mut c = MessageCache {
            max_bytes: 10 * 1000,
            ..Default::default()
        };
        let big = || CachedMessage::new(Id::new(9), "x".repeat(1000 - OVERHEAD), vec![], 0, t0);
        // Guild 1 holds 8 messages, guild 2 two; the next write to guild 2 goes over the cap.
        for i in 1..=8 {
            c.insert(Id::new(1), Id::new(i), big(), t0);
        }
        for i in 11..=13 {
            c.insert(Id::new(2), Id::new(i), big(), t0);
        }
        assert!(c.get(Id::new(1), Id::new(1), t0).is_none());
        assert!(c.get(Id::new(1), Id::new(2), t0).is_some());
        assert!(c.get(Id::new(2), Id::new(11), t0).is_some());
        assert_eq!(c.bytes, 10 * 1000);
        assert_eq!(c.bytes, c.guilds.values().map(|g| g.bytes).sum::<usize>());
        c.remove(Id::new(2), Id::new(13), t0);
        c.prune_all(t0 + TTL);
        assert_eq!(c.bytes, 0);
    }

    #[test]
    fn edit_keeps_newest_copy() {
        let t0 = Instant::now();
        let mut c = MessageCache::default();
        c.insert(Id::new(1), Id::new(2), msg(t0), t0);
        let t1 = t0 + TTL - Duration::from_secs(1);
        c.insert(Id::new(1), Id::new(2), msg(t1), t1);
        // The first order entry expires now, but the edited copy must survive it.
        let t2 = t0 + TTL + Duration::from_secs(1);
        c.insert(Id::new(1), Id::new(3), msg(t2), t2);
        assert!(c.get(Id::new(1), Id::new(2), t2).is_some());
    }
}

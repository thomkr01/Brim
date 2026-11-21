//! Purge: pages back through a channel until `count` messages match every filter or 1000 were scanned.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use twilight_model::{
    channel::Message,
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker},
        Id,
    },
};

use crate::{
    app::{now_secs, App},
    automod::count_emojis,
    modcmds::Anchor,
};

pub const MAX: usize = 1000;
/// Discord refuses bulk deletes of messages older than 14 days (a minute of margin).
const BULK_AGE_S: i64 = 14 * 86_400 - 60;

/// Purge types plus the role and webhook targets. A purge keeps messages matching all of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    Embeds,
    Suspicious,
    Bots,
    Mentions,
    Reactions,
    Emojis,
    Text,
    NoAvatar,
    NoRole,
    Attachments,
    Links,
    After(u64),
    Before(u64),
    Equals(String),
    Has(String),
    Starts(String),
    Webhook(u64),
    Role(u64),
}

pub const TYPES: &str = "embeds, suspicious, bots, mentions, reactions, emojis, text, noavatar, norole, attachments, links, after ID, before ID, equals TEXT, has TEXT, starts TEXT, webhook ID";

impl Filter {
    /// `?t TYPE [input]` with aliases.
    pub fn parse(kind: &str, input: Option<&str>) -> Result<Self, String> {
        let text = || {
            input
                .map(str::to_lowercase)
                .ok_or_else(|| format!("`{kind}` needs text after it"))
        };
        let id = || {
            input
                .and_then(|i| i.trim().parse().ok())
                .ok_or_else(|| format!("`{kind}` needs a message or webhook id after it"))
        };
        Ok(match kind {
            "embeds" | "embed" | "e" => Self::Embeds,
            "suspicious" | "sus" | "-sus" => Self::Suspicious,
            "bots" | "bot" | "b" | "-bot" | "robot" | "robots" => Self::Bots,
            "mentions" | "mention" | "ping" | "pings" | "m" | "-p" | "@" => Self::Mentions,
            "reactions" | "reaction" | "react" | "reacts" | "r" | "-r" => Self::Reactions,
            "emojis" | "emoji" | "emotes" => Self::Emojis,
            "text" | "txt" | "t" | "-txt" | "noimage" | "noembed" => Self::Text,
            "noavatar" | "nopfp" | "nopic" | "noav" | "-na" => Self::NoAvatar,
            "norole" | "noroles" | "stripped" | "-nr" => Self::NoRole,
            "attachments" | "attachment" | "files" | "images" | "a" | "-pic" | "image"
            | "attach" | "img" => Self::Attachments,
            "links" | "link" | "urls" | "l" | "-l" => Self::Links,
            "after" | "-a" => Self::After(id()?),
            "before" | "-b" => Self::Before(id()?),
            "equals" | "equal" | "eq" | "is" | "=" | "match" => Self::Equals(text()?),
            "has" | "contains" | "includes" | "-h" => Self::Has(text()?),
            "starts" | "start" | "startswith" | "prefix" | "-s" => Self::Starts(text()?),
            "webhook" | "webhooks" | "wh" => Self::Webhook(id()?),
            _ => return Err(format!("Unknown purge type `{kind}`. Types: {TYPES}")),
        })
    }

    /// How the filter reads in the purge note and log.
    fn label(&self) -> String {
        match self {
            Self::NoAvatar => "no avatar".into(),
            Self::NoRole => "no role".into(),
            Self::After(id) => format!("after message `{id}`"),
            Self::Before(id) => format!("before message `{id}`"),
            Self::Equals(s) => format!("equals `{s}`"),
            Self::Has(s) => format!("contains `{s}`"),
            Self::Starts(s) => format!("starts with `{s}`"),
            Self::Webhook(id) => format!("webhook `{id}`"),
            Self::Role(id) => format!("role <@&{id}>"),
            other => format!("{other:?}").to_lowercase(),
        }
    }

    fn needs_roles(&self) -> bool {
        matches!(self, Self::NoRole | Self::Role(_))
    }
}

/// The parts of a message the filters look at.
#[derive(Debug, Default)]
pub struct Msg {
    pub id: u64,
    pub author: u64,
    pub ts: i64,
    pub bot: bool,
    pub avatar: bool,
    pub webhook: Option<u64>,
    pub content: String,
    pub embeds: usize,
    pub attachments: usize,
    pub reactions: usize,
    pub mentions: usize,
}

impl From<&Message> for Msg {
    fn from(m: &Message) -> Self {
        Self {
            id: m.id.get(),
            author: m.author.id.get(),
            ts: m.timestamp.as_secs(),
            bot: m.author.bot,
            avatar: m.author.avatar.is_some(),
            webhook: m.webhook_id.map(|w| w.get()),
            content: m.content.clone(),
            embeds: m.embeds.len(),
            attachments: m.attachments.len(),
            reactions: m.reactions.len(),
            mentions: m.mentions.len() + m.mention_roles.len() + usize::from(m.mention_everyone),
        }
    }
}

/// An author's roles from a member read: one who left has none (so `norole` matches them,
/// Owner decision 11-17); a failed read is unknown and matches no role filter.
fn known_roles(read: anyhow::Result<Option<Vec<u64>>>) -> Option<Vec<u64>> {
    read.ok().map(Option::unwrap_or_default)
}

/// `roles` is the author's roles (None when not a member, or not looked up); `min_age_s` is join gate's minimum age.
fn matches(f: &Filter, m: &Msg, roles: Option<&[u64]>, min_age_s: i64, now: i64) -> bool {
    let text = m.content.to_lowercase();
    match f {
        Filter::Embeds => m.embeds > 0,
        Filter::Suspicious => {
            let created = ((m.author >> 22) + 1_420_070_400_000) as i64 / 1000;
            !m.avatar || now - created < min_age_s
        }
        Filter::Bots => m.bot,
        Filter::Mentions => m.mentions > 0,
        Filter::Reactions => m.reactions > 0,
        Filter::Emojis => count_emojis(&m.content) > 0,
        Filter::Text => m.embeds == 0 && m.attachments == 0 && !m.content.is_empty(),
        Filter::NoAvatar => !m.avatar,
        Filter::NoRole => roles.is_some_and(|r| r.is_empty()),
        Filter::Attachments => m.attachments > 0,
        Filter::Links => text.contains("http://") || text.contains("https://"),
        Filter::After(id) => m.id > *id,
        Filter::Before(id) => m.id < *id,
        Filter::Equals(s) => text == *s,
        Filter::Has(s) => text.contains(s.as_str()),
        Filter::Starts(s) => text.starts_with(s.as_str()),
        Filter::Webhook(id) => m.webhook == Some(*id),
        Filter::Role(id) => roles.is_some_and(|r| r.contains(id)),
    }
}

/// The filters of a purge as one line: "all messages" when there are none.
pub fn describe(user: Option<u64>, filters: &[Filter], from: Option<Anchor>) -> String {
    let mut parts: Vec<String> = user.map(|u| format!("from <@{u}>")).into_iter().collect();
    parts.extend(filters.iter().map(Filter::label));
    parts.extend(from.map(|a| match a {
        Anchor::From(m) => format!("from message `{m}` down"),
        Anchor::Until(m) => format!("above message `{m}`"),
    }));
    match parts.is_empty() {
        true => "all messages".into(),
        false => parts.join(", "),
    }
}

/// What a purge did: `too_old` matched but is past the 14 days Discord allows to bulk delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purged {
    pub deleted: usize,
    pub too_old: usize,
    /// Bulk delete files (name, text) to attach to the mod log entry instead of their own log.
    pub files: Vec<(String, String)>,
}

/// Picks up to `count` messages from `msgs` (newest first) by `user` matching every filter:
/// the ids younger than 14 days, and how many matched but are older.
fn pick(
    msgs: &[Msg],
    count: usize,
    user: Option<u64>,
    filters: &[Filter],
    roles: &HashMap<u64, Vec<u64>>,
    min_age_s: i64,
    now: i64,
) -> (Vec<u64>, usize) {
    let (young, old): (Vec<&Msg>, Vec<&Msg>) = msgs
        .iter()
        .filter(|m| user.is_none_or(|u| u == m.author))
        .filter(|m| {
            filters.iter().all(|f| {
                matches(
                    f,
                    m,
                    roles.get(&m.author).map(Vec::as_slice),
                    min_age_s,
                    now,
                )
            })
        })
        .take(count)
        .partition(|m| now - m.ts < BULK_AGE_S);
    (young.iter().map(|m| m.id).collect(), old.len())
}

/// A purge menu anchor reads one page from the clicked message.
pub async fn run(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    (count, own, cutoff): (usize, &[u64], Option<u64>),
    user: Option<u64>,
    filters: &[Filter],
    from: Option<Anchor>,
) -> Result<Purged> {
    let min_age_s = app.cfg(gid.get()).await?.joingate.min_age_ms() / 1000;
    let now = now_secs();
    let mut roles: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut looked = HashSet::new();
    let mut ids = vec![];
    let mut too_old = 0;
    // `count` messages from before the command; its own messages go too, uncounted.
    let mut before: Option<u64> = cutoff.filter(|_| from.is_none());
    let mut scanned = 0;
    while ids.len() < count && scanned < MAX {
        let req = app.http.channel_messages(ch).limit(100);
        let page = match (from, before) {
            // `after` is exclusive, so start one id earlier to include the clicked message.
            (Some(Anchor::From(m)), _) => req.after(Id::new(m.max(2) - 1)).await?,
            (Some(Anchor::Until(m)), None) => req.before(Id::new(m)).await?,
            (_, Some(b)) => req.before(Id::new(b)).await?,
            (None, None) => req.await?,
        };
        let msgs: Vec<Msg> = page.models().await?.iter().map(Msg::from).collect();
        if filters.iter().any(Filter::needs_roles) {
            use futures_util::StreamExt;
            // Webhooks are no members; each author is read once, a few at a time.
            let new: HashSet<u64> = (msgs.iter())
                .filter(|m| m.webhook.is_none() && looked.insert(m.author))
                .map(|m| m.author)
                .collect();
            let read: Vec<_> =
                futures_util::stream::iter(new)
                    .map(|a| async move {
                        (a, crate::http_err::member(&app.http, gid, Id::new(a)).await)
                    })
                    .buffer_unordered(5)
                    .collect()
                    .await;
            for (a, member) in read {
                let member = member.map(|m| m.map(|m| m.roles.iter().map(|r| r.get()).collect()));
                if let Some(r) = known_roles(member) {
                    roles.insert(a, r);
                }
            }
        }
        scanned += msgs.len();
        let (young, old) = pick(
            &msgs,
            count - ids.len(),
            user,
            filters,
            &roles,
            min_age_s,
            now,
        );
        ids.extend(young);
        too_old += old;
        let oldest = msgs.iter().map(|m| (m.id, m.ts)).min();
        match oldest {
            Some((id, ts)) if from.is_none() && msgs.len() == 100 && now - ts < BULK_AGE_S => {
                before = Some(id)
            }
            _ => break,
        }
    }
    let all = to_delete(&ids, own);
    let bulks = all.chunks(100).filter(|c| c.len() > 1).count();
    let watch = Watch::start(&all);
    let deleted = async {
        for chunk in all.chunks(100) {
            let chunk: Vec<Id<MessageMarker>> = chunk.iter().map(|i| Id::new(*i)).collect();
            match chunk.as_slice() {
                // The command's own message: someone may have deleted it already.
                [one] if own.contains(&one.get()) => {
                    let r = app.http.delete_message(ch, *one).await;
                    crate::http_err::best_effort(r, "delete the purge command");
                }
                [one] => {
                    app.http.delete_message(ch, *one).await?;
                }
                // Discord counts unknown ids in a bulk delete and skips them.
                many => {
                    app.http.delete_messages(ch, many).await?;
                }
            }
        }
        anyhow::Ok(())
    }
    .await;
    let held = watch.take(bulks).await;
    if let Err(e) = deleted {
        // No case follows, so the bulk logs it held are sent as they were.
        for m in &held {
            crate::events::send_held(app, m).await;
        }
        return Err(e);
    }
    Ok(Purged {
        deleted: ids.len(),
        too_old: too_old.min(count - ids.len()),
        files: held.into_iter().filter_map(|m| m.file).collect(),
    })
}

/// The matched ids and the command's own messages, each once.
fn to_delete(ids: &[u64], own: &[u64]) -> Vec<u64> {
    let mut all = ids.to_vec();
    all.extend(own.iter().filter(|o| !ids.contains(o)));
    all
}

/// One running purge: the ids it deletes, the bulk deletes of them seen, and the logs held
/// back for its mod log entry when both share a log channel (owner decision 11-17: one entry).
struct Watched {
    key: u64,
    ids: HashSet<u64>,
    seen: usize,
    held: Vec<brim_logging::LogMessage>,
}

/// Every running purge; a bulk delete belongs to the one whose ids it holds.
static WATCHED: std::sync::Mutex<Vec<Watched>> = std::sync::Mutex::new(Vec::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A purge's watch on its bulk deletes; dropped, they log as usual again.
struct Watch(u64);

impl Watch {
    fn start(ids: &[u64]) -> Self {
        let key = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        WATCHED.lock().unwrap().push(Watched {
            key,
            ids: ids.iter().copied().collect(),
            seen: 0,
            held: vec![],
        });
        Self(key)
    }

    fn with<T>(&self, f: impl FnOnce(&mut Watched) -> T) -> Option<T> {
        WATCHED
            .lock()
            .unwrap()
            .iter_mut()
            .find(|w| w.key == self.0)
            .map(f)
    }

    /// Waits up to 3 s for the gateway to report `bulks` bulk deletes, then takes the held logs.
    async fn take(self, bulks: usize) -> Vec<brim_logging::LogMessage> {
        for _ in 0..30 {
            if self.with(|w| w.seen).unwrap_or(bulks) >= bulks {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Removed in the same lock: a later bulk delete finds no watch and logs as usual.
        let mut all = WATCHED.lock().unwrap();
        let at = all.iter().position(|w| w.key == self.0);
        at.map(|i| all.swap_remove(i).held).unwrap_or_default()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        WATCHED.lock().unwrap().retain(|w| w.key != self.0);
    }
}

/// Gateway side of a bulk delete of `ids`: during the purge that deleted them, the logs bound
/// for the mod log channel `mod_log` are held for the purge's entry instead of being sent.
pub fn on_bulk(
    ids: &[u64],
    logs: &mut Vec<brim_logging::LogMessage>,
    mod_log: Option<Id<ChannelMarker>>,
) {
    let mut all = WATCHED.lock().unwrap();
    let Some(w) = (all.iter_mut()).find(|w| ids.first().is_some_and(|i| w.ids.contains(i))) else {
        return;
    };
    w.seen += 1;
    let (held, keep): (Vec<_>, Vec<_>) = std::mem::take(logs)
        .into_iter()
        .partition(|m| Some(m.channel_id) == mod_log);
    *logs = keep;
    w.held.extend(held);
}

#[cfg(test)]
mod tests {
    use super::*;
    use brim_logging::embed::Log;

    fn msg(id: u64, author: u64, ts: i64) -> Msg {
        Msg {
            id,
            author,
            ts,
            avatar: true,
            ..Default::default()
        }
    }

    #[test]
    fn picks_by_user_age_count() {
        let now = 10_000_000;
        let old = now - BULK_AGE_S - 1;
        let msgs = [
            msg(5, 1, now),
            msg(4, 2, now - 5),
            msg(3, 1, now - 10),
            msg(2, 1, old),
        ];
        let none = HashMap::new();
        assert_eq!(pick(&msgs, 2, None, &[], &none, 0, now), (vec![5, 4], 0));
        // The old message matched but cannot be bulk deleted: it is counted, not picked.
        assert_eq!(
            pick(&msgs, 10, Some(1), &[], &none, 0, now),
            (vec![5, 3], 1)
        );
        assert_eq!(pick(&msgs, 2, Some(1), &[], &none, 0, now), (vec![5, 3], 0));
    }

    #[test]
    fn filters_combine() {
        let now = 10_000_000;
        let mut a = msg(5, 1, now);
        a.content = "Free NITRO https://x.y".into();
        let mut b = msg(4, 2, now);
        b.bot = true;
        b.embeds = 1;
        let c = msg(3, 3, now);
        let msgs = [a, b, c];
        let roles = HashMap::from([(1, vec![7]), (3, vec![])]);
        let p = |f: &[Filter]| pick(&msgs, 10, None, f, &roles, 0, now).0;
        assert_eq!(p(&[Filter::Links]), vec![5]);
        assert_eq!(p(&[Filter::Has("nitro".into()), Filter::Role(7)]), vec![5]);
        assert_eq!(p(&[Filter::Bots, Filter::Embeds]), vec![4]);
        assert_eq!(p(&[Filter::NoRole]), vec![3]);
        assert_eq!(p(&[Filter::Before(5)]), vec![4, 3]);
    }

    #[test]
    fn an_author_who_left_has_no_role() {
        assert_eq!(known_roles(Ok(None)), Some(vec![]));
        assert_eq!(known_roles(Ok(Some(vec![7]))), Some(vec![7]));
        assert_eq!(known_roles(Err(anyhow::anyhow!("500"))), None);
    }

    #[test]
    fn the_command_is_deleted_but_not_counted() {
        // `t!purge 2`: two older messages plus the command (9).
        let all = to_delete(&[8, 7], &[9, 10]);
        assert_eq!(all, vec![8, 7, 9, 10]);
        assert_eq!(to_delete(&[], &[9]), vec![9]);
    }

    #[test]
    fn a_purge_holds_only_its_own_bulk_logs() {
        let log = |c: u64| brim_logging::LogMessage {
            guild: Id::new(1),
            channel_id: Id::new(c),
            kind: brim_logging::LogKind::Message,
            embed: Log::new("Message Delete Bulk", 0).build(),
            link: None,
            file: Some(("bulk-1.txt".into(), "1: hi\n".into())),
            actors: vec![],
            vars: vec![],
        };
        let mut logs = vec![log(50), log(60)];
        // No purge deleted 3: the logs stay.
        on_bulk(&[3, 4], &mut logs, Some(Id::new(50)));
        assert_eq!(logs.len(), 2);
        // Two purges in one channel: each keeps the bulk delete of its own ids.
        let (a, b) = (Watch::start(&[3, 4]), Watch::start(&[5, 6]));
        on_bulk(&[3, 4], &mut logs, Some(Id::new(50)));
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].channel_id, Id::new(60));
        assert_eq!(a.with(|w| (w.seen, w.held.len())), Some((1, 1)));
        assert_eq!(b.with(|w| (w.seen, w.held.len())), Some((0, 0)));
        drop(a);
        logs.push(log(50));
        on_bulk(&[3, 4], &mut logs, Some(Id::new(50)));
        assert_eq!(logs.len(), 2, "a finished purge holds nothing back");
    }

    #[test]
    fn describes_the_filters() {
        assert_eq!(describe(None, &[], None), "all messages");
        let f = [Filter::Bots, Filter::Has("free nitro".into())];
        assert_eq!(
            describe(Some(7), &f, None),
            "from <@7>, bots, contains `free nitro`"
        );
    }

    #[test]
    fn parses_types() {
        assert_eq!(Filter::parse("pings", None), Ok(Filter::Mentions));
        assert_eq!(
            Filter::parse("has", Some("Hi There")),
            Ok(Filter::Has("hi there".into()))
        );
        assert!(Filter::parse("has", None).is_err());
        assert_eq!(Filter::parse("after", Some("123")), Ok(Filter::After(123)));
        assert!(Filter::parse("nope", None).is_err());
    }
}

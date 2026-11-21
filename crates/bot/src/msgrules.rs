//! Message Builder rules on the bot's side: "every N messages" counting (kept across restarts),
//! trigger words in members' messages, the quiet wait before a sticky moves down, and a deleted or
//! remade channel's rules. The dashboard runs the sends (it holds the send code): the bot marks a
//! count rule due, or queues a trigger answer or a sticky repost.
use crate::app::App;
use anyhow::Result;
use brim_core::msgtrig;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use twilight_model::{
    channel::Message,
    id::{marker::GuildMarker, Id},
};

/// A guild's rules are read again after this long, so a changed rule works within a minute
/// without a database read per message.
const FRESH: Duration = Duration::from_secs(60);
/// Counts are written to the database at most this often (plan: no write per message).
const FLUSH: Duration = Duration::from_secs(30);
/// One trigger answer per member per this long in a server (plan 4).
const PER_MEMBER: Duration = Duration::from_secs(30);
/// Size caps: a map past its cap drops what has expired, and is cleared whole if still too big.
const MAX_GUILDS: usize = 20_000;
const MAX_KEYS: usize = 50_000;
/// A sticky moves down once its channel has been quiet this long (plan 2.6: fixed, not a setting).
const QUIET: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Rules {
    /// "Every N messages" schedules as (rule, channel, n).
    counts: Vec<(i64, i64, i64)>,
    /// Trigger rules, compiled once per read.
    triggers: Vec<(i64, msgtrig::Spec)>,
    /// Channels with a sticky that is on, and whether one of them also moves after other bots.
    stickies: HashMap<u64, bool>,
    /// Brim's own Message Builder webhooks: their posts (sticky copies too) never count.
    hooks: HashSet<u64>,
}

/// The servers that have a count or trigger rule at all, read again after [`FRESH`]: a message in
/// any other server costs no database read and no cache entry.
static ACTIVE: Mutex<Option<(Instant, HashSet<u64>)>> = Mutex::new(None);
type Cache = HashMap<u64, (Instant, Arc<Rules>)>;
static RULES: Mutex<Option<Cache>> = Mutex::new(None);
/// Member messages per (rule, channel) since its last send, and whether the database has it yet.
type Counts = HashMap<(i64, i64), (i64, bool)>;
static COUNTS: Mutex<Option<Counts>> = Mutex::new(None);
static FLUSHED: Mutex<Option<Instant>> = Mutex::new(None);
/// Last answer per (rule, channel), per (server, member), and last cooldown DM per (rule, member).
/// In memory (plan 4d.9): a restart can give at most one extra answer.
#[allow(clippy::type_complexity)]
static SEEN: Mutex<Option<[HashMap<(u64, u64), Instant>; 3]>> = Mutex::new(None);
/// Channels waiting out the quiet time: the last message, and whether a member (not a bot) wrote.
static WAITING: Mutex<Option<HashMap<u64, (Instant, bool)>>> = Mutex::new(None);

async fn active(app: &App, g: u64) -> Result<bool> {
    if let Some((at, set)) = ACTIVE.lock().unwrap().as_ref() {
        if at.elapsed() < FRESH {
            return Ok(set.contains(&g));
        }
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "select distinct guild_id from message_rules where enabled and
             (kind = 'trigger' or (kind = 'schedule' and spec->'lines' @> '[{\"kind\": \"messages\"}]'))
         union select distinct guild_id from sticky_msgs where enabled",
    )
    .fetch_all(&app.pool)
    .await?;
    let set: HashSet<u64> = ids.into_iter().map(|g| g as u64).collect();
    let on = set.contains(&g);
    *ACTIVE.lock().unwrap() = Some((Instant::now(), set));
    Ok(on)
}

async fn rules(app: &App, guild: Id<GuildMarker>) -> Result<Arc<Rules>> {
    let g = guild.get();
    if !active(app, g).await? {
        return Ok(Arc::default());
    }
    if let Some((at, r)) = RULES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(&g)
    {
        if at.elapsed() < FRESH {
            return Ok(r.clone());
        }
    }
    let rows: Vec<(i64, String, Option<i64>, serde_json::Value)> = sqlx::query_as(
        "select id, kind, channel_id, spec from message_rules where guild_id = $1 and enabled",
    )
    .bind(g as i64)
    .fetch_all(&app.pool)
    .await?;
    let mut r = Rules::default();
    for (id, kind, ch, spec) in rows {
        if kind == "trigger" {
            match msgtrig::parse(&spec) {
                Ok(s) => r.triggers.push((id, s)),
                Err(e) => tracing::warn!(id, e, "a stored trigger rule does not parse"),
            }
        } else if let (Some(ch), Some(n)) = (ch, brim_core::msgsched::messages_of(&spec)) {
            r.counts.push((id, ch, n));
        }
    }
    let stickies: Vec<(i64, bool)> = sqlx::query_as(
        "select channel_id, bool_or(bots) from sticky_msgs where guild_id = $1 and enabled group by channel_id",
    )
    .bind(g as i64)
    .fetch_all(&app.pool)
    .await?;
    r.stickies = stickies.into_iter().map(|(c, b)| (c as u64, b)).collect();
    if !r.stickies.is_empty() {
        let hooks: Vec<i64> =
            sqlx::query_scalar("select webhook_id from message_webhooks where guild_id = $1")
                .bind(g as i64)
                .fetch_all(&app.pool)
                .await?;
        r.hooks = hooks.into_iter().map(|h| h as u64).collect();
    }
    load_counts(app, &r.counts).await?;
    let r = Arc::new(r);
    let mut map = RULES.lock().unwrap();
    let map = map.get_or_insert_with(HashMap::new);
    if map.len() >= MAX_GUILDS {
        map.clear();
    }
    map.insert(g, (Instant::now(), r.clone()));
    Ok(r)
}

/// Reads the stored counts of rules the bot has not counted for yet (after a restart).
async fn load_counts(app: &App, rules: &[(i64, i64, i64)]) -> Result<()> {
    let missing: Vec<i64> = {
        let c = COUNTS.lock().unwrap();
        rules
            .iter()
            .filter(|(r, ch, _)| c.as_ref().is_none_or(|c| !c.contains_key(&(*r, *ch))))
            .map(|(r, _, _)| *r)
            .collect()
    };
    if missing.is_empty() {
        return Ok(());
    }
    let rows: Vec<(i64, i64, i64)> =
        sqlx::query_as("select rule_id, channel_id, n from message_counts where rule_id = any($1)")
            .bind(&missing)
            .fetch_all(&app.pool)
            .await?;
    let mut c = COUNTS.lock().unwrap();
    let c = c.get_or_insert_with(HashMap::new);
    for (r, ch, n) in rows {
        c.entry((r, ch)).or_insert((n, true));
    }
    Ok(())
}

/// Writes changed counts in one statement, at most every [`FLUSH`]; a crash loses at most that.
fn flush(app: &App) {
    {
        let mut at = FLUSHED.lock().unwrap();
        if at.is_some_and(|t| t.elapsed() < FLUSH) {
            return;
        }
        *at = Some(Instant::now());
    }
    let (mut rules, mut chans, mut ns) = (Vec::new(), Vec::new(), Vec::new());
    if let Some(c) = COUNTS.lock().unwrap().as_mut() {
        for ((r, ch), (n, _)) in c.iter().filter(|(_, (_, s))| !*s) {
            rules.push(*r);
            chans.push(*ch);
            ns.push(*n);
        }
    }
    if rules.is_empty() {
        return;
    }
    let pool = app.pool.clone();
    tokio::spawn(async move {
        // A rule deleted meanwhile is left out (its row would break the foreign key).
        let r = sqlx::query(
            "insert into message_counts (rule_id, channel_id, n)
             select u.r, u.c, u.n from unnest($1::bigint[], $2::bigint[], $3::bigint[]) as u(r, c, n)
             where exists (select 1 from message_rules where id = u.r)
             on conflict (rule_id, channel_id) do update set n = excluded.n",
        )
        .bind(&rules)
        .bind(&chans)
        .bind(&ns)
        .execute(&pool)
        .await;
        match r {
            // Marked as stored only once written, and only if no message came in meanwhile; a
            // failed write is tried again at the next flush.
            Ok(_) => {
                if let Some(c) = COUNTS.lock().unwrap().as_mut() {
                    for ((r, ch), n) in rules.iter().zip(&chans).zip(&ns) {
                        if let Some(e) = c.get_mut(&(*r, *ch)).filter(|e| e.0 == *n) {
                            e.1 = true;
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(?e, "saving message counts failed"),
        }
    });
}

/// Whether `key` in map `i` of [`SEEN`] is older than `gap` (or not there), without recording it.
fn free(i: usize, key: (u64, u64), gap: Duration) -> bool {
    SEEN.lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m[i].get(&key))
        .is_none_or(|t| t.elapsed() >= gap)
}

/// Records `key` in map `i` of [`SEEN`] if its last time is older than `gap`; false when it is not.
fn claim(i: usize, key: (u64, u64), gap: Duration) -> bool {
    let mut seen = SEEN.lock().unwrap();
    let map = &mut seen.get_or_insert_with(Default::default)[i];
    if map.len() >= MAX_KEYS {
        // Nothing is kept longer than a day (the longest cooldown).
        map.retain(|_, t| t.elapsed() < Duration::from_secs(86_400));
        // Then the rule (or, for map 1, the server) holding the most: other servers keep cooldowns.
        if map.len() >= MAX_KEYS {
            crate::caps::drop_biggest(map, |k| k.0);
        }
    }
    if map.get(&key).is_some_and(|t| t.elapsed() < gap) {
        return false;
    }
    map.insert(key, Instant::now());
    true
}

/// Any message in a server: moves its channel's stickies down after the quiet wait. A member's
/// message (never a bot's or a webhook's) also counts toward "every N messages" in its channel (at
/// N the rule is marked due), and may queue a trigger answer.
pub async fn on_message(app: &App, m: &Message, guild: Id<GuildMarker>) -> Result<()> {
    // The Messages module is off: no counting, no trigger answers, no sticky moves.
    if !app.cfg(guild.get()).await?.modules.messages {
        return Ok(());
    }
    let rules = rules(app, guild).await?;
    sticky(app, m, guild, &rules).await?;
    if m.author.bot || m.webhook_id.is_some() {
        return Ok(());
    }
    if rules.counts.is_empty() && rules.triggers.is_empty() {
        return Ok(());
    }
    let channel = m.channel_id.get() as i64;
    let here: Vec<(i64, i64, i64)> = rules
        .counts
        .iter()
        .filter(|(_, ch, _)| *ch == channel)
        .copied()
        .collect();
    // A count dropped from memory (the cap below) is read back before it goes on.
    load_counts(app, &here).await?;
    let due: Vec<i64> = {
        let mut counts = COUNTS.lock().unwrap();
        let counts = counts.get_or_insert_with(HashMap::new);
        if counts.len() >= MAX_KEYS {
            // Stored counts can be read back; the rest is only cleared if still too many.
            counts.retain(|_, (_, stored)| !*stored);
            if counts.len() >= MAX_KEYS {
                counts.clear();
            }
        }
        rules
            .counts
            .iter()
            .filter(|(_, ch, _)| *ch == channel)
            .filter_map(|(rule, _, n)| {
                let c = counts.entry((*rule, channel)).or_insert((0, false));
                c.0 += 1;
                c.1 = false;
                (c.0 >= *n).then(|| {
                    c.0 = 0;
                    *rule
                })
            })
            .collect()
    };
    if !rules.counts.is_empty() {
        flush(app);
    }
    for rule in due {
        sqlx::query(
            "update message_rules set due_messages = true where id = $1 and enabled and guild_id = $2",
        )
        .bind(rule)
        .bind(guild.get() as i64)
            .execute(&app.pool)
            .await?;
    }
    if !rules.triggers.is_empty() {
        trigger(app, m, guild, &rules).await?;
    }
    Ok(())
}

/// Starts (or extends) the quiet wait of a channel with stickies. Once nobody wrote there for
/// [`QUIET`], one repost is queued for the dashboard: a burst of 20 messages gives one repost.
async fn sticky(app: &App, m: &Message, guild: Id<GuildMarker>, rules: &Rules) -> Result<()> {
    let ch = m.channel_id.get();
    let Some(&bots) = rules.stickies.get(&ch) else {
        return Ok(());
    };
    // Brim's own posts (its sticky copies among them) never count.
    if m.author.id == app.me || m.webhook_id.is_some_and(|w| rules.hooks.contains(&w.get())) {
        return Ok(());
    }
    let member = !m.author.bot && m.webhook_id.is_none();
    if !member && !bots {
        return Ok(());
    }
    // A webhook Brim made after the rules were read (its first send in a channel) is its own
    // too; otherwise its sticky copy would move the sticky down again, over and over.
    if let Some(w) = m.webhook_id {
        let ours: bool = sqlx::query_scalar(
            "select exists(select 1 from message_webhooks where webhook_id = $1 and guild_id = $2)",
        )
        .bind(w.get() as i64)
        .bind(guild.get() as i64)
        .fetch_one(&app.pool)
        .await?;
        if ours {
            return Ok(());
        }
    }
    let start = {
        let mut w = WAITING.lock().unwrap();
        let w = w.get_or_insert_with(HashMap::new);
        let full = w.len() >= MAX_KEYS;
        match w.get_mut(&ch) {
            Some(e) => {
                e.0 = Instant::now();
                e.1 |= member;
                false
            }
            // Entries live 3 s; past the cap a channel simply waits for its next message.
            None if full => false,
            None => {
                w.insert(ch, (Instant::now(), member));
                true
            }
        }
    };
    if !start {
        return Ok(());
    }
    let pool = app.pool.clone();
    tokio::spawn(async move {
        let member = loop {
            let left = {
                let mut w = WAITING.lock().unwrap();
                let Some(e) = w.as_mut().and_then(|w| w.get(&ch).copied()) else {
                    return;
                };
                let left = QUIET.saturating_sub(e.0.elapsed());
                if left.is_zero() {
                    w.as_mut().map(|w| w.remove(&ch));
                    break e.1;
                }
                left
            };
            tokio::time::sleep(left).await;
        };
        let r = sqlx::query(
            "with q as (insert into sticky_runs (channel_id, guild_id, bots_only) values ($1, $2, $3)
                        on conflict (channel_id) do update set bots_only = sticky_runs.bots_only and excluded.bots_only
                        returning 1)
             select pg_notify('message_runs', '') from q",
        )
        .bind(ch as i64)
        .bind(guild.get() as i64)
        .bind(!member)
        .execute(&pool)
        .await;
        if let Err(e) = r {
            tracing::warn!(?e, "queueing a sticky repost failed");
        }
    });
    Ok(())
}

/// Finds the first trigger rule of this channel that the message sets off and queues its answer
/// (or, on cooldown, the cooldown DM). Prefix commands and timed-out members are skipped; the
/// dashboard checks again that the message is still there and the member is not held.
async fn trigger(app: &App, m: &Message, guild: Id<GuildMarker>, rules: &Rules) -> Result<()> {
    let (g, user, ch) = (guild.get(), m.author.id.get(), m.channel_id.get());
    let Some(member) = &m.member else {
        return Ok(());
    };
    if member
        .communication_disabled_until
        .is_some_and(|t| t.as_secs() > crate::automod::now_ms() / 1000)
    {
        return Ok(());
    }
    let pfx = app.cfg(g).await?.misc.prefix.clone();
    if !pfx.is_empty() && m.content.starts_with(&pfx) {
        return Ok(());
    }
    let roles: Vec<u64> = member.roles.iter().map(|r| r.get()).collect();
    for (rule, spec) in &rules.triggers {
        if !spec.channels.contains(&ch) || !spec.member_ok(&roles) {
            continue;
        }
        let Some(t) = spec.find(&m.content) else {
            continue;
        };
        // The member's 30 s slot is taken only for an answer; a rule on cooldown lets the next
        // matching rule try.
        if !free(1, (g, user), PER_MEMBER) {
            return Ok(());
        }
        let kind = if claim(
            0,
            (*rule as u64, ch),
            Duration::from_secs(spec.cooldown as u64),
        ) {
            claim(1, (g, user), PER_MEMBER);
            "answer"
        } else {
            match spec.dm_on_cooldown {
                Some(d) if claim(2, (*rule as u64, user), Duration::from_secs(d as u64)) => {
                    "cooldown"
                }
                _ => continue,
            }
        };
        sqlx::query(
            "with q as (insert into message_runs (rule_id, guild_id, channel_id, member_id, message_id, matched, kind)
                        values ($1, $2, $3, $4, $5, $6, $7) returning id)
             select pg_notify('message_runs', '') from q",
        )
        .bind(rule)
        .bind(g as i64)
        .bind(ch as i64)
        .bind(user as i64)
        .bind(m.id.get() as i64)
        .bind(&t.text)
        .bind(kind)
        .execute(&app.pool)
        .await?;
        return Ok(());
    }
    Ok(())
}

/// A deleted channel pauses (not deletes) the schedules that post there, with the reason; a
/// restore that makes the channel again moves them to it and switches them back on ([`relink`]).
/// Trigger rules watching it only get the reason.
pub async fn channel_deleted(
    app: &App,
    guild: Id<GuildMarker>,
    channel: u64,
    name: Option<&str>,
) -> Result<()> {
    let issue = match name {
        Some(n) => format!("Channel #{n} was deleted"),
        None => "The channel was deleted".to_string(),
    };
    sqlx::query(
        "update message_rules set enabled = false, next_at = null, due_messages = false, issue = $2
         where channel_id = $1 and enabled and guild_id = $3",
    )
    .bind(channel as i64)
    .bind(&issue)
    .bind(guild.get() as i64)
    .execute(&app.pool)
    .await?;
    // A trigger rule keeps the channel in its list (a restore puts the new id in its place) and
    // goes on in its other channels; the card says which one is gone.
    sqlx::query(
        "update message_rules set issue = $2 where kind = 'trigger' and spec->'channels' ? $1 and guild_id = $3",
    )
    .bind(channel.to_string())
    // The id lets a restore clear only this channel's note (see [`relink`]).
    .bind(format!("{issue} (id {channel})"))
    .bind(guild.get() as i64)
    .execute(&app.pool)
    .await?;
    // Its copy went with the channel.
    sqlx::query(
        "update sticky_msgs set enabled = false, issue = $2, posted_id = null, send_id = null
         where channel_id = $1 and enabled and guild_id = $3",
    )
    .bind(channel as i64)
    .bind(&issue)
    .bind(guild.get() as i64)
    .execute(&app.pool)
    .await?;
    Ok(())
}

/// Moves schedules to channels a nuke revert or a backup restore made again (old id -> new id),
/// and switches back on the ones the delete paused.
pub async fn relink(app: &App, guild: Id<GuildMarker>, map: &HashMap<u64, u64>) {
    for (old, new) in map {
        // Trigger rules watch a list of channels: the old id is swapped for the new one.
        let r = sqlx::query(
            "update message_rules set spec = jsonb_set(spec, '{channels}', (spec->'channels') - $2 || to_jsonb($3::text)),
                 issue = case when issue like '%(id ' || $2 || ')' then null else issue end
             where guild_id = $1 and kind = 'trigger' and spec->'channels' ? $2",
        )
        .bind(guild.get() as i64)
        .bind(old.to_string())
        .bind(new.to_string())
        .execute(&app.pool)
        .await;
        if let Err(e) = r {
            tracing::warn!(?e, "moving message triggers to a remade channel failed");
        }
        // Stickies the delete paused go on in the new channel, posted at once.
        let r = sqlx::query(
            "with s as (update sticky_msgs set channel_id = $3, posted_id = null, send_id = null,
                            enabled = enabled or issue like '%was deleted',
                            issue = case when issue like '%was deleted' then null else issue end
                        where guild_id = $1 and channel_id = $2 returning enabled),
                  q as (insert into sticky_runs (channel_id, guild_id, bots_only)
                        select $3, $1, false where exists (select 1 from s where enabled)
                        on conflict (channel_id) do update set bots_only = false returning 1)
             select pg_notify('message_runs', '') from q",
        )
        .bind(guild.get() as i64)
        .bind(*old as i64)
        .bind(*new as i64)
        .execute(&app.pool)
        .await;
        if let Err(e) = r {
            tracing::warn!(?e, "moving stickies to a remade channel failed");
        }
        let r = sqlx::query_as::<_, (i64, serde_json::Value, bool)>(
            "update message_rules set channel_id = $3 where guild_id = $1 and channel_id = $2
             returning id, spec, issue like '%was deleted'",
        )
        .bind(guild.get() as i64)
        .bind(*old as i64)
        .bind(*new as i64)
        .fetch_all(&app.pool)
        .await;
        let rows = match r {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(?e, "moving message schedules to a remade channel failed");
                continue;
            }
        };
        // A rule the delete paused goes on again from its next time, never at once.
        for (id, spec, paused) in rows.into_iter().filter(|r| r.2) {
            let next = brim_core::msgsched::next_of(&spec);
            let r = sqlx::query(
                "update message_rules set enabled = true, issue = null, due_messages = false, waiting_once = false,
                     next_at = to_timestamp($2) where id = $1",
            )
            .bind(id)
            .bind(next.map(|n| n as f64))
            .execute(&app.pool)
            .await;
            if let Err(e) = r {
                tracing::warn!(?e, paused, "switching a message schedule back on failed");
            }
        }
    }
}

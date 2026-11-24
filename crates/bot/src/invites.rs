//! Invite tracking and the public join-raid report link.
//!
//! One actor task per guild handles that guild's invite and member events in gateway order:
//! the gateway loop hands them over (`observe`) before each event's own task starts, so a slow
//! invite fetch is never overtaken by a later one. Nothing here may delay Join Gate or Join Raid.
use brim_core::invites::{self as store, How, Join};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{
        mpsc::{
            self,
            error::{TryRecvError, TrySendError},
        },
        oneshot,
    },
    time::{timeout, timeout_at, Instant},
};
use twilight_gateway::Event;
use twilight_model::{gateway::payload::incoming::GuildCreate, id::Id};

use crate::{app::App, automod::now_ms};

/// Events one guild may queue; past that the guild gets a gap.
const QUEUE: usize = 2048;
/// Joins this close together share one invite fetch.
const WINDOW: Duration = Duration::from_millis(250);
/// A join reaching us this soon after a fetch may already be counted in it.
const SETTLE: Duration = Duration::from_secs(1);
const FETCH: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_secs(15 * 60);
const MAX_BATCH: usize = 256;
/// Fetches one batch may take while joins keep arriving in the settle time.
const ROUNDS: usize = 5;
/// After Discord refused the list (no Manage Server), joins do not ask again for this long.
const FORBIDDEN_WAIT: Duration = Duration::from_secs(10 * 60);

/// One invite in a snapshot. `owner` is who gets the credit: the member a Brim link was made
/// for, else Discord's inviter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    pub uses: Option<u64>,
    pub owner: Option<u64>,
    /// 0 or unknown: no limit.
    pub max_uses: Option<u64>,
    pub expires_ms: Option<i64>,
    /// Removed by an `INVITE_DELETE`; it stays until the next list so the diff sees it go.
    pub deleted: bool,
}

pub type Baseline = HashMap<String, Code>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinIn {
    pub user: u64,
    pub bot: bool,
    pub joined_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub user: u64,
    pub joined_at_ms: i64,
    pub how: How,
    pub code: Option<String>,
    pub inviter: Option<u64>,
    pub reason: Option<&'static str>,
    /// Human joins matched together (1 for `Single`).
    pub group: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No baseline yet, or tracking is off.
    Waiting,
    Healthy {
        since_ms: i64,
    },
    Failing {
        why: &'static str,
        since_ms: i64,
    },
}

enum Msg {
    /// `true`: after a GuildCreate, spread out so a new session does not fetch every guild at once.
    Seed(bool),
    Created {
        code: String,
        code_info: Code,
    },
    Deleted(String),
    Join(JoinIn, oneshot::Sender<Resolved>),
    Leave(u64),
}

#[derive(Default)]
pub struct Tracker {
    actors: HashMap<u64, mpsc::Sender<Msg>>,
    /// Guilds whose queue overflowed: their next batch cannot be trusted.
    gaps: HashSet<u64>,
    status: HashMap<u64, Status>,
    /// Baselines of actors that exited idle. Any event of the guild would have kept its actor
    /// running, so the baseline is still current.
    parked: HashMap<u64, Baseline>,
    /// Each join's result, until `joingate::on_join` takes it.
    results: HashMap<(u64, u64), (Instant, oneshot::Receiver<Resolved>)>,
    /// To start an actor from a command, which only has `&App`.
    app: Option<std::sync::Weak<App>>,
}

/// The shared `App`, for a task started from code that only has `&App`. None before the first
/// gateway event.
pub fn arc(app: &App) -> Option<Arc<App>> {
    app.tracker.lock().unwrap().app.as_ref()?.upgrade()
}

/// Asks for a fresh invite list when tracking is not healthy (a permission given back, tracking
/// turned on), so a command does not wait for the next join.
pub fn reseed(app: &App, gid: u64) {
    if matches!(status(app, gid), Status::Healthy { .. }) {
        return;
    }
    let app = app
        .tracker
        .lock()
        .unwrap()
        .app
        .as_ref()
        .and_then(|a| a.upgrade());
    if let Some(app) = app {
        send(&app, gid, Msg::Seed(false));
    }
}

pub fn status(app: &App, gid: u64) -> Status {
    let t = app.tracker.lock().unwrap();
    t.status.get(&gid).copied().unwrap_or(Status::Waiting)
}

/// A baseline is set and the last invite fetch worked.
pub fn tracking_ok(app: &App, gid: u64) -> bool {
    matches!(status(app, gid), Status::Healthy { .. })
}

/// The tracker's result for this join, if it was queued.
pub fn take_result(app: &App, gid: u64, user: u64) -> Option<oneshot::Receiver<Resolved>> {
    let mut t = app.tracker.lock().unwrap();
    t.results.remove(&(gid, user)).map(|r| r.1)
}

/// Called by the gateway loop for every event, before the event's task is spawned. Copies the
/// few fields the tracker needs and never waits.
pub fn observe(app: &Arc<App>, ev: &Event) {
    let (gid, msg) = match ev {
        Event::GuildCreate(g) if matches!(**g, GuildCreate::Available(_)) => {
            (g.id(), Msg::Seed(true))
        }
        // Also when only unavailable: the actor and its state go, the stored data stays.
        Event::GuildDelete(g) => {
            let g = g.id.get();
            let mut t = app.tracker.lock().unwrap();
            t.actors.remove(&g);
            t.gaps.remove(&g);
            t.status.remove(&g);
            t.parked.remove(&g);
            t.results.retain(|k, _| k.0 != g);
            return;
        }
        Event::InviteCreate(i) => (
            i.guild_id,
            Msg::Created {
                code: i.code.clone(),
                code_info: Code {
                    uses: Some(0),
                    owner: i.inviter.as_ref().map(|u| u.id.get()),
                    max_uses: Some(i.max_uses),
                    expires_ms: (i.max_age > 0)
                        .then(|| i.created_at.as_micros() / 1000 + i.max_age as i64 * 1000),
                    deleted: false,
                },
            },
        ),
        Event::InviteDelete(i) => (i.guild_id, Msg::Deleted(i.code.clone())),
        Event::MemberAdd(m) => {
            let j = JoinIn {
                user: m.user.id.get(),
                bot: m.user.bot,
                joined_at_ms: m
                    .member
                    .joined_at
                    .map_or_else(now_ms, |t| t.as_micros() / 1000),
            };
            let (tx, rx) = oneshot::channel();
            let mut t = app.tracker.lock().unwrap();
            // A result nobody took (the join's task failed early) must not stay forever.
            if t.results.len() > 1024 {
                t.results
                    .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
            }
            let key = (m.guild_id.get(), j.user);
            t.results.insert(key, (Instant::now(), rx));
            (m.guild_id, Msg::Join(j, tx))
        }
        Event::MemberRemove(m) => (m.guild_id, Msg::Leave(m.user.id.get())),
        _ => return,
    };
    send(app, gid.get(), msg);
}

fn send(app: &Arc<App>, gid: u64, msg: Msg) {
    let mut t = app.tracker.lock().unwrap();
    if t.app.is_none() {
        t.app = Some(Arc::downgrade(app));
    }
    let sent = match t.actors.get(&gid) {
        Some(tx) => tx.try_send(msg),
        None => Err(TrySendError::Closed(msg)),
    };
    let msg = match sent {
        Ok(()) => return,
        // Never wait in the gateway loop: the dropped event makes the next batch unknown.
        Err(TrySendError::Full(_)) => {
            t.gaps.insert(gid);
            return;
        }
        Err(TrySendError::Closed(msg)) => msg,
    };
    let (tx, rx) = mpsc::channel(QUEUE);
    let _ = tx.try_send(msg);
    let me = tx.downgrade();
    t.actors.insert(gid, tx);
    drop(t);
    tokio::spawn(actor(app.clone(), gid, rx, me));
}

struct Actor {
    app: Arc<App>,
    gid: u64,
    rx: mpsc::Receiver<Msg>,
    /// Events read ahead (a batch ends at a non-join, `settle` reads on), still in gateway order.
    pending: VecDeque<Msg>,
    base: Option<Baseline>,
    gap: bool,
    me: mpsc::WeakSender<Msg>,
    /// No list fetch for joins until then (Discord answered 403).
    forbidden_until: Option<Instant>,
}

async fn actor(app: Arc<App>, gid: u64, rx: mpsc::Receiver<Msg>, me: mpsc::WeakSender<Msg>) {
    let base = app.tracker.lock().unwrap().parked.remove(&gid);
    let mut a = Actor {
        app,
        gid,
        rx,
        pending: VecDeque::new(),
        base,
        gap: false,
        me,
        forbidden_until: None,
    };
    while let Some(msg) = a.next().await {
        if !a.current() {
            return;
        }
        a.handle(msg).await;
    }
}

impl Actor {
    /// After a GuildDelete and a quick GuildCreate a newer actor owns the guild: two must not
    /// write. Checked before each event and again before a batch is saved.
    fn current(&self) -> bool {
        self.me.upgrade().is_some_and(|me| {
            let t = self.app.tracker.lock().unwrap();
            t.actors.get(&self.gid).is_some_and(|s| s.same_channel(&me))
        })
    }

    async fn next(&mut self) -> Option<Msg> {
        if let Some(m) = self.pending.pop_front() {
            return Some(m);
        }
        if let Ok(m) = timeout(IDLE, self.rx.recv()).await {
            return m;
        }
        // Under the lock `send` uses, so no event slips in between the check and the exit.
        let mut t = self.app.tracker.lock().unwrap();
        match self.rx.try_recv() {
            Ok(m) => Some(m),
            // Only the map holds a sender, so a live channel means the entry is this actor's.
            Err(TryRecvError::Empty) => {
                t.actors.remove(&self.gid);
                if let Some(b) = self.base.take() {
                    t.parked.insert(self.gid, b);
                }
                None
            }
            Err(TryRecvError::Disconnected) => None,
        }
    }

    async fn handle(&mut self, msg: Msg) {
        let (app, g) = (&self.app, self.gid as i64);
        let on = app
            .cfg(self.gid)
            .await
            .is_ok_and(|c| c.misc.invite_tracking);
        if !on {
            // Turned back on, tracking starts from a new baseline.
            self.base = None;
            app.tracker.lock().unwrap().status.remove(&self.gid);
        }
        match msg {
            // Leaves and gone links keep stored data true even while tracking is off.
            Msg::Leave(user) => {
                if let Err(e) = store::record_leave(&app.pool, g, user as i64).await {
                    tracing::warn!(?e, "saving an invite tracker leave failed");
                }
            }
            Msg::Deleted(code) => {
                if let Some(c) = self.base.as_mut().and_then(|b| b.get_mut(&code)) {
                    c.deleted = true;
                }
                if let Err(e) = store::links_gone(&app.pool, g, Some(&code), &[], 0).await {
                    tracing::warn!(?e, "marking a deleted invite link failed");
                }
            }
            _ if !on => {}
            Msg::Seed(spread) => {
                if spread {
                    let ms = rand::random_range(0..10_000);
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
                self.forbidden_until = None;
                self.base = self.fetch().await.ok();
                // Joins that came in meanwhile may be counted in this list already.
                self.gap |= self.settle().await;
            }
            Msg::Created { code, code_info } => {
                if let Some(b) = &mut self.base {
                    b.entry(code).or_insert(code_info);
                }
            }
            Msg::Join(j, tx) => self.batch(j, tx).await,
        }
    }

    async fn batch(&mut self, first: JoinIn, tx: oneshot::Sender<Resolved>) {
        let mut joins = vec![(first, tx)];
        let until = Instant::now() + WINDOW;
        while joins.len() < MAX_BATCH {
            let m = match self.pending.pop_front() {
                Some(m) => m,
                None => match timeout_at(until, self.rx.recv()).await {
                    Ok(Some(m)) => m,
                    _ => break,
                },
            };
            match m {
                Msg::Join(j, tx) => joins.push((j, tx)),
                m => {
                    self.pending.push_front(m);
                    break;
                }
            }
        }
        let overflow = self.app.tracker.lock().unwrap().gaps.remove(&self.gid);
        let gap = std::mem::take(&mut self.gap) || overflow;
        let mut new = self.fetch().await;
        let mut raced = self.settle().await;
        // Joins that came during the settle time join the batch and the list is read again, so a
        // steady stream of joins is still matched as a group instead of all becoming unknown.
        for _ in 1..ROUNDS {
            if !raced || new.is_err() || joins.len() >= MAX_BATCH {
                break;
            }
            while let Some(Msg::Join(..)) = self.pending.front() {
                if let Some(Msg::Join(j, tx)) = self.pending.pop_front() {
                    joins.push((j, tx));
                }
            }
            // A join queued behind another event cannot move ahead of it: stays unknown.
            if self.pending.iter().any(|m| matches!(m, Msg::Join(..))) {
                break;
            }
            new = self.fetch().await;
            raced = self.settle().await;
        }
        let ins: Vec<JoinIn> = joins.iter().map(|j| j.0).collect();
        let me = self.app.me.get();
        let out = match &new {
            Ok(new) => resolve(self.base.as_ref(), new, &ins, gap || raced, me, now_ms()),
            Err(why) => assign(&ins, Err(why)),
        };
        let g = self.gid as i64;
        let rows: Vec<Join> = out
            .iter()
            .map(|r| Join {
                guild_id: g,
                user_id: r.user as i64,
                joined_at_ms: r.joined_at_ms,
                code: r.code.as_deref(),
                inviter_id: r.inviter.map(|i| i as i64),
                how: r.how,
                reason: r.reason,
            })
            .collect();
        if !self.current() {
            return;
        }
        // A failed write still leaves `new` a correct, fresh baseline for the next batch.
        if let Err(e) = store::record_joins(&self.app.pool, &rows).await {
            tracing::warn!(?e, "saving invite tracker joins failed");
        }
        drop(rows);
        self.base = new.ok();
        for ((_, tx), r) in joins.into_iter().zip(out) {
            let _ = tx.send(r);
        }
    }

    /// Reads on for `SETTLE` after a fetch: true when a join is queued, as its invite use may be
    /// in the list just read, and a credit from that list could name the wrong joiner.
    async fn settle(&mut self) -> bool {
        let until = Instant::now() + SETTLE;
        while self.pending.len() < QUEUE {
            match timeout_at(until, self.rx.recv()).await {
                Ok(Some(m)) => self.pending.push_back(m),
                _ => break,
            }
        }
        self.pending.len() >= QUEUE || self.pending.iter().any(|m| matches!(m, Msg::Join(..)))
    }

    /// The guild's invites now, and the guild's status from it. A bad list is never a baseline.
    async fn fetch(&mut self) -> Result<Baseline, &'static str> {
        // Each 403 counts towards Discord's invalid request limit, which can block the whole bot.
        if self.forbidden_until.is_some_and(|t| Instant::now() < t) {
            return Err("forbidden");
        }
        let asked = now_ms();
        let r = fetch(&self.app, self.gid).await;
        if r == Err("forbidden") {
            self.forbidden_until = Some(Instant::now() + FORBIDDEN_WAIT);
        }
        let now = now_ms();
        {
            let mut t = self.app.tracker.lock().unwrap();
            let st = match (&r, t.status.get(&self.gid)) {
                (Ok(_), _) => Status::Healthy { since_ms: now },
                (Err(why), Some(Status::Failing { since_ms, .. })) => Status::Failing {
                    why,
                    since_ms: *since_ms,
                },
                (Err(why), _) => Status::Failing { why, since_ms: now },
            };
            t.status.insert(self.gid, st);
        }
        if let Err(why) = r {
            tracing::info!(
                guild = self.gid,
                why,
                "invite fetch failed, joins stay unknown"
            );
        }
        if let Ok(b) = &r {
            let live: Vec<String> = b.keys().cloned().collect();
            if let Err(e) =
                store::links_gone(&self.app.pool, self.gid as i64, None, &live, asked).await
            {
                tracing::warn!(?e, "marking gone invite links failed");
            }
        }
        r
    }
}

async fn fetch(app: &App, gid: u64) -> Result<Baseline, &'static str> {
    let list = async {
        let r =
            app.http.guild_invites(Id::new(gid)).await.map_err(
                |e| match crate::http_err::status(&e) {
                    Some(403) => "forbidden",
                    Some(429) => "rate_limited",
                    Some(500..) => "server_error",
                    _ => "fetch_failed",
                },
            )?;
        r.models().await.map_err(|_| "bad_response")
    };
    let list = timeout(FETCH, list).await.map_err(|_| "timeout")??;
    if list.iter().any(|i| i.uses.is_none()) {
        return Err("missing_uses");
    }
    let codes: Vec<String> = list.iter().map(|i| i.code.clone()).collect();
    // Without the owners a Brim link names the bot, which resolves to unknown, never wrong.
    let owners = store::link_owners(&app.pool, gid as i64, &codes)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(?e, "reading invite link owners failed");
            HashMap::new()
        });
    Ok(list
        .into_iter()
        .map(|i| {
            let owner = owners.get(&i.code).map(|o| *o as u64);
            let owner = owner.or(i.inviter.map(|u| u.id.get()));
            (
                i.code,
                Code {
                    uses: i.uses,
                    owner,
                    max_uses: i.max_uses,
                    expires_ms: i.expires_at.map(|t| t.as_micros() / 1000),
                    deleted: false,
                },
            )
        })
        .collect())
}

/// Ties a batch of joins to an invite: only when exactly one code grew, by exactly the number
/// of human joins, and nothing else in the list moved. Anything else is unknown, never a guess.
pub fn resolve(
    old: Option<&Baseline>,
    new: &Baseline,
    joins: &[JoinIn],
    gap: bool,
    bot_id: u64,
    now_ms: i64,
) -> Vec<Resolved> {
    let humans = joins.iter().filter(|j| !j.bot).count() as u64;
    let verdict = match old {
        None => Err("no_baseline"),
        Some(_) if gap => Err("gap"),
        Some(old) => matched(old, new, humans, bot_id, now_ms),
    };
    assign(joins, verdict)
}

fn matched(
    old: &Baseline,
    new: &Baseline,
    humans: u64,
    bot_id: u64,
    now_ms: i64,
) -> Result<(String, u64), &'static str> {
    let mut grown = vec![];
    for (code, c) in old.iter().filter(|(code, _)| !new.contains_key(*code)) {
        match (c.uses, c.max_uses) {
            // One use short of its limit: Discord deletes it at the last use.
            // Also expired: the join may have come another way, so no credit. A delete event is no
            // sign (Discord sends one for the last use too).
            (Some(u), Some(m))
                if m > 0 && u + 1 == m && c.expires_ms.is_some_and(|e| e <= now_ms) =>
            {
                return Err("ambiguous")
            }
            (Some(u), Some(m)) if m > 0 && u + 1 == m => grown.push((code, 1, c.owner)),
            // Expired or deleted (by staff, event seen) while not one short: no use in it.
            _ if c.expires_ms.is_some_and(|e| e <= now_ms) || c.deleted => {}
            // Gone with no event: a delete, a last use or an expiry we cannot tell apart.
            _ => return Err("ambiguous"),
        }
    }
    for (code, c) in new {
        let Some(now) = c.uses else {
            return Err("ambiguous");
        };
        match old.get(code).map(|o| o.uses) {
            // Unknown before: its uses may be older than the baseline.
            None if now > 0 => return Err("ambiguous"),
            None => {}
            Some(None) => return Err("ambiguous"),
            Some(Some(was)) if now < was => return Err("ambiguous"),
            Some(Some(was)) if now > was => grown.push((code, now - was, c.owner)),
            Some(Some(_)) => {}
        }
    }
    match grown[..] {
        [(code, d, owner)] if d == humans => match owner {
            Some(o) if o != bot_id => Ok((code.clone(), o)),
            _ => Err("no_owner"),
        },
        _ => Err("ambiguous"),
    }
}

fn assign(joins: &[JoinIn], verdict: Result<(String, u64), &'static str>) -> Vec<Resolved> {
    let humans = joins.iter().filter(|j| !j.bot).count();
    let matched = if humans == 1 { How::Single } else { How::Group };
    joins
        .iter()
        .map(|j| {
            let (how, code, inviter, reason) = match &verdict {
                _ if j.bot => (How::Bot, None, None, None),
                Ok((code, owner)) => (matched, Some(code.clone()), Some(*owner), None),
                Err(why) => (How::Unknown, None, None, Some(*why)),
            };
            Resolved {
                user: j.user,
                joined_at_ms: j.joined_at_ms,
                how,
                code,
                inviter,
                reason,
                group: humans,
            }
        })
        .collect()
}

/// The public report page of a raid, when the dashboard URL is configured.
pub fn raid_url(code: &str) -> Option<String> {
    let base = std::env::var("DASHBOARD_URL")
        .ok()
        .filter(|b| !b.is_empty())?;
    Some(format!("{}/raid/{code}", base.trim_end_matches('/')))
}

/// The log shows the first 8 digits of the id next to the name.
pub fn short_id(id: u64) -> String {
    id.to_string().chars().take(8).collect()
}

/// `<10 random>_<last 10 digits of the guild id>`: names a raid on the public report page.
pub fn raid_code(gid: u64) -> String {
    let random: String = (0..10)
        .map(|_| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[rand::random_range(0..36)]))
        .collect();
    let id = gid.to_string();
    format!("{random}_{}", &id[id.len().saturating_sub(10)..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raid_code_ends_in_the_guild_id() {
        let c = raid_code(123_456_789_012_345_678);
        let (random, id) = c.split_once('_').unwrap();
        assert_eq!((random.len(), id), (10, "9012345678"));
    }

    fn base(v: &[(&str, Option<u64>, u64)]) -> Baseline {
        v.iter()
            .map(|(c, uses, owner)| {
                let owner = Some(*owner);
                (
                    c.to_string(),
                    Code {
                        uses: *uses,
                        owner,
                        max_uses: None,
                        expires_ms: None,
                        deleted: false,
                    },
                )
            })
            .collect()
    }

    fn human(user: u64) -> JoinIn {
        JoinIn {
            user,
            bot: false,
            joined_at_ms: 7,
        }
    }

    const BOT: u64 = 1;

    type Row = (How, Option<String>, Option<u64>, Option<&'static str>);

    /// (how, code, inviter, reason) of each join.
    fn run(old: Option<&Baseline>, new: &Baseline, joins: &[JoinIn], gap: bool) -> Vec<Row> {
        resolve(old, new, joins, gap, BOT, 0)
            .into_iter()
            .map(|r| (r.how, r.code, r.inviter, r.reason))
            .collect()
    }

    fn unknown(why: &'static str) -> Row {
        (How::Unknown, None, None, Some(why))
    }

    #[test]
    fn one_code_grown_by_one_is_a_single_match() {
        let old = base(&[("a", Some(4), 50), ("b", Some(9), 60)]);
        let new = base(&[("a", Some(5), 50), ("b", Some(9), 60)]);
        let r = resolve(Some(&old), &new, &[human(10)], false, BOT, 0);
        assert_eq!(
            r,
            vec![Resolved {
                user: 10,
                joined_at_ms: 7,
                how: How::Single,
                code: Some("a".into()),
                inviter: Some(50),
                reason: None,
                group: 1,
            }]
        );
    }

    #[test]
    fn two_codes_grown_is_never_a_guess() {
        let old = base(&[("a", Some(4), 50), ("b", Some(9), 60)]);
        let new = base(&[("a", Some(5), 50), ("b", Some(10), 60)]);
        let joins = [human(10), human(11)];
        assert_eq!(
            run(Some(&old), &new, &joins[..1], false),
            [unknown("ambiguous")]
        );
        assert_eq!(
            run(Some(&old), &new, &joins, false),
            [unknown("ambiguous"), unknown("ambiguous")]
        );
    }

    #[test]
    fn one_code_grown_by_the_join_count_is_a_group() {
        let old = base(&[("a", Some(4), 50)]);
        let new = base(&[("a", Some(6), 50)]);
        let r = resolve(Some(&old), &new, &[human(10), human(11)], false, BOT, 0);
        assert!(r
            .iter()
            .all(|r| r.how == How::Group && r.inviter == Some(50) && r.group == 2));
        // Grown by more or fewer than the joins: unknown.
        assert_eq!(
            run(Some(&old), &new, &[human(10)], false),
            [unknown("ambiguous")]
        );
        let three = [human(10), human(11), human(12)];
        assert!(run(Some(&old), &new, &three, false)
            .iter()
            .all(|r| r.0 == How::Unknown));
    }

    #[test]
    fn anything_odd_in_the_list_makes_the_batch_unknown() {
        let old = base(&[("a", Some(4), 50), ("b", Some(9), 60)]);
        let j = [human(10)];
        // b vanished (last use, expiry or delete): a growing is not enough.
        let gone = base(&[("a", Some(5), 50)]);
        assert_eq!(run(Some(&old), &gone, &j, false), [unknown("ambiguous")]);
        assert_eq!(
            run(Some(&old), &base(&[("a", Some(4), 50)]), &j, false),
            [unknown("ambiguous")]
        );
        // A code the baseline does not know, already used.
        let fresh = base(&[("a", Some(4), 50), ("b", Some(9), 60), ("c", Some(1), 70)]);
        assert_eq!(run(Some(&old), &fresh, &j, false), [unknown("ambiguous")]);
        // Missing uses is not 0.
        let blind = base(&[("a", Some(5), 50), ("b", None, 60)]);
        assert_eq!(run(Some(&old), &blind, &j, false), [unknown("ambiguous")]);
        // A count that went down.
        let back = base(&[("a", Some(5), 50), ("b", Some(8), 60)]);
        assert_eq!(run(Some(&old), &back, &j, false), [unknown("ambiguous")]);
        // Nothing grew (vanity URL, Discovery): no person.
        assert_eq!(run(Some(&old), &old, &j, false), [unknown("ambiguous")]);
    }

    #[test]
    fn a_vanished_code_counts_only_when_it_explains_itself() {
        let mut old = base(&[("a", Some(4), 50), ("b", Some(2), 60)]);
        let j = [human(10)];
        // b was one use short of its limit: its last use deleted it.
        old.get_mut("b").unwrap().max_uses = Some(3);
        let r = run(Some(&old), &base(&[("a", Some(4), 50)]), &j, false);
        assert_eq!(r[0].0, How::Single);
        assert_eq!(r[0].2, Some(60));
        // One short but also expired: it may have expired, so the join is not credited to it.
        old.get_mut("b").unwrap().expires_ms = Some(-1);
        let r = run(Some(&old), &base(&[("a", Some(4), 50)]), &j, false);
        assert_eq!(r, [unknown("ambiguous")]);
        old.get_mut("b").unwrap().expires_ms = None;
        // Two uses short: a delete or something else, not a use.
        old.get_mut("b").unwrap().max_uses = Some(4);
        let r = run(Some(&old), &base(&[("a", Some(4), 50)]), &j, false);
        assert_eq!(r, [unknown("ambiguous")]);
        // Expired, or deleted with the event seen: no use in it, so a's growth decides.
        for expired in [true, false] {
            let mut old = base(&[("a", Some(4), 50), ("b", Some(2), 60)]);
            let b = old.get_mut("b").unwrap();
            if expired {
                b.expires_ms = Some(-1);
            } else {
                b.deleted = true;
            }
            let r = run(Some(&old), &base(&[("a", Some(5), 50)]), &j, false);
            assert_eq!(r[0].2, Some(50));
        }
    }

    #[test]
    fn a_new_unused_code_is_fine() {
        let old = base(&[("a", Some(4), 50)]);
        let new = base(&[("a", Some(5), 50), ("c", Some(0), 70)]);
        assert_eq!(run(Some(&old), &new, &[human(10)], false)[0].0, How::Single);
    }

    #[test]
    fn no_baseline_or_a_gap_is_unknown() {
        let old = base(&[("a", Some(4), 50)]);
        let new = base(&[("a", Some(5), 50)]);
        assert_eq!(
            run(None, &new, &[human(10)], false),
            [unknown("no_baseline")]
        );
        assert_eq!(run(Some(&old), &new, &[human(10)], true), [unknown("gap")]);
    }

    #[test]
    fn bots_are_not_counted_or_credited() {
        let old = base(&[("a", Some(4), 50)]);
        let new = base(&[("a", Some(5), 50)]);
        let bot = JoinIn {
            user: 20,
            bot: true,
            joined_at_ms: 7,
        };
        let r = run(Some(&old), &new, &[bot, human(10)], false);
        assert_eq!(r[0], (How::Bot, None, None, None));
        assert_eq!(r[1], (How::Single, Some("a".into()), Some(50), None));
        assert_eq!(
            run(Some(&old), &old, &[bot], false),
            [(How::Bot, None, None, None)]
        );
        // Even with no baseline a bot is a bot.
        assert_eq!(run(None, &new, &[bot], false)[0].0, How::Bot);
    }

    #[test]
    fn the_owner_must_be_a_member_not_the_bot() {
        // `fetch` already put a Brim link's owner over Discord's inviter (the bot).
        let old = base(&[("a", Some(4), BOT)]);
        let new = base(&[("a", Some(5), BOT)]);
        assert_eq!(
            run(Some(&old), &new, &[human(10)], false),
            [unknown("no_owner")]
        );
        let mut none = new.clone();
        none.get_mut("a").unwrap().owner = None;
        assert_eq!(
            run(Some(&old), &none, &[human(10)], false),
            [unknown("no_owner")]
        );
    }

    #[test]
    fn a_self_invite_is_stored_with_the_member_as_inviter() {
        let old = base(&[("a", Some(4), 10)]);
        let new = base(&[("a", Some(5), 10)]);
        let r = run(Some(&old), &new, &[human(10)], false);
        assert_eq!(r, [(How::Single, Some("a".into()), Some(10), None)]);
    }

    use crate::test_support::T;
    use serde_json::json;
    use twilight_model::gateway::payload::incoming::{MemberAdd, MemberRemove};

    fn member_add(t: &T, user: u64, at: &str) -> MemberAdd {
        t.set(|m| {
            m.members.insert((t.g(0).get(), user), vec![]);
        });
        serde_json::from_value(json!({
            "guild_id": t.g(0).to_string(), "roles": [], "joined_at": at,
            "deaf": false, "mute": false, "flags": 0,
            "user": {"id": user.to_string(), "username": "u", "discriminator": "0", "avatar": null},
        }))
        .unwrap()
    }

    fn leave(t: &T, user: u64) {
        let m: MemberRemove = serde_json::from_value(json!({"guild_id": t.g(0).to_string(),
            "user": {"id": user.to_string(), "username": "u", "discriminator": "0", "avatar": null}}))
        .unwrap();
        observe(&t.app, &Event::MemberRemove(m));
    }

    fn uses(t: &T, n: u64) {
        t.set(|m| {
            m.invites.insert(t.g(0).get(), vec![("abc", n, 500)]);
        });
    }

    async fn seed(t: &T) {
        send(&t.app, t.g(0).get(), Msg::Seed(false));
        for _ in 0..100 {
            if tracking_ok(&t.app, t.g(0).get()) {
                // A join during the seed's settle would rightly be a gap.
                tokio::time::sleep(SETTLE + Duration::from_millis(200)).await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no baseline");
    }

    /// (how, inviter, present, joins) of the member, once `done` holds.
    async fn row(
        t: &T,
        user: u64,
        done: impl Fn(bool, i32) -> bool,
    ) -> (String, Option<i64>, bool, i32) {
        for _ in 0..200 {
            let r: Option<(String, Option<i64>, bool, i32)> = sqlx::query_as(
                "SELECT how, inviter_id, present, joins FROM invite_members WHERE guild_id = $1 AND user_id = $2",
            )
            .bind(t.g(0).get() as i64)
            .bind(user as i64)
            .fetch_optional(&t.app.pool)
            .await
            .unwrap();
            if let Some(r) = r.filter(|r| done(r.2, r.3)) {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("member {user} never got there");
    }

    /// Gateway order wins: a join then a leave ends left, a leave then a rejoin ends present.
    #[tokio::test]
    async fn events_apply_in_gateway_order() {
        let Some(t) = T::new(950).await else { return };
        t.server_config(0, t.owner(), json!({})).await;
        uses(&t, 4);
        seed(&t).await;
        uses(&t, 5);
        let m = member_add(&t, 801, "2026-01-01T00:00:00.000000+00:00");
        observe(&t.app, &Event::MemberAdd(Box::new(m)));
        leave(&t, 801);
        let r = row(&t, 801, |present, _| !present).await;
        assert_eq!((r.0.as_str(), r.1), ("single", Some(500)));

        let m = member_add(&t, 802, "2026-01-01T00:00:01.000000+00:00");
        observe(&t.app, &Event::MemberAdd(Box::new(m)));
        row(&t, 802, |present, _| present).await;
        leave(&t, 802);
        let m = member_add(&t, 802, "2026-01-01T00:00:02.000000+00:00");
        observe(&t.app, &Event::MemberAdd(Box::new(m)));
        let r = row(&t, 802, |present, joins| present && joins == 2).await;
        assert_eq!(r.0, "unknown", "the invite did not grow for 802");
        t.cleanup().await;
    }

    /// A baselined single join is credited; a raid join is punished without waiting for the tracker.
    #[tokio::test]
    async fn a_join_is_credited_and_a_raid_does_not_wait() {
        let Some(t) = T::new(951).await else { return };
        let raid = json!({"joinraid": {
            "enabled": true, "count": 3, "span_ms": 60_000, "suspicious_only": false,
            "active_ms": 60_000, "string_patterns": [], "flag_younger_than_ms": null,
            "flag_no_avatar": true, "flag_spammer": false, "id_flag_min": null, "action": "Ban",
        }});
        t.server_config(0, t.owner(), raid).await;
        uses(&t, 4);
        seed(&t).await;
        uses(&t, 5);
        let at = "2026-01-01T00:00:00.000000+00:00";
        let m = member_add(&t, 811, at);
        observe(&t.app, &Event::MemberAdd(Box::new(m.clone())));
        crate::joingate::on_join(&t.app, &m, false).await.unwrap();
        let r = row(&t, 811, |_, _| true).await;
        assert_eq!((r.0.as_str(), r.1), ("single", Some(500)));

        uses(&t, 7);
        // The tracker's invite list is held back, so its batch cannot end: a raid join that
        // waited for it would hang. The limit is far above any runner's speed (a time limit near
        // the batch time was flaky on a loaded runner).
        t.set(|m| m.hang_invites = true);
        // The gateway loop hands both joins over before their tasks run.
        let joins = [member_add(&t, 812, at), member_add(&t, 813, at)];
        for m in &joins {
            observe(&t.app, &Event::MemberAdd(Box::new(m.clone())));
        }
        for m in &joins {
            tokio::time::timeout(
                Duration::from_secs(60),
                crate::joingate::on_join(&t.app, m, false),
            )
            .await
            .expect("a raid join waited for the invite tracker")
            .unwrap();
        }
        assert!(t.banned(0, 813).is_some(), "the raid join was not punished");
        t.set(|m| m.hang_invites = false);
        let r = row(&t, 813, |_, _| true).await;
        assert_eq!(r.0, "group", "812 and 813 share one batch and a +2");
        t.cleanup().await;
    }
}

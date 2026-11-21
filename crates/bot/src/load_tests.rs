//! Load and fairness tests (security review-11-17, s02:t-cap-pool-gateway and s02:t-cap-job-fifo):
//! does a flood or a big job in server A delay server B's protection? They print what they
//! measure and fail when B waits longer than a few seconds. The flood test is `#[ignore]` (two
//! minutes); run it with `cargo test -p brim load_tests -- --include-ignored --nocapture`.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::json;
use twilight_model::gateway::{event::Event, payload::incoming::MessageCreate};

use crate::{automod::more_tests::msg, events, test_support::T};

/// How long B may wait for its own protection.
const BUDGET: Duration = Duration::from_secs(3);
/// Discord's round trip for one request, as the mock answers it.
const RTT_MS: u64 = 100;

/// The READY test marks every configured server in the shared test DB, so the tests that read
/// guild_left take turns.
static GUILD_LEFT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Polls until `done` or 120 s, and returns the time it took.
async fn wait(done: impl Fn() -> bool) -> Duration {
    let start = Instant::now();
    while !done() && start.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    start.elapsed()
}

fn post(t: &T, i: u64, channel: u64, id: u64, user: u64) -> Event {
    let m = msg(
        json!({"guild_id": t.g(i).to_string(), "channel_id": channel.to_string(),
        "id": id.to_string(), "content": "darn it",
        "member": {"roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00", "deaf": false, "mute": false, "flags": 0},
        "author": {"id": user.to_string(), "username": "x", "discriminator": "0", "avatar": null}}),
    );
    Event::MessageCreate(Box::new(MessageCreate {
        channel_type: None,
        message: m,
    }))
}

/// A flood of filtered messages in A (each one a delete), then one filtered message in B: how
/// long until B's message is deleted, with twilight's real limit of 50 requests a second.
#[test]
#[ignore]
fn a_message_flood_in_one_server_delays_another() {
    // Debug builds make the event handler's future too deep for tokio's 2 MiB default.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(32 << 20)
        .enable_all()
        .build()
        .unwrap()
        .block_on(flood());
}

async fn flood() {
    let Some(t) = T::with_limiter(992, true).await else {
        return;
    };
    let cfg = json!({"automod": {"enabled": true, "filters": {
        "words": {"filter": {"enabled": true, "action": "delete"}, "strict": ["darn"]}}}});
    t.server_config(0, t.owner(), cfg.clone()).await;
    t.server_config(1, t.owner(), cfg).await;
    const FLOOD: u64 = 1000;
    const CHANNELS: u64 = 50;
    t.set(|m| {
        m.delay_ms = RTT_MS;
        for u in 0..FLOOD {
            m.members.insert((t.g(0).get(), t.base + 100 + u), vec![]);
        }
        m.members.insert((t.g(1).get(), t.base + 8), vec![]);
        // A's flood is spread over CHANNELS channels, as a raid posts in every channel it can.
        let spread: Vec<(u64, u64)> = (0..CHANNELS).map(|c| (0, 1000 + c)).collect();
        for i in 0..2 {
            let g = t.g(i).get();
            let mine: Vec<u64> = match i {
                0 => spread.iter().map(|x| x.1).collect(),
                _ => vec![8],
            };
            for c in &mine {
                m.text_channels.insert(*c, (g, true));
            }
            let ch: Vec<_> = mine
                .iter()
                .map(|c| {
                    json!({"id": c.to_string(), "type": 0,
                "guild_id": g.to_string(), "name": "chat", "position": 0,
                "permission_overwrites": []})
                })
                .collect();
            m.channels.insert(g, json!(ch));
        }
    });
    // As a GuildCreate fills it live: the bot knows the channels, so it fetches none per message.
    for (i, chans) in [
        (0, (1000..1000 + CHANNELS).collect::<Vec<_>>()),
        (1, vec![8]),
    ] {
        let info = crate::hierarchy::GuildInfo {
            owner: twilight_model::id::Id::new(t.owner()),
            roles: Default::default(),
            role_perms: Default::default(),
            managed: Default::default(),
            unpermittable: Default::default(),
            parents: Default::default(),
            channels: chans.into_iter().map(twilight_model::id::Id::new).collect(),
            nsfw: Default::default(),
            voice: Default::default(),
            bot_top: 10,
            fetched: Instant::now(),
        };
        t.app.guilds.lock().unwrap().insert(t.g(i), Arc::new(info));
    }
    let tr = &t;
    let b_deleted = |id: u64| {
        let what = format!("DELETE /channels/8/messages/{id}");
        move || tr.calls(&what) == 1
    };

    // Baseline: B alone.
    tokio::spawn(events::handle(
        t.app.clone(),
        post(&t, 1, 8, 900, t.base + 8),
        Default::default(),
    ));
    let alone = wait(b_deleted(900)).await;

    // A floods: FLOOD messages at once, as the gateway loop spawns them.
    let flood = Instant::now();
    for u in 0..FLOOD {
        let ev = post(&t, 0, 1000 + u % CHANNELS, 10_000 + u, t.base + 100 + u);
        tokio::spawn(events::handle(t.app.clone(), ev, Default::default()));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::spawn(events::handle(
        t.app.clone(),
        post(&t, 1, 8, 901, t.base + 8),
        Default::default(),
    ));
    let during = wait(b_deleted(901)).await;
    let drained = wait(|| t.calls("/messages/1") as u64 >= FLOOD).await;
    eprintln!(
        "flood: B alone {alone:?}; B during a {FLOOD}-message flood in A over {CHANNELS} channels {during:?}; \
         A's flood handled in {:?} ({} of {FLOOD} deletes)",
        flood.elapsed().max(drained),
        t.calls("/messages/1")
    );
    t.cleanup().await;
    assert!(during < BUDGET, "B waited {during:?} behind A's flood");
}

/// A member scan of a large server A (the slow kind of job), queued just before B's panic start:
/// how long until B's panic mode starts (3.25 s when panic start shared the scan's worker).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_big_job_in_one_server_does_not_delay_another_servers_panic_start() {
    let Some(t) = T::new(993).await else { return };
    const PAGES: u64 = 30;
    t.server_config(0, t.owner(), json!({"raider_list": {"scan_members": true}}))
        .await;
    t.server_config(1, t.owner(), json!({})).await;
    // A page is 2 members in tests (1000 live): PAGES pages stand for a server of PAGES * 1000.
    t.set(|m| {
        m.delay_ms = RTT_MS;
        for u in 0..PAGES * 2 {
            m.members.insert((t.g(0).get(), t.base + 100 + u), vec![]);
        }
    });
    let now = crate::app::now_secs();
    let (a, b) = (t.g(0).get() as i64, t.g(1).get() as i64);
    brim_core::jobs::enqueue(&t.app.pool, a, "raider_scan", json!({}), now - 2)
        .await
        .unwrap();
    brim_core::jobs::enqueue(
        &t.app.pool,
        b,
        "panic_start",
        json!({"by": t.owner()}),
        now - 1,
    )
    .await
    .unwrap();
    let workers = [
        tokio::spawn(crate::scheduler::run_restore(t.app.clone())),
        tokio::spawn(crate::scheduler::run_urgent(t.app.clone())),
    ];
    let started = {
        let app: Arc<_> = t.app.clone();
        wait(move || app.panic_runs.lock().unwrap().contains_key(&(b as u64))).await
    };
    let scan_pages = t.calls(&format!("GET /guilds/{a}/members"));
    workers.iter().for_each(|w| w.abort());
    eprintln!(
        "jobs: B's panic start began after {started:?}, while A's member scan ran \
         ({scan_pages} member pages at {RTT_MS} ms each so far)"
    );
    t.cleanup().await;
    assert!(
        started < BUDGET,
        "B's panic start waited {started:?} behind A's scan"
    );
}

/// s02:t-life-left-guild: a READY that leaves out a server the bot is in marks it left (its data
/// is purged 30 days later), and only that server's GuildCreate clears the mark.
#[tokio::test]
async fn a_partial_ready_marks_the_missing_server_left() {
    use twilight_model::{
        gateway::payload::incoming::{GuildCreate, Ready},
        guild::UnavailableGuild,
    };
    let _turn = GUILD_LEFT.lock().await;
    let Some(t) = T::new(994).await else { return };
    for i in 0..3 {
        t.server_config(i, t.owner(), json!({})).await;
    }
    let ready = |ids: &[u64]| -> Ready {
        let guilds: Vec<_> = ids
            .iter()
            .map(|i| json!({"id": t.g(*i).to_string(), "unavailable": true}))
            .collect();
        serde_json::from_value(
            json!({"application": {"id": "1", "flags": 0}, "guilds": guilds,
            "resume_gateway_url": "wss://x", "session_id": "s", "v": 10,
            "user": {"id": "1", "username": "bot", "discriminator": "0", "avatar": null,
                "bot": true, "mfa_enabled": false, "flags": 0, "public_flags": 0}}),
        )
        .unwrap()
    };
    let left = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT guild_id FROM guild_left WHERE guild_id BETWEEN $1 AND $1 + 9 ORDER BY 1",
        )
        .bind(t.g(0).get() as i64)
        .fetch_all(&t.app.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g as u64 - t.base)
        .collect::<Vec<_>>()
    };
    // The bot is in 0, 1 and 2; READY names only 0 (1 and 2 missing).
    crate::welcome::on_ready(&t.app, &ready(&[0]))
        .await
        .unwrap();
    assert_eq!(left().await, [1, 2], "the missing servers are marked left");
    // Server 1's GuildCreate clears its mark.
    let create = |i: u64| {
        GuildCreate::Unavailable(UnavailableGuild {
            id: t.g(i),
            unavailable: true,
        })
    };
    crate::welcome::on_guild_create(&t.app, &create(1))
        .await
        .unwrap();
    assert_eq!(left().await, [2]);
    t.cleanup().await;
}

/// A server marked left 31 days ago is purged only when Discord says the bot is not in it (404, or
/// 403 Missing Access). One Discord still answers for (a wrong mark) keeps its data and loses the
/// mark, also before it is due; a Cloudflare 403 or a 5xx is no answer and keeps both.
#[tokio::test]
async fn a_wrongly_marked_server_is_not_purged() {
    let _turn = GUILD_LEFT.lock().await;
    let Some(t) = T::new(995).await else { return };
    for i in 0..6 {
        t.server_config(i, t.owner(), json!({})).await;
    }
    // 0 and 5 are still there (5 marked only a day ago); 1 answers 404, 2 Missing Access, 3 a
    // Cloudflare 403 without a code, 4 a 502.
    t.set(|m| {
        m.owners.remove(&t.g(1).get());
        for (i, e) in [(2, (403, 50001)), (3, (403, 0)), (4, (502, 0))] {
            m.guild_err.insert(t.g(i).get(), e);
        }
    });
    t.exec(
        "INSERT INTO guild_left (guild_id, left_at) SELECT $1 + i, now() - interval '31 days'
         FROM generate_series(0, 4) i UNION ALL SELECT $1 + 5, now() - interval '1 day'",
    )
    .await;
    crate::welcome::purge_removed(&t.app).await.unwrap();
    let kept = sqlx::query_scalar::<_, i64>(
        "SELECT guild_id - $1 FROM guild_config WHERE guild_id BETWEEN $1 AND $1 + 9 ORDER BY 1",
    )
    .bind(t.g(0).get() as i64)
    .fetch_all(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(
        kept,
        [0, 3, 4, 5],
        "only the servers Discord says are gone are purged"
    );
    let marked = sqlx::query_scalar::<_, i64>(
        "SELECT guild_id - $1 FROM guild_left WHERE guild_id BETWEEN $1 AND $1 + 9 ORDER BY 1",
    )
    .bind(t.g(0).get() as i64)
    .fetch_all(&t.app.pool)
    .await
    .unwrap();
    assert_eq!(
        marked,
        [3, 4],
        "the wrong marks are cleared, the unanswered ones stay"
    );
    t.cleanup().await;
}

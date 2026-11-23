mod activity;
#[cfg(test)]
mod answer_texts_tests;
mod antinuke;
mod app;
mod appeals;
mod automod;
mod backup;
mod caps;
mod catch;
mod cmdcfg;
mod commands;
mod confirm;
#[cfg(test)]
mod d3_texts_tests;
#[cfg(test)]
mod d4_texts_tests;
#[cfg(test)]
mod d5_texts_tests;
#[cfg(test)]
mod d7_texts_tests;
mod datareq;
mod deadroles;
mod dehoist;
mod events;
mod feeds;
mod general;
mod giveaways;
mod hierarchy;
mod http_err;
mod info;
mod invite_cmds;
mod invites;
mod joingate;
#[cfg(test)]
mod load_tests;
mod lockdown;
mod metrics;
#[cfg(test)]
mod mod_texts_tests;
mod modcmds;
mod modslash;
mod modstats;
mod modview;
mod msgbackup;
mod msghooks;
mod msgmenu;
mod msgrules;
mod native;
mod nsfw;
mod owner;
#[cfg(test)]
mod panel_texts_tests;
mod panic;
mod permit_sync;
mod prefix;
mod presence;
mod previews;
mod profile_log;
mod proof;
mod protected;
mod purge;
mod qr;
#[cfg(test)]
mod quarantine_tests;
mod raiders;
mod relink;
mod reply;
mod reports;
mod retention;
mod scheduler;
mod settings;
mod setup;
mod slash;
mod slowmode;
mod staff_hoist;
mod sticky;
mod tags;
mod targets;
#[cfg(test)]
mod test_support;
mod tickets;
mod timeout_end;
mod timeout_roles;
mod undo;
mod users_only;
mod verify;
mod voice_nuke;
mod voice_raid;
mod webhooks;
mod welcome;
mod whitelist;

use anyhow::Result;
use brim_core::{config::Config, db};
use std::sync::Arc;
use twilight_gateway::{
    error::ReceiveMessageError, Event, EventTypeFlags, Intents, Message, Shard, ShardId,
};
use twilight_model::gateway::event::GatewayEventDeserializer;

/// Loads the ids of the application emojis that `tools/emojis.mjs upload` uploaded.
async fn load_emojis(http: &twilight_http::Client) -> Result<()> {
    let app = http.current_user_application().await?.model().await?.id;
    let list = http.get_application_emojis(app).await?.model().await?;
    brim_logging::emoji::load(list.items.into_iter().map(|e| (e.name, e.id.get())));
    Ok(())
}

/// The application's owner, the team owner for a team app (as the dashboard's `app_owner`).
async fn bot_owner(
    http: &twilight_http::Client,
) -> Option<twilight_model::id::Id<twilight_model::id::marker::UserMarker>> {
    let found = async { anyhow::Ok(http.current_user_application().await?.model().await?) }.await;
    match found {
        Ok(a) => a.team.map(|t| t.owner_user_id).or(a.owner.map(|o| o.id)),
        Err(e) => {
            tracing::warn!(?e, "bot owner unknown, the Raider List cannot exempt them");
            None
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _log = brim_core::log::init("bot", tracing_subscriber::EnvFilter::from_default_env());

    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;
    let http = twilight_http::Client::new(cfg.discord_token.clone());
    let me = http.current_user().await?.model().await?.id;
    let version = std::env::var("BRIM_SHA").unwrap_or_else(|_| "dev".into());
    tracing::info!(%version, bot = %me, "starting");
    if let Err(e) = load_emojis(&http).await {
        tracing::warn!(?e, "custom emojis not loaded, using Unicode fallbacks");
    }
    if let Err(e) = previews::publish(&pool).await {
        tracing::warn!(?e, "saving the dashboard previews failed");
    }
    let bot_owner = bot_owner(&http).await;
    let mut app = app::App::new(
        http,
        twilight_http::Client::new(cfg.discord_token.clone()),
        pool,
        brim_automod::discord_api::Api::new(&cfg.discord_token),
        me,
    );
    app.bot_owner = bot_owner;
    let app = Arc::new(app);
    tokio::spawn({
        let app = app.clone();
        async move {
            // Without the listener, saved config only applies after a restart, so it never gives up.
            loop {
                if let Err(e) = app.clone().listen_config().await {
                    // The pool closes at shutdown; that ends the listener, it is not an error.
                    if app.pool.is_closed() {
                        break;
                    }
                    tracing::error!(?e, "config listener stopped, retrying");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });

    if let Err(e) = welcome::load_audit_seen(&app).await {
        tracing::warn!(?e, "loading the last audit entries failed");
    }
    tokio::spawn(welcome::flush_audit_loop(app.clone()));
    if let Err(e) = antinuke::seed_windows(&app).await {
        tracing::warn!(
            ?e,
            "restoring the anti-nuke and command limit windows failed"
        );
    }
    if let Err(e) = panic::restore(&app).await {
        tracing::warn!(?e, "restoring panic mode failed");
    }
    if let Err(e) = voice_raid::restore(&app).await {
        tracing::warn!(?e, "restoring voice raids failed");
    }
    if let Err(e) = slowmode::restore(&app).await {
        tracing::warn!(?e, "loading the auto slowmode channels failed");
    }
    tokio::spawn({
        let app = app.clone();
        async move {
            if let Err(e) = lockdown::resume(&app).await {
                tracing::warn!(?e, "resuming interrupted locks failed");
            }
        }
    });
    metrics::load_seen(&app.pool).await;
    appeals::load_accepted(&app.pool).await;
    tokio::spawn(metrics::keep_seen(app.pool.clone()));
    metrics::STARTED.store(app::now_secs() as u64, std::sync::atomic::Ordering::Relaxed);
    tokio::spawn(scheduler::run(app.clone()));
    tokio::spawn(metrics::heartbeat());
    tokio::spawn({
        let app = app.clone();
        async move {
            let mut failures = 0;
            loop {
                let loaded = app.sites.refresh().await;
                let wait = brim_automod::sites::next_refresh(loaded, &mut failures);
                tokio::time::sleep(wait).await;
            }
        }
    });
    tokio::spawn(scheduler::run_restore(app.clone()));
    tokio::spawn(scheduler::run_urgent(app.clone()));
    if let Ok(addr) = std::env::var("METRICS_ADDR") {
        tokio::spawn({
            let app = app.clone();
            async move {
                if let Err(e) = metrics::serve(app, addr).await {
                    tracing::error!(?e, "metrics server stopped");
                }
            }
        });
    }
    tokio::spawn(events::flush_logs(app.clone()));
    tokio::spawn(backup::auto_backup(app.clone()));
    tokio::spawn(tickets::auto_close(app.clone()));
    tokio::spawn(welcome::purge_left(app.clone()));
    // CPU the image scan may use: one model run per worker at a time.
    let workers = std::env::var("NSFW_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1usize)
        .clamp(1, 8);
    for _ in 0..workers {
        tokio::spawn(nsfw::run(app.clone()));
    }
    tokio::spawn(nsfw::sweep(app.clone()));
    tokio::spawn(feeds::run(app.clone()));

    if let Err(e) = slash::register(&app).await {
        tracing::warn!(?e, "slash command registration failed");
    }

    let intents = Intents::GUILDS
        | Intents::GUILD_MEMBERS
        | Intents::GUILD_MODERATION
        | Intents::AUTO_MODERATION_EXECUTION
        | Intents::AUTO_MODERATION_CONFIGURATION
        | Intents::GUILD_MESSAGES
        | Intents::MESSAGE_CONTENT
        | Intents::GUILD_VOICE_STATES
        | Intents::GUILD_MESSAGE_REACTIONS
        | Intents::GUILD_INVITES
        | Intents::GUILD_MESSAGE_POLLS
        | Intents::GUILD_SCHEDULED_EVENTS
        | Intents::DIRECT_MESSAGES;
    let mut shard = Shard::new(ShardId::ONE, cfg.discord_token, intents);
    tokio::spawn({
        let (pool, sender) = (app.pool.clone(), shard.sender());
        async move {
            // Like the config listener: a dashboard change must not wait for the next READY.
            loop {
                if let Err(e) = presence::listen(pool.clone(), sender.clone()).await {
                    if pool.is_closed() || sender.is_closed() {
                        break;
                    }
                    tracing::error!(?e, "presence listener stopped, retrying");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });

    let stop = shutdown_signal();
    tokio::pin!(stop);
    loop {
        let msg = tokio::select! {
            _ = &mut stop => break,
            m = next_message(&mut shard) => m,
        };
        let Some(msg) = msg else { break };
        let Some(event) = msg.map_or_else(|e| Some(Err(e)), to_event) else {
            continue;
        };
        match event {
            Ok((event, extra)) => {
                if let Some(d) = shard.latency().average() {
                    metrics::LATENCY_MS
                        .store(d.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
                }
                if matches!(event, Event::Ready(_)) {
                    welcome::note_ready(&app);
                    let (pool, sender) = (app.pool.clone(), shard.sender());
                    tokio::spawn(async move {
                        if let Err(e) = presence::apply(&pool, &sender).await {
                            tracing::warn!(?e, "setting the bot presence failed");
                        }
                    });
                }
                // In gateway order, before the event's own task: the tracker needs that order.
                invites::observe(&app, &event);
                tokio::spawn(events::handle(app.clone(), event, extra));
            }
            Err(e) => {
                tracing::warn!(?e, "gateway error");
                metrics::GATEWAY_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    tracing::info!("shutting down");
    events::shutdown(&app).await;
    Ok(())
}

/// The next websocket message. Read raw (not `next_event`) so `to_event` sees the JSON before twilight parses it.
fn next_message<S: twilight_gateway::StreamExt + Unpin>(
    s: &mut S,
) -> impl std::future::Future<Output = Option<S::Item>> + '_ {
    std::future::poll_fn(move |cx| std::pin::Pin::new(&mut *s).poll_next(cx))
}

/// What twilight-gateway 0.18 `next_event` does with one message (`stream.rs`), plus the raw spam flag.
/// None: an event type or opcode twilight skips.
fn to_event(msg: Message) -> Option<Result<(Event, events::Extra), ReceiveMessageError>> {
    match msg {
        Message::Close(frame) => Some(Ok((Event::GatewayClose(frame), events::Extra::default()))),
        Message::Text(json) => {
            let extra = events::Extra {
                spammer: spam_flag(&json),
            };
            twilight_gateway::parse(json, EventTypeFlags::all())
                .transpose()
                .map(|r| r.map(|e| (e.into(), extra)))
        }
    }
}

/// Discord's undocumented "likely spammer" user flag (`1 << 20`) on a GUILD_MEMBER_ADD frame.
/// twilight's `UserFlags` drops unknown bits, so it is read from the raw JSON.
fn spam_flag(json: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Frame {
        t: Option<String>,
        d: Data,
    }
    #[derive(serde::Deserialize)]
    struct Data {
        user: User,
    }
    #[derive(serde::Deserialize)]
    struct User {
        public_flags: Option<u64>,
    }
    // Cheap check first: every frame passes here, only joins get parsed. twilight's own scan, so
    // whitespace or key order Discord might send does not hide a join.
    GatewayEventDeserializer::from_json(json)
        .is_some_and(|d| d.event_type() == Some("GUILD_MEMBER_ADD"))
        && serde_json::from_str::<Frame>(json).is_ok_and(|f| {
            f.t.as_deref() == Some("GUILD_MEMBER_ADD")
                && f.d.user.public_flags.unwrap_or(0) & brim_automod::join::SPAMMER_FLAG != 0
        })
}

/// Resolves on Ctrl-C or, on Unix, SIGTERM (what `docker stop` sends).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join(flags: &str) -> String {
        format!(
            r#"{{"op":0,"s":5,"t":"GUILD_MEMBER_ADD","d":{{"guild_id":"1","joined_at":"2026-01-01T00:00:00.000000+00:00","roles":[],"deaf":false,"mute":false,"flags":0,"user":{{"id":"2","username":"a","discriminator":"0","avatar":null{flags}}}}}}}"#
        )
    }

    #[test]
    fn spam_flag_reads_bit_20() {
        assert!(spam_flag(&join(r#","public_flags":1048576"#)));
        assert!(spam_flag(&join(r#","public_flags":1048832"#)));
        assert!(!spam_flag(&join(r#","public_flags":0"#)));
        assert!(!spam_flag(&join(r#","public_flags":256"#)));
        assert!(!spam_flag(&join(r#","public_flags":null"#)));
        assert!(!spam_flag(&join("")));
        let other =
            join(r#","public_flags":1048576"#).replace("GUILD_MEMBER_ADD", "GUILD_MEMBER_UPDATE");
        assert!(!spam_flag(&other));
        let spaced = join(r#","public_flags":1048576"#).replace(r#""t":"#, r#""t": "#);
        assert!(spaced.contains(r#""t": "GUILD_MEMBER_ADD""#));
        assert!(spam_flag(&spaced));
    }

    #[test]
    fn join_frame_still_parses_to_member_add() {
        let json = join(r#","public_flags":1048576"#);
        let Some(Ok((Event::MemberAdd(m), extra))) = to_event(Message::Text(json.clone())) else {
            panic!("not a MemberAdd");
        };
        assert!(extra.spammer);
        assert_eq!((m.guild_id.get(), m.user.id.get()), (1, 2));
        let old = twilight_gateway::parse(json, EventTypeFlags::all())
            .unwrap()
            .map(Event::from);
        assert_eq!(old, Some(Event::MemberAdd(m)));
        assert!(matches!(
            to_event(Message::Close(None)),
            Some(Ok((Event::GatewayClose(None), _)))
        ));
    }
}

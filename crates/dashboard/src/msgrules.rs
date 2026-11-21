//! Message Builder phase 5: schedule rules ("Automate" tab). Owner, Extra Owners and Trusted Admins
//! make them; the dashboard runs them (it holds the send code: variables, pings, webhooks), and the
//! bot only marks "every N messages" as due.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    embeds::{self, Sender},
    lockdown::audit,
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use brim_core::msgsched::{self, Spec};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

/// Schedule rules per message (plan 6: the reference's 7).
pub(crate) const MAX_RULES: i64 = 7;
/// A run more than this late (Brim was down) is skipped, not sent (plan 3).
const LATE: i64 = 10 * 60;
/// A "once" run that waits out panic mode, a lockdown or a nuke case is still sent within this.
const ONCE_GRACE: i64 = 60 * 60;

pub fn routes() -> Router<AppState> {
    let r = "/api/guilds/{guild}/embeds/{id}/rules";
    Router::new()
        .route(r, get(list).post(save))
        .route(&format!("{r}/preview"), post(preview))
        .route(&format!("{r}/{{rule}}/delete"), post(delete))
}

/// Each line with its next three runs (unix seconds), for the list under it.
fn lines_view(s: &Spec, now: DateTime<Utc>) -> Vec<Value> {
    s.lines
        .iter()
        .map(|l| {
            json!({
                "text": msgsched::describe(l),
                "next": msgsched::upcoming(s, l, now, 3).iter().map(DateTime::timestamp).collect::<Vec<_>>(),
            })
        })
        .collect()
}

async fn list(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, bool, Option<i64>, Value, Option<DateTime<Utc>>, Option<String>, Option<DateTime<Utc>>, i64)> =
        sqlx::query_as(
            "select id, enabled, channel_id, spec, next_at, issue, last_run, created_by from message_rules
             where embed_id = $1 and guild_id = $2 and kind = 'schedule' order by id",
        )
        .bind(id)
        .bind(guild)
        .fetch_all(&st.db)
        .await?;
    let now = Utc::now();
    Ok(Json(json!(rows
        .into_iter()
        .map(|(rid, on, ch, spec, next, issue, last, by)| {
            let lines = msgsched::parse(&spec, now)
                .map(|p| lines_view(&p, now))
                .unwrap_or_default();
            json!({
                "id": rid, "enabled": on, "channel": ch.map(|c| c.to_string()), "spec": spec,
                "next": next.map(|n| n.timestamp()), "issue": issue, "last_run": last.map(|l| l.timestamp()),
                "created_by": by.to_string(), "lines": lines,
            })
        })
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct Preview {
    spec: Value,
}

/// The next runs of a rule being edited, before it is saved.
async fn preview(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, _)): Path<(i64, i64)>,
    Json(b): Json<Preview>,
) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let now = Utc::now();
    let spec = msgsched::parse(&b.spec, now).map_err(ApiError::BadRequest)?;
    Ok(Json(json!({"lines": lines_view(&spec, now)})))
}

#[derive(Deserialize)]
struct Save {
    #[serde(default)]
    rule: Option<i64>,
    channel: Option<String>,
    spec: Value,
    enabled: bool,
    /// The message version the page showed (see [`embeds::VERSION`]).
    #[serde(default)]
    seen: Option<String>,
}

/// Rules in a whole server (plan 6: bounds the trigger path).
pub(crate) const MAX_SERVER_RULES: i64 = 50;

/// Refuses a new rule past [`MAX_SERVER_RULES`]; counted under a per-server lock so two saves
/// cannot both pass.
pub(crate) async fn server_room(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    guild: i64,
) -> Result<(), ApiError> {
    sqlx::query("select pg_advisory_xact_lock(hashtext('message_rules'), $1::int)")
        .bind((guild % i64::from(i32::MAX)) as i32)
        .execute(&mut **tx)
        .await?;
    let n: i64 = sqlx::query_scalar("select count(*) from message_rules where guild_id = $1")
        .bind(guild)
        .fetch_one(&mut **tx)
        .await?;
    if n >= MAX_SERVER_RULES {
        return Err(ApiError::BadRequest(format!(
            "a server has at most {MAX_SERVER_RULES} schedules and triggers: delete one first"
        )));
    }
    Ok(())
}

/// Locks the message row (a save of the message, `embeds::save`, checks for rules under the same
/// lock) and returns its version. A rule that is turned on vouches for the message, so then the
/// message must be ready (`answer`: for a trigger answer, where member variables work), be the
/// version the page showed, and not be a save by an older Brim (after a rollback).
pub(crate) async fn lock_message(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    guild: i64,
    id: i64,
    enabled: bool,
    seen: Option<&str>,
    answer: bool,
) -> Result<String, ApiError> {
    let msg: Option<(Value, String, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {}, {}, saved_at = updated_at from embeds where id = $1 and guild_id = $2 for update",
        embeds::MSG,
        embeds::VERSION
    )))
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut **tx)
    .await?;
    let (msg, version, current) = msg.ok_or(ApiError::NotFound)?;
    if enabled {
        if let Some(why) = embeds::missing_in(&msg, answer) {
            return Err(ApiError::BadRequest(format!(
                "the message is not ready, so the rule cannot be on: {why}"
            )));
        }
        if seen != Some(version.as_str()) {
            return Err(ApiError::BadRequest(
                "the message changed since you opened it: reload the page and check it first"
                    .into(),
            ));
        }
        if !current {
            return Err(ApiError::BadRequest(
                "an older Brim version saved this message: save it again on the Message tab first"
                    .into(),
            ));
        }
    }
    Ok(version)
}

/// A rule that is on sends the message as this Trusted Admin saw it ([`lock_message`] checked
/// that), so turning it on vouches for that version, as a save of the message would.
pub(crate) async fn vouch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    guild: i64,
    id: i64,
    level: Level,
) -> Result<String, ApiError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "update embeds set saved_level = greatest(saved_level, $3), saved_at = now(), updated_at = now()
         where id = $1 and guild_id = $2 returning {}",
        embeds::VERSION
    )))
    .bind(id)
    .bind(guild)
    .bind(if level >= Level::Owner { 4i16 } else { 3 })
    .fetch_one(&mut **tx)
    .await?)
}

/// Whether the message was last saved (or vouched for) by an Owner, Extra Owner or Trusted Admin of
/// this Brim version: an older image's save after a rollback skips the rights check.
pub(crate) async fn trusted(st: &AppState, guild: i64, embed: i64) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(
        "select saved_level >= 3 and saved_at = updated_at from embeds where id = $1 and guild_id = $2",
    )
    .bind(embed)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?
    .unwrap_or(false))
}

/// A rule runs on the rights of who last saved it: an Owner, Extra Owner or Trusted Admin, as
/// `save` asks (security review). Once they lost that level the rule is turned off with the reason
/// on it, and logged once; a save by someone with the level turns it on again. True: it may run.
pub(crate) async fn saver_may_run(
    st: &AppState,
    guild: i64,
    rule: i64,
    what: &str,
) -> Result<bool, ApiError> {
    // Sticky messages follow the same rule (owner decision 2026-11-17); `what` names the fixed table.
    let table = if what == "sticky message" {
        "sticky_msgs"
    } else {
        "message_rules"
    };
    let by: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select updated_by from {table} where id = $1 and guild_id = $2"
    )))
    .bind(rule)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let Some(by) = by else { return Ok(false) };
    let saver = Sess {
        id: String::new(),
        user_id: by,
        username: String::new(),
        access_token: String::new(),
        stepped_up: false,
        step_up_guild: None,
        csrf: String::new(),
    };
    // An unread level fails the run (it is tried again next time), and pauses nothing.
    if api::level_of(st, &saver, guild).await? >= Level::Admin {
        return Ok(true);
    }
    let text = format!(
        "Paused: the member who last saved this {what} (user id {by}) is no longer an Owner, Extra Owner or Trusted Admin. To confirm it, one of them chooses Change, turns it On and saves it."
    );
    // `updated_by` again: a save that confirmed it meanwhile is not paused.
    let paused = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update {table} set enabled = false, issue = $3 where id = $1 and guild_id = $4 and enabled and updated_by = $2"
    )))
    .bind(rule)
    .bind(by)
    .bind(&text)
    .bind(guild)
    .execute(&st.db)
    .await?
    .rows_affected();
    if paused > 0 {
        let (_, c) = crate::store::current(&st.db, guild).await?;
        let line = format!(
            "Brim paused the Message Builder {what} #{rule}: <@{by}>, who last saved it, is no longer an Owner, Extra Owner or Trusted Admin. It runs again once one of them saves it."
        );
        // 0: Brim itself made the change.
        api::log_change(st, guild, 0, &c, None, line).await;
    }
    Ok(false)
}

/// Checks a rule's channel: in this server, not one of Brim's own channels.
pub(crate) async fn check_channel(st: &AppState, guild: i64, channel: i64) -> Result<(), ApiError> {
    let ch = embeds::discord(st, Method::GET, &format!("/channels/{channel}"), None)
        .await?
        .unwrap_or_default();
    if ch["guild_id"].as_str().and_then(|g| g.parse().ok()) != Some(guild) {
        return Err(ApiError::BadRequest(
            "that channel is not in this server".into(),
        ));
    }
    let (_, config) = crate::store::current(&st.db, guild).await?;
    if embeds::bot_channel(&config, channel, &embeds::parents(st, &ch).await?) {
        return Err(ApiError::BadRequest(
            "that is a log, ticket, verification or appeal channel: Brim does not post built messages there".into(),
        ));
    }
    Ok(())
}

/// Adds or changes a schedule rule (Owner, Extra Owners, Trusted Admins).
async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Save>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Admin, false).await?;
    let now = Utc::now();
    let channel: i64 = b
        .channel
        .as_deref()
        .and_then(|c| c.trim().parse().ok())
        .ok_or_else(|| ApiError::BadRequest("Choose a channel first.".into()))?;
    check_channel(&st, guild, channel).await?;
    let mut tx = st.db.begin().await?;
    let version = lock_message(&mut tx, guild, id, b.enabled, b.seen.as_deref(), false).await?;
    // A changed rule keeps its "every N minutes" anchor only when that is all that changed is not
    // worth tracking: every save starts the repeats again from now.
    let mut given = b.spec.clone();
    given.as_object_mut().map(|o| o.remove("start"));
    let spec = msgsched::parse(&given, now).map_err(ApiError::BadRequest)?;
    if msgsched::too_close(&spec, now) {
        return Err(ApiError::BadRequest(format!(
            "two times of this schedule come less than {} minutes apart: move one",
            msgsched::MIN_MINUTES
        )));
    }
    let next = msgsched::next(&spec, now);
    if b.enabled && next.is_none() && msgsched::messages(&spec).is_none() {
        return Err(ApiError::BadRequest(
            "every time of this schedule is in the past: add a time".into(),
        ));
    }
    let stored = msgsched::to_json(&spec);
    let rid: i64 = match b.rule {
        Some(rid) => sqlx::query_scalar(
            "update message_rules set channel_id = $3, spec = $4, enabled = $5, next_at = $6, due_messages = false,
                 waiting_once = false, issue = null, updated_by = $7, updated_at = now()
             where id = $1 and embed_id = $2 and guild_id = $8 and kind = 'schedule' returning id",
        )
        .bind(rid)
        .bind(id)
        .bind(channel)
        .bind(&stored)
        .bind(b.enabled)
        .bind(next)
        .bind(s.user_id)
        .bind(guild)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?,
        None => {
            let n: i64 = sqlx::query_scalar(
                "select count(*) from message_rules where embed_id = $1 and kind = 'schedule' and guild_id = $2",
            )
            .bind(id)
            .bind(guild)
            .fetch_one(&mut *tx)
            .await?;
            if n >= MAX_RULES {
                return Err(ApiError::BadRequest(format!(
                    "a message has at most {MAX_RULES} schedules"
                )));
            }
            server_room(&mut tx, guild).await?;
            sqlx::query_scalar(
                "insert into message_rules (embed_id, guild_id, kind, enabled, channel_id, spec, next_at, created_by, updated_by)
                 values ($1, $2, 'schedule', $3, $4, $5, $6, $7, $7) returning id",
            )
            .bind(id)
            .bind(guild)
            .bind(b.enabled)
            .bind(channel)
            .bind(&stored)
            .bind(next)
            .bind(s.user_id)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    let version = if b.enabled {
        vouch(&mut tx, guild, id, level).await?
    } else {
        version
    };
    tx.commit().await?;
    let what: Vec<String> = spec.lines.iter().map(msgsched::describe).collect();
    audit(
        &st,
        &s,
        guild,
        format!(
            "{} the schedule “{}” → <#{channel}> of message #{id}",
            if b.rule.is_some() { "changed" } else { "added" },
            what.join(" · ")
        ),
    )
    .await?;
    Ok(Json(
        json!({"id": rid, "next": next.map(|n| n.timestamp()), "version": version}),
    ))
}

async fn delete(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id, rule)): Path<(i64, i64, i64)>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let gone = sqlx::query(
        "delete from message_rules where id = $1 and embed_id = $2 and guild_id = $3 and kind = 'schedule'",
    )
    .bind(rule)
    .bind(id)
    .bind(guild)
    .execute(&st.db)
    .await?
    .rows_affected();
    if gone == 0 {
        return Err(ApiError::NotFound);
    }
    audit(
        &st,
        &s,
        guild,
        format!("deleted a schedule of message #{id}"),
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

/// Runs due schedules, forever: one claim per rule (the next run is stored before the send), so a
/// second dashboard during an update, or a crash mid-send, never sends one run twice.
pub(crate) async fn runner(st: AppState) {
    // The bot NOTIFYs `message_runs` when it queues a trigger answer, so answers go out at once;
    // schedules and timed deletes are checked at least every 10 s.
    let mut listen = match sqlx::postgres::PgListener::connect_with(&st.db).await {
        Ok(mut l) => l.listen("message_runs").await.ok().map(|_| l),
        Err(e) => {
            tracing::warn!(?e, "message rules: no LISTEN, answers wait up to 10 s");
            None
        }
    };
    loop {
        let mut more = false;
        for r in [
            crate::msgtrig::answer_one(&st).await,
            tick(&st).await,
            crate::msgtrig::delete_due(&st).await,
            crate::msgsticky::repost_one(&st).await,
            crate::msgtags::run_one(&st).await,
        ] {
            match r {
                // More may be due: go on at once.
                Ok(m) => more |= m,
                Err(e) => tracing::warn!(?e, "message rules: a run failed"),
            }
        }
        if more {
            continue;
        }
        let wait = std::time::Duration::from_secs(10);
        match listen.as_mut() {
            Some(l) => {
                if let Ok(Err(e)) = tokio::time::timeout(wait, l.recv()).await {
                    tracing::warn!(?e, "message rules: LISTEN dropped");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
            None => tokio::time::sleep(wait).await,
        }
    }
}

/// A due rule claimed for one run.
struct Due {
    rule: i64,
    guild: i64,
    embed: i64,
    channel: Option<i64>,
    /// Why it runs, for the Sent log: the line that came due.
    why: String,
    /// Seconds late (0 for "every N messages").
    late: i64,
    once: bool,
}

/// Claims and runs one due rule; true when one ran (more may be due).
pub(crate) async fn tick(st: &AppState) -> Result<bool, ApiError> {
    let now = Utc::now();
    let mut tx = st.db.begin().await?;
    #[allow(clippy::type_complexity)]
    let row: Option<(
        i64,
        i64,
        i64,
        Option<i64>,
        Value,
        Option<DateTime<Utc>>,
        bool,
        bool,
        Option<DateTime<Utc>>,
    )> = sqlx::query_as(
        "select id, guild_id, embed_id, channel_id, spec, next_at, due_messages, waiting_once, last_clock
         from message_rules
         where kind = 'schedule' and enabled and (next_at <= now() or due_messages)
         order by next_at nulls first limit 1 for update skip locked",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some((rule, guild, embed, channel, spec, at, by_messages, waiting, last_clock)) = row
    else {
        return Ok(false);
    };
    let parsed = msgsched::parse(&spec, now);
    // A clock run is due at `at`; it counts only when on time (or a "once" run waiting out a pause)
    // and at least 15 minutes after the last clock run.
    let clock_late = at.map_or(0, |a| (now - a).num_seconds());
    let clock_due = at.is_some_and(|a| a <= now);
    let too_soon =
        last_clock.is_some_and(|l| (now - l).num_seconds() < msgsched::MIN_MINUTES * 60 - 30);
    let mut late = clock_late;
    let (next, line) = match &parsed {
        Ok(p) => {
            let line = if clock_due && waiting {
                // The "once" run that waited out panic mode, a lockdown or a nuke case.
                p.lines.iter().find(|l| match l {
                    msgsched::Line::Once(t) => {
                        let t = msgsched::resolve(p.tz, *t);
                        late = (now - t).num_seconds();
                        t <= now && late < ONCE_GRACE
                    }
                    _ => false,
                })
            } else if clock_due {
                let a = at.unwrap_or(now);
                p.lines.iter().find(|l| {
                    msgsched::line_next(p, l, a - chrono::Duration::seconds(1)) == Some(a)
                })
            } else {
                None
            };
            (msgsched::next(p, now), line)
        }
        Err(_) => (None, None),
    };
    let once = matches!(line, Some(msgsched::Line::Once(_)));
    let clock_ok = line.is_some() && !too_soon && (late <= LATE || (once && late <= ONCE_GRACE));
    // What runs: the clock run when it may, else the message count, else nothing (with the reason).
    let (why, skip) = match (&parsed, line) {
        (Err(_), _) => (String::new(), None),
        (Ok(_), Some(l)) if clock_ok => (msgsched::describe(l), None),
        (Ok(p), l) if by_messages => (
            format!("every {} messages", msgsched::messages(p).unwrap_or(0)),
            l.map(|l| skipped(l, too_soon)),
        ),
        (Ok(_), Some(l)) => (String::new(), Some(skipped(l, too_soon))),
        (Ok(_), None) if waiting => (
            String::new(),
            Some("Skipped a run: the pause lasted more than an hour".to_string()),
        ),
        (Ok(_), None) => (String::new(), None),
    };
    let sends = parsed.is_ok() && (clock_ok || by_messages);
    // The claim: the next run is stored before this one is sent.
    sqlx::query(
        "update message_rules set next_at = $2, due_messages = false, waiting_once = false, last_run = now(),
             last_clock = case when $3 then now() else last_clock end
         where id = $1",
    )
    .bind(rule)
    .bind(next)
    .bind(clock_ok)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if let Err(e) = parsed {
        return issue(st, guild, rule, &format!("this schedule cannot run: {e}"))
            .await
            .map(|_| true);
    }
    if let Some(text) = &skip {
        issue(st, guild, rule, text).await?;
    }
    if !sends {
        return Ok(true);
    }
    // The claim is stored, so the run is skipped, not kept for later.
    let off: Option<bool> = sqlx::query_scalar(
        "select coalesce(config->'modules'->>'messages', 'true') = 'false' from guild_config
         where guild_id = $1 order by version desc limit 1",
    )
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    if off == Some(true) {
        return issue(st, guild, rule, "Skipped a run: the Messages module is off")
            .await
            .map(|_| true);
    }
    // The claim is stored, so an unread level skips this run with the reason; nothing is paused.
    match saver_may_run(st, guild, rule, "schedule").await {
        Ok(true) => {}
        Ok(false) => return Ok(true),
        Err(e) => {
            tracing::warn!(?e, rule, "reading the schedule saver's level failed");
            let text = "Skipped a run: Brim could not read whether the member who last saved this schedule is still an Owner, Extra Owner or Trusted Admin. It asks again at the next run; a one-time schedule needs saving again.";
            return issue(st, guild, rule, text).await.map(|_| true);
        }
    }
    let d = Due {
        rule,
        guild,
        embed,
        channel,
        why,
        once: once && clock_ok,
        late,
    };
    if let Err(e) = run(st, &d).await {
        let text = match &e {
            ApiError::BadRequest(t) => t.clone(),
            _ => "the send failed: Brim will try again at the next run".into(),
        };
        tracing::warn!(?e, rule, "message schedule send failed");
        issue(st, guild, rule, &text).await?;
    }
    Ok(true)
}

/// The reason a clock run was not sent.
fn skipped(l: &msgsched::Line, too_soon: bool) -> String {
    if too_soon {
        format!(
            "Skipped a run ({}): less than {} minutes after the last run",
            msgsched::describe(l),
            msgsched::MIN_MINUTES
        )
    } else {
        format!(
            "Skipped a run ({}): Brim was offline at that time",
            msgsched::describe(l)
        )
    }
}

async fn issue(st: &AppState, guild: i64, rule: i64, text: &str) -> Result<(), ApiError> {
    sqlx::query("update message_rules set issue = $2 where id = $1 and guild_id = $3")
        .bind(rule)
        .bind(text)
        .bind(guild)
        .execute(&st.db)
        .await?;
    Ok(())
}

/// One run: skipped (with the reason on the rule) when paused or not allowed; else one copy.
async fn run(st: &AppState, d: &Due) -> Result<(), ApiError> {
    if let Some(ch) = d.channel {
        // The channel may have become a log, ticket, verification or appeal channel since the save.
        check_channel(st, d.guild, ch).await?;
    }
    if let Some(why) = embeds::emergency(st, d.guild).await? {
        // A "once" run waits out the pause for up to an hour; other runs go at their next time.
        if d.once && d.late < ONCE_GRACE {
            sqlx::query(
                "update message_rules set next_at = least(coalesce(next_at, 'infinity'), now() + interval '1 minute'),
                     waiting_once = true where id = $1",
            )
                .bind(d.rule)
                .execute(&st.db)
                .await?;
            return issue(st, d.guild, d.rule, &format!("Waiting: {why}")).await;
        }
        return issue(
            st,
            d.guild,
            d.rule,
            &format!("Skipped a run ({}): {why}", d.why),
        )
        .await;
    }
    // An older image's save (after a rollback) changed the text without the rights check.
    if !trusted(st, d.guild, d.embed).await? {
        return issue(
            st,
            d.guild,
            d.rule,
            "Skipped a run: the message was changed by an older Brim version. An Owner, Extra Owner or Trusted Admin must save it again",
        )
        .await;
    }
    let r = embeds::ready(st, d.guild, d.embed, d.channel, false).await?;
    let by = Sender {
        user: 0,
        who: format!("Sent by the schedule “{}”", d.why),
        rule: Some(d.rule),
        answer: None,
        sticky: false,
    };
    match embeds::send_copy(st, d.guild, d.embed, r, by).await? {
        None => {
            issue(
                st,
                d.guild,
                d.rule,
                &format!(
                    "Skipped a run ({}): more than {} scheduled sends in one minute in this server",
                    d.why,
                    embeds::SCHEDULE_CAP
                ),
            )
            .await
        }
        Some((_, warning)) => {
            sqlx::query("update message_rules set issue = $2 where id = $1 and guild_id = $3")
                .bind(d.rule)
                .bind(warning)
                .bind(d.guild)
                .execute(&st.db)
                .await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner claims a due rule once, moves it to its next run before it sends, and skips a
    /// run that is too late.
    #[tokio::test]
    async fn a_due_rule_is_claimed_once_and_a_late_run_is_skipped() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_500_000).await else {
            return;
        };
        let e: i64 = sqlx::query_scalar(
            "insert into embeds (guild_id, name, message, updated_by) values ($1, 'Daily', '{\"content\": \"hi\"}', 7) returning id",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        let spec = json!({"tz": "UTC", "start": 0, "lines": [{"kind": "every", "minutes": 60}]});
        let rid: i64 = sqlx::query_scalar(
            "insert into message_rules (embed_id, guild_id, kind, channel_id, spec, next_at, created_by, updated_by)
             values ($1, $2, 'schedule', 55, $3, date_trunc('hour', now()) - interval '1 hour', 7, 7) returning id",
        )
        .bind(e)
        .bind(g)
        .bind(&spec)
        .fetch_one(&st.db)
        .await
        .unwrap();
        // Other tests' rules may be due too: run until this one has run.
        for _ in 0..50 {
            let next: Option<DateTime<Utc>> =
                sqlx::query_scalar("select next_at from message_rules where id = $1")
                    .bind(rid)
                    .fetch_one(&st.db)
                    .await
                    .unwrap();
            if next.is_some_and(|n| n > Utc::now()) {
                break;
            }
            tick(&st).await.unwrap();
        }
        let (next, issue): (Option<DateTime<Utc>>, Option<String>) =
            sqlx::query_as("select next_at, issue from message_rules where id = $1")
                .bind(rid)
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert!(next.unwrap() > Utc::now());
        assert!(issue.unwrap_or_default().contains("offline"));
        let sent: i64 = sqlx::query_scalar("select count(*) from message_sends where rule_id = $1")
            .bind(rid)
            .fetch_one(&st.db)
            .await
            .unwrap();
        assert_eq!(sent, 0);
        sqlx::query("delete from embeds where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
    }

    /// security review: a rule whose last saver lost the level is paused once, with the reason; a
    /// save by an Owner turns it on again.
    #[tokio::test]
    async fn a_rule_of_a_removed_saver_is_paused_until_saved_again() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_510_000).await else {
            return;
        };
        st.discord.memo.lock().unwrap().insert(
            format!("/guilds/{g}/members/9"),
            (std::time::Instant::now(), Some(json!({"roles": []}))),
        );
        let e: i64 = sqlx::query_scalar(
            "insert into embeds (guild_id, name, message, updated_by) values ($1, 'Gone', '{}', 9) returning id",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        let rid: i64 = sqlx::query_scalar(
            "insert into message_rules (embed_id, guild_id, kind, spec, created_by, updated_by)
             values ($1, $2, 'trigger', '{}', 9, 9) returning id",
        )
        .bind(e)
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert!(!saver_may_run(&st, g, rid, "trigger").await.unwrap());
        let (on, issue): (bool, Option<String>) =
            sqlx::query_as("select enabled, issue from message_rules where id = $1")
                .bind(rid)
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert!(!on);
        assert!(issue
            .unwrap()
            .starts_with("Paused: the member who last saved this trigger (user id 9)"));
        // Confirmed by the owner (user 7): it runs.
        sqlx::query(
            "update message_rules set enabled = true, issue = null, updated_by = 7 where id = $1",
        )
        .bind(rid)
        .execute(&st.db)
        .await
        .unwrap();
        assert!(saver_may_run(&st, g, rid, "trigger").await.unwrap());
        sqlx::query("delete from embeds where id = $1")
            .bind(e)
            .execute(&st.db)
            .await
            .unwrap();
    }
}

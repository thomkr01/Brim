//! Message Builder phase 6: trigger rules ("Automate" tab, "When members write"). Owner, Extra
//! Owners and Trusted Admins make them. The bot finds a trigger in a member's message and queues an
//! answer (`message_runs`, with a NOTIFY); the dashboard sends it, since it holds the send code
//! (variables, webhooks, the Sent log). Answers that delete themselves are removed here too.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    embeds::{self, Answer, Sender},
    lockdown::audit,
    msgrules::{self, MAX_RULES},
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use brim_core::msgtrig;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

/// Patterns and words in all of a server's trigger rules (the bot matches every one per message).
const MAX_SERVER_PATTERNS: i64 = 30;
const MAX_SERVER_WORDS: i64 = 300;

/// An answer queued longer ago than this is dropped: a late answer reads as spam.
const STALE_SECS: i64 = 120;

pub fn routes() -> Router<AppState> {
    let r = "/api/guilds/{guild}/embeds/{id}/triggers";
    Router::new()
        .route(r, get(list).post(save))
        .route(&format!("{r}/try"), post(try_text))
        .route(&format!("{r}/{{rule}}/delete"), post(delete))
}

async fn list(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, bool, Value, Option<String>, Option<DateTime<Utc>>, i64)> = sqlx::query_as(
        "select id, enabled, spec, issue, last_run, created_by from message_rules
         where embed_id = $1 and guild_id = $2 and kind = 'trigger' order by id",
    )
    .bind(id)
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(rid, on, spec, issue, last, by)| json!({
            "id": rid, "enabled": on, "spec": spec, "issue": issue,
            "last_run": last.map(|l| l.timestamp()), "created_by": by.to_string(),
        }))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct Try {
    spec: Value,
    text: String,
}

/// "Try a message": which word or pattern of a rule being edited a message would trigger.
async fn try_text(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, _)): Path<(i64, i64)>,
    Json(b): Json<Try>,
) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let mut spec = b.spec;
    let Some(o) = spec.as_object_mut() else {
        return Err(ApiError::BadRequest("the rule must be an object".into()));
    };
    // A rule being edited may have no channel yet; it still shows what matches.
    if o.get("channels")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        o.insert("channels".into(), json!(["0"]));
    }
    let p = msgtrig::parse(&spec).map_err(ApiError::BadRequest)?;
    Ok(Json(
        json!({"matched": p.find(&b.text).map(|t| t.text.clone())}),
    ))
}

#[derive(Deserialize)]
struct Save {
    #[serde(default)]
    rule: Option<i64>,
    spec: Value,
    enabled: bool,
    #[serde(default)]
    seen: Option<String>,
}

/// Adds or changes a trigger rule (Owner, Extra Owners, Trusted Admins).
async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Save>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Admin, false).await?;
    let spec = msgtrig::parse(&b.spec).map_err(ApiError::BadRequest)?;
    for c in &spec.channels {
        msgrules::check_channel(&st, guild, *c as i64).await?;
    }
    if !spec.roles.is_empty() {
        let roles = embeds::discord(&st, Method::GET, &format!("/guilds/{guild}/roles"), None)
            .await?
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let here = |r: &u64| {
            roles
                .iter()
                .any(|x| x["id"].as_str() == Some(&r.to_string()))
        };
        if let Some(bad) = spec.roles.iter().find(|r| !here(r)) {
            return Err(ApiError::BadRequest(format!(
                "the role with id {bad} is not in this server"
            )));
        }
    }
    let stored = spec.to_json();
    let mut tx = st.db.begin().await?;
    // A whole server's patterns and words, so one server cannot load the bot with regex work.
    sqlx::query("select pg_advisory_xact_lock(hashtext('message_rules'), $1::int)")
        .bind((guild % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    let (patterns, words): (i64, i64) = sqlx::query_as(
        "select coalesce(sum(jsonb_array_length(spec->'patterns')), 0)::bigint,
                coalesce(sum(jsonb_array_length(spec->'words')), 0)::bigint
         from message_rules where guild_id = $1 and kind = 'trigger' and id <> coalesce($2, 0)",
    )
    .bind(guild)
    .bind(b.rule)
    .fetch_one(&mut *tx)
    .await?;
    let n = |k: &str| stored[k].as_array().map_or(0, Vec::len) as i64;
    if patterns + n("patterns") > MAX_SERVER_PATTERNS || words + n("words") > MAX_SERVER_WORDS {
        return Err(ApiError::BadRequest(format!(
            "a server has at most {MAX_SERVER_PATTERNS} patterns and {MAX_SERVER_WORDS} words in all its triggers"
        )));
    }
    let version =
        msgrules::lock_message(&mut tx, guild, id, b.enabled, b.seen.as_deref(), true).await?;
    let rid: i64 = match b.rule {
        Some(rid) => sqlx::query_scalar(
            "update message_rules set spec = $3, enabled = $4, issue = null, updated_by = $5, updated_at = now()
             where id = $1 and embed_id = $2 and guild_id = $6 and kind = 'trigger' returning id",
        )
        .bind(rid)
        .bind(id)
        .bind(&stored)
        .bind(b.enabled)
        .bind(s.user_id)
        .bind(guild)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?,
        None => {
            let n: i64 = sqlx::query_scalar(
                "select count(*) from message_rules where embed_id = $1 and kind = 'trigger' and guild_id = $2",
            )
            .bind(id)
            .bind(guild)
            .fetch_one(&mut *tx)
            .await?;
            if n >= MAX_RULES {
                return Err(ApiError::BadRequest(format!(
                    "a message has at most {MAX_RULES} triggers"
                )));
            }
            msgrules::server_room(&mut tx, guild).await?;
            sqlx::query_scalar(
                "insert into message_rules (embed_id, guild_id, kind, enabled, spec, created_by, updated_by)
                 values ($1, $2, 'trigger', $3, $4, $5, $5) returning id",
            )
            .bind(id)
            .bind(guild)
            .bind(b.enabled)
            .bind(&stored)
            .bind(s.user_id)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    let version = if b.enabled {
        msgrules::vouch(&mut tx, guild, id, level).await?
    } else {
        version
    };
    tx.commit().await?;
    let words: Vec<&str> = spec.triggers.iter().map(|t| t.text.as_str()).collect();
    let chans: Vec<String> = spec.channels.iter().map(|c| format!("<#{c}>")).collect();
    audit(
        &st,
        &s,
        guild,
        format!(
            "{} the trigger “{}” in {} of message #{id} (answer {})",
            if b.rule.is_some() { "changed" } else { "added" },
            words.join("”, “"),
            chans.join(", "),
            if spec.dm { "by DM" } else { "in the channel" }
        ),
    )
    .await?;
    Ok(Json(json!({"id": rid, "version": version})))
}

async fn delete(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id, rule)): Path<(i64, i64, i64)>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let gone = sqlx::query(
        "delete from message_rules where id = $1 and embed_id = $2 and guild_id = $3 and kind = 'trigger'",
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
        format!("deleted a trigger of message #{id}"),
    )
    .await?;
    Ok(Json(json!({"ok": true})))
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

/// One queued answer, claimed (deleted from the queue) before it is sent.
type Run = (i64, i64, i64, i64, i64, String, String, DateTime<Utc>);

/// Claims and sends one queued trigger answer; true when one was taken (more may wait).
pub(crate) async fn answer_one(st: &AppState) -> Result<bool, ApiError> {
    let run: Option<Run> = sqlx::query_as(
        "delete from message_runs where id = (select id from message_runs order by id limit 1 for update skip locked)
         returning rule_id, guild_id, channel_id, member_id, message_id, matched, kind, created_at",
    )
    .fetch_optional(&st.db)
    .await?;
    let Some((rule, guild, channel, member, message, matched, kind, at)) = run else {
        return Ok(false);
    };
    if (Utc::now() - at).num_seconds() > STALE_SECS {
        return Ok(true);
    }
    if let Err(e) = answer(st, rule, guild, channel, member, message, &matched, &kind).await {
        let text = match &e {
            ApiError::BadRequest(t) => format!("An answer failed: {t}"),
            _ => "An answer failed: Brim will answer the next time".into(),
        };
        tracing::warn!(?e, rule, "message trigger answer failed");
        issue(st, guild, rule, &text).await?;
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn answer(
    st: &AppState,
    rule: i64,
    guild: i64,
    channel: i64,
    member: i64,
    message: i64,
    matched: &str,
    kind: &str,
) -> Result<(), ApiError> {
    let row: Option<(i64, Value, bool)> = sqlx::query_as(
        "select embed_id, spec, enabled from message_rules where id = $1 and guild_id = $2 and kind = 'trigger'",
    )
    .bind(rule)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let Some((embed, spec, true)) = row else {
        return Ok(());
    };
    if !msgrules::saver_may_run(st, guild, rule, "trigger").await? {
        return Ok(());
    }
    let spec = msgtrig::parse(&spec).map_err(ApiError::BadRequest)?;
    if !spec.channels.contains(&(channel as u64)) {
        return Ok(());
    }
    // Paused answers are not sent later (plan 4).
    if let Some(why) = embeds::emergency(st, guild).await? {
        return issue(st, guild, rule, &format!("Skipped an answer: {why}")).await;
    }
    // Someone automod removed, timed out or quarantined gets no answer.
    let held: bool = sqlx::query_scalar(
        "select exists(select 1 from quarantines where guild_id = $1 and user_id = $2)",
    )
    .bind(guild)
    .bind(member)
    .fetch_one(&st.db)
    .await?;
    if held {
        return Ok(());
    }
    let Some(m) = embeds::discord(
        st,
        Method::GET,
        &format!("/guilds/{guild}/members/{member}"),
        None,
    )
    .await?
    else {
        return Ok(());
    };
    let timed_out = m["communication_disabled_until"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| t > Utc::now());
    let roles: Vec<u64> = m["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str()?.parse().ok())
        .collect();
    if timed_out || !spec.member_ok(&roles) {
        return Ok(());
    }
    if embeds::discord(
        st,
        Method::GET,
        &format!("/channels/{channel}/messages/{message}"),
        None,
    )
    .await?
    .is_none()
    {
        return Ok(());
    }
    // The rule's channel may have become a log, ticket, verification or appeal channel.
    msgrules::check_channel(st, guild, channel).await?;
    if kind == "cooldown" {
        return cooldown_note(st, guild, channel, member, matched, spec.cooldown).await;
    }
    // An older image's save (after a rollback) changed the text without the rights check.
    if !msgrules::trusted(st, guild, embed).await? {
        return issue(
            st,
            guild,
            rule,
            "Skipped an answer: the message was changed by an older Brim version. An Owner, Extra Owner or Trusted Admin must save it again",
        )
        .await;
    }
    let r = embeds::ready(st, guild, embed, Some(channel), true).await?;
    let who = m["user"]["username"]
        .as_str()
        .unwrap_or("a member")
        .to_string();
    let by = Sender {
        user: 0,
        who: format!("Answer to @{who} for “{matched}”"),
        rule: Some(rule),
        answer: Some(Answer {
            member: m,
            matched: matched.to_string(),
            channel,
            message,
            reply: spec.reply,
            dm: spec.dm,
            delete_after: spec.delete_after,
        }),
        sticky: false,
    };
    match embeds::send_copy(st, guild, embed, r, by).await? {
        None => {
            issue(
                st,
                guild,
                rule,
                &format!(
                    "Skipped an answer: more than {} trigger answers in one minute, or {} DMs in one hour, in this server",
                    embeds::TRIGGER_CAP,
                    embeds::DM_CAP
                ),
            )
            .await
        }
        Some((_, warning)) => {
            sqlx::query(
                "update message_rules set issue = $2, last_run = now() where id = $1 and guild_id = $3",
            )
            .bind(rule)
            .bind(warning)
            .bind(guild)
                .execute(&st.db)
                .await?;
            Ok(())
        }
    }
}

/// "DM on cooldown": tells the member, by DM, why there was no answer this time.
async fn cooldown_note(
    st: &AppState,
    guild: i64,
    channel: i64,
    member: i64,
    matched: &str,
    cooldown: i64,
) -> Result<(), ApiError> {
    let mut tx = st.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('message_sends'), $1::int)")
        .bind((guild % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    if !embeds::dm_claim(&mut tx, guild).await? {
        return Ok(());
    }
    tx.commit().await?;
    let Some(dm) = embeds::discord(
        st,
        Method::POST,
        "/users/@me/channels",
        Some(json!({"recipient_id": member.to_string()})),
    )
    .await?
    .and_then(|c| c["id"].as_str().map(String::from)) else {
        return Ok(());
    };
    let text = format!(
        "Brim answers “{}” in <#{channel}> at most once every {}, so there was no answer this time.",
        brim_core::msgvars::escape(matched),
        if cooldown % 60 == 0 { format!("{} minutes", cooldown / 60) } else { format!("{cooldown} seconds") }
    );
    // A member who does not take DMs from this server is fine: nothing else to do.
    let _ = embeds::discord(
        st,
        Method::POST,
        &format!("/channels/{dm}/messages"),
        Some(json!({"content": text, "allowed_mentions": {"parse": []}})),
    )
    .await;
    Ok(())
}

/// Deletes answers whose "delete after" time has come (up to 20 a call); true when any were due.
pub(crate) async fn delete_due(st: &AppState) -> Result<bool, ApiError> {
    #[allow(clippy::type_complexity)]
    let due: Vec<(i64, i64, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "update message_sends set delete_at = null where id in
             (select id from message_sends where delete_at <= now() order by delete_at limit 20 for update skip locked)
         returning id, guild_id, channel_id, posted_id, webhook_id",
    )
    .fetch_all(&st.db)
    .await?;
    for (send, guild, channel, posted, hook) in &due {
        let Some(posted) = posted else { continue };
        let r = match hook {
            Some(h) => crate::msghooks::delete(st, *guild, *h, *channel, *posted).await,
            None => embeds::discord(
                st,
                Method::DELETE,
                &format!("/channels/{channel}/messages/{posted}"),
                None,
            )
            .await
            .map(|_| ()),
        };
        match r {
            Ok(()) => {
                sqlx::query("update message_sends set posted_id = null where id = $1")
                    .bind(send)
                    .execute(&st.db)
                    .await?;
            }
            // Claimed once, so it is not tried again: the Sent log says so on the copy.
            Err(e) => {
                tracing::warn!(?e, send, "deleting a trigger answer failed");
                sqlx::query("update message_sends set warning = $2 where id = $1")
                    .bind(send)
                    .bind("Brim could not delete this copy at its delete time: delete it by hand")
                    .execute(&st.db)
                    .await?;
            }
        }
    }
    // The DM budget only looks back one hour.
    sqlx::query("delete from message_dms where sent_at < now() - interval '2 hours'")
        .execute(&st.db)
        .await?;
    Ok(!due.is_empty())
}

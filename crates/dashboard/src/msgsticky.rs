//! Message Builder phase 7: sticky messages ("Sticky" tab). A sticky is always the last message in
//! its channel: the bot waits until the channel has been quiet for 3 seconds and queues a repost
//! (`sticky_runs`, with a NOTIFY on `message_runs`); the dashboard posts a new copy and then
//! deletes its old one, by the id it stored. Owner, Extra Owners and Trusted Admins make them.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    embeds::{self, Sender},
    lockdown::audit,
    msgrules::{self, MAX_RULES},
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;

/// Stickies per channel (plan 2.6) and per server.
const PER_CHANNEL: i64 = 3;
const PER_SERVER: i64 = 50;
/// Reposts in a row caused by bot messages only, before the sticky pauses (plan 4d.7).
const BOT_STREAK: i32 = 10;
/// A repost queued longer ago than this is dropped (Brim was down); the next message queues one.
const STALE_SECS: i64 = 120;
const PAUSED: &str = "Paused: another bot keeps posting here. Switch it on again to go on";

pub fn routes() -> Router<AppState> {
    let r = "/api/guilds/{guild}/embeds/{id}/stickies";
    Router::new()
        .route(r, get(list).post(save))
        .route(&format!("{r}/{{sticky}}/delete"), post(delete))
}

async fn list(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, i16, bool, bool, Option<String>, bool, Option<i64>)> = sqlx::query_as(
        "select k.id, k.channel_id::text, k.position, k.bots, k.enabled, k.issue, k.posted_id is not null,
                extract(epoch from m.hook_gone_at)::bigint
         from sticky_msgs k left join message_sends m on m.id = k.send_id and m.posted_id is not null
         where k.embed_id = $1 and k.guild_id = $2 order by k.channel_id, k.position",
    )
    .bind(id)
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        // The copy that is up now lost its webhook in Discord (as the Sent log shows for other
        // copies). The next repost uses a new webhook, so this clears then.
        .map(|(sid, ch, pos, bots, on, issue, posted, gone)| json!({
            "id": sid, "channel": ch, "position": pos, "bots": bots, "enabled": on,
            "issue": issue, "posted": posted, "hook_gone": gone.map(|g| json!({"at": g})),
        }))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct Save {
    #[serde(default)]
    sticky: Option<i64>,
    channel: String,
    position: i16,
    #[serde(default)]
    bots: bool,
    enabled: bool,
    #[serde(default)]
    seen: Option<String>,
}

/// Queues a repost of a channel's stickies, as the bot does after 3 quiet seconds.
async fn queue(st: &AppState, guild: i64, channel: i64) -> Result<(), ApiError> {
    sqlx::query(
        "with q as (insert into sticky_runs (channel_id, guild_id, bots_only) values ($1, $2, false)
                    on conflict (channel_id) do update set bots_only = false returning 1)
         select pg_notify('message_runs', '') from q",
    )
    .bind(channel)
    .bind(guild)
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Deletes a copy Brim posted for a sticky (by the id it stored, never another message) and
/// marks its Sent log row as gone; a copy already gone counts as done.
async fn remove_copy(
    st: &AppState,
    guild: i64,
    channel: i64,
    posted: i64,
    send: Option<i64>,
) -> bool {
    let hook: Option<i64> = match send {
        Some(s) => sqlx::query_scalar(
            "select webhook_id from message_sends where id = $1 and guild_id = $2",
        )
        .bind(s)
        .bind(guild)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .flatten(),
        None => None,
    };
    let r = match hook {
        Some(h) => crate::msghooks::delete(st, guild, h, channel, posted).await,
        None => embeds::discord(
            st,
            Method::DELETE,
            &format!("/channels/{channel}/messages/{posted}"),
            None,
        )
        .await
        .map(|_| ()),
    };
    if let Err(e) = r {
        tracing::warn!(?e, channel, posted, "deleting an old sticky copy failed");
        return false;
    }
    let q = match send {
        Some(id) => {
            sqlx::query("update message_sends set posted_id = null where id = $1 and guild_id = $2")
                .bind(id)
                .bind(guild)
        }
        None => sqlx::query(
            "update message_sends set posted_id = null where posted_id = $1 and guild_id = $2 and source = 'sticky'",
        )
        .bind(posted)
        .bind(guild),
    };
    if let Err(e) = q.execute(&st.db).await {
        tracing::warn!(?e, "marking an old sticky copy gone failed");
    }
    true
}

/// Adds or changes a sticky (Owner, Extra Owners, Trusted Admins).
async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Save>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Admin, false).await?;
    let channel: i64 = b
        .channel
        .parse()
        .map_err(|_| ApiError::BadRequest("choose a channel".into()))?;
    if !(1..=PER_CHANNEL as i16).contains(&b.position) {
        return Err(ApiError::BadRequest(format!(
            "the order is 1 to {PER_CHANNEL}"
        )));
    }
    msgrules::check_channel(&st, guild, channel).await?;
    let mut tx = st.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('sticky_msgs'), $1::int)")
        .bind((guild % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    let version =
        msgrules::lock_message(&mut tx, guild, id, b.enabled, b.seen.as_deref(), false).await?;
    let (others, same, taken, mine, server): (i64, bool, bool, i64, i64) = sqlx::query_as(
        "select count(*) filter (where channel_id = $3),
                coalesce(bool_or(channel_id = $3 and embed_id = $2), false),
                coalesce(bool_or(channel_id = $3 and position = $4), false),
                count(*) filter (where embed_id = $2),
                count(*)
         from sticky_msgs where guild_id = $1 and id <> coalesce($5, 0)",
    )
    .bind(guild)
    .bind(id)
    .bind(channel)
    .bind(b.position)
    .bind(b.sticky)
    .fetch_one(&mut *tx)
    .await?;
    if same {
        return Err(ApiError::BadRequest(
            "this message is already sticky in that channel".into(),
        ));
    }
    if others >= PER_CHANNEL {
        return Err(ApiError::BadRequest(format!(
            "a channel has at most {PER_CHANNEL} sticky messages"
        )));
    }
    if taken {
        return Err(ApiError::BadRequest(format!(
            "another sticky message in that channel has order {}: pick another number",
            b.position
        )));
    }
    if mine >= MAX_RULES {
        return Err(ApiError::BadRequest(format!(
            "a message is sticky in at most {MAX_RULES} channels"
        )));
    }
    if server >= PER_SERVER {
        return Err(ApiError::BadRequest(format!(
            "a server has at most {PER_SERVER} sticky messages: delete one first"
        )));
    }
    // The copy to take down: the old one when the sticky moves to another channel or is switched off.
    let old: Option<(i64, Option<i64>, Option<i64>)> = match b.sticky {
        Some(sid) => {
            let row: Option<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
                "select channel_id, posted_id, send_id from sticky_msgs
                 where id = $1 and embed_id = $2 and guild_id = $3 for update",
            )
            .bind(sid)
            .bind(id)
            .bind(guild)
            .fetch_optional(&mut *tx)
            .await?;
            let row = row.ok_or(ApiError::NotFound)?;
            let moved = row.0 != channel || !b.enabled;
            sqlx::query(
                "update sticky_msgs set channel_id = $2, position = $3, bots = $4, enabled = $5, issue = null,
                     bot_streak = 0, updated_by = $6, updated_at = now(),
                     posted_id = case when $7 then null else posted_id end,
                     send_id = case when $7 then null else send_id end
                 where id = $1",
            )
            .bind(sid)
            .bind(channel)
            .bind(b.position)
            .bind(b.bots)
            .bind(b.enabled)
            .bind(s.user_id)
            .bind(moved)
            .execute(&mut *tx)
            .await?;
            moved.then_some(row)
        }
        None => {
            sqlx::query(
                "insert into sticky_msgs (embed_id, guild_id, channel_id, position, bots, enabled, created_by, updated_by)
                 values ($1, $2, $3, $4, $5, $6, $7, $7)",
            )
            .bind(id)
            .bind(guild)
            .bind(channel)
            .bind(b.position)
            .bind(b.bots)
            .bind(b.enabled)
            .bind(s.user_id)
            .execute(&mut *tx)
            .await?;
            None
        }
    };
    let version = if b.enabled {
        msgrules::vouch(&mut tx, guild, id, level).await?
    } else {
        version
    };
    tx.commit().await?;
    if let Some((ch, Some(posted), send)) = old {
        if !remove_copy(&st, guild, ch, posted, send).await {
            if let Some(sid) = b.sticky {
                issue(
                    &st,
                    guild,
                    &[sid],
                    &format!("Brim could not delete its old copy in <#{ch}>: delete it by hand"),
                )
                .await?;
            }
        }
    }
    if b.enabled {
        // Posted at once, and every sticky of the channel moves down in its order.
        queue(&st, guild, channel).await?;
    }
    audit(
        &st,
        &s,
        guild,
        format!(
            "{} message #{id} as sticky in <#{channel}> (order {}, {}{})",
            if b.sticky.is_some() {
                "changed"
            } else {
                "made"
            },
            b.position,
            if b.bots {
                "also after other bots"
            } else {
                "after members"
            },
            if b.enabled { "" } else { ", off" }
        ),
    )
    .await?;
    Ok(Json(json!({"version": version})))
}

async fn delete(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id, sticky)): Path<(i64, i64, i64)>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let gone: Option<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "delete from sticky_msgs where id = $1 and embed_id = $2 and guild_id = $3
         returning channel_id, posted_id, send_id",
    )
    .bind(sticky)
    .bind(id)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let (channel, posted, send) = gone.ok_or(ApiError::NotFound)?;
    // The sticky row is gone: a copy that could not be deleted is named in the answer.
    let left = match posted {
        Some(p) => !remove_copy(&st, guild, channel, p, send).await,
        None => false,
    };
    audit(
        &st,
        &s,
        guild,
        format!("removed message #{id} as sticky in <#{channel}>"),
    )
    .await?;
    if left {
        return Err(ApiError::BadRequest(
            "the sticky is removed, but Brim could not delete its last copy: delete it by hand"
                .into(),
        ));
    }
    Ok(Json(json!({"ok": true})))
}

async fn issue(st: &AppState, guild: i64, ids: &[i64], text: &str) -> Result<(), ApiError> {
    sqlx::query(
        "update sticky_msgs set issue = $2 where id = any($1) and issue is distinct from $2 and guild_id = $3",
    )
    .bind(ids)
    .bind(text)
    .bind(guild)
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Claims and runs one queued repost; true when one was taken (more may wait).
pub(crate) async fn repost_one(st: &AppState) -> Result<bool, ApiError> {
    let run: Option<(i64, i64, bool, DateTime<Utc>)> = sqlx::query_as(
        "delete from sticky_runs where channel_id =
             (select channel_id from sticky_runs order by created_at limit 1 for update skip locked)
         returning channel_id, guild_id, bots_only, created_at",
    )
    .fetch_optional(&st.db)
    .await?;
    let Some((channel, guild, bots_only, at)) = run else {
        return Ok(false);
    };
    if (Utc::now() - at).num_seconds() <= STALE_SECS {
        // One repost per channel at a time, also across two dashboards during an update: the lock
        // is held on its own connection and goes with it if the process dies.
        let mut conn = st.db.acquire().await?;
        let key = (channel % i64::from(i32::MAX)) as i32;
        let got: bool =
            sqlx::query_scalar("select pg_try_advisory_lock(hashtext('sticky_runs'), $1)")
                .bind(key)
                .fetch_one(&mut *conn)
                .await?;
        if !got {
            // The other run posts the newest copies; this one goes again after it.
            sqlx::query(
                "insert into sticky_runs (channel_id, guild_id, bots_only, created_at) values ($1, $2, $3, $4)
                 on conflict (channel_id) do update set bots_only = sticky_runs.bots_only and excluded.bots_only",
            )
            .bind(channel)
            .bind(guild)
            .bind(bots_only)
            .bind(at)
            .execute(&st.db)
            .await?;
            return Ok(false);
        }
        if let Err(e) = repost(st, guild, channel, bots_only).await {
            tracing::warn!(?e, channel, "sticky repost failed");
        }
        sqlx::query("select pg_advisory_unlock(hashtext('sticky_runs'), $1)")
            .bind(key)
            .execute(&mut *conn)
            .await?;
    }
    // Rows of copies already replaced only count toward the per-minute cap.
    sqlx::query(
        "delete from message_sends where guild_id = $1 and source = 'sticky' and posted_id is null
         and sent_at < now() - interval '2 minutes'",
    )
    .bind(guild)
    .execute(&st.db)
    .await?;
    Ok(true)
}

/// Posts a channel's stickies again in their order (lowest first, so the highest ends at the very
/// bottom); each new copy is posted before its old one is deleted.
async fn repost(st: &AppState, guild: i64, channel: i64, bots_only: bool) -> Result<(), ApiError> {
    let rows: Vec<(i64, i64, bool)> = sqlx::query_as(
        "select id, embed_id, bots from sticky_msgs where channel_id = $1 and guild_id = $2 and enabled
         order by position",
    )
    .bind(channel)
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    // Only bot messages came in: the first sticky that also moves after other bots goes down, and
    // every one after it in the order, so the order holds.
    let first = if bots_only {
        rows.iter().position(|r| r.2).unwrap_or(rows.len())
    } else {
        0
    };
    let rows: Vec<(i64, i64)> = rows[first..].iter().map(|r| (r.0, r.1)).collect();
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    // Paused reposts are not made later (plan 4): the next message after the pause queues one.
    if let Some(why) = embeds::emergency(st, guild).await? {
        return issue(st, guild, &ids, &format!("Skipped a repost: {why}")).await;
    }
    let mut ok = Vec::with_capacity(rows.len());
    for r in rows {
        if msgrules::saver_may_run(st, guild, r.0, "sticky message").await? {
            ok.push(r);
        }
    }
    let mut rows = ok;
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    if bots_only {
        let paused: Vec<Option<i64>> = sqlx::query_scalar(
            "update sticky_msgs set bot_streak = bot_streak + 1,
                 enabled = bot_streak + 1 < $2, issue = case when bot_streak + 1 >= $2 then $3 else issue end
             where id = any($1) returning case when not enabled then id end",
        )
        .bind(&ids)
        .bind(BOT_STREAK)
        .bind(PAUSED)
        .fetch_all(&st.db)
        .await?;
        let paused: Vec<i64> = paused.into_iter().flatten().collect();
        rows.retain(|r| !paused.contains(&r.0));
        if rows.is_empty() {
            return Ok(());
        }
    } else {
        sqlx::query(
            "update sticky_msgs set bot_streak = 0 where channel_id = $1 and guild_id = $2 and bot_streak > 0",
        )
        .bind(channel)
        .bind(guild)
        .execute(&st.db)
        .await?;
    }
    // The channel may have become a log, ticket, verification or appeal channel since the save.
    if let Err(e) = msgrules::check_channel(st, guild, channel).await {
        let text = match e {
            ApiError::BadRequest(t) => format!("Skipped a repost: {t}"),
            e => return Err(e),
        };
        return issue(st, guild, &ids, &text).await;
    }
    for (sid, embed) in rows {
        if let Err(e) = repost_sticky(st, guild, channel, sid, embed).await {
            let text = match &e {
                ApiError::BadRequest(t) => format!("A repost failed: {t}"),
                _ => "A repost failed: Brim tries again after the next message".into(),
            };
            tracing::warn!(?e, sid, "sticky repost failed");
            issue(st, guild, &[sid], &text).await?;
            if text.contains("per minute") {
                break;
            }
        }
    }
    Ok(())
}

async fn repost_sticky(
    st: &AppState,
    guild: i64,
    channel: i64,
    sid: i64,
    embed: i64,
) -> Result<(), ApiError> {
    // An older image's save (after a rollback) changed the text without the rights check.
    if !msgrules::trusted(st, guild, embed).await? {
        return Err(ApiError::BadRequest(
            "the message was changed by an older Brim version. An Owner, Extra Owner or Trusted Admin must save it again".into(),
        ));
    }
    let r = embeds::ready(st, guild, embed, Some(channel), false).await?;
    let by = Sender {
        user: 0,
        who: "Sticky".into(),
        rule: None,
        answer: None,
        sticky: true,
    };
    let Some((posted, warning)) = embeds::send_copy(st, guild, embed, r, by).await? else {
        return Err(ApiError::BadRequest(format!(
            "more than {} sticky reposts per minute in this server",
            embeds::STICKY_CAP
        )));
    };
    let send: Option<i64> = sqlx::query_scalar(
        "select id from message_sends where posted_id = $1 and embed_id = $2 and channel_id = $3
         and guild_id = $4 and source = 'sticky' order by id desc limit 1",
    )
    .bind(posted)
    .bind(embed)
    .bind(channel)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    // The row lock orders two reposts of one sticky: each takes down the copy it replaced.
    let old: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "update sticky_msgs s set posted_id = $2, send_id = $3, issue = $4
         from (select id, posted_id, send_id from sticky_msgs where id = $1 and guild_id = $6 for update) o
         where s.id = o.id and s.enabled and s.channel_id = $5
         returning o.posted_id, o.send_id",
    )
    .bind(sid)
    .bind(posted)
    .bind(send)
    .bind(&warning)
    .bind(channel)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    match old {
        // Switched off, moved or deleted meanwhile: the new copy goes too.
        None => {
            remove_copy(st, guild, channel, posted, send).await;
        }
        Some((Some(p), s)) if p != posted => {
            if !remove_copy(st, guild, channel, p, s).await {
                issue(
                    st,
                    guild,
                    &[sid],
                    "Brim could not delete an older copy: delete it by hand",
                )
                .await?;
            }
        }
        Some(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Ten reposts in a row caused by bots only pause the sticky, with the reason.
    #[tokio::test]
    async fn a_bot_streak_pauses_the_sticky() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_600_000).await else {
            return;
        };
        let embed: i64 = sqlx::query_scalar(
            "insert into embeds (guild_id, name, message, updated_by)
             values ($1, 'n', '{\"content\": \"hi\"}', 7) returning id",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        let ch = g + 7;
        sqlx::query(
            "insert into sticky_msgs (embed_id, guild_id, channel_id, position, bots, bot_streak, created_by, updated_by)
             values ($1, $2, $3, 1, true, 9, 7, 7)",
        )
        .bind(embed)
        .bind(g)
        .bind(ch)
        .execute(&st.db)
        .await
        .unwrap();
        super::repost(&st, g, ch, true).await.unwrap();
        let (on, issue): (bool, Option<String>) =
            sqlx::query_as("select enabled, issue from sticky_msgs where channel_id = $1")
                .bind(ch)
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert!(!on);
        assert!(issue.unwrap().starts_with("Paused"));
    }

    /// security review for stickies: a sticky whose last saver lost the level is not reposted.
    #[tokio::test]
    async fn a_sticky_of_a_removed_saver_is_paused() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_610_000).await else {
            return;
        };
        st.discord.memo.lock().unwrap().insert(
            format!("/guilds/{g}/members/9"),
            (
                std::time::Instant::now(),
                Some(serde_json::json!({"roles": []})),
            ),
        );
        let embed: i64 = sqlx::query_scalar(
            "insert into embeds (guild_id, name, message, updated_by)
             values ($1, 'n', '{\"content\": \"hi\"}', 9) returning id",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        let ch = g + 8;
        sqlx::query(
            "insert into sticky_msgs (embed_id, guild_id, channel_id, position, bots, created_by, updated_by)
             values ($1, $2, $3, 1, false, 9, 9)",
        )
        .bind(embed)
        .bind(g)
        .bind(ch)
        .execute(&st.db)
        .await
        .unwrap();
        super::repost(&st, g, ch, false).await.unwrap();
        let (on, issue): (bool, Option<String>) =
            sqlx::query_as("select enabled, issue from sticky_msgs where channel_id = $1")
                .bind(ch)
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert!(!on);
        assert!(issue
            .unwrap()
            .starts_with("Paused: the member who last saved this sticky message (user id 9)"));
    }
}

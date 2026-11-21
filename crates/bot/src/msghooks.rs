//! Upkeep of the Message Builder's webhooks (one per channel, made by the dashboard on first use).
use crate::{app::App, events::send_log};
use anyhow::Result;
use brim_logging::{embed::ORANGE, Log, LogKind};
use twilight_model::{
    guild::audit_log::{AuditLogEntry, AuditLogEventType},
    id::{marker::GuildMarker, Id},
};

/// The Message Builder webhooks of a server, which Brim's own cleanups (webhook spam, backup
/// restores) must not delete: only they can edit the copies they posted.
pub async fn ours(app: &App, guild: Id<GuildMarker>) -> Result<Vec<u64>> {
    let ids: Vec<i64> =
        sqlx::query_scalar("select webhook_id from message_webhooks where guild_id = $1")
            .bind(guild.get() as i64)
            .fetch_all(&app.pool)
            .await?;
    Ok(ids.into_iter().map(|i| i as u64).collect())
}

/// A Message Builder webhook deleted in Discord (by hand, or by another bot): its sent copies can
/// no longer be edited. Marks them for the Sent log and Overview, and logs who deleted it.
/// Anti-nuke counts the same audit entry on its own; this only records it.
pub async fn on_audit(app: &App, e: &AuditLogEntry) -> Result<()> {
    if e.action_type == AuditLogEventType::WebhookUpdate {
        return on_moved(app, e).await;
    }
    let (AuditLogEventType::WebhookDelete, Some(gid), Some(target)) =
        (e.action_type, e.guild_id, e.target_id)
    else {
        return Ok(());
    };
    let (guild, hook) = (gid.get() as i64, target.get() as i64);
    let by = e.user_id.map(|u| u.get() as i64);
    // Feeds posting through it: the next post makes a new webhook (feeds.rs). Logged here with
    // who deleted it; Brim removing its own is not news.
    let feeds: Vec<String> = sqlx::query_scalar(
        "select coalesce(nullif(f.name, ''), nullif(f.source_name, ''), f.target) from feeds f
         join message_webhooks w on w.channel_id = f.channel_id
         where w.webhook_id = $1 and w.guild_id = $2 order by f.id",
    )
    .bind(hook)
    .bind(guild)
    .fetch_all(&app.pool)
    .await?;
    if !feeds.is_empty() && e.user_id != Some(app.me) {
        let mut log = Log::new("Feed Webhook Deleted", ORANGE);
        if let Some(u) = e.user_id {
            log = log.user("Deleted by", u.get());
        }
        let names: Vec<String> = feeds
            .iter()
            .map(|n| format!("**{}**", n.replace('*', "")))
            .collect();
        let log = log.line("Feeds", &names.join(", ")).line(
            "Effect",
            "Brim makes a new webhook at the next post. During panic mode or an anti-nuke case it does not, and the feeds stop until someone clicks Send latest on the Feeds page.",
        );
        send_log(app, gid.get(), LogKind::Webhook, log).await?;
    }
    let mut tx = app.pool.begin().await?;
    let row: Option<i64> = sqlx::query_scalar(
        "delete from message_webhooks where webhook_id = $1 and guild_id = $2 returning channel_id",
    )
    .bind(hook)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?;
    // Copies the dashboard already marked (a send or edit found the webhook gone before this entry
    // came) get the name of who deleted it now, and count for the log.
    // Each name comes with what happens to its copies: a sticky copy is replaced at its next
    // repost, a copy with a delete time is still deleted then (Brim deletes it as the bot),
    // and only the other copies stay up and can no longer be edited.
    let hit: Vec<(i64, String, String)> = sqlx::query_as(
        "with m as (update message_sends set hook_gone_at = coalesce(hook_gone_at, now()), hook_gone_by = $3
                    where webhook_id = $1 and guild_id = $2 and posted_id is not null and hook_gone_by is null
                    returning id, embed_id, channel_id, source, delete_at)
         select min(m.channel_id), e.name,
                case when exists (select 1 from sticky_msgs k where k.send_id = m.id) then 'sticky' when m.delete_at is not null then 'timed' else 'kept' end k
         from m join embeds e on e.id = m.embed_id group by e.name, k order by e.name",
    )
    .bind(hook)
    .bind(guild)
    .bind(by)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    // Brim removing a webhook it no longer needs (no copy still up) is not news.
    let Some(channel) = row.or(hit.first().map(|h| h.0)) else {
        return Ok(());
    };
    if hit.is_empty() && e.user_id == Some(app.me) {
        return Ok(());
    }
    let effect = if hit.is_empty() {
        "No sent copy used it. The next send makes a new one.".to_string()
    } else {
        let names = |k: &str| {
            let n: Vec<String> = hit
                .iter()
                .filter(|h| h.2 == k)
                .map(|h| format!("**{}**", h.1))
                .collect();
            (!n.is_empty()).then(|| n.join(", "))
        };
        [
            names("kept").map(|n| format!("Sent copies of {n} can no longer be edited (only that webhook could edit them). Send the message again to post a new copy, then delete the old copy in Discord.")),
            names("timed").map(|n| format!("Copies of {n} with a delete time are still deleted then, by Brim as the bot (if one fails, its Sent log says so).")),
            names("sticky").map(|n| format!("Sticky copies of {n} need nothing: Brim replaces them at the next repost.")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
    };
    let mut log = Log::new("Message Builder Webhook Deleted", ORANGE)
        .line("Channel", &format!("<#{channel}>"));
    log = match e.user_id {
        Some(u) => log.user("Deleted by", u.get()),
        None => log,
    };
    send_log(
        app,
        gid.get(),
        LogKind::Webhook,
        log.line("Effect", &effect),
    )
    .await
}

/// Someone moved Brim's webhook to another channel: feeds would post in the wrong channel, so
/// when feeds use it Brim deletes it (the delete then marks its Message Builder copies) and
/// makes a new one in the feed's channel at the next post. Anti-nuke counts the edit on its own.
async fn on_moved(app: &App, e: &AuditLogEntry) -> Result<()> {
    let (Some(gid), Some(target)) = (e.guild_id, e.target_id) else {
        return Ok(());
    };
    let moved = e.changes.iter().any(|c| {
        matches!(
            c,
            twilight_model::guild::audit_log::AuditLogChange::ChannelId { .. }
        )
    });
    if !moved || e.user_id == Some(app.me) {
        return Ok(());
    }
    let (guild, hook) = (gid.get() as i64, target.get() as i64);
    let row: Option<i64> = sqlx::query_scalar(
        "select w.channel_id from message_webhooks w where w.webhook_id = $1 and w.guild_id = $2
         and exists (select 1 from feeds f where f.channel_id = w.channel_id)",
    )
    .bind(hook)
    .bind(guild)
    .fetch_optional(&app.pool)
    .await?;
    let Some(channel) = row else { return Ok(()) };
    sqlx::query("delete from message_webhooks where webhook_id = $1 and guild_id = $2")
        .bind(hook)
        .bind(guild)
        .execute(&app.pool)
        .await?;
    let deleted = app.http.delete_webhook(Id::new(target.get())).await;
    let effect = match deleted {
        Ok(_) => "Brim deleted that webhook and makes a new one in this channel at the next feed post.",
        Err(ref e) if crate::http_err::gone(e) => "Brim makes a new webhook in this channel at the next feed post.",
        Err(_) => "Brim could not delete that webhook (delete it by hand in the other channel). It makes a new one in this channel at the next feed post.",
    };
    let mut log =
        Log::new("Feed Webhook Moved", ORANGE).line("Feed channel", &format!("<#{channel}>"));
    if let Some(u) = e.user_id {
        log = log.user("Moved by", u.get());
    }
    send_log(app, gid.get(), LogKind::Webhook, log.line("Effect", effect)).await
}

/// A deleted channel takes its webhook with it: forget the row (and its token).
pub async fn channel_deleted(app: &App, guild: Id<GuildMarker>, channel: u64) -> Result<()> {
    sqlx::query("delete from message_webhooks where channel_id = $1 and guild_id = $2")
        .bind(channel as i64)
        .bind(guild.get() as i64)
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// Deletes webhooks no send used for 30 days, unless copies they posted are still up (only they
/// can edit those) or a feed posts through them. Discord saying it is already gone counts as done; any other refusal waits
/// 30 more days, so it does not hold the oldest 50 for good.
pub async fn sweep(app: &App) -> Result<()> {
    let old: Vec<(i64, i64)> = sqlx::query_as(
        "select channel_id, webhook_id from message_webhooks w where last_used < now() - interval '30 days'
         and not exists (select 1 from message_sends m where m.webhook_id = w.webhook_id and m.posted_id is not null)
         and not exists (select 1 from feeds f where f.channel_id = w.channel_id)
         order by last_used limit 50",
    )
    .fetch_all(&app.pool)
    .await?;
    for (channel, id) in old {
        let status = match app.http.delete_webhook(Id::new(id.max(1) as u64)).await {
            // Deleted now counts the same as already gone.
            Ok(_) => Some(404),
            Err(e) => crate::http_err::status(&e),
        };
        let sql = match status {
            Some(404) => "delete from message_webhooks where channel_id = $1 and webhook_id = $2",
            Some(400..500) => "update message_webhooks set last_used = now() where channel_id = $1 and webhook_id = $2",
            // An outage: try again at the next sweep.
            _ => continue,
        };
        sqlx::query(sql)
            .bind(channel)
            .bind(id)
            .execute(&app.pool)
            .await?;
    }
    Ok(())
}

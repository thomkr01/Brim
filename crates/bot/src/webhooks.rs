//! Webhook rules: the whitelist check for webhook messages and the cleanup after a heat trigger.
use anyhow::Result;
use brim_antinuke::{webhook_whitelisted, webhooks_to_delete, Webhook};
use brim_logging::{Log, LogKind};
use std::collections::{hash_map::Entry, HashMap};
use twilight_model::{
    id::{
        marker::{GuildMarker, WebhookMarker},
        Id,
    },
    user::UserFlags,
};

use crate::{
    app::{now_secs, App},
    events::send_log,
};

/// A webhook is exempt if its owner is whitelisted (bot-made hooks only, user hooks have
/// an exposed token) or it is listed itself; when the webhook cannot be fetched only the list counts.
pub(crate) async fn webhook_exempt(
    app: &App,
    gid: Id<GuildMarker>,
    hook: Id<WebhookMarker>,
    listed: bool,
) -> bool {
    if listed {
        return true;
    }
    let fetched = async { anyhow::Ok(app.http.webhook(hook).await?.model().await?) }.await;
    let cached = match fetched.map(|h| h.user) {
        Ok(Some(owner)) => app
            .whitelisted(gid, owner.id, "antinuke")
            .await
            .ok()
            .map(|wl| (wl, !owner.bot)),
        _ => None,
    };
    webhook_whitelisted(cached, listed)
}

/// Deletes webhooks made in the last 10 minutes unless their owner is immune.
pub(crate) async fn delete_new_webhooks(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let mut hooks = vec![];
    let mut immune: HashMap<u64, bool> = HashMap::new();
    for h in app.http.guild_webhooks(gid).await?.models().await? {
        // Hooks without an owner (integrations, channel follows) count as owner 0, never immune.
        let owner = h.user.as_ref().map(|u| u.id);
        if let Some(owner) = owner {
            if let Entry::Vacant(e) = immune.entry(owner.get()) {
                // Verified bots stand in for popular bots.
                let verified = h
                    .user
                    .as_ref()
                    .and_then(|u| u.public_flags)
                    .is_some_and(|f| f.contains(UserFlags::VERIFIED_BOT));
                e.insert(verified || app.is_immune(gid, owner).await?);
            }
        }
        // Snowflake timestamp: ms since the Discord epoch in the top bits.
        let created_at = ((h.id.get() >> 22) + 1_420_070_400_000) / 1000;
        hooks.push(Webhook {
            id: h.id.get(),
            owner: owner.map_or(0, |o| o.get()),
            created_at: created_at as i64,
        });
    }
    let cfg = app.cfg(gid.get()).await?;
    // The Message Builder's own webhooks: their token never leaves Brim, so they cannot spam.
    // Unread: the cleanup still runs, protection comes first.
    let ours = crate::msghooks::ours(app, gid).await.unwrap_or_else(|e| {
        tracing::warn!(
            ?e,
            "webhook cleanup: reading Message Builder webhooks failed"
        );
        vec![]
    });
    let doomed = webhooks_to_delete(
        &hooks,
        now_secs() - 600,
        |o| immune.get(&o).copied().unwrap_or(false),
        |id| ours.contains(&id) || cfg.whitelist.spam.webhooks.iter().any(|w| w.get() == id),
    );
    let mut deleted = 0;
    for id in &doomed {
        let r = app.http.delete_webhook(Id::new(*id)).await;
        deleted += usize::from(crate::http_err::best_effort(r, "delete spam webhook").is_some());
    }
    // Every message of a flood lands here; only the one that deleted hooks is logged.
    if deleted == 0 {
        return Ok(());
    }
    let log = deleted_log(deleted);
    send_log(app, gid.get(), LogKind::AutoMod, log).await
}

/// The automod log after webhook spam: how many hooks Brim deleted.
pub(crate) fn deleted_log(count: usize) -> Log {
    brim_logging::overlay::security_log(
        "Webhooks Deleted",
        "log.auto_mod.webhooks_deleted",
        &[("count", &format!("`{count}`"))],
    )
}

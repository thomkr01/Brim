//! The quarantine overwrite on channels (`misc.enforce_quarantine_perms`). Setting a server up is
//! the dashboard's job (Auto Setup and the setup wizard); the old `t!setup run` was removed
//! (owner decision 2026-11-16).
use anyhow::Result;
use twilight_model::{
    channel::Channel,
    guild::Permissions,
    http::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
    id::{
        marker::{ChannelMarker, GuildMarker, RoleMarker},
        Id,
    },
};

use twilight_http::request::AuditLogReason;

use crate::app::App;

use brim_core::sections::{verification_shape, QUARANTINE_DENY, VERIFY_ALLOW, VERIFY_DENY};

/// `misc.enforce_quarantine_perms`: puts the quarantine overwrite back on a channel that lacks it.
/// The verification channel gets [`quarantine_overwrite`]'s verification shape instead, which the
/// join quarantine can read.
pub async fn enforce_quarantine(app: &App, c: &Channel) -> Result<()> {
    let Some(gid) = c.guild_id else {
        return Ok(());
    };
    let cfg = app.cfg(gid.get()).await?;
    let Some(role) = cfg
        .antinuke
        .quarantine_role
        .filter(|_| cfg.misc.enforce_quarantine_perms)
    else {
        return Ok(());
    };
    let (mut normal, mut shaped) = (false, false);
    for o in c
        .permission_overwrites
        .iter()
        .flatten()
        .filter(|o| o.id == role.cast())
    {
        let (allow, deny) = (o.allow.bits(), o.deny.bits());
        normal = deny & QUARANTINE_DENY == QUARANTINE_DENY && allow & QUARANTINE_DENY == 0;
        shaped = verification_shape(allow, deny);
    }
    let verification = cfg.verification.channel == Some(c.id)
        || (shaped && new_verification_channel(app, gid, c.id).await?);
    if (verification && shaped) || (!verification && normal) {
        return Ok(());
    }
    app.http
        .update_channel_permission(c.id, &quarantine_overwrite(role, verification))
        .reason("Sanitizing the channel: quarantine permissions were missing")
        .await?;
    Ok(())
}

/// The quarantine role's overwrite: the full deny, or the verification channel's shape.
pub(crate) fn quarantine_overwrite(
    role: Id<RoleMarker>,
    verification: bool,
) -> PermissionOverwrite {
    let (allow, deny) = if verification {
        (VERIFY_ALLOW, VERIFY_DENY)
    } else {
        (0, QUARANTINE_DENY)
    };
    PermissionOverwrite {
        id: role.cast(),
        kind: PermissionOverwriteType::Role,
        allow: Some(Permissions::from_bits_truncate(allow)),
        deny: Some(Permissions::from_bits_truncate(deny)),
    }
}

/// A channel with the verification shape that is being set up as the verification channel: the
/// bot saves the one it made under `CHANNEL_LOCK` (its ChannelCreate can come in first), and the
/// dashboard's Setup Channel saves after making it, so while verification is on and no live
/// channel is saved, the shape marks it.
async fn new_verification_channel(
    app: &App,
    gid: Id<GuildMarker>,
    c: Id<ChannelMarker>,
) -> Result<bool> {
    let _made = crate::verify::CHANNEL_LOCK.lock().await;
    let cfg = app.cfg(gid.get()).await?;
    let v = &cfg.verification;
    Ok(match v.channel {
        Some(saved) if saved == c => true,
        _ if !v.enabled => false,
        None => true,
        // A saved channel deleted in Discord counts as none.
        Some(saved) => match app.http.channel(saved).await {
            Ok(_) => false,
            Err(e) => crate::http_err::gone(&e),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_denies_access_invites_and_threads() {
        let p = Permissions::VIEW_CHANNEL
            | Permissions::SEND_MESSAGES
            | Permissions::ADD_REACTIONS
            | Permissions::CONNECT
            | Permissions::CREATE_INVITE
            | Permissions::SEND_MESSAGES_IN_THREADS
            | Permissions::CREATE_PUBLIC_THREADS
            | Permissions::CREATE_PRIVATE_THREADS;
        assert_eq!(p.bits(), QUARANTINE_DENY);
        // A sanitized channel blocks at least invites, viewing, sending and every thread kind.
        let must = Permissions::CREATE_INVITE
            | Permissions::VIEW_CHANNEL
            | Permissions::SEND_MESSAGES
            | Permissions::CREATE_PUBLIC_THREADS
            | Permissions::CREATE_PRIVATE_THREADS
            | Permissions::SEND_MESSAGES_IN_THREADS;
        assert!(Permissions::from_bits_truncate(QUARANTINE_DENY).contains(must));
    }
}

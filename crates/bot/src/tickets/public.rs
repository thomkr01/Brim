//! The public ticket channel (#open-a-ticket): made in the ticket category with the panel
//! message, where members can only look and press the button. Turned off, it is deleted.

use super::staff_roles;
use crate::{app::App, http_err, settings};
use anyhow::Result;
use brim_core::sections::{
    default_panels, panel_message, TicketPanel, TicketsCfg, VerificationCfg,
};
use serde_json::{json, Value};
use twilight_model::{
    channel::{
        message::{AllowedMentions, Component, Embed},
        permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
        Channel, ChannelType,
    },
    guild::Permissions,
    id::{
        marker::{ChannelMarker, GuildMarker, RoleMarker},
        Id,
    },
};

pub(crate) const NAME: &str = "open-a-ticket";

/// One check at a time, so two saves in a row never make two channels.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Everything a member could do in a text channel besides reading and pressing a button.
const MEMBER_DENY: Permissions = Permissions::SEND_MESSAGES
    .union(Permissions::SEND_MESSAGES_IN_THREADS)
    .union(Permissions::CREATE_PUBLIC_THREADS)
    .union(Permissions::CREATE_PRIVATE_THREADS)
    .union(Permissions::ADD_REACTIONS)
    .union(Permissions::ATTACH_FILES)
    .union(Permissions::EMBED_LINKS)
    .union(Permissions::USE_EXTERNAL_EMOJIS)
    .union(Permissions::USE_EXTERNAL_STICKERS)
    .union(Permissions::MENTION_EVERYONE)
    .union(Permissions::SEND_TTS_MESSAGES)
    .union(Permissions::SEND_VOICE_MESSAGES)
    .union(Permissions::SEND_POLLS)
    .union(Permissions::USE_SLASH_COMMANDS)
    .union(Permissions::USE_EXTERNAL_APPS)
    .union(Permissions::USE_EMBEDDED_ACTIVITIES)
    .union(Permissions::CREATE_INVITE);
const LOOK: Permissions = Permissions::VIEW_CHANNEL.union(Permissions::READ_MESSAGE_HISTORY);
const BOT: Permissions = LOOK
    .union(Permissions::SEND_MESSAGES)
    .union(Permissions::EMBED_LINKS);

/// Who sees the channel: the verified roles, or everyone only when verification is off and the
/// server has no verified role (owner decision 11-15). Members can never write, react or post anything.
pub(crate) fn overwrites(
    gid: Id<GuildMarker>,
    bot: u64,
    verified: &[Id<RoleMarker>],
    verification_on: bool,
    staff: &[u64],
) -> Vec<PermissionOverwrite> {
    let everyone_sees = verified.is_empty() && !verification_on;
    let role = |id: u64, allow| PermissionOverwrite {
        allow,
        deny: Permissions::empty(),
        id: Id::new(id),
        kind: PermissionOverwriteType::Role,
    };
    let mut v = vec![
        PermissionOverwrite {
            allow: if everyone_sees {
                LOOK
            } else {
                Permissions::empty()
            },
            deny: if everyone_sees {
                MEMBER_DENY
            } else {
                MEMBER_DENY | Permissions::VIEW_CHANNEL
            },
            id: gid.cast(),
            kind: PermissionOverwriteType::Role,
        },
        PermissionOverwrite {
            allow: BOT,
            deny: Permissions::empty(),
            id: Id::new(bot),
            kind: PermissionOverwriteType::Member,
        },
    ];
    let everyone = gid.get();
    v.extend(
        verified
            .iter()
            .filter(|r| r.get() != everyone)
            .map(|r| role(r.get(), LOOK)),
    );
    v.extend(
        staff
            .iter()
            .filter(|s| **s != everyone && !verified.iter().any(|r| r.get() == **s))
            .map(|s| role(*s, LOOK)),
    );
    v
}

/// The configured verification roles, else the server's roles named "Member".
async fn verified_roles(
    app: &App,
    gid: Id<GuildMarker>,
    v: &VerificationCfg,
) -> Result<Vec<Id<RoleMarker>>> {
    if !v.roles.is_empty() {
        return Ok(v.roles.clone());
    }
    let roles = app.http.roles(gid).await?.models().await?;
    Ok(roles
        .into_iter()
        .filter(|r| r.name.eq_ignore_ascii_case("member"))
        .map(|r| r.id)
        .collect())
}

/// The newest saved config, not the cache: the cache can still miss the id this module just saved.
async fn latest(app: &App, gid: Id<GuildMarker>) -> Result<Value> {
    let doc: Option<Value> = sqlx::query_scalar(
        "select config from guild_config where guild_id = $1 order by version desc limit 1",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    Ok(doc.unwrap_or_else(|| json!({})))
}

/// The channel, or `None` when Discord says it is gone.
async fn fetch(app: &App, ch: Id<ChannelMarker>) -> Result<Option<Channel>> {
    match app.http.channel(ch).await.map_err(anyhow::Error::from) {
        Ok(r) => Ok(Some(r.model().await?)),
        Err(e) if http_err::code(&e) == Some(10003) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn save_id(
    app: &App,
    gid: Id<GuildMarker>,
    id: Option<Id<ChannelMarker>>,
    default: bool,
) -> Result<()> {
    settings::save(app, gid, app.me.get(), &["tickets"], |doc| {
        if !doc["tickets"].is_object() {
            doc["tickets"] = json!({});
        }
        doc["tickets"]["public_channel_id"] = json!(id.map(|i| i.to_string()));
        if default {
            let types: Vec<String> = match doc["tickets"]["types"].as_array() {
                Some(t) => t
                    .iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect(),
                None => TicketsCfg::default().types,
            };
            let panels = &mut doc["tickets"]["panels"];
            if !panels.is_array() {
                *panels = json!([]);
            }
            if let Some(p) = panels.as_array_mut().filter(|p| p.is_empty()) {
                p.extend(
                    default_panels(&types)
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                );
            }
        }
        Ok(())
    })
    .await?
    .map_err(anyhow::Error::msg)?;
    Ok(())
}

/// Keeps the channel when it is on: made when missing, its overwrites and category put back
/// when they drifted, and its panel message posted again when it has none. Off deletes it.
/// Returns the channel it made, if any.
pub async fn ensure(app: &App, gid: Id<GuildMarker>) -> Result<Option<Id<ChannelMarker>>> {
    let _one_at_a_time = LOCK.lock().await;
    let doc = latest(app, gid).await?;
    let t: TicketsCfg = serde_json::from_value(doc["tickets"].clone()).unwrap_or_default();
    let live = match t.public_channel_id {
        Some(c) => fetch(app, c).await?.filter(|ch| ch.guild_id == Some(gid)),
        None => None,
    };
    if !(t.enabled && t.public_channel) {
        if let Some(c) = t.public_channel_id {
            // Only the channel this module made: a config from elsewhere must never get another deleted.
            if live
                .as_ref()
                .is_some_and(|ch| ch.name.as_deref() == Some(NAME))
            {
                match app
                    .http
                    .delete_channel(c)
                    .await
                    .map_err(anyhow::Error::from)
                {
                    Ok(_) => {}
                    Err(e) if http_err::code(&e) == Some(10003) => {}
                    Err(e) => return Err(e),
                }
            }
            forget_posts(app, gid, c).await?;
            save_id(app, gid, None, false).await?;
            if let Some(cat) = t.category {
                drop_empty_category(app, gid, cat).await?;
            }
        }
        return Ok(None);
    }
    // A deleted or unset category is made again, or the channel could never be made.
    let Some(category) = super::ensure_category(app, gid).await? else {
        return Ok(None);
    };
    let v: VerificationCfg =
        serde_json::from_value(doc["verification"].clone()).unwrap_or_default();
    let verified = verified_roles(app, gid, &v).await?;
    let staff = staff_roles(app, gid).await?;
    let mut ows = overwrites(gid, app.me.get(), &verified, v.enabled, &staff);
    // No panel yet: the default one, which staff edit on the Tickets tab.
    let default = t.panels.is_empty();
    let fallback: Vec<TicketPanel> = serde_json::from_value(default_panels(&t.types))?;
    let mut panels: Vec<&TicketPanel> = t.panels.iter().filter(|p| p.enabled).collect();
    if default {
        panels.extend(&fallback);
    }
    let look = crate::reply::stored_msg(app, gid, "ticket.panel").await;
    if let Some(ch) = live {
        // The quarantine role's deny is added by the channel create handler: keep it.
        let q = app.cfg(gid.get()).await?.antinuke.quarantine_role;
        ows.extend(
            ch.permission_overwrites
                .iter()
                .flatten()
                .filter(|o| Some(o.id.cast()) == q)
                .cloned(),
        );
        let key = |v: &[PermissionOverwrite]| {
            let mut k: Vec<_> = v
                .iter()
                .map(|o| (o.id.get(), o.allow.bits(), o.deny.bits()))
                .collect();
            k.sort_unstable();
            k
        };
        if key(&ows) != key(ch.permission_overwrites.as_deref().unwrap_or_default())
            || ch.parent_id != Some(category)
        {
            app.http
                .update_channel(ch.id)
                .parent_id(Some(category))
                .permission_overwrites(&ows)
                .await?;
        }
        sync_post(app, gid, ch.id, &panels, look.as_ref()).await?;
        return Ok(None);
    }
    if let Some(old) = t.public_channel_id {
        forget_posts(app, gid, old).await?;
    }
    let ch = app
        .http
        .create_guild_channel(gid, NAME)
        .kind(ChannelType::GuildText)
        .parent_id(category)
        .permission_overwrites(&ows)
        .await?
        .model()
        .await?
        .id;
    if let Err(e) = save_id(app, gid, Some(ch), default).await {
        // Unrecorded, it would be made again on every save: take it down.
        let _ = app.http.delete_channel(ch).await;
        return Err(e);
    }
    if !panels.is_empty() {
        // A failed post is tried again on the next save (`sync_post`).
        post(app, gid, ch, &panels, look.as_ref()).await?;
    }
    Ok(Some(ch))
}

/// Off also removes the ticket category, but only while it holds no channel and no ticket is
/// open (owner decision 11-15). The next ticket makes it again (`ensure_category`).
async fn drop_empty_category(
    app: &App,
    gid: Id<GuildMarker>,
    cat: Id<ChannelMarker>,
) -> Result<()> {
    let open: bool = sqlx::query_scalar(
        "select exists (select 1 from tickets where guild_id = $1 and status <> 'closed')",
    )
    .bind(gid.get() as i64)
    .fetch_one(&app.pool)
    .await?;
    let channels = app.http.guild_channels(gid).await?.models().await?;
    if open
        || channels.iter().any(|c| c.parent_id == Some(cat))
        || !channels.iter().any(|c| c.id == cat)
    {
        return Ok(());
    }
    match app
        .http
        .delete_channel(cat)
        .await
        .map_err(anyhow::Error::from)
    {
        Ok(_) => {}
        Err(e) if http_err::code(&e) == Some(10003) => {}
        Err(e) => return Err(e),
    }
    settings::save(app, gid, app.me.get(), &["tickets"], |doc| {
        if doc["tickets"]["category"] == json!(cat.to_string()) {
            doc["tickets"]["category"] = Value::Null;
        }
        Ok(())
    })
    .await?
    .map_err(anyhow::Error::msg)?;
    Ok(())
}

async fn forget_posts(app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>) -> Result<()> {
    sqlx::query("delete from ticket_panel_posts where guild_id = $1 and channel_id = $2")
        .bind(gid.get() as i64)
        .bind(ch.get() as i64)
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// Whether `ch` is the public ticket channel while the setting is on.
pub async fn is_public(app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>) -> bool {
    app.cfg(gid.get()).await.is_ok_and(|c| {
        c.tickets.enabled && c.tickets.public_channel && c.tickets.public_channel_id == Some(ch)
    })
}

/// The body last put in each posted message, so a save that changed nothing edits nothing.
// shortcut: kept in memory, so the first save after a restart edits the post once; a column if that matters.
static SENT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<u64, String>>> =
    std::sync::LazyLock::new(Default::default);

/// The embed and buttons of the panel message, and the JSON they come from. `look`: the
/// server's edited `ticket.panel` entry.
fn body(panels: &[&TicketPanel], look: Option<&Value>) -> Result<(Embed, Vec<Component>, String)> {
    let m = panel_message(panels, false, look);
    let components: Vec<Component> = serde_json::from_value(m["components"].clone())?;
    let mut e = m["embeds"][0].clone();
    e["type"] = json!("rich");
    let embed: Embed = serde_json::from_value(e)?;
    Ok((embed, components, m.to_string()))
}

/// Posts the panel message, or edits the posted one when the panels changed (the Tickets tab
/// edits the button and text there). A post someone deleted is posted again; posts staff
/// made with Publish are left as they are.
async fn sync_post(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    panels: &[&TicketPanel],
    look: Option<&Value>,
) -> Result<()> {
    let posted: Vec<i64> = sqlx::query_scalar(
        "select message_id from ticket_panel_posts
         where guild_id = $1 and channel_id = $2 and posted_by = $3",
    )
    .bind(gid.get() as i64)
    .bind(ch.get() as i64)
    .bind(app.me.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    // Every panel turned off: no button may stay that opens one.
    if panels.is_empty() {
        for m in posted {
            match app
                .http
                .delete_message(ch, Id::new(m as u64))
                .await
                .map_err(anyhow::Error::from)
            {
                Ok(_) => {}
                Err(e) if http_err::code(&e) == Some(10008) => {}
                Err(e) => return Err(e),
            }
            sqlx::query("delete from ticket_panel_posts where message_id = $1")
                .bind(m)
                .execute(&app.pool)
                .await?;
        }
        return Ok(());
    }
    let (embed, components, sent) = body(panels, look)?;
    let ids: Vec<String> = panels.iter().map(|p| p.id.clone()).collect();
    let mut live = false;
    for m in posted {
        let id = m as u64;
        if SENT.lock().unwrap_or_else(|e| e.into_inner()).get(&id) == Some(&sent) {
            live = true;
            continue;
        }
        let edit = app
            .http
            .update_message(ch, Id::new(id))
            .embeds(Some(std::slice::from_ref(&embed)))
            .components(Some(&components))
            .await
            .map_err(anyhow::Error::from);
        match edit {
            Ok(_) => {
                sqlx::query("update ticket_panel_posts set panels = $2 where message_id = $1")
                    .bind(m)
                    .bind(&ids)
                    .execute(&app.pool)
                    .await?;
                SENT.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, sent.clone());
                live = true;
            }
            Err(e) if http_err::code(&e) == Some(10008) => {
                sqlx::query("delete from ticket_panel_posts where message_id = $1")
                    .bind(m)
                    .execute(&app.pool)
                    .await?;
            }
            Err(e) => return Err(e),
        }
    }
    if live {
        return Ok(());
    }
    post(app, gid, ch, panels, look).await
}

async fn post(
    app: &App,
    gid: Id<GuildMarker>,
    ch: Id<ChannelMarker>,
    panels: &[&TicketPanel],
    look: Option<&Value>,
) -> Result<()> {
    let (embed, components, sent) = body(panels, look)?;
    let msg = app
        .http
        .create_message(ch)
        .embeds(&[embed])
        .components(&components)
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await?
        .model()
        .await?;
    let ids: Vec<String> = panels.iter().map(|p| p.id.clone()).collect();
    sqlx::query(
        "insert into ticket_panel_posts (message_id, guild_id, channel_id, panels, posted_by)
         values ($1, $2, $3, $4, $5)",
    )
    .bind(msg.id.get() as i64)
    .bind(gid.get() as i64)
    .bind(ch.get() as i64)
    .bind(&ids)
    .bind(app.me.get() as i64)
    .execute(&app.pool)
    .await?;
    SENT.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(msg.id.get(), sent);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_only_look_and_everyone_sees_it_only_without_verification() {
        let g = Id::new(1);
        let verified = [Id::new(5)];
        let o = overwrites(g, 2, &verified, true, &[7, 5]);
        let everyone = o.iter().find(|o| o.id == g.cast()).unwrap();
        assert!(everyone
            .deny
            .contains(Permissions::VIEW_CHANNEL | MEMBER_DENY));
        let v = o.iter().find(|o| o.id.get() == 5).unwrap();
        assert_eq!(v.allow, LOOK);
        assert!(
            o.iter().filter(|o| o.id.get() == 5).count() == 1,
            "a staff role that is also verified is listed once"
        );
        assert!(
            o.iter()
                .all(|o| o.id.get() == 2 || !o.allow.intersects(MEMBER_DENY)),
            "only the bot may write"
        );
        // Verification off and no verified role: everyone sees it, still read only.
        let o = overwrites(g, 2, &[], false, &[]);
        assert_eq!(o[0].allow, LOOK);
        assert!(o[0].deny.contains(MEMBER_DENY) && !o[0].deny.contains(Permissions::VIEW_CHANNEL));
        // Verification on with no role: hidden.
        assert!(overwrites(g, 2, &[], true, &[])[0]
            .deny
            .contains(Permissions::VIEW_CHANNEL));
    }

    #[test]
    fn the_panel_message_buttons_parse() {
        let ps: Vec<TicketPanel> =
            serde_json::from_value(default_panels(&TicketsCfg::default().types)).unwrap();
        let refs: Vec<&TicketPanel> = ps.iter().collect();
        let m = panel_message(&refs, false, None);
        let c: Vec<Component> = serde_json::from_value(m["components"].clone()).unwrap();
        assert_eq!(c.len(), 1);
        // The default look and an edited one both become a Discord embed.
        let (e, _, _) = body(&refs, None).unwrap();
        assert!(e.footer.is_some() && e.description.is_some());
        let own = json!({"content": "", "embeds": [{"title": "Help desk", "description": "{text}", "color": 1}]});
        let (e, _, _) = body(&refs, Some(&own)).unwrap();
        assert_eq!(e.title.as_deref(), Some("Help desk"));
    }
}

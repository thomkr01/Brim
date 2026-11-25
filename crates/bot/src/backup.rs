//! Server backups (spec 4.3): snapshot roles and channels into Postgres, and restore by syncing
//! the live server to an image. Restore uses its own HTTP client (own rate-limit bucket) so a
//! flood of normal traffic can't slow it down during a nuke.
use anyhow::Result;
use brim_antinuke::{
    describe, map_overwrites, plan, Action, Change, ChannelImg, Image, OverwriteImg, Plan, RoleImg,
    Settings, WebhookImg, CATEGORY, DANGEROUS_PERMS,
};
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::types::Json;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use twilight_http::Client;
use twilight_model::{
    channel::{
        permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
        Channel, ChannelType,
    },
    guild::{
        DefaultMessageNotificationLevel, ExplicitContentFilter, Permissions, Role, RolePosition,
        VerificationLevel,
    },
    http::channel_position::Position,
    id::{marker::GuildMarker, Id},
};

use crate::app::App;
use brim_core::sections::BackupsCfg;
use brim_logging::embed::{NO, YES};

/// How often the auto-backup loop looks at each guild; the guild's own interval decides.
const TICK: Duration = Duration::from_secs(600);

// Role::color is deprecated upstream in favour of `colors`, but create/update_role still take it.
#[allow(deprecated)]
pub fn role_img(r: Role) -> RoleImg {
    RoleImg {
        id: r.id.get(),
        name: r.name,
        color: r.color,
        permissions: r.permissions.bits(),
        position: r.position,
        hoist: r.hoist,
        mentionable: r.mentionable,
        managed: r.managed,
    }
}

pub fn channel_img(c: Channel) -> ChannelImg {
    ChannelImg {
        id: c.id.get(),
        name: c.name.unwrap_or_default(),
        kind: u8::from(c.kind),
        parent: c.parent_id.map(|p| p.get()),
        position: c.position.map_or(0, i64::from),
        topic: c.topic,
        nsfw: c.nsfw.unwrap_or(false),
        overwrites: c
            .permission_overwrites
            .unwrap_or_default()
            .into_iter()
            .map(|o| OverwriteImg {
                target: o.id.get(),
                is_role: o.kind == PermissionOverwriteType::Role,
                allow: o.allow.bits(),
                deny: o.deny.bits(),
            })
            .collect(),
    }
}

pub async fn snapshot(http: &Client, gid: Id<GuildMarker>) -> Result<Image> {
    let g = http.guild(gid).await?.model().await?;
    let roles = crate::hierarchy::ranked(gid, http.roles(gid).await?.models().await?);
    let channels = http.guild_channels(gid).await?.models().await?;
    // Needs Manage Webhooks; without it the image has none and restores leave webhooks alone.
    let webhooks = match http.guild_webhooks(gid).await {
        Ok(r) => Some(
            r.models()
                .await?
                .into_iter()
                // twilight 0.18: channel_id is nullable; a webhook without a channel can't be restored.
                .filter_map(|w| {
                    Some(WebhookImg {
                        id: w.id.get(),
                        name: w.name.unwrap_or_default(),
                        channel: w.channel_id?.get(),
                    })
                })
                .collect(),
        ),
        Err(e) => {
            tracing::warn!(?e, "backup: listing webhooks failed");
            None
        }
    };
    Ok(Image {
        guild: gid.get(),
        webhooks,
        settings: Some(Settings {
            name: g.name.clone(),
            verification: u8::from(g.verification_level),
            notifications: u8::from(g.default_message_notifications),
            content_filter: u8::from(g.explicit_content_filter),
            afk_channel: g.afk_channel_id.map(|c| c.get()),
            afk_timeout: g.afk_timeout.get().into(),
            system_channel: g.system_channel_id.map(|c| c.get()),
            locale: g.preferred_locale,
            icon: g.icon.map(|h| h.to_string()),
        }),
        name: g.name,
        roles: roles.into_iter().map(role_img).collect(),
        channels: channels.into_iter().map(channel_img).collect(),
    })
}

/// Adds a role or channel made by a whitelisted member to the latest backup, so deleting it before
/// the next backup can't make it unrecoverable. Attackers' creations are never added, or a
/// restore would keep them. Does nothing without a backup.
pub async fn track_created(app: &App, gid: Id<GuildMarker>, action: Action, id: u64) -> Result<()> {
    let mut tx = app.pool.begin().await?;
    let row: Option<(i64, Json<Image>)> = sqlx::query_as(
        "SELECT id, image FROM backups WHERE guild_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1 FOR UPDATE",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((row_id, Json(mut image))) = row else {
        return Ok(());
    };
    if action == Action::RoleCreate {
        let roles = crate::hierarchy::ranked(gid, app.http.roles(gid).await?.models().await?);
        match roles.into_iter().find(|r| r.id.get() == id) {
            Some(r) if !image.roles.iter().any(|x| x.id == id) => image.roles.push(role_img(r)),
            _ => return Ok(()),
        }
    } else {
        let c = app.http.channel(Id::new(id)).await?.model().await?;
        if image.channels.iter().any(|x| x.id == id) {
            return Ok(());
        }
        image.channels.push(channel_img(c));
    }
    sqlx::query("UPDATE backups SET image = $1 WHERE id = $2")
        .bind(Json(&image))
        .bind(row_id)
        .execute(&mut *tx)
        .await?;
    Ok(tx.commit().await?)
}

/// Takes a backup (`by` is None for automatic ones) and returns its 6-char code. Only automatic ones
/// rotate out the oldest past Max Images: a person taking backups never deletes one (that is the delete bar).
pub async fn save(app: &App, gid: Id<GuildMarker>, by: Option<u64>) -> Result<String> {
    let cfg = app.cfg(gid.get()).await?.backups.clone();
    if !cfg.enabled {
        anyhow::bail!("Backups are turned off for this server. Turn on Backups status on the dashboard first.");
    }
    let image = snapshot(&app.http, gid).await?;
    // After the snapshot (seconds of reads), so it also holds for the dashboard's job and the automatic
    // loop: a lock, panic or nuke may have started since the backup was asked for.
    if under_attack(app, gid.get()) || locked_or_panic(app, gid).await? {
        anyhow::bail!("The server is locked, in panic mode or under a nuke, so a backup now would save that state. End it first.");
    }
    // The CDN deletes the icon once it changes, so the restore needs Brim's copy.
    if let Some(h) = image.settings.as_ref().and_then(|s| s.icon.as_deref()) {
        if let Err(e) =
            brim_automod::discord_api::save_icon(&app.pool, app.api.client(), gid.get(), h).await
        {
            tracing::warn!(?e, "backup: saving the server icon failed");
        }
    }
    let messages = crate::msgbackup::copy(app, gid).await?;
    let (code,): (String,) = sqlx::query_as(
        "INSERT INTO backups (guild_id, image, created_by, messages) VALUES ($1, $2, $3, $4) RETURNING code",
    )
    .bind(gid.get() as i64)
    .bind(Json(&image))
    .bind(by.map(|u| u as i64))
    .bind(&messages)
    .fetch_one(&app.pool)
    .await?;
    if by.is_some() {
        return Ok(code);
    }
    sqlx::query(
        "DELETE FROM backups WHERE guild_id = $1 AND id NOT IN \
         (SELECT id FROM backups WHERE guild_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2)",
    )
    .bind(gid.get() as i64)
    .bind(cfg.max_images.max(1))
    .execute(&app.pool)
    .await?;
    Ok(code)
}

/// The newest backup image, without former Staff roles' access ([`Former`]).
pub async fn latest(app: &App, gid: Id<GuildMarker>) -> Result<Option<Image>> {
    let row: Option<(Json<Image>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT image, created_at FROM backups WHERE guild_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    let Some((Json(i), at)) = row else {
        return Ok(None);
    };
    Ok(Some(Former::of(app, gid).await?.image(i, at)))
}

/// Live ids a restore must never delete (`sections::restore_keep`, N2).
async fn keep_set(app: &App, gid: Id<GuildMarker>) -> Result<HashSet<u64>> {
    let g = gid.get() as i64;
    let cfg: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT config FROM guild_config WHERE guild_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(g)
    .fetch_optional(&app.pool)
    .await?;
    let cfg = cfg.map(|c| c.0).unwrap_or_default();
    Ok(brim_core::sections::restore_keep(&app.pool, g, &cfg).await?)
}

/// Panic-mode restore: sync to the latest image and keep the server lock. `None` means there is
/// no backup yet.
pub async fn restore_latest(app: &App, gid: Id<GuildMarker>) -> Result<Option<Summary>> {
    let Some(image) = latest(app, gid).await? else {
        return Ok(None);
    };
    let keep = keep_set(app, gid).await?;
    let filters = app.cfg(gid.get()).await?.backups.clone();
    Ok(Some(
        restore(app, gid, &image, &keep, Some(&filters)).await?,
    ))
}

/// The 10 latest backups as `(code, created_at unix secs, roles, channels, author)`, newest first.
pub async fn list(
    app: &App,
    gid: Id<GuildMarker>,
) -> Result<Vec<(String, i64, i32, i32, Option<i64>)>> {
    Ok(sqlx::query_as(
        "SELECT code, EXTRACT(EPOCH FROM created_at)::BIGINT, jsonb_array_length(image->'roles')::INT, \
         jsonb_array_length(image->'channels')::INT, created_by FROM backups \
         WHERE guild_id = $1 ORDER BY created_at DESC, id DESC LIMIT 10",
    )
    .bind(gid.get() as i64)
    .fetch_all(&app.pool)
    .await?)
}

/// `t!backups ID`: who took the backup, when, and what it holds.
pub async fn info(app: &App, gid: Id<GuildMarker>, id: &str) -> Result<String> {
    let id = id.trim_start_matches('#').to_lowercase();
    type Row = (
        String,
        i64,
        Option<i64>,
        Json<Image>,
        Option<serde_json::Value>,
    );
    let row: Option<Row> =
        sqlx::query_as(
            "SELECT code, EXTRACT(EPOCH FROM created_at)::BIGINT, created_by, image, messages FROM backups \
             WHERE guild_id = $1 AND (code = $2 OR id::text = $2)",
        )
        .bind(gid.get() as i64)
        .bind(&id)
        .fetch_optional(&app.pool)
        .await?;
    let Some((code, at, by, Json(img), messages)) = row else {
        return Ok(format!("No backup `{id}`"));
    };
    Ok(info_text(&code, at, by, &img, messages.as_ref()))
}

fn info_text(
    code: &str,
    at: i64,
    by: Option<i64>,
    img: &Image,
    messages: Option<&serde_json::Value>,
) -> String {
    let by = by.map_or("Brim".into(), |u| format!("<@{u}>"));
    let cats = img.channels.iter().filter(|c| c.kind == CATEGORY).count();
    let mark = |b: bool| if b { YES } else { NO };
    let saved = messages.map_or(String::new(), |m| {
        format!(" · Custom messages: {}", crate::msgbackup::count(m))
    });
    format!(
        "**Backup `{code}`**\nTaken by {by} <t:{at}:R>\nRoles: {} · Channels: {} · Categories: {cats} · Webhooks: {} · Server settings: {}",
        img.roles.len(),
        img.channels.len() - cats,
        img.webhooks.as_ref().map_or(0, Vec::len),
        mark(img.settings.is_some()),
    ) + saved.as_str()
}

/// Deletes the backup with this code (or numeric dashboard id); `None` deletes all of them.
pub async fn delete(app: &App, gid: Id<GuildMarker>, key: Option<&str>) -> Result<bool> {
    let r = sqlx::query(
        "DELETE FROM backups WHERE guild_id = $1 AND ($2::text IS NULL OR code = $2 OR id::text = $2)",
    )
    .bind(gid.get() as i64)
    .bind(key.map(|k| k.trim_start_matches('#').to_lowercase()))
        .execute(&app.pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

/// `/backups load`: without `confirm` only describes what would change; with it, syncs the server
/// to the backup with this code (or numeric dashboard id).
pub async fn load(
    app: &App,
    gid: Id<GuildMarker>,
    id: &str,
    confirm: bool,
    by: u64,
) -> Result<String> {
    let id = id.trim_start_matches('#').to_lowercase();
    let row: Option<(Json<Image>, Option<serde_json::Value>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT image, messages, created_at FROM backups WHERE guild_id = $1 AND (code = $2 OR id::text = $2)",
    )
    .bind(gid.get() as i64)
    .bind(&id)
    .fetch_optional(&app.pool)
    .await?;
    let Some((Json(image), messages, at)) = row else {
        return Ok(format!("No backup #{id}"));
    };
    let image = Former::of(app, gid).await?.image(image, at);
    let keep = keep_set(app, gid).await?;
    if confirm {
        // A restore during a lock or panic would undo the locked permissions.
        if locked_or_panic(app, gid).await? {
            return Ok(
                "Cannot load a backup while the server is locked or in panic mode. End it first."
                    .into(),
            );
        }
        // Before the restore, so its channel moves (`relink`) reach the loaded rows too.
        if let Some(m) = &messages {
            crate::msgbackup::load(app, gid, m, by).await?;
        }
        let sum = restore(app, gid, &image, &keep, None).await;
        let sum = match (sum, &messages) {
            (Err(e), Some(_)) => {
                return Err(e.context("the custom messages were already loaded from the backup"))
            }
            (sum, _) => sum?,
        };
        if messages.is_some() {
            let pause = async {
                let live: HashSet<u64> = app
                    .http
                    .guild_channels(gid)
                    .await?
                    .models()
                    .await?
                    .iter()
                    .map(|c| c.id.get())
                    .collect();
                // Only channels the backup knew and the server lacks now: a thread is in neither
                // list, so it is never taken for a deleted channel.
                let gone: HashSet<u64> = image
                    .channels
                    .iter()
                    .map(|c| c.id)
                    .filter(|c| !live.contains(c))
                    .collect();
                crate::msgbackup::pause_gone(app, gid, &gone).await
            };
            if let Err(e) = pause.await {
                tracing::warn!(?e, "pausing messages whose channel is gone failed");
            }
        }
        let icon = if sum.icon_lost {
            "; the server icon could not be put back, Brim has no copy of it"
        } else {
            ""
        };
        return Ok(format!(
            "Restored backup #{id} ({} changes{icon}){}",
            sum.total(),
            sum.failures().map(|f| format!("\n{f}")).unwrap_or_default()
        ));
    }
    let live = snapshot(&app.http, gid).await?;
    let p = plan(&image, &live, &keep, false);
    let lines = describe(&p, &live);
    if lines.is_empty() {
        return Ok(format!("Backup #{id} already matches the server"));
    }
    let shown = lines.len().min(15);
    let mut text = format!(
        "Loading backup #{id} would change {} things:\n{}",
        lines.len(),
        lines[..shown].join("\n")
    );
    if lines.len() > shown {
        text += format!("\n...and {} more", lines.len() - shown).as_str();
    }
    if let Some(m) = &messages {
        text.push_str(&format!(
            "\nThe custom messages, schedules, triggers, stickies, tags and edited bot messages are replaced by the backup's ({} custom messages).",
            crate::msgbackup::count(m)
        ));
    }
    text += "\nRun again with confirm:true to apply.";
    Ok(text)
}

/// Anti-nuke: recreates the roles and channels with these backup ids (deleted by a nuker), each
/// from the newest image that has it, plus children re-parented to a recreated category. Nothing
/// else is touched.
pub async fn recreate(
    app: &App,
    gid: Id<GuildMarker>,
    ids: &HashSet<u64>,
    filters: Option<&BackupsCfg>,
) -> Result<Recreated> {
    let images: Vec<(Json<Image>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT image, created_at FROM backups WHERE guild_id = $1 ORDER BY created_at DESC, id DESC",
    )
    .bind(gid.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    let main = app.cfg(gid.get()).await?.lockdown.main_roles.clone();
    let staff: Vec<u64> = brim_core::permits::all_staff_roles(&app.pool, gid.get() as i64)
        .await?
        .into_iter()
        .map(|r| r as u64)
        .filter(|r| !main.iter().any(|m| m.get() == *r))
        .collect();
    let former = Former::of(app, gid).await?;
    let images = images.into_iter().map(|(Json(i), at)| former.image(i, at));
    let Some((image, stripped)) = newest_with(images, ids, &staff) else {
        return Ok(Recreated::default());
    };
    let live = snapshot(&app.restore_http, gid).await?;
    let keep: HashSet<u64> = live
        .roles
        .iter()
        .map(|r| r.id)
        .chain(live.channels.iter().map(|c| c.id))
        .collect();
    let mut p = plan(&image, &live, &keep, true);
    p.settings = None;
    p.roles
        .retain(|c| matches!(c, Change::Create(r) if ids.contains(&r.id)));
    // A protected channel Brim already made again counts as back (no second one).
    let mut copies: HashMap<u64, u64> = HashMap::new();
    for &i in ids {
        if let Some(n) = crate::protected::copy_of(i).await {
            copies.insert(i, n);
        }
    }
    p.channels.retain(|c| match c {
        Change::Create(c) => ids.contains(&c.id) && !copies.contains_key(&c.id),
        Change::Update { to, .. } => to.parent.is_some_and(|p| ids.contains(&p)),
        Change::Delete(_) => false,
    });
    if let Some(f) = filters {
        apply_filters(&mut p, f);
    }
    let (sum, mut id_map) = execute(app, gid, (&image, &live), p, true).await;
    for (old, new) in &copies {
        // Back into its category when that was made again (the copy went to the top level).
        let parent = image
            .channels
            .iter()
            .find(|c| c.id == *old)
            .and_then(|c| c.parent);
        if let Some(cat) = parent.and_then(|p| id_map.get(&p)) {
            let moved = app
                .http
                .update_channel(Id::new(*new))
                .parent_id(Some(Id::new(*cat)))
                .await;
            if let Err(e) = moved {
                tracing::warn!(?e, "moving a log channel copy into its category failed");
            }
        }
        id_map.insert(*old, *new);
    }
    crate::relink::relink(app, gid, &id_map, &image).await;
    crate::lockdown::relink_roles(app, gid, &id_map).await;
    crate::protected::lock_guild(app, gid).await;
    let back: HashSet<u64> = ids
        .iter()
        .copied()
        .filter(|i| id_map.contains_key(i))
        .collect();
    Ok(Recreated {
        changes: sum.total(),
        stripped: stripped.intersection(&back).count(),
        back,
    })
}

/// What [`recreate`] did.
#[derive(Debug, Default)]
pub struct Recreated {
    pub changes: usize,
    /// Which of the ids exist again (recreated, or matched to a live object).
    pub back: HashSet<u64>,
    /// Roles and channels that came back from an older image, without their dangerous permissions.
    pub stripped: usize,
}

/// The newest image (first), plus each of `ids` it lacks from the newest older image that has it:
/// a backup taken after the delete (or mid-nuke) must not make the object unrecoverable. A role
/// or channel from an older image loses its dangerous permissions (role permissions, overwrite
/// allows: they may predate staff taking them away); their ids are the second value. A `staff`
/// role (a staff permit role that is not a main role) keeps Mention @everyone, as everywhere.
fn newest_with(
    mut images: impl Iterator<Item = Image>,
    ids: &HashSet<u64>,
    staff: &[u64],
) -> Option<(Image, HashSet<u64>)> {
    let mask = |role: u64| {
        if staff.contains(&role) {
            brim_antinuke::STAFF_DANGEROUS
        } else {
            DANGEROUS_PERMS
        }
    };
    let mut image = images.next()?;
    let mut stripped = HashSet::new();
    for old in images {
        for mut r in old.roles {
            if ids.contains(&r.id) && !image.roles.iter().any(|x| x.id == r.id) {
                if r.permissions & mask(r.id) != 0 {
                    r.permissions &= !mask(r.id);
                    stripped.insert(r.id);
                }
                image.roles.push(r);
            }
        }
        for mut c in old.channels {
            if ids.contains(&c.id) && !image.channels.iter().any(|x| x.id == c.id) {
                for o in &mut c.overwrites {
                    let m = if o.is_role {
                        mask(o.target)
                    } else {
                        DANGEROUS_PERMS
                    };
                    if o.allow & m != 0 {
                        o.allow &= !m;
                        stripped.insert(c.id);
                    }
                }
                image.channels.push(c);
            }
        }
    }
    Some((image, stripped))
}

pub(crate) fn overwrites(list: &[OverwriteImg]) -> Vec<PermissionOverwrite> {
    list.iter()
        .map(|o| PermissionOverwrite {
            id: Id::new(o.target),
            kind: if o.is_role {
                PermissionOverwriteType::Role
            } else {
                PermissionOverwriteType::Member
            },
            allow: Permissions::from_bits_retain(o.allow),
            deny: Permissions::from_bits_retain(o.deny),
        })
        .collect()
}

/// Drops the creates and deletes the restore filters (`anr`) turned off; updates stay unless the
/// whole restore system is off.
fn apply_filters(p: &mut Plan, f: &BackupsCfg) {
    if !f.restore {
        *p = Plan::default();
        return;
    }
    fn keep<T>(c: &Change<T>, create: bool, delete: bool) -> bool {
        match c {
            Change::Create(_) => create,
            Change::Delete(_) => delete,
            Change::Update { .. } => true,
        }
    }
    p.roles.retain(|c| keep(c, f.restore_roles, f.delete_roles));
    p.channels
        .retain(|c| keep(c, f.restore_channels, f.delete_channels));
}

/// Roles that were the Staff role (`permits::former_staff_roles`): an image taken before one
/// stopped being it holds the Staff role's channel access, and the role is no longer guarded, so
/// anyone with Manage Roles could hand that access out. Every read of a backup image for a
/// restore drops those overwrites ([`Former::strip`]); later grants stay.
pub struct Former(Vec<(u64, DateTime<Utc>)>);

impl Former {
    pub async fn of(app: &App, gid: Id<GuildMarker>) -> Result<Self> {
        let rows = brim_core::permits::former_staff_roles(&app.pool, gid.get() as i64).await?;
        Ok(Self(
            rows.into_iter().map(|(r, at)| (r as u64, at)).collect(),
        ))
    }

    /// Drops from `overwrites` (of an image taken at `taken`) the former Staff roles' ones.
    pub fn strip(&self, overwrites: &mut Vec<OverwriteImg>, taken: DateTime<Utc>) {
        overwrites
            .retain(|o| !(o.is_role && self.0.iter().any(|(r, at)| *r == o.target && taken < *at)));
    }

    fn image(&self, mut image: Image, taken: DateTime<Utc>) -> Image {
        for c in &mut image.channels {
            self.strip(&mut c.overwrites, taken);
        }
        image
    }
}

/// Syncs the server to `image` and returns how many changes were made. `filters` is set for panic
/// mode: it applies the restore filters and keeps live role permissions and channel overwrites
/// (panic mode locks the server first). One failed change is logged and skipped, so a single
/// missing permission can't abort the whole restore.
pub async fn restore(
    app: &App,
    gid: Id<GuildMarker>,
    image: &Image,
    keep: &HashSet<u64>,
    filters: Option<&BackupsCfg>,
) -> Result<Summary> {
    let respect_lock = filters.is_some();
    let http = &app.restore_http;
    let live = snapshot(http, gid).await?;
    // The blind lock's notice channel, by the id the lock saved (a nuker can copy its name).
    let mut keep = keep.clone();
    keep.extend(
        brim_automod::store::locks(&app.pool, gid.get())
            .await?
            .iter()
            .filter_map(|s| s.locked_channel),
    );
    let mut p = plan(image, &live, &keep, respect_lock);
    if let Some(f) = filters {
        apply_filters(&mut p, f);
    }
    let (mut sum, id_map) = execute(app, gid, (image, &live), p, respect_lock).await;
    crate::relink::relink(app, gid, &id_map, image).await;
    crate::protected::lock_guild(app, gid).await;
    crate::lockdown::relink_roles(app, gid, &id_map).await;
    // Webhooks that did not exist in the image were made since (old images have no list).
    if let (Some(f), Some(old)) = (filters, &image.webhooks) {
        if f.restore && f.delete_webhooks {
            let mut known: HashSet<u64> = old.iter().map(|w| w.id).collect();
            // The Message Builder's webhooks stay: only they can edit the copies they posted.
            // Unread: delete none, rather than one of them.
            let read = match crate::msghooks::ours(app, gid).await {
                Ok(ours) => {
                    known.extend(ours);
                    true
                }
                Err(e) => {
                    tracing::warn!(
                        ?e,
                        "restore: reading Message Builder webhooks failed, no webhook deleted"
                    );
                    false
                }
            };
            let live_hooks = if read {
                live.webhooks.as_deref().unwrap_or_default()
            } else {
                &[]
            };
            for w in live_hooks.iter().filter(|w| !known.contains(&w.id)) {
                let r = http.delete_webhook(Id::new(w.id)).await.map(drop);
                sum.webhooks_deleted += usize::from(sum.count(r.map_err(Into::into), "webhook"));
            }
        }
    }
    Ok(sum)
}

/// Applies a plan. A failed step is logged and skipped, so one missing permission can't abort it.
async fn execute(
    app: &App,
    gid: Id<GuildMarker>,
    (image, live): (&Image, &Image),
    mut p: Plan,
    respect_lock: bool,
) -> (Summary, HashMap<u64, u64>) {
    let http = &app.restore_http;
    let mut done = Summary::default();
    let live_cats: HashSet<u64> = live
        .channels
        .iter()
        .filter(|c| c.kind == CATEGORY)
        .map(|c| c.id)
        .collect();
    let mut id_map = std::mem::take(&mut p.id_map);
    // (live id, image position) of each created channel: Discord puts it at the bottom.
    let mut made: Vec<Position> = Vec::new();
    let targets: HashSet<u64> = p
        .channels
        .iter()
        .filter_map(|c| match c {
            Change::Create(c) | Change::Update { to: c, .. } => Some(c),
            Change::Delete(_) => None,
        })
        .flat_map(|c| &c.overwrites)
        .filter(|o| !o.is_role)
        .map(|o| o.target)
        .collect();
    let (held, fates) = match member_fates(app, gid, &targets).await {
        Ok(f) => f,
        Err(e) => {
            // Unread: no member overwrite from the image, rather than a nuker's own grant.
            done.count(Err(e), "reading quarantines for member overwrites");
            let none = targets.iter().map(|t| (*t, None)).collect();
            (HashSet::new(), none)
        }
    };

    // A lock or panic that starts mid-restore stops a full load: the rest would write back the
    // permissions the lock just took. Checked before each step, not only at the start.
    let mut halted = false;
    for ch in &p.roles {
        if halt(app, gid, respect_lock, &mut halted, &mut done).await {
            break;
        }
        let r: Result<()> = async {
            match ch {
                Change::Create(r) => {
                    let new = http
                        .create_role(gid)
                        .name(&r.name)
                        .color(r.color)
                        .permissions(Permissions::from_bits_retain(r.permissions))
                        .hoist(r.hoist)
                        .mentionable(r.mentionable)
                        .await?
                        .model()
                        .await?;
                    id_map.insert(r.id, new.id.get());
                }
                Change::Update { live, to } => {
                    http.update_role(gid, Id::new(*live))
                        .name(Some(&to.name))
                        .color(Some(to.color))
                        .permissions(Permissions::from_bits_retain(to.permissions))
                        .hoist(to.hoist)
                        .mentionable(to.mentionable)
                        .await?;
                }
                Change::Delete(id) => {
                    http.delete_role(gid, Id::new(*id)).await?;
                }
            }
            Ok(())
        }
        .await;
        if done.count(r, "role") {
            done.roles.bump(ch);
        }
    }

    for ch in &p.channels {
        if halt(app, gid, respect_lock, &mut halted, &mut done).await {
            break;
        }
        let r: Result<()> = async {
            match ch {
                Change::Create(c) => {
                    let ows = overwrites(&guard_members(
                        map_overwrites(&c.overwrites, &id_map),
                        &fates,
                    ));
                    let parent = c.parent.and_then(|p| id_map.get(&p)).map(|p| Id::new(*p));
                    let mut req = http
                        .create_guild_channel(gid, &c.name)
                        .kind(ChannelType::from(c.kind))
                        .nsfw(c.nsfw)
                        .permission_overwrites(&ows);
                    if let Some(p) = parent {
                        req = req.parent_id(p);
                    }
                    if let Some(t) = &c.topic {
                        req = req.topic(t);
                    }
                    let new = req.await?.model().await?;
                    id_map.insert(c.id, new.id.get());
                    made.push(Position::from((
                        new.id,
                        u64::try_from(c.position).unwrap_or(0),
                    )));
                }
                Change::Update { live: lid, to } => {
                    let mut ows = guard_members(map_overwrites(&to.overwrites, &id_map), &fates);
                    // A quarantined member's live overwrite (the quarantine deny) stays.
                    ows.extend(
                        live.channels
                            .iter()
                            .filter(|c| c.id == *lid)
                            .flat_map(|c| &c.overwrites)
                            .filter(|o| !o.is_role && held.contains(&o.target))
                            .cloned(),
                    );
                    let ows = overwrites(&ows);
                    let parent = to.parent.and_then(|p| id_map.get(&p)).map(|p| Id::new(*p));
                    let mut req = http
                        .update_channel(Id::new(*lid))
                        .name(&to.name)
                        .nsfw(to.nsfw)
                        .parent_id(parent);
                    if !respect_lock {
                        req = req.permission_overwrites(&ows);
                    }
                    if let Some(t) = &to.topic {
                        req = req.topic(t);
                    }
                    req.await?;
                }
                Change::Delete(id) => {
                    http.delete_channel(Id::new(*id)).await?;
                }
            }
            Ok(())
        }
        .await;
        if done.count(r, "channel") {
            let cat = match ch {
                Change::Create(c) => c.kind == CATEGORY,
                Change::Update { to, .. } => to.kind == CATEGORY,
                Change::Delete(id) => live_cats.contains(id),
            };
            if cat {
                &mut done.categories
            } else {
                &mut done.channels
            }
            .bump(ch);
        }
    }

    if !made.is_empty() {
        let r = http.update_guild_channel_positions(gid, &made).await;
        done.count(r.map(drop).map_err(Into::into), "channel positions");
    }

    // A halted load writes no settings or role order over what the lock or revert set.
    if let Some(w) = p.settings.take().filter(|_| !halted) {
        let map = |c: Option<u64>| {
            c.and_then(|c| id_map.get(&c).copied().or(Some(c)))
                .map(Id::new)
        };
        let live_icon = live.settings.as_ref().and_then(|s| s.icon.as_deref());
        let icon = match w.icon.as_deref().filter(|h| Some(*h) != live_icon) {
            Some(h) => match brim_automod::discord_api::stored_icon(
                &app.pool,
                app.api.client(),
                gid.get(),
                h,
            )
            .await
            {
                Ok(uri) => Some(uri),
                Err(e) => {
                    tracing::warn!(?e, "backup restore: no copy of the server icon");
                    done.icon_lost = true;
                    None
                }
            },
            None => None,
        };
        let mut req = http.update_guild(gid);
        if let Some(uri) = &icon {
            req = req.icon(Some(uri));
        }
        let r = req
            .name(&w.name)
            .verification_level(Some(VerificationLevel::from(w.verification)))
            .default_message_notifications(Some(DefaultMessageNotificationLevel::from(
                w.notifications,
            )))
            .explicit_content_filter(Some(ExplicitContentFilter::from(w.content_filter)))
            .afk_channel_id(map(w.afk_channel))
            .afk_timeout(w.afk_timeout)
            .system_channel(map(w.system_channel))
            .preferred_locale(Some(&w.locale))
            .await;
        done.settings = done.count(r.map(drop).map_err(Into::into), "server settings");
    }

    // Recreated roles land at the bottom; put every restored role back in its image order.
    let order: HashMap<u64, i64> = image.roles.iter().map(|r| (r.id, r.position)).collect();
    let positions: Vec<RolePosition> = id_map
        .iter()
        .filter_map(|(img, live)| {
            Some(RolePosition {
                id: Id::new(*live),
                position: u64::try_from(*order.get(img)?).ok()?,
            })
        })
        .collect();
    if !positions.is_empty() && !halted {
        let r = http.update_role_positions(gid, &positions).await;
        done.count(r.map(drop).map_err(Into::into), "role positions");
    }
    (done, id_map)
}

/// What a restore does with a member overwrite of the image, per user: None drops it (quarantined,
/// or no longer in the server), Some(true) keeps it (immune, or Brim), Some(false) keeps it
/// without dangerous allows. Also returns the quarantined user ids.
async fn member_fates(
    app: &App,
    gid: Id<GuildMarker>,
    targets: &HashSet<u64>,
) -> Result<(HashSet<u64>, HashMap<u64, Option<bool>>)> {
    let held: HashSet<u64> =
        sqlx::query_scalar("SELECT user_id FROM quarantines WHERE guild_id = $1")
            .bind(gid.get() as i64)
            .fetch_all(&app.pool)
            .await?
            .into_iter()
            .map(|u: i64| u as u64)
            .collect();
    let mut out = HashMap::new();
    for &t in targets {
        let Some(uid) = Id::new_checked(t) else {
            out.insert(t, None);
            continue;
        };
        let fate = if held.contains(&t) {
            None
        } else if uid == app.me {
            Some(true)
        } else {
            match crate::http_err::member(&app.http, gid, uid).await {
                Ok(None) => None,
                Ok(Some(_)) => Some(app.is_immune(gid, uid).await.unwrap_or(false)),
                Err(e) => {
                    tracing::warn!(
                        ?e,
                        user = t,
                        "restore: member read failed, overwrite kept without dangerous allows"
                    );
                    Some(false)
                }
            }
        };
        out.insert(t, fate);
    }
    Ok((held, out))
}

/// Applies [`member_fates`] to a channel's overwrites; a member missing from `fates` is not immune.
fn guard_members(ows: Vec<OverwriteImg>, fates: &HashMap<u64, Option<bool>>) -> Vec<OverwriteImg> {
    ows.into_iter()
        .filter_map(|mut o| {
            if o.is_role {
                return Some(o);
            }
            match fates.get(&o.target).copied().unwrap_or(Some(false)) {
                None => None,
                Some(immune) => {
                    if !immune {
                        o.allow &= !DANGEROUS_PERMS;
                    }
                    Some(o)
                }
            }
        })
        .collect()
}

/// Successful changes of one object type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub created: usize,
    pub deleted: usize,
    pub updated: usize,
}

impl Counts {
    fn bump<T>(&mut self, c: &Change<T>) {
        match c {
            Change::Create(_) => self.created += 1,
            Change::Delete(_) => self.deleted += 1,
            Change::Update { .. } => self.updated += 1,
        }
    }
}

/// What a restore changed, for the panic report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub roles: Counts,
    pub channels: Counts,
    pub categories: Counts,
    pub webhooks_deleted: usize,
    pub settings: bool,
    /// The backup names a server icon that Brim has no copy of (pruned), so it was not put back.
    pub icon_lost: bool,
    /// Steps Discord refused or that failed, and the first one's reason.
    pub failed: usize,
    pub first_error: Option<String>,
}

impl Summary {
    /// Logs and counts a failed step; true when it went through.
    fn count(&mut self, r: Result<()>, what: &str) -> bool {
        let Err(e) = r else { return true };
        tracing::warn!(?e, what, "restore step failed");
        self.failed += 1;
        self.first_error.get_or_insert_with(|| {
            let why = match crate::http_err::code(&e) {
                Some(50013) => "missing permissions (Manage Roles, Manage Channels or Manage Server, or the role is above Brim's top role)".into(),
                _ => crate::http_err::why(&e),
            };
            format!("{what}: {why}")
        });
        false
    }

    /// "N steps failed: reason" when any step failed.
    pub fn failures(&self) -> Option<String> {
        let why = self.first_error.as_deref()?;
        let n = self.failed;
        Some(format!(
            "{n} step{} failed, first: {why}",
            if n == 1 { "" } else { "s" }
        ))
    }

    pub fn total(&self) -> usize {
        [self.roles, self.channels, self.categories]
            .iter()
            .map(|c| c.created + c.deleted + c.updated)
            .sum::<usize>()
            + self.webhooks_deleted
            + usize::from(self.settings)
    }
}

/// Backs up every guild whose backups setting is on, once its own interval has passed since the
/// last backup.
pub async fn auto_backup(app: std::sync::Arc<App>) {
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        match all_guilds(&app).await {
            Ok(list) => {
                for gid in list {
                    let Ok(cfg) = app.cfg(gid.get()).await else {
                        continue;
                    };
                    if !cfg.backups.auto || !cfg.backups.enabled || under_attack(&app, gid.get()) {
                        continue;
                    }
                    // A lock is saved state too: its overwrites would become the restore source.
                    if !matches!(locked_or_panic(&app, gid).await, Ok(false)) {
                        continue;
                    }
                    match due(&app, gid, cfg.backups.interval_hours).await {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(e) => {
                            tracing::warn!(?e, guild = gid.get(), "backup due check failed");
                            continue;
                        }
                    }
                    if let Err(e) = save(&app, gid, None).await {
                        tracing::warn!(?e, guild = gid.get(), "auto backup failed");
                    }
                }
            }
            Err(e) => tracing::warn!(?e, "listing guilds for backup failed"),
        }
    }
}

/// Every server Brim is in, paged: Discord returns at most 200 per request.
async fn all_guilds(app: &App) -> Result<Vec<Id<GuildMarker>>> {
    let mut out = Vec::new();
    loop {
        let mut req = app.http.current_user_guilds();
        if let Some(last) = out.last() {
            req = req.after(*last);
        }
        let page = req.await?.models().await?;
        let n = page.len();
        out.extend(page.into_iter().map(|g| g.id));
        if n < 200 {
            return Ok(out);
        }
    }
}

/// The server is in panic mode or has a lock on: a restore or a backup delete waits until it ends.
pub(crate) async fn locked_or_panic(app: &App, gid: Id<GuildMarker>) -> Result<bool> {
    Ok(app.panic_runs.lock().unwrap().contains_key(&gid.get())
        || !brim_automod::store::locks(&app.pool, gid.get())
            .await?
            .is_empty())
}

/// True once a full load (`respect_lock` false) must stop because a lock, panic mode or nuke
/// revert started; counts that as one failed step, so the reply and log say the rest was not done.
async fn halt(
    app: &App,
    gid: Id<GuildMarker>,
    respect_lock: bool,
    halted: &mut bool,
    done: &mut Summary,
) -> bool {
    // A nuke revert now runs beside heavy jobs: both recreating the same channels makes doubles.
    let reverting = || {
        app.nuke_runs
            .lock()
            .unwrap()
            .keys()
            .any(|k| k.0 == gid.get())
    };
    if !*halted && !respect_lock && (reverting() || locked_or_panic(app, gid).await.unwrap_or(true))
    {
        *halted = true;
        let e =
            anyhow::anyhow!("a lock, panic mode or nuke revert started, the rest was not restored");
        done.count(Err(e), "restore stopped");
    }
    *halted
}

/// A nuke run or panic mode is on: a backup now would save the damage as the newest image. A lock is
/// checked with it ([`locked_or_panic`]) for every backup.
fn under_attack(app: &App, gid: u64) -> bool {
    app.panic_runs.lock().unwrap().contains_key(&gid)
        || app.nuke_runs.lock().unwrap().keys().any(|k| k.0 == gid)
}

/// True when the newest backup is older than `hours` (or there is none).
async fn due(app: &App, gid: Id<GuildMarker>, hours: u64) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM backups WHERE guild_id = $1 \
         AND created_at > now() - make_interval(hours => $2))",
    )
    .bind(gid.get() as i64)
    .bind(i32::try_from(hours.max(1)).unwrap_or(i32::MAX))
    .fetch_one(&app.pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A role that was the Staff role gets no channel access back from a restore while it is no
    /// level role (it is no longer guarded); while it is the Staff role again, it does.
    #[tokio::test]
    async fn a_restore_gives_a_former_staff_role_no_access() {
        let Some(t) = crate::test_support::T::new(972).await else {
            return;
        };
        let g = t.g(0);
        let gi = g.get() as i64;
        let ow = |target, is_role| OverwriteImg {
            target,
            is_role,
            allow: 1 << 10,
            deny: 0,
        };
        let image = Image {
            channels: vec![ChannelImg {
                id: 5,
                name: "staff-chat".into(),
                kind: 0,
                parent: None,
                position: 0,
                topic: None,
                nsfw: false,
                overwrites: vec![ow(1000, true), ow(11, true), ow(1000, false)],
            }],
            ..Default::default()
        };
        let at = |secs| DateTime::<Utc>::from_timestamp(secs, 0).unwrap();
        let before = at(1_000_000_000);
        brim_core::permits::was_staff_role(&t.app.pool, gi, 1000)
            .await
            .unwrap();
        let former = Former::of(&t.app, g).await.unwrap();
        let got = former.image(image.clone(), before);
        let targets: Vec<(u64, bool)> = got.channels[0]
            .overwrites
            .iter()
            .map(|o| (o.target, o.is_role))
            .collect();
        assert_eq!(
            targets,
            vec![(11, true), (1000, false)],
            "a member overwrite with the same id stays"
        );
        // An image taken after off holds access given later: it stays.
        assert_eq!(former.image(image.clone(), at(4_000_000_000)), image);
        // The Staff role again, or a permit role now: guarded, so it keeps its access.
        sqlx::query(
            "INSERT INTO level_roles (guild_id, level, role_id) VALUES ($1, 'staff', 1000)",
        )
        .bind(gi)
        .execute(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(
            Former::of(&t.app, g)
                .await
                .unwrap()
                .image(image.clone(), before),
            image
        );
        for q in [
            "DELETE FROM level_roles WHERE guild_id = $1",
            "DELETE FROM former_staff_roles WHERE guild_id = $1",
        ] {
            sqlx::query(q).bind(gi).execute(&t.app.pool).await.unwrap();
        }
        t.cleanup().await;
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. A backup during panic, a person's or
    /// an automatic one, is refused before anything is saved or deleted.
    #[tokio::test]
    async fn no_backup_by_hand_during_panic() {
        let Some(t) = crate::test_support::T::new(971).await else {
            return;
        };
        t.server_config(
            0,
            t.owner(),
            serde_json::json!({"backups": {"enabled": true}}),
        )
        .await;
        let g = t.g(0).get();
        t.app.panic_runs.lock().unwrap().insert(
            g,
            crate::app::PanicRun {
                lock_mod_commands: false,
                starting: 0,
            },
        );
        let e = save(&t.app, t.g(0), Some(7)).await.unwrap_err();
        assert!(e.to_string().contains("panic"), "{e}");
        assert!(save(&t.app, t.g(0), None).await.is_err(), "automatic too");
        t.app.panic_runs.lock().unwrap().remove(&g);
        t.cleanup().await;
    }

    /// A panic that starts while a full load runs stops the load before its next step, so the load
    /// cannot write back the permissions panic just locked (found by the 11-18 security pass).
    #[tokio::test]
    async fn a_full_load_stops_when_panic_or_a_nuke_revert_starts() {
        let Some(t) = crate::test_support::T::new(996).await else {
            return;
        };
        t.server_config(0, t.owner(), serde_json::json!({})).await;
        let g = t.g(0).get();
        let image: Image = serde_json::from_value(serde_json::json!({"guild": g, "name": "x",
            "roles": [{"id": 5, "name": "open", "color": 0, "permissions": 2048, "position": 1,
                "hoist": false, "mentionable": false, "managed": false}],
            "channels": []}))
        .unwrap();
        for nuke in [false, true] {
            if nuke {
                t.app.nuke_runs.lock().unwrap().insert((g, 77), (1, false));
            } else {
                let run = crate::app::PanicRun {
                    lock_mod_commands: false,
                    starting: 0,
                };
                t.app.panic_runs.lock().unwrap().insert(g, run);
            }
            let sum = restore(&t.app, t.g(0), &image, &HashSet::new(), None)
                .await
                .unwrap();
            t.app.panic_runs.lock().unwrap().remove(&g);
            t.app.nuke_runs.lock().unwrap().clear();
            let made = t.calls(&format!("POST /guilds/{g}/roles"));
            assert_eq!(made, 0, "no role made (nuke run: {nuke})");
            let why = sum.failures().unwrap_or_default();
            assert!(why.contains("restore stopped"), "{why}");
        }
        t.cleanup().await;
    }

    #[test]
    fn filters_drop_creates_and_deletes_only() {
        let role = RoleImg {
            id: 1,
            name: "a".into(),
            color: 0,
            permissions: 0,
            position: 1,
            hoist: false,
            mentionable: false,
            managed: false,
        };
        let mut p = Plan {
            roles: vec![
                Change::Create(role.clone()),
                Change::Update { live: 1, to: role },
                Change::Delete(2),
            ],
            channels: vec![Change::Delete(3)],
            ..Default::default()
        };
        let f = BackupsCfg {
            restore_roles: false,
            delete_channels: false,
            ..Default::default()
        };
        apply_filters(&mut p, &f);
        assert_eq!(p.roles.len(), 2);
        assert!(matches!(p.roles[0], Change::Update { .. }));
        assert!(p.channels.is_empty());
    }

    #[test]
    fn a_deleted_object_comes_from_the_newest_image_that_has_it() {
        // Administrator (1 << 3) and Send Messages (1 << 11).
        let role = |id, name: &str| RoleImg {
            id,
            name: name.into(),
            color: 0,
            permissions: (1 << 3) | if id == 2 { 1 << 11 } else { 0 },
            position: 1,
            hoist: false,
            mentionable: false,
            managed: false,
        };
        let img = |roles| Image {
            roles,
            ..Default::default()
        };
        // Newest first: role 1 is in the newest image, role 2 only in the older ones.
        let images = vec![
            img(vec![role(1, "new")]),
            img(vec![role(1, "old"), role(2, "mid")]),
            img(vec![role(2, "oldest"), role(3, "not asked")]),
        ];
        let (got, stripped) =
            newest_with(images.clone().into_iter(), &HashSet::from([1, 2]), &[]).unwrap();
        let names: Vec<_> = got.roles.iter().map(|r| (r.id, r.name.as_str())).collect();
        assert_eq!(names, [(1, "new"), (2, "mid")]);
        // Role 2 came from an older image: Administrator is gone, Send Messages stays.
        assert_eq!(got.roles[1].permissions, 1 << 11);
        assert_eq!(stripped, HashSet::from([2]));
        assert_eq!(
            got.roles[0].permissions,
            1 << 3,
            "the newest image keeps its perms"
        );
        // A staff permit role keeps Mention @everyone, and loses the rest.
        let mut images = images;
        images[1].roles[1].permissions = (1 << 17) | (1 << 3);
        let (got, _) = newest_with(images.into_iter(), &HashSet::from([2]), &[2]).unwrap();
        assert_eq!(got.roles[1].permissions, 1 << 17);
        assert!(newest_with(std::iter::empty(), &HashSet::from([1]), &[]).is_none());
        // A channel from an older image loses dangerous overwrite allows, keeps the rest.
        let ow = OverwriteImg {
            target: 9,
            is_role: true,
            allow: (1 << 13) | (1 << 11), // Manage Messages, Send Messages
            deny: 0,
        };
        let chan = ChannelImg {
            id: 5,
            name: "c".into(),
            kind: 0,
            parent: None,
            position: 0,
            topic: None,
            nsfw: false,
            overwrites: vec![ow],
        };
        let older = Image {
            channels: vec![chan],
            ..Default::default()
        };
        let (got, stripped) = newest_with(
            [Image::default(), older].into_iter(),
            &HashSet::from([5]),
            &[],
        )
        .unwrap();
        assert_eq!(got.channels[0].overwrites[0].allow, 1 << 11);
        assert_eq!(stripped, HashSet::from([5]));
    }

    #[test]
    fn a_restore_drops_a_quarantined_members_overwrite_and_strips_the_rest() {
        let ow = |target, is_role| OverwriteImg {
            target,
            is_role,
            allow: (1 << 29) | (1 << 10), // Manage Webhooks, View Channel
            deny: 0,
        };
        // 7 is quarantined, 8 a plain member, 9 immune, 10 not listed (unknown).
        let fates = HashMap::from([(7, None), (8, Some(false)), (9, Some(true))]);
        let got = guard_members(
            vec![
                ow(1, true),
                ow(7, false),
                ow(8, false),
                ow(9, false),
                ow(10, false),
            ],
            &fates,
        );
        let view = 1 << 10;
        assert_eq!(
            got.iter().map(|o| (o.target, o.allow)).collect::<Vec<_>>(),
            [
                (1, (1 << 29) | view),
                (8, view),
                (9, (1 << 29) | view),
                (10, view)
            ]
        );
    }

    #[test]
    fn info_counts_categories_apart() {
        let ch = |id, kind| ChannelImg {
            id,
            name: "c".into(),
            kind,
            parent: None,
            position: 0,
            topic: None,
            nsfw: false,
            overwrites: vec![],
        };
        let img = Image {
            channels: vec![ch(1, 4), ch(2, 0), ch(3, 2)],
            webhooks: Some(vec![]),
            ..Default::default()
        };
        let t = info_text("abc123", 10, None, &img, None);
        assert!(t.contains("Backup `abc123`") && t.contains("Taken by Brim <t:10:R>"));
        assert!(
            t.contains("Roles: 0 · Channels: 2 · Categories: 1 · Webhooks: 0 · Server settings: ")
        );
    }

    /// D10: load and clear need owner level plus a fresh 2FA, and load is refused in panic.
    #[tokio::test]
    async fn load_and_clear_need_an_owner_with_2fa_and_no_panic() {
        use crate::{commands::backup_cmd, slash::BackupCmd, test_support::T};
        use brim_core::permits::Level;
        let Some(t) = T::new(200).await else { return };
        let (g, user) = (t.g(0), Id::new(t.base + 1));
        t.server_config(
            0,
            t.owner(),
            serde_json::json!({"backups": {"enabled": true}}),
        )
        .await;
        t.exec("DELETE FROM backups WHERE guild_id = $1").await;
        t.exec(r#"INSERT INTO backups (guild_id, image, code) VALUES ($1, '{"guild": 0, "name": "x", "roles": [], "channels": []}', 'h3load')"#).await;
        let custom = Level::Custom {
            rank: 1,
            perms: vec!["backups".into()],
            whitelist: vec![],
            limits: Default::default(),
        };
        let clear = || BackupCmd::Clear;
        let load = |confirm| BackupCmd::Load {
            id: "h3load".into(),
            confirm,
        };
        let run = |l: Level, c| {
            let app = &t.app;
            async move { backup_cmd(app, g, (user, &l), c).await.unwrap().text }
        };
        assert!(run(custom.clone(), clear())
            .await
            .starts_with("Only the owner"));
        assert!(run(custom.clone(), load(true))
            .await
            .starts_with("Only the owner"));
        assert!(run(Level::ExtraOwner, clear()).await.contains("needs 2FA"));
        t.exec("INSERT INTO dash_session (id, user_id, username, access_token, csrf, step_up_until, expires_at, step_up_guild) VALUES ('h3-' || $1::text, $1 + 1, 'o', 'x', 'x', now() + interval '5 minutes', now() + interval '1 day', $1)").await;
        t.app.panic_runs.lock().unwrap().insert(
            g.get(),
            crate::app::PanicRun {
                lock_mod_commands: false,
                starting: 0,
            },
        );
        assert!(run(Level::ExtraOwner, load(true))
            .await
            .starts_with("Cannot load a backup while"));
        // A backup delete is a clear one at a time (S-h3 2): the same bar.
        let delete = || BackupCmd::Delete {
            id: "h3load".into(),
        };
        assert!(run(Level::ExtraOwner, delete())
            .await
            .starts_with("Cannot delete backups while"));
        assert!(run(Level::ExtraOwner, clear())
            .await
            .starts_with("Cannot delete backups while"));
        t.app.panic_runs.lock().unwrap().clear();
        assert!(run(custom.clone(), delete())
            .await
            .starts_with("Only the owner"));
        t.exec("UPDATE dash_session SET step_up_until = now() - interval '1 minute' WHERE id = 'h3-' || $1::text").await;
        assert!(run(Level::Owner, delete()).await.contains("needs 2FA"));
        // security review: a step-up done for another server does not count here.
        t.exec("UPDATE dash_session SET step_up_until = now() + interval '5 minutes', step_up_guild = $1 + 1 WHERE id = 'h3-' || $1::text").await;
        assert!(run(Level::Owner, delete()).await.contains("needs 2FA"));
        t.exec("UPDATE dash_session SET step_up_guild = $1 WHERE id = 'h3-' || $1::text")
            .await;
        assert_eq!(run(Level::Owner, delete()).await, "Deleted backup `h3load`");
        assert_eq!(run(Level::Owner, clear()).await, "Deleted all backups");
        t.exec("DELETE FROM backups WHERE guild_id = $1").await;
    }
}

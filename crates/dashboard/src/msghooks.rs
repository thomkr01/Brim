//! Message Builder phase 4: sending as a webhook. One Brim webhook per channel ("Brim
//! messages"), made on first use; each message gives its own name and picture per send. The
//! webhook token stays in `message_webhooks` and never leaves the server.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    ApiError, AppState,
};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use reqwest::Method;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Upload size, stored pictures per server, and the size a picture is cut down to.
const MAX_BYTES: usize = 1 << 20;
const MAX_STORED: i64 = 50;
const SIDE: u32 = 512;
const MAX_PIXELS: u64 = 16_000_000;
const HOOK_NAME: &str = "Brim messages";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/guilds/{guild}/embeds/media",
            put(upload).layer(DefaultBodyLimit::max(MAX_BYTES + 1024)),
        )
        .route("/media/{file}", get(serve))
}

/// The dashboard's own address, where uploads are served.
pub(crate) fn base(st: &AppState) -> &str {
    st.discord.redirect_uri.trim_end_matches("/auth/callback")
}

/// The address Discord fetches an uploaded picture from.
fn media_url(st: &AppState, token: &str) -> String {
    format!("{}/media/{token}.png", base(st))
}

/// Checks a message's "Send as" value: `{"name", "avatar"?}` or null. Returns the cleaned value.
/// `own`: the dashboard's address; its uploads count as fine even without https (a local setup).
pub(crate) fn clean(v: &Value, own: &str) -> Result<Value, String> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    let name = v["name"].as_str().unwrap_or("").trim();
    let n = name.chars().count();
    if n == 0 || n > 80 {
        return Err("a webhook name is 1 to 80 characters".into());
    }
    // Discord refuses these in a webhook name.
    let low = name.to_lowercase();
    if low.contains("discord") || low.contains("clyde") {
        return Err("Discord does not allow \"discord\" or \"clyde\" in a webhook name".into());
    }
    let avatar = match v["avatar"]
        .as_str()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        None => Value::Null,
        Some(a)
            if a.len() <= 2000
                && (a.strip_prefix("https://").is_some_and(|r| !r.is_empty())
                    || a.strip_prefix(own)
                        .is_some_and(|r| r.starts_with("/media/"))) =>
        {
            json!(a)
        }
        Some(_) => return Err("the webhook picture must be an https link".into()),
    };
    Ok(json!({"name": name, "avatar": avatar}))
}

/// Parses an upload as a real image and encodes it again as a PNG of at most 512 x 512 (first
/// frame only), which drops metadata and anything appended to the file. The limits stop a small
/// file that claims a huge size before it allocates.
fn reencode(b: &[u8]) -> Result<Vec<u8>, String> {
    use image::{ImageFormat, ImageReader};
    use std::io::Cursor;
    let only = "only PNG, JPEG, GIF and WebP images can be used";
    let fmt = image::guess_format(b).map_err(|_| only.to_string())?;
    if !matches!(
        fmt,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(only.into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 << 20);
    let reader = || {
        let mut r = ImageReader::with_format(Cursor::new(b), fmt);
        r.limits(limits.clone());
        r
    };
    let unreadable = |_| "the image could not be read".to_string();
    let (w, h) = reader().into_dimensions().map_err(unreadable)?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("the image has more than 16 megapixels".into());
    }
    let mut img = reader().decode().map_err(unreadable)?;
    if w > SIDE || h > SIDE {
        img = img.thumbnail(SIDE, SIDE);
    }
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|_| "the image could not be read".to_string())?;
    Ok(out)
}

static DECODE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
/// /media needs no login, so only a few reads of it run at the same time.
static SERVE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// Upload a webhook picture (Trusted Admins and up). Returns the link to put in "Send as".
async fn upload(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>, body: Body) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    // Read only after the level check, so nobody else can make the server hold the bytes.
    let body = axum::body::to_bytes(body, MAX_BYTES)
        .await
        .map_err(|_| ApiError::BadRequest("the picture is larger than 1 MB".into()))?;
    let slot = DECODE.acquire().await.map_err(anyhow::Error::from)?;
    let png = tokio::task::spawn_blocking(move || (reencode(&body), drop(slot)).0)
        .await
        .map_err(anyhow::Error::from)?
        .map_err(ApiError::BadRequest)?;
    let sha: String = Sha256::digest(&png)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut tx = st.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('media'), $1::int)")
        .bind((guild % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    gc(&mut tx, guild).await?;
    let stored: i64 = sqlx::query_scalar("select count(*) from media where guild_id = $1")
        .bind(guild)
        .fetch_one(&mut *tx)
        .await?;
    let known: bool = sqlx::query_scalar(
        "select exists(select 1 from media where guild_id = $1 and sha256 = $2)",
    )
    .bind(guild)
    .bind(&sha)
    .fetch_one(&mut *tx)
    .await?;
    if !known && stored >= MAX_STORED {
        return Err(ApiError::BadRequest(format!(
            "this server already has {MAX_STORED} uploaded pictures: remove a picture from a message that no longer needs it"
        )));
    }
    // A random link, not the hash: nobody can test whether a server uploaded a picture they also
    // hold (security review). The same picture again in the same server keeps its link.
    let token: String = sqlx::query_scalar("insert into media (guild_id, sha256, png, added_by, token) values ($1, $2, $3, $4, $5) on conflict (guild_id, sha256) do update set added_at = now() returning token")
        .bind(guild)
        .bind(&sha)
        .bind(&png)
        .bind(s.user_id)
        .bind(crate::auth::token())
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"url": media_url(&st, &token)})))
}

/// Drops this server's uploads that no saved message uses. A fresh upload gets a day to be saved
/// into a message first.
pub(crate) async fn gc(c: &mut sqlx::PgConnection, guild: i64) -> Result<(), ApiError> {
    sqlx::query(
        "delete from media m where guild_id = $1 and added_at < now() - interval '1 day'
         and not exists (select 1 from embeds e where e.guild_id = $1
                         and strpos(e.webhook->>'avatar', '/media/' || m.token || '.png') > 0)",
    )
    .bind(guild)
    .execute(c)
    .await?;
    Ok(())
}

/// A stored picture, for Discord to fetch: a fixed PNG type, never sniffed, no cookies needed.
async fn serve(State(st): State<AppState>, Path(file): Path<String>) -> Response {
    // A random upload link (43 URL-safe base64 characters), or the hash link of an older upload.
    let Some(token) = file.strip_suffix(".png").filter(|s| {
        (s.len() == 43 || s.len() == 64)
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(Ok(_slot)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), SERVE.acquire()).await
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let png: Option<Vec<u8>> = sqlx::query_scalar("select png from media where token = $1 limit 1")
        .bind(token)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    match png {
        Some(b) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
                (
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=86400"),
                ),
                (
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("default-src 'none'; sandbox"),
                ),
            ],
            b,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Brim's webhook in `channel`, made on first use: (webhook id, token). `stale`: a webhook
/// Discord says is gone, so a new one is made in its place. A per-channel lock stops two first
/// sends at the same time from making two webhooks.
async fn hook(
    st: &AppState,
    guild: i64,
    channel: i64,
    stale: Option<i64>,
) -> Result<(i64, String), ApiError> {
    let mut tx = st.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('message_webhook'), $1::int)")
        .bind((channel % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    let row: Option<(i64, String)> = sqlx::query_as(
        "select webhook_id, token from message_webhooks where channel_id = $1 and guild_id = $2",
    )
    .bind(channel)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(r) = row.filter(|r| Some(r.0) != stale) {
        return Ok(r);
    }
    let r = st
        .discord
        .send(
            Method::POST,
            &format!("/channels/{channel}/webhooks"),
            Some(json!({"name": HOOK_NAME})),
        )
        .await?;
    match r.status().as_u16() {
        200..=299 => {}
        403 => {
            return Err(ApiError::BadRequest(
                "Brim needs Manage Webhooks in that channel to send as a webhook".into(),
            ))
        }
        s => {
            let v: Value = r.json().await.unwrap_or_default();
            return Err(ApiError::BadRequest(format!(
                "Discord refused the webhook ({s}): {}",
                v["message"].as_str().unwrap_or("")
            )));
        }
    }
    let v: Value = r.json().await.unwrap_or_default();
    let (Some(id), Some(token)) = (
        v["id"].as_str().and_then(|i| i.parse::<i64>().ok()),
        v["token"].as_str(),
    ) else {
        return Err(anyhow::anyhow!("Discord made a webhook without an id or token").into());
    };
    sqlx::query(
        "insert into message_webhooks (channel_id, guild_id, webhook_id, token) values ($1, $2, $3, $4)
         on conflict (channel_id) do update set guild_id = $2, webhook_id = $3, token = $4, last_used = now()",
    )
    .bind(channel)
    .bind(guild)
    .bind(id)
    .bind(token)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((id, token.to_string()))
}

/// The token of a webhook this server's copies went through.
async fn token(st: &AppState, guild: i64, webhook: i64) -> Result<Option<String>, ApiError> {
    Ok(sqlx::query_scalar(
        "select token from message_webhooks where webhook_id = $1 and guild_id = $2",
    )
    .bind(webhook)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?)
}

/// Discord's answer with readable errors; `None` for 404 (the webhook or the message is gone).
async fn answer(r: reqwest::Response) -> Result<Option<Value>, ApiError> {
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or_default();
    match status {
        200..=299 => Ok(Some(v)),
        404 => Ok(None),
        400 => Err(ApiError::BadRequest(format!(
            "Discord refused the message: {v}"
        ))),
        _ => Err(anyhow::anyhow!("discord {status}: {}", v["message"]).into()),
    }
}

/// Posts `body` in `channel` through Brim's webhook there, as `who`. Returns (message id,
/// webhook id). A webhook someone deleted is made again once.
pub(crate) async fn post(
    st: &AppState,
    guild: i64,
    channel: i64,
    who: &Value,
    mut body: Value,
) -> Result<(i64, i64, Value), ApiError> {
    body["username"] = who["name"].clone();
    if let Some(a) = who["avatar"].as_str() {
        body["avatar_url"] = json!(a);
    }
    let mut stale = None;
    for _ in 0..2 {
        let (id, tok) = hook(st, guild, channel, stale).await?;
        stale = Some(id);
        // with_components: without it Discord drops the message's buttons.
        let r = st
            .discord
            .send(
                Method::POST,
                &format!("/webhooks/{id}/{tok}?wait=true&with_components=true"),
                Some(body.clone()),
            )
            .await?;
        if r.status() == 404 {
            let v: Value = r.json().await.unwrap_or_default();
            if v["code"].as_u64() != Some(UNKNOWN_WEBHOOK) {
                break;
            }
            // Deleted in Discord (the bot notices it from the audit log too): make it again.
            mark_gone(st, guild, id).await?;
            continue;
        }
        if let Some(v) = answer(r).await? {
            sqlx::query(
                "update message_webhooks set last_used = now() where webhook_id = $1 and guild_id = $2",
            )
            .bind(id)
            .bind(guild)
                .execute(&st.db)
                .await?;
            let m = v["id"]
                .as_str()
                .and_then(|m| m.parse().ok())
                .ok_or_else(|| ApiError::from(anyhow::anyhow!("Discord gave no message id")))?;
            return Ok((m, id, v));
        }
    }
    Err(ApiError::BadRequest("that channel no longer exists".into()))
}

/// Discord's error code for "Unknown Webhook".
const UNKNOWN_WEBHOOK: u64 = 10015;

/// A webhook found gone (deleted in Discord): its copies still up are marked, so the Sent log and
/// Overview say they can no longer be edited, and its row goes so the next send makes a new one.
/// The bot marks them too, with who deleted it, when the audit log entry comes first.
async fn mark_gone(st: &AppState, guild: i64, webhook: i64) -> Result<(), ApiError> {
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "update message_sends set hook_gone_at = now() where webhook_id = $1 and guild_id = $2
         and posted_id is not null and hook_gone_at is null",
    )
    .bind(webhook)
    .bind(guild)
    .execute(&mut *tx)
    .await?;
    sqlx::query("delete from message_webhooks where webhook_id = $1 and guild_id = $2")
        .bind(webhook)
        .bind(guild)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Overview: one warning per message with copies that can no longer be edited because their
/// webhook was deleted in Discord. Clears once the message is sent again (or deleted).
pub(crate) async fn overview_check(
    st: &AppState,
    guild: i64,
    chans: &[brim_antinuke::ChannelImg],
) -> Result<Vec<brim_antinuke::security::Check>, ApiError> {
    use brim_antinuke::security::{steps, Check, Severity};
    let rows: Vec<(i64, String, i64, i64, Option<i64>)> = sqlx::query_as(
        "select e.id, e.name, min(s.channel_id), count(*), max(s.hook_gone_by) from message_sends s
         join embeds e on e.id = s.embed_id
         where s.guild_id = $1 and s.hook_gone_at is not null and s.posted_id is not null and s.source <> 'sticky'
         and not exists (select 1 from message_sends n where n.embed_id = s.embed_id
                         and n.posted_id is not null and n.sent_at > s.hook_gone_at)
         group by e.id, e.name order by e.name limit 10",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let mut out = Vec::new();
    for (id, name, channel, n, by) in rows {
        let at = chans.iter().find(|c| c.id as i64 == channel);
        // A failed lookup only loses the name.
        let user = match by {
            Some(u) => st.discord.user(u).await.ok().flatten(),
            None => None,
        };
        let who = user
            .as_ref()
            .and_then(|u| u["global_name"].as_str().or(u["username"].as_str()))
            .map_or("Someone".to_string(), |n| format!("@{n}"));
        let copies = if n == 1 {
            "copy".to_string()
        } else {
            format!("{n} copies")
        };
        let detail = match at {
            Some(c) => format!(
                "{who} deleted Brim's webhook in #{}, so the sent {copies} of “{name}” can no longer be edited. Open the message and click Send again here in its Sent log to post a new copy with the same settings, then delete the old copy in Discord.",
                c.name
            ),
            None => format!(
                "{who} deleted Brim's webhook for “{name}”, and the channel it was in is deleted too. Open the message, pick a new channel on its Send tab, then click Send."
            ),
        };
        out.push(Check {
            id: format!("message_webhook_gone_{id}"),
            severity: Severity::Warn,
            title: "Message Builder webhook deleted",
            detail,
            fix: None,
            steps: steps("message_webhook_gone"),
        });
    }
    Ok(out)
}

/// Edits a copy a webhook posted, with Discord's answer. `None`: the copy is gone. Only its webhook can edit it, so a
/// webhook that is gone (deleted in Discord) is an error and the copy stays in the log.
pub(crate) async fn edit(
    st: &AppState,
    guild: i64,
    webhook: i64,
    message: i64,
    body: Value,
) -> Result<Option<Value>, ApiError> {
    let gone = || {
        ApiError::BadRequest(
            "Brim's webhook for that copy was deleted in Discord, so the copy cannot be edited: send a new one and delete the old copy in Discord".into(),
        )
    };
    let Some(tok) = token(st, guild, webhook).await? else {
        mark_gone(st, guild, webhook).await?;
        return Err(gone());
    };
    let r = st
        .discord
        .send(
            Method::PATCH,
            &format!("/webhooks/{webhook}/{tok}/messages/{message}?with_components=true"),
            Some(body),
        )
        .await?;
    if r.status() == 404 {
        let v: Value = r.json().await.unwrap_or_default();
        return match v["code"].as_u64() {
            Some(UNKNOWN_WEBHOOK) => {
                mark_gone(st, guild, webhook).await?;
                Err(gone())
            }
            _ => Ok(None),
        };
    }
    answer(r).await
}

/// Deletes a copy a webhook posted; a copy already gone counts as done. When the webhook itself
/// is gone, Brim deletes the copy as a bot instead (this needs Manage Messages there).
pub(crate) async fn delete(
    st: &AppState,
    guild: i64,
    webhook: i64,
    channel: i64,
    message: i64,
) -> Result<(), ApiError> {
    if let Some(tok) = token(st, guild, webhook).await? {
        let r = st
            .discord
            .send(
                Method::DELETE,
                &format!("/webhooks/{webhook}/{tok}/messages/{message}"),
                None,
            )
            .await?;
        match r.status().as_u16() {
            200..=299 => return Ok(()),
            404 => {
                let v: Value = r.json().await.unwrap_or_default();
                if v["code"].as_u64() != Some(UNKNOWN_WEBHOOK) {
                    return Ok(());
                }
            }
            s => return Err(anyhow::anyhow!("discord {s} deleting a webhook copy").into()),
        }
    }
    let r = st
        .discord
        .send(
            Method::DELETE,
            &format!("/channels/{channel}/messages/{message}"),
            None,
        )
        .await?;
    match r.status().as_u16() {
        200..=299 | 404 => Ok(()),
        s => Err(anyhow::anyhow!("discord {s} deleting a webhook copy as the bot").into()),
    }
}

/// After a message is deleted: Brim's webhook in a channel that no saved message sends to
/// as a webhook any more goes too, unless copies it posted are still up (only it can edit them)
/// or a feed posts there.
pub(crate) async fn forget_unused(st: &AppState, guild: i64) -> Result<(), ApiError> {
    let unused: Vec<(i64, i64)> = sqlx::query_as(
        "select channel_id, webhook_id from message_webhooks w where guild_id = $1
         and not exists (select 1 from embeds e where e.guild_id = $1 and e.webhook is not null
                         and e.channel_id = w.channel_id)
         and not exists (select 1 from message_sends m where m.webhook_id = w.webhook_id
                         and m.posted_id is not null)
         and not exists (select 1 from feeds f where f.channel_id = w.channel_id)",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    for (channel, id) in unused {
        let r = st
            .discord
            .send(Method::DELETE, &format!("/webhooks/{id}"), None)
            .await;
        if r.as_ref()
            .is_ok_and(|r| r.status().is_success() || r.status() == 404)
        {
            sqlx::query("delete from message_webhooks where channel_id = $1 and webhook_id = $2")
                .bind(channel)
                .bind(id)
                .execute(&st.db)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// security review: a picture is served by its random link (and an older one by its hash
    /// link), and a saved message that uses the link keeps it from the cleanup.
    #[tokio::test]
    async fn pictures_are_served_by_their_link_and_kept_while_used() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_490_000_000).await else {
            return;
        };
        let db = st.db.clone();
        let tok = crate::auth::token();
        for (sha, token) in [
            ("a".repeat(64), tok.as_str()),
            ("b".repeat(64), "c".repeat(64).as_str()),
        ] {
            sqlx::query("insert into media (guild_id, sha256, png, added_by, token, added_at) values ($1, $2, '\\x00', 7, $3, now() - interval '2 days')")
                .bind(g).bind(sha).bind(token).execute(&db).await.unwrap();
        }
        sqlx::query("insert into embeds (guild_id, name, message, updated_by, webhook) values ($1, 'n', '{}', 7, $2)")
            .bind(g)
            .bind(serde_json::json!({"name": "a", "avatar": format!("https://x/media/{tok}.png")}))
            .execute(&db)
            .await
            .unwrap();
        let mut c = db.acquire().await.unwrap();
        super::gc(&mut c, g).await.unwrap();
        let left: Vec<String> = sqlx::query_scalar("select token from media where guild_id = $1")
            .bind(g)
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(
            left,
            vec![tok.clone()],
            "the used one stays, the unused one goes"
        );
        let status = |f: String| {
            let st = st.clone();
            async move {
                super::serve(axum::extract::State(st), axum::extract::Path(f))
                    .await
                    .status()
            }
        };
        assert_eq!(status(format!("{tok}.png")).await, 200);
        assert_eq!(
            status(format!("{}.png", "a".repeat(64))).await,
            404,
            "not by its hash"
        );
        // An upload from before migration 4032 got its hash as its link, so posted messages keep it.
        let old = "d".repeat(64);
        sqlx::query("insert into media (guild_id, sha256, png, added_by, token) values ($1, $2, '\\x00', 7, $2)")
            .bind(g)
            .bind(&old)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(
            status(format!("{old}.png")).await,
            200,
            "an old hash link still works"
        );
        for t in ["media", "embeds"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }

    use super::*;

    const O: &str = "http://127.0.0.1:8080";
    fn clean_t(v: &Value) -> Result<Value, String> {
        clean(v, O)
    }

    #[test]
    fn checks_the_send_as_value() {
        assert!(
            clean_t(&json!({"name": "a", "avatar": "http://127.0.0.1:8080/media/ab.png"})).is_ok()
        );
        assert!(clean_t(
            &json!({"name": "a", "avatar": "http://127.0.0.1:8080.evil/media/ab.png"})
        )
        .is_err());
        assert_eq!(clean(&Value::Null, O), Ok(Value::Null));
        assert_eq!(
            clean_t(&json!({"name": " News ", "avatar": ""})),
            Ok(json!({"name": "News", "avatar": null}))
        );
        assert!(clean_t(&json!({"name": ""})).is_err());
        assert!(clean_t(&json!({"name": "x".repeat(81)})).is_err());
        assert!(clean_t(&json!({"name": "My Discord bot"})).is_err());
        assert!(clean_t(&json!({"name": "a", "avatar": "http://x/a.png"})).is_err());
        assert!(clean_t(&json!({"name": "a", "avatar": "javascript:alert(1)"})).is_err());
        assert!(clean_t(&json!({"name": "a", "avatar": "https://cdn.example/a.png"})).is_ok());
    }

    /// A deleted webhook marks its copies, Edit message refuses them, Overview warns until the
    /// message is sent again.
    #[tokio::test]
    async fn a_deleted_webhook_marks_its_copies_until_sent_again() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_400_000).await else {
            return;
        };
        let e: i64 = sqlx::query_scalar(
            "insert into embeds (guild_id, name, message, updated_by) values ($1, 'News', '{}', 7) returning id",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        sqlx::query("insert into message_webhooks (channel_id, guild_id, webhook_id, token) values (55, $1, 66, 't')")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        let send = |at: &'static str| {
            sqlx::query(
                "insert into message_sends (embed_id, guild_id, channel_id, posted_id, body, sent_by, detail, webhook_id, sent_at)
                 values ($1, $2, 55, 77, '{}', 7, 'x', 66, now() + $3::interval)",
            )
            .bind(e)
            .bind(g)
            .bind(at)
        };
        send("-1 hour").execute(&st.db).await.unwrap();
        assert!(overview_check(&st, g, &[]).await.unwrap().is_empty());
        mark_gone(&st, g, 66).await.unwrap();
        let c = overview_check(&st, g, &[]).await.unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, format!("message_webhook_gone_{e}"));
        assert!(
            c[0].detail.contains("“News”") && c[0].detail.contains("pick a new channel"),
            "{}",
            c[0].detail
        );
        let left: i64 =
            sqlx::query_scalar("select count(*) from message_webhooks where webhook_id = 66")
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert_eq!(left, 0);
        // No token any more: Edit message says the webhook is gone, without calling Discord.
        assert!(edit(&st, g, 66, 77, json!({})).await.is_err());
        // Sent again after the delete: the warning clears.
        send("1 minute").execute(&st.db).await.unwrap();
        assert!(overview_check(&st, g, &[]).await.unwrap().is_empty());
        sqlx::query("delete from embeds where guild_id = $1")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
    }

    #[test]
    fn reencodes_pictures_small_and_refuses_others() {
        let mut big = Vec::new();
        image::RgbaImage::from_pixel(1024, 600, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut big), image::ImageFormat::Png)
            .unwrap();
        let out = reencode(&big).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (512, 300));
        assert!(reencode(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(reencode(b"GIF89a broken").is_err());
    }
}

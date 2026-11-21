//! The bot's own member profile in one server (nickname, bio, avatar, banner), changed only through
//! Discord's `PATCH /guilds/{g}/members/@me`, never the global `/users/@me`. Owners and Extra Owners
//! (Owner level: an Extra Owner can do all the owner can), with fresh 2FA for a change.
//! Also the bot's global status and activity, which
//! only the bot application's owner may set (Discord has no per-server presence).
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    update::is_bot_owner,
    ApiError, AppState,
};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Discord's limits for per-server profiles (support article 4409388345495): avatar 8 MB, banner
/// 10 MB, banner at least 600x240. The avatar minimum is Discord's 128x128.
const AVATAR_MAX: usize = 8 << 20;
const BANNER_MAX: usize = 10 << 20;
const AVATAR_MIN: (u32, u32) = (128, 128);
const BANNER_MIN: (u32, u32) = (600, 240);
/// Brim's own cap, so one decode stays under `max_alloc` (16 MP RGBA = 64 MB).
const MAX_PIXELS: u64 = 16_000_000;
const NICK_MAX: usize = 32;
/// Discord's profile editor stops at 190; the API took 191 in the live check.
const BIO_MAX: usize = 190;
const ACTIVITY_MAX: usize = 128;
/// Profile changes per server and hour, so a loop cannot drain the bot's REST budget.
const HOURLY: i64 = 30;
/// A change still `reserved` after this belongs to a process that died before Discord answered.
const LEASE_SECS: f64 = 180.0;

pub fn routes() -> Router<AppState> {
    let p = "/api/guilds/{guild}/bot-profile";
    Router::new()
        .route(p, get(show))
        .route(
            &format!("{p}/{{field}}"),
            axum::routing::put(set)
                .delete(reset)
                .layer(DefaultBodyLimit::max(BANNER_MAX + 1024)),
        )
        .route("/api/bot-presence", axum::routing::put(set_presence))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    Nick,
    Bio,
    Avatar,
    Banner,
}

impl Field {
    fn parse(s: &str) -> Result<Self, ApiError> {
        Ok(match s {
            "nick" => Field::Nick,
            "bio" => Field::Bio,
            "avatar" => Field::Avatar,
            "banner" => Field::Banner,
            _ => return Err(ApiError::NotFound),
        })
    }
    fn key(self) -> &'static str {
        match self {
            Field::Nick => "nick",
            Field::Bio => "bio",
            Field::Avatar => "avatar",
            Field::Banner => "banner",
        }
    }
    fn image(self) -> bool {
        matches!(self, Field::Avatar | Field::Banner)
    }
}

/// Owner level (the owner or an Extra Owner, resolved fresh on every request) and 2FA for writes.
async fn guard(st: &AppState, s: &Sess, guild: i64, write: bool) -> Result<(), ApiError> {
    api::require(st, s, guild, Level::Owner, write)
        .await
        .map(drop)
}

/// A Discord image hash, so a URL is only ever built from what Discord sent.
fn cdn(path: String, hash: &Value) -> Value {
    match hash.as_str().filter(|h| {
        !h.is_empty() && h.len() <= 64 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) {
        Some(h) => json!(format!(
            "https://cdn.discordapp.com/{path}/{h}.png?size=512"
        )),
        None => Value::Null,
    }
}

async fn show(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    guard(&st, &s, guild, false).await?;
    // Here too, or a slot left by a restart would keep every button off for good.
    sweep(&st, guild).await?;
    let bot = st.discord.bot_user().await?;
    let r = st
        .discord
        .send(Method::GET, &format!("/guilds/{guild}/members/{bot}"), None)
        .await?;
    if r.status() == 404 {
        return Err(ApiError::NotFound);
    }
    let m: Value = r.error_for_status()?.json().await?;
    // Discord's member object has no bio. Only the bot can change its own, so the last change it
    // made counts, unless the bot left and came back since (a rejoin starts a blank profile). After
    // a change with no answer the bio is unknown (null).
    let joined = m["joined_at"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
    let last_bio: Option<(Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "select case when op = 'set' and status = 'succeeded' then value end, created_at
         from bot_profile_ops where guild_id = $1 and field = 'bio'
         and status in ('succeeded', 'unknown') order by id desc limit 1",
    )
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let bio = last_bio
        .filter(|(_, at)| joined.is_none_or(|j| *at > j))
        .and_then(|(v, _)| v);
    type Op = (
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        i64,
        chrono::DateTime<chrono::Utc>,
    );
    let ops: Vec<Op> = sqlx::query_as(
        "select field, op, value, status, error, actor_id, created_at from bot_profile_ops
         where guild_id = $1 order by id desc limit 20",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let u = &m["user"];
    let name = u["global_name"].as_str().or(u["username"].as_str());
    // Bot-wide, shown read-only to everyone who may see this page; only the bot owner may change it.
    let (status, kind, text) = stored_presence(&st).await?;
    Ok(Json(json!({
        "presence": {"status": status, "kind": kind, "text": text, "editable": is_bot_owner(&st, &s).await},
        "name": name,
        "nick": m["nick"],
        "bio": bio,
        "avatar": cdn(format!("guilds/{guild}/users/{bot}/avatars"), &m["avatar"]),
        "banner": cdn(format!("guilds/{guild}/users/{bot}/banners"), &m["banner"]),
        "user_avatar": cdn(format!("avatars/{bot}"), &u["avatar"]),
        "busy": ops.iter().any(|o| o.3 == "reserved"),
        "ops": ops.iter().map(|(field, op, value, status, error, actor, at)| json!({
            "field": field, "op": op, "value": value, "status": status, "error": error,
            "actor": actor.to_string(), "at": at,
        })).collect::<Vec<_>>(),
    })))
}

/// A trimmed nickname or bio, refused when empty (Reset removes one), too long, or holding
/// control or text-direction characters that could make it read as something else.
fn clean(f: Field, v: &str) -> Result<String, String> {
    let (max, name) = if f == Field::Nick {
        (NICK_MAX, "nickname")
    } else {
        (BIO_MAX, "bio")
    };
    let v = v.trim();
    let bad = |c: char| {
        (c.is_control() && !(f == Field::Bio && c == '\n'))
            || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
    };
    if v.chars().any(bad) {
        return Err(format!(
            "the {name} has control or text-direction characters"
        ));
    }
    let visible = v.chars().any(|c| {
        !c.is_whitespace() && !matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
    });
    if !visible {
        return Err(format!("the {name} is empty; use Reset to remove it"));
    }
    if v.chars().count() > max {
        return Err(format!("the {name} is longer than {max} characters"));
    }
    Ok(v.to_string())
}

/// Parses an upload as a real PNG or JPEG and encodes it again, which drops EXIF, GPS, text chunks
/// and anything appended to the file (a polyglot). The limits stop a tiny file that claims a huge
/// size before it allocates.
pub(crate) fn reencode(b: &[u8], min: (u32, u32)) -> Result<(&'static str, Vec<u8>), String> {
    use image::{ImageFormat, ImageReader};
    use std::io::Cursor;
    let only = "only PNG and JPEG images can be used";
    let fmt = image::guess_format(b).map_err(|_| only.to_string())?;
    let mime = match fmt {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        _ => return Err(only.into()),
    };
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
    if w < min.0 || h < min.1 {
        return Err(format!(
            "the image must be at least {} x {} pixels",
            min.0, min.1
        ));
    }
    let img = reader().decode().map_err(unreadable)?;
    let mut out = Vec::new();
    let done = match fmt {
        ImageFormat::Png => img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png),
        _ => image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(&img.to_rgb8()),
    };
    done.map_err(|_| "the image could not be read".to_string())?;
    Ok((mime, out))
}

/// At most two images are decoded at a time, off the async threads.
pub(crate) static DECODE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

async fn set(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, field)): Path<(i64, String)>,
    body: Body,
) -> Res {
    let f = Field::parse(&field)?;
    guard(&st, &s, guild, true).await?;
    // Read only after the guard, so someone without access cannot make the server hold 10 MB.
    let body = axum::body::to_bytes(body, BANNER_MAX + 1024)
        .await
        .map_err(|_| ApiError::BadRequest("the image is larger than 10 MB".into()))?;
    let (value, sha, payload) = if f.image() {
        let (max, min) = if f == Field::Avatar {
            (AVATAR_MAX, AVATAR_MIN)
        } else {
            (BANNER_MAX, BANNER_MIN)
        };
        if body.len() > max {
            return Err(ApiError::BadRequest(format!(
                "the image is larger than {} MB",
                max >> 20
            )));
        }
        let sha: String = Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let slot = DECODE.acquire().await.map_err(anyhow::Error::from)?;
        // The slot goes with the work, which runs on even if the request is dropped.
        let (mime, out) = tokio::task::spawn_blocking(move || (reencode(&body, min), drop(slot)).0)
            .await
            .map_err(anyhow::Error::from)?
            .map_err(ApiError::BadRequest)?;
        let uri = format!("data:{mime};base64,{}", STANDARD.encode(out));
        (None, Some(sha), json!(uri))
    } else {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Text {
            value: String,
        }
        let t: Text = serde_json::from_slice(&body)
            .map_err(|_| ApiError::BadRequest("send {\"value\": \"...\"}".into()))?;
        let v = clean(f, &t.value).map_err(ApiError::BadRequest)?;
        (Some(v.clone()), None, json!(v))
    };
    apply(&st, &s, guild, f, "set", value, sha, payload).await
}

async fn reset(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, field)): Path<(i64, String)>,
) -> Res {
    let f = Field::parse(&field)?;
    guard(&st, &s, guild, true).await?;
    apply(&st, &s, guild, f, "reset", None, None, Value::Null).await
}

/// What Discord's refusal means for the owner, without echoing the request.
fn refusal(code: reqwest::StatusCode, f: Field, b: &Value) -> String {
    match code.as_u16() {
        403 if f == Field::Nick => {
            "Brim needs the Change Nickname permission in this server".into()
        }
        429 => "Discord is limiting profile changes right now; try again in a minute".into(),
        _ => {
            let detail = b["errors"][f.key()]["_errors"][0]["message"]
                .as_str()
                .or(b["message"].as_str())
                .unwrap_or("no reason given");
            format!(
                "Discord refused it: {}",
                detail.chars().take(200).collect::<String>()
            )
        }
    }
}

const NO_ANSWER: &str =
    "Discord did not answer, so the change may or may not have been made. Reload to see the profile.";

/// Frees a slot left by a process that died before Discord answered (a restart during an update).
async fn sweep(st: &AppState, guild: i64) -> Result<(), ApiError> {
    sqlx::query(
        "update bot_profile_ops set status = 'unknown', error = $3, finished_at = now()
         where guild_id = $1 and status = 'reserved' and created_at < now() - make_interval(secs => $2)",
    )
    .bind(guild)
    .bind(LEASE_SECS)
    .bind(NO_ANSWER)
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Holds the server's one change slot, sends the change and records how it ended.
#[allow(clippy::too_many_arguments)]
async fn apply(
    st: &AppState,
    s: &Sess,
    guild: i64,
    f: Field,
    op: &'static str,
    value: Option<String>,
    sha: Option<String>,
    payload: Value,
) -> Res {
    sweep(st, guild).await?;
    // Checked again just before the change: the image work took time, and access may have changed.
    guard(st, s, guild, true).await?;
    // The cap sits in the insert, and the partial unique index lets one change run per server.
    let id: Option<i64> = sqlx::query_scalar(
        "insert into bot_profile_ops (guild_id, actor_id, field, op, value, sha256)
         select $1, $2, $3, $4, $5, $6
         where (select count(*) from bot_profile_ops
                where guild_id = $1 and created_at > now() - interval '1 hour') < $7
         on conflict do nothing returning id",
    )
    .bind(guild)
    .bind(s.user_id)
    .bind(f.key())
    .bind(op)
    .bind(&value)
    .bind(&sha)
    .bind(HOURLY)
    .fetch_optional(&st.db)
    .await?;
    let Some(id) = id else {
        let running: bool = sqlx::query_scalar(
            "select exists(select 1 from bot_profile_ops where guild_id = $1 and status = 'reserved')",
        )
        .bind(guild)
        .fetch_one(&st.db)
        .await?;
        return Err(if running {
            ApiError::Busy("another profile change is still running; try again in a moment")
        } else {
            ApiError::TooMany(format!(
                "The bot profile can change at most {HOURLY} times an hour. Try again later."
            ))
        });
    };
    // Its own task, so a closed tab cannot stop it between Discord's answer and the record.
    let st2 = st.clone();
    let (status, error) = tokio::spawn(async move {
        let st = st2;
        let path = format!("/guilds/{guild}/members/@me");
        let body = json!({ f.key(): payload });
        let r = st.discord.send(Method::PATCH, &path, Some(body)).await;
        let (status, error): (&str, Option<String>) = match r {
            Err(e) => {
                tracing::warn!(guild, field = f.key(), error = %e, "bot profile change got no answer");
                ("unknown", Some(NO_ANSWER.into()))
            }
            Ok(res) if res.status().is_server_error() => ("unknown", Some(NO_ANSWER.into())),
            Ok(res) if res.status().is_success() => match res.json::<Value>().await {
                // Discord answers 200 and keeps the old image when it cannot use the data.
                Ok(m) if op == "set" && f.image() && m[f.key()].as_str().is_none() => (
                    "rejected",
                    Some("Discord did not accept the image".into()),
                ),
                Ok(_) => ("succeeded", None),
                Err(_) => ("unknown", Some(NO_ANSWER.into())),
            },
            Ok(res) => {
                let code = res.status();
                let b: Value = res.json().await.unwrap_or_default();
                ("rejected", Some(refusal(code, f, &b)))
            }
        };
        let done = sqlx::query(
            "update bot_profile_ops set status = $2, error = $3, finished_at = now()
             where id = $1 and status = 'reserved'",
        )
        .bind(id)
        .bind(status)
        .bind(&error)
        .execute(&st.db)
        .await;
        if let Err(e) = done {
            // The lease turns the row into "unknown" later; the answer below still tells the truth.
            tracing::error!(?e, guild, id, status, "recording a bot profile change failed");
        }
        (status, error)
    })
    .await
    .map_err(anyhow::Error::from)?;
    tracing::info!(
        guild,
        user = s.user_id,
        field = f.key(),
        op,
        status,
        "bot profile change"
    );
    match status {
        "succeeded" => Ok(Json(json!({"status": status}))),
        _ => Err(ApiError::BadRequest(error.unwrap_or_default())),
    }
}

async fn bot_owner(st: &AppState, s: &Sess) -> Result<(), ApiError> {
    if is_bot_owner(st, s).await {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "only Brim's owner can change its status",
        ))
    }
}

/// The bot-wide status and activity as stored, online with no activity before the first change.
async fn stored_presence(
    st: &AppState,
) -> Result<(String, Option<String>, Option<String>), ApiError> {
    let row = sqlx::query_as("select status, kind, text from bot_presence")
        .fetch_optional(&st.db)
        .await?;
    Ok(row.unwrap_or(("online".into(), None, None)))
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Presence {
    status: String,
    kind: Option<String>,
    text: Option<String>,
}

/// The status and activity as stored, or why they are refused.
fn check_presence(p: Presence) -> Result<(String, Option<String>, Option<String>), String> {
    if !["online", "idle", "dnd", "invisible"].contains(&p.status.as_str()) {
        return Err("status must be online, idle, dnd or invisible".into());
    }
    let text = p
        .text
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    match (&p.kind, &text) {
        (None, None) => {}
        (Some(k), Some(t)) => {
            if !["playing", "watching", "listening", "competing", "custom"].contains(&k.as_str()) {
                return Err(
                    "activity must be playing, watching, listening, competing or custom".into(),
                );
            }
            if t.chars().count() > ACTIVITY_MAX {
                return Err(format!(
                    "the activity is longer than {ACTIVITY_MAX} characters"
                ));
            }
            if t.chars().any(|c| {
                c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
            }) {
                return Err("the activity has control or text-direction characters".into());
            }
            let low = t.to_lowercase();
            if low.contains("://")
                || low.contains("discord.gg")
                || low.contains("discord.com/invite")
            {
                return Err("the activity cannot hold a link".into());
            }
        }
        _ => return Err("an activity needs both a type and a text".into()),
    }
    Ok((p.status, p.kind, text))
}

async fn set_presence(State(st): State<AppState>, s: Sess, Json(p): Json<Presence>) -> Res {
    bot_owner(&st, &s).await?;
    if !s.stepped_up {
        return Err(ApiError::StepUp);
    }
    let (status, kind, text) = check_presence(p).map_err(ApiError::BadRequest)?;
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "insert into bot_presence (id, status, kind, text, updated_by) values (true, $1, $2, $3, $4)
         on conflict (id) do update set status = $1, kind = $2, text = $3, updated_by = $4, updated_at = now()",
    )
    .bind(&status)
    .bind(&kind)
    .bind(&text)
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    // Delivered at commit; the bot reads the row again, so a lost NOTIFY only waits for its next READY.
    sqlx::query("select pg_notify('bot_presence', '')")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(user = s.user_id, status, ?kind, "bot presence changed");
    Ok(Json(json!({"status": status, "kind": kind, "text": text})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![];
        image::RgbaImage::from_pixel(w, h, image::Rgba([255, 111, 145, 255]))
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn reencode_keeps_png_and_jpeg_and_drops_extra_bytes() {
        let mut p = png(128, 128);
        p.extend_from_slice(b"PK\x03\x04 appended zip");
        let (mime, out) = reencode(&p, AVATAR_MIN).unwrap();
        assert_eq!(mime, "image/png");
        assert!(!out.windows(4).any(|w| w == b"PK\x03\x04"));
        let mut j = vec![];
        image::RgbImage::from_pixel(600, 240, image::Rgb([1, 2, 3]))
            .write_to(&mut std::io::Cursor::new(&mut j), image::ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(reencode(&j, BANNER_MIN).unwrap().0, "image/jpeg");
    }

    #[test]
    fn reencode_refuses_other_formats_garbage_and_huge_images() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        assert!(reencode(svg, AVATAR_MIN).is_err());
        assert!(reencode(b"GIF89a\x04\x00\x04\x00", AVATAR_MIN).is_err());
        let mut fake = png(8, 8);
        fake.truncate(40);
        assert!(reencode(&fake, (1, 1)).is_err());
        // Under Discord's minimum.
        assert_eq!(
            reencode(&png(599, 240), BANNER_MIN).unwrap_err(),
            "the image must be at least 600 x 240 pixels"
        );
        assert!(reencode(&png(127, 128), AVATAR_MIN).is_err());
        // 4100 x 4100 = 16.8 MP, over the cap; refused before it is decoded.
        let big = png(4100, 4100);
        assert_eq!(
            reencode(&big, AVATAR_MIN).unwrap_err(),
            "the image has more than 16 megapixels"
        );
    }

    #[test]
    fn clean_trims_and_refuses_empty_long_and_tricky_text() {
        assert_eq!(clean(Field::Nick, "  Sam  ").unwrap(), "Sam");
        assert!(clean(Field::Nick, "   ").is_err());
        assert!(clean(Field::Nick, "\u{200B}\u{FEFF}").is_err());
        assert!(clean(Field::Nick, &"x".repeat(33)).is_err());
        assert!(clean(Field::Nick, &"é".repeat(32)).is_ok());
        assert!(clean(Field::Nick, "a\nb").is_err());
        assert!(clean(Field::Bio, "line one\nline two").is_ok());
        assert!(clean(Field::Bio, "evil\u{202E}txt").is_err());
        assert!(clean(Field::Bio, &"x".repeat(191)).is_err());
    }

    #[test]
    fn cdn_urls_come_only_from_a_plain_hash() {
        assert_eq!(
            cdn("avatars/1".into(), &json!("a_abc123")),
            json!("https://cdn.discordapp.com/avatars/1/a_abc123.png?size=512")
        );
        assert_eq!(cdn("avatars/1".into(), &json!("../../x")), Value::Null);
        assert_eq!(cdn("avatars/1".into(), &Value::Null), Value::Null);
    }

    #[test]
    fn presence_needs_known_values_and_no_links() {
        let p = |status: &str, kind: Option<&str>, text: Option<&str>| {
            check_presence(Presence {
                status: status.into(),
                kind: kind.map(String::from),
                text: text.map(String::from),
            })
        };
        assert!(p("online", None, None).is_ok());
        assert!(p("idle", Some("watching"), Some("the server")).is_ok());
        assert!(p("offline", None, None).is_err());
        assert!(p("online", Some("streaming"), Some("x")).is_err());
        assert!(p("online", Some("playing"), None).is_err());
        assert!(p("online", Some("playing"), Some("   ")).is_err());
        assert!(p("online", Some("custom"), Some("join discord.gg/x")).is_err());
        assert!(p("online", Some("custom"), Some("see https://x.y")).is_err());
        assert!(p("online", Some("custom"), Some(&"x".repeat(129))).is_err());
    }
}

#[cfg(test)]
mod db_tests {
    use crate::{auth::Sess, discord::Discord, AppState};
    use axum::{body::Body, http::Request, routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    use tower::ServiceExt;

    /// Runs only when DATABASE_URL points at a scratch Postgres. Discord is a local mock.
    #[tokio::test]
    async fn only_the_real_owner_changes_only_this_servers_member_profile() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let pid = std::process::id() as i64;
        let (g, other) = (9_710_000_000 + pid, 9_711_000_000 + pid);
        let (owner, eo, extra) = (
            7_710_000_000 + pid,
            7_711_000_000 + pid,
            7_712_000_000 + pid,
        );
        let patches: Arc<Mutex<Vec<(String, Value)>>> = Default::default();
        let drop_image = Arc::new(AtomicBool::new(false));
        let (p, d) = (patches.clone(), drop_image.clone());
        let mock = Router::new()
            .route(
                "/guilds/{g}",
                get(move |axum::extract::Path(id): axum::extract::Path<i64>| async move {
                    let o = if id == g { owner } else { eo };
                    Json(json!({"owner_id": o.to_string(), "name": "T", "roles": []}))
                }),
            )
            .route("/users/@me", get(|| async { Json(json!({"id": "99"})) }))
            .route(
                "/applications/@me",
                get(move || async move { Json(json!({"owner": {"id": owner.to_string()}})) }),
            )
            .route(
                "/guilds/{g}/members/{u}",
                get(|| async {
                    Json(json!({"roles": [], "nick": null, "avatar": null, "banner": null,
                        "joined_at": "2020-01-01T00:00:00+00:00",
                        "user": {"id": "99", "username": "Brim", "avatar": "abc"}}))
                })
                .patch(
                    move |axum::extract::Path((gid, u)): axum::extract::Path<(String, String)>,
                          Json(b): Json<Value>| async move {
                        p.lock().unwrap().push((format!("{gid}/{u}"), b.clone()));
                        if b["nick"] == "deny" {
                            return Err(axum::http::StatusCode::FORBIDDEN);
                        }
                        let img = |k: &str| match &b[k] {
                            Value::String(_) if !d.load(Ordering::SeqCst) => json!("h1"),
                            _ => Value::Null,
                        };
                        Ok(Json(json!({"nick": b["nick"], "bio": b["bio"], "avatar": img("avatar"), "banner": img("banner")})))
                    },
                ),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, mock).await });
        let st = AppState {
            db: db.clone(),
            discord: Discord {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                client_id: "99".into(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                api,
                memo: Default::default(),
            },
            secure: false,
            totp: Default::default(),
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        };
        for t in [
            "bot_profile_ops",
            "extra_owners",
            "trusted_admins",
            "guild_owners",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = any($1)"
            )))
            .bind(vec![g, other])
            .execute(&db)
            .await
            .unwrap();
        }
        // `eo` is a Trusted Admin here; `extra` is the Extra Owner.
        sqlx::query("insert into trusted_admins (guild_id, user_id) values ($1, $2)")
            .bind(g)
            .bind(eo)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into extra_owners (guild_id, user_id) values ($1, $2)")
            .bind(g)
            .bind(extra)
            .execute(&db)
            .await
            .unwrap();
        let send = |m: &str, path: String, user: i64, stepped_up: bool, body: Body| {
            let mut req = Request::builder()
                .method(m)
                .uri(&path)
                .header("content-type", "application/json")
                .body(body)
                .unwrap();
            req.extensions_mut().insert(Sess {
                id: String::new(),
                user_id: user,
                username: "t".into(),
                access_token: String::new(),
                step_up_guild: path.split('/').nth(3).and_then(|g| g.parse().ok()),
                stepped_up,
                csrf: String::new(),
            });
            let app = crate::router(st.clone(), "/nonexistent");
            async move {
                let res = app.oneshot(req).await.unwrap();
                let code = res.status().as_u16();
                let b = axum::body::to_bytes(res.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (
                    code,
                    serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
                )
            }
        };
        let at = |f: &str| format!("/api/guilds/{g}/bot-profile/{f}");
        let text = |v: &str| Body::from(json!({ "value": v }).to_string());
        let n = || patches.lock().unwrap().len();

        let (code, r) = send(
            "GET",
            format!("/api/guilds/{g}/bot-profile"),
            owner,
            false,
            Body::empty(),
        )
        .await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(
            r["user_avatar"],
            "https://cdn.discordapp.com/avatars/99/abc.png?size=512"
        );
        // A Trusted Admin, an owner without 2FA, and the owner of another server change nothing.
        assert_eq!(
            send(
                "GET",
                format!("/api/guilds/{g}/bot-profile"),
                eo,
                true,
                Body::empty()
            )
            .await
            .0,
            403
        );
        assert_eq!(send("PUT", at("nick"), eo, true, text("x")).await.0, 403);
        let (code, r) = send("PUT", at("nick"), owner, false, text("x")).await;
        assert_eq!((code, r["error"].as_str()), (403, Some("step_up_required")));
        let b_path = format!("/api/guilds/{other}/bot-profile/nick");
        assert_eq!(send("PUT", b_path, owner, true, text("x")).await.0, 403);
        assert_eq!(
            send("PUT", at("username"), owner, true, text("x")).await.0,
            404
        );
        assert_eq!(n(), 0, "no refused request reached Discord");

        let (code, r) = send("PUT", at("nick"), owner, true, text("  Sam ")).await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(
            patches.lock().unwrap()[0],
            (format!("{g}/@me"), json!({"nick": "Sam"})),
            "only the changed field, only on the bot's own member in this server"
        );
        let mut png = vec![];
        image::RgbaImage::from_pixel(600, 240, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(
            send("PUT", at("avatar"), owner, true, Body::from(png.clone()))
                .await
                .0,
            200
        );
        let sent = patches.lock().unwrap()[1].1["avatar"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(sent.starts_with("data:image/png;base64,"));
        drop_image.store(true, Ordering::SeqCst);
        let (code, r) = send("PUT", at("banner"), owner, true, Body::from(png)).await;
        assert_eq!(
            (code, r["error"].as_str()),
            (400, Some("Discord did not accept the image"))
        );
        let (code, r) = send("PUT", at("nick"), owner, true, text("deny")).await;
        assert_eq!(code, 400);
        assert!(
            r["error"].as_str().unwrap().contains("Change Nickname"),
            "{r}"
        );
        assert_eq!(
            send("PUT", at("avatar"), owner, true, Body::from("not an image"))
                .await
                .0,
            400
        );
        assert_eq!(n(), 4, "an unreadable image never reaches Discord");

        assert_eq!(
            send("PUT", at("bio"), owner, true, text("hi there"))
                .await
                .0,
            200
        );
        let (_, r) = send(
            "GET",
            format!("/api/guilds/{g}/bot-profile"),
            owner,
            false,
            Body::empty(),
        )
        .await;
        assert_eq!(r["bio"], "hi there");
        assert_eq!(
            send("DELETE", at("bio"), owner, true, Body::empty())
                .await
                .0,
            200
        );
        assert_eq!(
            patches.lock().unwrap().last().unwrap().1,
            json!({"bio": null})
        );
        let (_, r) = send(
            "GET",
            format!("/api/guilds/{g}/bot-profile"),
            owner,
            false,
            Body::empty(),
        )
        .await;
        assert_eq!(r["bio"], Value::Null);
        let statuses: Vec<String> = r["ops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["status"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            statuses,
            [
                "succeeded",
                "succeeded",
                "rejected",
                "rejected",
                "succeeded",
                "succeeded"
            ]
        );

        // One change at a time; a slot left by a dead process is freed after the lease as "unknown".
        sqlx::query("insert into bot_profile_ops (guild_id, actor_id, field, op) values ($1, $2, 'nick', 'reset')")
            .bind(g).bind(owner).execute(&db).await.unwrap();
        assert_eq!(send("PUT", at("nick"), owner, true, text("y")).await.0, 409);
        sqlx::query("update bot_profile_ops set created_at = now() - interval '10 minutes' where guild_id = $1 and status = 'reserved'")
            .bind(g).execute(&db).await.unwrap();
        let (_, r) = send(
            "GET",
            format!("/api/guilds/{g}/bot-profile"),
            owner,
            false,
            Body::empty(),
        )
        .await;
        assert_eq!(
            r["busy"], false,
            "a slot left by a restart is freed on page load"
        );
        assert_eq!(send("PUT", at("nick"), owner, true, text("y")).await.0, 200);
        let unknown: i64 = sqlx::query_scalar(
            "select count(*) from bot_profile_ops where guild_id = $1 and status = 'unknown'",
        )
        .bind(g)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(unknown, 1);
        // An Extra Owner can do it too (owner decision 11-15).
        assert_eq!(
            send("PUT", at("nick"), extra, true, text("by extra"))
                .await
                .0,
            200
        );

        // Global presence: only the application owner, with 2FA.
        let pres = |v: Value| Body::from(v.to_string());
        let watching = json!({"status": "idle", "kind": "watching", "text": "the servers"});
        assert_eq!(
            send(
                "PUT",
                "/api/bot-presence".into(),
                eo,
                true,
                pres(watching.clone())
            )
            .await
            .0,
            403
        );
        assert_eq!(
            send(
                "PUT",
                "/api/bot-presence".into(),
                owner,
                false,
                pres(watching.clone())
            )
            .await
            .0,
            403
        );
        let had: Option<(String, Option<String>, Option<String>, i64)> =
            sqlx::query_as("select status, kind, text, updated_by from bot_presence")
                .fetch_optional(&db)
                .await
                .unwrap();
        assert_eq!(
            send(
                "PUT",
                "/api/bot-presence".into(),
                owner,
                true,
                pres(watching)
            )
            .await
            .0,
            200
        );
        let now: (String, Option<String>, Option<String>) =
            sqlx::query_as("select status, kind, text from bot_presence")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            now,
            (
                "idle".into(),
                Some("watching".into()),
                Some("the servers".into())
            )
        );
        // The presence row is global and shared by test runs; put back what was there.
        match had {
            Some((s, k, t, by)) => sqlx::query(
                "update bot_presence set status = $1, kind = $2, text = $3, updated_by = $4",
            )
            .bind(s)
            .bind(k)
            .bind(t)
            .bind(by)
            .execute(&db)
            .await
            .unwrap(),
            None => sqlx::query("delete from bot_presence")
                .execute(&db)
                .await
                .unwrap(),
        };
        for t in ["bot_profile_ops", "extra_owners", "trusted_admins"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = any($1)"
            )))
            .bind(vec![g, other])
            .execute(&db)
            .await
            .unwrap();
        }
    }
}

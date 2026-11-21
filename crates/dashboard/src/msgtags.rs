//! Message Builder tags: a short name staff use with `/tag` to post a saved message in the channel
//! where they type it. Owner, Extra Owners and Trusted Admins set the tag (in the Automate tab);
//! the bot checks the `tags` permit, the pause and the rate limits and queues a run
//! (`message_tag_runs`, with a NOTIFY on `message_runs`). This runner posts it as Brim with no
//! pings and writes the result back, which the bot shows to the member.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    embeds::{self, Sender},
    lockdown::audit,
    msgrules, ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

/// Longest tag (Discord shows autocomplete names up to 100 characters; short is easier to type).
const MAX_LEN: usize = 32;
/// A run queued longer ago than this is dropped unsent: the bot has told the member it timed out.
const STALE_SECS: f64 = 20.0;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/guilds/{guild}/embeds/{id}/tag", get(show).post(save))
}

async fn show(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let tag: Option<Option<String>> =
        sqlx::query_scalar("select tag from embeds where id = $1 and guild_id = $2")
            .bind(id)
            .bind(guild)
            .fetch_optional(&st.db)
            .await?;
    Ok(Json(json!({"tag": tag.ok_or(ApiError::NotFound)?})))
}

/// Lower case letters, digits, `-` and `_`, 1 to [`MAX_LEN`] long; empty removes the tag.
fn clean(tag: &str) -> Result<Option<String>, String> {
    let t = tag.trim().to_lowercase();
    if t.is_empty() {
        return Ok(None);
    }
    if t.chars().count() > MAX_LEN
        || !t
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "a tag is 1 to {MAX_LEN} characters: letters a to z, digits, - and _"
        ));
    }
    Ok(Some(t))
}

#[derive(Deserialize)]
struct Save {
    tag: String,
    #[serde(default)]
    seen: Option<String>,
}

/// Sets or removes the tag (Owner, Extra Owners, Trusted Admins). A tag vouches for the message
/// like a rule that is on: from then on only that level changes it.
async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Save>,
) -> Res {
    let level = api::require(&st, &s, guild, Level::Admin, false).await?;
    let tag = clean(&b.tag).map_err(ApiError::BadRequest)?;
    let mut tx = st.db.begin().await?;
    let version =
        msgrules::lock_message(&mut tx, guild, id, tag.is_some(), b.seen.as_deref(), false).await?;
    let taken: bool = sqlx::query_scalar(
        "select exists(select 1 from embeds where guild_id = $1 and tag = $2 and id <> $3)",
    )
    .bind(guild)
    .bind(&tag)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if taken {
        return Err(ApiError::BadRequest(format!(
            "another message in this server already has the tag “{}”",
            tag.unwrap_or_default()
        )));
    }
    let set = sqlx::query("update embeds set tag = $3 where id = $1 and guild_id = $2")
        .bind(id)
        .bind(guild)
        .bind(&tag)
        .execute(&mut *tx)
        .await;
    // Two saves of the same new tag at once: the unique index refuses the second.
    if let Err(sqlx::Error::Database(e)) = &set {
        if e.is_unique_violation() {
            return Err(ApiError::BadRequest(
                "another message in this server already has that tag".into(),
            ));
        }
    }
    set?;
    let version = if tag.is_some() {
        msgrules::vouch(&mut tx, guild, id, level).await?
    } else {
        version
    };
    tx.commit().await?;
    audit(
        &st,
        &s,
        guild,
        match &tag {
            Some(t) => format!("set the tag “{t}” on message #{id}"),
            None => format!("removed the tag of message #{id}"),
        },
    )
    .await?;
    Ok(Json(json!({"tag": tag, "version": version})))
}

/// Posts one queued `/tag` run, if any: claimed first (a second dashboard skips it), then the
/// result is stored for the bot's reply. Stale runs are dropped unsent.
pub(crate) async fn run_one(st: &AppState) -> Result<bool, ApiError> {
    #[allow(clippy::type_complexity)]
    let run: Option<(i64, i64, Option<i64>, Option<String>, i64, i64, String, f64)> = sqlx::query_as(
        "update message_tag_runs set claimed_at = now() where id =
             (select id from message_tag_runs where claimed_at is null order by id limit 1 for update skip locked)
         returning id, guild_id, embed_id, entry, channel_id, member_id, member_name,
                   extract(epoch from now() - created_at)::float8",
    )
    .fetch_optional(&st.db)
    .await?;
    let Some((run, guild, embed, entry, channel, member, name, age)) = run else {
        // Kept an hour for the rate limits, then gone.
        sqlx::query("delete from message_tag_runs where created_at < now() - interval '1 hour'")
            .execute(&st.db)
            .await?;
        return Ok(false);
    };
    let result = if age > STALE_SECS {
        "Not posted: Brim was busy. Try again.".to_string()
    } else {
        let sent = match (embed, entry) {
            (Some(embed), _) => post(st, guild, embed, channel, (member, &name)).await,
            (None, Some(key)) => post_entry(st, guild, &key, channel).await,
            (None, None) => Err(ApiError::NotFound),
        };
        match sent {
            Ok(r) => r,
            Err(ApiError::BadRequest(why)) => format!("Not posted: {why}."),
            Err(ApiError::NotFound) => "Not posted: the message was deleted.".into(),
            Err(e) => {
                tracing::warn!(?e, run, "posting a tag failed");
                "Not posted: something went wrong. Try again.".into()
            }
        }
    };
    sqlx::query("update message_tag_runs set result = $2 where id = $1 and guild_id = $3")
        .bind(run)
        .bind(&result)
        .bind(guild)
        .execute(&st.db)
        .await?;
    Ok(true)
}

async fn post(
    st: &AppState,
    guild: i64,
    embed: i64,
    channel: i64,
    (member, name): (i64, &str),
) -> Result<String, ApiError> {
    // The bot checked these; checked again here, as the state may have changed since.
    if let Some(why) = embeds::emergency(st, guild).await? {
        return Ok(format!("Not posted: {why}."));
    }
    let tag: Option<String> =
        sqlx::query_scalar("select tag from embeds where id = $1 and guild_id = $2")
            .bind(embed)
            .bind(guild)
            .fetch_optional(&st.db)
            .await?
            .flatten();
    let Some(tag) = tag else {
        return Ok("Not posted: that tag was removed.".into());
    };
    if !msgrules::trusted(st, guild, embed).await? {
        return Ok("Not posted: the message was changed by an older Brim version. An Owner, Extra Owner or Trusted Admin must save it again.".into());
    }
    // Not into a log, ticket, verification or appeal channel.
    msgrules::check_channel(st, guild, channel).await?;
    let mut r = embeds::ready(st, guild, embed, Some(channel), false).await?;
    // Owner decision 2026-11-18: a tag always posts as Brim and never pings.
    r.webhook = None;
    r.pings = json!({"parse": []});
    let by = Sender {
        user: member,
        who: format!("Sent by {name} with /tag {tag}"),
        rule: None,
        answer: None,
        sticky: false,
    };
    Ok(match embeds::send_copy(st, guild, embed, r, by).await? {
        Some((_, Some(warning))) => format!("Posted “{tag}”. {warning}"),
        _ => format!("Posted “{tag}”."),
    })
}

/// A built-in tag: posts the server's edited version of the entry `key`, or its default, as
/// Brim with no pings, after the same pause and channel checks as a saved message.
async fn post_entry(
    st: &AppState,
    guild: i64,
    key: &str,
    channel: i64,
) -> Result<String, ApiError> {
    if let Some(why) = embeds::emergency(st, guild).await? {
        return Ok(format!("Not posted: {why}."));
    }
    msgrules::check_channel(st, guild, channel).await?;
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("select message from bot_messages where guild_id = $1 and key = $2")
            .bind(guild)
            .bind(key)
            .fetch_optional(&st.db)
            .await?;
    let m = brim_logging::botmsg::render(key, stored.as_ref(), &[("permissions", &permissions())]);
    embeds::discord(
        st,
        reqwest::Method::POST,
        &format!("/channels/{channel}/messages"),
        Some(json!({"content": m["content"], "embeds": m["embeds"], "allowed_mentions": {"parse": []}})),
    )
    .await?
    .ok_or(ApiError::BadRequest("the channel is gone".into()))?;
    Ok("Posted the built-in tag.".into())
}

/// `{permissions}`: Brim's own dangerous Discord permissions, one per line, so the tag shows
/// exactly what the protection acts on.
fn permissions() -> String {
    brim_antinuke::dangerous_names(brim_antinuke::DANGEROUS_PERMS)
        .iter()
        .map(|n| format!("{} {n}", brim_logging::emoji::ARROW))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{clean, permissions};

    #[test]
    fn the_built_in_tag_lists_every_dangerous_permission() {
        let p = permissions();
        assert_eq!(
            p.lines().count() as u32,
            brim_antinuke::DANGEROUS_PERMS.count_ones()
        );
        assert!(p.contains("Administrator") && p.contains("Timeout Members"));
        let m =
            brim_logging::botmsg::render("tag.dangerous_permissions", None, &[("permissions", &p)]);
        assert_eq!(m["embeds"][0]["title"], "Dangerous Permissions");
        assert_eq!(m["embeds"][0]["description"], p);
    }

    #[test]
    fn tags_are_short_lower_case_words() {
        assert_eq!(clean("  Rules "), Ok(Some("rules".into())));
        assert_eq!(clean("faq_2-b"), Ok(Some("faq_2-b".into())));
        assert_eq!(clean(""), Ok(None));
        assert!(clean("two words").is_err());
        assert!(clean("<@123>").is_err());
        assert!(clean(&"a".repeat(33)).is_err());
    }
}

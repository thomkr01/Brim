//! `/tag name`: staff with the `tags` permit (or Owner, Extra Owners, Trusted Admins) post a saved
//! message (Message Builder) or a built-in entry ([`BUILT_IN`]) by its tag, in the channel where
//! they type it. The bot checks the permit and the rate limits and queues the post
//! (`message_tag_runs`); the dashboard sends it as Brim with no pings (paused during panic
//! mode, a lockdown or an anti-nuke case) and writes the result back, which the bot shows in its
//! reply.
use std::time::Duration;

use anyhow::Result;
use twilight_model::{
    application::{
        command::{CommandOptionChoice, CommandOptionChoiceValue},
        interaction::{
            application_command::{CommandDataOption, CommandOptionValue},
            Interaction, InteractionData,
        },
    },
    channel::message::Embed,
    guild::Permissions,
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    id::{
        marker::{GuildMarker, UserMarker},
        Id,
    },
};

use crate::app::App;

/// Tags every server has: the tag and the entry (Bot messages page) it posts. A server's own
/// saved message with the same tag comes first.
pub const BUILT_IN: &[(&str, &str)] = &[("dangerous-permissions", "tag.dangerous_permissions")];

/// How long the reply waits for the dashboard to post.
const WAIT: Duration = Duration::from_secs(8);

/// Whether this member may use `/tag` here.
async fn allowed(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> Result<bool> {
    let owner = app.guild_info(gid).await?.owner;
    Ok(app.level(gid, owner, user).await?.has("tags"))
}

/// The tags that contain what the member typed, for Discord's search list. Empty for a member
/// without the permit, so the list does not show the server's tags to everyone.
pub async fn autocomplete(app: &App, i: &Interaction) -> Result<()> {
    let (Some(gid), Some(user), Some(InteractionData::ApplicationCommand(d))) =
        (i.guild_id, i.author_id(), &i.data)
    else {
        return Ok(());
    };
    let typed = d
        .options
        .iter()
        .find_map(|o| match &o.value {
            CommandOptionValue::Focused(v, _) => Some(v.trim().to_lowercase()),
            _ => None,
        })
        .unwrap_or_default();
    // The tags first: nothing that matches answers without a permit lookup (a Discord call).
    let mut found: Vec<String> = sqlx::query_scalar(
        "select tag from embeds where guild_id = $1 and strpos(tag, $2) > 0
         order by strpos(tag, $2), tag limit 25",
    )
    .bind(gid.get() as i64)
    .bind(&typed)
    .fetch_all(&app.pool)
    .await
    .unwrap_or_default();
    for (t, _) in BUILT_IN {
        if t.contains(typed.as_str()) && !found.iter().any(|f| f == t) && found.len() < 25 {
            found.push((*t).into());
        }
    }
    let tags =
        if d.name == "tag" && !found.is_empty() && allowed(app, gid, user).await.unwrap_or(false) {
            found
        } else {
            vec![]
        };
    let choices = tags
        .into_iter()
        .map(|t| CommandOptionChoice {
            name: t.clone(),
            name_localizations: None,
            value: CommandOptionChoiceValue::String(t),
        })
        .collect();
    app.http
        .interaction(Id::new(app.me.get()))
        .create_response(
            i.id,
            &i.token,
            &InteractionResponse {
                kind: InteractionResponseType::ApplicationCommandAutocompleteResult,
                data: Some(InteractionResponseData {
                    choices: Some(choices),
                    ..Default::default()
                }),
            },
        )
        .await?;
    Ok(())
}

/// Runs `/tag` (already acknowledged, the reply is only for the member) and returns the reply.
pub async fn command(
    app: &App,
    i: &Interaction,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    o: &[CommandDataOption],
    out: &mut Vec<Embed>,
) -> Result<String> {
    // Every reply is an entry (`reply.tag.*`) on the Bot messages page.
    let mut say = async |key: &str, vars: &[(&str, &str)]| {
        let said = crate::reply::say(app, gid, key, vars).await;
        *out = said.embeds;
        Ok(said.text)
    };
    let owner = app.guild_info(gid).await?.owner;
    let level = app.level(gid, owner, user).await?;
    if !app.cfg(gid.get()).await?.misc.use_slash
        && !matches!(
            level,
            brim_core::permits::Level::Owner | brim_core::permits::Level::ExtraOwner
        )
    {
        return say("reply.tag.slash_off", &[]).await;
    }
    if !level.has("tags") {
        return say("reply.tag.no_permit", &[]).await;
    }
    if !app.cfg(gid.get()).await?.modules.messages {
        return say("reply.tag.module_off", &[]).await;
    }
    let Some(ch) = i.channel.as_ref() else {
        return say("reply.tag.use_in_channel", &[]).await;
    };
    let channel = ch.id.get() as i64;
    // Owner decision 2026-11-18: only where the member may write themselves (not #rules or #announcements).
    // Discord lets anyone run a slash command in a read-only channel. This limits where it posts,
    // it never gives rights: those come from the permit above.
    let send = if ch.kind.is_thread() {
        Permissions::SEND_MESSAGES_IN_THREADS
    } else {
        Permissions::SEND_MESSAGES
    };
    let can = i.member.as_ref().and_then(|m| m.permissions);
    if !can.is_some_and(|p| p.contains(send) || p.contains(Permissions::ADMINISTRATOR)) {
        return say("reply.tag.cannot_send", &[]).await;
    }
    let tag = crate::slash::string(o, "name")
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let g = gid.get() as i64;
    let embed: Option<i64> =
        sqlx::query_scalar("select id from embeds where guild_id = $1 and tag = $2")
            .bind(g)
            .bind(&tag)
            .fetch_optional(&app.pool)
            .await?;
    let entry = BUILT_IN.iter().find(|(t, _)| *t == tag).map(|(_, k)| *k);
    if embed.is_none() && entry.is_none() {
        return say("reply.tag.not_found", &[("tag", &tag)]).await;
    }
    let entry = if embed.is_some() { None } else { entry };
    let name = i
        .member
        .as_ref()
        .and_then(|m| m.user.as_ref())
        .map_or_else(|| user.to_string(), |u| u.name.clone());
    // One server at a time, so two quick uses cannot both pass the rate limits.
    let mut tx = app.pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('message_tag_runs'), $1::int)")
        .bind((g % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    let run: Option<i64> = sqlx::query_scalar(
        "insert into message_tag_runs (guild_id, embed_id, channel_id, member_id, member_name, entry)
         select $1, $2, $3, $4, $5, $6
         where not exists(select 1 from message_tag_runs where guild_id = $1 and member_id = $4
                          and created_at > now() - interval '10 seconds')
           and not exists(select 1 from message_tag_runs where guild_id = $1 and embed_id is not distinct from $2
                          and entry is not distinct from $6 and channel_id = $3 and created_at > now() - interval '30 seconds')
           and (select count(*) from message_tag_runs where guild_id = $1
                and created_at > now() - interval '1 minute') < 10
         returning id",
    )
    .bind(g)
    .bind(embed)
    .bind(channel)
    .bind(user.get() as i64)
    .bind(&name)
    .bind(entry)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(run) = run else {
        return say("reply.tag.slow_down", &[]).await;
    };
    sqlx::query("select pg_notify('message_runs', '')")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if let Some(r) = result(app, run, g, WAIT).await? {
        return Ok(r);
    }
    // Not claimed yet: take it back, so it can never post after this reply says it did not.
    let dropped = sqlx::query(
        "update message_tag_runs set claimed_at = now(), result = 'timed out'
         where id = $1 and guild_id = $2 and claimed_at is null",
    )
    .bind(run)
    .bind(g)
    .execute(&app.pool)
    .await?
    .rows_affected();
    if dropped == 1 {
        return say("reply.tag.no_answer", &[]).await;
    }
    // Claimed and being posted: wait for its result once more.
    match result(app, run, g, WAIT).await? {
        Some(r) => Ok(r),
        None => say("reply.tag.queued", &[("tag", &tag)]).await,
    }
}

/// The dashboard's result for a run, read until it is there or `wait` has passed.
async fn result(app: &App, run: i64, g: i64, wait: Duration) -> Result<Option<String>> {
    let start = tokio::time::Instant::now();
    while start.elapsed() < wait {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let r: Option<String> = sqlx::query_scalar(
            "select result from message_tag_runs where id = $1 and guild_id = $2",
        )
        .bind(run)
        .bind(g)
        .fetch_optional(&app.pool)
        .await?
        .flatten();
        if r.is_some() {
            return Ok(r);
        }
    }
    Ok(None)
}

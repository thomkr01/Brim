//! Invite Tracking storage (migration 3940): the join journal, one credit per member per server,
//! and the links Brim made for members. Every query is scoped to one guild.
use std::collections::HashMap;

use sqlx::PgPool;

/// How a join was tied to an invite. Only `Single` and `Group` can give credit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// One join, one invite grew by one.
    Single,
    /// N joins together, one invite grew by N.
    Group,
    /// A bot added through OAuth: no invite.
    Bot,
    Unknown,
}

impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            How::Single => "single",
            How::Group => "group",
            How::Bot => "bot",
            How::Unknown => "unknown",
        }
    }
}

pub struct Join<'a> {
    pub guild_id: i64,
    pub user_id: i64,
    /// Discord's `joined_at` in ms: with the user it names one membership, so a replayed join is skipped.
    pub joined_at_ms: i64,
    pub code: Option<&'a str>,
    pub inviter_id: Option<i64>,
    pub how: How,
    /// Why the invite is unknown (`no_baseline`, `ambiguous`, `fetch_failed`, ...).
    pub reason: Option<&'a str>,
}

impl Join<'_> {
    /// Credit needs a sure enough match and an inviter who is not the member.
    pub fn creditable(&self) -> bool {
        matches!(self.how, How::Single | How::Group)
            && self.inviter_id.is_some_and(|i| i != self.user_id)
    }
}

/// Saves a batch of joins in one transaction. A join already in the journal changes nothing; a
/// rejoin only moves `last_joined_at` and `joins`, never the first credit.
pub async fn record_joins(pool: &PgPool, joins: &[Join<'_>]) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for j in joins {
        let new: Option<i64> = sqlx::query_scalar(
            "INSERT INTO invite_joins (guild_id, user_id, joined_at, code, inviter_id, how, reason)
             VALUES ($1, $2, to_timestamp($3 / 1000.0), $4, $5, $6, $7)
             ON CONFLICT (guild_id, user_id, joined_at) DO NOTHING RETURNING id",
        )
        .bind(j.guild_id)
        .bind(j.user_id)
        .bind(j.joined_at_ms as f64)
        .bind(j.code)
        .bind(j.inviter_id)
        .bind(j.how.as_str())
        .bind(j.reason)
        .fetch_optional(&mut *tx)
        .await?;
        if new.is_none() {
            continue;
        }
        sqlx::query(
            "INSERT INTO invite_members
                 (guild_id, user_id, first_joined_at, last_joined_at, inviter_id, code, how, creditable)
             VALUES ($1, $2, to_timestamp($3 / 1000.0), to_timestamp($3 / 1000.0), $4, $5, $6, $7)
             ON CONFLICT (guild_id, user_id) DO UPDATE SET
                 present = CASE WHEN EXCLUDED.last_joined_at >= invite_members.last_joined_at
                           THEN true ELSE invite_members.present END,
                 last_joined_at = GREATEST(invite_members.last_joined_at, EXCLUDED.last_joined_at),
                 joins = invite_members.joins + 1",
        )
        .bind(j.guild_id)
        .bind(j.user_id)
        .bind(j.joined_at_ms as f64)
        .bind(j.inviter_id)
        .bind(j.code)
        .bind(j.how.as_str())
        .bind(j.creditable())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// A member left. The tracker handles a guild's events in gateway order, so a later rejoin is
/// written after this.
pub async fn record_leave(pool: &PgPool, guild: i64, user: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE invite_members SET present = false, left_at = now()
         WHERE guild_id = $1 AND user_id = $2 AND present",
    )
    .bind(guild)
    .bind(user)
    .execute(pool)
    .await
    .map(drop)
}

#[derive(Debug, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Stats {
    pub total: i64,
    pub present: i64,
    pub left: i64,
}

/// The members `inviter` got credit for.
pub async fn stats(pool: &PgPool, guild: i64, inviter: i64) -> sqlx::Result<Stats> {
    sqlx::query_as(
        "SELECT count(*) AS total, count(*) FILTER (WHERE present) AS present,
                count(*) FILTER (WHERE NOT present) AS left
         FROM invite_members WHERE guild_id = $1 AND inviter_id = $2 AND creditable",
    )
    .bind(guild)
    .bind(inviter)
    .fetch_one(pool)
    .await
}

/// Inviters by credited members, most first, ties by lowest id. `limit` is capped at 25.
pub async fn top(
    pool: &PgPool,
    guild: i64,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<(i64, Stats)>> {
    let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT inviter_id, count(*), count(*) FILTER (WHERE present), count(*) FILTER (WHERE NOT present)
         FROM invite_members WHERE guild_id = $1 AND creditable
         GROUP BY inviter_id ORDER BY count(*) DESC, inviter_id ASC
         LIMIT LEAST($2, 25) OFFSET GREATEST($3, 0)",
    )
    .bind(guild)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, total, present, left)| {
            (
                id,
                Stats {
                    total,
                    present,
                    left,
                },
            )
        })
        .collect())
}

/// Number of inviters with credit, for paging `top`.
pub async fn inviters(pool: &PgPool, guild: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(DISTINCT inviter_id) FROM invite_members WHERE guild_id = $1 AND creditable",
    )
    .bind(guild)
    .fetch_one(pool)
    .await
}

#[derive(Debug, sqlx::FromRow)]
pub struct Member {
    pub inviter_id: Option<i64>,
    pub code: Option<String>,
    pub how: String,
    pub creditable: bool,
    pub present: bool,
    pub first_joined_ms: i64,
    pub joins: i32,
}

/// Who invited `user` (their first join the tracker saw).
pub async fn who(pool: &PgPool, guild: i64, user: i64) -> sqlx::Result<Option<Member>> {
    sqlx::query_as(
        "SELECT inviter_id, code, how, creditable, present,
                (extract(epoch FROM first_joined_at) * 1000)::BIGINT AS first_joined_ms, joins
         FROM invite_members WHERE guild_id = $1 AND user_id = $2",
    )
    .bind(guild)
    .bind(user)
    .fetch_optional(pool)
    .await
}

/// (first join the tracker saw in ms, joins of the last 7 days whose invite is unknown).
pub async fn summary(pool: &PgPool, guild: i64) -> sqlx::Result<(Option<i64>, i64)> {
    sqlx::query_as(
        "SELECT (SELECT (extract(epoch FROM min(first_joined_at)) * 1000)::BIGINT
                 FROM invite_members WHERE guild_id = $1),
                (SELECT count(*) FROM invite_joins WHERE guild_id = $1 AND how = 'unknown'
                 AND recorded_at > now() - interval '7 days')",
    )
    .bind(guild)
    .fetch_one(pool)
    .await
}

/// Saves a link Brim made for `owner`; it is credited to them, not to the bot.
pub async fn save_link(
    pool: &PgPool,
    guild: i64,
    code: &str,
    owner: i64,
    channel: i64,
    max_uses: i32,
    expires_ms: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO invite_links (guild_id, code, owner_id, channel_id, max_uses, expires_at)
         VALUES ($1, $2, $3, $4, $5, to_timestamp($6 / 1000.0))",
    )
    .bind(guild)
    .bind(code)
    .bind(owner)
    .bind(channel)
    .bind(max_uses)
    .bind(expires_ms.map(|m| m as f64))
    .execute(pool)
    .await
    .map(drop)
}

/// The owners of the given codes among the links Brim made.
pub async fn link_owners(
    pool: &PgPool,
    guild: i64,
    codes: &[String],
) -> sqlx::Result<HashMap<String, i64>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT code, owner_id FROM invite_links WHERE guild_id = $1 AND code = ANY($2)",
    )
    .bind(guild)
    .bind(codes)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// `owner`'s links that still work: (code, expiry in ms), newest first.
pub async fn live_links(
    pool: &PgPool,
    guild: i64,
    owner: i64,
) -> sqlx::Result<Vec<(String, Option<i64>)>> {
    sqlx::query_as(
        "SELECT code, (extract(epoch FROM expires_at) * 1000)::BIGINT FROM invite_links
         WHERE guild_id = $1 AND owner_id = $2 AND deleted_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())
         ORDER BY created_at DESC",
    )
    .bind(guild)
    .bind(owner)
    .fetch_all(pool)
    .await
}

/// When `owner`'s newest link was made, in ms.
pub async fn last_link_ms(pool: &PgPool, guild: i64, owner: i64) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT (extract(epoch FROM max(created_at)) * 1000)::BIGINT FROM invite_links
         WHERE guild_id = $1 AND owner_id = $2",
    )
    .bind(guild)
    .bind(owner)
    .fetch_one(pool)
    .await
}

/// Links made with `/invites create` in the last minute, in the whole guild.
pub async fn recent_links(pool: &PgPool, guild: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM invite_links WHERE guild_id = $1 AND created_at > now() - interval '1 minute'",
    )
    .bind(guild)
    .fetch_one(pool)
    .await
}

/// Marks links gone: the one deleted (`INVITE_DELETE`), or every one missing from a full invite
/// list. Only links made before `before_ms` (when that list was asked for): a newer one is not in it.
pub async fn links_gone(
    pool: &PgPool,
    guild: i64,
    gone: Option<&str>,
    live: &[String],
    before_ms: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE invite_links SET deleted_at = now()
         WHERE guild_id = $1 AND deleted_at IS NULL
           AND (CASE WHEN $2::TEXT IS NULL
                THEN code <> ALL($3) AND created_at < to_timestamp($4 / 1000.0)
                ELSE code = $2 END)",
    )
    .bind(guild)
    .bind(gone)
    .bind(live)
    .bind(before_ms as f64)
    .execute(pool)
    .await
    .map(drop)
}

/// The journal keeps 90 days (PRIVACY.md); the first-join credits stay.
pub async fn prune(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM invite_joins WHERE recorded_at < now() - interval '90 days'")
        .execute(pool)
        .await
        .map(drop)
}

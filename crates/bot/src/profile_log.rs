//! The "User Avatar Changed" and "User Name Changed" logs. Discord's member update carries only
//! the new values, so each member's last seen username, display name and avatar hash are kept in
//! `profile_snapshot` (migration 4044). Stored only while the server sends one of the two logs
//! somewhere, for every member (the daily member list read, [`sweep`]); a row goes when the member
//! leaves, 90 days after the member was last seen (`retention.rs`), with the person's data delete request (`core::persondata`) and with the server.
use anyhow::Result;
use brim_logging::{
    embed::{avatar_url, Log, BLUE},
    LogEvent, LogKind,
};
use sqlx::types::chrono::{DateTime, Utc};
use std::{collections::HashMap, time::Instant};
use twilight_model::{
    gateway::payload::incoming::MemberUpdate,
    id::{
        marker::{GuildMarker, UserMarker},
        Id,
    },
    user::User,
};

use crate::app::App;

/// How often a server's whole member list is read again.
const SYNC_SECS: u64 = 86_400;

/// (username, display name, avatar hash).
pub(crate) type Seen = (String, Option<String>, Option<String>);

fn seen(u: &User) -> Seen {
    (
        u.name.clone(),
        u.global_name.clone(),
        u.avatar.map(|h| h.to_string()),
    )
}

/// Whether the server sends one of the two logs somewhere.
async fn routed(app: &App, guild: Id<GuildMarker>) -> Result<bool> {
    let cfg = app.cfg(guild.get()).await?;
    Ok([LogEvent::UserAvatarUpdate, LogEvent::UserNameUpdate]
        .iter()
        .any(|e| cfg.logging.target(LogKind::User, Some(*e)).is_some()))
}

/// Hourly from `welcome::purge_left`: reads the whole member list of each server that sends one of
/// the two logs somewhere, once a day (`synced` is in memory, so a restart reads once more, and a
/// server that turns the logs on is read within the hour). So a member's first change is logged
/// too, and a leave missed while the bot was offline is caught. `only`: one server (tests).
pub async fn sweep(app: &App, synced: &mut HashMap<u64, Instant>, only: Option<i64>) -> Result<()> {
    let guilds: Vec<i64> = sqlx::query_scalar(
        "SELECT guild_id FROM guild_owners WHERE guild_id NOT IN (SELECT guild_id FROM guild_left)
           AND ($1::bigint IS NULL OR guild_id = $1)",
    )
    .bind(only)
    .fetch_all(&app.pool)
    .await?;
    for g in guilds {
        let due = synced
            .get(&(g as u64))
            .is_none_or(|t| t.elapsed().as_secs() >= SYNC_SECS);
        if !due || !routed(app, Id::new(g as u64)).await.unwrap_or(false) {
            continue;
        }
        synced.insert(g as u64, Instant::now());
        if let Err(e) = sync(app, Id::new(g as u64)).await {
            tracing::warn!(
                ?e,
                guild = g,
                "reading the member list for the name logs failed"
            );
        }
    }
    Ok(())
}

/// Stores every member as seen now, then deletes the rows of everyone not listed (they left).
/// The delete runs only after a complete paging: a cut-short list would drop members' rows.
/// Nothing is logged here: a change made while the bot was offline only updates the row.
/// One request per 1,000 members, one after the other, so Discord's rate limit is kept.
async fn sync(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let g = gid.get() as i64;
    let now = || sqlx::query_scalar::<_, DateTime<Utc>>("SELECT now()").fetch_one(&app.pool);
    let start = now().await?;
    let mut after = None;
    loop {
        // A row an event wrote after this page was asked for is newer than the page: left alone.
        let asked = now().await?;
        let req = app.http.guild_members(gid).limit(1000);
        let req = match after {
            Some(a) => req.after(a),
            None => req,
        };
        let page = req.await?.models().await?;
        let (mut ids, mut names, mut globals, mut avatars) = (vec![], vec![], vec![], vec![]);
        for u in page.iter().map(|m| &m.user).filter(|u| u.id != app.me) {
            let (name, global, avatar) = seen(u);
            ids.push(u.id.get() as i64);
            names.push(name);
            globals.push(global);
            avatars.push(avatar);
        }
        sqlx::query(
            "INSERT INTO profile_snapshot (guild_id, user_id, username, global_name, avatar_hash)
             SELECT $1, * FROM UNNEST($2::bigint[], $3::text[], $4::text[], $5::text[])
             ON CONFLICT (guild_id, user_id) DO UPDATE
             SET username = EXCLUDED.username, global_name = EXCLUDED.global_name,
                 avatar_hash = EXCLUDED.avatar_hash, seen_at = now()
             WHERE profile_snapshot.seen_at < $6",
        )
        .bind(g)
        .bind(&ids)
        .bind(&names)
        .bind(&globals)
        .bind(&avatars)
        .bind(asked)
        .execute(&app.pool)
        .await?;
        match page.last() {
            Some(m) if page.len() == 1000 => after = Some(m.user.id),
            _ => break,
        }
    }
    sqlx::query("DELETE FROM profile_snapshot WHERE guild_id = $1 AND seen_at < $2")
        .bind(g)
        .bind(start)
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// Stores what `user` looks like now and returns what was stored before. `None`: this is the first
/// sight of the member (the member list has not been read yet), or the server sends neither log anywhere (then nothing is stored).
async fn swap(app: &App, guild: Id<GuildMarker>, user: &User) -> Result<Option<Seen>> {
    if !routed(app, guild).await? {
        return Ok(None);
    }
    let (name, global, avatar) = seen(user);
    // `old` reads the row as it was before this statement. An unchanged row is not written again.
    // FOR UPDATE: events run at once, so a second update of the member waits and then reads the
    // first one's values (else both would log the same change).
    Ok(sqlx::query_as(
        "WITH old AS (SELECT username, global_name, avatar_hash FROM profile_snapshot
                      WHERE guild_id = $1 AND user_id = $2 FOR UPDATE),
         new AS (INSERT INTO profile_snapshot (guild_id, user_id, username, global_name, avatar_hash)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (guild_id, user_id) DO UPDATE
                 SET username = EXCLUDED.username, global_name = EXCLUDED.global_name,
                     avatar_hash = EXCLUDED.avatar_hash, seen_at = now()
                 WHERE (profile_snapshot.username, profile_snapshot.global_name, profile_snapshot.avatar_hash)
                       IS DISTINCT FROM (EXCLUDED.username, EXCLUDED.global_name, EXCLUDED.avatar_hash))
         SELECT username, global_name, avatar_hash FROM old",
    )
    .bind(guild.get() as i64)
    .bind(user.id.get() as i64)
    .bind(name)
    .bind(global)
    .bind(avatar)
    .fetch_optional(&app.pool)
    .await?)
}

/// A join is the first sight of a member: stored, never logged.
pub async fn on_join(app: &App, guild: Id<GuildMarker>, user: &User) -> Result<()> {
    swap(app, guild, user).await.map(drop)
}

/// Logs what changed since the last sight. The ignore lists apply to the member (they made the change).
pub async fn on_member_update(app: &App, m: &MemberUpdate) -> Result<()> {
    let Some(old) = swap(app, m.guild_id, &m.user).await? else {
        return Ok(());
    };
    let new = seen(&m.user);
    let id = m.user.id;
    if old.2 != new.2 {
        let log = avatar_log(id.get(), old.2.as_deref(), new.2.as_deref());
        send(app, m.guild_id, LogEvent::UserAvatarUpdate, id, log).await?;
    }
    if (&old.0, &old.1) != (&new.0, &new.1) {
        send(
            app,
            m.guild_id,
            LogEvent::UserNameUpdate,
            id,
            name_log(id.get(), &old, &new),
        )
        .await?;
    }
    Ok(())
}

async fn send(
    app: &App,
    guild: Id<GuildMarker>,
    event: LogEvent,
    user: Id<UserMarker>,
    log: Log,
) -> Result<()> {
    crate::events::send_event_log(app, guild, LogKind::User, event, user, log).await
}

/// A member who left has no row here any more.
pub async fn on_leave(app: &App, guild: Id<GuildMarker>, user: Id<UserMarker>) -> Result<()> {
    sqlx::query("DELETE FROM profile_snapshot WHERE guild_id = $1 AND user_id = $2")
        .bind(guild.get() as i64)
        .bind(user.get() as i64)
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// The old picture is the small one, the new one the large one. Discord serves an old avatar only
/// for a while, so the old picture can be empty in an older log.
pub(crate) fn avatar_log(user: u64, old: Option<&str>, new: Option<&str>) -> Log {
    let (old, new) = (avatar_url(user, old), avatar_url(user, new));
    Log::new("User Avatar Changed", BLUE)
        .fact("Member", "member", &brim_logging::embed::user(user))
        .line("Old Avatar", &format!("[small picture]({old})"))
        .line("New Avatar", &format!("[large picture]({new})"))
        .var("before", &old)
        .var("after", &new)
        .pictures(&old, &new)
}

/// Names go in code blocks (`Log::diff`), so markdown or a link in a name shows as typed.
pub(crate) fn name_log(user: u64, old: &Seen, new: &Seen) -> Log {
    let mut log = Log::new("User Name Changed", BLUE).fact(
        "Member",
        "member",
        &brim_logging::embed::user(user),
    );
    let head = log.lines_text().len();
    if old.0 != new.0 {
        log = log.section("Username").diff(Some(&old.0), Some(&new.0));
    }
    if old.1 != new.1 {
        let none = |n: &Option<String>| n.clone().unwrap_or_else(|| "(none)".into());
        log = log
            .section("Display Name")
            .diff(Some(&none(&old.1)), Some(&none(&new.1)));
    }
    let changes = log.lines_text()[head..]
        .trim_start_matches('\n')
        .to_string();
    log.var("changes", &changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::T;
    use serde_json::json;

    fn update(guild: Id<GuildMarker>, user: u64, name: &str, avatar: Option<&str>) -> MemberUpdate {
        serde_json::from_value(json!({
            "guild_id": guild.to_string(), "roles": [], "deaf": false, "mute": false,
            "joined_at": "2026-01-01T00:00:00.000000+00:00", "pending": false, "avatar": null,
            "communication_disabled_until": null, "nick": null, "premium_since": null,
            "user": {"id": user.to_string(), "username": name, "global_name": "Shown",
                     "discriminator": "0", "avatar": avatar},
        }))
        .unwrap()
    }

    /// The first sight only stores; a change logs once with the old value; an ignored member and a
    /// server with no channel for the logs get none, and that server stores nothing; a leave
    /// removes the row.
    #[tokio::test]
    async fn a_change_is_logged_against_the_last_seen_values() {
        let Some(t) = T::new(701).await else { return };
        let (user, quiet) = (t.base + 5, t.base + 6);
        t.exec("DELETE FROM profile_snapshot WHERE guild_id BETWEEN $1 AND $1 + 9")
            .await;
        t.server_config(
            0,
            t.owner(),
            json!({"logging": {"fallback": "55", "ignored_users": [quiet.to_string()]}}),
        )
        .await;
        t.server_config(1, t.owner(), json!({})).await;
        let g0 = t.g(0).get();
        t.set(|m| {
            m.text_channels.insert(55, (g0, false));
        });
        let (a, b) = (
            "0123456789abcdef0123456789abcdef",
            "fedcba9876543210fedcba9876543210",
        );
        let up = |g: u64, u: u64, name: &'static str, av: Option<&'static str>| {
            let app = t.app.clone();
            let m = update(t.g(g), u, name, av);
            async move { on_member_update(&app, &m).await.unwrap() }
        };
        let logs = || (t.calls("User Avatar Changed"), t.calls("User Name Changed"));
        up(0, user, "old", Some(a)).await;
        up(0, user, "old", Some(a)).await;
        assert_eq!(logs(), (0, 0), "the first sight and no change log nothing");
        up(0, user, "old", Some(b)).await;
        assert_eq!(logs(), (1, 0));
        assert_eq!(t.calls(&format!("avatars/{user}/{a}.png")), 1, "old avatar");
        up(0, user, "new", Some(b)).await;
        assert_eq!(logs(), (1, 1));
        up(0, user, "new", None).await;
        assert_eq!(logs(), (2, 1), "a removed avatar is a change");
        // The ignore lists hide a member's own change; the new values are still stored.
        up(0, quiet, "old", None).await;
        up(0, quiet, "new", None).await;
        assert_eq!(logs(), (2, 1));
        // No channel for the logs: nothing is stored at all.
        up(1, user, "old", None).await;
        up(1, user, "new", None).await;
        on_join(&t.app, t.g(1), &update(t.g(1), user, "n", None).user)
            .await
            .unwrap();
        assert_eq!(logs(), (2, 1));
        let rows = || {
            t.count("SELECT count(*) FROM profile_snapshot WHERE guild_id BETWEEN $1 AND $1 + 9")
        };
        assert_eq!(rows().await, 2);
        // A join stores the first sight, so the next change is logged.
        let joiner = t.base + 7;
        on_join(&t.app, t.g(0), &update(t.g(0), joiner, "j", None).user)
            .await
            .unwrap();
        up(0, joiner, "j2", None).await;
        assert_eq!(logs(), (2, 2));
        for u in [user, quiet, joiner] {
            on_leave(&t.app, t.g(0), Id::new(u)).await.unwrap();
        }
        assert_eq!(rows().await, 0, "a leave removes the row");
        // The member list read stores members never seen before, so their first change is logged,
        // keeps the others as they are, and drops the row of a member no longer in the list.
        let gone = t.base + 8;
        up(0, user, "kept", None).await;
        on_join(&t.app, t.g(0), &update(t.g(0), gone, "g", None).user)
            .await
            .unwrap();
        t.set(|m| {
            m.members.insert((g0, user), vec![]);
            m.members.insert((g0, quiet + 20), vec![]);
        });
        sync(&t.app, t.g(0)).await.unwrap();
        let names: Vec<(i64, String)> = sqlx::query_as(
            "SELECT user_id, username FROM profile_snapshot WHERE guild_id = $1 ORDER BY 1",
        )
        .bind(g0 as i64)
        .fetch_all(&t.app.pool)
        .await
        .unwrap();
        assert_eq!(
            names,
            [(user as i64, "u".into()), (quiet as i64 + 20, "u".into())]
        );
        up(0, quiet + 20, "first change", None).await;
        assert_eq!(
            logs(),
            (2, 3),
            "a member only read from the list has an old value"
        );
        // The hourly sweep reads a server with a channel for the logs once a day, others never.
        let mut synced = HashMap::new();
        t.exec("DELETE FROM profile_snapshot WHERE guild_id BETWEEN $1 AND $1 + 9")
            .await;
        for g in [g0, t.g(1).get()] {
            sweep(&t.app, &mut synced, Some(g as i64)).await.unwrap();
        }
        assert_eq!(rows().await, 2);
        assert!(synced.contains_key(&g0) && !synced.contains_key(&t.g(1).get()));
        t.exec("DELETE FROM profile_snapshot WHERE guild_id BETWEEN $1 AND $1 + 9")
            .await;
        sweep(&t.app, &mut synced, Some(g0 as i64)).await.unwrap();
        assert_eq!(rows().await, 0, "not read again within the day");
        t.cleanup().await;
    }

    #[test]
    fn a_name_with_markdown_shows_as_typed() {
        let old = ("a".to_string(), None, None);
        let new = (
            "a".to_string(),
            Some("[x](https://e.example)".to_string()),
            None,
        );
        let e = name_log(5, &old, &new).build();
        let d = e.description.unwrap();
        assert!(
            d.contains("```diff\n- (none)\n```")
                && d.contains("```diff\n+ [x](https://e.example)\n```")
        );
        assert!(!d.contains("Username"), "only what changed is shown");
    }
}

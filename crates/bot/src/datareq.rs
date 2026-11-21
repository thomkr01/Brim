//! `/mydata show` and `/mydata delete` in a DM with the bot: a person asks to see or delete what
//! Brim stores about them (security review, owner decision 2026-11-17). The bot owner approves or
//! denies it on the dashboard, which queues a `data_request` job; the bot does it and DMs the person.
use anyhow::{Context, Result};
use brim_core::persondata::{self, PLACES};
use serde_json::{json, Map, Value};
use twilight_model::{
    application::{
        command::{Command, CommandType},
        interaction::{application_command::CommandData, Interaction, InteractionContextType},
    },
    http::attachment::Attachment,
    id::Id,
};
use twilight_util::builder::command::{CommandBuilder, SubCommandBuilder};

use crate::app::App;

pub const KIND: &str = "data_request";

pub fn definition() -> Command {
    CommandBuilder::new(
        "mydata",
        "Ask to see or delete what Brim stores about you",
        CommandType::ChatInput,
    )
    .contexts([InteractionContextType::BotDm])
    .option(SubCommandBuilder::new(
        "show",
        "Get a file with what Brim stores about you",
    ))
    .option(SubCommandBuilder::new(
        "delete",
        "Ask Brim to delete what it stores about you",
    ))
    .build()
}

/// Stores the request; at most one open request of each kind, and one new one a day.
pub async fn command(app: &App, i: &Interaction, data: &CommandData) -> Result<String> {
    if i.guild_id.is_some() {
        return Ok("Use this in a DM with Brim.".into());
    }
    let user = i.author().context("no user on the interaction")?;
    let kind = match data.options.first().map(|o| o.name.as_str()) {
        Some("show") => "show",
        Some("delete") => "delete",
        _ => return Ok("Pick show or delete.".into()),
    };
    let recent: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM data_requests WHERE user_id = $1 AND kind = $2
           AND (status IN ('pending', 'approved') OR created_at > now() - interval '1 day'))",
    )
    .bind(user.id.get() as i64)
    .bind(kind)
    .fetch_one(&app.pool)
    .await?;
    if recent {
        return Ok("You already asked for this in the last day. The bot owner answers it on the dashboard, and you get a DM when it is done.".into());
    }
    let r = sqlx::query("INSERT INTO data_requests (user_id, username, kind) VALUES ($1, $2, $3)")
        .bind(user.id.get() as i64)
        .bind(&user.name)
        .bind(kind)
        .execute(&app.pool)
        .await;
    // Two presses at once: the open-request index takes one.
    if let Err(e) = r {
        if !e
            .as_database_error()
            .is_some_and(|d| d.is_unique_violation())
        {
            return Err(e.into());
        }
    }
    Ok(match kind {
        "show" => "Request sent. The bot owner checks it, then Brim DMs you a file with what it stores about you.",
        _ => "Request sent. The bot owner checks it, then Brim deletes what it stores about you as a member and DMs you what was deleted and what is kept.",
    }
    .into())
}

/// `data_request` {id, by}: runs only while `by` is still the bot owner (security review).
pub async fn run_job(app: &App, p: &Value) -> Result<()> {
    let id = p["id"].as_i64().context("job payload has no id")?;
    let by = p["by"].as_str().and_then(|b| b.parse::<u64>().ok());
    let row: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT user_id, kind, status FROM data_requests WHERE id = $1 AND status IN ('approved', 'denied') AND done_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&app.pool)
    .await?;
    let Some((user, kind, status)) = row else {
        return Ok(()); // done already (a retried job)
    };
    // An unknown bot owner (the startup lookup failed) never becomes known in this run: an
    // error would only retry until the job is dropped, so the request goes back to pending.
    if by.is_none() || app.bot_owner.map(|o| o.get()) != by {
        tracing::warn!(
            id,
            ?by,
            "dropped a data request job: its asker is not the bot owner, or the bot owner is unknown"
        );
        reopen(&app.pool, id).await?;
        return Ok(());
    }
    let uid = Id::new(user as u64);
    let what = if kind == "show" { "see" } else { "delete" };
    let (text, file, result) = match (status.as_str(), kind.as_str()) {
        ("denied", _) => (
            format!("The bot owner declined your request to {what} your data. You can ask again in a day with `/mydata`, or reach the bot owner through the bot's support link."),
            None,
            Value::Null,
        ),
        (_, "show") => {
            let data = collect(&app.pool, user).await?;
            let counts = counts_of(&data);
            let bytes = serde_json::to_vec_pretty(&data)?;
            let file = Attachment::from_bytes("brim-your-data.json".into(), bytes, 0);
            (
                "Here is what Brim stores about you, one list per table. Staff members' and reporters' ids, report reasons and the text of staff notes are left out. Log files are deleted after 24 hours, so they are not in it.".into(),
                Some(file),
                counts,
            )
        }
        _ => {
            let counts = erase(&app.pool, user).await?;
            (
                "Brim deleted what it stores about you as a member: cases, notes, reports, appeals, closed tickets, strikes, invites, verification and join records, your stored name and avatar (a server you are still in stores them again within a day while its avatar and name logs are on), and your dashboard login. Your id is also removed from older config versions. A report or appeal staff have not handled yet, an open ticket and a warning that still counts stay until they end and are removed then. A server's newest case and ticket stay without your name, so their numbers are not reused. Kept: Raider List entries (they have their own appeal), appeal and report blacklists, staff lists, your dashboard 2FA and a running quarantine. Copies in servers' Discord log channels are the servers' own, and database backups expire within 30 days.".into(),
                None,
                counts,
            )
        }
    };
    // Recorded before the DM: a retry must not delete again or send a second file.
    sqlx::query("UPDATE data_requests SET status = CASE WHEN status = 'approved' THEN 'done' ELSE status END, done_at = now(), result = $2 WHERE id = $1")
        .bind(id)
        .bind(&result)
        .execute(&app.pool)
        .await?;
    let sent = async {
        let ch = app
            .http
            .create_private_channel(uid)
            .await?
            .model()
            .await?
            .id;
        let embeds = crate::reply::frame_embeds(app, "dm.data_request", &text).await;
        let msg = app.http.create_message(ch).embeds(&embeds);
        match &file {
            Some(f) => msg.attachments(std::slice::from_ref(f)).await?,
            None => msg.await?,
        };
        anyhow::Ok(())
    };
    if let Err(e) = sent.await {
        tracing::info!(?e, user, "data request: DM not sent");
        if file.is_some() {
            sqlx::query("UPDATE data_requests SET status = 'failed' WHERE id = $1")
                .bind(id)
                .execute(&app.pool)
                .await?;
        }
    }
    Ok(())
}

/// Back to pending, so the bot owner can answer it again: an approved or denied request without a
/// job blocks a new `/mydata` and the dashboard only answers pending ones.
async fn reopen(pool: &sqlx::PgPool, id: i64) -> Result<()> {
    sqlx::query("UPDATE data_requests SET status = 'pending', decided_by = NULL, decided_at = NULL WHERE id = $1 AND status IN ('approved', 'denied') AND done_at IS NULL")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The scheduler gave up on a `data_request` job (it failed every try).
pub async fn gave_up(app: &App, job: &brim_core::jobs::Job) {
    let Some(id) = job.payload["id"].as_i64() else {
        return;
    };
    if let Err(e) = reopen(&app.pool, id).await {
        tracing::warn!(
            ?e,
            id,
            "data request: could not reopen after the job gave up"
        );
    }
}

/// {table.column: [rows]} of every place shown to the person.
async fn collect(pool: &sqlx::PgPool, user: i64) -> Result<Value> {
    let mut out = Map::new();
    for p in PLACES.iter().filter(|p| p.show) {
        let rows: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(persondata::show_sql(p)))
            .bind(user)
            .fetch_one(pool)
            .await?;
        if rows.as_array().is_some_and(|a| a.len() >= 5000) {
            out.insert(
                format!("{}.{} (first 5000 rows only)", p.table, p.col),
                rows,
            );
        } else if rows.as_array().is_some_and(|a| !a.is_empty()) {
            out.insert(format!("{}.{}", p.table, p.col), rows);
        }
    }
    Ok(Value::Object(out))
}

fn counts_of(data: &Value) -> Value {
    let m = data.as_object().into_iter().flatten();
    Value::Object(
        m.map(|(k, v)| (k.clone(), json!(v.as_array().map_or(0, Vec::len))))
            .collect(),
    )
}

/// Deletes in one transaction and returns {table.column: rows deleted}.
async fn erase(pool: &sqlx::PgPool, user: i64) -> Result<Value> {
    let mut tx = pool.begin().await?;
    let mut out = Map::new();
    for p in PLACES {
        let Some(sql) = persondata::delete_sql(p) else {
            continue;
        };
        let n = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if n > 0 {
            out.insert(format!("{}.{}", p.table, p.col), json!(n));
        }
    }
    for q in persondata::ANONYMIZE {
        sqlx::query(*q).bind(user).execute(&mut *tx).await?;
    }
    let n = sqlx::query(persondata::SCRUB_CONFIG_HISTORY)
        .bind(format!("\"{user}\""))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n > 0 {
        out.insert("guild_config (older versions)".into(), json!(n));
    }
    tx.commit().await?;
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sweep_kept` covers every server, so two tests sweeping at once lock each other's rows
    /// (deadlock in CI); the tests that sweep take turns.
    static SWEEP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Every listed table and column exists, and a delete leaves the kept places and others' rows.
    #[tokio::test]
    async fn show_and_erase_cover_the_list() {
        let Some(t) = crate::test_support::T::new(108).await else {
            return;
        };
        let (g, pool) = (t.g(0).get() as i64, &t.app.pool);
        let (me, other) = (t.base as i64 + 5, t.base as i64 + 6);
        for (u, case) in [(me, 1), (other, 2)] {
            sqlx::query("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, reason) VALUES ($1, $2, 'warn', $3, $4, 'r')")
                .bind(g)
                .bind(case)
                .bind(u)
                .bind(other)
                .execute(pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO report_blacklist (guild_id, user_id, added_by) VALUES ($1, $2, $3)",
        )
        .bind(g)
        .bind(me)
        .bind(other)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO guild_config (guild_id, version, config) VALUES ($1, 1, $2), ($1, 2, '{}')")
            .bind(g)
            .bind(json!({"tickets": {"blocked": [me.to_string()]}}))
            .execute(pool)
            .await
            .unwrap();
        // The person's newest case stays without their name: its proofs (their message) and its
        // history (old reasons) go. Another member's proof stays.
        sqlx::query("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, reason) VALUES ($1, 3, 'ban', $2, $3, 'r')")
            .bind(g)
            .bind(me)
            .bind(other)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "WITH p AS (INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, verified)
                        VALUES ($1, 1, 'text', 'mine', 1, true), ($1, 3, 'text', 'mine', 1, true), ($1, 2, 'text', 'theirs', 1, true))
             INSERT INTO case_edits (guild_id, case_no, field, old_value, new_value, editor_id)
             VALUES ($1, 3, 'reason', 'said by me', 'r', 1), ($1, 2, 'reason', 'x', 'r', 1)",
        )
        .bind(g)
        .execute(pool)
        .await
        .unwrap();
        // The stored name and avatar (avatar and name change logs): shown, then deleted.
        sqlx::query(
            "INSERT INTO profile_snapshot (guild_id, user_id, username, avatar_hash) VALUES ($1, $2, 'me', 'h'), ($1, $3, 'other', NULL)
             ON CONFLICT DO NOTHING",
        )
        .bind(g)
        .bind(me)
        .bind(other)
        .execute(pool)
        .await
        .unwrap();
        let data = collect(pool, me).await.unwrap();
        assert_eq!(data["profile_snapshot.user_id"][0]["username"], "me");
        let case = &data["cases.user_id"][0];
        assert_eq!(case["reason"], "r");
        assert!(case.get("mod_id").is_none(), "staff ids are left out");
        assert!(data["report_blacklist.user_id"][0]
            .get("added_by")
            .is_none());
        let done = erase(pool, me).await.unwrap();
        assert_eq!(done["cases.user_id"], 1);
        assert_eq!(done["profile_snapshot.user_id"], 1);
        let names: Vec<String> =
            sqlx::query_scalar("SELECT username FROM profile_snapshot WHERE guild_id = $1")
                .bind(g)
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(names, ["other"], "another member's stored name stays");
        assert_eq!(done["guild_config (older versions)"], 1);
        let left: Vec<i64> =
            sqlx::query_scalar("SELECT user_id FROM cases WHERE guild_id = $1 ORDER BY case_no")
                .bind(g)
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(left, [other, 0]);
        let proofs: Vec<(i32, String)> = sqlx::query_as(
            "SELECT case_no, body FROM case_proofs WHERE guild_id = $1
             UNION ALL SELECT case_no, old_value FROM case_edits WHERE guild_id = $1 ORDER BY 2",
        )
        .bind(g)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(proofs, [(2, "theirs".into()), (2, "x".into())]);
        let kept: i64 =
            sqlx::query_scalar("SELECT count(*) FROM report_blacklist WHERE guild_id = $1")
                .bind(g)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(kept, 1, "blacklists are kept");
        let old: Value = sqlx::query_scalar(
            "SELECT config FROM guild_config WHERE guild_id = $1 AND version = 1",
        )
        .bind(g)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(old["tickets"]["blocked"][0], "0");
        for q in [
            "DELETE FROM cases WHERE guild_id = $1",
            "DELETE FROM report_blacklist WHERE guild_id = $1",
            "DELETE FROM guild_config WHERE guild_id = $1",
            "DELETE FROM profile_snapshot WHERE guild_id = $1",
        ] {
            sqlx::query(q).bind(g).execute(pool).await.unwrap();
        }
    }

    /// A warn that still counts stays the member's through a delete, and goes once it has ended.
    #[tokio::test]
    async fn a_delete_keeps_a_warn_that_counts_until_it_ends() {
        let _turn = SWEEP.lock().await;
        let Some(t) = crate::test_support::T::new(978).await else {
            return;
        };
        let (g, pool) = (t.g(0).get() as i64, &t.app.pool);
        let (me, other) = (t.base as i64 + 5, t.base as i64 + 6);
        let run = |sql: &'static str| sqlx::query(sql).bind(g).bind(me).bind(other).execute(pool);
        // An ended warn (1), a warn that counts (3), and a newer case after it.
        run("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id, points, expires_at) VALUES ($1, 1, 'warn', $2, $3, 1, NULL), ($1, 2, 'kick', $3, $3, 0, NULL), ($1, 3, 'warn', $2, $3, 2, now() + interval '1 day'), ($1, 4, 'kick', $3, $3, 0, NULL)").await.unwrap();
        let cases = || async {
            let q = "SELECT case_no, user_id FROM cases WHERE guild_id = $1 ORDER BY case_no";
            let r: Vec<(i32, i64)> = sqlx::query_as(q).bind(g).fetch_all(pool).await.unwrap();
            r
        };
        let points = || brim_core::cases::warn_points(pool, g, me);
        let sweep = || brim_core::persondata::sweep_kept(pool);
        erase(pool, me).await.unwrap();
        assert_eq!(cases().await, [(2, other), (3, me), (4, other)]);
        assert_eq!(points().await.unwrap(), 2);
        // The request is done: the warn stays while it counts; a case made later is new data.
        run("INSERT INTO data_requests (user_id, username, kind, status, done_at) VALUES ($2, 'm', 'delete', 'done', now())").await.unwrap();
        sweep().await.unwrap();
        assert_eq!(points().await.unwrap(), 2);
        run("INSERT INTO cases (guild_id, case_no, kind, user_id, mod_id) VALUES ($1, 5, 'kick', $2, $3)").await.unwrap();
        run("UPDATE cases SET expires_at = now() - interval '1 second' WHERE guild_id = $1 AND case_no = 3").await.unwrap();
        sweep().await.unwrap();
        assert_eq!(cases().await, [(2, other), (4, other), (5, me)]);
        run("DELETE FROM cases WHERE guild_id = $1").await.unwrap();
        run("DELETE FROM data_requests WHERE user_id = $2")
            .await
            .unwrap();
    }

    /// What a delete kept until it ended goes once it has ended; what came after the request stays.
    #[tokio::test]
    async fn a_done_delete_takes_kept_items_once_handled() {
        let _turn = SWEEP.lock().await;
        let Some(t) = crate::test_support::T::new(979).await else {
            return;
        };
        let (g, pool) = (t.g(0).get() as i64, &t.app.pool);
        let (me, other) = (t.base as i64 + 5, t.base as i64 + 6);
        let run = |sql: &'static str| sqlx::query(sql).bind(g).bind(me).bind(other).execute(pool);
        let count = || async {
            let q = "SELECT (SELECT count(*) FROM reports WHERE guild_id = $1),
                            (SELECT count(*) FROM appeals WHERE guild_id = $1),
                            (SELECT count(*) FROM tickets WHERE guild_id = $1 AND user_id = $2)";
            let r: (i64, i64, i64) = sqlx::query_as(q)
                .bind(g)
                .bind(me)
                .fetch_one(pool)
                .await
                .unwrap();
            r
        };
        for sql in [
            // Open: a report about them (message 1), one by them (2), an appeal, a ticket.
            "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, message_id, content) VALUES ($1, $3, $2, 1, 1, 'x'), ($1, $2, $3, 1, 2, 'x')",
            "INSERT INTO appeals (guild_id, user_id, username, kind, answers) VALUES ($1, $2, 'u', 'ban', '[]')",
            "INSERT INTO tickets (guild_id, number, user_id, channel_id) VALUES ($1, 1, $2, $1 + 1), ($1, 2, $3, $1 + 2)",
        ] {
            run(sql).await.unwrap();
        }
        erase(pool, me).await.unwrap();
        run("INSERT INTO data_requests (user_id, username, kind, status, done_at) VALUES ($2, 'm', 'delete', 'done', now())").await.unwrap();
        let sweep = || brim_core::persondata::sweep_kept(pool);
        sweep().await.unwrap();
        assert_eq!(count().await, (2, 1, 1), "open items stay");
        // Staff handle them; a report made after the request is new data and stays.
        for sql in [
            "UPDATE reports SET status = 'dismissed' WHERE guild_id = $1",
            "UPDATE appeals SET status = 'denied' WHERE guild_id = $1",
            "UPDATE tickets SET closed_at = now() WHERE guild_id = $1 AND user_id = $2",
            "INSERT INTO reports (guild_id, reporter_id, target_id, channel_id, message_id, content, status) VALUES ($1, $3, $2, 1, 3, 'x', 'dismissed')",
        ] {
            run(sql).await.unwrap();
        }
        sweep().await.unwrap();
        assert_eq!(count().await, (1, 0, 0));
        for sql in [
            "DELETE FROM reports WHERE guild_id = $1",
            "DELETE FROM tickets WHERE guild_id = $1",
            "DELETE FROM data_requests WHERE user_id = $2",
        ] {
            run(sql).await.unwrap();
        }
    }

    /// A dropped job puts the request back to pending; a done one stays done.
    #[tokio::test]
    async fn gave_up_reopens() {
        let Some(t) = crate::test_support::T::new(109).await else {
            return;
        };
        let pool = &t.app.pool;
        let user = t.base as i64 + 7;
        let id: i64 = sqlx::query_scalar("INSERT INTO data_requests (user_id, username, kind, status, decided_by, decided_at) VALUES ($1, 'x', 'delete', 'approved', 1, now()) RETURNING id")
            .bind(user)
            .fetch_one(pool)
            .await
            .unwrap();
        let job = |id: i64| brim_core::jobs::Job {
            id: 0,
            guild_id: 0,
            kind: KIND.into(),
            payload: json!({"id": id, "by": "1"}),
            attempts: 5,
        };
        gave_up(&t.app, &job(id)).await;
        let st: String = sqlx::query_scalar("SELECT status FROM data_requests WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(st, "pending");
        sqlx::query("UPDATE data_requests SET status = 'done', done_at = now() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        gave_up(&t.app, &job(id)).await;
        let st: String = sqlx::query_scalar("SELECT status FROM data_requests WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(st, "done");
        sqlx::query("DELETE FROM data_requests WHERE user_id = $1")
            .bind(user)
            .execute(pool)
            .await
            .unwrap();
    }
}

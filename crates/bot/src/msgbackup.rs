//! The Message Builder setup and the edited bot messages in a backup (plan "default bot messages",
//! section 6): saved messages with their tags, schedules, trigger rules and stickies, and the
//! `bot_messages` copies. Loading a backup replaces the current set with the backup's. What only
//! records what happened (Sent log, counts, posted copies, run times) is not kept: a kept row
//! keeps its own, a row the backup brings back starts without.
//! Channel ids move to remade channels with the rest of the restore (`msgrules::relink`); a rule or
//! sticky whose channel is gone is switched off with the reason on its card.
use anyhow::Result;
use brim_logging::botmsg;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use twilight_model::id::{marker::GuildMarker, Id};

use crate::app::App;

/// The copy stored next to the backup image. Only setup columns, named one by one, so a column a
/// later version adds is simply left at its default when an older backup is loaded.
pub async fn copy(app: &App, gid: Id<GuildMarker>) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "select jsonb_build_object(
           'embeds', (select coalesce(jsonb_agg(jsonb_build_object('id', id, 'name', name, 'message', message,
                'channel_id', channel_id, 'webhook', webhook, 'pings', pings, 'saved_level', saved_level,
                'saved_at', saved_at, 'tag', tag, 'updated_by', updated_by, 'updated_at', updated_at,
                'category_id', category_id, 'position', position)), '[]')
              from embeds where guild_id = $1),
           'categories', (select coalesce(jsonb_agg(jsonb_build_object('id', id, 'name', name,
                'position', position, 'updated_by', updated_by, 'updated_at', updated_at)), '[]')
              from message_categories where guild_id = $1),
           'rules', (select coalesce(jsonb_agg(jsonb_build_object('id', id, 'embed_id', embed_id, 'kind', kind,
                'enabled', enabled, 'channel_id', channel_id, 'spec', spec, 'issue', issue,
                'created_by', created_by, 'updated_by', updated_by, 'updated_at', updated_at)), '[]')
              from message_rules where guild_id = $1),
           'stickies', (select coalesce(jsonb_agg(jsonb_build_object('id', id, 'embed_id', embed_id,
                'channel_id', channel_id, 'position', position, 'bots', bots, 'enabled', enabled, 'issue', issue,
                'created_by', created_by, 'updated_by', updated_by, 'updated_at', updated_at)), '[]')
              from sticky_msgs where guild_id = $1),
           'bot_messages', (select coalesce(jsonb_agg(jsonb_build_object('key', key, 'message', message,
                'updated_by', updated_by, 'updated_at', updated_at)), '[]')
              from bot_messages where guild_id = $1))",
    )
    .bind(gid.get() as i64)
    .fetch_one(&app.pool)
    .await?)
}

/// How many saved messages a stored copy holds, for the backup info.
pub fn count(copy: &Value) -> usize {
    copy["embeds"].as_array().map_or(0, Vec::len)
}

/// The bot message rows of a backup that may come back, with today's variable names: one without a
/// required placeholder (saved before the save check) is left out, the same check as a manual save
/// and a kit import.
fn bot_messages_to_load(rows: &Value) -> Value {
    // A backup made before the variable names changed holds the old names (`{prize}`): rename
    // first, then check, so the check reads today's names.
    let renamed = |r: &Value| {
        let mut r = r.clone();
        if let Some(k) = r["key"].as_str().map(str::to_owned) {
            r["message"] = botmsg::to_public(&k, &r["message"]);
        }
        r
    };
    let ok = |r: &Value| {
        r["key"]
            .as_str()
            .is_some_and(|k| botmsg::missing_text(k, &r["message"]).is_none())
    };
    Value::Array(
        rows.as_array()
            .into_iter()
            .flatten()
            .map(renamed)
            .filter(ok)
            .collect(),
    )
}

/// Replaces the server's message setup with `copy`, in one transaction. Ids stay as they were, so
/// rows that still exist keep their Sent log and posted copies. Each security text the load
/// changes gets its own dashboard log line with both versions, under `by`.
pub async fn load(app: &App, gid: Id<GuildMarker>, copy: &Value, by: u64) -> Result<()> {
    let g = gid.get() as i64;
    let part = |k: &str| match &copy[k] {
        Value::Array(_) => copy[k].clone(),
        _ => Value::Array(vec![]),
    };
    let mut tx = app.pool.begin().await?;
    // Tags are unique per server: clear them first, so two messages that swap tags do not clash.
    sqlx::query("update embeds set tag = null where guild_id = $1")
        .bind(g)
        .execute(&mut *tx)
        .await?;
    // Categories first, so the messages below can point to them. A backup from before categories
    // has none: the server keeps its own, and its messages keep theirs.
    let cats = copy["categories"].is_array();
    if cats {
        sqlx::query(
            "delete from message_categories where guild_id = $1
               and id not in (select (e->>'id')::bigint from jsonb_array_elements($2) e)",
        )
        .bind(g)
        .bind(part("categories"))
        .execute(&mut *tx)
        .await?;
        // Every name is freed first, so swapped names cannot clash (names are unique per server).
        // A leading space is a name the dashboard cannot make; the upsert below sets each name back.
        sqlx::query("update message_categories set name = ' ' || id where guild_id = $1")
            .bind(g)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "insert into message_categories (id, guild_id, name, position, updated_by, updated_at)
             select id, $1, name, coalesce(position, 0), updated_by, coalesce(updated_at, now())
             from jsonb_populate_recordset(null::message_categories, $2)
             on conflict (id) do update set name = excluded.name, position = excluded.position,
                 updated_by = excluded.updated_by, updated_at = excluded.updated_at
             where message_categories.guild_id = excluded.guild_id",
        )
        .bind(g)
        .bind(part("categories"))
        .execute(&mut *tx)
        .await?;
    }
    // A deleted message takes its rules, stickies and Sent log with it (foreign keys).
    sqlx::query(
        "delete from embeds where guild_id = $1
           and id not in (select (e->>'id')::bigint from jsonb_array_elements($2) e)",
    )
    .bind(g)
    .bind(part("embeds"))
    .execute(&mut *tx)
    .await?;
    // `where embeds.guild_id = excluded.guild_id`: an id another server holds is never touched.
    sqlx::query(
        "insert into embeds (id, guild_id, name, message, channel_id, webhook, pings, saved_level, saved_at,
                             tag, updated_by, updated_at, category_id, position)
         select b.id, $1, b.name, b.message, b.channel_id, b.webhook, coalesce(b.pings, 'none'),
                coalesce(b.saved_level, 0), b.saved_at, b.tag, b.updated_by, coalesce(b.updated_at, now()),
                case when $3 then c.id else (select category_id from embeds k where k.id = b.id and k.guild_id = $1) end,
                case when $3 then coalesce(b.position, 0)
                     else coalesce((select position from embeds k where k.id = b.id and k.guild_id = $1), 0) end
         from jsonb_populate_recordset(null::embeds, $2) b
         left join message_categories c on c.id = b.category_id and c.guild_id = $1
         on conflict (id) do update set name = excluded.name, message = excluded.message,
             channel_id = excluded.channel_id, webhook = excluded.webhook, pings = excluded.pings,
             saved_level = excluded.saved_level, saved_at = excluded.saved_at, tag = excluded.tag,
             updated_by = excluded.updated_by, updated_at = excluded.updated_at,
             category_id = excluded.category_id, position = excluded.position
         where embeds.guild_id = excluded.guild_id",
    )
    .bind(g)
    .bind(part("embeds"))
    .bind(cats)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "delete from message_rules where guild_id = $1
           and id not in (select (e->>'id')::bigint from jsonb_array_elements($2) e)",
    )
    .bind(g)
    .bind(part("rules"))
    .execute(&mut *tx)
    .await?;
    let rules: Vec<(i64, Value)> = sqlx::query_as(
        "insert into message_rules (id, embed_id, guild_id, kind, enabled, channel_id, spec, issue,
                                    created_by, updated_by, updated_at)
         select r.id, r.embed_id, $1, r.kind, r.enabled, r.channel_id, r.spec, r.issue, r.created_by,
                r.updated_by, coalesce(r.updated_at, now())
         from jsonb_populate_recordset(null::message_rules, $2) r
         where r.embed_id in (select id from embeds where guild_id = $1)
         on conflict (id) do update set embed_id = excluded.embed_id, kind = excluded.kind,
             enabled = excluded.enabled, channel_id = excluded.channel_id, spec = excluded.spec,
             issue = excluded.issue, updated_by = excluded.updated_by, updated_at = excluded.updated_at,
             next_at = null, due_messages = false, waiting_once = false
         where message_rules.guild_id = excluded.guild_id
         returning id, spec",
    )
    .bind(g)
    .bind(part("rules"))
    .fetch_all(&mut *tx)
    .await?;
    // Clock runs go on from their next time, never at once (as `msgrules::relink` does).
    for (id, spec) in rules {
        sqlx::query(
            "update message_rules set next_at = to_timestamp($2)
             where id = $1 and enabled and kind = 'schedule'",
        )
        .bind(id)
        .bind(brim_core::msgsched::next_of(&spec).map(|n| n as f64))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "delete from sticky_msgs where guild_id = $1
           and id not in (select (e->>'id')::bigint from jsonb_array_elements($2) e)",
    )
    .bind(g)
    .bind(part("stickies"))
    .execute(&mut *tx)
    .await?;
    // A sticky that moves channel leaves its old copy: Brim only deletes the copy it knows.
    sqlx::query(
        "insert into sticky_msgs (id, embed_id, guild_id, channel_id, position, bots, enabled, issue,
                                  created_by, updated_by, updated_at)
         select s.id, s.embed_id, $1, s.channel_id, coalesce(s.position, 1), coalesce(s.bots, false),
                s.enabled, s.issue, s.created_by, s.updated_by, coalesce(s.updated_at, now())
         from jsonb_populate_recordset(null::sticky_msgs, $2) s
         where s.embed_id in (select id from embeds where guild_id = $1)
         on conflict (id) do update set embed_id = excluded.embed_id, channel_id = excluded.channel_id,
             position = excluded.position, bots = excluded.bots, enabled = excluded.enabled,
             issue = excluded.issue, updated_by = excluded.updated_by, updated_at = excluded.updated_at,
             posted_id = case when sticky_msgs.channel_id = excluded.channel_id then sticky_msgs.posted_id end,
             send_id = case when sticky_msgs.channel_id = excluded.channel_id then sticky_msgs.send_id end,
             bot_streak = 0
         where sticky_msgs.guild_id = excluded.guild_id",
    )
    .bind(g)
    .bind(part("stickies"))
    .execute(&mut *tx)
    .await?;
    // A save between the "before" read and the delete would be logged as the wrong old version.
    sqlx::query("lock table bot_messages in share row exclusive mode")
        .execute(&mut *tx)
        .await?;
    let security = botmsg::security_keys();
    let security_rows =
        "select key, message from bot_messages where guild_id = $1 and key = any($2)";
    let before: HashMap<String, Value> = sqlx::query_as(security_rows)
        .bind(g)
        .bind(&security)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
    sqlx::query("delete from bot_messages where guild_id = $1")
        .bind(g)
        .execute(&mut *tx)
        .await?;
    // A revert's undo copy belongs to the set the load replaced.
    sqlx::query("delete from bot_messages_undo where guild_id = $1")
        .bind(g)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "insert into bot_messages (guild_id, key, message, updated_by, updated_at)
         select $1, key, message, updated_by, coalesce(updated_at, now())
         from jsonb_populate_recordset(null::bot_messages, $2)
         where message is not null and key <> all($3)",
    )
    .bind(g)
    .bind(bot_messages_to_load(&part("bot_messages")))
    .bind(
        botmsg::BOT_WIDE
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<String>>(),
    )
    .execute(&mut *tx)
    .await?;
    let after: HashMap<String, Value> = sqlx::query_as(security_rows)
        .bind(g)
        .bind(&security)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
    for d in botmsg::defs().iter().filter(|d| d.security) {
        let (old, new) = (before.get(&d.key), after.get(&d.key));
        if old != new {
            sqlx::query(
                "insert into dashboard_audit (guild_id, user_id, text) values ($1, $2, $3)",
            )
            .bind(g)
            .bind(by as i64)
            .bind(format!(
                "<@{by}> {}",
                botmsg::change_line(d, "loaded a backup that replaced", old, new)
            ))
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// After the restore: rules and stickies that point at one of the `gone` channels are switched
/// off, with the reason on their card.
pub async fn pause_gone(app: &App, gid: Id<GuildMarker>, gone: &HashSet<u64>) -> Result<()> {
    let used: Vec<i64> = sqlx::query_scalar(
        "select distinct c from (
           select channel_id c from message_rules where guild_id = $1 and channel_id is not null
           union select channel_id from sticky_msgs where guild_id = $1
           union select x::bigint from message_rules, jsonb_array_elements_text(spec->'channels') x
             where guild_id = $1 and kind = 'trigger' and jsonb_typeof(spec->'channels') = 'array'
               and x ~ '^[0-9]+$') u",
    )
    .bind(gid.get() as i64)
    .fetch_all(&app.pool)
    .await?;
    for ch in used
        .into_iter()
        .map(|c| c as u64)
        .filter(|c| gone.contains(c))
    {
        crate::msgrules::channel_deleted(app, gid, ch, None).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only with a scratch Postgres: a load brings back the backup's set, drops what came
    /// after it, keeps the posted copy of a row that stayed, and a gone channel pauses its rows.
    #[tokio::test]
    async fn a_load_replaces_the_message_setup() {
        let Some(t) = crate::test_support::T::new(997).await else {
            return;
        };
        let (gid, g, p) = (t.g(0), t.g(0).get() as i64, &t.app.pool);
        let embed: i64 = sqlx::query_scalar(
            "insert into embeds (guild_id, name, message, updated_by, tag) values ($1, 'Rules', '{}', 1, 'rules') returning id",
        )
        .bind(g)
        .fetch_one(p)
        .await
        .unwrap();
        sqlx::query("with c as (insert into message_categories (guild_id, name, position, updated_by) values ($1, 'Info', 2, 1) returning id)
                     update embeds set category_id = (select id from c), position = 3 where id = $2")
            .bind(g).bind(embed).execute(p).await.unwrap();
        sqlx::query("insert into message_rules (embed_id, guild_id, kind, channel_id, spec, created_by, updated_by) values ($1, $2, 'trigger', null, '{\"channels\": [\"5\"]}', 1, 1)")
            .bind(embed).bind(g).execute(p).await.unwrap();
        sqlx::query("insert into sticky_msgs (embed_id, guild_id, channel_id, created_by, updated_by, posted_id) values ($1, $2, 5, 1, 1, 77)")
            .bind(embed).bind(g).execute(p).await.unwrap();
        sqlx::query("insert into bot_messages (guild_id, key, message, updated_by) values ($1, 'dm.punish.ban', '{\"content\": \"old\"}', 1)")
            .bind(g).execute(p).await.unwrap();
        let saved = copy(&t.app, gid).await.unwrap();
        assert_eq!(count(&saved), 1);

        // After the backup: the category is deleted (its message goes to Uncategorized) and another
        // takes its name.
        sqlx::query("delete from message_categories where guild_id = $1")
            .bind(g)
            .execute(p)
            .await
            .unwrap();
        sqlx::query(
            "insert into message_categories (guild_id, name, updated_by) values ($1, 'Info', 1)",
        )
        .bind(g)
        .execute(p)
        .await
        .unwrap();
        sqlx::query("update embeds set name = 'Changed', tag = null where id = $1")
            .bind(embed)
            .execute(p)
            .await
            .unwrap();
        sqlx::query("insert into embeds (guild_id, name, message, updated_by, tag) values ($1, 'Later', '{}', 1, 'rules')")
            .bind(g).execute(p).await.unwrap();
        sqlx::query(
            "update bot_messages set message = '{\"content\": \"new\"}' where guild_id = $1",
        )
        .bind(g)
        .execute(p)
        .await
        .unwrap();
        // A security text edited after the backup: the load puts the default back and logs both.
        sqlx::query("insert into bot_messages (guild_id, key, message, updated_by) values ($1, 'dm.security', '{\"content\": \"later\"}', 1)")
            .bind(g).execute(p).await.unwrap();
        load(&t.app, gid, &saved, 42).await.unwrap();
        let lines: Vec<(i64, String)> =
            sqlx::query_as("select user_id, text from dashboard_audit where guild_id = $1")
                .bind(g)
                .fetch_all(p)
                .await
                .unwrap();
        assert_eq!(
            lines.len(),
            1,
            "one line per changed security text, none for others: {lines:?}"
        );
        assert_eq!(lines[0].0, 42);
        assert!(
            lines[0]
                .1
                .starts_with("<@42> loaded a backup that replaced the security text"),
            "{}",
            lines[0].1
        );
        assert!(
            lines[0].1.contains("later") && lines[0].1.ends_with("now the default"),
            "{}",
            lines[0].1
        );

        let names: Vec<(String, Option<String>)> =
            sqlx::query_as("select name, tag from embeds where guild_id = $1")
                .bind(g)
                .fetch_all(p)
                .await
                .unwrap();
        assert_eq!(names, [("Rules".into(), Some("rules".into()))]);
        let home: (String, i32, i32) = sqlx::query_as(
            "select c.name, c.position, e.position from embeds e join message_categories c on c.id = e.category_id
             where e.guild_id = $1",
        )
        .bind(g)
        .fetch_one(p)
        .await
        .unwrap();
        assert_eq!(
            home,
            ("Info".into(), 2, 3),
            "the category and the order come back"
        );
        let cats: i64 =
            sqlx::query_scalar("select count(*) from message_categories where guild_id = $1")
                .bind(g)
                .fetch_one(p)
                .await
                .unwrap();
        assert_eq!(cats, 1, "the newer category is not in the backup");
        let sticky: (Option<i64>, bool) =
            sqlx::query_as("select posted_id, enabled from sticky_msgs where guild_id = $1")
                .bind(g)
                .fetch_one(p)
                .await
                .unwrap();
        assert_eq!(sticky, (Some(77), true), "a kept sticky keeps its copy");
        let msg: Value = sqlx::query_scalar("select message from bot_messages where guild_id = $1")
            .bind(g)
            .fetch_one(p)
            .await
            .unwrap();
        assert_eq!(msg["content"], "old");

        pause_gone(&t.app, gid, &HashSet::from([5])).await.unwrap();
        let (on, issue): (bool, Option<String>) =
            sqlx::query_as("select enabled, issue from sticky_msgs where guild_id = $1")
                .bind(g)
                .fetch_one(p)
                .await
                .unwrap();
        assert!(!on && issue.is_some_and(|i| i.contains("deleted")));
        let rule_issue: Option<String> =
            sqlx::query_scalar("select issue from message_rules where guild_id = $1")
                .bind(g)
                .fetch_one(p)
                .await
                .unwrap();
        assert!(rule_issue.is_some_and(|i| i.contains("(id 5)")));

        for q in [
            "delete from embeds where guild_id = $1",
            "delete from message_categories where guild_id = $1",
            "delete from bot_messages where guild_id = $1",
            "delete from dashboard_audit where guild_id = $1",
        ] {
            sqlx::query(q).bind(g).execute(p).await.unwrap();
        }
        t.cleanup().await;
    }

    /// Runs only with a scratch Postgres: names swapped after the backup (even to "B (5)" style
    /// names) do not stop the load.
    #[tokio::test]
    async fn swapped_category_names_still_load() {
        let Some(t) = crate::test_support::T::new(996).await else {
            return;
        };
        let (gid, g, p) = (t.g(0), t.g(0).get() as i64, &t.app.pool);
        let ids: Vec<i64> = sqlx::query_scalar(
            "insert into message_categories (guild_id, name, updated_by) values ($1, 'A', 1), ($1, 'B', 1) returning id",
        )
        .bind(g)
        .fetch_all(p)
        .await
        .unwrap();
        let saved = copy(&t.app, gid).await.unwrap();
        let rename = |id: i64, name: String| {
            sqlx::query("update message_categories set name = $2 where id = $1")
                .bind(id)
                .bind(name)
                .execute(p)
        };
        rename(ids[1], format!("B ({})", ids[0])).await.unwrap();
        rename(ids[0], "B".into()).await.unwrap();
        load(&t.app, gid, &saved, 42).await.unwrap();
        let names: Vec<String> = sqlx::query_scalar(
            "select name from message_categories where guild_id = $1 order by id",
        )
        .bind(g)
        .fetch_all(p)
        .await
        .unwrap();
        assert_eq!(names, ["A", "B"]);
        for q in [
            "delete from message_categories where guild_id = $1",
            "delete from dashboard_audit where guild_id = $1",
        ] {
            sqlx::query(q).bind(g).execute(p).await.unwrap();
        }
        t.cleanup().await;
    }
}

//! Tickets 2.0 on the dashboard: publish a panel message, list tickets, and ticket stats.
//! These tables hold no message text, and these routes never return any.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    embeds, store, ApiError, AppState,
};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use brim_core::sections::{panel_message, TicketPanel, TicketsCfg};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub(crate) struct Publish {
    expected_version: i32,
    channel: String,
    panels: Vec<String>,
    menu: bool,
}

/// Posts a panel message and records it, so the bot accepts its buttons.
pub(crate) async fn publish(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Publish>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let (cur, config) = store::current(&st.db, guild).await?;
    if cur != b.expected_version {
        return Err(ApiError::Conflict(cur));
    }
    let bad = |m: &str| ApiError::BadRequest(m.into());
    let t: TicketsCfg = serde_json::from_value(config.get("tickets").cloned().unwrap_or(json!({})))
        .map_err(|_| bad("the ticket settings cannot be read"))?;
    if !t.enabled {
        return Err(bad("turn on tickets first"));
    }
    if b.panels.is_empty() {
        return Err(bad("choose at least one panel"));
    }
    let mut picked: Vec<&TicketPanel> = vec![];
    for id in &b.panels {
        if picked.iter().any(|p| &p.id == id) {
            return Err(bad("a panel is chosen twice"));
        }
        match t.panels.iter().find(|p| &p.id == id && p.enabled) {
            Some(p) => picked.push(p),
            None => return Err(bad("a chosen panel does not exist or is turned off")),
        }
    }
    // Digits only: the id becomes part of the Discord path.
    let channel: u64 = b.channel.parse().map_err(|_| bad("bad channel"))?;
    // The bot is in many servers: never post into another server's channel.
    let ch = embeds::discord(&st, Method::GET, &format!("/channels/{channel}"), None).await?;
    let ok = ch.is_some_and(|c| {
        c["guild_id"].as_str() == Some(guild.to_string().as_str()) && c["type"] == 0
    });
    if !ok {
        return Err(bad("that is not a text channel of this server"));
    }
    let look: Option<Value> = sqlx::query_scalar(
        "select message from bot_messages where guild_id = $1 and key = 'ticket.panel'",
    )
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let body = panel_message(&picked, b.menu, look.as_ref());
    let posted = embeds::discord(
        &st,
        Method::POST,
        &format!("/channels/{channel}/messages"),
        Some(body),
    )
    .await?
    .ok_or_else(|| bad("that channel no longer exists"))?;
    let message: i64 = posted["id"]
        .as_str()
        .and_then(|m| m.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("no message id"))?;
    let ids: Vec<String> = picked.iter().map(|p| p.id.clone()).collect();
    let recorded = sqlx::query(
        "insert into ticket_panel_posts (message_id, guild_id, channel_id, panels, posted_by)
         values ($1, $2, $3, $4, $5)",
    )
    .bind(message)
    .bind(guild)
    .bind(channel as i64)
    .bind(&ids)
    .bind(s.user_id)
    .execute(&st.db)
    .await;
    if let Err(e) = recorded {
        // Unrecorded, its buttons would only be refused: take the message down again.
        let path = format!("/channels/{channel}/messages/{message}");
        if let Err(d) = embeds::discord(&st, Method::DELETE, &path, None).await {
            tracing::warn!(
                ?d,
                channel,
                message,
                "removing an unrecorded ticket panel failed"
            );
        }
        return Err(e.into());
    }
    api::log_change(
        &st,
        guild,
        s.user_id,
        &config,
        None,
        format!(
            "{} (<@{}>) published the ticket panels {} in <#{channel}>",
            s.username,
            s.user_id,
            ids.join(", ")
        ),
    )
    .await;
    Ok(Json(json!({"message": message.to_string()})))
}

/// The panel messages posted in this server, so staff can take old ones down.
pub(crate) async fn posts(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select message_id::text, channel_id::text from ticket_panel_posts where guild_id = $1 order by message_id",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|(m, c)| json!({"message": m, "channel": c}))
        .collect();
    Ok(Json(json!({"posts": rows})))
}

/// Deletes a panel message of this server and its record; its buttons stop working.
pub(crate) async fn unpost(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, message)): Path<(i64, i64)>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, true).await?;
    let channel: i64 = sqlx::query_scalar(
        "select channel_id from ticket_panel_posts where message_id = $1 and guild_id = $2",
    )
    .bind(message)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    // None: the message or channel is already gone, which is the goal.
    let path = format!("/channels/{channel}/messages/{message}");
    embeds::discord(&st, Method::DELETE, &path, None).await?;
    sqlx::query("delete from ticket_panel_posts where message_id = $1 and guild_id = $2")
        .bind(message)
        .bind(guild)
        .execute(&st.db)
        .await?;
    let (_, config) = store::current(&st.db, guild).await?;
    api::log_change(
        &st,
        guild,
        s.user_id,
        &config,
        None,
        format!(
            "{} (<@{}>) deleted a ticket panel message in <#{channel}>",
            s.username, s.user_id
        ),
    )
    .await;
    Ok(Json(json!({})))
}

#[derive(Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Status {
    #[default]
    Open,
    Closed,
}

#[derive(Deserialize)]
pub(crate) struct CasesQ {
    #[serde(default)]
    status: Status,
    #[serde(default)]
    page: u32,
}

const PAGE: i64 = 25;

type CaseRow = (
    i32,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    i16,
    Option<String>,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
);

/// One page of open or closed tickets, newest first.
pub(crate) async fn cases(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(q): Query<CasesQ>,
) -> Res {
    api::staff(&st, &s, guild, &["tickets"]).await?;
    let mut rows: Vec<CaseRow> = sqlx::query_as(
        "select t.number, t.user_id::text, t.source, t.panel, t.status, t.claimed_by::text,
                t.priority, t.channel_id::text, extract(epoch from t.opened_at)::bigint,
                extract(epoch from t.last_activity_at)::bigint,
                extract(epoch from t.closed_at)::bigint,
                tr.log_channel_id::text, tr.log_message_id::text
         from tickets t
         left join ticket_transcripts tr on tr.guild_id = t.guild_id and tr.number = t.number
         where t.guild_id = $1 and (t.closed_at is null) = $2
         order by t.number desc limit $3 offset $4",
    )
    .bind(guild)
    .bind(q.status == Status::Open)
    .bind(PAGE + 1)
    .bind(i64::from(q.page.min(10_000)) * PAGE)
    .fetch_all(&st.db)
    .await?;
    let more = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let transcript = match (r.11, r.12) {
                (Some(c), Some(m)) => json!({"channel": c, "message": m}),
                _ => Value::Null,
            };
            json!({
                "number": r.0, "user": r.1, "source": r.2, "panel": r.3, "status": r.4,
                "claimed_by": r.5, "priority": r.6, "channel": r.7, "opened_at": r.8,
                "last_activity_at": r.9, "closed_at": r.10, "transcript": transcript,
            })
        })
        .collect();
    Ok(Json(json!({"rows": rows, "more": more})))
}

/// Counts for the last 30 days, the open tickets now, and the median first staff response.
pub(crate) async fn stats(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::staff(&st, &s, guild, &["tickets"]).await?;
    let (opened, closed, open, median): (i64, i64, i64, Option<i64>) = sqlx::query_as(
        "select count(*) filter (where opened_at > now() - interval '30 days'),
                count(*) filter (where closed_at > now() - interval '30 days'),
                count(*) filter (where closed_at is null),
                round(percentile_cont(0.5) within group
                    (order by extract(epoch from first_staff_response_at - opened_at)::float8)
                    filter (where opened_at > now() - interval '30 days'
                            and first_staff_response_at is not null))::bigint
         from tickets where guild_id = $1",
    )
    .bind(guild)
    .fetch_one(&st.db)
    .await?;
    let panels: Vec<(Option<String>, i64)> = sqlx::query_as(
        "select panel, count(*) from tickets
         where guild_id = $1 and opened_at > now() - interval '30 days'
         group by panel order by count(*) desc, panel",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let panels: Vec<Value> = panels
        .into_iter()
        .map(|(panel, opened)| json!({"panel": panel, "opened": opened}))
        .collect();
    Ok(Json(json!({
        "opened": opened, "closed": closed, "open": open,
        "median_first_response": median, "panels": panels,
    })))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::g3_handler_tests::setup;

    /// A fake Discord: channel 801 is a text channel of `guild`, 802 is in another server,
    /// and a post answers a message whose id is the guild id
    /// (`message_id` is a primary key, so it must be unique per test run).
    /// Returns the address and the paths hit.
    pub(crate) async fn mock(guild: i64) -> (String, crate::Shared<String, Value>) {
        let seen: crate::Shared<String, Value> = Default::default();
        let (s1, s2, g) = (seen.clone(), seen.clone(), guild.to_string());
        let app = axum::Router::new()
            .route(
                "/channels/{c}",
                axum::routing::get(move |Path(c): Path<String>| {
                    s1.lock().unwrap().insert(format!("GET {c}"), Value::Null);
                    let g = if c == "801" { g.clone() } else { "1".into() };
                    async move { Json(json!({"id": c, "guild_id": g, "type": 0})) }
                }),
            )
            .route(
                "/channels/{c}/messages",
                axum::routing::post(move |Path(c): Path<String>, Json(b): Json<Value>| {
                    s2.lock().unwrap().insert(format!("POST {c}"), b);
                    async move { Json(json!({"id": guild.to_string()})) }
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        (api, seen)
    }

    pub(crate) async fn cleanup(st: &AppState, g: i64) {
        for t in ["tickets", "ticket_panel_posts", "guild_config"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        }
    }

    fn body(v: Value) -> Json<Publish> {
        Json(serde_json::from_value(v).unwrap())
    }

    #[tokio::test]
    async fn publish_checks_panels_and_channel_and_records_the_post() {
        let Some((mut st, s, g)) = setup(9_920_000_000).await else {
            return;
        };
        let (api, seen) = mock(g).await;
        st.discord.api = api;
        let config = json!({"tickets": {"enabled": true, "panels": [
            {"id": "help", "name": "Help", "style": "success", "emoji": "🎫"},
            {"id": "off", "name": "Off", "enabled": false},
        ]}});
        sqlx::query("insert into guild_config (guild_id, version, config) values ($1, 1, $2)")
            .bind(g)
            .bind(&config)
            .execute(&st.db)
            .await
            .unwrap();
        let try_ = |ch: &str, panels: Value| {
            publish(
                State(st.clone()),
                s.clone(),
                Path(g),
                body(
                    json!({"expected_version": 1, "channel": ch, "panels": panels, "menu": false}),
                ),
            )
        };
        for panels in [
            json!(["off"]),
            json!(["nope"]),
            json!([]),
            json!(["help", "help"]),
        ] {
            let r = try_("801", panels.clone()).await;
            assert!(matches!(r, Err(ApiError::BadRequest(_))), "{panels}: {r:?}");
        }
        let r = try_("802", json!(["help"])).await;
        assert!(
            matches!(r, Err(ApiError::BadRequest(_))),
            "other server: {r:?}"
        );
        assert!(!seen.lock().unwrap().contains_key("POST 802"));
        let Json(ok) = try_("801", json!(["help"])).await.unwrap();
        assert_eq!(ok, json!({"message": g.to_string()}));
        let sent = seen.lock().unwrap()["POST 801"].clone();
        let button = &sent["components"][0]["components"][0];
        assert_eq!(button["custom_id"], "tk2:open:help");
        assert_eq!(button["style"], 3);
        let row: (i64, i64, Vec<String>, i64) = sqlx::query_as(
            "select guild_id, channel_id, panels, posted_by from ticket_panel_posts where message_id = $1 and guild_id = $1",
        )
        .bind(g)
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert_eq!(row, (g, 801, vec!["help".to_string()], 7));
        cleanup(&st, g).await;
    }

    #[test]
    fn the_menu_has_one_option_per_panel() {
        let p: TicketPanel =
            serde_json::from_value(json!({"id": "a", "name": "A", "description": "d".repeat(100)}))
                .unwrap();
        let m = panel_message(&[&p, &p], true, None);
        let select = &m["components"][0]["components"][0];
        assert_eq!(select["custom_id"], "tk2:menu");
        assert_eq!(select["options"].as_array().unwrap().len(), 2);
        assert_eq!(select["options"][0]["value"], "a");
        // Six buttons need two rows of at most five.
        let m = panel_message(&[&p; 6], false, None);
        assert_eq!(m["components"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn cases_and_stats_show_only_this_server() {
        let Some((st, s, g)) = setup(9_930_000_000).await else {
            return;
        };
        // `other` has no access rows: it only holds tickets that must not show up.
        let other = g + 1;
        for (guild, number, closed) in [(g, 1, false), (g, 2, true), (other, 1, false)] {
            sqlx::query(
                "insert into tickets (guild_id, number, user_id, status, closed_at, first_staff_response_at)
                 values ($1, $2, 55, $3, case when $4 then now() end, now() + interval '90 seconds')",
            )
            .bind(guild)
            .bind(number)
            .bind(if closed { "closed" } else { "open" })
            .bind(closed)
            .execute(&st.db)
            .await
            .unwrap();
        }
        sqlx::query(
            "insert into ticket_transcripts (guild_id, number, log_channel_id, log_message_id, complete)
             values ($1, 2, 5, $1, true)",
        )
        .bind(g)
        .execute(&st.db)
        .await
        .unwrap();
        let page = |status: &str| {
            cases(
                State(st.clone()),
                s.clone(),
                Path(g),
                Query(serde_json::from_value(json!({"status": status})).unwrap()),
            )
        };
        let Json(open) = page("open").await.unwrap();
        assert_eq!(open["rows"].as_array().unwrap().len(), 1, "{open}");
        assert_eq!(open["rows"][0]["number"], 1);
        assert_eq!(open["rows"][0]["user"], "55");
        assert_eq!(open["rows"][0]["transcript"], Value::Null);
        assert_eq!(open["more"], false);
        let Json(closed) = page("closed").await.unwrap();
        assert_eq!(closed["rows"].as_array().unwrap().len(), 1, "{closed}");
        assert_eq!(
            closed["rows"][0]["transcript"],
            json!({"channel": "5", "message": g.to_string()})
        );
        let Json(n) = stats(State(st.clone()), s.clone(), Path(g)).await.unwrap();
        assert_eq!(n["opened"], 2, "{n}");
        assert_eq!(n["closed"], 1);
        assert_eq!(n["open"], 1);
        assert_eq!(n["median_first_response"], 90);
        assert_eq!(n["panels"], json!([{"panel": null, "opened": 2}]));
        cleanup(&st, g).await;
        cleanup(&st, other).await;
    }
}

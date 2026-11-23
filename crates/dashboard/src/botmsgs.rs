//! Bot messages: the server's own copies of Brim's built-in messages (`bot_messages`), edited
//! on the Message Builder. Anyone with dashboard access may look; Owner, Extra Owners and Trusted
//! Admins save, revert and undo (owner decision 2026-11-18). Security
//! texts a Trusted Admin with a fresh 2FA code, each change audited with the old and
//! new version (plan "all embeds editable", section 6). Locked texts are never saved; the
//! `botmsg::HIDDEN` ones are also left out of the list. A revert keeps the
//! removed copy 24 hours for Undo.
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    lockdown::audit,
    ApiError, AppState,
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    routing::{get, post},
    Json, Router,
};
use brim_core::giveaways;
use brim_logging::botmsg::{self, change_line, security_keys};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// The entry of the panel message in #open-a-ticket.
const PANEL: &str = "ticket.panel";

/// Tells the bot a server's ticket panel entry changed, so the panel message in
/// #open-a-ticket (`ticket.panel`) follows at once. The bot only re-renders it, so a lost or
/// forged notice does nothing harmful.
async fn ticket_panel_changed(st: &AppState, guild: i64) {
    if let Err(e) = sqlx::query("select pg_notify('ticket_panel', $1)")
        .bind(guild.to_string())
        .execute(&st.db)
        .await
    {
        tracing::warn!(?e, guild, "telling the bot about the ticket panel failed");
    }
}

/// The largest kit file the import takes.
const KIT_MAX: usize = 1 << 20;

/// One test DM per user in this time.
const TEST_GAP: Duration = Duration::from_secs(10);

pub fn routes() -> Router<AppState> {
    let g = "/api/guilds/{guild}/bot-messages";
    Router::new()
        .route(g, get(list))
        .route(&format!("{g}/revert-all"), post(revert_all))
        .route(&format!("{g}/kit"), get(kit_export))
        .route(
            &format!("{g}/import"),
            post(kit_import).layer(DefaultBodyLimit::max(KIT_MAX)),
        )
        .route(&format!("{g}/{{key}}"), post(save))
        .route(&format!("{g}/{{key}}/revert"), post(revert))
        .route(&format!("{g}/{{key}}/undo"), post(undo))
        .route(&format!("{g}/{{key}}/test"), post(test))
        .route(&format!("{g}/{{key}}/shown"), get(shown))
}

/// Every built-in message with its default, the server's copy and a pending undo.
async fn list(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    sqlx::query("delete from bot_messages_undo where reverted_at < now() - interval '24 hours'")
        .execute(&st.db)
        .await?;
    let saved: Vec<(String, Value, i64, f64)> = sqlx::query_as(
        "select key, message, updated_by, extract(epoch from updated_at)::float8 from bot_messages
         where (guild_id = $1 and key <> all($2)) or (guild_id = 0 and key = any($2))",
    )
    .bind(guild)
    .bind(bot_wide())
    .fetch_all(&st.db)
    .await?;
    let undo: Vec<(String, f64)> = sqlx::query_as(
        "select key, extract(epoch from reverted_at)::float8 from bot_messages_undo
         where (guild_id = $1 and key <> all($2)) or (guild_id = 0 and key = any($2))",
    )
    .bind(guild)
    .bind(bot_wide())
    .fetch_all(&st.db)
    .await?;
    let (_, cfg) = crate::store::current(&st.db, guild).await?;
    let tickets: brim_core::sections::TicketsCfg =
        serde_json::from_value(cfg["tickets"].clone()).unwrap_or_default();
    // What the ticket panel message shows for `{text}`: a line per enabled panel with a description.
    let panel_text: String = tickets
        .panels
        .iter()
        .filter(|p| p.enabled && !p.description.is_empty())
        .map(|p| format!("**{}**\n{}", p.name, p.description))
        .collect::<Vec<_>>()
        .join("\n\n");
    let site = std::env::var("DASHBOARD_URL").unwrap_or_default();
    let added = added_buttons(&st, guild, &tickets).await?;
    let rows: Vec<Value> = botmsg::defs()
        .iter()
        .filter(|d| !botmsg::HIDDEN.contains(&d.key.as_str()))
        .map(|d| {
            let mine = saved.iter().find(|r| r.0 == d.key).filter(|_| !d.locked);
            let vars: Vec<Value> = d
                .vars
                .iter()
                .map(|v| {
                    let (label, example) = botmsg::var_info(&d.key, v);
                    let example = if is_log_type(d, v) { &d.name } else { example };
                    let example = if d.key == "ticket.panel" && *v == "text" && !panel_text.is_empty() { panel_text.as_str() } else { example };
                    let banner = banner_example(&cfg, &site, &d.key);
                    let example = if *v == "banner" { banner.as_str() } else { example };
                    let example = if *v == "sponsor_picture" && cfg["giveaways"]["sponsor_picture"] != true { "" } else { example };
                    json!({"name": botmsg::public_name(&d.key, v), "label": label, "example": example})
                })
                .collect();
            let area = botmsg::area(d).unwrap_or(("Members and server", d.group));
            json!({
                "key": d.key, "name": d.name, "group": d.group, "locked": d.locked,
                "area": area.0, "sub": area.1,
                "security": d.security, "plain_facts": d.strict(), "details": d.details() && d.vars.contains(&"text"),
                "vars": vars, "important": names(d, &d.important), "default": botmsg::to_public(&d.key, &d.template),
                "buttons": added.get(d.key.as_str()),
                "required": names(d, &d.required), "what": d.what, "in_text": botmsg::in_text(d),
                "message": mine.map(|r| &r.1),
                "updated_by": mine.map(|r| r.2.to_string()),
                "updated_at": mine.map(|r| r.3 as i64),
                "undo_until": undo.iter().find(|u| u.0 == d.key).map(|u| u.1 as i64 + 86_400),
            })
        })
        .collect();
    Ok(Json(json!(rows)))
}

/// The buttons Brim adds under each message, as the bot saved them at startup (`previews.rs`).
/// The Enter button of a running giveaway takes this server's label and emoji.
async fn added_buttons(
    st: &AppState,
    guild: i64,
    t: &brim_core::sections::TicketsCfg,
) -> Result<Value, ApiError> {
    let mut all: Value = sqlx::query_scalar("select messages->'buttons' from bot_previews")
        .fetch_optional(&st.db)
        .await?
        .flatten()
        .unwrap_or_else(|| json!({}));
    // The panel of this server: the buttons the bot posts, from its real ticket types.
    let panels = if t.panels.is_empty() {
        serde_json::from_value(brim_core::sections::default_panels()).unwrap_or_default()
    } else {
        t.panels.clone()
    };
    let on: Vec<&brim_core::sections::TicketPanel> = panels.iter().filter(|p| p.enabled).collect();
    all["ticket.panel"] =
        brim_core::sections::panel_message(&on, false, None, &t.types, t.button_per_type)
            ["components"]
            .clone();
    // The appeal log is posted by the dashboard, with its decision buttons.
    all["log.appeal.appeal_sent"] = crate::appeals::decision_row(0);
    // Panels and menus that Brim builds from the live settings are not drawn: the notice says so.
    for d in botmsg::defs() {
        let k = d.key.as_str();
        if k.split('.').take(2).eq(["reply", "panel"])
            || matches!(
                k,
                "reply.help.panel"
                    | "reply.whitelist.prompt"
                    // The ticket type menus: one option per type of this server.
                    | "reply.dm.menu"
                    | "reply.ticket.ask_type"
                    | "reply.ticket.claimed_self"
                    | "reply.ticket.claimed_other"
                    | "reply.ticket.claim_failed"
            )
        {
            all[k] = json!("panel");
        }
    }
    if let Some(cfg) = crate::giveaways::settings(st, guild).await? {
        // The bot may not have saved its samples yet (it updates apart from the dashboard).
        if let Some(b) = all.pointer_mut("/giveaway.running/0/components/0") {
            if !cfg.button_label.trim().is_empty() {
                b["label"] = json!(cfg.button_label.trim());
            }
            b["emoji"] = match giveaways::button_emoji(&cfg.button_emoji) {
                Some(giveaways::ButtonEmoji::Unicode(name)) => json!({"name": name}),
                Some(giveaways::ButtonEmoji::Custom { animated, name, id }) => {
                    json!({"name": name, "id": id.to_string(), "animated": animated})
                }
                None => Value::Null,
            };
        }
    }
    Ok(all)
}

/// One message as members see it, with example values: the server's copy (else the default),
/// filled in, and the buttons Brim adds. A page draws it beside an Edit message link.
async fn shown(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, key)): Path<(i64, String)>,
) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let d = botmsg::find(&key).ok_or(ApiError::NotFound)?;
    if botmsg::HIDDEN.contains(&key.as_str()) {
        return Err(ApiError::NotFound);
    }
    let stored: Option<Value> = match d.locked {
        true => None,
        false => {
            sqlx::query_scalar("select message from bot_messages where guild_id = $1 and key = $2")
                .bind(botmsg::copy_guild(&key, guild))
                .bind(&key)
                .fetch_optional(&st.db)
                .await?
        }
    };
    let server = st
        .discord
        .guild_name(guild)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let (_, cfg) = crate::store::current(&st.db, guild).await?;
    let mut ex = examples(d, &server);
    // The giveaway banner the server chose (none: no picture), as members see it.
    if let Some(b) = ex.iter_mut().find(|(k, _)| *k == "banner") {
        b.1 = banner_example(
            &cfg,
            &std::env::var("DASHBOARD_URL").unwrap_or_default(),
            &key,
        );
    }
    // The sponsor's picture is off until the server turns it on.
    if let Some(b) = ex.iter_mut().find(|(k, _)| *k == "sponsor_picture") {
        if cfg["giveaways"]["sponsor_picture"] != true {
            b.1.clear();
        }
    }
    let vars: Vec<(&str, &str)> = ex.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut m = botmsg::render(&key, stored.as_ref(), &vars);
    fill_emoji(&mut m);
    let tickets: brim_core::sections::TicketsCfg =
        serde_json::from_value(cfg["tickets"].clone()).unwrap_or_default();
    let added = added_buttons(&st, guild, &tickets).await?;
    Ok(Json(
        json!({"message": m, "added": added.get(key.as_str())}),
    ))
}

/// Brim emoji tags (`{e:name}`) in the texts and fields, as `panel_message` fills them.
fn fill_emoji(m: &mut Value) {
    let fill = |v: &mut Value| {
        if let Some(t) = v.as_str() {
            *v = json!(brim_logging::emoji::fill(t).trim());
        }
    };
    for e in m["embeds"].as_array_mut().into_iter().flatten() {
        for p in ["/title", "/description", "/footer/text"] {
            e.pointer_mut(p).map(fill);
        }
        for f in e["fields"].as_array_mut().into_iter().flatten() {
            fill(&mut f["name"]);
            fill(&mut f["value"]);
        }
    }
}

/// The outside-server keys, for SQL.
fn bot_wide() -> Vec<String> {
    botmsg::BOT_WIDE.iter().map(|k| k.to_string()).collect()
}

/// Who may change the copy of `key` on `guild`: an outside-server entry only the bot owner, a
/// security text a Trusted Admin with a fresh 2FA code (owner decision 2026-11-21), any other entry
/// a Trusted Admin. A `botmsg::HIDDEN` text nobody (`editable` refuses it).
async fn may_edit(st: &AppState, s: &Sess, guild: i64, key: &str) -> Result<(), ApiError> {
    if botmsg::HIDDEN.contains(&key) {
        return Err(ApiError::Forbidden("nobody can change this message"));
    }
    if botmsg::BOT_WIDE.contains(&key) {
        if crate::update::is_bot_owner(st, s).await {
            return Ok(());
        }
        return Err(ApiError::Forbidden(
            "only the bot owner can change this message",
        ));
    }
    // A security text also needs a fresh 2FA step-up for this server, on every route that changes it.
    api::require(st, s, guild, Level::Admin, is_security(key)).await?;
    Ok(())
}

fn is_security(key: &str) -> bool {
    botmsg::find(key).is_some_and(|d| d.security)
}

pub(crate) async fn stored(
    db: impl sqlx::PgExecutor<'_>,
    guild: i64,
    key: &str,
) -> Result<Option<Value>, ApiError> {
    Ok(
        sqlx::query_scalar("select message from bot_messages where guild_id = $1 and key = $2")
            .bind(guild)
            .bind(key)
            .fetch_optional(db)
            .await?,
    )
}

/// The editable message behind `key`, or why not.
fn editable(key: &str) -> Result<&'static botmsg::Def, ApiError> {
    let d = botmsg::find(key).ok_or(ApiError::NotFound)?;
    if d.locked {
        return Err(ApiError::BadRequest(
            "this message cannot be changed".into(),
        ));
    }
    Ok(d)
}

/// Why `m` cannot be a copy of `key`, or `None`. The Message Builder limits, then what a built-in
/// message cannot hold: buttons (code adds them), builder-only times and unknown variables.
pub(crate) fn problem(key: &str, m: &Value) -> Option<String> {
    if let Err(e) = crate::embeds::limits(m) {
        return Some(e);
    }
    if m["components"].as_array().is_some_and(|c| !c.is_empty()) {
        return Some(
            "buttons and menus cannot be added here: Brim adds this message's buttons itself"
                .into(),
        );
    }
    let embeds = m["embeds"].as_array().map_or(&[][..], Vec::as_slice);
    if embeds.iter().any(|e| !e["timestamp"].is_null()) {
        return Some("an embed time cannot be used here: remove it from the footer".into());
    }
    if m.to_string().contains("<t:+") {
        return Some("a time counted from sending cannot be used here: use a fixed time".into());
    }
    let empty = m["content"].as_str().is_none_or(|c| c.trim().is_empty())
        && embeds
            .iter()
            .all(|e| e.as_object().is_none_or(|o| o.keys().all(|k| k == "color")));
    if empty {
        return Some("the message is empty: write something, or use Revert to default".into());
    }
    if let Some(why) = botmsg::missing_text(key, m) {
        return Some(why);
    }
    let unknown = botmsg::unknown_vars(key, m);
    (!unknown.is_empty()).then(|| {
        let list: Vec<String> = unknown.iter().map(|v| format!("{{{v}}}")).collect();
        format!(
            "this message cannot fill {}: remove it or pick one from the variables list",
            list.join(", ")
        )
    })
}

#[derive(Deserialize)]
struct Save {
    message: Value,
}

/// Saves the server's copy. A copy without a required placeholder is refused (`problem`); one that
/// dropped an important variable is saved with a warning.
async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, key)): Path<(i64, String)>,
    Json(b): Json<Save>,
) -> Res {
    may_edit(&st, &s, guild, &key).await?;
    let d = editable(&key)?;
    if let Some(why) = problem(&key, &b.message) {
        return Err(ApiError::BadRequest(why));
    }
    let m = json!({"content": b.message["content"].as_str().unwrap_or(""), "embeds": b.message["embeds"].as_array().cloned().unwrap_or_default()});
    let scope = botmsg::copy_guild(&key, guild);
    let mut tx = st.db.begin().await?;
    let old = stored(&mut *tx, scope, &key).await?;
    sqlx::query(
        "insert into bot_messages (guild_id, key, message, updated_by) values ($1, $2, $3, $4)
         on conflict (guild_id, key) do update set message = $3, updated_by = $4, updated_at = now()",
    )
    .bind(scope)
    .bind(&key)
    .bind(&m)
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    // A new edit replaces what Undo would bring back.
    sqlx::query("delete from bot_messages_undo where guild_id = $1 and key = $2")
        .bind(scope)
        .bind(&key)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    audit(
        &st,
        &s,
        guild,
        change_line(d, "changed", old.as_ref(), Some(&m)),
    )
    .await?;
    if key == PANEL {
        ticket_panel_changed(&st, guild).await;
    }
    Ok(Json(json!({"dropped": botmsg::dropped(&key, &m)})))
}

/// Moves the copy of `key` (all copies but those in `keep` when `None`) to the undo table.
/// Returns how many.
async fn move_to_undo(
    st: &AppState,
    guild: i64,
    key: Option<&str>,
    keep: &[String],
) -> Result<u64, ApiError> {
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "insert into bot_messages_undo (guild_id, key, message)
         select guild_id, key, message from bot_messages
         where guild_id = $1 and ($2::text is null or key = $2) and key <> all($3)
         on conflict (guild_id, key) do update set message = excluded.message, reverted_at = now()",
    )
    .bind(guild)
    .bind(key)
    .bind(keep)
    .execute(&mut *tx)
    .await?;
    let n = sqlx::query(
        "delete from bot_messages where guild_id = $1 and ($2::text is null or key = $2) and key <> all($3)",
    )
    .bind(guild)
    .bind(key)
    .bind(keep)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(n)
}

async fn revert(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, key)): Path<(i64, String)>,
) -> Res {
    may_edit(&st, &s, guild, &key).await?;
    let d = editable(&key)?;
    let scope = botmsg::copy_guild(&key, guild);
    let old = stored(&st.db, scope, &key).await?;
    if move_to_undo(&st, scope, Some(&key), &[]).await? == 0 {
        return Err(ApiError::BadRequest(
            "this message already is the default".into(),
        ));
    }
    let line = if d.strict() {
        change_line(d, "reverted", old.as_ref(), None)
    } else {
        format!("reverted the bot message “{}” to the default", d.name)
    };
    audit(&st, &s, guild, line).await?;
    if key == PANEL {
        ticket_panel_changed(&st, guild).await;
    }
    Ok(Json(json!({"ok": true})))
}

/// Reverts every edited entry but the security texts: those go one by one, each with its audit
/// line (plan section 6).
async fn revert_all(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let n = move_to_undo(&st, guild, None, &security_keys()).await?;
    if n > 0 {
        audit(
            &st,
            &s,
            guild,
            format!("reverted all {n} edited bot messages to the default"),
        )
        .await?;
    }
    ticket_panel_changed(&st, guild).await;
    Ok(Json(json!({"reverted": n})))
}

/// Brings back the copy a revert removed, within 24 hours.
async fn undo(State(st): State<AppState>, s: Sess, Path((guild, key)): Path<(i64, String)>) -> Res {
    may_edit(&st, &s, guild, &key).await?;
    let d = editable(&key)?;
    let scope = botmsg::copy_guild(&key, guild);
    let mut tx = st.db.begin().await?;
    let old = stored(&mut *tx, scope, &key).await?;
    let back: Option<Value> = sqlx::query_scalar(
        // A reverted copy can be brought back for 24 hours.
        "delete from bot_messages_undo where guild_id = $1 and key = $2 and reverted_at >= now() - interval '24 hours' returning message",
    )
    .bind(scope)
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(back) = back else {
        return Err(ApiError::BadRequest(
            "nothing to undo: the 24 hours have passed or the message was edited since".into(),
        ));
    };
    sqlx::query(
        "insert into bot_messages (guild_id, key, message, updated_by) values ($1, $2, $3, $4)
         on conflict (guild_id, key) do update set message = $3, updated_by = $4, updated_at = now()",
    )
    .bind(scope)
    .bind(&key)
    .bind(&back)
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let line = if d.strict() {
        change_line(d, "undid the revert of", old.as_ref(), Some(&back))
    } else {
        format!("undid the revert of the bot message “{}”", d.name)
    };
    audit(&st, &s, guild, line).await?;
    if key == PANEL {
        ticket_panel_changed(&st, guild).await;
    }
    Ok(Json(json!({"ok": true})))
}

/// The server's edited messages as a kit file: only copies of editable messages, never a locked one.
async fn kit_export(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let saved: Vec<(String, Value)> =
        sqlx::query_as("select key, message from bot_messages where guild_id = $1 order by key")
            .bind(guild)
            .fetch_all(&st.db)
            .await?;
    let messages: serde_json::Map<String, Value> = saved
        .into_iter()
        .filter(|(k, _)| botmsg::find(k).is_some_and(|d| !d.locked))
        .collect();
    Ok(Json(
        json!({"kit": "brim-messages", "version": 2, "messages": messages}),
    ))
}

/// The names members type for `vars` of `d`.
fn names(d: &botmsg::Def, vars: &[&str]) -> Vec<String> {
    vars.iter()
        .map(|v| botmsg::public_name(&d.key, v))
        .collect()
}

/// `kit` with today's variable names: a version 1 kit was exported before the names changed
/// (`{prize}` is `{ga_prize}` now), so its messages are renamed; version 2 is as it is.
fn current_names(kit: &Value) -> Value {
    let mut k = kit.clone();
    if k["version"] == 1 {
        if let Some(m) = k["messages"].as_object_mut() {
            for (key, msg) in m.iter_mut() {
                *msg = botmsg::to_public(key, msg);
            }
        }
        k["version"] = 2.into();
    }
    k
}

/// What an import does with each message of a kit: `apply` (new edit), `replace` (over an existing
/// edit) or `skip` with the reason. Imported text is untrusted, so every message passes `problem`,
/// the check of a manual save.
fn kit_plan(kit: &Value, existing: &[String]) -> Result<Vec<Value>, String> {
    // `tommybot-messages`: kits exported before the rename.
    let ours = matches!(
        kit["kit"].as_str(),
        Some("brim-messages" | "tommybot-messages")
    );
    if !ours || !matches!(kit["version"].as_u64(), Some(1 | 2)) {
        return Err("this is not a Brim message kit".into());
    }
    let Some(messages) = kit["messages"].as_object() else {
        return Err("the kit has no messages".into());
    };
    Ok(messages
        .iter()
        .map(|(key, m)| {
            let name = botmsg::find(key).map_or(key.as_str(), |d| &d.name);
            let skip =
                |why: String| json!({"key": key, "name": name, "action": "skip", "reason": why});
            let Some(d) = botmsg::find(key) else {
                return skip("this server's Brim has no such message".into());
            };
            if d.locked {
                return skip("this message cannot be changed".into());
            }
            if botmsg::BOT_WIDE.contains(&key.as_str()) {
                return skip("sent outside a server: only the bot owner can change it".into());
            }
            if let Some(why) = problem(key, m) {
                return skip(why);
            }
            let action = if existing.contains(key) {
                "replace"
            } else {
                "apply"
            };
            json!({"key": key, "name": name, "action": action, "dropped": botmsg::dropped(key, m)})
        })
        .collect())
}

#[derive(Deserialize)]
struct Import {
    kit: Value,
    /// false only previews: the same checks, nothing saved.
    apply: bool,
}

/// Imports a kit. Owner and Extra Owner only, with a fresh 2FA code when it changes a security
/// text; a replaced edit goes to the 24 hour Undo like a revert.
async fn kit_import(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Import>,
) -> Res {
    api::require(&st, &s, guild, Level::Owner, false).await?;
    let mut tx = st.db.begin().await?;
    let rows: Vec<(String, Value)> =
        sqlx::query_as("select key, message from bot_messages where guild_id = $1 for update")
            .bind(guild)
            .fetch_all(&mut *tx)
            .await?;
    let existing: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let kit = current_names(&b.kit);
    let plan = kit_plan(&kit, &existing).map_err(ApiError::BadRequest)?;
    let keys: Vec<&str> = plan
        .iter()
        .filter(|p| p["action"] != "skip")
        .filter_map(|p| p["key"].as_str())
        .collect();
    // An import that changes a security text needs the 2FA step-up of a manual save; a preview does not.
    if b.apply && keys.iter().any(|k| is_security(k)) && !s.stepped_up_for(guild) {
        return Err(ApiError::StepUp);
    }
    if b.apply && !keys.is_empty() {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        // What Undo brings back is the copy this import replaces; a key with none loses a stale one.
        sqlx::query("delete from bot_messages_undo where guild_id = $1 and key = any($2)")
            .bind(guild)
            .bind(&keys)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "insert into bot_messages_undo (guild_id, key, message)
             select guild_id, key, message from bot_messages where guild_id = $1 and key = any($2)
             on conflict (guild_id, key) do update set message = excluded.message, reverted_at = now()",
        )
        .bind(guild)
        .bind(&keys)
        .execute(&mut *tx)
        .await?;
        for k in &keys {
            let m = &kit["messages"][k];
            let m = json!({"content": m["content"].as_str().unwrap_or(""), "embeds": m["embeds"].as_array().cloned().unwrap_or_default()});
            sqlx::query(
                "insert into bot_messages (guild_id, key, message, updated_by) values ($1, $2, $3, $4)
                 on conflict (guild_id, key) do update set message = $3, updated_by = $4, updated_at = now()",
            )
            .bind(guild)
            .bind(k)
            .bind(&m)
            .bind(s.user_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    if b.apply && !keys.is_empty() {
        audit(
            &st,
            &s,
            guild,
            format!("imported {} bot messages from a message kit", keys.len()),
        )
        .await?;
        // Each security text the kit changed gets its own line with both versions.
        for d in keys
            .iter()
            .filter_map(|k| botmsg::find(k))
            .filter(|d| d.security)
        {
            let old = rows.iter().find(|r| r.0 == d.key).map(|r| &r.1);
            let m = &b.kit["messages"][&d.key];
            let new = json!({"content": m["content"].as_str().unwrap_or(""), "embeds": m["embeds"].as_array().cloned().unwrap_or_default()});
            let line = change_line(d, "imported", old, Some(&new));
            audit(&st, &s, guild, line).await?;
        }
    }
    if b.apply && keys.contains(&PANEL) {
        ticket_panel_changed(&st, guild).await;
    }
    Ok(Json(json!({"applied": b.apply, "messages": plan})))
}

/// A log type's `{title}` is its own name, not the first type's.
fn is_log_type(d: &botmsg::Def, var: &str) -> bool {
    var == "title" && d.key.matches('.').count() == 2 && d.key.starts_with("log.")
}

/// The link of the banner the server chose, for the examples in the editor and the preview;
/// empty when it chose none (or the dashboard has no address).
fn banner_example(cfg: &Value, site: &str, key: &str) -> String {
    let part = if key == "giveaway.ended" {
        "ended-"
    } else {
        ""
    };
    match cfg["giveaways"]["banner"].as_str().unwrap_or("a") {
        p @ ("a" | "b" | "c") if !site.is_empty() => {
            format!("{}/giveaway-{part}{p}.png", site.trim_end_matches('/'))
        }
        _ => String::new(),
    }
}

/// The examples for `key`'s variables; `server` and `name` (`{guild.name}`) get the real server name.
fn examples(d: &botmsg::Def, server: &str) -> Vec<(&'static str, String)> {
    d.vars
        .iter()
        .map(|v| {
            let ex = match *v {
                "server" | "name" => server.to_string(),
                "title" if is_log_type(d, v) => d.name.clone(),
                // Discord needs the full link; the editor's own preview uses the short one.
                "banner" => format!(
                    "{}{}",
                    std::env::var("DASHBOARD_URL")
                        .unwrap_or_default()
                        .trim_end_matches('/'),
                    botmsg::var_info(&d.key, v).1
                ),
                _ => botmsg::var_info(&d.key, v).1.replace("\\n", "\n"),
            };
            (*v, ex)
        })
        .collect()
}

#[derive(Deserialize)]
struct Test {
    /// The editor's unsaved copy; none sends the saved one (or the default).
    #[serde(default)]
    message: Option<Value>,
}

/// DMs the editor the message with example values, so they see it as members do.
async fn test(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, key)): Path<(i64, String)>,
    Json(b): Json<Test>,
) -> Res {
    api::require(&st, &s, guild, Level::Admin, false).await?;
    let d = botmsg::find(&key).ok_or(ApiError::NotFound)?;
    let stored = match b.message.filter(|_| !d.locked) {
        Some(m) => {
            if let Some(why) = problem(&key, &m) {
                return Err(ApiError::BadRequest(why));
            }
            Some(m)
        }
        // A locked key never reads a stored copy.
        None if d.locked => None,
        None => {
            sqlx::query_scalar("select message from bot_messages where guild_id = $1 and key = $2")
                .bind(botmsg::copy_guild(&key, guild))
                .bind(&key)
                .fetch_optional(&st.db)
                .await?
        }
    };
    static LAST: Mutex<Option<HashMap<i64, Instant>>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let map = last.get_or_insert_with(HashMap::new);
        map.retain(|_, t| t.elapsed() < TEST_GAP);
        if map.contains_key(&s.user_id) {
            return Err(ApiError::TooMany(
                "One test DM every 10 seconds. Try again in a moment.".into(),
            ));
        }
        map.insert(s.user_id, Instant::now());
    }
    let server = st.discord.guild_name(guild).await?.unwrap_or_default();
    let ex = examples(d, &server);
    let vars: Vec<(&str, &str)> = ex.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut m = botmsg::render(&key, stored.as_ref(), &vars);
    fill_emoji(&mut m);
    let content = format!(
        "-# Test of “{}” from the dashboard, with example values.\n{}",
        d.name,
        m["content"].as_str().unwrap_or("")
    );
    // The note line must not push a long text over Discord's 2000.
    let content = brim_logging::embed::clip(&content, 2000);
    let r = async {
        let ch = st
            .discord
            .call(
                reqwest::Method::POST,
                "/users/@me/channels",
                Some(json!({"recipient_id": s.user_id.to_string()})),
            )
            .await?;
        let ch = ch["id"].as_str().unwrap_or_default().to_string();
        st.discord
            .call(
                reqwest::Method::POST,
                &format!("/channels/{ch}/messages"),
                Some(json!({"content": content.trim_end(), "embeds": m["embeds"], "allowed_mentions": {"parse": []}})),
            )
            .await
    }
    .await;
    if let Err(e) = r {
        tracing::info!(?e, guild, "test bot message DM not sent");
        return Err(ApiError::BadRequest(
            "Brim could not DM you: allow DMs from this server's members and try again".into(),
        ));
    }
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_banner_example_follows_the_server_choice() {
        let site = "https://brim.example/";
        let b = |v: Value| {
            banner_example(
                &json!({"giveaways": {"banner": v}}),
                site,
                "giveaway.running",
            )
        };
        assert_eq!(b(json!("c")), "https://brim.example/giveaway-c.png");
        assert_eq!(b(json!("none")), "");
        assert_eq!(
            banner_example(
                &json!({"giveaways": {"banner": "b"}}),
                site,
                "giveaway.ended"
            ),
            "https://brim.example/giveaway-ended-b.png"
        );
        assert_eq!(
            banner_example(&json!({}), site, "giveaway.running"),
            "https://brim.example/giveaway-a.png"
        );
    }
    use super::*;

    /// Every route that changes a security text needs the 2FA step-up for this server, a call made
    /// straight to the API included; other entries do not. Runs only with a scratch Postgres.
    #[tokio::test]
    async fn security_texts_change_only_after_a_2fa_step_up() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let pid = std::process::id() as i64;
        let (g, owner) = (9_720_000_000 + pid, 7_720_000_000 + pid);
        let (eo, ta, ed) = (owner + 1, owner + 2, owner + 3);
        let mock = Router::new()
            .route(
                "/guilds/{g}",
                get(move || async move {
                    Json(json!({"owner_id": owner.to_string(), "name": "T", "roles": []}))
                }),
            )
            .route(
                "/guilds/{g}/members/{u}",
                get(|| async { Json(json!({"roles": []})) }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, mock).await });
        let st = AppState {
            db: db.clone(),
            discord: crate::discord::Discord {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                client_id: String::new(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                api,
                memo: Default::default(),
                limits: Default::default(),
            },
            secure: false,
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        };
        let clean = || async {
            for t in [
                "bot_messages",
                "bot_messages_undo",
                "dashboard_audit",
                "extra_owners",
                "trusted_admins",
                "permit_members",
                "permits",
            ] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "delete from {t} where guild_id = $1"
                )))
                .bind(g)
                .execute(&db)
                .await
                .unwrap();
            }
        };
        clean().await;
        for (q, user) in [
            ("insert into extra_owners (guild_id, user_id) values ($1, $2)", eo),
            ("insert into trusted_admins (guild_id, user_id) values ($1, $2)", ta),
            ("insert into permits (guild_id, name, perms) values ($1, 'Ed', '{dashboard_edit}') returning $2", ed),
            ("insert into permit_members (permit_id, guild_id, user_id) select id, $1, $2 from permits where guild_id = $1 and name = 'Ed'", ed),
        ] {
            sqlx::query(q)
                .bind(g)
                .bind(user)
                .execute(&db)
                .await
                .unwrap();
        }
        let call = move |user: i64, get_list: bool, path: &str, stepped_up: bool, body: Value| {
            let url = format!("/api/guilds/{g}/bot-messages/{path}");
            let mut req = if get_list {
                Request::get(url.trim_end_matches('/'))
                    .body(Body::empty())
                    .unwrap()
            } else {
                Request::post(url)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap()
            };
            req.extensions_mut().insert(Sess {
                id: String::new(),
                user_id: user,
                username: "t".into(),
                access_token: String::new(),
                stepped_up,
                step_up_guild: Some(g),
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
        let send =
            |path: &str, stepped_up: bool, body: Value| call(owner, false, path, stepped_up, body);
        let step_up = (403, Some("step_up_required"));
        let d = botmsg::find("dm.security").unwrap();
        let sec = d.template.clone();
        let plain =
            json!({"content": "", "embeds": [{"description": "Hi {guild.name}, {mod_reason}"}]});
        let kit = |key: &str, m: &Value| json!({"kit": "brim-messages", "version": 1, "messages": {key: m}});

        // An ordinary entry saves without a code.
        assert_eq!(
            send("dm.punish.ban", false, json!({"message": plain}))
                .await
                .0,
            200
        );
        // A security text: save, import, revert and undo are refused without the step-up.
        for (path, body) in [
            ("dm.security", json!({"message": sec})),
            (
                "import",
                json!({"kit": kit("dm.security", &sec), "apply": true}),
            ),
            ("dm.security/revert", json!({})),
            ("dm.security/undo", json!({})),
        ] {
            let (code, r) = send(path, false, body).await;
            assert_eq!((code, r["error"].as_str()), step_up, "{path}: {r}");
        }
        // The import preview only reads, and an import of ordinary entries needs no code.
        assert_eq!(
            send(
                "import",
                false,
                json!({"kit": kit("dm.security", &sec), "apply": false})
            )
            .await
            .0,
            200
        );
        assert_eq!(
            send(
                "import",
                false,
                json!({"kit": kit("dm.punish.ban", &plain), "apply": true})
            )
            .await
            .0,
            200
        );
        let n: i64 = sqlx::query_scalar(
            "select count(*) from bot_messages where guild_id = $1 and key = 'dm.security'",
        )
        .bind(g)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(n, 0, "no refused call changed the security text");
        // With the step-up the save goes, and the dashboard log has both versions.
        let (code, r) = send("dm.security", true, json!({"message": sec})).await;
        assert_eq!(code, 200, "{r}");
        let line: String = sqlx::query_scalar(
            "select text from dashboard_audit where guild_id = $1 and text like '%security text%'",
        )
        .bind(g)
        .fetch_one(&db)
        .await
        .unwrap();
        assert!(line.contains("was the default, now `"), "{line}");

        // Every level may look; the list never holds a recovery text (owner decision 2026-11-21).
        for user in [owner, eo, ta, ed] {
            let (code, r) = call(user, true, "", false, Value::Null).await;
            assert_eq!(code, 200, "{user}: {r}");
            let keys: Vec<&str> = r
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|x| x["key"].as_str())
                .collect();
            assert!(keys.contains(&"dm.security"));
            for k in botmsg::HIDDEN {
                assert!(!keys.contains(k), "{k} is listed");
            }
        }
        // A security text: Owner, Extra Owner and Trusted Admin save it with the step-up; an Editor
        // (a dashboard_edit permit, below Trusted Admin) may not.
        for (user, want) in [(owner, 200), (eo, 200), (ta, 200), (ed, 403)] {
            let (code, r) = call(user, false, "dm.security", true, json!({"message": sec})).await;
            assert_eq!(code, want, "{user}: {r}");
        }
        assert_eq!(
            call(ta, false, "dm.security", false, json!({"message": sec}))
                .await
                .1["error"],
            "step_up_required",
            "a Trusted Admin still needs the 2FA code"
        );
        // A recovery text: nobody changes it, the Owner with a fresh step-up included.
        for k in botmsg::HIDDEN {
            let m = botmsg::find(k).unwrap().template.clone();
            for user in [owner, eo, ta] {
                for (path, body) in [
                    (k.to_string(), json!({"message": m})),
                    (format!("{k}/revert"), json!({})),
                    (format!("{k}/undo"), json!({})),
                ] {
                    let (code, r) = call(user, false, &path, true, body).await;
                    assert_eq!(code, 403, "{path} by {user}: {r}");
                }
            }
            let (code, r) = send("import", true, json!({"kit": kit(k, &m), "apply": true})).await;
            assert!(code == 200 || code == 400, "{k}: {code} {r}");
        }
        let n: i64 = sqlx::query_scalar(
            "select count(*) from bot_messages where guild_id = $1 and key = any($2)",
        )
        .bind(g)
        .bind(
            botmsg::HIDDEN
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>(),
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(n, 0, "no recovery text was stored");
        clean().await;
    }

    #[test]
    fn problem_refuses_what_a_built_in_message_cannot_hold() {
        let ok =
            json!({"content": "", "embeds": [{"description": "Hi {guild.name}, {mod_reason}"}]});
        assert_eq!(problem("dm.punish.ban", &ok), None);
        let cases = [
            json!({"content": "", "embeds": [{"description": "Hi {member}"}]}),
            json!({"content": "", "embeds": [{"description": "x", "timestamp": "now"}]}),
            json!({"content": "<t:+60:R>", "embeds": []}),
            json!({"content": "x", "embeds": [], "components": [{"type": 1, "components": []}]}),
            json!({"content": " ", "embeds": [{"color": 1}]}),
        ];
        for m in cases {
            assert!(problem("dm.punish.ban", &m).is_some(), "{m}");
        }
        // A frame without {text} is refused on save and in a kit, with the reason.
        let no_text = json!({"content": "", "embeds": [{"title": "{title}", "description": "Hi"}]});
        let why = problem("log.message", &no_text).unwrap();
        assert!(why.contains("{text}"), "{why}");
        let kit =
            json!({"kit": "brim-messages", "version": 1, "messages": {"log.message": no_text}});
        let plan = kit_plan(&kit, &[]).unwrap();
        assert_eq!(plan[0]["action"], "skip");
        let with = json!({"content": "", "embeds": [{"description": "Log: {text}"}]});
        assert_eq!(problem("log.message", &with), None);
    }

    #[test]
    fn a_security_change_names_both_versions() {
        let d = botmsg::find("dm.security").unwrap();
        let new = json!({"content": "a `b`", "embeds": []});
        let line = change_line(d, "changed", None, Some(&new));
        assert!(line.contains("was the default, now `"), "{line}");
        assert!(line.contains("a 'b'"), "{line}");
        let d = botmsg::find("dm.punish.ban").unwrap();
        assert_eq!(
            change_line(d, "changed", None, Some(&new)),
            format!("changed the bot message “{}”", d.name)
        );
        assert!(security_keys().contains(&"log.panic".to_string()));
        assert!(!security_keys().contains(&"log.message".to_string()));
    }

    #[test]
    fn kit_plan_checks_every_message_like_a_manual_save() {
        let good = json!({"content": "", "embeds": [{"description": "Hi {server}, {reason}"}]});
        let kit = json!({"kit": "brim-messages", "version": 1, "messages": {
            "dm.punish.ban": good,
            "dm.punish.kick": good,
            "no.such.key": good,
            "log.message.message_deleted": {"embeds": [{"description": "{text}"}]},
            "log.message": {"embeds": [{"description": "{text}"}]},
            "log.anti_nuke": {"embeds": [{"description": "{text}"}]},
            "dm.punish.softban": {"content": "", "embeds": [{"description": "Hi {member}"}]},
        }});
        // A version 1 kit holds the old names: the import renames them first.
        let kit = current_names(&kit);
        assert_eq!(kit["version"], 2);
        assert_eq!(
            kit["messages"]["dm.punish.ban"]["embeds"][0]["description"],
            "Hi {guild.name}, {mod_reason}"
        );
        let plan = kit_plan(&kit, &["dm.punish.kick".to_string()]).unwrap();
        let act = |k: &str| plan.iter().find(|p| p["key"] == k).unwrap()["action"].clone();
        assert_eq!(act("dm.punish.ban"), "apply");
        assert_eq!(act("dm.punish.kick"), "replace");
        assert_eq!(act("no.such.key"), "skip");
        // A log type needs no special case: a kit made before the per-type entries still imports.
        assert_eq!(act("log.message.message_deleted"), "apply");
        assert_eq!(act("log.message"), "apply");
        // A security log without its {title} is refused; with it, the Owner may import it.
        assert_eq!(act("log.anti_nuke"), "skip");
        let sec = json!({"embeds": [{"title": "{title}", "description": "{text}"}]});
        let kit = json!({"kit": "brim-messages", "version": 1, "messages": {"log.anti_nuke": sec}});
        assert_eq!(kit_plan(&kit, &[]).unwrap()[0]["action"], "apply");
        let old =
            json!({"kit": "tommybot-messages", "version": 1, "messages": {"log.anti_nuke": sec}});
        assert_eq!(kit_plan(&old, &[]).unwrap()[0]["action"], "apply");
        assert_eq!(act("dm.punish.softban"), "skip");
        assert!(kit_plan(&json!({"kit": "x", "version": 1, "messages": {}}), &[]).is_err());
        assert!(kit_plan(
            &json!({"kit": "brim-messages", "version": 3, "messages": {}}),
            &[]
        )
        .is_err());
        assert!(kit_plan(&json!({"kit": "brim-messages", "version": 1}), &[]).is_err());
    }
}

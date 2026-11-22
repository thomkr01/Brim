use crate::{
    access::Level,
    api::{log_by, require, Res},
    auth::Sess,
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use brim_automod::{
    discord_api::{
        approve_rule, rule_body, rule_fingerprint, BotAction, DiscordAutomodCfg, MentionScope,
        RuleSpec,
    },
    lockdown::{kind_from, missing, tshoot, Fix, LockKind, Overwrite, Problem, RoleInfo, Snapshot},
};
use brim_core::jobs;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/guilds/{guild}/lockdown", get(status))
        .route("/api/guilds/{guild}/lockdown/lock", post(lock))
        .route("/api/guilds/{guild}/lockdown/unlock", post(unlock))
        .route("/api/guilds/{guild}/lockdown/update", post(send_update))
        .route("/api/guilds/{guild}/lockdown/channels", get(channels))
        .route("/api/guilds/{guild}/lockdown/check", get(check))
        .route("/api/guilds/{guild}/lockdown/fix", post(fix))
        .route(
            "/api/guilds/{guild}/discord-rules",
            get(rules).post(create_rule),
        )
        .route(
            "/api/guilds/{guild}/discord-rules/{rule}",
            put(update_rule).delete(delete_rule),
        )
        .route(
            "/api/guilds/{guild}/discord-rules/{rule}/enabled",
            post(set_enabled),
        )
        .route(
            "/api/guilds/{guild}/discord-rules-migrate-words",
            post(migrate_words),
        )
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn status(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Viewer, false).await?;
    let rows: Vec<(sqlx::types::Json<Snapshot>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "select snapshot, created_at from lock_snapshots where guild_id = $1 order by created_at",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let unlock_at: Option<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(
        "select run_at from scheduled_jobs where guild_id = $1 and kind = 'unlock' order by run_at limit 1",
    )
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    let locks: Vec<Value> = rows
        .iter()
        .map(|(snap, at)| {
            let kind = snap.0.kind.as_ref();
            let (channels, roles, join) = kind.map(LockKind::covers).unwrap_or_default();
            json!({"channels": channels, "roles": roles, "join": join.map(|j| format!("{j:?}")),
                   "channel": matches!(kind, Some(LockKind::Channel(_))), "since": at,
                   "hidden": matches!(kind, Some(LockKind::Blind)),
                   "invites_paused": snap.0.invites_paused})
        })
        .collect();
    Ok(Json(
        json!({"locks": locks, "unlock_at": unlock_at.map(|(t,)| t)}),
    ))
}

/// Below Trusted Admin, the dashboard asks what the chat command asks: the `perm` permit, the
/// lock permit of each kind, and no panic mode mod command lock.
async fn may_lock(
    st: &AppState,
    s: &Sess,
    guild: i64,
    level: Level,
    perm: &str,
    kinds: &[LockKind],
) -> Result<(), ApiError> {
    if level >= Level::Admin {
        return Ok(());
    }
    let actor = brim_core::permits::Level::Custom {
        rank: 0,
        perms: crate::api::permit_perms(st, s, guild).await?,
        whitelist: vec![],
        limits: Default::default(),
    };
    if !actor.has(perm) {
        return Err(ApiError::Forbidden("you lack the permit for this"));
    }
    if let Some(p) = kinds.iter().find_map(|k| missing(|p| actor.has(p), k)) {
        return Err(ApiError::Forbidden(match p {
            "lock_channels" => "you lack the lock_channels permit",
            "lock_staff_roles" => "you lack the lock_staff_roles permit",
            _ => "you lack the lock_joins permit",
        }));
    }
    if crate::appeals::mod_lock(&mut *st.db.acquire().await?, guild).await? {
        return Err(ApiError::Forbidden(
            "panic mode has locked mod commands; only owners and Trusted Admins can do this now",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct LockBody {
    kind: String,
    channel: Option<String>,
    duration_s: Option<i64>,
}

async fn lock(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<LockBody>,
) -> Res {
    let level = require(&st, &s, guild, Level::Editor, false).await?;
    let channel = b.channel.as_deref().and_then(|c| c.parse().ok());
    let kind = kind_from(&b.kind, channel)
        .ok_or_else(|| ApiError::BadRequest("unknown lock kind".into()))?;
    may_lock(&st, &s, guild, level, "lockdown", &[kind]).await?;
    let duration = b.duration_s.filter(|d| *d > 0);
    jobs::enqueue(
        &st.db,
        guild,
        "lock",
        // `by`: the bot runs it only while this person may still lock (security review).
        json!({"kind": b.kind, "channel": b.channel, "duration_s": duration, "by": s.user_id}),
        now(),
    )
    .await?;
    audit(&st, &s, guild, format!("locked the server ({})", b.kind)).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize, Default)]
struct UnlockBody {
    /// Lifts only the locks overlapping this kind; absent lifts everything.
    kind: Option<String>,
}

async fn unlock(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    body: Option<Json<UnlockBody>>,
) -> Res {
    let level = require(&st, &s, guild, Level::Editor, false).await?;
    let which = match body.and_then(|b| b.0.kind) {
        Some(k) => Some(
            kind_from(&k, None).ok_or_else(|| ApiError::BadRequest("unknown lock kind".into()))?,
        ),
        None => None,
    };
    // As in chat: lifting needs the permit of every lock it lifts, also a combined one it overlaps.
    let active: Vec<(sqlx::types::Json<Snapshot>,)> =
        sqlx::query_as("select snapshot from lock_snapshots where guild_id = $1")
            .bind(guild)
            .fetch_all(&st.db)
            .await?;
    let lifts: Vec<LockKind> = active
        .into_iter()
        .filter_map(|(snap,)| snap.0.kind)
        .filter(|a| which.as_ref().is_none_or(|k| k.overlaps(a)))
        .chain(which.clone())
        .collect();
    may_lock(&st, &s, guild, level, "lockdown", &lifts).await?;
    let payload = which.as_ref().map(|k| json!({"lock": k}));
    // Lift all leaves other timers to the bot, which drops the timer of each lock it lifts: a
    // thread lock is not lifted by it, so its timer must stay.
    if let Some(p) = &payload {
        sqlx::query(
            "delete from scheduled_jobs where guild_id = $1 and kind = 'unlock' and payload = $2",
        )
        .bind(guild)
        .bind(p)
        .execute(&st.db)
        .await?;
    }
    let mut job = payload.unwrap_or_else(|| json!({}));
    job["by"] = json!(s.user_id);
    jobs::enqueue(&st.db, guild, "unlock", job, now()).await?;
    audit(&st, &s, guild, "unlocked the server".into()).await?;
    Ok(Json(json!({"ok": true})))
}

/// Text-like channels with whether a server lock already denies sending there ("locked") and
/// whether @everyone cannot see them ("private"); the dashboard flags both in its channel list.
async fn channels(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Viewer, false).await?;
    let (_, c) = crate::store::current(&st.db, guild).await?;
    let main: Vec<String> = c["lockdown"]["main_roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(String::from))
        .chain([guild.to_string()])
        .collect();
    let all = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    let denies = |ch: &Value, bit: u64, roles: &[String]| {
        ch["permission_overwrites"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|o| {
                o["type"] == 0
                    && o["id"]
                        .as_str()
                        .is_some_and(|id| roles.iter().any(|r| r == id))
                    && o["deny"]
                        .as_str()
                        .and_then(|d| d.parse::<u64>().ok())
                        .is_some_and(|d| d & bit != 0)
            })
    };
    let everyone = [guild.to_string()];
    let list: Vec<Value> = all
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ch| matches!(ch["type"].as_u64(), Some(0 | 5 | 15)))
        .map(|ch| {
            json!({"id": ch["id"], "name": ch["name"],
            "locked": denies(ch, brim_automod::lockdown::SEND_MESSAGES, &main),
            "private": denies(ch, brim_automod::lockdown::VIEW_CHANNEL, &everyone)})
        })
        .collect();
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct UpdateBody {
    text: String,
}

/// "Send an update": a message to the people locked out, added to the `#server-locked` notice
/// of a hidden-channels lock, else posted in the Mod Logs channel.
async fn send_update(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<UpdateBody>,
) -> Res {
    let level = require(&st, &s, guild, Level::Editor, false).await?;
    may_lock(&st, &s, guild, level, "lock_updates", &[]).await?;
    let text = b.text.trim();
    if text.is_empty() || text.chars().count() > 1900 {
        return Err(ApiError::BadRequest("write 1 to 1900 characters".into()));
    }
    let locked: Option<(i32,)> = sqlx::query_as(
        "select 1 from lock_snapshots where guild_id = $1 and snapshot->>'locked_channel' is not null limit 1",
    )
    .bind(guild)
    .fetch_optional(&st.db)
    .await?;
    if locked.is_some() {
        // The bot owns the lock notice and numbers the update into it.
        jobs::enqueue(
            &st.db,
            guild,
            "lock_update",
            json!({"text": text, "by": s.user_id}),
            chrono::Utc::now().timestamp(),
        )
        .await?;
        return Ok(Json(json!({"ok": true})));
    }
    let (_, c) = crate::store::current(&st.db, guild).await?;
    let channel = c["logging"]["routes"]["mod_command"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| {
            ApiError::BadRequest("no lock channel or Mod Logs channel to post in".into())
        })?;
    let log = brim_logging::Log::new("Lockdown Update", brim_logging::embed::ALERT).text(text);
    // The server's frame of its Moderation logs applies, as it does to the bot's own.
    let log = crate::discord::framed(&st.db, guild, "log.mod_command", log).await;
    st.discord
        .post_embed_with(
            guild,
            &crate::discord::Readers::load(&st.db, guild, &c).await?,
            &channel,
            log,
            json!([]),
        )
        .await
        .map(drop)
        .map_err(|e| ApiError::BadRequest(format!("{e:#}")))?;
    Ok(Json(json!({"ok": true})))
}

pub(crate) async fn audit(
    st: &AppState,
    s: &Sess,
    guild: i64,
    what: String,
) -> Result<(), ApiError> {
    audit_version(st, s, guild, what, None).await
}

/// [`audit`] for a route that committed config `version`: the line names it, so it can be rolled back.
async fn audit_version(
    st: &AppState,
    s: &Sess,
    guild: i64,
    what: String,
    version: Option<i32>,
) -> Result<(), ApiError> {
    let (_, c) = crate::store::current(&st.db, guild).await?;
    log_by(
        st,
        s,
        guild,
        &c,
        version,
        &format!("{what} from the dashboard"),
    )
    .await;
    Ok(())
}

async fn roles(st: &AppState, guild: i64) -> Result<(Vec<RoleInfo>, i32), ApiError> {
    let (roles, bot_pos) = crate::ops::roles_and_bot_pos(st, guild).await?;
    let info = roles
        .iter()
        .map(|r| RoleInfo {
            id: r.id.parse().unwrap_or(0),
            perms: r.perms(),
            position: r.position as i32,
            managed: r.managed,
        })
        .collect();
    Ok((info, bot_pos as i32))
}

async fn problems(st: &AppState, guild: i64) -> Result<Vec<Problem>, ApiError> {
    let (_, c) = crate::store::current(&st.db, guild).await?;
    let main: Vec<u64> = c["lockdown"]["main_roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str()?.parse().ok())
        .collect();
    let (info, bot_pos) = roles(st, guild).await?;
    let channels = st
        .discord
        .call(Method::GET, &format!("/guilds/{guild}/channels"), None)
        .await?;
    let num = |v: &Value| v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
    let bot = st.discord.bot_id().await?;
    let me = bot.as_str();
    let tickets = crate::tickets::categories(&c);
    let overwrites: Vec<Overwrite> = channels
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| {
            // A ticket's member and the bot itself are let in on purpose.
            let ticket = c["parent_id"]
                .as_str()
                .is_some_and(|p| tickets.contains(&p));
            c["permission_overwrites"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(move |o| o["type"] == 0 || (o["type"] == 1 && !ticket && o["id"] != me))
                .map(|o| Overwrite {
                    channel: num(&c["id"]),
                    role: num(&o["id"]),
                    allow: num(&o["allow"]),
                    deny: num(&o["deny"]),
                    member: o["type"] == 1,
                })
        })
        .collect();
    Ok(tshoot(guild as u64, &main, &info, &overwrites, bot_pos))
}

async fn check(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Viewer, false).await?;
    Ok(Json(json!(problems(&st, guild).await?)))
}

#[derive(Deserialize, Default)]
struct FixPick {
    /// Ids of opt-in problems (an @everyone allow) the user ticked.
    #[serde(default)]
    picked: Vec<String>,
    /// Ids of the automatic fixes the page showed: Apply Fix runs only what was reviewed.
    #[serde(default)]
    shown: Vec<String>,
}

/// The fixes to run: every automatic one and the ticked opt-in ones, or `None` when the automatic
/// ones found now are not the ones the page showed (`shown`).
fn chosen(problems: &[Problem], b: &FixPick) -> Option<Vec<Fix>> {
    let mut auto: Vec<&String> = problems
        .iter()
        .filter(|p| p.fix.is_some() && !p.opt_in)
        .map(|p| &p.id)
        .collect();
    let mut shown: Vec<&String> = b.shown.iter().collect();
    auto.sort();
    shown.sort();
    (auto == shown).then(|| {
        problems
            .iter()
            .filter(|p| !p.opt_in || b.picked.contains(&p.id))
            .filter_map(|p| p.fix)
            .collect()
    })
}

/// Single Apply Fix button: applies every automatic fix the check found, and the opt-in ones
/// the user ticked. Refused when the automatic fixes are not the ones the page showed.
async fn fix(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    b: Option<Json<FixPick>>,
) -> Res {
    require(&st, &s, guild, Level::Admin, true).await?;
    let b = b.map(|b| b.0).unwrap_or_default();
    let fixes = chosen(&problems(&st, guild).await?, &b).ok_or(ApiError::Busy(
        "the check finds other fixes than the page showed: check again and review them",
    ))?;
    for f in &fixes {
        let (path, body) = match *f {
            Fix::SetRolePerms { role, perms } => (
                format!("/guilds/{guild}/roles/{role}"),
                json!({"permissions": perms.to_string()}),
            ),
            Fix::SetOverwrite(o) => (
                format!("/channels/{}/permissions/{}", o.channel, o.role),
                json!({"type": 0, "allow": o.allow.to_string(), "deny": o.deny.to_string()}),
            ),
        };
        let method = if matches!(f, Fix::SetOverwrite(_)) {
            Method::PUT
        } else {
            Method::PATCH
        };
        st.discord.call(method, &path, Some(body)).await?;
    }
    audit(
        &st,
        &s,
        guild,
        format!("applied {} lockdown compatibility fix(es)", fixes.len()),
    )
    .await?;
    Ok(Json(json!({"ok": true, "fixed": fixes.len()})))
}

async fn rules(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Viewer, false).await?;
    let raw = st
        .discord
        .call(
            Method::GET,
            &format!("/guilds/{guild}/auto-moderation/rules"),
            None,
        )
        .await?;
    let (_, config) = crate::store::current(&st.db, guild).await?;
    let mine: DiscordAutomodCfg =
        serde_json::from_value(config["discord_automod"].clone()).unwrap_or_default();
    let ok = approvals(
        &st.db,
        guild,
        raw.as_array().map_or(&[], Vec::as_slice),
        &mine,
    )
    .await?;
    let list: Vec<Value> = raw
        .as_array()
        .into_iter()
        .flatten()
        .zip(ok)
        .map(|(r, ok)| {
            let mut v = json!(RuleSpec::from_discord(r));
            v["id"] = r["id"].clone();
            v["approved"] = json!(ok);
            v["bot_actions"] = json!(r["id"]
                .as_str()
                .and_then(|id| mine.actions.get(id))
                .cloned()
                .unwrap_or_default());
            v["lock"] = json!(r["id"]
                .as_str()
                .and_then(|id| mine.locks.get(id))
                .cloned()
                .unwrap_or_default());
            if let Some(sc) = r["id"].as_str().and_then(|id| mine.scopes.get(id)) {
                v["scope"] = json!(sc.scope);
                v["scope_triggers"] = json!(sc.triggers);
                v["scope_duration_s"] = json!(sc.duration_s);
            }
            v
        })
        .collect();
    Ok(Json(json!(list)))
}

/// Per rule: whether the bot acts on its hits, as the bot decides it (`native::untrusted` and
/// `approved`). Before the guild's first-start approval (rule id 0 row) a rule not marked untrusted
/// and without an approval row counts as approved, because that approval takes it as it is.
/// A rule without Brim actions counts as approved too: the bot does nothing on its hits anyway.
async fn approvals(
    db: &sqlx::PgPool,
    guild: i64,
    raw: &[Value],
    mine: &DiscordAutomodCfg,
) -> Result<Vec<bool>, ApiError> {
    let saved: Vec<(i64, String)> = sqlx::query_as(
        "SELECT rule_id, fingerprint FROM automod_rule_approved WHERE guild_id = $1",
    )
    .bind(guild)
    .fetch_all(db)
    .await?;
    let bad: Vec<i64> = sqlx::query_scalar(
        "SELECT rule_id FROM automod_rule_untrusted WHERE guild_id = $1 AND NOT trusted",
    )
    .bind(guild)
    .fetch_all(db)
    .await?;
    let done = saved.iter().any(|(id, _)| *id == 0);
    Ok(raw
        .iter()
        .map(|r| {
            let id = r["id"]
                .as_str()
                .and_then(|i| i.parse::<i64>().ok())
                .unwrap_or(0);
            if r["id"]
                .as_str()
                .and_then(|i| mine.actions.get(i))
                .is_none_or(Vec::is_empty)
            {
                return true;
            }
            let fp = rule_fingerprint(r);
            // first_start inserts with ON CONFLICT DO NOTHING, so a stale row stays stale.
            let row = saved.iter().find(|(i, _)| *i == id);
            !bad.contains(&id) && row.map_or(!done, |(_, f)| *f == fp)
        })
        .collect())
}

/// Security check rows: one warning per Discord AutoMod rule with Brim actions that the bot
/// ignores until it is approved.
/// No rows when Discord cannot be reached, so paused rules are not shown then.
pub(crate) async fn paused_rule_checks(
    st: &AppState,
    guild: i64,
) -> Result<Vec<brim_antinuke::security::Check>, ApiError> {
    let Ok(raw) = st
        .discord
        .call(
            Method::GET,
            &format!("/guilds/{guild}/auto-moderation/rules"),
            None,
        )
        .await
    else {
        return Ok(vec![]);
    };
    let raw = raw.as_array().cloned().unwrap_or_default();
    let (_, config) = crate::store::current(&st.db, guild).await?;
    let mine: DiscordAutomodCfg =
        serde_json::from_value(config["discord_automod"].clone()).unwrap_or_default();
    let ok = approvals(&st.db, guild, &raw, &mine).await?;
    Ok(raw
        .iter()
        .zip(ok)
        .filter(|(_, ok)| !ok)
        .map(|(r, _)| brim_antinuke::security::Check {
            id: format!("automod_paused_{}", r["id"].as_str().unwrap_or_default()),
            severity: brim_antinuke::security::Severity::Warn,
            title: "Discord AutoMod rule paused",
            detail: format!(
                "Someone without a permit changed the rule \"{}\" in Discord. Brim ignores its hits until it is approved on the Discord AutoMod page.",
                r["name"].as_str().unwrap_or_default()
            ),
            fix: None,
            steps: vec!["Open the Discord AutoMod page and press Review and approve on the rule.".into()],
        })
        .collect())
}

fn body(spec: &RuleSpec) -> Result<Value, ApiError> {
    rule_body(spec).map_err(|e| ApiError::BadRequest(e.to_string()))
}

async fn create_rule(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(spec): Json<RuleSpec>,
) -> Res {
    require(&st, &s, guild, Level::Editor, false).await?;
    let b = body(&spec)?;
    let made = st
        .discord
        .call(
            Method::POST,
            &format!("/guilds/{guild}/auto-moderation/rules"),
            Some(b),
        )
        .await?;
    let id = made["id"].as_str().unwrap_or_default();
    trust_rule(&st, guild, id).await?;
    approve_rule(&st.db, guild, &made, s.user_id).await?;
    let v = set_bot_actions(&st, &s, guild, id, &spec).await?;
    audit_version(
        &st,
        &s,
        guild,
        format!("created the Discord AutoMod rule `{}`", spec.name),
        v,
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

async fn set_bot_actions(
    st: &AppState,
    s: &Sess,
    guild: i64,
    rule: &str,
    spec: &RuleSpec,
) -> Result<Option<i32>, ApiError> {
    // Under the config lock: the Discord rule already exists, so a save in between must not 409 this away.
    let mut tx = st.db.begin().await?;
    let (version, mut config) = crate::store::locked(&mut tx, guild).await?;
    // Mention scopes live next to the bot actions; Discord's own scope is on the rule.
    let scope = (spec.trigger == "mention" && matches!(spec.scope.as_str(), "user" | "server"))
        .then(|| MentionScope {
            scope: spec.scope.clone(),
            triggers: spec.scope_triggers.clamp(1, 100),
            duration_s: spec.scope_duration_s.clamp(1, 3600),
        });
    let mut changed = false;
    for (key, value) in [
        (
            "actions",
            (!spec.bot_actions.is_empty()).then(|| json!(spec.bot_actions)),
        ),
        ("scopes", scope.map(|s| json!(s))),
        (
            "locks",
            (spec.bot_actions.contains(&BotAction::Lock)
                && matches!(
                    spec.lock.as_str(),
                    "channel" | "server" | "kick" | "ban" | "invites"
                ))
            .then(|| json!(spec.lock)),
        ),
    ] {
        let section = &mut config["discord_automod"];
        match value {
            Some(v) => {
                section[key][rule] = v;
                changed = true;
            }
            // `get_mut`, not indexing: indexing a missing key stores a null the bot cannot read.
            None => {
                changed |= section
                    .get_mut(key)
                    .and_then(Value::as_object_mut)
                    .and_then(|m| m.remove(rule))
                    .is_some()
            }
        }
    }
    let committed = if changed {
        Some(crate::store::commit(&mut tx, guild, version, &config, s.user_id).await?)
    } else {
        None
    };
    tx.commit().await?;
    Ok(committed)
}

/// Rule ids are snowflakes; anything else would let a caller reach other Discord endpoints.
fn rule_path(guild: i64, rule: &str) -> Result<String, ApiError> {
    if rule.is_empty() || !rule.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::BadRequest("bad rule id".into()));
    }
    Ok(format!("/guilds/{guild}/auto-moderation/rules/{rule}"))
}

/// A dashboard Editor set this rule, so the bot acts on it again. Discord records the edit as the
/// bot's, and the bot's own audit entries keep the earlier verdict (bot `native::on_rule_audit`).
async fn trust_rule(st: &AppState, guild: i64, rule: &str) -> Result<(), ApiError> {
    let Ok(rule) = rule.parse::<i64>() else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO automod_rule_untrusted (guild_id, rule_id, audit_id, actor, trusted)
         VALUES ($1, $2, 0, 0, true)
         ON CONFLICT (guild_id, rule_id) DO UPDATE SET trusted = true, at = now()",
    )
    .bind(guild)
    .bind(rule)
    .execute(&st.db)
    .await?;
    Ok(())
}

async fn update_rule(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, rule)): Path<(i64, String)>,
    Json(spec): Json<RuleSpec>,
) -> Res {
    require(&st, &s, guild, Level::Editor, false).await?;
    let b = body(&spec)?;
    let now = st
        .discord
        .call(Method::PATCH, &rule_path(guild, &rule)?, Some(b))
        .await?;
    trust_rule(&st, guild, &rule).await?;
    approve_rule(&st.db, guild, &now, s.user_id).await?;
    let v = set_bot_actions(&st, &s, guild, &rule, &spec).await?;
    audit_version(
        &st,
        &s,
        guild,
        format!("edited the Discord AutoMod rule `{}`", spec.name),
        v,
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

/// "Migrate words to Discord's AutoMod": turns "Use Discord AutoMod" on, so the bot's sync (run on
/// this save) creates or updates its one keyword rule. The local lists stay: thread and reaction
/// names still read them, and messages read only what the rule does not block.
async fn migrate_words(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    require(&st, &s, guild, Level::Editor, false).await?;
    let (version, mut config) = crate::store::current(&st.db, guild).await?;
    let n = migrate_plan(&mut config["automod"]["filters"]["words"])?;
    let mut tx = st.db.begin().await?;
    let v = crate::store::commit(&mut tx, guild, version, &config, s.user_id).await?;
    tx.commit().await?;
    audit_version(
        &st,
        &s,
        guild,
        format!("migrated {n} word(s) to Discord AutoMod"),
        Some(v),
    )
    .await?;
    Ok(Json(json!({"ok": true, "migrated": n})))
}

/// Switches the words config `w` to the sync; the number of words it hands to Discord.
fn migrate_plan(w: &mut Value) -> Result<usize, ApiError> {
    if w["use_discord_automod"] == true {
        return Err(ApiError::BadRequest(
            "The words are already kept in Discord's AutoMod".into(),
        ));
    }
    let list =
        |k: &str| -> Vec<String> { serde_json::from_value(w[k].clone()).unwrap_or_default() };
    let flag = |k: &str| w[k] == true;
    let n = list("strict").len()
        + list("wildcard").len()
        + brim_automod::filters::premade_words(
            flag("premade_profane"),
            flag("premade_vulgar"),
            flag("premade_racist"),
        )
        .len();
    if n == 0 {
        return Err(ApiError::BadRequest("There are no words to migrate".into()));
    }
    w["use_discord_automod"] = json!(true);
    Ok(n)
}

#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
}

async fn set_enabled(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, rule)): Path<(i64, String)>,
    Json(b): Json<EnabledBody>,
) -> Res {
    require(&st, &s, guild, Level::Editor, false).await?;
    let now = st
        .discord
        .call(
            Method::PATCH,
            &rule_path(guild, &rule)?,
            Some(json!({"enabled": b.enabled})),
        )
        .await?;
    // The switch approves the rule only if nothing else changed since its approval: otherwise it
    // would approve a change made in Discord that nobody here reviewed.
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT fingerprint FROM automod_rule_approved WHERE guild_id = $1 AND rule_id = $2",
    )
    .bind(guild)
    .bind(rule.parse::<i64>().unwrap_or(0))
    .fetch_optional(&st.db)
    .await?;
    let only_switched = [true, false].iter().any(|&on| {
        let mut was = now.clone();
        was["enabled"] = json!(on);
        stored.as_deref() == Some(rule_fingerprint(&was).as_str())
    });
    if only_switched {
        approve_rule(&st.db, guild, &now, s.user_id).await?;
        trust_rule(&st, guild, &rule).await?;
    }
    audit(
        &st,
        &s,
        guild,
        format!(
            "{} a Discord AutoMod rule",
            if b.enabled {
                "activated"
            } else {
                "deactivated"
            }
        ),
    )
    .await?;
    if !only_switched {
        return Err(ApiError::Busy(
            "The switch is set, but the rule was changed outside the dashboard. Open it, review it and save it: until then Brim takes no action on its hits.",
        ));
    }
    Ok(Json(json!({"ok": true})))
}

async fn delete_rule(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, rule)): Path<(i64, String)>,
) -> Res {
    require(&st, &s, guild, Level::Editor, false).await?;
    st.discord
        .call(Method::DELETE, &rule_path(guild, &rule)?, None)
        .await?;
    let v = set_bot_actions(&st, &s, guild, &rule, &RuleSpec::default()).await?;
    audit_version(&st, &s, guild, "deleted a Discord AutoMod rule".into(), v).await?;
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only when DATABASE_URL points at a scratch Postgres. Bot actions are saved after the
    /// Discord rule exists, so saves running at the same time must all land, none refused with 409.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bot_actions_of_parallel_rules_all_land() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_860_000_000 + std::process::id() as i64;
        let st = AppState {
            db: db.clone(),
            discord: crate::discord::Discord {
                http: reqwest::Client::new(),
                client_id: String::new(),
                client_secret: String::new(),
                bot_token: String::new(),
                redirect_uri: String::new(),
                api: String::new(),
                memo: Default::default(),
                limits: Default::default(),
            },
            secure: false,
            rate: Default::default(),
            ips: Default::default(),
            vpn_logged: Default::default(),
        };
        let s = Sess {
            id: String::new(),
            user_id: 7,
            username: "t".into(),
            access_token: String::new(),
            stepped_up: true,
            step_up_guild: Some(g),
            csrf: String::new(),
        };
        let mut runs = tokio::task::JoinSet::new();
        for rule in 1..=8 {
            let (st, s) = (st.clone(), s.clone());
            runs.spawn(async move {
                let spec = RuleSpec {
                    bot_actions: vec![BotAction::Warn],
                    ..Default::default()
                };
                set_bot_actions(&st, &s, g, &rule.to_string(), &spec).await
            });
        }
        for r in runs.join_all().await {
            r.unwrap();
        }
        let (v, c) = crate::store::current(&db, g).await.unwrap();
        assert_eq!(v, 8);
        assert_eq!(
            c["discord_automod"]["actions"].as_object().unwrap().len(),
            8
        );
        // A rule without a mention scope or lock never stores `null` for those maps.
        for k in ["scopes", "locks"] {
            let saved = c["discord_automod"].get(k);
            assert!(saved.is_none_or(|v| !v.is_null()), "{k} saved as null");
        }
        sqlx::query("delete from guild_config where guild_id = $1")
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. An Editor's create and edit
    /// approve the rule as Discord returns it, so the bot acts on that state.
    #[tokio::test]
    async fn editor_saves_approve_the_rule_discord_returns() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_906_000_000 + std::process::id() as i64;
        let owner = g + 1;
        let rule = |words: Value| {
            json!({"id": "55", "guild_id": g.to_string(), "name": "r", "event_type": 1,
                   "trigger_type": 1, "trigger_metadata": {"keyword_filter": words},
                   "actions": [{"type": 1}], "enabled": true, "exempt_roles": [], "exempt_channels": []})
        };
        let (made, edited) = (rule(json!(["a"])), rule(json!(["b"])));
        let (m, e) = (made.clone(), edited.clone());
        let mock = Router::new()
            .route(
                "/guilds/{g}",
                get(move || async move { Json(json!({"owner_id": owner.to_string()})) }),
            )
            .route(
                "/guilds/{g}/auto-moderation/rules",
                post(move || async move { Json(m) }),
            )
            .route(
                "/guilds/{g}/auto-moderation/rules/{r}",
                axum::routing::patch(move || async move { Json(e) }),
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
        let s = Sess {
            id: String::new(),
            user_id: owner,
            username: "t".into(),
            access_token: String::new(),
            stepped_up: true,
            step_up_guild: Some(g),
            csrf: String::new(),
        };
        let approved = || async {
            sqlx::query_as::<_, (String, i64)>(
                "SELECT fingerprint, approved_by FROM automod_rule_approved
                 WHERE guild_id = $1 AND rule_id = 55",
            )
            .bind(g)
            .fetch_one(&db)
            .await
            .unwrap()
        };
        let spec = || RuleSpec {
            name: "r".into(),
            trigger: "keyword".into(),
            keywords: vec!["a".into()],
            block_message: true,
            ..Default::default()
        };
        let _ = create_rule(State(st.clone()), s.clone(), Path(g), Json(spec()))
            .await
            .unwrap();
        let fp = brim_automod::discord_api::rule_fingerprint;
        assert_eq!(approved().await, (fp(&made), owner));
        let _ = update_rule(
            State(st.clone()),
            s.clone(),
            Path((g, "55".into())),
            Json(spec()),
        )
        .await
        .unwrap();
        assert_eq!(approved().await, (fp(&edited), owner));
        // The switch approves only when nothing else changed: Discord returns `edited`.
        let switch = |on| Json(EnabledBody { enabled: on });
        let path = || Path((g, "55".into()));
        let _ = set_enabled(State(st.clone()), s.clone(), path(), switch(false))
            .await
            .unwrap();
        assert_eq!(approved().await, (fp(&edited), owner));
        approve_rule(&db, g, &made, 1).await.unwrap();
        let r = set_enabled(State(st.clone()), s.clone(), path(), switch(false)).await;
        assert!(matches!(r, Err(ApiError::Busy(_))), "changed outside");
        assert_eq!(approved().await, (fp(&made), 1), "not approved");
        for t in [
            "automod_rule_approved",
            "automod_rule_untrusted",
            "guild_config",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {t} WHERE guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }

    /// Runs only when DATABASE_URL points at a scratch Postgres. A rule counts as approved like the
    /// bot decides it: untrusted never, else its row's fingerprint, else (no row, guild not done) yes.
    #[tokio::test]
    async fn approved_follows_the_bot() {
        let Ok(url) = brim_core::db::test_url() else {
            return;
        };
        let db = brim_core::db::connect(&url).await.unwrap();
        let g = 9_907_000_000 + std::process::id() as i64;
        let rule = |id: &str, w: &str| {
            json!({"id": id, "name": "r", "event_type": 1, "trigger_type": 1,
                   "trigger_metadata": {"keyword_filter": [w]}, "actions": [{"type": 1}], "enabled": true})
        };
        let (same, changed, bad) = (rule("61", "a"), rule("62", "b"), rule("63", "c"));
        approve_rule(&db, g, &same, 1).await.unwrap();
        approve_rule(&db, g, &rule("62", "old"), 1).await.unwrap();
        sqlx::query(
            "INSERT INTO automod_rule_untrusted (guild_id, rule_id, audit_id, actor, trusted)
             VALUES ($1, 63, 1, 5, false)",
        )
        .bind(g)
        .execute(&db)
        .await
        .unwrap();
        let all = [same, changed, bad, rule("64", "d"), rule("65", "e")];
        // Rule 65 has no Brim actions, so it is never shown as paused.
        let mut mine = DiscordAutomodCfg::default();
        for id in ["61", "62", "63", "64"] {
            mine.actions.insert(id.into(), vec![BotAction::Warn]);
        }
        assert_eq!(
            approvals(&db, g, &all, &mine).await.unwrap(),
            [true, false, false, true, true],
            "not done"
        );
        sqlx::query(
            "INSERT INTO automod_rule_approved (guild_id, rule_id, fingerprint, approved_by)
             VALUES ($1, 0, '', 0)",
        )
        .bind(g)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(
            approvals(&db, g, &all, &mine).await.unwrap(),
            [true, false, false, false, true],
            "done"
        );
        for t in ["automod_rule_approved", "automod_rule_untrusted"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {t} WHERE guild_id = $1"
            )))
            .bind(g)
            .execute(&db)
            .await
            .unwrap();
        }
    }

    /// Migrating only turns the sync on and keeps the local lists (thread and reaction names read
    /// them); the sync makes the one rule. A second migrate is refused.
    #[test]
    fn migrate_keeps_the_local_lists() {
        let mut w = json!({"strict": ["darn"], "wildcard": ["heck"], "premade_vulgar": false});
        assert_eq!(migrate_plan(&mut w).unwrap(), 2);
        assert_eq!(w["strict"], json!(["darn"]));
        assert_eq!(w["wildcard"], json!(["heck"]));
        assert_eq!(w["use_discord_automod"], true);
        assert!(migrate_plan(&mut w).is_err());
        assert!(migrate_plan(&mut json!({})).is_err(), "no words");
    }

    #[test]
    fn rule_ids_must_be_snowflakes() {
        assert!(rule_path(1, "123").is_ok());
        assert!(rule_path(1, "../roles/1").is_err());
        assert!(rule_path(1, "").is_err());
    }

    #[test]
    fn apply_fix_runs_only_the_automatic_fixes_the_page_showed() {
        let p = |id: &str, opt_in| Problem {
            id: id.into(),
            text: String::new(),
            fix: Some(Fix::SetRolePerms { role: 1, perms: 0 }),
            opt_in,
        };
        let list = [p("a", false), p("b", false), p("e", true)];
        let pick = |shown: &[&str], picked: &[&str]| FixPick {
            shown: shown.iter().map(|s| s.to_string()).collect(),
            picked: picked.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(
            super::chosen(&list, &pick(&["b", "a"], &[])).map(|f| f.len()),
            Some(2)
        );
        assert_eq!(
            super::chosen(&list, &pick(&["a", "b"], &["e"])).map(|f| f.len()),
            Some(3)
        );
        // A new automatic fix appeared since the page loaded.
        assert!(super::chosen(&list, &pick(&["a"], &[])).is_none());
    }
}

//! Giveaways page: the running and ended giveaways of a server, a form to start one, and the
//! actions on them. Needs the Manage Giveaways permit perm (`giveaways`) or Trusted Admin and up,
//! as the `giveaway` command. Rows and rules are in `brim_core::giveaways`; what needs Discord
//! (the post, cancel, reroll, redraw) is a job the bot runs. Every change is audited.
use crate::{
    api::{self, Res},
    auth::Sess,
    lockdown::audit,
    ApiError, AppState,
};
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use brim_core::{
    giveaways::{self as store, EndNow, Extras, Giveaway, Req},
    jobs,
    sections::GiveawaysCfg,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    let g = "/api/guilds/{guild}/giveaways";
    Router::new()
        .route(g, get(list).post(start))
        .route(&format!("{g}/{{id}}"), post(edit))
        .route(&format!("{g}/{{id}}/end"), post(end))
        .route(&format!("{g}/{{id}}/cancel"), post(cancel))
        .route(&format!("{g}/{{id}}/reroll"), post(reroll))
        .route(&format!("{g}/{{id}}/time"), post(time))
        .route(&format!("{g}/{{id}}/claim"), post(claim_time))
        .route(&format!("{g}/{{id}}/channel"), post(channel))
        .route(&format!("{g}/{{id}}/entries"), get(entries))
}

fn bad(s: impl Into<String>) -> ApiError {
    ApiError::BadRequest(s.into())
}

async fn staff(st: &AppState, s: &Sess, guild: i64) -> Result<(), ApiError> {
    api::staff(st, s, guild, &["giveaways"]).await.map(drop)
}

/// A change: [`staff`], and not while panic mode locks mod commands (as the `giveaway` command).
async fn writer(st: &AppState, s: &Sess, guild: i64) -> Result<(), ApiError> {
    staff(st, s, guild).await?;
    crate::appeals::mod_lock_guard(st, s, guild, &mut *st.db.acquire().await?).await
}

/// A host or sponsor: a member of this server.
async fn member(
    st: &AppState,
    guild: i64,
    id: Option<i64>,
    what: &str,
) -> Result<Option<i64>, ApiError> {
    let Some(id) = id else { return Ok(None) };
    if id <= 0 {
        return Err(bad(format!("the {what} is not a Discord id")));
    }
    match st.discord.member(guild, &id.to_string()).await {
        Ok(Some(_)) => Ok(Some(id)),
        Ok(None) => Err(bad(format!("the {what} is not a member of this server"))),
        Err(_) => Err(bad(format!("could not check the {what}; try again"))),
    }
}

/// Discord ids go out as text: a JavaScript number cannot hold them.
fn ids(v: &[i64]) -> Vec<String> {
    v.iter().map(i64::to_string).collect()
}

fn row(g: &Giveaway, entries: i64) -> Value {
    row_with(g, entries, &[])
}

/// A giveaway for the page, with the claim state of each winner place.
fn row_with(g: &Giveaway, entries: i64, claims: &[store::Claim]) -> Value {
    let claims: Vec<Value> = claims
        .iter()
        .filter(|c| c.giveaway_id == g.id)
        .map(|c| {
            json!({"place": c.place, "user": c.user_id.to_string(), "deadline": c.deadline.timestamp(),
                   "claimed": c.claimed_at.map(|t| t.timestamp()), "redraws": c.redraws})
        })
        .collect();
    let id = |v: Option<i64>| v.map(|x| x.to_string());
    json!({
        "id": g.id, "channel": g.channel_id.to_string(), "message": id(g.message_id),
        "prize": g.prize, "host": id(g.host_id), "sponsor": id(g.sponsor_id),
        "winners": g.winners, "status": g.status, "ends_at": g.ends_at.timestamp(),
        "ended_at": g.ended_at.map(|t| t.timestamp()), "paused": g.paused_at.is_some(),
        "req": g.req.0, "winner_ids": ids(&g.winner_ids), "draws": g.draws,
        "needs_repost": g.needs_repost, "issue": g.issue, "entries": entries,
        "claim_hours": g.claim_hours, "claims": claims,
        "extras": g.extras.0, "has_code": g.has_code, "thread": id(g.thread_id),
    })
}

async fn list(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    staff(&st, &s, guild).await?;
    let all = store::list(&st.db, guild, 25).await?;
    let all_ids: Vec<i64> = all.iter().map(|g| g.id).collect();
    let counts = store::entry_counts(&st.db, guild, &all_ids).await?;
    let claims = store::claims(&st.db, guild, &all_ids).await?;
    let n = |id| counts.iter().find(|c| c.0 == id).map_or(0, |c| c.1);
    // The start form begins with the server's default rules.
    let (_, c) = crate::store::current(&st.db, guild).await?;
    let d: GiveawaysCfg = serde_json::from_value(c["giveaways"].clone()).unwrap_or_default();
    let defaults = Req {
        blocked: d.blocked_roles.clone(),
        min_age_days: d.min_account_age_days,
        min_member_hours: d.min_member_hours,
        block_raid_joiners: d.block_raid_joiners,
        ..Req::default()
    };
    Ok(Json(json!({
        "giveaways": all.iter().map(|g| row_with(g, n(g.id), &claims)).collect::<Vec<_>>(),
        "defaults": defaults,
        "extras": Extras::default(),
        "on": settings(&st, guild).await?.is_some(),
        "limits": {
            "winners": store::WINNERS.1, "max_secs": store::DURATION.1, "min_secs": store::DURATION.0,
            "prize": store::MAX_PRIZE, "roles": store::MAX_ROLES, "extra": store::EXTRA.1,
            "claim_hours": store::CLAIM_HOURS.1, "claim_default": d.claim_hours,
        },
    })))
}

/// The server's giveaway settings; `None` while the part or the Messages module is off.
async fn settings(st: &AppState, guild: i64) -> Result<Option<GiveawaysCfg>, ApiError> {
    let (_, c) = crate::store::current(&st.db, guild).await?;
    if c["modules"]["messages"] == false {
        return Ok(None);
    }
    let cfg: GiveawaysCfg = serde_json::from_value(c["giveaways"].clone()).unwrap_or_default();
    Ok(cfg.enabled.then_some(cfg))
}

fn id_of(s: &Option<String>) -> Result<Option<i64>, ApiError> {
    s.as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>()
                .map_err(|_| bad("that is not a Discord id"))
        })
        .transpose()
}

#[derive(Deserialize)]
struct Start {
    channel: String,
    prize: String,
    winners: i64,
    duration_secs: i64,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    sponsor: Option<String>,
    #[serde(default)]
    req: Req,
    /// Hours the winners have to claim; empty: the server's default.
    #[serde(default)]
    claim_hours: Option<i64>,
    #[serde(default)]
    extras: Extras,
    /// The entry code; empty: none. Only its hash is kept.
    #[serde(default)]
    code: String,
}

/// The winner role must be one Brim can give to anyone: below Brim, not managed, not @everyone,
/// with no dangerous Discord permissions. The bot checks again when it gives it.
async fn check_role(st: &AppState, guild: i64, x: &Extras) -> Result<(), ApiError> {
    let Some(role) = x.winner_role else {
        return Ok(());
    };
    let (roles, bot_pos) = crate::ops::roles_and_bot_pos(st, guild).await?;
    let r = roles
        .iter()
        .find(|r| r.id == role.to_string())
        .ok_or_else(|| bad("the winner role is not a role of this server"))?;
    if let Some(why) = r.unfit(guild, bot_pos) {
        return Err(bad(format!("Brim cannot give the winner role: {why}")));
    }
    if brim_antinuke::has_dangerous(r.perms()) {
        return Err(bad(
            "the winner role has dangerous Discord permissions: pick a role without them",
        ));
    }
    if brim_core::permits::permit_roles(&st.db, guild)
        .await?
        .contains(&(role.get() as i64))
    {
        return Err(bad(
            "the winner role holds a permit: pick a role without one",
        ));
    }
    Ok(())
}

async fn start(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Start>,
) -> Res {
    writer(&st, &s, guild).await?;
    let Some(cfg) = settings(&st, guild).await? else {
        return Err(bad("Giveaways are off: turn them on above first"));
    };
    let channel: i64 = b.channel.parse().map_err(|_| bad("pick a channel"))?;
    crate::feeds::check_channel(&st, guild, channel).await?;
    check_role(&st, guild, &b.extras).await?;
    let code = Some(b.code).filter(|c| !c.trim().is_empty());
    let new = store::New {
        guild_id: guild,
        channel_id: channel,
        prize: b.prize,
        host_id: member(&st, guild, id_of(&b.host)?, "host").await?,
        sponsor_id: member(&st, guild, id_of(&b.sponsor)?, "sponsor").await?,
        created_by: s.user_id,
        winners: b.winners,
        duration_secs: b.duration_secs,
        req: b.req,
        button_label: cfg.button_label.clone(),
        button_emoji: cfg.button_emoji.clone(),
        claim_hours: b.claim_hours.unwrap_or(cfg.claim_hours as i64),
        extras: b.extras,
        code,
        post_job: true,
    };
    let g = store::create(&st.db, &new, cfg.max_running as i64)
        .await?
        .map_err(bad)?;
    audit(
        &st,
        &s,
        guild,
        format!("started the giveaway #{} “{}”", g.id, g.prize),
    )
    .await?;
    Ok(Json(row(&g, 0)))
}

async fn found(st: &AppState, guild: i64, id: i64) -> Result<Giveaway, ApiError> {
    store::get(&st.db, guild, id)
        .await?
        .ok_or(ApiError::NotFound)
}

#[derive(Deserialize)]
struct Edit {
    prize: String,
    winners: i64,
    #[serde(default)]
    req: Req,
    #[serde(default)]
    extras: Extras,
}

async fn edit(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Edit>,
) -> Res {
    writer(&st, &s, guild).await?;
    // Entries keep the weight they got at the press: extra entries are fixed once anyone entered.
    let old = found(&st, guild, id).await?;
    let entered = store::entry_counts(&st.db, guild, &[id]).await?;
    if old.req.extra != b.req.extra && entered.first().is_some_and(|c| c.1 > 0) {
        return Err(bad("the extra entries cannot change once members entered"));
    }
    if old.extras.winner_role != b.extras.winner_role {
        check_role(&st, guild, &b.extras).await?;
    }
    if !store::edit(&st.db, guild, id, &b.prize, b.winners, &b.req, &b.extras)
        .await?
        .map_err(bad)?
    {
        return Err(bad("only a running giveaway can be changed"));
    }
    // The message shows the change at the next refresh (within a minute).
    jobs::enqueue_once(&st.db, guild, store::REFRESH_JOB, chrono_now()).await?;
    audit(&st, &s, guild, format!("changed the giveaway #{id}")).await?;
    Ok(Json(row(&found(&st, guild, id).await?, 0)))
}

fn chrono_now() -> i64 {
    sqlx::types::chrono::Utc::now().timestamp()
}

async fn end(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    writer(&st, &s, guild).await?;
    match store::end_now(&st.db, guild, id).await? {
        EndNow::NotRunning => return Err(bad("this giveaway is not running")),
        EndNow::Paused => {
            return Err(bad(
                "this giveaway is paused while the server is under attack; end it when the attack is over",
            ))
        }
        _ => {}
    }
    audit(&st, &s, guild, format!("ended the giveaway #{id}")).await?;
    Ok(Json(json!({"ok": true})))
}

/// Queues an action the bot runs with Discord.
async fn action(st: &AppState, guild: i64, payload: Value) -> Result<(), ApiError> {
    jobs::enqueue(&st.db, guild, store::ACTION_JOB, payload, chrono_now()).await?;
    Ok(())
}

async fn cancel(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    writer(&st, &s, guild).await?;
    if !found(&st, guild, id).await?.running() {
        return Err(bad("this giveaway is not running"));
    }
    action(
        &st,
        guild,
        json!({"op": "cancel", "id": id, "by": s.user_id}),
    )
    .await?;
    audit(&st, &s, guild, format!("cancelled the giveaway #{id}")).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Reroll {
    /// Winners to replace; empty: all of them.
    #[serde(default)]
    users: Vec<String>,
    #[serde(default)]
    reason: String,
}

async fn reroll(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Reroll>,
) -> Res {
    writer(&st, &s, guild).await?;
    let g = found(&st, guild, id).await?;
    if g.status != "ended" {
        return Err(bad("only an ended giveaway can be rerolled"));
    }
    let users = b
        .users
        .iter()
        .map(|u| u.parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| bad("that is not a Discord id"))?;
    if users.iter().any(|u| !g.winner_ids.contains(u)) {
        return Err(bad("pick winners of this giveaway"));
    }
    let reason: String = b.reason.trim().chars().take(300).collect();
    action(
        &st,
        guild,
        // The draw it was pressed on: a second press for the same draw does nothing.
        json!({"op": "reroll", "id": id, "by": s.user_id, "users": users, "reason": reason, "draws": g.draws}),
    )
    .await?;
    audit(&st, &s, guild, format!("rerolled the giveaway #{id}")).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Time {
    secs: i64,
}

async fn time(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Time>,
) -> Res {
    writer(&st, &s, guild).await?;
    let secs = b.secs.clamp(-store::DURATION.1, store::DURATION.1);
    let Some(ends) = store::shift(&st.db, guild, id, secs).await? else {
        return Err(bad("this giveaway is not running"));
    };
    sqlx::query("UPDATE giveaways SET shown_entries = -1 WHERE id = $1 AND guild_id = $2")
        .bind(id)
        .bind(guild)
        .execute(&st.db)
        .await?;
    jobs::enqueue_once(&st.db, guild, store::REFRESH_JOB, chrono_now()).await?;
    audit(
        &st,
        &s,
        guild,
        format!("moved the end of the giveaway #{id}"),
    )
    .await?;
    Ok(Json(json!({"ends_at": ends.timestamp()})))
}

#[derive(Deserialize)]
struct ClaimTime {
    hours: i64,
}

/// Staff set the claim time by hand: on an ended giveaway, every winner who has not claimed has
/// that long from now.
async fn claim_time(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<ClaimTime>,
) -> Res {
    writer(&st, &s, guild).await?;
    if !store::set_claim_time(&st.db, guild, id, b.hours)
        .await?
        .map_err(bad)?
    {
        return Err(bad("this giveaway was cancelled or is gone"));
    }
    audit(
        &st,
        &s,
        guild,
        format!(
            "set the claim time of the giveaway #{id} to {} hours",
            b.hours
        ),
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Channel {
    channel: String,
}

/// A new channel for a giveaway whose message could not be posted (channel deleted, no access).
async fn channel(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Channel>,
) -> Res {
    writer(&st, &s, guild).await?;
    let g = found(&st, guild, id).await?;
    if g.message_id.is_some() {
        return Err(bad(
            "the giveaway message is posted: delete it in Discord first",
        ));
    }
    let channel: i64 = b.channel.parse().map_err(|_| bad("pick a channel"))?;
    crate::feeds::check_channel(&st, guild, channel).await?;
    if !store::queue_post(&st.db, guild, id, Some(channel), false).await? {
        return Err(bad("this giveaway is not running"));
    }
    audit(
        &st,
        &s,
        guild,
        format!("moved the giveaway #{id} to another channel"),
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    page: i64,
}

async fn entries(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Query(p): Query<Page>,
) -> Res {
    staff(&st, &s, guild).await?;
    let g = found(&st, guild, id).await?;
    let total = store::entry_counts(&st.db, guild, &[id])
        .await?
        .first()
        .map_or(0, |c| c.1);
    let rows = store::entries_page(&st.db, guild, g.id, p.page).await?;
    Ok(Json(json!({
        "total": total, "per_page": store::ENTRIES_PAGE, "page": p.page.max(0),
        "entries": rows.iter().map(|(u, w)| json!({"user": u.to_string(), "weight": w})).collect::<Vec<_>>(),
    })))
}

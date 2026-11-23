//! Giveaway templates and plans (see `brim_core::giveaway_plans`): the list on the Giveaways page,
//! save and delete, a preview of the next runs, and the runner that starts a plan's giveaway. A
//! plan runs with the rights of who last saved it (the `giveaways` permit or Trusted Admin and up)
//! and stops when they have lost them.
use crate::{
    api::{self, Res},
    auth::Sess,
    giveaways::{bad, check_role, id_of, member, settings, writer},
    lockdown::audit,
    msgrules::lines_view,
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use brim_core::{
    giveaway_plans::{self as plans, Plan, Ran},
    giveaways::{Extras, Req},
    msgsched,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    let g = "/api/guilds/{guild}/giveaway-plans";
    Router::new()
        .route(g, post(save))
        .route(&format!("{g}/preview"), post(preview))
        .route(&format!("{g}/{{id}}/delete"), post(delete))
}

fn row(p: &Plan) -> Value {
    let id = |v: Option<i64>| v.map(|x| x.to_string());
    let now = Utc::now();
    let lines = p
        .spec
        .as_ref()
        .and_then(|s| msgsched::parse(s, now).ok())
        .map(|s| lines_view(&s, now));
    json!({
        "id": p.id, "name": p.name, "channel": id(p.channel_id), "prize": p.prize,
        "host": id(p.host_id), "sponsor": id(p.sponsor_id), "winners": p.winners,
        "duration_secs": p.duration_secs, "claim_hours": p.claim_hours, "req": p.req.0,
        "extras": p.extras.0, "spec": p.spec, "lines": lines, "enabled": p.enabled,
        "wait_for_end": p.wait_for_end, "held": p.held, "issue": p.issue,
        "next": p.next_at.map(|t| t.timestamp()), "last_run": p.last_run.map(|t| t.timestamp()),
    })
}

/// The templates and plans of a server for the page.
pub(crate) async fn view(st: &AppState, guild: i64) -> Result<Value, ApiError> {
    let all = plans::list(&st.db, guild).await?;
    let (plan, template): (Vec<&Plan>, Vec<&Plan>) = all.iter().partition(|p| p.spec.is_some());
    Ok(json!({
        "plans": plan.into_iter().map(row).collect::<Vec<_>>(),
        "templates": template.into_iter().map(row).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct SaveIn {
    /// The template or plan to change; none: a new one.
    #[serde(default)]
    id: Option<i64>,
    name: String,
    #[serde(default)]
    channel: Option<String>,
    prize: String,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    sponsor: Option<String>,
    winners: i64,
    duration_secs: i64,
    #[serde(default)]
    claim_hours: Option<i64>,
    #[serde(default)]
    req: Req,
    #[serde(default)]
    extras: Extras,
    /// The schedule of a plan (`msgsched`); none: a template.
    #[serde(default)]
    spec: Option<Value>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    wait_for_end: bool,
}

async fn save(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<SaveIn>,
) -> Res {
    writer(&st, &s, guild).await?;
    let channel = id_of(&b.channel)?;
    if let Some(c) = channel {
        crate::feeds::check_channel(&st, guild, c).await?;
    }
    check_role(&st, guild, &b.extras).await?;
    let save = plans::Save {
        name: b.name,
        channel_id: channel,
        prize: b.prize,
        host_id: member(&st, guild, id_of(&b.host)?, "host").await?,
        sponsor_id: member(&st, guild, id_of(&b.sponsor)?, "sponsor").await?,
        winners: b.winners,
        duration_secs: b.duration_secs,
        claim_hours: b.claim_hours,
        req: b.req,
        extras: b.extras,
        spec: b.spec,
        enabled: b.enabled,
        wait_for_end: b.wait_for_end,
    };
    let p = plans::save(&st.db, guild, b.id, &save, s.user_id, Utc::now())
        .await?
        .map_err(bad)?;
    let what = if p.spec.is_some() { "plan" } else { "template" };
    audit(
        &st,
        &s,
        guild,
        format!("saved the giveaway {what} “{}”", p.name),
    )
    .await?;
    Ok(Json(row(&p)))
}

#[derive(Deserialize)]
struct Preview {
    spec: Value,
}

/// The next runs of a schedule being edited, before it is saved.
async fn preview(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Preview>,
) -> Res {
    crate::giveaways::staff(&st, &s, guild).await?;
    let now = Utc::now();
    let spec = msgsched::parse(&b.spec, now).map_err(ApiError::BadRequest)?;
    Ok(Json(json!({"lines": lines_view(&spec, now)})))
}

async fn delete(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    writer(&st, &s, guild).await?;
    if !plans::delete(&st.db, guild, id).await? {
        return Err(ApiError::NotFound);
    }
    audit(
        &st,
        &s,
        guild,
        format!("deleted the giveaway plan or template #{id}"),
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

/// Starts one due plan; true when one was looked at (more may be due). The saver's rights are read
/// first: one who lost them stops the plan, and a level that cannot be read now is tried again.
/// A plan whose run fails is moved a minute on with the reason, so it never blocks the others.
pub(crate) async fn run_one(st: &AppState) -> Result<bool, ApiError> {
    for (id, guild, by) in plans::due(&st.db).await? {
        match try_plan(st, id, guild, by).await {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(?e, id, "a giveaway plan could not run");
                plans::delay(
                    &st.db,
                    id,
                    "Brim could not run this plan and tries again in a minute.",
                )
                .await?;
            }
        }
    }
    Ok(false)
}

/// One candidate of [`run_one`]; true when it ran or was stopped.
async fn try_plan(st: &AppState, id: i64, guild: i64, by: i64) -> Result<bool, ApiError> {
    let saver = Sess {
        id: String::new(),
        user_id: by,
        username: String::new(),
        access_token: String::new(),
        stepped_up: false,
        step_up_guild: None,
        csrf: String::new(),
    };
    match api::staff(st, &saver, guild, &["giveaways"]).await {
        Ok(_) => {}
        Err(ApiError::Forbidden(_)) => {
            let why = format!("Stopped: the member who last saved this plan (user id {by}) can no longer manage giveaways. Someone who can saves it again and turns it on.");
            plans::pause(&st.db, guild, id, by, &why).await?;
            return Ok(true);
        }
        Err(e) => return Err(e),
    }
    let cfg = settings(st, guild).await?;
    match plans::run(&st.db, id, by, cfg.as_ref()).await? {
        Some(Ran::Started(g)) => tracing::info!(plan = id, giveaway = g, "giveaway plan started"),
        Some(_) => {}
        None => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brim_core::giveaway_plans::Save;

    /// The runner starts the plan of an owner and stops the plan of a member without rights.
    #[tokio::test]
    async fn the_runner_stops_a_plan_whose_saver_lost_their_rights() {
        let Some((st, _, g)) = crate::api::g3_handler_tests::setup(9_520_000).await else {
            return;
        };
        st.discord.memo.lock().unwrap().insert(
            format!("/guilds/{g}/members/9"),
            (std::time::Instant::now(), Some(json!({"roles": []}))),
        );
        sqlx::query("insert into guild_config (guild_id, version, config) values ($1, 1, $2)")
            .bind(g)
            .bind(json!({"giveaways": {"enabled": true}}))
            .execute(&st.db)
            .await
            .unwrap();
        let hourly = Some(json!({"tz": "UTC", "lines": [{"kind": "every", "minutes": 60}]}));
        let mut ids = vec![];
        for (name, by) in [("Owner", 7), ("Gone", 9)] {
            let s = Save {
                name: name.into(),
                channel_id: Some(2),
                prize: "Nitro".into(),
                host_id: None,
                sponsor_id: None,
                winners: 1,
                duration_secs: 3600,
                claim_hours: None,
                req: Req::default(),
                extras: Extras::default(),
                spec: hourly.clone(),
                enabled: true,
                wait_for_end: false,
            };
            let p = plans::save(&st.db, g, None, &s, by, Utc::now())
                .await
                .unwrap()
                .unwrap();
            sqlx::query(
                "update giveaway_plans set next_at = now() - interval '1 minute' where id = $1",
            )
            .bind(p.id)
            .execute(&st.db)
            .await
            .unwrap();
            ids.push(p.id);
        }
        while run_one(&st).await.unwrap() {}
        let made = |plan: i64| {
            sqlx::query_scalar::<_, i64>("select count(*) from giveaways where plan_id = $1")
                .bind(plan)
                .fetch_one(&st.db)
        };
        assert_eq!(made(ids[0]).await.unwrap(), 1, "the owner's plan started");
        assert_eq!(made(ids[1]).await.unwrap(), 0);
        let (on, issue): (bool, Option<String>) =
            sqlx::query_as("select enabled, issue from giveaway_plans where id = $1")
                .bind(ids[1])
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert!(!on);
        assert!(issue
            .unwrap()
            .starts_with("Stopped: the member who last saved this plan (user id 9)"));
        for q in [
            "delete from giveaway_plans where guild_id = $1",
            "delete from giveaways where guild_id = $1",
            "delete from scheduled_jobs where guild_id = $1",
            "delete from guild_config where guild_id = $1",
        ] {
            sqlx::query(q).bind(g).execute(&st.db).await.unwrap();
        }
    }
}

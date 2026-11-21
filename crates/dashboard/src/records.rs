//! Staff records: cases, notes and reports (with the reporter blacklist). Each page needs its permit.
use crate::{api, auth::Sess, mcp::Via, ApiError, AppState};
use axum::{
    extract::{Path, Query, State},
    response::Html,
    routing::{get, post},
    Extension, Json, Router,
};
use brim_core::{
    cases::{self, CaseLog, CASE_TEXT_MAX},
    jobs,
    notes::{self, NoteAction, NoteLog},
};
use serde::Deserialize;
use serde_json::{json, Value};

type Res = Result<Json<Value>, ApiError>;
type At = chrono::DateTime<chrono::Utc>;
/// Number, kind, member, moderator, reason, proofs, points, open, created.
type CaseRow = (
    i32,
    String,
    String,
    String,
    Option<String>,
    i64,
    i32,
    bool,
    At,
);

const NOTE_MAX: usize = 500;
fn ok() -> Res {
    Ok(Json(json!({"ok": true})))
}

pub fn routes() -> Router<AppState> {
    let g = "/api/guilds/{guild}";
    Router::new()
        .route("/raid/{code}", get(raid_page))
        .route(&format!("{g}/cases"), get(cases))
        .route(&format!("{g}/cases/{{no}}"), get(case_page).post(case_edit))
        .route(&format!("{g}/cases/{{no}}/proofs"), post(proof_add))
        .route(
            &format!("{g}/cases/{{no}}/proofs/{{id}}/delete"),
            post(proof_remove),
        )
        .route(&format!("{g}/cases/{{no}}/delete"), post(case_delete))
        .route(&format!("{g}/mod_stats"), get(mod_stats))
        .route(&format!("{g}/mod_stats/{{user}}"), get(mod_actions))
        .route(&format!("{g}/raid_log"), get(raid_log))
        .route(&format!("{g}/joingate_log"), get(joingate_log))
        .route(&format!("{g}/raider_matches"), get(raider_matches))
        .route(&format!("{g}/notes"), get(notes).post(note_add))
        .route(&format!("{g}/notes/sweep"), post(note_sweep))
        .route(&format!("{g}/notes/{{id}}"), post(note_edit))
        .route(&format!("{g}/notes/{{id}}/delete"), post(note_delete))
        .route(&format!("{g}/reports"), get(reports))
        .route(&format!("{g}/reports/{{id}}/status"), post(report_status))
        .route(
            &format!("{g}/reports/blacklist"),
            get(blacklist).post(blacklist_add),
        )
        .route(
            &format!("{g}/reports/blacklist/{{user}}/remove"),
            post(blacklist_remove),
        )
}

fn user_id(raw: &str) -> Result<i64, ApiError> {
    raw.trim()
        .parse()
        .map_err(|_| ApiError::BadRequest("user ID must be a number".into()))
}

fn clip(text: &str, max: usize) -> Result<String, ApiError> {
    let t = text.trim();
    match t.chars().count() {
        0 => Err(ApiError::BadRequest("text is required".into())),
        n if n > max => Err(ApiError::BadRequest(format!(
            "longer than {max} characters"
        ))),
        _ => Ok(t.into()),
    }
}

/// The last 20 join raids, newest first, with every account each one caught.
async fn raid_log(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::staff(&st, &s, guild, &["cases"]).await?;
    let rows: Vec<(i64, String, String, At)> = sqlx::query_as(
        "select raid_ms, user_id::text, name, created_at from raid_log
         where guild_id = $1 and raid_ms in
           (select distinct raid_ms from raid_log where guild_id = $1 order by raid_ms desc limit 20)
         order by raid_ms desc, created_at",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    let mut raids: Vec<Value> = vec![];
    for (raid, user, name, at) in rows {
        if raids.last().is_none_or(|r| r["raid_ms"] != raid) {
            raids.push(json!({"raid_ms": raid, "accounts": []}));
        }
        let accounts = raids.last_mut().unwrap()["accounts"]
            .as_array_mut()
            .unwrap();
        accounts.push(json!({"user_id": user, "name": name, "at": at}));
    }
    Ok(Json(json!(raids)))
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    page: i64,
}

/// Join Gate punishments, newest first: what matched, the bot and who added it, and Discord's refusal.
async fn joingate_log(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(p): Query<Page>,
) -> Res {
    api::staff(&st, &s, guild, &["cases"]).await?;
    type Row = (
        String,
        bool,
        Option<String>,
        Value,
        String,
        String,
        bool,
        Option<String>,
        At,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select user_id::text, is_bot, adder_id::text, filters, reason, action, ok, error, created_at
         from joingate_actions where guild_id = $1 order by id desc limit 50 offset $2",
    )
    .bind(guild)
    .bind(offset(p.page))
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(user, bot, adder, filters, reason, action, ok, error, at)| json!({
            "user_id": user, "is_bot": bot, "adder_id": adder, "filters": filters, "reason": reason,
            "action": action, "ok": ok, "error": error, "at": at
        }))
        .collect::<Vec<_>>())))
}

/// What the Raider List did in this server, newest first. Only the entry's category and public note
/// come from the global list: never its evidence, other servers or who reported.
async fn raider_matches(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(p): Query<Page>,
) -> Res {
    api::require(&st, &s, guild, crate::access::Level::Admin, false).await?;
    type Row = (
        String,
        String,
        Option<i32>,
        At,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select a.user_id::text, a.action, a.case_no, a.created_at, r.category, r.public_note, r.status
         from raider_actions a left join raiders r on r.user_id = a.user_id
         where a.guild_id = $1 order by a.created_at desc, a.user_id limit 50 offset $2",
    )
    .bind(guild)
    .bind(offset(p.page))
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(user, action, case_no, at, category, note, status)| json!({
            "user_id": user, "action": action, "case_no": case_no, "at": at, "category": category,
            "public_note": note, "listed": status.as_deref() == Some("confirmed")
        }))
        .collect::<Vec<_>>())))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The public report of one join raid, open to anyone who has its code.
async fn raid_page(State(st): State<AppState>, Path(code): Path<String>) -> Html<String> {
    let rows: Vec<(i64, i64, String, Option<String>, i64)> = sqlx::query_as(
        // A member's own /invites create link may still work: the public page leaves its code out.
        "select guild_id, user_id, name,
                case when exists (select 1 from invite_links l
                                  where l.guild_id = r.guild_id and l.code = r.invite_code)
                     then null else invite_code end,
                raid_ms
         from raid_log r where code = $1 and created_at > now() - interval '30 days' order by created_at",
    )
    .bind(&code)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    // A link works 30 days; the sweep deletes the rows then (owner decision, security review).
    let body = match rows.first() {
        None => "<h1>Raid not found</h1><p>Raid links work for 30 days.</p>".to_string(),
        Some(first) => {
            let ids = rows
                .iter()
                .map(|r| r.1.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let list: String = rows
                .iter()
                .map(|(_, id, name, inv, _)| {
                    let inv = inv
                        .as_deref()
                        .map_or(String::new(), |i| format!(" via <code>{}</code>", esc(i)));
                    format!("<li><code>{id}</code> {}{inv}</li>", esc(name))
                })
                .collect();
            format!(
                "<h1>Join Raid Report</h1><p><code>{}</code></p><p>Server <code>{}</code> &middot; {}</p>\
                 <h2>{} flagged accounts</h2><button onclick=\"navigator.clipboard.writeText(this.dataset.ids)\" data-ids=\"{ids}\">Copy IDs</button><ul>{list}</ul>",
                esc(&code),
                first.0,
                chrono::DateTime::from_timestamp_millis(first.4).map_or_else(String::new, |d| d.format("%Y-%m-%d %H:%M UTC").to_string()),
                rows.len()
            )
        }
    };
    Html(format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Join Raid Report</title><style>body{{background:#1c1c1c;color:#eee;font:16px system-ui,sans-serif;max-width:40rem;margin:2rem auto;padding:0 1rem}}code{{background:#2f2f2f;padding:.1rem .4rem;border-radius:.3rem}}li{{margin:.3rem 0}}button{{background:#1be725;border:0;border-radius:.5rem;padding:.5rem 1rem;font-weight:600;cursor:pointer}}</style>{body}"#
    ))
}

/// Lists come in pages of 50 (the dashboard's `Pager`).
pub fn offset(page: i64) -> i64 {
    page.clamp(0, 10_000) * 50
}

#[derive(Deserialize)]
struct CaseFilter {
    q: Option<String>,
    #[serde(default)]
    page: i64,
    kind: Option<String>,
    /// "open", "closed" or anything else for both
    state: Option<String>,
}

async fn cases(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(f): Query<CaseFilter>,
) -> Res {
    api::staff(&st, &s, guild, &["cases"]).await?;
    let rows: Vec<CaseRow> = sqlx::query_as(
            "select case_no, kind, user_id::text, mod_id::text, reason,
                    (select count(*) from case_proofs p where p.guild_id = cases.guild_id and p.case_no = cases.case_no),
                    points, active, created_at
             from cases where guild_id = $1
               and ($2 = '' or user_id::text = $2 or mod_id::text = $2)
               and ($3 = '' or kind = $3)
               and ($4 = '' or active = ($4 = 'open'))
             order by case_no desc limit 50 offset $5",
        )
        .bind(guild)
        .bind(f.q.unwrap_or_default().trim())
        .bind(f.kind.unwrap_or_default())
        .bind(f.state.unwrap_or_default())
        .bind(offset(f.page))
        .fetch_all(&st.db)
        .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(
            |(no, kind, user, moderator, reason, proofs, points, active, at)| json!({
                "case_no": no, "kind": kind, "user_id": user, "mod_id": moderator, "reason": reason,
                "proofs": proofs, "points": points, "active": active, "at": at
            })
        )
        .collect::<Vec<_>>())))
}

/// One case with its proofs and its edit history, for the case page.
async fn case_page(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, no)): Path<(i64, i32)>,
) -> Res {
    api::staff(&st, &s, guild, &["cases"]).await?;
    let case: Option<Value> = sqlx::query_scalar(
        "select json_build_object('case_no', c.case_no, 'kind', c.kind, 'user_id', c.user_id::text,
                    'mod_id', c.mod_id::text, 'reason', c.reason, 'points', c.points, 'active', c.active,
                    'duration_s', c.duration_s, 'expires_at', c.expires_at, 'at', c.created_at,
                    'proofs_max', $3::bigint,
                    'proofs', (select coalesce(json_agg(json_build_object('id', p.id, 'kind', p.kind,
                            'body', p.body, 'verified', p.verified, 'by', p.added_by::text,
                            'ai_client', p.ai_client, 'at', p.added_at, 'channel_id', p.channel_id::text,
                            'message_id', p.message_id::text, 'sent_at', p.sent_at, 'files', p.files)
                            order by p.id), '[]'::json)
                        from case_proofs p where p.guild_id = c.guild_id and p.case_no = c.case_no),
                    'edits', (select coalesce(json_agg(json_build_object('field', e.field, 'old', e.old_value,
                            'new', e.new_value, 'by', e.editor_id::text, 'ai_client', e.ai_client,
                            'at', e.edited_at) order by e.id desc), '[]'::json)
                        from case_edits e where e.guild_id = c.guild_id and e.case_no = c.case_no))
         from cases c where c.guild_id = $1 and c.case_no = $2",
    )
    .bind(guild)
    .bind(no)
    .bind(cases::PROOFS_MAX)
    .fetch_optional(&st.db)
    .await?;
    Ok(Json(case.ok_or(ApiError::NotFound)?))
}

#[derive(Deserialize)]
struct CaseEdit {
    reason: String,
    active: bool,
}

/// The member a case is about, after the checks every case change shares: the Cases permit, the
/// bot's subject rule and the panic lock. Returns the editor's level too.
async fn case_guard(
    st: &AppState,
    s: &Sess,
    guild: i64,
    no: i32,
) -> Result<crate::access::Level, ApiError> {
    let level = api::staff(st, s, guild, &["cases"]).await?;
    let member: Option<i64> =
        sqlx::query_scalar("select user_id from cases where guild_id = $1 and case_no = $2")
            .bind(guild)
            .bind(no)
            .fetch_optional(&st.db)
            .await?;
    subject_guard(st, s, level, guild, member).await?;
    unlocked(st, s, guild).await?;
    Ok(level)
}

/// The bot posts the mod log entry of a case change (the dashboard has no Discord connection of
/// its own). The change is already saved, so a failed enqueue is only logged.
async fn case_log(st: &AppState, s: &Sess, via: &Option<Extension<Via>>, guild: i64, log: CaseLog) {
    let log = CaseLog {
        by: s.user_id as u64,
        ai: Via::of(via).map(String::from),
        ..log
    };
    let at = chrono::Utc::now().timestamp();
    if let Err(e) = jobs::enqueue(&st.db, guild, "case_log", json!(log), at).await {
        tracing::warn!(?e, guild, "queueing the case log failed");
    }
}

/// Close/reopen is the `active` flag, which is also what counts warn points. Every field that
/// really changes gets a history row and one mod log entry.
async fn case_edit(
    State(st): State<AppState>,
    s: Sess,
    via: Option<Extension<Via>>,
    Path((guild, no)): Path<(i64, i32)>,
    Json(b): Json<CaseEdit>,
) -> Res {
    let level = case_guard(&st, &s, guild, no).await?;
    // A warn case's `active` flag is its warn points, so changing it needs the `warn` permit perm,
    // as `unwarn` does in the bot.
    let may_warn = level >= crate::access::Level::Admin
        || api::permit_perms(&st, &s, guild)
            .await?
            .iter()
            .any(|p| p == "warn");
    let reason: String = b.reason.trim().chars().take(CASE_TEXT_MAX).collect();
    let reason = (!reason.is_empty()).then_some(reason);
    let mut tx = st.db.begin().await?;
    // The row is locked, so the state checked here is the state the edit changes.
    let warn_open: Option<bool> = sqlx::query_scalar(
        "select active from cases where guild_id = $1 and case_no = $2 and kind = 'warn' for update",
    )
    .bind(guild)
    .bind(no)
    .fetch_optional(&mut *tx)
    .await?;
    if !may_warn && warn_open.is_some_and(|open| open != b.active) {
        return Err(ApiError::Forbidden(
            "closing or reopening a warn case needs the Warn permit",
        ));
    }
    let by = (s.user_id, Via::of(&via));
    let new = (Some(reason.as_deref()), Some(b.active));
    let was = cases::edit(&mut tx, guild, no, new.0, new.1, by)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (reason_changed, active_changed) = (was.reason != reason, was.active != b.active);
    if reason_changed || active_changed {
        staff_action(
            &mut tx,
            guild,
            &s,
            "case_edit",
            was.user_id,
            no,
            Via::of(&via),
        )
        .await?;
    }
    tx.commit().await?;
    if reason_changed || active_changed {
        let log = CaseLog {
            case_no: no,
            reason: reason_changed.then_some((was.reason, reason)),
            active: active_changed.then_some(b.active),
            ..CaseLog::default()
        };
        case_log(&st, &s, &via, guild, log).await;
    }
    ok()
}

#[derive(Deserialize)]
struct ProofBody {
    text: String,
}

/// One more proof on a case: a web address alone is a link, anything else a short text.
async fn proof_add(
    State(st): State<AppState>,
    s: Sess,
    via: Option<Extension<Via>>,
    Path((guild, no)): Path<(i64, i32)>,
    Json(b): Json<ProofBody>,
) -> Res {
    case_guard(&st, &s, guild, no).await?;
    let text = clip(&b.text, CASE_TEXT_MAX)?;
    let by = (s.user_id, Via::of(&via));
    let what = (cases::proof_kind(&text), text.as_str());
    match cases::add_proof(&st.db, guild, no, what, by, None).await? {
        cases::Added::NoCase => return Err(ApiError::NotFound),
        cases::Added::Full => {
            return Err(ApiError::BadRequest(format!(
                "this case already has {} proofs, the most a case holds",
                cases::PROOFS_MAX
            )))
        }
        cases::Added::Done => {}
    }
    let log = CaseLog {
        case_no: no,
        proof_added: Some(text),
        ..CaseLog::default()
    };
    case_log(&st, &s, &via, guild, log).await;
    ok()
}

/// Removes one proof. A verified proof (the bot's own copy of the member's message) only by a
/// trusted admin or an owner, like deleting the case.
async fn proof_remove(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, no, id)): Path<(i64, i32, i64)>,
) -> Res {
    let level = case_guard(&st, &s, guild, no).await?;
    let mut tx = st.db.begin().await?;
    // No row lock: the dashboard's role has no UPDATE right on this table (FOR UPDATE needs it),
    // and `verified` never changes on a row.
    let verified: bool = sqlx::query_scalar(
        "select verified from case_proofs where guild_id = $1 and case_no = $2 and id = $3",
    )
    .bind(guild)
    .bind(no)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if verified && level < crate::access::Level::Admin {
        return Err(ApiError::Forbidden(
            "only an owner, extra owner or trusted admin can remove a verified proof",
        ));
    }
    let (kind, verified) = cases::remove_proof(&mut tx, guild, no, id, (s.user_id, None))
        .await?
        .ok_or(ApiError::NotFound)?;
    let member: i64 =
        sqlx::query_scalar("select user_id from cases where guild_id = $1 and case_no = $2")
            .bind(guild)
            .bind(no)
            .fetch_one(&mut *tx)
            .await?;
    staff_action(&mut tx, guild, &s, "case_edit", member, no, None).await?;
    tx.commit().await?;
    let log = CaseLog {
        case_no: no,
        proof_removed: Some(if verified {
            "verified message".into()
        } else {
            kind
        }),
        ..CaseLog::default()
    };
    case_log(&st, &s, &None, guild, log).await;
    ok()
}

/// The bot's rule for a case or note (`modcmds::run`): nobody changes one about themselves, and
/// only an owner, EO or TA changes one about an owner, EO or TA.
async fn subject_guard(
    st: &AppState,
    s: &Sess,
    level: crate::access::Level,
    guild: i64,
    member: Option<i64>,
) -> Result<(), ApiError> {
    let Some(m) = member.filter(|m| *m > 0) else {
        return Ok(());
    };
    if m == s.user_id {
        return Err(ApiError::Forbidden(
            "you cannot change a record about yourself",
        ));
    }
    if level >= crate::access::Level::Admin {
        return Ok(());
    }
    let listed: bool = sqlx::query_scalar(
        "select exists(select 1 from extra_owners where guild_id = $1 and user_id = $2)
             or exists(select 1 from trusted_admins where guild_id = $1 and user_id = $2)",
    )
    .bind(guild)
    .bind(m)
    .fetch_one(&st.db)
    .await?;
    if listed || crate::api::bot_owner(st, guild, false).await? == Some(m) {
        return Err(ApiError::Forbidden(
            "the member is immune: only an owner, extra owner or trusted admin can change this",
        ));
    }
    Ok(())
}

/// A dashboard change to a case counts as an action of its editor, so cases cannot be quietly rewritten.
async fn staff_action(
    tx: &mut sqlx::PgConnection,
    guild: i64,
    s: &Sess,
    kind: &str,
    user: i64,
    no: i32,
    ai_client: Option<&str>,
) -> Result<(), ApiError> {
    brim_core::cases::record(
        tx,
        brim_core::cases::ModAction {
            guild_id: guild,
            mod_id: s.user_id,
            kind,
            source: "dashboard",
            target_id: Some(user),
            case_no: Some(no),
            audit_id: None,
            ai_client,
        },
    )
    .await?;
    Ok(())
}

/// Deleting erases the audit trail, so it stays with the trusted admins and owners.
async fn case_delete(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, no)): Path<(i64, i32)>,
) -> Res {
    api::require(&st, &s, guild, crate::access::Level::Admin, false).await?;
    let mut tx = st.db.begin().await?;
    let (user, kind, moderator, reason): (i64, String, i64, Option<String>) = sqlx::query_as(
        "delete from cases where guild_id = $1 and case_no = $2 returning user_id, kind, mod_id, reason",
    )
    .bind(guild)
    .bind(no)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    staff_action(&mut tx, guild, &s, "case_delete", user, no, None).await?;
    tx.commit().await?;
    // The bot posts the mod log entry. The case is gone already, so a failed enqueue is only logged.
    let log = brim_core::cases::CaseDelete {
        case_no: no,
        kind,
        user: user as u64,
        moderator: moderator as u64,
        by: s.user_id as u64,
        reason,
    };
    let at = chrono::Utc::now().timestamp();
    if let Err(e) = jobs::enqueue(&st.db, guild, "case_delete_log", json!(log), at).await {
        tracing::warn!(?e, guild, "queueing the case delete log failed");
    }
    ok()
}

/// Owners, and roles an owner gave View Mod Stats; checked before anything in the request is read.
async fn stats_access(st: &AppState, s: &Sess, guild: i64) -> Result<(), ApiError> {
    let l = api::require(st, s, guild, crate::access::Level::Viewer, false).await?;
    let perms = if l == crate::access::Level::Owner {
        vec![]
    } else {
        api::permit_perms(st, s, guild).await?
    };
    if crate::access::mod_stats(l, &perms) {
        Ok(())
    } else {
        Err(ApiError::Forbidden("needs the View Mod Stats permit"))
    }
}

#[derive(Deserialize)]
struct StatsQuery {
    days: Option<String>,
}

/// Days in the period and days per bar: daily bars up to a month, weekly after.
fn period(raw: Option<&str>) -> Result<(i32, i32), ApiError> {
    match raw.unwrap_or("30") {
        "7" => Ok((7, 1)),
        "30" => Ok((30, 1)),
        "90" => Ok((90, 7)),
        "365" => Ok((365, 7)),
        _ => Err(ApiError::BadRequest("days must be 7, 30, 90 or 365".into())),
    }
}

/// Actions per staff member in the period, against the period before, with bars over time.
async fn mod_stats(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(q): Query<StatsQuery>,
) -> Res {
    stats_access(&st, &s, guild).await?;
    let (days, step) = period(q.days.as_deref())?;
    // The bot's own automatic actions are not staff work.
    let me = st.discord.bot_user().await?;
    // One transaction, so all three queries see the same now.
    let mut tx = st.db.begin().await?;
    let counts: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "select mod_id::text, kind,
                count(*) filter (where created_at >= now() - make_interval(days => $3)),
                count(*) filter (where created_at < now() - make_interval(days => $3))
         from mod_actions
         where guild_id = $1 and mod_id <> $2 and created_at >= now() - make_interval(days => $3 * 2)
         group by 1, 2",
    )
    .bind(guild)
    .bind(me)
    .bind(days)
    .fetch_all(&mut *tx)
    .await?;
    let last: Vec<(String, At)> = sqlx::query_as(
        "select mod_id::text, max(created_at) from mod_actions
         where guild_id = $1 and mod_id <> $2 group by 1",
    )
    .bind(guild)
    .bind(me)
    .fetch_all(&mut *tx)
    .await?;
    let bars: Vec<(String, i32, i64)> = sqlx::query_as(
        "select mod_id::text,
                floor(extract(epoch from now() - created_at) / ($4 * 86400))::int, count(*)
         from mod_actions
         where guild_id = $1 and mod_id <> $2 and created_at >= now() - make_interval(days => $3)
         group by 1, 2",
    )
    .bind(guild)
    .bind(me)
    .bind(days)
    .bind(step)
    .fetch_all(&mut *tx)
    .await?;
    Ok(Json(stats_json(days, step, counts, last, bars)))
}

#[derive(Default, serde::Serialize)]
struct ModRow {
    mod_id: String,
    by_kind: std::collections::BTreeMap<String, i64>,
    total: i64,
    prev_total: i64,
    last_at: Option<At>,
    series: Vec<i64>,
}

/// Builds the page's rows: oldest bar first, busiest member first, at most 200.
fn stats_json(
    days: i32,
    step: i32,
    counts: Vec<(String, String, i64, i64)>,
    last: Vec<(String, At)>,
    bars: Vec<(String, i32, i64)>,
) -> Value {
    use std::collections::{BTreeMap, BTreeSet};
    // Whole bars only: the days left over join the oldest bar instead of making a short one.
    let n = (days / step) as usize;
    let mut kinds = BTreeSet::new();
    let mut rows: BTreeMap<String, ModRow> = BTreeMap::new();
    fn row(rows: &mut BTreeMap<String, ModRow>, id: String, n: usize) -> &mut ModRow {
        rows.entry(id.clone()).or_insert_with(|| ModRow {
            mod_id: id,
            series: vec![0; n],
            ..Default::default()
        })
    }
    for (id, at) in last {
        row(&mut rows, id, n).last_at = Some(at);
    }
    for (id, kind, now, prev) in counts {
        let r = row(&mut rows, id, n);
        if now > 0 {
            r.by_kind.insert(kind.clone(), now);
            kinds.insert(kind);
        }
        r.total += now;
        r.prev_total += prev;
    }
    for (id, ago, c) in bars {
        // `ago` counts bars back from now; anything past the last whole bar joins the oldest.
        let i = n - 1 - (ago.max(0) as usize).min(n - 1);
        row(&mut rows, id, n).series[i] += c;
    }
    let mut out: Vec<ModRow> = rows.into_values().collect();
    out.sort_by(|a, b| b.total.cmp(&a.total).then(b.last_at.cmp(&a.last_at)));
    out.truncate(200);
    json!({"days": days, "step": step, "kinds": kinds, "mods": out})
}

/// One staff member's last 50 actions.
async fn mod_actions(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, user)): Path<(i64, String)>,
) -> Res {
    stats_access(&st, &s, guild).await?;
    let user = user_id(&user)?;
    let rows: Option<Value> = sqlx::query_scalar(
        "select jsonb_agg(r order by r.at desc, r.id desc) from (
            select id, kind, source, target_id::text as target_id, case_no, created_at as at from mod_actions
            where guild_id = $1 and mod_id = $2 order by created_at desc, id desc limit 50) r",
    )
    .bind(guild)
    .bind(user)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(rows.unwrap_or_else(|| json!([]))))
}

#[derive(Deserialize)]
struct NoteFilter {
    q: Option<String>,
    #[serde(default)]
    page: i64,
}

/// Short id, member, author, text, created, edits, AI client.
type NoteRow = (String, String, String, String, At, Value, Option<String>);

async fn notes(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(f): Query<NoteFilter>,
) -> Res {
    api::staff(&st, &s, guild, &["notes_view", "notes_manage"]).await?;
    let rows: Vec<NoteRow> = sqlx::query_as(
        "select short_id, user_id::text, author_id::text, text, created_at,
                (select coalesce(json_agg(json_build_object('at', e.edited_at, 'by', e.editor_id::text,
                        'old', e.old_text, 'new', e.new_text, 'ai_client', e.ai_client) order by e.id desc), '[]'::json)
                 from note_edits e where e.note_id = notes.id), ai_client
         from notes
         where guild_id = $1 and ($2 = '' or user_id::text = $2) order by id desc limit 50 offset $3",
    )
    .bind(guild)
    .bind(f.q.unwrap_or_default().trim())
    .bind(offset(f.page))
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(id, user, author, text, at, edits, ai)| json!({"id": id, "user_id": user, "author_id": author, "ai_client": ai, "text": text, "at": at, "edits": edits}))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct NoteBody {
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    text: String,
    /// Sweep: every note of the server instead of one member's.
    #[serde(default)]
    all: bool,
}

/// The bot sends the private mod-log entry (the dashboard has no Discord connection of its own).
/// The note change is already saved, so a failed enqueue is only logged: a retry would repeat it.
async fn note_log(st: &AppState, guild: i64, log: NoteLog) {
    let at = chrono::Utc::now().timestamp();
    if let Err(e) = jobs::enqueue(&st.db, guild, "note_log", json!(log), at).await {
        tracing::warn!(?e, guild, "queueing the note log failed");
    }
}

/// The bot runs every note command through `commands::gated`, so its panic lock holds here too.
async fn unlocked(st: &AppState, s: &Sess, guild: i64) -> Result<(), ApiError> {
    crate::appeals::mod_lock_guard(st, s, guild, &mut *st.db.acquire().await?).await
}

async fn note_add(
    State(st): State<AppState>,
    s: Sess,
    via: Option<Extension<Via>>,
    Path(guild): Path<i64>,
    Json(b): Json<NoteBody>,
) -> Res {
    let level = api::staff(&st, &s, guild, &["notes_manage"]).await?;
    let (user, text) = (user_id(&b.user_id)?, clip(&b.text, NOTE_MAX)?);
    subject_guard(&st, &s, level, guild, Some(user)).await?;
    unlocked(&st, &s, guild).await?;
    let (id,): (String,) = sqlx::query_as(
        "insert into notes (guild_id, user_id, author_id, text, ai_client) values ($1,$2,$3,$4,$5) returning short_id",
    )
    .bind(guild)
    .bind(user)
    .bind(s.user_id)
    .bind(&text)
    .bind(Via::of(&via))
    .fetch_one(&st.db)
    .await?;
    let log = NoteLog {
        action: NoteAction::Add,
        id: Some(id),
        user: Some(user as u64),
        by: s.user_id as u64,
        text: Some(text),
        ai: Via::of(&via).map(String::from),
        ..NoteLog::default()
    };
    note_log(&st, guild, log).await;
    ok()
}

async fn note_edit(
    State(st): State<AppState>,
    s: Sess,
    via: Option<Extension<Via>>,
    Path((guild, id)): Path<(i64, String)>,
    Json(b): Json<NoteBody>,
) -> Res {
    let level = api::staff(&st, &s, guild, &["notes_manage"]).await?;
    let member: Option<i64> =
        sqlx::query_scalar("select user_id from notes where guild_id = $1 and short_id = $2")
            .bind(guild)
            .bind(&id)
            .fetch_optional(&st.db)
            .await?;
    subject_guard(&st, &s, level, guild, member).await?;
    let text = clip(&b.text, NOTE_MAX)?;
    unlocked(&st, &s, guild).await?;
    let (user, old) = notes::edit(&st.db, guild, &id, &text, s.user_id, Via::of(&via))
        .await?
        .ok_or(ApiError::NotFound)?;
    if old == text {
        return ok();
    }
    let log = NoteLog {
        action: NoteAction::Modify,
        id: Some(id),
        user: Some(user as u64),
        by: s.user_id as u64,
        text: Some(text),
        old: Some(old),
        ai: Via::of(&via).map(String::from),
        ..NoteLog::default()
    };
    note_log(&st, guild, log).await;
    ok()
}

async fn note_delete(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, String)>,
) -> Res {
    let level = api::staff(&st, &s, guild, &["notes_manage"]).await?;
    let member: Option<i64> =
        sqlx::query_scalar("select user_id from notes where guild_id = $1 and short_id = $2")
            .bind(guild)
            .bind(&id)
            .fetch_optional(&st.db)
            .await?;
    subject_guard(&st, &s, level, guild, member).await?;
    unlocked(&st, &s, guild).await?;
    let gone: Option<(i64, String)> = sqlx::query_as(
        "delete from notes where guild_id = $1 and short_id = $2 returning user_id, text",
    )
    .bind(guild)
    .bind(&id)
    .fetch_optional(&st.db)
    .await?;
    if let Some((user, text)) = gone {
        let log = NoteLog {
            action: NoteAction::Remove,
            id: Some(id),
            user: Some(user as u64),
            by: s.user_id as u64,
            text: Some(text),
            ..NoteLog::default()
        };
        note_log(&st, guild, log).await;
    }
    ok()
}

async fn note_sweep(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<NoteBody>,
) -> Res {
    let level = api::staff(&st, &s, guild, &["notes_manage"]).await?;
    // The page asks before a sweep of the whole server; `all` must be explicit, an empty id is an error.
    let user = if b.all {
        None
    } else {
        Some(user_id(&b.user_id)?)
    };
    subject_guard(&st, &s, level, guild, user).await?;
    unlocked(&st, &s, guild).await?;
    let n = sqlx::query(
        "delete from notes where guild_id = $1 and ($2::bigint is null or user_id = $2)",
    )
    .bind(guild)
    .bind(user)
    .execute(&st.db)
    .await?
    .rows_affected();
    let log = NoteLog {
        action: NoteAction::Sweep,
        user: user.map(|u| u as u64),
        by: s.user_id as u64,
        count: Some(n),
        ..NoteLog::default()
    };
    note_log(&st, guild, log).await;
    Ok(Json(json!({"removed": n})))
}

#[derive(Deserialize)]
struct ReportFilter {
    /// "open", "dismissed", "actioned" or anything else for all
    state: Option<String>,
    #[serde(default)]
    page: i64,
}

async fn reports(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Query(f): Query<ReportFilter>,
) -> Res {
    api::staff(&st, &s, guild, &["reports"]).await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, String, String, String, String, String, Option<String>, Option<String>, At)> = sqlx::query_as(
        "select id, reporter_id::text, target_id::text, channel_id::text, message_id::text, content, status,
                action, handled_by::text, created_at
         from reports where guild_id = $1 and ($2 = '' or status = $2)
         order by (status = 'open') desc, id desc limit 50 offset $3",
    )
    .bind(guild)
    .bind(f.state.unwrap_or_default())
    .bind(offset(f.page))
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(
            |(id, reporter, target, channel, message, content, status, action, by, at)| json!({
                "id": id, "reporter_id": reporter, "target_id": target, "channel_id": channel,
                "message_id": message, "content": content, "status": status, "action": action,
                "handled_by": by, "at": at
            })
        )
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct Status {
    open: bool,
}

/// Dismiss or reopen. Bans, kicks and the like happen with the buttons in Discord, and a report
/// they actioned keeps its action and handler: only open and dismissed reports change here.
async fn report_status(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Status>,
) -> Res {
    api::staff(&st, &s, guild, &["reports"]).await?;
    let mut tx = st.db.begin().await?;
    crate::appeals::mod_lock_guard(&st, &s, guild, &mut tx).await?;
    let (status, kind) = if b.open {
        ("open", "report_reopen")
    } else {
        ("dismissed", "report_dismiss")
    };
    let row: Option<(String, i64)> = sqlx::query_as(
        "select status, target_id from reports where guild_id = $1 and id = $2 for update",
    )
    .bind(guild)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (old, target) = row.ok_or(ApiError::NotFound)?;
    if old == "actioned" {
        return Err(ApiError::BadRequest(
            "this report was actioned in Discord; its action and handler stay as recorded".into(),
        ));
    }
    if old == status {
        return ok();
    }
    sqlx::query(
        "update reports set status = $3, handled_by = case when $3 = 'open' then null else $4 end,
                handled_at = case when $3 = 'open' then null else now() end
         where guild_id = $1 and id = $2",
    )
    .bind(guild)
    .bind(id)
    .bind(status)
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    // A reopen clears the handler, so who dismissed and reopened it is kept here.
    brim_core::cases::record(
        &mut tx,
        brim_core::cases::ModAction {
            guild_id: guild,
            mod_id: s.user_id,
            kind,
            source: "dashboard",
            target_id: Some(target),
            case_no: None,
            audit_id: None,
            ai_client: None,
        },
    )
    .await?;
    tx.commit().await?;
    ok()
}

async fn blacklist(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::staff(&st, &s, guild, &["reports"]).await?;
    let rows: Vec<(String, At)> = sqlx::query_as(
        "select user_id::text, added_at from report_blacklist where guild_id = $1 order by added_at desc",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(u, at)| json!({"user_id": u, "at": at}))
        .collect::<Vec<_>>())))
}

async fn blacklist_add(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<NoteBody>,
) -> Res {
    api::staff(&st, &s, guild, &["reports"]).await?;
    sqlx::query("insert into report_blacklist (guild_id, user_id, added_by) values ($1,$2,$3) on conflict do nothing")
        .bind(guild)
        .bind(user_id(&b.user_id)?)
        .bind(s.user_id)
        .execute(&st.db)
        .await?;
    ok()
}

async fn blacklist_remove(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, user)): Path<(i64, i64)>,
) -> Res {
    api::staff(&st, &s, guild, &["reports"]).await?;
    sqlx::query("delete from report_blacklist where guild_id = $1 and user_id = $2")
        .bind(guild)
        .bind(user)
        .execute(&st.db)
        .await?;
    ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn report_status_keeps_an_action_and_panic_locks_non_admins() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_892_000_000).await else {
            return;
        };
        let run = |q: &'static str| {
            let db = st.db.clone();
            async move { sqlx::query(q).bind(g).execute(&db).await.unwrap() }
        };
        let status = |id: i64, open: bool| {
            report_status(
                State(st.clone()),
                s.clone(),
                Path((g, id)),
                Json(Status { open }),
            )
        };
        let add = |state: &'static str| {
            sqlx::query_scalar::<_, i64>(
                "insert into reports (guild_id, reporter_id, target_id, channel_id, message_id, content, status, action, handled_by)
                 values ($1, 1, 5, 2, length($2), 'x', $2, case when $2 = 'actioned' then 'ban' end, 6) returning id",
            )
            .bind(g)
            .bind(state)
            .fetch_one(&st.db)
        };
        for q in [
            "delete from reports where guild_id = $1",
            "delete from mod_actions where guild_id = $1",
        ] {
            run(q).await;
        }
        let banned = add("actioned").await.unwrap();
        assert!(matches!(
            status(banned, true).await,
            Err(ApiError::BadRequest(_))
        ));
        let open = add("open").await.unwrap();
        let _ = status(open, false).await.unwrap();
        let kinds: Vec<String> = sqlx::query_scalar(
            "select kind from mod_actions where guild_id = $1 and target_id = 5 order by id",
        )
        .bind(g)
        .fetch_all(&st.db)
        .await
        .unwrap();
        assert_eq!(kinds, ["report_dismiss"]);
        // Panic mode's mod lock: the owner (7) passes, a member without a level does not.
        run("insert into scheduled_jobs (guild_id, kind, payload, run_at) values ($1, 'panic_end', '{\"lock_mod_commands\": true}', now())").await;
        st.discord.memo.lock().unwrap().insert(
            format!("/guilds/{g}/members/8"),
            (std::time::Instant::now(), Some(json!({"roles": []}))),
        );
        let mut c = st.db.acquire().await.unwrap();
        crate::appeals::mod_lock_guard(&st, &s, g, &mut c)
            .await
            .unwrap();
        let other = Sess {
            user_id: 8,
            ..s.clone()
        };
        let r = crate::appeals::mod_lock_guard(&st, &other, g, &mut c).await;
        assert!(matches!(r, Err(ApiError::Forbidden(_))));
        for t in ["scheduled_jobs", "reports", "mod_actions"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {t} where guild_id = $1"
            )))
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        }
    }

    #[test]
    fn mod_stats_rows() {
        let at = |d: i64| chrono::Utc::now() - chrono::Duration::days(d);
        let v = stats_json(
            7,
            1,
            vec![
                ("1".into(), "ban".into(), 3, 1),
                ("1".into(), "warn".into(), 0, 2),
                ("2".into(), "kick".into(), 5, 0),
            ],
            vec![
                ("1".into(), at(0)),
                ("2".into(), at(1)),
                ("3".into(), at(200)),
            ],
            vec![("1".into(), 0, 2), ("1".into(), 6, 1), ("2".into(), 9, 5)],
        );
        assert_eq!(
            stats_json(
                365,
                7,
                vec![],
                vec![("1".into(), chrono::Utc::now())],
                vec![("1".into(), 52, 4)]
            )["mods"][0]["series"]
                .as_array()
                .map(|s| (s.len(), s[0].clone())),
            Some((52, json!(4))),
            "a year is 52 whole weeks; the extra day joins the oldest"
        );
        let m = v["mods"].as_array().unwrap();
        assert_eq!(
            v["kinds"],
            json!(["ban", "kick"]),
            "a kind with nothing in the period gets no column"
        );
        assert_eq!(m[0]["mod_id"], "2");
        assert_eq!(
            m[0]["series"],
            json!([5, 0, 0, 0, 0, 0, 0]),
            "a late bar joins the oldest"
        );
        assert_eq!(m[1]["mod_id"], "1");
        assert_eq!(m[1]["by_kind"], json!({"ban": 3}));
        assert_eq!(
            (m[1]["total"].as_i64(), m[1]["prev_total"].as_i64()),
            (Some(3), Some(3))
        );
        assert_eq!(m[1]["series"], json!([1, 0, 0, 0, 0, 0, 2]));
        assert_eq!(
            m[2]["mod_id"], "3",
            "quiet staff stay listed, with nothing in the period"
        );
        assert_eq!(m[2]["total"], 0);
    }

    #[test]
    fn mod_stats_periods() {
        assert_eq!(period(None).ok(), Some((30, 1)));
        assert_eq!(period(Some("365")).ok(), Some((365, 7)));
        assert!(period(Some("31")).is_err());
        assert!(period(Some("-7")).is_err());
    }

    #[test]
    fn input_checks() {
        assert!(user_id(" 123 ").is_ok());
        assert!(user_id("abc").is_err());
        assert!(clip("  ", 5).is_err());
        assert!(clip("123456", 5).is_err());
        assert_eq!(clip(" hi ", 5).unwrap(), "hi");
    }

    /// Closing or reopening a warn case changes warn points: `cases` alone may edit its text only.
    /// Runs only when DATABASE_URL points at a scratch Postgres.
    #[tokio::test]
    async fn a_warn_case_is_closed_only_with_the_warn_permit() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_893_100_000).await else {
            return;
        };
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
        for sql in [
            "insert into permit_members_migrated (guild_id) values ($1)",
            "insert into permits (guild_id, name, perms) values ($1, 'Clerks', '{dashboard_view,cases}'), ($1, 'Mods', '{dashboard_view,cases,warn}')",
            "insert into permit_members (permit_id, guild_id, user_id) select id, $1, case name when 'Clerks' then 31 else 32 end from permits where guild_id = $1",
            "insert into cases (guild_id, case_no, kind, user_id, mod_id) values ($1, 1, 'warn', 5, 6), ($1, 2, 'kick', 5, 6)",
        ] {
            sqlx::query(sql).bind(g).execute(&st.db).await.unwrap();
        }
        for u in [31, 32] {
            st.discord.memo.lock().unwrap().insert(
                format!("/guilds/{g}/members/{u}"),
                (std::time::Instant::now(), Some(json!({"roles": []}))),
            );
        }
        let edit = |by: i64, no: i32, active: bool| {
            let s = Sess {
                user_id: by,
                ..s.clone()
            };
            let b = CaseEdit {
                reason: "why".into(),
                active,
            };
            case_edit(State(st.clone()), s, None, Path((g, no)), Json(b))
        };
        let active = || {
            sqlx::query_scalar::<_, bool>(
                "select active from cases where guild_id = $1 and case_no = 1",
            )
            .bind(g)
            .fetch_one(&st.db)
        };
        // The Clerks permit edits the text of a warn case and closes other cases, no more.
        let r = edit(31, 1, true).await;
        assert!(r.is_ok(), "{r:?}");
        assert!(edit(31, 2, false).await.is_ok());
        let r = edit(31, 1, false).await;
        assert!(matches!(r, Err(ApiError::Forbidden(_))), "{r:?}");
        assert!(active().await.unwrap());
        assert!(edit(32, 1, false).await.is_ok());
        assert!(!active().await.unwrap());
        let r = edit(31, 1, true).await;
        assert!(matches!(r, Err(ApiError::Forbidden(_))), "reopen: {r:?}");
        assert!(matches!(edit(32, 3, true).await, Err(ApiError::NotFound)));
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
    }

    /// A deleted case is still said in the mod log: the bot reads what the case was from the job.
    #[tokio::test]
    async fn deleting_a_case_queues_its_mod_log() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_895_700_000).await else {
            return;
        };
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
        sqlx::query("insert into cases (guild_id, case_no, kind, user_id, mod_id, reason) values ($1, 1, 'warn', 5, 6, 'spam')")
            .bind(g)
            .execute(&st.db)
            .await
            .unwrap();
        let del = || case_delete(State(st.clone()), s.clone(), Path((g, 1)));
        let _ = del().await.unwrap();
        assert!(matches!(del().await, Err(ApiError::NotFound)));
        let jobs: Vec<Value> = sqlx::query_scalar(
            "select payload from scheduled_jobs where guild_id = $1 and kind = 'case_delete_log'",
        )
        .bind(g)
        .fetch_all(&st.db)
        .await
        .unwrap();
        assert_eq!(
            jobs,
            [
                json!({"case_no": 1, "kind": "warn", "user": 5, "moderator": 6, "by": s.user_id, "reason": "spam"})
            ]
        );
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
    }

    /// The case page lists the edit history newest first (owner decision 2026-11-19: "always newest top").
    #[tokio::test]
    async fn case_history_is_newest_first() {
        let Some((st, s, g)) = crate::api::g3_handler_tests::setup(9_894_300_000).await else {
            return;
        };
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
        for sql in [
            "insert into cases (guild_id, case_no, kind, user_id, mod_id) values ($1, 1, 'kick', 5, 6)",
            "insert into case_edits (guild_id, case_no, field, old_value, new_value, editor_id) values ($1, 1, 'reason', 'a', 'b', 7), ($1, 1, 'reason', 'b', 'c', 7)",
        ] {
            sqlx::query(sql).bind(g).execute(&st.db).await.unwrap();
        }
        let Json(c) = case_page(State(st.clone()), s, Path((g, 1))).await.unwrap();
        assert_eq!(c["edits"][0]["new"], "c");
        assert_eq!(c["edits"][1]["new"], "b");
        brim_core::db::purge_guild(&st.db, g).await.unwrap();
    }
}

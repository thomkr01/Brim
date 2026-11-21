//! The bot owner's list of people's own data requests (`/mydata` in a DM, security review). An
//! approve or deny only records the decision and queues a `data_request` job: the bot deletes or
//! collects the data and DMs the person, so nothing here touches their rows.
use crate::{api::Res, auth::Sess, ApiError, AppState};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use brim_core::{jobs, persondata};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/data-requests", get(list))
        .route("/api/data-requests/{id}/{decision}", post(decide))
}

async fn bot_owner(st: &AppState, s: &Sess) -> Result<(), ApiError> {
    if crate::update::is_bot_owner(st, s).await {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "only the bot owner can answer data requests",
        ))
    }
}

type Row = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<Value>,
);

/// Open requests with what each would touch per table, then the 50 newest answered ones.
async fn list(State(st): State<AppState>, s: Sess) -> Res {
    bot_owner(&st, &s).await?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, user_id::text, username, kind, status, created_at::text, decided_at::text, result
         FROM data_requests ORDER BY status <> 'pending', created_at DESC LIMIT 100",
    )
    .fetch_all(&st.db)
    .await?;
    let mut out = vec![];
    // Counts for the 20 newest open requests only, so many requests cannot slow the page.
    let mut counted = 0;
    for (id, user, name, kind, status, at, decided, result) in rows {
        let found = if status == "pending" && counted < 20 {
            counted += 1;
            Some(found(&st, user.parse().unwrap_or(0), &kind).await?)
        } else {
            None
        };
        out.push(
            json!({"id": id, "user_id": user, "username": name, "kind": kind, "status": status,
            "created_at": at, "decided_at": decided, "result": result, "found": found}),
        );
    }
    Ok(Json(json!(out)))
}

/// {table.column: rows} the request would show or delete; a kept place is marked "(kept)".
async fn found(st: &AppState, user: i64, kind: &str) -> Result<Value, ApiError> {
    let mut m = Map::new();
    for p in persondata::PLACES {
        if (kind == "show" && !p.show) || (kind == "delete" && !p.delete && !p.show) {
            continue;
        }
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(persondata::count_sql(p)))
            .bind(user)
            .fetch_one(&st.db)
            .await?;
        if n > 0 {
            let kept = if kind == "delete" && !p.delete {
                " (kept)"
            } else {
                ""
            };
            m.insert(format!("{}.{}{kept}", p.table, p.col), json!(n));
        }
    }
    Ok(Value::Object(m))
}

/// `approve` or `deny`, bot owner with a fresh 2FA step-up. The job is queued with the decision.
async fn decide(
    State(st): State<AppState>,
    s: Sess,
    Path((id, decision)): Path<(i64, String)>,
) -> Res {
    bot_owner(&st, &s).await?;
    if !s.stepped_up {
        return Err(ApiError::StepUp);
    }
    let status = match decision.as_str() {
        "approve" => "approved",
        "deny" => "denied",
        _ => return Err(ApiError::NotFound),
    };
    let mut tx = st.db.begin().await?;
    let done = sqlx::query(
        "UPDATE data_requests SET status = $2, decided_by = $3, decided_at = now()
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(status)
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(ApiError::BadRequest(
            "this request was already answered".into(),
        ));
    }
    let p = json!({"id": id, "by": s.user_id.to_string()});
    jobs::enqueue(
        &mut *tx,
        0,
        "data_request",
        p,
        chrono::Utc::now().timestamp(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok": true})))
}

//! Categories of saved messages: names the server chooses, shown as card rows on the Messages
//! page. A message without a category shows under "Uncategorized" (no row). The order is A to Z
//! until someone drags a row; then it is the order they set, until "Sort A to Z".
use crate::{
    access::Level,
    api::{self, Res},
    auth::Sess,
    lockdown::audit,
    ApiError, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

/// Own categories per server; "Uncategorized" is not counted.
const MAX_CATEGORIES: i64 = 25;
const MAX_NAME: usize = 50;

pub fn routes() -> Router<AppState> {
    let g = "/api/guilds/{guild}/message-categories";
    Router::new()
        .route(g, get(list).post(create))
        .route(&format!("{g}/order"), post(order))
        .route(&format!("{g}/{{id}}/rename"), post(rename))
        .route(&format!("{g}/{{id}}/delete"), post(delete))
}

async fn list(State(st): State<AppState>, s: Sess, Path(guild): Path<i64>) -> Res {
    api::require(&st, &s, guild, Level::Viewer, false).await?;
    let rows: Vec<(i64, String, i32)> = sqlx::query_as(
        "select id, name, position from message_categories where guild_id = $1
         order by position, lower(name), id",
    )
    .bind(guild)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(
            |(id, name, position)| json!({"id": id.to_string(), "name": name, "position": position})
        )
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct Name {
    name: String,
}

/// 1 to [`MAX_NAME`] characters, not "Uncategorized" (that one is Brim's).
fn clean(name: &str) -> Result<String, ApiError> {
    let n = name.trim();
    let len = n.chars().count();
    if len == 0 || len > MAX_NAME {
        return Err(ApiError::BadRequest(format!(
            "a category name is 1 to {MAX_NAME} characters"
        )));
    }
    if n.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "a category name is one line of text".into(),
        ));
    }
    if n.eq_ignore_ascii_case("uncategorized") {
        return Err(ApiError::BadRequest(
            "“Uncategorized” is Brim's own row: choose another name".into(),
        ));
    }
    Ok(n.to_string())
}

fn taken(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            ApiError::BadRequest("a category with this name exists already".into())
        }
        _ => e.into(),
    }
}

async fn create(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Name>,
) -> Res {
    api::require(&st, &s, guild, Level::Editor, false).await?;
    let name = clean(&b.name)?;
    // The server's lock: two creates at once cannot both pass the limit.
    let mut tx = st.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('message_categories'), hashtext($1::text))")
        .bind(guild)
        .execute(&mut *tx)
        .await?;
    // After a manual order a new category goes last; in A to Z order (all 0) it stays 0.
    let id: Option<i64> = sqlx::query_scalar(
        "insert into message_categories (guild_id, name, updated_by, position)
         select $1, $2, $3, (select case when max(position) > 0 then max(position) + 1 else 0 end
                             from message_categories where guild_id = $1)
         where (select count(*) from message_categories where guild_id = $1) < $4
         returning id",
    )
    .bind(guild)
    .bind(&name)
    .bind(s.user_id)
    .bind(MAX_CATEGORIES)
    .fetch_optional(&mut *tx)
    .await
    .map_err(taken)?;
    tx.commit().await?;
    let id = id.ok_or_else(|| {
        ApiError::BadRequest(format!(
            "this server already has {MAX_CATEGORIES} categories: delete one first"
        ))
    })?;
    audit(
        &st,
        &s,
        guild,
        format!("made the message category “{name}”"),
    )
    .await?;
    Ok(Json(json!({"id": id.to_string(), "name": name})))
}

async fn rename(
    State(st): State<AppState>,
    s: Sess,
    Path((guild, id)): Path<(i64, i64)>,
    Json(b): Json<Name>,
) -> Res {
    api::require(&st, &s, guild, Level::Editor, false).await?;
    let name = clean(&b.name)?;
    let old: String = sqlx::query_scalar(
        "update message_categories c set name = $3, updated_by = $4, updated_at = now()
         from message_categories o where c.id = $1 and c.guild_id = $2 and o.id = c.id
         returning o.name",
    )
    .bind(id)
    .bind(guild)
    .bind(&name)
    .bind(s.user_id)
    .fetch_optional(&st.db)
    .await
    .map_err(taken)?
    .ok_or(ApiError::NotFound)?;
    audit(
        &st,
        &s,
        guild,
        format!("renamed the message category “{old}” to “{name}”"),
    )
    .await?;
    Ok(Json(json!({"id": id.to_string(), "name": name})))
}

/// Its messages move to "Uncategorized" (`ON DELETE SET NULL`).
async fn delete(State(st): State<AppState>, s: Sess, Path((guild, id)): Path<(i64, i64)>) -> Res {
    api::require(&st, &s, guild, Level::Editor, false).await?;
    let name: String = sqlx::query_scalar(
        "delete from message_categories where id = $1 and guild_id = $2 returning name",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&st.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    audit(
        &st,
        &s,
        guild,
        format!("deleted the message category “{name}”"),
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Order {
    /// Category ids top down (strings: ids pass JavaScript numbers).
    #[serde(default)]
    categories: Vec<String>,
    /// Messages top down, each with its category ("" for Uncategorized).
    #[serde(default)]
    messages: Vec<(String, String)>,
    /// Back to A to Z: every position 0. The lists are then ignored.
    #[serde(default)]
    reset: bool,
}

/// Saves the order the page shows, and the category of each message (a drag into another
/// category). Ids of another server are ignored.
async fn order(
    State(st): State<AppState>,
    s: Sess,
    Path(guild): Path<i64>,
    Json(b): Json<Order>,
) -> Res {
    api::require(&st, &s, guild, Level::Editor, false).await?;
    let id = |v: &str| {
        v.parse::<i64>()
            .map_err(|_| ApiError::BadRequest("unknown id".into()))
    };
    // More than a server can have is refused before any work.
    if b.categories.len() > MAX_CATEGORIES as usize
        || b.messages.len() > crate::embeds::MAX_MESSAGES as usize
    {
        return Err(ApiError::BadRequest("too many ids".into()));
    }
    let mut tx = st.db.begin().await?;
    if b.reset {
        sqlx::query("update message_categories set position = 0 where guild_id = $1")
            .bind(guild)
            .execute(&mut *tx)
            .await?;
        sqlx::query("update embeds set position = 0 where guild_id = $1")
            .bind(guild)
            .execute(&mut *tx)
            .await?;
    } else {
        let cats = b
            .categories
            .iter()
            .map(|c| id(c))
            .collect::<Result<Vec<_>, _>>()?;
        sqlx::query(
            "update message_categories c set position = o.n::int
             from unnest($2::bigint[]) with ordinality o(id, n)
             where c.id = o.id and c.guild_id = $1",
        )
        .bind(guild)
        .bind(&cats)
        .execute(&mut *tx)
        .await?;
        let msgs = b
            .messages
            .iter()
            .map(|m| id(&m.0))
            .collect::<Result<Vec<_>, _>>()?;
        let homes = b
            .messages
            .iter()
            .map(|m| match m.1.as_str() {
                "" => Ok(None),
                c => id(c).map(Some),
            })
            .collect::<Result<Vec<_>, _>>()?;
        // A category of another server (or one deleted meanwhile) leaves the message where it was.
        sqlx::query(
            "update embeds e set position = o.n::int,
                 category_id = case when o.cat is null then null
                     else coalesce((select c.id from message_categories c
                                    where c.id = o.cat and c.guild_id = $1), e.category_id) end
             from unnest($2::bigint[], $3::bigint[]) with ordinality o(id, cat, n)
             where e.id = o.id and e.guild_id = $1",
        )
        .bind(guild)
        .bind(&msgs)
        .bind(&homes)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let what = if b.reset {
        "sorted the message categories A to Z"
    } else {
        "changed the order of the custom messages"
    };
    audit(&st, &s, guild, what.to_string()).await?;
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn names_are_checked() {
        assert_eq!(clean("  Rules ").unwrap(), "Rules");
        assert!(clean("").is_err());
        assert!(clean(&"x".repeat(51)).is_err());
        assert!(clean(&"é".repeat(50)).is_ok());
        assert!(clean("uncategorized").is_err());
        assert!(clean("a\nb").is_err());
    }
}

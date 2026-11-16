//! Public event routes and player sign-up.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use proto::{EventSummary, EventView, Ruleset, SignupReq};
use serde_json::json;
use sqlx::SqlitePool;

use super::{check_text, parse_body, ApiError, MAX_BUILD_CHARS};
use crate::auth::Player;
use crate::db::begin_write;
use crate::lifecycle::{load_bracket, LifecycleError, MAX_ENTRANTS};
use crate::logging::label;
use crate::state::AppState;

/// `GET /v1/events`: every event, oldest first.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<EventSummary>>, ApiError> {
    let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT e.id, e.name, e.state,
                (SELECT COUNT(*) FROM entrants n WHERE n.event_id = e.id)
         FROM events e ORDER BY e.id",
    )
    .fetch_all(&state.db)
    .await?;
    rows.into_iter()
        .map(|(id, name, state, entrants)| summary(id, name, state, entrants))
        .collect::<Result<_, _>>()
        .map(Json)
}

/// `GET /v1/events/{id}`.
pub async fn view(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<EventView>, ApiError> {
    event_view(&state.db, id).await.map(Json)
}

/// `POST /v1/events/{id}/entrants`: enters the signing player, seeded in sign-up order.
pub async fn signup(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    player: Player,
) -> Result<StatusCode, ApiError> {
    let body: SignupReq = parse_body(&player.body)?;
    check_text("engine_build", &body.engine_build, MAX_BUILD_CHARS)?;

    let mut tx = begin_write(&state.db).await?;
    let event_state: Option<String> = sqlx::query_scalar("SELECT state FROM events WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    match event_state.as_deref() {
        None => return Err(LifecycleError::NoSuchEvent.into()),
        Some("signup") => {}
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "event is not open for sign-up",
            ))
        }
    }

    let accepted = &state.cfg.accepted_builds;
    if !accepted.is_empty() && !accepted.contains(&body.engine_build) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            json!({ "error": "engine build not accepted", "accepted": accepted }),
        ));
    }

    let entered: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM entrants WHERE event_id = ? AND player_key = ?")
            .bind(id)
            .bind(&player.key)
            .fetch_optional(&mut *tx)
            .await?;
    if entered.is_some() {
        return Err(ApiError::new(StatusCode::CONFLICT, "already entered"));
    }

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entrants WHERE event_id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_ENTRANTS {
        return Err(ApiError::new(StatusCode::CONFLICT, "event is full"));
    }

    sqlx::query(
        "INSERT INTO entrants (event_id, player_key, seed, engine_build) VALUES (?, ?, ?, ?)",
    )
    .bind(id)
    .bind(&player.key)
    .bind(count)
    .bind(&body.engine_build)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(
        event = id,
        tag = %label(&player.tag),
        seed = count,
        engine_build = %label(&body.engine_build),
        "signed up"
    );
    Ok(StatusCode::CREATED)
}

/// The full view of one event; 404 if it does not exist.
pub(super) async fn event_view(db: &SqlitePool, id: i64) -> Result<EventView, ApiError> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT name, state, ruleset_json FROM events WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?;
    let (name, state, ruleset_json) = row.ok_or(LifecycleError::NoSuchEvent)?;
    let ruleset: Ruleset = serde_json::from_str(&ruleset_json)
        .map_err(|e| ApiError::internal(format_args!("event {id} ruleset_json: {e}")))?;

    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT p.tag FROM entrants n JOIN players p ON p.key = n.player_key
         WHERE n.event_id = ? ORDER BY n.seed",
    )
    .bind(id)
    .fetch_all(db)
    .await?;

    let bracket = match load_bracket(db, id).await {
        Ok(b) => Some(b),
        Err(LifecycleError::NotStarted) => None,
        Err(e) => return Err(e.into()),
    };

    let rows: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT set_id, wins0, wins1 FROM sets WHERE event_id = ? ORDER BY set_id")
            .bind(id)
            .fetch_all(db)
            .await?;
    let scores = rows
        .into_iter()
        .map(|(set_id, w0, w1)| Ok((to_int(set_id)?, [to_int(w0)?, to_int(w1)?])))
        .collect::<Result<_, ApiError>>()?;

    let decided: Vec<(i64, String)> = sqlx::query_as(
        "SELECT set_id, decided_by FROM sets
         WHERE event_id = ? AND decided_by IS NOT NULL ORDER BY set_id",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    let decided_by = decided
        .into_iter()
        .map(|(set_id, how)| Ok((to_int(set_id)?, how)))
        .collect::<Result<_, ApiError>>()?;

    let unverified_games = crate::lifecycle::unverified_games(db, id).await?;
    let entrants = i64::try_from(tags.len()).unwrap_or(i64::MAX);
    Ok(EventView {
        summary: summary(id, name, state, entrants)?,
        ruleset,
        tags,
        bracket,
        scores,
        decided_by,
        unverified_games,
    })
}

pub(super) fn summary(
    id: i64,
    name: String,
    state: String,
    entrants: i64,
) -> Result<EventSummary, ApiError> {
    Ok(EventSummary {
        id,
        name,
        state,
        entrants: to_int(entrants)?,
    })
}

/// Narrows a stored integer, treating an out-of-range value as corrupt data.
fn to_int<T: TryFrom<i64>>(v: i64) -> Result<T, ApiError> {
    T::try_from(v).map_err(|_| ApiError::internal(format_args!("stored integer {v} out of range")))
}

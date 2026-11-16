//! Public results routes: history, the players index and player profiles.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use proto::{EventResult, PlayerProfile, PlayerSummary};
use serde::Deserialize;

use super::ApiError;
use crate::results::{
    load_events, load_latest_complete, placings, player_profile, player_summaries,
};
use crate::state::AppState;

/// The most `?limit=` may ask for.
const MAX_HISTORY_LIMIT: u32 = 100;

#[derive(Deserialize)]
pub struct HistoryQuery {
    limit: Option<u32>,
}

/// `GET /v1/history[?limit=N]`: complete events, newest first, with their placings; `limit`
/// (1..=100) keeps the newest N, and without it every complete event is listed.
pub async fn history(
    State(state): State<AppState>,
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> Result<Json<Vec<EventResult>>, ApiError> {
    let Query(HistoryQuery { limit }) =
        query.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    if limit.is_some_and(|n| !(1..=MAX_HISTORY_LIMIT).contains(&n)) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("limit must be 1 to {MAX_HISTORY_LIMIT}"),
        ));
    }
    let events = load_latest_complete(&state.db, limit).await?;
    let results = events
        .iter()
        .map(|ev| EventResult {
            summary: ev.summary.clone(),
            placings: placings(ev),
        })
        .collect();
    Ok(Json(results))
}

/// `GET /v1/players`: every registered player, by tag ignoring case.
pub async fn players(State(state): State<AppState>) -> Result<Json<Vec<PlayerSummary>>, ApiError> {
    let tags: Vec<String> = sqlx::query_scalar("SELECT tag FROM players")
        .fetch_all(&state.db)
        .await?;
    let events = load_events(&state.db).await?;
    Ok(Json(player_summaries(&tags, &events)))
}

/// `GET /v1/players/{tag}`: the profile of the player whose tag matches in any case; 404 for
/// an unknown tag. Among case variants the exact spelling wins, then the one that is not a
/// marked duplicate.
pub async fn player(
    State(state): State<AppState>,
    Path(tag): Path<String>,
) -> Result<Json<PlayerProfile>, ApiError> {
    let (tag, since) = find_player(&state.db, &tag).await?;
    let events = load_events(&state.db).await?;
    Ok(Json(player_profile(&tag, since, &events)))
}

/// The canonical tag and registration time of the player whose tag matches `tag` in any
/// case; 404 `no such player` when there is none. Among case variants the exact spelling
/// wins, then the one that is not a marked duplicate.
pub(super) async fn find_player(
    db: &sqlx::SqlitePool,
    tag: &str,
) -> Result<(String, i64), ApiError> {
    let found: Option<(String, i64)> = sqlx::query_as(
        "SELECT tag, created_at FROM players WHERE tag = ? COLLATE NOCASE
         ORDER BY tag = ? DESC, tag_case_duplicate LIMIT 1",
    )
    .bind(tag)
    .bind(tag)
    .fetch_optional(db)
    .await?;
    found.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such player"))
}

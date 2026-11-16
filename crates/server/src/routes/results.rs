//! Public results routes: history, the players index and player profiles.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use proto::{EventResult, PlayerProfile, PlayerSummary};

use super::ApiError;
use crate::results::{load_events, placings, player_profile, player_summaries};
use crate::state::AppState;

/// `GET /v1/history`: complete events, newest first, with their placings.
pub async fn history(State(state): State<AppState>) -> Result<Json<Vec<EventResult>>, ApiError> {
    let events = load_events(&state.db).await?;
    let results = events
        .iter()
        .rev()
        .filter(|ev| ev.summary.state == "complete")
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
    let found: Option<(String, i64)> = sqlx::query_as(
        "SELECT tag, created_at FROM players WHERE tag = ? COLLATE NOCASE
         ORDER BY tag = ? DESC, tag_case_duplicate LIMIT 1",
    )
    .bind(&tag)
    .bind(&tag)
    .fetch_optional(&state.db)
    .await?;
    let (tag, since) =
        found.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such player"))?;
    let events = load_events(&state.db).await?;
    Ok(Json(player_profile(&tag, since, &events)))
}

//! Organiser routes over the quarantine corpus: games kept for study, and their replays.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use proto::QuarantineItem;

use super::admin::parse_stored;
use super::ApiError;
use crate::auth::To;
use crate::state::AppState;

type Row = (
    i64,
    i64,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// `GET /v1/quarantine`: every quarantined game, oldest first.
pub async fn list(
    State(state): State<AppState>,
    _to: To,
) -> Result<Json<Vec<QuarantineItem>>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT q.id, q.game_id, q.kind, q.created_at, g.verdict_json, g.resim_json,
                g.verifier_build, g.judged_at
         FROM quarantine q LEFT JOIN games g ON g.id = q.game_id ORDER BY q.id",
    )
    .fetch_all(&state.db)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for (id, game_id, kind, created_at, verdict, resim, verifier_build, judged_at) in rows {
        let client_builds: Vec<String> =
            sqlx::query_scalar("SELECT client_build FROM uploads WHERE game_id = ? ORDER BY slot")
                .bind(game_id)
                .fetch_all(&state.db)
                .await?;
        items.push(QuarantineItem {
            id,
            game_id,
            kind,
            verdict: parse_stored(verdict.as_deref())?,
            resim: parse_stored(resim.as_deref())?,
            verifier_build,
            client_builds,
            created_at,
            judged_at,
        });
    }
    Ok(Json(items))
}

/// `GET /v1/quarantine/{id}/replay/{slot}`: the `.slp` uploaded for `slot` of the
/// quarantined game, read from the path the server stored.
pub async fn replay(
    State(state): State<AppState>,
    path: Result<Path<(i64, i64)>, PathRejection>,
    _to: To,
) -> Result<impl IntoResponse, ApiError> {
    let Path((id, slot)) =
        path.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT u.path FROM quarantine q JOIN uploads u ON u.game_id = q.game_id
         WHERE q.id = ? AND u.slot = ?",
    )
    .bind(id)
    .bind(slot)
    .fetch_optional(&state.db)
    .await?;
    let stored = stored.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such replay"))?;
    let bytes = match tokio::fs::read(&stored).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(quarantine = id, path = %stored, "quarantined replay is missing");
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "replay file is missing",
            ));
        }
        Err(e) => return Err(ApiError::internal(format_args!("reading {stored}: {e}"))),
    };
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes))
}

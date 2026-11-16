//! Routes an external verifier (`autobracket verify-worker`) uses: claim a queued
//! re-simulation job, fetch its replay, and post the result.

use std::fmt::Display;

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use proto::{VerifyJob, VerifyResult};

use super::{parse_body, ApiError};
use crate::auth::Verifier;
use crate::db::{begin_write, now_unix};
use crate::logging::{key_prefix, label, resim_kind};
use crate::state::AppState;

/// A claim older than this is presumed abandoned and the job is offered again.
const CLAIM_TTL_SECS: i64 = 300;

/// `POST /v1/verify/claim`: takes the oldest job that is queued, or whose claim has gone
/// stale. `200 VerifyJob`, or `204` when there is none.
pub async fn claim(State(state): State<AppState>, v: Verifier) -> Result<Response, ApiError> {
    let now = now_unix();
    let mut tx = begin_write(&state.db).await?;
    let job: Option<(i64, i32)> = sqlx::query_as(
        "SELECT id, replay_last_frame FROM resim_jobs
         WHERE state = 'queued' OR (state = 'claimed' AND claimed_at < ?)
         ORDER BY id LIMIT 1",
    )
    .bind(now - CLAIM_TTL_SECS)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, replay_last_frame)) = job else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    sqlx::query("UPDATE resim_jobs SET state = 'claimed', claimed_at = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(job = id, verifier = %key_prefix(&v.key), "verify job claimed");
    Ok(Json(VerifyJob {
        id,
        replay_last_frame,
    })
    .into_response())
}

/// `GET /v1/verify/jobs/{id}/replay`: the replay file the job checks.
pub async fn replay(
    State(state): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
    _v: Verifier,
) -> Result<Response, ApiError> {
    let Path(id) = path.map_err(bad_request)?;
    // The file comes only from the job row, never from the request.
    let file: Option<String> = sqlx::query_scalar("SELECT path FROM resim_jobs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let file = file.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such job"))?;
    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|e| ApiError::internal(format_args!("reading {file} for job {id}: {e}")))?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response())
}

/// `POST /v1/verify/jobs/{id}/result` (`VerifyResult`): records the result of a claimed
/// job. `204`; `409` unless the job is claimed, so a finished job keeps its first result.
pub async fn result(
    State(state): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
    v: Verifier,
) -> Result<StatusCode, ApiError> {
    let Path(id) = path.map_err(bad_request)?;
    let result: VerifyResult = parse_body(&v.body)?;
    let json = serde_json::to_string(&result).map_err(ApiError::internal)?;
    let mut tx = begin_write(&state.db).await?;
    let job_state: Option<String> = sqlx::query_scalar("SELECT state FROM resim_jobs WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    match job_state.as_deref() {
        None => return Err(ApiError::new(StatusCode::NOT_FOUND, "no such job")),
        Some("claimed") => {}
        Some(_) => return Err(ApiError::new(StatusCode::CONFLICT, "job is not claimed")),
    }
    sqlx::query("UPDATE resim_jobs SET state = 'done', result_json = ? WHERE id = ?")
        .bind(&json)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(
        job = id,
        outcome = %resim_kind(&result.outcome),
        build = %label(&result.build),
        verifier = %key_prefix(&v.key),
        "verify result posted"
    );
    state.resim.result_posted();
    Ok(StatusCode::NO_CONTENT)
}

fn bad_request(e: impl Display) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, e.to_string())
}

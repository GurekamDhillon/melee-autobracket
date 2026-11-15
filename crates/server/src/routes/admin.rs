//! Organiser routes: creating events and invites, starting events.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use proto::{EventCreateReq, EventSummary, EventView};
use rand::rngs::OsRng;
use rand::Rng;
use serde_json::{json, Value};

use super::events::{event_view, summary};
use super::{parse_body, ApiError};
use crate::auth::To;
use crate::db::now_unix;
use crate::lifecycle::{self, format_to_str};
use crate::state::AppState;

/// RFC 4648 base32 alphabet.
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const INVITE_LEN: usize = 12;

/// `POST /v1/admin/events`: a new event open for sign-up. The ruleset defaults to `cfg.ruleset`.
pub async fn create_event(
    State(state): State<AppState>,
    to: To,
) -> Result<(StatusCode, Json<EventSummary>), ApiError> {
    let req: EventCreateReq = parse_body(&to.body)?;
    let ruleset = req.ruleset.unwrap_or_else(|| state.cfg.ruleset.clone());
    let ruleset_json = serde_json::to_string(&ruleset).map_err(ApiError::internal)?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO events (name, format, ruleset_json, state, created_at)
         VALUES (?, ?, ?, 'signup', ?) RETURNING id",
    )
    .bind(&req.name)
    .bind(format_to_str(req.format))
    .bind(ruleset_json)
    .bind(now_unix())
    .fetch_one(&state.db)
    .await?;
    let created = summary(id, req.name, "signup".into(), 0)?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// `POST /v1/admin/invites`: a fresh single-use registration code.
pub async fn create_invite(
    State(state): State<AppState>,
    _to: To,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let code: String = (0..INVITE_LEN)
        .map(|_| char::from(BASE32[OsRng.gen_range(0..BASE32.len())]))
        .collect();
    sqlx::query("INSERT INTO invites (code) VALUES (?)")
        .bind(&code)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "code": code }))))
}

/// `POST /v1/admin/events/{id}/start`: closes sign-up and builds the bracket.
pub async fn start_event(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    _to: To,
) -> Result<Json<EventView>, ApiError> {
    lifecycle::start_event(&state, id).await?;
    event_view(&state.db, id).await.map(Json)
}

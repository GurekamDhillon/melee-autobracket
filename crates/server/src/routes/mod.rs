//! HTTP routes.

use std::fmt::Display;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::state::AppState;

mod admin;
mod events;
mod players;
mod quarantine;
mod sets;
mod verify_jobs;
mod view;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(view::page))
        .route("/v1/players", post(players::register))
        .route("/v1/events", get(events::list))
        .route("/v1/events/{id}", get(events::view))
        .route("/v1/events/{id}/entrants", post(events::signup))
        .route("/v1/admin/events", post(admin::create_event))
        .route("/v1/admin/events/{id}/start", post(admin::start_event))
        .route("/v1/admin/invites", post(admin::create_invite))
        .route("/v1/me/next", get(sets::next))
        .route("/v1/sets/{event}/{set}/room", post(sets::room))
        .route("/v1/sets/{event}/{set}/no-show", post(sets::no_show))
        .route(
            "/v1/sets/{event}/{set}/games/{n}/replay",
            post(sets::upload),
        )
        .route("/v1/admin/queue", get(admin::queue))
        .route("/v1/admin/queue/{id}/resolve", post(admin::resolve))
        .route("/v1/admin/players/{old_key}/rekey", post(admin::rekey))
        .route("/v1/quarantine", get(quarantine::list))
        .route("/v1/quarantine/{id}/replay/{slot}", get(quarantine::replay))
        .route("/v1/verify/claim", post(verify_jobs::claim))
        .route("/v1/verify/jobs/{id}/replay", get(verify_jobs::replay))
        .route("/v1/verify/jobs/{id}/result", post(verify_jobs::result))
}

/// Longest engine build string a client may send (`engine_build`, `x-ab-build`).
pub(crate) const MAX_BUILD_CHARS: usize = 64;
/// Longest event name.
pub(crate) const MAX_EVENT_NAME_CHARS: usize = 80;

/// An error response: a status code and a JSON body of the form `{"error": "...", ...}`.
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub Value);

impl ApiError {
    /// A `{"error": msg}` body.
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        ApiError(status, json!({ "error": msg.into() }))
    }

    /// Logs the cause; the client only sees a generic 500.
    pub fn internal(e: impl Display) -> Self {
        tracing::error!(error = %crate::logging::detail(e), "internal error");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::internal(format_args!("database error: {e}"))
    }
}

/// 400 unless `value` is at most `max_chars` characters with nothing unprintable in it (no
/// control characters, no line or paragraph separators): text that is stored and logged.
pub(crate) fn check_text(field: &str, value: &str, max_chars: usize) -> Result<(), ApiError> {
    if crate::logging::is_printable_within(value, max_chars) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} must be at most {max_chars} printable characters"),
        ))
    }
}

/// Parses a JSON request body; failure is a 400.
fn parse_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid request body: {e}"),
        )
    })
}

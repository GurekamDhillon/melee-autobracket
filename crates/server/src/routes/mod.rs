//! HTTP routes.

use std::fmt::Display;

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
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
        .route(
            "/v1/admin/events/{id}/entrants/{tag}",
            delete(admin::remove_entrant),
        )
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
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "no such route")
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method not allowed on this route",
    )
}

/// Longest plain-text error body carried over into the JSON one.
const MAX_PLAIN_ERROR_BYTES: usize = 1024;

/// Gives every error response the `{"error": "..."}` JSON body: one from a handler already
/// has it; one from elsewhere (an extractor rejection, the rate limiter) gets its text, or
/// the status's name when it has none.
pub(crate) async fn json_errors(res: Response) -> Response {
    let is_json = res
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
    if !(res.status().is_client_error() || res.status().is_server_error()) || is_json {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let text = axum::body::to_bytes(body, MAX_PLAIN_ERROR_BYTES)
        .await
        .ok()
        .and_then(|b| String::from_utf8(b.to_vec()).ok())
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            parts
                .status
                .canonical_reason()
                .unwrap_or("error")
                .to_lowercase()
        });
    let body = json!({ "error": text }).to_string();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(body))
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

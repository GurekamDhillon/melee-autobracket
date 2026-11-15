//! HTTP routes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use proto::EventSummary;

use crate::state::AppState;

mod players;
mod view;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(view::page))
        .route("/v1/players", post(players::register))
        .route("/v1/events", get(list_events))
}

async fn list_events() -> Json<Vec<EventSummary>> {
    Json(Vec::new())
}

/// An error response: a status code and a plain-text message.
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        ApiError(status, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    /// Logs the cause; the client only sees a generic 500.
    fn from(e: sqlx::Error) -> Self {
        eprintln!("database error: {e}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

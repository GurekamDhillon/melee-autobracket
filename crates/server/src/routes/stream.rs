//! `GET /v1/stream`: where the live stream is, for the stream page and the landing page.

use axum::extract::State;
use axum::Json;
use proto::StreamInfo;

use crate::state::AppState;

/// The configured `stream_url` as it is, or `{"url": null}`. The page it reaches may frame
/// only this URL's origin (its CSP says so); the URL itself never appears in any page's HTML.
pub async fn stream(State(state): State<AppState>) -> Json<StreamInfo> {
    Json(StreamInfo {
        url: state.cfg.stream_url.clone(),
    })
}

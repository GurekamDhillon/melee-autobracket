//! The read-only bracket page for spectators.

use axum::response::Html;

const PAGE: &str = include_str!("../../assets/bracket.html");

/// `GET /`: one self-contained page that polls the public event endpoints.
pub async fn page() -> Html<&'static str> {
    Html(PAGE)
}

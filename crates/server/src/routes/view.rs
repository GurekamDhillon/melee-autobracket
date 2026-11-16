//! The public pages: each assembled once from the shell, the shared stylesheet and script and
//! its own fragments, and served as one self-contained document with a hash-pinned CSP.
//!
//! Adding a page is one entry in [`SOURCES`], one handler calling [`page`], and one route.

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use sha2::{Digest, Sha256};

use super::ApiError;
use crate::state::AppState;

/// The document every page is poured into: `{{title}}`, `{{style}}`, `{{body}}`, `{{script}}`.
const SHELL: &str = include_str!("../../assets/shell.html");
/// Every page's styles.
const SITE_CSS: &str = include_str!("../../assets/site.css");
/// Every page's script (`AB`), ahead of the page's own.
const SITE_JS: &str = include_str!("../../assets/site.js");

/// The pages: name, title, body fragment, page script.
const SOURCES: &[(&str, &str, &str, &str)] = &[
    (
        "home",
        "Autobracket",
        include_str!("../../assets/pages/home.html"),
        include_str!("../../assets/pages/home.js"),
    ),
    (
        "event",
        "Bracket",
        include_str!("../../assets/pages/event.html"),
        include_str!("../../assets/pages/event.js"),
    ),
    (
        "schedule",
        "Schedule",
        include_str!("../../assets/pages/schedule.html"),
        include_str!("../../assets/pages/schedule.js"),
    ),
    (
        "history",
        "History",
        include_str!("../../assets/pages/history.html"),
        include_str!("../../assets/pages/history.js"),
    ),
];

/// A finished page and the policy that pins its inline blocks.
struct Page {
    html: String,
    csp: String,
}

/// Every page, assembled on first use.
static PAGES: LazyLock<HashMap<&'static str, Page>> = LazyLock::new(|| {
    SOURCES
        .iter()
        .map(|(name, title, body, script)| (*name, assemble(title, body, script)))
        .collect()
});

/// Fills the shell for one page. The CSP allows nothing but the page's own inline script and
/// style (by hash), requests to this server, fonts from this server, the empty `data:` icon,
/// and no framing.
fn assemble(title: &str, body: &str, script: &str) -> Page {
    let script = format!("{SITE_JS}\n{script}");
    let html = SHELL
        .replace("{{title}}", title)
        .replace("{{style}}", SITE_CSS)
        .replace("{{body}}", body)
        .replace("{{script}}", &script);
    let csp = format!(
        "default-src 'none'; script-src 'sha256-{}'; style-src 'sha256-{}'; \
         connect-src 'self'; font-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
         frame-ancestors 'none'",
        inline_hash(&html, "script"),
        inline_hash(&html, "style"),
    );
    Page { html, csp }
}

/// The assembled page `name` (a key of [`SOURCES`]) with its headers.
fn page(name: &'static str) -> Response {
    match PAGES.get(name) {
        Some(p) => (
            [
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CONTENT_SECURITY_POLICY, p.csp.as_str()),
            ],
            Html(p.html.clone()),
        )
            .into_response(),
        None => ApiError::internal(format_args!("no page named {name}")).into_response(),
    }
}

/// `GET /`: the landing page.
pub async fn home() -> Response {
    page("home")
}

/// `GET /events/{id}`: the bracket page; 404 unless the event exists.
pub async fn event(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM events WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such event"));
    }
    Ok(page("event"))
}

/// `GET /schedule`: what is running and what is coming up.
pub async fn schedule() -> Response {
    page("schedule")
}

/// `GET /history`: completed events and their placings.
pub async fn history() -> Response {
    page("history")
}

/// The pages' fonts by file name (`assets/fonts/README.md` says where they come from).
const FONTS: [(&str, &[u8]); 2] = [
    (
        "bricolage-grotesque-800.woff2",
        include_bytes!("../../assets/fonts/bricolage-grotesque-800.woff2"),
    ),
    (
        "autobracket-text-700.woff2",
        include_bytes!("../../assets/fonts/autobracket-text-700.woff2"),
    ),
];

/// How long a font may be cached: a year, and never revalidated. A changed font gets a new
/// file name.
const FONT_CACHE: &str = "public, max-age=31536000, immutable";

/// `GET /assets/fonts/{file}`: one of the pages' fonts; any other name is a 404.
pub async fn font(Path(file): Path<String>) -> Result<Response, ApiError> {
    let (_, bytes) = FONTS
        .iter()
        .find(|(name, _)| *name == file)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such font"))?;
    Ok((
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, FONT_CACHE),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        *bytes,
    )
        .into_response())
}

/// Base64 SHA-256 of the text inside the page's one `<tag>…</tag>` block (empty if absent).
fn inline_hash(page: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let text = page
        .split_once(&open)
        .and_then(|(_, rest)| rest.split_once(&close))
        .map_or("", |(inner, _)| inner);
    base64(&Sha256::digest(text.as_bytes()))
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64, PAGES, SOURCES};

    #[test]
    fn base64_matches_rfc_4648() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected, "{input:?}");
        }
    }

    /// Every source page assembles, with no placeholder left unfilled.
    #[test]
    fn every_page_assembles() {
        assert_eq!(PAGES.len(), SOURCES.len());
        for (name, page) in PAGES.iter() {
            assert!(
                !page.html.contains("{{"),
                "{name}: placeholder left in page"
            );
            assert!(
                page.csp.contains("script-src 'sha256-"),
                "{name}: {}",
                page.csp
            );
        }
    }
}

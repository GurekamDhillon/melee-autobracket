//! The read-only bracket page for spectators.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use sha2::{Digest, Sha256};

use super::ApiError;

const PAGE: &str = include_str!("../../assets/bracket.html");

/// The page's fonts by file name (`assets/fonts/README.md` says where they come from).
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

/// The page's Content-Security-Policy: nothing but its own inline script and style (by hash),
/// requests to this server, fonts from this server, the empty `data:` icon, and no framing.
static CSP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "default-src 'none'; script-src 'sha256-{}'; style-src 'sha256-{}'; \
         connect-src 'self'; font-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
         frame-ancestors 'none'",
        inline_hash("script"),
        inline_hash("style"),
    )
});

/// `GET /`: one self-contained page that polls the public event endpoints.
pub async fn page() -> impl IntoResponse {
    (
        [
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_SECURITY_POLICY, CSP.as_str()),
        ],
        Html(PAGE),
    )
}

/// `GET /assets/fonts/{file}`: one of the page's fonts; any other name is a 404.
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
fn inline_hash(tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let text = PAGE
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
    use super::base64;

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
}

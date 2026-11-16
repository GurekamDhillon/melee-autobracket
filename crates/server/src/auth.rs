//! Signed-request extractors. Each buffers the request body (up to
//! `cfg.max_replay_bytes`), verifies the signature over method, path-and-query
//! and body, records the nonce, and hands the body on to the handler.

use axum::extract::{FromRequest, OriginalUri, Request};
use axum::http::{HeaderMap, StatusCode};
use bytes::Bytes;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use proto::{verify_request, SignedHeaders};

use crate::db::now_unix;
use crate::routes::ApiError;
use crate::state::AppState;

/// How long a nonce is remembered; longer than any request time can be accepted.
const NONCE_TTL_SECS: i64 = 300;

/// Rejections use the same `{"error": ...}` JSON body as handler errors.
pub type Rejection = ApiError;

/// A registered player. 401 when unsigned or invalid, 403 when the key is not registered.
pub struct Player {
    pub key: String,
    pub tag: String,
    pub body: Bytes,
}

/// A tournament organiser: 403 unless the key is in `cfg.to_keys`.
pub struct To {
    pub key: String,
    pub body: Bytes,
}

/// Any validly signed request, registered or not.
pub struct AnyKey {
    pub key: String,
    pub body: Bytes,
}

impl FromRequest<AppState> for AnyKey {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let (parts, body) = req.into_parts();
        // Sign over the URI the client sent, not one with a `Router::nest` prefix stripped.
        let uri = parts
            .extensions
            .get::<OriginalUri>()
            .map_or(&parts.uri, |o| &o.0);
        let headers = signed_headers(&parts.headers)
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing signature headers"))?;
        let body = match Limited::new(body, state.cfg.max_replay_bytes)
            .collect()
            .await
        {
            Ok(collected) => collected.to_bytes(),
            Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => {
                return Err(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body too large",
                ))
            }
            Err(_) => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "failed to read request body",
                ))
            }
        };
        let path_and_query = uri.path_and_query().map_or(uri.path(), |pq| pq.as_str());
        let now = now_unix();
        let key = verify_request(&headers, parts.method.as_str(), path_and_query, now, &body)
            .map_err(|_| ApiError::new(StatusCode::UNAUTHORIZED, "invalid signature"))?;
        record_nonce(state, &key, &headers.nonce, now).await?;
        Ok(AnyKey { key, body })
    }
}

impl FromRequest<AppState> for Player {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let AnyKey { key, body } = AnyKey::from_request(req, state).await?;
        let tag: Option<String> = sqlx::query_scalar("SELECT tag FROM players WHERE key = ?")
            .bind(&key)
            .fetch_optional(&state.db)
            .await
            .map_err(internal)?;
        let tag =
            tag.ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "key is not registered"))?;
        Ok(Player { key, tag, body })
    }
}

impl FromRequest<AppState> for To {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let AnyKey { key, body } = AnyKey::from_request(req, state).await?;
        if !state
            .cfg
            .to_keys
            .iter()
            .any(|k| k.eq_ignore_ascii_case(&key))
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "not a tournament organiser",
            ));
        }
        Ok(To { key, body })
    }
}

fn signed_headers(h: &HeaderMap) -> Option<SignedHeaders> {
    let get = |name: &str| h.get(name)?.to_str().ok().map(str::to_owned);
    Some(SignedHeaders {
        key: get("x-ab-key")?,
        time: get("x-ab-time")?,
        nonce: get("x-ab-nonce")?,
        sig: get("x-ab-sig")?,
    })
}

/// Forgets expired nonces, then records this one; a nonce already seen for the key is a replay.
async fn record_nonce(state: &AppState, key: &str, nonce: &str, now: i64) -> Result<(), Rejection> {
    sqlx::query("DELETE FROM nonces WHERE seen_at < ?")
        .bind(now - NONCE_TTL_SECS)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    let inserted =
        sqlx::query("INSERT OR IGNORE INTO nonces (key, nonce, seen_at) VALUES (?, ?, ?)")
            .bind(key)
            .bind(nonce)
            .bind(now)
            .execute(&state.db)
            .await
            .map_err(internal)?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "nonce already used",
        ));
    }
    Ok(())
}

/// Logs a store failure and maps it to a generic 500.
fn internal(e: sqlx::Error) -> Rejection {
    eprintln!("auth: database error: {e}");
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

//! Signed-request extractors. Each checks, in this order and before reading any of the body:
//! that the signature headers are well formed, that the signed time is within the allowed
//! clock skew, and that the key holds the role the route needs. Only then is the body read
//! (bounded in size and time), the signature verified over method, path-and-query and body,
//! and the nonce recorded. The body is handed on to the handler.

use std::time::Duration;

use axum::extract::{FromRequest, OriginalUri, Request};
use axum::http::{header, HeaderMap, StatusCode};
use bytes::Bytes;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use proto::{check_headers, verify_request, SignError, SignedHeaders, MAX_SKEW_SECS};

use crate::db::now_unix;
use crate::routes::ApiError;
use crate::state::AppState;

/// How long a nonce is remembered; longer than any request time can be accepted.
const NONCE_TTL_SECS: i64 = 300;
/// Largest body of every route except replay upload (which takes `cfg.max_replay_bytes`).
pub const MAX_JSON_BODY_BYTES: usize = 64 * 1024;
/// How long the body of a request may take to arrive.
pub const BODY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the body of a replay upload may take to arrive: as long as the client allows.
pub const UPLOAD_BODY_TIMEOUT: Duration = Duration::from_secs(120);

/// Rejections use the same `{"error": ...}` JSON body as handler errors.
pub type Rejection = ApiError;

/// A registered player. 401 when unsigned or invalid, 403 when the key is not registered.
pub struct Player {
    pub key: String,
    pub tag: String,
    pub body: Bytes,
}

/// A registered player uploading a replay: as [`Player`], with a body of up to
/// `cfg.max_replay_bytes` and [`UPLOAD_BODY_TIMEOUT`] to send it.
pub struct PlayerUpload(pub Player);

/// A tournament organiser: 403 unless the key is in `cfg.to_keys`.
pub struct To {
    pub key: String,
    pub body: Bytes,
}

/// A replay verifier: 403 unless the key is in `cfg.verifier_keys` or `cfg.to_keys`.
pub struct Verifier {
    pub key: String,
    pub body: Bytes,
}

/// Any validly signed request, registered or not.
pub struct AnyKey {
    pub key: String,
    pub body: Bytes,
}

/// The role a route needs, checked before the body is read.
enum Role {
    Any,
    Player,
    To,
    Verifier,
}

/// How much body a route accepts, and how long it may take to arrive.
struct BodyLimit {
    bytes: usize,
    timeout: Duration,
}

const JSON_BODY: BodyLimit = BodyLimit {
    bytes: MAX_JSON_BODY_BYTES,
    timeout: BODY_TIMEOUT,
};

/// A request whose signature verified.
struct Signed {
    key: String,
    /// The player's tag, for [`Role::Player`].
    tag: Option<String>,
    body: Bytes,
}

async fn authenticate(
    req: Request,
    state: &AppState,
    role: Role,
    limit: BodyLimit,
) -> Result<Signed, Rejection> {
    let (parts, body) = req.into_parts();
    let headers = signed_headers(&parts.headers).ok_or_else(|| {
        state.rejections.record();
        ApiError::new(StatusCode::UNAUTHORIZED, "missing signature headers")
    })?;
    // One clock reading for the skew check, the signature check and the nonce.
    let now = now_unix();
    let key = check_headers(&headers, now).map_err(|e| sign_rejection(state, e))?;
    let tag = authorize(state, &role, &key).await?;

    if declared_length(&parts.headers).is_some_and(|n| n > limit.bytes) {
        return Err(too_large());
    }
    let read = tokio::time::timeout(limit.timeout, Limited::new(body, limit.bytes).collect());
    let body = match read.await {
        Err(_) => {
            return Err(ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "request body not received in time",
            ))
        }
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(e)) if e.downcast_ref::<LengthLimitError>().is_some() => return Err(too_large()),
        Ok(Err(_)) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "failed to read request body",
            ))
        }
    };

    // Sign over the URI the client sent, not one with a `Router::nest` prefix stripped.
    let uri = parts
        .extensions
        .get::<OriginalUri>()
        .map_or(&parts.uri, |o| &o.0);
    let path_and_query = uri.path_and_query().map_or(uri.path(), |pq| pq.as_str());
    verify_request(&headers, parts.method.as_str(), path_and_query, now, &body)
        .map_err(|e| sign_rejection(state, e))?;
    record_nonce(state, &key, &headers.nonce, now).await?;
    Ok(Signed { key, tag, body })
}

/// Checks that `key` holds `role`; for a player, returns their tag.
async fn authorize(state: &AppState, role: &Role, key: &str) -> Result<Option<String>, Rejection> {
    let cfg = &state.cfg;
    let listed = |keys: &[String]| keys.iter().any(|k| k.eq_ignore_ascii_case(key));
    match role {
        Role::Any => Ok(None),
        Role::Player => {
            let tag: Option<String> = sqlx::query_scalar("SELECT tag FROM players WHERE key = ?")
                .bind(key)
                .fetch_optional(&state.db)
                .await
                .map_err(internal)?;
            tag.map(Some)
                .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "key is not registered"))
        }
        Role::To if listed(&cfg.to_keys) => Ok(None),
        Role::To => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not a tournament organiser",
        )),
        Role::Verifier if listed(&cfg.verifier_keys) || listed(&cfg.to_keys) => Ok(None),
        Role::Verifier => Err(ApiError::new(StatusCode::FORBIDDEN, "not a verifier")),
    }
}

impl FromRequest<AppState> for AnyKey {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let s = authenticate(req, state, Role::Any, JSON_BODY).await?;
        Ok(AnyKey {
            key: s.key,
            body: s.body,
        })
    }
}

impl FromRequest<AppState> for Player {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let s = authenticate(req, state, Role::Player, JSON_BODY).await?;
        player(s)
    }
}

impl FromRequest<AppState> for PlayerUpload {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let limit = BodyLimit {
            bytes: state.cfg.max_replay_bytes,
            timeout: UPLOAD_BODY_TIMEOUT,
        };
        let s = authenticate(req, state, Role::Player, limit).await?;
        player(s).map(PlayerUpload)
    }
}

fn player(s: Signed) -> Result<Player, Rejection> {
    let tag = s
        .tag
        .ok_or_else(|| ApiError::internal("player authorised without a tag"))?;
    Ok(Player {
        key: s.key,
        tag,
        body: s.body,
    })
}

impl FromRequest<AppState> for To {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let s = authenticate(req, state, Role::To, JSON_BODY).await?;
        Ok(To {
            key: s.key,
            body: s.body,
        })
    }
}

impl FromRequest<AppState> for Verifier {
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Rejection> {
        let s = authenticate(req, state, Role::Verifier, JSON_BODY).await?;
        Ok(Verifier {
            key: s.key,
            body: s.body,
        })
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

/// The `content-length` the client declared, if any.
fn declared_length(h: &HeaderMap) -> Option<usize> {
    h.get(header::CONTENT_LENGTH)?.to_str().ok()?.parse().ok()
}

fn too_large() -> Rejection {
    ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
}

/// A 401 for a signature failure, counted (not logged one by one).
fn sign_rejection(state: &AppState, e: SignError) -> Rejection {
    state.rejections.record();
    let msg = match e {
        SignError::Malformed => "malformed signature headers".to_owned(),
        SignError::Skew => format!(
            "clock skew: your clock differs from the server's by more than {MAX_SKEW_SECS} seconds"
        ),
        SignError::BadSignature => "invalid signature".to_owned(),
    };
    ApiError::new(StatusCode::UNAUTHORIZED, msg)
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
        state.rejections.record();
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "nonce already used",
        ));
    }
    Ok(())
}

/// Logs a store failure and maps it to a generic 500.
fn internal(e: sqlx::Error) -> Rejection {
    tracing::error!(error = %crate::logging::detail(&e), "auth: database error");
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

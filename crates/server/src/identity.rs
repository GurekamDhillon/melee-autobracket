//! Web identity: who is signed in (`WebUser`) and who is an organiser (`AdminUser`), plus the
//! Zitadel hook signature check.
//!
//! A request is signed in only when all of these hold: identity is configured; the TCP peer
//! is in `trusted_proxies`; `X-Autobracket-Proxy` equals `identity.proxy_secret` (compared in
//! constant time); `X-Id-Token: Bearer <jwt>` verifies against the issuer's keys; and
//! `X-Auth-Request-User` equals the token's `sub`. Groups, `amr` and `auth_time` come from the
//! verified token only: no other identity header is ever read. These extractors are the only
//! code that reads the identity headers.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::get;
use axum::Router;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::IdentityConfig;
use crate::db::{begin_write, now_unix};
use crate::jwks::TokenError;
use crate::routes::ApiError;
use crate::state::AppState;

/// Most seconds a hook's signed time may differ from ours, either way.
pub const HOOK_MAX_SKEW_SECS: i64 = 300;
/// Where the admin 403 sends a user to sign in again.
const ADMIN_SIGN_IN_URL: &str = "/oauth2/sign_in?rd=/admin/";

/// A signed-in web user. Its account row exists (created on first sight).
#[derive(Clone, Debug)]
pub struct WebUser {
    pub account_id: i64,
    pub sub: String,
    pub groups: Vec<String>,
    pub amr: Vec<String>,
    pub auth_time: Option<i64>,
    /// The Discord global name, else username; `None` before the first Discord login.
    pub display_name: Option<String>,
}

/// A signed-in organiser who signed in with a second factor recently.
#[derive(Clone, Debug)]
pub struct AdminUser(pub WebUser);

/// Whether `groups` holds the organiser group: the exact string, nothing like it.
pub fn is_admin(groups: &[String], cfg: &IdentityConfig) -> bool {
    groups.contains(&cfg.admin_group)
}

/// Lowercase hex SHA-256 of `s` (account tombstones).
pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn not_signed_in() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "not signed in")
}

/// Equal length and bytes, in time that depends only on the lengths.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// A header's value as text; absent or not visible ASCII is `None`.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// The checks before the database: the peer, the proxy secret, the token and the header
/// `sub`. Returns the verified claims.
async fn verified_claims(
    parts: &Parts,
    state: &AppState,
    cfg: &IdentityConfig,
    now: i64,
) -> Result<crate::jwks::IdClaims, ApiError> {
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .ok_or_else(not_signed_in)?;
    if !state.trusted.iter().any(|c| c.contains(peer)) {
        return Err(not_signed_in());
    }
    let secret = parts
        .headers
        .get("x-autobracket-proxy")
        .ok_or_else(not_signed_in)?;
    if !constant_time_eq(secret.as_bytes(), cfg.proxy_secret.as_bytes()) {
        return Err(not_signed_in());
    }
    let header_sub = header(&parts.headers, "x-auth-request-user").ok_or_else(not_signed_in)?;
    let jwt = header(&parts.headers, "x-id-token")
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(not_signed_in)?;
    let claims = match state.jwks.verify(cfg, jwt, now).await {
        Ok(claims) => claims,
        Err(TokenError::Invalid) => return Err(not_signed_in()),
        Err(TokenError::Unavailable) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "sign-in service unavailable",
            ))
        }
    };
    if claims.sub != header_sub {
        return Err(not_signed_in());
    }
    Ok(claims)
}

/// A write (anything but GET and HEAD) must carry `x-ab-csrf: 1` and our exact `Origin`.
fn csrf_ok(parts: &Parts, cfg: &IdentityConfig) -> bool {
    if parts.method == Method::GET || parts.method == Method::HEAD {
        return true;
    }
    header(&parts.headers, "x-ab-csrf") == Some("1")
        && header(&parts.headers, "origin") == Some(cfg.public_origin.as_str())
}

/// How often (seconds) a signed-in account's `last_seen_at` is written.
const LAST_SEEN_REFRESH_SECS: i64 = 300;
/// How far in the future (seconds) a token's `auth_time` may lie (clock skew).
const AUTH_TIME_LEEWAY_SECS: i64 = 60;

fn deleted() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "this account was deleted")
}

fn disabled() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "this account is disabled")
}

async fn is_tombstoned(conn: &mut sqlx::SqliteConnection, sub: &str) -> Result<bool, ApiError> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM account_tombstones WHERE sub_sha256 = ?")
            .bind(sha256_hex(sub))
            .fetch_optional(conn)
            .await?;
    Ok(found.is_some())
}

/// Creates the account of `sub`, or sets its `last_seen_at` to `now`; returns its id. The
/// tombstone and `disabled_at` are checked again inside the write transaction, so a removal
/// hook cannot slip between the check and the insert.
async fn touch_account(state: &AppState, sub: &str, now: i64) -> Result<i64, ApiError> {
    let mut tx = begin_write(&state.db).await?;
    if is_tombstoned(&mut tx, sub).await? {
        return Err(deleted());
    }
    let (id, disabled_at): (i64, Option<i64>) = sqlx::query_as(
        "INSERT INTO accounts (sub, created_at, last_seen_at) VALUES (?, ?, ?)
         ON CONFLICT (sub) DO UPDATE SET last_seen_at = excluded.last_seen_at
         RETURNING id, disabled_at",
    )
    .bind(sub)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    if disabled_at.is_some() {
        return Err(disabled());
    }
    tx.commit().await?;
    Ok(id)
}

impl FromRequestParts<AppState> for WebUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let Some(cfg) = state.cfg.identity.as_ref() else {
            return Err(ApiError::new(StatusCode::NOT_FOUND, "no such route"));
        };
        let now = now_unix();
        let claims = verified_claims(parts, state, cfg, now).await?;

        // Reads first, on a plain connection: most requests write nothing.
        let mut conn = state.db.acquire().await?;
        if is_tombstoned(&mut conn, &claims.sub).await? {
            return Err(deleted());
        }
        let row: Option<(i64, Option<i64>, i64, Option<String>)> = sqlx::query_as(
            "SELECT a.id, a.disabled_at, a.last_seen_at, COALESCE(p.global_name, p.username)
             FROM accounts a LEFT JOIN discord_profiles p ON p.discord_id = a.discord_id
             WHERE a.sub = ?",
        )
        .bind(&claims.sub)
        .fetch_optional(&mut *conn)
        .await?;
        // At most one pool connection per request: never acquire another while this is held.
        drop(conn);
        if matches!(row, Some((_, Some(_), _, _))) {
            return Err(disabled());
        }
        if !csrf_ok(parts, cfg) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "cross-site request refused",
            ));
        }
        let (account_id, display_name) = match row {
            Some((id, _, seen, name)) if now.saturating_sub(seen) < LAST_SEEN_REFRESH_SECS => {
                (id, name)
            }
            Some((_, _, _, name)) => (touch_account(state, &claims.sub, now).await?, name),
            // A new account has no Discord id yet, so no name.
            None => (touch_account(state, &claims.sub, now).await?, None),
        };
        Ok(WebUser {
            account_id,
            sub: claims.sub,
            groups: claims.groups,
            amr: claims.amr,
            auth_time: claims.auth_time,
            display_name,
        })
    }
}

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = WebUser::from_request_parts(parts, state).await?;
        let Some(cfg) = state.cfg.identity.as_ref() else {
            return Err(ApiError::new(StatusCode::NOT_FOUND, "no such route"));
        };
        if !is_admin(&user.groups, cfg) {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "not an organiser"));
        }
        let max_age = i64::try_from(cfg.admin_max_auth_age_secs).unwrap_or(i64::MAX);
        let now = now_unix();
        let recent = user.auth_time.is_some_and(|t| {
            t <= now.saturating_add(AUTH_TIME_LEEWAY_SECS) && now.saturating_sub(t) <= max_age
        });
        if !user.amr.iter().any(|m| m == "mfa") || !recent {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                json!({
                    "error": "admin pages need a recent sign-in with a second factor",
                    "mfa_setup_url": cfg.mfa_setup_url,
                    "sign_in_url": ADMIN_SIGN_IN_URL,
                }),
            ));
        }
        Ok(AdminUser(user))
    }
}

/// Checks a Zitadel `zitadel-signature` header (`t=<unix>,v1=<hex>[,v1=<hex>…]`, parts in any
/// order, spaces allowed) over the raw `body` with `key`: HMAC-SHA256 of `"<t>.<body>"`,
/// compared in constant time, `t` within [`HOOK_MAX_SKEW_SECS`] of `now` either way. Returns
/// `(t, v1)` of the matching signature for the replay cache, `v1` as lowercase hex whatever
/// its spelling in the header; `None` on any failure.
pub fn verify_hook(
    sig_header: Option<&str>,
    body: &[u8],
    key: &str,
    now: i64,
) -> Option<(i64, String)> {
    let mut t = None;
    let mut sigs = Vec::new();
    for part in sig_header?.split(',') {
        let (name, value) = part.trim().split_once('=')?;
        match name.trim() {
            "t" => {
                let value = value.trim();
                if t.is_some()
                    || value.is_empty()
                    || value.len() > 18
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return None;
                }
                t = Some(value.parse::<i64>().ok()?);
            }
            "v1" => sigs.push(value.trim()),
            _ => {}
        }
    }
    let t = t?;
    if (now - t).abs() > HOOK_MAX_SKEW_SECS {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(t.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    sigs.into_iter().find_map(|v1| {
        let bytes = hex::decode(v1).ok()?;
        mac.clone()
            .verify_slice(&bytes)
            .ok()
            // Canonical lowercase hex: a re-spelled replay must hit the replay cache.
            .map(|()| (t, hex::encode(&bytes)))
    })
}

/// `GET`/`POST /account/api/ping` (signed in) and `GET /admin/api/ping` (organiser): 204.
/// Lets tests and the deploy check reach the extractors without any other route.
pub fn probe_router() -> Router<AppState> {
    async fn account(_: WebUser) -> StatusCode {
        StatusCode::NO_CONTENT
    }
    async fn admin(_: AdminUser) -> StatusCode {
        StatusCode::NO_CONTENT
    }
    Router::new()
        .route("/account/api/ping", get(account).post(account))
        .route("/admin/api/ping", get(admin))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] = br#"{"a":1}"#;
    const NOW: i64 = 1_800_000_000;

    fn v1(key: &str, t: i64, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(format!("{t}.").as_bytes());
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn verify_hook_accepts_a_valid_signature() {
        let sig = v1("k", NOW, BODY);
        let header = format!("t={NOW},v1={sig}");
        assert_eq!(verify_hook(Some(&header), BODY, "k", NOW), Some((NOW, sig)));
    }

    #[test]
    fn verify_hook_window_is_both_ways() {
        for t in [NOW - 301, NOW + 301] {
            let header = format!("t={t},v1={}", v1("k", t, BODY));
            assert_eq!(verify_hook(Some(&header), BODY, "k", NOW), None, "t = {t}");
        }
        for t in [NOW - 300, NOW + 300] {
            let header = format!("t={t},v1={}", v1("k", t, BODY));
            assert!(
                verify_hook(Some(&header), BODY, "k", NOW).is_some(),
                "t = {t}"
            );
        }
    }

    #[test]
    fn verify_hook_refuses_another_key_or_body() {
        let header = format!("t={NOW},v1={}", v1("other", NOW, BODY));
        assert_eq!(verify_hook(Some(&header), BODY, "k", NOW), None);
        let header = format!("t={NOW},v1={}", v1("k", NOW, BODY));
        assert_eq!(verify_hook(Some(&header), br#"{"a":2}"#, "k", NOW), None);
        // The signed time is part of the MAC.
        let header = format!("t={},v1={}", NOW + 1, v1("k", NOW, BODY));
        assert_eq!(verify_hook(Some(&header), BODY, "k", NOW), None);
    }

    #[test]
    fn verify_hook_any_v1_may_match() {
        let good = v1("k", NOW, BODY);
        let header = format!("t={NOW},v1={},v1={good}", v1("old", NOW, BODY));
        assert_eq!(
            verify_hook(Some(&header), BODY, "k", NOW),
            Some((NOW, good))
        );
    }

    /// The returned `v1` is canonical lowercase hex, so a re-spelled replay hits the cache.
    #[test]
    fn verify_hook_returns_canonical_v1() {
        let good = v1("k", NOW, BODY);
        let mixed: String = good
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        assert_ne!(mixed, good);
        for spelled in [good.to_uppercase(), mixed] {
            let header = format!("t={NOW},v1={spelled}");
            assert_eq!(
                verify_hook(Some(&header), BODY, "k", NOW),
                Some((NOW, good.clone())),
                "{spelled}"
            );
        }
    }

    #[test]
    fn verify_hook_parts_reordered_and_spaced() {
        let good = v1("k", NOW, BODY);
        for header in [
            format!("v1={good},t={NOW}"),
            format!(" t = {NOW} , v1 = {good} "),
            format!("v1={good}, t={NOW}, v0=ignored"),
            format!("t={NOW},v1={}", good.to_uppercase()),
        ] {
            assert_eq!(
                verify_hook(Some(&header), BODY, "k", NOW),
                Some((NOW, good.clone())),
                "{header}"
            );
        }
    }

    #[test]
    fn verify_hook_missing_or_malformed_header() {
        let good = v1("k", NOW, BODY);
        for header in [
            String::new(),
            format!("t={NOW}"),
            format!("v1={good}"),
            format!("t={NOW},v1={good},garbage"),
            format!("t=+{NOW},v1={good}"),
            format!("t=-{NOW},v1={good}"),
            format!("t={NOW},t={NOW},v1={good}"),
            format!("t=x,v1={good}"),
            format!("t=99999999999999999999,v1={good}"),
            format!("t={NOW},v1=zz{}", &good[2..]),
            format!("t={NOW},v1={}", &good[..62]),
        ] {
            assert_eq!(
                verify_hook(Some(&header), BODY, "k", NOW),
                None,
                "{header:?}"
            );
        }
        assert_eq!(verify_hook(None, BODY, "k", NOW), None);
    }

    #[test]
    fn admin_group_is_exact() {
        let cfg: IdentityConfig = toml::from_str(
            r#"
            public_origin = "https://bracket.test"
            issuer = "https://auth.test"
            client_id = "c"
            project_id = "p"
            org_id = "o"
            discord_idp_id = "i"
            proxy_secret = "test-proxy-secret-test-proxy-secret-00"
            sign_out_url = "/oauth2/sign_out"
            mfa_setup_url = "https://auth.test/mfa"
            [hook_keys]
            idp_intent = "a"
            token = "b"
            user_events = "c"
            "#,
        )
        .unwrap();
        let groups = |g: &[&str]| g.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_admin(&groups(&["x", "autobracket-admins"]), &cfg));
        for near in [
            "autobracket-admins-old",
            "AUTOBRACKET-ADMINS",
            "autobracket-admins ",
        ] {
            assert!(!is_admin(&groups(&[near]), &cfg), "{near}");
        }
        assert!(!is_admin(&[], &cfg));
    }

    #[test]
    fn constant_time_eq_compares_exactly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }
}

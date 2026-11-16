//! The identity provider's signing keys (JWKS) and ID-token verification.
//!
//! Keys are fetched from `identity.jwks_url()` on first use and kept for an hour. A token
//! whose `kid` is unknown makes one re-fetch, at most once a minute (key rotation). Only RSA
//! (RS256) and Ed25519 (EdDSA) keys are used, each with its own algorithm alone, so a token
//! can never choose how it is checked (no `none`, no HMAC with a public key).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::config::IdentityConfig;

/// How long a fetched key set is used before it is fetched again.
const CACHE_TTL: Duration = Duration::from_secs(3600);
/// Least time between two re-fetches caused by an unknown `kid`.
const UNKNOWN_KID_REFETCH: Duration = Duration::from_secs(60);
/// Least time between two attempts after a failed fetch.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(10);
/// How long one fetch may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest key set accepted.
const MAX_JWKS_BYTES: usize = 64 * 1024;
/// Largest token looked at.
const MAX_TOKEN_BYTES: usize = 16 * 1024;
/// Clock skew allowed on `exp`.
const EXP_LEEWAY_SECS: i64 = 60;
/// Longest accepted `sub`.
const MAX_SUB_CHARS: usize = 64;

/// What a verified ID token says about its user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdClaims {
    pub sub: String,
    pub groups: Vec<String>,
    pub amr: Vec<String>,
    pub auth_time: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenError {
    /// The token is malformed, wrongly signed, for someone else, expired, or its key unknown.
    Invalid,
    /// No key could be fetched yet, so no token can be checked.
    Unavailable,
}

/// A usable key and the one algorithm it verifies.
#[derive(Clone)]
struct Key {
    key: DecodingKey,
    alg: Algorithm,
}

#[derive(Default)]
struct Cache {
    keys: HashMap<String, Key>,
    /// The last successful fetch.
    fetched_at: Option<Instant>,
    /// The last failed fetch.
    failed_at: Option<Instant>,
    /// The last re-fetch caused by an unknown `kid`.
    refetched_at: Option<Instant>,
}

/// The JWKS cache. One per server; cheap to share behind an `Arc`.
///
/// Requests whose key is cached never wait on the network: a stale key set is refreshed in
/// the background while the cached keys keep answering. Only a request that needs keys the
/// cache does not have (none fetched yet, or an unknown `kid`) waits for a fetch, and fetches
/// happen one at a time.
pub struct Jwks {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::Client,
    /// Held only to read or update, never across a fetch.
    cache: std::sync::Mutex<Cache>,
    /// Held across a fetch, so there is one fetch at a time.
    fetch: tokio::sync::Mutex<()>,
    /// A background refresh is running.
    refreshing: AtomicBool,
}

impl Jwks {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            // The configured URL is the one fetched (validation allows only https there).
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Jwks {
            inner: Arc::new(Inner {
                client,
                cache: std::sync::Mutex::default(),
                fetch: tokio::sync::Mutex::new(()),
                refreshing: AtomicBool::new(false),
            }),
        })
    }

    /// Verifies `jwt` (the compact form, without `Bearer `) as an ID token of `cfg.issuer`
    /// for `cfg.client_id`, unexpired at `now` (unix seconds, 60 s leeway).
    pub async fn verify(
        &self,
        cfg: &IdentityConfig,
        jwt: &str,
        now: i64,
    ) -> Result<IdClaims, TokenError> {
        if jwt.len() > MAX_TOKEN_BYTES {
            return Err(TokenError::Invalid);
        }
        let header = jsonwebtoken::decode_header(jwt).map_err(|_| TokenError::Invalid)?;
        let kid = header.kid.ok_or(TokenError::Invalid)?;
        let key = self.key(cfg, &kid).await?;
        check_token(&key, cfg, jwt, now)
    }

    /// The key named `kid`, fetching the key set when needed.
    async fn key(&self, cfg: &IdentityConfig, kid: &str) -> Result<Key, TokenError> {
        let url = cfg.jwks_url();
        // Fast path: a cached key, refreshed in the background once stale.
        {
            let cache = self.inner.lock_cache();
            if let (Some(fetched_at), Some(key)) = (cache.fetched_at, cache.keys.get(kid)) {
                let key = key.clone();
                let due = fetched_at.elapsed() >= CACHE_TTL && cache.may_retry();
                drop(cache);
                if due {
                    self.refresh_in_background(url);
                }
                return Ok(key);
            }
        }

        // Slow path: no keys yet, or an unknown kid. One fetch at a time; look again once it
        // is our turn, since the fetch before ours may have brought the key.
        let _turn = self.inner.fetch.lock().await;
        let fetch = {
            let mut cache = self.inner.lock_cache();
            if let Some(key) = cache.keys.get(kid) {
                return Ok(key.clone());
            }
            if cache.fetched_at.is_none() {
                cache.may_retry()
            } else if cache
                .refetched_at
                .is_none_or(|t| t.elapsed() >= UNKNOWN_KID_REFETCH)
            {
                cache.refetched_at = Some(Instant::now());
                true
            } else {
                false
            }
        };
        if fetch {
            self.inner.refresh(&url).await;
        }
        let cache = self.inner.lock_cache();
        match cache.keys.get(kid) {
            Some(key) => Ok(key.clone()),
            None if cache.fetched_at.is_none() => Err(TokenError::Unavailable),
            None => Err(TokenError::Invalid),
        }
    }

    /// Starts a refresh of the key set unless one is already running.
    fn refresh_in_background(&self, url: String) {
        if self.inner.refreshing.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = self.inner.clone();
        tokio::spawn(async move {
            {
                let _turn = inner.fetch.lock().await;
                inner.refresh(&url).await;
            }
            inner.refreshing.store(false, Ordering::SeqCst);
        });
    }
}

impl Cache {
    /// Whether a fetch may be tried: none failed in the last [`RETRY_AFTER_FAILURE`].
    fn may_retry(&self) -> bool {
        self.failed_at
            .is_none_or(|t| t.elapsed() >= RETRY_AFTER_FAILURE)
    }
}

impl Inner {
    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Fetches the key set into the cache; on failure keeps what it had. The cache lock is
    /// not held while fetching.
    async fn refresh(&self, url: &str) {
        let fetched = self.fetch(url).await;
        let mut cache = self.lock_cache();
        match fetched {
            Ok(keys) => {
                if keys.is_empty() {
                    tracing::warn!("the JWKS has no usable RS256 or EdDSA key");
                }
                cache.keys = keys;
                cache.fetched_at = Some(Instant::now());
                cache.failed_at = None;
            }
            Err(e) => {
                tracing::warn!(error = %crate::logging::detail(format_args!("{e:#}")), "fetching the JWKS failed");
                cache.failed_at = Some(Instant::now());
            }
        }
    }

    async fn fetch(&self, url: &str) -> anyhow::Result<HashMap<String, Key>> {
        let mut res = self.client.get(url).send().await?;
        if !res.status().is_success() {
            anyhow::bail!("the JWKS answered {}", res.status());
        }
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await? {
            if body.len() + chunk.len() > MAX_JWKS_BYTES {
                anyhow::bail!("the JWKS is larger than {MAX_JWKS_BYTES} bytes");
            }
            body.extend_from_slice(&chunk);
        }
        parse_jwks(&body)
    }
}

/// The usable keys of a JWKS document, by `kid`. Keys without a `kid`, of another type or
/// algorithm, or that fail to parse are skipped (one odd key does not hide the others).
fn parse_jwks(body: &[u8]) -> anyhow::Result<HashMap<String, Key>> {
    #[derive(Deserialize)]
    struct Set {
        keys: Vec<serde_json::Value>,
    }
    let set: Set = serde_json::from_slice(body)?;
    let mut keys = HashMap::new();
    for value in set.keys {
        let Ok(jwk) = serde_json::from_value::<Jwk>(value) else {
            continue;
        };
        if jwk
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|u| *u != jsonwebtoken::jwk::PublicKeyUse::Signature)
        {
            continue;
        }
        let Some(kid) = jwk.common.key_id.clone() else {
            continue;
        };
        let Some(alg) = key_algorithm(&jwk) else {
            continue;
        };
        let Ok(key) = DecodingKey::from_jwk(&jwk) else {
            continue;
        };
        keys.insert(kid, Key { key, alg });
    }
    Ok(keys)
}

/// RS256 for an RSA key, EdDSA for an Ed25519 key; a stated `alg` must agree. Anything else
/// is not used.
fn key_algorithm(jwk: &Jwk) -> Option<Algorithm> {
    let alg = match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Algorithm::RS256,
        AlgorithmParameters::OctetKeyPair(p) if p.curve == EllipticCurve::Ed25519 => {
            Algorithm::EdDSA
        }
        _ => return None,
    };
    match jwk.common.key_algorithm {
        None => Some(alg),
        Some(KeyAlgorithm::RS256) if alg == Algorithm::RS256 => Some(alg),
        Some(KeyAlgorithm::EdDSA) if alg == Algorithm::EdDSA => Some(alg),
        Some(_) => None,
    }
}

/// The claims read from a verified token. `groups` and `amr` default to empty.
#[derive(Deserialize)]
struct Claims {
    sub: String,
    exp: i64,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    amr: Vec<String>,
    #[serde(default)]
    auth_time: Option<i64>,
}

/// Checks the signature with `key` (its algorithm only), `iss`, `aud`, and `exp` against
/// `now`, and that `sub` is 1-64 visible ASCII characters.
fn check_token(
    key: &Key,
    cfg: &IdentityConfig,
    jwt: &str,
    now: i64,
) -> Result<IdClaims, TokenError> {
    let mut validation = Validation::new(key.alg);
    validation.set_issuer(&[&cfg.issuer]);
    validation.set_audience(&[&cfg.client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    // `exp` is checked below against `now`, not the library's own clock reading.
    validation.validate_exp = false;
    let data = jsonwebtoken::decode::<Claims>(jwt, &key.key, &validation)
        .map_err(|_| TokenError::Invalid)?;
    let claims = data.claims;
    if claims.exp.saturating_add(EXP_LEEWAY_SECS) < now {
        return Err(TokenError::Invalid);
    }
    let sub_ok = (1..=MAX_SUB_CHARS).contains(&claims.sub.len())
        && claims.sub.bytes().all(|b| b.is_ascii_graphic());
    if !sub_ok {
        return Err(TokenError::Invalid);
    }
    Ok(IdClaims {
        sub: claims.sub,
        groups: claims.groups,
        amr: claims.amr,
        auth_time: claims.auth_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2048-bit RSA test key's JWKS entry and an RS256 ID token it signed (made once with
    /// openssl): `iss` https://auth.test, `aud` ["client-test", "p1"], `sub` "u1", `exp`
    /// 2000000000, `auth_time` 1999990000, groups ["autobracket-admins"], amr ["mfa"].
    const RSA_JWKS: &str = include_str!("../tests/fixtures/identity/rsa_jwks.json");
    const RSA_TOKEN: &str = include_str!("../tests/fixtures/identity/rsa_token.txt");

    fn cfg() -> IdentityConfig {
        let text = r#"
            public_origin = "https://bracket.test"
            issuer = "https://auth.test"
            client_id = "client-test"
            project_id = "p1"
            org_id = "o1"
            discord_idp_id = "idp1"
            proxy_secret = "test-proxy-secret-test-proxy-secret-00"
            sign_out_url = "/oauth2/sign_out"
            mfa_setup_url = "https://auth.test/mfa"
            [hook_keys]
            idp_intent = "k-idp"
            token = "k-token"
            user_events = "k-events"
        "#;
        toml::from_str(text).unwrap()
    }

    fn rsa_key() -> Key {
        let keys = parse_jwks(RSA_JWKS.as_bytes()).unwrap();
        keys.get("r1").expect("the RSA key is usable").clone()
    }

    #[test]
    fn rs256_token_verifies() {
        let claims = check_token(&rsa_key(), &cfg(), RSA_TOKEN.trim(), 1_999_999_000).unwrap();
        assert_eq!(
            claims,
            IdClaims {
                sub: "u1".into(),
                groups: vec!["autobracket-admins".into()],
                amr: vec!["mfa".into()],
                auth_time: Some(1_999_990_000),
            }
        );
    }

    #[test]
    fn rs256_token_checks_exp_iss_aud_and_signature() {
        let token = RSA_TOKEN.trim();
        // exp 2000000000 with 60 s leeway.
        assert!(check_token(&rsa_key(), &cfg(), token, 2_000_000_060).is_ok());
        assert_eq!(
            check_token(&rsa_key(), &cfg(), token, 2_000_000_061),
            Err(TokenError::Invalid)
        );
        let mut other = cfg();
        other.issuer = "https://auth.other".into();
        assert_eq!(
            check_token(&rsa_key(), &other, token, 1_999_999_000),
            Err(TokenError::Invalid)
        );
        let mut other = cfg();
        other.client_id = "p2".into();
        assert_eq!(
            check_token(&rsa_key(), &other, token, 1_999_999_000),
            Err(TokenError::Invalid)
        );
        // A flipped signature character.
        let mut bad = token.to_owned();
        let last = bad.pop().unwrap();
        bad.push(if last == 'A' { 'B' } else { 'A' });
        assert_eq!(
            check_token(&rsa_key(), &cfg(), &bad, 1_999_999_000),
            Err(TokenError::Invalid)
        );
    }

    /// The token's header cannot pick HS256 with the RSA public key as the secret, or `none`.
    #[test]
    fn algorithm_comes_from_the_key() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let payload = RSA_TOKEN.trim().split('.').nth(1).unwrap();
        for header in [
            r#"{"alg":"HS256","kid":"r1"}"#,
            r#"{"alg":"none","kid":"r1"}"#,
            r#"{"alg":"EdDSA","kid":"r1"}"#,
        ] {
            let token = format!("{}.{payload}.", b64.encode(header));
            assert_eq!(
                check_token(&rsa_key(), &cfg(), &token, 1_999_999_000),
                Err(TokenError::Invalid),
                "{header}"
            );
        }
    }

    #[test]
    fn jwks_skips_unusable_keys() {
        let doc = r#"{"keys":[
            {"kty":"oct","kid":"h","k":"c2VjcmV0"},
            {"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"},
            {"kty":"OKP","crv":"Ed25519","kid":"enc","use":"enc","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"},
            {"kty":"OKP","crv":"Ed25519","kid":"wrongalg","alg":"RS256","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"},
            {"kty":"weird","kid":"w"},
            {"kty":"OKP","crv":"Ed25519","kid":"ok","alg":"EdDSA","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}
        ]}"#;
        let keys = parse_jwks(doc.as_bytes()).unwrap();
        let mut kids: Vec<&String> = keys.keys().collect();
        kids.sort();
        assert_eq!(kids, ["ok"]);
        assert!(parse_jwks(b"not json").is_err());
    }

    // --- Fetching: a local JWKS server with one Ed25519 key, `kid "e1"` ---

    use std::sync::atomic::AtomicUsize;

    use base64::Engine;
    use ed25519_dalek::Signer;

    struct Idp {
        url: String,
        key: ed25519_dalek::SigningKey,
        fetches: Arc<AtomicUsize>,
        /// When set, `/keys` answers 500.
        down: Arc<AtomicBool>,
    }

    /// Serves `/keys` (the JWKS) and `/redirect` (302 to `/keys`).
    async fn idp() -> Idp {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let doc = serde_json::json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": "e1", "alg": "EdDSA",
            "x": b64.encode(key.verifying_key().to_bytes()),
        }]});
        let fetches = Arc::new(AtomicUsize::new(0));
        let down = Arc::new(AtomicBool::new(false));
        let (f, d) = (fetches.clone(), down.clone());
        let router = axum::Router::new()
            .route(
                "/keys",
                axum::routing::get(move || {
                    f.fetch_add(1, Ordering::SeqCst);
                    let res = if d.load(Ordering::SeqCst) {
                        Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    } else {
                        Ok(axum::Json(doc.clone()))
                    };
                    async move { res }
                }),
            )
            .route(
                "/redirect",
                axum::routing::get(|| async { axum::response::Redirect::to("/keys") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Idp {
            url: format!("http://{addr}"),
            key,
            fetches,
            down,
        }
    }

    fn ed_token(key: &ed25519_dalek::SigningKey, alg: &str, now: i64) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = serde_json::json!({"alg": alg, "kid": "e1"});
        let claims = serde_json::json!({
            "iss": "https://auth.test", "aud": "client-test", "sub": "u1", "exp": now + 600,
        });
        let input = format!(
            "{}.{}",
            b64.encode(header.to_string()),
            b64.encode(claims.to_string())
        );
        let sig = key.sign(input.as_bytes()).to_bytes();
        format!("{input}.{}", b64.encode(sig))
    }

    fn cfg_at(url: String) -> IdentityConfig {
        let mut cfg = cfg();
        cfg.jwks_url = Some(url);
        cfg
    }

    /// Pretends the key set was fetched two hours ago.
    fn age(jwks: &Jwks) {
        let mut cache = jwks.inner.cache.lock().unwrap();
        cache.fetched_at = Instant::now().checked_sub(Duration::from_secs(7200));
    }

    /// Once keys are cached, a stale set is refreshed in the background: the request is
    /// answered from the cache at once, also while the identity provider is down.
    #[tokio::test]
    async fn stale_keys_are_served_while_refreshing() {
        let idp = idp().await;
        let cfg = cfg_at(format!("{}/keys", idp.url));
        let jwks = Jwks::new().unwrap();
        let now = crate::db::now_unix();
        let token = ed_token(&idp.key, "EdDSA", now);
        assert!(jwks.verify(&cfg, &token, now).await.is_ok());
        assert_eq!(idp.fetches.load(Ordering::SeqCst), 1);

        age(&jwks);
        assert!(jwks.verify(&cfg, &token, now).await.is_ok());
        let deadline = Instant::now() + Duration::from_secs(5);
        while idp.fetches.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline, "no background refresh");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Down, and the refresh is slow to fail: the cached key still answers at once.
        idp.down.store(true, Ordering::SeqCst);
        // Let the background refresh finish so it is not still marked as running.
        tokio::time::sleep(Duration::from_millis(50)).await;
        age(&jwks);
        let started = Instant::now();
        assert!(jwks.verify(&cfg, &token, now).await.is_ok());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// Redirects are not followed: the configured URL is the one fetched.
    #[tokio::test]
    async fn redirects_are_not_followed() {
        let idp = idp().await;
        let now = crate::db::now_unix();
        let token = ed_token(&idp.key, "EdDSA", now);
        let jwks = Jwks::new().unwrap();
        let cfg = cfg_at(format!("{}/redirect", idp.url));
        assert_eq!(
            jwks.verify(&cfg, &token, now).await,
            Err(TokenError::Unavailable)
        );
        assert_eq!(idp.fetches.load(Ordering::SeqCst), 0);
    }

    /// HS256 keyed with the Ed25519 public key bytes is refused (algorithm confusion).
    #[tokio::test]
    async fn hs256_with_the_ed25519_key_is_refused() {
        use hmac::{Hmac, Mac};
        let idp = idp().await;
        let cfg = cfg_at(format!("{}/keys", idp.url));
        let jwks = Jwks::new().unwrap();
        let now = crate::db::now_unix();
        assert!(jwks
            .verify(&cfg, &ed_token(&idp.key, "EdDSA", now), now)
            .await
            .is_ok());
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let good = ed_token(&idp.key, "EdDSA", now);
        let payload = good.split('.').nth(1).unwrap();
        let input = format!("{}.{payload}", b64.encode(r#"{"alg":"HS256","kid":"e1"}"#));
        let mut mac =
            Hmac::<sha2::Sha256>::new_from_slice(&idp.key.verifying_key().to_bytes()).unwrap();
        mac.update(input.as_bytes());
        let forged = format!("{input}.{}", b64.encode(mac.finalize().into_bytes()));
        assert_eq!(
            jwks.verify(&cfg, &forged, now).await,
            Err(TokenError::Invalid)
        );
    }
}

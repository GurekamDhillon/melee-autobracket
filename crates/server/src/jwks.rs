//! The identity provider's signing keys (JWKS) and ID-token verification.
//!
//! Keys are fetched from `identity.jwks_url()` on first use and kept for an hour. A token
//! whose `kid` is unknown makes one re-fetch, at most once a minute (key rotation). Only RSA
//! (RS256) and Ed25519 (EdDSA) keys are used, each with its own algorithm alone, so a token
//! can never choose how it is checked (no `none`, no HMAC with a public key).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::Mutex;

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
pub struct Jwks {
    client: reqwest::Client,
    cache: Mutex<Cache>,
}

impl Jwks {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build()?;
        Ok(Jwks {
            client,
            cache: Mutex::default(),
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
        // Held across a fetch, so concurrent requests wait for one fetch instead of each
        // making their own.
        let mut cache = self.cache.lock().await;
        let stale = cache.fetched_at.is_none_or(|t| t.elapsed() >= CACHE_TTL);
        let may_retry = cache
            .failed_at
            .is_none_or(|t| t.elapsed() >= RETRY_AFTER_FAILURE);
        let mut fetched = false;
        if stale && may_retry {
            self.refresh(&mut cache, cfg).await;
            fetched = true;
        }
        if let Some(key) = cache.keys.get(kid) {
            return Ok(key.clone());
        }
        if cache.fetched_at.is_none() {
            return Err(TokenError::Unavailable);
        }
        let may_refetch = cache
            .refetched_at
            .is_none_or(|t| t.elapsed() >= UNKNOWN_KID_REFETCH);
        if !fetched && may_refetch {
            cache.refetched_at = Some(Instant::now());
            self.refresh(&mut cache, cfg).await;
            if let Some(key) = cache.keys.get(kid) {
                return Ok(key.clone());
            }
        }
        Err(TokenError::Invalid)
    }

    /// Fetches the key set into `cache`; on failure keeps what it had.
    async fn refresh(&self, cache: &mut Cache, cfg: &IdentityConfig) {
        match self.fetch(&cfg.jwks_url()).await {
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
        let mut res = self.client.get(url).send().await?.error_for_status()?;
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
}

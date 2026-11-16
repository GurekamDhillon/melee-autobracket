//! Request signing. Each request carries the headers `x-ab-key`, `x-ab-time`,
//! `x-ab-nonce` and `x-ab-sig`; the signature covers [`canonical`].

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Largest accepted difference, in seconds, between the signed time and the verifier's clock.
pub const MAX_SKEW_SECS: i64 = 120;

/// Header values: `key` is the 32-byte verifying key and `sig` the 64-byte
/// signature, `nonce` 16 random bytes, all lowercase hex; `time` is unix seconds in decimal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedHeaders {
    pub key: String,
    pub time: String,
    pub nonce: String,
    pub sig: String,
}

#[derive(thiserror::Error, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignError {
    #[error("malformed signature headers")]
    Malformed,
    #[error("bad signature")]
    BadSignature,
    #[error("request time outside the allowed clock skew")]
    Skew,
}

/// The signed message: `"{METHOD}\n{path_and_query}\n{time}\n{nonce}\n{sha256_hex(body)}"`.
pub fn canonical(
    method: &str,
    path_and_query: &str,
    time: &str,
    nonce: &str,
    body: &[u8],
) -> String {
    let body_hash = hex::encode(Sha256::digest(body));
    format!("{method}\n{path_and_query}\n{time}\n{nonce}\n{body_hash}")
}

pub fn sign_request(
    sk: &SigningKey,
    method: &str,
    path_and_query: &str,
    now_unix: i64,
    body: &[u8],
) -> SignedHeaders {
    let mut nonce = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let nonce = hex::encode(nonce);
    let time = now_unix.to_string();
    let sig = sk.sign(canonical(method, path_and_query, &time, &nonce, body).as_bytes());
    SignedHeaders {
        key: hex::encode(sk.verifying_key().to_bytes()),
        time,
        nonce,
        sig: hex::encode(sig.to_bytes()),
    }
}

/// The checks that need no body: every header is well formed (the key a valid Ed25519
/// point) and the signed time is within [`MAX_SKEW_SECS`] of `now_unix`. Returns the signer's
/// verifying key as lowercase hex. A server makes these checks before reading the body; the
/// signature itself is checked by [`verify_request`].
pub fn check_headers(h: &SignedHeaders, now_unix: i64) -> Result<String, SignError> {
    let (key, _) = parse_headers(h, now_unix)?;
    Ok(hex::encode(key.to_bytes()))
}

/// Checks the headers against the request and the caller's clock; on success
/// returns the signer's verifying key as lowercase hex.
pub fn verify_request(
    h: &SignedHeaders,
    method: &str,
    path_and_query: &str,
    now_unix: i64,
    body: &[u8],
) -> Result<String, SignError> {
    let (key, sig) = parse_headers(h, now_unix)?;
    let msg = canonical(method, path_and_query, &h.time, &h.nonce, body);
    key.verify_strict(msg.as_bytes(), &sig)
        .map_err(|_| SignError::BadSignature)?;
    Ok(hex::encode(key.to_bytes()))
}

fn parse_headers(h: &SignedHeaders, now_unix: i64) -> Result<(VerifyingKey, Signature), SignError> {
    let key_bytes: [u8; 32] = decode_fixed(&h.key)?;
    let sig_bytes: [u8; 64] = decode_fixed(&h.sig)?;
    let _: [u8; 16] = decode_fixed(&h.nonce)?;
    let time: i64 = h.time.parse().map_err(|_| SignError::Malformed)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| SignError::Malformed)?;
    if now_unix.abs_diff(time) > MAX_SKEW_SECS as u64 {
        return Err(SignError::Skew);
    }
    Ok((key, Signature::from_bytes(&sig_bytes)))
}

fn decode_fixed<const N: usize>(s: &str) -> Result<[u8; N], SignError> {
    let mut out = [0u8; N];
    hex::decode_to_slice(s, &mut out).map_err(|_| SignError::Malformed)?;
    Ok(out)
}

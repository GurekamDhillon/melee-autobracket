//! Shared protocol for the bracket server and CLI: JSON request/response
//! types and the Ed25519 request-signing scheme that stands in for accounts.

pub mod ruleset;
pub mod sign;
pub mod types;

pub use ed25519_dalek::{SigningKey, VerifyingKey};
pub use ruleset::Ruleset;
pub use sign::{canonical, sign_request, verify_request, SignError, SignedHeaders, MAX_SKEW_SECS};
pub use types::*;

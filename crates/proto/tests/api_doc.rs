//! `docs/api.md` agrees with the code: its worked signing example verifies, and every JSON
//! example marked `<!-- checked: Type -->` decodes into that `proto` type and encodes back to
//! exactly the same JSON (so it names every field, with the right shapes).
//!
//! `<!-- checked: Type; pending: a, b -->` leaves top-level fields `a` and `b` out of the
//! comparison, and `<!-- pending: Type -->` an example that does not decode yet: both are for
//! fields and variants added in the same release by another part of the server. Whoever
//! integrates the release drops the `pending` parts once they exist.

use ed25519_dalek::Signer;
use proto::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const DOC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api.md"));

/// Every `<!-- marker -->` line directly followed by a fenced block: `(marker, block)`.
fn marked_blocks() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut lines = DOC.lines();
    while let Some(line) = lines.next() {
        let Some(marker) = line
            .strip_prefix("<!-- ")
            .and_then(|l| l.strip_suffix(" -->"))
        else {
            continue;
        };
        let fence = lines.next().unwrap_or("");
        assert!(
            fence.starts_with("```"),
            "{marker:?} is not followed by a fenced block"
        );
        let block: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
        out.push((marker, block.join("\n")));
    }
    out
}

fn block(marker: &str) -> String {
    marked_blocks()
        .into_iter()
        .find(|(m, _)| *m == marker)
        .unwrap_or_else(|| panic!("no {marker:?} block in docs/api.md"))
        .1
}

#[test]
fn worked_signing_example_verifies() {
    let fields: Vec<(String, String)> = block("signing-example")
        .lines()
        .map(|l| {
            let (k, v) = l.split_once("  ").expect("`name  value` line");
            (k.trim().to_owned(), v.trim().to_owned())
        })
        .collect();
    let get = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("no {name:?} in the signing example"))
    };
    let secret: [u8; 32] = hex::decode(get("secret key")).unwrap().try_into().unwrap();
    let sk = SigningKey::from_bytes(&secret);
    let body = get("body");
    let time = get("time");
    let nonce = get("nonce");

    assert_eq!(
        get("body sha256"),
        hex::encode(Sha256::digest(body.as_bytes()))
    );
    let canon = canonical(&get("method"), &get("path"), &time, &nonce, body.as_bytes());
    assert_eq!(block("signing-canonical"), canon, "canonical string");
    assert_eq!(get("x-ab-key"), hex::encode(sk.verifying_key().to_bytes()));
    // Ed25519 is deterministic: the documented signature is the one this key makes.
    let sig = hex::encode(sk.sign(canon.as_bytes()).to_bytes());
    assert_eq!(get("x-ab-sig"), sig, "signature");

    let headers = SignedHeaders {
        key: get("x-ab-key"),
        time: time.clone(),
        nonce,
        sig,
    };
    let now: i64 = time.parse().unwrap();
    assert_eq!(
        verify_request(&headers, &get("method"), &get("path"), now, body.as_bytes()),
        Ok(get("x-ab-key"))
    );
    // The empty-body hash the document quotes.
    assert!(DOC.contains(&hex::encode(Sha256::digest(b""))));
}

/// Decodes `json` as `T`, encodes it back, and compares, leaving `pending` top-level fields out.
fn round_trip<T: DeserializeOwned + Serialize>(json: &str, pending: &[&str]) {
    let doc: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"));
    let decoded: T = serde_json::from_value(doc.clone()).unwrap_or_else(|e| panic!("{e}: {json}"));
    let mut encoded = serde_json::to_value(&decoded).unwrap();
    let mut doc = doc;
    for v in [&mut doc, &mut encoded] {
        if let Value::Object(map) = v {
            for field in pending {
                map.remove(*field);
            }
        }
    }
    assert_eq!(doc, encoded, "the example differs from the type's encoding");
}

#[test]
fn json_examples_match_the_types() {
    let mut checked = Vec::new();
    for (marker, json) in marked_blocks() {
        if let Some(ty) = marker.strip_prefix("pending: ") {
            // Added in the same release elsewhere: valid JSON is all that can be checked here.
            serde_json::from_str::<Value>(&json).unwrap_or_else(|e| panic!("{ty}: {e}"));
            continue;
        }
        let Some(spec) = marker.strip_prefix("checked: ") else {
            continue;
        };
        let (ty, pending) = match spec.split_once("; pending: ") {
            Some((ty, fields)) => (ty, fields.split(", ").collect::<Vec<_>>()),
            None => (spec, Vec::new()),
        };
        let p = pending.as_slice();
        match ty {
            "RegisterReq" => round_trip::<RegisterReq>(&json, p),
            "SignupReq" => round_trip::<SignupReq>(&json, p),
            "SlippiCodeReq" => round_trip::<SlippiCodeReq>(&json, p),
            "RoomReq" => round_trip::<RoomReq>(&json, p),
            "NoShowReq" => round_trip::<NoShowReq>(&json, p),
            "EventCreateReq" => round_trip::<EventCreateReq>(&json, p),
            "EventScheduleReq" => round_trip::<EventScheduleReq>(&json, p),
            "Ruleset" => round_trip::<Ruleset>(&json, p),
            "EventSummary" => round_trip::<EventSummary>(&json, p),
            "EventView" => round_trip::<EventView>(&json, p),
            "StreamInfo" => round_trip::<StreamInfo>(&json, p),
            "Next" => round_trip::<Next>(&json, p),
            "Verdict" => round_trip::<Verdict>(&json, p),
            "ResimOutcome" => round_trip::<ResimOutcome>(&json, p),
            "Resolve" => round_trip::<Resolve>(&json, p),
            "QueueItem" => round_trip::<QueueItem>(&json, p),
            "QuarantineItem" => round_trip::<QuarantineItem>(&json, p),
            "EventResult" => round_trip::<EventResult>(&json, p),
            "PlayerSummary" => round_trip::<PlayerSummary>(&json, p),
            "PlayerProfile" => round_trip::<PlayerProfile>(&json, p),
            "RekeyReq" => round_trip::<RekeyReq>(&json, p),
            "VerifyJob" => round_trip::<VerifyJob>(&json, p),
            "VerifyResult" => round_trip::<VerifyResult>(&json, p),
            other => panic!("docs/api.md checks unknown type {other:?}"),
        }
        checked.push(ty);
    }
    // Every request and response body type has an example.
    for ty in [
        "RegisterReq",
        "SignupReq",
        "SlippiCodeReq",
        "RoomReq",
        "NoShowReq",
        "EventCreateReq",
        "EventScheduleReq",
        "Ruleset",
        "EventSummary",
        "EventView",
        "StreamInfo",
        "Next",
        "Verdict",
        "ResimOutcome",
        "Resolve",
        "QueueItem",
        "QuarantineItem",
        "EventResult",
        "PlayerSummary",
        "PlayerProfile",
        "RekeyReq",
        "VerifyJob",
        "VerifyResult",
    ] {
        assert!(checked.contains(&ty), "no checked example of {ty}");
    }
}

/// Every variant of the enums a client must handle has an example.
#[test]
fn every_variant_is_shown() {
    let examples: Vec<String> = marked_blocks()
        .into_iter()
        .filter(|(m, _)| m.starts_with("checked: Next") || m.starts_with("checked: Verdict"))
        .map(|(_, json)| json)
        .collect();
    let all = examples.join("\n");
    for variant in ["Play", "Review", "Waiting"] {
        assert!(all.contains(variant), "Next::{variant} example");
    }
    // The rest are listed in the variant tables.
    for name in [
        "\"NotEntered\"",
        "\"Eliminated\"",
        "\"Champion\"",
        "\"VerifiedInexact\"",
        "\"PeerOnly\"",
        "\"Conflicted\"",
        "\"Single\"",
        "\"Undecided\"",
        "\"Inexact\"",
        "\"no-show\"",
        "\"client-failure\"",
    ] {
        assert!(DOC.contains(name), "{name} is documented");
    }
}

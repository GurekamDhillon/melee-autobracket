use proto::*;

const NOW: i64 = 1_763_200_000;
const BODY: &[u8] = br#"{"tag":"mango"}"#;

fn key() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

#[test]
fn round_trip() {
    let sk = key();
    let h = sign_request(&sk, "POST", "/events/1/signup?x=1", NOW, BODY);
    let got = verify_request(&h, "POST", "/events/1/signup?x=1", NOW, BODY);
    assert_eq!(got, Ok(hex::encode(sk.verifying_key().to_bytes())));
    assert_eq!(h.key, hex::encode(sk.verifying_key().to_bytes()));
    assert_eq!(h.time, NOW.to_string());
    assert_eq!(h.nonce.len(), 32);
    assert_eq!(h.sig.len(), 128);
}

#[test]
fn tamper() {
    let sk = key();
    let h = sign_request(&sk, "POST", "/events/1/signup", NOW, BODY);
    assert_eq!(
        verify_request(&h, "POST", "/events/1/signup", NOW, b"{}"),
        Err(SignError::BadSignature)
    );
    assert_eq!(
        verify_request(&h, "POST", "/events/2/signup", NOW, BODY),
        Err(SignError::BadSignature)
    );
    assert_eq!(
        verify_request(&h, "PUT", "/events/1/signup", NOW, BODY),
        Err(SignError::BadSignature)
    );
}

#[test]
fn skew() {
    let sk = key();
    let h = sign_request(&sk, "GET", "/events", NOW, b"");
    assert_eq!(
        verify_request(&h, "GET", "/events", NOW + 121, b""),
        Err(SignError::Skew)
    );
    assert!(verify_request(&h, "GET", "/events", NOW + 120, b"").is_ok());
}

#[test]
fn malformed() {
    let sk = key();
    let h = sign_request(&sk, "GET", "/events", NOW, b"");

    let mut bad_key = h.clone();
    bad_key.key = "zz".repeat(32);
    assert_eq!(
        verify_request(&bad_key, "GET", "/events", NOW, b""),
        Err(SignError::Malformed)
    );

    let mut short_sig = h.clone();
    short_sig.sig.truncate(126);
    assert_eq!(
        verify_request(&short_sig, "GET", "/events", NOW, b""),
        Err(SignError::Malformed)
    );
}

#[test]
fn ruleset_default() {
    assert_eq!(Ruleset::default().legal_stages, vec![2, 3, 8, 28, 31, 32]);
}

#[test]
fn verdict_counts() {
    for v in [Verdict::Verified, Verdict::VerifiedInexact, Verdict::PeerOnly] {
        assert!(v.counts(), "{v:?} should count");
    }
    for v in [
        Verdict::Conflicted,
        Verdict::Single,
        Verdict::Illegal("banned stage".into()),
        Verdict::Undecided,
        Verdict::Inexact,
    ] {
        assert!(!v.counts(), "{v:?} should not count");
    }
}

#[test]
fn serde_round_trip() {
    let next = Next::Play {
        set_id: 7,
        opponent_tag: "mango".into(),
        role: Role::Host,
        relay: "relay.example:7000".into(),
        room_code: Some("ABCD".into()),
        game_no: 2,
        wins: [1, 0],
        best_of: 3,
        stocks: 4,
        timer_secs: 480,
        awaiting_verdict: true,
    };
    let s = serde_json::to_string(&next).unwrap();
    assert_eq!(serde_json::from_str::<Next>(&s).unwrap(), next);

    let verdict = Verdict::Illegal("banned character".into());
    let s = serde_json::to_string(&verdict).unwrap();
    assert_eq!(serde_json::from_str::<Verdict>(&s).unwrap(), verdict);

    let outcome = ResimOutcome::Diverged {
        frame: 1234,
        port: 1,
        field: "pos_x".into(),
        engine_value: "1.5".into(),
        replay_value: "1.25".into(),
    };
    let s = serde_json::to_string(&outcome).unwrap();
    assert_eq!(serde_json::from_str::<ResimOutcome>(&s).unwrap(), outcome);
}

#[test]
fn ruleset_partial() {
    let r: Ruleset = serde_json::from_str(r#"{"stocks":3}"#).unwrap();
    assert_eq!(r, Ruleset { stocks: 3, ..Ruleset::default() });
}

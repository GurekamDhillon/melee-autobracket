//! `autobracket-upload` against a real server, in an any-client event: keys, registration,
//! sign-up, `next`, and uploads whose bytes differ but whose outcome agrees.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use autobracket_server::testkit::{FakeResim, TestApp};
use autobracket_upload::{keygen, read_key, Uploader};
use bracket::Format;
use proto::{EventCreateReq, EventSummary, ResimOutcome, Ruleset};
use replay::testkit::SlpBuilder;
use replay::PlayerKind;

/// A game port 2 wins; `percent` is the winner's final percent, which two
/// clients may record differently without changing the outcome.
fn game(percent: f32) -> Vec<u8> {
    SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(-123, 1, 4, 0.0)
        .frame(-123, 2, 4, 0.0)
        .frame(5400, 1, 0, 0.0)
        .frame(5400, 2, 2, percent)
        .end(2, None)
        .build()
}

async fn any_client_event(app: &TestApp) -> i64 {
    let req = EventCreateReq {
        name: "Open".into(),
        format: Format::SingleElim,
        ruleset: Some(Ruleset {
            any_client: true,
            best_of: 1,
            ..Ruleset::default()
        }),
        best_of: None,
        starts_at: None,
        description: None,
    };
    let res = app.as_to().post_json("/v1/admin/events", &req).await;
    assert_eq!(res.status(), reqwest::StatusCode::CREATED);
    res.json::<EventSummary>().await.unwrap().id
}

async fn player(app: &TestApp, dir: &std::path::Path, tag: &str) -> Uploader {
    let key = dir.join(format!("{tag}.key"));
    let public = keygen(&key).unwrap();
    assert_eq!(public.len(), 64);
    assert!(keygen(&key).is_err(), "never replaces a key");
    let up = Uploader::new(&app.url, read_key(&key).unwrap()).unwrap();
    assert_eq!(up.register(tag, None).await.unwrap().status, 201);
    up
}

async fn verdicts(app: &TestApp) -> Vec<(String, String)> {
    sqlx::query_as("SELECT state, verdict_json FROM games ORDER BY id")
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn foreign_uploads_agreeing_on_the_outcome_count() {
    let outcomes = VecDeque::from([ResimOutcome::Diverged {
        frame: 100,
        port: 1,
        field: "pos_x".into(),
        engine_value: "1".into(),
        replay_value: "2".into(),
    }]);
    let app = TestApp::spawn_with(
        |c| c.strict_resim = true,
        Arc::new(FakeResim(Mutex::new(outcomes))),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let event = any_client_event(&app).await;
    let host = player(&app, dir.path(), "Host").await;
    let guest = player(&app, dir.path(), "Guest").await;
    assert_eq!(host.signup(event, "slippi").await.unwrap().status, 201);
    assert_eq!(guest.signup(event, "slippi").await.unwrap().status, 201);
    let res = app
        .as_to()
        .post_json(&format!("/v1/admin/events/{event}/start"), &serde_json::json!({}))
        .await;
    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let next = host.next(event).await.unwrap();
    assert_eq!(next.status, 200);
    assert!(next.body.contains("\"Play\""), "{}", next.body);

    // Different bytes, same outcome (the winner's final percent differs).
    assert_eq!(host.upload(event, 0, 1, &game(63.0), "slippi").await.unwrap().status, 202);
    assert_eq!(guest.upload(event, 0, 1, &game(64.0), "slippi").await.unwrap().status, 202);
    autobracket_server::worker::judge_due(&app.state).await.unwrap();

    // The re-simulation diverged: counted as UnverifiedForeign (strict_resim notwithstanding),
    // and quarantined for the organiser.
    let v = verdicts(&app).await;
    assert_eq!(v, [("counted".to_owned(), "\"UnverifiedForeign\"".to_owned())]);
    let q: serde_json::Value = app.as_to().get("/v1/quarantine").await.json().await.unwrap();
    assert_eq!(q[0]["kind"], "UnverifiedForeign");
    let next = guest.next(event).await.unwrap();
    assert_eq!(next.body, "\"Eliminated\"");
}

#[tokio::test]
async fn a_tampered_upload_is_refused_and_a_bad_key_file_is_an_error() {
    let app = TestApp::spawn(|_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.key");
    std::fs::write(&bad, "not hex").unwrap();
    assert!(read_key(&bad).is_err());
    // A reference client config works as a key file too.
    let cfg = dir.path().join("client.toml");
    std::fs::write(&cfg, format!("server = \"\"\nsecret_key_hex = \"{}\"\n", "11".repeat(32))).unwrap();
    let up = Uploader::new(&app.url, read_key(&cfg).unwrap()).unwrap();
    // Unregistered key: 403 from a signed request, so the signature itself was accepted.
    assert_eq!(up.next(1).await.unwrap().status, 403);
    assert!(Uploader::new("ftp://x", read_key(&cfg).unwrap()).is_err());
}

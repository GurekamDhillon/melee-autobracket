//! The client's calls: timeouts, and what a refusal tells the player.

mod common;

use std::time::{Duration, Instant};

use autobracket_client::api::{insecure_server_warning, is_retryable, Api, Timeouts};
use common::Stub;
use proto::SigningKey;
use tokio::net::TcpListener;

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

/// A server that accepts connections and never answers.
async fn silent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((conn, _)) = listener.accept().await {
            held.push(conn);
        }
    });
    url
}

#[tokio::test]
async fn a_request_that_gets_no_answer_times_out_and_is_retryable() {
    let url = silent().await;
    let api = Api::with_timeouts(
        &url,
        key(),
        "b1",
        Timeouts {
            connect: Duration::from_secs(1),
            request: Duration::from_millis(300),
            upload: Duration::from_secs(60),
        },
    );

    let start = Instant::now();
    let err = api.next(1).await.unwrap_err();

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert!(is_retryable(&err), "{err:#}");
}

#[tokio::test]
async fn an_upload_has_its_own_timeout() {
    let url = silent().await;
    let api = Api::with_timeouts(
        &url,
        key(),
        "b1",
        Timeouts {
            connect: Duration::from_secs(1),
            request: Duration::from_secs(60),
            upload: Duration::from_millis(300),
        },
    );

    let start = Instant::now();
    let err = api.upload_replay(1, 0, 1, vec![0; 16]).await.unwrap_err();

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert!(is_retryable(&err), "{err:#}");
}

#[test]
fn the_default_timeouts_are_as_documented() {
    let t = Timeouts::default();
    assert_eq!(t.connect, Duration::from_secs(10));
    assert_eq!(t.request, Duration::from_secs(30));
    assert_eq!(t.upload, Duration::from_secs(120));
}

#[tokio::test]
async fn a_clock_skew_refusal_says_the_clock_is_wrong() {
    let stub = Stub::start(|_| {
        (
            401,
            r#"{"error":"clock skew: your clock differs from the server's by more than 120 seconds"}"#
                .into(),
        )
    })
    .await;

    let err = Api::new(&stub.url, key(), "b1").next(1).await.unwrap_err();

    let msg = format!("{err:#}");
    assert!(msg.contains("CLOCK IS WRONG"), "{msg}");
    assert!(msg.contains("120 seconds"), "{msg}");
    assert!(!is_retryable(&err));
}

#[tokio::test]
async fn a_refused_build_lists_the_builds_the_server_accepts() {
    let stub = Stub::start(|_| {
        (
            403,
            r#"{"error":"engine build not accepted","accepted":["melee 1.0","melee 1.1"]}"#.into(),
        )
    })
    .await;

    let err = Api::new(&stub.url, key(), "melee 0.9+abc")
        .signup(1)
        .await
        .unwrap_err();

    let msg = format!("{err:#}");
    assert!(msg.contains("engine build not accepted"), "{msg}");
    assert!(msg.contains(r#""melee 1.0", "melee 1.1""#), "{msg}");
    assert!(msg.contains("melee 0.9+abc"), "{msg}");
    assert!(msg.contains("403"), "{msg}");
}

#[tokio::test]
async fn sign_up_and_uploads_carry_the_build_and_a_no_show_its_kind() {
    let stub = Stub::start(|_| (204, String::new())).await;
    let api = Api::new(&stub.url, key(), "melee 1.0+0123456789ab");

    api.signup(1).await.unwrap();
    api.upload_replay(1, 0, 1, b"slp".to_vec()).await.unwrap();
    api.report_no_show(
        1,
        0,
        Some("note"),
        autobracket_client::api::NoShowKind::ClientFailure,
    )
    .await
    .unwrap();

    let signup = &stub.requests("POST", "/entrants")[0];
    assert_eq!(signup.json()["engine_build"], "melee 1.0+0123456789ab");
    let upload = &stub.requests("POST", "/replay")[0];
    assert_eq!(upload.header("x-ab-build"), Some("melee 1.0+0123456789ab"));
    let report = &stub.requests("POST", "/no-show")[0];
    assert_eq!(
        report.json(),
        serde_json::json!({"detail": "note", "kind": "client-failure"})
    );
}

#[test]
fn plain_http_to_another_machine_is_warned_about() {
    for remote in ["http://bracket.example:8080", "http://192.0.2.1:8080"] {
        assert!(insecure_server_warning(remote).is_some(), "{remote}");
    }
    for fine in [
        "https://bracket.example",
        "http://127.0.0.1:8080",
        "http://localhost:8080",
        "http://[::1]:8080",
    ] {
        assert_eq!(insecure_server_warning(fine), None, "{fine}");
    }
}

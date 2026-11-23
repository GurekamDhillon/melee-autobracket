//! The live relay (docs/design/live-relay.md): signed ingest, checks before the upgrade, the
//! handshake and resume, gaps, replacement of an older connection, and viewer fan-out.

use std::time::Duration;

use autobracket_server::testkit::{TestApp, TestPlayer};
use base64::Engine as _;
use bracket::Format;
use futures_util::{SinkExt, StreamExt};
use proto::sign_request;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn ws_url(app: &TestApp, path: &str) -> String {
    format!("{}{path}", app.url.replacen("http://", "ws://", 1))
}

/// Opens the ingest for `path` signed by `p`; the HTTP status on a refusal.
async fn ingest(app: &TestApp, p: &TestPlayer, path: &str) -> Result<Ws, u16> {
    let mut req = ws_url(app, path).into_client_request().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let h = sign_request(&p.sk, "GET", path, now, b"");
    let hd = req.headers_mut();
    hd.insert("x-ab-key", h.key.parse().unwrap());
    hd.insert("x-ab-time", h.time.parse().unwrap());
    hd.insert("x-ab-nonce", h.nonce.parse().unwrap());
    hd.insert("x-ab-sig", h.sig.parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("connect: {e}"),
    }
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a message in time")
            .expect("open")
            .expect("ok");
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

fn event_msg(cursor: u64, bytes: &[u8]) -> Value {
    json!({"type": "game_event", "cursor": cursor, "next_cursor": cursor + 1,
           "payload": base64::engine::general_purpose::STANDARD.encode(bytes)})
}

/// Waits until the listing shows `n` messages stored for set 0 (the ingest has no acks).
async fn wait_messages(app: &TestApp, event: i64, n: u64) {
    for _ in 0..200 {
        let list: Value = reqwest::get(format!("{}/v1/live?event={event}", app.url))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if list[0]["messages"] == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the ingest never stored {n} messages");
}

async fn setup() -> (TestApp, TestPlayer, TestPlayer, i64) {
    let app = TestApp::spawn(|_| {}).await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app.started_event(Format::SingleElim, &[&host, &guest]).await;
    (app, host, guest, event)
}

#[tokio::test]
async fn refusals_before_upgrade() {
    let (app, host, guest, event) = setup().await;
    let path = format!("/v1/sets/{event}/0/games/1/live");
    assert_eq!(ingest(&app, &guest, &path).await.err(), Some(403));
    assert_eq!(
        ingest(&app, &host, &format!("/v1/sets/{event}/9/games/1/live")).await.err(),
        Some(404)
    );
    assert_eq!(
        ingest(&app, &host, &format!("/v1/sets/{event}/0/games/2/live")).await.err(),
        Some(409)
    );
    // Unsigned.
    let r = tokio_tungstenite::connect_async(ws_url(&app, &path)).await;
    assert!(matches!(r, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 401));
    // Viewing a set nobody streams.
    let r = tokio_tungstenite::connect_async(ws_url(&app, &format!("/v1/live/{event}/0"))).await;
    assert!(matches!(r, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 404));
}

#[tokio::test]
async fn stream_resume_gap_and_fan_out() {
    let (app, host, _guest, event) = setup().await;
    let path = format!("/v1/sets/{event}/0/games/1/live");
    let mut a = ingest(&app, &host, &path).await.unwrap();
    send(&mut a, json!({"type": "connect_request", "cursor": 0, "version": 1})).await;
    assert_eq!(recv(&mut a).await, json!({"type": "connect_reply", "cursor": 0}));
    send(&mut a, json!({"type": "start_game", "cursor": 0, "next_cursor": 1})).await;
    send(&mut a, event_msg(1, &[0x35, 0x04, 0x3A, 0x00, 0x0C])).await;
    send(&mut a, event_msg(2, &[0x3A, 1, 2, 3])).await;
    send(&mut a, json!({"type": "keepalive"})).await;
    wait_messages(&app, event, 3).await;

    // A reconnect replaces the first connection and resumes at the server's cursor.
    let mut b = ingest(&app, &host, &path).await.unwrap();
    send(&mut b, json!({"type": "connect_request", "cursor": 2, "version": 1})).await;
    assert_eq!(recv(&mut b).await, json!({"type": "connect_reply", "cursor": 3}));
    // The old connection's late message is discarded (and closes it).
    send(&mut a, event_msg(3, &[0x3A, 9, 9, 9])).await;

    // A viewer joins: the head and the recent messages, then live ones.
    let (mut v, _) = tokio_tungstenite::connect_async(ws_url(&app, &format!("/v1/live/{event}/0")))
        .await
        .unwrap();
    let got: Vec<Value> = vec![recv(&mut v).await, recv(&mut v).await, recv(&mut v).await];
    assert_eq!(got[0]["type"], "start_game");
    assert_eq!(got[1]["cursor"], 1);
    assert_eq!(got[2]["cursor"], 2);
    assert_eq!(got[2]["game_no"], 1);

    // A skip ahead (back-pressure) is a recorded gap, not an error.
    send(&mut b, event_msg(10, &[0x3A, 4, 5, 6])).await;
    let m = recv(&mut v).await;
    assert_eq!(m["cursor"], 10);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(m["payload"].as_str().unwrap())
            .unwrap(),
        vec![0x3A, 4, 5, 6]
    );
    send(&mut b, json!({"type": "end_game", "cursor": 11, "next_cursor": 12})).await;
    assert_eq!(recv(&mut v).await["type"], "end_game");

    let list: Value = reqwest::get(format!("{}/v1/live?event={event}", app.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list[0]["set_id"], 0);
    assert_eq!(list[0]["ended"], true);
    assert_eq!(list[0]["gaps"], 1);
    assert_eq!(list[0]["messages"], 5);
    assert_eq!(list[0]["viewers"], 1);
}

#[tokio::test]
async fn relay_off() {
    let app = TestApp::spawn(|c| c.live_relay = false).await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app.started_event(Format::SingleElim, &[&host, &guest]).await;
    let path = format!("/v1/sets/{event}/0/games/1/live");
    assert_eq!(ingest(&app, &host, &path).await.err(), Some(404));
}

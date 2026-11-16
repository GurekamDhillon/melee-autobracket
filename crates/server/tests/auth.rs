use std::time::{Duration, SystemTime, UNIX_EPOCH};

use autobracket_server::auth::{Player, To};
use autobracket_server::testkit::TestApp;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use proto::{sign_request, RegisterReq, SigningKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower::ServiceExt;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn new_key() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

fn reg(tag: &str, invite: Option<&str>) -> Vec<u8> {
    serde_json::to_vec(&RegisterReq {
        tag: tag.into(),
        invite: invite.map(Into::into),
    })
    .unwrap()
}

/// A signed `POST /v1/players` built by hand, so a test can pick the time and resend it verbatim.
fn signed_register(
    app: &TestApp,
    sk: &SigningKey,
    time: i64,
    body: &[u8],
) -> reqwest::RequestBuilder {
    let h = sign_request(sk, "POST", "/v1/players", time, body);
    reqwest::Client::new()
        .post(format!("{}/v1/players", app.url))
        .header("content-type", "application/json")
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .body(body.to_vec())
}

async fn player_count(app: &TestApp, tag: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM players WHERE tag = ?")
        .bind(tag)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn register_ok() {
    let app = TestApp::spawn(|_| {}).await;
    let sk = new_key();
    let res = signed_register(&app, &sk, now(), &reg("Mango", None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let (key, tag): (String, String) = sqlx::query_as("SELECT key, tag FROM players")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(key, hex::encode(sk.verifying_key().to_bytes()));
    assert_eq!(tag, "Mango");
}

#[tokio::test]
async fn unsigned_is_401() {
    let app = TestApp::spawn(|_| {}).await;
    let res = reqwest::Client::new()
        .post(format!("{}/v1/players", app.url))
        .header("content-type", "application/json")
        .body(reg("Mango", None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(player_count(&app, "Mango").await, 0);
}

#[tokio::test]
async fn replayed_nonce_is_401() {
    let app = TestApp::spawn(|_| {}).await;
    let sk = new_key();
    let req = signed_register(&app, &sk, now(), &reg("Mango", None));
    let again = req.try_clone().unwrap();
    assert_eq!(req.send().await.unwrap().status(), StatusCode::CREATED);
    assert_eq!(
        again.send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn stale_time_is_401() {
    let app = TestApp::spawn(|_| {}).await;
    let res = signed_register(&app, &new_key(), now() - 121, &reg("Mango", None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(player_count(&app, "Mango").await, 0);
}

#[tokio::test]
async fn bad_tag_400_dup_tag_409() {
    let app = TestApp::spawn(|_| {}).await;
    for bad in [
        "a",
        "seventeen_chars_x",
        "has space",
        "bad-dash",
        "émoji",
        "",
    ] {
        let res = signed_register(&app, &new_key(), now(), &reg(bad, None))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "tag {bad:?}");
    }
    for good in ["ab", "sixteen_chars_xx", "Hbox_99"] {
        let res = signed_register(&app, &new_key(), now(), &reg(good, None))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED, "tag {good:?}");
    }

    // The same tag under a new key, in any case.
    for taken in ["Hbox_99", "HBOX_99", "hbox_99"] {
        let res = signed_register(&app, &new_key(), now(), &reg(taken, None))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT, "{taken}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body, serde_json::json!({"error": "tag is taken"}));
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM players WHERE tag LIKE 'hbox_99'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // The same key under a new tag.
    let sk = new_key();
    let res = signed_register(&app, &sk, now(), &reg("Armada", None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = signed_register(&app, &sk, now(), &reg("Armada2", None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(player_count(&app, "Armada2").await, 0);
}

#[tokio::test]
async fn invite_required() {
    let app = TestApp::spawn(|c| c.require_invite = true).await;
    sqlx::query("INSERT INTO invites (code) VALUES ('good-code')")
        .execute(&app.state.db)
        .await
        .unwrap();

    let none = signed_register(&app, &new_key(), now(), &reg("Mango", None))
        .send()
        .await
        .unwrap();
    assert_eq!(none.status(), StatusCode::FORBIDDEN);

    let unknown = signed_register(&app, &new_key(), now(), &reg("Mango", Some("nope")))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::FORBIDDEN);

    let sk = new_key();
    let valid = signed_register(&app, &sk, now(), &reg("Mango", Some("good-code")))
        .send()
        .await
        .unwrap();
    assert_eq!(valid.status(), StatusCode::CREATED);
    let used_by: Option<String> =
        sqlx::query_scalar("SELECT used_by FROM invites WHERE code = 'good-code'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(used_by, Some(hex::encode(sk.verifying_key().to_bytes())));

    let reused = signed_register(&app, &new_key(), now(), &reg("Armada", Some("good-code")))
        .send()
        .await
        .unwrap();
    assert_eq!(reused.status(), StatusCode::FORBIDDEN);
    assert_eq!(player_count(&app, "Armada").await, 0);
}

/// Every route but replay upload takes at most 64 KiB, whatever `max_replay_bytes` is.
#[tokio::test]
async fn oversized_body_is_413() {
    let app = TestApp::spawn(|_| {}).await;
    let to = app.as_to();
    let res = to
        .post_bytes("/v1/players", vec![b'x'; 64 * 1024 + 1], &[])
        .await;
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"error": "request body too large"}));
    // At the limit the body is read (and then refused as a registration body).
    let res = to
        .post_bytes("/v1/players", vec![b'x'; 64 * 1024], &[])
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rate_limited() {
    let app = TestApp::spawn(|c| {
        c.rate_per_sec = 20;
        c.rate_burst = 40;
    })
    .await;
    let client = reqwest::Client::new();
    let mut statuses = Vec::new();
    for _ in 0..200 {
        let res = client
            .get(format!("{}/v1/events", app.url))
            .send()
            .await
            .unwrap();
        statuses.push(res.status());
    }
    assert_eq!(statuses[0], StatusCode::OK);
    assert!(statuses.contains(&StatusCode::TOO_MANY_REQUESTS));
}

#[tokio::test]
async fn testkit_player_registers_and_signs() {
    let app = TestApp::spawn(|_| {}).await;
    let p = app.player("Zain").await;
    assert_eq!(p.tag, "Zain");
    assert_eq!(player_count(&app, "Zain").await, 1);
    assert_eq!(p.get("/v1/events").await.status(), StatusCode::OK);
}

/// `Player` needs a registered key, `To` a key listed in `cfg.to_keys`.
#[tokio::test]
async fn extractors_enforce_roles() {
    let app = TestApp::spawn(|_| {}).await;
    let router = Router::new()
        .route(
            "/p",
            post(|p: Player| async move { format!("{}:{}", p.tag, p.body.len()) }),
        )
        .route("/t", post(|t: To| async move { t.body.len().to_string() }))
        .with_state(app.state.clone());
    let call = |sk: SigningKey, path: &'static str| {
        let router = router.clone();
        async move {
            let h = sign_request(&sk, "POST", path, now(), b"abc");
            let req = Request::post(path)
                .header("x-ab-key", h.key)
                .header("x-ab-time", h.time)
                .header("x-ab-nonce", h.nonce)
                .header("x-ab-sig", h.sig)
                .body(Body::from("abc"))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let status = res.status();
            let body = axum::body::to_bytes(res.into_body(), 1 << 16)
                .await
                .unwrap();
            (status, String::from_utf8(body.to_vec()).unwrap())
        }
    };

    let registered = app.player("Mango").await;
    assert_eq!(
        call(registered.sk.clone(), "/p").await,
        (StatusCode::OK, "Mango:3".into())
    );
    assert_eq!(
        call(new_key(), "/p").await,
        (
            StatusCode::FORBIDDEN,
            r#"{"error":"key is not registered"}"#.into()
        )
    );

    assert_eq!(
        call(app.to.clone(), "/t").await,
        (StatusCode::OK, "3".into())
    );
    assert_eq!(
        call(registered.sk.clone(), "/t").await,
        (
            StatusCode::FORBIDDEN,
            r#"{"error":"not a tournament organiser"}"#.into()
        )
    );

    // A signature over a different body is rejected.
    let h = sign_request(&app.to, "POST", "/t", now(), b"abc");
    let req = Request::post("/t")
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .body(Body::from("abd"))
        .unwrap();
    assert_eq!(
        router.oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

/// Concurrent read-then-write registrations must not trip over SQLite's write lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_registrations_all_succeed() {
    let app = TestApp::spawn(|_| {}).await;
    let sends = (0..20)
        .map(|i| signed_register(&app, &new_key(), now(), &reg(&format!("p{i}"), None)).send());
    let results = spawn_all(sends).await;
    for res in results {
        assert_eq!(res.unwrap().status(), StatusCode::CREATED);
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM players")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 20);
}

async fn spawn_all<F: std::future::Future + Send + 'static>(
    futs: impl Iterator<Item = F>,
) -> Vec<F::Output>
where
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

/// Under `Router::nest` the signature still covers the full path the client sent.
#[tokio::test]
async fn signature_covers_full_path_when_nested() {
    let app = TestApp::spawn(|_| {}).await;
    let inner = Router::new().route("/t", post(|t: To| async move { t.body.len().to_string() }));
    let router = Router::new()
        .nest("/v1/nested", inner)
        .with_state(app.state.clone());
    let h = sign_request(&app.to, "POST", "/v1/nested/t?x=1", now(), b"abc");
    let req = Request::post("/v1/nested/t?x=1")
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .body(Body::from("abc"))
        .unwrap();
    assert_eq!(router.oneshot(req).await.unwrap().status(), StatusCode::OK);
}

/// Extractor rejections carry the same `{"error": ...}` JSON body as handler errors.
#[tokio::test]
async fn rejections_are_json() {
    let app = TestApp::spawn(|_| {}).await;
    let res = reqwest::Client::new()
        .post(format!("{}/v1/admin/events", app.url))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()["content-type"],
        "application/json",
        "content type"
    );
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(
        body,
        serde_json::json!({"error": "missing signature headers"})
    );
}

/// The server's `host:port`.
fn addr(app: &TestApp) -> String {
    app.url.trim_start_matches("http://").to_owned()
}

/// Sends `head` (request line and headers, ending in a blank line) on a fresh connection and
/// never sends a body. Returns what the server answers within `wait` (empty if nothing).
async fn send_head_only(app: &TestApp, head: &str, wait: Duration) -> String {
    let mut stream = TcpStream::connect(addr(app)).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    let _ = tokio::time::timeout(wait, async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    // Complete once the JSON error body is in.
                    if got.ends_with(b"}") {
                        break;
                    }
                }
            }
        }
    })
    .await;
    String::from_utf8_lossy(&got).into_owned()
}

/// A signed request head for `method path` declaring a 10 MB body that never comes.
fn head_with_huge_body(sk: &SigningKey, method: &str, path: &str, time: i64) -> String {
    let h = sign_request(sk, method, path, time, b"never sent");
    format!(
        "{method} {path} HTTP/1.1\r\nhost: test\r\ncontent-type: application/json\r\n\
         content-length: 10000000\r\nx-ab-key: {}\r\nx-ab-time: {}\r\nx-ab-nonce: {}\r\n\
         x-ab-sig: {}\r\n\r\n",
        h.key, h.time, h.nonce, h.sig
    )
}

/// Well-formed headers from the wrong key, or at the wrong time, are refused before a byte of
/// the body is read: the answer comes while the client is still "sending" 10 MB.
#[tokio::test]
async fn refused_before_reading_the_body() {
    let app = TestApp::spawn(|_| {}).await;
    let player = app.player("Mango").await;
    let event = app.create_event(bracket::Format::SingleElim).await;
    let wait = Duration::from_secs(5);

    // Player route, unregistered key.
    let path = format!("/v1/events/{event}/entrants");
    let res = send_head_only(
        &app,
        &head_with_huge_body(&new_key(), "POST", &path, now()),
        wait,
    )
    .await;
    assert!(res.starts_with("HTTP/1.1 403"), "{res:?}");
    assert!(
        res.contains(r#"{"error":"key is not registered"}"#),
        "{res:?}"
    );

    // Organiser route, a registered player's key.
    let res = send_head_only(
        &app,
        &head_with_huge_body(&player.sk, "POST", "/v1/admin/events", now()),
        wait,
    )
    .await;
    assert!(res.starts_with("HTTP/1.1 403"), "{res:?}");
    assert!(res.contains("not a tournament organiser"), "{res:?}");

    // Verifier route, a player's key.
    let res = send_head_only(
        &app,
        &head_with_huge_body(&player.sk, "POST", "/v1/verify/claim", now()),
        wait,
    )
    .await;
    assert!(res.starts_with("HTTP/1.1 403"), "{res:?}");

    // Any route, a time outside the skew window (either side).
    for time in [now() - 500, now() + 500] {
        let res = send_head_only(
            &app,
            &head_with_huge_body(&app.to, "POST", "/v1/players", time),
            wait,
        )
        .await;
        assert!(res.starts_with("HTTP/1.1 401"), "{res:?}");
        assert!(res.contains("clock skew"), "{res:?}");
    }

    // Malformed headers.
    let head = "POST /v1/players HTTP/1.1\r\nhost: test\r\ncontent-length: 10000000\r\n\
                x-ab-key: zz\r\nx-ab-time: 1\r\nx-ab-nonce: 00\r\nx-ab-sig: 00\r\n\r\n";
    let res = send_head_only(&app, head, wait).await;
    assert!(res.starts_with("HTTP/1.1 401"), "{res:?}");

    // A declared body over the limit is refused without reading it, once the caller is known.
    let res = send_head_only(
        &app,
        &head_with_huge_body(&app.to, "POST", "/v1/admin/events", now()),
        wait,
    )
    .await;
    assert!(res.starts_with("HTTP/1.1 413"), "{res:?}");

    // No nonce was recorded for any of them.
    let nonces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nonces WHERE key <> ?")
        .bind(hex::encode(player.sk.verifying_key().to_bytes()))
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    // The organiser's own requests (event creation) are the only others.
    assert_eq!(nonces, 1);
}

/// A clock that is off is named as such, in both directions.
#[tokio::test]
async fn skewed_time_is_401_naming_the_clock() {
    let app = TestApp::spawn(|_| {}).await;
    for time in [now() - 130, now() + 130] {
        let res = signed_register(&app, &new_key(), time, &reg("Mango", None))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(
            body,
            serde_json::json!({"error": "clock skew: your clock differs from the server's by more than 120 seconds"})
        );
    }
    assert_eq!(player_count(&app, "Mango").await, 0);
}

/// The signature covers the query string.
#[tokio::test]
async fn altered_query_is_401() {
    let app = TestApp::spawn(|_| {}).await;
    let p = app.player("Mango").await;
    let h = sign_request(&p.sk, "GET", "/v1/me/next?event=1", now(), b"");
    let res = reqwest::Client::new()
        .get(format!("{}/v1/me/next?event=2", app.url))
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"error": "invalid signature"}));
}

/// Nonces older than the replay window are forgotten; younger ones are kept.
#[tokio::test]
async fn expired_nonces_are_forgotten() {
    let app = TestApp::spawn(|_| {}).await;
    for (nonce, age) in [("old", 301), ("young", 200)] {
        sqlx::query("INSERT INTO nonces (key, nonce, seen_at) VALUES ('k', ?, ?)")
            .bind(nonce)
            .bind(now() - age)
            .execute(&app.state.db)
            .await
            .unwrap();
    }
    let res = signed_register(&app, &new_key(), now(), &reg("Mango", None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let left: Vec<String> =
        sqlx::query_scalar("SELECT nonce FROM nonces WHERE key = 'k' ORDER BY nonce")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(left, vec!["young".to_string()]);
}

/// A body that stops arriving is given up on after 30 seconds.
#[tokio::test]
async fn slow_body_times_out() {
    let app = TestApp::spawn(|_| {}).await;
    let body = reg("Mango", None);
    let h = sign_request(&app.to, "POST", "/v1/players", now(), &body);
    let head = format!(
        "POST /v1/players HTTP/1.1\r\nhost: test\r\ncontent-length: {}\r\nx-ab-key: {}\r\n\
         x-ab-time: {}\r\nx-ab-nonce: {}\r\nx-ab-sig: {}\r\n\r\n{}",
        body.len(),
        h.key,
        h.time,
        h.nonce,
        h.sig,
        // Half the body, then nothing.
        String::from_utf8_lossy(&body[..body.len() / 2])
    );
    let started = std::time::Instant::now();
    let res = send_head_only(&app, &head, Duration::from_secs(40)).await;
    assert!(res.starts_with("HTTP/1.1 408"), "{res:?}");
    assert!(res.contains(r#"{"error":"#), "{res:?}");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(29) && took < Duration::from_secs(35),
        "{took:?}"
    );
}

/// A connection that sends nothing is closed after 30 seconds.
#[tokio::test]
async fn idle_connection_is_closed() {
    let app = TestApp::spawn(|_| {}).await;
    let mut stream = TcpStream::connect(addr(&app)).await.unwrap();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(40), stream.read(&mut buf)).await;
    assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "{read:?}");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(29) && took < Duration::from_secs(35),
        "{took:?}"
    );
}

/// At most `max_connections` connections are served at once.
#[tokio::test]
async fn connection_limit() {
    let app = TestApp::spawn(|c| c.max_connections = 1).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let held = TcpStream::connect(addr(&app)).await.unwrap();
    // Let the server accept the held connection first.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client
        .get(format!("{}/v1/events", app.url))
        .send()
        .await
        .is_err());
    drop(held);
    let res = client.get(format!("{}/v1/events", app.url)).send().await;
    assert_eq!(res.unwrap().status(), StatusCode::OK);
}

/// Every error is `{"error": ...}` JSON: unknown routes, wrong methods, bad paths and queries.
#[tokio::test]
async fn every_error_is_json() {
    let app = TestApp::spawn(|_| {}).await;
    let p = app.player("Mango").await;
    let client = reqwest::Client::new();
    let mut cases = vec![
        (
            client
                .get(format!("{}/nope", app.url))
                .send()
                .await
                .unwrap(),
            StatusCode::NOT_FOUND,
        ),
        (
            client
                .delete(format!("{}/v1/events", app.url))
                .send()
                .await
                .unwrap(),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            client
                .get(format!("{}/v1/events/abc", app.url))
                .send()
                .await
                .unwrap(),
            StatusCode::BAD_REQUEST,
        ),
        (
            p.get("/v1/me/next?event=abc").await,
            StatusCode::BAD_REQUEST,
        ),
        (
            p.post_bytes("/v1/sets/x/0/room", b"{}".to_vec(), &[]).await,
            StatusCode::BAD_REQUEST,
        ),
        (
            app.as_to()
                .post_bytes("/v1/admin/queue/abc/resolve", b"{}".to_vec(), &[])
                .await,
            StatusCode::BAD_REQUEST,
        ),
        (
            p.post_bytes("/v1/events/abc/entrants", b"{}".to_vec(), &[])
                .await,
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (res, status) in cases.drain(..) {
        assert_eq!(res.status(), status, "{}", res.url());
        let ctype = res.headers()["content-type"].to_str().unwrap().to_owned();
        assert_eq!(ctype, "application/json", "{}", res.url());
        let body: serde_json::Value = res.json().await.unwrap();
        assert!(body["error"].is_string(), "{body}");
    }
}

/// Behind a trusted proxy each forwarded client has its own bucket; from anyone else
/// `X-Forwarded-For` is ignored, so a client cannot spoof its way out of its bucket.
#[tokio::test]
async fn rate_limit_keys_on_the_forwarded_client_only_behind_a_trusted_proxy() {
    let limited = |trusted: bool| {
        move |c: &mut autobracket_server::Config| {
            c.rate_per_sec = 1;
            c.rate_burst = 3;
            if trusted {
                c.trusted_proxies = vec!["127.0.0.1/32".into()];
            }
        }
    };
    async fn statuses(app: &TestApp, xff: &str, n: usize) -> Vec<StatusCode> {
        let client = reqwest::Client::new();
        let mut out = Vec::new();
        for _ in 0..n {
            let res = client
                .get(format!("{}/v1/events", app.url))
                .header("x-forwarded-for", xff)
                .send()
                .await
                .unwrap();
            out.push(res.status());
        }
        out
    }
    let ok3 = vec![StatusCode::OK; 3];

    let app = TestApp::spawn(limited(true)).await;
    let first = statuses(&app, "203.0.113.1", 4).await;
    assert_eq!(first[..3], ok3[..]);
    assert_eq!(first[3], StatusCode::TOO_MANY_REQUESTS);
    // Another forwarded client is not limited by the first.
    assert_eq!(statuses(&app, "203.0.113.2", 3).await, ok3);
    // A client's own claim to the left of the proxy's entry changes nothing.
    assert_eq!(
        statuses(&app, "198.51.100.1, 203.0.113.1", 1).await,
        vec![StatusCode::TOO_MANY_REQUESTS]
    );
    // IPv6 clients in one /64 share a bucket; the next /64 does not.
    assert_eq!(statuses(&app, "2001:db8::1", 3).await, ok3);
    assert_eq!(
        statuses(&app, "2001:db8::2", 1).await,
        vec![StatusCode::TOO_MANY_REQUESTS]
    );
    assert_eq!(
        statuses(&app, "2001:db8:0:1::1", 1).await,
        vec![StatusCode::OK]
    );

    // Not trusted: the header is ignored and everyone here is one peer.
    let app = TestApp::spawn(limited(false)).await;
    assert_eq!(statuses(&app, "203.0.113.1", 3).await, ok3);
    let spoofed = reqwest::Client::new()
        .get(format!("{}/v1/events", app.url))
        .header("x-forwarded-for", "203.0.113.2")
        .send()
        .await
        .unwrap();
    assert_eq!(spoofed.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(spoofed.headers()["content-type"], "application/json");
    let body: serde_json::Value = spoofed.json().await.unwrap();
    assert!(body["error"].is_string(), "{body}");
}

/// A replay upload may take as long to arrive as the client allows it (120 s), so a slow
/// uplink is not refused with 408 on every retry.
#[test]
fn an_upload_body_may_take_as_long_as_the_client_allows() {
    assert_eq!(
        autobracket_server::auth::UPLOAD_BODY_TIMEOUT,
        Duration::from_secs(120)
    );
}

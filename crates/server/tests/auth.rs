use std::time::{SystemTime, UNIX_EPOCH};

use autobracket_server::auth::{Player, To};
use autobracket_server::testkit::TestApp;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use proto::{sign_request, RegisterReq, SigningKey};
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

    // The same tag under a new key.
    let res = signed_register(&app, &new_key(), now(), &reg("Hbox_99", None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

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

#[tokio::test]
async fn oversized_body_is_413() {
    let app = TestApp::spawn(|c| c.max_replay_bytes = 1024).await;
    let to = app.as_to();
    let res = to.post_bytes("/v1/players", vec![b'x'; 2048], &[]).await;
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
    assert_eq!(call(new_key(), "/p").await.0, StatusCode::FORBIDDEN);

    assert_eq!(
        call(app.to.clone(), "/t").await,
        (StatusCode::OK, "3".into())
    );
    assert_eq!(
        call(registered.sk.clone(), "/t").await.0,
        StatusCode::FORBIDDEN
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

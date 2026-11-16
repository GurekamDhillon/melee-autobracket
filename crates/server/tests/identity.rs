//! Web identity: the `WebUser` / `AdminUser` extractors behind `/account/api/ping` and
//! `/admin/api/ping`, ID-token verification against the test JWKS, CSRF, tombstones, and
//! linked client keys signing as their player.

use autobracket_server::auth::PlayerHead;
use autobracket_server::identity::sha256_hex;
use autobracket_server::testkit::{TestApp, TestWeb, TokenMode};
use autobracket_server::AppState;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::{Json, Router};
use bracket::Format;
use proto::{sign_request, Next, RegisterReq, SigningKey};
use rand::rngs::OsRng;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

const PING: &str = "/account/api/ping";
const ADMIN_PING: &str = "/admin/api/ping";

/// Asserts the status and the `error` field of a response; returns the JSON body (or `Null`).
async fn expect(res: reqwest::Response, status: StatusCode, error: Option<&str>) -> Value {
    let got = res.status();
    let body = res.text().await.unwrap();
    assert_eq!(got, status, "body: {body}");
    let json: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&body).unwrap()
    };
    if let Some(error) = error {
        assert_eq!(json["error"], error, "body: {body}");
    }
    json
}

async fn accounts(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

fn with_token(mut who: TestWeb, token: TokenMode) -> TestWeb {
    who.token = token;
    who
}

/// Review Focus 1: every forged or incomplete identity is anonymous (401), never a 500, and
/// never creates an account; a forged groups header gives no admin rights.
#[tokio::test]
async fn forged_identity_is_refused() {
    // A peer outside `trusted_proxies`, with an otherwise perfect request.
    let untrusted =
        TestApp::spawn_identity(|cfg| cfg.trusted_proxies = vec!["10.0.0.1/32".into()]).await;
    let res = untrusted.web(&TestWeb::admin("u1")).get(PING).await;
    expect(res, StatusCode::UNAUTHORIZED, Some("not signed in")).await;
    let res = untrusted.web(&TestWeb::admin("u1")).get(ADMIN_PING).await;
    expect(res, StatusCode::UNAUTHORIZED, Some("not signed in")).await;
    // Forwarding headers naming the trusted proxy change nothing: only the TCP peer counts.
    let res = untrusted
        .web(&TestWeb::admin("u1"))
        .request(Method::GET, PING, &[])
        .header("x-forwarded-for", "10.0.0.1")
        .header("x-real-ip", "10.0.0.1")
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::UNAUTHORIZED, Some("not signed in")).await;
    assert_eq!(accounts(&untrusted).await, 0);

    let app = TestApp::spawn_identity(|_| {}).await;
    let mut cases = Vec::new();
    for secret in ["wrong", ""] {
        let mut who = TestWeb::admin("u1");
        who.proxy_secret = Some(secret.into());
        cases.push((format!("proxy secret {secret:?}"), who));
    }
    // The right secret with something appended, and a prefix of it.
    for secret in [
        "test-proxy-secret-test-proxy-secret-000",
        "test-proxy-secret-test-proxy-secret-0",
    ] {
        let mut who = TestWeb::admin("u1");
        who.proxy_secret = Some(secret.into());
        cases.push((format!("proxy secret {secret:?}"), who));
    }
    for mode in [
        TokenMode::Unsigned,
        TokenMode::WrongKey,
        TokenMode::WrongIss,
        TokenMode::WrongAud,
        TokenMode::Expired,
        TokenMode::Absent,
    ] {
        cases.push((
            format!("token {mode:?}"),
            with_token(TestWeb::admin("u1"), mode),
        ));
    }
    let mut who = TestWeb::admin("u1");
    who.header_sub = Some("u2".into());
    cases.push(("header sub differs".into(), who));

    for (what, who) in &cases {
        for path in [PING, ADMIN_PING] {
            let res = app.web(who).get(path).await;
            assert_ne!(res.status(), StatusCode::INTERNAL_SERVER_ERROR, "{what}");
            let got = res.status();
            let body: Value = res.json().await.unwrap();
            assert_eq!(got, StatusCode::UNAUTHORIZED, "{what} {path}: {body}");
            assert_eq!(body["error"], "not signed in", "{what} {path}");
        }
        let res = app
            .web(who)
            .request(Method::POST, PING, &[])
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{what} POST");
    }

    // Garbage in the token and the identity headers.
    let web = app.web(&TestWeb::player("u1"));
    for token in [
        "Bearer ",
        "Bearer a.b.c",
        "Bearer ....",
        "bearer x",
        "Basic dXNlcjpwdw==",
    ] {
        let res = web
            .request(Method::GET, PING, &["x-id-token"])
            .header("x-id-token", token)
            .send()
            .await
            .unwrap();
        expect(res, StatusCode::UNAUTHORIZED, Some("not signed in")).await;
    }
    let res = web
        .request(Method::GET, PING, &["x-auth-request-user"])
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::UNAUTHORIZED, Some("not signed in")).await;
    let res = web
        .request(Method::GET, PING, &["x-auth-request-user"])
        .header("x-auth-request-user", vec![0xffu8, 0xfe])
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::UNAUTHORIZED, Some("not signed in")).await;

    assert_eq!(accounts(&app).await, 0, "no 401 creates an account");

    // A player's valid identity plus a forged groups header is still not an organiser.
    let res = web
        .request(Method::GET, ADMIN_PING, &[])
        .header("x-auth-request-groups", "autobracket-admins")
        .header("x-forwarded-groups", "autobracket-admins")
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::FORBIDDEN, Some("not an organiser")).await;
    // And that player can use the account routes.
    expect(web.get(PING).await, StatusCode::NO_CONTENT, None).await;
}

#[tokio::test]
async fn identity_off_is_404() {
    let app = TestApp::spawn(|_| {}).await;
    let res = reqwest::get(format!("{}{PING}", app.url)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = reqwest::get(format!("{}{ADMIN_PING}", app.url))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Review Focus 2: only the exact group counts; MFA and sign-in age problems are a 403 that
/// says where to set up a second factor.
#[tokio::test]
async fn admin_requires_exact_group_recent_mfa() {
    let app = TestApp::spawn_identity(|_| {}).await;
    for group in [
        "autobracket-admins-old",
        "AUTOBRACKET-ADMINS",
        " autobracket-admins",
    ] {
        let mut who = TestWeb::admin("u1");
        who.groups = vec![group.into()];
        let res = app.web(&who).get(ADMIN_PING).await;
        expect(res, StatusCode::FORBIDDEN, Some("not an organiser")).await;
    }

    let mfa_error = "admin pages need a recent sign-in with a second factor";
    let check_mfa_403 = |body: Value| {
        assert_eq!(body["error"], mfa_error);
        assert_eq!(body["mfa_setup_url"], "https://auth.test/mfa");
        assert_eq!(body["sign_in_url"], "/oauth2/sign_in?rd=/admin/");
    };
    let mut no_mfa = TestWeb::admin("u1");
    no_mfa.amr = vec!["user".into()];
    check_mfa_403(
        expect(
            app.web(&no_mfa).get(ADMIN_PING).await,
            StatusCode::FORBIDDEN,
            None,
        )
        .await,
    );

    // A sign-in time in the future (beyond clock skew) is not a recent sign-in.
    let mut future = TestWeb::admin("u1");
    future.auth_time_ago = -3600;
    check_mfa_403(
        expect(
            app.web(&future).get(ADMIN_PING).await,
            StatusCode::FORBIDDEN,
            None,
        )
        .await,
    );

    let mut old = TestWeb::admin("u1");
    old.auth_time_ago = 43_201;
    check_mfa_403(
        expect(
            app.web(&old).get(ADMIN_PING).await,
            StatusCode::FORBIDDEN,
            None,
        )
        .await,
    );

    let admin = TestWeb::admin("u1");
    let mut claims = app.id_claims(&admin);
    claims.as_object_mut().unwrap().remove("auth_time");
    let token = app.sign_id_token("t1", &claims);
    let res = app
        .web(&admin)
        .request(Method::GET, ADMIN_PING, &["x-id-token"])
        .header("x-id-token", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    check_mfa_403(expect(res, StatusCode::FORBIDDEN, None).await);

    // Just inside the window, and the real thing.
    let mut recent = TestWeb::admin("u1");
    recent.auth_time_ago = 43_100;
    expect(
        app.web(&recent).get(ADMIN_PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    expect(
        app.web(&admin).get(ADMIN_PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    // An admin is a signed-in user too; the MFA problem does not stop account routes.
    expect(
        app.web(&no_mfa).get(PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
}

#[tokio::test]
async fn unknown_kid_refetches() {
    let app = TestApp::spawn_identity(|_| {}).await;
    let player = TestWeb::player("u1");
    expect(
        app.web(&player).get(PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    assert_eq!(app.jwks_fetches(), 1);
    // Cached: no second fetch.
    expect(
        app.web(&player).get(PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    assert_eq!(app.jwks_fetches(), 1);

    app.rotate_jwks();
    let rotated = with_token(TestWeb::player("u1"), TokenMode::Kid2);
    expect(
        app.web(&rotated).get(PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    assert_eq!(app.jwks_fetches(), 2, "the unknown kid re-fetched the keys");

    // Another unknown kid within a minute: refused without fetching again.
    let token = app.sign_id_token("t3", &app.id_claims(&player));
    let res = app
        .web(&player)
        .request(Method::GET, PING, &["x-id-token"])
        .header("x-id-token", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::UNAUTHORIZED, Some("not signed in")).await;
    assert_eq!(app.jwks_fetches(), 2);
    // Both known keys still work.
    expect(
        app.web(&player).get(PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    expect(
        app.web(&rotated).get(PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    assert_eq!(app.jwks_fetches(), 2);
}

#[tokio::test]
async fn jwks_down_is_503() {
    let app = TestApp::spawn_identity(|_| {}).await;
    app.stop_jwks().await;
    let res = app.web(&TestWeb::player("u1")).get(PING).await;
    expect(
        res,
        StatusCode::SERVICE_UNAVAILABLE,
        Some("sign-in service unavailable"),
    )
    .await;
    assert_eq!(accounts(&app).await, 0);
}

/// `last_seen_at` is written when the account is created and then at most every 5 minutes.
#[tokio::test]
async fn last_seen_is_refreshed_only_when_stale() {
    let app = TestApp::spawn_identity(|_| {}).await;
    let web = app.web(&TestWeb::player("u1"));
    expect(web.get(PING).await, StatusCode::NO_CONTENT, None).await;
    let last_seen = |app: &TestApp| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT last_seen_at FROM accounts WHERE sub = 'u1'")
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    let now = autobracket_server::db::now_unix();
    assert!(last_seen(&app).await >= now - 5);

    // Recent: left alone.
    let recent = now - 60;
    sqlx::query("UPDATE accounts SET last_seen_at = ? WHERE sub = 'u1'")
        .bind(recent)
        .execute(&app.state.db)
        .await
        .unwrap();
    expect(web.get(PING).await, StatusCode::NO_CONTENT, None).await;
    assert_eq!(last_seen(&app).await, recent);

    // Stale: refreshed.
    sqlx::query("UPDATE accounts SET last_seen_at = ? WHERE sub = 'u1'")
        .bind(now - 301)
        .execute(&app.state.db)
        .await
        .unwrap();
    expect(web.get(PING).await, StatusCode::NO_CONTENT, None).await;
    assert!(last_seen(&app).await >= now - 5);
    assert_eq!(accounts(&app).await, 1);
}

/// Many concurrent signed-in requests do not exhaust the database pool: each request holds
/// at most one connection at a time.
#[tokio::test]
async fn concurrent_pings_do_not_exhaust_the_pool() {
    let app = TestApp::spawn_identity(|_| {}).await;
    let web = std::sync::Arc::new(app.web(&TestWeb::player("u1")));
    expect(web.get(PING).await, StatusCode::NO_CONTENT, None).await;
    let handles: Vec<_> = (0..32)
        .map(|_| {
            let web = web.clone();
            tokio::spawn(async move { web.get(PING).await.status() })
        })
        .collect();
    let all = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.unwrap());
        }
        out
    })
    .await
    .expect("32 concurrent pings finish within 10 s");
    assert!(all.iter().all(|s| *s == StatusCode::NO_CONTENT), "{all:?}");
}

#[tokio::test]
async fn tombstoned_sub_is_refused() {
    let app = TestApp::spawn_identity(|_| {}).await;
    sqlx::query("INSERT INTO account_tombstones (sub_sha256, removed_at) VALUES (?, 1)")
        .bind(sha256_hex("u9"))
        .execute(&app.state.db)
        .await
        .unwrap();
    for path in [PING, ADMIN_PING] {
        let res = app.web(&TestWeb::admin("u9")).get(path).await;
        expect(
            res,
            StatusCode::UNAUTHORIZED,
            Some("this account was deleted"),
        )
        .await;
    }
    assert_eq!(
        accounts(&app).await,
        0,
        "a removed user is never re-created"
    );

    // First sight creates the account (no Discord id); a disabled account is refused.
    expect(
        app.web(&TestWeb::admin("u8")).get(PING).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    let rows: Vec<(String, Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT sub, discord_id, disabled_at FROM accounts")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(rows, vec![("u8".to_string(), None, None)]);
    sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE sub = 'u8'")
        .execute(&app.state.db)
        .await
        .unwrap();
    for path in [PING, ADMIN_PING] {
        let res = app.web(&TestWeb::admin("u8")).get(path).await;
        expect(res, StatusCode::FORBIDDEN, Some("this account is disabled")).await;
    }
    assert_eq!(accounts(&app).await, 1);
}

#[tokio::test]
async fn csrf_is_required_on_writes() {
    let app = TestApp::spawn_identity(|_| {}).await;
    let web = app.web(&TestWeb::player("u1"));
    let refused = "cross-site request refused";
    let res = web
        .request(Method::POST, PING, &["x-ab-csrf"])
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::FORBIDDEN, Some(refused)).await;
    let res = web
        .request(Method::POST, PING, &["x-ab-csrf"])
        .header("x-ab-csrf", "0")
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::FORBIDDEN, Some(refused)).await;
    for origin in [
        Some("https://evil.test"),
        Some("https://bracket.test.evil.test"),
        Some("http://bracket.test"),
        Some("null"),
        None,
    ] {
        let mut req = web.request(Method::POST, PING, &["origin"]);
        if let Some(origin) = origin {
            req = req.header("origin", origin);
        }
        let res = req.send().await.unwrap();
        expect(res, StatusCode::FORBIDDEN, Some(refused)).await;
    }
    assert_eq!(accounts(&app).await, 0, "a refused write creates nothing");
    expect(
        web.post_json(PING, &serde_json::json!({})).await,
        StatusCode::NO_CONTENT,
        None,
    )
    .await;
    // Reads need neither.
    let res = web
        .request(Method::GET, PING, &["x-ab-csrf", "origin"])
        .send()
        .await
        .unwrap();
    expect(res, StatusCode::NO_CONTENT, None).await;
}

/// A key in `player_keys` signs as the player it points to: handlers see the canonical key,
/// and the nonce is recorded under the signing key.
#[tokio::test]
async fn alias_key_signs_as_player() {
    let app = TestApp::spawn(|_| {}).await;
    let alice = app.player("Alice").await;
    let k1 = hex::encode(alice.sk.verifying_key().to_bytes());
    let event = app.create_event(Format::SingleElim).await;

    let alias = SigningKey::generate(&mut OsRng);
    let k2 = hex::encode(alias.verifying_key().to_bytes());
    sqlx::query(
        "INSERT INTO player_keys (key, player_key, label, linked_at) VALUES (?, ?, NULL, 1)",
    )
    .bind(&k2)
    .bind(&k1)
    .execute(&app.state.db)
    .await
    .unwrap();
    let client = app.client(alias.clone(), "Alice");
    let res = client.get(&format!("/v1/me/next?event={event}")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.json::<Next>().await.unwrap(), Next::NotEntered);
    let nonces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nonces WHERE key = ?")
        .bind(&k2)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(nonces, 1, "the nonce is recorded under the signer");

    // Signing up through the alias enters the canonical player.
    client.sign_up(event).await;
    let entrant: String = sqlx::query_scalar("SELECT player_key FROM entrants WHERE event_id = ?")
        .bind(event)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(entrant, k1);
    let res = client.get(&format!("/v1/me/next?event={event}")).await;
    assert_eq!(res.json::<Next>().await.unwrap(), Next::Waiting);

    // The alias cannot register as a player of its own.
    let res = client
        .post_json(
            "/v1/players",
            &RegisterReq {
                tag: "Other".into(),
                invite: None,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // `PlayerHead` (headers only, empty body) resolves the alias too.
    let router: Router<AppState> = Router::new().route(
        "/head",
        get(|head: PlayerHead| async move { Json((head.key, head.signer, head.tag)) }),
    );
    let router = router.with_state(app.state.clone());
    let h = sign_request(
        &alias,
        "GET",
        "/head",
        autobracket_server::db::now_unix(),
        b"",
    );
    let req = Request::get("/head")
        .header("x-ab-key", &h.key)
        .header("x-ab-time", &h.time)
        .header("x-ab-nonce", &h.nonce)
        .header("x-ab-sig", &h.sig)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let (key, signer, tag): (String, String, String) = serde_json::from_slice(&body).unwrap();
    assert_eq!((key, signer, tag), (k1.clone(), k2.clone(), "Alice".into()));
    // The same nonce again is a replay; an unknown key is 403.
    let req = Request::get("/head")
        .header("x-ab-key", &h.key)
        .header("x-ab-time", &h.time)
        .header("x-ab-nonce", &h.nonce)
        .header("x-ab-sig", &h.sig)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let stranger = SigningKey::generate(&mut OsRng);
    let h = sign_request(
        &stranger,
        "GET",
        "/head",
        autobracket_server::db::now_unix(),
        b"",
    );
    let req = Request::get("/head")
        .header("x-ab-key", &h.key)
        .header("x-ab-time", &h.time)
        .header("x-ab-nonce", &h.nonce)
        .header("x-ab-sig", &h.sig)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

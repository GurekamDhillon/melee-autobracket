//! Test harness: a server on a random local port backed by a temporary
//! directory, and signing HTTP clients for organisers and players.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use bracket::Format;
use proto::{
    sign_request, EventCreateReq, EventSummary, RegisterReq, ResimOutcome, SigningKey, SignupReq,
};
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::task::JoinHandle;

use crate::config::{HookKeys, IdentityConfig};
use crate::db::now_unix;
use crate::{build_app, AppState, Config, NoResim, Resim};

pub struct TestApp {
    /// Base URL, e.g. `http://127.0.0.1:41234`.
    pub url: String,
    /// The organiser key; its public half is in `cfg.to_keys`.
    pub to: SigningKey,
    pub state: AppState,
    /// Holds the database and replay directory.
    pub dir: TempDir,
    /// The fake identity provider, for [`TestApp::spawn_identity`].
    identity: Option<IdentityKit>,
}

impl TestApp {
    /// Spawns with [`NoResim`]. Rate limits are raised out of reach, the no-show grace period
    /// after a room code is off, and the background worker is off (tests judge with
    /// [`crate::worker::judge_due`]) unless `edit` changes them.
    pub async fn spawn(edit: impl FnOnce(&mut Config)) -> Self {
        Self::spawn_with(edit, Arc::new(NoResim)).await
    }

    pub async fn spawn_with(edit: impl FnOnce(&mut Config), resim: Arc<dyn Resim>) -> Self {
        let dir = TempDir::new().expect("create temp dir");
        let to = SigningKey::generate(&mut OsRng);
        let mut cfg = Config::new(vec![hex::encode(to.verifying_key().to_bytes())]);
        cfg.bind = "127.0.0.1:0".into();
        cfg.db_path = dir.path().join("autobracket.sqlite");
        cfg.replay_dir = dir.path().join("replays");
        cfg.rate_per_sec = 1_000_000;
        cfg.rate_burst = 1_000_000;
        cfg.worker_enabled = false;
        cfg.no_show_grace_secs = 0;
        edit(&mut cfg);

        let (app, state) = build_app(cfg, resim).await.expect("build app");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let max_connections = state.cfg.max_connections;
        let server = tokio::spawn(crate::serve(listener, app, max_connections));
        state.tasks.add(server.abort_handle());
        TestApp {
            url: format!("http://{addr}"),
            to,
            state,
            dir,
            identity: None,
        }
    }

    /// Generates a key and registers it under `tag`, creating an invite first if one is required.
    pub async fn player(&self, tag: &str) -> TestPlayer {
        let p = TestPlayer::new(SigningKey::generate(&mut OsRng), tag, &self.url);
        let invite = if self.state.cfg.require_invite {
            let mut code = [0u8; 8];
            OsRng.fill_bytes(&mut code);
            let code = hex::encode(code);
            sqlx::query("INSERT INTO invites (code) VALUES (?)")
                .bind(&code)
                .execute(&self.state.db)
                .await
                .expect("insert invite");
            Some(code)
        } else {
            None
        };
        let res = p
            .post_json(
                "/v1/players",
                &RegisterReq {
                    tag: tag.into(),
                    invite,
                },
            )
            .await;
        assert_eq!(
            res.status(),
            reqwest::StatusCode::CREATED,
            "registering {tag}"
        );
        p
    }

    /// A client signing as the organiser (not registered as a player).
    pub fn as_to(&self) -> TestPlayer {
        TestPlayer::new(self.to.clone(), "TO", &self.url)
    }

    /// Creates an event (default ruleset) as the organiser and returns its id.
    pub async fn create_event(&self, format: Format) -> i64 {
        let req = EventCreateReq {
            name: "Weekly".into(),
            format,
            ruleset: None,
            best_of: None,
            starts_at: None,
            description: None,
        };
        let res = self.as_to().post_json("/v1/admin/events", &req).await;
        assert_eq!(res.status(), reqwest::StatusCode::CREATED, "creating event");
        let summary: EventSummary = res.json().await.expect("event summary");
        summary.id
    }

    /// Creates an event, signs `players` up in order (so `players[i]` is seed `i`), and starts it.
    pub async fn started_event(&self, format: Format, players: &[&TestPlayer]) -> i64 {
        let event = self.create_event(format).await;
        for p in players {
            p.sign_up(event).await;
        }
        let res = self
            .as_to()
            .post_json(
                &format!("/v1/admin/events/{event}/start"),
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(res.status(), reqwest::StatusCode::OK, "starting event");
        event
    }
}

impl Drop for TestApp {
    /// Stops the server and background tasks.
    fn drop(&mut self) {
        self.state.tasks.abort_all();
    }
}

/// A verifier that returns queued outcomes in order, then `Match`; its build is `"fake"`.
pub struct FakeResim(pub Mutex<VecDeque<ResimOutcome>>);

#[async_trait]
impl Resim for FakeResim {
    async fn check(&self, _slp: &Path, _replay_last_frame: i32) -> ResimOutcome {
        self.0
            .lock()
            .expect("FakeResim lock")
            .pop_front()
            .unwrap_or(ResimOutcome::Match)
    }

    fn build(&self) -> String {
        "fake".into()
    }
}

/// Plays game `n` of `set`: `host` (slot 0) and `guest` upload an agreeing pair that
/// `winner_port` wins (with different `start_at`), then the due games are judged.
pub async fn play_game(
    app: &TestApp,
    event: i64,
    set: u32,
    n: u8,
    host: &TestPlayer,
    guest: &TestPlayer,
    winner_port: u8,
) {
    let path = format!("/v1/sets/{event}/{set}/games/{n}/replay");
    for (p, start_at) in [
        (host, "2026-11-15T20:00:00Z"),
        (guest, "2026-11-15T20:00:01Z"),
    ] {
        let file = replay::testkit::simple_game(winner_port, start_at);
        let res = p.post_bytes(&path, file, &[("x-ab-build", "b1")]).await;
        assert_eq!(
            res.status(),
            reqwest::StatusCode::ACCEPTED,
            "{} uploading game {n} of set {set}",
            p.tag
        );
    }
    crate::worker::judge_due(&app.state)
        .await
        .expect("judging uploaded games");
}

pub struct TestPlayer {
    pub sk: SigningKey,
    pub tag: String,
    url: String,
    client: reqwest::Client,
}

impl TestPlayer {
    fn new(sk: SigningKey, tag: &str, url: &str) -> Self {
        TestPlayer {
            sk,
            tag: tag.into(),
            url: url.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Enters `event` with engine build `"b1"`; asserts it succeeds.
    pub async fn sign_up(&self, event: i64) {
        let req = SignupReq {
            engine_build: "b1".into(),
        };
        let res = self
            .post_json(&format!("/v1/events/{event}/entrants"), &req)
            .await;
        assert_eq!(
            res.status(),
            reqwest::StatusCode::CREATED,
            "{} signing up",
            self.tag
        );
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.send(Method::GET, path, Vec::new(), &[]).await
    }

    pub async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> reqwest::Response {
        let body = serde_json::to_vec(body).expect("serialize body");
        self.send(
            Method::POST,
            path,
            body,
            &[("content-type", "application/json")],
        )
        .await
    }

    pub async fn post_bytes(
        &self,
        path: &str,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> reqwest::Response {
        self.send(Method::POST, path, body, headers).await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> reqwest::Response {
        let h = sign_request(&self.sk, method.as_str(), path, now_unix(), &body);
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.url))
            .header("x-ab-key", h.key)
            .header("x-ab-time", h.time)
            .header("x-ab-nonce", h.nonce)
            .header("x-ab-sig", h.sig);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        req.body(body).send().await.expect("send request")
    }
}

impl TestApp {
    /// A client signing with `sk` (registered or not).
    pub fn client(&self, sk: SigningKey, tag: &str) -> TestPlayer {
        TestPlayer::new(sk, tag, &self.url)
    }
}

// --- Web identity (phase 2b) ---

/// The identity provider's issuer in tests.
pub const TEST_ISSUER: &str = "https://auth.test";
/// The site's origin in tests.
pub const TEST_ORIGIN: &str = "https://bracket.test";
/// The bracket web client id (the ID token's audience) in tests.
pub const TEST_CLIENT_ID: &str = "client-test";
/// `identity.proxy_secret` in tests.
pub const TEST_PROXY_SECRET: &str = "test-proxy-secret-test-proxy-secret-00";

/// A fake identity provider: a JWKS server on a random local port and its signing keys.
struct IdentityKit {
    /// Signs `kid "t1"`, served from the start.
    t1: SigningKey,
    /// Signs `kid "t2"`, served after [`TestApp::rotate_jwks`].
    t2: SigningKey,
    /// The JWKS document's keys.
    keys: Arc<Mutex<Vec<Value>>>,
    /// Requests the JWKS server has answered.
    fetches: Arc<AtomicUsize>,
    server: Mutex<Option<JoinHandle<()>>>,
}

/// How [`TestApp::web`] makes the `X-Id-Token`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenMode {
    /// Signed with `t1`, every claim right.
    Valid,
    /// `alg: none` and no signature.
    Unsigned,
    /// Claims `kid "t1"` but signed with another key.
    WrongKey,
    /// `iss` of another issuer.
    WrongIss,
    /// `aud` without our client id.
    WrongAud,
    /// Expired an hour ago.
    Expired,
    /// Signed with `t2` (served after [`TestApp::rotate_jwks`]).
    Kid2,
    /// No `X-Id-Token` header.
    Absent,
}

/// A web user as nginx and oauth2-proxy would present them.
#[derive(Clone, Debug)]
pub struct TestWeb {
    pub sub: String,
    pub groups: Vec<String>,
    pub amr: Vec<String>,
    /// Seconds since the sign-in (`auth_time = now - auth_time_ago`).
    pub auth_time_ago: i64,
    pub token: TokenMode,
    /// `X-Auth-Request-User`; `None` sends `sub`.
    pub header_sub: Option<String>,
    /// `X-Autobracket-Proxy`; `None` sends the right secret, `Some("")` none at all.
    pub proxy_secret: Option<String>,
}

impl TestWeb {
    /// A signed-in user with no groups.
    pub fn player(sub: &str) -> Self {
        TestWeb {
            sub: sub.into(),
            groups: Vec::new(),
            amr: vec!["user".into()],
            auth_time_ago: 60,
            token: TokenMode::Valid,
            header_sub: None,
            proxy_secret: None,
        }
    }

    /// An organiser who signed in a minute ago with a second factor.
    pub fn admin(sub: &str) -> Self {
        TestWeb {
            groups: vec!["autobracket-admins".into()],
            amr: vec!["user".into(), "mfa".into()],
            ..TestWeb::player(sub)
        }
    }
}

/// An HTTP client sending a [`TestWeb`]'s identity headers, plus `Origin` and `x-ab-csrf`.
pub struct WebClient {
    url: String,
    client: reqwest::Client,
    headers: Vec<(&'static str, String)>,
}

impl WebClient {
    /// A request with every header except those named in `omit` (lowercase).
    pub fn request(&self, method: Method, path: &str, omit: &[&str]) -> reqwest::RequestBuilder {
        let mut req = self.client.request(method, format!("{}{path}", self.url));
        for (name, value) in &self.headers {
            if !omit.contains(name) {
                req = req.header(*name, value);
            }
        }
        req
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.request(Method::GET, path, &[])
            .send()
            .await
            .expect("send request")
    }

    pub async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> reqwest::Response {
        self.request(Method::POST, path, &[])
            .json(body)
            .send()
            .await
            .expect("send request")
    }

    pub async fn delete(&self, path: &str) -> reqwest::Response {
        self.request(Method::DELETE, path, &[])
            .send()
            .await
            .expect("send request")
    }
}

/// The public JWK of an Ed25519 key.
fn ed25519_jwk(kid: &str, key: &SigningKey) -> Value {
    json!({
        "kty": "OKP",
        "crv": "Ed25519",
        "use": "sig",
        "alg": "EdDSA",
        "kid": kid,
        "x": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
    })
}

/// A compact JWT of `claims` with header `alg` and `kid`, signed with `key` (EdDSA), or with
/// an empty signature when `key` is `None`.
fn jwt(alg: &str, kid: &str, claims: &Value, key: Option<&SigningKey>) -> String {
    use ed25519_dalek::Signer;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = json!({ "alg": alg, "typ": "JWT", "kid": kid });
    let signing_input = format!(
        "{}.{}",
        b64.encode(header.to_string()),
        b64.encode(claims.to_string())
    );
    let sig = key.map_or_else(String::new, |k| {
        b64.encode(k.sign(signing_input.as_bytes()).to_bytes())
    });
    format!("{signing_input}.{sig}")
}

impl TestApp {
    /// As [`TestApp::spawn`], with `[identity]` set up against a fake identity provider: a
    /// JWKS server on a random local port serving one Ed25519 key (`kid "t1"`), the test
    /// origin, issuer and client id, proxy secret [`TEST_PROXY_SECRET`],
    /// `trusted_proxies = ["127.0.0.1/32"]` and hook keys `k-idp`, `k-token`, `k-events`.
    /// `edit` runs last.
    pub async fn spawn_identity(edit: impl FnOnce(&mut Config)) -> Self {
        let t1 = SigningKey::generate(&mut OsRng);
        let t2 = SigningKey::generate(&mut OsRng);
        let keys = Arc::new(Mutex::new(vec![ed25519_jwk("t1", &t1)]));
        let fetches = Arc::new(AtomicUsize::new(0));
        let router = {
            let (keys, fetches) = (keys.clone(), fetches.clone());
            axum::Router::new().route(
                "/oauth/v2/keys",
                axum::routing::get(move || {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    let doc = json!({ "keys": keys.lock().expect("JWKS lock").clone() });
                    async move { axum::Json(doc) }
                }),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind JWKS server");
        let jwks_addr = listener.local_addr().expect("JWKS address");
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("JWKS server");
        });

        let mut app = Self::spawn(move |cfg| {
            cfg.trusted_proxies = vec!["127.0.0.1/32".into()];
            cfg.identity = Some(IdentityConfig {
                public_origin: TEST_ORIGIN.into(),
                issuer: TEST_ISSUER.into(),
                client_id: TEST_CLIENT_ID.into(),
                jwks_url: Some(format!("http://{jwks_addr}/oauth/v2/keys")),
                project_id: "p1".into(),
                org_id: "o1".into(),
                discord_idp_id: "idp1".into(),
                proxy_secret: TEST_PROXY_SECRET.into(),
                admin_group: "autobracket-admins".into(),
                admin_max_auth_age_secs: 43_200,
                sign_out_url: "/oauth2/sign_out".into(),
                mfa_setup_url: "https://auth.test/mfa".into(),
                hook_peers: Vec::new(),
                hook_keys: HookKeys {
                    idp_intent: "k-idp".into(),
                    token: "k-token".into(),
                    user_events: "k-events".into(),
                },
            });
            edit(cfg);
        })
        .await;
        app.state.tasks.add(server.abort_handle());
        app.identity = Some(IdentityKit {
            t1,
            t2,
            keys,
            fetches,
            server: Mutex::new(Some(server)),
        });
        app
    }

    fn kit(&self) -> &IdentityKit {
        self.identity
            .as_ref()
            .expect("identity helpers need TestApp::spawn_identity")
    }

    /// Adds a second key (`kid "t2"`) to the served JWKS.
    pub fn rotate_jwks(&self) {
        let kit = self.kit();
        kit.keys
            .lock()
            .expect("JWKS lock")
            .push(ed25519_jwk("t2", &kit.t2));
    }

    /// Stops the JWKS server; fetching the keys then fails.
    pub async fn stop_jwks(&self) {
        let server = self.kit().server.lock().expect("JWKS lock").take();
        if let Some(server) = server {
            server.abort();
            let _ = server.await;
        }
    }

    /// How many times the JWKS has been fetched.
    pub fn jwks_fetches(&self) -> usize {
        self.kit().fetches.load(Ordering::SeqCst)
    }

    /// The claims of a valid ID token for `who` (ignoring `who.token`).
    pub fn id_claims(&self, who: &TestWeb) -> Value {
        let now = now_unix();
        json!({
            "iss": TEST_ISSUER,
            "aud": [TEST_CLIENT_ID, "p1"],
            "sub": who.sub,
            "iat": now,
            "exp": now + 3600,
            "auth_time": now - who.auth_time_ago,
            "groups": who.groups,
            "amr": who.amr,
        })
    }

    /// `claims` as an EdDSA ID token with header `kid`: signed with `t1` or `t2` for those
    /// kids, with a fresh unknown key for any other.
    pub fn sign_id_token(&self, kid: &str, claims: &Value) -> String {
        let kit = self.kit();
        let other;
        let key = match kid {
            "t1" => &kit.t1,
            "t2" => &kit.t2,
            _ => {
                other = SigningKey::generate(&mut OsRng);
                &other
            }
        };
        jwt("EdDSA", kid, claims, Some(key))
    }

    /// The `X-Id-Token` value (without `Bearer `) for `who`, per `who.token`.
    pub fn id_token(&self, who: &TestWeb) -> Option<String> {
        let mut claims = self.id_claims(who);
        let now = now_unix();
        match who.token {
            TokenMode::Valid => {}
            TokenMode::Unsigned => return Some(jwt("none", "t1", &claims, None)),
            TokenMode::WrongKey => {
                let other = SigningKey::generate(&mut OsRng);
                return Some(jwt("EdDSA", "t1", &claims, Some(&other)));
            }
            TokenMode::WrongIss => claims["iss"] = json!("https://evil.test"),
            TokenMode::WrongAud => claims["aud"] = json!(["other-client"]),
            TokenMode::Expired => {
                claims["iat"] = json!(now - 7200);
                claims["exp"] = json!(now - 3600);
            }
            TokenMode::Kid2 => return Some(self.sign_id_token("t2", &claims)),
            TokenMode::Absent => return None,
        }
        Some(self.sign_id_token("t1", &claims))
    }

    /// A client presenting `who` the way nginx would: `X-Auth-Request-User`,
    /// `X-Id-Token: Bearer <jwt>`, `X-Autobracket-Proxy`, plus `Origin` and `x-ab-csrf: 1`.
    pub fn web(&self, who: &TestWeb) -> WebClient {
        let mut headers = vec![(
            "x-auth-request-user",
            who.header_sub.clone().unwrap_or_else(|| who.sub.clone()),
        )];
        if let Some(token) = self.id_token(who) {
            headers.push(("x-id-token", format!("Bearer {token}")));
        }
        match who.proxy_secret.as_deref() {
            None => headers.push(("x-autobracket-proxy", TEST_PROXY_SECRET.into())),
            Some("") => {}
            Some(secret) => headers.push(("x-autobracket-proxy", secret.into())),
        }
        headers.push(("origin", TEST_ORIGIN.into()));
        headers.push(("x-ab-csrf", "1".into()));
        WebClient {
            url: self.url.clone(),
            client: reqwest::Client::new(),
            headers,
        }
    }
}

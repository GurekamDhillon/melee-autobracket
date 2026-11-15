//! Test harness: a server on a random local port backed by a temporary
//! directory, and signing HTTP clients for organisers and players.

use std::net::SocketAddr;
use std::sync::Arc;

use proto::{sign_request, RegisterReq, SigningKey};
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::Method;
use serde::Serialize;
use tempfile::TempDir;

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
}

impl TestApp {
    /// Spawns with [`NoResim`]. Rate limits are raised out of reach unless `edit` lowers them.
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
        edit(&mut cfg);

        let (app, state) = build_app(cfg, resim).await.expect("build app");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("serve");
        });
        TestApp {
            url: format!("http://{addr}"),
            to,
            state,
            dir,
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

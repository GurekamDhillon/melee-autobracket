//! Signed calls to the bracket server, one method per route.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use proto::{
    sign_request, EventSummary, EventView, Next, RegisterReq, RoomReq, SigningKey, SignupReq,
};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// A non-success response, carrying the server's `{"error": "..."}` message.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {})", self.message, self.status.as_u16())
    }
}

impl std::error::Error for ApiError {}

pub struct Api {
    base: String,
    sk: SigningKey,
    engine_build: String,
    http: reqwest::Client,
}

impl Api {
    /// `server` is the base URL, e.g. `http://127.0.0.1:8080`.
    pub fn new(server: &str, sk: SigningKey, engine_build: &str) -> Self {
        Api {
            base: server.trim_end_matches('/').to_string(),
            sk,
            engine_build: engine_build.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// `POST /v1/players`: binds this key to `tag`.
    pub async fn register(&self, tag: &str, invite: Option<&str>) -> anyhow::Result<()> {
        let req = RegisterReq {
            tag: tag.into(),
            invite: invite.map(String::from),
        };
        self.post_json("/v1/players", &req).await.map(drop)
    }

    /// `GET /v1/events`.
    pub async fn events(&self) -> anyhow::Result<Vec<EventSummary>> {
        self.get("/v1/events").await
    }

    /// `GET /v1/events/{id}`.
    pub async fn event(&self, id: i64) -> anyhow::Result<EventView> {
        self.get(&format!("/v1/events/{id}")).await
    }

    /// `POST /v1/events/{id}/entrants` with this client's engine build.
    pub async fn signup(&self, event: i64) -> anyhow::Result<()> {
        let req = SignupReq {
            engine_build: self.engine_build.clone(),
        };
        self.post_json(&format!("/v1/events/{event}/entrants"), &req)
            .await
            .map(drop)
    }

    /// `GET /v1/me/next?event={id}`.
    pub async fn next(&self, event: i64) -> anyhow::Result<Next> {
        self.get(&format!("/v1/me/next?event={event}")).await
    }

    /// `POST /v1/sets/{event}/{set}/room`: the host hands over the room code.
    pub async fn post_room(&self, event: i64, set: u32, code: &str) -> anyhow::Result<()> {
        let req = RoomReq { code: code.into() };
        self.post_json(&format!("/v1/sets/{event}/{set}/room"), &req)
            .await
            .map(drop)
    }

    /// `POST /v1/sets/{event}/{set}/games/{n}/replay`: uploads a raw `.slp`. Returns `202`
    /// when stored, `200` when the identical file was already stored.
    pub async fn upload_replay(
        &self,
        event: i64,
        set: u32,
        n: u8,
        slp: Vec<u8>,
    ) -> anyhow::Result<StatusCode> {
        let path = format!("/v1/sets/{event}/{set}/games/{n}/replay");
        let res = self
            .send(
                Method::POST,
                &path,
                slp,
                &[("x-ab-build", self.engine_build.as_str())],
            )
            .await?;
        Ok(res.status())
    }

    async fn get<T: DeserializeOwned>(&self, path_and_query: &str) -> anyhow::Result<T> {
        let res = self
            .send(Method::GET, path_and_query, Vec::new(), &[])
            .await?;
        res.json()
            .await
            .with_context(|| format!("reading the response to GET {path_and_query}"))
    }

    async fn post_json<T: Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> anyhow::Result<reqwest::Response> {
        let body = serde_json::to_vec(body).context("serialising request body")?;
        self.send(
            Method::POST,
            path,
            body,
            &[("content-type", "application/json")],
        )
        .await
    }

    /// Signs and sends a request; a non-2xx status becomes an [`ApiError`].
    async fn send(
        &self,
        method: Method,
        path_and_query: &str,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<reqwest::Response> {
        let h = sign_request(&self.sk, method.as_str(), path_and_query, now_unix(), &body);
        let url = format!("{}{path_and_query}", self.base);
        let mut req = self
            .http
            .request(method.clone(), &url)
            .header("x-ab-key", h.key)
            .header("x-ab-time", h.time)
            .header("x-ab-nonce", h.nonce)
            .header("x-ab-sig", h.sig);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let res = req
            .body(body)
            .send()
            .await
            .with_context(|| format!("{method} {url}"))?;
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let text = res.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(String::from))
            .unwrap_or_else(|| {
                let text = text.trim();
                if text.is_empty() {
                    status.canonical_reason().unwrap_or("error").to_string()
                } else {
                    text.to_string()
                }
            });
        Err(ApiError { status, message }.into())
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

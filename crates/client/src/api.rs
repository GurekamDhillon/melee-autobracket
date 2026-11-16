//! Signed calls to the bracket server, one method per route.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use proto::{
    sign_request, EventSummary, EventView, Next, RegisterReq, RoomReq, SigningKey, SignupReq,
};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// A failed call: a non-success response carrying the server's `{"error": "..."}` message,
/// or (`status: None`) a transport failure such as a refused or reset connection.
#[derive(Debug)]
pub struct ApiError {
    pub status: Option<StatusCode>,
    pub message: String,
}

impl ApiError {
    /// Worth trying again: transport failures, 5xx, and 429 (rate limited). Any other
    /// 4xx is the server refusing the request and will not change by retrying.
    pub fn retryable(&self) -> bool {
        self.status
            .is_none_or(|s| s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(s) => write!(f, "{} (HTTP {})", self.message, s.as_u16()),
            None => write!(f, "cannot reach the server: {}", self.message),
        }
    }
}

impl std::error::Error for ApiError {}

/// Whether `e` is an [`ApiError`] worth retrying.
pub fn is_retryable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ApiError>()
        .is_some_and(ApiError::retryable)
}

/// A request that failed without a response. A request that could not even be built
/// (a malformed server URL, say) is a fatal error; anything else is a retryable
/// [`ApiError`] transport failure.
fn transport(what: &str, e: &reqwest::Error) -> anyhow::Error {
    let mut message = format!("{what}: {e}");
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        message.push_str(&format!(": {s}"));
        source = s.source();
    }
    if e.is_builder() {
        return anyhow::anyhow!("invalid request: {message}");
    }
    ApiError {
        status: None,
        message,
    }
    .into()
}

/// A warning when `server` is plain `http://` to a host that is not this machine: requests
/// and answers (room codes, the relay address the game connects to) cross the network
/// unauthenticated and in the clear.
pub fn insecure_server_warning(server: &str) -> Option<String> {
    let url = reqwest::Url::parse(server).ok()?;
    if url.scheme() != "http" {
        return None;
    }
    let host = url.host_str()?;
    let ip = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || ip
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    (!loopback).then(|| {
        format!(
            "warning: {server} is plain http; anyone on the path can read your room codes and \
             change what the server tells your client. Ask the organiser for an https:// address."
        )
    })
}

/// How long a call may take.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// To open the connection.
    pub connect: Duration,
    /// For a whole request, answer included.
    pub request: Duration,
    /// For a whole replay upload.
    pub upload: Duration,
}

impl Default for Timeouts {
    /// 10 s to connect, 30 s per request, 120 s per replay upload.
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(10),
            request: Duration::from_secs(30),
            upload: Duration::from_secs(120),
        }
    }
}

/// Why a no-show is reported: the opponent did not turn up (`no-show`), or this client
/// could not get a game going (`client-failure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoShowKind {
    NoShow,
    ClientFailure,
}

impl NoShowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoShowKind::NoShow => "no-show",
            NoShowKind::ClientFailure => "client-failure",
        }
    }
}

/// Checks that `server` is an absolute `http://` or `https://` URL with a host.
pub fn check_server_url(server: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(server).with_context(|| {
        format!("server {server:?} is not a valid URL (expected e.g. http://host:8080)")
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        anyhow::bail!("server {server:?} must be an http:// or https:// URL with a host");
    }
    Ok(())
}

pub struct Api {
    base: String,
    sk: SigningKey,
    engine_build: String,
    http: reqwest::Client,
    upload_timeout: Duration,
}

impl Api {
    /// `server` is the base URL, e.g. `http://127.0.0.1:8080`; `engine_build` is sent at
    /// sign-up and with each replay. Uses the default [`Timeouts`].
    pub fn new(server: &str, sk: SigningKey, engine_build: &str) -> Self {
        Self::with_timeouts(server, sk, engine_build, Timeouts::default())
    }

    pub fn with_timeouts(
        server: &str,
        sk: SigningKey,
        engine_build: &str,
        timeouts: Timeouts,
    ) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .build()
            // Fails only if the TLS backend cannot start: nothing a caller could act on.
            .expect("building the HTTP client");
        Api {
            base: server.trim_end_matches('/').to_string(),
            sk,
            engine_build: engine_build.to_string(),
            http,
            upload_timeout: timeouts.upload,
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

    /// `POST /v1/sets/{event}/{set}/no-show`: the set cannot be played, for `kind`'s reason.
    /// On success the set goes to the organiser's review. The server answers `409` when the
    /// set is already in review, not ready, or has a game in progress, or (for `no-show`)
    /// when a room code was posted moments ago; a server without the route, `404`.
    pub async fn report_no_show(
        &self,
        event: i64,
        set: u32,
        detail: Option<&str>,
        kind: NoShowKind,
    ) -> anyhow::Result<()> {
        #[derive(Serialize)]
        struct NoShowReq<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            detail: Option<&'a str>,
            kind: &'static str,
        }
        self.post_json(
            &format!("/v1/sets/{event}/{set}/no-show"),
            &NoShowReq {
                detail,
                kind: kind.as_str(),
            },
        )
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
            .send_with(
                Method::POST,
                &path,
                slp,
                &[("x-ab-build", self.engine_build.as_str())],
                Some(self.upload_timeout),
            )
            .await?;
        Ok(res.status())
    }

    async fn get<T: DeserializeOwned>(&self, path_and_query: &str) -> anyhow::Result<T> {
        let res = self
            .send(Method::GET, path_and_query, Vec::new(), &[])
            .await?;
        let body = res
            .bytes()
            .await
            .map_err(|e| transport(&format!("reading GET {path_and_query}"), &e))?;
        serde_json::from_slice(&body)
            .with_context(|| format!("unexpected response to GET {path_and_query}"))
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

    async fn send(
        &self,
        method: Method,
        path_and_query: &str,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<reqwest::Response> {
        self.send_with(method, path_and_query, body, headers, None)
            .await
    }

    /// Signs and sends a request (with its own `timeout`, if given); a non-2xx status
    /// becomes an [`ApiError`].
    async fn send_with(
        &self,
        method: Method,
        path_and_query: &str,
        body: Vec<u8>,
        headers: &[(&str, &str)],
        timeout: Option<Duration>,
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
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let res = req
            .body(body)
            .send()
            .await
            .map_err(|e| transport(&format!("{method} {url}"), &e))?;
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let text = res.text().await.unwrap_or_default();
        let json = serde_json::from_str::<serde_json::Value>(&text).ok();
        let mut message = json
            .as_ref()
            .and_then(|v| v.get("error")?.as_str().map(String::from))
            .unwrap_or_else(|| {
                let text = text.trim();
                if text.is_empty() {
                    status.canonical_reason().unwrap_or("error").to_string()
                } else {
                    text.to_string()
                }
            });
        if status == StatusCode::UNAUTHORIZED && message.starts_with("clock skew") {
            message = format!(
                "YOUR COMPUTER'S CLOCK IS WRONG. The server says: {message}. Set this \
                 computer's date and time correctly (turn on automatic time), then try again"
            );
        }
        if let Some(accepted) = json
            .as_ref()
            .and_then(|v| v.get("accepted")?.as_array().cloned())
        {
            let list: Vec<String> = accepted
                .iter()
                .filter_map(|b| b.as_str().map(|b| format!("{b:?}")))
                .collect();
            message = format!(
                "{message}. Your engine build is {:?}; this server accepts: {}",
                self.engine_build,
                if list.is_empty() {
                    "(none listed)".to_string()
                } else {
                    list.join(", ")
                }
            );
        }
        Err(ApiError {
            status: Some(status),
            message,
        }
        .into())
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

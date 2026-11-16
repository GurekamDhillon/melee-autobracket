//! `autobracket verify-worker`: re-simulates replays for a server in external verifier mode.
//! It claims jobs over the signed API, runs the engine locally, and posts the results.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use proto::{sign_request, SigningKey, VerifyJob, VerifyResult};
use reqwest::{Method, StatusCode};
use tracing::{info, warn};

use crate::db::now_unix;
use crate::logging::resim_kind;
use crate::resim::{Resim, ScratchDir, Slpcheck};

/// Pause after the server reports no queued jobs.
const IDLE_SLEEP: Duration = Duration::from_secs(2);
/// Pause after a network or server error.
const ERROR_SLEEP: Duration = Duration::from_secs(5);
/// How long establishing a connection to the server may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one request may take in all, response body included (enough for a 16 MiB replay).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Name of the downloaded replay. Stored replays are named after their slot, which says
/// nothing; the engine ignores the name.
const REPLAY_FILE: &str = "match.slp";

/// Reads a secret key as written by `autobracket keygen` (64 hex digits).
pub fn read_key_file(path: &Path) -> anyhow::Result<SigningKey> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let bytes = hex::decode(text.trim()).with_context(|| format!("{}: not hex", path.display()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{}: expected a 32-byte secret key", path.display()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

/// A warning when `server` is plain `http://` to a host that is not this machine: the
/// server's answers (the job and its replay) would then be unauthenticated.
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
            "{server} is plain http; anyone on the path can swap the replays this worker \
             checks. Use https:// in production."
        )
    })
}

/// Claims and checks jobs forever. Errors are logged and retried; only setting up the
/// HTTP client can fail.
pub async fn run(server: String, key: SigningKey, engine: Slpcheck) -> anyhow::Result<()> {
    if let Some(warning) = insecure_server_warning(&server) {
        warn!("{warning}");
    }
    let client = Client::new(&server, key, CONNECT_TIMEOUT, REQUEST_TIMEOUT)?;
    info!(
        server = %client.base,
        build = %engine.build(),
        "verify-worker started"
    );
    loop {
        match step(&client, &engine).await {
            Ok(true) => {}
            Ok(false) => tokio::time::sleep(IDLE_SLEEP).await,
            Err(e) => {
                warn!(error = %format_args!("{e:#}"), "verify-worker: retrying after an error");
                tokio::time::sleep(ERROR_SLEEP).await;
            }
        }
    }
}

/// Asks the server for a job: `None` when there is none.
pub async fn claim(client: &Client) -> anyhow::Result<Option<VerifyJob>> {
    let res = client
        .send(Method::POST, "/v1/verify/claim", Vec::new())
        .await?;
    match res.status() {
        StatusCode::NO_CONTENT => return Ok(None),
        StatusCode::OK => {}
        _ => bail!("claiming a job: {}", describe(res).await),
    }
    let job = res.json().await.context("reading the claimed job")?;
    Ok(Some(job))
}

/// Claims one job and checks it. `false` when there was no job.
async fn step(client: &Client, engine: &Slpcheck) -> anyhow::Result<bool> {
    let Some(job) = claim(client).await? else {
        return Ok(false);
    };
    info!(job = job.id, "verify job claimed");

    let res = client
        .send(
            Method::GET,
            &format!("/v1/verify/jobs/{}/replay", job.id),
            Vec::new(),
        )
        .await?;
    if res.status() != StatusCode::OK {
        bail!("fetching job {} replay: {}", job.id, describe(res).await);
    }
    let bytes = res
        .bytes()
        .await
        .with_context(|| format!("downloading job {} replay", job.id))?;
    let dir = ScratchDir::create().context("creating a temp dir")?;
    let slp = dir.path().join(REPLAY_FILE);
    tokio::fs::write(&slp, &bytes)
        .await
        .with_context(|| format!("writing {}", slp.display()))?;

    let outcome = engine.check(&slp, job.replay_last_frame).await;
    drop(dir);

    let result = VerifyResult {
        outcome,
        build: engine.build(),
    };
    let body = serde_json::to_vec(&result).context("encoding the result")?;
    let res = client
        .send(
            Method::POST,
            &format!("/v1/verify/jobs/{}/result", job.id),
            body,
        )
        .await?;
    match res.status() {
        StatusCode::NO_CONTENT => info!(
            job = job.id,
            outcome = %resim_kind(&result.outcome),
            build = %result.build,
            "verify result posted"
        ),
        // The server gave up on the job, or another verifier finished it first.
        StatusCode::CONFLICT => info!(
            job = job.id,
            outcome = %resim_kind(&result.outcome),
            "verify result not needed"
        ),
        _ => bail!("posting job {} result: {}", job.id, describe(res).await),
    }
    Ok(true)
}

/// A signing HTTP client for one server.
pub struct Client {
    base: String,
    key: SigningKey,
    http: reqwest::Client,
}

impl Client {
    /// `connect_timeout` bounds establishing a connection, `timeout` each whole request, so
    /// a stalled server surfaces as an error instead of blocking the worker.
    pub fn new(
        server: &str,
        key: SigningKey,
        connect_timeout: Duration,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(timeout)
            .build()
            .context("building the HTTP client")?;
        Ok(Client {
            base: server.trim_end_matches('/').to_owned(),
            key,
            http,
        })
    }

    /// Sends a request signed with the worker's key.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
    ) -> anyhow::Result<reqwest::Response> {
        let h = sign_request(&self.key, method.as_str(), path, now_unix(), &body);
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.base))
            .header("x-ab-key", h.key)
            .header("x-ab-time", h.time)
            .header("x-ab-nonce", h.nonce)
            .header("x-ab-sig", h.sig);
        if !body.is_empty() {
            req = req.header("content-type", "application/json");
        }
        req.body(body)
            .send()
            .await
            .with_context(|| format!("{method} {}{path}", self.base))
    }
}

/// The status and body of an unexpected response.
async fn describe(res: reqwest::Response) -> String {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    format!("server answered {status}: {}", body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_server_is_an_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accepts connections and never answers.
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let key = SigningKey::from_bytes(&[7; 32]);
        let client = Client::new(
            &format!("http://{addr}"),
            key,
            Duration::from_millis(200),
            Duration::from_millis(300),
        )
        .unwrap();
        let claimed = tokio::time::timeout(Duration::from_secs(5), claim(&client))
            .await
            .expect("claim gives up instead of hanging");
        assert!(claimed.is_err(), "{claimed:?}");
        server.abort();
    }

    #[test]
    fn plain_http_warning_only_for_remote_hosts() {
        for remote in ["http://bracket.example:8080", "http://10.0.0.5/"] {
            assert!(insecure_server_warning(remote).is_some(), "{remote}");
        }
        for fine in [
            "https://bracket.example",
            "http://localhost:8080",
            "http://127.0.0.1:1",
            "http://[::1]:8080",
        ] {
            assert_eq!(insecure_server_warning(fine), None, "{fine}");
        }
    }
}

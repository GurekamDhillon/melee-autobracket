//! `autobracket verify-worker`: re-simulates replays for a server in external verifier mode.
//! It claims jobs over the signed API, runs the engine locally, and posts the results.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use proto::{sign_request, SigningKey, VerifyJob, VerifyResult};
use reqwest::{Method, StatusCode};

use crate::db::now_unix;
use crate::resim::{Resim, ScratchDir, Slpcheck};

/// Pause after the server reports no queued jobs.
const IDLE_SLEEP: Duration = Duration::from_secs(2);
/// Pause after a network or server error.
const ERROR_SLEEP: Duration = Duration::from_secs(5);
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

/// Claims and checks jobs forever. Errors are logged and retried, never returned.
pub async fn run(server: String, key: SigningKey, engine: Slpcheck) {
    let client = Client {
        base: server.trim_end_matches('/').to_owned(),
        key,
        http: reqwest::Client::new(),
    };
    eprintln!(
        "verify-worker: checking jobs from {} with engine build {}",
        client.base,
        engine.build()
    );
    loop {
        match step(&client, &engine).await {
            Ok(true) => {}
            Ok(false) => tokio::time::sleep(IDLE_SLEEP).await,
            Err(e) => {
                eprintln!("verify-worker: {e:#}");
                tokio::time::sleep(ERROR_SLEEP).await;
            }
        }
    }
}

/// Claims one job and checks it. `false` when there was no job.
async fn step(client: &Client, engine: &Slpcheck) -> anyhow::Result<bool> {
    let res = client
        .send(Method::POST, "/v1/verify/claim", Vec::new())
        .await?;
    match res.status() {
        StatusCode::NO_CONTENT => return Ok(false),
        StatusCode::OK => {}
        _ => bail!("claiming a job: {}", describe(res).await),
    }
    let job: VerifyJob = res.json().await.context("reading the claimed job")?;

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
    eprintln!("verify-worker: job {}: {outcome:?}", job.id);

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
        StatusCode::NO_CONTENT => {}
        // The server gave up on the job, or another verifier finished it first.
        StatusCode::CONFLICT => eprintln!("verify-worker: job {}: result not needed", job.id),
        _ => bail!("posting job {} result: {}", job.id, describe(res).await),
    }
    Ok(true)
}

struct Client {
    base: String,
    key: SigningKey,
    http: reqwest::Client,
}

impl Client {
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

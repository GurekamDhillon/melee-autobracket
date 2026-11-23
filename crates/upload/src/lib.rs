//! `autobracket-upload`: the smallest signed client of the bracket API, for a player whose game
//! runs on another client (Slippi Dolphin, say) in an event with any-client mode on. It makes a
//! key, registers it, signs up, says what to play next, and uploads a `.slp` — each one request
//! signed exactly as `docs/api.md` section 2 says, with the player's Ed25519 key.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use proto::{sign_request, SigningKey};
use rand::rngs::OsRng;

/// The key in a key file: 64 hex digits (what `keygen` writes), or a reference client's
/// config TOML (its `secret_key_hex`).
pub fn read_key(path: &Path) -> anyhow::Result<SigningKey> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let hexkey = match toml::from_str::<toml::Table>(&text) {
        Ok(t) if t.contains_key("secret_key_hex") => t["secret_key_hex"]
            .as_str()
            .context("secret_key_hex must be a string")?
            .to_owned(),
        _ => text.trim().to_owned(),
    };
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(hexkey.trim(), &mut bytes)
        .with_context(|| format!("{}: the key must be 64 hex digits", path.display()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

/// Writes a new key to `path` (refusing to replace one) and returns its public half, hex.
pub fn keygen(path: &Path) -> anyhow::Result<String> {
    if path.exists() {
        bail!("{} exists; not replacing a key", path.display());
    }
    let sk = SigningKey::generate(&mut OsRng);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    opts.open(path)
        .with_context(|| format!("creating {}", path.display()))?
        .write_all(format!("{}\n", hex::encode(sk.to_bytes())).as_bytes())?;
    Ok(hex::encode(sk.verifying_key().to_bytes()))
}

/// A signing client for one server.
pub struct Uploader {
    base: String,
    sk: SigningKey,
    http: reqwest::Client,
}

/// A response: status and body text.
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Uploader {
    pub fn new(server: &str, sk: SigningKey) -> anyhow::Result<Self> {
        if !(server.starts_with("http://") || server.starts_with("https://")) {
            bail!("the server must be an http:// or https:// URL");
        }
        Ok(Uploader {
            base: server.trim_end_matches('/').to_owned(),
            sk,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(150))
                .build()?,
        })
    }

    /// One signed request: `path_and_query` is signed exactly as sent.
    pub async fn send(
        &self,
        method: reqwest::Method,
        path_and_query: &str,
        body: Vec<u8>,
        extra: &[(&str, &str)],
    ) -> anyhow::Result<Reply> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let h = sign_request(&self.sk, method.as_str(), path_and_query, now, &body);
        let mut req = self
            .http
            .request(method, format!("{}{path_and_query}", self.base))
            .header("x-ab-key", h.key)
            .header("x-ab-time", h.time)
            .header("x-ab-nonce", h.nonce)
            .header("x-ab-sig", h.sig)
            .body(body);
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let res = req.send().await.context("sending the request")?;
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        Ok(Reply { status, body })
    }

    pub async fn register(&self, tag: &str, invite: Option<&str>) -> anyhow::Result<Reply> {
        let body = serde_json::json!({ "tag": tag, "invite": invite }).to_string();
        self.send(reqwest::Method::POST, "/v1/players", body.into_bytes(), &[("content-type", "application/json")])
            .await
    }

    /// Sets the Slippi connect code (`ABCD#123`) of the registered player, so any-client games
    /// can tell the two players apart by code instead of by port.
    pub async fn slippi_code(&self, code: &str) -> anyhow::Result<Reply> {
        let body = serde_json::json!({ "code": code }).to_string();
        self.send(reqwest::Method::POST, "/v1/players/slippi", body.into_bytes(), &[("content-type", "application/json")])
            .await
    }

    pub async fn signup(&self, event: i64, build: &str) -> anyhow::Result<Reply> {
        let body = serde_json::json!({ "engine_build": build }).to_string();
        self.send(
            reqwest::Method::POST,
            &format!("/v1/events/{event}/entrants"),
            body.into_bytes(),
            &[("content-type", "application/json")],
        )
        .await
    }

    pub async fn next(&self, event: i64) -> anyhow::Result<Reply> {
        self.send(reqwest::Method::GET, &format!("/v1/me/next?event={event}"), Vec::new(), &[])
            .await
    }

    /// Uploads `slp` for game `game` of set `set`; `build` goes in `x-ab-build`.
    pub async fn upload(
        &self,
        event: i64,
        set: i64,
        game: i64,
        slp: &[u8],
        build: &str,
    ) -> anyhow::Result<Reply> {
        self.send(
            reqwest::Method::POST,
            &format!("/v1/sets/{event}/{set}/games/{game}/replay"),
            slp.to_vec(),
            &[("content-type", "application/octet-stream"), ("x-ab-build", build)],
        )
        .await
    }
}

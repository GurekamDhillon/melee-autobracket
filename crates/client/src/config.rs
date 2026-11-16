//! The client config file, `~/.config/autobracket/client.toml` unless `--config` names
//! another. It holds the player's secret key, so it is only ever written with mode 0600.
//!
//! Relative `melee`, `iso` and `work_dir` paths in the file are relative to the directory
//! the file is in; they are made absolute when it is loaded. A `melee` that is a bare name
//! (no `/`) is looked up on `PATH` instead.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use proto::SigningKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

/// The comment block at the top of every config file written.
const HEADER: &str = "\
# autobracket-client config. It holds your secret key: keep it private.
#
# Paths: a relative `melee`, `iso` or `work_dir` is relative to the directory this file is
# in. A `melee` with no `/` in it is looked up on PATH.
#
# No display (a server, or a machine without a desktop)? Create the config with
# `autobracket-client init --headless`, which adds \"--headless\", \"--no-audio\" to
# both host_args and join_args, or add those two arguments to both by hand.
#
# Opponent no-show: a newer engine can stop hosting when nobody joins within SECS seconds
# (an engine without the flag refuses to start). The client then reports a no-show to the
# organiser after two such runs. To use it, add to host_args:
#   \"--np-join-wait\", \"120\",
#
# Preset fighters and stage instead of netplay character select. Use it while character
# select can crash both games (an engine bug in name entry): replace
# \"--np-css\", \"--quit-after-matches\", \"1\" with
#   host_args: \"--match\", \"<fighter>,<stage>\",   e.g. \"--match\", \"falcon,32\",
#   join_args: \"--match\", \"<fighter>\",           e.g. \"--match\", \"fox\",

";

/// The arguments `init --headless` adds, for machines with no display or sound.
pub const HEADLESS_ARGS: [&str; 2] = ["--headless", "--no-audio"];

/// Picks the room code out of the host's stderr line
/// `play: netplay host: MATCH CODE K7QX (give it to the other player)`.
pub const DEFAULT_CODE_REGEX: &str = "MATCH CODE ([A-Z0-9]+)";

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct ClientConfig {
    /// Base URL of the bracket server; empty until `register --server`.
    pub server: String,
    pub secret_key_hex: String,
    /// Sent at sign-up and with every replay upload.
    pub engine_build: String,
    /// The `melee` engine binary.
    pub melee: PathBuf,
    pub iso: PathBuf,
    /// Each game runs in a fresh `{work_dir}/run-{n}/`, kept afterwards.
    pub work_dir: PathBuf,
    /// Engine arguments; see [`crate::engine::CommandEngine`] for the placeholders.
    pub host_args: Vec<String>,
    pub join_args: Vec<String>,
    pub code_regex: String,
}

impl fmt::Debug for ClientConfig {
    /// Like a derived `Debug`, with the secret key redacted.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("server", &self.server)
            .field("secret_key_hex", &"<redacted>")
            .field("engine_build", &self.engine_build)
            .field("melee", &self.melee)
            .field("iso", &self.iso)
            .field("work_dir", &self.work_dir)
            .field("host_args", &self.host_args)
            .field("join_args", &self.join_args)
            .field("code_regex", &self.code_regex)
            .finish()
    }
}

/// `play --iso {iso} --netplay host --np-server {relay} --record {mrpl} --slp auto --np-css
/// --quit-after-matches 1 --stocks {stocks} --time-secs {timer_secs}`.
pub fn default_host_args() -> Vec<String> {
    engine_args(&["host"])
}

/// [`default_host_args`] with `--netplay join {code}`.
pub fn default_join_args() -> Vec<String> {
    engine_args(&["join", "{code}"])
}

fn engine_args(netplay: &[&str]) -> Vec<String> {
    let mut args = vec!["play", "--iso", "{iso}", "--netplay"];
    args.extend_from_slice(netplay);
    args.extend_from_slice(&[
        "--np-server",
        "{relay}",
        "--record",
        "{mrpl}",
        "--slp",
        "auto",
        "--np-css",
        "--quit-after-matches",
        "1",
        "--stocks",
        "{stocks}",
        "--time-secs",
        "{timer_secs}",
    ]);
    args.into_iter().map(String::from).collect()
}

impl ClientConfig {
    /// A config with the default engine arguments and no server yet.
    pub fn with_defaults(
        secret_key_hex: String,
        engine_build: String,
        melee: PathBuf,
        iso: PathBuf,
        work_dir: PathBuf,
    ) -> Self {
        ClientConfig {
            server: String::new(),
            secret_key_hex,
            engine_build,
            melee,
            iso,
            work_dir,
            host_args: default_host_args(),
            join_args: default_join_args(),
            code_regex: DEFAULT_CODE_REGEX.into(),
        }
    }

    /// Reads the config, checking `server` if one is set.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let cfg = Self::load_unchecked(path)?;
        // Empty until `register --server`.
        if !cfg.server.is_empty() {
            crate::api::check_server_url(&cfg.server)
                .with_context(|| format!("bad `server` in {}", path.display()))?;
        }
        Ok(cfg)
    }

    /// Reads the config without checking `server`, for `register --server` to replace it.
    /// Relative paths are resolved against the config file's directory.
    pub fn load_unchecked(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {} (run `init` first?)", path.display()))?;
        let mut cfg: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let dir = std::path::absolute(dir)
            .with_context(|| format!("resolving the directory of {}", path.display()))?;
        if cfg.melee.components().count() > 1 {
            cfg.melee = dir.join(&cfg.melee);
        }
        cfg.iso = dir.join(&cfg.iso);
        cfg.work_dir = dir.join(&cfg.work_dir);
        Ok(cfg)
    }

    /// Writes the config with mode 0600, replacing any file already at `path`.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = HEADER.to_string() + &toml::to_string(self).context("serialising config")?;
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        // Written beside the destination and renamed over it, so a crash never leaves a
        // half-written key file and the secret is never readable by others.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let _ = std::fs::remove_file(&tmp);
        let written = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written.with_context(|| format!("writing {}", path.display()))
    }

    pub fn signing_key(&self) -> anyhow::Result<SigningKey> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(self.secret_key_hex.trim(), &mut bytes)
            .context("secret_key_hex must be 64 hex digits")?;
        Ok(SigningKey::from_bytes(&bytes))
    }
}

/// `$XDG_CONFIG_HOME/autobracket/client.toml`, else `~/.config/autobracket/client.toml`.
pub fn default_path() -> anyhow::Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?.join("autobracket/client.toml"))
}

/// `$XDG_DATA_HOME/autobracket`, else `~/.local/share/autobracket`.
pub fn default_work_dir() -> anyhow::Result<PathBuf> {
    Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?.join("autobracket"))
}

fn xdg_dir(var: &str, under_home: &str) -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .context("HOME is not set")?;
    Ok(PathBuf::from(home).join(under_home))
}

/// Writes a new config at `path` with a fresh key and the default engine arguments, plus
/// [`HEADLESS_ARGS`] in both templates when `headless`. Relative paths are made absolute
/// against the current directory. `engine_build` is the first line of `{melee} version`.
/// Refuses to replace an existing config (and its key) unless `force`.
pub fn init(
    path: &Path,
    melee: &Path,
    iso: &Path,
    work_dir: &Path,
    force: bool,
    headless: bool,
) -> anyhow::Result<ClientConfig> {
    if !force && path.exists() {
        bail!(
            "{} already exists; pass --force to replace it (and its key)",
            path.display()
        );
    }
    let engine_build = engine_build(melee)?;
    let melee = if melee.components().count() > 1 {
        absolute(melee)?
    } else {
        melee.to_path_buf()
    };
    let key = SigningKey::generate(&mut OsRng);
    let mut cfg = ClientConfig::with_defaults(
        hex::encode(key.to_bytes()),
        engine_build,
        melee,
        absolute(iso)?,
        absolute(work_dir)?,
    );
    if headless {
        for args in [&mut cfg.host_args, &mut cfg.join_args] {
            // After the `play` subcommand.
            let at = 1.min(args.len());
            args.splice(at..at, HEADLESS_ARGS.map(String::from));
        }
    }
    cfg.save(path)?;
    Ok(cfg)
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))
}

/// The first line of `{melee} version`.
fn engine_build(melee: &Path) -> anyhow::Result<String> {
    let out = Command::new(melee)
        .arg("version")
        .output()
        .with_context(|| format!("running {} version", melee.display()))?;
    if !out.status.success() {
        bail!("{} version failed: {}", melee.display(), out.status);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        bail!("{} version printed nothing", melee.display());
    }
    Ok(line.to_string())
}

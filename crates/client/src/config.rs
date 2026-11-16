//! The client config file, `~/.config/autobracket/client.toml` unless `--config` names
//! another. It holds the player's secret key, so it is only ever written with mode 0600.
//!
//! Relative `melee`, `iso` and `work_dir` paths in the file are relative to the directory
//! the file is in; they are made absolute when it is loaded. A `melee` that is a bare name
//! (no `/`) is looked up on `PATH` instead.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use proto::SigningKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
# Character select (\"--np-css\") can crash both games: an engine bug in the name-entry
# screen. Engine builds that include branch port/np-nameentry (or later) fix it. With an
# older build, use preset fighters and stage: `autobracket-client init --match
# FIGHTER[,STAGE]` writes them, or replace \"--np-css\", \"--quit-after-matches\", \"1\"
# by hand with
#   host_args: \"--match\", \"<fighter>,<stage>\",   e.g. \"--match\", \"falcon,32\",
#   join_args: \"--match\", \"<fighter>\",           e.g. \"--match\", \"fox\",
# A preset is the same fighter (and, for the host, stage) in every game of every set.
#
# \"--verify-disc\" makes the engine check the disc image before it starts and exit 4 if it
# is not NTSC 1.02 Melee; `play` then stops with a message instead of retrying.
#
# Opponent no-show: the engine stops hosting when nobody joins within its --np-timeout
# (60 s unless you add \"--np-timeout\", \"SECS\"); after two such runs in a row `play`
# reports a no-show to the organiser. A guest that gets no room code for
# no_code_wait_secs reports one too (0 turns that off).
#
# Optional host_args for engine builds that include branch port/np-nameentry or later
# (an older build ignores flags it does not know):
#   \"--np-join-wait\", \"120\",   stop hosting after 120 s with nobody joined; the engine
#                               then prints \"play: netplay: no opponent joined\" (exit 5)
#   \"--np-no-name-entry\",       with \"--np-css\": no name-entry screen on either peer
#
# allow_direct_connect = true lets `play` join an opponent's HOST:PORT instead of a
# match code (only for LAN events whose server allows it).

";

/// The arguments `init --headless` adds, for machines with no display or sound.
pub const HEADLESS_ARGS: [&str; 2] = ["--headless", "--no-audio"];

/// Picks the room code out of the host's stderr line
/// `play: netplay host: MATCH CODE K7QX (give it to the other player)`.
pub const DEFAULT_CODE_REGEX: &str = "MATCH CODE ([A-Z0-9]+)";

/// The smallest file accepted as a disc image (the same floor the server's verifier uses).
pub const MIN_DISC_BYTES: u64 = 1 << 30;

/// How long a guest waits for a room code before reporting a no-show, by default.
pub const DEFAULT_NO_CODE_WAIT_SECS: u64 = 600;

fn default_no_code_wait() -> u64 {
    DEFAULT_NO_CODE_WAIT_SECS
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct ClientConfig {
    /// Base URL of the bracket server; empty until `register --server`.
    pub server: String,
    pub secret_key_hex: String,
    /// The `melee` engine binary.
    pub melee: PathBuf,
    pub iso: PathBuf,
    /// Each game runs in a fresh `{work_dir}/run-{n}/`, kept afterwards.
    pub work_dir: PathBuf,
    /// Engine arguments; see [`crate::engine::CommandEngine`] for the placeholders.
    pub host_args: Vec<String>,
    pub join_args: Vec<String>,
    pub code_regex: String,
    /// A guest that has had no room code for this long reports a no-show; 0 never does.
    #[serde(default = "default_no_code_wait")]
    pub no_code_wait_secs: u64,
    /// Accept a `HOST:PORT` room code (direct connect) from the server, not only a match code.
    #[serde(default)]
    pub allow_direct_connect: bool,
}

impl fmt::Debug for ClientConfig {
    /// Like a derived `Debug`, with the secret key redacted.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("server", &self.server)
            .field("secret_key_hex", &"<redacted>")
            .field("melee", &self.melee)
            .field("iso", &self.iso)
            .field("work_dir", &self.work_dir)
            .field("host_args", &self.host_args)
            .field("join_args", &self.join_args)
            .field("code_regex", &self.code_regex)
            .field("no_code_wait_secs", &self.no_code_wait_secs)
            .field("allow_direct_connect", &self.allow_direct_connect)
            .finish()
    }
}

/// `play --iso {iso} --verify-disc --netplay host --np-server {relay} --record {mrpl} --slp
/// auto --np-css --quit-after-matches 1 --stocks {stocks} --time-secs {timer_secs}`.
pub fn default_host_args() -> Vec<String> {
    engine_args(&["host"])
}

/// [`default_host_args`] with `--netplay join {code}`.
pub fn default_join_args() -> Vec<String> {
    engine_args(&["join", "{code}"])
}

fn engine_args(netplay: &[&str]) -> Vec<String> {
    let mut args = vec!["play", "--iso", "{iso}", "--verify-disc", "--netplay"];
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

/// `init --match FIGHTER[,STAGE]`: the player's fixed fighter and, when hosting, the stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    pub fighter: String,
    pub stage: Option<String>,
}

impl Preset {
    /// Parses `FIGHTER[,STAGE]`: names as the engine's `--match` takes them (`falcon`,
    /// `fox`, ...), the stage by external id or name (`32`, `fd`, `battlefield`, ...).
    pub fn parse(spec: &str) -> anyhow::Result<Self> {
        let mut parts = spec.split(',').map(str::trim);
        let fighter = parts.next().unwrap_or("");
        let stage = parts.next();
        if parts.next().is_some() {
            bail!("--match {spec:?}: expected FIGHTER or FIGHTER,STAGE");
        }
        // Never starting with `-`: the engine looks its flags up anywhere in its arguments,
        // so a value spelled like a flag would act as one.
        let fine = |s: &str, extra: &str| {
            !s.is_empty()
                && s.len() <= 40
                && !s.starts_with('-')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_:.-".contains(c) || extra.contains(c))
        };
        if !fine(fighter, "") {
            bail!("--match {spec:?}: {fighter:?} is not a fighter name");
        }
        if let Some(stage) = stage.filter(|s| !fine(s, " '")) {
            bail!("--match {spec:?}: {stage:?} is not a stage id or name");
        }
        Ok(Preset {
            fighter: fighter.to_string(),
            stage: stage.map(String::from),
        })
    }

    /// The `--match` value for the host (`FIGHTER,STAGE`, or just `FIGHTER` for the
    /// engine's default stage, Final Destination) or the guest (`FIGHTER`: the host's stage
    /// is played).
    fn arg(&self, host: bool) -> String {
        match (&self.stage, host) {
            (Some(stage), true) => format!("{},{stage}", self.fighter),
            _ => self.fighter.clone(),
        }
    }
}

/// Replaces the character select (`--np-css --quit-after-matches 1`) in `args` with
/// `--match` for `preset`.
fn apply_preset(args: &mut Vec<String>, preset: &Preset, host: bool) {
    if let Some(i) = args
        .iter()
        .position(|a| a == "--quit-after-matches")
        .filter(|&i| i + 1 < args.len())
    {
        args.drain(i..i + 2);
    }
    let value = preset.arg(host);
    match args.iter().position(|a| a == "--np-css") {
        Some(i) => {
            args.splice(i..i + 1, ["--match".to_string(), value]);
        }
        None => args.extend(["--match".to_string(), value]),
    }
}

impl ClientConfig {
    /// A config with the default engine arguments and no server yet.
    pub fn with_defaults(
        secret_key_hex: String,
        melee: PathBuf,
        iso: PathBuf,
        work_dir: PathBuf,
    ) -> Self {
        ClientConfig {
            server: String::new(),
            secret_key_hex,
            melee,
            iso,
            work_dir,
            host_args: default_host_args(),
            join_args: default_join_args(),
            code_regex: DEFAULT_CODE_REGEX.into(),
            no_code_wait_secs: DEFAULT_NO_CODE_WAIT_SECS,
            allow_direct_connect: false,
        }
    }

    /// Whether either template starts the game on the netplay character select.
    pub fn uses_character_select(&self) -> bool {
        self.host_args
            .iter()
            .chain(&self.join_args)
            .any(|a| a == "--np-css")
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

/// What `init` writes.
pub struct InitOptions<'a> {
    pub melee: &'a Path,
    pub iso: &'a Path,
    pub work_dir: &'a Path,
    /// Replace an existing config, and its key.
    pub force: bool,
    /// Add [`HEADLESS_ARGS`] to both templates.
    pub headless: bool,
    /// Preset fighters and stage instead of the character select.
    pub preset: Option<&'a Preset>,
}

/// Writes a new config at `path` with a fresh key and the default engine arguments (adjusted
/// by `opts`). Relative paths are made absolute against the current directory; the work
/// directory is created and stored resolved ([`resolve_work_dir`]). Checks first
/// that the disc image looks like one ([`check_disc`]) and that `{melee} version` runs.
/// Refuses to replace an existing config (and its key) unless `opts.force`.
pub fn init(path: &Path, opts: &InitOptions) -> anyhow::Result<ClientConfig> {
    if !opts.force && path.exists() {
        bail!(
            "{} already exists; pass --force to replace it (and its key)",
            path.display()
        );
    }
    check_disc(opts.iso)?;
    version_line(opts.melee)?;
    let melee = if opts.melee.components().count() > 1 {
        absolute(opts.melee)?
    } else {
        opts.melee.to_path_buf()
    };
    let key = SigningKey::generate(&mut OsRng);
    let mut cfg = ClientConfig::with_defaults(
        hex::encode(key.to_bytes()),
        melee,
        absolute(opts.iso)?,
        resolve_work_dir(opts.work_dir)?,
    );
    if opts.headless {
        for args in [&mut cfg.host_args, &mut cfg.join_args] {
            // After the `play` subcommand.
            let at = 1.min(args.len());
            args.splice(at..at, HEADLESS_ARGS.map(String::from));
        }
    }
    if let Some(preset) = opts.preset {
        apply_preset(&mut cfg.host_args, preset, true);
        apply_preset(&mut cfg.join_args, preset, false);
    }
    cfg.save(path)?;
    Ok(cfg)
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))
}

/// Creates the work directory `path` if needed and returns its canonical path (absolute,
/// with no symlinks and no `.` or `..`), the form the kernel reports a game's working
/// directory in.
pub fn resolve_work_dir(path: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
    std::fs::canonicalize(path).with_context(|| format!("resolving {}", path.display()))
}

/// Fails with a message naming `iso` unless it is a regular file (after symlinks) of at least
/// [`MIN_DISC_BYTES`] that can be read.
pub fn check_disc(iso: &Path) -> anyhow::Result<()> {
    let hint = "set `iso` to your own NTSC 1.02 Melee disc image (a plain .iso)";
    let meta =
        std::fs::metadata(iso).with_context(|| format!("disc image {}: {hint}", iso.display()))?;
    if !meta.is_file() {
        bail!("disc image {} is not a regular file: {hint}", iso.display());
    }
    if meta.len() < MIN_DISC_BYTES {
        bail!(
            "disc image {} is only {} bytes, too small for a Melee disc image: {hint}",
            iso.display(),
            meta.len()
        );
    }
    let mut head = [0u8; 8];
    std::fs::File::open(iso)
        .and_then(|mut f| f.read(&mut head))
        .with_context(|| format!("reading disc image {}", iso.display()))?;
    Ok(())
}

/// The first line of `{melee} version`.
fn version_line(melee: &Path) -> anyhow::Result<String> {
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

/// The engine build as the server's verifier records it: the first line of
/// `{melee} version`, `+`, and the first 12 hex digits of the binary's SHA-256. Computed
/// afresh each time, so it follows an engine update.
pub fn engine_build(melee: &Path) -> anyhow::Result<String> {
    let line = version_line(melee)?;
    let binary = find_program(melee)?;
    let mut file =
        std::fs::File::open(&binary).with_context(|| format!("opening {}", binary.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("hashing {}", binary.display()))?;
    let digest = hasher.finalize();
    Ok(format!("{line}+{}", hex::encode(&digest[..6])))
}

/// `melee` itself when it names a path, else the first file of that name on `PATH`.
fn find_program(melee: &Path) -> anyhow::Result<PathBuf> {
    if melee.components().count() > 1 {
        return Ok(melee.to_path_buf());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(melee))
        .find(|p| p.is_file())
        .with_context(|| format!("{} is not on PATH", melee.display()))
}

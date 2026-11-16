//! Server configuration, read from TOML. Every field has a default except `to_keys`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use proto::Ruleset;
use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_replay_dir")]
    pub replay_dir: PathBuf,
    #[serde(default = "default_relay")]
    pub relay: String,
    #[serde(default)]
    pub require_invite: bool,
    /// Tournament organiser public keys, lowercase hex.
    pub to_keys: Vec<String>,
    #[serde(default)]
    pub verifier_keys: Vec<String>,
    /// Accepted client engine builds; empty accepts any.
    #[serde(default)]
    pub accepted_builds: Vec<String>,
    #[serde(default = "default_upload_timeout_secs")]
    pub upload_timeout_secs: u64,
    #[serde(default)]
    pub strict_resim: bool,
    /// Largest accepted request body, in bytes.
    #[serde(default = "default_max_replay_bytes")]
    pub max_replay_bytes: usize,
    /// Sustained requests per second allowed from one IP address.
    #[serde(default = "default_rate_per_sec")]
    pub rate_per_sec: u64,
    /// Requests one IP address may make in a burst before being limited.
    #[serde(default = "default_rate_burst")]
    pub rate_burst: u32,
    #[serde(default)]
    pub verifier: VerifierConfig,
    #[serde(default)]
    pub ruleset: Ruleset,
    /// Runs the background judging worker. Off only in tests that drive judging themselves.
    #[serde(default = "default_worker_enabled")]
    pub worker_enabled: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct VerifierConfig {
    pub mode: VerifierMode,
    pub engine: PathBuf,
    pub iso: PathBuf,
    pub timeout_secs: u64,
    pub jobs: usize,
}

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerifierMode {
    InProcess,
    External,
    #[default]
    Off,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        VerifierConfig {
            mode: VerifierMode::Off,
            engine: PathBuf::new(),
            iso: PathBuf::new(),
            timeout_secs: 120,
            jobs: 2,
        }
    }
}

impl Config {
    /// A configuration with every defaulted field at its default.
    pub fn new(to_keys: Vec<String>) -> Self {
        Config {
            bind: default_bind(),
            db_path: default_db_path(),
            replay_dir: default_replay_dir(),
            relay: default_relay(),
            require_invite: false,
            to_keys,
            verifier_keys: Vec::new(),
            accepted_builds: Vec::new(),
            upload_timeout_secs: default_upload_timeout_secs(),
            strict_resim: false,
            max_replay_bytes: default_max_replay_bytes(),
            rate_per_sec: default_rate_per_sec(),
            rate_burst: default_rate_burst(),
            verifier: VerifierConfig::default(),
            ruleset: Ruleset::default(),
            worker_enabled: default_worker_enabled(),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

fn default_bind() -> String {
    "0.0.0.0:8080".into()
}

fn default_db_path() -> PathBuf {
    "autobracket.sqlite".into()
}

fn default_replay_dir() -> PathBuf {
    "replays".into()
}

fn default_relay() -> String {
    "netplay.gsd.sh:51600".into()
}

fn default_upload_timeout_secs() -> u64 {
    120
}

fn default_max_replay_bytes() -> usize {
    16 * 1024 * 1024
}

fn default_rate_per_sec() -> u64 {
    20
}

fn default_rate_burst() -> u32 {
    40
}

fn default_worker_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_toml_gets_defaults() {
        let cfg: Config = toml::from_str(r#"to_keys = ["ab"]"#).unwrap();
        assert_eq!(cfg.to_keys, vec!["ab".to_string()]);
        assert_eq!(cfg.bind, "0.0.0.0:8080");
        assert_eq!(cfg.relay, "netplay.gsd.sh:51600");
        assert_eq!(cfg.upload_timeout_secs, 120);
        assert_eq!(cfg.max_replay_bytes, 16 * 1024 * 1024);
        assert_eq!((cfg.rate_per_sec, cfg.rate_burst), (20, 40));
        assert_eq!(cfg.verifier.mode, VerifierMode::Off);
        assert_eq!((cfg.verifier.timeout_secs, cfg.verifier.jobs), (120, 2));
        assert!(cfg.accepted_builds.is_empty());
        assert_eq!(cfg.ruleset, Ruleset::default());
        assert!(cfg.worker_enabled);
    }

    #[test]
    fn to_keys_is_required() {
        assert!(toml::from_str::<Config>("bind = \"127.0.0.1:1\"").is_err());
    }

    #[test]
    fn verifier_section_parses() {
        let cfg: Config = toml::from_str(
            "to_keys = []\n[verifier]\nmode = \"in_process\"\njobs = 4\n[ruleset]\nstocks = 3\n",
        )
        .unwrap();
        assert_eq!(cfg.verifier.mode, VerifierMode::InProcess);
        assert_eq!(cfg.verifier.jobs, 4);
        assert_eq!(cfg.verifier.timeout_secs, 120);
        assert_eq!(cfg.ruleset.stocks, 3);
    }
}

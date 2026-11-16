//! Server configuration, read from TOML. Every field has a default except `to_keys`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use proto::Ruleset;
use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
        Self::parse(&text).with_context(|| format!("loading {}", path.display()))
    }

    fn parse(text: &str) -> anyhow::Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Rejects values the server cannot run with, naming the offending field.
    pub fn validate(&self) -> anyhow::Result<()> {
        let r = &self.ruleset;
        for (field, n) in [
            ("ruleset.best_of", r.best_of),
            ("ruleset.best_of_finals", r.best_of_finals),
        ] {
            if n % 2 == 0 {
                bail!("{field} must be an odd number of games, not {n}");
            }
        }
        if r.stocks == 0 {
            bail!("ruleset.stocks must be at least 1");
        }
        if r.legal_stages.is_empty() {
            bail!("ruleset.legal_stages must list at least one stage");
        }
        for (field, keys) in [
            ("to_keys", &self.to_keys),
            ("verifier_keys", &self.verifier_keys),
        ] {
            if let Some(bad) = keys.iter().find(|k| !is_public_key_hex(k)) {
                bail!("{field}: {bad:?} is not a public key (64 lowercase hex characters)");
            }
        }
        if self.rate_per_sec == 0 {
            bail!("rate_per_sec must be at least 1");
        }
        if self.rate_burst == 0 {
            bail!("rate_burst must be at least 1");
        }
        Ok(())
    }
}

/// 64 lowercase hex characters: the form keys are compared in.
fn is_public_key_hex(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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

    const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    /// The full error chain of parsing `to_keys = [KEY]` plus `extra`.
    fn rejection(extra: &str) -> String {
        let text = format!("to_keys = [\"{KEY}\"]\n{extra}");
        format!("{:#}", Config::parse(&text).expect_err(extra))
    }

    #[test]
    fn valid_config_parses() {
        let cfg = Config::parse(&format!(
            "to_keys = [\"{KEY}\"]\nverifier_keys = [\"{KEY}\"]\n"
        ))
        .unwrap();
        assert_eq!(cfg.verifier_keys, vec![KEY.to_string()]);
        Config::new(vec![KEY.into()]).validate().unwrap();
    }

    #[test]
    fn rejects_bad_best_of() {
        for (extra, field) in [
            ("[ruleset]\nbest_of = 0", "ruleset.best_of"),
            ("[ruleset]\nbest_of = 4", "ruleset.best_of"),
            ("[ruleset]\nbest_of_finals = 0", "ruleset.best_of_finals"),
            ("[ruleset]\nbest_of_finals = 6", "ruleset.best_of_finals"),
        ] {
            let err = rejection(extra);
            assert!(err.contains(field), "{extra:?}: {err}");
        }
    }

    #[test]
    fn rejects_zero_stocks() {
        let err = rejection("[ruleset]\nstocks = 0");
        assert!(err.contains("ruleset.stocks"), "{err}");
    }

    #[test]
    fn rejects_empty_legal_stages() {
        let err = rejection("[ruleset]\nlegal_stages = []");
        assert!(err.contains("ruleset.legal_stages"), "{err}");
    }

    #[test]
    fn rejects_malformed_keys() {
        let upper = KEY.to_uppercase();
        let short = &KEY[..62];
        let long = format!("{KEY}00");
        let not_hex = format!("{}zz", &KEY[..62]);
        for bad in [upper.as_str(), short, long.as_str(), not_hex.as_str(), ""] {
            let text = format!("to_keys = [\"{bad}\"]\n");
            let err = format!("{:#}", Config::parse(&text).expect_err(bad));
            assert!(err.contains("to_keys"), "{bad:?}: {err}");

            let err = rejection(&format!("verifier_keys = [\"{KEY}\", \"{bad}\"]"));
            assert!(err.contains("verifier_keys"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn rejects_zero_rates() {
        let err = rejection("rate_per_sec = 0");
        assert!(err.contains("rate_per_sec"), "{err}");
        let err = rejection("rate_burst = 0");
        assert!(err.contains("rate_burst"), "{err}");
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = rejection("require_invites = true");
        assert!(err.contains("require_invites"), "{err}");
        let err = rejection("[verifier]\nmodee = \"off\"");
        assert!(err.contains("modee"), "{err}");
    }

    #[test]
    fn load_validates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autobracket.toml");
        std::fs::write(&path, format!("to_keys = [\"{KEY}\"]\nrate_burst = 0\n")).unwrap();
        let err = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(err.contains("rate_burst"), "{err}");
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

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
    /// Tournament organiser public keys, lowercase hex. Must not be empty unless
    /// `allow_no_organiser` is set.
    pub to_keys: Vec<String>,
    /// Lets the server start with no organiser key (nobody can create or run events). Only
    /// for trying the server out.
    #[serde(default)]
    pub allow_no_organiser: bool,
    #[serde(default)]
    pub verifier_keys: Vec<String>,
    /// Accepted engine builds, for clients at sign-up and for the verifier; empty accepts any.
    /// See [`Config::build_accepted`].
    #[serde(default)]
    pub accepted_builds: Vec<String>,
    #[serde(default = "default_upload_timeout_secs")]
    pub upload_timeout_secs: u64,
    /// Seconds a `ready` set may go without activity (a room code, an upload, a counted game)
    /// before the judging worker sends it to the organiser queue as `stalled`. 0 disables.
    #[serde(default = "default_set_stall_secs")]
    pub set_stall_secs: u64,
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
    /// Runs the background judging worker. Not settable from the configuration file: only
    /// tests that drive judging themselves turn it off, in code.
    #[serde(skip_deserializing, default = "default_worker_enabled")]
    pub worker_enabled: bool,
    // --- API surface (final fix wave, engineer B) ---
    /// Most connections served at once; further ones wait to be accepted.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Reverse proxies (CIDRs) whose `X-Forwarded-For` names the client. Empty: the TCP peer
    /// is the client.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// Seconds after a room code is posted during which a `no-show` report is refused.
    #[serde(default = "default_no_show_grace_secs")]
    pub no_show_grace_secs: u64,
    /// Accept `host:port` room codes (direct connection, for LAN events).
    #[serde(default)]
    pub allow_direct_connect: bool,
    /// The live stream the `/stream` page embeds (an `http` or `https` URL with a host, e.g.
    /// a MediaMTX WebRTC viewer). Only its origin ([`Config::stream_origin`]) may be framed.
    #[serde(default)]
    pub stream_url: Option<String>,
    /// Web sign-in through Zitadel and oauth2-proxy (`/account/`, `/admin/`, the Zitadel
    /// hooks). Absent: those routes are 404.
    #[serde(default)]
    pub identity: Option<IdentityConfig>,
    /// Who runs this server, for the privacy and terms pages.
    #[serde(default)]
    pub site: SiteConfig,
}

/// The `[identity]` table. Secrets (`proxy_secret`, the hook keys) never appear in `Debug`.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    /// `https://host[:port]`: the site's own origin, compared with `Origin` on writes.
    pub public_origin: String,
    /// `https://host[:port]`: the ID token's `iss`.
    pub issuer: String,
    /// The ID token's `aud` must contain it.
    pub client_id: String,
    /// The issuer's JWKS; default [`IdentityConfig::jwks_url`].
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// Groups come only from grants of this Zitadel project and organisation.
    pub project_id: String,
    pub org_id: String,
    pub discord_idp_id: String,
    /// Sent by nginx as `X-Autobracket-Proxy` on every request; at least 32 characters.
    pub proxy_secret: String,
    #[serde(default = "default_admin_group")]
    pub admin_group: String,
    /// How old (seconds) the sign-in behind an admin request may be.
    #[serde(default = "default_admin_max_auth_age_secs")]
    pub admin_max_auth_age_secs: u64,
    pub sign_out_url: String,
    /// Shown on the admin 403 for a sign-in without a second factor.
    pub mfa_setup_url: String,
    /// Peers (CIDRs) allowed to call `/hooks/zitadel/*`; empty allows any.
    #[serde(default)]
    pub hook_peers: Vec<String>,
    pub hook_keys: HookKeys,
}

/// The signing keys of the Zitadel Actions v2 targets, one per hook.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct HookKeys {
    pub idp_intent: String,
    pub token: String,
    pub user_events: String,
}

impl std::fmt::Debug for IdentityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityConfig")
            .field("public_origin", &self.public_origin)
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("jwks_url", &self.jwks_url)
            .field("project_id", &self.project_id)
            .field("org_id", &self.org_id)
            .field("discord_idp_id", &self.discord_idp_id)
            .field("proxy_secret", &"<redacted>")
            .field("admin_group", &self.admin_group)
            .field("admin_max_auth_age_secs", &self.admin_max_auth_age_secs)
            .field("sign_out_url", &self.sign_out_url)
            .field("mfa_setup_url", &self.mfa_setup_url)
            .field("hook_peers", &self.hook_peers)
            .field("hook_keys", &self.hook_keys)
            .finish()
    }
}

impl std::fmt::Debug for HookKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HookKeys { <redacted> }")
    }
}

impl IdentityConfig {
    /// `jwks_url`, or by default `<issuer>/oauth/v2/keys`.
    pub fn jwks_url(&self) -> String {
        self.jwks_url
            .clone()
            .unwrap_or_else(|| format!("{}/oauth/v2/keys", self.issuer))
    }

    /// Rejects values the identity layer cannot run with, naming the field.
    fn validate(&self) -> anyhow::Result<()> {
        for (field, value) in [
            ("public_origin", &self.public_origin),
            ("issuer", &self.issuer),
        ] {
            if !is_https_origin(value) {
                bail!(
                    "identity.{field} must be an https origin with no path, \
                     e.g. \"https://bracket.example.org\""
                );
            }
        }
        if let Some(url) = &self.jwks_url {
            if !is_jwks_url(url) {
                bail!(
                    "identity.jwks_url must be an https URL (plain http only to a loopback \
                     address)"
                );
            }
        }
        if self.proxy_secret.chars().count() < MIN_PROXY_SECRET_CHARS {
            bail!("identity.proxy_secret must be at least {MIN_PROXY_SECRET_CHARS} characters");
        }
        for (field, value) in [
            ("client_id", &self.client_id),
            ("project_id", &self.project_id),
            ("org_id", &self.org_id),
            ("discord_idp_id", &self.discord_idp_id),
            ("admin_group", &self.admin_group),
            ("hook_keys.idp_intent", &self.hook_keys.idp_intent),
            ("hook_keys.token", &self.hook_keys.token),
            ("hook_keys.user_events", &self.hook_keys.user_events),
        ] {
            if value.trim().is_empty() {
                bail!("identity.{field} must not be empty");
            }
        }
        crate::net::parse_cidrs(&self.hook_peers).context("identity.hook_peers")?;
        Ok(())
    }
}

/// The `[site]` table: shown on the privacy and terms pages.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    pub operator: Option<String>,
    pub contact: Option<String>,
}

/// Shortest accepted `identity.proxy_secret`.
const MIN_PROXY_SECRET_CHARS: usize = 32;

/// `https://host[:port]` exactly as a browser sends it in `Origin`: lowercase, no default
/// port, no path (not even `/`), credentials, query or fragment.
fn is_https_origin(text: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(text) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.origin().ascii_serialization() == text
}

/// An https URL with a host, or an http one to a loopback address (a JWKS on the same host).
fn is_jwks_url(text: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(text) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let loopback = host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" => true,
        "http" => loopback,
        _ => false,
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct VerifierConfig {
    pub mode: VerifierMode,
    pub engine: PathBuf,
    pub iso: PathBuf,
    pub timeout_secs: u64,
    /// Games judged at the same time (each may wait on one re-simulation). At least 1.
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

impl VerifierMode {
    /// The mode as written in the configuration file.
    pub fn as_str(self) -> &'static str {
        match self {
            VerifierMode::InProcess => "in_process",
            VerifierMode::External => "external",
            VerifierMode::Off => "off",
        }
    }
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
            allow_no_organiser: false,
            verifier_keys: Vec::new(),
            accepted_builds: Vec::new(),
            upload_timeout_secs: default_upload_timeout_secs(),
            set_stall_secs: default_set_stall_secs(),
            strict_resim: false,
            max_replay_bytes: default_max_replay_bytes(),
            rate_per_sec: default_rate_per_sec(),
            rate_burst: default_rate_burst(),
            verifier: VerifierConfig::default(),
            ruleset: Ruleset::default(),
            worker_enabled: default_worker_enabled(),
            max_connections: default_max_connections(),
            trusted_proxies: Vec::new(),
            no_show_grace_secs: default_no_show_grace_secs(),
            allow_direct_connect: false,
            stream_url: None,
            identity: None,
            site: SiteConfig::default(),
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
        if let Err(e) = self.ruleset.validate() {
            bail!("ruleset.{e}");
        }
        for (field, keys) in [
            ("to_keys", &self.to_keys),
            ("verifier_keys", &self.verifier_keys),
        ] {
            if let Some(bad) = keys.iter().find(|k| !is_public_key_hex(k)) {
                bail!("{field}: {bad:?} is not a public key (64 lowercase hex characters)");
            }
        }
        if self.to_keys.is_empty() && !self.allow_no_organiser {
            bail!(
                "to_keys is empty, so nobody could create or run an event: add your organiser \
                 public key (from `autobracket keygen`), or set allow_no_organiser = true"
            );
        }
        if self.rate_per_sec == 0 {
            bail!("rate_per_sec must be at least 1");
        }
        if self.rate_burst == 0 {
            bail!("rate_burst must be at least 1");
        }
        if self.upload_timeout_secs == 0 {
            bail!("upload_timeout_secs must be at least 1");
        }
        let v = &self.verifier;
        if v.mode != VerifierMode::InProcess {
            for (field, path) in [("verifier.engine", &v.engine), ("verifier.iso", &v.iso)] {
                if !path.as_os_str().is_empty() {
                    bail!(
                        "{field} is set but verifier.mode is \"{}\": the engine runs here only \
                         with mode = \"in_process\" (remove {field}, or set the mode)",
                        v.mode.as_str()
                    );
                }
            }
        }
        if v.jobs == 0 {
            bail!("verifier.jobs must be at least 1");
        }
        self.validate_api()?;
        if let Some(identity) = &self.identity {
            if self.trusted_proxies.is_empty() {
                bail!(
                    "trusted_proxies must name the reverse proxy when [identity] is set: \
                     identity headers are accepted only from it"
                );
            }
            identity.validate()?;
        }
        Ok(())
    }

    /// The API surface keys: `max_connections`, `trusted_proxies`.
    fn validate_api(&self) -> anyhow::Result<()> {
        if self.max_connections == 0 {
            bail!("max_connections must be at least 1");
        }
        crate::net::parse_cidrs(&self.trusted_proxies).context("trusted_proxies")?;
        if let Some(url) = &self.stream_url {
            if parse_stream_url(url).is_none() {
                bail!(
                    "stream_url must be an http or https URL with a host, \
                     e.g. \"https://live.example.org/melee/\""
                );
            }
        }
        Ok(())
    }

    /// `scheme://host[:port]` of `stream_url`, the one origin the stream page may frame; `None`
    /// when no stream is configured. Lowercase, ASCII, without a default port.
    pub fn stream_origin(&self) -> Option<String> {
        self.stream_url
            .as_deref()
            .and_then(parse_stream_url)
            .map(|url| url.origin().ascii_serialization())
    }

    /// Whether `build` is on `accepted_builds` (always, when the list is empty). An entry
    /// matches when it equals the whole build string, or the part before its first `+` (the
    /// version, without the binary's hash).
    pub fn build_accepted(&self, build: &str) -> bool {
        let version = build.split_once('+').map_or(build, |(v, _)| v);
        self.accepted_builds.is_empty()
            || self
                .accepted_builds
                .iter()
                .any(|a| a == build || a == version)
    }
}

/// `text` as the URL of a stream viewer, or `None` unless it is an `http` or `https` URL with a
/// host and nothing else before the path (no credentials) whose origin is plain enough to go
/// into a CSP directive as it is: ASCII letters, digits, `.`, `-`, `:`, `/` and IPv6 brackets.
fn parse_stream_url(text: &str) -> Option<reqwest::Url> {
    // The URL parser forgives `https:///host` (and more slashes); the organiser meant a typo.
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))?;
    if rest.is_empty() || rest.starts_with('/') {
        return None;
    }
    let url = reqwest::Url::parse(text).ok()?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let origin = url.origin();
    let plain = origin.is_tuple()
        && origin
            .ascii_serialization()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:/[]".contains(&b));
    plain.then_some(url)
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

fn default_set_stall_secs() -> u64 {
    1800
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

fn default_max_connections() -> usize {
    512
}

fn default_no_show_grace_secs() -> u64 {
    90
}

fn default_admin_group() -> String {
    "autobracket-admins".into()
}

fn default_admin_max_auth_age_secs() -> u64 {
    43_200
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
        assert_eq!(cfg.set_stall_secs, 1800);
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
    fn rejects_bad_gecko_hashes_and_ruleset_typos() {
        let err = rejection("[ruleset]\nallowed_gecko = [\"ABC\"]");
        assert!(err.contains("ruleset.allowed_gecko"), "{err}");
        let err = rejection("[ruleset]\nbest_of_final = 5");
        assert!(err.contains("best_of_final"), "{err}");
    }

    #[test]
    fn rejects_engine_or_iso_unless_in_process() {
        for mode in ["off", "external"] {
            for field in ["engine", "iso"] {
                let err = rejection(&format!(
                    "[verifier]\nmode = \"{mode}\"\n{field} = \"/opt/melee\""
                ));
                assert!(err.contains(&format!("verifier.{field}")), "{err}");
                assert!(err.contains(mode), "{err}");
            }
        }
        // The mode defaults to off, so this is the TO who forgot to set it.
        let err = rejection("[verifier]\nengine = \"/opt/melee\"");
        assert!(
            err.contains("verifier.engine") && err.contains("off"),
            "{err}"
        );
        Config::parse(&format!(
            "to_keys = [\"{KEY}\"]\n[verifier]\nmode = \"in_process\"\nengine = \"/e\"\niso = \"/i\"\n"
        ))
        .unwrap();
    }

    #[test]
    fn rejects_zero_upload_timeout_and_jobs() {
        let err = rejection("upload_timeout_secs = 0");
        assert!(err.contains("upload_timeout_secs"), "{err}");
        let err = rejection("[verifier]\njobs = 0");
        assert!(err.contains("verifier.jobs"), "{err}");
    }

    #[test]
    fn empty_to_keys_needs_allow_no_organiser() {
        let err = format!("{:#}", Config::parse("to_keys = []\n").unwrap_err());
        assert!(
            err.contains("to_keys") && err.contains("allow_no_organiser"),
            "{err}"
        );
        let cfg = Config::parse("to_keys = []\nallow_no_organiser = true\n").unwrap();
        assert!(cfg.to_keys.is_empty());
    }

    #[test]
    fn worker_enabled_is_not_a_config_key() {
        let err = rejection("worker_enabled = false");
        assert!(err.contains("worker_enabled"), "{err}");
    }

    #[test]
    fn build_acceptance_matches_whole_or_version_part() {
        let mut cfg = Config::new(vec![KEY.into()]);
        assert!(cfg.build_accepted("anything+0123456789ab"));
        cfg.accepted_builds = vec!["melee 1.2.0".into(), "melee 1.3.0+aaaaaaaaaaaa".into()];
        assert!(cfg.build_accepted("melee 1.2.0"));
        assert!(cfg.build_accepted("melee 1.2.0+0123456789ab"));
        assert!(cfg.build_accepted("melee 1.3.0+aaaaaaaaaaaa"));
        assert!(!cfg.build_accepted("melee 1.3.0+bbbbbbbbbbbb"));
        assert!(!cfg.build_accepted("melee 1.2.1+0123456789ab"));
        assert!(!cfg.build_accepted("melee 1.2"));
    }

    #[test]
    fn api_keys_default_and_parse() {
        let cfg = Config::parse(&format!("to_keys = [\"{KEY}\"]\n")).unwrap();
        assert_eq!(cfg.max_connections, 512);
        assert!(cfg.trusted_proxies.is_empty());
        assert_eq!(cfg.no_show_grace_secs, 90);
        assert!(!cfg.allow_direct_connect);
        let cfg = Config::parse(&format!(
            "to_keys = [\"{KEY}\"]\nmax_connections = 8\ntrusted_proxies = [\"172.16.0.0/12\", \"::1/128\", \"10.0.0.1\"]\nno_show_grace_secs = 0\nallow_direct_connect = true\n"
        ))
        .unwrap();
        assert_eq!(cfg.max_connections, 8);
        assert_eq!(cfg.trusted_proxies.len(), 3);
        assert_eq!(cfg.no_show_grace_secs, 0);
        assert!(cfg.allow_direct_connect);
    }

    #[test]
    fn rejects_bad_api_keys() {
        let err = rejection("max_connections = 0");
        assert!(err.contains("max_connections"), "{err}");
        for bad in [
            "172.16.0.0/33",
            "nonsense",
            "10.0.0.0/",
            "::1/129",
            "1.2.3.4/-1",
        ] {
            let err = rejection(&format!("trusted_proxies = [\"10.0.0.0/8\", \"{bad}\"]"));
            assert!(err.contains("trusted_proxies"), "{bad}: {err}");
            assert!(err.contains(bad), "the entry is named: {err}");
        }
    }

    #[test]
    fn stream_url_parses_and_gives_its_origin() {
        let cfg = Config::parse(&format!(
            "to_keys = [\"{KEY}\"]\nstream_url = \"https://live.example.org:8889/melee/\"\n"
        ))
        .unwrap();
        assert_eq!(
            cfg.stream_origin().as_deref(),
            Some("https://live.example.org:8889")
        );
        assert_eq!(Config::new(vec![KEY.into()]).stream_origin(), None);
    }

    /// The origin is `scheme://host[:port]` and nothing more: no path, no default port, no
    /// credentials, lowercase host; `http` is allowed (a viewer on the LAN).
    #[test]
    fn stream_origin_is_scheme_host_and_port_only() {
        for (url, origin) in [
            (
                "https://live.example.org/melee/",
                "https://live.example.org",
            ),
            (
                "https://Live.Example.org:443/x?y#z",
                "https://live.example.org",
            ),
            ("http://192.168.1.20:8889/melee", "http://192.168.1.20:8889"),
            ("http://[::1]:8889/", "http://[::1]:8889"),
        ] {
            let cfg =
                Config::parse(&format!("to_keys = [\"{KEY}\"]\nstream_url = \"{url}\"\n")).unwrap();
            assert_eq!(cfg.stream_url.as_deref(), Some(url));
            assert_eq!(cfg.stream_origin().as_deref(), Some(origin), "{url}");
        }
    }

    #[test]
    fn rejects_bad_stream_url() {
        for bad in [
            "ftp://live.example.org/",
            "live.example.org/melee",
            "https://",
            "https:///melee",
            "",
            "https://user:pw@live.example.org/",
            "https://user@live.example.org/",
            "https://live.example.org:99999/",
            "https://live example.org/",
            "https://live.example.org;frame-src/",
            "https://live.example.org'/",
            "https://live.example.org\\\"/",
            "data:text/html,hi",
            "javascript:alert(1)",
        ] {
            let err = rejection(&format!("stream_url = \"{bad}\""));
            assert!(err.contains("stream_url"), "{bad}: {err}");
        }
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

    /// A valid `[identity]` configuration, with `trusted_proxies`.
    fn identity_toml() -> String {
        format!(
            "to_keys = [\"{KEY}\"]\n\
             trusted_proxies = [\"172.17.0.1/32\"]\n\
             [identity]\n\
             public_origin = \"https://bracket.example.org\"\n\
             issuer = \"https://auth.example.org\"\n\
             client_id = \"c1\"\n\
             project_id = \"p1\"\n\
             org_id = \"o1\"\n\
             discord_idp_id = \"idp1\"\n\
             proxy_secret = \"0123456789abcdef0123456789abcdef\"\n\
             sign_out_url = \"/oauth2/sign_out\"\n\
             mfa_setup_url = \"https://auth.example.org/mfa\"\n\
             hook_peers = [\"10.203.48.0/28\"]\n\
             [identity.hook_keys]\n\
             idp_intent = \"ki\"\n\
             token = \"kt\"\n\
             user_events = \"ku\"\n"
        )
    }

    /// The error chain of `identity_toml()` with the line starting `key =` replaced by
    /// `line` (removed when `line` is empty).
    fn identity_rejection(key: &str, line: &str) -> String {
        let text: Vec<String> = identity_toml()
            .lines()
            .map(|l| {
                if l.starts_with(&format!("{key} =")) {
                    line.to_owned()
                } else {
                    l.to_owned()
                }
            })
            .collect();
        let text = text.join("\n");
        assert_ne!(text, identity_toml().trim_end(), "{key} not found");
        format!("{:#}", Config::parse(&text).expect_err(line))
    }

    #[test]
    fn identity_parses_with_defaults() {
        let cfg = Config::parse(&identity_toml()).unwrap();
        let id = cfg.identity.as_ref().unwrap();
        assert_eq!(id.admin_group, "autobracket-admins");
        assert_eq!(id.admin_max_auth_age_secs, 43_200);
        assert_eq!(id.jwks_url(), "https://auth.example.org/oauth/v2/keys");
        assert_eq!(id.hook_keys.token, "kt");
        assert_eq!(cfg.site.operator, None);
        let debug = format!("{cfg:?}");
        for secret in [
            "0123456789abcdef0123456789abcdef",
            "\"ki\"",
            "\"kt\"",
            "\"ku\"",
        ] {
            assert!(!debug.contains(secret), "Debug shows {secret}: {debug}");
        }

        let text = identity_toml().replace(
            "[identity.hook_keys]",
            "jwks_url = \"http://127.0.0.1:9/keys\"\n[identity.hook_keys]",
        ) + "[site]\noperator = \"GD\"\ncontact = \"x@example.org\"\n";
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.identity.unwrap().jwks_url(), "http://127.0.0.1:9/keys");
        assert_eq!(cfg.site.operator.as_deref(), Some("GD"));
        assert_eq!(cfg.site.contact.as_deref(), Some("x@example.org"));
        // No identity table: no identity.
        assert!(Config::parse(&format!("to_keys = [\"{KEY}\"]\n"))
            .unwrap()
            .identity
            .is_none());
    }

    #[test]
    fn identity_rejects_bad_values() {
        let secret31 = "x".repeat(31);
        let cases = [
            (
                "proxy_secret",
                format!("proxy_secret = \"{secret31}\""),
                "identity.proxy_secret",
            ),
            (
                "trusted_proxies",
                "trusted_proxies = []".into(),
                "trusted_proxies",
            ),
            ("trusted_proxies", String::new(), "trusted_proxies"),
            (
                "public_origin",
                "public_origin = \"http://x\"".into(),
                "identity.public_origin",
            ),
            (
                "public_origin",
                "public_origin = \"https://bracket.example.org/app\"".into(),
                "identity.public_origin",
            ),
            (
                "public_origin",
                "public_origin = \"https://bracket.example.org/\"".into(),
                "identity.public_origin",
            ),
            (
                "public_origin",
                "public_origin = \"https://Bracket.example.org\"".into(),
                "identity.public_origin",
            ),
            (
                "issuer",
                "issuer = \"https://auth.example.org/oidc\"".into(),
                "identity.issuer",
            ),
            (
                "issuer",
                "issuer = \"auth.example.org\"".into(),
                "identity.issuer",
            ),
            ("client_id", "client_id = \"\"".into(), "identity.client_id"),
            (
                "project_id",
                "project_id = \"\"".into(),
                "identity.project_id",
            ),
            ("org_id", "org_id = \" \"".into(), "identity.org_id"),
            (
                "discord_idp_id",
                "discord_idp_id = \"\"".into(),
                "identity.discord_idp_id",
            ),
            (
                "idp_intent",
                "idp_intent = \"\"".into(),
                "identity.hook_keys.idp_intent",
            ),
            ("token", "token = \"\"".into(), "identity.hook_keys.token"),
            (
                "user_events",
                "user_events = \"\"".into(),
                "identity.hook_keys.user_events",
            ),
            (
                "hook_peers",
                "hook_peers = [\"10.0.0.0/8\", \"10.0.0.0/33\"]".into(),
                "identity.hook_peers",
            ),
            ("proxy_secret", String::new(), "proxy_secret"),
            ("hook_peers", "hook_peerz = []".into(), "hook_peerz"),
        ];
        for (key, line, field) in cases {
            let err = identity_rejection(key, &line);
            assert!(err.contains(field), "{line:?}: {err}");
        }
        for bad in [
            "https://",
            "ftp://auth.example.org/keys",
            "http://auth.example.org/keys",
            "http://10.0.0.1/keys",
            "https://user:pw@auth.example.org/keys",
        ] {
            let text = identity_toml().replace(
                "[identity.hook_keys]",
                &format!("jwks_url = \"{bad}\"\n[identity.hook_keys]"),
            );
            let err = format!("{:#}", Config::parse(&text).expect_err(bad));
            assert!(err.contains("identity.jwks_url"), "{bad}: {err}");
        }
    }
}

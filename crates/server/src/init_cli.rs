//! `autobracket init`: one command from nothing to a working server directory. It writes a new
//! organiser key, a configuration that names it, and an empty replay directory, then prints the
//! next steps with the real paths. The configuration is checked with the server's own
//! [`Config::validate`] before anything is written, and loaded back with [`Config::load`] before
//! it replaces anything.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use autobracket_server::config::VerifierMode;
use autobracket_server::Config;
use clap::Args;
use proto::SigningKey;
use serde::Serialize;

const KEY_FILE: &str = "organiser.key";
const CONFIG_FILE: &str = "autobracket.toml";
const DB_FILE: &str = "autobracket.sqlite";
const REPLAY_DIR: &str = "replays";

#[derive(Args)]
pub struct InitArgs {
    /// Directory for the key, configuration, database and replays. Created (mode 0700) if it
    /// does not exist.
    #[arg(long, value_name = "DIR")]
    dir: PathBuf,
    /// Address and port the server listens on. Inside a container, use 0.0.0.0:8080.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8080")]
    bind: String,
    /// The http(s) address players and the admin CLI will use, e.g.
    /// https://bracket.example.org. Only used in the printed next steps.
    #[arg(long, value_name = "URL")]
    public_url: Option<String>,
    /// Address range (CIDR) of a reverse proxy whose X-Forwarded-For names the client
    /// (`trusted_proxies`). Repeat for several.
    #[arg(long = "proxy-cidr", value_name = "CIDR")]
    proxy_cidrs: Vec<String>,
    /// Replace an existing autobracket.toml and organiser.key (the old key stops working).
    #[arg(long)]
    force: bool,
}

pub fn run(args: InitArgs) -> anyhow::Result<()> {
    let public_url = args
        .public_url
        .as_deref()
        .map(check_public_url)
        .transpose()?;
    let dir = std::path::absolute(&args.dir)
        .with_context(|| format!("resolving {}", args.dir.display()))?;
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let public = hex::encode(sk.verifying_key().to_bytes());
    let config_for = |dir: &Path| {
        let mut cfg = Config::new(vec![public.clone()]);
        cfg.bind = args.bind.clone();
        cfg.db_path = dir.join(DB_FILE);
        cfg.replay_dir = dir.join(REPLAY_DIR);
        cfg.trusted_proxies = args.proxy_cidrs.clone();
        cfg.verifier.mode = VerifierMode::Off;
        cfg
    };
    // Everything that can be refused is refused before the first write.
    config_for(&dir)
        .validate()
        .context("the configuration would not load")?;
    if !args.force {
        for name in [CONFIG_FILE, KEY_FILE] {
            let path = dir.join(name);
            if path.exists() {
                bail!(
                    "{} already exists; pass --force to replace {CONFIG_FILE} and {KEY_FILE} \
                     (the old organiser key then stops working)",
                    path.display()
                );
            }
        }
    }

    make_dir(&dir)?;
    let dir = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    let replays = dir.join(REPLAY_DIR);
    if !replays.is_dir() {
        DirBuilder::new()
            .mode(0o700)
            .create(&replays)
            .with_context(|| format!("creating {}", replays.display()))?;
    }
    let cfg = config_for(&dir);
    let text = render(&cfg)?;

    // Write both files beside their final names, check the configuration loads, then move
    // them into place, so a failure never leaves a half-written file under either name.
    let key_tmp = dir.join(format!(".{KEY_FILE}.new"));
    let config_tmp = dir.join(format!(".{CONFIG_FILE}.new"));
    for tmp in [&key_tmp, &config_tmp] {
        remove_if_present(tmp)?;
    }
    let staged = (|| {
        crate::write_key_file(&key_tmp, &sk)?;
        write_private(&config_tmp, &text)?;
        Config::load(&config_tmp).context("the written configuration does not load")?;
        anyhow::Ok(())
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_file(&key_tmp);
        let _ = std::fs::remove_file(&config_tmp);
        return Err(e);
    }
    let key_file = dir.join(KEY_FILE);
    let config_file = dir.join(CONFIG_FILE);
    std::fs::rename(&key_tmp, &key_file)
        .with_context(|| format!("moving the key into {}", key_file.display()))?;
    std::fs::rename(&config_tmp, &config_file)
        .with_context(|| format!("moving the configuration into {}", config_file.display()))?;

    let url = public_url.unwrap_or_else(|| local_url(&cfg.bind));
    print!(
        "{}",
        next_steps(&dir, &key_file, &config_file, &public, &url)
    );
    Ok(())
}

/// `url` without a trailing `/`, if it is an absolute http(s) URL with a host.
fn check_public_url(url: &str) -> anyhow::Result<String> {
    let parsed = reqwest::Url::parse(url).with_context(|| {
        format!("--public-url {url:?} is not a URL (e.g. https://bracket.example.org)")
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("--public-url {url:?} must be an http:// or https:// address with a host");
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Creates `dir` with mode 0700. An existing directory is used as it is, with a warning when
/// other users can reach into it.
fn make_dir(dir: &Path) -> anyhow::Result<()> {
    if dir.is_dir() {
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "warning: {} is accessible to other users (mode {:o}); consider chmod 700",
                dir.display(),
                mode & 0o777
            );
        }
        return Ok(());
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("setting the mode of {}", dir.display()))
}

fn remove_if_present(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Writes `text` to a new file `path` with mode 0600.
fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// The configuration file: every key with a one-line comment above it. Keys at their default
/// are written commented out, showing the default.
fn render(cfg: &Config) -> anyhow::Result<String> {
    let d = Config::new(Vec::new());
    let (v, dv) = (&cfg.verifier, &d.verifier);
    let (r, dr) = (&cfg.ruleset, &d.ruleset);
    let mut f = TomlFile::default();
    f.line("# autobracket server configuration, written by `autobracket init`.");
    f.line("# A line \"# key = value\" shows that key's default: remove the \"# \" to change it.");
    f.line("# Reference: deploy/autobracket.example.toml and the README.");
    f.line("");
    f.set(
        "Address and port the HTTP server listens on.",
        "bind",
        &cfg.bind,
    )?;
    f.set(
        "SQLite database file (README, \"Backups\").",
        "db_path",
        &cfg.db_path,
    )?;
    f.set(
        "Directory where uploaded replays are stored.",
        "replay_dir",
        &cfg.replay_dir,
    )?;
    f.set(
        "Organiser public keys; the matching secret key is organiser.key in this directory.",
        "to_keys",
        &cfg.to_keys,
    )?;
    f.key(
        "melee_relay handed to clients; the default is the author's public relay: run your own (README, \"The relay\").",
        "relay",
        &cfg.relay,
        &d.relay,
    )?;
    f.key(
        "Require an invite code (`autobracket admin ... invite new`) to register a player key.",
        "require_invite",
        cfg.require_invite,
        d.require_invite,
    )?;
    f.key(
        "Public keys of external verifier workers (verifier mode \"external\").",
        "verifier_keys",
        &cfg.verifier_keys,
        &d.verifier_keys,
    )?;
    f.key(
        "Engine builds players may sign up with and the verifier must run; empty accepts any.",
        "accepted_builds",
        &cfg.accepted_builds,
        &d.accepted_builds,
    )?;
    f.key(
        "Seconds to wait for the second replay before judging one upload (verdict Single).",
        "upload_timeout_secs",
        cfg.upload_timeout_secs,
        d.upload_timeout_secs,
    )?;
    f.key(
        "Seconds a ready set may go without activity before it is queued as stalled; 0 disables.",
        "set_stall_secs",
        cfg.set_stall_secs,
        d.set_stall_secs,
    )?;
    f.key(
        "Queue diverging or unverifiable games for a human instead of counting them.",
        "strict_resim",
        cfg.strict_resim,
        d.strict_resim,
    )?;
    f.key(
        "Largest accepted request body (a replay upload), in bytes.",
        "max_replay_bytes",
        cfg.max_replay_bytes,
        d.max_replay_bytes,
    )?;
    f.key(
        "Sustained requests per second allowed from one client address.",
        "rate_per_sec",
        cfg.rate_per_sec,
        d.rate_per_sec,
    )?;
    f.key(
        "Requests one client address may make in a burst before being limited.",
        "rate_burst",
        cfg.rate_burst,
        d.rate_burst,
    )?;
    f.key(
        "Most connections served at once; more wait to be accepted.",
        "max_connections",
        cfg.max_connections,
        d.max_connections,
    )?;
    f.key(
        "Reverse proxies (CIDRs) whose X-Forwarded-For names the client (README, \"TLS and reverse proxies\").",
        "trusted_proxies",
        &cfg.trusted_proxies,
        &d.trusted_proxies,
    )?;
    f.key(
        "Seconds after a room code is posted during which a no-show report is refused.",
        "no_show_grace_secs",
        cfg.no_show_grace_secs,
        d.no_show_grace_secs,
    )?;
    f.key(
        "Accept HOST:PORT room codes (direct connections, for LAN events; reveals addresses).",
        "allow_direct_connect",
        cfg.allow_direct_connect,
        d.allow_direct_connect,
    )?;

    f.line("");
    f.line("[verifier]");
    f.set(
        "\"off\": no re-simulation, games count on both players' agreement (README, \"Verifier modes\").",
        "mode",
        v.mode.as_str(),
    )?;
    f.key(
        "Path to the melee engine binary (only with mode \"in_process\").",
        "engine",
        &v.engine,
        &dv.engine,
    )?;
    f.key(
        "Path to your own NTSC 1.02 disc image (only with mode \"in_process\").",
        "iso",
        &v.iso,
        &dv.iso,
    )?;
    f.key(
        "Seconds one re-simulation may run before it is stopped and recorded as crashed.",
        "timeout_secs",
        v.timeout_secs,
        dv.timeout_secs,
    )?;
    f.key(
        "Games judged at the same time, each with at most one re-simulation; at least 1.",
        "jobs",
        v.jobs,
        dv.jobs,
    )?;

    f.line("");
    f.line("[ruleset]");
    f.key("Stocks per game.", "stocks", r.stocks, dr.stocks)?;
    f.key(
        "Game timer, in seconds.",
        "timer_secs",
        r.timer_secs,
        dr.timer_secs,
    )?;
    f.key(
        "Games per set (best of N, odd); `event create --best-of` overrides it.",
        "best_of",
        r.best_of,
        dr.best_of,
    )?;
    f.key(
        "Games per grand final set (and reset) in double elimination.",
        "best_of_finals",
        r.best_of_finals,
        dr.best_of_finals,
    )?;
    f.key(
        "Legal stage ids, as Game Start records them.",
        "legal_stages",
        &r.legal_stages,
        &dr.legal_stages,
    )?;
    f.key(
        "Character ids that may not be played.",
        "banned_characters",
        &r.banned_characters,
        &dr.banned_characters,
    )?;
    f.key(
        "Permitted Gecko code lists, as lowercase SHA-256 hex (docs/real-engine.md).",
        "allowed_gecko",
        &r.allowed_gecko,
        &dr.allowed_gecko,
    )?;
    Ok(f.text)
}

#[derive(Default)]
struct TomlFile {
    text: String,
}

impl TomlFile {
    fn line(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// `doc` as a comment, then `key = value`.
    fn set(&mut self, doc: &str, key: &str, value: impl Serialize) -> anyhow::Result<()> {
        self.entry(doc, key, &value, false)
    }

    /// `doc` as a comment, then `key = value`, commented out when `value` is `default`.
    fn key<T: Serialize + PartialEq>(
        &mut self,
        doc: &str,
        key: &str,
        value: T,
        default: T,
    ) -> anyhow::Result<()> {
        self.entry(doc, key, &value, value == default)
    }

    fn entry(
        &mut self,
        doc: &str,
        key: &str,
        value: &impl Serialize,
        commented: bool,
    ) -> anyhow::Result<()> {
        let value = toml::Value::try_from(value).with_context(|| {
            format!("{key} cannot be written to TOML (a path that is not UTF-8?)")
        })?;
        let hash = if commented { "# " } else { "" };
        self.line(&format!("# {doc}"));
        self.line(&format!("{hash}{key} = {value}"));
        Ok(())
    }
}

/// The address the server answers on from this machine, for a `bind` address.
fn local_url(bind: &str) -> String {
    match bind.parse::<SocketAddr>() {
        Ok(mut addr) => {
            if addr.ip().is_unspecified() {
                let loopback = match addr {
                    SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                    SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
                };
                addr.set_ip(loopback);
            }
            format!("http://{addr}")
        }
        Err(_) => format!("http://{bind}"),
    }
}

/// `arg` quoted for a POSIX shell, unless it needs no quoting.
fn sh_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-:@%+=,".contains(&b));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

fn next_steps(dir: &Path, key_file: &Path, config_file: &Path, public: &str, url: &str) -> String {
    let q = |p: &Path| sh_quote(&p.to_string_lossy());
    let admin = format!(
        "autobracket admin --server {} --key-file {}",
        sh_quote(url),
        q(key_file)
    );
    format!(
        "\
autobracket is set up in {dir}:
  {KEY_FILE}     your organiser secret key (mode 0600; never share it)
  {CONFIG_FILE}  the server configuration
  {REPLAY_DIR}/          uploaded replays (the database is created on first start)
Organiser public key (already in to_keys): {public}

Next steps

1. Start the server:

   autobracket serve --config {config}

2. In another terminal, create your first event. It prints the event as JSON, with its id:

   {admin} event create --name Nightly --format double --best-of 3

3. Once players have signed up (they run `autobracket-client register --server {url} ...`),
   start the event, using the id from step 2:

   {admin} event start 1

4. The live bracket is at {url}/

Before you go further
- TLS: the server speaks plain HTTP. Put a TLS reverse proxy in front of it before you expose
  it to the internet, and list the proxy in trusted_proxies
  (README, \"TLS and reverse proxies\").
- Verification is off: games count on the two players' agreement and the ruleset alone
  (verdict PeerOnly) until you configure an engine and a disc image (README, \"Verifier modes\").
- Back up your organiser key somewhere private (README, \"Backups\"):
    {key}
  Whoever holds it runs your events, and a lost key cannot be recovered.
",
        dir = dir.display(),
        config = q(config_file),
        key = key_file.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_url_turns_wildcards_into_loopback() {
        assert_eq!(local_url("0.0.0.0:8080"), "http://127.0.0.1:8080");
        assert_eq!(local_url("[::]:8080"), "http://[::1]:8080");
        assert_eq!(local_url("10.1.2.3:80"), "http://10.1.2.3:80");
        assert_eq!(local_url("localhost:8080"), "http://localhost:8080");
    }

    #[test]
    fn sh_quote_quotes_only_when_needed() {
        assert_eq!(sh_quote("/srv/ab/organiser.key"), "/srv/ab/organiser.key");
        assert_eq!(sh_quote("/srv/my ab/x"), "'/srv/my ab/x'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
    }
}

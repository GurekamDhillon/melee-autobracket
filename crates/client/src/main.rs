use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use autobracket_client::api::{check_server_url, Api};
use autobracket_client::config::{self, ClientConfig};
use autobracket_client::engine::CommandEngine;
use autobracket_client::play::{describe, play_event_until, PlayOptions};
use clap::{Parser, Subcommand};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Parser)]
#[command(
    name = "autobracket-client",
    about = "Play your sets in an autobracket Melee tournament"
)]
struct Cli {
    /// Config file [default: ~/.config/autobracket/client.toml]
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a new config with a fresh signing key; prints the public key.
    ///
    /// Relative paths given here are made absolute. Paths edited into the config by hand
    /// may be relative to the config file's directory. The config's opening comments show
    /// optional engine arguments: `--np-join-wait` (no-show detection) and a preset
    /// `--match` start instead of netplay character select.
    Init {
        /// The `melee` engine binary.
        #[arg(long)]
        melee: PathBuf,
        #[arg(long)]
        iso: PathBuf,
        /// Where each game's run directory goes [default: ~/.local/share/autobracket]
        #[arg(long)]
        work_dir: Option<PathBuf>,
        /// Replace an existing config, and its key.
        #[arg(long)]
        force: bool,
        /// For a machine with no display: add `--headless --no-audio` to both engine
        /// argument templates (`host_args` and `join_args`).
        #[arg(long)]
        headless: bool,
    },
    /// Register your key under a tag on a server, and remember the server.
    Register {
        #[arg(long)]
        server: String,
        #[arg(long)]
        tag: String,
        #[arg(long)]
        invite: Option<String>,
    },
    /// List events.
    Events,
    /// Enter an event.
    Signup { event: i64 },
    /// Show an event and what you should do next in it.
    Status { event: i64 },
    /// Play your sets in an event until you are out of it.
    ///
    /// Each game runs in its own `{work_dir}/run-{n}/` directory. Ctrl-C or SIGTERM stops
    /// `play` but not a game in progress: `play` prints where its replay goes and the
    /// `upload` command for it, then exits with status 130 (SIGINT) or 143 (SIGTERM).
    ///
    /// If your opponent never joins: with an engine that supports it, add
    /// `"--np-join-wait", "SECS"` to `host_args` in the config. The game then stops hosting
    /// after SECS seconds with nobody joined (printing `play: netplay: no opponent joined`
    /// and exiting 5), and after two such runs in a row `play` reports a no-show to the
    /// organiser and waits for their decision. An engine too old for the flag exits 2.
    ///
    /// `play` stops at once, reporting nothing, when the engine's exit says your own setup
    /// is broken: 4 (its disc check failed: check `iso`) or 2 (it refused its arguments:
    /// check `host_args` / `join_args`). A game that keeps ending without a replay for
    /// other reasons is reported to the organiser as this client failing, before `play`
    /// gives up.
    Play {
        event: i64,
        /// Seconds between checks with the server.
        #[arg(long, default_value_t = 2)]
        poll_secs: u64,
    },
    /// Upload one replay file by hand, e.g. one `play` could not upload.
    Upload {
        event: i64,
        set: u32,
        game: u8,
        file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let path = match cli.config {
        Some(p) => p,
        None => config::default_path()?,
    };
    match cli.cmd {
        Cmd::Init {
            melee,
            iso,
            work_dir,
            force,
            headless,
        } => {
            let work_dir = match work_dir {
                Some(d) => d,
                None => config::default_work_dir()?,
            };
            let cfg = config::init(&path, &melee, &iso, &work_dir, force, headless)?;
            let key = cfg.signing_key()?;
            println!("Wrote {}", path.display());
            println!("Engine build: {}", cfg.engine_build);
            println!(
                "Public key: {}",
                hex::encode(key.verifying_key().to_bytes())
            );
        }
        Cmd::Register {
            server,
            tag,
            invite,
        } => {
            let mut cfg = ClientConfig::load_unchecked(&path)?;
            check_server_url(&server)?;
            cfg.server = server;
            api(&cfg)?.register(&tag, invite.as_deref()).await?;
            cfg.save(&path)?;
            println!("Registered as {tag} on {}", cfg.server);
        }
        Cmd::Events => {
            let events = api(&load(&path)?)?.events().await?;
            if events.is_empty() {
                println!("No events");
            }
            for e in events {
                println!(
                    "{}  {}  ({}, {} entrants)",
                    e.id, e.name, e.state, e.entrants
                );
            }
        }
        Cmd::Signup { event } => {
            api(&load(&path)?)?.signup(event).await?;
            println!("Entered event {event}");
        }
        Cmd::Status { event } => {
            let api = api(&load(&path)?)?;
            let view = api.event(event).await?;
            let s = &view.summary;
            println!(
                "Event {} {} ({}, {} entrants)",
                s.id, s.name, s.state, s.entrants
            );
            println!("{}", describe(&api.next(event).await?));
        }
        Cmd::Play { event, poll_secs } => {
            let cfg = load(&path)?;
            let api = api(&cfg)?;
            let mut engine = CommandEngine::new(&cfg)?;
            // Registered before playing, so an early signal is not missed.
            let mut sigint = signal(SignalKind::interrupt()).context("catching SIGINT")?;
            let mut sigterm = signal(SignalKind::terminate()).context("catching SIGTERM")?;
            let mut exit_status = 0;
            let stop = async {
                exit_status = tokio::select! {
                    _ = sigint.recv() => 130,
                    _ = sigterm.recv() => 143,
                };
                eprintln!("Interrupted: stopping `play` (a running game is left running)");
            };
            let opts = PlayOptions::new(Duration::from_secs(poll_secs));
            let ended = play_event_until(&api, &mut engine, event, &opts, stop).await?;
            if ended.is_none() {
                drop(engine);
                std::process::exit(exit_status);
            }
        }
        Cmd::Upload {
            event,
            set,
            game,
            file,
        } => {
            let slp =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let status = api(&load(&path)?)?
                .upload_replay(event, set, game, slp)
                .await?;
            if status == reqwest::StatusCode::OK {
                println!("Game {game} of set {set}: this replay was already uploaded");
            } else {
                println!("Uploaded game {game} of set {set}");
            }
        }
    }
    Ok(())
}

/// Loads a config that has a server.
fn load(path: &Path) -> anyhow::Result<ClientConfig> {
    let cfg = ClientConfig::load(path)?;
    if cfg.server.is_empty() {
        bail!(
            "no server in {}; run `register --server URL` first",
            path.display()
        );
    }
    Ok(cfg)
}

fn api(cfg: &ClientConfig) -> anyhow::Result<Api> {
    Ok(Api::new(&cfg.server, cfg.signing_key()?, &cfg.engine_build))
}

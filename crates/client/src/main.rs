use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use autobracket_client::api::{check_server_url, insecure_server_warning, Api};
use autobracket_client::config::{self, ClientConfig, InitOptions, Preset};
use autobracket_client::engine::CommandEngine;
use autobracket_client::play::{describe, play_event_until, report_current_no_show, PlayOptions};
use clap::{Parser, Subcommand};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Parser)]
#[command(
    name = "autobracket-client",
    about = "Play your sets in an autobracket Melee tournament (Linux)"
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
    /// Checks first that the disc image is a readable file of at least 1 GiB and that
    /// `melee version` runs. Relative paths given here are made absolute. Paths edited into
    /// the config by hand may be relative to the config file's directory.
    ///
    /// The default engine arguments start each game on the netplay character select, which
    /// can crash both games on engine builds without the fix from engine branch
    /// port/np-nameentry. With such a build, use `--match`.
    Init {
        /// The `melee` engine binary.
        #[arg(long)]
        melee: PathBuf,
        /// Your own NTSC 1.02 Melee disc image.
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
        /// Preset fighter (and, when you host, stage) instead of the character select, e.g.
        /// `falcon,32` or `fox`: the same for every game of every set. The stage is an
        /// external id or name (default Final Destination).
        #[arg(long = "match", value_name = "FIGHTER[,STAGE]")]
        preset: Option<String>,
    },
    /// Register your key under a tag on a server, and remember the server.
    Register {
        #[arg(long)]
        server: String,
        #[arg(long)]
        tag: String,
        /// An invite code from the organiser, if the server requires one.
        #[arg(long)]
        invite: Option<String>,
    },
    /// List events.
    Events,
    /// Enter an event, sending your engine build.
    Signup { event: i64 },
    /// Show an event and what you should do next in it.
    Status { event: i64 },
    /// Play your sets in an event until you are out of it.
    ///
    /// Each game runs in its own `{work_dir}/run-{n}/` directory. Only one `play` may use a
    /// work directory at a time. Ctrl-C or SIGTERM stops `play` but not a game in progress,
    /// and exits with status 130 (SIGINT) or 143 (SIGTERM). Run `play` again: it takes over
    /// a game still running (it never starts a second one) and uploads the replay of a game
    /// that ended in the meantime.
    ///
    /// A set sent to the organiser does not stop a game in progress; its replay is uploaded
    /// when it ends. A game is stopped only when the server offers a different set or you
    /// are out of the event.
    ///
    /// If your opponent never joins: the engine stops hosting after its `--np-timeout`
    /// (60 s) and prints `play: netplay host: no connection after 60 s`, or, on builds with
    /// `--np-join-wait SECS` in `host_args`, `play: netplay: no opponent joined`. After two
    /// such runs in a row `play` reports a no-show to the organiser and waits for their
    /// decision. A guest that gets no room code for `no_code_wait_secs` (default 600)
    /// reports one too. You can also report at once with the `no-show` command.
    ///
    /// `play` stops at once, reporting nothing, when the engine says your own setup is
    /// broken: exit 4 (its `--verify-disc` check failed: check `iso`), exit 2 (a usage
    /// error: check `host_args` / `join_args`), or an unknown `--match` fighter. A game that
    /// keeps ending without a replay for other reasons is reported to the organiser as this
    /// client failing (kind `client-failure`) before `play` gives up.
    Play {
        event: i64,
        /// Seconds between checks with the server.
        #[arg(long, default_value_t = 2)]
        poll_secs: u64,
    },
    /// Tell the organiser your opponent has not shown up for your current set in EVENT.
    NoShow {
        event: i64,
        /// A short note for the organiser (at most 200 characters).
        #[arg(long)]
        note: Option<String>,
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
            preset,
        } => {
            let work_dir = match work_dir {
                Some(d) => d,
                None => config::default_work_dir()?,
            };
            let preset = preset.as_deref().map(Preset::parse).transpose()?;
            let cfg = config::init(
                &path,
                &InitOptions {
                    melee: &melee,
                    iso: &iso,
                    work_dir: &work_dir,
                    force,
                    headless,
                    preset: preset.as_ref(),
                },
            )?;
            let key = cfg.signing_key()?;
            println!("Wrote {}", path.display());
            println!("Engine build: {}", config::engine_build(&cfg.melee)?);
            println!(
                "Public key: {}",
                hex::encode(key.verifying_key().to_bytes())
            );
            if cfg.uses_character_select() {
                eprintln!(
                    "warning: the engine arguments start each game on the netplay character \
                     select, which crashes both games when a player reaches the name-entry \
                     screen, unless your engine build includes the fix from engine branch \
                     port/np-nameentry. Until it does, run `init --force --match \
                     FIGHTER[,STAGE]` (e.g. `--match falcon,32`) to use a preset fighter and \
                     stage instead."
                );
            }
        }
        Cmd::Register {
            server,
            tag,
            invite,
        } => {
            let mut cfg = ClientConfig::load_unchecked(&path)?;
            check_server_url(&server)?;
            cfg.server = server;
            api(&cfg, "")?.register(&tag, invite.as_deref()).await?;
            cfg.save(&path)?;
            println!("Registered as {tag} on {}", cfg.server);
        }
        Cmd::Events => {
            let events = api(&load(&path)?, "")?.events().await?;
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
            let cfg = load(&path)?;
            let build = config::engine_build(&cfg.melee)?;
            api(&cfg, &build)?.signup(event).await?;
            println!("Entered event {event} with engine build {build}");
        }
        Cmd::Status { event } => {
            let api = api(&load(&path)?, "")?;
            let view = api.event(event).await?;
            let s = &view.summary;
            println!(
                "Event {} {} ({}, {} entrants)",
                s.id, s.name, s.state, s.entrants
            );
            println!("{}", describe(&api.next(event).await?));
        }
        Cmd::Play { event, poll_secs } => {
            autobracket_client::session::require_proc(Path::new("/proc/self/stat"))?;
            let cfg = load(&path)?;
            config::check_disc(&cfg.iso)?;
            let build = config::engine_build(&cfg.melee)?;
            let api = api(&cfg, &build)?;
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
            let opts = PlayOptions {
                no_code_wait: (cfg.no_code_wait_secs > 0)
                    .then(|| Duration::from_secs(cfg.no_code_wait_secs)),
                allow_direct_connect: cfg.allow_direct_connect,
                ..PlayOptions::new(Duration::from_secs(poll_secs))
            };
            let ended = play_event_until(&api, &mut engine, event, &opts, stop).await?;
            if ended.is_none() {
                drop(engine);
                std::process::exit(exit_status);
            }
        }
        Cmd::NoShow { event, note } => {
            let api = api(&load(&path)?, "")?;
            println!(
                "{}",
                report_current_no_show(&api, event, note.as_deref()).await?
            );
        }
        Cmd::Upload {
            event,
            set,
            game,
            file,
        } => {
            let slp =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let cfg = load(&path)?;
            let build = config::engine_build(&cfg.melee)?;
            let status = api(&cfg, &build)?
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

/// The server's API, sending `build` as this client's engine build; warns about a plain
/// http server that is not this machine.
fn api(cfg: &ClientConfig, build: &str) -> anyhow::Result<Api> {
    if let Some(warning) = insecure_server_warning(&cfg.server) {
        eprintln!("{warning}");
    }
    Ok(Api::new(&cfg.server, cfg.signing_key()?, build))
}

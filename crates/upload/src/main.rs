//! `autobracket-upload`: see the crate docs and README "Any client mode".

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use autobracket_upload::{keygen, read_key, Reply, Uploader};
use clap::{Parser, Subcommand};

/// Sign and send bracket requests with your player key, for a game played on another client.
#[derive(Parser)]
#[command(name = "autobracket-upload", version)]
struct Cli {
    /// The bracket server, e.g. https://bracket.example.org
    #[arg(long, global = true, default_value = "")]
    server: String,
    /// Your key file: 64 hex digits (from `keygen`), or an autobracket-client config.
    #[arg(long, global = true, default_value = "player.key")]
    key: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Make a new key file (at --key) and print its public key.
    Keygen,
    /// Register your key under a tag.
    Register {
        #[arg(long)]
        tag: String,
        #[arg(long)]
        invite: Option<String>,
    },
    /// Enter an event in sign-up.
    Signup {
        event: i64,
        /// The client you play on, as the organiser's `accepted_builds` lists it.
        #[arg(long, default_value = "slippi")]
        build: String,
    },
    /// What to play next in an event (the server's `Next`, as JSON).
    Next { event: i64 },
    /// Upload a game's .slp.
    Upload {
        #[arg(long)]
        event: i64,
        #[arg(long)]
        set: i64,
        #[arg(long)]
        game: i64,
        /// Sent as `x-ab-build` (not signed, not trusted; kept for the organiser).
        #[arg(long, default_value = "slippi")]
        build: String,
        file: PathBuf,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("autobracket-upload: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn report(r: Reply) -> ExitCode {
    if (200..300).contains(&r.status) {
        if !r.body.is_empty() {
            println!("{}", r.body);
        } else {
            println!("ok ({})", r.status);
        }
        ExitCode::SUCCESS
    } else {
        eprintln!("refused ({}): {}", r.status, r.body);
        ExitCode::FAILURE
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    if let Cmd::Keygen = cli.cmd {
        println!("{}", keygen(&cli.key)?);
        return Ok(ExitCode::SUCCESS);
    }
    let up = Uploader::new(&cli.server, read_key(&cli.key)?)?;
    let r = match cli.cmd {
        Cmd::Keygen => unreachable!(),
        Cmd::Register { tag, invite } => up.register(&tag, invite.as_deref()).await?,
        Cmd::Signup { event, build } => up.signup(event, &build).await?,
        Cmd::Next { event } => up.next(event).await?,
        Cmd::Upload { event, set, game, build, file } => {
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            up.upload(event, set, game, &bytes, &build).await?
        }
    };
    Ok(report(r))
}

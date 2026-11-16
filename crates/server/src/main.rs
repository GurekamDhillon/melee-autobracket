use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use autobracket_server::{build_app, Config, NoResim};
use clap::{Parser, Subcommand};
use proto::SigningKey;

mod admin_cli;

#[derive(Parser)]
#[command(
    name = "autobracket",
    about = "Self-hosted Melee tournament bracket server"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the HTTP server.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Generate a signing key: writes the secret key hex to FILE (mode 0600)
    /// and prints the public key hex.
    Keygen {
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
    },
    /// Organiser commands against a running server.
    Admin(admin_cli::AdminArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve { config } => serve(Config::load(&config)?).await,
        Cmd::Keygen { out } => keygen(&out),
        Cmd::Admin(args) => admin_cli::run(args).await,
    }
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    let bind = cfg.bind.clone();
    let (app, _state) = build_app(cfg, Arc::new(NoResim)).await?;
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    println!("listening on {}", listener.local_addr()?);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn keygen(out: &PathBuf) -> anyhow::Result<()> {
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)
        .with_context(|| format!("creating {}", out.display()))?;
    writeln!(file, "{}", hex::encode(sk.to_bytes()))?;
    println!("{}", hex::encode(sk.verifying_key().to_bytes()));
    Ok(())
}

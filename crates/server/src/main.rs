use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use autobracket_server::config::VerifierMode;
use autobracket_server::resim::EXTERNAL_RESULT_TIMEOUT;
use autobracket_server::{
    build_app, verify_worker, Config, ExternalResim, NoResim, Resim, Slpcheck,
};
use clap::{Parser, Subcommand};
use proto::SigningKey;

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
    /// Re-simulate replays for a server in external verifier mode: claim jobs, run the
    /// engine locally, and post the results.
    VerifyWorker {
        /// Server base URL. Use `https://` in production: with plain `http://` the server's
        /// answers (the jobs and their replays) can be tampered with on the way.
        #[arg(long, value_name = "URL")]
        server: String,
        /// Secret key file written by `autobracket keygen`.
        #[arg(long, value_name = "FILE")]
        key_file: PathBuf,
        #[arg(long, value_name = "PATH")]
        engine: PathBuf,
        #[arg(long, value_name = "PATH")]
        iso: PathBuf,
        #[arg(long, default_value_t = 120)]
        timeout_secs: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve { config } => serve(Config::load(&config)?).await,
        Cmd::Keygen { out } => keygen(&out),
        Cmd::VerifyWorker {
            server,
            key_file,
            engine,
            iso,
            timeout_secs,
        } => {
            let key = verify_worker::read_key_file(&key_file)?;
            let engine = Slpcheck::new(engine, iso, Duration::from_secs(timeout_secs))
                .await
                .context("the engine cannot be run")?;
            verify_worker::run(server, key, engine).await
        }
    }
}

/// The verifier `cfg.verifier.mode` asks for.
async fn resim_for(cfg: &Config) -> anyhow::Result<Arc<dyn Resim>> {
    let v = &cfg.verifier;
    Ok(match v.mode {
        VerifierMode::InProcess => Arc::new(
            Slpcheck::new(
                v.engine.clone(),
                v.iso.clone(),
                Duration::from_secs(v.timeout_secs),
            )
            .await
            .with_context(|| {
                format!(
                    "verifier.mode = \"in_process\" but the engine {} cannot be run",
                    v.engine.display()
                )
            })?,
        ),
        VerifierMode::External => Arc::new(ExternalResim::new(EXTERNAL_RESULT_TIMEOUT)),
        VerifierMode::Off => Arc::new(NoResim),
    })
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    let bind = cfg.bind.clone();
    let resim = resim_for(&cfg).await?;
    let (app, _state) = build_app(cfg, resim).await?;
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

//! `autobracket admin`: the organiser's command line. Each subcommand is one signed request
//! to the server; the JSON response is pretty-printed to stdout, and a refusal prints the
//! error body to stderr and exits with status 1.

use std::path::{Path, PathBuf};

use anyhow::Context;
use autobracket_server::db::now_unix;
use bracket::Format;
use clap::{Args, Subcommand, ValueEnum};
use proto::{sign_request, EventCreateReq, RekeyReq, Resolve, SigningKey};
use reqwest::Method;
use serde::Serialize;

#[derive(Args)]
pub struct AdminArgs {
    /// Server base URL, e.g. http://127.0.0.1:8080.
    #[arg(long)]
    server: String,
    /// File holding the organiser's secret key hex, as written by `autobracket keygen`.
    #[arg(long, value_name = "FILE")]
    key_file: PathBuf,
    #[command(subcommand)]
    cmd: AdminCmd,
}

#[derive(Subcommand)]
enum AdminCmd {
    /// Create and start events.
    #[command(subcommand)]
    Event(EventCmd),
    /// Registration invites.
    #[command(subcommand)]
    Invite(InviteCmd),
    /// The review queue.
    #[command(subcommand)]
    Queue(QueueCmd),
    /// Registered players.
    #[command(subcommand)]
    Player(PlayerCmd),
    /// Quarantined games.
    #[command(subcommand)]
    Quarantine(QuarantineCmd),
}

#[derive(Subcommand)]
enum EventCmd {
    /// Create an event open for sign-up.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum)]
        format: FormatArg,
        /// Games per set outside the grand final (odd); the rest of the server's ruleset applies.
        #[arg(long, value_name = "N")]
        best_of: Option<u8>,
    },
    /// Close sign-up and build the bracket.
    Start { id: i64 },
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Single,
    Double,
}

#[derive(Subcommand)]
enum InviteCmd {
    /// Create a single-use registration code.
    New,
}

#[derive(Subcommand)]
enum QueueCmd {
    /// List the open queue items.
    List,
    /// Resolve a queue item.
    Resolve {
        id: i64,
        #[command(subcommand)]
        how: ResolveCmd,
    },
}

#[derive(Subcommand)]
enum ResolveCmd {
    /// Award the set to a slot (0 is the host).
    Award {
        #[arg(long)]
        slot: u8,
    },
    /// Void the game; the set plays it again.
    Replay,
    /// Disqualify the entrant in a slot.
    Dq {
        #[arg(long)]
        slot: u8,
    },
}

#[derive(Subcommand)]
enum PlayerCmd {
    /// Move a player from one key to another.
    Rekey { old: String, new: String },
}

#[derive(Subcommand)]
enum QuarantineCmd {
    /// List quarantined games.
    List,
}

/// Runs one admin subcommand.
pub async fn run(args: AdminArgs) -> anyhow::Result<()> {
    let sk = read_key(&args.key_file)?;
    let (method, path, body) = request(args.cmd)?;
    let base = args.server.trim_end_matches('/');

    let h = sign_request(&sk, method.as_str(), &path, now_unix(), &body);
    let res = reqwest::Client::new()
        .request(method, format!("{base}{path}"))
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .with_context(|| format!("sending request to {base}"))?;
    let status = res.status();
    let text = res.text().await.context("reading the response")?;
    if !status.is_success() {
        eprintln!(
            "{}",
            if text.is_empty() {
                status.to_string()
            } else {
                text
            }
        );
        std::process::exit(1);
    }
    if !text.is_empty() {
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
            Err(_) => println!("{text}"),
        }
    }
    Ok(())
}

/// The method, path and JSON body of a subcommand's request.
fn request(cmd: AdminCmd) -> anyhow::Result<(Method, String, Vec<u8>)> {
    Ok(match cmd {
        AdminCmd::Event(EventCmd::Create {
            name,
            format,
            best_of,
        }) => {
            let req = EventCreateReq {
                name,
                format: match format {
                    FormatArg::Single => Format::SingleElim,
                    FormatArg::Double => Format::DoubleElim,
                },
                // The server applies `best_of` to its own configured ruleset.
                ruleset: None,
                best_of,
            };
            (Method::POST, "/v1/admin/events".into(), json(&req)?)
        }
        AdminCmd::Event(EventCmd::Start { id }) => (
            Method::POST,
            format!("/v1/admin/events/{id}/start"),
            Vec::new(),
        ),
        AdminCmd::Invite(InviteCmd::New) => (Method::POST, "/v1/admin/invites".into(), Vec::new()),
        AdminCmd::Queue(QueueCmd::List) => (Method::GET, "/v1/admin/queue".into(), Vec::new()),
        AdminCmd::Queue(QueueCmd::Resolve { id, how }) => {
            let r = match how {
                ResolveCmd::Award { slot } => Resolve::Award { slot },
                ResolveCmd::Replay => Resolve::Replay,
                ResolveCmd::Dq { slot } => Resolve::Dq { slot },
            };
            (
                Method::POST,
                format!("/v1/admin/queue/{id}/resolve"),
                json(&r)?,
            )
        }
        AdminCmd::Player(PlayerCmd::Rekey { old, new }) => (
            Method::POST,
            format!("/v1/admin/players/{old}/rekey"),
            json(&RekeyReq { new_key: new })?,
        ),
        AdminCmd::Quarantine(QuarantineCmd::List) => {
            (Method::GET, "/v1/quarantine".into(), Vec::new())
        }
    })
}

fn json<T: Serialize>(v: &T) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(v).context("encoding the request body")
}

/// Reads the secret key hex written by `autobracket keygen`.
fn read_key(path: &Path) -> anyhow::Result<SigningKey> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading key file {}", path.display()))?;
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(text.trim(), &mut bytes).with_context(|| {
        format!(
            "key file {} must hold a 64-character hex secret key",
            path.display()
        )
    })?;
    Ok(SigningKey::from_bytes(&bytes))
}

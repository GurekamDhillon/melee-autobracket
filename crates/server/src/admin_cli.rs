//! `autobracket admin`: the organiser's command line. Each subcommand is one signed request
//! to the server; the JSON response is pretty-printed to stdout (`queue list` first prints a
//! line per item; `quarantine get` writes the replay to a file), and a refusal prints the
//! error body to stderr and exits with status 1.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use autobracket_server::db::now_unix;
use autobracket_server::logging::{clean, label};
use autobracket_server::verify_worker::insecure_server_warning;
use bracket::Format;
use clap::{Args, Subcommand, ValueEnum};
use proto::{sign_request, EventCreateReq, QueueItem, RekeyReq, Resolve, SigningKey};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;

/// How long establishing a connection to the server may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one request may take in all.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a replay download may take in all.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(120);

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
    /// Event entrants.
    #[command(subcommand)]
    Entrant(EntrantCmd),
}

#[derive(Subcommand)]
enum EntrantCmd {
    /// Take a player out of an event that is still open for sign-up; later seeds move up.
    Remove { event: i64, tag: String },
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
    /// List the open queue items: one line each (players, reporter, what the evidence says),
    /// then the full JSON.
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
    /// Save the replay a slot (0 is the host) uploaded for a quarantined game.
    Get {
        id: i64,
        slot: u8,
        /// File to write; it must not exist yet.
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
    },
}

/// What to do with a successful response.
enum Output {
    /// Pretty-print the JSON.
    Json,
    /// A line per queue item, then the JSON.
    QueueList,
    /// Write the body to this new file.
    File(PathBuf),
}

/// A signed request a subcommand makes.
struct Request {
    method: Method,
    path: String,
    body: Vec<u8>,
    output: Output,
}

/// An HTTP client that gives up on a server that does not answer: `connect` bounds
/// establishing the connection, `request` each whole request.
fn http_client(connect: Duration, request: Duration) -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(request)
        .build()
        .context("building the HTTP client")
}

/// Runs one admin subcommand.
pub async fn run(args: AdminArgs) -> anyhow::Result<()> {
    let sk = read_key(&args.key_file)?;
    let req = request(args.cmd)?;
    let base = args.server.trim_end_matches('/');
    if let Some(warning) = insecure_server_warning(&args.server) {
        eprintln!("warning: {warning}");
    }
    let timeout = match req.output {
        Output::File(_) => REPLAY_TIMEOUT,
        _ => REQUEST_TIMEOUT,
    };
    // Refuse before asking the server, so an existing file is never touched.
    if let Output::File(out) = &req.output {
        if out.exists() {
            anyhow::bail!("{} already exists", out.display());
        }
    }

    let h = sign_request(&sk, req.method.as_str(), &req.path, now_unix(), &req.body);
    let res = http_client(CONNECT_TIMEOUT, timeout)?
        .request(req.method, format!("{base}{}", req.path))
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .header("content-type", "application/json")
        .body(req.body)
        .send()
        .await
        .with_context(|| format!("sending request to {base}"))?;
    let status = res.status();
    let bytes = res.bytes().await.context("reading the response")?;
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        eprintln!(
            "{}",
            if text.is_empty() {
                status.to_string()
            } else {
                text.into_owned()
            }
        );
        std::process::exit(1);
    }
    match req.output {
        Output::File(out) => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&out)
                .with_context(|| format!("creating {}", out.display()))?;
            file.write_all(&bytes)
                .with_context(|| format!("writing {}", out.display()))?;
            println!("wrote {} bytes to {}", bytes.len(), out.display());
        }
        Output::QueueList => {
            let items: Vec<QueueItem> =
                serde_json::from_slice(&bytes).context("reading the queue")?;
            for item in &items {
                println!("{}", queue_line(item));
            }
            print_json(&bytes)?;
        }
        Output::Json => print_json(&bytes)?,
    }
    Ok(())
}

/// Pretty-prints a JSON body (as is if it is not JSON); nothing for an empty one.
fn print_json(bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(_) => println!("{}", String::from_utf8_lossy(bytes)),
    }
    Ok(())
}

/// One line about a queue item: id, place, reason, players, reporter, and what the evidence
/// says. Text from the server is cleaned, so it cannot forge lines on the terminal.
fn queue_line(item: &QueueItem) -> String {
    let mut line = format!(
        "#{} event {} set {} game {}: {}",
        item.id,
        item.event_id,
        item.set_id,
        item.game_id
            .map_or_else(|| "-".to_owned(), |g| g.to_string()),
        label(&item.reason)
    );
    if let Some([host, guest]) = &item.tags {
        line.push_str(&format!(
            " [slot 0 {} vs slot 1 {}]",
            label(host),
            label(guest)
        ));
    }
    if let Some(tag) = &item.reporter_tag {
        line.push_str(&format!(" reported by {}", label(tag)));
    }
    if let Some(detail) = &item.detail {
        line.push_str(&format!(" ({})", clean(detail, 200)));
    }
    match &item.evidence {
        Some(Value::Array(games)) => {
            let games: Vec<String> = games.iter().map(game_summary).collect();
            line.push_str(&format!(" | {}", games.join(" | ")));
        }
        Some(game) => line.push_str(&format!(" | {}", game_summary(game))),
        None => line.push_str(" | no evidence"),
    }
    line
}

/// One game's evidence in a few words: each upload's derived winner, end and stage, and the
/// re-simulation.
fn game_summary(ev: &Value) -> String {
    let text = |v: &Value| clean(&v.as_str().map_or_else(|| v.to_string(), str::to_owned), 80);
    let mut parts = vec![format!(
        "game {} {} ({})",
        text(&ev["game_no"]),
        text(&ev["verdict"]),
        text(&ev["state"])
    )];
    for u in ev["uploads"].as_array().into_iter().flatten() {
        let who = format!("slot {} {}", text(&u["slot"]), text(&u["tag"]));
        if u["parse"] == "ok" {
            parts.push(format!(
                "{who}: winner p{}, end {}, stage {}, legality {}",
                text(&u["winner_port"]),
                text(&u["end_method"]),
                text(&u["stage"]),
                text(&u["legality"])
            ));
        } else {
            parts.push(format!("{who}: {}", text(&u["parse"])));
        }
    }
    let resim = &ev["resim"];
    if !resim.is_null() {
        let only = if resim["evidence_only"] == true {
            " (evidence only)"
        } else {
            ""
        };
        parts.push(format!(
            "resim {}{only} {}",
            text(&resim["outcome"]),
            text(&resim["detail"])
        ));
    }
    parts.join("; ")
}

/// The request a subcommand makes.
fn request(cmd: AdminCmd) -> anyhow::Result<Request> {
    let simple = |method, path: String, body| Request {
        method,
        path,
        body,
        output: Output::Json,
    };
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
            simple(Method::POST, "/v1/admin/events".into(), json(&req)?)
        }
        AdminCmd::Event(EventCmd::Start { id }) => simple(
            Method::POST,
            format!("/v1/admin/events/{id}/start"),
            Vec::new(),
        ),
        AdminCmd::Invite(InviteCmd::New) => {
            simple(Method::POST, "/v1/admin/invites".into(), Vec::new())
        }
        AdminCmd::Queue(QueueCmd::List) => Request {
            output: Output::QueueList,
            ..simple(Method::GET, "/v1/admin/queue".into(), Vec::new())
        },
        AdminCmd::Queue(QueueCmd::Resolve { id, how }) => {
            let r = match how {
                ResolveCmd::Award { slot } => Resolve::Award { slot },
                ResolveCmd::Replay => Resolve::Replay,
                ResolveCmd::Dq { slot } => Resolve::Dq { slot },
            };
            simple(
                Method::POST,
                format!("/v1/admin/queue/{id}/resolve"),
                json(&r)?,
            )
        }
        AdminCmd::Player(PlayerCmd::Rekey { old, new }) => simple(
            Method::POST,
            format!("/v1/admin/players/{old}/rekey"),
            json(&RekeyReq { new_key: new })?,
        ),
        AdminCmd::Quarantine(QuarantineCmd::List) => {
            simple(Method::GET, "/v1/quarantine".into(), Vec::new())
        }
        AdminCmd::Quarantine(QuarantineCmd::Get { id, slot, out }) => Request {
            output: Output::File(out),
            ..simple(
                Method::GET,
                format!("/v1/quarantine/{id}/replay/{slot}"),
                Vec::new(),
            )
        },
        AdminCmd::Entrant(EntrantCmd::Remove { event, tag }) => simple(
            Method::DELETE,
            path_of(&[
                "v1",
                "admin",
                "events",
                &event.to_string(),
                "entrants",
                &tag,
            ])?,
            Vec::new(),
        ),
    })
}

/// A URL path from its segments, each percent-encoded as needed (the server signs and checks
/// the encoded form).
fn path_of(segments: &[&str]) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse("http://server/").context("building a path")?;
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("building a path"))?
        .extend(segments);
    Ok(url.path().to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_server_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accepts connections and never answers.
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let client = http_client(Duration::from_millis(200), Duration::from_millis(300)).unwrap();
        let sent = tokio::time::timeout(
            Duration::from_secs(5),
            client.get(format!("http://{addr}/v1/admin/queue")).send(),
        )
        .await
        .expect("the request gives up instead of hanging");
        assert!(sent.is_err(), "{sent:?}");
        server.abort();
    }

    #[test]
    fn timeouts_are_as_documented() {
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(30));
        assert_eq!(REPLAY_TIMEOUT, Duration::from_secs(120));
    }

    #[test]
    fn paths_are_encoded() {
        assert_eq!(
            path_of(&["v1", "admin", "events", "3", "entrants", "Mango"]).unwrap(),
            "/v1/admin/events/3/entrants/Mango"
        );
        assert_eq!(
            path_of(&["entrants", "a b/c?"]).unwrap(),
            "/entrants/a%20b%2Fc%3F"
        );
    }

    #[test]
    fn queue_line_is_one_clean_line() {
        let item = QueueItem {
            id: 7,
            event_id: 1,
            set_id: 2,
            game_id: None,
            reason: "no-show".into(),
            verdict: None,
            resim: None,
            detail: Some("reported by slot 1 (Bob): gone\nINFO forged".into()),
            evidence: Some(serde_json::json!([{
                "game_no": 1, "verdict": "PeerOnly", "state": "void",
                "uploads": [{"slot": 0, "tag": "Al\u{202e}", "parse": "ok", "winner_port": 1,
                             "end_method": 2, "stage": 32, "legality": "ok"}],
                "resim": null,
            }])),
            reporter_tag: Some("Bob".into()),
            tags: Some(["Al".into(), "Bob".into()]),
        };
        let line = queue_line(&item);
        assert!(
            !line.contains('\n') && !line.contains('\u{202e}'),
            "{line:?}"
        );
        for part in [
            "#7",
            "no-show",
            "slot 0 Al vs slot 1 Bob",
            "reported by Bob",
            "winner p1",
        ] {
            assert!(line.contains(part), "{part:?} in {line:?}");
        }
    }
}

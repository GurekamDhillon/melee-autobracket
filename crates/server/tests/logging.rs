//! The server logs what happens in a tournament, one line per event with fields, and never
//! logs secrets: room codes or full keys.

use std::io::Write;
use std::sync::{Arc, Mutex};

use autobracket_server::testkit::{play_game, TestApp};
use bracket::Format;
use proto::RoomReq;
use reqwest::StatusCode;
use tracing_subscriber::fmt::MakeWriter;

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Capture {
    fn lines(&self) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The one line containing `message` and every field in `fields`.
fn line_with<'a>(lines: &'a [String], message: &str, fields: &[&str]) -> &'a str {
    let found: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains(message) && fields.iter().all(|f| l.contains(f)))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "one line with {message:?} and {fields:?} in:\n{}",
        lines.join("\n")
    );
    found[0]
}

/// The longest run of hex digits in `s`.
fn longest_hex_run(s: &str) -> usize {
    let mut best = 0;
    let mut run = 0;
    for c in s.chars() {
        run = if c.is_ascii_hexdigit() { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

// A current-thread runtime (the `tokio::test` default), so the server's tasks run on this
// thread and the thread-local subscriber sees them.
#[tokio::test]
async fn tournament_is_logged_without_secrets() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = TestApp::spawn(|_| {}).await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    let res = host
        .post_json(
            &format!("/v1/sets/{event}/0/room"),
            &RoomReq {
                code: "QZ7K".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    // Best of 3, both games to the host.
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    play_game(&app, event, 0, 2, &host, &guest, 1).await;

    let lines = capture.lines();
    let host_key = hex::encode(host.sk.verifying_key().to_bytes());
    let guest_key = hex::encode(guest.sk.verifying_key().to_bytes());
    let to_key = hex::encode(app.to.verifying_key().to_bytes());

    line_with(
        &lines,
        "player registered",
        &["tag=Alice", &format!("key={}", &host_key[..8])],
    );
    line_with(&lines, "player registered", &["tag=Bob"]);
    line_with(
        &lines,
        "event started",
        &[
            &format!("event={event}"),
            "entrants=2",
            "format=single_elim",
        ],
    );
    line_with(&lines, "signed up", &["tag=Alice", "seed=0"]);
    line_with(
        &lines,
        "room code posted",
        &[&format!("event={event}"), "set=0", "game=1"],
    );
    for game in [1, 2] {
        for slot in [0, 1] {
            let upload = line_with(
                &lines,
                "replay uploaded",
                &[
                    &format!("game={game} "),
                    &format!("slot={slot}"),
                    "client_build=b1",
                ],
            );
            assert!(upload.contains("bytes="), "{upload}");
            let sha = upload
                .split("sha256=")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .unwrap_or_else(|| panic!("no sha256 in {upload}"));
            assert_eq!(sha.len(), 12, "{upload}");
        }
        let verdict = line_with(
            &lines,
            "verdict",
            &[
                &format!("game={game} "),
                "verdict=PeerOnly",
                "resim=Skipped",
                "verifier_build=none",
            ],
        );
        assert!(verdict.contains("judging_ms="), "{verdict}");
        assert!(verdict.contains("resim_ms="), "{verdict}");
    }
    line_with(
        &lines,
        "set decided",
        &[
            &format!("event={event}"),
            "set=0",
            "winner=Alice",
            "score=2-0",
            "how=played",
        ],
    );
    line_with(
        &lines,
        "event completed",
        &[&format!("event={event}"), "champion=Alice"],
    );

    for line in &lines {
        assert!(!line.contains("QZ7K"), "room code logged: {line}");
        for key in [&host_key, &guest_key, &to_key] {
            assert!(!line.contains(key.as_str()), "full key logged: {line}");
        }
        assert!(longest_hex_run(line) < 64, "64 hex digits logged: {line}");
        assert!(!line.contains('\u{1b}'), "colour codes in: {line}");
    }
}

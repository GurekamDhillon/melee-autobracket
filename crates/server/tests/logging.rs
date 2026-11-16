//! The server logs what happens in a tournament, one line per event with fields, and never
//! logs secrets: room codes or full keys.

use std::io::Write;
use std::sync::{Arc, Mutex};

use autobracket_server::testkit::{play_game, TestApp};
use bracket::Format;
use proto::{EventCreateReq, RoomReq, SignupReq};
use replay::testkit::simple_game;
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
                // Verifier off: nothing was re-simulated.
                "verifier_build=-",
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

/// A started-event fixture without a subscriber: `(app, host, guest, event)`.
async fn two_player() -> (
    TestApp,
    autobracket_server::testkit::TestPlayer,
    autobracket_server::testkit::TestPlayer,
    i64,
) {
    let app = TestApp::spawn(|_| {}).await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    (app, host, guest, event)
}

#[tokio::test]
async fn signup_rejects_unprintable_or_long_engine_build() {
    let app = TestApp::spawn(|_| {}).await;
    let p = app.player("Alice").await;
    let event = app.create_event(Format::SingleElim).await;
    let path = format!("/v1/events/{event}/entrants");
    for bad in [
        "b1\n2026-11-15T00:00:00Z  INFO forged".to_string(),
        "b1\r".into(),
        "b1\u{1b}[31m".into(),
        "b1\u{85}".into(),
        "b1\u{2028}".into(),
        "b".repeat(65),
    ] {
        let res = p
            .post_json(
                &path,
                &SignupReq {
                    engine_build: bad.clone(),
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad:?}");
    }
    let res = p
        .post_json(
            &path,
            &SignupReq {
                engine_build: "b".repeat(64),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn upload_rejects_unprintable_or_long_build_header() {
    let (_app, host, _guest, event) = two_player().await;
    let path = format!("/v1/sets/{event}/0/games/1/replay");
    let long = "b".repeat(65);
    for bad in ["b\t1", long.as_str()] {
        let res = host
            .post_bytes(
                &path,
                simple_game(1, "2026-11-15T20:00:00Z"),
                &[("x-ab-build", bad)],
            )
            .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad:?}");
    }
    let res = host
        .post_bytes(
            &path,
            simple_game(1, "2026-11-15T20:00:00Z"),
            &[("x-ab-build", "b".repeat(64).as_str())],
        )
        .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn event_name_must_be_short_and_printable() {
    let app = TestApp::spawn(|_| {}).await;
    for bad in [
        "Night\nly".to_string(),
        "Nightly\u{2029}".into(),
        "n".repeat(81),
    ] {
        let req = EventCreateReq {
            name: bad.clone(),
            format: Format::SingleElim,
            ruleset: None,
            best_of: None,
            starts_at: None,
            description: None,
        };
        let res = app.as_to().post_json("/v1/admin/events", &req).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad:?}");
    }
    let req = EventCreateReq {
        name: "n".repeat(80),
        format: Format::SingleElim,
        ruleset: None,
        best_of: None,
        starts_at: None,
        description: None,
    };
    let res = app.as_to().post_json("/v1/admin/events", &req).await;
    assert_eq!(res.status(), StatusCode::CREATED);
}

/// Text that got into the store some other way (an older server, a hand edit) cannot forge
/// log lines either: it is escaped on the way out.
#[tokio::test]
async fn stored_text_cannot_forge_log_lines() {
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
    let event = app.create_event(Format::SingleElim).await;
    host.sign_up(event).await;
    guest.sign_up(event).await;
    let forged = "W\n2026-11-15T20:00:00.000000Z  INFO autobracket_server::lifecycle: \
                  set decided event=1 set=0 winner=Mallory score=2-0 how=played\r\u{1b}[2K\u{2028}x";
    sqlx::query("UPDATE events SET name = ? WHERE id = ?")
        .bind(forged)
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app
        .as_to()
        .post_json(
            &format!("/v1/admin/events/{event}/start"),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let lines = capture.lines();
    // Exactly one line per log event: each starts with its own timestamp.
    for line in &lines {
        assert!(
            line.len() > 4 && line[..4].bytes().all(|b| b.is_ascii_digit()),
            "line without a timestamp (forged?): {line:?}"
        );
        assert!(!line.contains('\r') && !line.contains('\u{1b}') && !line.contains('\u{2028}'));
    }
    let started = line_with(&lines, "event started", &[&format!("event={event}")]);
    assert!(started.contains("\\n"), "{started}");
    assert!(started.contains("\\r"), "{started}");
    assert!(started.contains("\\u{1b}"), "{started}");
    assert!(started.contains("\\u{2028}"), "{started}");
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("set decided") && !l.contains("event started")),
        "a forged set decided line: {lines:?}"
    );
}

/// A configured verifier that cannot check games (not strict): every such game gets its own
/// warning, and the first one after start-up an error-level line too.
#[tokio::test]
async fn unavailable_verifier_is_an_error_once() {
    use autobracket_server::testkit::FakeResim;
    use proto::ResimOutcome;
    use std::collections::VecDeque;

    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let unavailable = || ResimOutcome::Unavailable("no disc".into());
    let resim = Arc::new(FakeResim(Mutex::new(VecDeque::from([
        unavailable(),
        unavailable(),
    ]))));
    let app = TestApp::spawn_with(
        |c| c.verifier.mode = autobracket_server::config::VerifierMode::InProcess,
        resim,
    )
    .await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    play_game(&app, event, 0, 2, &host, &guest, 1).await;

    let lines = capture.lines();
    let warnings = lines
        .iter()
        .filter(|l| l.contains(" WARN ") && l.contains("verifier unavailable"))
        .count();
    assert_eq!(warnings, 2, "{lines:#?}");
    let errors: Vec<&String> = lines.iter().filter(|l| l.contains(" ERROR ")).collect();
    assert_eq!(errors.len(), 1, "{lines:#?}");
    assert!(errors[0].contains("peer-only"), "{}", errors[0]);
}

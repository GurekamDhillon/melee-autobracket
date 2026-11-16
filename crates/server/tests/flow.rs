//! End-to-end judging: uploads are judged by the worker, verdicts recorded, sets
//! won and the bracket advanced, and anything doubtful sent to the organiser queue.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use autobracket_server::db::now_unix;
use autobracket_server::lifecycle::{apply_verdict, load_bracket};
use autobracket_server::testkit::{play_game, FakeResim, TestApp, TestPlayer};
use autobracket_server::worker::{judge_due, recover};
use bracket::{Format, SetState, Side, Slot};
use proto::{EventView, Next, ResimOutcome, Verdict};
use replay::testkit::{simple_game, SlpBuilder};
use replay::PlayerKind;
use reqwest::StatusCode;

const START_A: &str = "2026-11-15T20:00:00Z";
const START_B: &str = "2026-11-15T20:00:01Z";

fn fake(outcomes: Vec<ResimOutcome>) -> Arc<FakeResim> {
    Arc::new(FakeResim(Mutex::new(VecDeque::from(outcomes))))
}

fn diverged() -> ResimOutcome {
    ResimOutcome::Diverged {
        frame: 100,
        port: 1,
        field: "x".into(),
        engine_value: "1.0".into(),
        replay_value: "2.0".into(),
    }
}

async fn upload(p: &TestPlayer, event: i64, set: u32, n: i64, file: Vec<u8>) -> StatusCode {
    p.post_bytes(
        &format!("/v1/sets/{event}/{set}/games/{n}/replay"),
        file,
        &[("x-ab-build", "b1")],
    )
    .await
    .status()
}

async fn next(p: &TestPlayer, event: i64) -> Next {
    let res = p.get(&format!("/v1/me/next?event={event}")).await;
    assert_eq!(res.status(), StatusCode::OK, "next for {}", p.tag);
    res.json().await.unwrap()
}

async fn event_view(app: &TestApp, event: i64) -> EventView {
    let res = app.as_to().get(&format!("/v1/events/{event}")).await;
    assert_eq!(res.status(), StatusCode::OK);
    res.json().await.unwrap()
}

async fn scalar_i64(app: &TestApp, sql: &'static str, event: i64) -> i64 {
    sqlx::query_scalar(sql)
        .bind(event)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

async fn queue_reasons(app: &TestApp, event: i64) -> Vec<String> {
    sqlx::query_scalar("SELECT reason FROM queue WHERE event_id = ? AND state = 'open' ORDER BY id")
        .bind(event)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

async fn quarantine_kinds(app: &TestApp) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM quarantine ORDER BY id")
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

/// `(state, verdict, winner_slot, resim, verifier_build)` of the only game of `set`.
async fn game(
    app: &TestApp,
    event: i64,
    set: u32,
    n: i64,
) -> (
    String,
    Option<Verdict>,
    Option<i64>,
    Option<ResimOutcome>,
    Option<String>,
) {
    let (state, verdict, slot, resim, build): (
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT state, verdict_json, winner_slot, resim_json, verifier_build FROM games
         WHERE event_id = ? AND set_id = ? AND game_no = ?",
    )
    .bind(event)
    .bind(set)
    .bind(n)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    (
        state,
        verdict.map(|v| serde_json::from_str(&v).unwrap()),
        slot,
        resim.map(|r| serde_json::from_str(&r).unwrap()),
        build,
    )
}

/// `(state, wins0, wins1)` of a `sets` row.
async fn set_row(app: &TestApp, event: i64, set: u32) -> (String, i64, i64) {
    sqlx::query_as("SELECT state, wins0, wins1 FROM sets WHERE event_id = ? AND set_id = ?")
        .bind(event)
        .bind(set)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

/// The seeds in slots 0 and 1 of `set`.
async fn seats(app: &TestApp, event: i64, set: u32) -> [u32; 2] {
    let b = load_bracket(&app.state.db, event).await.unwrap();
    b.sets[set as usize].slots.map(|s| match s {
        Slot::Entrant(e) => e,
        other => panic!("set {set} slot {other:?}"),
    })
}

/// A started 2-player single elimination event: `(app, host, guest, event)`.
async fn two_player(app: TestApp) -> (TestApp, TestPlayer, TestPlayer, i64) {
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    (app, host, guest, event)
}

/// Plays every game of `set` (as many as needed) so that the entrant `winner` wins it.
async fn win_set(
    app: &TestApp,
    event: i64,
    set: u32,
    players: &[&TestPlayer],
    winner: u32,
    games: u8,
) {
    let [s0, s1] = seats(app, event, set).await;
    let port = if s0 == winner { 1 } else { 2 };
    for n in 1..=games {
        play_game(
            app,
            event,
            set,
            n,
            players[s0 as usize],
            players[s1 as usize],
            port,
        )
        .await;
    }
}

#[tokio::test]
async fn four_player_double_elim_runs_to_champion() {
    let app = TestApp::spawn_with(|_| {}, fake(vec![])).await;
    let mut players = Vec::new();
    for tag in ["P0", "P1", "P2", "P3"] {
        players.push(app.player(tag).await);
    }
    let refs: Vec<&TestPlayer> = players.iter().collect();
    let event = app.started_event(Format::DoubleElim, &refs).await;

    let mut played = 0;
    while event_view(&app, event).await.summary.state != "complete" {
        assert!(played < 20, "bracket never completed");
        let ready: Vec<u32> = sqlx::query_scalar(
            "SELECT set_id FROM sets WHERE event_id = ? AND state = 'ready' ORDER BY set_id",
        )
        .bind(event)
        .fetch_all(&app.state.db)
        .await
        .unwrap();
        assert!(!ready.is_empty(), "running event with nothing to play");
        for set in ready {
            // Seed 0 wins everything; otherwise the better seed wins. Every set is a
            // sweep: 2-0, or 3-0 in the best-of-5 grand final.
            let [s0, s1] = seats(&app, event, set).await;
            let side = load_bracket(&app.state.db, event).await.unwrap().sets[set as usize].side;
            let games = if side == Side::GrandFinal { 3 } else { 2 };
            win_set(&app, event, set, &refs, s0.min(s1), games).await;
            assert_eq!(set_row(&app, event, set).await.0, "done");
            played += 1;
        }
    }

    let view = event_view(&app, event).await;
    assert_eq!(view.bracket.as_ref().unwrap().champion(), Some(0));
    assert_eq!(played, 6);
    let verdicts: Vec<String> = sqlx::query_scalar("SELECT verdict_json FROM games")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    // Five sets 2-0 and the grand final 3-0.
    assert_eq!(verdicts.len(), 13);
    for v in verdicts {
        assert_eq!(
            serde_json::from_str::<Verdict>(&v).unwrap(),
            Verdict::Verified
        );
    }
    assert!(queue_reasons(&app, event).await.is_empty());
    assert_eq!(next(&players[0], event).await, Next::Champion);
    assert_eq!(next(&players[1], event).await, Next::Eliminated);
}

#[tokio::test]
async fn matched_game_records_the_verifier_build() {
    let (app, host, guest, event) =
        two_player(TestApp::spawn_with(|_| {}, fake(vec![])).await).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    let (state, verdict, _, resim, build) = game(&app, event, 0, 1).await;
    assert_eq!(state, "counted");
    assert_eq!(verdict, Some(Verdict::Verified));
    assert_eq!(resim, Some(ResimOutcome::Match));
    assert_eq!(build.as_deref(), Some("fake"));
}

#[tokio::test]
async fn inexact_counts_and_is_quarantined() {
    let (app, host, guest, event) =
        two_player(TestApp::spawn_with(|_| {}, fake(vec![diverged()])).await).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;

    let (state, verdict, slot, resim, build) = game(&app, event, 0, 1).await;
    assert_eq!(state, "counted");
    assert_eq!(verdict, Some(Verdict::VerifiedInexact));
    assert_eq!(slot, Some(0));
    assert_eq!(resim, Some(diverged()));
    assert_eq!(build.as_deref(), Some("fake"));
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));
    assert_eq!(quarantine_kinds(&app).await, vec!["VerifiedInexact"]);
    assert!(queue_reasons(&app, event).await.is_empty());
}

#[tokio::test]
async fn strict_resim_sends_inexact_to_queue() {
    let app = TestApp::spawn_with(|c| c.strict_resim = true, fake(vec![diverged()])).await;
    let (app, host, guest, event) = two_player(app).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;

    let (state, verdict, slot, _, _) = game(&app, event, 0, 1).await;
    assert_eq!(state, "review");
    assert_eq!(verdict, Some(Verdict::Inexact));
    assert_eq!(slot, None);
    assert_eq!(set_row(&app, event, 0).await, ("review".into(), 0, 0));
    assert_eq!(next(&host, event).await, Next::Review { set_id: 0 });
    assert_eq!(next(&guest, event).await, Next::Review { set_id: 0 });
    assert_eq!(queue_reasons(&app, event).await, vec!["Inexact"]);
    assert_eq!(quarantine_kinds(&app).await, vec!["Inexact"]);
}

#[tokio::test]
async fn conflicting_uploads_go_to_queue() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(2, START_B)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);

    let (state, verdict, slot, _, build) = game(&app, event, 0, 1).await;
    assert_eq!(build, None, "never re-simulated");
    assert_eq!(state, "review");
    assert_eq!(verdict, Some(Verdict::Conflicted));
    assert_eq!(slot, None);
    assert_eq!(set_row(&app, event, 0).await, ("review".into(), 0, 0));
    assert_eq!(queue_reasons(&app, event).await, vec!["Conflicted"]);
    assert_eq!(quarantine_kinds(&app).await, vec!["Conflicted"]);
}

#[tokio::test]
async fn single_upload_times_out_to_queue() {
    let app = TestApp::spawn(|c| c.upload_timeout_secs = 1).await;
    let (app, host, _guest, event) = two_player(app).await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    // The upload timeout has passed.
    sqlx::query("UPDATE games SET first_upload_at = first_upload_at - 2")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);

    let (state, verdict, slot, _, build) = game(&app, event, 0, 1).await;
    assert_eq!(build, None, "never re-simulated");
    assert_eq!(state, "review");
    assert_eq!(verdict, Some(Verdict::Single));
    assert_eq!(slot, None);
    assert_eq!(set_row(&app, event, 0).await, ("review".into(), 0, 0));
    assert_eq!(queue_reasons(&app, event).await, vec!["Single"]);
    assert_eq!(quarantine_kinds(&app).await, vec!["Single"]);
    // Nothing further is judged, and the game is never counted.
    assert_eq!(judge_due(&app.state).await.unwrap(), 0);
    assert_eq!(game(&app, event, 0, 1).await.0, "review");
}

#[tokio::test]
async fn single_upload_waits_before_timeout() {
    let app = TestApp::spawn(|c| c.upload_timeout_secs = 3600).await;
    let (app, host, _guest, event) = two_player(app).await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(judge_due(&app.state).await.unwrap(), 0);
    assert_eq!(game(&app, event, 0, 1).await.0, "uploading");
}

#[tokio::test]
async fn illegal_stage_goes_to_queue() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    // Stage 4 is not in the default legal list.
    let illegal = |start: &str| {
        SlpBuilder::new()
            .stage(4)
            .player(1, 2, PlayerKind::Human)
            .player(2, 9, PlayerKind::Human)
            .frame(-123, 1, 4, 0.0)
            .frame(-123, 2, 4, 0.0)
            .frame(5400, 1, 2, 63.0)
            .frame(5400, 2, 0, 0.0)
            .end(2, None)
            .start_at(start)
            .build()
    };
    assert_eq!(
        upload(&host, event, 0, 1, illegal(START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&guest, event, 0, 1, illegal(START_B)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);

    let (state, verdict, slot, _, build) = game(&app, event, 0, 1).await;
    assert_eq!(build, None, "never re-simulated");
    assert_eq!(state, "review");
    assert_eq!(verdict, Some(Verdict::Illegal("stage".into())));
    assert_eq!(slot, None);
    assert_eq!(set_row(&app, event, 0).await, ("review".into(), 0, 0));
    assert_eq!(queue_reasons(&app, event).await, vec!["Illegal"]);
    assert!(quarantine_kinds(&app).await.is_empty());
}

#[tokio::test]
async fn verifier_off_is_peer_only() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    play_game(&app, event, 0, 1, &host, &guest, 2).await;

    let (state, verdict, slot, resim, build) = game(&app, event, 0, 1).await;
    assert_eq!(state, "counted");
    assert_eq!(verdict, Some(Verdict::PeerOnly));
    assert_eq!(slot, Some(1));
    assert_eq!(resim, Some(ResimOutcome::Skipped));
    // The verifier is off: nothing was re-simulated, so no build is recorded.
    assert_eq!(build, None);
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 0, 1));
    let (first_upload, judged): (i64, Option<i64>) =
        sqlx::query_as("SELECT first_upload_at, judged_at FROM games WHERE event_id = ?")
            .bind(event)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    let judged = judged.expect("judged_at");
    assert!(judged >= first_upload && judged <= now_unix(), "{judged}");
    assert!(queue_reasons(&app, event).await.is_empty());
    assert!(quarantine_kinds(&app).await.is_empty());
}

#[tokio::test]
async fn unavailable_verifier_is_peer_only() {
    let app = TestApp::spawn_with(
        |_| {},
        fake(vec![ResimOutcome::Unavailable("no disc".into())]),
    )
    .await;
    let (app, host, guest, event) = two_player(app).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    let (state, verdict, _, resim, build) = game(&app, event, 0, 1).await;
    assert_eq!(state, "counted");
    assert_eq!(verdict, Some(Verdict::PeerOnly));
    assert_eq!(resim, Some(ResimOutcome::Unavailable("no disc".into())));
    // The engine never ran, so no build is recorded.
    assert_eq!(build, None);
}

/// A server configured to re-simulate (`in_process`; the verifier itself is the fake).
fn in_process(c: &mut autobracket_server::Config) {
    c.verifier.mode = autobracket_server::config::VerifierMode::InProcess;
}

/// With `strict_resim`, a game the configured verifier could not check does not count: it
/// goes to the organiser as `VerifierUnavailable`.
#[tokio::test]
async fn unavailable_verifier_under_strict_resim_goes_to_queue() {
    let app = TestApp::spawn_with(
        |c| {
            in_process(c);
            c.strict_resim = true;
        },
        fake(vec![ResimOutcome::Unavailable("no disc".into())]),
    )
    .await;
    let (app, host, guest, event) = two_player(app).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    let (state, verdict, slot, resim, build) = game(&app, event, 0, 1).await;
    assert_eq!(state, "review");
    assert_eq!(verdict, Some(Verdict::VerifierUnavailable));
    assert_eq!(slot, None);
    assert_eq!(resim, Some(ResimOutcome::Unavailable("no disc".into())));
    assert_eq!(build, None);
    assert_eq!(set_row(&app, event, 0).await, ("review".into(), 0, 0));
    assert_eq!(
        queue_reasons(&app, event).await,
        vec!["VerifierUnavailable"]
    );
    assert!(quarantine_kinds(&app).await.is_empty());
    assert_eq!(event_view(&app, event).await.unverified_games, 0);
}

/// Without `strict_resim`, such a game counts peer-only, and the event view says how many
/// counted games no verifier checked.
#[tokio::test]
async fn unavailable_verifier_counts_peer_only_and_is_visible() {
    let app = TestApp::spawn_with(
        in_process,
        fake(vec![ResimOutcome::Unavailable("no disc".into())]),
    )
    .await;
    let (app, host, guest, event) = two_player(app).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    let (state, verdict, _, _, _) = game(&app, event, 0, 1).await;
    assert_eq!(state, "counted");
    assert_eq!(verdict, Some(Verdict::PeerOnly));
    // The second game is re-simulated (the fake answers Match).
    play_game(&app, event, 0, 2, &host, &guest, 1).await;
    assert_eq!(game(&app, event, 0, 2).await.1, Some(Verdict::Verified));
    assert_eq!(event_view(&app, event).await.unverified_games, 1);

    // With the verifier off, PeerOnly is the normal verdict and is not counted as unverified.
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    assert_eq!(game(&app, event, 0, 1).await.1, Some(Verdict::PeerOnly));
    assert_eq!(event_view(&app, event).await.unverified_games, 0);
}

/// An in-process verifier whose build is not on a non-empty `accepted_builds` refuses to
/// start.
#[tokio::test]
async fn in_process_verifier_build_must_be_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = autobracket_server::Config::new(vec!["ab".repeat(32)]);
    cfg.db_path = dir.path().join("db.sqlite");
    cfg.replay_dir = dir.path().join("replays");
    cfg.worker_enabled = false;
    in_process(&mut cfg);
    cfg.accepted_builds = vec!["melee 1.0".into()];
    let err = autobracket_server::build_app(cfg.clone(), fake(vec![]))
        .await
        .err()
        .expect("the fake build is not accepted");
    let err = format!("{err:#}");
    assert!(
        err.contains("accepted_builds") && err.contains("fake"),
        "{err}"
    );

    // The fake's build is accepted by its whole string.
    cfg.accepted_builds.push("fake".into());
    let (_, state) = autobracket_server::build_app(cfg, fake(vec![]))
        .await
        .unwrap();
    state.tasks.abort_all();
}

/// Re-simulation runs on the host's upload (slot 0), with the replay's last frame.
#[tokio::test]
async fn resimulation_receives_the_host_file() {
    struct Recording(Mutex<Vec<(std::path::PathBuf, i32)>>);
    #[async_trait::async_trait]
    impl autobracket_server::Resim for Recording {
        async fn check(&self, slp: &std::path::Path, last_frame: i32) -> ResimOutcome {
            self.0.lock().unwrap().push((slp.to_owned(), last_frame));
            ResimOutcome::Match
        }
        fn build(&self) -> String {
            "rec".into()
        }
    }
    let rec = Arc::new(Recording(Mutex::new(Vec::new())));
    let app = TestApp::spawn_with(|_| {}, rec.clone()).await;
    let (app, host, guest, event) = two_player(app).await;
    play_game(&app, event, 0, 1, &host, &guest, 2).await;
    let host_path: String = sqlx::query_scalar("SELECT path FROM uploads WHERE slot = 0")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let calls = rec.0.lock().unwrap().clone();
    assert_eq!(calls, vec![(host_path.into(), 5400)]);
}

#[tokio::test]
async fn game_never_counted_twice() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    play_game(&app, event, 0, 1, &host, &guest, 1).await;
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));

    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::CONFLICT
    );
    assert_eq!(judge_due(&app.state).await.unwrap(), 0);

    // A late or duplicate apply of the same verdict is a no-op.
    let game_id: i64 = sqlx::query_scalar("SELECT id FROM games WHERE event_id = ?")
        .bind(event)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    apply_verdict(
        &app.state,
        game_id,
        &Verdict::PeerOnly,
        Some(0),
        &ResimOutcome::Skipped,
        Some("none"),
    )
    .await
    .unwrap();
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));
    assert_eq!(
        scalar_i64(
            &app,
            "SELECT COUNT(*) FROM games WHERE event_id = ? AND state = 'counted'",
            event
        )
        .await,
        1
    );
}

#[tokio::test]
async fn restart_recovers_judging_games() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::ACCEPTED
    );
    // As if the server died mid-judging.
    sqlx::query("UPDATE games SET state = 'judging'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(judge_due(&app.state).await.unwrap(), 0);
    recover(&app.state).await.unwrap();
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);
    assert_eq!(game(&app, event, 0, 1).await.0, "counted");
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));

    // A lone upload whose timeout lapsed while the server was down.
    let app = TestApp::spawn(|c| c.upload_timeout_secs = 5).await;
    let (app, host, _guest, event) = two_player(app).await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    sqlx::query("UPDATE games SET state = 'judging', first_upload_at = ?")
        .bind(now_unix() - 10)
        .execute(&app.state.db)
        .await
        .unwrap();
    recover(&app.state).await.unwrap();
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);
    let (state, verdict, _, _, _) = game(&app, event, 0, 1).await;
    assert_eq!(state, "review");
    assert_eq!(verdict, Some(Verdict::Single));
}

#[tokio::test]
async fn grand_final_uses_best_of_finals() {
    let app = TestApp::spawn(|_| {}).await;
    let a = app.player("Alice").await;
    let b = app.player("Bob").await;
    let players = [&a, &b];
    let event = app.started_event(Format::DoubleElim, &players).await;

    let winners_final = load_bracket(&app.state.db, event)
        .await
        .unwrap()
        .ready_sets()[0];
    win_set(&app, event, winners_final, &players, 0, 2).await;
    assert_eq!(set_row(&app, event, winners_final).await.0, "done");

    let bracket = load_bracket(&app.state.db, event).await.unwrap();
    let ready = bracket.ready_sets();
    assert_eq!(ready.len(), 1);
    let gf = ready[0];
    assert_eq!(bracket.sets[gf as usize].side, Side::GrandFinal);

    // Best of 5: two wins are not enough.
    win_set(&app, event, gf, &players, 0, 2).await;
    let (state, w0, w1) = set_row(&app, event, gf).await;
    assert_eq!(state, "ready");
    assert_eq!(w0 + w1, 2);
    assert_eq!(event_view(&app, event).await.summary.state, "running");

    let [s0, s1] = seats(&app, event, gf).await;
    play_game(
        &app,
        event,
        gf,
        3,
        players[s0 as usize],
        players[s1 as usize],
        if s0 == 0 { 1 } else { 2 },
    )
    .await;
    assert_eq!(set_row(&app, event, gf).await.0, "done");
    let view = event_view(&app, event).await;
    assert_eq!(view.summary.state, "complete");
    assert_eq!(view.bracket.unwrap().champion(), Some(0));
}

#[tokio::test]
async fn dq_entrant_loses_newly_ready_set() {
    let app = TestApp::spawn(|_| {}).await;
    let mut players = Vec::new();
    for tag in ["P0", "P1", "P2", "P3"] {
        players.push(app.player(tag).await);
    }
    let refs: Vec<&TestPlayer> = players.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    let semis = load_bracket(&app.state.db, event)
        .await
        .unwrap()
        .ready_sets();
    assert_eq!(semis.len(), 2);
    let (with_0, other): (Vec<u32>, Vec<u32>) = {
        let mut a = Vec::new();
        let mut b = Vec::new();
        for s in semis {
            if seats(&app, event, s).await.contains(&0) {
                a.push(s);
            } else {
                b.push(s);
            }
        }
        (a, b)
    };

    // Seed 1 wins its semi, then is disqualified before the final opens.
    win_set(&app, event, other[0], &refs, 1, 2).await;
    sqlx::query("UPDATE entrants SET dq = 1 WHERE event_id = ? AND seed = 1")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    win_set(&app, event, with_0[0], &refs, 0, 2).await;

    let bracket = load_bracket(&app.state.db, event).await.unwrap();
    let final_set = bracket.sets.iter().find(|s| s.winner_to.is_none()).unwrap();
    let winner = match final_set.state {
        SetState::Done { winner } => winner,
        other => panic!("final {other:?}"),
    };
    assert_eq!(final_set.slots[winner as usize], Slot::Entrant(0));
    assert_eq!(
        set_row(&app, event, final_set.id).await,
        ("done".into(), 0, 0)
    );
    let view = event_view(&app, event).await;
    assert_eq!(view.summary.state, "complete");
    assert_eq!(view.bracket.unwrap().champion(), Some(0));
}

#[tokio::test]
async fn running_worker_judges_uploads() {
    let app = TestApp::spawn(|c| c.worker_enabled = true).await;
    let (app, host, guest, event) = two_player(app).await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::ACCEPTED
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while game(&app, event, 0, 1).await.0 != "counted" {
        assert!(Instant::now() < deadline, "worker never judged the game");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));
}

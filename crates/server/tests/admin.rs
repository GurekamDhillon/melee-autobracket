//! Organiser tooling: the review queue and its resolutions, player rekeying, the
//! quarantine corpus, and the `autobracket admin` command line.

use autobracket_server::lifecycle::{apply_verdict, award_set, load_bracket};
use autobracket_server::testkit::{play_game, TestApp, TestPlayer};
use autobracket_server::worker::judge_due;
use bracket::{Format, SetState, Slot};
use proto::{
    EventSummary, EventView, Next, QuarantineItem, QueueItem, RekeyReq, ResimOutcome, Resolve,
    SigningKey, Verdict,
};
use rand::rngs::OsRng;
use replay::testkit::simple_game;
use reqwest::StatusCode;
use serde_json::{json, Value};

const START_A: &str = "2026-11-15T20:00:00Z";
const START_B: &str = "2026-11-15T20:00:01Z";

async fn upload(p: &TestPlayer, event: i64, set: u32, n: i64, file: Vec<u8>) -> StatusCode {
    p.post_bytes(
        &format!("/v1/sets/{event}/{set}/games/{n}/replay"),
        file,
        &[("x-ab-build", "b1")],
    )
    .await
    .status()
}

/// Game `n` of `set` is uploaded as a disagreeing pair and judged `Conflicted`.
async fn conflict(
    app: &TestApp,
    event: i64,
    set: u32,
    n: i64,
    host: &TestPlayer,
    guest: &TestPlayer,
) {
    assert_eq!(
        upload(host, event, set, n, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(guest, event, set, n, simple_game(2, START_B)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);
}

async fn next(p: &TestPlayer, event: i64) -> Next {
    let res = p.get(&format!("/v1/me/next?event={event}")).await;
    assert_eq!(res.status(), StatusCode::OK, "next for {}", p.tag);
    res.json().await.unwrap()
}

async fn queue(app: &TestApp) -> Vec<QueueItem> {
    let res = app.as_to().get("/v1/admin/queue").await;
    assert_eq!(res.status(), StatusCode::OK);
    res.json().await.unwrap()
}

async fn resolve(app: &TestApp, id: i64, r: &Resolve) -> StatusCode {
    app.as_to()
        .post_json(&format!("/v1/admin/queue/{id}/resolve"), r)
        .await
        .status()
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

/// States of the games of `set`, oldest first.
async fn game_states(app: &TestApp, event: i64, set: u32) -> Vec<String> {
    sqlx::query_scalar("SELECT state FROM games WHERE event_id = ? AND set_id = ? ORDER BY id")
        .bind(event)
        .bind(set)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

async fn open_queue_rows(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM queue WHERE state = 'open'")
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

/// The slot that won a decided set.
async fn winner(app: &TestApp, event: i64, set: u32) -> u8 {
    match load_bracket(&app.state.db, event).await.unwrap().sets[set as usize].state {
        SetState::Done { winner } => winner,
        other => panic!("set {set} is {other:?}"),
    }
}

/// Plays enough games of `set` (best of 3) for entrant `seed` to win it.
async fn win_set(app: &TestApp, event: i64, set: u32, players: &[&TestPlayer], seed: u32) {
    let [s0, s1] = seats(app, event, set).await;
    let port = if s0 == seed { 1 } else { 2 };
    for n in 1..=2 {
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

/// The ready set whose slots hold `seed`.
async fn set_with(app: &TestApp, event: i64, seed: u32) -> u32 {
    let b = load_bracket(&app.state.db, event).await.unwrap();
    b.sets
        .iter()
        .find(|s| s.state == SetState::Ready && s.slots.contains(&Slot::Entrant(seed)))
        .map(|s| s.id)
        .unwrap_or_else(|| panic!("no ready set holds seed {seed}"))
}

/// `EventView::decided_by` of `event`.
async fn decided_by(app: &TestApp, event: i64) -> Vec<(u32, String)> {
    let res = app.as_to().get(&format!("/v1/events/{event}")).await;
    assert_eq!(res.status(), StatusCode::OK);
    res.json::<EventView>().await.unwrap().decided_by
}

async fn players(app: &TestApp, n: usize) -> Vec<TestPlayer> {
    let mut out = Vec::new();
    for i in 0..n {
        out.push(app.player(&format!("P{i}")).await);
    }
    out
}

#[tokio::test]
async fn queue_lists_conflict_with_evidence() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    conflict(&app, event, 0, 1, &ps[0], &ps[1]).await;

    let items = queue(&app).await;
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.event_id, event);
    assert_eq!(item.set_id, 0);
    assert_eq!(item.reason, "Conflicted");
    assert!(item.game_id.is_some());
    assert_eq!(item.verdict, Some(Verdict::Conflicted));
    assert_eq!(item.resim, Some(ResimOutcome::Skipped));

    // Who is in the set, and what each file says.
    assert_eq!(item.tags, Some(["P0".to_string(), "P1".to_string()]));
    assert_eq!(item.reporter_tag, None);
    let ev = item.evidence.as_ref().expect("evidence");
    assert_eq!(ev["game_id"], json!(item.game_id.unwrap()));
    assert_eq!(ev["game_no"], 1);
    assert_eq!(ev["state"], "review");
    assert_eq!(ev["verdict"], "Conflicted");
    let uploads = ev["uploads"].as_array().unwrap();
    assert_eq!(uploads.len(), 2);
    for (slot, (tag, winner)) in [("P0", 1), ("P1", 2)].into_iter().enumerate() {
        let u = &uploads[slot];
        assert_eq!(u["slot"], slot, "{u}");
        assert_eq!(u["tag"], tag, "{u}");
        assert_eq!(u["sha256"].as_str().unwrap().len(), 12, "{u}");
        assert_eq!(u["client_build"], "b1", "{u}");
        assert_eq!(u["parse"], "ok", "{u}");
        assert_eq!(u["stage"], 32, "{u}");
        assert_eq!(u["timer_secs"], 480, "{u}");
        assert_eq!(u["end_method"], 2, "{u}");
        assert_eq!(u["winner_port"], winner, "{u}");
        assert_eq!(u["legality"], "ok", "{u}");
        let players = u["players"].as_array().unwrap();
        assert_eq!(players.len(), 2, "{u}");
        assert_eq!(players[0]["port"], 1, "{u}");
        assert!(players[0]["character"].is_u64(), "{u}");
        assert!(players[0]["final_stocks"].is_u64(), "{u}");
        assert!(players[0]["final_percent"].is_number(), "{u}");
    }
    assert!(
        ev["resim"].is_null(),
        "a conflicted pair is not re-simulated: {ev}"
    );
}

/// A lone upload is re-simulated as evidence only: the outcome is shown to the organiser,
/// and the game still never counts.
#[tokio::test]
async fn single_upload_evidence_includes_an_evidence_only_resimulation() {
    use autobracket_server::testkit::FakeResim;
    let resim = std::sync::Arc::new(FakeResim(std::sync::Mutex::new(
        vec![ResimOutcome::Crashed(
            "boom\n2026 INFO forged\u{202e}".into(),
        )]
        .into(),
    )));
    let app = TestApp::spawn_with(
        |c| {
            c.upload_timeout_secs = 1;
            c.verifier.mode = autobracket_server::config::VerifierMode::InProcess;
        },
        resim,
    )
    .await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    assert_eq!(
        upload(&ps[1], event, 0, 1, simple_game(2, START_B)).await,
        StatusCode::ACCEPTED
    );
    sqlx::query("UPDATE games SET first_upload_at = first_upload_at - 2")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);

    let items = queue(&app).await;
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.verdict, Some(Verdict::Single));
    // The game itself was not re-simulated for counting.
    assert_eq!(item.resim, Some(ResimOutcome::Skipped));
    let ev = item.evidence.as_ref().expect("evidence");
    let uploads = ev["uploads"].as_array().unwrap();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0]["slot"], 1);
    assert_eq!(uploads[0]["tag"], "P1");
    assert_eq!(uploads[0]["winner_port"], 2);
    let resim = &ev["resim"];
    assert_eq!(resim["evidence_only"], true, "{ev}");
    assert_eq!(resim["slot"], 1, "{ev}");
    assert_eq!(resim["outcome"], "Crashed", "{ev}");
    assert_eq!(resim["build"], "fake", "{ev}");
    // Verifier text is cleaned like log text.
    let detail = resim["detail"].as_str().unwrap();
    assert!(detail.contains("boom"), "{detail}");
    assert!(
        !detail.contains('\n') && !detail.contains('\u{202e}'),
        "{detail:?}"
    );
    let (state, build): (String, Option<String>) =
        sqlx::query_as("SELECT state, verifier_build FROM games")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!((state.as_str(), build), ("review", None));
}

/// A no-show item names who reported it; a game judged after the set went to review (as an
/// upload accepted in review is) is voided, and its evidence is attached to the set's item.
#[tokio::test]
async fn set_items_carry_the_reporter_and_the_evidence_of_late_games() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    let res = ps[1]
        .post_json(
            &format!("/v1/sets/{event}/0/no-show"),
            &json!({ "detail": "never joined" }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let items = queue(&app).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].reason, "no-show");
    assert_eq!(items[0].reporter_tag.as_deref(), Some("P1"));
    assert_eq!(items[0].tags, Some(["P0".to_string(), "P1".to_string()]));
    assert_eq!(items[0].evidence, None);

    // The game was played after all: its uploads arrive while the set is in review (stored
    // here directly, as the upload route does for a set waiting on a no-show).
    sqlx::query("UPDATE sets SET state = 'ready'")
        .execute(&app.state.db)
        .await
        .unwrap();
    for (p, start) in [(&ps[0], START_A), (&ps[1], START_B)] {
        assert_eq!(
            upload(p, event, 0, 1, simple_game(1, start)).await,
            StatusCode::ACCEPTED
        );
    }
    sqlx::query("UPDATE sets SET state = 'review'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);
    assert_eq!(game_states(&app, event, 0).await, vec!["void"]);

    let items = queue(&app).await;
    assert_eq!(items.len(), 1);
    let ev = items[0]
        .evidence
        .as_ref()
        .expect("the late game's evidence");
    let games = ev.as_array().expect("one entry per game of the set");
    assert_eq!(games.len(), 1);
    assert_eq!(games[0]["state"], "void");
    assert_eq!(games[0]["verdict"], "PeerOnly");
    assert_eq!(games[0]["uploads"].as_array().unwrap().len(), 2);
    assert_eq!(games[0]["uploads"][0]["winner_port"], 1);
}

#[tokio::test]
async fn award_advances_bracket() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 4).await;
    let refs: Vec<&TestPlayer> = ps.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    let semi_a = set_with(&app, event, 0).await;
    let semi_b = set_with(&app, event, 1).await;
    win_set(&app, event, semi_a, &refs, 0).await;

    let [s0, s1] = seats(&app, event, semi_b).await;
    conflict(&app, event, semi_b, 1, &ps[s0 as usize], &ps[s1 as usize]).await;
    // A second open item on the same set is closed by the same award.
    sqlx::query(
        "INSERT INTO queue (event_id, set_id, game_id, reason, state, created_at)
         VALUES (?, ?, NULL, 'extra', 'open', 0)",
    )
    .bind(event)
    .bind(semi_b)
    .execute(&app.state.db)
    .await
    .unwrap();
    let item = queue(&app).await[0].id;

    assert_eq!(
        resolve(&app, item, &Resolve::Award { slot: 1 }).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(winner(&app, event, semi_b).await, 1);
    assert_eq!(set_row(&app, event, semi_b).await.0, "done");
    // The played semi is not listed; the awarded one is.
    assert_eq!(
        decided_by(&app, event).await,
        vec![(semi_b, "award".into())]
    );
    assert_eq!(game_states(&app, event, semi_b).await, vec!["void"]);
    assert!(queue(&app).await.is_empty());
    let json: String = sqlx::query_scalar("SELECT resolution_json FROM queue WHERE id = ?")
        .bind(item)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let stored: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(stored["resolve"], json!({ "Award": { "slot": 1 } }));
    assert_eq!(
        stored["by"],
        json!(hex::encode(app.to.verifying_key().to_bytes()))
    );
    assert!(stored["at"].as_i64().unwrap() > 0);

    // The final opens with the awarded entrant, and can be played to a champion.
    let final_set = set_with(&app, event, s1).await;
    assert_eq!(seats(&app, event, final_set).await, [0, s1]);
    win_set(&app, event, final_set, &refs, s1).await;
    assert_eq!(next(&ps[s1 as usize], event).await, Next::Champion);
    assert_eq!(
        decided_by(&app, event).await,
        vec![(semi_b, "award".into())]
    );
}

#[tokio::test]
async fn replay_voids_game_and_reopens_set() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    conflict(&app, event, 0, 1, &ps[0], &ps[1]).await;
    assert_eq!(next(&ps[0], event).await, Next::Review { set_id: 0 });
    let item = queue(&app).await[0].id;
    // A second open item on the same set is closed by the same replay.
    sqlx::query(
        "INSERT INTO queue (event_id, set_id, game_id, reason, state, created_at)
         VALUES (?, 0, NULL, 'extra', 'open', 0)",
    )
    .bind(event)
    .execute(&app.state.db)
    .await
    .unwrap();
    assert_eq!(queue(&app).await.len(), 2);

    assert_eq!(
        resolve(&app, item, &Resolve::Replay).await,
        StatusCode::NO_CONTENT
    );
    assert!(queue(&app).await.is_empty());
    assert_eq!(game_states(&app, event, 0).await, vec!["void"]);
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 0, 0));
    match next(&ps[0], event).await {
        Next::Play {
            game_no, room_code, ..
        } => {
            assert_eq!(game_no, 1);
            assert_eq!(room_code, None);
        }
        other => panic!("expected Play, got {other:?}"),
    }

    // Game 1 again, as a fresh row, and this time it counts.
    play_game(&app, event, 0, 1, &ps[0], &ps[1], 1).await;
    assert_eq!(game_states(&app, event, 0).await, vec!["void", "counted"]);
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));
}

#[tokio::test]
async fn dq_awards_current_and_future_sets() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 4).await;
    let refs: Vec<&TestPlayer> = ps.iter().collect();
    let event = app.started_event(Format::DoubleElim, &refs).await;
    let with_1 = set_with(&app, event, 1).await;
    let with_0 = set_with(&app, event, 0).await;
    let [s0, s1] = seats(&app, event, with_1).await;
    conflict(&app, event, with_1, 1, &ps[s0 as usize], &ps[s1 as usize]).await;
    let item = queue(&app).await[0].id;

    let dq_slot = if s0 == 1 { 0 } else { 1 };
    assert_eq!(
        resolve(&app, item, &Resolve::Dq { slot: dq_slot }).await,
        StatusCode::NO_CONTENT
    );
    let dq: Vec<i64> =
        sqlx::query_scalar("SELECT dq FROM entrants WHERE event_id = ? ORDER BY seed")
            .bind(event)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(dq, vec![0, 1, 0, 0]);
    assert_eq!(winner(&app, event, with_1).await, 1 - dq_slot);
    assert_eq!(game_states(&app, event, with_1).await, vec!["void"]);

    // Seed 1 dropped to the losers bracket; once its opponent arrives that set is
    // awarded without being played.
    win_set(&app, event, with_0, &refs, 0).await;
    let bracket = load_bracket(&app.state.db, event).await.unwrap();
    let losers = bracket
        .sets
        .iter()
        .find(|s| s.slots.contains(&Slot::Entrant(1)) && s.side == bracket::Side::Losers)
        .expect("seed 1 in the losers bracket");
    let SetState::Done { winner } = losers.state else {
        panic!("losers set {:?}", losers.state);
    };
    assert_eq!(losers.slots[winner as usize], Slot::Entrant(3));
    assert_eq!(set_row(&app, event, losers.id).await, ("done".into(), 0, 0));
    let mut expected = vec![(with_1, "dq".to_string()), (losers.id, "dq".to_string())];
    expected.sort();
    assert_eq!(decided_by(&app, event).await, expected);
    assert_eq!(next(&ps[1], event).await, Next::Eliminated);
    assert!(queue(&app).await.is_empty());
}

#[tokio::test]
async fn dq_from_decided_set_awards_already_ready_set() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 4).await;
    let refs: Vec<&TestPlayer> = ps.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    let semi_a = set_with(&app, event, 0).await;
    let semi_b = set_with(&app, event, 1).await;
    win_set(&app, event, semi_a, &refs, 0).await;
    win_set(&app, event, semi_b, &refs, 1).await;
    let final_set = set_with(&app, event, 0).await;

    // An item left on seed 0's finished semi; disqualifying through it decides the
    // final seed 0 is already sitting in.
    let item: i64 = sqlx::query_scalar(
        "INSERT INTO queue (event_id, set_id, game_id, reason, state, created_at)
         VALUES (?, ?, NULL, 'late', 'open', 0) RETURNING id",
    )
    .bind(event)
    .bind(semi_a)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let slot = if seats(&app, event, semi_a).await[0] == 0 {
        0
    } else {
        1
    };
    assert_eq!(
        resolve(&app, item, &Resolve::Dq { slot }).await,
        StatusCode::NO_CONTENT
    );

    let [f0, _] = seats(&app, event, final_set).await;
    assert_eq!(
        winner(&app, event, final_set).await,
        if f0 == 0 { 1 } else { 0 }
    );
    assert_eq!(next(&ps[1], event).await, Next::Champion);
    assert!(queue(&app).await.is_empty());
}

#[tokio::test]
async fn double_dq_set_is_decided_and_queued() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 4).await;
    let refs: Vec<&TestPlayer> = ps.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    let semi_a = set_with(&app, event, 0).await;
    let semi_b = set_with(&app, event, 1).await;
    win_set(&app, event, semi_b, &refs, 1).await;
    sqlx::query("UPDATE entrants SET dq = 1 WHERE event_id = ? AND seed IN (0, 1)")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    win_set(&app, event, semi_a, &refs, 0).await;

    let bracket = load_bracket(&app.state.db, event).await.unwrap();
    let final_set = bracket.sets.iter().find(|s| s.winner_to.is_none()).unwrap();
    assert_eq!(final_set.state, SetState::Done { winner: 0 });
    assert!(bracket.is_complete());
    assert_eq!(
        decided_by(&app, event).await,
        vec![(final_set.id, "double-dq".into())]
    );
    let items = queue(&app).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].reason, "double-dq");
    assert_eq!(items[0].set_id, final_set.id);
    assert_eq!(items[0].game_id, None);

    // The decided set cannot be replayed or re-awarded; awarding the recorded winner
    // acknowledges the item.
    assert_eq!(
        resolve(&app, items[0].id, &Resolve::Replay).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        resolve(&app, items[0].id, &Resolve::Award { slot: 1 }).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        resolve(&app, items[0].id, &Resolve::Award { slot: 0 }).await,
        StatusCode::NO_CONTENT
    );
    assert!(queue(&app).await.is_empty());
}

#[tokio::test]
async fn resolve_twice_is_409() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    conflict(&app, event, 0, 1, &ps[0], &ps[1]).await;
    let item = queue(&app).await[0].id;

    assert_eq!(
        resolve(&app, item, &Resolve::Award { slot: 2 }).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        resolve(&app, item, &Resolve::Dq { slot: 7 }).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        resolve(&app, item + 100, &Resolve::Replay).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        resolve(&app, item, &Resolve::Replay).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        resolve(&app, item, &Resolve::Replay).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        resolve(&app, item, &Resolve::Award { slot: 0 }).await,
        StatusCode::CONFLICT
    );
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 0, 0));
}

#[tokio::test]
async fn queue_requires_to() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    conflict(&app, event, 0, 1, &ps[0], &ps[1]).await;
    let item = queue(&app).await[0].id;
    let p = &ps[0];

    assert_eq!(
        p.get("/v1/admin/queue").await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        p.post_json(
            &format!("/v1/admin/queue/{item}/resolve"),
            &Resolve::Award { slot: 0 }
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        p.post_json(
            &format!(
                "/v1/admin/players/{}/rekey",
                hex::encode(p.sk.verifying_key().to_bytes())
            ),
            &RekeyReq {
                new_key: "ab".repeat(32)
            }
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        p.get("/v1/quarantine").await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        p.get("/v1/quarantine/1/replay/0").await.status(),
        StatusCode::FORBIDDEN
    );
    let unsigned = reqwest::get(format!("{}/v1/admin/queue", app.url))
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(open_queue_rows(&app).await, 1);
}

#[tokio::test]
async fn rekey_moves_identity() {
    let app = TestApp::spawn(|c| c.require_invite = true).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    let before = next(&ps[0], event).await;
    let old = hex::encode(ps[0].sk.verifying_key().to_bytes());
    let new_sk = SigningKey::generate(&mut OsRng);
    let new = hex::encode(new_sk.verifying_key().to_bytes());
    let rekey = |old: String, new_key: String| {
        let to = app.as_to();
        async move {
            to.post_json(
                &format!("/v1/admin/players/{old}/rekey"),
                &RekeyReq { new_key },
            )
            .await
            .status()
        }
    };

    assert_eq!(
        rekey("00".repeat(32), new.clone()).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        rekey(old.clone(), new.to_uppercase()).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        rekey(old.clone(), "ab".repeat(31)).await,
        StatusCode::BAD_REQUEST
    );
    let taken = hex::encode(ps[1].sk.verifying_key().to_bytes());
    assert_eq!(rekey(old.clone(), taken).await, StatusCode::CONFLICT);
    assert_eq!(
        rekey(old.clone(), new.clone()).await,
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        ps[0]
            .get(&format!("/v1/me/next?event={event}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let moved = app.client(new_sk, "P0");
    assert_eq!(next(&moved, event).await, before);
    let used_by: Vec<Option<String>> =
        sqlx::query_scalar("SELECT used_by FROM invites ORDER BY used_by")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(used_by.contains(&Some(new.clone())));
    assert!(!used_by.contains(&Some(old.clone())));
    assert_eq!(rekey(old, new).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn quarantine_serves_replay_bytes() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    let host_file = simple_game(1, START_A);
    let guest_file = simple_game(2, START_B);
    assert_eq!(
        upload(&ps[0], event, 0, 1, host_file.clone()).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&ps[1], event, 0, 1, guest_file.clone()).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);

    let res = app.as_to().get("/v1/quarantine").await;
    assert_eq!(res.status(), StatusCode::OK);
    let items: Vec<QuarantineItem> = res.json().await.unwrap();
    assert_eq!(items.len(), 1);
    let q = &items[0];
    assert_eq!(q.kind, "Conflicted");
    assert_eq!(q.verdict, Some(Verdict::Conflicted));
    assert_eq!(q.resim, Some(ResimOutcome::Skipped));
    // Never re-simulated, so no verifier build is recorded.
    assert_eq!(q.verifier_build, None);
    assert_eq!(q.client_builds, vec!["b1", "b1"]);
    assert!(q.created_at > 0);
    let judged = q.judged_at.expect("judged_at");
    assert!(
        judged >= q.created_at - 5 && judged <= q.created_at + 5,
        "{judged}"
    );

    for (slot, file) in [(0, &host_file), (1, &guest_file)] {
        let res = app
            .as_to()
            .get(&format!("/v1/quarantine/{}/replay/{slot}", q.id))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.bytes().await.unwrap().as_ref(), file.as_slice());
    }
    for path in [
        format!("/v1/quarantine/{}/replay/2", q.id),
        format!("/v1/quarantine/{}/replay/0", q.id + 1),
    ] {
        assert_eq!(app.as_to().get(&path).await.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn quarantine_single_upload_has_no_guest_replay() {
    let app = TestApp::spawn(|c| c.upload_timeout_secs = 1).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    assert_eq!(
        upload(&ps[0], event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    // The upload timeout has passed.
    sqlx::query("UPDATE games SET first_upload_at = first_upload_at - 2")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(judge_due(&app.state).await.unwrap(), 1);
    let items: Vec<QuarantineItem> = app
        .as_to()
        .get("/v1/quarantine")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(items[0].kind, "Single");
    assert_eq!(items[0].client_builds, vec!["b1"]);
    // The worker does not run the engine on a lone upload.
    assert_eq!(items[0].verifier_build, None);
    assert!(items[0].judged_at.is_some());
    let to = app.as_to();
    for (slot, status) in [(0, StatusCode::OK), (1, StatusCode::NOT_FOUND)] {
        let path = format!("/v1/quarantine/{}/replay/{slot}", items[0].id);
        assert_eq!(to.get(&path).await.status(), status);
    }
}

/// Claims game 1 of set 0 as the worker would, then decides the set behind its back.
async fn judging_game_of_decided_set(app: &TestApp, ps: &[TestPlayer], event: i64) -> i64 {
    assert_eq!(
        upload(&ps[0], event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&ps[1], event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::ACCEPTED
    );
    let game_id: i64 = sqlx::query_scalar("UPDATE games SET state = 'judging' RETURNING id")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    award_set(&app.state, event, 0, 1).await.unwrap();
    game_id
}

#[tokio::test]
async fn late_counting_verdict_for_decided_set_is_void() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    let game_id = judging_game_of_decided_set(&app, &ps, event).await;

    apply_verdict(
        &app.state,
        game_id,
        &Verdict::PeerOnly,
        Some(0),
        &ResimOutcome::Skipped,
        Some("none"),
        None,
    )
    .await
    .unwrap();
    let (state, verdict, build, judged_at): (String, Option<String>, Option<String>, Option<i64>) =
        sqlx::query_as(
            "SELECT state, verdict_json, verifier_build, judged_at FROM games WHERE id = ?",
        )
        .bind(game_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(state, "void");
    assert!(judged_at.is_some(), "a late verdict is still timestamped");
    assert_eq!(
        serde_json::from_str::<Verdict>(&verdict.unwrap()).unwrap(),
        Verdict::PeerOnly
    );
    assert_eq!(build.as_deref(), Some("none"));
    assert_eq!(set_row(&app, event, 0).await, ("done".into(), 0, 0));
    assert_eq!(winner(&app, event, 0).await, 1);
    assert_eq!(open_queue_rows(&app).await, 0);
    assert_eq!(judge_due(&app.state).await.unwrap(), 0);
}

#[tokio::test]
async fn late_noncounting_verdict_does_not_reopen_set() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    let game_id = judging_game_of_decided_set(&app, &ps, event).await;

    apply_verdict(
        &app.state,
        game_id,
        &Verdict::Conflicted,
        None,
        &ResimOutcome::Skipped,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(game_states(&app, event, 0).await, vec!["void"]);
    assert_eq!(set_row(&app, event, 0).await, ("done".into(), 0, 0));
    assert_eq!(open_queue_rows(&app).await, 0);
    // The replay still joins the corpus, as it would have on time.
    let quarantined: Vec<(i64, String)> = sqlx::query_as("SELECT game_id, kind FROM quarantine")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert_eq!(quarantined, vec![(game_id, "Conflicted".to_string())]);
    assert_eq!(judge_due(&app.state).await.unwrap(), 0);
}

/// A server ruleset that differs from the protocol default.
fn configured_ruleset(c: &mut autobracket_server::Config) {
    c.ruleset.legal_stages = vec![32];
    c.ruleset.stocks = 3;
    c.ruleset.best_of_finals = 7;
}

async fn create(app: &TestApp, body: Value) -> (StatusCode, Value) {
    let res = app.as_to().post_json("/v1/admin/events", &body).await;
    let status = res.status();
    let body: Value = res.json().await.unwrap();
    if status != StatusCode::CREATED {
        return (status, body);
    }
    let view: Value = app
        .as_to()
        .get(&format!("/v1/events/{}", body["id"]))
        .await
        .json()
        .await
        .unwrap();
    (status, view["ruleset"].clone())
}

#[tokio::test]
async fn best_of_overrides_configured_ruleset() {
    let app = TestApp::spawn(configured_ruleset).await;
    let base = json!({ "name": "W", "format": "SingleElim", "ruleset": null });

    let mut req = base.clone();
    req["best_of"] = json!(5);
    let (status, ruleset) = create(&app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(ruleset["best_of"], 5);
    assert_eq!(ruleset["best_of_finals"], 7);
    assert_eq!(ruleset["legal_stages"], json!([32]));
    assert_eq!(ruleset["stocks"], 3);

    // Without `best_of` the configured ruleset is used as is.
    let (status, ruleset) = create(&app, base.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(ruleset["best_of"], 3);
    assert_eq!(ruleset["legal_stages"], json!([32]));

    // A supplied ruleset is the base `best_of` overrides.
    let mut req = base.clone();
    req["ruleset"] = json!({ "stocks": 2, "best_of": 3 });
    req["best_of"] = json!(1);
    let (status, ruleset) = create(&app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(ruleset["best_of"], 1);
    assert_eq!(ruleset["stocks"], 2);

    for n in [0, 4] {
        let mut req = base.clone();
        req["best_of"] = json!(n);
        assert_eq!(
            create(&app, req).await,
            (
                StatusCode::BAD_REQUEST,
                json!({ "error": format!("best_of must be an odd number of games, not {n}") })
            )
        );
    }
    for (field, n) in [("best_of", 2), ("best_of_finals", 0), ("best_of_finals", 6)] {
        let mut req = base.clone();
        req["ruleset"] = json!({ field: n });
        assert_eq!(
            create(&app, req).await,
            (
                StatusCode::BAD_REQUEST,
                json!({ "error": format!("{field} must be an odd number of games, not {n}") })
            )
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 3);
}

/// An event's ruleset is checked like the configured one: rules no game could pass, a
/// malformed Gecko hash and an unknown rule are refused.
#[tokio::test]
async fn create_event_validates_the_whole_ruleset() {
    let app = TestApp::spawn(|_| {}).await;
    for (ruleset, field) in [
        (json!({ "stocks": 0 }), "stocks"),
        (json!({ "legal_stages": [] }), "legal_stages"),
        (json!({ "allowed_gecko": ["ABC"] }), "allowed_gecko"),
        (json!({ "best_of_final": 5 }), "best_of_final"),
    ] {
        let req = json!({ "name": "W", "format": "SingleElim", "ruleset": ruleset });
        let (status, body) = create(&app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {body}");
        let error = body["error"].as_str().unwrap();
        assert!(error.contains(field), "{field}: {error}");
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn cli_creates_event() {
    let app = TestApp::spawn(configured_ruleset).await;
    let key_file = app.dir.path().join("to.key");
    std::fs::write(&key_file, format!("{}\n", hex::encode(app.to.to_bytes()))).unwrap();
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_autobracket"))
        .args(["admin", "--server", &app.url, "--key-file"])
        .arg(&key_file)
        .args([
            "event",
            "create",
            "--name",
            "Friday",
            "--format",
            "double",
            "--best-of",
            "5",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let created: EventSummary = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(created.name, "Friday");

    let events: Vec<EventSummary> = app.as_to().get("/v1/events").await.json().await.unwrap();
    assert_eq!(events, vec![created.clone()]);
    let view: Value = app
        .as_to()
        .get(&format!("/v1/events/{}", created.id))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(view["ruleset"]["best_of"], 5);
    assert_eq!(view["ruleset"]["best_of_finals"], 7);
    assert_eq!(view["ruleset"]["legal_stages"], json!([32]));
    assert_eq!(view["ruleset"]["stocks"], 3);

    // A refused call prints the error body to stderr and exits 1.
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_autobracket"))
        .args(["admin", "--server", &app.url, "--key-file"])
        .arg(&key_file)
        .args(["event", "start", "999"])
        .output()
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"], "no such event");
}

/// Runs `autobracket admin --server URL --key-file KEY ARGS...` as the organiser.
async fn cli(app: &TestApp, args: &[&str]) -> std::process::Output {
    let key_file = app.dir.path().join("cli-to.key");
    if !key_file.exists() {
        std::fs::write(&key_file, format!("{}\n", hex::encode(app.to.to_bytes()))).unwrap();
    }
    tokio::process::Command::new(env!("CARGO_BIN_EXE_autobracket"))
        .args(["admin", "--server", &app.url, "--key-file"])
        .arg(&key_file)
        .args(args)
        .output()
        .await
        .unwrap()
}

/// `queue list` prints a line per item with the players and what the evidence says, then the
/// JSON.
#[tokio::test]
async fn cli_queue_list_summarises_each_item() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    conflict(&app, event, 0, 1, &ps[0], &ps[1]).await;

    let out = cli(&app, &["queue", "list"]).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let (summary, json) = stdout.split_at(stdout.find("\n[").expect("JSON after the summary") + 1);
    let lines: Vec<&str> = summary.lines().collect();
    assert_eq!(lines.len(), 1, "{summary}");
    let line = lines[0];
    for part in [
        "Conflicted",
        "P0",
        "P1",
        "slot 0",
        "slot 1",
        "winner p1",
        "winner p2",
    ] {
        assert!(line.contains(part), "{part:?} missing from {line:?}");
    }
    let items: Vec<QueueItem> = serde_json::from_str(json).unwrap();
    assert_eq!(items.len(), 1);
}

/// `quarantine get ID SLOT --out FILE` saves a quarantined replay, and never overwrites a file.
#[tokio::test]
async fn cli_quarantine_get_saves_the_replay() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    conflict(&app, event, 0, 1, &ps[0], &ps[1]).await;
    let id: i64 = sqlx::query_scalar("SELECT id FROM quarantine")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let stored: String = sqlx::query_scalar("SELECT path FROM uploads WHERE slot = 1")
        .fetch_one(&app.state.db)
        .await
        .unwrap();

    let dest = app.dir.path().join("guest.slp");
    let dest_arg = dest.to_str().unwrap();
    let out = cli(
        &app,
        &["quarantine", "get", &id.to_string(), "1", "--out", dest_arg],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        std::fs::read(&stored).unwrap()
    );

    // The file exists now: refused, and left alone.
    let out = cli(
        &app,
        &["quarantine", "get", &id.to_string(), "0", "--out", dest_arg],
    )
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        std::fs::read(&stored).unwrap()
    );
    // No such slot: the server's refusal, and no file.
    let missing = app.dir.path().join("none.slp");
    let out = cli(
        &app,
        &[
            "quarantine",
            "get",
            &id.to_string(),
            "2",
            "--out",
            missing.to_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such replay"));
    assert!(!missing.exists());
}

/// A signed `DELETE` (the test client has no helper for it).
async fn signed_delete(url: &str, sk: &SigningKey, path: &str) -> StatusCode {
    let h = proto::sign_request(sk, "DELETE", path, autobracket_server::db::now_unix(), &[]);
    reqwest::Client::new()
        .delete(format!("{url}{path}"))
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .send()
        .await
        .unwrap()
        .status()
}

async fn entrant_seeds(app: &TestApp, event: i64) -> Vec<(String, i64)> {
    sqlx::query_as(
        "SELECT p.tag, n.seed FROM entrants n JOIN players p ON p.key = n.player_key
         WHERE n.event_id = ? ORDER BY n.seed",
    )
    .bind(event)
    .fetch_all(&app.state.db)
    .await
    .unwrap()
}

/// An organiser removes an entrant during sign-up; later seeds move up, so seeds stay
/// contiguous. Not once the event has started.
#[tokio::test]
async fn entrant_remove_during_signup_only() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 4).await;
    let event = app.create_event(Format::SingleElim).await;
    for p in &ps {
        p.sign_up(event).await;
    }
    let to = app.as_to();
    let delete = |event: i64, tag: &str| {
        let path = format!("/v1/admin/events/{event}/entrants/{tag}");
        let sk = app.to.clone();
        let url = app.url.clone();
        async move { signed_delete(&url, &sk, &path).await }
    };

    // Only an organiser may.
    assert_eq!(
        signed_delete(
            &app.url,
            &ps[0].sk,
            &format!("/v1/admin/events/{event}/entrants/P1")
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(delete(event, "P1").await, StatusCode::NO_CONTENT);
    assert_eq!(
        entrant_seeds(&app, event).await,
        vec![("P0".into(), 0), ("P2".into(), 1), ("P3".into(), 2)]
    );
    assert_eq!(delete(event, "P1").await, StatusCode::NOT_FOUND);
    assert_eq!(delete(event, "Nobody").await, StatusCode::NOT_FOUND);
    assert_eq!(delete(999, "P0").await, StatusCode::NOT_FOUND);

    // The CLI does the same; a later sign-up takes the next seed.
    let out = cli(&app, &["entrant", "remove", &event.to_string(), "P0"]).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        entrant_seeds(&app, event).await,
        vec![("P2".into(), 0), ("P3".into(), 1)]
    );
    ps[1].sign_up(event).await;
    assert_eq!(
        entrant_seeds(&app, event).await,
        vec![("P2".into(), 0), ("P3".into(), 1), ("P1".into(), 2)]
    );

    let res = to
        .post_json(&format!("/v1/admin/events/{event}/start"), &json!({}))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(delete(event, "P2").await, StatusCode::CONFLICT);
    assert_eq!(entrant_seeds(&app, event).await.len(), 3);
}

/// An organiser replays a game after one was counted: the set stays at 1-0 and the replayed
/// game keeps its number, 2.
#[tokio::test]
async fn replay_after_a_counted_game_keeps_the_game_number() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 2).await;
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    play_game(&app, event, 0, 1, &ps[0], &ps[1], 1).await;
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));
    conflict(&app, event, 0, 2, &ps[0], &ps[1]).await;
    assert_eq!(set_row(&app, event, 0).await, ("review".into(), 1, 0));
    let item = queue(&app).await.remove(0);
    assert_eq!(
        resolve(&app, item.id, &Resolve::Replay).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 1, 0));
    assert_eq!(game_states(&app, event, 0).await, vec!["counted", "void"]);
    match next(&ps[0], event).await {
        Next::Play { game_no, wins, .. } => assert_eq!((game_no, wins), (2, [1, 0])),
        other => panic!("{other:?}"),
    }
    // Game 2 again; game 1 is not accepted any more.
    assert_eq!(
        upload(&ps[0], event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::CONFLICT
    );
    play_game(&app, event, 0, 2, &ps[0], &ps[1], 1).await;
    assert_eq!(set_row(&app, event, 0).await, ("done".into(), 2, 0));
}

/// Disqualifications through the resolution route that leave two disqualified entrants
/// facing each other: in double elimination both drop to the losers bracket, where their set
/// is decided for slot 0 with a `double-dq` queue item.
#[tokio::test]
async fn double_dq_through_the_resolution_route() {
    let app = TestApp::spawn(|_| {}).await;
    let ps = players(&app, 4).await;
    let refs: Vec<&TestPlayer> = ps.iter().collect();
    let event = app.started_event(Format::DoubleElim, &refs).await;
    for seed in [0u32, 1] {
        let semi = set_with(&app, event, seed).await;
        let [s0, s1] = seats(&app, event, semi).await;
        conflict(&app, event, semi, 1, refs[s0 as usize], refs[s1 as usize]).await;
        let item = queue(&app)
            .await
            .into_iter()
            .find(|i| i.set_id == semi)
            .unwrap();
        let slot = if s0 == seed { 0 } else { 1 };
        assert_eq!(
            resolve(&app, item.id, &Resolve::Dq { slot }).await,
            StatusCode::NO_CONTENT
        );
        assert!(decided_by(&app, event).await.contains(&(semi, "dq".into())));
    }
    // Seeds 0 and 1 met in the losers bracket, both disqualified.
    let bracket = load_bracket(&app.state.db, event).await.unwrap();
    let losers = bracket
        .sets
        .iter()
        .find(|s| {
            s.slots == [Slot::Entrant(0), Slot::Entrant(1)]
                || s.slots == [Slot::Entrant(1), Slot::Entrant(0)]
        })
        .expect("the disqualified pair's set");
    assert_eq!(losers.state, SetState::Done { winner: 0 });
    assert!(decided_by(&app, event)
        .await
        .contains(&(losers.id, "double-dq".into())));
    let items = queue(&app).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].reason, "double-dq");
    assert_eq!(items[0].set_id, losers.id);
}

/// A set waiting on a no-show with a game of it uploaded meanwhile (`uploading` or `judging`,
/// as the upload route allows): the organiser's decision voids that game in the same
/// transaction, so it can neither be joined nor count afterwards.
async fn no_show_set_with_game(app: &TestApp, ps: &[TestPlayer], game_state: &str) -> (i64, i64) {
    let event = app
        .started_event(Format::SingleElim, &[&ps[0], &ps[1]])
        .await;
    let res = ps[1]
        .post_json(&format!("/v1/sets/{event}/0/no-show"), &json!({}))
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    // The host's upload arrives while the set is in review (stored through the route with the
    // set briefly ready, as the review-time upload route would store it).
    sqlx::query("UPDATE sets SET state = 'ready' WHERE event_id = ?")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        upload(&ps[0], event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    sqlx::query("UPDATE sets SET state = 'review' WHERE event_id = ?")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    let game_id: i64 =
        sqlx::query_scalar("UPDATE games SET state = ? WHERE event_id = ? RETURNING id")
            .bind(game_state)
            .bind(event)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    (event, game_id)
}

#[tokio::test]
async fn replay_voids_a_game_uploaded_during_review() {
    for game_state in ["uploading", "judging"] {
        let app = TestApp::spawn(|_| {}).await;
        let ps = players(&app, 2).await;
        let (event, game_id) = no_show_set_with_game(&app, &ps, game_state).await;
        let item = queue(&app).await.remove(0);
        assert_eq!(
            resolve(&app, item.id, &Resolve::Replay).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            game_states(&app, event, 0).await,
            vec!["void"],
            "{game_state}"
        );
        assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 0, 0));

        // A verdict for the voided game arriving late changes nothing.
        apply_verdict(
            &app.state,
            game_id,
            &Verdict::PeerOnly,
            Some(0),
            &ResimOutcome::Skipped,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(game_states(&app, event, 0).await, vec!["void"]);
        assert_eq!(set_row(&app, event, 0).await, ("ready".into(), 0, 0));

        // The next upload starts a fresh game 1.
        assert_eq!(
            upload(&ps[1], event, 0, 1, simple_game(1, START_B)).await,
            StatusCode::ACCEPTED
        );
        let games: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, state FROM games WHERE event_id = ? ORDER BY id")
                .bind(event)
                .fetch_all(&app.state.db)
                .await
                .unwrap();
        assert_eq!(games.len(), 2);
        assert_ne!(games[1].0, game_id);
        assert_eq!(games[1].1, "uploading");
        assert_eq!(
            judge_due(&app.state).await.unwrap(),
            0,
            "one upload, not yet due"
        );
    }
}

#[tokio::test]
async fn award_voids_a_game_uploaded_during_review() {
    for game_state in ["uploading", "judging"] {
        let app = TestApp::spawn(|_| {}).await;
        let ps = players(&app, 2).await;
        let (event, _) = no_show_set_with_game(&app, &ps, game_state).await;
        let item = queue(&app).await.remove(0);
        assert_eq!(
            resolve(&app, item.id, &Resolve::Award { slot: 0 }).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            game_states(&app, event, 0).await,
            vec!["void"],
            "{game_state}"
        );
        assert_eq!(set_row(&app, event, 0).await, ("done".into(), 0, 0));
        assert_eq!(judge_due(&app.state).await.unwrap(), 0);
    }
}

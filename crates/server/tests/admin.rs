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
    )
    .await
    .unwrap();
    assert_eq!(game_states(&app, event, 0).await, vec!["void"]);
    assert_eq!(set_row(&app, event, 0).await, ("done".into(), 0, 0));
    assert_eq!(open_queue_rows(&app).await, 0);
    let quarantined: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quarantine")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(quarantined, 0);
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

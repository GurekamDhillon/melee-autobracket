use std::path::PathBuf;
use std::time::Duration;

use autobracket_server::lifecycle::{load_bracket, store_bracket};
use autobracket_server::testkit::{TestApp, TestPlayer};
use bracket::{Format, Side};
use proto::{Next, Role, RoomReq};
use replay::testkit::simple_game;
use reqwest::StatusCode;
use serde_json::{json, Value};

const START_A: &str = "2026-11-15T20:00:00Z";
const START_B: &str = "2026-11-15T20:00:01Z";

async fn next(p: &TestPlayer, event: i64) -> Next {
    let res = p.get(&format!("/v1/me/next?event={event}")).await;
    assert_eq!(res.status(), StatusCode::OK, "next for {}", p.tag);
    res.json().await.unwrap()
}

/// The `room_code` and `awaiting_verdict` of a `Play`.
async fn play_state(p: &TestPlayer, event: i64) -> (Option<String>, bool) {
    match next(p, event).await {
        Next::Play {
            room_code,
            awaiting_verdict,
            ..
        } => (room_code, awaiting_verdict),
        other => panic!("{} expected Play, got {other:?}", p.tag),
    }
}

async fn room(p: &TestPlayer, event: i64, set: u32, code: &str) -> StatusCode {
    p.post_json(
        &format!("/v1/sets/{event}/{set}/room"),
        &RoomReq { code: code.into() },
    )
    .await
    .status()
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

async fn count(app: &TestApp, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

/// Every file under the replay directory.
fn stored_files(app: &TestApp) -> Vec<PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries {
            let path = e.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&app.state.cfg.replay_dir, &mut out);
    out
}

/// A started 2-player single elimination event: `(app, host, guest, event)`.
async fn two_player() -> (TestApp, TestPlayer, TestPlayer, i64) {
    two_player_with(|_| {}).await
}

/// [`two_player`] on a server configured by `edit`.
async fn two_player_with(
    edit: impl FnOnce(&mut autobracket_server::Config),
) -> (TestApp, TestPlayer, TestPlayer, i64) {
    let app = TestApp::spawn(edit).await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    (app, host, guest, event)
}

#[tokio::test]
async fn next_roles() {
    let (app, host, guest, event) = two_player().await;
    let relay = app.state.cfg.relay.clone();
    assert_eq!(
        next(&host, event).await,
        Next::Play {
            event_id: event,
            set_id: 0,
            opponent_tag: "Bob".into(),
            role: Role::Host,
            relay: relay.clone(),
            room_code: None,
            game_no: 1,
            wins: [0, 0],
            best_of: 3,
            stocks: 4,
            timer_secs: 480,
            awaiting_verdict: false,
        }
    );
    assert_eq!(
        next(&guest, event).await,
        Next::Play {
            event_id: event,
            set_id: 0,
            opponent_tag: "Alice".into(),
            role: Role::Guest,
            relay,
            room_code: None,
            game_no: 1,
            wins: [0, 0],
            best_of: 3,
            stocks: 4,
            timer_secs: 480,
            awaiting_verdict: false,
        }
    );
}

#[tokio::test]
async fn next_states() {
    let app = TestApp::spawn(|_| {}).await;
    let p = app.player("Alice").await;
    let event = app.create_event(Format::SingleElim).await;
    assert_eq!(next(&p, event).await, Next::NotEntered);
    p.sign_up(event).await;
    assert_eq!(next(&p, event).await, Next::Waiting);

    let missing = p.get("/v1/me/next?event=999").await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let bad = p.get("/v1/me/next").await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let body: Value = bad.json().await.unwrap();
    assert!(body["error"].is_string(), "{body}");
}

/// Seed 0 has a bye in a 3-player bracket: alive, but nothing to play yet.
#[tokio::test]
async fn next_waiting_on_feeder_set() {
    let app = TestApp::spawn(|_| {}).await;
    let a = app.player("Alice").await;
    let b = app.player("Bob").await;
    let c = app.player("Carol").await;
    let event = app.started_event(Format::SingleElim, &[&a, &b, &c]).await;
    assert_eq!(next(&a, event).await, Next::Waiting);
    assert!(matches!(next(&b, event).await, Next::Play { .. }));
    assert!(matches!(next(&c, event).await, Next::Play { .. }));
}

#[tokio::test]
async fn next_review_champion_eliminated() {
    let (app, host, guest, event) = two_player().await;
    sqlx::query("UPDATE sets SET state = 'review' WHERE event_id = ? AND set_id = 0")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        next(&host, event).await,
        Next::Review {
            event_id: event,
            set_id: 0,
        }
    );
    assert_eq!(
        next(&guest, event).await,
        Next::Review {
            event_id: event,
            set_id: 0,
        }
    );

    let mut b = load_bracket(&app.state.db, event).await.unwrap();
    b.report(0, 0).unwrap();
    store_bracket(&app.state.db, event, &b).await.unwrap();
    sqlx::query("UPDATE sets SET state = 'done', wins0 = 2 WHERE event_id = ? AND set_id = 0")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(next(&host, event).await, Next::Champion);
    assert_eq!(next(&guest, event).await, Next::Eliminated);
}

/// `wins` is `[me, opponent]` and `game_no` counts the counted games.
#[tokio::test]
async fn next_score_is_from_my_side() {
    let (app, host, guest, event) = two_player().await;
    sqlx::query(
        "INSERT INTO games (event_id, set_id, game_no, state, winner_slot) VALUES (?, 0, 1, 'counted', 1)",
    )
    .bind(event)
    .execute(&app.state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE sets SET wins1 = 1 WHERE event_id = ? AND set_id = 0")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    match next(&host, event).await {
        Next::Play { game_no, wins, .. } => assert_eq!((game_no, wins), (2, [0, 1])),
        other => panic!("{other:?}"),
    }
    match next(&guest, event).await {
        Next::Play { game_no, wins, .. } => assert_eq!((game_no, wins), (2, [1, 0])),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn grand_final_uses_best_of_finals() {
    let app = TestApp::spawn(|_| {}).await;
    let a = app.player("Alice").await;
    let b = app.player("Bob").await;
    let event = app.started_event(Format::DoubleElim, &[&a, &b]).await;
    let mut br = load_bracket(&app.state.db, event).await.unwrap();
    let first = br.ready_sets()[0];
    let opened = br.report(first, 0).unwrap();
    store_bracket(&app.state.db, event, &br).await.unwrap();
    sqlx::query("UPDATE sets SET state = 'done' WHERE event_id = ? AND set_id = ?")
        .bind(event)
        .bind(first)
        .execute(&app.state.db)
        .await
        .unwrap();
    let gf = opened
        .into_iter()
        .find(|&s| br.sets[s as usize].side == Side::GrandFinal)
        .expect("grand final opened");
    sqlx::query(
        "INSERT INTO sets (event_id, set_id, state, wins0, wins1) VALUES (?, ?, 'ready', 0, 0)",
    )
    .bind(event)
    .bind(gf)
    .execute(&app.state.db)
    .await
    .unwrap();
    match next(&a, event).await {
        Next::Play {
            set_id, best_of, ..
        } => assert_eq!((set_id, best_of), (gf, 5)),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn room_code_flow() {
    let (_app, host, guest, event) = two_player().await;
    assert_eq!(room(&guest, event, 0, "KQ7X").await, StatusCode::FORBIDDEN);
    assert_eq!(play_state(&host, event).await.0, None);

    assert_eq!(room(&host, event, 0, "KQ7X").await, StatusCode::NO_CONTENT);
    assert_eq!(play_state(&guest, event).await.0.as_deref(), Some("KQ7X"));

    assert_eq!(room(&host, event, 0, "AB12").await, StatusCode::NO_CONTENT);
    assert_eq!(play_state(&guest, event).await.0.as_deref(), Some("AB12"));

    assert_eq!(
        room(&host, event, 0, "bad code!").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(play_state(&guest, event).await.0.as_deref(), Some("AB12"));

    // A direct host:port address is also accepted, where the server allows it.
    assert_eq!(
        room(&host, event, 0, "10.0.0.5:51600").await,
        StatusCode::BAD_REQUEST
    );
    let (_app, host, guest, event) = two_player_with(|c| c.allow_direct_connect = true).await;
    assert_eq!(
        room(&host, event, 0, "10.0.0.5:51600").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        play_state(&guest, event).await.0.as_deref(),
        Some("10.0.0.5:51600")
    );
}

#[tokio::test]
async fn room_code_validation() {
    let (_app, host, _guest, event) = two_player_with(|c| c.allow_direct_connect = true).await;
    let longest_host = format!("{}:51600", "a".repeat(253));
    for good in [
        "KQ7X",
        "0000",
        "relay.gsd.sh:51600",
        "a-b.c:1",
        "x:12345",
        "x:65535",
        longest_host.as_str(),
    ] {
        assert_eq!(
            room(&host, event, 0, good).await,
            StatusCode::NO_CONTENT,
            "{good:?}"
        );
    }
    let too_long_host = format!("{}:51600", "a".repeat(254));
    for bad in [
        too_long_host.as_str(),
        "x:0",
        "x:00000",
        "x:65536",
        "x:99999",
        "",
        "kq7x",
        "KQ7",
        "KQ7XY",
        "KQ 7",
        ":1",
        "host:",
        "host:123456",
        "h_st:1",
        "a:b:1",
        "host:-1",
    ] {
        assert_eq!(
            room(&host, event, 0, bad).await,
            StatusCode::BAD_REQUEST,
            "{bad:?}"
        );
    }
    let res = host
        .post_json(&format!("/v1/sets/{event}/0/room"), &json!({"nope": 1}))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn room_code_needs_ready_set() {
    let (app, host, _guest, event) = two_player().await;
    assert_eq!(room(&host, event, 9, "KQ7X").await, StatusCode::NOT_FOUND);
    sqlx::query("UPDATE sets SET state = 'review' WHERE event_id = ? AND set_id = 0")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(room(&host, event, 0, "KQ7X").await, StatusCode::CONFLICT);
}

#[tokio::test]
async fn upload_stores_and_is_idempotent() {
    let (app, host, _guest, event) = two_player().await;
    let file = simple_game(1, START_A);
    let other = simple_game(1, START_B);
    assert_ne!(file, other);

    assert_eq!(
        upload(&host, event, 0, 1, file.clone()).await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(1), app.state.wake.notified())
        .await
        .expect("upload wakes the background worker");

    assert_eq!(
        upload(&host, event, 0, 1, file.clone()).await,
        StatusCode::OK
    );
    assert_eq!(
        upload(&host, event, 0, 1, other).await,
        StatusCode::CONFLICT
    );

    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 1);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM games").await, 1);
    let (game_id, slot, path, sha, build): (i64, i64, String, String, String) =
        sqlx::query_as("SELECT game_id, slot, path, sha256, client_build FROM uploads")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(slot, 0);
    assert_eq!(build, "b1");
    use sha2::Digest;
    assert_eq!(sha, hex::encode(sha2::Sha256::digest(&file)));
    let expected = app
        .state
        .cfg
        .replay_dir
        .join(event.to_string())
        .join("0")
        .join(game_id.to_string())
        .join("0.slp");
    assert_eq!(PathBuf::from(&path), expected);
    assert_eq!(std::fs::read(&expected).unwrap(), file);
    assert_eq!(stored_files(&app), vec![expected]);
}

/// While the game is being judged, re-sending the same file is still a harmless `200`
/// (a client retrying after a lost response) and stores nothing new; a different file is `409`.
#[tokio::test]
async fn reupload_while_judging_is_idempotent() {
    let (app, host, _guest, event) = two_player().await;
    let file = simple_game(1, START_A);
    assert_eq!(
        upload(&host, event, 0, 1, file.clone()).await,
        StatusCode::ACCEPTED
    );
    sqlx::query("UPDATE games SET state = 'judging'")
        .execute(&app.state.db)
        .await
        .unwrap();

    assert_eq!(upload(&host, event, 0, 1, file).await, StatusCode::OK);
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM uploads WHERE slot = 0").await,
        1
    );
    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 1);
    assert_eq!(stored_files(&app).len(), 1);
}

#[tokio::test]
async fn upload_without_build_header_stores_empty_build() {
    let (app, host, _guest, event) = two_player().await;
    let res = host
        .post_bytes(
            &format!("/v1/sets/{event}/0/games/1/replay"),
            simple_game(1, START_A),
            &[],
        )
        .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let build: String = sqlx::query_scalar("SELECT client_build FROM uploads")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(build, "");
}

#[tokio::test]
async fn upload_wrong_game_no_is_409() {
    let (app, host, _guest, event) = two_player().await;
    for n in [0, 2, 3] {
        assert_eq!(
            upload(&host, event, 0, n, simple_game(1, START_A)).await,
            StatusCode::CONFLICT,
            "game {n}"
        );
    }
    assert_eq!(count(&app, "SELECT COUNT(*) FROM games").await, 0);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 0);
    assert!(stored_files(&app).is_empty());
}

/// Uploads go to the current game, which only advances once a game is counted.
#[tokio::test]
async fn upload_follows_current_game() {
    let (app, host, guest, event) = two_player().await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(count(&app, "SELECT COUNT(*) FROM games").await, 1);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 2);

    // Being judged: nothing more is accepted for it.
    sqlx::query("UPDATE games SET state = 'judging'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert!(play_state(&host, event).await.1);
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::CONFLICT
    );

    // Counted: game 2 is next and is a fresh row.
    sqlx::query("UPDATE games SET state = 'counted', winner_slot = 0")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::CONFLICT
    );
    assert!(!play_state(&host, event).await.1);
    assert_eq!(
        upload(&host, event, 0, 2, simple_game(1, START_B)).await,
        StatusCode::ACCEPTED
    );
    let games: Vec<(i64, String)> = sqlx::query_as("SELECT game_no, state FROM games ORDER BY id")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert_eq!(games, vec![(1, "counted".into()), (2, "uploading".into())]);

    // A voided game is replayed under the same number.
    sqlx::query("UPDATE games SET state = 'void' WHERE game_no = 2")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert!(!play_state(&host, event).await.1);
    assert_eq!(
        upload(&host, event, 0, 2, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM games WHERE game_no = 2").await,
        2
    );
}

#[tokio::test]
async fn upload_needs_ready_set() {
    let (app, host, _guest, event) = two_player().await;
    assert_eq!(
        upload(&host, event, 9, 1, simple_game(1, START_A)).await,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE sets SET state = 'review' WHERE event_id = ? AND set_id = 0")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 0);
}

#[tokio::test]
async fn first_upload_clears_code_and_sets_awaiting() {
    let (app, host, guest, event) = two_player().await;
    assert_eq!(room(&host, event, 0, "KQ7X").await, StatusCode::NO_CONTENT);
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(play_state(&host, event).await, (None, true));
    assert_eq!(play_state(&guest, event).await, (None, false));
    let (state, first): (String, Option<i64>) =
        sqlx::query_as("SELECT state, first_upload_at FROM games")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(state, "uploading");
    assert!(first.is_some());

    // No new code while the game awaits its second upload (the host would start it again).
    assert_eq!(room(&host, event, 0, "AB12").await, StatusCode::CONFLICT);
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(play_state(&guest, event).await, (None, true));
    let slots: Vec<i64> = sqlx::query_scalar("SELECT slot FROM uploads ORDER BY slot")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert_eq!(slots, vec![0, 1]);
}

#[tokio::test]
async fn upload_garbage_is_400_and_stores_nothing() {
    let (app, host, _guest, event) = two_player().await;
    let full = simple_game(1, START_A);
    let truncated = full[..full.len() / 2].to_vec();
    assert!(replay::parse(&truncated).is_err());
    for bad in [b"nope".to_vec(), truncated] {
        assert_eq!(
            upload(&host, event, 0, 1, bad).await,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(count(&app, "SELECT COUNT(*) FROM games").await, 0);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 0);
    assert!(stored_files(&app).is_empty());
}

#[tokio::test]
async fn upload_over_limit_is_413_and_stores_nothing() {
    const LIMIT: usize = 256;
    let app = TestApp::spawn(|c| c.max_replay_bytes = LIMIT).await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    let file = simple_game(1, START_A);
    assert!(file.len() > LIMIT, "test replay is {} bytes", file.len());
    assert_eq!(
        upload(&host, event, 0, 1, file).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(count(&app, "SELECT COUNT(*) FROM games").await, 0);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 0);
    assert!(stored_files(&app).is_empty());
}

#[tokio::test]
async fn outsider_is_403() {
    let (app, _host, _guest, event) = two_player().await;
    let outsider = app.player("Carol").await;
    assert_eq!(
        room(&outsider, event, 0, "KQ7X").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        upload(&outsider, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::FORBIDDEN
    );
    // Even garbage: membership is checked before the file is looked at.
    assert_eq!(
        upload(&outsider, event, 0, 1, b"nope".to_vec()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(count(&app, "SELECT COUNT(*) FROM games").await, 0);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM uploads").await, 0);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM sets WHERE room_code IS NOT NULL"
        )
        .await,
        0
    );
    assert!(stored_files(&app).is_empty());
    assert_eq!(next(&outsider, event).await, Next::NotEntered);
}

/// `host:port` codes (a direct connection to the host) are refused unless the server allows them.
#[tokio::test]
async fn direct_connect_is_off_by_default() {
    let (_app, host, _guest, event) = two_player().await;
    let res = host
        .post_json(
            &format!("/v1/sets/{event}/0/room"),
            &RoomReq {
                code: "relay.gsd.sh:51600".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: Value = res.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("direct"), "{body}");
    assert_eq!(room(&host, event, 0, "KQ7X").await, StatusCode::NO_CONTENT);
}

/// A host whose upload went in cannot hand out a new code until the game is judged.
#[tokio::test]
async fn room_code_is_refused_while_a_game_is_in_progress() {
    let (app, host, guest, event) = two_player().await;
    assert_eq!(room(&host, event, 0, "KQ7X").await, StatusCode::NO_CONTENT);
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(1, START_B)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(room(&host, event, 0, "AB12").await, StatusCode::CONFLICT);
    // Judging too.
    sqlx::query("UPDATE games SET state = 'judging'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(room(&host, event, 0, "AB12").await, StatusCode::CONFLICT);
    // Once the game is decided, the next game's code is welcome.
    sqlx::query("UPDATE games SET state = 'counted'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(room(&host, event, 0, "AB12").await, StatusCode::NO_CONTENT);
}

async fn no_show(p: &TestPlayer, event: i64, set: u32, body: Value) -> reqwest::Response {
    p.post_json(&format!("/v1/sets/{event}/{set}/no-show"), &body)
        .await
}

async fn queue_reasons(app: &TestApp) -> Vec<(String, String)> {
    sqlx::query_as("SELECT reason, state FROM queue ORDER BY id")
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

/// A guest may still be joining a freshly posted room: a `no-show` within the grace period is
/// refused; `client-failure` (the reporter's own client gave up) has no grace period.
#[tokio::test]
async fn no_show_grace_period_after_a_room_code() {
    let (app, host, guest, event) = two_player_with(|c| c.no_show_grace_secs = 90).await;
    // No code posted yet: a no-show is fine at once... but first the code.
    assert_eq!(room(&host, event, 0, "KQ7X").await, StatusCode::NO_CONTENT);
    for body in [
        json!({}),
        json!({"kind": "no-show"}),
        json!({"detail": "gone"}),
    ] {
        let res = no_show(&host, event, 0, body.clone()).await;
        assert_eq!(res.status(), StatusCode::CONFLICT, "{body}");
        let err: Value = res.json().await.unwrap();
        assert_eq!(
            err,
            json!({"error": "a room code was posted less than 90 seconds ago"})
        );
    }
    // An unknown kind is a bad request.
    let res = no_show(&guest, event, 0, json!({"kind": "rage-quit"})).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(queue_reasons(&app).await.is_empty());

    // Once the grace period has passed, the report goes through.
    sqlx::query("UPDATE sets SET room_posted_at = room_posted_at - 91")
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = no_show(&host, event, 0, json!({"kind": "no-show"})).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        queue_reasons(&app).await,
        vec![("no-show".into(), "open".into())]
    );
}

#[tokio::test]
async fn client_failure_has_no_grace_period() {
    let (app, host, _guest, event) = two_player_with(|c| c.no_show_grace_secs = 90).await;
    assert_eq!(room(&host, event, 0, "KQ7X").await, StatusCode::NO_CONTENT);
    let res = no_show(
        &host,
        event,
        0,
        json!({"kind": "client-failure", "detail": "engine exited 1 four times"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        queue_reasons(&app).await,
        vec![("client-failure".into(), "open".into())]
    );
    let detail: String = sqlx::query_scalar("SELECT detail FROM queue")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(detail.contains("slot 0"), "{detail}");
    assert!(detail.contains("engine exited 1 four times"), "{detail}");
    assert_eq!(
        next(&host, event).await,
        Next::Review {
            event_id: event,
            set_id: 0,
        }
    );
}

/// Reports `kind` on set 0 (grace period off) and checks the set went to review.
async fn report(app: &TestApp, p: &TestPlayer, event: i64, kind: &str) {
    let res = no_show(p, event, 0, json!({ "kind": kind })).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT, "{kind}");
    let state: String = sqlx::query_scalar("SELECT state FROM sets WHERE set_id = 0")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(state, "review");
}

/// After a no-show report (or a stall flag, or a client failure) the game that was in fact
/// played is still uploaded: judged as evidence, never counted.
#[tokio::test]
async fn review_for_a_no_show_still_takes_the_played_game() {
    for kind in ["no-show", "client-failure", "stalled"] {
        let (app, host, guest, event) = two_player().await;
        if kind == "stalled" {
            // As the worker's stall check does it.
            sqlx::query("UPDATE sets SET state = 'review' WHERE set_id = 0")
                .execute(&app.state.db)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO queue (event_id, set_id, reason, state, created_at)
                 VALUES (?, 0, 'stalled', 'open', 0)",
            )
            .bind(event)
            .execute(&app.state.db)
            .await
            .unwrap();
        } else {
            report(&app, &guest, event, kind).await;
        }

        // The wrong game number is still refused.
        assert_eq!(
            upload(&host, event, 0, 2, simple_game(1, START_A)).await,
            StatusCode::CONFLICT,
            "{kind}"
        );
        assert_eq!(
            upload(&host, event, 0, 1, b"nope".to_vec()).await,
            StatusCode::BAD_REQUEST,
            "{kind}"
        );
        assert_eq!(
            upload(&host, event, 0, 1, simple_game(1, START_A)).await,
            StatusCode::ACCEPTED,
            "{kind}"
        );
        // Idempotent, and one file per slot.
        assert_eq!(
            upload(&host, event, 0, 1, simple_game(1, START_A)).await,
            StatusCode::OK,
            "{kind}"
        );
        assert_eq!(
            upload(&host, event, 0, 1, simple_game(2, START_A)).await,
            StatusCode::CONFLICT,
            "{kind}"
        );
        assert_eq!(
            upload(&guest, event, 0, 1, simple_game(1, START_B)).await,
            StatusCode::ACCEPTED,
            "{kind}"
        );
        // Both players are still told to wait for the organiser.
        assert_eq!(
            next(&host, event).await,
            Next::Review {
                event_id: event,
                set_id: 0,
            }
        );

        autobracket_server::worker::judge_due(&app.state)
            .await
            .unwrap();
        // The verdict is stored, the game voided, and nothing counts.
        let (state, verdict): (String, Option<String>) =
            sqlx::query_as("SELECT state, verdict_json FROM games")
                .fetch_one(&app.state.db)
                .await
                .unwrap();
        assert_eq!(state, "void", "{kind}");
        assert!(verdict.is_some(), "{kind}");
        let (set_state, wins0, wins1): (String, i64, i64) =
            sqlx::query_as("SELECT state, wins0, wins1 FROM sets WHERE set_id = 0")
                .fetch_one(&app.state.db)
                .await
                .unwrap();
        assert_eq!(
            (set_state.as_str(), wins0, wins1),
            ("review", 0, 0),
            "{kind}"
        );
        assert_eq!(
            queue_reasons(&app).await,
            vec![(kind.to_owned(), "open".to_owned())]
        );

        // A retry of either file after the verdict is still a no-op, and no second game
        // opens for this review.
        assert_eq!(
            upload(&guest, event, 0, 1, simple_game(1, START_B)).await,
            StatusCode::OK,
            "{kind}"
        );
        assert_eq!(count(&app, "SELECT COUNT(*) FROM games").await, 1);
        assert!(
            stored_files(&app)
                .iter()
                .all(|f| f.extension().is_some_and(|e| e == "slp")),
            "no temporary files are left behind"
        );
    }
}

/// A set in review for a verdict (the organiser must look at the games already uploaded)
/// takes no further uploads.
#[tokio::test]
async fn review_for_a_verdict_takes_no_upload() {
    let (app, host, guest, event) = two_player().await;
    // Two uploads that disagree on the winner.
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&guest, event, 0, 1, simple_game(2, START_B)).await,
        StatusCode::ACCEPTED
    );
    autobracket_server::worker::judge_due(&app.state)
        .await
        .unwrap();
    assert_eq!(
        next(&host, event).await,
        Next::Review {
            event_id: event,
            set_id: 0,
        }
    );
    let reasons = queue_reasons(&app).await;
    assert_eq!(reasons.len(), 1);
    assert!(!["no-show", "stalled", "client-failure"].contains(&reasons[0].0.as_str()));
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::CONFLICT
    );
    // Mixed: a no-show item next to a verdict item still takes nothing.
    sqlx::query(
        "INSERT INTO queue (event_id, set_id, reason, state, created_at)
         VALUES (?, 0, 'no-show', 'open', 0)",
    )
    .bind(event)
    .execute(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::CONFLICT
    );
}

/// Temporary files of refused or repeated uploads are removed.
#[tokio::test]
async fn refused_uploads_leave_no_temporary_files() {
    let (app, host, _guest, event) = two_player().await;
    let file = simple_game(1, START_A);
    assert_eq!(
        upload(&host, event, 0, 1, file.clone()).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(upload(&host, event, 0, 1, file).await, StatusCode::OK);
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(2, START_A)).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        upload(&host, event, 0, 2, simple_game(2, START_A)).await,
        StatusCode::CONFLICT
    );
    let files = stored_files(&app);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with("0.slp"), "{files:?}");
}

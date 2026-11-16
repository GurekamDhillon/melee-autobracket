//! A set that is not being played reaches the organiser: a player reports a no-show, or the
//! judging worker finds the set stalled.

use autobracket_server::lifecycle::load_bracket;
use autobracket_server::testkit::{play_game, TestApp, TestPlayer};
use autobracket_server::worker::flag_stalled_sets;
use bracket::{Format, SetState, Slot};
use proto::{EventView, Next, NoShowReq, QueueItem, Resolve, RoomReq};
use replay::testkit::simple_game;
use reqwest::StatusCode;

const START_A: &str = "2026-11-15T20:00:00Z";

async fn no_show(p: &TestPlayer, event: i64, set: u32, detail: Option<&str>) -> StatusCode {
    p.post_json(
        &format!("/v1/sets/{event}/{set}/no-show"),
        &NoShowReq {
            detail: detail.map(Into::into),
        },
    )
    .await
    .status()
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

async fn upload(p: &TestPlayer, event: i64, set: u32, n: i64, file: Vec<u8>) -> StatusCode {
    p.post_bytes(
        &format!("/v1/sets/{event}/{set}/games/{n}/replay"),
        file,
        &[("x-ab-build", "b1")],
    )
    .await
    .status()
}

/// `(state, room_code)` of a `sets` row.
async fn set_row(app: &TestApp, event: i64, set: u32) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, room_code FROM sets WHERE event_id = ? AND set_id = ?")
        .bind(event)
        .bind(set)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

/// Moves the set's last activity `secs` into the past.
async fn age_set(app: &TestApp, event: i64, set: u32, secs: i64) {
    sqlx::query("UPDATE sets SET updated_at = updated_at - ? WHERE event_id = ? AND set_id = ?")
        .bind(secs)
        .bind(event)
        .bind(set)
        .execute(&app.state.db)
        .await
        .unwrap();
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

/// The ready set whose slots hold `seed`.
async fn set_with(app: &TestApp, event: i64, seed: u32) -> u32 {
    let b = load_bracket(&app.state.db, event).await.unwrap();
    b.sets
        .iter()
        .find(|s| s.state == SetState::Ready && s.slots.contains(&Slot::Entrant(seed)))
        .map(|s| s.id)
        .unwrap_or_else(|| panic!("no ready set holds seed {seed}"))
}

#[tokio::test]
async fn reporter_not_in_set_is_403() {
    let app = TestApp::spawn(|_| {}).await;
    let mut ps = Vec::new();
    for i in 0..4 {
        ps.push(app.player(&format!("P{i}")).await);
    }
    let refs: Vec<&TestPlayer> = ps.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    let outsider = app.player("Outsider").await;
    let set0 = set_with(&app, event, 0).await;
    let other = set_with(&app, event, 1).await;
    assert_ne!(set0, other);

    assert_eq!(
        no_show(&outsider, event, set0, None).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        no_show(&ps[0], event, other, None).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        no_show(&ps[0], event, 99, None).await,
        StatusCode::NOT_FOUND
    );
    assert!(queue(&app).await.is_empty());
    assert_eq!(set_row(&app, event, set0).await.0, "ready");
}

#[tokio::test]
async fn no_show_sends_set_to_review() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    let res = host
        .post_json(
            &format!("/v1/sets/{event}/0/room"),
            &RoomReq {
                code: "AB12".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let long = "x".repeat(300);
    assert_eq!(
        no_show(&host, event, 0, Some(&long)).await,
        StatusCode::NO_CONTENT
    );

    let items = queue(&app).await;
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.reason, "no-show");
    assert_eq!((item.event_id, item.set_id, item.game_id), (event, 0, None));
    let detail = item.detail.as_deref().expect("no-show detail");
    assert!(detail.contains("slot 0"), "{detail}");
    assert!(detail.contains("Alice"), "{detail}");
    assert!(detail.contains(&"x".repeat(200)), "{detail}");
    assert!(!detail.contains(&"x".repeat(201)), "{detail}");

    assert_eq!(set_row(&app, event, 0).await, ("review".into(), None));
    assert_eq!(next(&host, event).await, Next::Review { set_id: 0 });
    assert_eq!(next(&guest, event).await, Next::Review { set_id: 0 });

    // A second report, from either player, is refused.
    assert_eq!(no_show(&guest, event, 0, None).await, StatusCode::CONFLICT);
    assert_eq!(no_show(&host, event, 0, None).await, StatusCode::CONFLICT);
    assert_eq!(queue(&app).await.len(), 1);
}

#[tokio::test]
async fn no_show_without_detail_records_the_slot() {
    let (app, _host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    assert_eq!(
        no_show(&guest, event, 0, None).await,
        StatusCode::NO_CONTENT
    );
    let detail = queue(&app).await[0].detail.clone().unwrap();
    assert!(detail.contains("slot 1"), "{detail}");
    assert!(detail.contains("Bob"), "{detail}");
}

#[tokio::test]
async fn no_show_while_a_game_is_in_progress_is_409() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    // One upload in: the game is being played or uploaded.
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(no_show(&guest, event, 0, None).await, StatusCode::CONFLICT);
    // Awaiting a verdict.
    sqlx::query("UPDATE games SET state = 'judging'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(no_show(&host, event, 0, None).await, StatusCode::CONFLICT);
    assert_eq!(no_show(&guest, event, 0, None).await, StatusCode::CONFLICT);
    assert!(queue(&app).await.is_empty());
    assert_eq!(set_row(&app, event, 0).await.0, "ready");
}

#[tokio::test]
async fn replay_resolution_reopens_the_set() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    assert_eq!(
        no_show(&host, event, 0, Some("never joined")).await,
        StatusCode::NO_CONTENT
    );
    let item = queue(&app).await[0].id;
    assert_eq!(
        resolve(&app, item, &Resolve::Replay).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(set_row(&app, event, 0).await.0, "ready");
    assert!(matches!(next(&guest, event).await, Next::Play { .. }));
    // The reopened set counts as fresh: it is not flagged as stalled straight away.
    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 0);

    // Best of 3: two games won by the guest decide it.
    play_game(&app, event, 0, 1, &host, &guest, 2).await;
    play_game(&app, event, 0, 2, &host, &guest, 2).await;
    assert_eq!(next(&guest, event).await, Next::Champion);
    assert_eq!(next(&host, event).await, Next::Eliminated);
}

#[tokio::test]
async fn award_to_reporter_advances_bracket() {
    let (app, host, guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    assert_eq!(
        no_show(&guest, event, 0, None).await,
        StatusCode::NO_CONTENT
    );
    let item = queue(&app).await[0].id;
    assert_eq!(
        resolve(&app, item, &Resolve::Award { slot: 1 }).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(next(&guest, event).await, Next::Champion);
    assert_eq!(next(&host, event).await, Next::Eliminated);
    assert!(queue(&app).await.is_empty());
    let view: EventView = app
        .as_to()
        .get(&format!("/v1/events/{event}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(view.decided_by, vec![(0, "award".into())]);
    assert_eq!(view.scores, vec![(0, [0, 0])]);
}

#[tokio::test]
async fn stalled_set_is_flagged_and_fresh_one_is_not() {
    let app = TestApp::spawn(|_| {}).await;
    let mut ps = Vec::new();
    for i in 0..4 {
        ps.push(app.player(&format!("P{i}")).await);
    }
    let refs: Vec<&TestPlayer> = ps.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    let old = set_with(&app, event, 0).await;
    let fresh = set_with(&app, event, 1).await;
    age_set(&app, event, old, 1801).await;
    // 10 s short of the limit (default 1800 s).
    age_set(&app, event, fresh, 1790).await;

    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 1);
    let items = queue(&app).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].reason, "stalled");
    assert_eq!((items[0].set_id, items[0].game_id), (old, None));
    assert!(items[0].detail.is_some());
    assert_eq!(set_row(&app, event, old).await, ("review".into(), None));
    assert_eq!(set_row(&app, event, fresh).await.0, "ready");
    let [s0, s1] = load_bracket(&app.state.db, event).await.unwrap().sets[old as usize]
        .slots
        .map(|s| match s {
            Slot::Entrant(e) => e as usize,
            other => panic!("{other:?}"),
        });
    assert_eq!(next(&ps[s0], event).await, Next::Review { set_id: old });
    assert_eq!(next(&ps[s1], event).await, Next::Review { set_id: old });

    // Flagged once only.
    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 0);
    assert_eq!(queue(&app).await.len(), 1);
}

#[tokio::test]
async fn posting_a_room_code_is_activity() {
    let (app, host, _guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    age_set(&app, event, 0, 5000).await;
    let res = host
        .post_json(
            &format!("/v1/sets/{event}/0/room"),
            &RoomReq {
                code: "AB12".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 0);
}

#[tokio::test]
async fn stalled_check_ignores_a_set_with_a_game_in_progress() {
    let (app, host, _guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    assert_eq!(
        upload(&host, event, 0, 1, simple_game(1, START_A)).await,
        StatusCode::ACCEPTED
    );
    age_set(&app, event, 0, 5000).await;
    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 0);
    sqlx::query("UPDATE games SET state = 'judging'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 0);
    assert_eq!(set_row(&app, event, 0).await.0, "ready");
    assert!(queue(&app).await.is_empty());
}

#[tokio::test]
async fn stalled_check_ignores_events_not_running() {
    let (app, _host, _guest, event) = two_player(TestApp::spawn(|_| {}).await).await;
    age_set(&app, event, 0, 5000).await;
    sqlx::query("UPDATE events SET state = 'complete' WHERE id = ?")
        .bind(event)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 0);
    assert_eq!(set_row(&app, event, 0).await.0, "ready");
}

#[tokio::test]
async fn stalled_check_is_off_at_zero() {
    let app = TestApp::spawn(|c| c.set_stall_secs = 0).await;
    let (app, _host, _guest, event) = two_player(app).await;
    age_set(&app, event, 0, 1_000_000).await;
    assert_eq!(flag_stalled_sets(&app.state).await.unwrap(), 0);
    assert_eq!(set_row(&app, event, 0).await.0, "ready");
}

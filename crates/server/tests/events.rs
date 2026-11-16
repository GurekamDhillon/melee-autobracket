use autobracket_server::lifecycle::MAX_ENTRANTS;
use autobracket_server::testkit::{TestApp, TestPlayer};
use bracket::{Format, SetState};
use proto::{EventCreateReq, EventSummary, EventView, SignupReq};
use reqwest::StatusCode;
use serde_json::{json, Value};

fn create_req(format: Format) -> EventCreateReq {
    EventCreateReq {
        name: "Weekly".into(),
        format,
        ruleset: None,
        best_of: None,
    }
}

/// Creates an event as the organiser and returns its id.
async fn create_event(app: &TestApp, format: Format) -> i64 {
    let res = app
        .as_to()
        .post_json("/v1/admin/events", &create_req(format))
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let summary: EventSummary = res.json().await.unwrap();
    summary.id
}

async fn signup(p: &TestPlayer, event: i64, build: &str) -> reqwest::Response {
    p.post_json(
        &format!("/v1/events/{event}/entrants"),
        &SignupReq {
            engine_build: build.into(),
        },
    )
    .await
}

async fn start(app: &TestApp, event: i64) -> reqwest::Response {
    app.as_to()
        .post_json(&format!("/v1/admin/events/{event}/start"), &json!({}))
        .await
}

async fn sets_rows(app: &TestApp, event: i64) -> Vec<(i64, String)> {
    sqlx::query_as("SELECT set_id, state FROM sets WHERE event_id = ? ORDER BY set_id")
        .bind(event)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

/// Registers `n` players and signs each up, in order.
async fn sign_up_n(app: &TestApp, event: i64, n: usize) -> Vec<TestPlayer> {
    let mut players = Vec::new();
    for i in 0..n {
        let p = app.player(&format!("p{i}")).await;
        assert_eq!(signup(&p, event, "b1").await.status(), StatusCode::CREATED);
        players.push(p);
    }
    players
}

#[tokio::test]
async fn create_requires_to() {
    let app = TestApp::spawn(|_| {}).await;
    let p = app.player("Mango").await;
    let res = p
        .post_json("/v1/admin/events", &create_req(Format::DoubleElim))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = p.post_json("/v1/admin/invites", &json!({})).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn create_defaults_ruleset_and_lists() {
    let app = TestApp::spawn(|c| c.ruleset.stocks = 3).await;
    let res = app
        .as_to()
        .post_json("/v1/admin/events", &create_req(Format::SingleElim))
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let summary: EventSummary = res.json().await.unwrap();
    assert_eq!(summary.name, "Weekly");
    assert_eq!(summary.state, "signup");
    assert_eq!(summary.entrants, 0);

    let list: Vec<EventSummary> = reqwest::get(format!("{}/v1/events", app.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list, vec![summary.clone()]);

    let view: EventView = reqwest::get(format!("{}/v1/events/{}", app.url, summary.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view.summary, summary);
    assert_eq!(view.ruleset.stocks, 3);
    assert!(view.bracket.is_none());
    assert!(view.tags.is_empty());

    let missing = reqwest::get(format!("{}/v1/events/999", app.url))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invite_code_is_12_base32() {
    let app = TestApp::spawn(|_| {}).await;
    let res = app.as_to().post_json("/v1/admin/invites", &json!({})).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let body: Value = res.json().await.unwrap();
    let code = body["code"].as_str().unwrap();
    assert_eq!(code.len(), 12);
    assert!(code
        .bytes()
        .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)));
    let stored: Option<String> = sqlx::query_scalar("SELECT used_by FROM invites WHERE code = ?")
        .bind(code)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(stored, None);
}

#[tokio::test]
async fn signup_and_start_4() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::DoubleElim).await;
    sign_up_n(&app, event, 4).await;

    let res = start(&app, event).await;
    assert_eq!(res.status(), StatusCode::OK);
    let view: EventView = res.json().await.unwrap();
    assert_eq!(view.summary.state, "running");
    assert_eq!(view.summary.entrants, 4);
    assert_eq!(view.tags, vec!["p0", "p1", "p2", "p3"]);
    let bracket = view.bracket.unwrap();
    assert_eq!(bracket.sets.len(), 7);
    let ready: Vec<u32> = bracket.ready_sets();
    assert_eq!(ready.len(), 2);
    assert!(bracket
        .sets
        .iter()
        .filter(|s| s.state == SetState::Ready)
        .all(|s| ready.contains(&s.id)));
    assert_eq!(
        view.scores,
        ready.iter().map(|&id| (id, [0, 0])).collect::<Vec<_>>()
    );

    let rows = sets_rows(&app, event).await;
    assert_eq!(
        rows,
        ready
            .iter()
            .map(|&id| (i64::from(id), "ready".to_string()))
            .collect::<Vec<_>>()
    );

    // The public view matches what start returned.
    let public: EventView = reqwest::get(format!("{}/v1/events/{event}", app.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(public.bracket, Some(bracket));
}

#[tokio::test]
async fn start_with_one_entrant_is_409() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::DoubleElim).await;
    sign_up_n(&app, event, 1).await;

    let res = start(&app, event).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body, json!({"error": "need at least 2 entrants"}));
    assert!(sets_rows(&app, event).await.is_empty());
    let state: String = sqlx::query_scalar("SELECT state FROM events WHERE id = ?")
        .bind(event)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(state, "signup");
}

#[tokio::test]
async fn start_twice_is_409() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::SingleElim).await;
    sign_up_n(&app, event, 2).await;
    assert_eq!(start(&app, event).await.status(), StatusCode::OK);
    assert_eq!(start(&app, event).await.status(), StatusCode::CONFLICT);
    assert_eq!(sets_rows(&app, event).await.len(), 1);
}

#[tokio::test]
async fn start_by_player_is_403_and_unknown_event_is_404() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::SingleElim).await;
    let players = sign_up_n(&app, event, 2).await;
    let res = players[0]
        .post_json(&format!("/v1/admin/events/{event}/start"), &json!({}))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(start(&app, 999).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        signup(&players[0], 999, "b1").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn duplicate_signup_is_409() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::DoubleElim).await;
    let p = app.player("Mango").await;
    assert_eq!(signup(&p, event, "b1").await.status(), StatusCode::CREATED);
    assert_eq!(signup(&p, event, "b1").await.status(), StatusCode::CONFLICT);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entrants WHERE event_id = ?")
        .bind(event)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn signup_after_start_is_409() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::DoubleElim).await;
    sign_up_n(&app, event, 2).await;
    assert_eq!(start(&app, event).await.status(), StatusCode::OK);
    let late = app.player("Late").await;
    assert_eq!(
        signup(&late, event, "b1").await.status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn unaccepted_build_is_403() {
    let app = TestApp::spawn(|c| c.accepted_builds = vec!["b1".into()]).await;
    let event = create_event(&app, Format::DoubleElim).await;
    let p = app.player("Mango").await;

    let res = signup(&p, event, "b2").await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body: Value = res.json().await.unwrap();
    assert_eq!(
        body,
        json!({"error": "engine build not accepted", "accepted": ["b1"]})
    );

    assert_eq!(signup(&p, event, "b1").await.status(), StatusCode::CREATED);
    let build: String = sqlx::query_scalar("SELECT engine_build FROM entrants WHERE event_id = ?")
        .bind(event)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(build, "b1");
}

#[tokio::test]
async fn a_version_only_accepted_build_matches_at_signup() {
    let version = "melee (rust) 0.1.0 development build";
    let app = TestApp::spawn(|c| c.accepted_builds = vec![version.into()]).await;
    let event = create_event(&app, Format::DoubleElim).await;

    let p = app.player("Mango").await;
    let build = format!("{version}+0123456789ab");
    assert_eq!(
        signup(&p, event, &build).await.status(),
        StatusCode::CREATED
    );

    let other = app.player("Armada").await;
    let res = signup(&other, event, "melee (rust) 0.2.0+0123456789ab").await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body: Value = res.json().await.unwrap();
    assert_eq!(
        body,
        json!({"error": "engine build not accepted", "accepted": [version]})
    );
}

#[tokio::test]
async fn three_entrants_start() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::DoubleElim).await;
    sign_up_n(&app, event, 3).await;
    let res = start(&app, event).await;
    assert_eq!(res.status(), StatusCode::OK);
    let view: EventView = res.json().await.unwrap();
    assert_eq!(view.bracket.unwrap().ready_sets().len(), 1);
    assert_eq!(sets_rows(&app, event).await.len(), 1);
}

#[tokio::test]
async fn full_event_refuses_signup() {
    let app = TestApp::spawn(|_| {}).await;
    let event = create_event(&app, Format::DoubleElim).await;
    // Fill the event directly; signing up 1024 players over HTTP would be slow.
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?)
         INSERT INTO entrants (event_id, player_key, seed, engine_build)
         SELECT ?, 'filler' || i, i, 'b1' FROM n",
    )
    .bind(MAX_ENTRANTS)
    .bind(event)
    .execute(&app.state.db)
    .await
    .unwrap();

    let p = app.player("Mango").await;
    let res = signup(&p, event, "b1").await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body, json!({"error": "event is full"}));
}

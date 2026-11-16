//! The results API: `GET /v1/history`, `GET /v1/players`, `GET /v1/players/{tag}`.

use autobracket_server::db::now_unix;
use autobracket_server::lifecycle::award_set;
use autobracket_server::testkit::{play_game, TestApp, TestPlayer};
use bracket::{Format, SetState, Slot};
use proto::{EventResult, EventView, PlayerProfile, PlayerSummary};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

/// An unsigned GET of a public route, decoded; asserts 200.
async fn get_json<T: DeserializeOwned>(app: &TestApp, path: &str) -> T {
    let res = reqwest::get(format!("{}{path}", app.url)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "GET {path}");
    res.json().await.unwrap()
}

async fn event_view(app: &TestApp, event: i64) -> EventView {
    get_json(app, &format!("/v1/events/{event}")).await
}

/// The ids of the sets that are ready to be played.
fn ready_sets(view: &EventView) -> Vec<u32> {
    view.bracket
        .as_ref()
        .map(|b| b.ready_sets())
        .unwrap_or_default()
}

/// Plays `set` out with the entrants in its slots: `winner_slot` wins games 1 and 2.
async fn win_set(app: &TestApp, event: i64, set: u32, players: &[TestPlayer], winner_slot: u8) {
    let view = event_view(app, event).await;
    let bracket = view.bracket.as_ref().expect("started event");
    let bracket_set = &bracket.sets[set as usize];
    assert_eq!(bracket_set.state, SetState::Ready, "set {set} is ready");
    let in_slot = |i: usize| -> &TestPlayer {
        let Slot::Entrant(id) = bracket_set.slots[i] else {
            panic!("set {set} slot {i} holds no entrant");
        };
        let tag = &view.tags[id as usize];
        players
            .iter()
            .find(|p| &p.tag == tag)
            .unwrap_or_else(|| panic!("no test player {tag}"))
    };
    let (host, guest) = (in_slot(0), in_slot(1));
    for n in 1..=2 {
        play_game(app, event, set, n, host, guest, winner_slot + 1).await;
    }
}

/// Registers Mango, Armada, Zain and Cody (seeds 0..4), starts a single-elimination event
/// and plays every set out with slot 0 winning. Returns the event and the players.
async fn played_out_se4(app: &TestApp) -> (i64, Vec<TestPlayer>) {
    let mut players = Vec::new();
    for tag in ["Mango", "Armada", "Zain", "Cody"] {
        players.push(app.player(tag).await);
    }
    let refs: Vec<&TestPlayer> = players.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    loop {
        let view = event_view(app, event).await;
        if view.summary.state == "complete" {
            break;
        }
        let ready = ready_sets(&view);
        assert!(!ready.is_empty(), "a running event has a ready set");
        for set in ready {
            win_set(app, event, set, &players, 0).await;
        }
    }
    (event, players)
}

#[tokio::test]
async fn empty_server_has_empty_history_and_players() {
    let app = TestApp::spawn(|_| {}).await;
    let history: Vec<EventResult> = get_json(&app, "/v1/history").await;
    assert!(history.is_empty());
    let players: Vec<PlayerSummary> = get_json(&app, "/v1/players").await;
    assert!(players.is_empty());
}

#[tokio::test]
async fn history_lists_complete_events_with_placings() {
    let app = TestApp::spawn(|_| {}).await;
    let (a, b) = (app.player("Alice").await, app.player("Bob").await);
    let running = app.started_event(Format::SingleElim, &[&a, &b]).await;
    let (event, _) = played_out_se4(&app).await;
    let h: Vec<EventResult> = get_json(&app, "/v1/history").await;
    assert_eq!(h.len(), 1, "a running event is not history");
    assert_ne!(h[0].summary.id, running);
    assert_eq!(h[0].summary.id, event);
    assert_eq!(h[0].summary.state, "complete");
    assert_eq!(h[0].summary.entrants, 4);
    let places: Vec<u32> = h[0].placings.iter().map(|p| p.place).collect();
    assert_eq!(places, vec![1, 2, 3, 3]);
    assert_eq!(h[0].placings[0].tag, "Mango"); // seed 1 won every set as slot 0
    assert_eq!(h[0].placings[1].tag, "Armada");
    // Ties are ordered by seed.
    assert_eq!(h[0].placings[2].tag, "Zain");
    assert_eq!(h[0].placings[3].tag, "Cody");
}

#[tokio::test]
async fn history_is_newest_first() {
    let app = TestApp::spawn(|_| {}).await;
    let (first, players) = played_out_se4(&app).await;
    let refs: Vec<&TestPlayer> = players.iter().take(2).collect();
    let second = app.started_event(Format::SingleElim, &refs).await;
    win_set(&app, second, 0, &players, 1).await;
    let h: Vec<EventResult> = get_json(&app, "/v1/history").await;
    let ids: Vec<i64> = h.iter().map(|r| r.summary.id).collect();
    assert_eq!(ids, vec![second, first]);
    assert_eq!(h[0].placings[0].tag, "Armada");
}

/// `?limit=N` keeps the newest N complete events; the default is every one; 1..=100 only.
#[tokio::test]
async fn history_limit_keeps_the_newest() {
    let app = TestApp::spawn(|_| {}).await;
    let (first, players) = played_out_se4(&app).await;
    let refs: Vec<&TestPlayer> = players.iter().take(2).collect();
    let second = app.started_event(Format::SingleElim, &refs).await;
    win_set(&app, second, 0, &players, 1).await;
    // A newer running event does not use up the limit.
    app.started_event(Format::SingleElim, &refs).await;

    let h: Vec<EventResult> = get_json(&app, "/v1/history?limit=1").await;
    let ids: Vec<i64> = h.iter().map(|r| r.summary.id).collect();
    assert_eq!(ids, vec![second]);
    assert_eq!(h[0].placings.len(), 2, "placings come with the event");
    for path in [
        "/v1/history",
        "/v1/history?limit=2",
        "/v1/history?limit=100",
    ] {
        let h: Vec<EventResult> = get_json(&app, path).await;
        let ids: Vec<i64> = h.iter().map(|r| r.summary.id).collect();
        assert_eq!(ids, vec![second, first], "{path}");
    }
    for path in [
        "/v1/history?limit=0",
        "/v1/history?limit=101",
        "/v1/history?limit=-1",
        "/v1/history?limit=x",
    ] {
        let res = reqwest::get(format!("{}{path}", app.url)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{path}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert!(body["error"].is_string(), "{path}: {body}");
    }
}

#[tokio::test]
async fn players_index_counts_events_titles_and_sets() {
    let app = TestApp::spawn(|_| {}).await;
    played_out_se4(&app).await;
    app.player("Idle").await;
    let players: Vec<PlayerSummary> = get_json(&app, "/v1/players").await;
    let tags: Vec<&str> = players.iter().map(|p| p.tag.as_str()).collect();
    assert_eq!(tags, vec!["Armada", "Cody", "Idle", "Mango", "Zain"]);
    let by_tag = |tag: &str| players.iter().find(|p| p.tag == tag).unwrap();
    let mango = by_tag("Mango");
    assert_eq!((mango.events, mango.titles, mango.sets), (1, 1, [2, 0]));
    let armada = by_tag("Armada");
    assert_eq!((armada.events, armada.titles, armada.sets), (1, 0, [1, 1]));
    for third in ["Zain", "Cody"] {
        let p = by_tag(third);
        assert_eq!((p.events, p.titles, p.sets), (1, 0, [0, 1]), "{third}");
    }
    let idle = by_tag("Idle");
    assert_eq!((idle.events, idle.titles, idle.sets), (0, 0, [0, 0]));
}

#[tokio::test]
async fn player_profile_has_events_and_head_to_head() {
    let app = TestApp::spawn(|_| {}).await;
    let (event, _) = played_out_se4(&app).await;
    let profile: PlayerProfile = get_json(&app, "/v1/players/mango").await;
    assert_eq!(profile.tag, "Mango", "the canonical spelling");
    assert!(
        (profile.since - now_unix()).abs() < 60,
        "since {} is now",
        profile.since
    );
    assert_eq!(profile.events.len(), 1);
    let e = &profile.events[0];
    assert_eq!(e.event.id, event);
    assert_eq!(e.event.state, "complete");
    assert_eq!(e.seed, 0);
    assert_eq!(e.place, Some(1));
    assert_eq!(e.sets, [2, 0]);
    assert_eq!(e.games, [4, 0]);
    let h2h: Vec<(&str, [u32; 2], [u32; 2])> = profile
        .head_to_head
        .iter()
        .map(|h| (h.tag.as_str(), h.sets, h.games))
        .collect();
    assert_eq!(
        h2h,
        vec![("Armada", [1, 0], [2, 0]), ("Cody", [1, 0], [2, 0])]
    );

    // The loser's side of the same sets.
    let cody: PlayerProfile = get_json(&app, "/v1/players/CODY").await;
    assert_eq!(cody.events[0].place, Some(3));
    assert_eq!(cody.events[0].seed, 3);
    assert_eq!(cody.events[0].sets, [0, 1]);
    assert_eq!(cody.events[0].games, [0, 2]);
    assert_eq!(cody.head_to_head.len(), 1);
    assert_eq!(cody.head_to_head[0].tag, "Mango");
    assert_eq!(cody.head_to_head[0].sets, [0, 1]);
    assert_eq!(cody.head_to_head[0].games, [0, 2]);

    let res = reqwest::get(format!("{}/v1/players/nobody", app.url))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"error": "no such player"}));
}

#[tokio::test]
async fn running_event_appears_in_the_profile_without_a_place() {
    let app = TestApp::spawn(|_| {}).await;
    let (a, b) = (app.player("Alice").await, app.player("Bob").await);
    let event = app.started_event(Format::SingleElim, &[&a, &b]).await;
    let profile: PlayerProfile = get_json(&app, "/v1/players/alice").await;
    assert_eq!(profile.events.len(), 1);
    assert_eq!(profile.events[0].event.id, event);
    assert_eq!(profile.events[0].place, None);
    assert_eq!(profile.events[0].sets, [0, 0]);
    assert!(profile.head_to_head.is_empty());
    let players: Vec<PlayerSummary> = get_json(&app, "/v1/players").await;
    assert_eq!(players[0].tag, "Alice");
    assert_eq!((players[0].events, players[0].titles), (1, 0));
}

#[tokio::test]
async fn awarded_sets_count_as_sets_without_games() {
    let app = TestApp::spawn(|_| {}).await;
    let mut players = Vec::new();
    for tag in ["Mango", "Armada", "Zain", "Cody"] {
        players.push(app.player(tag).await);
    }
    let refs: Vec<&TestPlayer> = players.iter().collect();
    let event = app.started_event(Format::SingleElim, &refs).await;
    loop {
        let view = event_view(&app, event).await;
        if view.summary.state == "complete" {
            break;
        }
        for set in ready_sets(&view) {
            award_set(&app.state, event, set, 0).await.unwrap();
        }
    }
    let h: Vec<EventResult> = get_json(&app, "/v1/history").await;
    assert_eq!(h.len(), 1);
    let places: Vec<u32> = h[0].placings.iter().map(|p| p.place).collect();
    assert_eq!(places, vec![1, 2, 3, 3]);
    let champion = &h[0].placings[0].tag;
    let profile: PlayerProfile = get_json(&app, &format!("/v1/players/{champion}")).await;
    assert_eq!(profile.events[0].sets, [2, 0]);
    assert_eq!(profile.events[0].games, [0, 0]);
    let players: Vec<PlayerSummary> = get_json(&app, "/v1/players").await;
    let p = players.iter().find(|p| &p.tag == champion).unwrap();
    assert_eq!((p.titles, p.sets), (1, [2, 0]));
}

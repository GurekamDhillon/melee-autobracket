//! Reporting a no-show: the play loop against a stub server, with an engine whose runs
//! end as scripted.

mod common;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use autobracket_client::api::Api;
use autobracket_client::config::ClientConfig;
use autobracket_client::engine::{CommandEngine, Engine, GameParams, RunExit};
use autobracket_client::play::{play_event_with, PlayOptions, MAX_FAILED_RUNS};
use common::{host_play, json, Stub};
use proto::{Next, SigningKey};
use tempfile::TempDir;

const POLL: Duration = Duration::from_millis(10);

/// How one scripted run ends.
#[derive(Clone, Copy)]
struct Ending {
    replay: bool,
    exit: Option<RunExit>,
}

const NO_OPPONENT: Ending = Ending {
    replay: false,
    exit: Some(RunExit {
        code: Some(5),
        no_opponent: true,
        host_gone: false,
    }),
};
const NORMAL: Ending = Ending {
    replay: true,
    exit: Some(RunExit {
        code: Some(0),
        no_opponent: false,
        host_gone: false,
    }),
};
const FAILED: Ending = Ending {
    replay: false,
    exit: Some(RunExit {
        code: Some(1),
        no_opponent: false,
        host_gone: false,
    }),
};

/// Each run prints a room code and exits at once, ending as the next scripted [`Ending`];
/// the last one repeats.
struct Scripted {
    script: VecDeque<Ending>,
    last: Ending,
    hosts: Arc<AtomicUsize>,
    joins: Arc<Mutex<Vec<String>>>,
    finished: Option<Ending>,
    active: bool,
    dir: TempDir,
}

impl Scripted {
    fn new(script: &[Ending]) -> Self {
        Scripted {
            script: script.iter().copied().collect(),
            last: *script.last().unwrap(),
            hosts: Arc::default(),
            joins: Arc::default(),
            finished: None,
            active: false,
            dir: TempDir::new().unwrap(),
        }
    }

    fn start(&mut self) {
        assert!(!self.active, "started while a run is active");
        self.active = true;
        self.finished = Some(self.script.pop_front().unwrap_or(self.last));
    }
}

impl Engine for Scripted {
    fn host(&mut self, _p: &GameParams) -> anyhow::Result<()> {
        self.hosts.fetch_add(1, Ordering::SeqCst);
        self.start();
        Ok(())
    }

    fn join(&mut self, _p: &GameParams, code: &str) -> anyhow::Result<()> {
        self.joins.lock().unwrap().push(code.to_string());
        self.start();
        Ok(())
    }

    fn room_code(&mut self, _wait: Duration) -> anyhow::Result<Option<String>> {
        Ok(self.active.then(|| "K7QX".to_string()))
    }

    fn running(&mut self) -> bool {
        false
    }

    fn take_replays(&mut self) -> anyhow::Result<Vec<PathBuf>> {
        if !self.active {
            return Ok(Vec::new());
        }
        self.active = false;
        let n = self.hosts.load(Ordering::SeqCst) + self.joins.lock().unwrap().len();
        if !self.finished.is_some_and(|e| e.replay) {
            return Ok(Vec::new());
        }
        let path = self.dir.path().join(format!("run-{n}.slp"));
        std::fs::write(&path, b"replay")?;
        Ok(vec![path])
    }

    fn stop(&mut self) {
        self.active = false;
    }

    fn last_exit(&mut self) -> Option<RunExit> {
        self.finished.and_then(|e| e.exit)
    }
}

/// A server where this player hosts set 0 game 1 until a no-show report is accepted, and
/// then the set is in review. `no_show` gives the status of each no-show report in turn
/// (the last repeats); a 204 puts the set in review.
async fn server(no_show: &[u16]) -> Stub {
    let answers: Vec<u16> = no_show.to_vec();
    let reports = AtomicUsize::new(0);
    let reviewed = AtomicBool::new(false);
    Stub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => {
            let next = if reviewed.load(Ordering::SeqCst) {
                Next::Review { set_id: 0 }
            } else {
                host_play()
            };
            (200, json(&next))
        }
        ("POST", "/v1/sets/1/0/no-show") => {
            let i = reports.fetch_add(1, Ordering::SeqCst);
            let status = answers[i.min(answers.len() - 1)];
            match status {
                204 => {
                    reviewed.store(true, Ordering::SeqCst);
                    (204, String::new())
                }
                404 => (404, String::new()),
                // The server's grace period after a room code.
                1409 => (
                    409,
                    r#"{"error":"a room code was posted less than 90 seconds ago"}"#.into(),
                ),
                // The same, with a 1 s grace period.
                2409 => (
                    409,
                    r#"{"error":"a room code was posted less than 1 seconds ago"}"#.into(),
                ),
                // A grace refusal whose period cannot be read.
                3409 => (
                    409,
                    r#"{"error":"a room code was posted less than a while ago"}"#.into(),
                ),
                s => (s, r#"{"error":"no"}"#.into()),
            }
        }
        ("POST", "/v1/sets/1/0/room") => (204, String::new()),
        ("POST", "/v1/sets/1/0/games/1/replay") => (202, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await
}

fn api(stub: &Stub) -> Api {
    Api::new(&stub.url, SigningKey::from_bytes(&[7; 32]), "b1")
}

fn opts() -> PlayOptions {
    PlayOptions {
        launch_backoff: Duration::from_millis(5),
        retry_cap: Duration::from_millis(50),
        ..PlayOptions::new(POLL)
    }
}

/// Runs `play` until `done` holds, failing if `play` returns first or it takes too long.
async fn play_until(stub: &Stub, engine: &mut dyn Engine, done: impl Fn(&Stub) -> bool) {
    play_until_with(stub, engine, &opts(), done).await;
}

async fn play_until_with(
    stub: &Stub,
    engine: &mut dyn Engine,
    opts: &PlayOptions,
    done: impl Fn(&Stub) -> bool,
) {
    let api = api(stub);
    let wait = async {
        while !done(stub) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = wait => {}
            r = play_event_with(&api, engine, 1, opts) => panic!("play returned {r:?}"),
        }
    })
    .await
    .expect("timed out");
}

fn no_shows(stub: &Stub) -> Vec<serde_json::Value> {
    stub.requests("POST", "/no-show")
        .iter()
        .map(common::Req::json)
        .collect()
}

#[tokio::test]
async fn two_no_opponent_runs_report_a_no_show_and_keep_polling() {
    let stub = server(&[204]).await;
    let mut engine = Scripted::new(&[NO_OPPONENT, NO_OPPONENT, NORMAL]);
    let hosts = engine.hosts.clone();

    play_until(&stub, &mut engine, |s| s.polls_after("/no-show") > 5).await;

    assert_eq!(
        no_shows(&stub),
        vec![serde_json::json!({"detail": "no opponent joined (2 attempts)", "kind": "no-show"})]
    );
    let reports = stub.requests("POST", "/no-show");
    assert!(
        reports[0].header("x-ab-sig").is_some(),
        "the report is signed"
    );
    assert_eq!(
        hosts.load(Ordering::SeqCst),
        2,
        "no relaunch once in review"
    );
}

#[tokio::test]
async fn a_normal_run_resets_the_no_show_strikes() {
    let stub = server(&[204]).await;
    let mut engine = Scripted::new(&[NO_OPPONENT, NORMAL, NO_OPPONENT, NO_OPPONENT]);
    let hosts = engine.hosts.clone();

    play_until(&stub, &mut engine, |s| s.polls_after("/no-show") > 5).await;

    assert_eq!(no_shows(&stub).len(), 1);
    assert_eq!(
        hosts.load(Ordering::SeqCst),
        4,
        "reported only after runs 3 and 4"
    );
    assert_eq!(stub.requests("POST", "/replay").len(), 1);
}

#[tokio::test]
async fn giving_up_reports_the_stuck_set_first() {
    let stub = server(&[204]).await;
    let mut engine = Scripted::new(&[FAILED]);

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        play_event_with(&api(&stub), &mut engine, 1, &opts()),
    )
    .await
    .expect("play never gave up")
    .expect_err("play must fail");

    assert!(format!("{err:#}").contains("without a replay"), "{err:#}");
    assert_eq!(
        engine.hosts.load(Ordering::SeqCst),
        MAX_FAILED_RUNS as usize
    );
    let reports = no_shows(&stub);
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        reports[0]["detail"],
        format!(
            "the reporting player's client could not complete game 1: {MAX_FAILED_RUNS} runs \
             without a replay, last exit code 1"
        )
    );
    assert_eq!(reports[0]["kind"], "client-failure");
}

async fn refused_report_is_not_fatal(status: u16) {
    let stub = server(&[status]).await;
    let mut engine = Scripted::new(&[NO_OPPONENT]);
    let hosts = engine.hosts.clone();

    // Still hosting after the refused report: the loop went on.
    play_until(&stub, &mut engine, |s| {
        s.count("/v1/sets/1/0/no-show") >= 1 && hosts.load(Ordering::SeqCst) >= 3
    })
    .await;
}

#[tokio::test]
async fn a_missing_no_show_route_is_not_fatal() {
    refused_report_is_not_fatal(404).await;
}

#[tokio::test]
async fn a_conflicting_no_show_is_not_fatal() {
    refused_report_is_not_fatal(409).await;
}

#[tokio::test]
async fn a_forbidden_no_show_is_not_fatal() {
    refused_report_is_not_fatal(403).await;
}

#[tokio::test]
async fn a_bad_request_no_show_is_not_fatal() {
    refused_report_is_not_fatal(400).await;
}

#[tokio::test]
async fn a_retryable_no_show_failure_is_retried() {
    let stub = server(&[503, 204]).await;
    let mut engine = Scripted::new(&[NO_OPPONENT]);
    let hosts = engine.hosts.clone();

    play_until(&stub, &mut engine, |s| {
        s.count("/v1/sets/1/0/no-show") == 2 && s.polls_after("/no-show") > 5
    })
    .await;

    assert_eq!(no_shows(&stub).len(), 2);
    assert_eq!(hosts.load(Ordering::SeqCst), 2);
}

/// Plays with the real command engine running `fake_melee.sh` in `mode` for every host run;
/// `play`'s error, and the work directory.
async fn play_command_engine(stub: &Stub, mode: &str) -> (anyhow::Error, TempDir) {
    let work = TempDir::new().unwrap();
    let mut cfg = ClientConfig::with_defaults(
        "07".repeat(32),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh"),
        PathBuf::from("/isos/my melee.iso"),
        work.path().to_path_buf(),
    );
    cfg.host_args = vec![mode.into(), "--np-join-wait".into(), "120".into()];
    let mut engine = CommandEngine::new(&cfg).unwrap();
    engine.set_output(|_| {});
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        play_event_with(&api(stub), &mut engine, 1, &opts()),
    )
    .await
    .expect("play did not stop")
    .expect_err("play must fail");
    (err, work)
}

fn runs(work: &TempDir) -> usize {
    std::fs::read_dir(work.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("run-")
        })
        .count()
}

#[tokio::test]
async fn a_failed_disc_check_ends_play_at_once_without_a_report() {
    let stub = server(&[204]).await;

    let (err, work) = play_command_engine(&stub, "disc-fail").await;

    let msg = format!("{err:#}");
    assert!(msg.contains("disc check failed"), "{msg}");
    assert!(msg.contains("/isos/my melee.iso"), "{msg}");
    assert_eq!(runs(&work), 1, "exactly one host run");
    assert!(no_shows(&stub).is_empty(), "reported a no-show");
}

#[tokio::test]
async fn a_usage_error_ends_play_at_once_without_a_report() {
    let stub = server(&[204]).await;

    let (err, work) = play_command_engine(&stub, "usage").await;

    let msg = format!("{err:#}");
    assert!(msg.contains("exit code 2"), "{msg}");
    assert!(msg.contains("--np-join-wait"), "the template: {msg}");
    assert!(
        msg.contains("error: unexpected argument '--np-join-wait' found"),
        "the log tail: {msg}"
    );
    assert_eq!(runs(&work), 1, "exactly one host run");
    assert!(no_shows(&stub).is_empty(), "reported a no-show");
}

const HOST_GONE: Ending = Ending {
    replay: false,
    exit: Some(RunExit {
        code: Some(1),
        no_opponent: false,
        host_gone: true,
    }),
};

#[tokio::test]
async fn the_installed_engines_no_connection_line_reports_a_no_show() {
    let stub = server(&[204]).await;
    let work = TempDir::new().unwrap();
    let mut cfg = ClientConfig::with_defaults(
        "07".repeat(32),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh"),
        PathBuf::from("/isos/melee.iso"),
        work.path().to_path_buf(),
    );
    cfg.host_args = vec!["np-timeout".into()];
    let mut engine = CommandEngine::new(&cfg).unwrap();
    engine.set_output(|_| {});

    play_until(&stub, &mut engine, |s| s.polls_after("/no-show") > 5).await;

    assert_eq!(
        no_shows(&stub),
        vec![serde_json::json!({"detail": "no opponent joined (2 attempts)", "kind": "no-show"})]
    );
    assert_eq!(runs(&work), 2);
}

#[tokio::test]
async fn no_connection_after_a_guest_arrived_counts_as_a_failed_run_not_a_no_show() {
    let stub = server(&[204]).await;

    let (err, work) = play_command_engine(&stub, "np-timeout-introduced").await;

    assert!(format!("{err:#}").contains("without a replay"), "{err:#}");
    assert_eq!(runs(&work), MAX_FAILED_RUNS as usize);
    let reports = no_shows(&stub);
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0]["kind"], "client-failure");
}

#[tokio::test]
async fn a_no_show_refused_for_a_fresh_room_code_is_tried_again_without_hosting() {
    let stub = server(&[1409, 1409, 204]).await;
    let mut engine = Scripted::new(&[NO_OPPONENT]);
    let hosts = engine.hosts.clone();
    let opts = PlayOptions {
        no_show_retry: Duration::from_millis(30),
        ..opts()
    };

    play_until_with(&stub, &mut engine, &opts, |s| {
        s.requests("POST", "/no-show").len() == 3 && s.polls_after("/no-show") > 5
    })
    .await;

    assert_eq!(no_shows(&stub).len(), 3);
    assert_eq!(
        hosts.load(Ordering::SeqCst),
        2,
        "hosted again while waiting"
    );
}

#[tokio::test]
async fn a_no_show_is_retried_until_the_grace_period_named_has_passed() {
    // More refusals than the fixed retry budget, all within the 1 s grace period named.
    let mut answers = vec![2409; 12];
    answers.push(204);
    let stub = server(&answers).await;
    let mut engine = Scripted::new(&[NO_OPPONENT]);
    let hosts = engine.hosts.clone();
    let opts = PlayOptions {
        no_show_retry: Duration::from_millis(30),
        ..opts()
    };

    play_until_with(&stub, &mut engine, &opts, |s| {
        s.requests("POST", "/no-show").len() == 13 && s.polls_after("/no-show") > 5
    })
    .await;

    assert_eq!(
        hosts.load(Ordering::SeqCst),
        2,
        "hosted again while waiting"
    );
}

#[tokio::test]
async fn a_grace_period_that_cannot_be_read_falls_back_to_the_fixed_retries() {
    let stub = server(&[3409]).await;
    let mut engine = Scripted::new(&[NO_OPPONENT]);
    let hosts = engine.hosts.clone();
    let opts = PlayOptions {
        no_show_retry: Duration::from_millis(5),
        ..opts()
    };

    // After the first refusal and 8 more, it hosts again.
    play_until_with(&stub, &mut engine, &opts, |_| {
        hosts.load(Ordering::SeqCst) >= 3
    })
    .await;

    // (A tenth may follow the next host run before the test looks.)
    let reports = stub.requests("POST", "/no-show").len();
    assert!((9..=10).contains(&reports), "{reports} reports");
}

/// This player is the guest of set 0 game 1; `code` gives the room code per poll.
async fn guest_server(code: impl Fn() -> Option<String> + Send + Sync + 'static) -> Stub {
    Stub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => {
            let mut next = host_play();
            if let Next::Play {
                role, room_code, ..
            } = &mut next
            {
                *role = proto::Role::Guest;
                *room_code = code();
            }
            (200, json(&next))
        }
        ("POST", "/v1/sets/1/0/no-show") => (204, String::new()),
        ("POST", "/v1/sets/1/0/games/1/replay") => (202, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await
}

#[tokio::test]
async fn a_guest_with_no_room_code_reports_a_no_show() {
    let stub = guest_server(|| None).await;
    let mut engine = Scripted::new(&[NORMAL]);
    let opts = PlayOptions {
        no_code_wait: Some(Duration::from_millis(100)),
        ..opts()
    };

    play_until_with(&stub, &mut engine, &opts, |s| {
        s.count("/v1/sets/1/0/no-show") >= 1
    })
    .await;

    let reports = no_shows(&stub);
    assert_eq!(reports[0]["kind"], "no-show");
    let detail = reports[0]["detail"].as_str().unwrap();
    assert!(detail.contains("no room code"), "{detail}");
    assert!(engine.joins.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_guest_never_rejoins_a_room_that_is_gone_and_does_not_count_it() {
    let code = Arc::new(Mutex::new("AB12".to_string()));
    let shared = code.clone();
    let stub = guest_server(move || Some(shared.lock().unwrap().clone())).await;
    let mut engine = Scripted::new(&[
        HOST_GONE, HOST_GONE, HOST_GONE, HOST_GONE, HOST_GONE, NORMAL,
    ]);
    let joins = engine.joins.clone();
    let api = api(&stub);
    let opts = opts();

    tokio::time::timeout(Duration::from_secs(10), async {
        let play = play_event_with(&api, &mut engine, 1, &opts);
        tokio::pin!(play);
        // More rooms gone than MAX_FAILED_RUNS: none of them is this client's failure.
        for n in 0..5 {
            let wait = async {
                while joins.lock().unwrap().len() < n + 1 {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                // The same code again is not joined.
                let polls = stub.count("/v1/me/next");
                while stub.count("/v1/me/next") < polls + 10 {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            };
            tokio::select! {
                () = wait => {}
                r = &mut play => panic!("play returned {r:?}"),
            }
            assert_eq!(joins.lock().unwrap().len(), n + 1);
            *code.lock().unwrap() = format!("AB{}", 20 + n);
        }
        let replayed = async {
            while stub.requests("POST", "/replay").is_empty() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        };
        tokio::select! {
            () = replayed => {}
            r = &mut play => panic!("play returned {r:?}"),
        }
    })
    .await
    .expect("timed out");

    assert_eq!(
        *joins.lock().unwrap(),
        ["AB12", "AB20", "AB21", "AB22", "AB23", "AB24"]
    );
    assert!(no_shows(&stub).is_empty());
}

async fn joins_with(code: &str, allow_direct_connect: bool) -> Vec<String> {
    let code = code.to_string();
    let stub = guest_server(move || Some(code.clone())).await;
    let mut engine = Scripted::new(&[NORMAL]);
    let opts = PlayOptions {
        allow_direct_connect,
        no_code_wait: None,
        ..opts()
    };
    play_until_with(&stub, &mut engine, &opts, |s| s.count("/v1/me/next") > 20).await;
    // A run that produced a replay is followed by another (this stub never says the
    // verdict is awaited), so only the codes joined matter.
    let mut joins = engine.joins.lock().unwrap().clone();
    joins.dedup();
    joins
}

#[tokio::test]
async fn only_valid_room_codes_reach_the_game() {
    assert_eq!(joins_with("K7QX", false).await, ["K7QX"]);
    for bad in ["--np-css", "k7qx", "K7QX5", "K7 X", "1.2.3.4:5000"] {
        assert!(joins_with(bad, false).await.is_empty(), "{bad}");
    }
    assert_eq!(joins_with("1.2.3.4:5000", true).await, ["1.2.3.4:5000"]);
    assert!(joins_with("-x:5000", true).await.is_empty());
}

#[tokio::test]
async fn an_invalid_relay_ends_play_with_a_message() {
    let stub = Stub::start(|req| match req.path.as_str() {
        p if p.starts_with("/v1/me/next") => {
            let mut next = host_play();
            if let Next::Play { relay, .. } = &mut next {
                *relay = "--np-css".into();
            }
            (200, json(&next))
        }
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await;
    let mut engine = Scripted::new(&[NORMAL]);

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        play_event_with(&api(&stub), &mut engine, 1, &opts()),
    )
    .await
    .unwrap()
    .unwrap_err();

    assert!(format!("{err:#}").contains("relay"), "{err:#}");
    assert_eq!(engine.hosts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_failure_count_starts_again_after_a_review() {
    let hosts = Arc::new(AtomicUsize::new(0));
    let seen = hosts.clone();
    let review_polls = AtomicUsize::new(0);
    let stub = Stub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => {
            // After three failed runs the organiser reviews the set and replays it.
            if seen.load(Ordering::SeqCst) == MAX_FAILED_RUNS as usize - 1
                && review_polls.fetch_add(1, Ordering::SeqCst) < 5
            {
                return (200, json(&Next::Review { set_id: 0 }));
            }
            (200, json(&host_play()))
        }
        ("POST", "/v1/sets/1/0/room") => (204, String::new()),
        ("POST", "/v1/sets/1/0/no-show") => (204, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await;
    let mut engine = Scripted::new(&[FAILED]);
    engine.hosts = hosts.clone();

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        play_event_with(&api(&stub), &mut engine, 1, &opts()),
    )
    .await
    .unwrap()
    .unwrap_err();

    assert!(format!("{err:#}").contains("without a replay"), "{err:#}");
    assert_eq!(
        hosts.load(Ordering::SeqCst),
        MAX_FAILED_RUNS as usize - 1 + MAX_FAILED_RUNS as usize
    );
}

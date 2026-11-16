//! Reporting a no-show: the play loop against a stub server, with an engine whose runs
//! end as scripted.

mod common;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
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
    }),
};
const NORMAL: Ending = Ending {
    replay: true,
    exit: Some(RunExit {
        code: Some(0),
        no_opponent: false,
    }),
};
const FAILED: Ending = Ending {
    replay: false,
    exit: Some(RunExit {
        code: Some(1),
        no_opponent: false,
    }),
};

/// Each run prints a room code and exits at once, ending as the next scripted [`Ending`];
/// the last one repeats.
struct Scripted {
    script: VecDeque<Ending>,
    last: Ending,
    hosts: Arc<AtomicUsize>,
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

    fn join(&mut self, _p: &GameParams, _code: &str) -> anyhow::Result<()> {
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
        let n = self.hosts.load(Ordering::SeqCst);
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
async fn play_until(stub: &Stub, engine: &mut Scripted, done: impl Fn(&Stub) -> bool) {
    let api = api(stub);
    let opts = opts();
    let wait = async {
        while !done(stub) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = wait => {}
            r = play_event_with(&api, engine, 1, &opts) => panic!("play returned {r:?}"),
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
        vec![serde_json::json!({"detail": "no opponent joined (2 attempts)"})]
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
            "client could not complete a game: {MAX_FAILED_RUNS} runs without a replay, \
             last exit code 1"
        )
    );
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
        "fake".into(),
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

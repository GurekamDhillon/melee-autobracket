//! The client against a real server: the `play_event` loop with scripted engines, the
//! command engine against `fixtures/fake_melee.sh`, and `init`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use autobracket_client::api::Api;
use autobracket_client::config::{self, ClientConfig};
use autobracket_client::engine::{CommandEngine, Engine, GameParams};
use autobracket_client::play::play_event;
use autobracket_server::testkit::{TestApp, TestPlayer};
use autobracket_server::worker::judge_due;
use bracket::Format;
use proto::{Next, Role};
use replay::testkit::simple_game;
use tempfile::TempDir;

const POLL: Duration = Duration::from_millis(10);
const START_A: &str = "2026-11-15T20:00:00Z";
const START_B: &str = "2026-11-15T20:00:01Z";

/// What a [`ScriptedEngine`] was asked to do.
#[derive(Default, Debug)]
struct Calls {
    /// `(relay, stocks, timer_secs)` per `host()`.
    hosts: Vec<(String, u8, u32)>,
    /// The code per `join()`.
    joins: Vec<String>,
}

/// Records `host()`/`join()` calls and yields a preset room code. A run "exits" at once
/// (a host's run lasts until its `peer` has joined as many times as it has hosted) and
/// yields the preset replays, written to files.
struct ScriptedEngine {
    calls: Arc<Mutex<Calls>>,
    code: String,
    replays: Vec<Vec<u8>>,
    peer: Option<Arc<Mutex<Calls>>>,
    dir: TempDir,
    runs: usize,
    active: bool,
}

impl ScriptedEngine {
    fn new(code: &str, replays: Vec<Vec<u8>>) -> Self {
        ScriptedEngine {
            calls: Arc::default(),
            code: code.into(),
            replays,
            peer: None,
            dir: TempDir::new().unwrap(),
            runs: 0,
            active: false,
        }
    }

    fn hosts(&self) -> usize {
        self.calls.lock().unwrap().hosts.len()
    }

    fn joins(&self) -> Vec<String> {
        self.calls.lock().unwrap().joins.clone()
    }
}

impl Engine for ScriptedEngine {
    fn host(&mut self, p: &GameParams) -> anyhow::Result<()> {
        assert!(!self.active, "host() while a run is active");
        self.calls
            .lock()
            .unwrap()
            .hosts
            .push((p.relay.to_string(), p.stocks, p.timer_secs));
        self.runs += 1;
        self.active = true;
        Ok(())
    }

    fn join(&mut self, _p: &GameParams, code: &str) -> anyhow::Result<()> {
        assert!(!self.active, "join() while a run is active");
        self.calls.lock().unwrap().joins.push(code.to_string());
        self.runs += 1;
        self.active = true;
        Ok(())
    }

    fn room_code(&mut self, _wait: Duration) -> anyhow::Result<Option<String>> {
        Ok(self.active.then(|| self.code.clone()))
    }

    fn running(&mut self) -> bool {
        self.active
            && self
                .peer
                .as_ref()
                .is_some_and(|peer| peer.lock().unwrap().joins.len() < self.runs)
    }

    fn take_replays(&mut self) -> anyhow::Result<Vec<PathBuf>> {
        if !self.active || self.running() {
            return Ok(Vec::new());
        }
        self.active = false;
        let mut paths = Vec::new();
        for (i, bytes) in self.replays.iter().enumerate() {
            let path = self.dir.path().join(format!("run-{}-{i}.slp", self.runs));
            std::fs::write(&path, bytes)?;
            paths.push(path);
        }
        Ok(paths)
    }

    fn stop(&mut self) {
        self.active = false;
    }
}

fn api(app: &TestApp, p: &TestPlayer) -> Api {
    Api::new(&app.url, p.sk.clone(), "b1")
}

/// A started two-player single-elimination event: `(app, host, guest, event)`.
async fn setup() -> (TestApp, TestPlayer, TestPlayer, i64) {
    let app = TestApp::spawn(|_| {}).await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    let next = api(&app, &host).next(event).await.unwrap();
    assert!(
        matches!(
            next,
            Next::Play {
                role: Role::Host,
                ..
            }
        ),
        "{next:?}"
    );
    (app, host, guest, event)
}

/// An agreeing pair for the host and guest engines, won by `winner_port`.
fn engines(winner_port: u8) -> (ScriptedEngine, ScriptedEngine) {
    let guest = ScriptedEngine::new("", vec![simple_game(winner_port, START_B)]);
    let mut host = ScriptedEngine::new("K7QX", vec![simple_game(winner_port, START_A)]);
    host.peer = Some(guest.calls.clone());
    (host, guest)
}

async fn judge_forever(app: &TestApp) {
    loop {
        judge_due(&app.state).await.expect("judging");
        tokio::time::sleep(POLL).await;
    }
}

/// Runs both clients to their terminal states while a judge loop runs.
async fn play_out(
    app: &TestApp,
    event: i64,
    (host, guest): (&TestPlayer, &TestPlayer),
    (he, ge): (&mut ScriptedEngine, &mut ScriptedEngine),
) -> (Next, Next) {
    let (ha, ga) = (api(app, host), api(app, guest));
    let both = async {
        tokio::join!(
            play_event(&ha, he, event, POLL),
            play_event(&ga, ge, event, POLL)
        )
    };
    let (h, g) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            r = both => r,
            () = judge_forever(app) => unreachable!(),
        }
    })
    .await
    .expect("clients did not finish");
    (h.expect("host play_event"), g.expect("guest play_event"))
}

#[tokio::test]
async fn host_posts_code_and_guest_joins_with_it() {
    let (app, host, guest, event) = setup().await;
    let (mut he, mut ge) = engines(1);
    let guest_calls = ge.calls.clone();
    let (ha, ga) = (api(&app, &host), api(&app, &guest));

    let joined = async {
        while guest_calls.lock().unwrap().joins.is_empty() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = joined => {}
            r = play_event(&ha, &mut he, event, POLL) => panic!("host returned {r:?}"),
            r = play_event(&ga, &mut ge, event, POLL) => panic!("guest returned {r:?}"),
        }
    })
    .await
    .expect("guest never joined");

    assert_eq!(ge.joins(), vec!["K7QX".to_string()]);
    let hosts = he.calls.lock().unwrap().hosts.clone();
    assert_eq!(hosts, vec![(app.state.cfg.relay.clone(), 4, 480)]);
}

#[tokio::test]
async fn two_clients_play_a_bo3_to_champion() {
    let (app, host, guest, event) = setup().await;
    let (mut he, mut ge) = engines(1);

    let (h, g) = play_out(&app, event, (&host, &guest), (&mut he, &mut ge)).await;

    assert_eq!(h, Next::Champion);
    assert_eq!(g, Next::Eliminated);
    assert_eq!(he.hosts(), 2, "one host() per game");
    assert_eq!(ge.joins(), vec!["K7QX".to_string(), "K7QX".to_string()]);
}

#[tokio::test]
async fn does_not_relaunch_while_awaiting_verdict() {
    let (app, host, guest, event) = setup().await;
    let (mut he, mut ge) = engines(1);
    let (ha, ga) = (api(&app, &host), api(&app, &guest));

    tokio::select! {
        () = tokio::time::sleep(POLL * 40) => {}
        r = play_event(&ha, &mut he, event, POLL) => panic!("host returned {r:?}"),
        r = play_event(&ga, &mut ge, event, POLL) => panic!("guest returned {r:?}"),
    }

    assert_eq!(he.hosts(), 1);
    assert_eq!(ge.joins().len(), 1);
    for a in [&ha, &ga] {
        match a.next(event).await.unwrap() {
            Next::Play {
                game_no,
                awaiting_verdict,
                ..
            } => {
                assert_eq!(game_no, 1);
                assert!(awaiting_verdict);
            }
            other => panic!("expected Play, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn upload_conflict_is_not_fatal() {
    let (app, host, guest, event) = setup().await;
    let (mut he, mut ge) = engines(1);
    // A second, different replay of the same game: the server refuses it with 409.
    he.replays.push(simple_game(2, START_A));

    let (h, g) = play_out(&app, event, (&host, &guest), (&mut he, &mut ge)).await;

    assert_eq!(h, Next::Champion);
    assert_eq!(g, Next::Eliminated);
}

fn fake_melee() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh")
}

fn fake_config(work_dir: &Path) -> ClientConfig {
    ClientConfig::with_defaults(
        "00".repeat(32),
        "fake".into(),
        fake_melee(),
        PathBuf::from("/isos/melee.iso"),
        work_dir.to_path_buf(),
    )
}

fn wait_for_exit(engine: &mut CommandEngine) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.running() {
        assert!(Instant::now() < deadline, "fake melee never exited");
        std::thread::sleep(Duration::from_millis(20));
    }
}

const PARAMS: GameParams<'static> = GameParams {
    relay: "relay.example:7000",
    stocks: 3,
    timer_secs: 420,
};

#[test]
fn command_engine_reads_code_and_collects_replays() {
    let work = TempDir::new().unwrap();
    let mut engine = CommandEngine::new(&fake_config(work.path())).unwrap();

    engine.host(&PARAMS).unwrap();
    let code = engine.room_code(Duration::from_secs(5)).unwrap();
    assert_eq!(code.as_deref(), Some("K7QX"));
    assert!(engine.running());
    assert!(engine.take_replays().unwrap().is_empty(), "while running");

    wait_for_exit(&mut engine);
    let replays = engine.take_replays().unwrap();
    assert_eq!(
        replays,
        vec![work.path().join("run-1/Game_20261115T200000.slp")]
    );
    assert!(engine.take_replays().unwrap().is_empty(), "second call");
    assert!(replays[0].exists(), "replays are left in place");
}

#[test]
fn command_engine_substitutes_arguments() {
    let work = TempDir::new().unwrap();
    let mut cfg = fake_config(work.path());
    cfg.join_args = [
        "play",
        "--iso",
        "{iso}",
        "--np-server={relay}",
        "--netplay",
        "join",
        "{code}",
        "--record",
        "{mrpl}",
        "{stocks} stocks, {timer_secs} s",
    ]
    .map(String::from)
    .to_vec();
    let mut engine = CommandEngine::new(&cfg).unwrap();

    engine.join(&PARAMS, "AB12").unwrap();
    wait_for_exit(&mut engine);

    let run = work.path().join("run-1");
    let argv = std::fs::read_to_string(run.join("argv")).unwrap();
    let mrpl = run.join("game.mrpl");
    let expected = [
        "play",
        "--iso",
        "/isos/melee.iso",
        "--np-server=relay.example:7000",
        "--netplay",
        "join",
        "AB12",
        "--record",
        mrpl.to_str().unwrap(),
        "3 stocks, 420 s",
    ];
    assert_eq!(argv.lines().collect::<Vec<_>>(), expected);
}

#[test]
fn init_writes_0600_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("autobracket/client.toml");
    let iso = PathBuf::from("/isos/melee.iso");
    let work = dir.path().join("runs");

    let cfg = config::init(&path, &fake_melee(), &iso, &work, false).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(cfg.engine_build, "melee (fake) 0.0.1 test build");
    assert_eq!(cfg.host_args, config::default_host_args());
    assert_eq!(cfg.join_args, config::default_join_args());
    assert_eq!(cfg.code_regex, config::DEFAULT_CODE_REGEX);
    assert_eq!(ClientConfig::load(&path).unwrap(), cfg);
    cfg.signing_key().unwrap();

    let before = std::fs::read(&path).unwrap();
    assert!(config::init(&path, &fake_melee(), &iso, &work, false).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before, "not overwritten");

    let forced = config::init(&path, &fake_melee(), &iso, &work, true).unwrap();
    assert_ne!(forced.secret_key_hex, cfg.secret_key_hex);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

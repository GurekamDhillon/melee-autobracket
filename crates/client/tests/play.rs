//! The client against a real server: the `play_event` loop with scripted engines, the
//! command engine against `fixtures/fake_melee.sh`, and `init`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use autobracket_client::api::Api;
use autobracket_client::config::ClientConfig;
use autobracket_client::engine::{CommandEngine, Engine, GameParams};
use autobracket_client::play::{play_event, play_event_with, PlayOptions, MAX_FAILED_RUNS};
use autobracket_server::testkit::{TestApp, TestPlayer};
use autobracket_server::worker::judge_due;
use bracket::Format;
use proto::{Next, Role, RoomReq, SigningKey};
use replay::testkit::simple_game;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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
    api_at(&app.url, p)
}

fn api_at(url: &str, p: &TestPlayer) -> Api {
    Api::new(url, p.sk.clone(), "b1")
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
    url: &str,
    event: i64,
    (host, guest): (&TestPlayer, &TestPlayer),
    (he, ge): (&mut ScriptedEngine, &mut ScriptedEngine),
) -> (Next, Next) {
    let (ha, ga) = (api_at(url, host), api_at(url, guest));
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

    let (h, g) = play_out(&app, &app.url, event, (&host, &guest), (&mut he, &mut ge)).await;

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

    let (h, g) = play_out(&app, &app.url, event, (&host, &guest), (&mut he, &mut ge)).await;

    assert_eq!(h, Next::Champion);
    assert_eq!(g, Next::Eliminated);
}

fn fake_melee() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh")
}

fn fake_config(work_dir: &Path) -> ClientConfig {
    ClientConfig::with_defaults(
        "00".repeat(32),
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
    let output = capture_output(&mut engine);

    engine.host(&PARAMS).unwrap();
    let code = engine.room_code(Duration::from_secs(5)).unwrap();
    assert_eq!(code.as_deref(), Some("K7QX"));
    let run = work.path().join("run-1");
    assert_stderr_is_log(&run);
    let log = std::fs::read_to_string(run.join("engine.log")).unwrap();
    assert!(log.contains("MATCH CODE K7QX"), "engine.log: {log:?}");
    assert!(
        output
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("MATCH CODE K7QX")),
        "forwarded: {:?}",
        output.lock().unwrap()
    );
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

/// Collects the lines the engine forwards.
fn capture_output(engine: &mut CommandEngine) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    engine.set_output(move |line| sink.lock().unwrap().push(line.to_string()));
    lines
}

/// The game's stderr is a file in its run directory, not a pipe into the client.
fn assert_stderr_is_log(run: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let target = loop {
        let t = std::fs::read_to_string(run.join("stderr-target")).unwrap_or_default();
        if !t.is_empty() || Instant::now() > deadline {
            break t;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        Path::new(target.trim()),
        run.join("engine.log"),
        "the game's stderr"
    );
}

/// A config whose host runs use one of `fake_melee.sh`'s modes.
fn mode_config(work_dir: &Path, mode: &str) -> ClientConfig {
    let mut cfg = fake_config(work_dir);
    cfg.host_args = vec![mode.to_string()];
    cfg
}

fn wait_for_file(path: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while !path.exists() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

fn read_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap().trim().to_string()
}

#[test]
fn command_engine_sees_a_code_printed_just_before_exit() {
    let work = TempDir::new().unwrap();
    let mut engine = CommandEngine::new(&mode_config(work.path(), "quick")).unwrap();
    let output = capture_output(&mut engine);

    engine.host(&PARAMS).unwrap();
    wait_for_exit(&mut engine);

    let code = engine.room_code(Duration::from_secs(5)).unwrap();
    assert_eq!(code.as_deref(), Some("K7QX"));
    assert!(output
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("MATCH CODE K7QX")));
}

#[test]
fn game_survives_the_engine_being_dropped() {
    let work = TempDir::new().unwrap();
    let mut engine = CommandEngine::new(&mode_config(work.path(), "slow")).unwrap();
    let output = capture_output(&mut engine);
    let run = work.path().join("run-1");

    engine.host(&PARAMS).unwrap();
    // The fake writes its pid files before this line, so once it is forwarded they are
    // complete. Output is forwarded only while the engine exists, so dropping the engine
    // any earlier could also miss the line.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output
        .lock()
        .unwrap()
        .iter()
        .any(|l| l == "fake: first line")
    {
        assert!(
            Instant::now() < deadline,
            "the first line was never forwarded"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_stderr_is_log(&run);
    // Its own process group, so Ctrl-C in the client's terminal does not reach it.
    assert_eq!(
        read_trimmed(&run.join("pgid")),
        read_trimmed(&run.join("pid"))
    );
    drop(engine);

    assert!(
        wait_for_file(
            &run.join("Game_20261115T200000.slp"),
            Duration::from_secs(10)
        ),
        "the game died with the client"
    );
    let log = std::fs::read_to_string(run.join("engine.log")).unwrap();
    assert!(log.contains("fake: second line"), "engine.log: {log:?}");
    assert!(
        output
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "fake: first line"),
        "forwarded: {:?}",
        output.lock().unwrap()
    );
}

#[test]
fn stop_terminates_a_running_game() {
    let work = TempDir::new().unwrap();
    let mut engine = CommandEngine::new(&mode_config(work.path(), "spawner")).unwrap();
    let run = work.path().join("run-1");

    engine.host(&PARAMS).unwrap();
    assert!(wait_for_file(&run.join("bgpid"), Duration::from_secs(5)));
    let bgpid = read_trimmed(&run.join("bgpid"));
    assert!(Path::new(&format!("/proc/{bgpid}")).exists());
    engine.stop();
    assert!(!engine.running());

    // The game's own child goes with it (its whole process group is killed). Once its
    // parent is gone it is reparented, and an init that does not reap orphans (some
    // containers) leaves it a zombie: dead all the same.
    let deadline = Instant::now() + Duration::from_secs(2);
    while alive(&bgpid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(&bgpid), "the game's child {bgpid} outlived stop()");

    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !run.join("Game_20261115T200000.slp").exists(),
        "a stopped game writes no replay"
    );
}

#[test]
fn loading_a_config_checks_its_server_url() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client.toml");
    let mut cfg = fake_config(dir.path());

    cfg.save(&path).unwrap();
    assert_eq!(
        ClientConfig::load(&path).unwrap(),
        cfg,
        "no server yet is fine"
    );

    cfg.server = "not a url".into();
    cfg.save(&path).unwrap();
    let msg = format!("{:#}", ClientConfig::load(&path).unwrap_err());
    assert!(msg.contains("not a url"), "{msg}");
    assert!(msg.contains(&path.display().to_string()), "{msg}");
}

#[tokio::test]
async fn malformed_server_url_ends_play() {
    let api = Api::new("not a url", SigningKey::from_bytes(&[7; 32]), "b1");
    let mut engine = ScriptedEngine::new("", vec![]);

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        play_event(&api, &mut engine, 1, POLL),
    )
    .await
    .expect("a malformed server URL must end play, not be retried");

    assert!(result.is_err());
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

/// How the [`Proxy`] fails a request.
#[derive(Clone, Copy, Debug)]
enum FaultKind {
    /// Answers `503 Service Unavailable` without forwarding.
    Unavailable,
    /// Closes the connection without answering.
    Reset,
}

struct Fault {
    /// Matched against the bytes that carry the request line.
    pattern: &'static str,
    kind: FaultKind,
    left: usize,
}

/// A TCP proxy in front of the server that fails the next `left` requests matching each
/// fault's pattern, and forwards everything else unchanged.
struct Proxy {
    url: String,
    faults: Arc<Mutex<Vec<Fault>>>,
}

impl Proxy {
    async fn start(upstream: &str, faults: Vec<Fault>) -> Proxy {
        let upstream = upstream.trim_start_matches("http://").to_string();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let faults = Arc::new(Mutex::new(faults));
        let shared = faults.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(proxy_conn(client, upstream.clone(), shared.clone()));
            }
        });
        Proxy { url, faults }
    }

    /// Faults not yet used up.
    fn unused(&self) -> Vec<(&'static str, usize)> {
        let faults = self.faults.lock().unwrap();
        faults
            .iter()
            .filter(|f| f.left > 0)
            .map(|f| (f.pattern, f.left))
            .collect()
    }
}

fn take_fault(faults: &Mutex<Vec<Fault>>, chunk: &[u8]) -> Option<FaultKind> {
    let mut faults = faults.lock().unwrap();
    let f = faults.iter_mut().find(|f| {
        f.left > 0
            && chunk
                .windows(f.pattern.len())
                .any(|w| w == f.pattern.as_bytes())
    })?;
    f.left -= 1;
    Some(f.kind)
}

async fn proxy_conn(mut client: TcpStream, upstream: String, faults: Arc<Mutex<Vec<Fault>>>) {
    let Ok(mut server) = TcpStream::connect(&upstream).await else {
        return;
    };
    let (mut cbuf, mut sbuf) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        tokio::select! {
            r = client.read(&mut cbuf) => {
                let Ok(n @ 1..) = r else { return };
                match take_fault(&faults, &cbuf[..n]) {
                    Some(FaultKind::Reset) => return,
                    Some(FaultKind::Unavailable) => {
                        let body = r#"{"error":"down for maintenance"}"#;
                        let res = format!(
                            "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = client.write_all(res.as_bytes()).await;
                        return;
                    }
                    None => {
                        if server.write_all(&cbuf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
            r = server.read(&mut sbuf) => {
                let Ok(n @ 1..) = r else { return };
                if client.write_all(&sbuf[..n]).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[tokio::test]
async fn server_errors_are_retried() {
    let (app, host, guest, event) = setup().await;
    let proxy = Proxy::start(
        &app.url,
        vec![
            Fault {
                pattern: "GET /v1/me/next",
                kind: FaultKind::Unavailable,
                left: 3,
            },
            Fault {
                pattern: "/room HTTP/1.1",
                kind: FaultKind::Unavailable,
                left: 2,
            },
        ],
    )
    .await;
    let (mut he, mut ge) = engines(1);

    let (h, g) = play_out(&app, &proxy.url, event, (&host, &guest), (&mut he, &mut ge)).await;

    assert_eq!(h, Next::Champion);
    assert_eq!(g, Next::Eliminated);
    assert_eq!(proxy.unused(), vec![], "every fault was hit");
}

#[tokio::test]
async fn upload_transport_error_is_retried() {
    let (app, host, guest, event) = setup().await;
    let proxy = Proxy::start(
        &app.url,
        vec![Fault {
            pattern: "/replay HTTP/1.1",
            kind: FaultKind::Reset,
            left: 2,
        }],
    )
    .await;
    let (mut he, mut ge) = engines(1);

    // The set can only be decided if every reset upload was retried and landed.
    let (h, g) = play_out(&app, &proxy.url, event, (&host, &guest), (&mut he, &mut ge)).await;

    assert_eq!(h, Next::Champion);
    assert_eq!(g, Next::Eliminated);
    assert_eq!(proxy.unused(), vec![], "every fault was hit");
    assert_eq!(he.hosts(), 2, "no game was replayed");
}

#[tokio::test]
async fn forbidden_ends_play_with_the_servers_message() {
    let (app, _host, _guest, event) = setup().await;
    let stranger = Api::new(&app.url, SigningKey::from_bytes(&[7; 32]), "b1");
    let mut engine = ScriptedEngine::new("", vec![]);

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        play_event(&stranger, &mut engine, event, POLL),
    )
    .await
    .expect("a 403 must end play, not be retried")
    .expect_err("an unregistered key cannot play");

    let msg = format!("{err:#}");
    assert!(msg.contains("key is not registered"), "{msg}");
    assert!(msg.contains("403"), "{msg}");
}

fn fast_backoff() -> PlayOptions {
    PlayOptions {
        launch_backoff: Duration::from_millis(5),
        ..PlayOptions::new(POLL)
    }
}

#[tokio::test]
async fn host_gives_up_after_runs_without_replays() {
    let (app, host, _guest, event) = setup().await;
    let mut engine = ScriptedEngine::new("K7QX", vec![]);

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        play_event_with(&api(&app, &host), &mut engine, event, &fast_backoff()),
    )
    .await
    .expect("play never gave up")
    .expect_err("play must fail");

    assert!(format!("{err:#}").contains("without a replay"), "{err:#}");
    assert_eq!(engine.hosts(), MAX_FAILED_RUNS as usize);
}

#[tokio::test]
async fn guest_gives_up_after_runs_without_replays() {
    let (app, host, guest, event) = setup().await;
    let codes: Vec<String> = (0..MAX_FAILED_RUNS)
        .map(|i| format!("AB{:02}", i + 10))
        .collect();
    let post = |code: String| {
        let host = &host;
        async move {
            let res = host
                .post_json(&format!("/v1/sets/{event}/0/room"), &RoomReq { code })
                .await;
            assert_eq!(res.status(), reqwest::StatusCode::NO_CONTENT);
        }
    };
    post(codes[0].clone()).await;
    let mut engine = ScriptedEngine::new("", vec![]);
    let calls = engine.calls.clone();
    // The host relaunches with a new code after each failed join, as a real host does
    // when nobody connected: a code that failed is not joined again.
    let host_relaunches = async {
        let mut posted = 1;
        loop {
            let joins = calls.lock().unwrap().joins.len();
            if joins == posted && posted < codes.len() {
                post(codes[posted].clone()).await;
                posted += 1;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    };

    let (guest_api, opts) = (api(&app, &guest), fast_backoff());
    let err = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            r = play_event_with(&guest_api, &mut engine, event, &opts) => r,
            () = host_relaunches => unreachable!(),
        }
    })
    .await
    .expect("play never gave up")
    .expect_err("play must fail");

    assert!(format!("{err:#}").contains("without a replay"), "{err:#}");
    assert_eq!(engine.joins(), codes);
}

#[test]
fn config_debug_redacts_the_key() {
    let cfg = fake_config(Path::new("/tmp"));
    assert!(!format!("{cfg:?}").contains(&cfg.secret_key_hex));
}

#[test]
fn server_urls_are_checked() {
    use autobracket_client::api::check_server_url;
    check_server_url("http://127.0.0.1:8080").unwrap();
    check_server_url("https://bracket.example").unwrap();
    for bad in [
        "not a url",
        "127.0.0.1:8080",
        "ftp://bracket.example",
        "http//x",
    ] {
        assert!(check_server_url(bad).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn a_guest_without_a_room_code_reports_a_no_show_to_the_real_server() {
    let (app, host, guest, event) = setup().await;
    let mut engine = ScriptedEngine::new("", vec![]);
    let guest_api = api(&app, &guest);
    let opts = PlayOptions {
        no_code_wait: Some(Duration::from_millis(100)),
        ..PlayOptions::new(POLL)
    };
    let host_api = api(&app, &host);

    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            r = play_event_with(&guest_api, &mut engine, event, &opts) => panic!("play returned {r:?}"),
            () = async {
                while !matches!(host_api.next(event).await.unwrap(), Next::Review { .. }) {
                    tokio::time::sleep(POLL).await;
                }
            } => {}
        }
    })
    .await
    .expect("the set never went to review");

    assert!(engine.joins().is_empty());
    let text = queue(&app).await;
    assert!(text.contains("no-show"), "{text}");
    assert!(text.contains("no room code"), "{text}");
}

#[tokio::test]
async fn the_no_show_command_reports_the_current_set() {
    let (app, host, guest, event) = setup().await;
    let dir = TempDir::new().unwrap();
    let mut cfg = fake_config(dir.path());
    cfg.server = app.url.clone();
    cfg.secret_key_hex = hex::encode(guest.sk.to_bytes());
    let config = dir.path().join("client.toml");
    cfg.save(&config).unwrap();

    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_autobracket-client"))
        .arg("--config")
        .arg(&config)
        .args([
            "no-show",
            &event.to_string(),
            "--note",
            "not here after 10 min",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Reported a no-show for set 0"), "{stdout}");
    assert!(matches!(
        api(&app, &host).next(event).await.unwrap(),
        Next::Review {
            event_id: 1,
            set_id: 0,
        }
    ));
    let text = queue(&app).await;
    assert!(text.contains("not here after 10 min"), "{text}");
}

/// The organiser's queue, as JSON text.
async fn queue(app: &TestApp) -> String {
    app.as_to()
        .get("/v1/admin/queue")
        .await
        .text()
        .await
        .unwrap()
}

/// Whether `pid` is a live process: it exists and is not a zombie.
fn alive(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with(['Z', 'X']))
    })
}

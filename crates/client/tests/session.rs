//! The play loop across restarts and set changes, with the real command engine running
//! `fixtures/fake_melee.sh` against a stub server: a game is never started twice, never
//! killed because its set went to review, and its replay is always uploaded for the set and
//! game it was started for.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use autobracket_client::api::Api;
use autobracket_client::config::ClientConfig;
use autobracket_client::engine::CommandEngine;
use autobracket_client::play::{play_event_until, play_event_with, PlayOptions};
use common::{host_play, json, Stub};
use proto::{Next, Role, SigningKey};
use tempfile::TempDir;

const POLL: Duration = Duration::from_millis(10);

fn fake_melee() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh")
}

fn config(work: &Path, mode: &str) -> ClientConfig {
    let mut cfg = ClientConfig::with_defaults(
        "07".repeat(32),
        fake_melee(),
        PathBuf::from("/isos/melee.iso"),
        work.to_path_buf(),
    );
    cfg.host_args = vec![mode.into()];
    cfg.join_args = vec![mode.into(), "{code}".into()];
    cfg
}

fn engine(cfg: &ClientConfig) -> CommandEngine {
    let mut e = CommandEngine::new(cfg).unwrap();
    e.set_output(|_| {});
    e
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

/// Waits until `cond` holds.
async fn until(cond: impl Fn() -> bool) {
    while !cond() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Runs `play` until `done` holds, failing if `play` returns first or it takes too long.
async fn play_until(stub: &Stub, engine: &mut CommandEngine, done: impl Fn(&Stub) -> bool) {
    let api = api(stub);
    let opts = opts();
    tokio::time::timeout(Duration::from_secs(20), async {
        tokio::select! {
            () = until(|| done(stub)) => {}
            r = play_event_with(&api, engine, 1, &opts) => panic!("play returned {r:?}"),
        }
    })
    .await
    .expect("timed out");
}

/// Runs `play` until `stop` holds, as an interrupted `play` would end.
async fn play_then_stop(stub: &Stub, engine: &mut CommandEngine, stop: impl Fn(&Stub) -> bool) {
    let ended = tokio::time::timeout(
        Duration::from_secs(20),
        play_event_until(&api(stub), engine, 1, &opts(), until(|| stop(stub))),
    )
    .await
    .expect("timed out")
    .expect("play failed");
    assert!(ended.is_none(), "play ended on its own: {ended:?}");
}

fn runs(work: &Path) -> Vec<PathBuf> {
    let mut runs: Vec<PathBuf> = std::fs::read_dir(work)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("run-"))
        })
        .collect();
    runs.sort();
    runs
}

fn pid_of(run: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = std::fs::read_to_string(run.join("pid")) {
            if let Ok(pid) = s.trim().parse() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "no pid in {}", run.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `pid` is a live process (not a zombie).
fn alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        let state = s.rsplit_once(')').map(|(_, rest)| rest.trim_start());
        !state.is_some_and(|r| r.starts_with('Z') || r.starts_with('X'))
    })
}

async fn wait_dead(pid: i32) {
    tokio::time::timeout(Duration::from_secs(10), until(|| !alive(pid)))
        .await
        .expect("the game never ended");
}

/// Kills a fake game's process group when the test ends, however it ends.
struct GameGuard(i32);

impl Drop for GameGuard {
    fn drop(&mut self) {
        // SAFETY: `killpg` only sends a signal.
        unsafe {
            libc::killpg(self.0, libc::SIGKILL);
        }
    }
}

const REPLAY: &str = "/v1/sets/1/0/games/1/replay";

/// Whether `play` has recorded, in `work`'s session file, that the server took the room
/// code (the stub sees the post a moment before `play` sees the answer).
fn code_posted(work: &Path) -> bool {
    matches!(
        autobracket_client::session::SessionFile::new(work).load(),
        autobracket_client::session::Loaded::Record(r) if r.code_posted
    )
}

/// This player hosts set 0 game 1 until its replay is uploaded, and then waits for the
/// verdict. `room` answers each room-code post in turn (the last repeats); `replay` each
/// upload.
async fn host_server(room: &[u16], replay: u16) -> Stub {
    let room: Vec<u16> = room.to_vec();
    let rooms = AtomicUsize::new(0);
    let uploaded = AtomicBool::new(false);
    Stub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => {
            let mut next = host_play();
            if let Next::Play {
                awaiting_verdict, ..
            } = &mut next
            {
                *awaiting_verdict = uploaded.load(Ordering::SeqCst);
            }
            (200, json(&next))
        }
        ("POST", "/v1/sets/1/0/room") => {
            let i = rooms.fetch_add(1, Ordering::SeqCst);
            match room[i.min(room.len() - 1)] {
                204 => (204, String::new()),
                s => (s, r#"{"error":"set is not in play"}"#.into()),
            }
        }
        ("POST", REPLAY) => {
            if replay == 202 {
                uploaded.store(true, Ordering::SeqCst);
                (202, String::new())
            } else {
                (replay, r#"{"error":"not a valid replay"}"#.into())
            }
        }
        ("GET", p) if p.starts_with("/v1/events/") => (404, r#"{"error":"no"}"#.into()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await
}

#[tokio::test]
async fn restarting_play_mid_game_adopts_the_game_instead_of_starting_another() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    let cfg = config(work.path(), "game2");

    // `play` is interrupted (Ctrl-C) once the code is sent; the game keeps running.
    play_then_stop(&stub, &mut engine(&cfg), |s| {
        s.count("/v1/sets/1/0/room") == 1 && code_posted(work.path())
    })
    .await;
    let game = pid_of(&work.path().join("run-1"));
    let _guard = GameGuard(game);
    assert!(alive(game), "the game must outlive the interrupted `play`");

    play_until(&stub, &mut engine(&cfg), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert_eq!(runs(work.path()).len(), 1, "a second game was started");
    assert_eq!(
        stub.count("/v1/sets/1/0/room"),
        1,
        "a second room code was posted"
    );
    assert_eq!(stub.requests("POST", REPLAY)[0].body, b"fake replay");
}

/// As [`restarting_play_mid_game_adopts_the_game_instead_of_starting_another`], with the
/// work directory given as `work_dir`, a path to the directory `real`.
async fn a_restart_adopts_the_game_in(work_dir: &Path, real: &Path) {
    let stub = host_server(&[204], 202).await;
    let cfg = config(work_dir, "game2");

    play_then_stop(&stub, &mut engine(&cfg), |s| {
        s.count("/v1/sets/1/0/room") == 1 && code_posted(work_dir)
    })
    .await;
    let game = pid_of(&real.join("run-1"));
    let _guard = GameGuard(game);
    assert!(alive(game), "the game must outlive the interrupted `play`");

    play_until(&stub, &mut engine(&cfg), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert_eq!(runs(real).len(), 1, "a second game was started");
    assert_eq!(
        stub.count("/v1/sets/1/0/room"),
        1,
        "a second room code was posted"
    );
    assert_eq!(stub.requests("POST", REPLAY)[0].body, b"fake replay");
}

#[tokio::test]
async fn a_work_dir_reached_through_a_symlink_is_adopted_on_restart() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    a_restart_adopts_the_game_in(&link, &real).await;
}

#[tokio::test]
async fn a_work_dir_given_with_dot_dot_is_adopted_on_restart() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("other")).unwrap();
    let work = dir.path().join("other/../work");

    a_restart_adopts_the_game_in(&work, &dir.path().join("work")).await;
}

#[tokio::test]
async fn restarting_play_after_the_game_ended_uploads_its_replay_first() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    let cfg = config(work.path(), "game2");

    play_then_stop(&stub, &mut engine(&cfg), |s| {
        s.count("/v1/sets/1/0/room") == 1
    })
    .await;
    let run = work.path().join("run-1");
    wait_dead(pid_of(&run)).await;
    assert!(run.join("Game_20261115T200000.slp").exists());
    let before = stub.count("/");

    play_until(&stub, &mut engine(&cfg), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert_eq!(runs(work.path()).len(), 1, "a second game was started");
    let after: Vec<String> = stub.paths()[before..].to_vec();
    assert_eq!(after[0], REPLAY, "not uploaded first: {after:?}");
}

#[tokio::test]
async fn a_second_play_on_the_same_work_dir_is_refused() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    let cfg = config(work.path(), "game2");
    let api1 = api(&stub);
    let opts = opts();
    let mut first = engine(&cfg);

    tokio::time::timeout(Duration::from_secs(20), async {
        let first_play = play_event_with(&api1, &mut first, 1, &opts);
        tokio::pin!(first_play);
        // The first `play` hosts; then a second one starts on the same work directory.
        tokio::select! {
            () = until(|| stub.count("/v1/sets/1/0/room") == 1) => {}
            r = &mut first_play => panic!("play returned {r:?}"),
        }
        let mut second = engine(&cfg);
        let err = play_event_with(&api(&stub), &mut second, 1, &opts)
            .await
            .expect_err("a second play on one work directory must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("already"), "{msg}");
        assert!(msg.contains(&work.path().display().to_string()), "{msg}");
        // The first one is undisturbed: its game finishes and is uploaded.
        tokio::select! {
            () = until(|| stub.count(REPLAY) == 1 && stub.polls_after(REPLAY) > 3) => {}
            r = &mut first_play => panic!("play returned {r:?}"),
        }
    })
    .await
    .expect("timed out");
    assert_eq!(runs(work.path()).len(), 1);
}

#[tokio::test]
async fn review_does_not_stop_a_running_game_and_its_replay_is_uploaded() {
    let reviewed = Arc::new(AtomicBool::new(false));
    let flag = reviewed.clone();
    let stub = Stub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => {
            if flag.load(Ordering::SeqCst) {
                (200, json(&Next::Review { set_id: 0 }))
            } else {
                (200, json(&host_play()))
            }
        }
        ("POST", "/v1/sets/1/0/room") => {
            // The opponent reports a no-show just as the game starts.
            flag.store(true, Ordering::SeqCst);
            (204, String::new())
        }
        // The server takes the replay of a set in review as evidence.
        ("POST", REPLAY) => (202, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await;
    let work = TempDir::new().unwrap();

    play_until(&stub, &mut engine(&config(work.path(), "game2")), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert!(work.path().join("run-1/Game_20261115T200000.slp").exists());
    assert_eq!(runs(work.path()).len(), 1);
}

#[tokio::test]
async fn a_different_set_stops_the_game() {
    let moved = Arc::new(AtomicBool::new(false));
    let flag = moved.clone();
    let stub = Stub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => {
            if flag.load(Ordering::SeqCst) {
                let mut next = host_play();
                if let Next::Play { set_id, role, .. } = &mut next {
                    *set_id = 1;
                    *role = Role::Guest;
                }
                (200, json(&next))
            } else {
                (200, json(&host_play()))
            }
        }
        ("POST", "/v1/sets/1/0/room") => (204, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await;
    let work = TempDir::new().unwrap();
    let cfg = config(work.path(), "sleepy");
    let api = api(&stub);
    let opts = opts();
    let mut e = engine(&cfg);

    tokio::time::timeout(Duration::from_secs(20), async {
        let play = play_event_with(&api, &mut e, 1, &opts);
        tokio::pin!(play);
        tokio::select! {
            () = until(|| stub.count("/v1/sets/1/0/room") == 1) => {}
            r = &mut play => panic!("play returned {r:?}"),
        }
        let game = pid_of(&work.path().join("run-1"));
        let _guard = GameGuard(game);
        moved.store(true, Ordering::SeqCst);
        tokio::select! {
            () = until(|| !alive(game)) => {}
            r = &mut play => panic!("play returned {r:?}"),
        }
    })
    .await
    .expect("the game of the old set was not stopped");
}

#[tokio::test]
async fn a_refused_room_code_stops_that_run_and_play_carries_on() {
    let stub = host_server(&[409, 204], 202).await;
    let work = TempDir::new().unwrap();

    play_until(&stub, &mut engine(&config(work.path(), "game2")), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    let runs = runs(work.path());
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        !runs[0].join("Game_20261115T200000.slp").exists(),
        "the refused run was not stopped"
    );
    assert_eq!(stub.count("/v1/sets/1/0/room"), 2);
}

#[tokio::test]
async fn a_room_refused_for_a_game_in_progress_is_not_hosted_again_until_next_changes() {
    // The server's 409 while a game of the set is still uploading or being judged; then
    // (`next_game`) it offers game 2.
    let next_game = Arc::new(AtomicBool::new(false));
    let flag = next_game.clone();
    let stub = Stub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => {
            let mut next = host_play();
            if let Next::Play { game_no, .. } = &mut next {
                *game_no = if flag.load(Ordering::SeqCst) { 2 } else { 1 };
            }
            (200, json(&next))
        }
        ("POST", "/v1/sets/1/0/room") if !flag.load(Ordering::SeqCst) => (
            409,
            r#"{"error":"a game of this set is in progress or awaiting a verdict"}"#.into(),
        ),
        ("POST", "/v1/sets/1/0/room") => (204, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await;
    let work = TempDir::new().unwrap();

    play_until(&stub, &mut engine(&config(work.path(), "game2")), |s| {
        let rooms = s.count("/v1/sets/1/0/room");
        if !next_game.load(Ordering::SeqCst) {
            // Not hosted again within 50 polls (the launch backoff here is 5 ms).
            assert!(rooms <= 1, "hosted again after the 409: {rooms} room posts");
            if rooms == 1 && s.polls_after("/room") > 50 {
                next_game.store(true, Ordering::SeqCst);
            }
        }
        rooms == 2
    })
    .await;

    assert_eq!(runs(work.path()).len(), 2, "game 2 was hosted once");
}

#[tokio::test]
async fn a_refused_upload_keeps_the_file_and_play_carries_on() {
    let stub = host_server(&[204], 400).await;
    let work = TempDir::new().unwrap();

    play_until(&stub, &mut engine(&config(work.path(), "game2")), |s| {
        s.count(REPLAY) >= 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert!(work.path().join("run-1/Game_20261115T200000.slp").exists());
}

use autobracket_client::session::{ProcId, Record, SessionFile};

/// A finished run directory holding one replay, as a game leaves it.
fn finished_run(work: &Path, n: u32) -> PathBuf {
    let run = work.join(format!("run-{n}"));
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("engine.log"), "play: done\n").unwrap();
    std::fs::write(run.join("Game_20261115T200000.slp"), b"left over").unwrap();
    run
}

fn record(run_dir: &Path, process: ProcId) -> Record {
    Record {
        event: 1,
        set_id: 0,
        game_no: 1,
        host: true,
        run_dir: run_dir.to_path_buf(),
        process,
        code_posted: true,
        handled: Vec::new(),
    }
}

#[tokio::test]
async fn a_session_whose_process_is_gone_uploads_the_replay_and_plays_on() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    let run = finished_run(work.path(), 1);
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let process = ProcId::of(gone.id()).unwrap();
    gone.wait().unwrap();
    SessionFile::new(work.path())
        .save(&record(&run, process))
        .unwrap();

    play_until(&stub, &mut engine(&config(work.path(), "game2")), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert_eq!(stub.paths()[0], REPLAY, "not uploaded first");
    assert_eq!(stub.requests("POST", REPLAY)[0].body, b"left over");
    assert_eq!(
        runs(work.path()).len(),
        1,
        "no new game: the server awaits the verdict"
    );
    assert!(!work.path().join("session.json").exists());
}

#[tokio::test]
async fn a_recycled_pid_is_not_adopted_or_killed() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    let run = finished_run(work.path(), 1);
    // An unrelated live process with the recorded pid: the start time does not match.
    let mut stranger = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut process = ProcId::of(stranger.id()).unwrap();
    process.start_time += 1;
    SessionFile::new(work.path())
        .save(&record(&run, process))
        .unwrap();

    play_until(&stub, &mut engine(&config(work.path(), "game2")), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert_eq!(stub.requests("POST", REPLAY)[0].body, b"left over");
    assert!(
        stranger.try_wait().unwrap().is_none(),
        "the unrelated process was killed"
    );
    let _ = stranger.kill();
    let _ = stranger.wait();
}

#[tokio::test]
async fn a_live_recorded_process_in_another_directory_stops_play() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    let run = finished_run(work.path(), 1);
    // Right pid, start time and boot, but not running in the recorded run directory: it
    // may be the game, so nothing is started over it and it is not treated as ended.
    let elsewhere = TempDir::new().unwrap();
    let mut stranger = std::process::Command::new("sleep")
        .arg("30")
        .current_dir(elsewhere.path())
        .spawn()
        .unwrap();
    let process = ProcId::of(stranger.id()).unwrap();
    SessionFile::new(work.path())
        .save(&record(&run, process))
        .unwrap();

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        play_event_with(
            &api(&stub),
            &mut engine(&config(work.path(), "game2")),
            1,
            &opts(),
        ),
    )
    .await
    .expect("play did not stop")
    .expect_err("play must refuse to start a game");

    let msg = format!("{err:#}");
    assert!(msg.contains(&run.display().to_string()), "{msg}");
    let cwd = elsewhere.path().canonicalize().unwrap();
    assert!(msg.contains(&cwd.display().to_string()), "{msg}");
    assert!(msg.contains("may still be running"), "{msg}");
    assert_eq!(runs(work.path()).len(), 1, "a game was started");
    assert_eq!(stub.count("/v1/sets/1/0/room"), 0);
    assert!(stub.requests("POST", REPLAY).is_empty());
    assert!(
        work.path().join("session.json").exists(),
        "the record of a game that may be running was dropped"
    );
    assert!(
        stranger.try_wait().unwrap().is_none(),
        "the process was killed"
    );
    let _ = stranger.kill();
    let _ = stranger.wait();
}

#[tokio::test]
async fn a_corrupt_session_file_is_ignored() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    std::fs::write(work.path().join("session.json"), b"{not json").unwrap();

    play_until(&stub, &mut engine(&config(work.path(), "game2")), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    assert_eq!(runs(work.path()).len(), 1, "it played normally");
}

#[tokio::test]
async fn an_adopted_host_game_whose_code_was_not_sent_sends_it() {
    let stub = host_server(&[204], 202).await;
    let work = TempDir::new().unwrap();
    let cfg = config(work.path(), "game2");
    // An earlier `play` started the game and died before sending its code.
    let mut first = engine(&cfg);
    autobracket_client::engine::Engine::host(
        &mut first,
        &autobracket_client::engine::GameParams {
            relay: "relay.example:7000",
            stocks: 4,
            timer_secs: 480,
        },
    )
    .unwrap();
    let run = work.path().join("run-1");
    let game = pid_of(&run);
    let _guard = GameGuard(game);
    let process = autobracket_client::engine::Engine::process(&first).unwrap();
    let mut rec = record(&run, process);
    rec.code_posted = false;
    SessionFile::new(work.path()).save(&rec).unwrap();
    drop(first);

    play_until(&stub, &mut engine(&cfg), |s| {
        s.count(REPLAY) == 1 && s.polls_after(REPLAY) > 3
    })
    .await;

    let rooms = stub.requests("POST", "/v1/sets/1/0/room");
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].json()["code"], "K7QX");
    assert_eq!(runs(work.path()).len(), 1);
}

/// A host run whose game ends at the worst moment: `running()` is true exactly once after
/// the room code is handed over, then false.
struct EndsBetweenChecks {
    hosts: usize,
    trues_left: u32,
    dir: TempDir,
    replay_taken: bool,
}

impl autobracket_client::engine::Engine for EndsBetweenChecks {
    fn host(&mut self, _p: &autobracket_client::engine::GameParams) -> anyhow::Result<()> {
        self.hosts += 1;
        self.trues_left = 1;
        self.replay_taken = false;
        Ok(())
    }
    fn join(
        &mut self,
        _p: &autobracket_client::engine::GameParams,
        _c: &str,
    ) -> anyhow::Result<()> {
        unreachable!("only hosts")
    }
    fn room_code(&mut self, _wait: Duration) -> anyhow::Result<Option<String>> {
        Ok(Some("K7QX".into()))
    }
    fn running(&mut self) -> bool {
        if self.trues_left > 0 {
            self.trues_left -= 1;
            return true;
        }
        false
    }
    fn take_replays(&mut self) -> anyhow::Result<Vec<PathBuf>> {
        if std::mem::replace(&mut self.replay_taken, true) {
            return Ok(Vec::new());
        }
        let path = self.dir.path().join(format!("run-{}.slp", self.hosts));
        std::fs::write(&path, b"replay")?;
        Ok(vec![path])
    }
    fn stop(&mut self) {}
}

#[tokio::test]
async fn a_game_that_ends_mid_poll_is_collected_not_followed_by_another() {
    let stub = host_server(&[204], 202).await;
    let mut engine = EndsBetweenChecks {
        hosts: 0,
        trues_left: 0,
        dir: TempDir::new().unwrap(),
        replay_taken: false,
    };
    let api = api(&stub);
    let opts = opts();

    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = until(|| stub.count(REPLAY) == 1 && stub.polls_after(REPLAY) > 3) => {}
            r = play_event_with(&api, &mut engine, 1, &opts) => panic!("play returned {r:?}"),
        }
    })
    .await
    .expect("timed out");

    assert_eq!(
        engine.hosts, 1,
        "a second game started before the first was collected"
    );
}

#[test]
fn play_needs_proc() {
    autobracket_client::session::require_proc(Path::new("/proc/self/stat")).unwrap();
    let dir = TempDir::new().unwrap();
    let err = autobracket_client::session::require_proc(&dir.path().join("self/stat"))
        .expect_err("no /proc must be refused");
    let msg = format!("{err:#}");
    assert!(msg.contains("Linux"), "{msg}");
    assert!(msg.contains("/proc"), "{msg}");
}

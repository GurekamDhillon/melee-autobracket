//! Interrupting `play`: the built binary against a stub server, with a fake game that keeps
//! running.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use autobracket_client::config::ClientConfig;
use common::{host_play, json, Stub};
use tempfile::TempDir;

/// Kills the fake game's process group when the test ends, however it ends.
struct GameGuard(Option<i32>);

impl Drop for GameGuard {
    fn drop(&mut self) {
        if let Some(pgid) = self.0 {
            // SAFETY: `killpg` only sends a signal.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

/// Kills the client if the test fails before it exits.
struct ClientGuard(Child);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A sparse file of the smallest size `play` accepts as a disc image.
fn disc(dir: &Path) -> PathBuf {
    let path = dir.join("melee.iso");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(autobracket_client::config::MIN_DISC_BYTES)
        .unwrap();
    path
}

fn fake_melee() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh")
}

async fn interrupted_play_leaves_guidance(signal: i32, expected_status: i32) {
    let stub = Stub::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => (200, json(&host_play())),
        ("POST", "/v1/sets/1/0/room") => (204, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await;
    let dir = TempDir::new().unwrap();
    let work = dir.path().join("work");
    let mut cfg = ClientConfig::with_defaults(
        "07".repeat(32),
        fake_melee(),
        disc(dir.path()),
        work.clone(),
    );
    cfg.server = stub.url.clone();
    cfg.host_args = vec!["sleepy".into()];
    let config = dir.path().join("client.toml");
    cfg.save(&config).unwrap();
    let (stdout, stderr) = (dir.path().join("stdout"), dir.path().join("stderr"));

    let child = Command::new(env!("CARGO_BIN_EXE_autobracket-client"))
        .arg("--config")
        .arg(&config)
        .args(["play", "1", "--poll-secs", "1"])
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let mut client = ClientGuard(child);
    let mut game = GameGuard(None);

    // Wait until the game is running and its room code was handed over.
    let run = work.join("run-1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while stub.requests("POST", "/room").is_empty() || !run.join("pid").exists() {
        assert!(Instant::now() < deadline, "the game never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let game_pid: i32 = std::fs::read_to_string(run.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    game.0 = Some(game_pid);

    // SAFETY: `kill` only sends a signal.
    unsafe {
        libc::kill(client.0.id() as i32, signal);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = client.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "play did not exit on the signal");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let out =
        std::fs::read_to_string(&stdout).unwrap() + &std::fs::read_to_string(&stderr).unwrap();
    assert_eq!(status.code(), Some(expected_status), "{status:?}\n{out}");
    assert!(
        out.contains("autobracket-client upload 1 0 1 FILE"),
        "no upload command in:\n{out}"
    );
    assert!(
        out.contains(&run.display().to_string()),
        "no run directory in:\n{out}"
    );
    assert!(out.contains("set 0 game 1"), "no set and game in:\n{out}");
    assert!(
        Path::new(&format!("/proc/{game_pid}")).exists(),
        "the game was killed with the client"
    );
}

#[tokio::test]
async fn sigterm_leaves_the_game_running_and_says_how_to_upload() {
    interrupted_play_leaves_guidance(libc::SIGTERM, 143).await;
}

#[tokio::test]
async fn sigint_leaves_the_game_running_and_says_how_to_upload() {
    interrupted_play_leaves_guidance(libc::SIGINT, 130).await;
}

#[tokio::test]
async fn a_second_play_on_the_same_work_directory_is_refused() {
    let stub = Stub::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.starts_with("/v1/me/next") => (200, json(&host_play())),
        ("POST", "/v1/sets/1/0/room") => (204, String::new()),
        _ => (404, r#"{"error":"not found"}"#.into()),
    })
    .await;
    let dir = TempDir::new().unwrap();
    let work = dir.path().join("work");
    let mut cfg = ClientConfig::with_defaults(
        "07".repeat(32),
        fake_melee(),
        disc(dir.path()),
        work.clone(),
    );
    cfg.server = stub.url.clone();
    cfg.host_args = vec!["sleepy".into()];
    let config = dir.path().join("client.toml");
    cfg.save(&config).unwrap();
    let play = || {
        Command::new(env!("CARGO_BIN_EXE_autobracket-client"))
            .arg("--config")
            .arg(&config)
            .args(["play", "1", "--poll-secs", "1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut first = ClientGuard(play());
    let mut game = GameGuard(None);
    let run = work.join("run-1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while stub.requests("POST", "/room").is_empty() || !run.join("pid").exists() {
        assert!(Instant::now() < deadline, "the game never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    game.0 = std::fs::read_to_string(run.join("pid"))
        .ok()
        .and_then(|p| p.trim().parse().ok());

    let second = play().wait_with_output().unwrap();

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(!second.status.success());
    assert!(stderr.contains("already running"), "{stderr}");
    assert!(
        stderr.contains(&format!("pid {}", first.0.id())),
        "{stderr}"
    );
    assert!(
        first.0.try_wait().unwrap().is_none(),
        "the first play stopped"
    );
    assert!(!work.join("run-2").exists(), "a second game was started");
}

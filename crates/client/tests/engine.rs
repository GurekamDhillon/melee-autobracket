//! The command engine against `fixtures/fake_melee.sh`: how runs end, where they run, and
//! the paths and templates in the config.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use autobracket_client::config::{self, ClientConfig};
use autobracket_client::engine::{CommandEngine, Engine, GameParams, RunExit};
use tempfile::TempDir;

const PARAMS: GameParams<'static> = GameParams {
    relay: "relay.example:7000",
    stocks: 3,
    timer_secs: 420,
};

fn fake_melee() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh")
}

fn mode_config(work_dir: &Path, mode: &str) -> ClientConfig {
    let mut cfg = ClientConfig::with_defaults(
        "00".repeat(32),
        "fake".into(),
        fake_melee(),
        PathBuf::from("/isos/melee.iso"),
        work_dir.to_path_buf(),
    );
    cfg.host_args = vec![mode.to_string()];
    cfg
}

fn wait_for_exit(engine: &mut CommandEngine) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.running() {
        assert!(Instant::now() < deadline, "fake melee never exited");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How a host run in `mode` ended.
fn exit_of(mode: &str) -> Option<RunExit> {
    let work = TempDir::new().unwrap();
    let mut engine = CommandEngine::new(&mode_config(work.path(), mode)).unwrap();
    engine.set_output(|_| {});
    assert_eq!(engine.last_exit(), None, "before any run");
    engine.host(&PARAMS).unwrap();
    wait_for_exit(&mut engine);
    engine.last_exit()
}

#[test]
fn no_opponent_line_and_exit_4_is_a_no_opponent_exit() {
    assert_eq!(
        exit_of("no-opponent"),
        Some(RunExit {
            code: Some(4),
            no_opponent: true
        })
    );
}

#[test]
fn exit_4_alone_is_a_no_opponent_exit() {
    assert_eq!(
        exit_of("exit4"),
        Some(RunExit {
            code: Some(4),
            no_opponent: true
        })
    );
}

#[test]
fn the_no_opponent_line_alone_is_a_no_opponent_exit() {
    assert_eq!(
        exit_of("line-exit1"),
        Some(RunExit {
            code: Some(1),
            no_opponent: true
        })
    );
}

#[test]
fn exit_1_is_not_a_no_opponent_exit() {
    assert_eq!(
        exit_of("fail"),
        Some(RunExit {
            code: Some(1),
            no_opponent: false
        })
    );
    assert_eq!(
        exit_of("quick"),
        Some(RunExit {
            code: Some(0),
            no_opponent: false
        })
    );
}

#[test]
fn the_game_runs_in_its_run_directory() {
    let work = TempDir::new().unwrap();
    let mut engine = CommandEngine::new(&mode_config(work.path(), "cwd")).unwrap();
    engine.host(&PARAMS).unwrap();
    wait_for_exit(&mut engine);

    let stray = work.path().join("run-1/stray.txt");
    let landed_here = Path::new("stray.txt").exists();
    let _ = std::fs::remove_file("stray.txt");
    assert!(!landed_here, "the game wrote to the client's directory");
    assert!(
        stray.exists(),
        "the game did not write to its run directory"
    );
}

#[test]
fn relative_config_paths_resolve_against_the_config_directory() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client.toml");
    let mut cfg = mode_config(Path::new("runs"), "x");
    cfg.iso = PathBuf::from("discs/melee.iso");
    cfg.save(&path).unwrap();
    // A relative `melee` too, written by hand.
    let text = std::fs::read_to_string(&path).unwrap();
    let melee = format!("melee = {:?}", fake_melee().display().to_string());
    assert!(text.contains(&melee), "{text}");
    std::fs::write(&path, text.replace(&melee, "melee = \"bin/melee\"")).unwrap();

    // Loaded from a different working directory than the config's.
    let loaded = ClientConfig::load(&path).unwrap();
    assert_eq!(loaded.melee, dir.path().join("bin/melee"));
    assert_eq!(loaded.iso, dir.path().join("discs/melee.iso"));
    assert_eq!(loaded.work_dir, dir.path().join("runs"));
}

#[test]
fn iso_and_mrpl_substitutions_are_absolute() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client.toml");
    let mut cfg = mode_config(Path::new("runs"), "quick");
    cfg.iso = PathBuf::from("melee.iso");
    cfg.host_args = vec!["quick".into(), "{iso}".into(), "{mrpl}".into()];
    cfg.save(&path).unwrap();

    let mut engine = CommandEngine::new(&ClientConfig::load(&path).unwrap()).unwrap();
    engine.set_output(|_| {});
    engine.host(&PARAMS).unwrap();
    wait_for_exit(&mut engine);

    let run = dir.path().join("runs/run-1");
    let argv = std::fs::read_to_string(run.join("argv")).unwrap();
    let iso = dir.path().join("melee.iso");
    let mrpl = run.join("game.mrpl");
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        ["quick", iso.to_str().unwrap(), mrpl.to_str().unwrap()]
    );
}

fn count(args: &[String], flag: &str) -> usize {
    args.iter().filter(|a| *a == flag).count()
}

#[test]
fn init_headless_adds_the_display_less_flags_once() {
    let dir = TempDir::new().unwrap();
    let iso = PathBuf::from("/isos/melee.iso");
    let work = dir.path().join("runs");

    let path = dir.path().join("headless.toml");
    let cfg = config::init(&path, &fake_melee(), &iso, &work, false, true).unwrap();
    for args in [&cfg.host_args, &cfg.join_args] {
        assert_eq!(count(args, "--headless"), 1, "{args:?}");
        assert_eq!(count(args, "--no-audio"), 1, "{args:?}");
    }
    assert_eq!(ClientConfig::load(&path).unwrap(), cfg);

    let path = dir.path().join("plain.toml");
    let cfg = config::init(&path, &fake_melee(), &iso, &work, false, false).unwrap();
    assert_eq!(cfg.host_args, config::default_host_args());
    assert_eq!(cfg.join_args, config::default_join_args());
    for args in [&cfg.host_args, &cfg.join_args] {
        assert_eq!(count(args, "--headless"), 0, "{args:?}");
        assert_eq!(count(args, "--no-audio"), 0, "{args:?}");
    }
}

#[test]
fn the_written_config_explains_the_alternatives() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client.toml");
    config::init(
        &path,
        &fake_melee(),
        Path::new("/isos/melee.iso"),
        &dir.path().join("runs"),
        false,
        false,
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let comments: String = text
        .lines()
        .filter(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "--np-join-wait",
        "init --headless",
        "--match",
        "--np-css",
        "relative",
    ] {
        assert!(
            comments.contains(needle),
            "{needle:?} missing from:\n{comments}"
        );
    }
}

#[test]
fn init_makes_relative_paths_absolute() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client.toml");
    let cfg = config::init(
        &path,
        &fake_melee(),
        Path::new("discs/melee.iso"),
        Path::new("runs"),
        false,
        false,
    )
    .unwrap();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(cfg.iso, cwd.join("discs/melee.iso"));
    assert_eq!(cfg.work_dir, cwd.join("runs"));
    assert_eq!(ClientConfig::load(&path).unwrap(), cfg);
}

fn help(sub: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_autobracket-client"))
        .args([sub, "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn help_documents_headless_and_join_wait() {
    let init = help("init");
    assert!(init.contains("--headless"), "{init}");
    assert!(init.contains("--no-audio"), "{init}");
    let play = help("play");
    assert!(play.contains("--np-join-wait"), "{play}");
}

//! The command engine against `fixtures/fake_melee.sh`: how runs end, where they run, and
//! the paths and templates in the config.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use autobracket_client::config::ClientConfig;
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

fn exit(code: i32, no_opponent: bool) -> Option<RunExit> {
    Some(RunExit {
        code: Some(code),
        no_opponent,
        host_gone: false,
    })
}

#[test]
fn the_no_opponent_line_with_exit_5_is_a_no_opponent_exit() {
    assert_eq!(exit_of("no-opponent"), exit(5, true));
}

#[test]
fn the_no_opponent_line_decides_whatever_the_exit_code() {
    assert_eq!(exit_of("line-exit0"), exit(0, true));
    assert_eq!(exit_of("line-exit1"), exit(1, true));
}

#[test]
fn exit_5_without_the_line_is_not_a_no_opponent_exit() {
    assert_eq!(exit_of("exit5"), exit(5, false));
}

#[test]
fn a_failed_disc_check_is_not_a_no_opponent_exit() {
    assert_eq!(exit_of("disc-fail"), exit(4, false));
}

#[test]
fn other_exits_are_not_no_opponent_exits() {
    assert_eq!(exit_of("fail"), exit(1, false));
    assert_eq!(exit_of("quick"), exit(0, false));
    assert_eq!(exit_of("usage"), exit(2, false));
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

#[test]
fn the_installed_engines_no_connection_line_is_a_no_opponent_exit() {
    // engine 0cd085d, play_net.rs:1154: the host gives up after --np-timeout, exit 1.
    assert_eq!(exit_of("np-timeout"), exit(1, true));
}

/// How a guest run in `mode` ended.
fn guest_exit_of(mode: &str) -> Option<RunExit> {
    let work = TempDir::new().unwrap();
    let mut cfg = mode_config(work.path(), mode);
    cfg.join_args = vec![mode.to_string(), "{code}".into()];
    let mut engine = CommandEngine::new(&cfg).unwrap();
    engine.set_output(|_| {});
    engine.join(&PARAMS, "K7QX").unwrap();
    wait_for_exit(&mut engine);
    engine.last_exit()
}

#[test]
fn a_room_that_is_gone_is_a_host_gone_exit() {
    let gone = Some(RunExit {
        code: Some(1),
        no_opponent: false,
        host_gone: true,
    });
    assert_eq!(guest_exit_of("no-room"), gone);
    assert_eq!(guest_exit_of("host-closed"), gone);
    assert_eq!(guest_exit_of("fail"), exit(1, false));
}

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use autobracket_server::resim::{outcome_from_csv, Resim, Slpcheck, MIN_DISC_BYTES};
use proto::ResimOutcome;
use replay::testkit::SlpBuilder;
use replay::PlayerKind;

const HEADER: &str = "file,status,slippi,stage,roster,ucf_dash_shield,frozen_ps,pal,gecko_codes,gecko_hash,replay_frames,frames_compared,last_frame,first_frame,first_port,first_field,engine_value,replay_value,fd_x,fd_y,panic,notes,seed_first";

fn fake_engine() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_slpcheck.sh")
}

/// A sparse file of `len` bytes at `dir/disc.iso`: it takes no space on disk, and reads as zeros.
fn sparse_disc(dir: &tempfile::TempDir, len: u64) -> PathBuf {
    let path = dir.path().join("disc.iso");
    std::fs::File::create(&path).unwrap().set_len(len).unwrap();
    path
}

/// Stands in for the disc image: `Slpcheck::new` only checks that the disc is a readable regular
/// file of disc size, and the fake engine never reads it.
fn fake_disc(dir: &tempfile::TempDir) -> PathBuf {
    sparse_disc(dir, MIN_DISC_BYTES + (MIN_DISC_BYTES >> 1))
}

async fn fake(timeout: Duration) -> Slpcheck {
    // The disc only has to exist while `new` checks it.
    let dir = tempfile::tempdir().unwrap();
    Slpcheck::new(fake_engine(), fake_disc(&dir), timeout)
        .await
        .expect("fake engine starts")
}

/// A replay file in a fresh directory, named for the fake engine's behaviour.
fn replay(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    replay_with(dir, name, b"not a real replay")
}

fn replay_with(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.path().join(format!("{name}.slp"));
    std::fs::write(&p, bytes).unwrap();
    p
}

/// A readable two-player replay, with or without a Gecko code list.
fn built_replay(gecko: bool) -> Vec<u8> {
    let bytes = SlpBuilder::new()
        .gecko(gecko)
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(-123, 1, 4, 0.0)
        .frame(-123, 2, 4, 0.0)
        .frame(4000, 1, 2, 63.0)
        .frame(4000, 2, 0, 0.0)
        .end(2, None)
        .build();
    let facts = replay::parse(&bytes).expect("the built replay parses");
    assert_eq!(facts.has_gecko, gecko);
    bytes
}

/// A CSV with one row whose `status` is `status`.
fn status_csv(status: &str) -> String {
    format!("{HEADER}\nf.slp,{status},,,,,,,,,,,,,,,,,,,,,\n")
}

#[test]
fn csv_match() {
    let csv = format!(
        "{HEADER}\nnetplay_host.slp,match,3.18.0,32,p1:0+p2:2,11/11,0,0,0,none,3098,6196,2974,,,,,,,,,\"a, b; c\",\n"
    );
    assert_eq!(outcome_from_csv(&csv, 2974), ResimOutcome::Match);
}

#[test]
fn csv_diverged_fields() {
    let csv = format!(
        "{HEADER}\nf.slp,diverged,3.18.0,32,p1:0+p2:2,11/11,0,0,0,none,3098,1500,797,797,2,x,12.5,13.0,12.5,0,,notes,\n"
    );
    assert_eq!(
        outcome_from_csv(&csv, 2974),
        ResimOutcome::Diverged {
            frame: 797,
            port: 2,
            field: "x".into(),
            engine_value: "12.5".into(),
            replay_value: "13.0".into(),
        }
    );
}

#[test]
fn csv_early_end() {
    let csv = format!(
        "{HEADER}\nf.slp,match,3.18.0,32,p1:0+p2:2,11/11,0,0,0,none,5124,2000,1000,,,,,,,,,,\n"
    );
    assert_eq!(
        outcome_from_csv(&csv, 5000),
        ResimOutcome::EarlyEnd {
            engine_last: 1000,
            replay_last: 5000
        }
    );
}

#[test]
fn csv_panic_status_is_crashed() {
    let csv = format!(
        "{HEADER}\nf.slp,panic@412,3.18.0,32,p1:0+p2:2,11/11,0,0,0,none,5124,824,412,,,,,,,,boom,,\n"
    );
    assert_eq!(
        outcome_from_csv(&csv, 5000),
        ResimOutcome::Crashed("panic@412".into())
    );
}

#[test]
fn csv_refused_is_crashed() {
    // The engine will not simulate this replay: a fact about the file, not the verifier.
    let refused = format!(
        "{HEADER}\nf.slp,refused: gecko codes,3.18.0,32,p1:0+p2:2,11/11,0,0,3,abc,5124,0,,,,,,,,,,,\n"
    );
    assert_eq!(
        outcome_from_csv(&refused, 5000),
        ResimOutcome::Crashed("refused: gecko codes".into())
    );
    assert_eq!(
        outcome_from_csv(&status_csv("refused: p1: team 161 is outside 0..=2"), 5000),
        ResimOutcome::Crashed("refused: p1: team 161 is outside 0..=2".into())
    );
}

#[test]
fn csv_parse_error_is_crashed() {
    // The engine cannot read this replay: a fact about the file, not the verifier.
    assert_eq!(
        outcome_from_csv(
            &status_csv("parse-error: the first event is not Event Payloads (0x35)"),
            5000
        ),
        ResimOutcome::Crashed("parse-error: the first event is not Event Payloads (0x35)".into())
    );
}

#[test]
fn csv_boot_error_is_unavailable() {
    // The engine could not boot the disc image: the verifier is broken, the replay is not.
    let status = "boot-error: <disc>: No such file or directory (os error 2)";
    assert_eq!(
        outcome_from_csv(&status_csv(status), 5000),
        ResimOutcome::Unavailable(status.into())
    );
    assert_eq!(
        outcome_from_csv(&status_csv("boot-error"), 5000),
        ResimOutcome::Unavailable("boot-error".into())
    );
    assert_eq!(
        outcome_from_csv(&status_csv("boot-error disc unreadable"), 5000),
        ResimOutcome::Unavailable("boot-error disc unreadable".into())
    );
}

#[test]
fn csv_unknown_status_is_crashed_with_its_text() {
    assert_eq!(
        outcome_from_csv(&status_csv("something-new: details here"), 5000),
        ResimOutcome::Crashed("something-new: details here".into())
    );
    // A word that only starts like a known one is still unknown.
    assert_eq!(
        outcome_from_csv(&status_csv("boot-errors: x"), 5000),
        ResimOutcome::Crashed("boot-errors: x".into())
    );
}

#[test]
fn csv_columns_found_by_name_not_position() {
    let csv =
        "replay_value,first_field,notes,last_frame,engine_value,first_port,status,first_frame\n\
               13.0,y,\"quoted, with commas\",900,12.5,1,diverged,801\n";
    assert_eq!(
        outcome_from_csv(csv, 5000),
        ResimOutcome::Diverged {
            frame: 801,
            port: 1,
            field: "y".into(),
            engine_value: "12.5".into(),
            replay_value: "13.0".into(),
        }
    );
}

#[test]
fn csv_uses_last_row_and_tolerates_unquoted_trailing_commas() {
    let csv = format!(
        "{HEADER}\nold.slp,diverged,3.18.0,32,p,11/11,0,0,0,none,1,1,1,1,1,x,1,2,,,,,\n\
         new.slp,match,3.18.0,32,p,11/11,0,0,0,none,3098,6196,2974,,,,,,,,,note, with, commas,\r\n"
    );
    assert_eq!(outcome_from_csv(&csv, 2974), ResimOutcome::Match);
}

#[test]
fn csv_without_rows_is_crashed() {
    assert!(matches!(
        outcome_from_csv(HEADER, 2974),
        ResimOutcome::Crashed(_)
    ));
    assert!(matches!(
        outcome_from_csv("", 2974),
        ResimOutcome::Crashed(_)
    ));
}

#[test]
fn csv_match_with_unreadable_last_frame_is_crashed() {
    let csv = "status,last_frame\nmatch,\n";
    assert!(matches!(
        outcome_from_csv(csv, 2974),
        ResimOutcome::Crashed(_)
    ));
}

#[tokio::test]
async fn run_match() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    assert_eq!(
        engine.check(&replay(&dir, "match"), 5000).await,
        ResimOutcome::Match
    );
}

#[tokio::test]
async fn run_early_end() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    assert_eq!(
        engine.check(&replay(&dir, "early"), 5000).await,
        ResimOutcome::EarlyEnd {
            engine_last: 1000,
            replay_last: 5000
        }
    );
}

#[tokio::test]
async fn run_diverged_despite_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    assert_eq!(
        engine.check(&replay(&dir, "diverged"), 5000).await,
        ResimOutcome::Diverged {
            frame: 797,
            port: 2,
            field: "x".into(),
            engine_value: "12.5".into(),
            replay_value: "13.0".into(),
        }
    );
}

#[tokio::test]
async fn run_panic_is_crashed() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    match engine.check(&replay(&dir, "panic"), 5000).await {
        ResimOutcome::Crashed(why) => assert!(why.contains("134"), "{why}"),
        other => panic!("expected Crashed, got {other:?}"),
    }
}

/// The real engine's answer to a disc it cannot open: a `boot-error:` row and exit 1.
#[tokio::test]
async fn run_missing_disc_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    match engine.check(&replay(&dir, "nodisc"), 5000).await {
        ResimOutcome::Unavailable(why) => assert!(why.starts_with("boot-error: "), "{why}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

/// Exit 2 (a usage error or an unreadable `.slp`), with no CSV row.
#[tokio::test]
async fn run_exit_2_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    match engine.check(&replay(&dir, "usage"), 5000).await {
        ResimOutcome::Unavailable(why) => {
            assert!(why.contains("cannot read the replay"), "{why}")
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn run_timeout_is_crashed() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(1)).await;
    let start = Instant::now();
    let outcome = engine.check(&replay(&dir, "hang"), 5000).await;
    assert_eq!(outcome, ResimOutcome::Crashed("timeout".into()));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn run_concurrent_checks_do_not_share_csv() {
    let dir = tempfile::tempdir().unwrap();
    let engine = std::sync::Arc::new(fake(Duration::from_secs(10)).await);
    let mut tasks = Vec::new();
    for i in 0..8 {
        let engine = engine.clone();
        let slp = replay(&dir, if i % 2 == 0 { "match" } else { "diverged" });
        tasks.push(tokio::spawn(
            async move { (i, engine.check(&slp, 5000).await) },
        ));
    }
    for t in tasks {
        let (i, outcome) = t.await.unwrap();
        if i % 2 == 0 {
            assert_eq!(outcome, ResimOutcome::Match);
        } else {
            assert!(
                matches!(outcome, ResimOutcome::Diverged { .. }),
                "{outcome:?}"
            );
        }
    }
}

/// A replay without a Gecko list (this engine's own export) runs with the variable set.
#[tokio::test]
async fn env_var_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let slp = replay_with(&dir, "envcheck", &built_replay(false));
    // The script really refuses without the variable...
    let csv = dir.path().join("direct.csv");
    let status = std::process::Command::new(fake_engine())
        .args(["slpcheck", "--slp"])
        .arg(&slp)
        .args(["--iso", "x.iso", "--csv"])
        .arg(&csv)
        .env_remove("SLPCHECK_VANILLA_SPAWN")
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(2));
    // ...and Slpcheck sets it.
    let engine = fake(Duration::from_secs(10)).await;
    assert_eq!(engine.check(&slp, 5000).await, ResimOutcome::Match);
}

/// A replay with a Gecko list (Slippi's replay rules apply) runs without the variable.
#[tokio::test]
async fn env_var_is_unset_for_gecko_replays() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    // The modes discriminate: each refuses the other kind of replay.
    assert!(matches!(
        engine
            .check(&replay_with(&dir, "envcheck", &built_replay(true)), 5000)
            .await,
        ResimOutcome::Unavailable(_)
    ));
    let slp = replay_with(&dir, "noenvcheck", &built_replay(true));
    assert_eq!(engine.check(&slp, 5000).await, ResimOutcome::Match);
}

/// A file our reader cannot parse still runs, without the variable (the engine's default).
#[tokio::test]
async fn unparseable_replay_runs_without_env_var() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    let slp = replay(&dir, "noenvcheck");
    assert!(replay::parse(&std::fs::read(&slp).unwrap()).is_err());
    assert_eq!(engine.check(&slp, 5000).await, ResimOutcome::Match);
}

#[tokio::test]
async fn build_string() {
    let engine = fake(Duration::from_secs(10)).await;
    let build = engine.build();
    let hash = build.strip_prefix("fake 0.0+").expect(&build);
    assert_eq!(hash.len(), 12, "{build}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{build}");
}

#[tokio::test]
async fn missing_engine_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let r = Slpcheck::new(
        PathBuf::from("/no/such/engine"),
        fake_disc(&dir),
        Duration::from_secs(1),
    )
    .await;
    let err = format!("{:#}", r.err().expect("a missing engine is refused"));
    assert!(err.contains("/no/such/engine"), "{err}");
}

#[tokio::test]
async fn non_executable_engine_fails_to_start() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let engine = dir.path().join("melee");
    std::fs::copy(fake_engine(), &engine).unwrap();
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o644)).unwrap();
    let r = Slpcheck::new(engine.clone(), fake_disc(&dir), Duration::from_secs(1)).await;
    let err = format!("{:#}", r.err().expect("a non-executable engine is refused"));
    assert!(err.contains(&engine.display().to_string()), "{err}");
}

#[tokio::test]
async fn missing_disc_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let disc = dir.path().join("no-such-disc.iso");
    let r = Slpcheck::new(fake_engine(), disc.clone(), Duration::from_secs(1)).await;
    let err = format!("{:#}", r.err().expect("a missing disc is refused"));
    assert!(err.contains(&disc.display().to_string()), "{err}");
}

#[tokio::test]
async fn directory_disc_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let r = Slpcheck::new(
        fake_engine(),
        dir.path().to_path_buf(),
        Duration::from_secs(1),
    )
    .await;
    let err = format!("{:#}", r.err().expect("a directory is not a disc image"));
    assert!(err.contains(&dir.path().display().to_string()), "{err}");
}

#[tokio::test]
async fn unreadable_disc_fails_to_start() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let disc = fake_disc(&dir);
    std::fs::set_permissions(&disc, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&disc).is_ok() {
        // Running as root: permissions do not stop the read, so there is nothing to test.
        return;
    }
    let r = Slpcheck::new(fake_engine(), disc.clone(), Duration::from_secs(1)).await;
    let err = format!("{:#}", r.err().expect("an unreadable disc is refused"));
    assert!(err.contains(&disc.display().to_string()), "{err}");
}

#[tokio::test]
#[ignore = "needs the real engine, disc image and replay: set AB_ENGINE, AB_ISO, AB_FIXTURE_A"]
async fn real_engine_match() {
    let var = |k: &str| PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("{k} not set")));
    let engine = Slpcheck::new(var("AB_ENGINE"), var("AB_ISO"), Duration::from_secs(300))
        .await
        .expect("engine starts");
    println!("build: {}", engine.build());
    assert_eq!(
        engine.check(&var("AB_FIXTURE_A"), 2974).await,
        ResimOutcome::Match
    );
}

#[tokio::test]
async fn small_disc_fails_to_start() {
    for len in [0, 6, MIN_DISC_BYTES - 1] {
        let dir = tempfile::tempdir().unwrap();
        let disc = sparse_disc(&dir, len);
        let r = Slpcheck::new(fake_engine(), disc.clone(), Duration::from_secs(1)).await;
        let err = format!("{:#}", r.err().expect("a too-small disc is refused"));
        assert!(err.contains(&disc.display().to_string()), "{err}");
        assert!(err.contains(&format!("{len} bytes")), "{err}");
    }
}

#[tokio::test]
async fn disc_of_minimum_size_starts() {
    let dir = tempfile::tempdir().unwrap();
    let disc = sparse_disc(&dir, MIN_DISC_BYTES);
    Slpcheck::new(fake_engine(), disc, Duration::from_secs(1))
        .await
        .expect("a disc of exactly MIN_DISC_BYTES is accepted");
}

/// An engine that cannot be started any more (the binary was removed after start-up) is an
/// internal error for the judging worker, which retries the game; it is not "unavailable",
/// which could count the game unchecked.
#[tokio::test]
async fn engine_gone_after_start_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    // A symlink, not a copy: executing a file just written can fail with "text file busy"
    // while another test forks.
    let engine = dir.path().join("melee");
    std::os::unix::fs::symlink(fake_engine(), &engine).unwrap();
    let checker = Slpcheck::new(engine.clone(), fake_disc(&dir), Duration::from_secs(10))
        .await
        .unwrap();
    std::fs::remove_file(&engine).unwrap();
    let slp = replay(&dir, "match");
    let err = checker
        .check_with_build(&slp, 5000)
        .await
        .expect_err("the engine cannot start");
    assert!(format!("{err:#}").contains("engine"), "{err:#}");
    // Through `check` (what an external verify-worker reports) it is the verifier's fault.
    assert!(matches!(
        checker.check(&slp, 5000).await,
        ResimOutcome::Unavailable(_)
    ));
}

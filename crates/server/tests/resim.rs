use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use autobracket_server::resim::{outcome_from_csv, Resim, Slpcheck};
use proto::ResimOutcome;

const HEADER: &str = "file,status,slippi,stage,roster,ucf_dash_shield,frozen_ps,pal,gecko_codes,gecko_hash,replay_frames,frames_compared,last_frame,first_frame,first_port,first_field,engine_value,replay_value,fd_x,fd_y,panic,notes,seed_first";

fn fake_engine() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_slpcheck.sh")
}

async fn fake(timeout: Duration) -> Slpcheck {
    Slpcheck::new(fake_engine(), PathBuf::from("/no/such/disc.iso"), timeout)
        .await
        .expect("fake engine starts")
}

/// A replay file in a fresh directory, named for the fake engine's behaviour.
fn replay(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    let p = dir.path().join(format!("{name}.slp"));
    std::fs::write(&p, b"not a real replay").unwrap();
    p
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
    let refused = format!(
        "{HEADER}\nf.slp,refused: gecko codes,3.18.0,32,p1:0+p2:2,11/11,0,0,3,abc,5124,0,,,,,,,,,,,\n"
    );
    assert_eq!(
        outcome_from_csv(&refused, 5000),
        ResimOutcome::Crashed("refused: gecko codes".into())
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

#[tokio::test]
async fn run_missing_disc_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let engine = fake(Duration::from_secs(10)).await;
    match engine.check(&replay(&dir, "nodisc"), 5000).await {
        ResimOutcome::Unavailable(why) => assert!(why.contains("disc image not found"), "{why}"),
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

#[tokio::test]
async fn env_var_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let slp = replay(&dir, "envcheck");
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
    let r = Slpcheck::new(
        PathBuf::from("/no/such/engine"),
        PathBuf::from("/no/such/disc.iso"),
        Duration::from_secs(1),
    )
    .await;
    assert!(r.is_err());
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

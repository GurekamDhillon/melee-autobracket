//! Checks against the real engine and a corpus of real replay files.
//!
//! Every test is `#[ignore]`: they need the engine binary, a disc image and the replay corpus,
//! none of which live in this repository. Run them with
//!
//! ```text
//! AB_ENGINE=/path/to/melee AB_ISO=/path/to/disc.iso AB_FIXTURES=/path/to/autobracket-fixtures \
//!   cargo test -p autobracket-server --test real_engine -- --ignored --nocapture --test-threads=2
//! ```
//!
//! `AB_FIXTURES` is the corpus directory: the one holding `MANIFEST.txt` and the folders
//! `own-netplay`, `own-offline`, `dolphin`, `tournament`, `edge` and `synthetic`.
//! Results and findings are written up in `docs/real-engine.md`.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use autobracket_server::judge::{finish, legality, pre_judge, Pre};
use autobracket_server::resim::{outcome_from_csv, Resim, Slpcheck, EARLY_END_SLACK_FRAMES};
use proto::{ResimOutcome, Ruleset, Verdict};
use replay::{GameEnd, PlayerKind, ReplayFacts};

/// Generous: the longest corpus game (21639 frames) takes well under a minute.
const SLPCHECK_TIMEOUT: Duration = Duration::from_secs(300);
/// The variable the server sets for every `slpcheck` run (`resim.rs`).
const SPAWN_ENV: &str = "SLPCHECK_VANILLA_SPAWN";

// ---------------------------------------------------------------------------------------------
// Environment and corpus

struct Env {
    engine: PathBuf,
    iso: PathBuf,
    fixtures: PathBuf,
}

fn var(name: &str, what: &str) -> PathBuf {
    match std::env::var_os(name) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => panic!("{name} is not set: it must name {what} (see the header of real_engine.rs)"),
    }
}

fn env() -> Env {
    let env = Env {
        engine: var("AB_ENGINE", "the engine binary (`melee`)"),
        iso: var("AB_ISO", "the Melee 1.02 NTSC disc image"),
        fixtures: var(
            "AB_FIXTURES",
            "the replay corpus directory (holding MANIFEST.txt)",
        ),
    };
    for (name, path) in [
        ("AB_ENGINE", &env.engine),
        ("AB_ISO", &env.iso),
        ("AB_FIXTURES", &env.fixtures),
    ] {
        assert!(path.exists(), "{name}={} does not exist", path.display());
    }
    env
}

async fn slpcheck(env: &Env) -> Slpcheck {
    Slpcheck::new(env.engine.clone(), env.iso.clone(), SLPCHECK_TIMEOUT)
        .await
        .expect("the engine runs `version`")
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// One line of `MANIFEST.txt` (tab-separated: relpath, sha256, source, slpcheck_status, notes).
struct Entry {
    relpath: String,
    status: String,
}

impl Entry {
    /// Whether the engine authors measured this file as `match`.
    fn is_match(&self) -> bool {
        self.status.starts_with("match")
    }
}

fn manifest(env: &Env) -> Vec<Entry> {
    let path = env.fixtures.join("MANIFEST.txt");
    let text = String::from_utf8(read(&path)).expect("MANIFEST.txt is UTF-8");
    let entries: Vec<Entry> = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            assert!(cols.len() >= 4, "short MANIFEST line: {l}");
            Entry {
                relpath: cols[0].to_string(),
                status: cols[3].to_string(),
            }
        })
        .collect();
    assert!(!entries.is_empty(), "MANIFEST.txt lists no files");
    entries
}

/// Host/guest export pairs in `own-netplay`: every `*host.slp` with a `*guest.slp` beside it.
fn netplay_pairs(env: &Env) -> Vec<(PathBuf, PathBuf)> {
    let dir = env.fixtures.join("own-netplay");
    let mut pairs = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("own-netplay is readable") {
        let host = entry.expect("own-netplay entry").path();
        let name = host.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(stem) = name.strip_suffix("host.slp") {
            let guest = dir.join(format!("{stem}guest.slp"));
            if guest.exists() {
                pairs.push((host, guest));
            }
        }
    }
    pairs.sort();
    pairs
}

/// A ruleset under which this game is legal: its own stocks, timer and stage.
fn ruleset_for(f: &ReplayFacts) -> Ruleset {
    Ruleset {
        stocks: f.players.first().map_or(4, |p| p.start_stocks),
        timer_secs: f.timer_secs,
        legal_stages: vec![f.stage],
        ..Ruleset::default()
    }
}

// ---------------------------------------------------------------------------------------------
// Running the engine directly (to read the CSV and to try other settings)

/// One direct `slpcheck` run.
struct EngineRun {
    exit: Option<i32>,
    /// The whole CSV the engine wrote (header and one row), empty if none.
    csv: String,
    secs: f64,
}

impl EngineRun {
    fn col(&self, name: &str) -> Option<String> {
        let records = parse_csv(&self.csv);
        let (header, row) = (records.first()?, records.last()?);
        let i = header.iter().position(|h| h.trim() == name)?;
        row.get(i).map(|v| v.trim().to_string())
    }

    fn status(&self) -> String {
        self.col("status")
            .unwrap_or_else(|| format!("no CSV row (exit {:?})", self.exit))
    }

    fn last_frame(&self) -> Option<i32> {
        self.col("last_frame")?.parse().ok()
    }

    /// `diverged@frame:pN:field`, or the plain status.
    fn summary(&self) -> String {
        let status = self.status();
        if status == "diverged" {
            format!(
                "diverged@{}:p{}:{}",
                self.col("first_frame").unwrap_or_default(),
                self.col("first_port").unwrap_or_default(),
                self.col("first_field").unwrap_or_default()
            )
        } else {
            status
        }
    }
}

type RunCell = Arc<OnceLock<Arc<EngineRun>>>;

/// Runs `slpcheck` on `file` exactly as the server does (`vanilla_spawn = true`) or without the
/// spawn variable. Runs are cached for the life of the test binary, so tests can share them.
fn engine_run(env: &Env, file: &Path, vanilla_spawn: bool) -> Arc<EngineRun> {
    static CACHE: OnceLock<Mutex<HashMap<(PathBuf, bool), RunCell>>> = OnceLock::new();
    let cell = {
        let mut map = CACHE
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.entry((file.to_path_buf(), vanilla_spawn))
            .or_default()
            .clone()
    };
    cell.get_or_init(|| Arc::new(run_engine(env, file, vanilla_spawn)))
        .clone()
}

fn run_engine(env: &Env, file: &Path, vanilla_spawn: bool) -> EngineRun {
    let dir = tempfile::tempdir().expect("temp dir");
    let csv_path = dir.path().join("slpcheck.csv");
    let mut cmd = Command::new(&env.engine);
    cmd.arg("slpcheck")
        .arg("--slp")
        .arg(file)
        .arg("--iso")
        .arg(&env.iso)
        .arg("--csv")
        .arg(&csv_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if vanilla_spawn {
        cmd.env(SPAWN_ENV, "1");
    } else {
        cmd.env_remove(SPAWN_ENV);
    }
    let start = Instant::now();
    let status = cmd.status().expect("the engine starts");
    let secs = start.elapsed().as_secs_f64();
    let csv = std::fs::read(&csv_path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    EngineRun {
        exit: status.code(),
        csv,
        secs,
    }
}

/// Splits CSV text into records; double-quoted fields may hold commas, newlines and `""`.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (true, _) => field.push(c),
            (false, '"') => quoted = true,
            (false, ',') => record.push(std::mem::take(&mut field)),
            (false, '\n') => {
                record.push(std::mem::take(&mut field));
                if !(record.len() == 1 && record[0].is_empty()) {
                    records.push(std::mem::take(&mut record));
                }
                record.clear();
            }
            (false, '\r') => {}
            (false, _) => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

// ---------------------------------------------------------------------------------------------
// Reporting helpers

fn outcome_word(o: &ResimOutcome) -> String {
    match o {
        ResimOutcome::Match => "Match".into(),
        ResimOutcome::Diverged {
            frame, port, field, ..
        } => format!("Diverged@{frame}:p{port}:{field}"),
        ResimOutcome::EarlyEnd {
            engine_last,
            replay_last,
        } => format!("EarlyEnd({engine_last}<{replay_last})"),
        ResimOutcome::Crashed(s) => format!("Crashed({})", first_line(s)),
        ResimOutcome::Unavailable(s) => format!("Unavailable({})", first_line(s)),
        ResimOutcome::Skipped => "Skipped".into(),
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn players(f: &ReplayFacts) -> String {
    f.players
        .iter()
        .map(|p| {
            let kind = match p.kind {
                PlayerKind::Human => "hu",
                PlayerKind::Cpu => "cpu",
                PlayerKind::Demo => "demo",
                PlayerKind::Empty => "empty",
            };
            format!("p{}:c{}:{kind}:{}st", p.port, p.character, p.start_stocks)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn end_word(end: &Option<GameEnd>) -> String {
    match end {
        None => "none".into(),
        Some(GameEnd {
            method,
            lras_initiator: Some(p),
        }) => format!("{method} lras=p{p}"),
        Some(GameEnd { method, .. }) => format!("{method}"),
    }
}

/// (min, median, max) of a non-empty list.
fn spread<T: Copy + PartialOrd>(values: &[T]) -> (T, T, T) {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).expect("comparable"));
    (v[0], v[v.len() / 2], v[v.len() - 1])
}

// ---------------------------------------------------------------------------------------------
// Tests

#[test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
fn fixture_pairs_raw_elements_equal() {
    let env = env();
    let pairs = netplay_pairs(&env);
    assert!(
        pairs.len() >= 2,
        "expected at least the plain and the lag/loss pair in own-netplay, found {pairs:?}"
    );
    for (host, guest) in &pairs {
        let (h, g) = (read(host), read(guest));
        let rh = replay::raw_element(&h).expect("host raw element");
        let rg = replay::raw_element(&g).expect("guest raw element");
        assert!(
            rh == rg,
            "{} and {} have different raw elements",
            host.display(),
            guest.display()
        );
        println!(
            "{} == {}: raw {} bytes, whole files {}",
            host.display(),
            guest.display(),
            rh.len(),
            if h == g { "identical" } else { "differ" }
        );
    }
}

#[test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
fn fixture_parses_with_expected_facts() {
    let env = env();
    let f = replay::parse(&read(&env.fixtures.join("own-netplay/netplay_host.slp")))
        .expect("netplay_host.slp parses");
    println!("{f:#?}");
    let summary: Vec<(u8, u8, PlayerKind, u8)> = f
        .players
        .iter()
        .map(|p| (p.port, p.character, p.kind, p.start_stocks))
        .collect();
    assert_eq!(
        summary,
        vec![(1, 0, PlayerKind::Human, 4), (2, 2, PlayerKind::Human, 4)]
    );
    assert_eq!(f.stage, 32);
    assert_eq!(f.last_frame, 2974);
    assert_eq!(replay::winner_port(&f), Some(1));
}

#[tokio::test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
async fn slpcheck_says_match_on_own_build_exports() {
    let env = env();
    let checker = slpcheck(&env).await;
    println!("engine build: {}", checker.build());
    let own: Vec<Entry> = manifest(&env)
        .into_iter()
        .filter(|e| e.relpath.starts_with("own-netplay/") && e.is_match())
        .collect();
    assert!(
        own.len() >= 5,
        "the manifest lists {} own-netplay files",
        own.len()
    );
    let mut wrong = Vec::new();
    for e in &own {
        let path = env.fixtures.join(&e.relpath);
        let facts = replay::parse(&read(&path)).expect("own export parses");
        let start = Instant::now();
        let outcome = checker.check(&path, facts.last_frame).await;
        println!(
            "{:<55} last_frame {:>5}  {:<10} {:.2}s",
            e.relpath,
            facts.last_frame,
            outcome_word(&outcome),
            start.elapsed().as_secs_f64()
        );
        if outcome != ResimOutcome::Match {
            wrong.push(format!("{}: {outcome:?}", e.relpath));
        }
    }
    assert!(wrong.is_empty(), "not Match: {wrong:#?}");
}

#[tokio::test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
async fn pre_judge_then_finish_is_verified() {
    let env = env();
    let checker = slpcheck(&env).await;
    let pairs = netplay_pairs(&env);
    assert!(pairs.len() >= 2, "own-netplay pairs: {pairs:?}");
    for (host, guest) in &pairs {
        let (h, g) = (read(host), read(guest));
        let facts = replay::parse(&h).expect("host export parses");
        let rules = ruleset_for(&facts);
        let pre = pre_judge([Some(&h), Some(&g)], &rules);
        let Pre::NeedsResim {
            winner_port,
            last_frame,
        } = pre
        else {
            panic!("{}: pre_judge gave {pre:?}", host.display());
        };
        assert_eq!(last_frame, facts.last_frame);
        let outcome = checker.check(host, last_frame).await;
        let verdict = finish(&outcome, false);
        println!(
            "{}: winner p{winner_port}, last_frame {last_frame}, {} -> {verdict:?}",
            host.display(),
            outcome_word(&outcome)
        );
        assert_eq!(verdict, Verdict::Verified, "{}", host.display());
    }
}

#[tokio::test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
async fn disconnect_host_only_is_single() {
    let env = env();
    let path = env
        .fixtures
        .join("own-netplay/falcon_fox_fd_guest_quit_25s_host.slp");
    let bytes = read(&path);
    assert_eq!(
        pre_judge([Some(&bytes), None], &Ruleset::default()),
        Pre::Final(Verdict::Single)
    );

    // Recorded, not asserted: what the reader and the engine make of the file.
    let facts = replay::parse(&bytes).expect("the host export parses");
    let rules = ruleset_for(&facts);
    println!("facts: {facts:#?}");
    println!(
        "Game End: {}; winner_port: {:?}; finals: {:?}",
        end_word(&facts.end),
        replay::winner_port(&facts),
        facts.finals
    );
    println!(
        "if both peers had uploaded this file (own ruleset): {:?}",
        pre_judge([Some(&bytes), Some(&bytes)], &rules)
    );
    let outcome = slpcheck(&env).await.check(&path, facts.last_frame).await;
    println!("slpcheck: {}", outcome_word(&outcome));
}

#[tokio::test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
async fn corpus_sweep_never_panics_and_agrees_with_manifest() {
    let env = env();
    let checker = slpcheck(&env).await;
    let entries = manifest(&env);
    let sweep_start = Instant::now();

    let mut rows = Vec::new();
    let mut facts_rows = Vec::new();
    let mut panics = Vec::new();
    let mut disagreements = Vec::new();
    let mut check_secs = Vec::new();
    let mut full_secs = Vec::new();

    for e in &entries {
        let path = env.fixtures.join(&e.relpath);
        let bytes = read(&path);
        let parsed = catch_unwind(AssertUnwindSafe(|| replay::parse(&bytes)));
        let (parse_word, ours, secs) = match parsed {
            Err(_) => {
                panics.push(format!("{}: replay::parse panicked", e.relpath));
                ("PANIC".to_string(), "-".to_string(), 0.0)
            }
            Ok(Err(err)) => {
                // The server never re-simulates a file it cannot read; ask the engine anyway.
                let run = engine_run(&env, &path, true);
                (
                    format!("Err({err:?})"),
                    format!("(engine: {})", run.summary()),
                    run.secs,
                )
            }
            Ok(Ok(f)) => {
                let start = Instant::now();
                let outcome = checker.check(&path, f.last_frame).await;
                let secs = start.elapsed().as_secs_f64();
                check_secs.push(secs);
                if let ResimOutcome::Crashed(s) = &outcome {
                    if s.starts_with("panic") {
                        panics.push(format!("{}: engine {s}", e.relpath));
                    }
                }
                if e.is_match() && outcome != ResimOutcome::Match {
                    disagreements.push(format!(
                        "{}: manifest `{}`, Slpcheck::check gave {}",
                        e.relpath,
                        e.status,
                        outcome_word(&outcome)
                    ));
                }
                let pre = pre_judge([Some(&bytes), Some(&bytes)], &Ruleset::default());
                facts_rows.push(format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {:?} | {} | {:?} |",
                    e.relpath,
                    players(&f),
                    f.stage,
                    f.timer_secs,
                    f.last_frame,
                    if f.has_gecko { "yes" } else { "no" },
                    end_word(&f.end),
                    replay::winner_port(&f),
                    legality(&f, &Ruleset::default()).err().unwrap_or("ok"),
                    pre
                ));
                (
                    format!("Ok({})", f.last_frame),
                    outcome_word(&outcome),
                    secs,
                )
            }
        };
        // The same file without the spawn variable, as the engine authors measured the corpus.
        let plain = engine_run(&env, &path, false);
        if plain.status() == "match" {
            full_secs.push(plain.secs);
        }
        let row = format!(
            "| {} | {} | {} ({secs:.2}s) | {} ({:.2}s) | {} |",
            e.relpath,
            parse_word,
            ours,
            plain.summary(),
            plain.secs,
            e.status
        );
        println!("{row}");
        rows.push(row);
    }

    println!("\n## Sweep ({} files)\n", entries.len());
    println!("| file | replay::parse | Slpcheck::check (server: {SPAWN_ENV}=1) | engine without {SPAWN_ENV} | manifest status |");
    println!("|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }
    println!("\n## Facts of the readable files\n");
    println!("| file | players | stage | timer | last_frame | gecko | Game End | winner_port | legality (default rules) | pre_judge (pair of itself, default rules) |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for r in &facts_rows {
        println!("{r}");
    }
    println!(
        "\nGecko list present in {} of {} readable files.",
        facts_rows.iter().filter(|r| r.contains("| yes |")).count(),
        facts_rows.len()
    );
    if !check_secs.is_empty() {
        let (lo, mid, hi) = spread(&check_secs);
        println!(
            "Slpcheck::check wall clock over {} runs: min {lo:.2}s median {mid:.2}s max {hi:.2}s",
            check_secs.len()
        );
    }
    if !full_secs.is_empty() {
        let (lo, mid, hi) = spread(&full_secs);
        println!(
            "full-length `match` runs without {SPAWN_ENV}, {} runs: min {lo:.2}s median {mid:.2}s max {hi:.2}s, total {:.1}s",
            full_secs.len(),
            full_secs.iter().sum::<f64>()
        );
    }
    println!("whole sweep: {:.1}s", sweep_start.elapsed().as_secs_f64());

    assert!(panics.is_empty(), "panics: {panics:#?}");
    assert!(
        disagreements.is_empty(),
        "{} file(s) the manifest lists as `match` did not come back Match:\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
}

/// A disc image that cannot be read is the verifier's problem, not the replay's: the game must
/// fall back to `PeerOnly` (via `Unavailable`), not count as `VerifiedInexact` (via `Crashed`).
#[tokio::test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
async fn missing_disc_is_unavailable() {
    let env = env();
    let dir = tempfile::tempdir().expect("temp dir");
    let checker = Slpcheck::new(
        env.engine.clone(),
        dir.path().join("no-such-disc.iso"),
        SLPCHECK_TIMEOUT,
    )
    .await
    .expect("the engine runs `version`");
    let path = env.fixtures.join("own-netplay/netplay_host.slp");
    let outcome = checker.check(&path, 2974).await;
    println!(
        "missing disc: {} -> {:?}",
        outcome_word(&outcome),
        finish(&outcome, false)
    );
    assert!(
        matches!(outcome, ResimOutcome::Unavailable(_)),
        "a missing disc image gave {outcome:?}, which `finish` turns into {:?}",
        finish(&outcome, false)
    );
}

#[test]
#[ignore = "needs AB_ENGINE, AB_ISO and AB_FIXTURES"]
fn early_end_slack_is_calibrated() {
    let env = env();
    // (file, replay last frame, engine last frame) for runs the server would read as Match or
    // EarlyEnd, and separately for every complete `match` run without the spawn variable.
    let mut server = Vec::new();
    let mut plain = Vec::new();
    for e in manifest(&env) {
        let path = env.fixtures.join(&e.relpath);
        let Ok(f) = replay::parse(&read(&path)) else {
            continue;
        };
        let run = engine_run(&env, &path, true);
        if matches!(
            outcome_from_csv(&run.csv, f.last_frame),
            ResimOutcome::Match | ResimOutcome::EarlyEnd { .. }
        ) {
            let engine_last = run.last_frame().expect("a match row has last_frame");
            server.push((e.relpath.clone(), f.last_frame, engine_last));
        }
        let run = engine_run(&env, &path, false);
        if run.status() == "match" {
            let engine_last = run.last_frame().expect("a match row has last_frame");
            plain.push((e.relpath.clone(), f.last_frame, engine_last));
        }
    }

    let mut bad = Vec::new();
    for (label, set) in [
        (format!("server runs ({SPAWN_ENV}=1)"), &server),
        (format!("runs without {SPAWN_ENV}"), &plain),
    ] {
        assert!(
            !set.is_empty(),
            "{label}: no Match/EarlyEnd runs to measure"
        );
        let gaps: Vec<i32> = set.iter().map(|(_, r, c)| r - c).collect();
        let (lo, mid, hi) = spread(&gaps);
        println!(
            "{label}: {} files, replay_last_frame - csv_last_frame min {lo} median {mid} max {hi}",
            set.len()
        );
        for (file, r, c) in set.iter() {
            let gap = r - c;
            if !(0..=EARLY_END_SLACK_FRAMES).contains(&gap) {
                bad.push(format!(
                    "{label}: {file}: replay {r}, engine {c}, gap {gap}"
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "gaps outside 0..={EARLY_END_SLACK_FRAMES}:\n{}",
        bad.join("\n")
    );
}

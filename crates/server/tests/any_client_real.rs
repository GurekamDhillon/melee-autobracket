//! Any-client mode on real Slippi Dolphin replays (never committed; point at your own).
//!
//! `#[ignore]`d: it needs a directory of `.slp` files recorded by Slippi Dolphin:
//!
//! AB_SLP_DIR=/path/to/slp-folder [AB_SLP_LIMIT=40] [AB_ENGINE=/path/to/melee AB_ISO=/path/to/disc.iso] \
//!   cargo test -p autobracket-server --test any_client_real -- --ignored --nocapture
//!
//! For each legal two-player game it checks: the file paired with a copy whose bytes differ only
//! outside the outcome (one pre-frame byte) is `Conflicted` with any-client off and agrees with it
//! on; two different games never agree. With `AB_ENGINE` and `AB_ISO` it also re-simulates each
//! file with the real `melee slpcheck` and reports how many are `Verified` and how many would
//! count as `UnverifiedForeign`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use autobracket_server::judge::{finish, outcome, pre_judge, Pre};
use autobracket_server::Slpcheck;
use proto::{Ruleset, Verdict};
use replay::ReplayFacts;

fn slp_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            slp_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "slp") {
            out.push(p);
        }
    }
}

/// A ruleset under which this game is legal, any-client on: its own stocks, timer, stage and
/// Gecko list.
fn rules_for(f: &ReplayFacts, any_client: bool) -> Ruleset {
    Ruleset {
        stocks: f.players.first().map_or(4, |p| p.start_stocks),
        timer_secs: f.timer_secs,
        legal_stages: vec![f.stage],
        allowed_gecko: f.gecko_sha256.iter().cloned().collect(),
        any_client,
        ..Ruleset::default()
    }
}

/// A copy of `file` with the last byte of its first Pre-Frame Update (0x37) changed: different
/// `raw` bytes, the same derived outcome (judging reads post-frames and Game End only).
fn tweak(file: &[u8]) -> Option<Vec<u8>> {
    let head = b"{U\x03raw[$U#l";
    if !file.starts_with(head) {
        return None;
    }
    let start = head.len() + 4;
    let raw = &file[start..];
    if raw.first() != Some(&0x35) {
        return None;
    }
    let len = usize::from(raw[1]);
    let mut sizes = [0usize; 256];
    let mut k = 2;
    while k + 2 < 1 + len {
        sizes[usize::from(raw[k])] = usize::from(u16::from_be_bytes([raw[k + 1], raw[k + 2]]));
        k += 3;
    }
    let mut i = 1 + len;
    while i < raw.len() {
        let code = raw[i];
        let size = sizes[usize::from(code)];
        if size == 0 {
            return None;
        }
        if code == 0x37 {
            let mut out = file.to_vec();
            out[start + i + size] ^= 0x01;
            return Some(out);
        }
        i += 1 + size;
    }
    None
}

#[tokio::test]
#[ignore = "needs AB_SLP_DIR (real Slippi Dolphin replays)"]
async fn any_client_on_real_slippi_replays() {
    let dir = PathBuf::from(std::env::var_os("AB_SLP_DIR").expect("set AB_SLP_DIR"));
    let limit: usize = std::env::var("AB_SLP_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let mut files = Vec::new();
    slp_files(&dir, &mut files);
    let checker = match (std::env::var_os("AB_ENGINE"), std::env::var_os("AB_ISO")) {
        (Some(e), Some(i)) => Some(
            Slpcheck::new(e.into(), i.into(), Duration::from_secs(300))
                .await
                .expect("the engine runs `version`"),
        ),
        _ => None,
    };

    let (mut tried, mut agreed, mut skipped) = (0, 0, 0);
    let mut outcome_checked = 0;
    let mut why: std::collections::BTreeMap<String, usize> = Default::default();
    let (mut verified, mut foreign) = (0, 0);
    let mut previous: Option<(PathBuf, Vec<u8>)> = None;
    for path in files {
        if tried >= limit {
            break;
        }
        let bytes = std::fs::read(&path).unwrap();
        let Ok(facts) = replay::parse(&bytes) else {
            *why.entry("unparseable".into()).or_default() += 1;
            skipped += 1;
            continue;
        };
        // Outcome agreement itself, on every readable file: a copy with other bytes derives the
        // same outcome, whether or not the game would be legal in a bracket.
        if let Some(t) = tweak(&bytes) {
            assert_ne!(t, bytes);
            let tf = replay::parse(&t).expect("the tweaked copy parses");
            assert_eq!(outcome(&tf), outcome(&facts), "{}", path.display());
            outcome_checked += 1;
        }
        let on = rules_for(&facts, true);
        // Only legal, decided two-player games are what a bracket would see.
        let pre = pre_judge([Some(&bytes), Some(&bytes)], &on);
        let Pre::NeedsResim { last_frame, .. } = pre else {
            *why.entry(format!("{pre:?}")).or_default() += 1;
            skipped += 1;
            continue;
        };
        let Some(other) = tweak(&bytes) else {
            *why.entry("no pre-frame to tweak".into()).or_default() += 1;
            skipped += 1;
            continue;
        };
        tried += 1;
        let off = rules_for(&facts, false);
        assert_eq!(
            pre_judge([Some(&bytes), Some(&other)], &off),
            Pre::Final(Verdict::Conflicted),
            "{}: off, bytes must match",
            path.display()
        );
        assert!(
            matches!(pre_judge([Some(&bytes), Some(&other)], &on), Pre::NeedsResim { .. }),
            "{}: on, the outcomes agree",
            path.display()
        );
        agreed += 1;
        if let Some((prev, pbytes)) = &previous {
            let pf = replay::parse(pbytes).unwrap();
            if outcome(&pf) != outcome(&facts) {
                assert_eq!(
                    pre_judge([Some(pbytes), Some(&bytes)], &on),
                    Pre::Final(Verdict::Conflicted),
                    "{} vs {}",
                    prev.display(),
                    path.display()
                );
            }
        }
        if let Some(c) = &checker {
            let res = c.run(&path, last_frame).await.expect("slpcheck runs");
            match finish(&res, true, true, true) {
                Verdict::Verified => verified += 1,
                Verdict::UnverifiedForeign => foreign += 1,
                v => panic!("{}: {v:?}", path.display()),
            }
        }
        previous = Some((path, bytes));
    }
    println!(
        "RESULT tried {tried}, outcome-agreement {agreed}, skipped {skipped}; resim: Verified {verified}, UnverifiedForeign {foreign}{}",
        if checker.is_none() { " (no AB_ENGINE: resim not run)" } else { "" }
    );
    println!("skipped because: {why:?}; outcome agreement checked on {outcome_checked} readable files");
    assert!(outcome_checked > 0, "no readable replays under {}", dir.display());
}

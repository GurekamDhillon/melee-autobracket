//! Verdict logic: legality, pre-judging an upload pair, and finishing after resim.

use autobracket_server::judge::{finish, legality, pre_judge, winner_slot, Pre};
use proto::{ResimOutcome, Ruleset, Verdict};
use replay::testkit::{simple_game, SlpBuilder};
use replay::PlayerKind;

const T0: &str = "2026-11-15T20:00:00Z";
const T1: &str = "2026-11-15T20:00:01Z";

/// A legal game (per `Ruleset::default()`) that port 2 wins, before `end`/`build`.
fn base() -> SlpBuilder {
    SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(-123, 1, 4, 0.0)
        .frame(-123, 2, 4, 0.0)
        .frame(5400, 1, 0, 0.0)
        .frame(5400, 2, 2, 63.0)
}

/// Both players upload the same file.
fn judge_pair(file: &[u8]) -> Pre {
    pre_judge([Some(file), Some(file)], &Ruleset::default())
}

fn illegal(word: &str) -> Pre {
    Pre::Final(Verdict::Illegal(word.to_string()))
}

#[test]
fn agreeing_pair_needs_resim() {
    let a = simple_game(2, T0);
    let b = simple_game(2, T1);
    assert_ne!(a, b, "metadata differs");
    assert_eq!(
        pre_judge([Some(&a), Some(&b)], &Ruleset::default()),
        Pre::NeedsResim {
            winner_port: 2,
            last_frame: 5400
        }
    );
}

#[test]
fn differing_raw_is_conflicted() {
    let a = base().end(2, None).build();
    let b = base().frame(5401, 2, 2, 63.0).end(2, None).build();
    assert_eq!(
        pre_judge([Some(&a), Some(&b)], &Ruleset::default()),
        Pre::Final(Verdict::Conflicted)
    );
}

#[test]
fn single_upload() {
    let a = simple_game(1, T0);
    let rules = Ruleset::default();
    assert_eq!(
        pre_judge([Some(&a), None], &rules),
        Pre::Final(Verdict::Single)
    );
    assert_eq!(
        pre_judge([None, Some(&a)], &rules),
        Pre::Final(Verdict::Single)
    );
    // Content does not matter for a single upload.
    assert_eq!(
        pre_judge([Some(b"garbage"), None], &rules),
        Pre::Final(Verdict::Single)
    );
    assert_eq!(pre_judge([None, None], &rules), Pre::Final(Verdict::Single));
}

#[test]
fn illegal_stage() {
    assert_eq!(
        judge_pair(&base().stage(4).end(2, None).build()),
        illegal("stage")
    );
}

#[test]
fn illegal_stocks() {
    assert_eq!(
        judge_pair(&base().stocks(3).end(2, None).build()),
        illegal("stocks")
    );
}

#[test]
fn illegal_timer() {
    assert_eq!(
        judge_pair(&base().timer_secs(420).end(2, None).build()),
        illegal("timer")
    );
}

#[test]
fn illegal_gecko() {
    assert_eq!(
        judge_pair(&base().gecko(true).end(2, None).build()),
        illegal("gecko")
    );
}

#[test]
fn illegal_teams() {
    assert_eq!(
        judge_pair(&base().teams(true).end(2, None).build()),
        illegal("teams")
    );
}

#[test]
fn illegal_banned_character() {
    let rules = Ruleset {
        banned_characters: vec![9],
        ..Ruleset::default()
    };
    let a = base().end(2, None).build();
    assert_eq!(
        pre_judge([Some(&a), Some(&a)], &rules),
        illegal("character")
    );
}

#[test]
fn first_failure_wins() {
    // Bad stage, stocks, timer and gecko at once: stage is checked first.
    let a = base()
        .stage(4)
        .stocks(3)
        .timer_secs(420)
        .gecko(true)
        .end(2, None)
        .build();
    assert_eq!(judge_pair(&a), illegal("stage"));
}

#[test]
fn cpu_player_is_illegal_players() {
    let a = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Cpu)
        .frame(5400, 1, 2, 10.0)
        .frame(5400, 2, 0, 0.0)
        .end(2, None)
        .build();
    assert_eq!(judge_pair(&a), illegal("players"));
}

/// Ruling (a): exactly two humans on ports 1 and 2, nobody else present.
#[test]
fn players_must_be_exactly_two_humans_on_ports_1_and_2() {
    let extra_cpu = base().player(3, 20, PlayerKind::Cpu).end(2, None).build();
    let extra_demo = base().player(4, 20, PlayerKind::Demo).end(2, None).build();
    let third_human = base().player(3, 20, PlayerKind::Human).end(2, None).build();
    let wrong_ports = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(3, 9, PlayerKind::Human)
        .frame(5400, 1, 0, 0.0)
        .frame(5400, 3, 2, 63.0)
        .end(2, None)
        .build();
    let alone = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .frame(5400, 1, 2, 0.0)
        .end(2, None)
        .build();
    for file in [extra_cpu, extra_demo, third_human, wrong_ports, alone] {
        assert_eq!(judge_pair(&file), illegal("players"));
        let facts = replay::parse(&file).unwrap();
        assert_eq!(legality(&facts, &Ruleset::default()), Err("players"));
    }
    // An explicitly Empty port is not a player.
    let empty_port = base().player(3, 0, PlayerKind::Empty).end(2, None).build();
    assert!(matches!(judge_pair(&empty_port), Pre::NeedsResim { .. }));
}

#[test]
fn legal_game_passes_legality() {
    let facts = replay::parse(&simple_game(1, T0)).unwrap();
    assert_eq!(legality(&facts, &Ruleset::default()), Ok(()));
}

#[test]
fn no_game_end_is_undecided() {
    assert_eq!(judge_pair(&base().build()), Pre::Final(Verdict::Undecided));
}

#[test]
fn tied_game_is_undecided() {
    let a = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(5400, 1, 1, 50.0)
        .frame(5400, 2, 1, 50.0)
        .end(1, None)
        .build();
    assert_eq!(judge_pair(&a), Pre::Final(Verdict::Undecided));
}

/// Ruling (b): only methods 1 (time), 2 (game) and 7 (no contest) decide a winner.
#[test]
fn only_known_end_methods_decide() {
    for method in [1u8, 2] {
        assert_eq!(
            judge_pair(&base().end(method, None).build()),
            Pre::NeedsResim {
                winner_port: 2,
                last_frame: 5400
            },
            "method {method}"
        );
    }
    // No contest: port 1 quit out, so port 2 wins.
    assert_eq!(
        judge_pair(&base().end(7, Some(1)).build()),
        Pre::NeedsResim {
            winner_port: 2,
            last_frame: 5400
        }
    );
    assert_eq!(
        judge_pair(&base().end(7, Some(2)).build()),
        Pre::NeedsResim {
            winner_port: 1,
            last_frame: 5400
        }
    );
    for method in [0u8, 3, 4, 5, 6, 8, 255] {
        assert_eq!(
            judge_pair(&base().end(method, None).build()),
            Pre::Final(Verdict::Undecided),
            "method {method}"
        );
    }
}

/// No contest decides only through an LRAS initiator who is one of the two players.
#[test]
fn no_contest_without_player_initiator_is_undecided() {
    assert_eq!(
        judge_pair(&base().end(7, None).build()),
        Pre::Final(Verdict::Undecided),
        "no initiator"
    );
    assert_eq!(
        judge_pair(&base().end(7, Some(3)).build()),
        Pre::Final(Verdict::Undecided),
        "initiator on an empty port"
    );
}

/// A replay with a Game Start and a decisive-looking Game End but no frames has
/// nothing to re-simulate and decides nothing.
#[test]
fn replay_without_frames_is_undecided() {
    let empty = || {
        SlpBuilder::new()
            .player(1, 2, PlayerKind::Human)
            .player(2, 9, PlayerKind::Human)
    };
    assert_eq!(
        judge_pair(&empty().end(7, Some(1)).build()),
        Pre::Final(Verdict::Undecided)
    );
    assert_eq!(
        judge_pair(&empty().end(2, None).build()),
        Pre::Final(Verdict::Undecided)
    );
}

#[test]
fn unreadable_raw_is_conflicted() {
    let good = simple_game(1, T0);
    let rules = Ruleset::default();
    for bad in [&b""[..], b"not a replay", b"{U\x03raw[$U#l\x00\x00\x00\xFF"] {
        assert_eq!(
            pre_judge([Some(&good), Some(bad)], &rules),
            Pre::Final(Verdict::Conflicted)
        );
        assert_eq!(
            pre_judge([Some(bad), Some(&good)], &rules),
            Pre::Final(Verdict::Conflicted)
        );
        assert_eq!(
            pre_judge([Some(bad), Some(bad)], &rules),
            Pre::Final(Verdict::Conflicted)
        );
    }
}

#[test]
fn unparseable_identical_pair_is_undecided() {
    // A readable raw element whose event stream is junk.
    let mut a = b"{U\x03raw[$U#l\x00\x00\x00\x03".to_vec();
    a.extend_from_slice(&[0x00, 0x01, 0x02]);
    a.extend_from_slice(b"U\x08metadata{}}");
    assert!(replay::raw_element(&a).is_ok());
    assert!(replay::parse(&a).is_err());
    let mut b = a.clone();
    b.extend_from_slice(b"trailing metadata difference");
    assert_eq!(
        pre_judge([Some(&a), Some(&b)], &Ruleset::default()),
        Pre::Final(Verdict::Undecided)
    );
}

#[test]
fn garbage_never_panics() {
    let rules = Ruleset::default();
    let good = simple_game(2, T0);
    let mut inputs: Vec<Vec<u8>> = vec![
        vec![],
        vec![0],
        vec![0xFF; 64],
        b"{U\x03raw[$U#l".to_vec(),
        b"{U\x03raw[$U#l\xFF\xFF\xFF\xFF".to_vec(),
        good.clone(),
    ];
    // Every truncation and a byte flip at many offsets of a real file.
    for cut in (0..good.len()).step_by(7) {
        inputs.push(good[..cut].to_vec());
        let mut flipped = good.clone();
        flipped[cut] ^= 0xA5;
        inputs.push(flipped);
    }
    // Deterministic pseudo-random blobs, some wearing the raw header.
    let mut seed: u32 = 0x1234_5678;
    for i in 0..200 {
        let mut blob = if i % 2 == 0 {
            b"{U\x03raw[$U#l".to_vec()
        } else {
            Vec::new()
        };
        for _ in 0..(i * 13 % 900) {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            blob.push((seed >> 24) as u8);
        }
        inputs.push(blob);
    }
    for a in &inputs {
        let _ = pre_judge([Some(a), Some(a)], &rules);
        let _ = pre_judge([Some(a), Some(&good)], &rules);
        let _ = pre_judge([Some(&good), Some(a)], &rules);
        let _ = pre_judge([Some(a), None], &rules);
        let _ = pre_judge([None, Some(a)], &rules);
    }
}

#[test]
fn finish_table() {
    let diverged = ResimOutcome::Diverged {
        frame: 100,
        port: 1,
        field: "percent".into(),
        engine_value: "10".into(),
        replay_value: "11".into(),
    };
    let early = ResimOutcome::EarlyEnd {
        engine_last: 100,
        replay_last: 5400,
    };
    let crashed = ResimOutcome::Crashed("boom".into());
    let unavailable = ResimOutcome::Unavailable("no dolphin".into());
    let cases = [
        (ResimOutcome::Match, Verdict::Verified, Verdict::Verified),
        (diverged, Verdict::Inexact, Verdict::VerifiedInexact),
        (early, Verdict::Inexact, Verdict::VerifiedInexact),
        (crashed, Verdict::Inexact, Verdict::VerifiedInexact),
        (unavailable, Verdict::PeerOnly, Verdict::PeerOnly),
        (ResimOutcome::Skipped, Verdict::PeerOnly, Verdict::PeerOnly),
    ];
    for (outcome, strict, lenient) in cases {
        assert_eq!(finish(&outcome, true), strict, "{outcome:?} strict");
        assert_eq!(finish(&outcome, false), lenient, "{outcome:?} lenient");
    }
}

#[test]
fn winner_slot_mapping() {
    assert_eq!(winner_slot(1), 0);
    assert_eq!(winner_slot(2), 1);
}

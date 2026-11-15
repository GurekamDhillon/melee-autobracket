use bracket::*;

#[allow(non_snake_case)]
fn E(n: EntrantId) -> Slot {
    Slot::Entrant(n)
}

/// Reports sets from `ready_sets()` (choosing the winner slot with `pick`) until the
/// bracket is complete; returns how many sets were reported.
fn play_out(b: &mut Bracket, mut pick: impl FnMut(&Set) -> u8) -> u32 {
    let mut played = 0;
    while !b.is_complete() {
        let ready = b.ready_sets();
        assert!(
            !ready.is_empty(),
            "bracket stuck: incomplete with no ready sets"
        );
        for id in ready {
            let winner = pick(&b.sets[id as usize]);
            b.report(id, winner).unwrap();
            played += 1;
        }
    }
    played
}

#[test]
fn seed_positions_8() {
    assert_eq!(seed_positions(8), vec![0, 7, 3, 4, 1, 6, 2, 5]);
}

#[test]
fn too_few() {
    assert!(matches!(
        build(Format::SingleElim, 1),
        Err(BracketError::TooFewEntrants)
    ));
    assert!(matches!(
        build(Format::SingleElim, 0),
        Err(BracketError::TooFewEntrants)
    ));
}

#[test]
fn single_8_first_round_pairs() {
    let b = build(Format::SingleElim, 8).unwrap();
    let r1: Vec<_> = b
        .sets
        .iter()
        .filter(|s| s.round == 1)
        .map(|s| s.slots)
        .collect();
    assert_eq!(r1, [[E(0), E(7)], [E(3), E(4)], [E(1), E(6)], [E(2), E(5)]]);
    assert_eq!(b.ready_sets().len(), 4);
}

#[test]
fn single_5_byes_go_to_top_seeds() {
    let b = build(Format::SingleElim, 5).unwrap();
    // Only 3v4 needs playing in round 1; seeds 0, 1, 2 receive the byes.
    let round_of = |id: SetId| b.sets[id as usize].round;
    let ready_r1: Vec<_> = b
        .ready_sets()
        .into_iter()
        .filter(|&id| round_of(id) == 1)
        .collect();
    assert_eq!(ready_r1.len(), 1);
    assert_eq!(b.sets[ready_r1[0] as usize].slots, [E(3), E(4)]);
    assert!(!b.sets.iter().any(|s| s.round == 1 && b.was_played(s.id)));
    // Seeds 1 and 2 both advanced on byes, so their round-2 set is already Ready.
    let ready: Vec<_> = b
        .ready_sets()
        .iter()
        .map(|&id| b.sets[id as usize].slots)
        .collect();
    assert_eq!(ready, [[E(3), E(4)], [E(1), E(2)]]);
}

#[test]
fn single_plays_n_minus_1_and_top_seed_wins_chalk() {
    for n in 2..=33 {
        let mut b = build(Format::SingleElim, n).unwrap();
        let played = play_out(&mut b, |_set| 0);
        assert_eq!(played, n - 1, "n = {n}");
        assert_eq!(b.champion(), Some(0), "n = {n}");
    }
}

#[test]
fn report_not_ready() {
    let mut b = build(Format::SingleElim, 8).unwrap();
    let final_id = b.sets.iter().find(|s| s.winner_to.is_none()).unwrap().id;
    assert_eq!(b.report(final_id, 0), Err(BracketError::NotReady));
    let ready = b.ready_sets()[0];
    assert_eq!(b.report(ready, 2), Err(BracketError::BadSlot));
    assert_eq!(b.report(9999, 0), Err(BracketError::UnknownSet));
}

#[test]
fn serde_round_trip() {
    let mut b = build(Format::SingleElim, 6).unwrap();
    let first = b.ready_sets()[0];
    b.report(first, 1).unwrap();
    let json = serde_json::to_string(&b).unwrap();
    let back: Bracket = serde_json::from_str(&json).unwrap();
    assert_eq!(back, b);
}

#[test]
fn single_upset_every_set_still_finishes() {
    for n in 2..=33 {
        let mut b = build(Format::SingleElim, n).unwrap();
        let played = play_out(&mut b, |_set| 1);
        assert_eq!(played, n - 1, "n = {n}");
        assert!(b.champion().is_some(), "n = {n}");
    }
}

#[test]
fn report_returns_newly_ready_sets() {
    let mut b = build(Format::SingleElim, 8).unwrap();
    assert_eq!(b.report(0, 0), Ok(vec![]));
    assert_eq!(b.report(1, 0), Ok(vec![4]));
}

/// The grand-final reset: the `Side::GrandFinal` set with `round == 2`.
fn reset(b: &Bracket) -> &Set {
    b.sets
        .iter()
        .find(|s| s.side == Side::GrandFinal && s.round == 2)
        .expect("double elimination bracket has a reset")
}

/// Tiny xorshift64 PRNG so random play-outs are deterministic without a dependency.
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        // xorshift must never hold zero; mix the seed so nearby seeds diverge quickly.
        XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Plays every ready set until the bracket is complete, choosing winners at random from
/// `seed`. Returns the number of losses each entrant took in sets that were actually played.
fn play_out_random(b: &mut Bracket, seed: u64) -> Vec<u32> {
    let mut rng = XorShift::new(seed);
    let mut losses = vec![0; b.entrants as usize];
    while !b.is_complete() {
        let ready = b.ready_sets();
        assert!(
            !ready.is_empty(),
            "bracket stuck: incomplete with no ready sets"
        );
        for id in ready {
            let winner = (rng.next() & 1) as u8;
            match b.sets[id as usize].slots[1 - winner as usize] {
                Slot::Entrant(loser) => losses[loser as usize] += 1,
                other => panic!("ready set {id} has non-entrant slot {other:?}"),
            }
            b.report(id, winner).unwrap();
        }
    }
    losses
}

#[test]
fn double_4_structure() {
    let b = build(Format::DoubleElim, 4).unwrap();
    assert_eq!(b.sets.len(), 7); // 3 winners, 2 losers, GF, reset
}

#[test]
fn chalk_skips_reset() {
    let mut b = build(Format::DoubleElim, 8).unwrap();
    assert_eq!(play_out(&mut b, |_| 0), 14); // 2n-2
    assert_eq!(b.champion(), Some(0));
    assert!(matches!(reset(&b).state, SetState::Skipped));
    assert_eq!(b.report(reset(&b).id, 0), Err(BracketError::NotReady));
}

#[test]
fn losers_side_winning_gf1_forces_reset() {
    let n = 8;
    let mut b = build(Format::DoubleElim, n).unwrap();
    let gf1 = b
        .sets
        .iter()
        .find(|s| s.side == Side::GrandFinal && s.round == 1)
        .unwrap()
        .id;
    // Play chalk until grand final 1 is ready.
    while !b.ready_sets().contains(&gf1) {
        let ready = b.ready_sets();
        assert!(!ready.is_empty(), "bracket stuck before grand final");
        for id in ready {
            b.report(id, 0).unwrap();
        }
    }
    let gf_slots = b.sets[gf1 as usize].slots;
    let reset_id = reset(&b).id;
    assert_eq!(b.report(gf1, 1), Ok(vec![reset_id]));
    assert_eq!(reset(&b).state, SetState::Ready);
    assert_eq!(reset(&b).slots, gf_slots);
    assert!(!b.is_complete());
    assert_eq!(b.champion(), None);

    assert_eq!(b.report(reset_id, 1), Ok(vec![]));
    let Slot::Entrant(losers_side) = gf_slots[1] else {
        panic!("GF slot 1 is not an entrant")
    };
    assert!(b.is_complete());
    assert_eq!(b.champion(), Some(losers_side));
    let played = b.sets.iter().filter(|s| b.was_played(s.id)).count() as u32;
    assert_eq!(played, 2 * n - 1);
}

#[test]
fn loss_count_invariant() {
    for n in 2..=33 {
        for seed in 0..20u64 {
            let mut b = build(Format::DoubleElim, n).unwrap();
            let losses = play_out_random(&mut b, seed);
            let champ = b.champion().unwrap() as usize;
            assert!(losses[champ] <= 1, "n={n} seed={seed}");
            for (e, l) in losses.iter().enumerate() {
                if e != champ {
                    assert_eq!(*l, 2, "n={n} seed={seed} e={e}");
                }
            }
            assert!(b.is_complete());
        }
    }
}

#[test]
fn byes_never_need_play() {
    let has_bye = |b: &Bracket| {
        b.ready_sets()
            .iter()
            .any(|&id| b.sets[id as usize].slots.contains(&Slot::Bye))
    };
    for n in [3, 5, 6, 7] {
        for seed in 0..20u64 {
            let mut b = build(Format::DoubleElim, n).unwrap();
            let mut rng = XorShift::new(seed);
            assert!(!has_bye(&b), "n={n} seed={seed}");
            while !b.is_complete() {
                let ready = b.ready_sets();
                assert!(!ready.is_empty(), "bracket stuck: n={n} seed={seed}");
                for id in ready {
                    b.report(id, (rng.next() & 1) as u8).unwrap();
                    assert!(!has_bye(&b), "n={n} seed={seed}");
                }
            }
        }
    }
}

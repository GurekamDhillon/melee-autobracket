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
        assert!(!ready.is_empty(), "bracket stuck: incomplete with no ready sets");
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
    assert!(matches!(build(Format::SingleElim, 1), Err(BracketError::TooFewEntrants)));
    assert!(matches!(build(Format::SingleElim, 0), Err(BracketError::TooFewEntrants)));
}

#[test]
fn single_8_first_round_pairs() {
    let b = build(Format::SingleElim, 8).unwrap();
    let r1: Vec<_> = b.sets.iter().filter(|s| s.round == 1).map(|s| s.slots).collect();
    assert_eq!(r1, [[E(0), E(7)], [E(3), E(4)], [E(1), E(6)], [E(2), E(5)]]);
    assert_eq!(b.ready_sets().len(), 4);
}

#[test]
fn single_5_byes_go_to_top_seeds() {
    let b = build(Format::SingleElim, 5).unwrap();
    // Only 3v4 needs playing in round 1; seeds 0, 1, 2 receive the byes.
    let round_of = |id: SetId| b.sets[id as usize].round;
    let ready_r1: Vec<_> = b.ready_sets().into_iter().filter(|&id| round_of(id) == 1).collect();
    assert_eq!(ready_r1.len(), 1);
    assert_eq!(b.sets[ready_r1[0] as usize].slots, [E(3), E(4)]);
    assert!(!b.sets.iter().any(|s| s.round == 1 && b.was_played(s.id)));
    // Seeds 1 and 2 both advanced on byes, so their round-2 set is already Ready.
    let ready: Vec<_> = b.ready_sets().iter().map(|&id| b.sets[id as usize].slots).collect();
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

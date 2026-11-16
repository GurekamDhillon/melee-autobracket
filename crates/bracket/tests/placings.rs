use std::collections::BTreeMap;

use bracket::*;

/// Reports `pick(set)` as the winner of every ready set until the bracket is complete.
fn play_out(b: &mut Bracket, pick: impl Fn(&Set) -> u8) {
    while !b.is_complete() {
        let ready = b.ready_sets();
        assert!(
            !ready.is_empty(),
            "bracket stuck: incomplete with no ready sets"
        );
        for id in ready {
            let winner = pick(&b.sets[id as usize]);
            b.report(id, winner).unwrap();
        }
    }
}

/// How many entrants finished at each place.
fn counts(p: &[u32]) -> BTreeMap<u32, usize> {
    let mut counts = BTreeMap::new();
    for &place in p {
        *counts.entry(place).or_insert(0) += 1;
    }
    counts
}

fn chalk(format: Format, entrants: u32) -> Vec<u32> {
    let mut b = build(format, entrants).unwrap();
    play_out(&mut b, |_| 0);
    let placings = b.placings().expect("complete bracket has placings");
    assert_eq!(placings.len(), entrants as usize);
    assert_eq!(placings[b.champion().unwrap() as usize], 1);
    placings
}

#[test]
fn incomplete_bracket_has_no_placings() {
    let b = build(Format::SingleElim, 4).unwrap();
    assert_eq!(b.placings(), None);
}

#[test]
fn single_elim_4_is_1_2_3_3() {
    let p = chalk(Format::SingleElim, 4);
    assert_eq!(counts(&p), BTreeMap::from([(1, 1), (2, 1), (3, 2)]));
    assert_eq!(p[0], 1);
}

#[test]
fn single_elim_8_is_1_2_3_3_5x4() {
    let p = chalk(Format::SingleElim, 8);
    assert_eq!(counts(&p), BTreeMap::from([(1, 1), (2, 1), (3, 2), (5, 4)]));
}

#[test]
fn single_elim_3_with_a_bye_is_1_2_3() {
    let p = chalk(Format::SingleElim, 3);
    assert_eq!(p.len(), 3);
    assert_eq!(counts(&p), BTreeMap::from([(1, 1), (2, 1), (3, 1)]));
}

#[test]
fn double_elim_4_is_1_2_3_4() {
    let p = chalk(Format::DoubleElim, 4);
    assert_eq!(counts(&p), BTreeMap::from([(1, 1), (2, 1), (3, 1), (4, 1)]));
}

#[test]
fn double_elim_8_is_1_2_3_4_5_5_7_7() {
    let p = chalk(Format::DoubleElim, 8);
    assert_eq!(
        counts(&p),
        BTreeMap::from([(1, 1), (2, 1), (3, 1), (4, 1), (5, 2), (7, 2)])
    );
}

#[test]
fn grand_final_reset_loser_is_second() {
    // DE 4: slot 0 wins every set except grand final round 1 (slot 1 wins, so the reset is
    // played), then slot 0 wins the reset. The reset's slot 0 entrant is 1st, its slot 1
    // entrant 2nd.
    let mut b = build(Format::DoubleElim, 4).unwrap();
    play_out(&mut b, |set| {
        if set.side == Side::GrandFinal && set.round == 1 {
            1
        } else {
            0
        }
    });
    let reset = b
        .sets
        .iter()
        .find(|s| s.side == Side::GrandFinal && s.round == 2)
        .unwrap();
    assert_eq!(reset.state, SetState::Done { winner: 0 });
    let [Slot::Entrant(first), Slot::Entrant(second)] = reset.slots else {
        panic!(
            "reset was not played between two entrants: {:?}",
            reset.slots
        )
    };
    let p = b.placings().unwrap();
    assert_eq!(b.champion(), Some(first));
    assert_eq!(p[first as usize], 1);
    assert_eq!(p[second as usize], 2);
    assert_eq!(counts(&p), BTreeMap::from([(1, 1), (2, 1), (3, 1), (4, 1)]));
}

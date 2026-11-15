//! Single elimination layout.

use crate::{seed_positions, Bracket, Format, Set, SetId, SetState, Side, Slot};

/// Builds a single elimination bracket for `entrants` (at least 2) seed-ordered entrants.
pub(crate) fn build(entrants: u32) -> Bracket {
    let size = entrants.next_power_of_two();
    let mut bracket = Bracket {
        format: Format::SingleElim,
        entrants,
        sets: winners_sets(size),
    };
    place_seeds(&mut bracket);
    bracket
}

/// Empty winners-bracket sets for a bracket of `size` (a power of two), numbered round by
/// round from id 0. The final's `winner_to` is `None`; every `loser_to` is `None`.
pub(crate) fn winners_sets(size: u32) -> Vec<Set> {
    let rounds = size.trailing_zeros();
    let mut sets = Vec::with_capacity(size as usize - 1);
    // `first` is the id of the round's first set.
    let mut first: SetId = 0;
    for round in 1..=rounds {
        let count = size >> round;
        let next_first = first + count;
        for k in 0..count {
            let winner_to = (round < rounds).then(|| (next_first + k / 2, (k % 2) as u8));
            sets.push(Set {
                id: first + k,
                side: Side::Winners,
                round: round as u8,
                slots: [Slot::Empty; 2],
                winner_to,
                loser_to: None,
                state: SetState::Waiting,
            });
        }
        first = next_first;
    }
    sets
}

/// Places entrants (and byes) into the first winners round, which starts at set id 0,
/// letting byes cascade through the bracket.
pub(crate) fn place_seeds(bracket: &mut Bracket) {
    let size = bracket.entrants.next_power_of_two();
    for (pos, seed) in seed_positions(size).into_iter().enumerate() {
        let slot = if seed < bracket.entrants {
            Slot::Entrant(seed)
        } else {
            Slot::Bye
        };
        bracket.fill(((pos / 2) as SetId, (pos % 2) as u8), slot, &mut Vec::new());
    }
}

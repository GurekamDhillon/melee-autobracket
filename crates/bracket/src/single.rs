//! Single elimination layout.

use crate::{seed_positions, Bracket, Format, Set, SetId, SetState, Side, Slot};

/// Builds a single elimination bracket for `entrants` (at least 2) seed-ordered entrants.
pub(crate) fn build(entrants: u32) -> Bracket {
    let size = entrants.next_power_of_two();
    let rounds = size.trailing_zeros();
    let mut sets = Vec::with_capacity(size as usize - 1);
    // Sets are numbered round by round; `first` is the id of the round's first set.
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
    let mut bracket = Bracket { format: Format::SingleElim, entrants, sets };
    for (pos, seed) in seed_positions(size).into_iter().enumerate() {
        let slot = if seed < entrants { Slot::Entrant(seed) } else { Slot::Bye };
        bracket.fill(((pos / 2) as SetId, (pos % 2) as u8), slot, &mut Vec::new());
    }
    bracket
}

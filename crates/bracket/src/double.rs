//! Double elimination layout: winners bracket, losers bracket, grand final and reset.

use crate::single::{place_seeds, winners_sets};
use crate::{Bracket, Format, Set, SetId, SetState, Side, Slot};

/// Builds a double elimination bracket for `entrants` (at least 2) seed-ordered entrants.
///
/// For a bracket of `size` with `R` winners rounds the losers bracket has `2(R - 1)` rounds.
/// Odd losers rounds pair up survivors (round 1 pairs winners-round-1 losers); even losers
/// round `2k` meets those survivors (slot 0) against the losers of winners round `k + 1`
/// (slot 1), dropped in reversed order on every other round to delay rematches.
/// The grand final (`round: 1`) has the winners champion in slot 0 and the losers champion
/// in slot 1; the reset (`round: 2`) is wired up by `Bracket::finish`, not by links.
pub(crate) fn build(entrants: u32) -> Bracket {
    let size = entrants.next_power_of_two();
    let rounds = size.trailing_zeros();
    let losers_rounds = 2 * (rounds - 1);
    let mut sets = winners_sets(size);

    // First id of each winners round (index r - 1) and each losers round (index lr - 1).
    let winners_first: Vec<SetId> = (1..=rounds).map(|r| size - (size >> (r - 1))).collect();
    let losers_count = |lr: u32| size >> (lr.div_ceil(2) + 1);
    let mut losers_first = Vec::with_capacity(losers_rounds as usize);
    let mut next = sets.len() as SetId;
    for lr in 1..=losers_rounds {
        losers_first.push(next);
        next += losers_count(lr);
    }
    let grand_final = next;
    let reset = next + 1;

    // Winners bracket: send each loser down, and the champion to the grand final.
    for set in sets.iter_mut() {
        let r = set.round as u32;
        let k = set.id - winners_first[r as usize - 1];
        set.loser_to = Some(if rounds == 1 {
            (grand_final, 1)
        } else if r == 1 {
            (losers_first[0] + k / 2, (k % 2) as u8)
        } else {
            let count = size >> r;
            let index = if r.is_multiple_of(2) {
                count - 1 - k
            } else {
                k
            };
            (losers_first[2 * (r as usize - 1) - 1] + index, 1)
        });
        if set.winner_to.is_none() {
            set.winner_to = Some((grand_final, 0));
        }
    }

    // Losers bracket.
    for lr in 1..=losers_rounds {
        let first = losers_first[lr as usize - 1];
        for k in 0..losers_count(lr) {
            let winner_to = if lr == losers_rounds {
                (grand_final, 1)
            } else if lr % 2 == 1 {
                (losers_first[lr as usize] + k, 0)
            } else {
                (losers_first[lr as usize] + k / 2, (k % 2) as u8)
            };
            sets.push(empty_set(
                first + k,
                Side::Losers,
                lr as u8,
                Some(winner_to),
            ));
        }
    }

    sets.push(empty_set(grand_final, Side::GrandFinal, 1, None));
    sets.push(empty_set(reset, Side::GrandFinal, 2, None));

    let mut bracket = Bracket {
        format: Format::DoubleElim,
        entrants,
        sets,
    };
    place_seeds(&mut bracket);
    bracket
}

fn empty_set(id: SetId, side: Side, round: u8, winner_to: Option<(SetId, u8)>) -> Set {
    Set {
        id,
        side,
        round,
        slots: [Slot::Empty; 2],
        winner_to,
        loser_to: None,
        state: SetState::Waiting,
    }
}

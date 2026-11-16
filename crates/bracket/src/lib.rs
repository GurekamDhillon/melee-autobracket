//! Pure bracket engine: structure and progression, no IO.

use serde::{Deserialize, Serialize};

/// Index into the seed-ordered entrant list; 0 is the top seed.
pub type EntrantId = u32;
pub type SetId = u32;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    SingleElim,
    DoubleElim,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Winners,
    Losers,
    GrandFinal,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Empty,
    Bye,
    Entrant(EntrantId),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetState {
    Waiting,
    Ready,
    Done { winner: u8 },
    Skipped,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Set {
    pub id: SetId,
    pub side: Side,
    pub round: u8,
    pub slots: [Slot; 2],
    pub winner_to: Option<(SetId, u8)>,
    pub loser_to: Option<(SetId, u8)>,
    pub state: SetState,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Bracket {
    pub format: Format,
    pub entrants: u32,
    pub sets: Vec<Set>,
}

#[derive(thiserror::Error, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BracketError {
    #[error("a bracket needs at least two entrants")]
    TooFewEntrants,
    #[error("no such set")]
    UnknownSet,
    #[error("set is not ready to be reported")]
    NotReady,
    #[error("winner slot must be 0 or 1")]
    BadSlot,
}

pub mod stages;

mod double;
mod seed;
mod single;

pub use seed::seed_positions;

/// Builds a bracket for `entrants` seed-ordered entrants (ids `0..entrants`, 0 = top seed).
pub fn build(format: Format, entrants: u32) -> Result<Bracket, BracketError> {
    if entrants < 2 {
        return Err(BracketError::TooFewEntrants);
    }
    match format {
        Format::SingleElim => Ok(single::build(entrants)),
        Format::DoubleElim => Ok(double::build(entrants)),
    }
}

impl Bracket {
    /// Records that `winner_slot` won `set`. Returns the sets that became Ready as a result.
    pub fn report(&mut self, set: SetId, winner_slot: u8) -> Result<Vec<SetId>, BracketError> {
        let state = self
            .sets
            .get(set as usize)
            .ok_or(BracketError::UnknownSet)?
            .state;
        if winner_slot > 1 {
            return Err(BracketError::BadSlot);
        }
        if state != SetState::Ready {
            return Err(BracketError::NotReady);
        }
        let mut newly_ready = Vec::new();
        self.finish(set, winner_slot, &mut newly_ready);
        Ok(newly_ready)
    }

    pub fn ready_sets(&self) -> Vec<SetId> {
        self.sets
            .iter()
            .filter(|s| s.state == SetState::Ready)
            .map(|s| s.id)
            .collect()
    }

    /// True once every set is resolved (Done or Skipped).
    pub fn is_complete(&self) -> bool {
        self.sets
            .iter()
            .all(|s| matches!(s.state, SetState::Done { .. } | SetState::Skipped))
    }

    /// Winner of the final set, once the bracket is complete. In double elimination that is
    /// the reset when it was played, otherwise grand final 1 (both have no `winner_to`).
    pub fn champion(&self) -> Option<EntrantId> {
        if !self.is_complete() {
            return None;
        }
        self.sets
            .iter()
            .rev()
            .find_map(|s| match (s.winner_to, s.state) {
                (None, SetState::Done { winner }) => match s.slots[winner as usize] {
                    Slot::Entrant(e) => Some(e),
                    _ => None,
                },
                _ => None,
            })
    }

    /// Final placings, once the bracket is complete: index = entrant id, value = 1-based place.
    ///
    /// The champion is 1st. Every other entrant is ranked by the highest `(side, round)` set
    /// they lost, with Winners < Losers < GrandFinal; their place is 1 + the number of entrants
    /// ranked strictly higher (the champion counts as highest). Equal ranks share a place, so
    /// single elimination gives 1, 2, 3, 3, 5, 5, 5, 5 and double elimination 1, 2, 3, 4, 5, 5,
    /// 7, 7. Sets decided by a bye are not losses.
    pub fn placings(&self) -> Option<Vec<u32>> {
        let champion = self.champion()?;
        let mut ranks = vec![(0u8, 0u8); self.entrants as usize];
        for set in &self.sets {
            let SetState::Done { winner } = set.state else {
                continue;
            };
            let (Slot::Entrant(_), Slot::Entrant(loser)) =
                (set.slots[winner as usize], set.slots[1 - winner as usize])
            else {
                continue;
            };
            let rank = &mut ranks[loser as usize];
            *rank = (*rank).max((set.side as u8, set.round));
        }
        ranks[champion as usize] = (u8::MAX, u8::MAX);
        Some(
            ranks
                .iter()
                .map(|rank| 1 + ranks.iter().filter(|other| *other > rank).count() as u32)
                .collect(),
        )
    }

    /// True if `set` was actually played: Done with an entrant in both slots.
    pub fn was_played(&self, set: SetId) -> bool {
        self.sets.get(set as usize).is_some_and(|s| {
            matches!(s.state, SetState::Done { .. })
                && s.slots.iter().all(|slot| matches!(slot, Slot::Entrant(_)))
        })
    }

    /// Puts `slot` into `target`, then settles that set: it becomes Ready when both slots hold
    /// entrants, and resolves itself when a slot holds a Bye.
    pub(crate) fn fill(&mut self, target: (SetId, u8), slot: Slot, newly_ready: &mut Vec<SetId>) {
        let (id, side) = target;
        let set = &mut self.sets[id as usize];
        set.slots[side as usize] = slot;
        match set.slots {
            [Slot::Empty, _] | [_, Slot::Empty] => {}
            [Slot::Entrant(_), Slot::Entrant(_)] => {
                set.state = SetState::Ready;
                newly_ready.push(id);
            }
            [Slot::Bye, Slot::Bye] => {
                set.state = SetState::Skipped;
                let (winner_to, loser_to) = (set.winner_to, set.loser_to);
                self.forward(winner_to, Slot::Bye, newly_ready);
                self.forward(loser_to, Slot::Bye, newly_ready);
            }
            [Slot::Entrant(_), Slot::Bye] => self.finish(id, 0, newly_ready),
            [Slot::Bye, Slot::Entrant(_)] => self.finish(id, 1, newly_ready),
        }
    }

    /// Marks `id` Done with `winner` and sends winner and loser on to their next sets.
    fn finish(&mut self, id: SetId, winner: u8, newly_ready: &mut Vec<SetId>) {
        let set = &mut self.sets[id as usize];
        set.state = SetState::Done { winner };
        let (won, lost) = (set.slots[winner as usize], set.slots[1 - winner as usize]);
        let (winner_to, loser_to) = (set.winner_to, set.loser_to);
        if set.side == Side::GrandFinal && set.round == 1 {
            let slots = set.slots;
            self.settle_reset(slots, winner, newly_ready);
        }
        self.forward(winner_to, won, newly_ready);
        self.forward(loser_to, lost, newly_ready);
    }

    /// After grand final 1: the reset is skipped if the winners-side entrant (slot 0) won,
    /// otherwise it is played between the same two entrants in the same slots.
    fn settle_reset(&mut self, gf: [Slot; 2], gf_winner: u8, newly_ready: &mut Vec<SetId>) {
        let reset = self
            .sets
            .iter()
            .position(|s| s.side == Side::GrandFinal && s.round == 2);
        let Some(reset) = reset.map(|i| i as SetId) else {
            return;
        };
        if gf_winner == 0 {
            self.sets[reset as usize].state = SetState::Skipped;
        } else {
            self.fill((reset, 0), gf[0], newly_ready);
            self.fill((reset, 1), gf[1], newly_ready);
        }
    }

    fn forward(&mut self, to: Option<(SetId, u8)>, slot: Slot, newly_ready: &mut Vec<SetId>) {
        if let Some(target) = to {
            self.fill(target, slot, newly_ready);
        }
    }
}

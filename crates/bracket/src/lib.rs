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

//! Slippi-style stage selection, held by the server: pure and deterministic.
//!
//! Game 1 strikes over the event's starter list (slot 0 first, turns alternating; turn `k`
//! strikes 1 if `k == 0`, else `min(2, remaining - 1)`, until one stage remains). Games 2+: the
//! previous winner bans `bans` stages, then the other slot picks from the rest of `legal`.
//! Duplicate stage ids in an input list count once. For `Later`, `bans` is clamped so at least
//! one stage stays pickable.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageKind {
    Strike,
    Ban,
    Pick,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    pub slot: u8,
    pub kind: StageKind,
    pub stage: u16,
}

/// Whose move it is; `count` is the number of moves left in this turn.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Turn {
    pub slot: u8,
    pub kind: StageKind,
    pub count: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameCtx<'a> {
    First {
        starters: &'a [u16],
    },
    Later {
        legal: &'a [u16],
        bans: u8,
        prev_winner: u8,
    },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Selection {
    /// Stages still available, in list order.
    pub pool: Vec<u16>,
    /// Stages struck or banned, in move order.
    pub removed: Vec<u16>,
    pub turn: Option<Turn>,
    pub chosen: Option<u16>,
}

#[derive(thiserror::Error, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageError {
    #[error("not your turn")]
    NotYourTurn,
    #[error("wrong kind of move for this turn")]
    WrongKind,
    #[error("stage is not in the pool")]
    NotInPool,
    #[error("stage was already struck or banned")]
    AlreadyRemoved,
    #[error("selection is complete")]
    Complete,
    #[error("slot must be 0 or 1")]
    BadSlot,
}

fn dedup(list: &[u16]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(list.len());
    for s in list {
        if !out.contains(s) {
            out.push(*s);
        }
    }
    out
}

/// Turn that follows a completed one (or the first), given the pool size; `None` when done.
fn first_strike_turn(slot: u8, remaining: usize, turn_no: usize) -> Option<Turn> {
    if remaining <= 1 {
        return None;
    }
    let count = if turn_no == 0 {
        1
    } else {
        remaining.saturating_sub(1).min(2)
    };
    Some(Turn {
        slot,
        kind: StageKind::Strike,
        count: count as u8,
    })
}

struct State {
    pool: Vec<u16>,
    removed: Vec<u16>,
    turn: Option<Turn>,
    chosen: Option<u16>,
    turn_no: usize,
    /// Later games: the slot that picks once the bans are done.
    picker: u8,
    first: bool,
}

impl State {
    fn new(ctx: GameCtx) -> Result<State, StageError> {
        match ctx {
            GameCtx::First { starters } => {
                let pool = dedup(starters);
                Ok(State {
                    turn: first_strike_turn(0, pool.len(), 0),
                    chosen: if pool.len() == 1 { Some(pool[0]) } else { None },
                    pool,
                    removed: Vec::new(),
                    turn_no: 0,
                    picker: 0,
                    first: true,
                })
            }
            GameCtx::Later {
                legal,
                bans,
                prev_winner,
            } => {
                if prev_winner > 1 {
                    return Err(StageError::BadSlot);
                }
                let pool = dedup(legal);
                let bans = (bans as usize).min(pool.len().saturating_sub(1)) as u8;
                let picker = 1 - prev_winner;
                let turn = if pool.is_empty() {
                    None
                } else if bans > 0 {
                    Some(Turn {
                        slot: prev_winner,
                        kind: StageKind::Ban,
                        count: bans,
                    })
                } else {
                    Some(Turn {
                        slot: picker,
                        kind: StageKind::Pick,
                        count: 1,
                    })
                };
                Ok(State {
                    pool,
                    removed: Vec::new(),
                    turn,
                    chosen: None,
                    turn_no: 0,
                    picker,
                    first: false,
                })
            }
        }
    }

    fn step(&mut self, mv: Move) -> Result<(), StageError> {
        if self.chosen.is_some() {
            return Err(StageError::Complete);
        }
        if mv.slot > 1 {
            return Err(StageError::BadSlot);
        }
        let Some(turn) = self.turn else {
            return Err(StageError::Complete);
        };
        if mv.slot != turn.slot {
            return Err(StageError::NotYourTurn);
        }
        if mv.kind != turn.kind {
            return Err(StageError::WrongKind);
        }
        let Some(idx) = self.pool.iter().position(|s| *s == mv.stage) else {
            return Err(if self.removed.contains(&mv.stage) {
                StageError::AlreadyRemoved
            } else {
                StageError::NotInPool
            });
        };
        self.pool.remove(idx);
        let left = turn.count - 1;
        if mv.kind == StageKind::Pick {
            self.chosen = Some(mv.stage);
            self.turn = None;
            return Ok(());
        }
        self.removed.push(mv.stage);
        if self.first {
            if self.pool.len() == 1 {
                self.chosen = Some(self.pool[0]);
                self.turn = None;
            } else if left > 0 {
                self.turn = Some(Turn {
                    count: left,
                    ..turn
                });
            } else {
                self.turn_no += 1;
                self.turn = first_strike_turn(1 - turn.slot, self.pool.len(), self.turn_no);
            }
        } else if left > 0 {
            self.turn = Some(Turn {
                count: left,
                ..turn
            });
        } else {
            self.turn = Some(Turn {
                slot: self.picker,
                kind: StageKind::Pick,
                count: 1,
            });
        }
        Ok(())
    }

    fn finish(self) -> Selection {
        Selection {
            pool: self.pool,
            removed: self.removed,
            turn: self.turn,
            chosen: self.chosen,
        }
    }
}

/// Validates a stored history move by move and returns the resulting selection.
pub fn replay(ctx: GameCtx, moves: &[Move]) -> Result<Selection, StageError> {
    let mut st = State::new(ctx)?;
    for mv in moves {
        st.step(*mv)?;
    }
    Ok(st.finish())
}

/// `replay` of `moves + [next]`.
pub fn apply(ctx: GameCtx, moves: &[Move], next: Move) -> Result<Selection, StageError> {
    let mut st = State::new(ctx)?;
    for mv in moves.iter().chain(std::iter::once(&next)) {
        st.step(*mv)?;
    }
    Ok(st.finish())
}

use serde::{Deserialize, Serialize};

use crate::Ruleset;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Role {
    Host,
    Guest,
}

/// What a player should do next in an event.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Next {
    NotEntered,
    Waiting,
    Review {
        set_id: u32,
    },
    Eliminated,
    Champion,
    Play {
        set_id: u32,
        opponent_tag: String,
        role: Role,
        relay: String,
        room_code: Option<String>,
        game_no: u8,
        /// `[me, opponent]`.
        wins: [u8; 2],
        best_of: u8,
        stocks: u8,
        timer_secs: u32,
        /// My replay for `game_no` is uploaded and not yet judged.
        awaiting_verdict: bool,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ResimOutcome {
    Match,
    Diverged {
        frame: i32,
        port: u8,
        field: String,
        engine_value: String,
        replay_value: String,
    },
    EarlyEnd {
        engine_last: i32,
        replay_last: i32,
    },
    Crashed(String),
    Unavailable(String),
    Skipped,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Verdict {
    Verified,
    VerifiedInexact,
    PeerOnly,
    Conflicted,
    Single,
    Illegal(String),
    Undecided,
    Inexact,
}

impl Verdict {
    /// Whether a game with this verdict counts toward the set score.
    pub fn counts(&self) -> bool {
        matches!(
            self,
            Verdict::Verified | Verdict::VerifiedInexact | Verdict::PeerOnly
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RegisterReq {
    pub tag: String,
    pub invite: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SignupReq {
    pub engine_build: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoomReq {
    pub code: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventCreateReq {
    pub name: String,
    pub format: bracket::Format,
    pub ruleset: Option<Ruleset>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventSummary {
    pub id: i64,
    pub name: String,
    pub state: String,
    pub entrants: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventView {
    pub summary: EventSummary,
    pub ruleset: Ruleset,
    /// Tags in seed order.
    pub tags: Vec<String>,
    pub bracket: Option<bracket::Bracket>,
    /// `(set_id, wins)`.
    pub scores: Vec<(u32, [u8; 2])>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Resolve {
    Award { slot: u8 },
    Replay,
    Dq { slot: u8 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QueueItem {
    pub id: i64,
    pub event_id: i64,
    pub set_id: u32,
    pub game_id: Option<i64>,
    pub reason: String,
    pub verdict: Option<Verdict>,
    pub resim: Option<ResimOutcome>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VerifyJob {
    pub id: i64,
    pub replay_last_frame: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VerifyResult {
    pub outcome: ResimOutcome,
    pub build: String,
}

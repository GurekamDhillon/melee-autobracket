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

/// A player's report that their opponent has not shown up for the set.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct NoShowReq {
    /// Optional short note for the organiser; the server keeps the first 200 characters.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventCreateReq {
    pub name: String,
    pub format: bracket::Format,
    pub ruleset: Option<Ruleset>,
    /// Overrides `best_of` of the event's ruleset (`ruleset`, or else the server's own).
    #[serde(default)]
    pub best_of: Option<u8>,
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
    /// What opened the item, beyond `reason`: for a no-show, the slot and tag that reported
    /// it and their note; for a stalled set, how long it had been idle.
    #[serde(default)]
    pub detail: Option<String>,
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

/// A game kept for later study, with the evidence it was judged on.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QuarantineItem {
    pub id: i64,
    pub game_id: i64,
    pub kind: String,
    pub verdict: Option<Verdict>,
    pub resim: Option<ResimOutcome>,
    pub verifier_build: Option<String>,
    /// The `x-ab-build` of each upload, in slot order.
    pub client_builds: Vec<String>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RekeyReq {
    /// The player's new verifying key, 64 lowercase hex characters.
    pub new_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_create_req_best_of_is_optional() {
        let req: EventCreateReq =
            serde_json::from_str(r#"{"name":"W","format":"SingleElim","ruleset":null}"#).unwrap();
        assert_eq!(req.best_of, None);
        let req: EventCreateReq = serde_json::from_str(
            r#"{"name":"W","format":"DoubleElim","ruleset":null,"best_of":5}"#,
        )
        .unwrap();
        assert_eq!(req.best_of, Some(5));
    }
}

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
        /// The event this answer is about. Absent (0) from servers before phase 2b.
        #[serde(default)]
        event_id: i64,
        set_id: u32,
    },
    Eliminated,
    Champion,
    Play {
        /// The event this answer is about. Absent (0) from servers before phase 2b.
        #[serde(default)]
        event_id: i64,
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
    /// A verifier is configured (`strict_resim` on) but could not check the game (no verifier
    /// online, no disc, an engine that will not boot, a verifier build not accepted). Does not
    /// count; the organiser decides.
    VerifierUnavailable,
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
    /// Optional short note for the organiser: at most 200 characters, none of them control
    /// characters or line separators (the server answers 400 otherwise).
    #[serde(default)]
    pub detail: Option<String>,
    /// What happened; the organiser queue item's reason. Absent means `NoShow`.
    #[serde(default)]
    pub kind: NoShowKind,
}

/// Why a player reports that a set cannot be played.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NoShowKind {
    /// `"no-show"`: the opponent never joined. Refused for `no_show_grace_secs` after a room
    /// code is posted.
    #[default]
    NoShow,
    /// `"client-failure"`: the reporter's own client could not complete a game.
    ClientFailure,
}

impl NoShowKind {
    /// The JSON value, which is also the organiser queue reason.
    pub const fn as_str(self) -> &'static str {
        match self {
            NoShowKind::NoShow => "no-show",
            NoShowKind::ClientFailure => "client-failure",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventCreateReq {
    pub name: String,
    pub format: bracket::Format,
    pub ruleset: Option<Ruleset>,
    /// Overrides `best_of` of the event's ruleset (`ruleset`, or else the server's own).
    #[serde(default)]
    pub best_of: Option<u8>,
    /// When the event starts, unix seconds (0 to 4102444800).
    #[serde(default)]
    pub starts_at: Option<i64>,
    /// What the event is about: at most 280 printable characters; empty or whitespace is
    /// stored as absent.
    #[serde(default)]
    pub description: Option<String>,
}

/// Sets or clears an event's start time and description (`POST
/// /v1/admin/events/{id}/schedule`): both fields are replaced, so an omitted one clears it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct EventScheduleReq {
    /// When the event starts, unix seconds (0 to 4102444800).
    #[serde(default)]
    pub starts_at: Option<i64>,
    /// At most 280 printable characters; empty or whitespace clears it.
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventSummary {
    pub id: i64,
    pub name: String,
    pub state: String,
    pub entrants: u32,
    /// When the event starts, unix seconds; null when the organiser has not said.
    #[serde(default)]
    pub starts_at: Option<i64>,
    /// What the event is about, or null.
    #[serde(default)]
    pub description: Option<String>,
    /// When the event was created, unix seconds.
    #[serde(default)]
    pub created_at: i64,
}

/// Where the live stream is (`GET /v1/stream`): the organiser's `stream_url`, or null when
/// no stream is configured.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct StreamInfo {
    pub url: Option<String>,
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
    /// Sets decided other than by played games, as `(set_id, how)`: `"award"` (an organiser
    /// award), `"dq"` (the opponent was disqualified) or `"double-dq"` (both were). Their
    /// `scores` count only the games played before the decision.
    #[serde(default)]
    pub decided_by: Vec<(u32, String)>,
    /// Counted games of this event that no verifier checked although one is configured: they
    /// count on the two players' agreement alone (`PeerOnly`, because the verifier was
    /// unavailable and `strict_resim` is off). Always 0 on a server with the verifier off.
    #[serde(default)]
    pub unverified_games: u32,
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
    /// What the judging worker found in the uploads: for an item about one game, that game's
    /// evidence (an object); for an item about the set (a no-show, a stalled set), an array
    /// with the evidence of every game of the set that has any (a game judged after the set
    /// went to review, say). `None` when there is none. Each game's evidence holds `game_id`,
    /// `game_no`, `state`, `verdict`, `uploads` (per slot: `slot`, `tag`, `sha256` (12 hex
    /// digits), `client_build`, `parse`, and for a readable file `stage`, `timer_secs`,
    /// `players` (`port`, `character`, `kind`, `start_stocks`, `final_stocks`,
    /// `final_percent`), `last_frame`, `end_method`, `lras_initiator`, `winner_port`,
    /// `legality` (`"ok"` or the first rule broken)), and `resim` (`outcome`,
    /// `detail`, `build`, `slot`, `evidence_only`: a lone upload is re-simulated as evidence
    /// only, which never counts) or null. Text from outside the server is cleaned of control
    /// characters, line separators and bidirectional controls.
    #[serde(default)]
    pub evidence: Option<serde_json::Value>,
    /// The tag of the player who reported a no-show.
    #[serde(default)]
    pub reporter_tag: Option<String>,
    /// The tags of the set's players, `[slot 0 (host), slot 1]`.
    #[serde(default)]
    pub tags: Option<[String; 2]>,
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
    /// When the verdict was applied, unix seconds.
    #[serde(default)]
    pub judged_at: Option<i64>,
}

/// One entrant's final place in a complete event. Ties share a place (two 3rds, four 5ths).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Placing {
    /// 1-based.
    pub place: u32,
    pub tag: String,
}

/// A complete event and its placings (`GET /v1/history`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventResult {
    pub summary: EventSummary,
    /// Ordered by place, then seed.
    pub placings: Vec<Placing>,
}

/// One row of the players index (`GET /v1/players`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlayerSummary {
    pub tag: String,
    /// Events entered, in any state.
    pub events: u32,
    /// Complete events won.
    pub titles: u32,
    /// Sets `[won, lost]`.
    pub sets: [u32; 2],
}

/// A player's entry in one event.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlayerEvent {
    pub event: EventSummary,
    /// The entrant's index in the event's `tags` (0 = top seed).
    pub seed: u32,
    /// The final place; null unless the event is complete.
    pub place: Option<u32>,
    /// Sets `[won, lost]` in this event.
    pub sets: [u32; 2],
    /// Games `[won, lost]` in this event.
    pub games: [u32; 2],
}

/// A player's record against one opponent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HeadToHead {
    pub tag: String,
    /// Sets `[won, lost]`.
    pub sets: [u32; 2],
    /// Games `[won, lost]`.
    pub games: [u32; 2],
}

/// A player's profile (`GET /v1/players/{tag}`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlayerProfile {
    /// The canonical spelling.
    pub tag: String,
    /// When the player registered, unix seconds.
    pub since: i64,
    /// Newest first.
    pub events: Vec<PlayerEvent>,
    /// By sets played descending, then tag.
    pub head_to_head: Vec<HeadToHead>,
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
        assert_eq!(req.starts_at, None);
        assert_eq!(req.description, None);
        let req: EventCreateReq = serde_json::from_str(
            r#"{"name":"W","format":"DoubleElim","ruleset":null,"best_of":5}"#,
        )
        .unwrap();
        assert_eq!(req.best_of, Some(5));
    }
}

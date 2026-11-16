//! Phase 2b wire types: accounts and linking, the in-game client's home, badge, set flow and
//! server-held stage selection, the live relay's public view, and site information.
//!
//! Perspective is the signer's: every pair is `[me, opponent]`, as in [`Next`], and [`Who`]
//! names a player relative to the signer. Stages are always identified by their stable Melee
//! stage id (the id a replay's Game Start records), never by a position in a list; responses
//! carry the id with a display name ([`Stage`]).

use serde::{Deserialize, Serialize};

use crate::{EventSummary, Next, Role, Verdict};

/// Display names of the stages, by stable Melee stage id (the id a replay's Game Start
/// records). Ids missing here are named by [`Stage::from_id`] as `Stage {id}`.
const STAGE_NAMES: [(u16, &str); 30] = [
    (2, "Fountain of Dreams"),
    (3, "Pokémon Stadium"),
    (4, "Princess Peach's Castle"),
    (5, "Kongo Jungle"),
    (6, "Brinstar"),
    (7, "Corneria"),
    (8, "Yoshi's Story"),
    (9, "Onett"),
    (10, "Mute City"),
    (11, "Rainbow Cruise"),
    (12, "Jungle Japes"),
    (13, "Great Bay"),
    (14, "Hyrule Temple"),
    (15, "Brinstar Depths"),
    (16, "Yoshi's Island"),
    (17, "Green Greens"),
    (18, "Fourside"),
    (19, "Mushroom Kingdom I"),
    (20, "Mushroom Kingdom II"),
    (22, "Venom"),
    (23, "Poké Floats"),
    (24, "Big Blue"),
    (25, "Icicle Mountain"),
    (26, "Icetop"),
    (27, "Flat Zone"),
    (28, "Dream Land N64"),
    (29, "Yoshi's Island N64"),
    (30, "Kongo Jungle N64"),
    (31, "Battlefield"),
    (32, "Final Destination"),
];

/// A stage as responses carry it: its stable id and a display name. Requests name a stage by
/// `id` alone ([`StageMoveReq::stage`]).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    /// The Melee stage id, as a replay's Game Start records it and `Ruleset.legal_stages`
    /// lists it.
    pub id: u16,
    /// For display only; never compare by name.
    pub name: String,
}

impl Stage {
    /// The stage with id `id` and its name from the server's table (`Stage {id}` for an id
    /// the table does not know).
    pub fn from_id(id: u16) -> Stage {
        let name = STAGE_NAMES
            .iter()
            .find(|(s, _)| *s == id)
            .map_or_else(|| format!("Stage {id}"), |(_, n)| (*n).to_owned());
        Stage { id, name }
    }
}

/// A bracket side, with the same strings as the bracket JSON's `side`. Server code that also
/// uses `bracket::Side` should name this one `proto::Side` rather than import it bare.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Winners,
    Losers,
    GrandFinal,
}

impl From<bracket::Side> for Side {
    fn from(side: bracket::Side) -> Side {
        match side {
            bracket::Side::Winners => Side::Winners,
            bracket::Side::Losers => Side::Losers,
            bracket::Side::GrandFinal => Side::GrandFinal,
        }
    }
}

/// What the site offers (`GET /v1/site`, public).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SiteInfo {
    /// Whether accounts (Discord sign-in) are enabled on this server.
    pub identity: bool,
    /// Who runs the server, as configured, or null.
    pub operator: Option<String>,
    /// How to reach the operator, as configured, or null.
    pub contact: Option<String>,
}

/// The signer's player (`GET /v1/me`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MeStatus {
    pub tag: String,
    /// The player belongs to an account.
    pub linked: bool,
    /// The account's display name, or null when not linked.
    pub display_name: Option<String>,
}

/// Links the signing key to an account (`POST /v1/me/link`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LinkReq {
    /// The one-use code from the account page.
    pub code: String,
}

/// The player the signing key now plays as.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LinkResult {
    pub tag: String,
}

/// The in-game client's home screen (`GET /v1/me/home`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Home {
    /// `action` is `SetUp` and the signer has not said ready.
    pub badge: bool,
    pub action: HomeAction,
    /// Every event in sign-up or running the player entered, oldest first.
    pub events: Vec<MyEvent>,
}

/// One of the player's events and what to do next in it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MyEvent {
    pub event: EventSummary,
    pub next: Next,
}

/// The one thing the home screen puts first.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum HomeAction {
    /// Nothing to do or wait for.
    None,
    /// A set is called: the first event whose `next` is `Play`.
    SetUp {
        event_id: i64,
        set_id: u32,
        side: Side,
        round: u8,
        /// The round's name, as `docs/api.md` section 7 derives it (`"Winners semis"`).
        round_name: String,
        opponent_tag: String,
        best_of: u8,
        game_no: u32,
        /// `[me, opponent]`.
        ready: [bool; 2],
    },
    /// The player's set is with the organiser.
    Review { event_id: i64, set_id: u32 },
    /// The player's next set waits on a feeder set.
    Upcoming {
        event_id: i64,
        set_id: u32,
        side: Side,
        round: u8,
        /// The round's name, as `docs/api.md` section 7 derives it.
        round_name: String,
        /// Null while the opponent is not known yet.
        opponent_tag: Option<String>,
        /// Estimated seconds until the set is called; null when it cannot be estimated.
        eta_secs: Option<u32>,
    },
}

/// The main-menu star (`GET /v1/me/badge`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Badge {
    /// A set is called and the signer has not said ready.
    pub set_up: bool,
    pub event_id: Option<i64>,
    pub set_id: Option<u32>,
}

/// Says ready for a set (`POST /v1/sets/{event}/{set}/ready`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReadyReq {
    pub engine_build: String,
}

/// A stage-selection move, as the wire carries it. `bracket::stages` has its own `StageKind`;
/// code using both should name this one `proto::StageKind`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StageKind {
    Strike,
    Ban,
    Pick,
}

/// A player relative to the signer.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Who {
    Me,
    Opponent,
}

/// One stage-selection move (`POST /v1/sets/{event}/{set}/games/{n}/stage`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StageMoveReq {
    pub kind: StageKind,
    /// The stage's stable id.
    pub stage: u16,
}

/// How a game's stage is chosen.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StageMode {
    /// Game 1: strikes over the starters.
    Strike,
    /// Later games: the previous winner bans, the loser picks.
    Counterpick,
}

/// A game's stage selection.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StageState {
    pub mode: StageMode,
    /// The stages selection is over: the starters for game 1, else every legal stage.
    pub pool: Vec<Stage>,
    /// The moves so far, in order; their stages are gone from the pool's choice.
    pub moves: Vec<StageMove>,
    /// Whose move it is; null once the stage is chosen.
    pub turn: Option<StageTurn>,
    pub chosen: Option<Stage>,
}

/// A move made.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StageMove {
    pub who: Who,
    pub kind: StageKind,
    pub stage: Stage,
}

/// The move awaited.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageTurn {
    pub who: Who,
    pub kind: StageKind,
    /// Moves left in this turn.
    pub count: u8,
}

/// Where a set is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetPhase {
    ReadyCheck,
    StageSelect,
    Playing,
    Judging,
    Review,
    Done,
}

/// One game of a set.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GameResult {
    pub game_no: u32,
    /// The stage played (from the replays), or null when not known.
    pub stage: Option<Stage>,
    /// Null when the replays determine no winner.
    pub winner: Option<Who>,
    /// The winner's stocks left at the end, or null when not known.
    pub winner_stocks: Option<u8>,
    /// The winner's percent at the end, or null when not known.
    pub winner_percent: Option<f32>,
    pub counted: bool,
    pub verdict: Option<Verdict>,
}

/// A set from one player's side (`GET /v1/sets/{event}/{set}/flow`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SetFlow {
    pub event_id: i64,
    pub set_id: u32,
    pub side: Side,
    pub round: u8,
    pub opponent_tag: String,
    /// Who hosts the netplay room: `Host` is slot 0.
    pub role: Role,
    /// The netplay relay address, `host:port`, as in `Next::Play`.
    pub relay: String,
    /// The room code the host posted for the current game, as in `Next::Play`; null until
    /// posted.
    pub room_code: Option<String>,
    pub best_of: u8,
    /// `[me, opponent]`.
    pub wins: [u32; 2],
    /// The current game, 1-based.
    pub game_no: u32,
    /// `[me, opponent]`.
    pub ready: [bool; 2],
    pub phase: SetPhase,
    /// The current game's selection as it stands: present in `stage_select`, `playing` and
    /// `judging` (`chosen` is null in the last two when the game was started without
    /// completing it); null in `ready_check`, `review` and `done`.
    pub stage: Option<StageState>,
    pub games: Vec<GameResult>,
    /// Every counted game's verdict is `Verified`.
    pub verified: bool,
    /// Once done, how the set ended for the player; else null.
    pub outcome: Option<SetOutcome>,
}

/// How a decided set ended for the player, with what comes next.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum SetOutcome {
    /// Won the set and still in the event; `next` is the player's `Next` in it.
    Advanced { next: Next },
    /// Lost the set and still in the event: a winners-side set of a double-elimination event
    /// (or a grand final whose reset follows); `next` is the player's `Next` in it.
    DroppedToLosers { next: Next },
    /// Lost the set and out of the event.
    Eliminated,
    /// Won the event.
    Champion,
}

/// A game being relayed live (`GET /v1/live`, public).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LiveGame {
    pub event_id: i64,
    pub set_id: u32,
    pub game_no: u32,
    /// `[slot 0, slot 1]`.
    pub tags: [String; 2],
    pub stage: Option<Stage>,
    pub ports: Vec<LivePort>,
    /// The latest frame received.
    pub frame: Option<i32>,
    pub ended: bool,
    /// Ranges of the stream that never arrived.
    pub gaps: u32,
    /// Unix seconds of the last message.
    pub updated_at: i64,
}

/// One player's state in a live game.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LivePort {
    pub port: u8,
    /// External character id.
    pub character: u8,
    pub stocks: u8,
    pub percent: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names_by_id() {
        assert_eq!(
            Stage::from_id(31),
            Stage {
                id: 31,
                name: "Battlefield".into()
            }
        );
        assert_eq!(Stage::from_id(32).name, "Final Destination");
        assert_eq!(Stage::from_id(99).name, "Stage 99");
    }

    #[test]
    fn side_matches_the_bracket_strings() {
        for side in [
            bracket::Side::Winners,
            bracket::Side::Losers,
            bracket::Side::GrandFinal,
        ] {
            assert_eq!(
                serde_json::to_value(Side::from(side)).unwrap(),
                serde_json::to_value(side).unwrap()
            );
        }
    }
}

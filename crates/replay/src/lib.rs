//! Minimal reader for Slippi `.slp` replay files.
//!
//! Extracts only the facts needed to verify a match result. Input is
//! untrusted: every malformed file yields a [`ReplayError`], never a panic.
//! Ports in all public types are 1-based (the file stores 0-based indices).

mod events;
mod raw;
#[cfg(feature = "testkit")]
pub mod testkit;

pub use raw::raw_element;

use events::{Events, GAME_END, GAME_START, POST_FRAME, PRE_FRAME};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    Human,
    Cpu,
    Demo,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerStart {
    /// 1-based.
    pub port: u8,
    /// External character id.
    pub character: u8,
    pub kind: PlayerKind,
    pub start_stocks: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerFinal {
    /// 1-based.
    pub port: u8,
    pub stocks: u8,
    pub percent: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEnd {
    /// 1 = TIME!, 2 = GAME!, 7 = No Contest (2.0.0+).
    pub method: u8,
    /// 1-based port of the player who quit out (LRAS), if any.
    pub lras_initiator: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReplayFacts {
    pub slippi_version: [u8; 4],
    pub stage: u16,
    pub timer_secs: u32,
    pub is_teams: bool,
    /// Non-Empty ports only, in port order.
    pub players: Vec<PlayerStart>,
    /// Whether the replay carries a non-empty Gecko code list; always
    /// `gecko_sha256.is_some()`.
    pub has_gecko: bool,
    /// SHA-256 of the Gecko code list bytes (see [`gecko_fnv32`] for which
    /// bytes), lowercase hex; `None` when there is no list. The legality identity.
    pub gecko_sha256: Option<String>,
    /// Highest Post-Frame Update frame number (-123 is the first frame); `i32::MIN` if none.
    pub last_frame: i32,
    pub end: Option<GameEnd>,
    /// Leader (non-follower) state on `last_frame`, in port order. A port with
    /// no update on that frame is absent.
    pub finals: Vec<PlayerFinal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    #[error("not a Slippi replay file")]
    NotSlp,
    #[error("replay file is truncated")]
    Truncated,
    #[error("replay event stream has a malformed or incomplete event size table")]
    BadEventSizes,
    #[error("replay has no usable Game Start event")]
    MissingGameStart,
    /// A second Game Start, a second Game End, or a Pre- or Post-Frame Update after Game
    /// End: a file the engine and this reader could read as different games.
    #[error("replay has a second Game Start or Game End, or frames after Game End")]
    UnexpectedEvent,
}

pub fn parse(file: &[u8]) -> Result<ReplayFacts, ReplayError> {
    let mut events = Events::new(raw_element(file)?)?;
    // The spec places Game Start immediately after Event Payloads.
    let start = match events.next() {
        Some(Ok((GAME_START, ev))) => {
            events::game_start(ev).ok_or(ReplayError::MissingGameStart)?
        }
        Some(Err(e)) => return Err(e),
        _ => return Err(ReplayError::MissingGameStart),
    };

    let mut gecko = Vec::new();
    let mut last_frame = i32::MIN;
    let mut end = None;
    let mut seen_end = false;
    // Latest leader update per player index: (frame, stocks, percent).
    let mut latest: [Option<(i32, u8, f32)>; 4] = [None; 4];

    for event in events {
        let (code, ev) = event?;
        match code {
            // The engine keeps the last Game Start and never reads Game End: refuse any file
            // where that could differ from what is judged here.
            GAME_START => return Err(ReplayError::UnexpectedEvent),
            PRE_FRAME | POST_FRAME | GAME_END if seen_end => {
                return Err(ReplayError::UnexpectedEvent)
            }
            POST_FRAME => {
                let Some(pf) = events::post_frame(ev) else {
                    continue;
                };
                last_frame = last_frame.max(pf.frame);
                let slot = &mut latest[usize::from(pf.index)];
                // `>=`: under rollback a re-sent frame supersedes the earlier copy.
                if !pf.is_follower && slot.is_none_or(|(frame, _, _)| pf.frame >= frame) {
                    *slot = Some((pf.frame, pf.stocks, pf.percent));
                }
            }
            GAME_END => {
                seen_end = true;
                if let Some((method, lras)) = events::game_end(ev) {
                    end = Some(GameEnd {
                        method,
                        lras_initiator: lras.map(|i| i + 1),
                    });
                }
            }
            _ => {
                if let Some(chunk) = events::gecko_chunk(code, ev) {
                    gecko.extend_from_slice(chunk);
                }
            }
        }
    }

    let finals = (1u8..)
        .zip(latest)
        .filter_map(|(port, slot)| match slot {
            Some((frame, stocks, percent)) if frame == last_frame => Some(PlayerFinal {
                port,
                stocks,
                percent,
            }),
            _ => None,
        })
        .collect();

    Ok(ReplayFacts {
        slippi_version: start.version,
        stage: start.stage,
        timer_secs: start.timer_secs,
        is_teams: start.is_teams,
        players: start.players,
        has_gecko: !gecko.is_empty(),
        gecko_sha256: (!gecko.is_empty()).then(|| hex(&Sha256::digest(&gecko))),
        last_frame,
        end,
        finals,
    })
}

/// The engine's display hash of the replay's Gecko code list, as `slpcheck`
/// prints it in its `gecko_hash` CSV column: FNV-1a 64-bit over the list bytes,
/// truncated to the low 32 bits, 8 lowercase hex digits. `None` when there is
/// no list. For display and cross-checking only, never for legality (use
/// [`ReplayFacts::gecko_sha256`]).
///
/// The list bytes: walking the `raw` element's events in file order, every
/// Message Splitter event (0x10) of at least 0x205 bytes whose internal command
/// (0x203) is Gecko List (0x3D) contributes bytes `1..1 + min(size at 0x201, 512)`.
/// Errors when the event stream cannot be walked to its end; unlike [`parse`]
/// it needs no Game Start.
pub fn gecko_fnv32(file: &[u8]) -> Result<Option<String>, ReplayError> {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut any = false;
    for event in Events::new(raw_element(file)?)? {
        let (code, ev) = event?;
        for &b in events::gecko_chunk(code, ev).unwrap_or_default() {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            any = true;
        }
    }
    Ok(any.then(|| format!("{:08x}", hash as u32)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The winning port of a 1v1 between two humans, or `None` when the replay
/// does not determine one.
pub fn winner_port(f: &ReplayFacts) -> Option<u8> {
    let humans: Vec<u8> = f
        .players
        .iter()
        .filter(|p| p.kind == PlayerKind::Human)
        .map(|p| p.port)
        .collect();
    let [a, b] = humans[..] else { return None };
    let end = f.end.as_ref()?;

    if end.method == 7 {
        return match end.lras_initiator? {
            p if p == a => Some(b),
            p if p == b => Some(a),
            _ => None,
        };
    }

    let final_of = |port| f.finals.iter().find(|p| p.port == port);
    let (fa, fb) = (final_of(a)?, final_of(b)?);
    match fa.stocks.cmp(&fb.stocks) {
        std::cmp::Ordering::Greater => Some(a),
        std::cmp::Ordering::Less => Some(b),
        std::cmp::Ordering::Equal => match fa.percent.partial_cmp(&fb.percent)? {
            std::cmp::Ordering::Less => Some(a),
            std::cmp::Ordering::Greater => Some(b),
            std::cmp::Ordering::Equal => None,
        },
    }
}

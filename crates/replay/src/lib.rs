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

use events::{Events, GAME_END, GAME_START, POST_FRAME};

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
    pub has_gecko: bool,
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

    let mut has_gecko = false;
    let mut last_frame = i32::MIN;
    let mut end = None;
    // Latest leader update per player index: (frame, stocks, percent).
    let mut latest: [Option<(i32, u8, f32)>; 4] = [None; 4];

    for event in events {
        let (code, ev) = event?;
        match code {
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
                if let Some((method, lras)) = events::game_end(ev) {
                    end = Some(GameEnd {
                        method,
                        lras_initiator: lras.map(|i| i + 1),
                    });
                }
            }
            _ => has_gecko |= events::carries_gecko(code, ev),
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
        has_gecko,
        last_frame,
        end,
        finals,
    })
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

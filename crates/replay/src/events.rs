//! Splitting the event stream and decoding the few events we care about.
//!
//! Offsets follow the Slippi replay spec (v3.18 layout) and are measured from
//! the event's command byte, exactly as the spec's tables list them. Every
//! read is bounds-checked against the event's declared size, so fields that
//! older replay versions do not carry simply read as absent.

use crate::{PlayerKind, PlayerStart, ReplayError};

pub(crate) const MESSAGE_SPLITTER: u8 = 0x10;
pub(crate) const EVENT_PAYLOADS: u8 = 0x35;
pub(crate) const GAME_START: u8 = 0x36;
pub(crate) const POST_FRAME: u8 = 0x38;
pub(crate) const GAME_END: u8 = 0x39;
pub(crate) const GECKO_LIST: u8 = 0x3D;

/// Iterator over `(command byte, whole event including the command byte)`.
/// Stops after the first error.
pub(crate) struct Events<'a> {
    rest: &'a [u8],
    sizes: [Option<u16>; 256],
    failed: bool,
}

impl<'a> Events<'a> {
    /// Reads the leading Event Payloads event (the size table).
    pub(crate) fn new(raw: &'a [u8]) -> Result<Self, ReplayError> {
        let (&code, after) = raw.split_first().ok_or(ReplayError::Truncated)?;
        if code != EVENT_PAYLOADS {
            return Err(ReplayError::BadEventSizes);
        }
        // The payload size counts its own byte: 3n + 1 for n command/size pairs.
        let n = usize::from(*after.first().ok_or(ReplayError::Truncated)?);
        if n == 0 || (n - 1) % 3 != 0 {
            return Err(ReplayError::BadEventSizes);
        }
        if after.len() < n {
            return Err(ReplayError::Truncated);
        }
        let (table, rest) = after.split_at(n);
        let mut sizes = [None; 256];
        let (pairs, _) = table[1..].as_chunks::<3>();
        for &[code, hi, lo] in pairs {
            let size = u16::from_be_bytes([hi, lo]);
            // The engine treats a size of 0 as "not listed" and stops walking there, so
            // a 1-byte event would let us read events it never sees. Real files never
            // list one.
            if size == 0 {
                return Err(ReplayError::BadEventSizes);
            }
            sizes[usize::from(code)] = Some(size);
        }
        Ok(Self {
            rest,
            sizes,
            failed: false,
        })
    }
}

impl<'a> Iterator for Events<'a> {
    type Item = Result<(u8, &'a [u8]), ReplayError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let &code = self.rest.first()?;
        let result = match self.sizes[usize::from(code)] {
            None => Err(ReplayError::BadEventSizes),
            Some(size) => {
                let total = 1 + usize::from(size);
                if self.rest.len() < total {
                    Err(ReplayError::Truncated)
                } else {
                    let (event, rest) = self.rest.split_at(total);
                    self.rest = rest;
                    Ok((code, event))
                }
            }
        };
        self.failed = result.is_err();
        Some(result)
    }
}

fn bytes<const N: usize>(ev: &[u8], off: usize) -> Option<[u8; N]> {
    ev.get(off..off + N)?.try_into().ok()
}
fn u8_at(ev: &[u8], off: usize) -> Option<u8> {
    ev.get(off).copied()
}
fn u16_at(ev: &[u8], off: usize) -> Option<u16> {
    bytes(ev, off).map(u16::from_be_bytes)
}
fn u32_at(ev: &[u8], off: usize) -> Option<u32> {
    bytes(ev, off).map(u32::from_be_bytes)
}
fn i32_at(ev: &[u8], off: usize) -> Option<i32> {
    bytes(ev, off).map(i32::from_be_bytes)
}
fn f32_at(ev: &[u8], off: usize) -> Option<f32> {
    bytes(ev, off).map(f32::from_be_bytes)
}

pub(crate) struct GameStart {
    pub version: [u8; 4],
    pub stage: u16,
    pub timer_secs: u32,
    pub is_teams: bool,
    /// Non-empty ports only.
    pub players: Vec<PlayerStart>,
}

/// The Game Info Block starts at 0x5 in the Game Start event.
const GAME_INFO: usize = 0x5;

/// `None` if the event is too short to hold the fields we need.
pub(crate) fn game_start(ev: &[u8]) -> Option<GameStart> {
    let mut players = Vec::new();
    for i in 0..4u8 {
        let base = GAME_INFO + 0x24 * usize::from(i);
        let character = u8_at(ev, base + 0x60)?;
        let kind = match u8_at(ev, base + 0x61)? {
            0 => PlayerKind::Human,
            1 => PlayerKind::Cpu,
            3 => PlayerKind::Empty,
            // 2 is Demo; the game writes nothing above 3, so anything else is
            // kept (not silently dropped) but never counted as Human.
            _ => PlayerKind::Demo,
        };
        let start_stocks = u8_at(ev, base + 0x62)?;
        if kind != PlayerKind::Empty {
            players.push(PlayerStart {
                port: i + 1,
                character,
                kind,
                start_stocks,
            });
        }
    }
    Some(GameStart {
        version: bytes(ev, 0x1)?,
        is_teams: u8_at(ev, GAME_INFO + 0x8)? != 0,
        stage: u16_at(ev, GAME_INFO + 0xE)?,
        timer_secs: u32_at(ev, GAME_INFO + 0x10)?,
        players,
    })
}

pub(crate) struct PostFrame {
    pub frame: i32,
    /// 0-based player index, 0..=3.
    pub index: u8,
    pub is_follower: bool,
    pub percent: f32,
    pub stocks: u8,
}

/// `None` if the event is too short or names a player index outside 0..=3.
pub(crate) fn post_frame(ev: &[u8]) -> Option<PostFrame> {
    let index = u8_at(ev, 0x5).filter(|&i| i < 4)?;
    Some(PostFrame {
        frame: i32_at(ev, 0x1)?,
        index,
        is_follower: u8_at(ev, 0x6)? != 0,
        percent: f32_at(ev, 0x16)?,
        stocks: u8_at(ev, 0x21)?,
    })
}

/// `(method, LRAS initiator as a 0-based index)`; `None` if the method is absent.
/// The initiator field was added in 2.0.0; earlier events read as "not applicable".
pub(crate) fn game_end(ev: &[u8]) -> Option<(u8, Option<u8>)> {
    let method = u8_at(ev, 0x1)?;
    let lras = u8_at(ev, 0x2).filter(|&i| i < 4);
    Some((method, lras))
}

/// Size of the fixed data block of a Message Splitter event (at 0x1).
const SPLITTER_BLOCK: usize = 512;

/// The part of the Gecko List this event carries, if it is a Message Splitter
/// chunk of one: at least 0x205 bytes long, internal command (0x203) Gecko
/// List, and the first `min(actual size at 0x201, 512)` bytes of its block.
/// The list is these chunks concatenated in file order. This is exactly the
/// engine's selection (`melee_data::slp`): a bare Gecko List event is ignored.
pub(crate) fn gecko_chunk(code: u8, ev: &[u8]) -> Option<&[u8]> {
    if code != MESSAGE_SPLITTER || ev.len() < 0x205 || ev[0x203] != GECKO_LIST {
        return None;
    }
    let n = usize::from(u16_at(ev, 0x201)?).min(SPLITTER_BLOCK);
    ev.get(1..1 + n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_table_rejects_zero_sizes() {
        // Event Payloads listing 0x36 with size 2, then 0x42 with size 0.
        let ok = [EVENT_PAYLOADS, 4, 0x36, 0, 2];
        assert!(Events::new(&ok).is_ok());
        let zero = [EVENT_PAYLOADS, 7, 0x36, 0, 2, 0x42, 0, 0, 0x42];
        assert!(matches!(
            Events::new(&zero),
            Err(ReplayError::BadEventSizes)
        ));
    }

    #[test]
    fn pre_2_0_game_end_has_no_lras() {
        assert_eq!(game_end(&[GAME_END, 3]), Some((3, None)));
        assert_eq!(game_end(&[GAME_END]), None);
        assert_eq!(game_end(&[GAME_END, 7, 0xFF]), Some((7, None))); // -1
        assert_eq!(game_end(&[GAME_END, 7, 1]), Some((7, Some(1))));
    }

    #[test]
    fn short_events_read_as_absent() {
        assert!(game_start(&[GAME_START; 0xD3]).is_none());
        assert!(game_start(&[0; 0xD4]).is_some());
        assert!(post_frame(&[0; 0x21]).is_none());
        assert!(post_frame(&[0; 0x22]).is_some());
        let mut far_index = [0; 0x22];
        far_index[0x5] = 4;
        assert!(post_frame(&far_index).is_none());
    }

    #[test]
    fn splitter_gecko_chunk() {
        let mut ev = [0u8; 0x205];
        ev[0] = MESSAGE_SPLITTER;
        ev[1..9].copy_from_slice(b"ABCDEFGH");
        assert_eq!(gecko_chunk(MESSAGE_SPLITTER, &ev), None); // another internal command
        ev[0x203] = GECKO_LIST;
        assert_eq!(gecko_chunk(MESSAGE_SPLITTER, &ev), Some(&[][..])); // actual size 0
        ev[0x202] = 8;
        assert_eq!(gecko_chunk(MESSAGE_SPLITTER, &ev), Some(&b"ABCDEFGH"[..]));
        // Shorter than the 0x205 bytes the spec requires (size table says so).
        assert_eq!(gecko_chunk(MESSAGE_SPLITTER, &ev[..0x204]), None);
        assert_eq!(gecko_chunk(MESSAGE_SPLITTER, &ev[..0x203]), None);
        // An actual size beyond the 512-byte block is capped, never read past.
        ev[0x201..0x203].copy_from_slice(&0xFFFFu16.to_be_bytes());
        assert_eq!(gecko_chunk(MESSAGE_SPLITTER, &ev), Some(&ev[1..513]));
        // A bare Gecko List event is not part of the list (neither is it for the engine).
        assert_eq!(gecko_chunk(GECKO_LIST, &[GECKO_LIST, 0, 1, 2]), None);
        assert_eq!(gecko_chunk(GAME_END, &ev), None);
    }
}

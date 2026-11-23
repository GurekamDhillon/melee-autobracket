//! Fabricates small, spec-shaped `.slp` files for tests.
//!
//! Output follows the real layout (UBJSON framing, an Event Payloads size
//! table, a 3.18-sized Game Start, Post-Frame Updates, Game End, a `metadata`
//! object with `startAt`), so built files and real ones share one code path.
//! Builder misuse (port outside 1..=4) panics: this is test-only code.

use crate::events::{
    EVENT_PAYLOADS, GAME_END, GAME_START, GECKO_LIST, MESSAGE_SPLITTER, POST_FRAME,
};
use crate::PlayerKind;

/// v3.18 payload sizes (excluding the command byte), from the spec's tables.
const GAME_START_SIZE: usize = 0x2F8;
const POST_FRAME_SIZE: usize = 0x54;
const GAME_END_SIZE: usize = 0x6;
const SPLITTER_SIZE: usize = 0x204;
const SPLITTER_BLOCK: usize = 512;

/// A plausible-looking gecko list: one 04-type write, then the end marker.
const GECKO_CODES: &[u8] = &[
    0x04, 0x1A, 0x45, 0x0C, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Debug, Clone)]
struct Frame {
    frame: i32,
    port: u8,
    follower: bool,
    stocks: u8,
    percent: f32,
}

#[derive(Debug, Clone)]
pub struct SlpBuilder {
    stage: u16,
    timer_secs: u32,
    stocks: u8,
    teams: bool,
    gecko: Vec<u8>,
    players: Vec<(u8, u8, PlayerKind)>,
    extra: Vec<(u8, Vec<u8>)>,
    frames: Vec<Frame>,
    end: Option<(u8, Option<u8>)>,
    start_at: String,
    codes: Vec<(u8, String)>,
}

impl Default for SlpBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn index(port: u8) -> usize {
    assert!((1..=4).contains(&port), "port {port} is not 1..=4");
    usize::from(port - 1)
}

impl SlpBuilder {
    /// Final Destination, 8 minutes, 4 stocks, singles, no gecko codes, no players.
    pub fn new() -> Self {
        Self {
            stage: 32,
            timer_secs: 480,
            stocks: 4,
            teams: false,
            gecko: Vec::new(),
            players: Vec::new(),
            extra: Vec::new(),
            frames: Vec::new(),
            end: None,
            start_at: "2026-11-15T20:00:00Z".to_string(),
            codes: Vec::new(),
        }
    }
    pub fn stage(mut self, stage: u16) -> Self {
        self.stage = stage;
        self
    }
    pub fn timer_secs(mut self, secs: u32) -> Self {
        self.timer_secs = secs;
        self
    }
    /// Starting stocks for every player.
    pub fn stocks(mut self, stocks: u8) -> Self {
        self.stocks = stocks;
        self
    }
    pub fn teams(mut self, teams: bool) -> Self {
        self.teams = teams;
        self
    }
    /// Emit a small non-empty Gecko List (`true`) or none (`false`).
    pub fn gecko(self, gecko: bool) -> Self {
        self.gecko_bytes(if gecko { GECKO_CODES } else { &[] })
    }
    /// Emit exactly these Gecko List bytes, as Message Splitter chunks of up to
    /// 512 bytes each like real files (so a longer list spans several events).
    /// Empty means no list.
    pub fn gecko_bytes(mut self, list: &[u8]) -> Self {
        self.gecko = list.to_vec();
        self
    }
    /// Occupy a 1-based port. Unset ports are Empty.
    pub fn player(mut self, port: u8, character: u8, kind: PlayerKind) -> Self {
        index(port);
        self.players.push((port, character, kind));
        self
    }
    /// A leader Post-Frame Update. Events are written in call order.
    pub fn frame(mut self, frame: i32, port: u8, stocks: u8, percent: f32) -> Self {
        index(port);
        self.frames.push(Frame {
            frame,
            port,
            follower: false,
            stocks,
            percent,
        });
        self
    }
    /// A follower (Nana) Post-Frame Update.
    pub fn follower_frame(mut self, frame: i32, port: u8, stocks: u8, percent: f32) -> Self {
        index(port);
        self.frames.push(Frame {
            frame,
            port,
            follower: true,
            stocks,
            percent,
        });
        self
    }
    /// Game End with the given method and 1-based LRAS initiator port.
    pub fn end(mut self, method: u8, lras: Option<u8>) -> Self {
        if let Some(p) = lras {
            index(p);
        }
        self.end = Some((method, lras));
        self
    }
    /// Write `code` as `port`'s connect code in the metadata (`players.<i>.names.code`).
    pub fn code(mut self, port: u8, code: &str) -> Self {
        index(port);
        self.codes.push((port, code.to_string()));
        self
    }
    pub fn start_at(mut self, start_at: &str) -> Self {
        self.start_at = start_at.to_string();
        self
    }
    /// An arbitrary event, written after Game Start (and the gecko list) and
    /// declared in the size table with `payload.len()`.
    pub fn event(mut self, code: u8, payload: Vec<u8>) -> Self {
        self.extra.push((code, payload));
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let mut sizes: Vec<(u8, usize)> = vec![
            (GAME_START, GAME_START_SIZE),
            (POST_FRAME, POST_FRAME_SIZE),
            (GAME_END, GAME_END_SIZE),
            (MESSAGE_SPLITTER, SPLITTER_SIZE),
            (GECKO_LIST, GECKO_CODES.len()),
        ];
        for (code, payload) in &self.extra {
            match sizes.iter().find(|(c, _)| c == code) {
                Some(&(_, size)) => {
                    assert_eq!(
                        size,
                        payload.len(),
                        "event {code:#x} declared with another size"
                    )
                }
                None => sizes.push((*code, payload.len())),
            }
        }

        let mut s = vec![
            EVENT_PAYLOADS,
            u8::try_from(sizes.len() * 3 + 1).expect("size table"),
        ];
        for (code, size) in &sizes {
            s.push(*code);
            s.extend_from_slice(&u16::try_from(*size).expect("event size").to_be_bytes());
        }

        s.extend(self.game_start());
        for (i, chunk) in self.gecko.chunks(SPLITTER_BLOCK).enumerate() {
            let mut ev = vec![0u8; 1 + SPLITTER_SIZE];
            ev[0] = MESSAGE_SPLITTER;
            ev[1..1 + chunk.len()].copy_from_slice(chunk);
            ev[0x201..0x203].copy_from_slice(&(chunk.len() as u16).to_be_bytes());
            ev[0x203] = GECKO_LIST;
            ev[0x204] = u8::from((i + 1) * SPLITTER_BLOCK >= self.gecko.len());
            s.extend(ev);
        }
        for (code, payload) in &self.extra {
            s.push(*code);
            s.extend_from_slice(payload);
        }
        for f in &self.frames {
            let mut ev = vec![0u8; 1 + POST_FRAME_SIZE];
            ev[0] = POST_FRAME;
            ev[0x1..0x5].copy_from_slice(&f.frame.to_be_bytes());
            ev[0x5] = index(f.port) as u8;
            ev[0x6] = u8::from(f.follower);
            ev[0x16..0x1A].copy_from_slice(&f.percent.to_be_bytes());
            ev[0x21] = f.stocks;
            s.extend(ev);
        }
        if let Some((method, lras)) = self.end {
            let mut ev = vec![0u8; 1 + GAME_END_SIZE];
            ev[0] = GAME_END;
            ev[0x1] = method;
            ev[0x2] = lras.map_or(0xFF, |p| index(p) as u8); // int8 -1 = not applicable
            for i in 0..4 {
                let occupied = self
                    .players
                    .iter()
                    .any(|&(p, _, k)| index(p) == i && k != PlayerKind::Empty);
                ev[0x3 + i] = if occupied { 0 } else { 0xFF };
            }
            s.extend(ev);
        }

        let mut file = b"{U\x03raw[$U#l".to_vec();
        file.extend_from_slice(&u32::try_from(s.len()).expect("raw length").to_be_bytes());
        file.extend(s);
        file.extend_from_slice(b"U\x08metadata{");
        ubjson_key(&mut file, "startAt");
        ubjson_string(&mut file, &self.start_at);
        ubjson_key(&mut file, "lastFrame");
        file.push(b'l');
        let last = self.frames.iter().map(|f| f.frame).max().unwrap_or(-123);
        file.extend_from_slice(&last.to_be_bytes());
        ubjson_key(&mut file, "playedOn");
        ubjson_string(&mut file, "dolphin");
        if !self.codes.is_empty() {
            ubjson_key(&mut file, "players");
            file.push(b'{');
            for (port, code) in &self.codes {
                ubjson_key(&mut file, &(port - 1).to_string());
                file.push(b'{');
                ubjson_key(&mut file, "names");
                file.push(b'{');
                ubjson_key(&mut file, "code");
                ubjson_string(&mut file, code);
                file.extend_from_slice(b"}}");
            }
            file.push(b'}');
        }
        file.extend_from_slice(b"}}");
        file
    }

    fn game_start(&self) -> Vec<u8> {
        const INFO: usize = 0x5;
        let mut ev = vec![0u8; 1 + GAME_START_SIZE];
        ev[0] = GAME_START;
        ev[0x1..0x5].copy_from_slice(&[3, 18, 0, 0]);
        ev[INFO + 0x8] = u8::from(self.teams);
        ev[INFO + 0xE..INFO + 0x10].copy_from_slice(&self.stage.to_be_bytes());
        ev[INFO + 0x10..INFO + 0x14].copy_from_slice(&self.timer_secs.to_be_bytes());
        for i in 0..4 {
            let base = INFO + 0x24 * i;
            ev[base + 0x61] = 3; // Empty unless set below
        }
        for &(port, character, kind) in &self.players {
            let base = INFO + 0x24 * index(port);
            ev[base + 0x60] = character;
            ev[base + 0x61] = match kind {
                PlayerKind::Human => 0,
                PlayerKind::Cpu => 1,
                PlayerKind::Demo => 2,
                PlayerKind::Empty => 3,
            };
            ev[base + 0x62] = self.stocks;
        }
        ev[0x1A4] = 0x2; // major scene: VS mode
        ev
    }
}

fn ubjson_key(out: &mut Vec<u8>, key: &str) {
    out.push(b'U');
    out.push(u8::try_from(key.len()).expect("short key"));
    out.extend_from_slice(key.as_bytes());
}

fn ubjson_string(out: &mut Vec<u8>, value: &str) {
    out.push(b'S');
    match u8::try_from(value.len()) {
        Ok(n) => out.extend_from_slice(&[b'U', n]),
        Err(_) => {
            out.push(b'l');
            out.extend_from_slice(
                &u32::try_from(value.len())
                    .expect("string length")
                    .to_be_bytes(),
            );
        }
    }
    out.extend_from_slice(value.as_bytes());
}

/// A legal 4-stock singles game on Final Destination between humans on ports
/// 1 (Fox) and 2 (Marth); `winner_port` takes the last stock of the other.
pub fn simple_game(winner_port: u8, start_at: &str) -> Vec<u8> {
    assert!(
        winner_port == 1 || winner_port == 2,
        "winner_port must be 1 or 2"
    );
    let loser = 3 - winner_port;
    SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(-123, 1, 4, 0.0)
        .frame(-123, 2, 4, 0.0)
        .frame(5400, winner_port, 2, 63.0)
        .frame(5400, loser, 0, 0.0)
        .end(2, None)
        .start_at(start_at)
        .build()
}

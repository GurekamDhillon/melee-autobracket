//! The live relay (docs/design/live-relay.md): the host's engine streams each finalised frame's
//! Slippi event bytes here over a WebSocket, and the bracket page's viewers get them fanned out.
//!
//! Display only. Nothing here is read by judging, the verifier, the organiser queue or any
//! result: a game's result comes only from its `.slp` uploads. The state lives in memory and is
//! gone on a restart, which costs viewers a few seconds of picture and nothing else.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

/// Recent messages a viewer who joins mid-game is sent (besides the game's head): ten seconds.
pub const RECENT_MESSAGES: usize = 600;
/// Largest message the ingest accepts (live-relay.md section 6).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Largest head (the messages before the first frame: Event Payloads, Game Start, Gecko List).
const MAX_HEAD_BYTES: usize = 1024 * 1024;
/// Buffered messages per viewer before a slow viewer starts missing some.
const VIEWER_BUFFER: usize = 1024;
/// Most viewers of one set at once.
pub const MAX_VIEWERS_PER_SET: usize = 64;

/// Slippi's Frame Start event: the first event of every frame, so the end of a game's head.
const FRAME_START: u8 = 0x3A;
/// Slippi's Pre-Frame Update, for streams without Frame Start events.
const PRE_FRAME: u8 = 0x37;

/// Every set's live stream, keyed by `(event, set)`.
#[derive(Default)]
pub struct LiveHub {
    sets: Mutex<HashMap<(i64, i64), Arc<LiveSet>>>,
}

/// One set's stream. The channel lives as long as the set has one, across its games, so a
/// viewer who stays on the page sees the next game begin.
pub struct LiveSet {
    tx: broadcast::Sender<Arc<str>>,
    inner: Mutex<SetInner>,
}

#[derive(Default)]
struct SetInner {
    game: Option<Game>,
    /// Bumped by each ingest connection; an older connection that sees a newer value stops.
    generation: u64,
    viewers: usize,
}

struct Game {
    game_no: i64,
    /// The next cursor the server needs (the one after the last message stored).
    next_cursor: u64,
    /// Messages up to the first frame, for viewers who join later.
    head: Vec<Arc<str>>,
    head_bytes: usize,
    head_done: bool,
    recent: VecDeque<Arc<str>>,
    /// Cursor ranges the sender skipped (`[from, to)`), recorded and otherwise ignored.
    gaps: Vec<(u64, u64)>,
    ended: bool,
    started_at: i64,
    messages: u64,
    event_bytes: u64,
}

/// What the public listing says about a live game.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LiveInfo {
    pub event_id: i64,
    pub set_id: i64,
    pub game_no: i64,
    /// The game's `end_game` has arrived.
    pub ended: bool,
    pub started_at: i64,
    pub messages: u64,
    pub event_bytes: u64,
    pub gaps: usize,
    pub viewers: usize,
}

/// A message from the engine, as the ingest understood it.
#[derive(Debug, PartialEq, Eq)]
pub enum Ingest {
    /// Answer with `connect_reply` carrying this cursor.
    Reply(u64),
    /// Stored (or a duplicate, ignored); nothing to answer.
    Ok,
    /// The connection is to be closed: this text says why.
    Close(&'static str),
}

impl LiveHub {
    /// The stream of `(event, set)`, made when first asked for.
    pub fn set(&self, event: i64, set: i64) -> Arc<LiveSet> {
        let mut sets = self.sets.lock().unwrap_or_else(|e| e.into_inner());
        sets.entry((event, set))
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(VIEWER_BUFFER);
                Arc::new(LiveSet {
                    tx,
                    inner: Mutex::default(),
                })
            })
            .clone()
    }

    /// The stream of `(event, set)` if there is one.
    pub fn get(&self, event: i64, set: i64) -> Option<Arc<LiveSet>> {
        let sets = self.sets.lock().unwrap_or_else(|e| e.into_inner());
        sets.get(&(event, set)).cloned()
    }

    /// Every set with a game streamed, optionally of one event, in set order.
    pub fn list(&self, event: Option<i64>) -> Vec<LiveInfo> {
        let sets: Vec<((i64, i64), Arc<LiveSet>)> = {
            let sets = self.sets.lock().unwrap_or_else(|e| e.into_inner());
            sets.iter()
                .filter(|((e, _), _)| event.is_none_or(|want| *e == want))
                .map(|(k, v)| (*k, v.clone()))
                .collect()
        };
        let mut out: Vec<LiveInfo> = sets
            .into_iter()
            .filter_map(|((event_id, set_id), s)| s.info(event_id, set_id))
            .collect();
        out.sort_by_key(|i| (i.event_id, i.set_id));
        out
    }

    /// Forgets the stream of a set that is decided (or gone to review): its viewers' channel
    /// closes once they drop it.
    pub fn forget(&self, event: i64, set: i64) {
        self.sets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(event, set));
    }
}

impl LiveSet {
    fn lock(&self) -> std::sync::MutexGuard<'_, SetInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn info(&self, event_id: i64, set_id: i64) -> Option<LiveInfo> {
        let inner = self.lock();
        let g = inner.game.as_ref()?;
        Some(LiveInfo {
            event_id,
            set_id,
            game_no: g.game_no,
            ended: g.ended,
            started_at: g.started_at,
            messages: g.messages,
            event_bytes: g.event_bytes,
            gaps: g.gaps.len(),
            viewers: inner.viewers,
        })
    }

    /// A new ingest connection for game `game_no`: it replaces any older one (whose next
    /// message makes it stop). Returns the connection's generation.
    pub fn connect(&self, game_no: i64, now: i64) -> u64 {
        let mut inner = self.lock();
        inner.generation += 1;
        if inner.game.as_ref().is_none_or(|g| g.game_no != game_no) {
            inner.game = Some(Game {
                game_no,
                next_cursor: 0,
                head: Vec::new(),
                head_bytes: 0,
                head_done: false,
                recent: VecDeque::new(),
                gaps: Vec::new(),
                ended: false,
                started_at: now,
                messages: 0,
                event_bytes: 0,
            });
        }
        inner.generation
    }

    /// Whether connection `generation` is still the current one.
    pub fn current(&self, generation: u64) -> bool {
        self.lock().generation == generation
    }

    /// One text message from ingest connection `generation`.
    pub fn ingest(&self, generation: u64, text: &str) -> Ingest {
        if text.len() > MAX_MESSAGE_BYTES {
            return Ingest::Close("message too large");
        }
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Ingest::Close("not a JSON object");
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let mut inner = self.lock();
        if inner.generation != generation {
            return Ingest::Close("replaced by a newer connection");
        }
        let Some(g) = inner.game.as_mut() else {
            return Ingest::Close("no game");
        };
        match kind {
            "connect_request" => return Ingest::Reply(g.next_cursor),
            "start_game" | "game_event" | "end_game" => {}
            // `keepalive`, and anything a later version adds.
            _ => return Ingest::Ok,
        }
        let Some(cursor) = v.get("cursor").and_then(Value::as_u64) else {
            return Ingest::Close("message without a cursor");
        };
        if cursor < g.next_cursor {
            return Ingest::Ok; // resent after a reconnect: already stored
        }
        if cursor > g.next_cursor {
            // The sender skipped ahead to stay current (section 5): accept, record, carry on.
            g.gaps.push((g.next_cursor, cursor));
        }
        let mut out = json!({ "type": kind, "cursor": cursor, "game_no": g.game_no });
        if kind == "game_event" {
            let Some(payload) = v.get("payload").and_then(Value::as_str) else {
                return Ingest::Close("game_event without a payload");
            };
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload) else {
                return Ingest::Close("payload is not base64");
            };
            g.event_bytes += bytes.len() as u64;
            out["payload"] = Value::from(payload);
            let first = bytes.first().copied();
            if matches!(first, Some(FRAME_START | PRE_FRAME)) {
                g.head_done = true;
            }
        }
        if kind == "end_game" {
            g.ended = true;
        }
        g.next_cursor = cursor + 1;
        g.messages += 1;
        let msg: Arc<str> = out.to_string().into();
        if !g.head_done && g.head_bytes + msg.len() <= MAX_HEAD_BYTES {
            g.head_bytes += msg.len();
            g.head.push(msg.clone());
        } else {
            if g.recent.len() == RECENT_MESSAGES {
                g.recent.pop_front();
            }
            g.recent.push_back(msg.clone());
        }
        // No viewers is not an error.
        let _ = self.tx.send(msg);
        Ingest::Ok
    }

    /// A viewer joins: what to send first (the game's head and its recent messages), and the
    /// channel of everything after, subscribed under the same lock so nothing falls between.
    /// `None` when the set already has [`MAX_VIEWERS_PER_SET`] viewers.
    pub fn watch(self: &Arc<Self>) -> Option<Viewer> {
        let mut inner = self.lock();
        if inner.viewers >= MAX_VIEWERS_PER_SET {
            return None;
        }
        inner.viewers += 1;
        let backlog = inner
            .game
            .as_ref()
            .map(|g| g.head.iter().chain(g.recent.iter()).cloned().collect())
            .unwrap_or_default();
        Some(Viewer {
            backlog,
            rx: self.tx.subscribe(),
            set: self.clone(),
        })
    }
}

/// A viewer's subscription; dropping it frees its place.
pub struct Viewer {
    pub backlog: Vec<Arc<str>>,
    pub rx: broadcast::Receiver<Arc<str>>,
    set: Arc<LiveSet>,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let mut inner = self.set.lock();
        inner.viewers = inner.viewers.saturating_sub(1);
    }
}

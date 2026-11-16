//! Helpers for the server's log (the `tracing` events; `main` installs the subscriber).
//!
//! The log never carries secret keys, full public keys ([`key_prefix`] only), room codes or
//! request bodies. Lines about state changes made inside a transaction are collected in a
//! [`Journal`] and written only once the transaction has committed.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use proto::ResimOutcome;
use tracing::{info, warn};

/// Characters of a public key shown in the log.
pub const KEY_PREFIX_CHARS: usize = 8;
/// Hex digits of a replay's SHA-256 shown in the log.
pub const SHA_PREFIX_CHARS: usize = 12;
/// Longest client-supplied label (an engine build) shown in the log.
const MAX_LABEL_CHARS: usize = 64;

/// The first [`KEY_PREFIX_CHARS`] characters of a key: enough to tell keys apart in the log.
pub fn key_prefix(key: &str) -> String {
    clean(key, KEY_PREFIX_CHARS)
}

/// The first [`SHA_PREFIX_CHARS`] hex digits of a digest.
pub fn sha_prefix(sha256: &str) -> &str {
    sha256.get(..SHA_PREFIX_CHARS).unwrap_or(sha256)
}

/// A client-supplied label as logged: [`clean`], cut to [`MAX_LABEL_CHARS`] characters.
pub fn label(s: &str) -> String {
    clean(s, MAX_LABEL_CHARS)
}

/// Longest error or engine message shown in the log.
pub const MAX_DETAIL_CHARS: usize = 300;

/// Text from outside the process (players, verifiers, the engine, stored rows) made safe for a
/// one-line log record: cut to `max_chars` characters, then every character that is not
/// printable (control characters, the line and paragraph separators U+2028/U+2029, NEL, and
/// anything else `char::escape_debug` would escape) written as its Rust escape (`\n`,
/// `\u{1b}`). The log formatter does not escape `%` fields itself, so without this a newline
/// in a field would start a forged log line.
pub fn clean(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().take(max_chars) {
        match c {
            // `escape_debug` escapes these too, but they cannot break a line.
            '\\' | '"' | '\'' => out.push(c),
            _ => out.extend(c.escape_debug()),
        }
    }
    out
}

/// An error or engine message as logged: [`clean`], cut to [`MAX_DETAIL_CHARS`] characters.
pub fn detail(s: impl std::fmt::Display) -> String {
    clean(&s.to_string(), MAX_DETAIL_CHARS)
}

/// Whether text from a request may be stored: at most `max_chars` characters and nothing
/// [`clean`] would have to escape except quotes and backslashes (so no control characters,
/// line or paragraph separators, or other unprintable characters).
pub fn is_printable_within(s: &str, max_chars: usize) -> bool {
    s.chars().count() <= max_chars
        && s.chars()
            .all(|c| matches!(c, '\\' | '"' | '\'') || c.escape_debug().len() == 1)
}

/// The variant name of a re-simulation outcome.
pub fn resim_kind(r: &ResimOutcome) -> &'static str {
    match r {
        ResimOutcome::Match => "Match",
        ResimOutcome::Diverged { .. } => "Diverged",
        ResimOutcome::EarlyEnd { .. } => "EarlyEnd",
        ResimOutcome::Crashed(_) => "Crashed",
        ResimOutcome::Unavailable(_) => "Unavailable",
        ResimOutcome::Skipped => "Skipped",
    }
}

/// Least time between two summaries of rejected requests.
const REJECTION_SUMMARY_PERIOD: Duration = Duration::from_secs(60);

/// Counts requests refused for a missing or bad signature or a reused nonce. They are not
/// logged one by one (anyone can send them, so that would let anyone flood the log): at
/// most one summary line per [`REJECTION_SUMMARY_PERIOD`] reports how many there were.
#[derive(Default)]
pub struct Rejections(Mutex<RejectionState>);

#[derive(Default)]
struct RejectionState {
    /// Rejections not yet reported.
    pending: u64,
    last_summary: Option<Instant>,
}

impl Rejections {
    pub fn record(&self) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.pending += 1;
        if s.last_summary
            .is_none_or(|t| t.elapsed() >= REJECTION_SUMMARY_PERIOD)
        {
            warn!(
                count = s.pending,
                period_secs = REJECTION_SUMMARY_PERIOD.as_secs(),
                "rejected unsigned or badly signed requests"
            );
            s.pending = 0;
            s.last_summary = Some(Instant::now());
        }
    }
}

/// Log lines about changes made in a transaction, written by [`Journal::emit`] after it
/// commits (and never, if it rolls back).
#[derive(Default)]
#[must_use = "a journal logs nothing until emitted"]
pub struct Journal(Vec<Entry>);

enum Entry {
    QueueOpened {
        id: i64,
        event_id: i64,
        set_id: u32,
        reason: String,
    },
    SetDecided {
        event_id: i64,
        set_id: u32,
        winner: String,
        score: [i64; 2],
        how: &'static str,
    },
    EventCompleted {
        event_id: i64,
        champion: String,
    },
}

/// The log target of journal lines: the state changes they record are made there.
const TARGET: &str = "autobracket_server::lifecycle";

impl Journal {
    pub fn queue_opened(&mut self, id: i64, event_id: i64, set_id: u32, reason: &str) {
        self.0.push(Entry::QueueOpened {
            id,
            event_id,
            set_id,
            reason: reason.to_owned(),
        });
    }

    /// `how` is `played`, or how an organiser decided it (`award`, `dq`, `double-dq`).
    pub fn set_decided(
        &mut self,
        event_id: i64,
        set_id: u32,
        winner: String,
        score: [i64; 2],
        how: &'static str,
    ) {
        self.0.push(Entry::SetDecided {
            event_id,
            set_id,
            winner,
            score,
            how,
        });
    }

    pub fn event_completed(&mut self, event_id: i64, champion: String) {
        self.0.push(Entry::EventCompleted { event_id, champion });
    }

    /// Writes the collected lines, in order.
    pub fn emit(self) {
        for entry in self.0 {
            match entry {
                Entry::QueueOpened {
                    id,
                    event_id,
                    set_id,
                    reason,
                } => info!(
                    target: TARGET,
                    id,
                    event = event_id,
                    set = set_id,
                    reason = %label(&reason),
                    "queue item opened"
                ),
                Entry::SetDecided {
                    event_id,
                    set_id,
                    winner,
                    score: [w0, w1],
                    how,
                } => info!(
                    target: TARGET,
                    event = event_id,
                    set = set_id,
                    winner = %label(&winner),
                    score = %format_args!("{w0}-{w1}"),
                    how = %how,
                    "set decided"
                ),
                Entry::EventCompleted { event_id, champion } => {
                    info!(target: TARGET, event = event_id, champion = %label(&champion), "event completed")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_short() {
        let key = "ab".repeat(32);
        assert_eq!(key_prefix(&key), "abababab");
        assert_eq!(key_prefix("abc"), "abc");
        assert_eq!(sha_prefix(&"0f".repeat(32)), "0f0f0f0f0f0f");
        assert_eq!(label(&"x".repeat(100)).len(), 64);
        assert_eq!(key_prefix("ab\ncdefghij"), "ab\\ncdefg");
    }

    #[test]
    fn label_escapes_everything_that_could_break_a_line() {
        let raw = "a\nb\rc\u{1b}[2Kd\u{2028}e\u{2029}f\u{85}g\th\u{0}i";
        let out = label(raw);
        assert_eq!(
            out,
            "a\\nb\\rc\\u{1b}[2Kd\\u{2028}e\\u{2029}f\\u{85}g\\th\\0i"
        );
        assert!(!out.chars().any(|c| c.is_control()));
        // Printable text, quotes and backslashes pass through.
        assert_eq!(label(r#"melee "1.0" \ build"#), r#"melee "1.0" \ build"#);
        assert_eq!(label("Ünïcødé 🎮"), "Ünïcødé 🎮");
        // Cut to 64 characters before escaping.
        assert_eq!(label(&"\n".repeat(100)), "\\n".repeat(64));
        assert_eq!(clean("é".repeat(10).as_str(), 3), "ééé");
    }

    #[test]
    fn rejections_are_summarised_at_most_once_a_minute() {
        let r = Rejections::default();
        r.record();
        // The first one was summarised at once; the next ones wait for the next summary.
        r.record();
        r.record();
        let s = r.0.lock().unwrap();
        assert_eq!(s.pending, 2);
        assert!(s.last_summary.is_some());
    }
}

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

/// Whether `c` must not appear raw in a one-line log record or in stored text: a control
/// character (`\n`, `\r`, ESC, NUL, NEL, ...), the line and paragraph separators U+2028 and
/// U+2029, or a Unicode bidirectional control (U+202A-U+202E, U+2066-U+2069, U+200E, U+200F,
/// U+061C), which can make a line read differently from what it holds. Everything else
/// (combining marks, any script, emoji sequences) is ordinary text. [`clean`] escapes exactly
/// these and [`is_printable_within`] refuses exactly these.
pub fn needs_escape(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{61c}'
        )
}

/// Text from outside the process (players, verifiers, the engine, stored rows) made safe for a
/// one-line log record: cut to `max_chars` characters, then every character [`needs_escape`]
/// names written as its Rust escape (`\n`, `\u{1b}`, `\u{202e}`). The log formatter does not
/// escape `%` fields itself, so without this a newline in a field would start a forged log line.
pub fn clean(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().take(max_chars) {
        if needs_escape(c) {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

/// An error or engine message as logged: [`clean`], cut to [`MAX_DETAIL_CHARS`] characters.
pub fn detail(s: impl std::fmt::Display) -> String {
    clean(&s.to_string(), MAX_DETAIL_CHARS)
}

/// Whether text from a request may be stored: at most `max_chars` characters and nothing
/// [`clean`] would have to escape (see [`needs_escape`]).
pub fn is_printable_within(s: &str, max_chars: usize) -> bool {
    s.chars().count() <= max_chars && !s.chars().any(needs_escape)
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

    /// Ordinary text in any script is accepted and logged as-is: combining marks, Thai,
    /// Devanagari, emoji with a variation selector or a zero-width joiner.
    #[test]
    fn ordinary_text_in_any_script_is_accepted_as_is() {
        for ok in [
            "Cafe\u{301} Melee",                          // combining acute accent
            "\u{e21}\u{e27}\u{e22}\u{e44}\u{e17}\u{e22}", // Thai
            "\u{915}\u{94d}\u{937}",                      // Devanagari conjunct (virama)
            "\u{2764}\u{fe0f} netplay",                   // emoji + variation selector
            "\u{1f468}\u{200d}\u{1f4bb}",                 // emoji ZWJ sequence
            "Ünïcødé 🎮",
        ] {
            assert!(is_printable_within(ok, 80), "{ok:?}");
            assert_eq!(label(ok), ok, "{ok:?}");
        }
    }

    /// Control characters, the line and paragraph separators and the bidirectional controls
    /// are refused, and escaped by the same predicate.
    #[test]
    fn controls_separators_and_bidi_controls_are_refused_and_escaped() {
        for c in [
            '\n', '\r', '\t', '\0', '\u{1b}', '\u{7f}', '\u{85}', '\u{2028}', '\u{2029}',
            '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{61c}',
        ] {
            let s = format!("a{c}b");
            assert!(!is_printable_within(&s, 80), "{c:?}");
            let out = label(&s);
            assert!(!out.contains(c), "{c:?} kept in {out:?}");
            assert!(out.starts_with('a') && out.ends_with('b'), "{out:?}");
        }
        assert_eq!(label("x\u{202e}y"), "x\\u{202e}y");
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

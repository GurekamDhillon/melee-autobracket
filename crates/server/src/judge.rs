//! Verdict logic: decides from the two uploaded replays of one game whether
//! it counts and who won. Pure functions; the inputs are untrusted bytes.

use proto::{ResimOutcome, Ruleset, Verdict};
use replay::{GameEnd, PlayerKind, ReplayFacts};

/// The outcome of judging an upload pair before any resimulation.
#[derive(Debug, Clone, PartialEq)]
pub enum Pre {
    /// Decided without resim: `Conflicted`, `Single`, `Illegal(_)` or `Undecided`.
    Final(Verdict),
    /// The pair agrees on a legal game with a winner; resim decides how sure we are.
    NeedsResim { winner_port: u8, last_frame: i32 },
}

/// Checks a replay against the ruleset. The first failing rule wins and is
/// returned as its word: `players`, `teams`, `stage`, `stocks`, `timer`,
/// `character`, `gecko` (a Gecko code list whose SHA-256 is not in
/// `rules.allowed_gecko`).
pub fn legality(f: &ReplayFacts, rules: &Ruleset) -> Result<(), &'static str> {
    // Exactly two players, both human, on ports 1 and 2. `players` lists every
    // non-Empty port, so any CPU, Demo or third human makes this fail.
    let two_humans = matches!(
        &f.players[..],
        [a, b] if a.port == 1 && b.port == 2
            && a.kind == PlayerKind::Human && b.kind == PlayerKind::Human
    );
    if !two_humans {
        return Err("players");
    }
    if f.is_teams {
        return Err("teams");
    }
    if !rules.legal_stages.contains(&f.stage) {
        return Err("stage");
    }
    if f.players.iter().any(|p| p.start_stocks != rules.stocks) {
        return Err("stocks");
    }
    if f.timer_secs != rules.timer_secs {
        return Err("timer");
    }
    if f.players
        .iter()
        .any(|p| rules.banned_characters.contains(&p.character))
    {
        return Err("character");
    }
    // No list (this project's own exports) is legal; a list must be on the
    // allow-list, compared exactly (lowercase hex SHA-256).
    if let Some(sha) = &f.gecko_sha256 {
        if !rules.allowed_gecko.contains(sha) {
            return Err("gecko");
        }
    }
    Ok(())
}

/// Judges the uploads for one game (host's slot first).
///
/// Fewer than two uploads is `Single`, whatever the content. Two uploads must
/// carry byte-identical `raw` event streams (metadata may differ); otherwise,
/// or if either stream cannot be located, the pair is `Conflicted`.
pub fn pre_judge(uploads: [Option<&[u8]>; 2], rules: &Ruleset) -> Pre {
    let [Some(a), Some(b)] = uploads else {
        return Pre::Final(Verdict::Single);
    };
    match (replay::raw_element(a), replay::raw_element(b)) {
        (Ok(ra), Ok(rb)) if ra == rb => {}
        _ => return Pre::Final(Verdict::Conflicted),
    }
    // Identical raw streams parse identically, so one parse speaks for both.
    let Ok(facts) = replay::parse(a) else {
        return Pre::Final(Verdict::Undecided);
    };
    if let Err(word) = legality(&facts, rules) {
        return Pre::Final(Verdict::Illegal(word.to_string()));
    }
    // No frames at all (`last_frame` keeps the parser's `i32::MIN` sentinel): nothing was
    // played, so there is nothing to re-simulate and no winner, whatever Game End says.
    if facts.last_frame == i32::MIN {
        return Pre::Final(Verdict::Undecided);
    }
    // Only TIME! (1), GAME! (2) and No Contest (7, decided by the LRAS
    // initiator) can produce an accepted winner.
    if !matches!(
        facts.end,
        Some(GameEnd {
            method: 1 | 2 | 7,
            ..
        })
    ) {
        return Pre::Final(Verdict::Undecided);
    }
    match replay::winner_port(&facts) {
        Some(winner_port) => Pre::NeedsResim {
            winner_port,
            last_frame: facts.last_frame,
        },
        None => Pre::Final(Verdict::Undecided),
    }
}

/// Turns a resim outcome into the final verdict for a game that needed one. `verifier_on` is
/// whether the server is configured to re-simulate (`verifier.mode` not `off`): then a game
/// the verifier could not check (`Unavailable`) does not count under `strict_resim`
/// (`VerifierUnavailable`), and counts on peer agreement alone (`PeerOnly`) otherwise.
pub fn finish(resim: &ResimOutcome, strict_resim: bool, verifier_on: bool) -> Verdict {
    match resim {
        ResimOutcome::Match => Verdict::Verified,
        ResimOutcome::Diverged { .. }
        | ResimOutcome::EarlyEnd { .. }
        | ResimOutcome::Crashed(_) => {
            if strict_resim {
                Verdict::Inexact
            } else {
                Verdict::VerifiedInexact
            }
        }
        ResimOutcome::Unavailable(_) if verifier_on && strict_resim => Verdict::VerifierUnavailable,
        ResimOutcome::Unavailable(_) | ResimOutcome::Skipped => Verdict::PeerOnly,
    }
}

/// Maps a winning port to its bracket slot: port 1 => slot 0 (host), port 2 =>
/// slot 1. `pre_judge` only yields ports 1 and 2 (legality enforces it).
pub fn winner_slot(winner_port: u8) -> u8 {
    debug_assert!(matches!(winner_port, 1 | 2), "port {winner_port}");
    u8::from(winner_port == 2)
}

/// What the server knows about one stored upload, besides its bytes.
#[derive(Debug, Clone, Default)]
pub struct UploadMeta {
    /// The tag of the player in this slot, if known.
    pub tag: Option<String>,
    /// SHA-256 of the file, lowercase hex.
    pub sha256: String,
    /// The `x-ab-build` the client sent with it.
    pub client_build: String,
}

/// What one uploaded file says, for the organiser: its slot and player, a hash prefix, and
/// whether it parses; for a readable file also the stage, timer, each player's port,
/// character, start and final stocks and final percent, how the game ended, the winning port
/// the replay derives, and the first rule it breaks (`"ok"` when legal). `file` is `None` when
/// the stored file could not be read. Text from outside the server is [`clean`]ed.
///
/// [`clean`]: crate::logging::clean
pub fn upload_evidence(
    slot: u8,
    meta: &UploadMeta,
    file: Option<&[u8]>,
    rules: &Ruleset,
) -> serde_json::Value {
    use crate::logging::{clean, label, sha_prefix};
    let mut ev = serde_json::json!({
        "slot": slot,
        "tag": meta.tag.as_deref().map(label),
        "sha256": label(sha_prefix(&meta.sha256)),
        "client_build": label(&meta.client_build),
    });
    let facts = match file.map(replay::parse) {
        None => {
            ev["parse"] = "stored file unreadable".into();
            return ev;
        }
        Some(Err(e)) => {
            ev["parse"] = clean(&e.to_string(), crate::logging::MAX_DETAIL_CHARS).into();
            return ev;
        }
        Some(Ok(facts)) => facts,
    };
    let players: Vec<serde_json::Value> = facts
        .players
        .iter()
        .map(|p| {
            let last = facts.finals.iter().find(|f| f.port == p.port);
            serde_json::json!({
                "port": p.port,
                "character": p.character,
                "kind": format!("{:?}", p.kind),
                "start_stocks": p.start_stocks,
                "final_stocks": last.map(|f| f.stocks),
                "final_percent": last.map(|f| f.percent),
            })
        })
        .collect();
    ev["parse"] = "ok".into();
    ev["stage"] = facts.stage.into();
    ev["timer_secs"] = facts.timer_secs.into();
    ev["players"] = players.into();
    ev["last_frame"] = (facts.last_frame != i32::MIN)
        .then_some(facts.last_frame)
        .into();
    ev["end_method"] = facts.end.as_ref().map(|e| e.method).into();
    ev["lras_initiator"] = facts.end.as_ref().and_then(|e| e.lras_initiator).into();
    ev["winner_port"] = replay::winner_port(&facts).into();
    ev["legality"] = legality(&facts, rules).err().unwrap_or("ok").into();
    ev
}

/// The last frame to re-simulate a lone upload to, as evidence: `Some` when the file parses,
/// is legal under `rules` and has frames, so the engine could check it.
pub fn evidence_resim_frame(file: &[u8], rules: &Ruleset) -> Option<i32> {
    let facts = replay::parse(file).ok()?;
    (legality(&facts, rules).is_ok() && facts.last_frame != i32::MIN).then_some(facts.last_frame)
}

/// A re-simulation outcome for the organiser: its kind and a one-line description, cleaned
/// (an external verifier writes the strings in it).
pub fn resim_evidence(outcome: &ResimOutcome) -> (&'static str, String) {
    use crate::logging::{clean, resim_kind, MAX_DETAIL_CHARS};
    let text = match outcome {
        ResimOutcome::Match => String::new(),
        ResimOutcome::Diverged {
            frame,
            port,
            field,
            engine_value,
            replay_value,
        } => format!(
            "frame {frame}, port {port}, {field}: engine {engine_value}, replay {replay_value}"
        ),
        ResimOutcome::EarlyEnd {
            engine_last,
            replay_last,
        } => format!("the engine stopped at frame {engine_last}; the replay ends at {replay_last}"),
        ResimOutcome::Crashed(why) | ResimOutcome::Unavailable(why) => why.clone(),
        ResimOutcome::Skipped => "verifier off".into(),
    };
    (resim_kind(outcome), clean(&text, MAX_DETAIL_CHARS))
}

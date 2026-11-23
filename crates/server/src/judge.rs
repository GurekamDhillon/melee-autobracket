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
    legality_with(f, rules, false)
}

/// [`legality`], with `any_ports` true when the two players were identified by connect code
/// (see [`resolve_slots`]): then they may be on any two ports.
pub fn legality_with(
    f: &ReplayFacts,
    rules: &Ruleset,
    any_ports: bool,
) -> Result<(), &'static str> {
    // Exactly two players, both human, on ports 1 and 2 (any two when identified by code).
    // `players` lists every non-Empty port, so any CPU, Demo or third human makes this fail.
    let two_humans = matches!(
        &f.players[..],
        [a, b] if (any_ports || (a.port == 1 && b.port == 2))
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
    pre_judge_coded(uploads, rules, &[None, None]).0
}

/// Which port of a replay is which bracket slot, and how that was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotMap {
    /// The port (1-based) of slot 0 (the host's slot) and of slot 1.
    pub ports: [u8; 2],
    /// `true` when identified by connect code, `false` for the port rule (port 1 is slot 0,
    /// port 2 is slot 1).
    pub by_code: bool,
    /// Why: the evidence shown to the organiser.
    pub note: String,
}

impl SlotMap {
    fn port_rule(note: impl Into<String>) -> Self {
        SlotMap {
            ports: [1, 2],
            by_code: false,
            note: note.into(),
        }
    }

    /// The slot (0 or 1) playing on `port`.
    pub fn slot_of(&self, port: u8) -> u8 {
        u8::from(self.ports[1] == port)
    }
}

/// Maps the replay's two human ports to the set's slots by connect code. `slot_codes` are the
/// players' registered codes, normalised. Any-client mode only.
///
/// - Both slots have a code and the replay carries a code on both human ports: each slot is
///   the port whose code equals its own, on any two ports. If the replay's codes are not the
///   set's players' codes the result is `Err` (judged `Illegal("codes")`).
/// - Otherwise (a slot has no registered code, or the replay has none or only one) the port
///   rule applies, and the note says why.
pub fn resolve_slots(f: &ReplayFacts, slot_codes: &[Option<String>; 2]) -> Result<SlotMap, String> {
    let humans: Vec<u8> = f
        .players
        .iter()
        .filter(|p| p.kind == PlayerKind::Human)
        .map(|p| p.port)
        .collect();
    let code_of = |port: u8| f.codes.iter().find(|(p, _)| *p == port).map(|(_, c)| c.as_str());
    if let Some(n) = slot_codes.iter().position(Option::is_none) {
        return Ok(SlotMap::port_rule(format!(
            "port rule: slot {n} has no registered connect code"
        )));
    }
    let [a, b] = humans[..] else {
        return Ok(SlotMap::port_rule("port rule: the replay does not have two human players"));
    };
    let (ca, cb) = (code_of(a), code_of(b));
    if ca.is_none() && cb.is_none() {
        return Ok(SlotMap::port_rule("port rule: the replay carries no connect codes"));
    }
    let [Some(s0), Some(s1)] = slot_codes.each_ref().map(|c| c.as_deref()) else {
        unreachable!("both slot codes are present")
    };
    let find = |code: &str| [a, b].into_iter().find(|p| code_of(*p) == Some(code));
    match (find(s0), find(s1)) {
        (Some(p0), Some(p1)) if p0 != p1 => Ok(SlotMap {
            ports: [p0, p1],
            by_code: true,
            note: format!("connect codes: slot 0 on port {p0}, slot 1 on port {p1}"),
        }),
        _ if ca.is_none() || cb.is_none() => Ok(SlotMap::port_rule(
            "port rule: the replay carries a connect code for only one player",
        )),
        _ => Err("the replay's connect codes are not those of the set's players".to_string()),
    }
}

/// [`pre_judge`] that also says which port is which slot. `slot_codes` are the set's two
/// players' registered connect codes (normalised, `None` when unknown); they matter only in
/// any-client mode, where players may sit on any two ports and are identified by code.
pub fn pre_judge_coded(
    uploads: [Option<&[u8]>; 2],
    rules: &Ruleset,
    slot_codes: &[Option<String>; 2],
) -> (Pre, SlotMap) {
    let mut map = SlotMap::port_rule("port rule: port 1 is the host, port 2 the guest");
    let [Some(a), Some(b)] = uploads else {
        return (Pre::Final(Verdict::Single), map);
    };
    let fin = |v: Verdict, map: SlotMap| (Pre::Final(v), map);
    let facts = if rules.any_client {
        // Different clients need not write byte-identical streams: the pair agrees when both
        // parse and derive the same outcome. Legality is then checked on each.
        match (replay::parse(a), replay::parse(b)) {
            (Ok(fa), Ok(fb)) if outcome(&fa) == outcome(&fb) => {
                let (ma, mb) = match (resolve_slots(&fa, slot_codes), resolve_slots(&fb, slot_codes)) {
                    (Ok(ma), Ok(mb)) => (ma, mb),
                    (Err(note), _) | (_, Err(note)) => {
                        return fin(
                            Verdict::Illegal("codes".to_string()),
                            SlotMap { by_code: true, note, ..map },
                        )
                    }
                };
                if ma.ports != mb.ports {
                    return fin(Verdict::Conflicted, ma);
                }
                map = if ma.by_code { ma } else { mb };
                for f in [&fb, &fa] {
                    if let Err(word) = legality_with(f, rules, map.by_code) {
                        return fin(Verdict::Illegal(word.to_string()), map);
                    }
                }
                fa
            }
            (Err(_), Err(_)) => return fin(Verdict::Undecided, map),
            _ => return fin(Verdict::Conflicted, map),
        }
    } else {
        match (replay::raw_element(a), replay::raw_element(b)) {
            (Ok(ra), Ok(rb)) if ra == rb => {}
            _ => return fin(Verdict::Conflicted, map),
        }
        // Identical raw streams parse identically, so one parse speaks for both.
        let Ok(facts) = replay::parse(a) else {
            return fin(Verdict::Undecided, map);
        };
        if let Err(word) = legality(&facts, rules) {
            return fin(Verdict::Illegal(word.to_string()), map);
        }
        facts
    };
    if undecided_reason(&facts, rules.any_client).is_some() {
        return fin(Verdict::Undecided, map);
    }
    match replay::winner_port(&facts) {
        Some(winner_port) => (
            Pre::NeedsResim {
                winner_port,
                last_frame: facts.last_frame,
            },
            map,
        ),
        None => fin(Verdict::Undecided, map),
    }
}

/// Whether exactly one of the two human players ended on zero stocks and the other did not:
/// a decided stock-out, whatever the file's end method says.
fn stock_out(f: &ReplayFacts) -> bool {
    let stocks: Vec<u8> = f
        .players
        .iter()
        .filter(|p| p.kind == PlayerKind::Human)
        .filter_map(|p| f.finals.iter().find(|x| x.port == p.port).map(|x| x.stocks))
        .collect();
    matches!(stocks[..], [a, b] if (a == 0) != (b == 0))
}

/// Why a legal replay has no clear result (the organiser decides such a game), or `None` when
/// it has one. Only TIME! (1), GAME! (2) and No Contest (7, an LRAS quit: the quitter
/// forfeits) can produce a winner; every other ending, a file without a Game End, a game with
/// no frames and a dead tie stays undecided.
///
/// One addition in any-client mode (`any_client`): Slippi before 2.0 wrote end method 3
/// ("RESOLVED") for a game that ended in a stock-out (on real tournament replays 485 of 486
/// such files end with one player on zero stocks). Such a file counts, but only when one
/// player really is on zero stocks and the other is not; any other method-3 file, and method 0
/// ("unresolved"), stays undecided.
pub fn undecided_reason(f: &ReplayFacts, any_client: bool) -> Option<String> {
    // No frames at all (`last_frame` keeps the parser's `i32::MIN` sentinel): nothing was
    // played, so there is nothing to re-simulate and no winner, whatever Game End says.
    if f.last_frame == i32::MIN {
        return Some("no frames".into());
    }
    match &f.end {
        None => Some("no Game End (the file stops before the game ended)".into()),
        Some(GameEnd { method: 7, lras_initiator: None }) => {
            Some("No Contest without an LRAS initiator".into())
        }
        Some(GameEnd { method: 3, .. }) if any_client => {
            if stock_out(f) {
                None
            } else {
                Some("end method 3 (RESOLVED) without a stock-out".into())
            }
        }
        Some(GameEnd { method: 1 | 2 | 7, .. }) => match replay::winner_port(f) {
            Some(_) => None,
            None if f.end.as_ref().is_some_and(|e| e.method == 7) => {
                Some("LRAS by a port that is not one of the two players".into())
            }
            None => Some("tie (equal stocks and percent) or a player's final state missing".into()),
        },
        Some(GameEnd { method, .. }) => Some(format!("game ended by method {method}")),
    }
}

/// What two uploads must agree on in any-client mode: the derived result of the game, not its
/// bytes. Stage; each player's port, character and final stocks; how the game ended (method
/// and LRAS initiator); the winning port; and the last frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub stage: u16,
    pub players: Vec<(u8, u8, Option<u8>)>,
    pub end: Option<(u8, Option<u8>)>,
    pub winner_port: Option<u8>,
    pub last_frame: i32,
}

/// The derived outcome of one parsed replay.
pub fn outcome(f: &ReplayFacts) -> Outcome {
    Outcome {
        stage: f.stage,
        players: f
            .players
            .iter()
            .map(|p| {
                let last = f.finals.iter().find(|x| x.port == p.port);
                (p.port, p.character, last.map(|x| x.stocks))
            })
            .collect(),
        end: f.end.as_ref().map(|e| (e.method, e.lras_initiator)),
        winner_port: replay::winner_port(f),
        last_frame: f.last_frame,
    }
}

/// Turns a resim outcome into the final verdict for a game that needed one. `verifier_on` is
/// whether the server is configured to re-simulate (`verifier.mode` not `off`): then a game
/// the verifier could not check (`Unavailable`) does not count under `strict_resim`
/// (`VerifierUnavailable`), and counts on peer agreement alone (`PeerOnly`) otherwise.
///
/// In any-client mode (`any_client`) a re-simulation that diverged, ended early or crashed is
/// `UnverifiedForeign` (the replay may come from another client): it counts on peer agreement
/// and the ruleset, and is quarantined for the organiser, whatever `strict_resim` says.
pub fn finish(
    resim: &ResimOutcome,
    strict_resim: bool,
    verifier_on: bool,
    any_client: bool,
) -> Verdict {
    match resim {
        ResimOutcome::Match => Verdict::Verified,
        ResimOutcome::Diverged { .. } | ResimOutcome::EarlyEnd { .. } | ResimOutcome::Crashed(_)
            if any_client =>
        {
            Verdict::UnverifiedForeign
        }
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
    /// The registered connect codes of the set's two players (normalised), if known; used to
    /// say which port is which slot in any-client events.
    pub slot_codes: [Option<String>; 2],
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
    if rules.any_client {
        match resolve_slots(&facts, &meta.slot_codes) {
            Ok(map) => {
                ev["mapping"] = clean(&map.note, crate::logging::MAX_DETAIL_CHARS).into();
                ev["legality"] = legality_with(&facts, rules, map.by_code)
                    .err()
                    .unwrap_or("ok")
                    .into();
            }
            Err(note) => {
                ev["mapping"] = clean(&note, crate::logging::MAX_DETAIL_CHARS).into();
                ev["legality"] = "codes".into();
            }
        }
        ev["codes"] = facts.codes.iter().map(|(p, c)| format!("port {p}: {c}")).collect::<Vec<_>>().into();
    } else {
        ev["legality"] = legality(&facts, rules).err().unwrap_or("ok").into();
    }
    if legality_ok(&ev) {
        if let Some(why) = undecided_reason(&facts, rules.any_client) {
            ev["undecided_reason"] = clean(&why, crate::logging::MAX_DETAIL_CHARS).into();
        }
    }
    ev
}

fn legality_ok(ev: &serde_json::Value) -> bool {
    ev["legality"] == "ok"
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

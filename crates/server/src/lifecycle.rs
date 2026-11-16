//! Event state transitions over the store: starting an event, applying verdicts, and
//! awarding sets. Each function does its reads and writes in a single transaction where it
//! changes state. The organiser queue and its resolutions are in [`crate::review`].

use axum::http::StatusCode;
use bracket::{Bracket, BracketError, Format, SetId, SetState, Side, Slot};
use proto::{ResimOutcome, Ruleset, Verdict};
use sqlx::{SqliteConnection, SqlitePool};

use crate::db::{begin_write, now_unix};
use crate::review::open_queue_item;
use crate::routes::ApiError;
use crate::state::AppState;

/// Most entrants one event accepts; the bracket builder is never handed more.
pub const MAX_ENTRANTS: i64 = 1024;

#[derive(thiserror::Error, Debug)]
pub enum LifecycleError {
    #[error("no such event")]
    NoSuchEvent,
    #[error("event has already started")]
    AlreadyStarted,
    #[error("event has not started")]
    NotStarted,
    #[error("need at least 2 entrants")]
    TooFewEntrants,
    #[error("set cannot be awarded: {0}")]
    Bracket(#[from] BracketError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
}

impl From<LifecycleError> for ApiError {
    fn from(e: LifecycleError) -> Self {
        match e {
            LifecycleError::NoSuchEvent => ApiError::new(StatusCode::NOT_FOUND, e.to_string()),
            LifecycleError::AlreadyStarted
            | LifecycleError::NotStarted
            | LifecycleError::TooFewEntrants
            | LifecycleError::Bracket(_) => ApiError::new(StatusCode::CONFLICT, e.to_string()),
            LifecycleError::Db(_) | LifecycleError::Corrupt(_) => ApiError::internal(e),
        }
    }
}

/// The text stored in `events.format`.
pub fn format_to_str(f: Format) -> &'static str {
    match f {
        Format::SingleElim => "single_elim",
        Format::DoubleElim => "double_elim",
    }
}

pub fn format_from_str(s: &str) -> Option<Format> {
    match s {
        "single_elim" => Some(Format::SingleElim),
        "double_elim" => Some(Format::DoubleElim),
        _ => None,
    }
}

/// Moves an event from `signup` to `running`: builds the bracket over the
/// entrants in seed order, stores it, and opens a `sets` row for each Ready set.
pub async fn start_event(st: &AppState, event_id: i64) -> Result<(), LifecycleError> {
    let mut tx = begin_write(&st.db).await?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT state, format FROM events WHERE id = ?")
            .bind(event_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (state, format) = row.ok_or(LifecycleError::NoSuchEvent)?;
    if state != "signup" {
        return Err(LifecycleError::AlreadyStarted);
    }
    let format = format_from_str(&format)
        .ok_or_else(|| LifecycleError::Corrupt(format!("event format {format:?}")))?;

    let entrants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entrants WHERE event_id = ?")
        .bind(event_id)
        .fetch_one(&mut *tx)
        .await?;
    if entrants > MAX_ENTRANTS {
        return Err(LifecycleError::Corrupt(format!("{entrants} entrants")));
    }
    let entrants = u32::try_from(entrants)
        .map_err(|_| LifecycleError::Corrupt(format!("{entrants} entrants")))?;
    let bracket = bracket::build(format, entrants).map_err(|e| match e {
        BracketError::TooFewEntrants => LifecycleError::TooFewEntrants,
        other => LifecycleError::Corrupt(other.to_string()),
    })?;

    write_bracket(&mut tx, event_id, &bracket).await?;
    sqlx::query("UPDATE events SET state = 'running' WHERE id = ?")
        .bind(event_id)
        .execute(&mut *tx)
        .await?;
    for set_id in bracket.ready_sets() {
        open_set(&mut tx, event_id, set_id).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The stored bracket of a started event.
pub async fn load_bracket(db: &SqlitePool, event_id: i64) -> Result<Bracket, LifecycleError> {
    let mut conn = db.acquire().await?;
    read_bracket(&mut conn, event_id).await
}

pub async fn store_bracket(
    db: &SqlitePool,
    event_id: i64,
    b: &Bracket,
) -> Result<(), LifecycleError> {
    let mut conn = db.acquire().await?;
    write_bracket(&mut conn, event_id, b).await
}

pub(crate) async fn read_bracket(
    conn: &mut SqliteConnection,
    event_id: i64,
) -> Result<Bracket, LifecycleError> {
    let json: Option<Option<String>> =
        sqlx::query_scalar("SELECT bracket_json FROM events WHERE id = ?")
            .bind(event_id)
            .fetch_optional(&mut *conn)
            .await?;
    let json = json
        .ok_or(LifecycleError::NoSuchEvent)?
        .ok_or(LifecycleError::NotStarted)?;
    serde_json::from_str(&json).map_err(|e| LifecycleError::Corrupt(format!("bracket_json: {e}")))
}

async fn write_bracket(
    conn: &mut SqliteConnection,
    event_id: i64,
    b: &Bracket,
) -> Result<(), LifecycleError> {
    let json = serde_json::to_string(b)
        .map_err(|e| LifecycleError::Corrupt(format!("serialising bracket: {e}")))?;
    let updated = sqlx::query("UPDATE events SET bracket_json = ? WHERE id = ?")
        .bind(json)
        .bind(event_id)
        .execute(&mut *conn)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(LifecycleError::NoSuchEvent);
    }
    Ok(())
}

/// Inserts the `sets` row for a set that has just become Ready.
async fn open_set(
    conn: &mut SqliteConnection,
    event_id: i64,
    set_id: u32,
) -> Result<(), LifecycleError> {
    sqlx::query(
        "INSERT INTO sets (event_id, set_id, state, wins0, wins1, updated_at)
         VALUES (?, ?, 'ready', 0, 0, ?)",
    )
    .bind(event_id)
    .bind(set_id)
    .bind(now_unix())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The stored bracket of a started event, read on `conn` (e.g. inside a caller's transaction).
pub async fn load_bracket_on(
    conn: &mut SqliteConnection,
    event_id: i64,
) -> Result<Bracket, LifecycleError> {
    read_bracket(conn, event_id).await
}

/// Records the judged verdict of a game in `judging`, in one transaction: the game, the
/// set score, the organiser queue and quarantine, and, when the game wins the set, the
/// bracket advance. Does nothing if the game is no longer in `judging`, so a duplicate or
/// late apply cannot count a game twice.
///
/// A counting verdict needs `winner_slot` (0 is the host). `verifier_build` is recorded on
/// the game as the build that re-simulated it: `None` when no re-simulation ran (the
/// verdicts decided before one: `Single`, `Conflicted`, `Illegal`, `Undecided`). The time of
/// the verdict is recorded as `judged_at`, a late (voided) one included.
pub async fn apply_verdict(
    st: &AppState,
    game_id: i64,
    v: &Verdict,
    winner_slot: Option<u8>,
    resim: &ResimOutcome,
    verifier_build: Option<&str>,
) -> Result<(), LifecycleError> {
    let verdict_json = to_json(v)?;
    let resim_json = to_json(resim)?;
    let name = verdict_name(v);
    let judged_at = now_unix();
    let mut tx = begin_write(&st.db).await?;
    let row: Option<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT g.event_id, g.set_id, g.state, e.ruleset_json
         FROM games g JOIN events e ON e.id = g.event_id WHERE g.id = ?",
    )
    .bind(game_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((event_id, set_id, game_state, ruleset_json)) = row else {
        return Ok(());
    };
    if game_state != "judging" {
        return Ok(());
    }
    let set_id = SetId::try_from(set_id)
        .map_err(|_| LifecycleError::Corrupt(format!("game {game_id} set_id {set_id}")))?;

    // The set may have been decided or sent back while the game was judged (an organiser
    // resolution, say). The late verdict is then moot: keep it on the game, change nothing else.
    let set_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM sets WHERE event_id = ? AND set_id = ?")
            .bind(event_id)
            .bind(set_id)
            .fetch_optional(&mut *tx)
            .await?;
    if set_state.as_deref() != Some("ready") {
        sqlx::query(
            "UPDATE games SET state = 'void', winner_slot = NULL, verdict_json = ?,
                 resim_json = ?, verifier_build = ?, judged_at = ? WHERE id = ?",
        )
        .bind(&verdict_json)
        .bind(&resim_json)
        .bind(verifier_build)
        .bind(judged_at)
        .bind(game_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        eprintln!(
            "judge: game {game_id} judged {name} after set {event_id}/{set_id} left play \
             ({}); voided",
            set_state.as_deref().unwrap_or("no sets row")
        );
        return Ok(());
    }

    if v.counts() {
        let slot = winner_slot.filter(|s| *s <= 1).ok_or_else(|| {
            LifecycleError::Corrupt(format!("{name} verdict without a winner slot"))
        })?;
        sqlx::query(
            "UPDATE games SET state = 'counted', winner_slot = ?, verdict_json = ?,
                 resim_json = ?, verifier_build = ?, judged_at = ? WHERE id = ?",
        )
        .bind(slot)
        .bind(&verdict_json)
        .bind(&resim_json)
        .bind(verifier_build)
        .bind(judged_at)
        .bind(game_id)
        .execute(&mut *tx)
        .await?;
        let bump = if slot == 0 {
            "UPDATE sets SET wins0 = wins0 + 1, updated_at = ?
             WHERE event_id = ? AND set_id = ? AND state = 'ready' RETURNING wins0"
        } else {
            "UPDATE sets SET wins1 = wins1 + 1, updated_at = ?
             WHERE event_id = ? AND set_id = ? AND state = 'ready' RETURNING wins1"
        };
        let wins: i64 = sqlx::query_scalar(bump)
            .bind(now_unix())
            .bind(event_id)
            .bind(set_id)
            .fetch_one(&mut *tx)
            .await?;

        let ruleset: Ruleset = serde_json::from_str(&ruleset_json)
            .map_err(|e| LifecycleError::Corrupt(format!("event {event_id} ruleset_json: {e}")))?;
        let bracket = read_bracket(&mut tx, event_id).await?;
        let side = bracket
            .sets
            .get(set_id as usize)
            .ok_or(LifecycleError::Bracket(BracketError::UnknownSet))?
            .side;
        let best_of = if side == Side::GrandFinal {
            ruleset.best_of_finals
        } else {
            ruleset.best_of
        };
        if wins >= i64::from(best_of / 2 + 1) {
            award_on(&mut tx, event_id, bracket, set_id, slot, None).await?;
        }
    } else {
        sqlx::query(
            "UPDATE games SET state = 'review', winner_slot = NULL, verdict_json = ?,
                 resim_json = ?, verifier_build = ?, judged_at = ? WHERE id = ?",
        )
        .bind(&verdict_json)
        .bind(&resim_json)
        .bind(verifier_build)
        .bind(judged_at)
        .bind(game_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE sets SET state = 'review', updated_at = ? WHERE event_id = ? AND set_id = ?",
        )
        .bind(now_unix())
        .bind(event_id)
        .bind(set_id)
        .execute(&mut *tx)
        .await?;
        open_queue_item(&mut tx, event_id, set_id, Some(game_id), name, None).await?;
    }

    if matches!(
        v,
        Verdict::VerifiedInexact | Verdict::Inexact | Verdict::Conflicted | Verdict::Single
    ) {
        sqlx::query("INSERT INTO quarantine (game_id, kind, created_at) VALUES (?, ?, ?)")
            .bind(game_id)
            .bind(name)
            .bind(now_unix())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Awards `set_id` to `winner_slot` as an organiser decision (`decided_by` `award`) and
/// advances the bracket, in one transaction.
pub async fn award_set(
    st: &AppState,
    event_id: i64,
    set_id: u32,
    winner_slot: u8,
) -> Result<(), LifecycleError> {
    let mut tx = begin_write(&st.db).await?;
    let bracket = read_bracket(&mut tx, event_id).await?;
    award_on(
        &mut tx,
        event_id,
        bracket,
        set_id,
        winner_slot,
        Some(DecidedBy::Award),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Marks the set `done`, reports it to `bracket`, opens a `sets` row for each newly Ready
/// set, and stores the bracket. A newly Ready set holding a disqualified entrant is awarded
/// straight to the other entrant; when both are disqualified it goes to slot 0 and a
/// `double-dq` queue item tells the organiser. The event becomes `complete` with the bracket.
pub(crate) async fn award_on(
    conn: &mut SqliteConnection,
    event_id: i64,
    mut bracket: Bracket,
    set_id: SetId,
    winner_slot: u8,
    how: Option<DecidedBy>,
) -> Result<(), LifecycleError> {
    let mut pending = vec![(set_id, winner_slot, how)];
    while let Some((set_id, winner_slot, how)) = pending.pop() {
        let newly_ready = bracket.report(set_id, winner_slot)?;
        let marked = sqlx::query(
            "UPDATE sets SET state = 'done', room_code = NULL, updated_at = ?, decided_by = ?
             WHERE event_id = ? AND set_id = ?",
        )
        .bind(now_unix())
        .bind(how.map(DecidedBy::as_str))
        .bind(event_id)
        .bind(set_id)
        .execute(&mut *conn)
        .await?;
        if marked.rows_affected() == 0 {
            return Err(LifecycleError::Corrupt(format!(
                "set {event_id}/{set_id} has no sets row"
            )));
        }
        for id in newly_ready {
            let set = &bracket.sets[id as usize];
            if set.state != SetState::Ready {
                continue;
            }
            open_set(conn, event_id, id).await?;
            if let Some(award) = dq_award(conn, event_id, set.slots).await? {
                if award.double {
                    open_queue_item(conn, event_id, id, None, DOUBLE_DQ, None).await?;
                }
                pending.push((id, award.slot, Some(award.decided_by())));
            }
        }
    }
    write_bracket(conn, event_id, &bracket).await?;
    if bracket.is_complete() {
        sqlx::query("UPDATE events SET state = 'complete' WHERE id = ?")
            .bind(event_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Queue reason for a set awarded with both of its entrants disqualified.
pub const DOUBLE_DQ: &str = "double-dq";

/// How a set was decided when not by its played games, as stored in `sets.decided_by`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecidedBy {
    /// An organiser awarded it.
    Award,
    /// One entrant was disqualified.
    Dq,
    /// Both entrants were disqualified (it went to slot 0).
    DoubleDq,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::Award => "award",
            DecidedBy::Dq => "dq",
            DecidedBy::DoubleDq => DOUBLE_DQ,
        }
    }
}

/// A set awarded because of a disqualification.
pub(crate) struct DqAward {
    pub(crate) slot: u8,
    /// Both entrants are disqualified (the set goes to slot 0).
    pub(crate) double: bool,
}

impl DqAward {
    pub(crate) fn decided_by(&self) -> DecidedBy {
        if self.double {
            DecidedBy::DoubleDq
        } else {
            DecidedBy::Dq
        }
    }
}

/// How a Ready set is awarded because of a disqualification, if it is.
pub(crate) async fn dq_award(
    conn: &mut SqliteConnection,
    event_id: i64,
    slots: [Slot; 2],
) -> Result<Option<DqAward>, LifecycleError> {
    let mut dq = [false; 2];
    for (i, slot) in slots.iter().enumerate() {
        let Slot::Entrant(seed) = *slot else {
            return Err(LifecycleError::Corrupt(format!(
                "ready set slot {slot:?} in event {event_id}"
            )));
        };
        let flag: Option<bool> =
            sqlx::query_scalar("SELECT dq != 0 FROM entrants WHERE event_id = ? AND seed = ?")
                .bind(event_id)
                .bind(seed)
                .fetch_optional(&mut *conn)
                .await?;
        dq[i] = flag.ok_or_else(|| {
            LifecycleError::Corrupt(format!("event {event_id} has no entrant seeded {seed}"))
        })?;
    }
    let (slot, double) = match dq {
        [false, false] => return Ok(None),
        [true, false] => (1, false),
        [false, true] => (0, false),
        [true, true] => (0, true),
    };
    Ok(Some(DqAward { slot, double }))
}

/// The verdict's variant name, as used for queue reasons and quarantine kinds.
pub fn verdict_name(v: &Verdict) -> &'static str {
    match v {
        Verdict::Verified => "Verified",
        Verdict::VerifiedInexact => "VerifiedInexact",
        Verdict::PeerOnly => "PeerOnly",
        Verdict::Conflicted => "Conflicted",
        Verdict::Single => "Single",
        Verdict::Illegal(_) => "Illegal",
        Verdict::Undecided => "Undecided",
        Verdict::Inexact => "Inexact",
    }
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Result<String, LifecycleError> {
    serde_json::to_string(v).map_err(|e| LifecycleError::Corrupt(format!("serialising: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_round_trips() {
        for f in [Format::SingleElim, Format::DoubleElim] {
            assert_eq!(format_from_str(format_to_str(f)), Some(f));
        }
        assert_eq!(format_from_str("SingleElim"), None);
    }

    #[test]
    fn entrant_cap_is_1024() {
        assert_eq!(MAX_ENTRANTS, 1024);
    }
}

//! Organiser review: how a set reaches the organiser queue (a verdict that does not count, a
//! reported no-show, a stalled set, a double disqualification) and how the organiser
//! resolves a queue item. Each public function does its reads and writes in one transaction,
//! or runs inside the caller's.

use axum::http::StatusCode;
use bracket::{BracketError, SetId, SetState, Slot};
use proto::Resolve;
use sqlx::SqliteConnection;
use tracing::info;

use crate::db::{begin_write, now_unix};
use crate::lifecycle::{
    award_on, dq_award, read_bracket, to_json, DecidedBy, LifecycleError, DOUBLE_DQ,
};
use crate::logging::{key_prefix, Journal};
use crate::routes::ApiError;
use crate::state::AppState;

/// Queue reason for a set a player reported their opponent missing from.
pub const NO_SHOW: &str = "no-show";
/// Queue reason for a `ready` set that saw no activity for `set_stall_secs`.
pub const STALLED: &str = "stalled";
/// Longest no-show note kept, in characters.
pub const MAX_NOTE_CHARS: usize = 200;

/// Opens an organiser queue item on a set (and the game in question, if any). Returns its id.
pub(crate) async fn open_queue_item(
    conn: &mut SqliteConnection,
    event_id: i64,
    set_id: SetId,
    game_id: Option<i64>,
    reason: &str,
    detail: Option<&str>,
    journal: &mut Journal,
) -> Result<i64, LifecycleError> {
    let id = sqlx::query_scalar(
        "INSERT INTO queue (event_id, set_id, game_id, reason, state, created_at, detail)
         VALUES (?, ?, ?, ?, 'open', ?, ?) RETURNING id",
    )
    .bind(event_id)
    .bind(set_id)
    .bind(game_id)
    .bind(reason)
    .bind(now_unix())
    .bind(detail)
    .fetch_one(&mut *conn)
    .await?;
    journal.queue_opened(id, event_id, set_id, reason);
    Ok(id)
}

/// Whether a game of the set is under way as far as the server knows: `uploading` with an
/// upload in, or `judging`.
pub(crate) async fn game_in_progress(
    conn: &mut SqliteConnection,
    event_id: i64,
    set_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM games g WHERE g.event_id = ? AND g.set_id = ?
           AND (g.state = 'judging'
                OR (g.state = 'uploading'
                    AND EXISTS (SELECT 1 FROM uploads u WHERE u.game_id = g.id))))",
    )
    .bind(event_id)
    .bind(set_id)
    .fetch_one(&mut *conn)
    .await
}

/// Sends a `ready` set to the organiser, inside the caller's write transaction: opens a queue
/// item with `reason` and `detail`, and moves the set to `review` with no room code. Both
/// players are then told to wait for the organiser. The caller has checked that the set is
/// `ready` with no game in progress. Returns the queue item id.
pub(crate) async fn send_to_review(
    conn: &mut SqliteConnection,
    event_id: i64,
    set_id: SetId,
    reason: &str,
    detail: Option<&str>,
    journal: &mut Journal,
) -> Result<i64, LifecycleError> {
    let moved = sqlx::query(
        "UPDATE sets SET state = 'review', room_code = NULL, updated_at = ?
         WHERE event_id = ? AND set_id = ? AND state = 'ready'",
    )
    .bind(now_unix())
    .bind(event_id)
    .bind(set_id)
    .execute(&mut *conn)
    .await?;
    if moved.rows_affected() == 0 {
        return Err(LifecycleError::Corrupt(format!(
            "set {event_id}/{set_id} is not ready"
        )));
    }
    open_queue_item(conn, event_id, set_id, None, reason, detail, journal).await
}

/// The queue detail of a no-show report: who reported it, and their note cut to
/// [`MAX_NOTE_CHARS`] characters (blank notes are dropped).
pub fn no_show_detail(slot: u8, tag: &str, note: Option<&str>) -> String {
    let note: String = note
        .unwrap_or("")
        .trim()
        .chars()
        .take(MAX_NOTE_CHARS)
        .collect();
    if note.is_empty() {
        format!("reported by slot {slot} ({tag})")
    } else {
        format!("reported by slot {slot} ({tag}): {note}")
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ResolveError {
    #[error("no such queue item")]
    NoSuchItem,
    #[error("queue item is already resolved")]
    AlreadyResolved,
    #[error("slot must be 0 or 1")]
    BadSlot,
    #[error("{0}")]
    Conflict(&'static str),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
}

impl From<sqlx::Error> for ResolveError {
    fn from(e: sqlx::Error) -> Self {
        ResolveError::Lifecycle(LifecycleError::Db(e))
    }
}

impl From<ResolveError> for ApiError {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::NoSuchItem => ApiError::new(StatusCode::NOT_FOUND, e.to_string()),
            ResolveError::BadSlot => ApiError::new(StatusCode::BAD_REQUEST, e.to_string()),
            ResolveError::AlreadyResolved | ResolveError::Conflict(_) => {
                ApiError::new(StatusCode::CONFLICT, e.to_string())
            }
            ResolveError::Lifecycle(e) => e.into(),
        }
    }
}

/// Resolves an open organiser queue item, in one transaction, recording `r`, the organiser's
/// key and the time in `resolution_json`.
///
/// - `Award { slot }`: the set's games in review become `void`, its open items are resolved,
///   and the set is awarded to `slot`. On a set already decided for `slot` this only
///   acknowledges the item; one decided the other way is a conflict.
/// - `Replay`: the set's games in review become `void`, its open items are resolved, and the
///   set goes back to `ready` with no room code, so the same game number is played again.
///   Wins are unchanged.
/// - `Dq { slot }`: the entrant in `slot` is disqualified, and every Ready set holding a
///   disqualified entrant (this one included) is awarded to the opponent, which also
///   decides sets that become Ready later.
pub async fn resolve_queue_item(
    st: &AppState,
    item_id: i64,
    r: &Resolve,
    to_key: &str,
) -> Result<(), ResolveError> {
    if let Resolve::Award { slot } | Resolve::Dq { slot } = r {
        if *slot > 1 {
            return Err(ResolveError::BadSlot);
        }
    }
    let resolution = to_json(&serde_json::json!({
        "resolve": r,
        "by": to_key,
        "at": now_unix(),
    }))?;

    let mut journal = Journal::default();
    let mut tx = begin_write(&st.db).await?;
    let row: Option<(i64, i64, String)> =
        sqlx::query_as("SELECT event_id, set_id, state FROM queue WHERE id = ?")
            .bind(item_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (event_id, set_id, state) = row.ok_or(ResolveError::NoSuchItem)?;
    if state != "open" {
        return Err(ResolveError::AlreadyResolved);
    }
    let set_id = SetId::try_from(set_id)
        .map_err(|_| LifecycleError::Corrupt(format!("queue item {item_id} set_id {set_id}")))?;
    let bracket = read_bracket(&mut tx, event_id).await?;
    let set = bracket
        .sets
        .get(set_id as usize)
        .ok_or(LifecycleError::Bracket(BracketError::UnknownSet))?;

    match *r {
        Resolve::Award { slot } => match set.state {
            SetState::Ready => {
                close_set_items(&mut tx, event_id, set_id, &resolution).await?;
                award_on(
                    &mut tx,
                    event_id,
                    bracket,
                    set_id,
                    slot,
                    Some(DecidedBy::Award),
                    &mut journal,
                )
                .await?;
            }
            SetState::Done { winner } if winner == slot => {
                close_set_items(&mut tx, event_id, set_id, &resolution).await?;
            }
            _ => return Err(ResolveError::Conflict("set is already decided")),
        },
        Resolve::Replay => {
            if set.state != SetState::Ready {
                return Err(ResolveError::Conflict("set is already decided"));
            }
            close_set_items(&mut tx, event_id, set_id, &resolution).await?;
            sqlx::query(
                "UPDATE sets SET state = 'ready', room_code = NULL, updated_at = ?
                 WHERE event_id = ? AND set_id = ?",
            )
            .bind(now_unix())
            .bind(event_id)
            .bind(set_id)
            .execute(&mut *tx)
            .await?;
        }
        Resolve::Dq { slot } => {
            let Slot::Entrant(seed) = set.slots[usize::from(slot)] else {
                return Err(ResolveError::Conflict("no entrant in that slot"));
            };
            sqlx::query("UPDATE entrants SET dq = 1 WHERE event_id = ? AND seed = ?")
                .bind(event_id)
                .bind(seed)
                .execute(&mut *tx)
                .await?;
            award_dq_ready(&mut tx, event_id, &resolution, &mut journal).await?;
        }
    }

    sqlx::query("UPDATE queue SET state = 'resolved', resolution_json = ? WHERE id = ?")
        .bind(&resolution)
        .bind(item_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    journal.emit();
    let (resolution, slot) = match *r {
        Resolve::Award { slot } => ("award", Some(slot)),
        Resolve::Replay => ("replay", None),
        Resolve::Dq { slot } => ("dq", Some(slot)),
    };
    info!(
        id = item_id,
        event = event_id,
        set = set_id,
        resolution = %resolution,
        slot = %slot.map_or_else(|| "-".to_owned(), |s| s.to_string()),
        to = %key_prefix(to_key),
        "queue item resolved"
    );
    Ok(())
}

/// Before an organiser decides a set: its games in review become `void` and its open queue
/// items are resolved with `resolution`.
async fn close_set_items(
    conn: &mut SqliteConnection,
    event_id: i64,
    set_id: SetId,
    resolution: &str,
) -> Result<(), LifecycleError> {
    sqlx::query(
        "UPDATE games SET state = 'void' WHERE event_id = ? AND set_id = ? AND state = 'review'",
    )
    .bind(event_id)
    .bind(set_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE queue SET state = 'resolved', resolution_json = ?
         WHERE event_id = ? AND set_id = ? AND state = 'open'",
    )
    .bind(resolution)
    .bind(event_id)
    .bind(set_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Awards every Ready set that holds a disqualified entrant, one at a time (each award can
/// make more sets Ready), until none is left.
async fn award_dq_ready(
    conn: &mut SqliteConnection,
    event_id: i64,
    resolution: &str,
    journal: &mut Journal,
) -> Result<(), LifecycleError> {
    loop {
        let bracket = read_bracket(conn, event_id).await?;
        let mut found = None;
        for id in bracket.ready_sets() {
            if let Some(award) = dq_award(conn, event_id, bracket.sets[id as usize].slots).await? {
                found = Some((id, award));
                break;
            }
        }
        let Some((id, award)) = found else {
            return Ok(());
        };
        close_set_items(conn, event_id, id, resolution).await?;
        award_on(
            conn,
            event_id,
            bracket,
            id,
            award.slot,
            Some(award.decided_by()),
            journal,
        )
        .await?;
        if award.double {
            open_queue_item(conn, event_id, id, None, DOUBLE_DQ, None, journal).await?;
        }
    }
}

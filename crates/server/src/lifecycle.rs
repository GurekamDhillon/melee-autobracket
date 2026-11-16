//! Event state transitions over the store. Each function does its reads and
//! writes in a single transaction where it changes state.

use axum::http::StatusCode;
use bracket::{Bracket, BracketError, Format};
use sqlx::{SqliteConnection, SqlitePool};

use crate::db::begin_write;
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
            | LifecycleError::TooFewEntrants => ApiError::new(StatusCode::CONFLICT, e.to_string()),
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

async fn read_bracket(
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
        "INSERT INTO sets (event_id, set_id, state, wins0, wins1) VALUES (?, ?, 'ready', 0, 0)",
    )
    .bind(event_id)
    .bind(set_id)
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

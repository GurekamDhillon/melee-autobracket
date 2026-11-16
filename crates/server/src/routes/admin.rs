//! Organiser routes: creating events and invites, starting events, the review queue, and
//! moving a player to a new key.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use proto::{EventCreateReq, EventSummary, EventView, QueueItem, RekeyReq, Resolve};
use rand::rngs::OsRng;
use rand::Rng;
use serde_json::{json, Value};

use super::events::{event_view, summary};
use super::{parse_body, ApiError};
use crate::auth::To;
use crate::db::{begin_write, now_unix};
use crate::lifecycle::{self, format_to_str};
use crate::logging::{key_prefix, label};
use crate::review;
use crate::state::AppState;

/// RFC 4648 base32 alphabet.
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const INVITE_LEN: usize = 12;

/// `POST /v1/admin/events`: a new event open for sign-up. The ruleset is the supplied one,
/// or else `cfg.ruleset`, with `best_of` overridden when the request gives it. Every
/// `best_of` and `best_of_finals` must be odd.
pub async fn create_event(
    State(state): State<AppState>,
    to: To,
) -> Result<(StatusCode, Json<EventSummary>), ApiError> {
    let req: EventCreateReq = parse_body(&to.body)?;
    if let Some(r) = &req.ruleset {
        check_odd("best_of", r.best_of)?;
        check_odd("best_of_finals", r.best_of_finals)?;
    }
    let mut ruleset = req.ruleset.unwrap_or_else(|| state.cfg.ruleset.clone());
    if let Some(best_of) = req.best_of {
        check_odd("best_of", best_of)?;
        ruleset.best_of = best_of;
    }
    let ruleset_json = serde_json::to_string(&ruleset).map_err(ApiError::internal)?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO events (name, format, ruleset_json, state, created_at)
         VALUES (?, ?, ?, 'signup', ?) RETURNING id",
    )
    .bind(&req.name)
    .bind(format_to_str(req.format))
    .bind(ruleset_json)
    .bind(now_unix())
    .fetch_one(&state.db)
    .await?;
    tracing::info!(
        event = id,
        name = %label(&req.name),
        format = %format_to_str(req.format),
        best_of = ruleset.best_of,
        "event created"
    );
    let created = summary(id, req.name, "signup".into(), 0)?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// `POST /v1/admin/invites`: a fresh single-use registration code.
pub async fn create_invite(
    State(state): State<AppState>,
    _to: To,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let code: String = (0..INVITE_LEN)
        .map(|_| char::from(BASE32[OsRng.gen_range(0..BASE32.len())]))
        .collect();
    sqlx::query("INSERT INTO invites (code) VALUES (?)")
        .bind(&code)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "code": code }))))
}

/// `POST /v1/admin/events/{id}/start`: closes sign-up and builds the bracket.
pub async fn start_event(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    _to: To,
) -> Result<Json<EventView>, ApiError> {
    lifecycle::start_event(&state, id).await?;
    event_view(&state.db, id).await.map(Json)
}

/// `GET /v1/admin/queue`: the open queue items, oldest first, with the judged evidence of
/// the game in question.
pub async fn queue(
    State(state): State<AppState>,
    _to: To,
) -> Result<Json<Vec<QueueItem>>, ApiError> {
    let rows: Vec<QueueRow> = sqlx::query_as(
        "SELECT q.id, q.event_id, q.set_id, q.game_id, q.reason, g.verdict_json, g.resim_json,
                q.detail
         FROM queue q LEFT JOIN games g ON g.id = q.game_id
         WHERE q.state = 'open' ORDER BY q.id",
    )
    .fetch_all(&state.db)
    .await?;
    rows.into_iter()
        .map(
            |(id, event_id, set_id, game_id, reason, verdict, resim, detail)| {
                Ok(QueueItem {
                    id,
                    event_id,
                    set_id: u32::try_from(set_id).map_err(|_| {
                        ApiError::internal(format_args!("queue item {id} set_id {set_id}"))
                    })?,
                    game_id,
                    reason,
                    verdict: parse_stored(verdict.as_deref())?,
                    resim: parse_stored(resim.as_deref())?,
                    detail,
                })
            },
        )
        .collect::<Result<_, _>>()
        .map(Json)
}

type QueueRow = (
    i64,
    i64,
    i64,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// `POST /v1/admin/queue/{id}/resolve` (`Resolve`).
pub async fn resolve(
    State(state): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
    to: To,
) -> Result<StatusCode, ApiError> {
    let Path(id) = path.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let r: Resolve = parse_body(&to.body)?;
    review::resolve_queue_item(&state, id, &r, &to.key).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/admin/players/{old_key}/rekey` (`RekeyReq`): moves a player, their entries and
/// the invite they used to a new key. The old key stops working.
pub async fn rekey(
    State(state): State<AppState>,
    Path(old_key): Path<String>,
    to: To,
) -> Result<StatusCode, ApiError> {
    let req: RekeyReq = parse_body(&to.body)?;
    let mut tx = begin_write(&state.db).await?;
    let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM players WHERE key = ?")
        .bind(&old_key)
        .fetch_optional(&mut *tx)
        .await?;
    if known.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such player"));
    }
    let valid = req.new_key.len() == 64
        && req
            .new_key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "new_key must be 64 lowercase hex characters",
        ));
    }
    let taken: Option<i64> = sqlx::query_scalar("SELECT 1 FROM players WHERE key = ?")
        .bind(&req.new_key)
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "new_key is already registered",
        ));
    }
    for sql in [
        "UPDATE players SET key = ? WHERE key = ?",
        "UPDATE entrants SET player_key = ? WHERE player_key = ?",
        "UPDATE invites SET used_by = ? WHERE used_by = ?",
    ] {
        match sqlx::query(sql)
            .bind(&req.new_key)
            .bind(&old_key)
            .execute(&mut *tx)
            .await
        {
            Ok(_) => {}
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "new_key is already in use",
                ));
            }
            Err(e) => return Err(e.into()),
        }
    }
    tx.commit().await?;
    tracing::info!(
        old = %key_prefix(&old_key),
        new = %key_prefix(&req.new_key),
        to = %key_prefix(&to.key),
        "player rekeyed"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Parses an optional stored JSON column; a bad value is corrupt data.
pub(super) fn parse_stored<T: serde::de::DeserializeOwned>(
    json: Option<&str>,
) -> Result<Option<T>, ApiError> {
    json.map(|j| {
        serde_json::from_str(j)
            .map_err(|e| ApiError::internal(format_args!("stored JSON {j:?}: {e}")))
    })
    .transpose()
}

/// 400 unless a set length is odd (and so not 0).
fn check_odd(field: &str, n: u8) -> Result<(), ApiError> {
    if n % 2 == 1 {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} must be odd"),
        ))
    }
}

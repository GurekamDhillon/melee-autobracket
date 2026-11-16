//! Organiser routes: creating events and invites, starting events, the review queue, and
//! moving a player to a new key.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use std::collections::HashMap;

use axum::Json;
use bracket::Bracket;
use proto::{EventCreateReq, EventSummary, EventView, QueueItem, RekeyReq, Resolve};
use rand::rngs::OsRng;
use rand::Rng;
use serde_json::{json, Value};
use sqlx::SqliteConnection;

use super::events::{event_view, summary};
use super::{check_text, parse_body, ApiError, MAX_EVENT_NAME_CHARS};
use crate::auth::To;
use crate::db::{begin_write, now_unix};
use crate::lifecycle::{self, format_to_str};
use crate::logging::key_prefix;
use crate::review;
use crate::state::AppState;

/// RFC 4648 base32 alphabet.
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const INVITE_LEN: usize = 12;

/// `POST /v1/admin/events`: a new event open for sign-up. The ruleset is the supplied one,
/// or else `cfg.ruleset`, with `best_of` overridden when the request gives it. The result
/// must pass [`proto::Ruleset::validate`] (400 naming the field otherwise).
pub async fn create_event(
    State(state): State<AppState>,
    to: To,
) -> Result<(StatusCode, Json<EventSummary>), ApiError> {
    let req: EventCreateReq = parse_body(&to.body)?;
    check_text("name", &req.name, MAX_EVENT_NAME_CHARS)?;
    let mut ruleset = req.ruleset.unwrap_or_else(|| state.cfg.ruleset.clone());
    ruleset
        .validate()
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e))?;
    if let Some(best_of) = req.best_of {
        ruleset.best_of = best_of;
        ruleset
            .validate()
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e))?;
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
        name = %crate::logging::detail(&req.name),
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

/// `GET /v1/admin/queue`: the open queue items, oldest first, with the judged verdict of the
/// game in question, the players' tags, who reported a no-show, and the evidence: that
/// game's, or for an item about the whole set, every game of the set that has some (see
/// [`QueueItem::evidence`]). Read in one transaction.
pub async fn queue(
    State(state): State<AppState>,
    _to: To,
) -> Result<Json<Vec<QueueItem>>, ApiError> {
    let mut tx = state.db.begin().await?;
    let rows: Vec<QueueRow> = sqlx::query_as(
        "SELECT q.id, q.event_id, q.set_id, q.game_id, q.reason, g.verdict_json, g.resim_json,
                q.detail, q.reporter_slot
         FROM queue q LEFT JOIN games g ON g.id = q.game_id
         WHERE q.state = 'open' ORDER BY q.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut brackets: HashMap<i64, Option<Bracket>> = HashMap::new();
    let mut items = Vec::with_capacity(rows.len());
    for (id, event_id, set_id, game_id, reason, verdict, resim, detail, reporter_slot) in rows {
        let set = u32::try_from(set_id)
            .map_err(|_| ApiError::internal(format_args!("queue item {id} set_id {set_id}")))?;
        if let std::collections::hash_map::Entry::Vacant(e) = brackets.entry(event_id) {
            e.insert(lifecycle::load_bracket_on(&mut tx, event_id).await.ok());
        }
        let slots = brackets
            .get(&event_id)
            .and_then(Option::as_ref)
            .and_then(|b| b.sets.get(set as usize))
            .map(|s| s.slots);
        let tags = match slots {
            Some(slots) => lifecycle::slot_tags(&mut tx, event_id, slots).await?,
            None => None,
        };
        let reporter_tag = reporter_slot
            .and_then(|s| usize::try_from(s).ok())
            .and_then(|s| tags.as_ref().and_then(|t| t.get(s).cloned()));
        let evidence = match game_id {
            Some(game_id) => game_evidence(&mut tx, game_id).await?,
            None => {
                let games = game_evidence_of_set(&mut tx, event_id, set_id).await?;
                (!games.is_empty()).then_some(Value::Array(games))
            }
        };
        items.push(QueueItem {
            id,
            event_id,
            set_id: set,
            game_id,
            reason,
            verdict: parse_stored(verdict.as_deref())?,
            resim: parse_stored(resim.as_deref())?,
            detail,
            evidence,
            reporter_tag,
            tags,
        });
    }
    tx.commit().await?;
    Ok(Json(items))
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
    Option<i64>,
);

/// The stored evidence of one game, with its `game_id` and current `state`; `None` when the
/// game has none.
async fn game_evidence(
    conn: &mut SqliteConnection,
    game_id: i64,
) -> Result<Option<Value>, ApiError> {
    let row: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT id, state, evidence_json FROM games WHERE id = ? AND evidence_json IS NOT NULL",
    )
    .bind(game_id)
    .fetch_optional(&mut *conn)
    .await?;
    row.map(evidence_value).transpose()
}

/// [`game_evidence`] of every game of one set that has some, oldest first.
async fn game_evidence_of_set(
    conn: &mut SqliteConnection,
    event_id: i64,
    set_id: i64,
) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, state, evidence_json FROM games
         WHERE event_id = ? AND set_id = ? AND evidence_json IS NOT NULL ORDER BY id",
    )
    .bind(event_id)
    .bind(set_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter().map(evidence_value).collect()
}

/// A stored evidence row as shown: the evidence object with `game_id` and `state` added.
fn evidence_value((game_id, state, json): (i64, String, String)) -> Result<Value, ApiError> {
    let mut ev: Value = serde_json::from_str(&json)
        .map_err(|e| ApiError::internal(format_args!("game {game_id} evidence_json: {e}")))?;
    if let Some(obj) = ev.as_object_mut() {
        obj.insert("game_id".into(), game_id.into());
        obj.insert("state".into(), state.into());
    }
    Ok(ev)
}

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

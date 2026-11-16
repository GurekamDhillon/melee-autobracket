//! Routes players use while playing a set: what to do next, the room code
//! hand-off, and replay upload. Nothing here decides a winner; uploads are
//! judged later.

use std::fmt::Display;
use std::path::{Path as FsPath, PathBuf};

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use bracket::{Bracket, EntrantId, SetState, Side, Slot};
use proto::{Next, NoShowReq, Role, RoomReq, Ruleset};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

use super::{parse_body, ApiError};
use crate::auth::Player;
use crate::db::{begin_write, now_unix};
use crate::lifecycle::{load_bracket_on, LifecycleError};
use crate::review::{self, NO_SHOW};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct NextQuery {
    event: i64,
}

/// `GET /v1/me/next?event={id}`: what the signing player should do next in the event.
pub async fn next(
    State(state): State<AppState>,
    query: Result<Query<NextQuery>, QueryRejection>,
    player: Player,
) -> Result<Json<Next>, ApiError> {
    let Query(NextQuery { event }) = query.map_err(bad_request)?;
    // Read-only, so a deferred transaction: one consistent snapshot.
    let mut tx = state.db.begin().await?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT state, ruleset_json FROM events WHERE id = ?")
            .bind(event)
            .fetch_optional(&mut *tx)
            .await?;
    let (event_state, ruleset_json) = row.ok_or(LifecycleError::NoSuchEvent)?;
    let Some(me) = entrant_id(&mut tx, event, &player.key).await? else {
        return Ok(Json(Next::NotEntered));
    };
    if event_state == "signup" {
        return Ok(Json(Next::Waiting));
    }
    let bracket = load_bracket_on(&mut tx, event).await?;

    let open: Vec<(i64, String, Option<String>, i64, i64)> = sqlx::query_as(
        "SELECT set_id, state, room_code, wins0, wins1 FROM sets
         WHERE event_id = ? AND state IN ('ready', 'review') ORDER BY set_id",
    )
    .bind(event)
    .fetch_all(&mut *tx)
    .await?;
    for (set_id, set_state, room_code, wins0, wins1) in open {
        let set = bracket_set(&bracket, set_id)?;
        let Some(slot) = slot_of(set, me) else {
            continue;
        };
        if set_state == "review" {
            return Ok(Json(Next::Review { set_id: set.id }));
        }
        let Slot::Entrant(opponent) = set.slots[1 - usize::from(slot)] else {
            return Err(corrupt(format_args!(
                "open set {event}/{set_id} lacks an opponent"
            )));
        };
        let opponent_tag: String = sqlx::query_scalar(
            "SELECT p.tag FROM entrants n JOIN players p ON p.key = n.player_key
             WHERE n.event_id = ? AND n.seed = ?",
        )
        .bind(event)
        .bind(opponent)
        .fetch_one(&mut *tx)
        .await?;
        let ruleset: Ruleset = serde_json::from_str(&ruleset_json)
            .map_err(|e| corrupt(format_args!("event {event} ruleset_json: {e}")))?;
        let game = current_game(&mut tx, event, set_id).await?;
        let awaiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM uploads u JOIN games g ON g.id = u.game_id
             WHERE g.event_id = ? AND g.set_id = ? AND g.game_no = ?
               AND g.state IN ('uploading', 'judging') AND u.slot = ?)",
        )
        .bind(event)
        .bind(set_id)
        .bind(game.game_no)
        .bind(slot)
        .fetch_one(&mut *tx)
        .await?;
        let (mine, theirs) = if slot == 0 {
            (wins0, wins1)
        } else {
            (wins1, wins0)
        };
        return Ok(Json(Next::Play {
            set_id: set.id,
            opponent_tag,
            role: if slot == 0 { Role::Host } else { Role::Guest },
            relay: state.cfg.relay.clone(),
            room_code,
            game_no: narrow(game.game_no)?,
            wins: [narrow(mine)?, narrow(theirs)?],
            best_of: if set.side == Side::GrandFinal {
                ruleset.best_of_finals
            } else {
                ruleset.best_of
            },
            stocks: ruleset.stocks,
            timer_secs: ruleset.timer_secs,
            awaiting_verdict: awaiting,
        }));
    }

    if bracket.champion() == Some(me) {
        return Ok(Json(Next::Champion));
    }
    // Still alive while placed in an unfinished set, waiting on the set that feeds it.
    let alive = bracket.sets.iter().any(|s| {
        matches!(s.state, SetState::Waiting | SetState::Ready) && slot_of(s, me).is_some()
    });
    Ok(Json(if alive {
        Next::Waiting
    } else {
        Next::Eliminated
    }))
}

/// `POST /v1/sets/{event}/{set}/room` (`RoomReq`): the host posts the room code for the
/// current game, replacing any earlier one.
pub async fn room(
    State(state): State<AppState>,
    path: Result<Path<(i64, i64)>, PathRejection>,
    player: Player,
) -> Result<StatusCode, ApiError> {
    let Path((event, set)) = path.map_err(bad_request)?;
    let mut tx = begin_write(&state.db).await?;
    let seat = seat(&mut tx, event, set, &player.key).await?;
    if seat.slot != 0 {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only the host posts the room code",
        ));
    }
    if seat.set_state != "ready" {
        return Err(ApiError::new(StatusCode::CONFLICT, "set is not in play"));
    }
    let req: RoomReq = parse_body(&player.body)?;
    if !valid_room_code(&req.code) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "room code must be 4 characters of A-Z or 0-9, or host:port",
        ));
    }
    sqlx::query("UPDATE sets SET room_code = ?, updated_at = ? WHERE event_id = ? AND set_id = ?")
        .bind(&req.code)
        .bind(now_unix())
        .bind(event)
        .bind(set)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/sets/{event}/{set}/no-show` (`NoShowReq`; an empty body is no note): a player of
/// the set reports that the opponent has not shown up. The set goes to the organiser queue
/// (reason `no-show`, detail naming the reporting slot and their note) and to `review`, with
/// no room code. `409` unless the set is `ready` with no game uploading or awaiting a verdict.
pub async fn no_show(
    State(state): State<AppState>,
    path: Result<Path<(i64, i64)>, PathRejection>,
    player: Player,
) -> Result<StatusCode, ApiError> {
    let Path((event, set)) = path.map_err(bad_request)?;
    let mut tx = begin_write(&state.db).await?;
    let seat = seat(&mut tx, event, set, &player.key).await?;
    match seat.set_state.as_str() {
        "ready" => {}
        "review" => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "set is already waiting for the organiser",
            ))
        }
        _ => return Err(ApiError::new(StatusCode::CONFLICT, "set is not in play")),
    }
    let req: NoShowReq = if player.body.is_empty() {
        NoShowReq::default()
    } else {
        parse_body(&player.body)?
    };
    if review::game_in_progress(&mut tx, event, set).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "a game of this set is in progress or awaiting a verdict",
        ));
    }
    let detail = review::no_show_detail(seat.slot, &player.tag, req.detail.as_deref());
    review::send_to_review(&mut tx, event, narrow(set)?, NO_SHOW, Some(&detail)).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/sets/{event}/{set}/games/{n}/replay` (raw `.slp` bytes, header `x-ab-build`):
/// stores the caller's replay of game `n`. `202` when stored, `200` when the identical file
/// was already stored for this slot.
pub async fn upload(
    State(state): State<AppState>,
    path: Result<Path<(i64, i64, i64)>, PathRejection>,
    headers: HeaderMap,
    player: Player,
) -> Result<StatusCode, ApiError> {
    let Path((event, set, n)) = path.map_err(bad_request)?;
    let client_build = match headers.get("x-ab-build") {
        None => String::new(),
        Some(v) => v
            .to_str()
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid x-ab-build header"))?
            .to_owned(),
    };

    // Parse and hash off the async runtime, before taking the write lock.
    let body = player.body.clone();
    let (parses, sha256) = tokio::task::spawn_blocking(move || {
        (
            replay::parse(&body).is_ok(),
            hex::encode(Sha256::digest(&body)),
        )
    })
    .await
    .map_err(ApiError::internal)?;

    let mut tx = begin_write(&state.db).await?;
    let seat = seat(&mut tx, event, set, &player.key).await?;
    if seat.set_state != "ready" {
        return Err(ApiError::new(StatusCode::CONFLICT, "set is not in play"));
    }
    let game = current_game(&mut tx, event, set).await?;
    if n != game.game_no {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("the current game is {}", game.game_no),
        ));
    }
    if !parses {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "not a readable replay file",
        ));
    }

    let game_id = match game.open {
        Some((game_id, game_state)) => {
            let stored: Option<String> =
                sqlx::query_scalar("SELECT sha256 FROM uploads WHERE game_id = ? AND slot = ?")
                    .bind(game_id)
                    .bind(seat.slot)
                    .fetch_optional(&mut *tx)
                    .await?;
            match stored {
                Some(s) if s == sha256 => return Ok(StatusCode::OK),
                Some(_) => {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "a different replay is already uploaded for this game",
                    ))
                }
                None if game_state != "uploading" => {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "game is no longer accepting uploads",
                    ))
                }
                None => game_id,
            }
        }
        None => {
            // First upload of this game: open its row and retire the room code.
            let game_id: i64 = sqlx::query_scalar(
                "INSERT INTO games (event_id, set_id, game_no, state, first_upload_at)
                 VALUES (?, ?, ?, 'uploading', ?) RETURNING id",
            )
            .bind(event)
            .bind(set)
            .bind(game.game_no)
            .bind(now_unix())
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE sets SET room_code = NULL, updated_at = ? WHERE event_id = ? AND set_id = ?",
            )
            .bind(now_unix())
            .bind(event)
                .bind(set)
                .execute(&mut *tx)
                .await?;
            game_id
        }
    };

    let dest = state
        .cfg
        .replay_dir
        .join(event.to_string())
        .join(set.to_string())
        .join(game_id.to_string())
        .join(format!("{}.slp", seat.slot));
    let dest_str = dest
        .to_str()
        .ok_or_else(|| ApiError::internal(format_args!("non-UTF-8 replay path {dest:?}")))?
        .to_owned();
    write_atomically(&dest, &player.body)
        .await
        .map_err(|e| ApiError::internal(format_args!("storing {}: {e}", dest.display())))?;

    let recorded = async {
        sqlx::query(
            "INSERT INTO uploads (game_id, slot, path, sha256, client_build, uploaded_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(game_id)
        .bind(seat.slot)
        .bind(&dest_str)
        .bind(&sha256)
        .bind(&client_build)
        .bind(now_unix())
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = recorded {
        // Not recorded, so not kept.
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(e.into());
    }
    state.wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// The caller's place in a set.
struct Seat {
    /// 0 is the host.
    slot: u8,
    /// `sets.state`.
    set_state: String,
}

/// Finds the caller's slot in an opened set: `404` when there is no such set, `403` when the
/// caller is not one of its two players.
async fn seat(
    conn: &mut SqliteConnection,
    event: i64,
    set: i64,
    key: &str,
) -> Result<Seat, ApiError> {
    let set_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM sets WHERE event_id = ? AND set_id = ?")
            .bind(event)
            .bind(set)
            .fetch_optional(&mut *conn)
            .await?;
    let set_state = set_state.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such set"))?;
    let me = entrant_id(conn, event, key).await?;
    let bracket = load_bracket_on(conn, event).await?;
    let bset = bracket_set(&bracket, set)?;
    let slot = me
        .and_then(|me| slot_of(bset, me))
        .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "you are not playing in this set"))?;
    Ok(Seat { slot, set_state })
}

/// The game a set is on: one more than its counted games.
struct CurrentGame {
    game_no: i64,
    /// Its row, once something was uploaded for it: `(id, state)`.
    open: Option<(i64, String)>,
}

async fn current_game(
    conn: &mut SqliteConnection,
    event: i64,
    set: i64,
) -> Result<CurrentGame, ApiError> {
    let counted: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM games WHERE event_id = ? AND set_id = ? AND state = 'counted'",
    )
    .bind(event)
    .bind(set)
    .fetch_one(&mut *conn)
    .await?;
    let game_no = counted + 1;
    let open: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, state FROM games
         WHERE event_id = ? AND set_id = ? AND game_no = ? AND state NOT IN ('counted', 'void')
         ORDER BY id DESC LIMIT 1",
    )
    .bind(event)
    .bind(set)
    .bind(game_no)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(CurrentGame { game_no, open })
}

/// The caller's entrant id (seed) in `event`, if entered.
async fn entrant_id(
    conn: &mut SqliteConnection,
    event: i64,
    key: &str,
) -> Result<Option<EntrantId>, ApiError> {
    let seed: Option<i64> =
        sqlx::query_scalar("SELECT seed FROM entrants WHERE event_id = ? AND player_key = ?")
            .bind(event)
            .bind(key)
            .fetch_optional(&mut *conn)
            .await?;
    seed.map(narrow).transpose()
}

/// The bracket set a `sets` row refers to.
fn bracket_set(bracket: &Bracket, set_id: i64) -> Result<&bracket::Set, ApiError> {
    usize::try_from(set_id)
        .ok()
        .and_then(|i| bracket.sets.get(i))
        .filter(|s| i64::from(s.id) == set_id)
        .ok_or_else(|| corrupt(format_args!("sets row {set_id} is not in the bracket")))
}

fn slot_of(set: &bracket::Set, me: EntrantId) -> Option<u8> {
    set.slots
        .iter()
        .position(|s| *s == Slot::Entrant(me))
        .and_then(|i| u8::try_from(i).ok())
}

/// Longest host name in a `host:port` room code (the DNS limit).
const MAX_ROOM_HOST_BYTES: usize = 253;

/// `^[A-Z0-9]{4}$`, or `^[A-Za-z0-9.\-]{1,253}:[0-9]{1,5}$` with the port in 1..=65535.
fn valid_room_code(code: &str) -> bool {
    let short = code.len() == 4
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let address = code.split_once(':').is_some_and(|(host, port)| {
        (1..=MAX_ROOM_HOST_BYTES).contains(&host.len())
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            && (1..=5).contains(&port.len())
            && port.bytes().all(|b| b.is_ascii_digit())
            && port.parse::<u16>().is_ok_and(|p| p != 0)
    });
    short || address
}

/// Writes `data` to a temporary file beside `dest`, then renames it into place, so `dest`
/// is never seen half-written. Creates missing directories.
async fn write_atomically(dest: &FsPath, data: &[u8]) -> std::io::Result<()> {
    let dir = dest
        .parent()
        .ok_or_else(|| std::io::Error::other("destination has no parent directory"))?;
    tokio::fs::create_dir_all(dir).await?;
    let mut suffix = [0u8; 8];
    OsRng.fill_bytes(&mut suffix);
    let tmp: PathBuf = dir.join(format!(".upload-{}.tmp", hex::encode(suffix)));
    let written = async {
        tokio::fs::write(&tmp, data).await?;
        tokio::fs::rename(&tmp, dest).await
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    written
}

fn bad_request(e: impl Display) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, e.to_string())
}

fn corrupt(e: impl Display) -> ApiError {
    ApiError::internal(format_args!("stored data is invalid: {e}"))
}

/// Narrows a stored integer, treating an out-of-range value as corrupt data.
fn narrow<T: TryFrom<i64>>(v: i64) -> Result<T, ApiError> {
    T::try_from(v).map_err(|_| corrupt(format_args!("stored integer {v} out of range")))
}

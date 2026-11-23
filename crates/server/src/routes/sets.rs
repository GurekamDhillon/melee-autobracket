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
use proto::{Next, NoShowKind, NoShowReq, Role, RoomReq, Ruleset};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

use super::{check_text, parse_body, ApiError, MAX_BUILD_CHARS};
use crate::auth::{Player, PlayerUpload};
use crate::db::{begin_write, now_unix};
use crate::lifecycle::{load_bracket_on, LifecycleError};
use crate::logging::{label, sha_prefix, Journal};
use crate::review::{self, NO_SHOW, STALLED};
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
            live_path: (slot == 0 && state.cfg.live_relay).then(|| {
                format!("/v1/sets/{event}/{}/games/{}/live", set.id, game.game_no)
            }),
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
    match room_code_kind(&req.code) {
        None => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "room code must be 4 characters of A-Z or 0-9, or host:port",
            ))
        }
        Some(RoomCode::Direct) if !state.cfg.allow_direct_connect => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "this server does not accept direct-connect (host:port) room codes",
            ))
        }
        Some(_) => {}
    }
    // A host whose upload is in must not start the same game again while the guest waits.
    if review::game_in_progress(&mut tx, event, set).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "a game of this set is in progress or awaiting a verdict",
        ));
    }
    let game = current_game(&mut tx, event, set).await?;
    let now = now_unix();
    sqlx::query(
        "UPDATE sets SET room_code = ?, room_posted_at = ?, updated_at = ?
         WHERE event_id = ? AND set_id = ?",
    )
    .bind(&req.code)
    .bind(now)
    .bind(now)
    .bind(event)
    .bind(set)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    // Never the code itself: it is the key to the netplay room.
    tracing::info!(event, set, game = game.game_no, "room code posted");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/sets/{event}/{set}/no-show` (`NoShowReq`; an empty body is a `no-show` with no
/// note): a player of the set reports that it cannot be played. The set goes to the organiser
/// queue (reason `no-show` or `client-failure`, detail naming the reporting slot and their
/// note) and to `review`, with no room code. `409` unless the set is `ready` with no game
/// uploading or awaiting a verdict, and for a `no-show`, unless the room code was posted at
/// least `no_show_grace_secs` ago.
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
    if let Some(note) = &req.detail {
        check_text("detail", note, review::MAX_NOTE_CHARS)?;
    }
    if review::game_in_progress(&mut tx, event, set).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "a game of this set is in progress or awaiting a verdict",
        ));
    }
    if req.kind == NoShowKind::NoShow {
        let grace = i64::try_from(state.cfg.no_show_grace_secs).unwrap_or(i64::MAX);
        let posted: Option<i64> =
            sqlx::query_scalar("SELECT room_posted_at FROM sets WHERE event_id = ? AND set_id = ?")
                .bind(event)
                .bind(set)
                .fetch_one(&mut *tx)
                .await?;
        if posted.is_some_and(|t| now_unix().saturating_sub(t) < grace) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("a room code was posted less than {grace} seconds ago"),
            ));
        }
    }
    let detail = review::no_show_detail(seat.slot, &player.tag, req.detail.as_deref());
    let mut journal = Journal::default();
    review::send_to_review_reported(
        &mut tx,
        event,
        narrow(set)?,
        req.kind.as_str(),
        Some(&detail),
        Some(seat.slot),
        &mut journal,
    )
    .await?;
    tx.commit().await?;
    // The note is the player's own text, so it stays in the queue item, not the log.
    tracing::info!(
        event,
        set,
        slot = seat.slot,
        tag = %label(&player.tag),
        kind = req.kind.as_str(),
        "no-show reported"
    );
    journal.emit();
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/sets/{event}/{set}/games/{n}/replay` (raw `.slp` bytes, header `x-ab-build`):
/// stores the caller's replay of game `n`. `202` when stored, `200` when the identical file
/// was already stored for this slot.
///
/// Accepted while the set is `ready`, and also while it is in `review` with every open queue
/// item of reason `no-show`, `stalled` or `client-failure` (the game may have been played
/// after all): such a game is judged, but its set is not in play, so its verdict is stored
/// and the game voided. Nothing counts; the organiser decides with the evidence. One game per
/// review: once it has left `uploading`, further files for it are refused.
pub async fn upload(
    State(state): State<AppState>,
    path: Result<Path<(i64, i64, i64)>, PathRejection>,
    headers: HeaderMap,
    PlayerUpload(player): PlayerUpload,
) -> Result<StatusCode, ApiError> {
    let Path((event, set, n)) = path.map_err(bad_request)?;
    let client_build = match headers.get("x-ab-build") {
        None => String::new(),
        Some(v) => v
            .to_str()
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid x-ab-build header"))?
            .to_owned(),
    };
    check_text("x-ab-build", &client_build, MAX_BUILD_CHARS)?;

    // Only a player of the set gets the file looked at (checked again under the write lock).
    {
        let mut conn = state.db.acquire().await?;
        seat(&mut conn, event, set, &player.key).await?;
    }
    // Parse, hash and write to a temporary file off the async runtime and before taking the
    // write lock, so other requests do not queue behind the disk.
    let body = player.body.clone();
    let incoming = state.cfg.replay_dir.join(INCOMING_DIR);
    let (parses, sha256, staged) = tokio::task::spawn_blocking(move || {
        let parses = replay::parse(&body).is_ok();
        let sha256 = hex::encode(Sha256::digest(&body));
        let staged = parses.then(|| Staged::write(&incoming, &body));
        (parses, sha256, staged)
    })
    .await
    .map_err(ApiError::internal)?;
    let staged = staged
        .transpose()
        .map_err(|e| ApiError::internal(format_args!("staging an upload: {e}")))?;

    let mut tx = begin_write(&state.db).await?;
    let seat = seat(&mut tx, event, set, &player.key).await?;
    let review_since = match seat.set_state.as_str() {
        "ready" => None,
        "review" => match review_awaiting_a_game(&mut tx, event, set).await? {
            Some(since) => Some(since),
            None => return Err(ApiError::new(StatusCode::CONFLICT, "set is not in play")),
        },
        _ => return Err(ApiError::new(StatusCode::CONFLICT, "set is not in play")),
    };
    let game = current_game(&mut tx, event, set).await?;
    if n != game.game_no {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("the current game is {}", game.game_no),
        ));
    }
    let Some(staged) = staged.filter(|_| parses) else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "not a readable replay file",
        ));
    };
    let open = match review_since {
        None => game.open,
        Some(since) => review_game(&mut tx, event, set, game.game_no, since).await?,
    };

    let game_id = match open {
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
    staged
        .persist(&dest)
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
    tracing::info!(
        event,
        set,
        game = n,
        game_id,
        slot = seat.slot,
        bytes = player.body.len(),
        sha256 = %sha_prefix(&sha256),
        client_build = %label(&client_build),
        in_review = review_since.is_some(),
        "replay uploaded"
    );
    state.wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// Queue reasons under which a set in `review` still takes the current game's replays: none
/// of them is about a game already uploaded.
const UPLOAD_IN_REVIEW_REASONS: [&str; 3] = [NO_SHOW, STALLED, NoShowKind::ClientFailure.as_str()];

/// For a set in `review`: when every open queue item has one of the
/// [`UPLOAD_IN_REVIEW_REASONS`], the time the earliest was opened (unix seconds); otherwise
/// (or with no open item) `None`.
async fn review_awaiting_a_game(
    conn: &mut SqliteConnection,
    event: i64,
    set: i64,
) -> Result<Option<i64>, ApiError> {
    let reasons: Vec<(String, i64)> = sqlx::query_as(
        "SELECT reason, created_at FROM queue
         WHERE event_id = ? AND set_id = ? AND state = 'open'",
    )
    .bind(event)
    .bind(set)
    .fetch_all(&mut *conn)
    .await?;
    let all_awaiting = !reasons.is_empty()
        && reasons
            .iter()
            .all(|(r, _)| UPLOAD_IN_REVIEW_REASONS.contains(&r.as_str()));
    Ok(all_awaiting
        .then(|| reasons.iter().map(|&(_, at)| at).min())
        .flatten())
}

/// The game a review has opened for `game_no` (first uploaded at or after `since`), in any
/// state: `(id, state)`.
async fn review_game(
    conn: &mut SqliteConnection,
    event: i64,
    set: i64,
    game_no: i64,
    since: i64,
) -> Result<Option<(i64, String)>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT id, state FROM games
         WHERE event_id = ? AND set_id = ? AND game_no = ? AND first_upload_at >= ?
         ORDER BY id DESC LIMIT 1",
    )
    .bind(event)
    .bind(set)
    .bind(game_no)
    .bind(since)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Directory under `replay_dir` where uploads are written before they are recorded.
const INCOMING_DIR: &str = ".incoming";

/// An upload written to a temporary file, removed on drop unless persisted.
struct Staged(Option<PathBuf>);

impl Staged {
    /// Writes `data` to a new temporary file in `dir` (created if missing).
    fn write(dir: &FsPath, data: &[u8]) -> std::io::Result<Staged> {
        std::fs::create_dir_all(dir)?;
        let mut suffix = [0u8; 8];
        OsRng.fill_bytes(&mut suffix);
        let path = dir.join(format!("upload-{}.tmp", hex::encode(suffix)));
        let staged = Staged(Some(path.clone()));
        std::fs::write(&path, data)?;
        Ok(staged)
    }

    /// Renames the file to `dest`, creating missing directories, so `dest` is never seen
    /// half-written.
    async fn persist(mut self, dest: &FsPath) -> std::io::Result<()> {
        let dir = dest
            .parent()
            .ok_or_else(|| std::io::Error::other("destination has no parent directory"))?;
        tokio::fs::create_dir_all(dir).await?;
        let Some(tmp) = self.0.as_ref() else {
            return Err(std::io::Error::other("upload already persisted"));
        };
        tokio::fs::rename(tmp, dest).await?;
        self.0 = None;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
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

/// The two forms of room code.
enum RoomCode {
    /// A relay room: `^[A-Z0-9]{4}$`.
    Relay,
    /// A direct connection to the host: `^[A-Za-z0-9.\-]{1,253}:[0-9]{1,5}$`, port 1..=65535.
    Direct,
}

/// Which form `code` has, or `None` when it has neither.
fn room_code_kind(code: &str) -> Option<RoomCode> {
    let short = code.len() == 4
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if short {
        Some(RoomCode::Relay)
    } else if valid_room_code(code) {
        Some(RoomCode::Direct)
    } else {
        None
    }
}

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

/// The live relay's ingest checks (docs/design/live-relay.md section 2), after the signature:
/// 404 for an unknown set, 403 unless `key` is the set's host (slot 0), 409 unless `n` is the
/// set's current game and the set is in play.
pub(crate) async fn live_ingest_check(
    state: &AppState,
    event: i64,
    set: i64,
    n: i64,
    key: &str,
) -> Result<(), ApiError> {
    let mut tx = state.db.begin().await?;
    let seat = seat(&mut tx, event, set, key).await?;
    if seat.slot != 0 {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only the host streams the game live",
        ));
    }
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
    Ok(())
}

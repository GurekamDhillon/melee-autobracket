//! The live relay's routes (docs/design/live-relay.md): the host's signed ingest WebSocket, the
//! public viewer WebSocket and the public listing. Display only; nothing here touches results.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use super::ApiError;
use crate::db::now_unix;
use crate::live::{Ingest, LiveInfo, MAX_MESSAGE_BYTES};
use crate::state::AppState;

/// An ingest connection with nothing for this long is closed (keepalives come every 2 s).
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// `GET /v1/sets/{event}/{set}/games/{n}/live`: the host's engine streams the game. Signed like
/// any request (empty body); 401, 404, 403 and 409 are answered before the upgrade.
pub async fn ingest(State(state): State<AppState>, req: Request) -> Result<Response, ApiError> {
    if !state.cfg.live_relay {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "live relay is off on this server"));
    }
    let (mut parts, _body) = req.into_parts();
    let Path((event, set, n)) = Path::<(i64, i64, i64)>::from_request_parts(&mut parts, &state)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let key = crate::auth::upgrade_player(&state, &parts).await?;
    super::sets::live_ingest_check(&state, event, set, n, &key).await?;
    let ws = WebSocketUpgrade::from_request_parts(&mut parts, &state)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.body_text()))?;
    let live = state.live.set(event, set);
    let generation = live.connect(n, now_unix());
    tracing::info!(event, set, game = n, generation, "live: host connected");
    Ok(ws
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| run_ingest(socket, live, generation, event, set, n)))
}

async fn run_ingest(
    mut socket: WebSocket,
    live: std::sync::Arc<crate::live::LiveSet>,
    generation: u64,
    event: i64,
    set: i64,
    n: i64,
) {
    let why = loop {
        let msg = match tokio::time::timeout(IDLE_TIMEOUT, socket.recv()).await {
            Err(_) => break "idle",
            Ok(None) | Ok(Some(Err(_))) => break "closed",
            Ok(Some(Ok(m))) => m,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break "closed",
            Message::Binary(_) => break "binary message",
            _ => continue,
        };
        match live.ingest(generation, text.as_str()) {
            Ingest::Ok => {}
            Ingest::Reply(cursor) => {
                let reply = json!({ "type": "connect_reply", "cursor": cursor }).to_string();
                if socket.send(Message::Text(reply.into())).await.is_err() {
                    break "closed";
                }
            }
            Ingest::Close(why) => {
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: 1008,
                        reason: why.into(),
                    })))
                    .await;
                break why;
            }
        }
    };
    tracing::info!(event, set, game = n, generation, why, "live: host connection ended");
}

/// `GET /v1/live/{event}/{set}`: a viewer's WebSocket (public). The server sends the current
/// game's head and recent messages, then every message as it arrives, as the ingest's JSON
/// objects (`start_game`, `game_event` with base64 `payload`, `end_game`), each with `game_no`.
/// It reads nothing from the viewer but close frames.
pub async fn watch(
    State(state): State<AppState>,
    Path((event, set)): Path<(i64, i64)>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    if !state.cfg.live_relay {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "live relay is off on this server"));
    }
    let live = state
        .live
        .get(event, set)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "this set is not live"))?;
    let viewer = live
        .watch()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "too many viewers"))?;
    Ok(ws.on_upgrade(move |socket| run_viewer(socket, viewer)).into_response())
}

async fn run_viewer(socket: WebSocket, mut viewer: crate::live::Viewer) {
    let (mut tx, mut rx) = socket.split();
    for m in std::mem::take(&mut viewer.backlog) {
        if tx.send(Message::Text(m.as_ref().into())).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            got = viewer.rx.recv() => match got {
                Ok(m) => {
                    if tx.send(Message::Text(m.as_ref().into())).await.is_err() {
                        return;
                    }
                }
                // A slow viewer misses some frames; display only, it carries on.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => {
                    let _ = tx.send(Message::Close(None)).await;
                    return;
                }
            },
            m = rx.next() => match m {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    event: Option<i64>,
}

/// `GET /v1/live[?event={id}]`: the sets with a streamed game (public).
pub async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Json<Vec<LiveInfo>> {
    if !state.cfg.live_relay {
        return Json(Vec::new());
    }
    Json(state.live.list(q.event))
}

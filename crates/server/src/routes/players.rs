use axum::extract::State;
use axum::http::StatusCode;
use proto::RegisterReq;

use super::ApiError;
use crate::auth::AnyKey;
use crate::db::{begin_write, now_unix};
use crate::logging::key_prefix;
use crate::state::AppState;

/// `POST /v1/players`: binds the signing key to a tag.
pub async fn register(State(state): State<AppState>, req: AnyKey) -> Result<StatusCode, ApiError> {
    let body: RegisterReq = serde_json::from_slice(&req.body).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid request body: {e}"),
        )
    })?;
    if !valid_tag(&body.tag) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "tag must be 2-16 characters of A-Z, a-z, 0-9 or _",
        ));
    }

    let mut tx = begin_write(&state.db).await?;
    let existing: Option<String> = sqlx::query_scalar("SELECT key FROM players WHERE key = ?")
        .bind(&req.key)
        .fetch_optional(&mut *tx)
        .await?;
    if existing.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "key is already registered",
        ));
    }

    if state.cfg.require_invite {
        let code = body
            .invite
            .as_deref()
            .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "an invite code is required"))?;
        let used = sqlx::query("UPDATE invites SET used_by = ? WHERE code = ? AND used_by IS NULL")
            .bind(&req.key)
            .bind(code)
            .execute(&mut *tx)
            .await?;
        if used.rows_affected() != 1 {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "invalid or used invite code",
            ));
        }
    }

    let inserted = sqlx::query("INSERT INTO players (key, tag, created_at) VALUES (?, ?, ?)")
        .bind(&req.key)
        .bind(&body.tag)
        .bind(now_unix())
        .execute(&mut *tx)
        .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::new(StatusCode::CONFLICT, "tag is taken"));
        }
        Err(e) => return Err(e.into()),
    }
    tx.commit().await?;
    tracing::info!(tag = %body.tag, key = %key_prefix(&req.key), "player registered");
    Ok(StatusCode::CREATED)
}

/// `^[A-Za-z0-9_]{2,16}$`
fn valid_tag(tag: &str) -> bool {
    (2..=16).contains(&tag.len()) && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

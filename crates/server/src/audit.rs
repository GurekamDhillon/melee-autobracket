//! The audit log: one append-only row per organiser action or account change, with its actor.

use serde::Serialize;
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

use crate::db::now_unix;
use crate::identity::WebUser;

/// Hex digits of a key kept as an actor id.
const ACTOR_KEY_CHARS: usize = 16;

/// Who did something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    /// A signed request; the full public key (only its first 16 hex digits are stored).
    Key(String),
    /// A signed-in web user: the account id and the display name at the time.
    Account { id: i64, name: Option<String> },
    /// A Zitadel hook.
    Zitadel,
    /// The server itself.
    System,
}

impl From<&WebUser> for Actor {
    fn from(user: &WebUser) -> Self {
        Actor::Account {
            id: user.account_id,
            name: user.display_name.clone(),
        }
    }
}

impl Actor {
    /// `(actor_kind, actor_id, actor_name)` as stored.
    fn columns(&self) -> (&'static str, String, Option<String>) {
        match self {
            Actor::Key(key) => ("key", key.chars().take(ACTOR_KEY_CHARS).collect(), None),
            Actor::Account { id, name } => ("account", id.to_string(), name.clone()),
            Actor::Zitadel => ("zitadel", "zitadel".into(), None),
            Actor::System => ("system", "system".into(), None),
        }
    }
}

/// A row of the audit log.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AuditEntry {
    pub id: i64,
    pub at: i64,
    pub actor_kind: String,
    pub actor_id: String,
    pub actor_name: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<Value>,
}

/// Appends one row. Call it inside the transaction that makes the change it records.
pub async fn record(
    conn: &mut SqliteConnection,
    actor: &Actor,
    action: &str,
    target: Option<&str>,
    detail: Option<Value>,
) -> Result<(), sqlx::Error> {
    let (kind, id, name) = actor.columns();
    sqlx::query(
        "INSERT INTO audit_log (at, actor_kind, actor_id, actor_name, action, target, detail_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(now_unix())
    .bind(kind)
    .bind(id)
    .bind(name)
    .bind(action)
    .bind(target)
    .bind(detail.map(|d| d.to_string()))
    .execute(conn)
    .await?;
    Ok(())
}

/// Up to `limit` rows, newest first, with ids below `before` when given.
pub async fn list(
    db: &SqlitePool,
    before: Option<i64>,
    limit: u32,
) -> Result<Vec<AuditEntry>, sqlx::Error> {
    type Row = (
        i64,
        i64,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, at, actor_kind, actor_id, actor_name, action, target, detail_json
         FROM audit_log WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
    )
    .bind(before)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, at, actor_kind, actor_id, actor_name, action, target, detail)| AuditEntry {
                id,
                at,
                actor_kind,
                actor_id,
                actor_name,
                action,
                target,
                // Written by `record` from a `Value`; anything else is kept as text.
                detail: detail.map(|d| serde_json::from_str(&d).unwrap_or(Value::String(d))),
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_and_list_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(&dir.path().join("a.sqlite")).await.unwrap();
        let mut conn = db.acquire().await.unwrap();
        let key = "ab".repeat(32);
        record(&mut conn, &Actor::Key(key), "event.create", Some("1"), None)
            .await
            .unwrap();
        let actor = Actor::Account {
            id: 7,
            name: Some("Mango".into()),
        };
        let detail = serde_json::json!({"tag": "x"});
        record(
            &mut conn,
            &actor,
            "player.rekey",
            None,
            Some(detail.clone()),
        )
        .await
        .unwrap();
        record(
            &mut conn,
            &Actor::Zitadel,
            "account.removed",
            Some("7"),
            None,
        )
        .await
        .unwrap();
        drop(conn);

        let all = list(&db, None, 50).await.unwrap();
        let summary: Vec<(&str, &str, Option<&str>, &str)> = all
            .iter()
            .map(|e| {
                (
                    e.actor_kind.as_str(),
                    e.actor_id.as_str(),
                    e.actor_name.as_deref(),
                    e.action.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("zitadel", "zitadel", None, "account.removed"),
                ("account", "7", Some("Mango"), "player.rekey"),
                ("key", "abababababababab", None, "event.create"),
            ]
        );
        assert_eq!(all[1].detail, Some(detail));
        assert_eq!(all[2].target.as_deref(), Some("1"));

        let older = list(&db, Some(all[0].id), 1).await.unwrap();
        assert_eq!(older.len(), 1);
        assert_eq!(older[0].id, all[1].id);
    }
}

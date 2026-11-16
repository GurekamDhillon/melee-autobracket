//! Migrations apply to databases made by earlier versions of the server.

use std::borrow::Cow;

use autobracket_server::db::{self, now_unix};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A database at the 0001 schema with a running event, a set, a game and a queue item, then
/// opened by the current server: 0002 applies and gives the old rows sane values.
#[tokio::test]
async fn migration_0002_applies_to_a_populated_0001_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");

    let mut first = sqlx::migrate!("./migrations");
    assert!(first.migrations.len() >= 2, "expected 0001 and 0002");
    first.migrations = Cow::Owned(vec![first.migrations[0].clone()]);
    assert_eq!(first.migrations[0].version, 1);
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
    first.run(&pool).await.unwrap();
    for sql in [
        "INSERT INTO players (key, tag, created_at) VALUES ('k0', 'Alice', 1), ('k1', 'Bob', 1)",
        "INSERT INTO events (id, name, format, ruleset_json, state, bracket_json, created_at)
         VALUES (1, 'Old', 'single_elim', '{}', 'running', NULL, 1)",
        "INSERT INTO entrants (event_id, player_key, seed, engine_build) VALUES (1, 'k0', 0, 'b'),
         (1, 'k1', 1, 'b')",
        "INSERT INTO sets (event_id, set_id, state, room_code, wins0, wins1)
         VALUES (1, 0, 'ready', 'AB12', 1, 0)",
        "INSERT INTO games (id, event_id, set_id, game_no, state, verdict_json, winner_slot,
                            first_upload_at, resim_json, verifier_build)
         VALUES (1, 1, 0, 1, 'counted', '\"Verified\"', 0, 1, '\"Match\"', 'old-build')",
        "INSERT INTO queue (id, event_id, set_id, game_id, reason, state, created_at)
         VALUES (1, 1, 0, NULL, 'Conflicted', 'open', 1)",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    pool.close().await;

    let before = now_unix();
    let pool = db::open(&path).await.unwrap();
    let (updated_at, decided_by, room_code): (Option<i64>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT updated_at, decided_by, room_code FROM sets")
            .fetch_one(&pool)
            .await
            .unwrap();
    let updated_at = updated_at.expect("updated_at");
    assert!(
        updated_at >= before - 1 && updated_at <= now_unix() + 1,
        "{updated_at}"
    );
    assert_eq!(decided_by, None);
    assert_eq!(room_code.as_deref(), Some("AB12"));
    let (judged_at, build): (Option<i64>, Option<String>) =
        sqlx::query_as("SELECT judged_at, verifier_build FROM games")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(judged_at, None);
    assert_eq!(build.as_deref(), Some("old-build"));
    let detail: Option<String> = sqlx::query_scalar("SELECT detail FROM queue")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(detail, None);

    // Opening again is a no-op.
    pool.close().await;
    db::open(&path).await.unwrap();
}

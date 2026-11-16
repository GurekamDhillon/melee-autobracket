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

/// A database at the 0002 schema with a game, a queue item and a verifier job, then opened by
/// the current server: 0003 adds the judging attempt count, the evidence and the no-show
/// reporter, and the indexes.
#[tokio::test]
async fn migration_0003_applies_to_a_populated_0002_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");

    let mut old = sqlx::migrate!("./migrations");
    assert!(old.migrations.len() >= 3, "expected 0001 to 0003");
    old.migrations = Cow::Owned(old.migrations[..2].to_vec());
    assert_eq!(old.migrations[1].version, 2);
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
    old.run(&pool).await.unwrap();
    for sql in [
        "INSERT INTO events (id, name, format, ruleset_json, state, bracket_json, created_at)
         VALUES (1, 'Old', 'single_elim', '{}', 'running', NULL, 1)",
        "INSERT INTO sets (event_id, set_id, state, room_code, wins0, wins1, updated_at)
         VALUES (1, 0, 'review', NULL, 0, 0, 5)",
        "INSERT INTO games (id, event_id, set_id, game_no, state, verdict_json, first_upload_at,
                            judged_at)
         VALUES (1, 1, 0, 1, 'review', '\"Single\"', 1, 2)",
        "INSERT INTO queue (id, event_id, set_id, game_id, reason, state, created_at, detail)
         VALUES (1, 1, 0, 1, 'Single', 'open', 2, NULL),
                (2, 1, 0, NULL, 'no-show', 'open', 3, 'reported by slot 0 (A)')",
        "INSERT INTO resim_jobs (id, game_id, path, replay_last_frame, state)
         VALUES (1, 1, '/r.slp', 10, 'done')",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    pool.close().await;

    let pool = db::open(&path).await.unwrap();
    let (attempts, evidence, judged_at): (i64, Option<String>, Option<i64>) =
        sqlx::query_as("SELECT judge_attempts, evidence_json, judged_at FROM games")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((attempts, evidence, judged_at), (0, None, Some(2)));
    let reporters: Vec<(i64, Option<i64>, Option<String>)> =
        sqlx::query_as("SELECT id, reporter_slot, detail FROM queue ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        reporters,
        vec![
            (1, None, None),
            (2, None, Some("reported by slot 0 (A)".into()))
        ]
    );
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'core_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        vec![
            "core_games_set",
            "core_games_state",
            "core_queue_state",
            "core_resim_jobs_state"
        ]
    );

    // Opening again is a no-op.
    pool.close().await;
    db::open(&path).await.unwrap();
}

/// A database at the 0004 schema with an event, then opened by the current server: 0005 adds
/// the start time and the description, both empty for the old row.
#[tokio::test]
async fn migration_0005_applies_to_a_populated_0004_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");

    let mut old = sqlx::migrate!("./migrations");
    assert!(old.migrations.len() >= 5, "expected 0001 to 0005");
    old.migrations = Cow::Owned(old.migrations[..4].to_vec());
    assert_eq!(old.migrations[3].version, 4);
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
    old.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO events (id, name, format, ruleset_json, state, bracket_json, created_at)
         VALUES (1, 'Old', 'single_elim', '{}', 'signup', NULL, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let pool = db::open(&path).await.unwrap();
    let (starts_at, description): (Option<i64>, Option<String>) =
        sqlx::query_as("SELECT starts_at, description FROM events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((starts_at, description), (None, None));

    // Opening again is a no-op.
    pool.close().await;
    db::open(&path).await.unwrap();
}

/// A database at the 0005 schema with phase-1 data (players, an invite, a running event with
/// entrants, a set, a game and a queue item), then opened by the current server: 0006 adds
/// the identity tables and columns, and every old row is intact.
#[tokio::test]
async fn migration_0006_applies_to_a_populated_0005_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");

    let mut old = sqlx::migrate!("./migrations");
    assert!(old.migrations.len() >= 6, "expected 0001 to 0006");
    old.migrations = Cow::Owned(old.migrations[..5].to_vec());
    assert_eq!(old.migrations[4].version, 5);
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
    old.run(&pool).await.unwrap();
    for sql in [
        "INSERT INTO players (key, tag, created_at) VALUES ('k0', 'Alice', 1), ('k1', 'Bob', 2)",
        "INSERT INTO invites (code, used_by) VALUES ('inv', 'k0')",
        "INSERT INTO events (id, name, format, ruleset_json, state, bracket_json, created_at,
                             starts_at, description)
         VALUES (1, 'Old', 'single_elim', '{}', 'running', NULL, 1, 100, 'weekly')",
        "INSERT INTO entrants (event_id, player_key, seed, engine_build) VALUES (1, 'k0', 0, 'b'),
         (1, 'k1', 1, 'b')",
        "INSERT INTO sets (event_id, set_id, state, room_code, wins0, wins1, updated_at)
         VALUES (1, 0, 'ready', 'AB12', 1, 0, 5)",
        "INSERT INTO games (id, event_id, set_id, game_no, state, verdict_json, winner_slot,
                            first_upload_at)
         VALUES (1, 1, 0, 1, 'counted', '\"Verified\"', 0, 1)",
        "INSERT INTO queue (id, event_id, set_id, game_id, reason, state, created_at)
         VALUES (1, 1, 0, NULL, 'Conflicted', 'open', 1)",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    pool.close().await;

    let pool = db::open(&path).await.unwrap();
    let players: Vec<(String, String, i64, Option<i64>)> =
        sqlx::query_as("SELECT key, tag, created_at, account_id FROM players ORDER BY key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        players,
        vec![
            ("k0".into(), "Alice".into(), 1, None),
            ("k1".into(), "Bob".into(), 2, None)
        ]
    );
    let used_by: String = sqlx::query_scalar("SELECT used_by FROM invites")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(used_by, "k0");
    let entrants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entrants")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(entrants, 2);
    let set: (String, Option<String>, i64, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT state, room_code, wins0, ready0_at, ready1_at FROM sets WHERE set_id = 0",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(set, ("ready".into(), Some("AB12".into()), 1, None, None));
    let (description, games, queue): (Option<String>, i64, i64) = sqlx::query_as(
        "SELECT (SELECT description FROM events), (SELECT COUNT(*) FROM games),
                (SELECT COUNT(*) FROM queue)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (description.as_deref(), games, queue),
        (Some("weekly"), 1, 1)
    );
    for table in [
        "accounts",
        "account_tombstones",
        "discord_profiles",
        "player_keys",
        "link_codes",
        "audit_log",
        "stage_moves",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    // One player per account.
    sqlx::query("INSERT INTO accounts (id, sub, created_at, last_seen_at) VALUES (1, 's', 1, 1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE players SET account_id = 1 WHERE key = 'k0'")
        .execute(&pool)
        .await
        .unwrap();
    let second = sqlx::query("UPDATE players SET account_id = 1 WHERE key = 'k1'")
        .execute(&pool)
        .await;
    assert!(second.is_err(), "a second player for one account");

    // Opening again is a no-op.
    pool.close().await;
    db::open(&path).await.unwrap();
}

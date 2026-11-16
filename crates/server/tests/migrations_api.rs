//! Migration 0004 (API surface) applies to databases made by earlier versions of the server,
//! including one that already holds tags differing only in case.

use std::borrow::Cow;

use autobracket_server::db;
use autobracket_server::testkit::TestApp;
use proto::{sign_request, RegisterReq, SigningKey};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// Creates `path` with every migration before 0004 applied, then runs `rows`.
async fn database_before_0004(path: &std::path::Path, rows: &[&'static str]) {
    let mut earlier = sqlx::migrate!("./migrations");
    let before: Vec<_> = earlier
        .migrations
        .iter()
        .filter(|m| m.version < 4)
        .cloned()
        .collect();
    assert!(before.len() < earlier.migrations.len(), "0004 exists");
    earlier.migrations = Cow::Owned(before);
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
    earlier.run(&pool).await.unwrap();
    for &sql in rows {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    pool.close().await;
}

#[tokio::test]
async fn upgrade_keeps_case_variant_tags_and_refuses_new_ones() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    database_before_0004(
        &path,
        &[
            "INSERT INTO players (key, tag, created_at) VALUES
             ('k-mango', 'mango', 1), ('k-MANGO', 'MANGO', 2), ('k-Mango', 'Mango', 2),
             ('k-bob', 'Bob', 1)",
            "INSERT INTO sets (event_id, set_id, state, room_code, wins0, wins1)
             VALUES (1, 0, 'ready', 'AB12', 0, 0)",
        ],
    )
    .await;

    // The migration applies (a plain unique index would fail here) and keeps every player.
    let pool = db::open(&path).await.unwrap();
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT tag, tag_case_duplicate FROM players ORDER BY key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("MANGO".into(), 1),
            ("Mango".into(), 1),
            ("Bob".into(), 0),
            ("mango".into(), 0), // the earliest registered
        ]
    );
    let posted: Option<i64> = sqlx::query_scalar("SELECT room_posted_at FROM sets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(posted, None);

    // The index still refuses any new case variant, of a duplicated tag or another.
    for tag in ["MaNgO", "BOB"] {
        let err = sqlx::query("INSERT INTO players (key, tag, created_at) VALUES (?, ?, 3)")
            .bind(format!("new-{tag}"))
            .bind(tag)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            err.as_database_error()
                .is_some_and(|e| e.is_unique_violation()),
            "{tag}: {err}"
        );
    }
    sqlx::query("INSERT INTO players (key, tag, created_at) VALUES ('new', 'Carol', 3)")
        .execute(&pool)
        .await
        .unwrap();

    // Opening again is a no-op.
    pool.close().await;
    db::open(&path).await.unwrap();
}

/// Through the API: a case variant of a registered tag is 409.
#[tokio::test]
async fn registration_refuses_a_case_variant() {
    let app = TestApp::spawn(|_| {}).await;
    app.player("Mango").await;
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let body = serde_json::to_vec(&RegisterReq {
        tag: "mANGO".into(),
        invite: None,
    })
    .unwrap();
    let h = sign_request(&sk, "POST", "/v1/players", db::now_unix(), &body);
    let res = reqwest::Client::new()
        .post(format!("{}/v1/players", app.url))
        .header("x-ab-key", h.key)
        .header("x-ab-time", h.time)
        .header("x-ab-nonce", h.nonce)
        .header("x-ab-sig", h.sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::CONFLICT);
}

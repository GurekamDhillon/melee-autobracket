//! The account store: web accounts, the player an account owns, linked client keys
//! (`player_keys`), placeholder keys and rekeying.
//!
//! `players.key` stays a player's canonical id (what `entrants.player_key` refers to). A
//! signing key is that key or one in `player_keys`; [`resolve_key`] maps it to the canonical
//! one. A player made from the web gets a **placeholder** canonical key: 32 random bytes that
//! are not a valid Ed25519 public key, so nobody can ever sign as it and [`is_placeholder`]
//! can tell it from a device key.

use ed25519_dalek::VerifyingKey;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Serialize;
use sqlx::SqliteConnection;

use crate::config::IdentityConfig;
use crate::db::now_unix;
use crate::identity::sha256_hex;

/// Hex digits of a key shown to its owner.
const KEY_PREFIX_HEX: usize = 16;

/// A fresh placeholder key: 64 lowercase hex characters (32 random bytes) that do not decode
/// to an Ed25519 public key.
pub fn placeholder_key() -> String {
    loop {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        // About half of all 32-byte strings are not a curve point; keep one of those.
        if VerifyingKey::from_bytes(&bytes).is_err() {
            return hex::encode(bytes);
        }
    }
}

/// Whether `key` is a placeholder: not a valid Ed25519 public key, so no device holds it.
pub fn is_placeholder(key: &str) -> bool {
    let Ok(bytes) = hex::decode(key) else {
        return true;
    };
    let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) else {
        return true;
    };
    VerifyingKey::from_bytes(&bytes).is_err()
}

/// A player tag: `^[A-Za-z0-9_]{2,16}$`.
pub fn valid_tag(tag: &str) -> bool {
    (2..=16).contains(&tag.len()) && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The player `account_id` owns: `(canonical key, tag)`.
pub async fn account_player(
    conn: &mut SqliteConnection,
    account_id: i64,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT key, tag FROM players WHERE account_id = ?")
        .bind(account_id)
        .fetch_optional(conn)
        .await
}

#[derive(Debug, thiserror::Error)]
pub enum CreatePlayerError {
    #[error("the account already has a player")]
    HasPlayer,
    #[error("tag must be 2-16 characters of A-Z, a-z, 0-9 or _")]
    BadTag,
    #[error("tag is taken")]
    TagTaken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Creates the player of `account_id` under `tag`, with a placeholder key; returns the key.
/// Run it in a write transaction.
pub async fn create_player(
    conn: &mut SqliteConnection,
    account_id: i64,
    tag: &str,
) -> Result<String, CreatePlayerError> {
    if !valid_tag(tag) {
        return Err(CreatePlayerError::BadTag);
    }
    if account_player(&mut *conn, account_id).await?.is_some() {
        return Err(CreatePlayerError::HasPlayer);
    }
    let taken: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM players WHERE tag = ? COLLATE NOCASE LIMIT 1")
            .bind(tag)
            .fetch_optional(&mut *conn)
            .await?;
    if taken.is_some() {
        return Err(CreatePlayerError::TagTaken);
    }
    let key = placeholder_key();
    let inserted =
        sqlx::query("INSERT INTO players (key, tag, created_at, account_id) VALUES (?, ?, ?, ?)")
            .bind(&key)
            .bind(tag)
            .bind(now_unix())
            .bind(account_id)
            .execute(&mut *conn)
            .await;
    match inserted {
        Ok(_) => Ok(key),
        // The tag index or the one-player-per-account index.
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            if account_player(&mut *conn, account_id).await?.is_some() {
                Err(CreatePlayerError::HasPlayer)
            } else {
                Err(CreatePlayerError::TagTaken)
            }
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RekeyError {
    #[error("no such player")]
    NoSuchPlayer,
    #[error("the new key is already in use")]
    KeyInUse,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Moves the player whose canonical key is `old` to `new`: the player row, their entries,
/// the invite they used and their linked keys. `new` must be in neither `players` nor
/// `player_keys`. Run it in a write transaction.
pub async fn rekey_player(
    conn: &mut SqliteConnection,
    old: &str,
    new: &str,
) -> Result<(), RekeyError> {
    let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM players WHERE key = ?")
        .bind(old)
        .fetch_optional(&mut *conn)
        .await?;
    if known.is_none() {
        return Err(RekeyError::NoSuchPlayer);
    }
    if key_in_use(&mut *conn, new).await? {
        return Err(RekeyError::KeyInUse);
    }
    for sql in [
        "UPDATE players SET key = ?1 WHERE key = ?2",
        "UPDATE entrants SET player_key = ?1 WHERE player_key = ?2",
        "UPDATE invites SET used_by = ?1 WHERE used_by = ?2",
        "UPDATE player_keys SET player_key = ?1 WHERE player_key = ?2",
    ] {
        match sqlx::query(sql)
            .bind(new)
            .bind(old)
            .execute(&mut *conn)
            .await
        {
            Ok(_) => {}
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(RekeyError::KeyInUse)
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Whether `key` is a player's canonical key or a linked key.
pub async fn key_in_use(conn: &mut SqliteConnection, key: &str) -> Result<bool, sqlx::Error> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM players WHERE key = ?1 UNION ALL SELECT 1 FROM player_keys WHERE key = ?1
         LIMIT 1",
    )
    .bind(key)
    .fetch_optional(conn)
    .await?;
    Ok(found.is_some())
}

/// The player a signing key belongs to: `(canonical key, tag)`, through `players.key` or
/// `player_keys.key`.
pub async fn resolve_key(
    conn: &mut SqliteConnection,
    signer: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT key, tag FROM players WHERE key = ?1
         UNION ALL
         SELECT p.key, p.tag FROM player_keys k JOIN players p ON p.key = k.player_key
         WHERE k.key = ?1
         LIMIT 1",
    )
    .bind(signer)
    .fetch_optional(conn)
    .await
}

/// What [`bind_discord`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindOutcome {
    /// A new account for `sub` with that Discord id.
    Created,
    /// The existing account had no Discord id; now it has this one.
    Bound,
    /// The account already had this Discord id.
    Unchanged,
    /// Nothing changed: the account has another Discord id, the Discord id belongs to
    /// another account, or `sub` was removed.
    Conflict,
}

/// Records that Zitadel user `sub` is Discord user `discord_id`. The only writer of
/// `accounts.discord_id`: it sets it once and never re-binds either side. Run it in a write
/// transaction.
pub async fn bind_discord(
    conn: &mut SqliteConnection,
    sub: &str,
    discord_id: &str,
) -> Result<BindOutcome, sqlx::Error> {
    let tombstoned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM account_tombstones WHERE sub_sha256 = ?")
            .bind(sha256_hex(sub))
            .fetch_optional(&mut *conn)
            .await?;
    if tombstoned.is_some() {
        return Ok(BindOutcome::Conflict);
    }
    let owner: Option<String> = sqlx::query_scalar("SELECT sub FROM accounts WHERE discord_id = ?")
        .bind(discord_id)
        .fetch_optional(&mut *conn)
        .await?;
    let existing: Option<Option<String>> =
        sqlx::query_scalar("SELECT discord_id FROM accounts WHERE sub = ?")
            .bind(sub)
            .fetch_optional(&mut *conn)
            .await?;
    match (existing, owner) {
        (Some(Some(current)), _) if current == discord_id => Ok(BindOutcome::Unchanged),
        (Some(Some(_)), _) => Ok(BindOutcome::Conflict),
        (_, Some(_)) => Ok(BindOutcome::Conflict),
        (Some(None), None) => {
            let done = sqlx::query(
                "UPDATE accounts SET discord_id = ? WHERE sub = ? AND discord_id IS NULL",
            )
            .bind(discord_id)
            .bind(sub)
            .execute(&mut *conn)
            .await;
            unique_is_conflict(done, BindOutcome::Bound)
        }
        (None, None) => {
            let now = now_unix();
            let done = sqlx::query(
                "INSERT INTO accounts (sub, discord_id, created_at, last_seen_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(sub)
            .bind(discord_id)
            .bind(now)
            .bind(now)
            .execute(&mut *conn)
            .await;
            unique_is_conflict(done, BindOutcome::Created)
        }
    }
}

/// `ok` when one row changed; a unique violation (a concurrent writer) is a conflict.
fn unique_is_conflict(
    done: Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error>,
    ok: BindOutcome,
) -> Result<BindOutcome, sqlx::Error> {
    match done {
        Ok(r) if r.rows_affected() == 1 => Ok(ok),
        Ok(_) => Ok(BindOutcome::Conflict),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(BindOutcome::Conflict),
        Err(e) => Err(e),
    }
}

/// `GET /account/api/me` and the other account routes' answer.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AccountView {
    /// The Discord global name, else the username; null before the first Discord login.
    pub display_name: Option<String>,
    pub discord_username: Option<String>,
    /// The Discord avatar, or Discord's default avatar; null without a Discord id.
    pub avatar_url: Option<String>,
    pub tag: Option<String>,
    pub connect_code: Option<String>,
    pub notify: bool,
    pub share_analysis: bool,
    /// The verified token's groups hold the organiser group (MFA not checked here).
    pub is_admin: bool,
    /// The player's canonical key first, then linked keys oldest first.
    pub clients: Vec<ClientView>,
    /// Events in sign-up or running that the player entered, by id.
    pub entered: Vec<i64>,
    pub sign_out_url: String,
}

/// A client key of the account's player.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ClientView {
    /// The key's first 16 hex digits.
    pub key_prefix: String,
    pub label: Option<String>,
    pub linked_at: i64,
    /// A placeholder key: no device holds it.
    pub placeholder: bool,
}

/// The account page's view of `account_id`.
pub async fn view(
    conn: &mut SqliteConnection,
    account_id: i64,
    cfg: &IdentityConfig,
    is_admin: bool,
) -> Result<AccountView, sqlx::Error> {
    type Row = (
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let (discord_id, connect_code, notify, share_analysis, username, global_name, avatar): Row =
        sqlx::query_as(
            "SELECT a.discord_id, a.connect_code, a.notify, a.share_analysis,
                    p.username, p.global_name, p.avatar
             FROM accounts a LEFT JOIN discord_profiles p ON p.discord_id = a.discord_id
             WHERE a.id = ?",
        )
        .bind(account_id)
        .fetch_one(&mut *conn)
        .await?;

    let player = account_player(&mut *conn, account_id).await?;
    let mut clients = Vec::new();
    let mut entered = Vec::new();
    if let Some((key, _)) = &player {
        let created_at: i64 = sqlx::query_scalar("SELECT created_at FROM players WHERE key = ?")
            .bind(key)
            .fetch_one(&mut *conn)
            .await?;
        clients.push(client_view(key, None, created_at));
        let aliases: Vec<(String, Option<String>, i64)> = sqlx::query_as(
            "SELECT key, label, linked_at FROM player_keys WHERE player_key = ?
             ORDER BY linked_at, key",
        )
        .bind(key)
        .fetch_all(&mut *conn)
        .await?;
        clients.extend(
            aliases
                .into_iter()
                .map(|(k, label, at)| client_view(&k, label, at)),
        );
        entered = sqlx::query_scalar(
            "SELECT e.id FROM entrants n JOIN events e ON e.id = n.event_id
             WHERE n.player_key = ? AND e.state IN ('signup', 'running') ORDER BY e.id",
        )
        .bind(key)
        .fetch_all(&mut *conn)
        .await?;
    }

    Ok(AccountView {
        display_name: global_name.clone().or_else(|| username.clone()),
        discord_username: username,
        avatar_url: discord_id
            .as_deref()
            .and_then(|id| avatar_url(id, avatar.as_deref())),
        tag: player.map(|(_, tag)| tag),
        connect_code,
        notify: notify != 0,
        share_analysis: share_analysis != 0,
        is_admin,
        clients,
        entered,
        sign_out_url: cfg.sign_out_url.clone(),
    })
}

fn client_view(key: &str, label: Option<String>, linked_at: i64) -> ClientView {
    ClientView {
        key_prefix: key.chars().take(KEY_PREFIX_HEX).collect(),
        label,
        linked_at,
        placeholder: is_placeholder(key),
    }
}

/// The CDN URL of a Discord avatar: the user's own when `hash` looks like one (`a_` and/or
/// 32 hex digits), else Discord's default avatar `(id >> 22) % 6`. `None` for an id that is
/// not a Discord snowflake.
fn avatar_url(discord_id: &str, hash: Option<&str>) -> Option<String> {
    if discord_id.is_empty() || discord_id.len() > 20 {
        return None;
    }
    let id: u64 = discord_id.parse().ok()?;
    let valid_hash = |h: &str| {
        let hex = h.strip_prefix("a_").unwrap_or(h);
        hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit())
    };
    Some(match hash.filter(|h| valid_hash(h)) {
        Some(h) => format!("https://cdn.discordapp.com/avatars/{id}/{h}.png"),
        None => format!(
            "https://cdn.discordapp.com/embed/avatars/{}.png",
            (id >> 22) % 6
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(&dir.path().join("a.sqlite")).await.unwrap();
        (dir, db)
    }

    async fn exec(db: &SqlitePool, sql: &str) {
        sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(db)
            .await
            .unwrap();
    }

    fn real_key(n: u8) -> String {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[n; 32]);
        hex::encode(sk.verifying_key().to_bytes())
    }

    #[test]
    fn placeholder_keys_are_not_device_keys() {
        for _ in 0..50 {
            let key = placeholder_key();
            assert_eq!(key.len(), 64);
            assert!(key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            assert!(is_placeholder(&key));
        }
        assert!(!is_placeholder(&real_key(1)));
        assert_ne!(placeholder_key(), placeholder_key());
    }

    #[tokio::test]
    async fn rekey_player_moves_everything() {
        let (_dir, db) = db().await;
        let (old, alias, new) = (real_key(1), real_key(2), real_key(3));
        for sql in [
            format!("INSERT INTO players (key, tag, created_at) VALUES ('{old}', 'Alice', 1)"),
            format!("INSERT INTO players (key, tag, created_at) VALUES ('{new}x', 'Bob', 1)"),
            "INSERT INTO events (id, name, format, ruleset_json, state, created_at)
             VALUES (1, 'E', 'single_elim', '{}', 'signup', 1)"
                .into(),
            format!(
                "INSERT INTO entrants (event_id, player_key, seed, engine_build)
                 VALUES (1, '{old}', 0, 'b')"
            ),
            format!("INSERT INTO invites (code, used_by) VALUES ('c1', '{old}')"),
            format!(
                "INSERT INTO player_keys (key, player_key, label, linked_at)
                 VALUES ('{alias}', '{old}', 'laptop', 5)"
            ),
        ] {
            exec(&db, &sql).await;
        }
        let mut conn = db.acquire().await.unwrap();
        rekey_player(&mut conn, &old, &new).await.unwrap();
        let player: String = sqlx::query_scalar("SELECT key FROM players WHERE tag = 'Alice'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(player, new);
        let entrant: String = sqlx::query_scalar("SELECT player_key FROM entrants")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(entrant, new);
        let used_by: String = sqlx::query_scalar("SELECT used_by FROM invites")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(used_by, new);
        assert_eq!(
            resolve_key(&mut conn, &alias).await.unwrap(),
            Some((new.clone(), "Alice".into())),
            "the alias follows"
        );
        assert_eq!(resolve_key(&mut conn, &old).await.unwrap(), None);

        // The new key in use, as a player's key or as an alias; an unknown old key.
        let bob = format!("{new}x");
        assert!(matches!(
            rekey_player(&mut conn, &new, &bob).await,
            Err(RekeyError::KeyInUse)
        ));
        assert!(matches!(
            rekey_player(&mut conn, &new, &alias).await,
            Err(RekeyError::KeyInUse)
        ));
        assert!(matches!(
            rekey_player(&mut conn, &old, &real_key(4)).await,
            Err(RekeyError::NoSuchPlayer)
        ));
        let player: String = sqlx::query_scalar("SELECT key FROM players WHERE tag = 'Alice'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(player, new, "a refused rekey changes nothing");
    }

    /// The Discord id of `sub`'s account; `None` without an account.
    async fn bound(conn: &mut SqliteConnection, sub: &str) -> Option<Option<String>> {
        sqlx::query_scalar("SELECT discord_id FROM accounts WHERE sub = ?")
            .bind(sub)
            .fetch_optional(conn)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn bind_discord_never_rebinds() {
        let (_dir, db) = db().await;
        let mut conn = db.acquire().await.unwrap();

        assert_eq!(
            bind_discord(&mut conn, "s1", "100").await.unwrap(),
            BindOutcome::Created
        );
        assert_eq!(
            bind_discord(&mut conn, "s1", "100").await.unwrap(),
            BindOutcome::Unchanged
        );
        assert_eq!(
            bind_discord(&mut conn, "s1", "200").await.unwrap(),
            BindOutcome::Conflict
        );
        assert_eq!(bound(&mut conn, "s1").await, Some(Some("100".into())));
        // The same Discord id under another sub, new or existing.
        assert_eq!(
            bind_discord(&mut conn, "s2", "100").await.unwrap(),
            BindOutcome::Conflict
        );
        assert_eq!(bound(&mut conn, "s2").await, None);
        exec(
            &db,
            "INSERT INTO accounts (sub, created_at, last_seen_at) VALUES ('s3', 1, 1)",
        )
        .await;
        assert_eq!(
            bind_discord(&mut conn, "s3", "100").await.unwrap(),
            BindOutcome::Conflict
        );
        assert_eq!(bound(&mut conn, "s3").await, Some(None));
        // An account without a Discord id gets one, once.
        assert_eq!(
            bind_discord(&mut conn, "s3", "300").await.unwrap(),
            BindOutcome::Bound
        );
        assert_eq!(bound(&mut conn, "s3").await, Some(Some("300".into())));
        // A removed sub gets no row.
        sqlx::query("INSERT INTO account_tombstones (sub_sha256, removed_at) VALUES (?, 1)")
            .bind(sha256_hex("s9"))
            .execute(&mut *conn)
            .await
            .unwrap();
        assert_eq!(
            bind_discord(&mut conn, "s9", "900").await.unwrap(),
            BindOutcome::Conflict
        );
        assert_eq!(bound(&mut conn, "s9").await, None);
    }

    #[tokio::test]
    async fn create_player_and_view() {
        let (_dir, db) = db().await;
        let cfg: IdentityConfig = toml::from_str(
            r#"
            public_origin = "https://bracket.test"
            issuer = "https://auth.test"
            client_id = "c"
            project_id = "p"
            org_id = "o"
            discord_idp_id = "i"
            proxy_secret = "test-proxy-secret-test-proxy-secret-00"
            sign_out_url = "/oauth2/sign_out"
            mfa_setup_url = "https://auth.test/mfa"
            [hook_keys]
            idp_intent = "a"
            token = "b"
            user_events = "c"
            "#,
        )
        .unwrap();
        let mut conn = db.acquire().await.unwrap();
        assert_eq!(
            bind_discord(&mut conn, "s1", "80351110224678912")
                .await
                .unwrap(),
            BindOutcome::Created
        );
        exec(
            &db,
            "INSERT INTO accounts (sub, created_at, last_seen_at) VALUES ('s2', 1, 1)",
        )
        .await;
        let view0 = view(&mut conn, 1, &cfg, false).await.unwrap();
        assert_eq!(view0.tag, None);
        assert!(view0.clients.is_empty());
        assert_eq!(view0.display_name, None);
        assert_eq!(
            view0.avatar_url.as_deref(),
            Some("https://cdn.discordapp.com/embed/avatars/5.png"),
            "(80351110224678912 >> 22) % 6"
        );
        assert_eq!(view0.sign_out_url, "/oauth2/sign_out");

        assert!(matches!(
            create_player(&mut conn, 1, "x").await,
            Err(CreatePlayerError::BadTag)
        ));
        let key = create_player(&mut conn, 1, "Mango").await.unwrap();
        assert!(is_placeholder(&key));
        assert!(matches!(
            create_player(&mut conn, 1, "Other").await,
            Err(CreatePlayerError::HasPlayer)
        ));
        assert!(matches!(
            create_player(&mut conn, 2, "mango").await,
            Err(CreatePlayerError::TagTaken)
        ));
        assert_eq!(
            account_player(&mut conn, 1).await.unwrap(),
            Some((key.clone(), "Mango".into()))
        );

        let alias = real_key(7);
        sqlx::query(
            "INSERT INTO player_keys (key, player_key, label, linked_at) VALUES (?, ?, 'pc', 9)",
        )
        .bind(&alias)
        .bind(&key)
        .execute(&mut *conn)
        .await
        .unwrap();
        exec(
            &db,
            "INSERT INTO discord_profiles (discord_id, username, global_name, avatar, updated_at)
             VALUES ('80351110224678912', 'mango', 'Mang0', 'a_0123456789abcdef0123456789abcdef', 1)",
        )
        .await;
        for (id, state) in [(1, "signup"), (2, "running"), (3, "complete")] {
            exec(
                &db,
                &format!(
                    "INSERT INTO events (id, name, format, ruleset_json, state, created_at)
                     VALUES ({id}, 'E', 'single_elim', '{{}}', '{state}', 1)"
                ),
            )
            .await;
            exec(
                &db,
                &format!(
                    "INSERT INTO entrants (event_id, player_key, seed, engine_build)
                     VALUES ({id}, '{key}', 0, 'b')"
                ),
            )
            .await;
        }
        let v = view(&mut conn, 1, &cfg, true).await.unwrap();
        assert_eq!(v.display_name.as_deref(), Some("Mang0"));
        assert_eq!(v.discord_username.as_deref(), Some("mango"));
        assert_eq!(
            v.avatar_url.as_deref(),
            Some("https://cdn.discordapp.com/avatars/80351110224678912/a_0123456789abcdef0123456789abcdef.png")
        );
        assert_eq!(v.tag.as_deref(), Some("Mango"));
        assert!(v.is_admin);
        assert_eq!(v.entered, vec![1, 2]);
        assert_eq!(v.clients.len(), 2);
        assert_eq!(v.clients[0].key_prefix, key[..16]);
        assert!(v.clients[0].placeholder);
        assert_eq!(
            v.clients[1],
            ClientView {
                key_prefix: alias[..16].to_owned(),
                label: Some("pc".into()),
                linked_at: 9,
                placeholder: false,
            }
        );
    }

    #[test]
    fn avatar_hash_is_checked() {
        assert_eq!(
            avatar_url("6", Some("../../evil")).as_deref(),
            Some("https://cdn.discordapp.com/embed/avatars/0.png")
        );
        assert_eq!(avatar_url("not-a-number", None), None);
        assert_eq!(avatar_url("", None), None);
    }
}

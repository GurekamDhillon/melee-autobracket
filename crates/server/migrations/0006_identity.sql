-- Phase 2b: web accounts (Zitadel users), linked client keys, the organiser audit log, ready
-- check-in and server-held stage selection. All timestamps are unix seconds.

-- One row per Zitadel user, created on first sight (the web extractor or the token hook).
-- `sub` is the Zitadel user id; after `user.removed` it becomes "removed:<id>" and the personal
-- fields are cleared. `discord_id` is set once (only by `accounts::bind_discord`) and never
-- re-bound. `disabled_at` is set while the user is deactivated or locked in Zitadel.
CREATE TABLE accounts (
    id             INTEGER PRIMARY KEY,
    sub            TEXT NOT NULL UNIQUE,
    discord_id     TEXT NULL UNIQUE,
    connect_code   TEXT NULL,
    notify         INTEGER NOT NULL DEFAULT 0,
    share_analysis INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    last_seen_at   INTEGER NOT NULL,
    disabled_at    INTEGER NULL
);

-- Removed Zitadel users, by SHA-256 (hex) of their `sub`: refused forever, never re-created.
CREATE TABLE account_tombstones (
    sub_sha256 TEXT PRIMARY KEY NOT NULL,
    removed_at INTEGER NOT NULL
);

-- The Discord profile fields of a user, refreshed on every login by the idp-intent hook.
CREATE TABLE discord_profiles (
    discord_id  TEXT PRIMARY KEY NOT NULL,
    username    TEXT NOT NULL,
    global_name TEXT NULL,
    avatar      TEXT NULL,
    updated_at  INTEGER NOT NULL
);

-- The player (tag and results) an account owns: at most one each way.
ALTER TABLE players ADD COLUMN account_id INTEGER NULL;
CREATE UNIQUE INDEX players_account ON players (account_id) WHERE account_id IS NOT NULL;

-- Additional client keys that sign as a player (`player_key` is that player's `players.key`).
-- A key is in at most one of `players.key` and `player_keys.key` (checked by the server).
CREATE TABLE player_keys (
    key        TEXT PRIMARY KEY NOT NULL,
    player_key TEXT NOT NULL,
    label      TEXT NULL,
    linked_at  INTEGER NOT NULL
);
CREATE INDEX player_keys_player ON player_keys (player_key);

-- One-time codes that link a game client to an account; at most one live code per account.
CREATE TABLE link_codes (
    code_sha256 TEXT PRIMARY KEY NOT NULL,
    account_id  INTEGER NOT NULL UNIQUE,
    expires_at  INTEGER NOT NULL
);

-- Every organiser action and account change, with its actor. Append-only.
CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY,
    at          INTEGER NOT NULL,
    actor_kind  TEXT NOT NULL,
    actor_id    TEXT NOT NULL,
    actor_name  TEXT NULL,
    action      TEXT NOT NULL,
    target      TEXT NULL,
    detail_json TEXT NULL
);

-- Ready check-in per bracket slot; NULL when not ready.
ALTER TABLE sets ADD COLUMN ready0_at INTEGER NULL;
ALTER TABLE sets ADD COLUMN ready1_at INTEGER NULL;

-- Server-held stage selection: the moves of each game, in order.
CREATE TABLE stage_moves (
    event_id INTEGER NOT NULL,
    set_id   INTEGER NOT NULL,
    game_no  INTEGER NOT NULL,
    seq      INTEGER NOT NULL,
    slot     INTEGER NOT NULL,
    kind     TEXT NOT NULL CHECK (kind IN ('strike', 'ban', 'pick')),
    stage    INTEGER NOT NULL,
    at       INTEGER NOT NULL,
    PRIMARY KEY (event_id, set_id, game_no, seq)
);

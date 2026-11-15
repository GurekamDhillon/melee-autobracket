-- All timestamps are unix seconds.

CREATE TABLE players (
    key        TEXT PRIMARY KEY NOT NULL,
    tag        TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE invites (
    code    TEXT PRIMARY KEY NOT NULL,
    used_by TEXT NULL
);

CREATE TABLE nonces (
    key     TEXT NOT NULL,
    nonce   TEXT NOT NULL,
    seen_at INTEGER NOT NULL,
    PRIMARY KEY (key, nonce)
);
CREATE INDEX nonces_seen_at ON nonces (seen_at);

CREATE TABLE events (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    format       TEXT NOT NULL,
    ruleset_json TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('signup', 'running', 'complete')),
    bracket_json TEXT NULL,
    created_at   INTEGER NOT NULL
);

CREATE TABLE entrants (
    event_id     INTEGER NOT NULL,
    player_key   TEXT NOT NULL,
    seed         INTEGER NOT NULL,
    engine_build TEXT NOT NULL,
    dq           INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (event_id, player_key)
);

CREATE TABLE sets (
    event_id  INTEGER NOT NULL,
    set_id    INTEGER NOT NULL,
    state     TEXT NOT NULL CHECK (state IN ('ready', 'review', 'done')),
    room_code TEXT NULL,
    wins0     INTEGER NOT NULL,
    wins1     INTEGER NOT NULL,
    PRIMARY KEY (event_id, set_id)
);

CREATE TABLE games (
    id              INTEGER PRIMARY KEY,
    event_id        INTEGER NOT NULL,
    set_id          INTEGER NOT NULL,
    game_no         INTEGER NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('uploading', 'judging', 'counted', 'void', 'review')),
    verdict_json    TEXT NULL,
    winner_slot     INTEGER NULL,
    first_upload_at INTEGER NULL,
    resim_json      TEXT NULL,
    verifier_build  TEXT NULL
);

CREATE TABLE uploads (
    game_id      INTEGER NOT NULL,
    slot         INTEGER NOT NULL,
    path         TEXT NOT NULL,
    sha256       TEXT NOT NULL,
    client_build TEXT NOT NULL,
    uploaded_at  INTEGER NOT NULL,
    PRIMARY KEY (game_id, slot)
);

CREATE TABLE queue (
    id              INTEGER PRIMARY KEY,
    event_id        INTEGER NOT NULL,
    set_id          INTEGER NOT NULL,
    game_id         INTEGER NULL,
    reason          TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('open', 'resolved')),
    resolution_json TEXT NULL,
    created_at      INTEGER NOT NULL
);

CREATE TABLE quarantine (
    id         INTEGER PRIMARY KEY,
    game_id    INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE resim_jobs (
    id                INTEGER PRIMARY KEY,
    game_id           INTEGER NOT NULL,
    path              TEXT NOT NULL,
    replay_last_frame INTEGER NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('queued', 'claimed', 'done')),
    result_json       TEXT NULL,
    claimed_at        INTEGER NULL
);

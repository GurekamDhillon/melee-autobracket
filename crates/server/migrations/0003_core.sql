-- Server core: bounded judging retries, per-game evidence for the organiser, and the reporter
-- of a no-show. Columns and indexes only: SQLite cannot change the CHECK constraints of 0001,
-- so no state gains a new value (a failed game goes to `review` with a queue item instead).

-- Failed attempts to judge the game (an internal error, or a panic). After 5 the game goes to
-- the organiser queue as `judging-failed`.
ALTER TABLE games ADD COLUMN judge_attempts INTEGER NOT NULL DEFAULT 0;

-- What the judging worker found in the uploads of a judged game (JSON): per slot the player,
-- the file's hash, whether it parses and the facts it records, and, for a lone upload, the
-- evidence-only re-simulation. Shown to the organiser with the queue item. NULL for games
-- judged before this migration.
ALTER TABLE games ADD COLUMN evidence_json TEXT NULL;

-- The bracket slot (0 is the host) of the player who reported a no-show; NULL otherwise.
ALTER TABLE queue ADD COLUMN reporter_slot INTEGER NULL;

CREATE INDEX core_games_state ON games (state);
CREATE INDEX core_games_set ON games (event_id, set_id, game_no);
CREATE INDEX core_queue_state ON queue (state);
CREATE INDEX core_resim_jobs_state ON resim_jobs (state);

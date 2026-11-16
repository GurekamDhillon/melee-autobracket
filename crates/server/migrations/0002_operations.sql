-- Operational records found missing in the first real tournament run.
-- Columns only: SQLite cannot change the CHECK constraints of 0001.

-- When a set last changed (opened, room code posted, upload, game counted, state change),
-- unix seconds. The judging worker flags a `ready` set idle for `set_stall_secs`. Rows that
-- exist before this migration count from the moment it runs.
ALTER TABLE sets ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
UPDATE sets SET updated_at = CAST(strftime('%s', 'now') AS INTEGER);

-- What opened a queue item, beyond its reason (e.g. who reported a no-show, and their note).
ALTER TABLE queue ADD COLUMN detail TEXT NULL;

-- When the verdict of a game was applied (late, voided verdicts included), unix seconds.
ALTER TABLE games ADD COLUMN judged_at INTEGER NULL;

-- How a set was decided when not by played games: 'award', 'dq' or 'double-dq'. NULL for a
-- set won on the board or not yet decided.
ALTER TABLE sets ADD COLUMN decided_by TEXT NULL;

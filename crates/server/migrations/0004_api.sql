-- API surface (final fix wave). Columns and indexes only.

-- When the current room code of a set was posted, unix seconds; NULL before any. A `no-show`
-- report is refused for `no_show_grace_secs` after it (the guest may still be joining).
ALTER TABLE sets ADD COLUMN room_posted_at INTEGER NULL;

-- Tags are unique regardless of case ("Mango" and "MANGO" are one tag). A database made by an
-- earlier version may already hold tags that differ only in case, and a plain unique index
-- would then fail to build and stop the server from starting. So in each such group every
-- player but the earliest registered is marked `tag_case_duplicate = 1` and left as is (an
-- organiser can see them with `SELECT tag FROM players WHERE tag_case_duplicate = 1`), and the
-- index covers the unmarked rows: exactly one per tag, ignoring case. A new player is never
-- marked, so a new registration collides with the group's unmarked row.
ALTER TABLE players ADD COLUMN tag_case_duplicate INTEGER NOT NULL DEFAULT 0;
UPDATE players SET tag_case_duplicate = 1
WHERE EXISTS (
    SELECT 1 FROM players earlier
    WHERE earlier.tag = players.tag COLLATE NOCASE
      AND (earlier.created_at < players.created_at
           OR (earlier.created_at = players.created_at AND earlier.rowid < players.rowid))
);
CREATE UNIQUE INDEX players_tag_nocase ON players (tag COLLATE NOCASE)
WHERE tag_case_duplicate = 0;

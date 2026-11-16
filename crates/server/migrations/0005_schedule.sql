-- Public pages (phase 2a). Columns only.

-- When the event starts, unix seconds, 0 to 4102444800 (the year 2100); NULL when the organiser
-- has not said. Shown on the schedule; nothing in the server acts on it.
ALTER TABLE events ADD COLUMN starts_at INTEGER NULL;

-- What the event is about, at most 280 printable characters; NULL when there is none (an empty
-- or all-whitespace description is stored as NULL).
ALTER TABLE events ADD COLUMN description TEXT NULL;

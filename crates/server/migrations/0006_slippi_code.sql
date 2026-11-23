-- A player's Slippi connect code, normalised (upper case, ASCII '#'): `ABCD#123`. NULL until the
-- player registers one. Used by any-client events to tell which port of a replay is which
-- bracket player. Unique, so unique within every event too.
ALTER TABLE players ADD COLUMN slippi_code TEXT NULL;
CREATE UNIQUE INDEX players_slippi_code ON players (slippi_code) WHERE slippi_code IS NOT NULL;

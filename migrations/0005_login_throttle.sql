-- Persistent PIN login throttling.
--
-- Failed logins are counted per normalized user name in a fixed window. When
-- the threshold is reached the user is locked for a short period. The counter
-- is shared by web, API, and snapshot-linking authentication so a PIN cannot
-- be guessed by switching channels.

CREATE TABLE login_throttle (
    key_hash TEXT PRIMARY KEY NOT NULL,
    failure_count INTEGER NOT NULL CHECK (failure_count > 0),
    window_started_at_ms INTEGER NOT NULL,
    locked_until_ms INTEGER,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX login_throttle_updated_idx
    ON login_throttle(updated_at_ms);

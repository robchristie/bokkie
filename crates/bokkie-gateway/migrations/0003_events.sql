-- The per-user client event log. Sequence numbers start at 1 for each user
-- and increase by one with no gaps; the gateway keeps a bounded tail.

CREATE TABLE events (
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    at INTEGER NOT NULL,
    -- The event as JSON, in the wire format of bokkie-protocol.
    body TEXT NOT NULL,
    PRIMARY KEY (user_id, seq)
) WITHOUT ROWID;

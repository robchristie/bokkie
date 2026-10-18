-- Failed and in-flight sign-in attempts, so that a restart does not reset
-- the sign-in limits. The gateway keeps the same counts in memory and
-- writes each change here; rows older than the limiter's window are
-- deleted.

CREATE TABLE login_failures (
    id INTEGER PRIMARY KEY,
    -- The limited thing: `u:<username>`, `a:<address>` or `p:<user id>`.
    key TEXT NOT NULL,
    at INTEGER NOT NULL
);

CREATE INDEX login_failures_by_key ON login_failures (key, at);
CREATE INDEX login_failures_by_time ON login_failures (at);

-- Per-user usage, for the allowances an admin sets. The subscription's own
-- rate limits are shared by every user and come from Codex.

-- One row per turn a user starts. Steering a running turn is not a turn.
CREATE TABLE turns (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    at INTEGER NOT NULL
);

CREATE INDEX turns_by_user ON turns (user_id, at);

-- Tokens used, from thread/tokenUsage/updated: each row is the growth of a
-- thread's total since the previous report.
CREATE TABLE token_usage (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    at INTEGER NOT NULL,
    tokens INTEGER NOT NULL CHECK (tokens >= 0)
);

CREATE INDEX token_usage_by_user ON token_usage (user_id, at);

-- The thread's token total at its last report, to measure growth.
ALTER TABLE bots ADD COLUMN tokens_reported INTEGER NOT NULL DEFAULT 0;

-- Accounts, sessions and invitations. Times are milliseconds since the Unix
-- epoch. Secrets (passwords, session tokens, invitation codes) are stored
-- only as hashes.

CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'member')),
    -- An Argon2id PHC string.
    password_hash TEXT NOT NULL,
    disabled INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
    -- Per-user limits; NULL means unlimited.
    daily_turns INTEGER CHECK (daily_turns >= 0),
    weekly_tokens INTEGER CHECK (weekly_tokens >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- SHA-256 of the cookie's token.
    token_hash BLOB NOT NULL UNIQUE,
    user_agent TEXT,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE INDEX sessions_by_user ON sessions (user_id);

CREATE TABLE invitations (
    id INTEGER PRIMARY KEY,
    -- SHA-256 of the invitation code.
    code_hash BLOB NOT NULL UNIQUE,
    role TEXT NOT NULL CHECK (role IN ('admin', 'member')),
    display_name TEXT,
    created_by INTEGER NOT NULL REFERENCES users (id),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER,
    used_by INTEGER REFERENCES users (id),
    withdrawn_at INTEGER
);

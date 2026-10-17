-- Bots and their conversations. Each Bot belongs to one user and has one
-- conversation, carried by one long-lived Codex thread.

CREATE TABLE bots (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT NOT NULL,
    avatar TEXT,
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    hidden INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1)),
    -- The Codex thread, once the first message starts it.
    thread_id TEXT UNIQUE,
    -- Increases whenever the name, label or description changes.
    profile_revision INTEGER NOT NULL DEFAULT 1,
    -- The revision the model has seen: in the thread's developer
    -- instructions, or in a later turn's context.
    profile_sent_revision INTEGER NOT NULL DEFAULT 0,
    -- Set when the gateway refused the Bot's thread for a safety reason.
    refused_reason TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX bots_name_per_user ON bots (user_id, name COLLATE NOCASE);

CREATE TABLE messages (
    id INTEGER PRIMARY KEY,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    author TEXT NOT NULL CHECK (author IN ('user', 'bot')),
    text TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('streaming', 'complete', 'interrupted', 'failed')),
    -- For Bot replies: the Codex turn and item that produced them.
    turn_id TEXT,
    item_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX messages_by_bot ON messages (bot_id, id);
CREATE INDEX messages_streaming ON messages (user_id, state) WHERE state = 'streaming';

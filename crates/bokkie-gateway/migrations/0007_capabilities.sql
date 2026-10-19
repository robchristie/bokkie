-- Per-Bot capability grants, the grants each Bot's thread started with,
-- conversation markers, Bots' notes and the tool-call log. Existing Bots
-- keep their behaviour: no grants, and their threads started with none,
-- so no thread is replaced.

-- The Bot's grants, as catalogue names joined by commas, such as
-- "notes,web.search". Empty: none.
ALTER TABLE bots ADD COLUMN grants TEXT NOT NULL DEFAULT '';
-- The grants the Bot's current thread started with. A turn runs on the
-- thread only while they equal `grants`; otherwise a fresh thread starts.
ALTER TABLE bots ADD COLUMN thread_grants TEXT NOT NULL DEFAULT '';
-- Set when a fresh thread replaced the Bot's thread and has not yet had a
-- turn: the marker message that begins it. The first turn carries the
-- conversation before it as context.
ALTER TABLE bots ADD COLUMN carryover_marker INTEGER;

-- What a message is: text someone said, or a marker from the gateway.
ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'text'
    CHECK (kind IN ('text', 'marker'));

-- One notes document per Bot, written by the Bot through its tools.
CREATE TABLE bot_notes (
    bot_id INTEGER PRIMARY KEY REFERENCES bots (id) ON DELETE CASCADE,
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Every brokered tool call, kept 90 days.
CREATE TABLE tool_calls (
    id INTEGER PRIMARY KEY,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    -- The routine run that made the call, once routines exist.
    routine_run_id INTEGER,
    tool TEXT NOT NULL,
    -- A bounded summary of the arguments, such as a size or a URL.
    summary TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('ok', 'failed', 'refused', 'timed_out')),
    duration_ms INTEGER NOT NULL,
    at INTEGER NOT NULL
);

CREATE INDEX tool_calls_by_bot ON tool_calls (bot_id, id);
CREATE INDEX tool_calls_by_time ON tool_calls (at);

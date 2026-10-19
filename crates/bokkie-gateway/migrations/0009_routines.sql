-- Routines: prompts a Bot runs on a schedule in its owner's time zone, and
-- their run history. Existing accounts get the default time zone.

-- The IANA time zone the user's routines are scheduled in.
ALTER TABLE users ADD COLUMN time_zone TEXT NOT NULL DEFAULT 'Australia/Adelaide';

CREATE TABLE routines (
    id INTEGER PRIMARY KEY,
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    prompt TEXT NOT NULL,
    -- The schedule, in its wire JSON, such as
    -- {"repeat":"daily","at":"07:00"}.
    schedule TEXT NOT NULL,
    push TEXT NOT NULL CHECK (push IN ('every_run', 'notify_only')),
    paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    -- Failed runs in a row; three pause the routine.
    failures INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    -- The next time the schedule names; NULL while paused.
    next_run_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX routines_name_per_bot ON routines (bot_id, name COLLATE NOCASE);
CREATE INDEX routines_by_user ON routines (user_id, id);
CREATE INDEX routines_due ON routines (next_run_at) WHERE paused = 0;

-- Every run of a routine, kept 90 days.
CREATE TABLE routine_runs (
    id INTEGER PRIMARY KEY,
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    routine_id INTEGER NOT NULL REFERENCES routines (id) ON DELETE CASCADE,
    trigger TEXT NOT NULL CHECK (trigger IN ('schedule', 'manual')),
    -- When the schedule said to run, or when "run now" was asked for.
    scheduled_for INTEGER NOT NULL,
    -- When the run was queued: a waiting run gives up on a busy Bot 30
    -- minutes later.
    queued_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    state TEXT NOT NULL CHECK (state IN ('waiting', 'running', 'succeeded', 'failed',
                                         'missed', 'skipped', 'interrupted')),
    -- Why it ended as it did, or what a waiting run waits for.
    reason TEXT,
    -- What a waiting run last waited for: 'busy' (the Bot's turn) or
    -- 'signed_out' (the household's ChatGPT sign-in).
    waiting_for TEXT CHECK (waiting_for IN ('busy', 'signed_out')),
    -- The Codex thread and turn it ran in.
    thread_id TEXT,
    turn_id TEXT,
    -- When the gateway interrupted the turn for running too long.
    deadline_passed_at INTEGER,
    -- The tokens its turn used, as Codex reported them, totalled when it
    -- ended.
    tokens INTEGER CHECK (tokens >= 0)
);

CREATE INDEX routine_runs_by_routine ON routine_runs (routine_id, id);
CREATE INDEX routine_runs_active ON routine_runs (state) WHERE state IN ('waiting', 'running');
CREATE INDEX routine_runs_by_turn ON routine_runs (user_id, turn_id) WHERE turn_id IS NOT NULL;
CREATE INDEX routine_runs_by_time ON routine_runs (queued_at);

-- The marker that begins a routine's run in the conversation names the
-- routine and the run, and holds the prompt the Bot was given. No foreign
-- keys: the conversation keeps the marker when the routine goes.
ALTER TABLE messages ADD COLUMN routine_id INTEGER;
ALTER TABLE messages ADD COLUMN routine_run_id INTEGER;
ALTER TABLE messages ADD COLUMN detail TEXT;

CREATE INDEX messages_by_run ON messages (routine_run_id) WHERE routine_run_id IS NOT NULL;
CREATE INDEX messages_by_turn ON messages (bot_id, turn_id) WHERE turn_id IS NOT NULL;

-- The Codex turn whose report a token count came from, so that a routine
-- run's tokens can be totalled when it ends. NULL for counts from before.
ALTER TABLE token_usage ADD COLUMN turn_id TEXT;

CREATE INDEX token_usage_by_turn ON token_usage (user_id, turn_id) WHERE turn_id IS NOT NULL;

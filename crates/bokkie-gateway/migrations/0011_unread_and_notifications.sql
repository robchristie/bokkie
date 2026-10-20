-- Unread state and the notify tool.

-- Each user's read marker on each of their Bots: the newest message of
-- the Bot's they have seen, on any device. It only moves forward.
CREATE TABLE read_markers (
    bot_id INTEGER PRIMARY KEY REFERENCES bots (id) ON DELETE CASCADE,
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    updated_at INTEGER NOT NULL
);

-- Nothing is unread on upgrade: every conversation counts as seen up to
-- its newest message.
INSERT INTO read_markers (bot_id, user_id, message_id, updated_at)
SELECT bot_id, user_id, max(id), max(created_at)
FROM messages
GROUP BY bot_id, user_id;

-- Finding a Bot's newest reply or marker quickly.
CREATE INDEX messages_by_author ON messages (bot_id, author, id);

-- A notification's marker in the conversation: its kind, and its body in
-- `detail`. The marker's text is the title.
ALTER TABLE messages ADD COLUMN notification_kind TEXT
    CHECK (notification_kind IN ('result', 'question'));

-- Every notification a Bot sent with its notify tool, kept 90 days. Its
-- marker in the conversation stays.
CREATE TABLE notifications (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    -- Its marker in the conversation.
    message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    -- The tool call that sent it.
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    -- The routine run whose turn sent it, if a routine's. No foreign key:
    -- runs are pruned on their own schedule.
    routine_run_id INTEGER,
    kind TEXT NOT NULL CHECK (kind IN ('result', 'question')),
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX notifications_by_user ON notifications (user_id, id);
CREATE INDEX notifications_by_turn ON notifications (user_id, turn_id);
CREATE INDEX notifications_by_time ON notifications (created_at);

-- Every Codex thread each Bot has had, and the items each Bot has shown its
-- owner.

-- A Bot's threads: its current one and those a fresh thread replaced, so
-- that a replaced thread's late events, such as the end of its last turn,
-- still reach its Bot. Each thread's token total is measured from its own
-- last report, so a fresh thread's tokens count from zero.
CREATE TABLE bot_threads (
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    thread_id TEXT NOT NULL,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    -- The thread's token total at its last report, to measure growth.
    tokens_reported INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, thread_id)
);

CREATE INDEX bot_threads_by_bot ON bot_threads (bot_id);

-- Existing Bots keep their threads, with the totals already reported.
INSERT INTO bot_threads (user_id, thread_id, bot_id, tokens_reported)
    SELECT user_id, thread_id, id, tokens_reported FROM bots WHERE thread_id IS NOT NULL;

ALTER TABLE bots DROP COLUMN tokens_reported;

-- Whenever a Bot's thread is set or replaced, the new thread joins its
-- threads, in the same statement.
CREATE TRIGGER bots_record_thread AFTER UPDATE OF thread_id ON bots
WHEN NEW.thread_id IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO bot_threads (user_id, thread_id, bot_id)
        VALUES (NEW.user_id, NEW.thread_id, NEW.id);
END;

-- The items a Bot has shown its owner, by canonical id: an arXiv id, a DOI
-- or a normalised link. Kept 365 days from when the item was last shown.
CREATE TABLE seen_items (
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The canonical id, such as "arxiv:2401.12345" or "doi:10.1000/xyz".
    item TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT,
    shown_at INTEGER NOT NULL,
    PRIMARY KEY (bot_id, item)
);

CREATE INDEX seen_items_by_bot_and_time ON seen_items (bot_id, shown_at);
CREATE INDEX seen_items_by_time ON seen_items (shown_at);

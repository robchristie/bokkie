-- Web Push: each session's subscription, and the pushes queued for
-- delivery.

-- A device's Web Push subscription. It belongs to the session that made
-- it: one per session, and an endpoint belongs to one session at a time.
CREATE TABLE push_subscriptions (
    id INTEGER PRIMARY KEY,
    -- The owning user, repeated so that every query can be scoped by it.
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    session_id INTEGER NOT NULL UNIQUE REFERENCES sessions (id) ON DELETE CASCADE,
    -- The push service's https URL for this device.
    endpoint TEXT NOT NULL UNIQUE,
    -- The device's P-256 public key, uncompressed (65 bytes), and its
    -- authentication secret (16 bytes).
    p256dh BLOB NOT NULL CHECK (length(p256dh) = 65),
    auth BLOB NOT NULL CHECK (length(auth) = 16),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX push_subscriptions_by_user ON push_subscriptions (user_id);

-- Signing out and every revocation (another session, a password change or
-- reset, disabling the account) set `revoked_at`; the subscription goes in
-- the same statement, so in the same transaction.
CREATE TRIGGER push_subscriptions_end_with_session
AFTER UPDATE OF revoked_at ON sessions
WHEN NEW.revoked_at IS NOT NULL
BEGIN
    DELETE FROM push_subscriptions WHERE session_id = NEW.id;
END;

-- Every push the gateway decided to send, queued in the transaction that
-- caused it and settled by the sender. Kept a day.
CREATE TABLE pushes (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    bot_id INTEGER NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    -- What caused it.
    cause TEXT NOT NULL CHECK (cause IN ('notify', 'run', 'failure', 'miss', 'pause')),
    -- The notification or routine run it is about, if any. No foreign
    -- keys: both are pruned on their own schedules.
    notification_id INTEGER,
    routine_run_id INTEGER,
    urgency TEXT NOT NULL CHECK (urgency IN ('normal', 'high')),
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued' CHECK (state IN (
        -- Waiting for the sender.
        'queued',
        -- Being delivered, then delivered to at least one device or none.
        'sending', 'sent', 'failed',
        -- Not sent: the Bot was on screen, the hourly limit was reached,
        -- the user has no subscription, sending is not configured, the
        -- account is disabled, the push waited too long, or a routine
        -- run's own push carries it.
        'shown', 'limited', 'unsubscribed', 'unconfigured', 'disabled', 'expired', 'merged'
    )),
    created_at INTEGER NOT NULL,
    -- When the sender began delivering it.
    sent_at INTEGER,
    settled_at INTEGER
);

CREATE INDEX pushes_queued ON pushes (id) WHERE state = 'queued';
CREATE INDEX pushes_by_user ON pushes (user_id, sent_at);
CREATE INDEX pushes_by_time ON pushes (created_at);

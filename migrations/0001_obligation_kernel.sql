CREATE TABLE obligations (
    id                  TEXT PRIMARY KEY,
    description         TEXT NOT NULL,
    state               TEXT NOT NULL CHECK (state IN (
                            'pending', 'awaiting_approval', 'running',
                            'retry_scheduled', 'attention', 'completed', 'cancelled'
                        )),
    occurrence          INTEGER NOT NULL DEFAULT 1 CHECK (occurrence > 0),
    scheduled_at        INTEGER NOT NULL,
    next_wake_at        INTEGER,
    recurrence_cron     TEXT,
    recurrence_timezone TEXT,
    approval_required   INTEGER NOT NULL DEFAULT 0 CHECK (approval_required IN (0, 1)),
    attempts_made       INTEGER NOT NULL DEFAULT 0 CHECK (attempts_made >= 0),
    max_attempts        INTEGER NOT NULL CHECK (max_attempts > 0),
    retry_base_seconds  INTEGER NOT NULL CHECK (retry_base_seconds > 0),
    retry_max_seconds   INTEGER NOT NULL CHECK (retry_max_seconds >= retry_base_seconds),
    lease_token         TEXT,
    lease_generation    INTEGER NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
    lease_expires_at    INTEGER,
    last_error          TEXT,
    last_evidence       TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    CHECK ((recurrence_cron IS NULL) = (recurrence_timezone IS NULL)),
    CHECK (
        (state = 'running' AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL
            AND next_wake_at IS NULL)
        OR
        (state <> 'running' AND lease_token IS NULL AND lease_expires_at IS NULL)
    ),
    CHECK (
        state IN ('awaiting_approval', 'attention', 'completed', 'cancelled', 'running')
        OR next_wake_at IS NOT NULL
    )
);

CREATE INDEX obligations_due_idx ON obligations(next_wake_at, id)
    WHERE state IN ('pending', 'retry_scheduled');
CREATE INDEX obligations_expired_lease_idx ON obligations(lease_expires_at, id)
    WHERE state = 'running';

CREATE TABLE approvals (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    obligation_id TEXT NOT NULL REFERENCES obligations(id),
    occurrence    INTEGER NOT NULL CHECK (occurrence > 0),
    decision      TEXT NOT NULL CHECK (decision IN ('approved', 'rejected')),
    actor         TEXT NOT NULL,
    note          TEXT,
    decided_at    INTEGER NOT NULL
);

CREATE INDEX approvals_occurrence_idx
    ON approvals(obligation_id, occurrence, id);

CREATE TABLE attempts (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    obligation_id    TEXT NOT NULL REFERENCES obligations(id),
    occurrence       INTEGER NOT NULL CHECK (occurrence > 0),
    attempt_number   INTEGER NOT NULL CHECK (attempt_number > 0),
    lease_generation INTEGER NOT NULL CHECK (lease_generation > 0),
    lease_token      TEXT NOT NULL,
    claimed_at       INTEGER NOT NULL,
    completed_at     INTEGER,
    outcome          TEXT NOT NULL CHECK (outcome IN ('running', 'succeeded', 'failed', 'lease_expired')),
    retryable        INTEGER CHECK (retryable IN (0, 1)),
    error            TEXT,
    evidence         TEXT,
    UNIQUE (obligation_id, occurrence, attempt_number),
    UNIQUE (obligation_id, lease_generation)
);

CREATE TABLE audit_events (
    sequence      INTEGER PRIMARY KEY AUTOINCREMENT,
    obligation_id TEXT NOT NULL REFERENCES obligations(id),
    occurrence    INTEGER NOT NULL CHECK (occurrence > 0),
    event_type    TEXT NOT NULL,
    occurred_at   INTEGER NOT NULL,
    from_state    TEXT,
    to_state      TEXT NOT NULL,
    details_json  TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX audit_events_obligation_idx
    ON audit_events(obligation_id, sequence);

ALTER TABLE attempts ADD COLUMN failure_disposition TEXT
    CHECK (failure_disposition IN (
        'retry_safe', 'needs_reconciliation', 'human_decision', 'terminal', 'cancelled'
    ));

ALTER TABLE obligations ADD COLUMN failure_disposition TEXT
    CHECK (failure_disposition IN (
        'retry_safe', 'needs_reconciliation', 'human_decision', 'terminal', 'cancelled'
    ));

-- Migration-owned backfill is the sole exception to completed-attempt
-- immutability. Recreate the same guard before this transaction can commit.
DROP TRIGGER completed_attempts_no_update;

UPDATE attempts
SET failure_disposition = CASE
    WHEN outcome = 'lease_expired' THEN 'retry_safe'
    WHEN outcome = 'failed' AND retryable = 1 THEN 'retry_safe'
    WHEN outcome = 'failed' AND retryable = 0 THEN 'terminal'
    ELSE NULL
END;

CREATE TRIGGER completed_attempts_no_update
BEFORE UPDATE ON attempts
WHEN OLD.completed_at IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'completed attempts are immutable');
END;

UPDATE obligations
SET failure_disposition = CASE
    WHEN state = 'cancelled' THEN 'cancelled'
    ELSE (
        SELECT a.failure_disposition
        FROM attempts a
        WHERE a.obligation_id = obligations.id
          AND a.occurrence = obligations.occurrence
          AND a.completed_at IS NOT NULL
        ORDER BY a.id DESC
        LIMIT 1
    )
END
WHERE state IN ('retry_scheduled', 'attention', 'cancelled');

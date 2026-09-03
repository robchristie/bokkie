CREATE TRIGGER audit_events_no_update
BEFORE UPDATE ON audit_events
BEGIN
    SELECT RAISE(ABORT, 'audit events are append-only');
END;

CREATE TRIGGER audit_events_no_delete
BEFORE DELETE ON audit_events
BEGIN
    SELECT RAISE(ABORT, 'audit events are append-only');
END;

CREATE TRIGGER approvals_no_update
BEFORE UPDATE ON approvals
BEGIN
    SELECT RAISE(ABORT, 'approval decisions are immutable');
END;

CREATE TRIGGER approvals_no_delete
BEFORE DELETE ON approvals
BEGIN
    SELECT RAISE(ABORT, 'approval decisions are immutable');
END;

CREATE TRIGGER completed_attempts_no_update
BEFORE UPDATE ON attempts
WHEN OLD.completed_at IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'completed attempts are immutable');
END;

CREATE TRIGGER attempts_no_delete
BEFORE DELETE ON attempts
BEGIN
    SELECT RAISE(ABORT, 'attempts are immutable');
END;

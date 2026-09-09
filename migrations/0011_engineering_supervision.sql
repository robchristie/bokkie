CREATE TABLE engineering_outcomes (
    id TEXT PRIMARY KEY,
    root_obligation_id TEXT NOT NULL UNIQUE REFERENCES obligations(id),
    state_revision INTEGER NOT NULL CHECK (state_revision > 0)
);
CREATE TABLE engineering_bindings (
    obligation_id TEXT PRIMARY KEY REFERENCES obligations(id),
    outcome_id TEXT NOT NULL REFERENCES engineering_outcomes(id),
    package_id TEXT UNIQUE,
    role TEXT NOT NULL CHECK (role IN ('supervisor', 'worker')),
    CHECK ((role = 'supervisor') = (package_id IS NULL))
);
CREATE TABLE engineering_versions (
    outcome_id TEXT NOT NULL REFERENCES engineering_outcomes(id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    snapshot_json TEXT NOT NULL,
    PRIMARY KEY (outcome_id, revision)
);
CREATE TABLE engineering_receipts (
    command_id TEXT PRIMARY KEY,
    payload_digest TEXT NOT NULL,
    envelope_json TEXT NOT NULL,
    actor_json TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    outcome_id TEXT NOT NULL REFERENCES engineering_outcomes(id)
);
CREATE TABLE engineering_dispatches (
    execution_id TEXT PRIMARY KEY,
    outcome_id TEXT NOT NULL REFERENCES engineering_outcomes(id),
    obligation_id TEXT NOT NULL REFERENCES obligations(id),
    lease_generation INTEGER NOT NULL,
    dispatch_json TEXT NOT NULL,
    UNIQUE (obligation_id, lease_generation)
);
CREATE TABLE engineering_writers (
    workspace TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL UNIQUE REFERENCES engineering_dispatches(execution_id)
);
CREATE TRIGGER engineering_versions_no_update BEFORE UPDATE ON engineering_versions BEGIN
    SELECT RAISE(ABORT, 'engineering versions are immutable'); END;
CREATE TRIGGER engineering_versions_no_delete BEFORE DELETE ON engineering_versions BEGIN
    SELECT RAISE(ABORT, 'engineering versions are immutable'); END;
CREATE TRIGGER engineering_receipts_no_update BEFORE UPDATE ON engineering_receipts BEGIN
    SELECT RAISE(ABORT, 'engineering receipts are immutable'); END;
CREATE TRIGGER engineering_receipts_no_delete BEFORE DELETE ON engineering_receipts BEGIN
    SELECT RAISE(ABORT, 'engineering receipts are immutable'); END;
CREATE TRIGGER engineering_dispatches_no_update BEFORE UPDATE ON engineering_dispatches BEGIN
    SELECT RAISE(ABORT, 'engineering dispatches are immutable'); END;
CREATE TRIGGER engineering_dispatches_no_delete BEFORE DELETE ON engineering_dispatches BEGIN
    SELECT RAISE(ABORT, 'engineering dispatches are immutable'); END;
CREATE TRIGGER engineering_bindings_no_update BEFORE UPDATE ON engineering_bindings BEGIN
    SELECT RAISE(ABORT, 'engineering bindings are immutable'); END;
CREATE TRIGGER engineering_bindings_no_delete BEFORE DELETE ON engineering_bindings BEGIN
    SELECT RAISE(ABORT, 'engineering bindings are immutable'); END;
CREATE INDEX engineering_bindings_outcome ON engineering_bindings(outcome_id);

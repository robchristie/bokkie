-- Recovery is additive: the original stopped result and execution stay intact.
CREATE TABLE workspace_execution_recoveries (
    execution_id TEXT PRIMARY KEY REFERENCES workspace_executions(execution_id),
    event_sequence INTEGER NOT NULL CHECK(event_sequence > 0),
    result_json TEXT NOT NULL CHECK(json_valid(result_json)),
    provenance_json TEXT NOT NULL CHECK(json_valid(provenance_json)),
    recorded_at INTEGER NOT NULL,
    FOREIGN KEY(execution_id,event_sequence) REFERENCES workspace_execution_events(execution_id,sequence)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER workspace_recovery_no_update BEFORE UPDATE ON workspace_execution_recoveries
BEGIN SELECT RAISE(ABORT,'workspace recovery is immutable'); END;
CREATE TRIGGER workspace_recovery_no_delete BEFORE DELETE ON workspace_execution_recoveries
BEGIN SELECT RAISE(ABORT,'workspace recovery is retained'); END;

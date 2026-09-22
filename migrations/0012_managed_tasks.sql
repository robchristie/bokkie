-- Definitions are separate from execution obligations. Drafts do not invent work.
CREATE TABLE managed_tasks (
    id TEXT PRIMARY KEY,
    configuration_revision INTEGER NOT NULL CHECK(configuration_revision > 0),
    active_revision INTEGER,
    candidate_revision INTEGER,
    status TEXT NOT NULL CHECK(status IN ('draft', 'active', 'paused', 'completed')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE managed_definitions (
    task_id TEXT NOT NULL REFERENCES managed_tasks(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    definition_json TEXT NOT NULL CHECK(json_valid(definition_json)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(task_id, revision)
);
CREATE TABLE managed_bindings (
    obligation_id TEXT PRIMARY KEY REFERENCES obligations(id),
    task_id TEXT NOT NULL REFERENCES managed_tasks(id),
    definition_revision INTEGER NOT NULL,
    profile_revision TEXT NOT NULL,
    admitted_at INTEGER,
    FOREIGN KEY(task_id, definition_revision) REFERENCES managed_definitions(task_id, revision)
);
CREATE INDEX managed_bindings_task ON managed_bindings(task_id, obligation_id);
CREATE TABLE managed_results (
    obligation_id TEXT PRIMARY KEY REFERENCES managed_bindings(obligation_id),
    result TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE managed_receipts (
    command_id TEXT PRIMARY KEY,
    request_json TEXT NOT NULL,
    receipt_json TEXT NOT NULL
);
CREATE TRIGGER managed_definitions_no_update BEFORE UPDATE ON managed_definitions
BEGIN SELECT RAISE(ABORT, 'managed definitions are immutable'); END;
CREATE TRIGGER managed_definitions_no_delete BEFORE DELETE ON managed_definitions
BEGIN SELECT RAISE(ABORT, 'managed definitions are immutable'); END;
CREATE TRIGGER managed_results_no_update BEFORE UPDATE ON managed_results
BEGIN SELECT RAISE(ABORT, 'managed results are immutable'); END;
CREATE TRIGGER managed_results_no_delete BEFORE DELETE ON managed_results
BEGIN SELECT RAISE(ABORT, 'managed results are immutable'); END;
CREATE TRIGGER managed_receipts_no_update BEFORE UPDATE ON managed_receipts
BEGIN SELECT RAISE(ABORT, 'managed receipts are immutable'); END;
CREATE TRIGGER managed_receipts_no_delete BEFORE DELETE ON managed_receipts
BEGIN SELECT RAISE(ABORT, 'managed receipts are immutable'); END;

-- A genuine non-obligation stream for task definitions and conversation state.
CREATE TABLE domain_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    details_json TEXT NOT NULL CHECK(json_valid(details_json))
);
CREATE TRIGGER domain_events_no_update BEFORE UPDATE ON domain_events
BEGIN SELECT RAISE(ABORT, 'domain events are append-only'); END;
CREATE TRIGGER domain_events_no_delete BEFORE DELETE ON domain_events
BEGIN SELECT RAISE(ABORT, 'domain events are append-only'); END;

-- Rebuild only the envelope schema, preserving every historical cursor exactly.
DROP TRIGGER audit_events_global_envelope;
DROP TRIGGER gardener_events_global_envelope;
DROP TRIGGER gardener_run_events_global_envelope;
CREATE TABLE event_envelopes_extended (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    provenance TEXT NOT NULL CHECK(provenance IN ('legacy_non_causal', 'live_append')),
    source_kind TEXT NOT NULL CHECK(source_kind IN ('audit_event', 'gardener_event', 'gardener_run_event', 'domain_event')),
    audit_event_sequence INTEGER UNIQUE REFERENCES audit_events(sequence),
    gardener_event_sequence INTEGER UNIQUE REFERENCES gardener_events(sequence),
    gardener_run_event_sequence INTEGER UNIQUE REFERENCES gardener_run_events(sequence),
    domain_event_sequence INTEGER UNIQUE REFERENCES domain_events(sequence),
    CHECK (
      (source_kind='audit_event' AND audit_event_sequence IS NOT NULL AND gardener_event_sequence IS NULL AND gardener_run_event_sequence IS NULL AND domain_event_sequence IS NULL) OR
      (source_kind='gardener_event' AND audit_event_sequence IS NULL AND gardener_event_sequence IS NOT NULL AND gardener_run_event_sequence IS NULL AND domain_event_sequence IS NULL) OR
      (source_kind='gardener_run_event' AND audit_event_sequence IS NULL AND gardener_event_sequence IS NULL AND gardener_run_event_sequence IS NOT NULL AND domain_event_sequence IS NULL) OR
      (source_kind='domain_event' AND audit_event_sequence IS NULL AND gardener_event_sequence IS NULL AND gardener_run_event_sequence IS NULL AND domain_event_sequence IS NOT NULL)
    )
);
INSERT INTO event_envelopes_extended(sequence, provenance, source_kind, audit_event_sequence, gardener_event_sequence, gardener_run_event_sequence)
SELECT sequence, provenance, source_kind, audit_event_sequence, gardener_event_sequence, gardener_run_event_sequence FROM event_envelopes ORDER BY sequence;
DROP TABLE event_envelopes;
ALTER TABLE event_envelopes_extended RENAME TO event_envelopes;
CREATE TRIGGER audit_events_global_envelope AFTER INSERT ON audit_events
BEGIN INSERT INTO event_envelopes(provenance, source_kind, audit_event_sequence) VALUES ('live_append','audit_event',NEW.sequence); END;
CREATE TRIGGER gardener_events_global_envelope AFTER INSERT ON gardener_events
BEGIN INSERT INTO event_envelopes(provenance, source_kind, gardener_event_sequence) VALUES ('live_append','gardener_event',NEW.sequence); END;
CREATE TRIGGER gardener_run_events_global_envelope AFTER INSERT ON gardener_run_events
BEGIN INSERT INTO event_envelopes(provenance, source_kind, gardener_run_event_sequence) VALUES ('live_append','gardener_run_event',NEW.sequence); END;
CREATE TRIGGER domain_events_global_envelope AFTER INSERT ON domain_events
BEGIN INSERT INTO event_envelopes(provenance, source_kind, domain_event_sequence) VALUES ('live_append','domain_event',NEW.sequence); END;
CREATE TRIGGER event_envelopes_no_update BEFORE UPDATE ON event_envelopes
BEGIN SELECT RAISE(ABORT, 'global event envelopes are append-only'); END;
CREATE TRIGGER event_envelopes_no_delete BEFORE DELETE ON event_envelopes
BEGIN SELECT RAISE(ABORT, 'global event envelopes are append-only'); END;

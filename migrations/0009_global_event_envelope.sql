-- The three existing event streams have independent sequence spaces. Historical
-- transaction causality therefore cannot be reconstructed. The deterministic
-- legacy order below is suitable for complete replay, but is explicitly marked
-- non-causal; only live_append rows carry authoritative cross-stream insertion
-- order.
CREATE TABLE event_envelopes (
    sequence                     INTEGER PRIMARY KEY AUTOINCREMENT,
    provenance                   TEXT NOT NULL CHECK (provenance IN (
                                     'legacy_non_causal', 'live_append'
                                 )),
    source_kind                  TEXT NOT NULL CHECK (source_kind IN (
                                     'audit_event', 'gardener_event',
                                     'gardener_run_event'
                                 )),
    audit_event_sequence         INTEGER UNIQUE
        REFERENCES audit_events(sequence) ON UPDATE RESTRICT ON DELETE RESTRICT,
    gardener_event_sequence      INTEGER UNIQUE
        REFERENCES gardener_events(sequence) ON UPDATE RESTRICT ON DELETE RESTRICT,
    gardener_run_event_sequence  INTEGER UNIQUE
        REFERENCES gardener_run_events(sequence) ON UPDATE RESTRICT ON DELETE RESTRICT,
    CHECK (
        (source_kind = 'audit_event'
            AND audit_event_sequence IS NOT NULL
            AND gardener_event_sequence IS NULL
            AND gardener_run_event_sequence IS NULL)
        OR
        (source_kind = 'gardener_event'
            AND audit_event_sequence IS NULL
            AND gardener_event_sequence IS NOT NULL
            AND gardener_run_event_sequence IS NULL)
        OR
        (source_kind = 'gardener_run_event'
            AND audit_event_sequence IS NULL
            AND gardener_event_sequence IS NULL
            AND gardener_run_event_sequence IS NOT NULL)
    )
);

INSERT INTO event_envelopes(
    provenance, source_kind, audit_event_sequence,
    gardener_event_sequence, gardener_run_event_sequence
)
SELECT 'legacy_non_causal', source_kind, audit_event_sequence,
       gardener_event_sequence, gardener_run_event_sequence
FROM (
    SELECT occurred_at, 1 AS source_rank, sequence AS source_sequence,
           'audit_event' AS source_kind,
           sequence AS audit_event_sequence,
           NULL AS gardener_event_sequence,
           NULL AS gardener_run_event_sequence
    FROM audit_events
    UNION ALL
    SELECT occurred_at, 2, sequence, 'gardener_event',
           NULL, sequence, NULL
    FROM gardener_events
    UNION ALL
    SELECT occurred_at, 3, sequence, 'gardener_run_event',
           NULL, NULL, sequence
    FROM gardener_run_events
)
ORDER BY occurred_at, source_rank, source_sequence;

CREATE TRIGGER audit_events_global_envelope
AFTER INSERT ON audit_events
BEGIN
    INSERT INTO event_envelopes(
        provenance, source_kind, audit_event_sequence
    ) VALUES ('live_append', 'audit_event', NEW.sequence);
END;

CREATE TRIGGER gardener_events_global_envelope
AFTER INSERT ON gardener_events
BEGIN
    INSERT INTO event_envelopes(
        provenance, source_kind, gardener_event_sequence
    ) VALUES ('live_append', 'gardener_event', NEW.sequence);
END;

CREATE TRIGGER gardener_run_events_global_envelope
AFTER INSERT ON gardener_run_events
BEGIN
    INSERT INTO event_envelopes(
        provenance, source_kind, gardener_run_event_sequence
    ) VALUES ('live_append', 'gardener_run_event', NEW.sequence);
END;

CREATE TRIGGER event_envelopes_no_update
BEFORE UPDATE ON event_envelopes
BEGIN
    SELECT RAISE(ABORT, 'global event envelopes are append-only');
END;

CREATE TRIGGER event_envelopes_no_delete
BEFORE DELETE ON event_envelopes
BEGIN
    SELECT RAISE(ABORT, 'global event envelopes are append-only');
END;

-- Public projection orderings are part of the pagination contract. These
-- indexes let SQLite seek after the opaque cursor key and stop at LIMIT N+1.
CREATE INDEX obligations_projection_order_idx
    ON obligations(created_at, id);
CREATE INDEX gardener_inspections_projection_order_idx
    ON gardener_inspections(started_at, id);
CREATE INDEX gardener_proposals_projection_order_idx
    ON gardener_proposals(created_at, fingerprint);
CREATE INDEX gardener_instances_projection_order_idx
    ON gardener_proposal_instances(created_at, id);
CREATE INDEX gardener_runs_projection_order_idx
    ON gardener_implementation_runs(created_at, id);

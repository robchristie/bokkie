-- Source-bound proposal generations are additive evidence. Existing proposals,
-- observations, approvals, audit events, runs and gardener events remain intact.
CREATE TABLE gardener_proposal_instances (
    id                           TEXT PRIMARY KEY
        CHECK (length(id) BETWEEN 1 AND 160),
    proposal_fingerprint         TEXT NOT NULL REFERENCES gardener_proposals(fingerprint)
        CHECK (length(proposal_fingerprint) = 64),
    source_commit                TEXT NOT NULL
        CHECK (length(source_commit) IN (40, 64) AND source_commit = lower(source_commit)),
    source_observation_id        INTEGER NOT NULL UNIQUE
        REFERENCES gardener_proposal_observations(id),
    source_inspection_id         TEXT NOT NULL REFERENCES gardener_inspections(id)
        CHECK (length(source_inspection_id) BETWEEN 1 AND 160),
    generation                   INTEGER NOT NULL CHECK (generation > 0),
    implementation_obligation_id TEXT NOT NULL UNIQUE REFERENCES obligations(id)
        CHECK (length(implementation_obligation_id) BETWEEN 1 AND 128),
    created_at                   INTEGER NOT NULL,
    UNIQUE (proposal_fingerprint, source_commit),
    UNIQUE (proposal_fingerprint, generation)
);

CREATE TABLE gardener_proposal_observation_instances (
    observation_id INTEGER PRIMARY KEY REFERENCES gardener_proposal_observations(id),
    instance_id    TEXT NOT NULL REFERENCES gardener_proposal_instances(id)
        CHECK (length(instance_id) BETWEEN 1 AND 160),
    mapped_at      INTEGER NOT NULL
);

CREATE TABLE gardener_proposal_instance_supersessions (
    superseded_instance_id TEXT PRIMARY KEY REFERENCES gardener_proposal_instances(id)
        CHECK (length(superseded_instance_id) BETWEEN 1 AND 160),
    superseding_instance_id TEXT NOT NULL REFERENCES gardener_proposal_instances(id)
        CHECK (length(superseding_instance_id) BETWEEN 1 AND 160),
    occurred_at             INTEGER NOT NULL,
    reason                  TEXT NOT NULL DEFAULT 'new_source_observed'
        CHECK (reason = 'new_source_observed'),
    CHECK (superseded_instance_id <> superseding_instance_id)
);

-- This table makes an existing approval exact without changing the immutable
-- approval row. Multi-source legacy approvals are deliberately not backfilled.
CREATE TABLE gardener_proposal_instance_decisions (
    approval_id          INTEGER PRIMARY KEY REFERENCES approvals(id),
    instance_id          TEXT NOT NULL REFERENCES gardener_proposal_instances(id)
        CHECK (length(instance_id) BETWEEN 1 AND 160),
    proposal_fingerprint TEXT NOT NULL CHECK (length(proposal_fingerprint) = 64),
    source_commit        TEXT NOT NULL
        CHECK (length(source_commit) IN (40, 64) AND source_commit = lower(source_commit)),
    generation           INTEGER NOT NULL CHECK (generation > 0),
    obligation_id        TEXT NOT NULL CHECK (length(obligation_id) BETWEEN 1 AND 128),
    occurrence           INTEGER NOT NULL CHECK (occurrence > 0),
    decision             TEXT NOT NULL CHECK (decision IN ('approved', 'rejected')),
    recorded_at          INTEGER NOT NULL
);

CREATE TABLE gardener_implementation_run_instances (
    run_id               TEXT PRIMARY KEY REFERENCES gardener_implementation_runs(id)
        CHECK (length(run_id) BETWEEN 1 AND 160),
    instance_id          TEXT NOT NULL REFERENCES gardener_proposal_instances(id)
        CHECK (length(instance_id) BETWEEN 1 AND 160),
    proposal_fingerprint TEXT NOT NULL CHECK (length(proposal_fingerprint) = 64),
    source_commit        TEXT NOT NULL
        CHECK (length(source_commit) IN (40, 64) AND source_commit = lower(source_commit)),
    generation           INTEGER NOT NULL CHECK (generation > 0),
    mapped_at            INTEGER NOT NULL
);

CREATE INDEX gardener_instances_goal_idx
    ON gardener_proposal_instances(proposal_fingerprint, generation);
CREATE INDEX gardener_observation_instances_instance_idx
    ON gardener_proposal_observation_instances(instance_id, observation_id);
CREATE INDEX gardener_instance_supersessions_new_idx
    ON gardener_proposal_instance_supersessions(superseding_instance_id);
CREATE INDEX gardener_instance_decisions_instance_idx
    ON gardener_proposal_instance_decisions(instance_id, approval_id);
CREATE INDEX gardener_run_instances_instance_idx
    ON gardener_implementation_run_instances(instance_id, run_id);

-- Determine generations by the first durable observation of each distinct
-- canonical source. The same expression is repeated because SQLite CTEs are
-- statement-scoped.
INSERT INTO obligations(
    id, description, state, occurrence, scheduled_at, next_wake_at,
    recurrence_cron, recurrence_timezone, approval_required, attempts_made,
    max_attempts, retry_base_seconds, retry_max_seconds, lease_generation,
    created_at, updated_at
)
WITH source_ids AS (
    SELECT po.proposal_fingerprint, lower(po.source_commit) AS source_commit,
           min(po.id) AS first_observation_id
    FROM gardener_proposal_observations po
    GROUP BY po.proposal_fingerprint, lower(po.source_commit)
), sources AS (
    SELECT source_ids.*, po.observed_at AS created_at
    FROM source_ids
    JOIN gardener_proposal_observations po ON po.id = source_ids.first_observation_id
), generations AS (
    SELECT *, row_number() OVER (
        PARTITION BY proposal_fingerprint ORDER BY first_observation_id, source_commit
    ) AS generation
    FROM sources
)
SELECT 'gardener:implement:' || proposal_fingerprint || ':g' || generation,
       'Implement approved gardener proposal ' || proposal_fingerprint ||
           ' generation ' || generation,
       'awaiting_approval', 1, created_at, NULL, NULL, NULL, 1, 0,
       1, 60, 3600, 0, created_at, created_at
FROM generations
WHERE generation > 1;

INSERT INTO gardener_obligation_bindings(obligation_id, kind, created_at)
WITH source_ids AS (
    SELECT po.proposal_fingerprint, lower(po.source_commit) AS source_commit,
           min(po.id) AS first_observation_id
    FROM gardener_proposal_observations po
    GROUP BY po.proposal_fingerprint, lower(po.source_commit)
), sources AS (
    SELECT source_ids.*, po.observed_at AS created_at
    FROM source_ids
    JOIN gardener_proposal_observations po ON po.id = source_ids.first_observation_id
), generations AS (
    SELECT *, row_number() OVER (
        PARTITION BY proposal_fingerprint ORDER BY first_observation_id, source_commit
    ) AS generation
    FROM sources
)
SELECT 'gardener:implement:' || proposal_fingerprint || ':g' || generation,
       'implementation', created_at
FROM generations
WHERE generation > 1;

INSERT INTO audit_events(
    obligation_id, occurrence, event_type, occurred_at, from_state, to_state, details_json
)
WITH source_ids AS (
    SELECT po.proposal_fingerprint, lower(po.source_commit) AS source_commit,
           min(po.id) AS first_observation_id
    FROM gardener_proposal_observations po
    GROUP BY po.proposal_fingerprint, lower(po.source_commit)
), sources AS (
    SELECT source_ids.*, po.observed_at AS created_at
    FROM source_ids
    JOIN gardener_proposal_observations po ON po.id = source_ids.first_observation_id
), generations AS (
    SELECT *, row_number() OVER (
        PARTITION BY proposal_fingerprint ORDER BY first_observation_id, source_commit
    ) AS generation
    FROM sources
)
SELECT 'gardener:implement:' || proposal_fingerprint || ':g' || generation,
       1, 'created', created_at, NULL, 'awaiting_approval',
       json_object('scheduled_at', created_at, 'proposal_fingerprint', proposal_fingerprint,
                   'source_commit', source_commit, 'generation', generation,
                   'migration_backfill', json('true'))
FROM generations
WHERE generation > 1;

INSERT INTO gardener_proposal_instances(
    id, proposal_fingerprint, source_commit, source_observation_id,
    source_inspection_id, generation,
    implementation_obligation_id, created_at
)
WITH source_ids AS (
    SELECT po.proposal_fingerprint, lower(po.source_commit) AS source_commit,
           min(po.id) AS first_observation_id
    FROM gardener_proposal_observations po
    GROUP BY po.proposal_fingerprint, lower(po.source_commit)
), sources AS (
    SELECT source_ids.*, po.observed_at AS created_at
    FROM source_ids
    JOIN gardener_proposal_observations po ON po.id = source_ids.first_observation_id
), generations AS (
    SELECT *, row_number() OVER (
        PARTITION BY proposal_fingerprint ORDER BY first_observation_id, source_commit
    ) AS generation
    FROM sources
)
SELECT 'pi:' || proposal_fingerprint || ':' || source_commit || ':' || generation,
       proposal_fingerprint, source_commit, first_observation_id,
       (SELECT inspection_id FROM gardener_proposal_observations po
        WHERE po.id = generations.first_observation_id),
       generation,
       CASE WHEN generation = 1
            THEN (SELECT implementation_obligation_id FROM gardener_proposals p
                  WHERE p.fingerprint = generations.proposal_fingerprint)
            ELSE 'gardener:implement:' || proposal_fingerprint || ':g' || generation
       END,
       created_at
FROM generations;

INSERT INTO gardener_proposal_observation_instances(observation_id, instance_id, mapped_at)
SELECT po.id,
       'pi:' || po.proposal_fingerprint || ':' || lower(po.source_commit) || ':' || pi.generation,
       po.observed_at
FROM gardener_proposal_observations po
JOIN gardener_proposal_instances pi
  ON pi.proposal_fingerprint = po.proposal_fingerprint
 AND pi.source_commit = lower(po.source_commit);

INSERT INTO gardener_proposal_instance_supersessions(
    superseded_instance_id, superseding_instance_id, occurred_at
)
SELECT earlier.id, later.id, later.created_at
FROM gardener_proposal_instances earlier
JOIN gardener_proposal_instances later
  ON later.proposal_fingerprint = earlier.proposal_fingerprint
 AND later.generation = earlier.generation + 1;

-- Preserve safe one-source authority. A multi-source legacy decision did not
-- identify which source was reviewed, so it remains visible only in approvals.
INSERT INTO gardener_proposal_instance_decisions(
    approval_id, instance_id, proposal_fingerprint, source_commit, generation,
    obligation_id, occurrence, decision, recorded_at
)
SELECT a.id, pi.id, pi.proposal_fingerprint, pi.source_commit, pi.generation,
       a.obligation_id, a.occurrence, a.decision, a.decided_at
FROM approvals a
JOIN gardener_proposals p ON p.implementation_obligation_id = a.obligation_id
JOIN gardener_proposal_instances pi ON pi.proposal_fingerprint = p.fingerprint
WHERE (SELECT count(*) FROM gardener_proposal_instances all_pi
       WHERE all_pi.proposal_fingerprint = p.fingerprint) = 1;

-- A run's own immutable source is enough to identify its instance, even when
-- the old approval was ambiguous. Inconsistent runs abort the migration; the
-- Store separately fences continuation when exact approval authority is absent.
INSERT INTO gardener_implementation_run_instances(
    run_id, instance_id, proposal_fingerprint, source_commit, generation, mapped_at
)
SELECT r.id, pi.id, pi.proposal_fingerprint, pi.source_commit, pi.generation, r.created_at
FROM gardener_implementation_runs r
JOIN gardener_proposal_instances pi
  ON pi.proposal_fingerprint = r.proposal_fingerprint
 AND pi.source_commit = lower(r.source_commit);

CREATE TABLE migration_0006_run_mapping_assertion (
    invalid INTEGER NOT NULL CHECK (invalid = 0)
);
INSERT INTO migration_0006_run_mapping_assertion(invalid)
SELECT 1
WHERE EXISTS (
    SELECT 1 FROM gardener_implementation_runs r
    LEFT JOIN gardener_implementation_run_instances ri ON ri.run_id = r.id
    WHERE ri.run_id IS NULL
);
DROP TABLE migration_0006_run_mapping_assertion;

-- Earlier generated obligations are no longer actionable. Existing running
-- work is left leased and is governed by its exact run mapping; other states
-- are cancelled with append-only audit evidence.
INSERT INTO audit_events(
    obligation_id, occurrence, event_type, occurred_at, from_state, to_state, details_json
)
SELECT o.id, o.occurrence, 'proposal_instance_superseded', s.occurred_at,
       o.state, 'cancelled',
       json_object('proposal_instance_id', pi.id,
                   'superseding_instance_id', s.superseding_instance_id,
                   'source_commit', pi.source_commit,
                   'generation', pi.generation,
                   'migration_backfill', json('true'))
FROM gardener_proposal_instance_supersessions s
JOIN gardener_proposal_instances pi ON pi.id = s.superseded_instance_id
JOIN obligations o ON o.id = pi.implementation_obligation_id
WHERE o.state NOT IN ('completed', 'cancelled', 'running');

UPDATE obligations
SET state = 'cancelled', next_wake_at = NULL, lease_token = NULL,
    lease_expires_at = NULL,
    updated_at = (SELECT s.occurred_at
                  FROM gardener_proposal_instance_supersessions s
                  JOIN gardener_proposal_instances pi
                    ON pi.id = s.superseded_instance_id
                  WHERE pi.implementation_obligation_id = obligations.id)
WHERE id IN (
    SELECT pi.implementation_obligation_id
    FROM gardener_proposal_instance_supersessions s
    JOIN gardener_proposal_instances pi ON pi.id = s.superseded_instance_id
)
AND state NOT IN ('completed', 'cancelled', 'running');

CREATE TRIGGER gardener_instances_no_update
BEFORE UPDATE ON gardener_proposal_instances
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal instances are immutable');
END;
CREATE TRIGGER gardener_instances_insert_guard
BEFORE INSERT ON gardener_proposal_instances
WHEN NOT EXISTS (
    SELECT 1 FROM gardener_proposal_observations po
    WHERE po.id = NEW.source_observation_id
      AND po.proposal_fingerprint = NEW.proposal_fingerprint
      AND lower(po.source_commit) = NEW.source_commit
      AND po.inspection_id = NEW.source_inspection_id
)
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal instance source evidence does not match');
END;
CREATE TRIGGER gardener_instances_no_delete
BEFORE DELETE ON gardener_proposal_instances
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal instances are immutable');
END;
CREATE TRIGGER gardener_observation_instances_no_update
BEFORE UPDATE ON gardener_proposal_observation_instances
BEGIN
    SELECT RAISE(ABORT, 'gardener observation-instance mappings are append-only');
END;
CREATE TRIGGER gardener_observation_instances_insert_guard
BEFORE INSERT ON gardener_proposal_observation_instances
WHEN NOT EXISTS (
    SELECT 1
    FROM gardener_proposal_observations po
    JOIN gardener_proposal_instances pi ON pi.id = NEW.instance_id
    WHERE po.id = NEW.observation_id
      AND po.proposal_fingerprint = pi.proposal_fingerprint
      AND lower(po.source_commit) = pi.source_commit
)
BEGIN
    SELECT RAISE(ABORT, 'gardener observation does not match proposal instance');
END;
CREATE TRIGGER gardener_observation_instances_no_delete
BEFORE DELETE ON gardener_proposal_observation_instances
BEGIN
    SELECT RAISE(ABORT, 'gardener observation-instance mappings are append-only');
END;
CREATE TRIGGER gardener_instance_supersessions_no_update
BEFORE UPDATE ON gardener_proposal_instance_supersessions
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal supersessions are append-only');
END;
CREATE TRIGGER gardener_instance_supersessions_insert_guard
BEFORE INSERT ON gardener_proposal_instance_supersessions
WHEN NOT EXISTS (
    SELECT 1
    FROM gardener_proposal_instances old
    JOIN gardener_proposal_instances new
      ON new.id = NEW.superseding_instance_id
    WHERE old.id = NEW.superseded_instance_id
      AND old.proposal_fingerprint = new.proposal_fingerprint
      AND new.generation = old.generation + 1
)
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal supersession is not consecutive');
END;
CREATE TRIGGER gardener_instance_supersessions_no_delete
BEFORE DELETE ON gardener_proposal_instance_supersessions
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal supersessions are append-only');
END;
CREATE TRIGGER gardener_instance_decisions_no_update
BEFORE UPDATE ON gardener_proposal_instance_decisions
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal instance decisions are immutable');
END;
CREATE TRIGGER gardener_instance_decisions_insert_guard
BEFORE INSERT ON gardener_proposal_instance_decisions
WHEN NOT EXISTS (
    SELECT 1
    FROM gardener_proposal_instances pi
    JOIN approvals a ON a.id = NEW.approval_id
    WHERE pi.id = NEW.instance_id
      AND pi.proposal_fingerprint = NEW.proposal_fingerprint
      AND pi.source_commit = NEW.source_commit
      AND pi.generation = NEW.generation
      AND pi.implementation_obligation_id = NEW.obligation_id
      AND a.obligation_id = NEW.obligation_id
      AND a.occurrence = NEW.occurrence
      AND a.decision = NEW.decision
)
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal decision evidence does not match');
END;
CREATE TRIGGER gardener_instance_decisions_no_delete
BEFORE DELETE ON gardener_proposal_instance_decisions
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal instance decisions are immutable');
END;
CREATE TRIGGER gardener_run_instances_no_update
BEFORE UPDATE ON gardener_implementation_run_instances
BEGIN
    SELECT RAISE(ABORT, 'gardener run-instance mappings are immutable');
END;
CREATE TRIGGER gardener_run_instances_insert_guard
BEFORE INSERT ON gardener_implementation_run_instances
WHEN NOT EXISTS (
    SELECT 1
    FROM gardener_proposal_instances pi
    JOIN gardener_implementation_runs r ON r.id = NEW.run_id
    WHERE pi.id = NEW.instance_id
      AND pi.proposal_fingerprint = NEW.proposal_fingerprint
      AND pi.source_commit = NEW.source_commit
      AND pi.generation = NEW.generation
      AND r.proposal_fingerprint = NEW.proposal_fingerprint
      AND lower(r.source_commit) = NEW.source_commit
      AND r.obligation_id = pi.implementation_obligation_id
)
BEGIN
    SELECT RAISE(ABORT, 'gardener run does not match proposal instance');
END;
CREATE TRIGGER gardener_run_instances_no_delete
BEFORE DELETE ON gardener_implementation_run_instances
BEGIN
    SELECT RAISE(ABORT, 'gardener run-instance mappings are immutable');
END;

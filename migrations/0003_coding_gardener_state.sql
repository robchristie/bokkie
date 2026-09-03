CREATE TABLE gardener_obligation_bindings (
    obligation_id TEXT PRIMARY KEY REFERENCES obligations(id),
    kind          TEXT NOT NULL CHECK (kind IN ('inspection', 'implementation')),
    created_at    INTEGER NOT NULL
);

CREATE TABLE gardener_repositories (
    repository               TEXT PRIMARY KEY CHECK (repository = 'robchristie/bokkie'),
    default_branch           TEXT NOT NULL CHECK (default_branch = 'main'),
    checkout_path            TEXT NOT NULL,
    inspection_cron          TEXT NOT NULL,
    inspection_timezone      TEXT NOT NULL,
    first_inspection_at      INTEGER NOT NULL,
    inspection_obligation_id TEXT NOT NULL UNIQUE
        REFERENCES gardener_obligation_bindings(obligation_id),
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL
);

CREATE TABLE gardener_inspections (
    id               TEXT PRIMARY KEY,
    repository       TEXT NOT NULL REFERENCES gardener_repositories(repository),
    obligation_id    TEXT NOT NULL REFERENCES gardener_obligation_bindings(obligation_id),
    occurrence       INTEGER NOT NULL CHECK (occurrence > 0),
    lease_generation INTEGER NOT NULL CHECK (lease_generation > 0),
    lease_token      TEXT NOT NULL,
    source_commit    TEXT NOT NULL,
    worktree_path    TEXT NOT NULL,
    prompt_digest    TEXT NOT NULL,
    codex_thread_id  TEXT,
    codex_turn_id    TEXT,
    result_json      TEXT,
    started_at       INTEGER NOT NULL,
    completed_at     INTEGER,
    UNIQUE (obligation_id, occurrence, lease_generation),
    CHECK ((result_json IS NULL) = (completed_at IS NULL)),
    CHECK (result_json IS NULL OR json_valid(result_json))
);

CREATE TABLE gardener_proposals (
    fingerprint                 TEXT PRIMARY KEY,
    repository                  TEXT NOT NULL REFERENCES gardener_repositories(repository),
    prompt                      TEXT NOT NULL,
    implementation_obligation_id TEXT NOT NULL UNIQUE REFERENCES obligations(id),
    created_at                  INTEGER NOT NULL
);

CREATE TABLE gardener_proposal_observations (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    proposal_fingerprint TEXT NOT NULL REFERENCES gardener_proposals(fingerprint),
    inspection_id        TEXT NOT NULL REFERENCES gardener_inspections(id),
    source_commit        TEXT NOT NULL,
    observed_at          INTEGER NOT NULL,
    UNIQUE (proposal_fingerprint, inspection_id)
);

CREATE TABLE gardener_events (
    sequence             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository           TEXT NOT NULL REFERENCES gardener_repositories(repository),
    inspection_id        TEXT REFERENCES gardener_inspections(id),
    proposal_fingerprint TEXT REFERENCES gardener_proposals(fingerprint),
    event_type           TEXT NOT NULL,
    occurred_at          INTEGER NOT NULL,
    details_json         TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX gardener_events_repository_idx
    ON gardener_events(repository, sequence);
CREATE INDEX gardener_observations_proposal_idx
    ON gardener_proposal_observations(proposal_fingerprint, id);

CREATE TRIGGER gardener_bindings_no_update
BEFORE UPDATE ON gardener_obligation_bindings
BEGIN
    SELECT RAISE(ABORT, 'gardener obligation bindings are write-once');
END;

CREATE TRIGGER gardener_bindings_no_delete
BEFORE DELETE ON gardener_obligation_bindings
BEGIN
    SELECT RAISE(ABORT, 'gardener obligation bindings are write-once');
END;

CREATE TRIGGER gardener_repositories_no_update
BEFORE UPDATE ON gardener_repositories
BEGIN
    SELECT RAISE(ABORT, 'gardener repository registrations are immutable');
END;

CREATE TRIGGER gardener_repositories_no_delete
BEFORE DELETE ON gardener_repositories
BEGIN
    SELECT RAISE(ABORT, 'gardener repository registrations are immutable');
END;

CREATE TRIGGER gardener_inspection_identity_guard
BEFORE UPDATE ON gardener_inspections
WHEN OLD.repository IS NOT NEW.repository
  OR OLD.obligation_id IS NOT NEW.obligation_id
  OR OLD.occurrence IS NOT NEW.occurrence
  OR OLD.lease_generation IS NOT NEW.lease_generation
  OR OLD.lease_token IS NOT NEW.lease_token
  OR OLD.source_commit IS NOT NEW.source_commit
  OR OLD.worktree_path IS NOT NEW.worktree_path
  OR OLD.prompt_digest IS NOT NEW.prompt_digest
  OR OLD.started_at IS NOT NEW.started_at
  OR (OLD.codex_thread_id IS NOT NULL AND OLD.codex_thread_id IS NOT NEW.codex_thread_id)
  OR (OLD.codex_turn_id IS NOT NULL AND OLD.codex_turn_id IS NOT NEW.codex_turn_id)
  OR (OLD.result_json IS NOT NULL AND OLD.result_json IS NOT NEW.result_json)
  OR (OLD.completed_at IS NOT NULL AND OLD.completed_at IS NOT NEW.completed_at)
BEGIN
    SELECT RAISE(ABORT, 'gardener inspection identities and results are write-once');
END;

CREATE TRIGGER gardener_inspections_no_delete
BEFORE DELETE ON gardener_inspections
BEGIN
    SELECT RAISE(ABORT, 'gardener inspections are immutable');
END;

CREATE TRIGGER gardener_proposals_no_update
BEFORE UPDATE ON gardener_proposals
BEGIN
    SELECT RAISE(ABORT, 'gardener proposals are immutable');
END;

CREATE TRIGGER gardener_proposals_no_delete
BEFORE DELETE ON gardener_proposals
BEGIN
    SELECT RAISE(ABORT, 'gardener proposals are immutable');
END;

CREATE TRIGGER gardener_observations_no_update
BEFORE UPDATE ON gardener_proposal_observations
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal observations are append-only');
END;

CREATE TRIGGER gardener_observations_no_delete
BEFORE DELETE ON gardener_proposal_observations
BEGIN
    SELECT RAISE(ABORT, 'gardener proposal observations are append-only');
END;

CREATE TRIGGER gardener_events_no_update
BEFORE UPDATE ON gardener_events
BEGIN
    SELECT RAISE(ABORT, 'gardener events are append-only');
END;

CREATE TRIGGER gardener_events_no_delete
BEFORE DELETE ON gardener_events
BEGIN
    SELECT RAISE(ABORT, 'gardener events are append-only');
END;

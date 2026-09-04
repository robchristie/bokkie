ALTER TABLE gardener_implementation_runs
    ADD COLUMN publication_state TEXT NOT NULL DEFAULT 'not_created'
        CHECK (publication_state IN ('not_created', 'draft', 'ready_pending', 'ready'));

ALTER TABLE gardener_implementation_runs
    ADD COLUMN pull_request_ready_at INTEGER;

-- Preserve the observable state of databases created by the pre-P2 workflow.
UPDATE gardener_implementation_runs
SET publication_state = CASE
        WHEN verification_verdict = 'pass' THEN 'ready'
        WHEN pull_request_number IS NOT NULL THEN 'draft'
        ELSE 'not_created'
    END,
    pull_request_ready_at = CASE
        WHEN verification_verdict = 'pass' THEN verification_finished_at
        ELSE NULL
    END;

CREATE TABLE gardener_run_reproducibility (
    run_id                          TEXT PRIMARY KEY
        REFERENCES gardener_implementation_runs(id),
    bokkie_build                    TEXT NOT NULL CHECK (length(bokkie_build) > 0),
    source_commit                   TEXT NOT NULL CHECK (length(source_commit) IN (40, 64)),
    prompt_digest                   TEXT NOT NULL CHECK (length(prompt_digest) = 64),
    implementation_schema_digest   TEXT NOT NULL CHECK (length(implementation_schema_digest) = 64),
    verification_schema_digest     TEXT NOT NULL CHECK (length(verification_schema_digest) = 64),
    codex_profile                   TEXT,
    codex_model                     TEXT,
    executable_manifest_json       TEXT NOT NULL CHECK (
                                        json_valid(executable_manifest_json)
                                        AND json_type(executable_manifest_json) = 'array'
                                    ),
    sandbox_policy_digest           TEXT NOT NULL CHECK (length(sandbox_policy_digest) = 64),
    environment_policy_digest       TEXT NOT NULL CHECK (length(environment_policy_digest) = 64),
    check_commands_json             TEXT NOT NULL CHECK (
                                        json_valid(check_commands_json)
                                        AND json_type(check_commands_json) = 'array'
                                    ),
    recorded_at                     INTEGER NOT NULL
);

CREATE TABLE gardener_candidate_qualifications (
    run_id              TEXT PRIMARY KEY REFERENCES gardener_implementation_runs(id),
    head                TEXT NOT NULL CHECK (length(head) IN (40, 64)),
    diff_manifest_json  TEXT NOT NULL CHECK (
                            json_valid(diff_manifest_json)
                            AND json_type(diff_manifest_json) = 'array'
                        ),
    tree_manifest_json  TEXT NOT NULL CHECK (
                            json_valid(tree_manifest_json)
                            AND json_type(tree_manifest_json) = 'array'
                        ),
    checks_json         TEXT NOT NULL CHECK (
                            json_valid(checks_json)
                            AND json_type(checks_json) = 'array'
                        ),
    duration_ms         INTEGER NOT NULL CHECK (duration_ms >= 0),
    qualified_at        INTEGER NOT NULL
);

-- New P2 runs leave the base row at ready_pending after recording ready intent.
-- Store projections derive observable ready state only when this immutable
-- post-mutation observation exists; direct diagnostic readers must join it.
CREATE TABLE gardener_pull_request_ready_observations (
    run_id      TEXT PRIMARY KEY REFERENCES gardener_implementation_runs(id),
    number      INTEGER NOT NULL CHECK (number > 0),
    url         TEXT NOT NULL,
    head        TEXT NOT NULL CHECK (length(head) IN (40, 64)),
    ready_at    INTEGER NOT NULL
);

CREATE TRIGGER gardener_reproducibility_no_update
BEFORE UPDATE ON gardener_run_reproducibility
BEGIN
    SELECT RAISE(ABORT, 'gardener reproducibility manifests are write-once');
END;

CREATE TRIGGER gardener_reproducibility_no_delete
BEFORE DELETE ON gardener_run_reproducibility
BEGIN
    SELECT RAISE(ABORT, 'gardener reproducibility manifests are durable');
END;

CREATE TRIGGER gardener_candidate_qualifications_no_update
BEFORE UPDATE ON gardener_candidate_qualifications
BEGIN
    SELECT RAISE(ABORT, 'gardener candidate qualifications are write-once');
END;

CREATE TRIGGER gardener_candidate_qualifications_no_delete
BEFORE DELETE ON gardener_candidate_qualifications
BEGIN
    SELECT RAISE(ABORT, 'gardener candidate qualifications are durable');
END;

CREATE TRIGGER gardener_ready_observations_no_update
BEFORE UPDATE ON gardener_pull_request_ready_observations
BEGIN
    SELECT RAISE(ABORT, 'gardener ready observations are write-once');
END;

CREATE TRIGGER gardener_ready_observations_no_delete
BEFORE DELETE ON gardener_pull_request_ready_observations
BEGIN
    SELECT RAISE(ABORT, 'gardener ready observations are durable');
END;

CREATE TRIGGER gardener_publication_state_guard
BEFORE UPDATE OF publication_state, pull_request_ready_at ON gardener_implementation_runs
WHEN NOT (
    (OLD.publication_state = 'not_created' AND NEW.publication_state = 'draft'
        AND NEW.pull_request_ready_at IS NULL)
    OR (OLD.publication_state = 'draft' AND NEW.publication_state = 'ready_pending'
        AND NEW.pull_request_ready_at IS NULL)
    OR (OLD.publication_state = 'ready_pending' AND NEW.publication_state = 'ready'
        AND NEW.pull_request_ready_at IS NOT NULL)
)
BEGIN
    SELECT RAISE(ABORT, 'gardener publication state transition is out of order');
END;

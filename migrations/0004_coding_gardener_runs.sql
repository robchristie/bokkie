CREATE TABLE gardener_implementation_runs (
    id                                  TEXT PRIMARY KEY,
    repository                          TEXT NOT NULL CHECK (repository = 'robchristie/bokkie')
        REFERENCES gardener_repositories(repository),
    proposal_fingerprint                TEXT NOT NULL REFERENCES gardener_proposals(fingerprint),
    obligation_id                       TEXT NOT NULL REFERENCES gardener_obligation_bindings(obligation_id),
    occurrence                          INTEGER NOT NULL CHECK (occurrence > 0),
    attempt_number                      INTEGER NOT NULL CHECK (attempt_number > 0),
    lease_generation                    INTEGER NOT NULL CHECK (lease_generation > 0),
    lease_token                         TEXT NOT NULL,
    source_commit                       TEXT NOT NULL,
    implementation_worktree_path        TEXT NOT NULL CHECK (implementation_worktree_path LIKE '/%'),
    branch                              TEXT NOT NULL CHECK (
                                            branch GLOB 'codex/gardener-*'
                                            AND length(branch) > length('codex/gardener-')
                                        ),
    phase                               TEXT NOT NULL CHECK (phase IN (
                                            'created',
                                            'implementation_thread_recorded',
                                            'implementation_turn_recorded',
                                            'implementation_finished',
                                            'git_commit_recorded',
                                            'push_observed',
                                            'pull_request_ready',
                                            'verification_started',
                                            'verification_thread_recorded',
                                            'verification_turn_recorded',
                                            'verification_finished'
                                        )),
    implementation_thread_id            TEXT,
    implementation_turn_id              TEXT,
    implementation_final_message_json   TEXT,
    git_commit                          TEXT,
    pushed_head                         TEXT,
    pull_request_number                 INTEGER CHECK (pull_request_number > 0),
    pull_request_url                    TEXT,
    pull_request_head                   TEXT,
    verification_worktree_path          TEXT CHECK (verification_worktree_path LIKE '/%'),
    verification_head                   TEXT,
    verification_thread_id              TEXT,
    verification_turn_id                TEXT,
    verification_verdict                TEXT CHECK (verification_verdict IN (
                                            'pass', 'blocking', 'inconclusive'
                                        )),
    verification_reported_head          TEXT,
    verification_summary                TEXT,
    created_at                          INTEGER NOT NULL,
    updated_at                          INTEGER NOT NULL,
    implementation_thread_recorded_at   INTEGER,
    implementation_turn_recorded_at     INTEGER,
    implementation_finished_at          INTEGER,
    git_commit_recorded_at              INTEGER,
    push_observed_at                    INTEGER,
    pull_request_recorded_at             INTEGER,
    verification_started_at             INTEGER,
    verification_thread_recorded_at      INTEGER,
    verification_turn_recorded_at        INTEGER,
    verification_finished_at             INTEGER,
    UNIQUE (obligation_id, lease_generation),
    CHECK (updated_at >= created_at),
    CHECK ((implementation_thread_id IS NULL) = (implementation_thread_recorded_at IS NULL)),
    CHECK ((implementation_turn_id IS NULL) = (implementation_turn_recorded_at IS NULL)),
    CHECK ((implementation_final_message_json IS NULL) = (implementation_finished_at IS NULL)),
    CHECK (implementation_final_message_json IS NULL OR (
               json_valid(implementation_final_message_json)
               AND json_type(implementation_final_message_json) = 'object'
           )),
    CHECK ((git_commit IS NULL) = (git_commit_recorded_at IS NULL)),
    CHECK ((pushed_head IS NULL) = (push_observed_at IS NULL)),
    CHECK (
        (pull_request_number IS NULL AND pull_request_url IS NULL
            AND pull_request_head IS NULL AND pull_request_recorded_at IS NULL)
        OR
        (pull_request_number IS NOT NULL AND pull_request_url IS NOT NULL
            AND pull_request_head IS NOT NULL AND pull_request_recorded_at IS NOT NULL)
    ),
    CHECK (
        (verification_worktree_path IS NULL AND verification_head IS NULL
            AND verification_started_at IS NULL)
        OR
        (verification_worktree_path IS NOT NULL AND verification_head IS NOT NULL
            AND verification_started_at IS NOT NULL)
    ),
    CHECK ((verification_thread_id IS NULL) = (verification_thread_recorded_at IS NULL)),
    CHECK ((verification_turn_id IS NULL) = (verification_turn_recorded_at IS NULL)),
    CHECK (
        (verification_verdict IS NULL AND verification_reported_head IS NULL
            AND verification_summary IS NULL AND verification_finished_at IS NULL)
        OR
        (verification_verdict IS NOT NULL AND verification_reported_head IS NOT NULL
            AND verification_summary IS NOT NULL AND verification_finished_at IS NOT NULL)
    ),
    CHECK (implementation_turn_id IS NULL OR implementation_thread_id IS NOT NULL),
    CHECK (implementation_final_message_json IS NULL OR implementation_turn_id IS NOT NULL),
    CHECK (git_commit IS NULL OR implementation_final_message_json IS NOT NULL),
    CHECK (pushed_head IS NULL OR git_commit IS NOT NULL),
    CHECK (pushed_head IS NULL OR pushed_head = git_commit),
    CHECK (pull_request_head IS NULL OR pushed_head IS NOT NULL),
    CHECK (pull_request_head IS NULL OR pull_request_head = git_commit),
    CHECK (verification_head IS NULL OR pull_request_head IS NOT NULL),
    CHECK (verification_head IS NULL OR verification_head = pull_request_head),
    CHECK (verification_worktree_path IS NULL
           OR verification_worktree_path <> implementation_worktree_path),
    CHECK (verification_thread_id IS NULL OR verification_head IS NOT NULL),
    CHECK (verification_thread_id IS NULL OR verification_thread_id <> implementation_thread_id),
    CHECK (verification_turn_id IS NULL OR verification_thread_id IS NOT NULL),
    CHECK (verification_turn_id IS NULL OR verification_turn_id <> implementation_turn_id),
    CHECK (verification_verdict IS NULL OR verification_turn_id IS NOT NULL),
    CHECK (verification_reported_head IS NULL OR verification_reported_head = verification_head),
    CHECK (implementation_thread_recorded_at IS NULL
           OR implementation_thread_recorded_at >= created_at),
    CHECK (implementation_turn_recorded_at IS NULL
           OR implementation_turn_recorded_at >= implementation_thread_recorded_at),
    CHECK (implementation_finished_at IS NULL
           OR implementation_finished_at >= implementation_turn_recorded_at),
    CHECK (git_commit_recorded_at IS NULL
           OR git_commit_recorded_at >= implementation_finished_at),
    CHECK (push_observed_at IS NULL OR push_observed_at >= git_commit_recorded_at),
    CHECK (pull_request_recorded_at IS NULL
           OR pull_request_recorded_at >= push_observed_at),
    CHECK (verification_started_at IS NULL
           OR verification_started_at >= pull_request_recorded_at),
    CHECK (verification_thread_recorded_at IS NULL
           OR verification_thread_recorded_at >= verification_started_at),
    CHECK (verification_turn_recorded_at IS NULL
           OR verification_turn_recorded_at >= verification_thread_recorded_at),
    CHECK (verification_finished_at IS NULL
           OR verification_finished_at >= verification_turn_recorded_at),
    CHECK (
        CASE phase
            WHEN 'created' THEN 0
            WHEN 'implementation_thread_recorded' THEN 1
            WHEN 'implementation_turn_recorded' THEN 2
            WHEN 'implementation_finished' THEN 3
            WHEN 'git_commit_recorded' THEN 4
            WHEN 'push_observed' THEN 5
            WHEN 'pull_request_ready' THEN 6
            WHEN 'verification_started' THEN 7
            WHEN 'verification_thread_recorded' THEN 8
            WHEN 'verification_turn_recorded' THEN 9
            WHEN 'verification_finished' THEN 10
        END = CASE
            WHEN verification_verdict IS NOT NULL THEN 10
            WHEN verification_turn_id IS NOT NULL THEN 9
            WHEN verification_thread_id IS NOT NULL THEN 8
            WHEN verification_head IS NOT NULL THEN 7
            WHEN pull_request_head IS NOT NULL THEN 6
            WHEN pushed_head IS NOT NULL THEN 5
            WHEN git_commit IS NOT NULL THEN 4
            WHEN implementation_final_message_json IS NOT NULL THEN 3
            WHEN implementation_turn_id IS NOT NULL THEN 2
            WHEN implementation_thread_id IS NOT NULL THEN 1
            ELSE 0
        END
    )
);

CREATE INDEX gardener_runs_obligation_idx
    ON gardener_implementation_runs(obligation_id, lease_generation);
CREATE INDEX gardener_runs_proposal_idx
    ON gardener_implementation_runs(proposal_fingerprint, created_at, id);

CREATE TABLE gardener_run_events (
    sequence     INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id       TEXT NOT NULL REFERENCES gardener_implementation_runs(id),
    event_type   TEXT NOT NULL,
    occurred_at  INTEGER NOT NULL,
    details_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(details_json))
);

CREATE INDEX gardener_run_events_run_idx
    ON gardener_run_events(run_id, sequence);

CREATE TRIGGER gardener_runs_identity_guard
BEFORE UPDATE ON gardener_implementation_runs
WHEN OLD.id IS NOT NEW.id
  OR OLD.repository IS NOT NEW.repository
  OR OLD.proposal_fingerprint IS NOT NEW.proposal_fingerprint
  OR OLD.obligation_id IS NOT NEW.obligation_id
  OR OLD.occurrence IS NOT NEW.occurrence
  OR OLD.attempt_number IS NOT NEW.attempt_number
  OR OLD.lease_generation IS NOT NEW.lease_generation
  OR OLD.lease_token IS NOT NEW.lease_token
  OR OLD.source_commit IS NOT NEW.source_commit
  OR OLD.implementation_worktree_path IS NOT NEW.implementation_worktree_path
  OR OLD.branch IS NOT NEW.branch
  OR OLD.created_at IS NOT NEW.created_at
  OR (OLD.implementation_thread_id IS NOT NULL
      AND OLD.implementation_thread_id IS NOT NEW.implementation_thread_id)
  OR (OLD.implementation_turn_id IS NOT NULL
      AND OLD.implementation_turn_id IS NOT NEW.implementation_turn_id)
  OR (OLD.implementation_final_message_json IS NOT NULL
      AND OLD.implementation_final_message_json IS NOT NEW.implementation_final_message_json)
  OR (OLD.git_commit IS NOT NULL AND OLD.git_commit IS NOT NEW.git_commit)
  OR (OLD.pushed_head IS NOT NULL AND OLD.pushed_head IS NOT NEW.pushed_head)
  OR (OLD.pull_request_number IS NOT NULL
      AND OLD.pull_request_number IS NOT NEW.pull_request_number)
  OR (OLD.pull_request_url IS NOT NULL AND OLD.pull_request_url IS NOT NEW.pull_request_url)
  OR (OLD.pull_request_head IS NOT NULL AND OLD.pull_request_head IS NOT NEW.pull_request_head)
  OR (OLD.verification_worktree_path IS NOT NULL
      AND OLD.verification_worktree_path IS NOT NEW.verification_worktree_path)
  OR (OLD.verification_head IS NOT NULL AND OLD.verification_head IS NOT NEW.verification_head)
  OR (OLD.verification_thread_id IS NOT NULL
      AND OLD.verification_thread_id IS NOT NEW.verification_thread_id)
  OR (OLD.verification_turn_id IS NOT NULL
      AND OLD.verification_turn_id IS NOT NEW.verification_turn_id)
  OR (OLD.verification_verdict IS NOT NULL
      AND OLD.verification_verdict IS NOT NEW.verification_verdict)
  OR (OLD.verification_reported_head IS NOT NULL
      AND OLD.verification_reported_head IS NOT NEW.verification_reported_head)
  OR (OLD.verification_summary IS NOT NULL
      AND OLD.verification_summary IS NOT NEW.verification_summary)
  OR (OLD.implementation_thread_recorded_at IS NOT NULL
      AND OLD.implementation_thread_recorded_at IS NOT NEW.implementation_thread_recorded_at)
  OR (OLD.implementation_turn_recorded_at IS NOT NULL
      AND OLD.implementation_turn_recorded_at IS NOT NEW.implementation_turn_recorded_at)
  OR (OLD.implementation_finished_at IS NOT NULL
      AND OLD.implementation_finished_at IS NOT NEW.implementation_finished_at)
  OR (OLD.git_commit_recorded_at IS NOT NULL
      AND OLD.git_commit_recorded_at IS NOT NEW.git_commit_recorded_at)
  OR (OLD.push_observed_at IS NOT NULL AND OLD.push_observed_at IS NOT NEW.push_observed_at)
  OR (OLD.pull_request_recorded_at IS NOT NULL
      AND OLD.pull_request_recorded_at IS NOT NEW.pull_request_recorded_at)
  OR (OLD.verification_started_at IS NOT NULL
      AND OLD.verification_started_at IS NOT NEW.verification_started_at)
  OR (OLD.verification_thread_recorded_at IS NOT NULL
      AND OLD.verification_thread_recorded_at IS NOT NEW.verification_thread_recorded_at)
  OR (OLD.verification_turn_recorded_at IS NOT NULL
      AND OLD.verification_turn_recorded_at IS NOT NEW.verification_turn_recorded_at)
  OR (OLD.verification_finished_at IS NOT NULL
      AND OLD.verification_finished_at IS NOT NEW.verification_finished_at)
BEGIN
    SELECT RAISE(ABORT, 'gardener run identities and evidence are write-once');
END;

CREATE TRIGGER gardener_runs_phase_guard
BEFORE UPDATE ON gardener_implementation_runs
WHEN (OLD.phase = 'created' AND NEW.phase <> 'implementation_thread_recorded')
  OR (OLD.phase = 'implementation_thread_recorded' AND NEW.phase <> 'implementation_turn_recorded')
  OR (OLD.phase = 'implementation_turn_recorded' AND NEW.phase <> 'implementation_finished')
  OR (OLD.phase = 'implementation_finished' AND NEW.phase <> 'git_commit_recorded')
  OR (OLD.phase = 'git_commit_recorded' AND NEW.phase <> 'push_observed')
  OR (OLD.phase = 'push_observed' AND NEW.phase <> 'pull_request_ready')
  OR (OLD.phase = 'pull_request_ready' AND NEW.phase <> 'verification_started')
  OR (OLD.phase = 'verification_started' AND NEW.phase <> 'verification_thread_recorded')
  OR (OLD.phase = 'verification_thread_recorded' AND NEW.phase <> 'verification_turn_recorded')
  OR (OLD.phase = 'verification_turn_recorded' AND NEW.phase <> 'verification_finished')
  OR OLD.phase = 'verification_finished'
BEGIN
    SELECT RAISE(ABORT, 'gardener run phase transition is out of order');
END;

CREATE TRIGGER gardener_runs_no_delete
BEFORE DELETE ON gardener_implementation_runs
BEGIN
    SELECT RAISE(ABORT, 'gardener runs are durable');
END;

CREATE TRIGGER gardener_run_events_no_update
BEFORE UPDATE ON gardener_run_events
BEGIN
    SELECT RAISE(ABORT, 'gardener run events are append-only');
END;

CREATE TRIGGER gardener_run_events_no_delete
BEFORE DELETE ON gardener_run_events
BEGIN
    SELECT RAISE(ABORT, 'gardener run events are append-only');
END;

-- One immutable dispatch owns an admitted managed workspace occurrence.
CREATE TABLE workspace_executions (
    execution_id TEXT PRIMARY KEY,
    obligation_id TEXT NOT NULL UNIQUE REFERENCES managed_bindings(obligation_id),
    host_id TEXT NOT NULL,
    dispatch_json TEXT NOT NULL CHECK(json_valid(dispatch_json)),
    status TEXT NOT NULL CHECK(status IN ('dispatching','running','waiting','attention','cancelling','stopped','completed','cancelled')),
    progress TEXT NOT NULL DEFAULT '',
    last_event_sequence INTEGER NOT NULL DEFAULT 0 CHECK(last_event_sequence >= 0),
    cancellation_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancellation_requested IN (0,1)),
    cessation_verified INTEGER NOT NULL DEFAULT 0 CHECK(cessation_verified IN (0,1)),
    result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
    admitted_at INTEGER NOT NULL,
    deadline_at INTEGER NOT NULL CHECK(deadline_at > admitted_at)
);
CREATE INDEX workspace_execution_host ON workspace_executions(host_id,execution_id);
CREATE TABLE workspace_execution_events (
    execution_id TEXT NOT NULL REFERENCES workspace_executions(execution_id),
    sequence INTEGER NOT NULL CHECK(sequence > 0),
    event_json TEXT NOT NULL CHECK(json_valid(event_json)),
    observed_at INTEGER NOT NULL,
    PRIMARY KEY(execution_id,sequence)
);
CREATE TABLE workspace_execution_questions (
    execution_id TEXT NOT NULL REFERENCES workspace_executions(execution_id),
    question_id TEXT NOT NULL,
    question_json TEXT NOT NULL CHECK(json_valid(question_json)),
    sequence INTEGER NOT NULL,
    PRIMARY KEY(execution_id,question_id)
);
CREATE TABLE workspace_execution_answers (
    execution_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    answer_json TEXT NOT NULL CHECK(json_valid(answer_json)),
    answered_at INTEGER NOT NULL,
    PRIMARY KEY(execution_id,question_id),
    FOREIGN KEY(execution_id,question_id) REFERENCES workspace_execution_questions(execution_id,question_id)
);
CREATE TABLE workspace_execution_commands (
    command_id TEXT PRIMARY KEY,
    request_json TEXT NOT NULL CHECK(json_valid(request_json))
);
CREATE TRIGGER workspace_dispatch_immutable BEFORE UPDATE OF execution_id,obligation_id,host_id,dispatch_json,admitted_at,deadline_at ON workspace_executions
BEGIN SELECT RAISE(ABORT,'workspace dispatch is immutable'); END;
CREATE TRIGGER workspace_cancellation_monotonic BEFORE UPDATE OF cancellation_requested ON workspace_executions
WHEN OLD.cancellation_requested=1 AND NEW.cancellation_requested=0
BEGIN SELECT RAISE(ABORT,'workspace cancellation is monotonic'); END;
CREATE TRIGGER workspace_cessation_monotonic BEFORE UPDATE OF cessation_verified ON workspace_executions
WHEN OLD.cessation_verified=1 AND NEW.cessation_verified=0
BEGIN SELECT RAISE(ABORT,'workspace cessation is retained'); END;
CREATE TRIGGER workspace_stopped_result_immutable BEFORE UPDATE OF result_json ON workspace_executions
WHEN OLD.cessation_verified=1 AND NEW.result_json IS NOT OLD.result_json
BEGIN SELECT RAISE(ABORT,'stopped workspace result is immutable'); END;
CREATE TRIGGER workspace_execution_no_delete BEFORE DELETE ON workspace_executions
BEGIN SELECT RAISE(ABORT,'workspace executions are retained'); END;
CREATE TRIGGER workspace_event_no_update BEFORE UPDATE ON workspace_execution_events
BEGIN SELECT RAISE(ABORT,'workspace events are immutable'); END;
CREATE TRIGGER workspace_event_no_delete BEFORE DELETE ON workspace_execution_events
BEGIN SELECT RAISE(ABORT,'workspace events are immutable'); END;
CREATE TRIGGER workspace_question_no_update BEFORE UPDATE ON workspace_execution_questions
BEGIN SELECT RAISE(ABORT,'workspace questions are immutable'); END;
CREATE TRIGGER workspace_question_no_delete BEFORE DELETE ON workspace_execution_questions
BEGIN SELECT RAISE(ABORT,'workspace questions are immutable'); END;
CREATE TRIGGER workspace_answer_no_update BEFORE UPDATE ON workspace_execution_answers
BEGIN SELECT RAISE(ABORT,'workspace answers are immutable'); END;
CREATE TRIGGER workspace_answer_no_delete BEFORE DELETE ON workspace_execution_answers
BEGIN SELECT RAISE(ABORT,'workspace answers are immutable'); END;
CREATE TRIGGER workspace_execution_command_no_update BEFORE UPDATE ON workspace_execution_commands
BEGIN SELECT RAISE(ABORT,'workspace commands are immutable'); END;
CREATE TRIGGER workspace_execution_command_no_delete BEFORE DELETE ON workspace_execution_commands
BEGIN SELECT RAISE(ABORT,'workspace commands are immutable'); END;

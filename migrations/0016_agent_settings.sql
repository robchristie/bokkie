CREATE TABLE agent_profile_revisions (
    revision INTEGER PRIMARY KEY,
    profile_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE agent_profile_active (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    revision INTEGER NOT NULL REFERENCES agent_profile_revisions(revision)
);
CREATE TABLE agent_settings_commands (
    command_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    revision INTEGER NOT NULL REFERENCES agent_profile_revisions(revision)
);
ALTER TABLE conversation_requests ADD COLUMN accepted_profile_json TEXT;
CREATE TRIGGER agent_profile_no_update BEFORE UPDATE ON agent_profile_revisions BEGIN SELECT RAISE(ABORT,'agent profiles are immutable'); END;
CREATE TRIGGER agent_profile_no_delete BEFORE DELETE ON agent_profile_revisions BEGIN SELECT RAISE(ABORT,'agent profiles are immutable'); END;
CREATE TRIGGER agent_settings_commands_no_update BEFORE UPDATE ON agent_settings_commands BEGIN SELECT RAISE(ABORT,'settings receipts are immutable'); END;
CREATE TRIGGER agent_settings_commands_no_delete BEFORE DELETE ON agent_settings_commands BEGIN SELECT RAISE(ABORT,'settings receipts are immutable'); END;
CREATE TRIGGER conversation_profile_no_update BEFORE UPDATE OF accepted_profile_json ON conversation_requests WHEN OLD.accepted_profile_json IS NOT NULL BEGIN SELECT RAISE(ABORT,'accepted profiles are immutable'); END;
CREATE TABLE conversation_invocations (
    request_id TEXT NOT NULL REFERENCES conversation_requests(command_id),
    ordinal INTEGER NOT NULL CHECK(ordinal>=0 AND ordinal<4),
    purpose TEXT NOT NULL,
    profile_revision INTEGER,
    status TEXT NOT NULL CHECK(status IN ('dispatched','completed','failed','interrupted')),
    outcome_json TEXT,
    dispatched_at INTEGER NOT NULL,
    PRIMARY KEY(request_id,ordinal)
);

CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL DEFAULT 0,
    selected_task_id TEXT,
    candidates_json TEXT NOT NULL DEFAULT '[]',
    proposal_id TEXT,
    receipt_json TEXT,
    updated_at INTEGER NOT NULL
);
CREATE TABLE conversation_messages (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    request_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('user','assistant','system')),
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(conversation_id,request_id,role)
);
CREATE INDEX conversation_messages_recent ON conversation_messages(conversation_id,sequence);
CREATE TABLE conversation_requests (
    command_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    payload_json TEXT NOT NULL,
    session_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','complete','failed','interrupted')),
    output_json TEXT,
    error TEXT,
    created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX conversation_one_request ON conversation_requests(conversation_id) WHERE status='running';
CREATE TABLE conversation_proposals (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    review_json TEXT NOT NULL,
    profiles_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE conversation_commands (
    command_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    result_json TEXT NOT NULL
);
CREATE TRIGGER conversation_messages_no_update BEFORE UPDATE ON conversation_messages BEGIN SELECT RAISE(ABORT,'conversation messages are immutable'); END;
CREATE TRIGGER conversation_messages_no_delete BEFORE DELETE ON conversation_messages BEGIN SELECT RAISE(ABORT,'conversation messages are immutable'); END;
CREATE TRIGGER conversation_proposals_no_update BEFORE UPDATE ON conversation_proposals BEGIN SELECT RAISE(ABORT,'conversation proposals are immutable'); END;
CREATE TRIGGER conversation_proposals_no_delete BEFORE DELETE ON conversation_proposals BEGIN SELECT RAISE(ABORT,'conversation proposals are immutable'); END;
CREATE TRIGGER conversation_commands_no_update BEFORE UPDATE ON conversation_commands BEGIN SELECT RAISE(ABORT,'conversation receipts are immutable'); END;
CREATE TRIGGER conversation_commands_no_delete BEFORE DELETE ON conversation_commands BEGIN SELECT RAISE(ABORT,'conversation receipts are immutable'); END;

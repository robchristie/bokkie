CREATE TABLE workspace_projects (
    id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision > 0),
    registration_json TEXT NOT NULL
);
CREATE TABLE workspace_handoff_drafts (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    source_request_id TEXT NOT NULL UNIQUE REFERENCES conversation_requests(command_id),
    project_query TEXT NOT NULL,
    brief_json TEXT NOT NULL
);
ALTER TABLE conversations ADD COLUMN handoff_draft_id TEXT REFERENCES workspace_handoff_drafts(id);
CREATE TABLE workspace_handoff_snapshots (
    handoff_id TEXT NOT NULL REFERENCES workspace_handoff_drafts(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    snapshot_json TEXT NOT NULL,
    PRIMARY KEY(handoff_id, revision)
);
CREATE TABLE workspace_handoff_activities (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    handoff_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    activity_json TEXT NOT NULL,
    FOREIGN KEY(handoff_id, revision) REFERENCES workspace_handoff_snapshots(handoff_id, revision)
);
CREATE INDEX workspace_handoff_activity_order ON workspace_handoff_activities(handoff_id,revision,sequence);
CREATE TABLE workspace_handoff_commands (
    command_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    result_json TEXT NOT NULL
);
CREATE TRIGGER workspace_draft_no_update BEFORE UPDATE ON workspace_handoff_drafts BEGIN SELECT RAISE(ABORT,'hand-off drafts are immutable'); END;
CREATE TRIGGER workspace_draft_no_delete BEFORE DELETE ON workspace_handoff_drafts BEGIN SELECT RAISE(ABORT,'hand-off drafts are immutable'); END;
CREATE TRIGGER workspace_snapshot_no_update BEFORE UPDATE ON workspace_handoff_snapshots BEGIN SELECT RAISE(ABORT,'hand-off snapshots are immutable'); END;
CREATE TRIGGER workspace_snapshot_no_delete BEFORE DELETE ON workspace_handoff_snapshots BEGIN SELECT RAISE(ABORT,'hand-off snapshots are immutable'); END;
CREATE TRIGGER workspace_activity_no_update BEFORE UPDATE ON workspace_handoff_activities BEGIN SELECT RAISE(ABORT,'hand-off activities are immutable'); END;
CREATE TRIGGER workspace_activity_no_delete BEFORE DELETE ON workspace_handoff_activities BEGIN SELECT RAISE(ABORT,'hand-off activities are immutable'); END;
CREATE TRIGGER workspace_command_no_update BEFORE UPDATE ON workspace_handoff_commands BEGIN SELECT RAISE(ABORT,'hand-off receipts are immutable'); END;
CREATE TRIGGER workspace_command_no_delete BEFORE DELETE ON workspace_handoff_commands BEGIN SELECT RAISE(ABORT,'hand-off receipts are immutable'); END;

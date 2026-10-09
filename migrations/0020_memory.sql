-- Mutable recall projection. Sources survive correction and removal; task history is separate.
CREATE TABLE memory_entries (
    entry_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK(revision >= 1),
    kind TEXT NOT NULL CHECK(kind IN ('preference','task_outcome','decision','operational_knowledge')),
    provenance TEXT NOT NULL CHECK(provenance IN ('explicit','inferred')),
    content TEXT,
    sources_json TEXT NOT NULL CHECK(json_valid(sources_json)),
    task_id TEXT,
    source_key TEXT UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed INTEGER NOT NULL DEFAULT 0 CHECK(removed IN (0,1)),
    CHECK((removed=0 AND content IS NOT NULL) OR (removed=1 AND content IS NULL))
);
CREATE INDEX memory_task ON memory_entries(task_id,entry_id);
CREATE TABLE memory_commands (
    command_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    result_json TEXT NOT NULL CHECK(json_valid(result_json))
);
CREATE TRIGGER memory_sources_retained BEFORE UPDATE OF entry_id,kind,provenance,sources_json,task_id,source_key,created_at ON memory_entries
BEGIN SELECT RAISE(ABORT,'memory sources are retained'); END;
CREATE TRIGGER memory_removal_retained BEFORE UPDATE ON memory_entries WHEN OLD.removed=1
BEGIN SELECT RAISE(ABORT,'removed memory cannot regenerate'); END;
CREATE TRIGGER memory_no_delete BEFORE DELETE ON memory_entries
BEGIN SELECT RAISE(ABORT,'memory source tombstones are retained'); END;
CREATE TRIGGER memory_commands_no_update BEFORE UPDATE ON memory_commands
BEGIN SELECT RAISE(ABORT,'memory receipts are immutable'); END;
CREATE TRIGGER memory_commands_no_delete BEFORE DELETE ON memory_commands
BEGIN SELECT RAISE(ABORT,'memory receipts are immutable'); END;

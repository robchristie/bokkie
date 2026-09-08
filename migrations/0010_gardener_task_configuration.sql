-- Registration remains immutable. Only task-specific inspection guidance changes.
CREATE TABLE gardener_task_settings (
    obligation_id TEXT PRIMARY KEY REFERENCES gardener_repositories(inspection_obligation_id),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    instruction_mode TEXT NOT NULL DEFAULT 'extend' CHECK (instruction_mode IN ('extend', 'replace')),
    instructions TEXT NOT NULL DEFAULT '' CHECK (length(instructions) <= 6000)
);
INSERT INTO gardener_task_settings(obligation_id)
SELECT inspection_obligation_id FROM gardener_repositories;

CREATE TRIGGER gardener_task_settings_revision_guard
BEFORE UPDATE ON gardener_task_settings
WHEN OLD.obligation_id IS NOT NEW.obligation_id OR NEW.revision != OLD.revision + 1
BEGIN
    SELECT RAISE(ABORT, 'task settings updates require the next revision');
END;
CREATE TRIGGER gardener_task_settings_no_delete
BEFORE DELETE ON gardener_task_settings
BEGIN
    SELECT RAISE(ABORT, 'task settings cannot be deleted');
END;

-- Historical inspections deliberately have no invented configuration evidence.
ALTER TABLE gardener_inspections ADD COLUMN configuration_json TEXT
    CHECK (configuration_json IS NULL OR json_valid(configuration_json));
CREATE TRIGGER gardener_inspection_configuration_guard
BEFORE UPDATE OF configuration_json ON gardener_inspections
WHEN OLD.configuration_json IS NOT NEW.configuration_json
BEGIN
    SELECT RAISE(ABORT, 'inspection configuration snapshots are immutable');
END;

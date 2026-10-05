-- Each immutable reminder result owns one independent delivery obligation.
CREATE TABLE notification_deliveries (
    id TEXT PRIMARY KEY REFERENCES obligations(id),
    task_id TEXT NOT NULL REFERENCES managed_tasks(id),
    source_obligation_id TEXT NOT NULL UNIQUE REFERENCES managed_results(obligation_id),
    destination TEXT NOT NULL,
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    message_id TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    dispatch_started_at INTEGER,
    reconciled_at INTEGER
);
CREATE INDEX notification_deliveries_task ON notification_deliveries(task_id, id);
CREATE TRIGGER notification_payload_no_update BEFORE UPDATE ON notification_deliveries
WHEN NEW.id IS NOT OLD.id OR NEW.task_id IS NOT OLD.task_id
 OR NEW.source_obligation_id IS NOT OLD.source_obligation_id
 OR NEW.destination IS NOT OLD.destination OR NEW.subject IS NOT OLD.subject
 OR NEW.body IS NOT OLD.body OR NEW.message_id IS NOT OLD.message_id
 OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'notification intent is immutable'); END;
CREATE TRIGGER notification_deliveries_no_delete BEFORE DELETE ON notification_deliveries
BEGIN SELECT RAISE(ABORT, 'notification intents cannot be deleted'); END;

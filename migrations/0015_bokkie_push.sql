-- One explicitly selected device; immutable generations retain admitted work.
CREATE TABLE push_devices (
 id TEXT PRIMARY KEY,
 label TEXT NOT NULL,
 endpoint TEXT NOT NULL,
 p256dh TEXT NOT NULL,
 auth TEXT NOT NULL,
 vapid_public_key TEXT NOT NULL,
 ttl_seconds INTEGER NOT NULL,
 created_at INTEGER NOT NULL
);
CREATE TRIGGER push_device_no_update BEFORE UPDATE ON push_devices
BEGIN SELECT RAISE(ABORT, 'push subscription generations are immutable'); END;
CREATE TRIGGER push_device_no_delete BEFORE DELETE ON push_devices
BEGIN SELECT RAISE(ABORT, 'push subscription history cannot be deleted'); END;
CREATE TABLE push_configuration (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 revision INTEGER NOT NULL,
 device_id TEXT REFERENCES push_devices(id),
 active INTEGER NOT NULL CHECK(active IN (0,1))
);
INSERT INTO push_configuration VALUES(1,0,NULL,0);
CREATE TABLE push_commands (
 id TEXT PRIMARY KEY,
 request_hash TEXT NOT NULL,
 result_json TEXT NOT NULL
);
CREATE TRIGGER push_command_no_update BEFORE UPDATE ON push_commands
BEGIN SELECT RAISE(ABORT, 'push configuration receipts are immutable'); END;
CREATE TRIGGER push_command_no_delete BEFORE DELETE ON push_commands
BEGIN SELECT RAISE(ABORT, 'push configuration receipts cannot be deleted'); END;
CREATE TABLE push_deliveries (
 id TEXT PRIMARY KEY REFERENCES notification_deliveries(id),
 device_id TEXT NOT NULL REFERENCES push_devices(id),
 expires_at INTEGER NOT NULL,
 receipt_hash TEXT NOT NULL,
 accepted_ttl_seconds INTEGER,
 device_status TEXT CHECK(device_status IN ('displayed','opened','expired')),
 device_reported_at INTEGER
);
CREATE TRIGGER push_delivery_payload_no_update BEFORE UPDATE ON push_deliveries
WHEN NEW.id IS NOT OLD.id OR NEW.device_id IS NOT OLD.device_id
 OR NEW.expires_at IS NOT OLD.expires_at OR NEW.receipt_hash IS NOT OLD.receipt_hash
BEGIN SELECT RAISE(ABORT, 'push delivery intent is immutable'); END;
CREATE TRIGGER push_delivery_no_delete BEFORE DELETE ON push_deliveries
BEGIN SELECT RAISE(ABORT, 'push delivery history cannot be deleted'); END;

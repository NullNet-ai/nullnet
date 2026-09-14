CREATE TABLE service_trigger_peers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    peer TEXT NOT NULL,
    UNIQUE (service_id, peer)
);
INSERT INTO service_trigger_peers (id, service_id, peer)
    SELECT MIN(id), service_id, peer FROM service_triggers GROUP BY service_id, peer;
DROP TABLE service_triggers;
ALTER TABLE service_trigger_peers RENAME TO service_triggers;

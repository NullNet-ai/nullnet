CREATE TABLE service_triggers_with_ports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    port INTEGER NOT NULL,
    peer TEXT NOT NULL,
    UNIQUE (service_id, port)
);
INSERT INTO service_triggers_with_ports (id, service_id, port, peer)
    SELECT t.id, t.service_id, p.port, t.peer FROM service_triggers t
    JOIN services s ON s.id = t.service_id
    JOIN services p ON p.stack = s.stack AND p.name = t.peer;
DROP TABLE service_triggers;
ALTER TABLE service_triggers_with_ports RENAME TO service_triggers;

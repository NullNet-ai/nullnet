CREATE TABLE events (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    kind      TEXT NOT NULL,
    severity  TEXT NOT NULL,
    timestamp BIGINT NOT NULL,
    payload   TEXT NOT NULL
);

CREATE INDEX events_timestamp_idx ON events (timestamp);
CREATE INDEX events_kind_idx ON events (kind);
CREATE INDEX events_severity_idx ON events (severity);

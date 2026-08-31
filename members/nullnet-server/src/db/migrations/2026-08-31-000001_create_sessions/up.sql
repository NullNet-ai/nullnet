CREATE TABLE sessions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    direction    TEXT NOT NULL,
    stack        TEXT NOT NULL,
    service      TEXT NOT NULL,
    net_id       INTEGER NOT NULL,
    peer_ip      TEXT NOT NULL,
    country_code TEXT,
    asn          TEXT,
    org          TEXT,
    blocked      BOOL NOT NULL DEFAULT 0,
    detail       TEXT NOT NULL,
    started_at   BIGINT NOT NULL,
    last_seen    BIGINT NOT NULL,
    ended_at     BIGINT
);

CREATE INDEX sessions_started_at_idx ON sessions (started_at);
CREATE INDEX sessions_direction_idx ON sessions (direction);
CREATE INDEX sessions_stack_service_idx ON sessions (stack, service);
-- Every open/touch/close matches on the open rows only; partial index keeps
-- that lookup independent of how much closed history has piled up.
CREATE INDEX sessions_open_idx ON sessions (direction, net_id, peer_ip) WHERE ended_at IS NULL;

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
-- Every open/touch/close matches on the open rows only, so the partial index
-- keeps that lookup independent of how much closed history has piled up. UNIQUE
-- because net ids are recycled: at most one row per session may be open at a
-- time, or a later generation's reports would land on a predecessor's row.
-- Closed rows are deliberately outside the constraint — successive generations
-- of the same net id are supposed to accumulate as separate history.
CREATE UNIQUE INDEX sessions_open_idx
    ON sessions (direction, net_id, service, peer_ip) WHERE ended_at IS NULL;

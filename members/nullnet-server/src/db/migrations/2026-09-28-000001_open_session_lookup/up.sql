-- The non-backend uniqueness predicate does not cover parameterized lookups.
CREATE INDEX sessions_open_edge_idx
    ON sessions (net_id, direction, peer_ip, service)
    WHERE ended_at IS NULL;

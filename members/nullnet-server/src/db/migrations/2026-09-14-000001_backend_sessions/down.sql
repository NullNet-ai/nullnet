DELETE FROM sessions WHERE direction = 'backend';
DROP INDEX sessions_open_idx;
CREATE UNIQUE INDEX sessions_open_idx
    ON sessions (direction, net_id, service, peer_ip) WHERE ended_at IS NULL;

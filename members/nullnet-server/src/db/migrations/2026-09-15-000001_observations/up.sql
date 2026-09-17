CREATE TABLE observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    stack TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE,
    started_at BIGINT NOT NULL,
    ended_at BIGINT,
    applied BOOL NOT NULL DEFAULT 0,
    saved_config TEXT NOT NULL
);
CREATE UNIQUE INDEX observations_active_stack ON observations(stack) WHERE ended_at IS NULL;

CREATE TABLE observation_counts (
    observation_id BIGINT NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    destination TEXT NOT NULL,
    count BIGINT NOT NULL,
    PRIMARY KEY (observation_id, source, destination)
);

-- Include backend sessions already open at activation.
CREATE TRIGGER observation_seed AFTER INSERT ON observations BEGIN
    INSERT INTO observation_counts (observation_id, source, destination, count)
    SELECT NEW.id, service, peer_ip, COUNT(*) FROM sessions
    WHERE stack = NEW.stack AND direction = 'backend' AND blocked = 0
        AND ended_at IS NULL AND service != peer_ip
    GROUP BY service, peer_ip;
END;

-- Session recording and counting succeed together; pruning never changes counts.
CREATE TRIGGER observation_backend_session AFTER INSERT ON sessions
WHEN NEW.direction = 'backend' AND NEW.blocked = 0 AND NEW.service != NEW.peer_ip BEGIN
    INSERT INTO observation_counts (observation_id, source, destination, count)
    SELECT id, NEW.service, NEW.peer_ip, 1 FROM observations
    WHERE stack = NEW.stack AND ended_at IS NULL
    ON CONFLICT (observation_id, source, destination) DO UPDATE SET count = count + 1;
END;

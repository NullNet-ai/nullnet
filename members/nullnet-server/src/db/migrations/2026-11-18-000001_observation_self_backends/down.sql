DROP TRIGGER observation_backend_session;
DROP TRIGGER observation_seed;

CREATE TRIGGER observation_seed AFTER INSERT ON observations BEGIN
    INSERT INTO observation_counts (observation_id, source, destination, count)
    SELECT NEW.id, service, peer_ip, COUNT(*) FROM sessions
    WHERE stack = NEW.stack AND direction = 'backend' AND blocked = 0
        AND ended_at IS NULL AND service != peer_ip
    GROUP BY service, peer_ip;
END;

CREATE TRIGGER observation_backend_session AFTER INSERT ON sessions
WHEN NEW.direction = 'backend' AND NEW.blocked = 0 AND NEW.service != NEW.peer_ip BEGIN
    INSERT INTO observation_counts (observation_id, source, destination, count)
    SELECT id, NEW.service, NEW.peer_ip, 1 FROM observations
    WHERE stack = NEW.stack AND ended_at IS NULL
    ON CONFLICT (observation_id, source, destination) DO UPDATE SET count = count + 1;
END;

CREATE INDEX sessions_active_counts_idx ON sessions (stack, service, direction) WHERE ended_at IS NULL;

CREATE TABLE session_policy_counts (
    stack TEXT NOT NULL,
    direction TEXT NOT NULL,
    blocked BOOL NOT NULL,
    session_count BIGINT NOT NULL,
    PRIMARY KEY (stack, direction, blocked)
);

INSERT INTO session_policy_counts (stack, direction, blocked, session_count)
SELECT stack, direction, blocked, COUNT(*) FROM sessions
WHERE direction <> 'backend' GROUP BY stack, direction, blocked;

CREATE TRIGGER session_policy_insert AFTER INSERT ON sessions
WHEN NEW.direction <> 'backend'
BEGIN
    INSERT INTO session_policy_counts (stack, direction, blocked, session_count)
    VALUES (NEW.stack, NEW.direction, NEW.blocked, 1)
    ON CONFLICT (stack, direction, blocked) DO UPDATE SET session_count = session_count + 1;
END;

CREATE TRIGGER session_policy_delete AFTER DELETE ON sessions
WHEN OLD.direction <> 'backend'
BEGIN
    UPDATE session_policy_counts SET session_count = session_count - 1
    WHERE stack = OLD.stack AND direction = OLD.direction AND blocked = OLD.blocked;
END;

CREATE TRIGGER session_policy_update AFTER UPDATE OF stack, direction, blocked ON sessions
WHEN OLD.stack <> NEW.stack OR OLD.direction <> NEW.direction OR OLD.blocked <> NEW.blocked
BEGIN
    UPDATE session_policy_counts SET session_count = session_count - 1
    WHERE stack = OLD.stack AND direction = OLD.direction AND blocked = OLD.blocked
      AND OLD.direction <> 'backend';
    INSERT INTO session_policy_counts (stack, direction, blocked, session_count)
    SELECT NEW.stack, NEW.direction, NEW.blocked, 1 WHERE NEW.direction <> 'backend'
    ON CONFLICT (stack, direction, blocked) DO UPDATE SET session_count = session_count + 1;
END;

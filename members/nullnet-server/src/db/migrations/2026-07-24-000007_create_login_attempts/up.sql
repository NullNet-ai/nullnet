CREATE TABLE login_attempts (
    username     TEXT NOT NULL PRIMARY KEY,
    failed_count INTEGER NOT NULL,
    locked_until BIGINT,
    updated_at   BIGINT NOT NULL
);

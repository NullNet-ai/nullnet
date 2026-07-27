CREATE TABLE user_scopes (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    scope   TEXT NOT NULL,
    PRIMARY KEY (user_id, scope)
);

CREATE TABLE users (
    id               TEXT NOT NULL PRIMARY KEY,
    username         TEXT NOT NULL UNIQUE,
    password_hash    TEXT NOT NULL,
    role             TEXT NOT NULL,
    mfa_secret_enc   TEXT,
    mfa_confirmed_at BIGINT,
    created_at       BIGINT NOT NULL,
    updated_at       BIGINT NOT NULL
);

CREATE TABLE certificates (
    domain        TEXT NOT NULL PRIMARY KEY,
    fullchain_pem TEXT NOT NULL,
    key_pem_enc   TEXT NOT NULL,
    not_after     BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL
);

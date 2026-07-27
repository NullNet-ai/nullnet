CREATE TABLE services (
    stack        TEXT NOT NULL PRIMARY KEY,
    service_json TEXT NOT NULL,
    updated_at   BIGINT NOT NULL
);

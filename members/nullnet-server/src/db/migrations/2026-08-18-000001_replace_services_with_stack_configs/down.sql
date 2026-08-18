DROP TABLE routes;
DROP TABLE service_dependencies;
DROP TABLE service_triggers;
DROP TABLE services;
DROP TABLE stacks;

CREATE TABLE services (
    stack        TEXT NOT NULL PRIMARY KEY,
    service_json TEXT NOT NULL,
    updated_at   BIGINT NOT NULL
);

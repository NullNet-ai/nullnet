-- `services` was created ahead of need and never wired up (no code path ever
-- wrote to it), so dropping it is safe. Issue #140 moves stack service
-- config here as real, normalized tables — one stack's full config
-- (services + their triggers/dependency branches + routes) is the atomic
-- unit the app always loads/saves as a whole, but each part gets its own
-- table so editing one never risks the others.
DROP TABLE services;

CREATE TABLE stacks (
    name       TEXT NOT NULL PRIMARY KEY,
    updated_at BIGINT NOT NULL
);

CREATE TABLE services (
    id                         INTEGER PRIMARY KEY AUTOINCREMENT,
    stack                      TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE,
    name                       TEXT NOT NULL,
    docker_container           TEXT,
    process_path               TEXT,
    port                       INTEGER,
    timeout                    BIGINT,
    max_networks               INTEGER,
    protocol                   TEXT,
    listen_port                INTEGER,
    egress_blocked_countries   TEXT,
    egress_allowed_countries   TEXT,
    ingress_blocked_countries  TEXT,
    ingress_allowed_countries  TEXT,
    UNIQUE (stack, name)
);

-- Backend-triggered chains: one row per `[[services.triggers]]` entry.
-- `chain` is a JSON-encoded ordered list of service names — never queried
-- element-wise, so not worth its own table.
CREATE TABLE service_triggers (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    port       INTEGER NOT NULL,
    chain      TEXT NOT NULL,
    UNIQUE (service_id, port)
);

-- Proxy-triggered dependency branches: one row per independent branch of
-- `proxy_dependencies`. `branch_index` preserves branch order; `chain` is
-- a JSON-encoded ordered list, same rationale as `service_triggers.chain`.
CREATE TABLE service_dependencies (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    service_id   INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    branch_index INTEGER NOT NULL,
    chain        TEXT NOT NULL
);

CREATE TABLE routes (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    stack            TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE,
    host             TEXT NOT NULL,
    path             TEXT NOT NULL,
    target_kind      TEXT NOT NULL,
    target_service   TEXT,
    strip_prefix     BOOLEAN NOT NULL DEFAULT 0,
    redirect_to      TEXT,
    redirect_status  INTEGER,
    preserve_path    BOOLEAN NOT NULL DEFAULT 0,
    preserve_query   BOOLEAN NOT NULL DEFAULT 0,
    UNIQUE (stack, host, path)
);

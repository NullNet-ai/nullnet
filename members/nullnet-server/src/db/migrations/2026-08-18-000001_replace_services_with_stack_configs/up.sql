-- `services` was created ahead of need and never wired up (no code path ever
-- wrote to it), so dropping it is safe. `stack_configs` replaces it as the
-- real config store: one row per stack, holding its raw stack TOML text —
-- issue #140 moves `./services/<stack>.toml` files here.
DROP TABLE services;

CREATE TABLE stack_configs (
    stack       TEXT NOT NULL PRIMARY KEY,
    config_toml TEXT NOT NULL,
    updated_at  BIGINT NOT NULL
);

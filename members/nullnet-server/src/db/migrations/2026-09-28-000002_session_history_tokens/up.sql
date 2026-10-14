ALTER TABLE sessions ADD COLUMN history_token TEXT;
CREATE UNIQUE INDEX sessions_history_token_idx ON sessions (history_token);

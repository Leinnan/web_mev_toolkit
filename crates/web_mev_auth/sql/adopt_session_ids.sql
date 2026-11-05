-- For an app whose `sessions` table uses `token_hash` as its primary key, as
-- klucznik and oxi_feed do. Copy this into one of the app's migrations. Existing
-- sessions keep working, so nobody is logged out.
ALTER TABLE sessions RENAME TO sessions_before_ids;
DROP INDEX IF EXISTS idx_sessions_user;
DROP INDEX IF EXISTS sessions_user;

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_hash TEXT NOT NULL UNIQUE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_token TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX sessions_user_idx ON sessions(user_id);
CREATE INDEX sessions_expiry_idx ON sessions(expires_at);

INSERT INTO sessions (token_hash, user_id, csrf_token, expires_at, created_at)
SELECT token_hash, user_id, csrf_token, expires_at, created_at FROM sessions_before_ids;

DROP TABLE sessions_before_ids;

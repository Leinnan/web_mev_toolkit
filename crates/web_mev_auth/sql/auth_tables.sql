-- The tables web_mev_auth reads and writes. Copy this into one of the app's own
-- migrations, after the one creating `users`. The crate checks them at boot with
-- `verify_schema`, and never runs migrations of its own.

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- SHA-256 of the cookie's token. The token itself is never stored.
    token_hash TEXT NOT NULL UNIQUE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_token TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX sessions_user_idx ON sessions(user_id);
CREATE INDEX sessions_expiry_idx ON sessions(expires_at);

CREATE TABLE login_throttle (
    -- SHA-256 of the normalized login name, so the raw name never rests here.
    identity_hash TEXT PRIMARY KEY,
    failures INTEGER NOT NULL,
    locked_until INTEGER,
    updated_at INTEGER NOT NULL
);

-- A single-row guard that makes first-run setup safe under concurrent requests.
CREATE TABLE bootstrap_lock (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    touched_at INTEGER NOT NULL
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    target_type TEXT,
    target_id TEXT,
    details_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX audit_log_created_idx ON audit_log(created_at DESC);

CREATE TABLE devices (
 id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('client','node')),
 level TEXT NOT NULL CHECK(level IN ('read','write','admin')),
 token_hash TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL,
 last_seen_at TEXT, revoked_at TEXT
);
CREATE UNIQUE INDEX devices_active_name ON devices(name) WHERE revoked_at IS NULL;

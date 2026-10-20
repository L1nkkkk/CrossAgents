-- Initial schema. See docs/DESIGN.md §12.
-- Ids are UUID strings, timestamps RFC 3339 strings, structured values JSON text.

CREATE TABLE agents (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    plugin      TEXT NOT NULL,
    driver      TEXT,
    model       TEXT,
    role        TEXT NOT NULL,
    cwd         TEXT NOT NULL,
    config      TEXT NOT NULL DEFAULT 'null',
    status      TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE runs (
    id          TEXT PRIMARY KEY NOT NULL,
    title       TEXT NOT NULL,
    planner_id  TEXT REFERENCES agents(id) ON DELETE SET NULL,
    status      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE tasks (
    id          TEXT PRIMARY KEY NOT NULL,
    run_id      TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    parent_id   TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    title       TEXT NOT NULL,
    spec        TEXT NOT NULL,
    assignee_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
    status      TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX tasks_run_id ON tasks(run_id);
CREATE INDEX tasks_status ON tasks(status);

CREATE TABLE messages (
    id           TEXT PRIMARY KEY NOT NULL,
    from_id      TEXT,
    to_id        TEXT NOT NULL,
    body         TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    delivered_at TEXT
);
CREATE INDEX messages_to_id ON messages(to_id);

CREATE TABLE events (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    id      TEXT NOT NULL UNIQUE,
    ts      TEXT NOT NULL,
    schema  INTEGER NOT NULL,
    type    TEXT NOT NULL,
    data    TEXT NOT NULL
);
CREATE INDEX events_type ON events(type);

CREATE TABLE usage (
    id                 TEXT PRIMARY KEY NOT NULL,
    run_id             TEXT,
    task_id            TEXT,
    agent_id           TEXT,
    model              TEXT,
    role               TEXT,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    ts                 TEXT NOT NULL
);
CREATE INDEX usage_run_id ON usage(run_id);

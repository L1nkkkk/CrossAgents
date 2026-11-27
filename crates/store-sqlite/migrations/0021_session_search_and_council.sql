-- Cross-session full-text search index (trigram FTS5 for English + CJK substring search)
-- and persistent records for Multi-Model Council consultations.

CREATE TABLE session_search_docs (
    id         TEXT PRIMARY KEY NOT NULL,
    agent_id   TEXT NOT NULL,
    message_id TEXT,
    seq        INTEGER NOT NULL,
    role       TEXT NOT NULL,
    ts         TEXT NOT NULL,
    text       TEXT NOT NULL
);
CREATE INDEX session_search_docs_agent ON session_search_docs(agent_id, seq);

CREATE VIRTUAL TABLE session_search_fts USING fts5(
    id UNINDEXED,
    text,
    tokenize = 'trigram'
);

CREATE TABLE session_search_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE councils (
    id         TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX councils_created ON councils(created_at DESC);

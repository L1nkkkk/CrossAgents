-- Expertise profiles (docs/tasks/EXPERTISE.md): what good work looks like for the person and
-- for a project. Full records as JSON; the columns beside them are what queries filter on.

CREATE TABLE expertise_entries (
    id         TEXT PRIMARY KEY NOT NULL,
    owner_kind TEXT NOT NULL,
    owner_id   TEXT NOT NULL,
    domain     TEXT NOT NULL,
    kind       TEXT NOT NULL,
    status     TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX expertise_entries_owner ON expertise_entries(owner_kind, owner_id, domain, updated_at);

-- Search over what an agent would type, or paste from a task's goal. Trigrams, so words
-- inside longer strings (and Chinese, which has no spaces) match too.
CREATE VIRTUAL TABLE expertise_fts USING fts5(
    id UNINDEXED, title, domain, applies_when, text,
    tokenize = 'trigram'
);

-- Entries an agent proposed and only the person can accept, reject or arbitrate.
CREATE TABLE expertise_candidates (
    id         TEXT PRIMARY KEY NOT NULL,
    owner_kind TEXT NOT NULL,
    owner_id   TEXT NOT NULL,
    status     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX expertise_candidates_owner ON expertise_candidates(owner_kind, owner_id, status, created_at);

-- Every time an entry was pushed, pulled or named by a task; the outcome is filled in when
-- the task is decided, and stays empty while it is not.
CREATE TABLE expertise_usage (
    id       TEXT PRIMARY KEY NOT NULL,
    entry_id TEXT NOT NULL,
    task_id  TEXT NOT NULL,
    outcome  TEXT,
    at       TEXT NOT NULL,
    data     TEXT NOT NULL
);
CREATE INDEX expertise_usage_entry ON expertise_usage(entry_id, at);
CREATE INDEX expertise_usage_task ON expertise_usage(task_id);

-- Projects and their troubleshooting records (docs/tasks/LESSONS.md). Full records as JSON;
-- the columns beside them are what queries filter on.

CREATE TABLE projects (
    id   TEXT PRIMARY KEY NOT NULL,
    data TEXT NOT NULL
);

CREATE TABLE lessons (
    id         TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    status     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX lessons_project ON lessons(project_id, created_at);

-- Search over what a later agent would type or paste from an error. Trigrams, so words
-- inside longer strings (and Chinese, which has no spaces) match too.
CREATE VIRTUAL TABLE lessons_fts USING fts5(
    id UNINDEXED, title, symptom, keywords, resolution,
    tokenize = 'trigram'
);

-- How each recorded problem (a run note under `issue/`) was sorted; one row per issue.
CREATE TABLE issue_triage (
    run_id TEXT NOT NULL,
    key    TEXT NOT NULL,
    data   TEXT NOT NULL,
    PRIMARY KEY (run_id, key)
);

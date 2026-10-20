-- Runs and tasks keep their full record as JSON in `data`; the other columns exist for
-- lookups and ordering. Both tables were unused before this migration, so recreate them.

DROP TABLE tasks;
DROP TABLE runs;

CREATE TABLE runs (
    id          TEXT PRIMARY KEY NOT NULL,
    status      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    data        TEXT NOT NULL
);
CREATE INDEX runs_created_at ON runs(created_at);

CREATE TABLE tasks (
    id          TEXT PRIMARY KEY NOT NULL,
    run_id      TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    status      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    data        TEXT NOT NULL
);
CREATE INDEX tasks_run_id ON tasks(run_id);

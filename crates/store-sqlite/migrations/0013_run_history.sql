-- What a run left behind in its workspace, kept after the run itself is deleted: deleting a
-- run removes its record and tasks, never what it did. The full record is JSON in `data`, the
-- other columns exist for lookups and ordering.
CREATE TABLE IF NOT EXISTS run_history (
    run_id     TEXT PRIMARY KEY NOT NULL,
    work_id    TEXT,
    repo       TEXT NOT NULL,
    status     TEXT NOT NULL,
    -- 1 once the run itself was deleted; the record then stays, read-only.
    deleted    INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    ended_at   TEXT,
    data       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS run_history_repo ON run_history(repo, created_at DESC);
CREATE INDEX IF NOT EXISTS run_history_work ON run_history(work_id);

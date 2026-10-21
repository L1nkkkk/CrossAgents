-- Shared notes (docs/tasks/H1.md §5.5). `run_id` is '' for team notes. Full record as JSON.

CREATE TABLE notes (
    scope   TEXT NOT NULL,
    run_id  TEXT NOT NULL,
    key     TEXT NOT NULL,
    data    TEXT NOT NULL,
    PRIMARY KEY (scope, run_id, key)
);

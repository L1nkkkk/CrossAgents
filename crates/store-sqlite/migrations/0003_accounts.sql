-- Vendor accounts that agents bind to (docs/tasks/M5.md). Full record as JSON in `data`.

CREATE TABLE accounts (
    id          TEXT PRIMARY KEY NOT NULL,
    provider    TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    data        TEXT NOT NULL
);

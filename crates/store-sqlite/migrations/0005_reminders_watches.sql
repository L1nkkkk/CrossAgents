-- Reminders and watches agents set for themselves (docs/tasks/H1.md §5.3). Full record as
-- JSON in `data`.

CREATE TABLE reminders (
    id       TEXT PRIMARY KEY NOT NULL,
    owner    TEXT NOT NULL,
    due_at   TEXT NOT NULL,
    data     TEXT NOT NULL
);
CREATE INDEX reminders_due ON reminders (due_at);

CREATE TABLE watches (
    id       TEXT PRIMARY KEY NOT NULL,
    owner    TEXT NOT NULL,
    data     TEXT NOT NULL
);

-- Questions and notices agents put in front of the person (docs/tasks/H1.md §5.4). Full
-- record as JSON in `data`.

CREATE TABLE human_items (
    id          TEXT PRIMARY KEY NOT NULL,
    status      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    data        TEXT NOT NULL
);
CREATE INDEX human_items_status ON human_items (status);

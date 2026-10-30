-- Existing runs stay ungrouped; missing work_id deserializes to None.
CREATE TABLE IF NOT EXISTS works (
    id TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS runs_one_open_per_work
ON runs(json_extract(data, '$.work_id'))
WHERE json_extract(data, '$.work_id') IS NOT NULL
  AND status NOT IN ('completed', 'failed', 'cancelled');

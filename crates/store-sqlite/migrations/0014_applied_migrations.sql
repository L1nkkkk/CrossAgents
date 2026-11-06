-- One row per one-off data migration run from Rust (see `apply_data_migrations` in
-- `src/lib.rs`). Schema changes are tracked by sqlx's own `_sqlx_migrations`; this table marks
-- the changes to existing rows that must happen exactly once per database.
CREATE TABLE IF NOT EXISTS applied_migrations (
    name       TEXT PRIMARY KEY NOT NULL,
    applied_at TEXT NOT NULL
);

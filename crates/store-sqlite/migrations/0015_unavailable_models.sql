CREATE TABLE unavailable_models (
    plugin TEXT NOT NULL, account TEXT NOT NULL, model TEXT NOT NULL,
    observed_at TEXT NOT NULL, error TEXT NOT NULL,
    PRIMARY KEY (plugin, account, model)
);

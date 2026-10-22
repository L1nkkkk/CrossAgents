-- Agent identity: the name is for people and can be renamed (`name_source` says whether the
-- hub made it up), a chat has a title of its own, and a conversation can be archived.
ALTER TABLE agents ADD COLUMN name_source TEXT NOT NULL DEFAULT 'generated';
ALTER TABLE agents ADD COLUMN topic TEXT;
ALTER TABLE agents ADD COLUMN archived_at TEXT;
-- Names compare ignoring case ("Luna" and "luna" are one agent).
CREATE UNIQUE INDEX agents_name_nocase ON agents(name COLLATE NOCASE);

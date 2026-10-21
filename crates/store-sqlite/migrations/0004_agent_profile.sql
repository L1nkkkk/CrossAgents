-- An agent's card: display name, title, avatar, colour and persona (JSON). See docs/DESIGN.md §4.3.
ALTER TABLE agents ADD COLUMN profile TEXT NOT NULL DEFAULT '{}';

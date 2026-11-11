-- The node (device id) an agent runs on; NULL is the hub's built-in node. Rows from before
-- nodes stay NULL (the built-in node), nothing is migrated.
ALTER TABLE agents ADD COLUMN node TEXT;

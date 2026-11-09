-- The system a device last reported (x-crossagents-client-os); NULL until it does.
ALTER TABLE devices ADD COLUMN os TEXT;

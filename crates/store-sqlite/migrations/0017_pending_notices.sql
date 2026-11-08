-- Notices waiting to be carried to an agent. Recording is separate from waking.
-- A row stays until the turn that carried it ends successfully. carried_by is that
-- message; a newer line for the same topic clears it. wake_tries counts automatic
-- wakes that failed or were interrupted. At 2 the row only rides the next wake.
-- The same agent and topic replaces the line; first_at is left as it was.

CREATE TABLE pending_notices (
    agent      TEXT NOT NULL,
    topic      TEXT NOT NULL,
    line       TEXT NOT NULL,
    class      TEXT NOT NULL,
    first_at   TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    carried_by TEXT,
    wake_tries INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent, topic)
);
CREATE INDEX pending_notices_agent ON pending_notices (agent, updated_at);

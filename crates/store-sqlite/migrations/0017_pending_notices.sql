-- Notices waiting to be carried to an agent. Recording is separate from waking: a row
-- stays until the wake that included it is consumed (or a person's message carries it).
-- The same agent and topic replaces the line; first_at is left as it was.

CREATE TABLE pending_notices (
    agent      TEXT NOT NULL,
    topic      TEXT NOT NULL,
    line       TEXT NOT NULL,
    class      TEXT NOT NULL,
    first_at   TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (agent, topic)
);
CREATE INDEX pending_notices_agent ON pending_notices (agent, updated_at);

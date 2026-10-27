-- Agents belong to a workspace (docs/DESIGN.md §4.7): `home` is it, `cwd` where the agent
-- works now. Agents that lived in a run's worktree get the run's repository at hub start
-- (the worktree root is configuration, not data).
ALTER TABLE agents ADD COLUMN home TEXT;
UPDATE agents SET home = cwd;

-- What holds an agent: one row per agent, so taking it is one insert that fails when
-- another run (or task) has it (docs/tasks/S1.md §3.1).
CREATE TABLE agent_claims (
    agent_id TEXT PRIMARY KEY NOT NULL,
    run_id   TEXT NOT NULL,
    task_id  TEXT,
    role     TEXT NOT NULL,
    fresh    INTEGER NOT NULL DEFAULT 0,
    since    TEXT NOT NULL
);
CREATE INDEX agent_claims_run ON agent_claims(run_id);

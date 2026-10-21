//! How each role works with the hub: given to an agent once, as session instructions, and
//! never repeated in its conversations (docs/tasks/H1.md §2). Tool descriptions carry the
//! details of each call.

use crossagents_core::Role;

const COMMON: &str = "You work in CrossAgents, a hub where agents from different vendors \
collaborate. Use the `crossagents` tools to see your teammates (whoami, agent_list), message \
them (agent_send), ask the person (human_ask, only when you truly need a decision) and set \
reminders or watches for yourself. Messages from teammates start with \"Message from …\"; \
\"CrossAgents notices\" are things you asked to hear about. Retrying a tool call is safe.";

const LEAD: &str = "As the LEAD (planner) of a run: understand the code, split the person's \
request into small tasks for cheaper worker models, and own the result. Do not implement the \
change yourself.\n\
1. Plan: call `plan_submit` once with all tasks (key, title, spec.goal written so a worker who \
sees only it and the repository can do it, scope, interfaces, references, optional \
spec.acceptance commands, depends_on, assignee from the roster). Depending on the run's policy \
the person approves it first, or it starts at once; end your turn after submitting. Later you \
can add tasks with `task_create`.\n\
2. Tasks start by themselves once their dependencies are merged; each worker works in its own \
git worktree and branch.\n\
3. When a worker reports done you get its summary and the facts (branch, changes, check \
results). Check the work: `workspace_diff`, `workspace_read`, `workspace_run` (tests, builds), \
`checks_run`, or `review_request` for a reviewer. Then decide: `task_accept` merges it into the \
run branch (on a conflict you get the conflicting hunks: resolve with `task_rework` or \
`branch_sync`), `task_rework` sends it back with what to fix, `task_drop` discards it.\n\
4. Answer worker questions with `task_answer`. A task that is \"stalled\" means its worker \
stopped without reporting: message it, `task_assign` it to someone else, or drop it.\n\
5. When every task has ended, land the run branch with `run_land` if the run's policy lets you \
(otherwise the person lands it), then close the run with `run_finish` and a short summary for \
the person: what changed, what did not, anything they should check.";

const WORKER: &str = "As a WORKER on a task: do exactly what the task's spec asks, in your \
working directory (the task's own git worktree). Do not commit; the hub does. Report with the \
task_id from your task: `task_progress` for a short status now and then, `task_ask` if the spec \
is unclear (then end your turn and wait for the answer), `task_complete` with a summary of what \
you changed when done, `task_fail` with the reason if it cannot be done. The task's issuer \
checks your work and may send it back with notes; then continue and report again.";

const REVIEWER: &str = "As a REVIEWER: read the change you are asked to review (the diff of \
its branch against the run branch, in your working directory) and judge it against the task. \
Call `review_submit` once with approve true or false and, for a rejection, exactly what to fix.";

/// The guide for an agent of `role`.
pub fn guide(role: Role) -> String {
    let specific = match role {
        Role::Planner => LEAD,
        Role::Worker => WORKER,
        Role::Reviewer => REVIEWER,
    };
    format!("{COMMON}\n\n{specific}")
}

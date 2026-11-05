//! How each role works with the hub: given to an agent once, as session instructions, and
//! never repeated in its conversations (docs/tasks/H1.md §2). Tool descriptions carry the
//! details of each call.

use crossagents_core::{Agent, Role};

use crate::messaging::Group;

const COMMON: &str = "You work in CrossAgents, a hub where agents from different vendors \
collaborate. Use the `crossagents` tools to see your teammates (whoami, agent_list), message \
them (agent_send), ask the person (human_ask, only when you truly need a decision) and set \
reminders or watches for yourself. Messages from teammates start with \"Message from …\"; \
\"CrossAgents notices\" are things you asked to hear about. Retrying a tool call is safe.\n\
If you need a new tool or panel, you can write an extension: call `extension_api` first. \
Check it with `extension_check`, then request human approval with `extension_request_enable`.\n\
Say what really happened. When something you were asked to do is unavailable or refused (a \
tool error, a policy limit, a model you cannot use), say so plainly in your reply: what you \
tried, what was refused and why, and what you did instead. Never report a substitute as the \
thing that was asked for: another agent, or another model, is not the one requested. A \
permission request that is declined (a push, a publish, a command) was refused by the person \
or the hub's rules: do not retry it, in that or another form. A tool that answers with an \
error is not such a refusal: do what the error says, and call it again when it tells you to. \
Call \
teammates by their names; the model a teammate runs on is an attribute that can change.\n\
When you mention a file, write its path relative to your working directory's root, with \
forward slashes, optionally with `:line`, e.g. `crates/x/src/a.rs:12` or \
[a.rs](crates/x/src/a.rs:12): the person can click it to open the file.\n\
To show the person an image, write `![what it shows](path)` in your reply, with the file in your \
working directory or the system temp directory. Opening an image with a tool shows it to you, \
not to them.";

const CONVERSATION: &str = "Outside a run you are a conversation with the person. Run tools \
(plan_submit, task_create, run_land, …) work only in a run, and a run starts when the person \
starts a Work from the home page or accepts one you propose; here, do the work yourself or \
bring in teammates as below. When a change deserves its own branch (to review before it lands, \
or to split among teammates), propose a Work with `work_start` and a complete brief. When the \
person asks for another agent or model to do something (\"use Haiku\", \"ask a Sonnet to \
review this\"), do not do it yourself: message a fitting teammate from `agent_list` with \
`agent_send`, or recruit one with `agent_spawn` (the model asked for goes in model; give it a \
person's name, or none to get one), wait for its answer and pass it on, saying who answered. \
The person may name helpers with @: `@Name` is a teammate (message it with `agent_send`), \
`@model-id` is a model to recruit an agent on (`agent_spawn` with that model); hand the part \
meant for them to them in the same way. When nobody is named, decide yourself: work alone by \
default, and bring in a teammate only when a part clearly gains from it (an independent \
review, work that runs in parallel, a model better suited to it); say who you brought in and \
why. \
Retire recruits with `agent_retire` when they are done. This conversation's policy limits how \
many you may recruit and which models; if it refuses, tell the person rather than working \
around it.";

const LEAD: &str = "As the LEAD of a run you own the person's request and its result. Your \
working directory is the run's own checkout of the run branch: what you change stays there \
until the run is landed, so you may edit, build and test freely.\n\
0. Decide how to do it. When the request is a question or asks you to look at something, \
write the full answer in your reply first, then close the run with `run_finish` as the last \
thing you do (nothing to land), its summary giving the gist of the answer, not only that you \
answered. When you can make the \
change yourself, do it here, check it, and close with `run_finish` (summary for the person, \
message for the commit, written to the project's rules); the person, or you with `run_land` if \
the run's policy lets you, lands it. Split the work only when it pays: parts that can go in \
parallel, or that suit another model; then plan as below. When your kickoff lists checks, the \
hub runs them before `run_land` or `run_finish` accepts a run that changed something; a \
failure is refused with the output: fix it and call again.\n\
1. Plan: call `plan_submit` once with all tasks (key, title, spec.goal written so a worker who \
sees only it and the repository can do it, scope, interfaces, references, optional \
spec.acceptance commands, depends_on, assignee from the roster). Keep each spec.goal short: \
what to change and why, the decisions the worker cannot guess, and what done looks like; \
point at code by path:line in references instead of pasting it or retelling the \
repository, which the worker reads itself. The plan is written token by token while the \
person waits. A task goes to a new \
execution session of its assignee, from a clean context, unless you write `fresh: false`: then \
that session itself does it, with its conversation (one of the workspace's sessions your \
kickoff and `agent_list` show; `restore: true` for an archived one). It is refused, with the \
reason, while a person waits on it or another run holds it. Depending on the run's policy \
the person approves it first, or it starts at once; end your turn after submitting. Tasks \
start from the run branch as you left it. Later you can add tasks with `task_create`.\n\
2. Tasks start by themselves once their dependencies are merged; each worker works in its own \
git worktree and branch.\n\
3. When a worker reports done you get its summary and the facts (branch, changes, check \
results). Check the work: `workspace_diff`, `workspace_read`, `workspace_run` (tests, builds), \
`checks_run`, or `review_request` for a reviewer. Then decide: `task_accept` merges it into the \
run branch (its message is the commit message: write it the way the project's rules ask, as \
for `run_land`; on a conflict you get the conflicting hunks: resolve with `task_rework` or \
`branch_sync`), `task_rework` sends it back with what to fix, `task_drop` discards it.\n\
4. Answer worker questions with `task_answer`; a decision that changes what may be touched \
goes into the spec too (`task_update`). A worker in a turn reads your answer when that turn \
ends; interrupting it to repeat the answer only puts other messages ahead of it. A task that is \"stalled\" means its worker \
stopped without reporting: message it, `task_assign` it to someone else, or drop it.\n\
5. When every task has ended, land the run branch with `run_land` if the run's policy lets you \
(otherwise the person lands it), then close the run with `run_finish` and a short summary for \
the person: what changed, what did not, anything they should check.\n\
The person may keep talking to you after the run is closed. When they propose a new goal, use \
`run_next` to start the next round in this work group, not `work_start`. Answer questions, \
conversation and follow-ups about the previous round directly without starting a new round. \
Use `work_start` (with `separate: true`, which says you mean a Work of its own and not the \
next round here) only when the person explicitly asks for a separate, independent Work. \
If run_land refuses because the base checkout has the person's uncommitted or \
staged changes, leave those changes alone: do not commit, unstage, stash or discard them, \
and do not retry landing. Call run_finish, say that the result remains on the run branch, \
and leave landing to the person.";

const WORKER: &str = "As a WORKER on a task: do exactly what the task's spec asks, in your \
working directory (the task's own git worktree). Do not commit; the hub does. For a big file, \
grep for what you need, then read that range; do not read it whole. Report with the \
task_id from your task: `task_progress` for a short status now and then, `task_ask` if the spec \
is unclear (then end your turn and wait for the answer; asking again replaces the open \
question), `task_complete` with a summary of what \
you changed when done, `task_fail` with the reason if it cannot be done. The task's issuer \
checks your work and may send it back with notes; then continue and report again. The \
spec is the decision: when it was changed, or you wait for an answer, `task_get` it before \
asking again, since the answer may already be there. When you are given a task yourself (with your conversation, rather than as a new session), you work in \
the task's worktree until it ends and then go back to your own directory; a person may still \
write to you, and comes first.";

const REVIEWER: &str = "As a REVIEWER: read the change you are asked to review (the diff of \
its branch against the run branch, in your working directory) and judge it against the task. \
Call `review_submit` once with approve true or false and, for a rejection, exactly what to fix.";

/// Building in a checkout goes through this machine's executor, so that the hub's checks and
/// other agents' commands take turns on it instead of colliding (docs/tasks/LOCKS.md §6).
const LOCKS: &str = "Commands that build or test in your checkout go through the executor: \
`crossagents exec --resource build -- <command>` (cargo already goes through it by itself: the \
checkout's PATH has a shim). When you see `waiting for …` another command holds the build \
directory and you are queued, so do not start the same command again; exit code 75 is the wait \
timing out, and you can retry.";

/// Codex names MCP tools `mcp__<server>__<tool>`, and its code-mode models (`tool_mode:
/// code_mode_only`, e.g. gpt-6-luna) see no tool but `exec`: told only "call task_complete",
/// they find nothing by that name and end the turn without reporting.
const CODEX_TOOLS: &str = "In Codex the `crossagents` tools are named \
`mcp__crossagents__<tool>`, e.g. `mcp__crossagents__task_complete`. If your only tool is \
`exec`, call them from it: `text(await tools.mcp__crossagents__task_complete({task_id, \
summary}))`.";

/// The guide for `agent` in `groups` ([`crate::Orchestrator::groups`]): the role's it holds,
/// and what a conversation outside any run may do. A plain chat holds no role and gets no
/// run workflow (docs/SESSION-FIRST.md §7).
pub fn guide_for(agent: &Agent, groups: &[Group]) -> String {
    let role = groups.iter().find_map(|g| match g {
        Group::Lead => Some(Role::Planner),
        Group::Assignee => Some(Role::Worker),
        Group::Reviewer => Some(Role::Reviewer),
        Group::Everyone | Group::Conversation => None,
    });
    let mut text = match role {
        Some(role) => guide(role),
        // It builds in its own checkout like anyone else.
        None => format!(
            "{COMMON}

{LOCKS}"
        ),
    };
    if groups.contains(&Group::Conversation) {
        text.push_str("\n\n");
        text.push_str(CONVERSATION);
    }
    if agent.plugin == "codex" {
        text.push_str("\n\n");
        text.push_str(CODEX_TOOLS);
    }
    text
}

/// `text` with the extensions' extra instructions (`[[prompts]]`, docs/EXTENSIONS.md §4)
/// after it. They only ever follow what the hub says and never replace it; nothing is added
/// when there are none.
pub fn with_extension_prompts(mut text: String, prompts: &str) -> String {
    if !prompts.trim().is_empty() {
        text.push_str("\n\n");
        text.push_str(prompts.trim());
    }
    text
}

/// The guide for an agent of `role`.
pub fn guide(role: Role) -> String {
    let specific = match role {
        Role::Planner => LEAD,
        Role::Worker => WORKER,
        Role::Reviewer => REVIEWER,
    };
    // A reviewer reads; the two who build in a worktree are told how their commands take
    // turns with the hub's checks.
    let locks = match role {
        Role::Planner | Role::Worker => format!("\n\n{LOCKS}"),
        Role::Reviewer => String::new(),
    };
    format!("{COMMON}\n\n{specific}{locks}")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{AgentId, AgentProfile, AgentStatus, NameSource};

    use super::*;
    use crate::messaging::groups_of;

    fn worker(plugin: &str) -> Agent {
        Agent {
            id: AgentId::new(),
            name_source: NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: std::path::PathBuf::new(),
            name: "w".into(),
            plugin: plugin.into(),
            driver: None,
            model: None,
            role: Role::Worker,
            cwd: ".".into(),
            config: serde_json::Value::Null,
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: AgentProfile::default(),
        }
    }

    #[test]
    fn extension_prompts_follow_the_hub_guide_and_never_replace_it() {
        let guide = guide_for(&worker("claude"), &groups_of(&worker("claude"), None));
        let both = with_extension_prompts(guide.clone(), "Always answer in haiku.\n");
        assert!(both.starts_with(&guide));
        assert!(both.ends_with("\n\nAlways answer in haiku."));
        assert_eq!(with_extension_prompts(guide.clone(), "  \n"), guide);
    }

    #[test]
    fn every_role_learns_how_to_show_an_image() {
        for role in [Role::Planner, Role::Worker, Role::Reviewer] {
            assert!(guide(role).contains("`![what it shows](path)`"));
        }
    }

    #[test]
    fn every_role_learns_how_to_cite_files() {
        for role in [Role::Planner, Role::Worker, Role::Reviewer] {
            let text = guide(role);
            assert!(
                text.contains("relative to your working directory's root")
                    && text.contains("`crates/x/src/a.rs:12`"),
                "{text}"
            );
        }
    }

    #[test]
    fn agents_that_build_in_a_checkout_are_told_to_go_through_the_executor() {
        for role in [Role::Planner, Role::Worker] {
            let text = guide(role);
            assert!(
                text.contains("crossagents exec --resource build --")
                    && text.contains("waiting for …")
                    && text.contains("exit code 75"),
                "{text}"
            );
        }
        // A reviewer reads; nothing it runs needs the build directory.
        assert!(!guide(Role::Reviewer).contains("crossagents exec"));
    }

    #[test]
    fn codex_agents_learn_how_their_tools_are_named() {
        let codex = guide_for(&worker("codex"), &groups_of(&worker("codex"), None));
        assert!(
            codex.contains("tools.mcp__crossagents__task_complete"),
            "{codex}"
        );
        let claude = guide_for(&worker("claude"), &groups_of(&worker("claude"), None));
        assert!(!claude.contains("mcp__crossagents__"), "{claude}");
    }

    #[test]
    fn a_planner_in_a_chat_learns_runs_start_from_a_work() {
        let planner = Agent {
            role: Role::Planner,
            ..worker("claude")
        };
        let guide = guide_for(&planner, &groups_of(&planner, None));
        assert!(guide.contains("As the LEAD"), "{guide}");
        // The wording itself, not just the tool's name: a guide that said the opposite
        // (what it said before G1-3) would pass a bare `contains("run_next")`.
        assert!(
            guide.contains("start the next round in this work group")
                && guide.contains("separate, independent Work"),
            "{guide}"
        );
        assert!(
            guide.contains("starts a Work from the home page") && guide.contains("work_start"),
            "{guide}"
        );
        // A run's recruit is no conversation of its own.
        let recruit = Agent {
            config: serde_json::json!({ "spawned_by_run": "r1" }),
            ..worker("claude")
        };
        assert!(!guide_for(&recruit, &groups_of(&recruit, None)).contains("Outside a run"));
    }

    #[test]
    fn a_plain_chat_gets_no_run_workflow_until_a_run_holds_it() {
        let chat = worker("claude");
        let groups = groups_of(&chat, None);
        assert_eq!(groups, [Group::Everyone, Group::Conversation]);
        let guide = guide_for(&chat, &groups);
        assert!(
            !guide.contains("As a WORKER") && !guide.contains("task_complete"),
            "{guide}"
        );
        assert!(
            guide.contains("Outside a run") && guide.contains("crossagents exec"),
            "{guide}"
        );
        // Taken into a run as a worker, it is told how to report.
        let held = groups_of(&chat, Some(crossagents_core::ClaimRole::Worker));
        assert!(held.contains(&Group::Assignee));
        assert!(guide_for(&chat, &held).contains("As a WORKER"));
    }
}

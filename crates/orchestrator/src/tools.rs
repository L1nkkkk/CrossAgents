//! Collaboration tools (docs/DESIGN.md §9, docs/tasks/H1.md §5): definitions by permission
//! group and dispatch.

use std::sync::Arc;

use crossagents_core::{Agent, CoreError, PlannedTask, RunStatus, ToolInfo};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::lessons::outcome_schema;
use crate::messaging::Group;

const ALL: &[Group] = &[Group::Everyone];
/// Anyone a run holds: tasks, their worktrees and checks exist only in a run.
const RUN: &[Group] = &[Group::Lead, Group::Assignee, Group::Reviewer];
const PLANNER: &[Group] = &[Group::Lead];
const WORKER: &[Group] = &[Group::Assignee];
const REVIEWER: &[Group] = &[Group::Reviewer];
const RECRUITERS: &[Group] = &[Group::Lead, Group::Conversation];
const CONVERSATION: &[Group] = &[Group::Conversation];

struct ToolDef {
    name: &'static str,
    description: &'static str,
    groups: &'static [Group],
    schema: fn() -> Value,
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn task_id_schema(extra: Value) -> Value {
    let mut properties =
        json!({ "task_id": { "type": "string", "description": "The task_id from your brief." } });
    let mut required = vec![json!("task_id")];
    if let Value::Object(fields) = extra {
        for (name, schema) in fields {
            required.push(json!(name));
            properties[&name] = schema;
        }
    }
    json!({ "type": "object", "properties": properties, "required": required })
}

/// A task by key (`auth`, `#auth`) or id, plus `extra` fields (all required).
fn task_schema(extra: Value) -> Value {
    let mut properties = json!({ "task": { "type": "string", "description": "Task key (e.g. \"auth\") or task id." } });
    let mut required = vec![json!("task")];
    if let Value::Object(fields) = extra {
        for (name, schema) in fields {
            properties[&name] = schema;
        }
    }
    required.dedup();
    json!({ "type": "object", "properties": properties, "required": required })
}

fn planned_task_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "key": { "type": "string", "description": "Short unique id: a-z, 0-9, -" },
            "title": { "type": "string" },
            "spec": spec_schema(),
            "depends_on": { "type": "array", "items": { "type": "string" } },
            "assignee": { "type": "string", "description": "Name of a teammate whose specialty fits this task, or of any session (a new execution session copies its model); omit for the default worker. With fresh: false, any session of this workspace (see agent_list or your kickoff's sessions)" },
            "fresh": { "type": "boolean", "description": "Default true: a new execution session of the assignee takes the task, from a clean context. false: the assignee itself does it, with its conversation; refused (with the reason) when it is busy with a person or another run, archived, or cannot move" },
            "restore": { "type": "boolean", "description": "With fresh: false, restore the assignee first if it is archived" },
            "reviewer": { "type": "string", "description": "Name of a session to review it: a reviewer on your team (e.g. a security reviewer) or any other session" },
            "reviewer_fresh": { "type": "boolean", "description": "Like fresh, for the reviewer (default true)" }
        },
        "required": ["key", "title", "spec"]
    })
}

fn spec_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "properties": {
            "goal": { "type": "string", "description": "Short: what to change and why, decisions the worker cannot guess, what done looks like. Point at code by path:line (or in references) instead of pasting it." },
            "scope": {
                "type": "object",
                "properties": { "allow": strings, "deny": strings }
            },
            "interfaces": strings,
            "acceptance": { "type": "array", "items": { "type": "string" }, "description": "Optional shell commands the hub runs when the worker reports done." },
            "references": strings,
            "result_schema": { "type": "object", "description": "JSON Schema for the required structured result." },
            "stage": { "type": "integer", "description": "Optional stage: tasks of a later stage start only once every task of an earlier stage has ended, and you are woken when each stage's batch ends. Omit for no staging." }
        },
        "required": ["goal"]
    })
}

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "whoami",
        description: "Who you are here: role, permission groups, your run, the tasks you work on or issued, and the tools you can use.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "agent_list",
        description: "Your teammates and the sessions of your workspace: name, title, specialty, role, status, model, current tasks, state (idle, busy with which run's task, archived) and last execution. Archived sessions of your workspace are listed too.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "agent_get",
        description: "One teammate in detail: full persona, model, working directory, status and current tasks.",
        groups: ALL,
        schema: || object(json!({ "name": { "type": "string" } }), &["name"]),
    },
    ToolDef {
        name: "agent_send",
        description: "Send a message. `to` is a name, a list of names, or \"@team\" (everyone in your run). It arrives as a new turn, or after the recipient's current turn (`now: true` interrupts it instead). To answer a message, give `reply_to` with the id from its header (`Message from … (id …):`); `to` may then be left out, and the reply goes to whoever sent it. `task` names the task it is about. Returns the message ids. Retrying the same call is safe: identical messages within a minute, or the same idempotency_key, are sent once.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "to": { "oneOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] },
                    "text": { "type": "string" },
                    "reply_to": { "type": "string", "description": "Id of the message this answers." },
                    "task": { "type": "string", "description": "Task key or id this is about." },
                    "now": { "type": "boolean" },
                    "idempotency_key": { "type": "string" }
                }),
                &["text"],
            )
        },
    },
    ToolDef {
        name: "message_withdraw",
        description: "Take back a message you sent that the recipient has not started on yet.",
        groups: ALL,
        schema: || {
            object(
                json!({ "message_id": { "type": "string" } }),
                &["message_id"],
            )
        },
    },
    ToolDef {
        name: "remind_set",
        description: "Wake yourself later with a note: after_minutes, or at (RFC 3339). With task and only_if_open, it fires only if that task has not ended by then. The reminder arrives in your notices. At most 20 at a time.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "after_minutes": { "type": "number" },
                    "at": { "type": "string" },
                    "note": { "type": "string" },
                    "task": { "type": "string" },
                    "only_if_open": { "type": "boolean" }
                }),
                &["note"],
            )
        },
    },
    ToolDef {
        name: "remind_list",
        description: "Your pending reminders.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "remind_cancel",
        description: "Cancel one of your reminders by id.",
        groups: ALL,
        schema: || object(json!({ "id": { "type": "string" } }), &["id"]),
    },
    ToolDef {
        name: "watch",
        description: "Hear about a task (by key or id) or a teammate (by name) when something happens: on = done, failed, asked, stalled, idle or any (default any). News arrives in your notices, merged while you are busy. At most 20 at a time.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "task": { "type": "string" },
                    "agent": { "type": "string" },
                    "on": { "type": "array", "items": { "type": "string", "enum": ["done", "failed", "asked", "stalled", "idle", "any"] } }
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "human_ask",
        description: "Ask the person a question and end your turn; the answer arrives as a message. Give options (and a default) when you can: depending on the run's policy, an unanswered question falls back to its default after a while (you are told when that happens), or asking is not allowed and you must decide yourself.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "text": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" } },
                    "default": { "type": "string", "description": "Used if nobody answers in time; defaults to the first option." },
                    "task": { "type": "string" }
                }),
                &["text"],
            )
        },
    },
    ToolDef {
        name: "human_notify",
        description: "Tell the person something that needs no answer: a milestone, a finished report, a risk.",
        groups: ALL,
        schema: || {
            object(
                json!({ "text": { "type": "string" }, "task": { "type": "string" } }),
                &["text"],
            )
        },
    },
    ToolDef {
        name: "note_write",
        description: "Write a shared note under a key (replacing what was there): conventions, decisions, interfaces, pitfalls. scope \"run\" (default, this run) or \"team\" (lasts across runs). text null deletes it (author or a lead only). When you hit a problem and got past it (or did not), record it in the run under issue/<task_id>/<a new uuid>, as it happens: 问题 (the symptom, with the error text), 环境 (what matters: system, tool versions), 尝试 (what did not work), 处理 (what you did), 结果 (已解决, 临时绕过 or 未解决), 相关文件. Teammates in the run read them with note_list prefix issue/; the run's issues are sorted into the project's troubleshooting records when it ends.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "key": { "type": "string" },
                    "text": { "type": ["string", "null"] },
                    "scope": { "type": "string", "enum": ["run", "team"] }
                }),
                &["key", "text"],
            )
        },
    },
    ToolDef {
        name: "note_read",
        description: "Read a shared note by key (run notes first, then team notes, unless scope is given).",
        groups: ALL,
        schema: || {
            object(
                json!({ "key": { "type": "string" }, "scope": { "type": "string", "enum": ["run", "team"] } }),
                &["key"],
            )
        },
    },
    ToolDef {
        name: "note_list",
        description: "List shared notes, optionally by key prefix and scope, with the start of each.",
        groups: ALL,
        schema: || {
            object(
                json!({ "prefix": { "type": "string" }, "scope": { "type": "string", "enum": ["run", "team"] } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "issue_pending",
        description: "The problems recorded in a run (run notes under issue/) that nobody has sorted yet, with their text. Defaults to your run; name another run of this project to sort one that already ended, failed or was cancelled. Sort each with lesson_create, lesson_merge or issue_keep.",
        groups: ALL,
        schema: || {
            object(
                json!({ "run": { "type": "string", "description": "Run id, or 4+ characters of it. Default: yours." } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "lesson_create",
        description: "Turn recorded problems into one troubleshooting record of this project, for later agents to find with lesson_search when they hit the same thing. Write what someone with the error in front of them needs: the symptom with the exact error text, keywords (error codes, tool names), when it applies, what to do. A record is reference for later agents, not a rule. Issues already sorted are left as they were, so repeating a call is safe.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "run": { "type": "string" },
                    "issues": { "type": "array", "items": { "type": "string" }, "description": "Issue note keys (issue/<task>/<id>)." },
                    "title": { "type": "string", "description": "One line." },
                    "symptom": { "type": "string" },
                    "keywords": { "type": "array", "items": { "type": "string" } },
                    "conditions": { "type": "string", "description": "Where it applies: system, tool versions, node, files." },
                    "resolution": { "type": "string" },
                    "outcome": outcome_schema()
                }),
                &["issues", "title", "symptom", "resolution", "outcome"],
            )
        },
    },
    ToolDef {
        name: "lesson_merge",
        description: "Sort recorded problems into an existing troubleshooting record (find it with lesson_search): adds them as evidence, optionally with a better resolution or a new outcome.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "run": { "type": "string" },
                    "issues": { "type": "array", "items": { "type": "string" } },
                    "lesson": { "type": "string", "description": "The record's id." },
                    "resolution": { "type": "string" },
                    "outcome": outcome_schema(),
                    "note": { "type": "string" }
                }),
                &["issues", "lesson"],
            )
        },
    },
    ToolDef {
        name: "issue_keep",
        description: "Mark recorded problems as sorted without a record: nothing in them is worth reusing. They stay in the run.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "run": { "type": "string" },
                    "issues": { "type": "array", "items": { "type": "string" } },
                    "why": { "type": "string" }
                }),
                &["issues"],
            )
        },
    },
    ToolDef {
        name: "lesson_search",
        description: "Search this project's troubleshooting records: problems earlier agents hit here and what they did. Paste the error or a few keywords. A match is a lead, not proof it applies: check its conditions and evidence.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "description": "1-10, default 5." },
                    "include_superseded": { "type": "boolean" }
                }),
                &["query"],
            )
        },
    },
    ToolDef {
        name: "lesson_get",
        description: "A troubleshooting record in full: conditions, resolution, how often it was tried and with what outcome, the latest tries, the latest failure, and the runs it came from.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "id": { "type": "string" },
                    "evidence_offset": { "type": "integer", "description": "Skip this many of the newest tries, to page back. Default 0." },
                    "evidence_limit": { "type": "integer", "description": "1-50, default 5." }
                }),
                &["id"],
            )
        },
    },
    ToolDef {
        name: "expertise_search",
        description: "Search what this project and the person consider good work: judgements, worked examples and limits, by keyword. A match is a lead, not a rule: check its status and applies_when before going by it.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "query": { "type": "string" },
                    "domain": { "type": "string", "description": "Only this area (rust, gui, windows, testing, …)." },
                    "kind": { "type": "string", "enum": ["judgment", "example", "limit"] },
                    "limit": { "type": "integer", "description": "1-10, default 5." }
                }),
                &["query"],
            )
        },
    },
    ToolDef {
        name: "expertise_get",
        description: "One expertise entry in full: its text, and for an example the situation, what was rejected and why, and what was chosen. Give its id or the short tag a brief showed (the id's first 8 characters).",
        groups: ALL,
        schema: || object(json!({ "id": { "type": "string" } }), &["id"]),
    },
    ToolDef {
        name: "expertise_pending",
        description: "The expertise candidates waiting for the person in this project and in the person's profile: what a rework, a first-try merge or a reviewer's fail proposed, with the entries each may conflict with. Only the person accepts or rejects one; a lead may flag one with expertise_triage.",
        groups: PLANNER,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "expertise_triage",
        description: "Flag one pending expertise candidate for the person: sets suggested_by_lead and adds your reason to its note. That is all a lead can do — accept and reject belong to the person alone, and asking for either is refused.",
        groups: PLANNER,
        schema: || {
            object(
                json!({
                    "candidate": { "type": "string", "description": "The candidate's id, or the start of it." },
                    "reason": { "type": "string", "description": "Why the person should look at it." }
                }),
                &["candidate", "reason"],
            )
        },
    },
    ToolDef {
        name: "lesson_report",
        description: "Say how a troubleshooting record went when you used it. One that did not work is put up for review; its history stays.",
        groups: ALL,
        schema: || {
            object(
                json!({ "id": { "type": "string" }, "worked": { "type": "boolean" }, "note": { "type": "string" } }),
                &["id", "worked"],
            )
        },
    },
    ToolDef {
        name: "usage_get",
        description: "What your run has spent (cost, sessions, by agent), its budget and time used, and each account's quota windows.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "inbox_read",
        description: "For agents running outside the hub: take the messages waiting for you (teammates' messages, task briefs, notices). Call it at the start of each turn of work.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "watch_list",
        description: "What you are watching.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "watch_cancel",
        description: "Stop a watch by id.",
        groups: ALL,
        schema: || object(json!({ "id": { "type": "string" } }), &["id"]),
    },
    ToolDef {
        name: "work_start",
        description: "Propose doing this on a branch of its own (a Work) instead of here: for a change worth reviewing before it lands, or one to split among teammates. The person sees a card and starts it or declines; nothing starts without them. The Work is led by a copy of you in a fresh checkout that cannot see this conversation, so write the request as a complete brief: the goal and everything agreed here.",
        groups: CONVERSATION,
        schema: || {
            object(
                json!({
                    "request": { "type": "string", "description": "The brief for the Work: what to do, and what was agreed in this conversation." },
                    "title": { "type": "string", "description": "A short title for the card and the run." },
                    "separate": { "type": "boolean", "description": "Set this when you already led a round in a work group and the person asked for a separate, independent Work rather than the next round there (which is run_next's job)." }
                }),
                &["request"],
            )
        },
    },
    ToolDef {
        name: "run_next",
        description: "Start the next round in your work group after your run ended. Write the person's request verbatim. Uses this group's plan policy. If moving your session must wait for this turn to end, returns status scheduled instead of a run id; the hub waits up to 120 seconds for you to be idle and notifies you if it fails. Creation then uses the GUI path and its existing bounds; after 120 seconds in creation a notice reports that it is still starting and has not been cancelled. A schedule is not persistent: a hub restart loses it, so call run_next again after restart. Set from to base to leave the previous round's unlanded changes behind.",
        groups: RECRUITERS,
        schema: || {
            object(
                json!({
                    "title": { "type": "string" },
                    "request": { "type": "string" },
                    "from": { "type": "string", "enum": ["base"] }
                }),
                &["title", "request"],
            )
        },
    },
    ToolDef {
        name: "plan_submit",
        description: "Submit the plan for your current run: the full list of tasks for workers. A task spec may include result_schema, a JSON Schema the worker must satisfy in task_complete.result. Each task goes to a new execution session of its assignee by default (fresh: true); write fresh: false to have that session itself, with its conversation, do it. Depending on the run's policy the person approves it first, or it starts at once.",
        groups: PLANNER,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "tasks": { "type": "array", "items": planned_task_schema() }
                },
                "required": ["tasks"]
            })
        },
    },
    ToolDef {
        name: "task_create",
        description: "Add tasks to your running run (same fields as plan_submit's tasks); spec.result_schema optionally defines a JSON Schema for the worker's result. They start once their dependencies are merged. Keys already in the run are left as they are. If the run lets workers delegate, a worker can split parts off its own task this way: they branch from it and merge back into it, and the worker accepts them. Like plan_submit, each task goes to a new execution session unless it says fresh: false; tasks that could not start are listed in not_started with the reason.",
        groups: RUN,
        schema: || {
            object(
                json!({ "tasks": { "type": "array", "items": planned_task_schema() } }),
                &["tasks"],
            )
        },
    },
    ToolDef {
        name: "task_list",
        description: "Tasks of your run: key, title, status, issuer, assignee, progress, open question. Filter with status, or mine (tasks you issued or work on).",
        groups: RUN,
        schema: || {
            object(
                json!({ "status": { "type": "string" }, "mine": { "type": "boolean" } }),
                &[],
            )
        },
    },
    ToolDef {
        name: "task_get",
        description: "One task in full: spec, report (worker summary, checks, diff stat, failure, question, review), branch, worktree, run branch.",
        groups: RUN,
        schema: || {
            task_schema(
                json!({ "path": { "type": "string", "description": "JSON Pointer into report.result" } }),
            )
        },
    },
    ToolDef {
        name: "task_update",
        description: "Change a task you issued: title, spec, depends_on. Its worker gets the new version.",
        groups: RUN,
        schema: || {
            task_schema(json!({
                "title": { "type": "string" },
                "spec": spec_schema(),
                "depends_on": { "type": "array", "items": { "type": "string" } }
            }))
        },
    },
    ToolDef {
        name: "task_assign",
        description: "Give a task you issued to another session (by name). By default a new execution session of `to` takes it (fresh: true); fresh: false gives it to `to` itself, with its conversation (a session of this workspace; restore: true brings back an archived one), and is refused with the reason when it cannot take it now. A started task keeps its worktree; the new worker continues from it and gets a handoff card (files changed, last word, rework notes, open questions). reason says why it changes hands.",
        groups: RUN,
        schema: || {
            task_schema(json!({
                "to": { "type": "string" },
                "reason": { "type": "string" },
                "fresh": { "type": "boolean", "description": "Default true: a new execution session. false: `to` itself, with its conversation" },
                "restore": { "type": "boolean", "description": "With fresh: false, restore `to` first if it is archived" }
            }))
        },
    },
    ToolDef {
        name: "task_cancel",
        description: "Cancel a task you issued. keep_branch keeps its branch for inspection.",
        groups: RUN,
        schema: || task_schema(json!({ "keep_branch": { "type": "boolean" } })),
    },
    ToolDef {
        name: "task_answer",
        description: "Answer the open question on a task you issued (replying to the question message with agent_send works too).",
        groups: RUN,
        schema: || task_schema(json!({ "answer": { "type": "string" } })),
    },
    ToolDef {
        name: "workspace_diff",
        description: "What a task changed against the run branch, committed or not: a unified diff (paths to narrow it; stat_only for a summary).",
        groups: RUN,
        schema: || {
            task_schema(
                json!({ "paths": { "type": "array", "items": { "type": "string" } }, "stat_only": { "type": "boolean" } }),
            )
        },
    },
    ToolDef {
        name: "workspace_read",
        description: "Read a file in a task's worktree (path relative to it), optionally a line range.",
        groups: RUN,
        schema: || {
            task_schema(
                json!({ "path": { "type": "string" }, "from_line": { "type": "integer" }, "to_line": { "type": "integer" } }),
            )
        },
    },
    ToolDef {
        name: "workspace_run",
        description: "Run a shell command in a task's worktree (tests, builds) and get exit code and output. Default limit 300 s, at most 1800.",
        groups: RUN,
        schema: || {
            task_schema(
                json!({ "command": { "type": "string" }, "timeout_secs": { "type": "integer" } }),
            )
        },
    },
    ToolDef {
        name: "gui_screenshot",
        description: "See the CrossAgents GUI as it looks in your working directory: renders a scenario of the GUI's mock preview (apps/gui/src/dev/scenarios.ts, e.g. \"running\", \"review\", \"solo\") in a hidden window and returns a PNG. For checking GUI changes; add a scenario for a new screen first. Needs `npm --prefix apps/gui ci` once.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "scenario": { "type": "string", "description": "Scenario key; default \"running\"." },
                    "route": { "type": "string", "description": "Page to open instead of the scenario's own, e.g. \"#/usage\"." },
                    "width": { "type": "integer", "description": "Default 1280." },
                    "height": { "type": "integer", "description": "Default 800." },
                    "theme": { "type": "string", "enum": ["light", "dark"] },
                    "wait_ms": { "type": "integer", "description": "How long the page runs before the picture (its script plays meanwhile). Default 1500." }
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "checks_run",
        description: "Run a task's spec.acceptance commands and get the results.",
        groups: RUN,
        schema: || task_schema(json!({})),
    },
    ToolDef {
        name: "review_request",
        description: "Ask a reviewer (any session by name, else the task's or run's reviewer) for a verdict on a task reported done; its verdict comes back to you. A new execution session of the reviewer reviews by default; fresh: false has the reviewer itself do it, with its conversation.",
        groups: RUN,
        schema: || {
            task_schema(json!({
                "reviewer": { "type": "string" },
                "focus": { "type": "string" },
                "fresh": { "type": "boolean", "description": "false: the reviewer itself, with its conversation (default: a new execution session)" },
                "restore": { "type": "boolean", "description": "With fresh: false, restore the reviewer first if it is archived" }
            }))
        },
    },
    ToolDef {
        name: "task_accept",
        description: "Merge a task you issued into the run branch. Only a verified task is accepted plainly: every check passed, and nothing changed after they ran; only the verified commit is merged (nothing the worker wrote afterwards). Refused while the task is still being verified (force does not bypass that). Accepting without a verification - checks that failed or never ran, a task with no checks, a verification the hub restarted out of, a stalled task, or work done after the checks - takes force: true and a non-empty reason, which is recorded. On a conflict nothing changes and you get the conflicting files and hunks. message is the commit message, written the way the project's rules ask (default: \"<key>: <title>\").",
        groups: RUN,
        schema: || {
            task_schema(json!({
                "message": { "type": "string" },
                "force": { "type": "boolean", "description": "Accept although nothing verified it; needs reason, and is recorded" },
                "reason": { "type": "string", "description": "Why you accept it unverified (required with force)" }
            }))
        },
    },
    ToolDef {
        name: "task_verify",
        description: "Run a task's acceptance checks again on its current worktree, or for the first time after the hub restarted before they finished; the result comes back to you as a message. For a task whose report is ready (passed).",
        groups: RUN,
        schema: || task_schema(json!({})),
    },
    ToolDef {
        name: "task_rework",
        description: "Send a task you issued back to its worker with notes on what to fix.",
        groups: RUN,
        schema: || task_schema(json!({ "notes": { "type": "string" } })),
    },
    ToolDef {
        name: "task_drop",
        description: "Discard a task's result and branch. requeue starts it over (task_assign first for another worker); otherwise it is cancelled.",
        groups: RUN,
        schema: || {
            task_schema(json!({ "requeue": { "type": "boolean" }, "reason": { "type": "string" } }))
        },
    },
    ToolDef {
        name: "branch_sync",
        description: "Merge the run branch (work merged since the task started) into a task's worktree; returns conflicts if any.",
        groups: RUN,
        schema: || task_schema(json!({})),
    },
    ToolDef {
        name: "agent_spawn",
        description: "Recruit a teammate: a new agent you can then give work. As a run's lead, for the run (default plugin and model: the run's default worker's); in a conversation outside any run, for this conversation (default: your own plugin and model; it works in your folder and is archived with the conversation). Give a model (as agent_list or the person names it: haiku, sonnet, gpt-6-luna) to choose what it runs on; the plugin follows from the model, whichever vendor it is; name is what it is called (a person's name, not a model's; leave it out to get one). Optional: role (worker or reviewer), title, persona. The policy caps how many you may recruit and which models; when it refuses, the error says why: tell the person, do not substitute another agent and report it as done.",
        groups: RECRUITERS,
        schema: || {
            object(
                json!({
                    "name": { "type": "string" },
                    "role": { "type": "string", "enum": ["worker", "reviewer"] },
                    "plugin": { "type": "string" },
                    "model": { "type": "string" },
                    "display_name": { "type": "string" },
                    "title": { "type": "string" },
                    "persona": { "type": "string" }
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "agent_retire",
        description: "Retire a teammate you recruited (its conversation stays on record). In a run it must have no open tasks; in a conversation it is archived.",
        groups: RECRUITERS,
        schema: || object(json!({ "name": { "type": "string" } }), &["name"]),
    },
    ToolDef {
        name: "agent_set_model",
        description: "Change the model of a teammate in your run, e.g. move a stuck task's worker to a stronger model. Applies from its next turn.",
        groups: PLANNER,
        schema: || {
            object(
                json!({ "name": { "type": "string" }, "model": { "type": "string" } }),
                &["name", "model"],
            )
        },
    },
    ToolDef {
        name: "agent_interrupt",
        description: "Stop a teammate's current turn.",
        groups: PLANNER,
        schema: || object(json!({ "name": { "type": "string" } }), &["name"]),
    },
    ToolDef {
        name: "run_land",
        description: "Bring your run's branch (merged tasks and your own commits) into its base branch once every task has ended: mode merge (default), squash, or pr (opens a pull request). The run's policy decides whether you may; a dirty checkout of the base branch is never touched. message is the landing commit's message (or the pull request's title), written the way the project's rules ask (default: \"crossagents: <run title>\").",
        groups: PLANNER,
        schema: || {
            object(
                json!({
                    "mode": { "type": "string", "enum": ["merge", "squash", "pr"] },
                    "message": { "type": "string" }
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "run_finish",
        description: "Close your run with a summary for the person. What you changed yourself and did not commit is committed on the run branch with message (written the way the project's rules ask; default: the summary's first line). Tasks still open are refused unless cancel_open. A run that changed something must first pass the project's checks (crossagents.toml), run by the hub in your checkout; a failure is refused with their output. The person also sees every decision taken by default. A run that changed something can then be landed (`run_land`). Before closing, sort the problems recorded in the run (issue_pending, then lesson_create, lesson_merge or issue_keep); closing does not wait for it, and the answer says how many are left.",
        groups: PLANNER,
        schema: || {
            object(
                json!({
                    "summary": { "type": "string" },
                    "message": { "type": "string" },
                    "cancel_open": { "type": "boolean" },
                }),
                &["summary"],
            )
        },
    },
    ToolDef {
        name: "task_progress",
        description: "One line on how your task is going; shown on the task card.",
        groups: WORKER,
        schema: || task_schema(json!({ "text": { "type": "string" } })),
    },
    ToolDef {
        name: "task_complete",
        description: "Report your task done with a summary of what you changed. The hub commits your work, runs the task's checks and passes your summary to the task's issuer only once they are done, so the issuer sees checked work. Name the expertise entries you went by in `used`; the result of this task is counted on them.",
        groups: WORKER,
        schema: || {
            let mut schema = task_id_schema(
                json!({ "summary": { "type": "string", "description": "What you changed." } }),
            );
            schema["properties"]["result"] = json!({ "description": "Structured JSON result; required when the task has a result_schema." });
            // Optional, unlike the fields `task_id_schema` lists as required.
            schema["properties"]["used"] = json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "The expertise entries you went by: their ids, or the short tag a brief showed (the id's first 8 characters, e.g. 01a19b48). A tag that names no entry, or several, is skipped and comes back in unrecognized_used."
            });
            schema
        },
    },
    ToolDef {
        name: "task_fail",
        description: "Say you cannot finish your task, and why; the issuer decides what happens next.",
        groups: WORKER,
        schema: || task_id_schema(json!({ "reason": { "type": "string" } })),
    },
    ToolDef {
        name: "task_ask",
        description: "Ask the task's issuer a question; then end your turn, the answer arrives as a message.",
        groups: WORKER,
        schema: || task_id_schema(json!({ "question": { "type": "string" } })),
    },
    ToolDef {
        name: "review_submit",
        description: "Give your verdict on the change you are reviewing; it goes to whoever asked for the review. Grade each finding P0-P3 with a confidence and give a merge verdict: P0 blocks the merge and must be fixed, P1 should be fixed before it, P2 can follow, P3 is a suggestion. Answer the numbered checklist too when your brief has one: a fail proposes a replacement to the person.",
        groups: REVIEWER,
        schema: || {
            let mut schema = task_id_schema(json!({
                "approve": { "type": "boolean" },
                "notes": { "type": "string", "description": "For a rejection: exactly what to fix." }
            }));
            // Optional, unlike the fields `task_id_schema` lists as required.
            schema["properties"]["findings"] = json!({
                "type": "array",
                "description": "Problems found, most severe first when you list them. P0 blocks the merge and must be fixed; P1 should be fixed before it; P2 can follow; P3 is a suggestion.",
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": { "type": "string", "enum": ["P0", "P1", "P2", "P3"] },
                        "confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "How sure you are: a number from 0 (a guess) to 1 (certain), not high/medium/low." },
                        "file": { "type": "string", "description": "The file it is in, relative to the repository root." },
                        "line": { "type": "integer", "minimum": 1, "description": "The line on the new side of the diff." },
                        "title": { "type": "string", "description": "One line: what is wrong." },
                        "detail": { "type": "string", "description": "Why it matters, or how to fix it." }
                    },
                    "required": ["severity", "confidence", "title"]
                }
            });
            schema["properties"]["verdict"] = json!({
                "type": "string",
                "enum": ["merge", "merge_after_fixes", "do_not_merge"],
                "description": "Whether the change can merge. It must agree with approve (true is merge, false is do_not_merge) and with the findings (a P0 cannot merge). Omit it and approve decides."
            });
            schema["properties"]["checklist"] = json!({
                "type": "array",
                "description": "One verdict per numbered item of the reviewer checklist.",
                "items": {
                    "type": "object",
                    "properties": {
                        "entry_id": { "type": "string", "description": "The item's tag or entry id." },
                        "verdict": { "type": "string", "enum": ["pass", "fail", "na"] },
                        "note": { "type": "string", "description": "Why it failed, or anything to add." }
                    },
                    "required": ["entry_id", "verdict"]
                }
            });
            schema
        },
    },
    ToolDef {
        name: "extension_api",
        description: "Get the extension API, event types, groups, GUI slots, runnable example, stdin self-test and measured CLI MCP timeouts. Start here when creating a tool or panel.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "extension_check",
        description: "Statically check an extension manifest, schemas and entry files. Never executes extension code or enables it.",
        groups: ALL,
        schema: || object(json!({ "name": { "type": "string" } }), &["name"]),
    },
    ToolDef {
        name: "extension_request_enable",
        description: "Ask the hub to snapshot an extension and create a pending human approval record. Does not approve, enable or change auto-trust.",
        groups: ALL,
        schema: || object(json!({ "name": { "type": "string" } }), &["name"]),
    },
    ToolDef {
        name: "extension_logs",
        description: "Read the extension log tail (last 64 KiB), including stderr and call status.",
        groups: ALL,
        schema: || object(json!({ "name": { "type": "string" } }), &["name"]),
    },
    ToolDef {
        name: "extension_tools",
        description: "List the tools of enabled extensions that you may call right now, with their input schemas. A session that started before an extension was enabled does not see its tools in the tool list; this lists them, and `extension_call` runs one.",
        groups: ALL,
        schema: || object(json!({}), &[]),
    },
    ToolDef {
        name: "extension_call",
        description: "Call a tool of an enabled extension by its full name (`<extension>__<tool>`, see `extension_tools`). Same as calling the tool directly; use this when the tool is not in your tool list.",
        groups: ALL,
        schema: || {
            object(
                json!({
                    "tool": { "type": "string", "description": "Full tool name, e.g. standup__report." },
                    "input": { "type": "object", "description": "The tool's input, as its schema in `extension_tools` says." }
                }),
                &["tool"],
            )
        },
    },
];

pub(crate) fn args<T: DeserializeOwned>(value: Value) -> Result<T, CoreError> {
    serde_json::from_value(if value.is_null() { json!({}) } else { value })
        .map_err(|e| CoreError::Conflict(format!("invalid arguments: {e}")))
}

impl Orchestrator {
    /// The tools `agent` may call, by its permission groups.
    pub async fn tools_for(&self, agent: &Agent) -> Vec<ToolInfo> {
        let groups = self.groups(agent).await;
        let mut tools: Vec<ToolInfo> = TOOLS
            .iter()
            .filter(|t| t.groups.iter().any(|g| groups.contains(g)))
            .map(|t| ToolInfo {
                name: t.name.to_owned(),
                description: t.description.to_owned(),
                input_schema: (t.schema)(),
            })
            .collect();
        // The enabled extensions' tools, by the groups each declares (docs/EXTENSIONS.md §3).
        tools.extend(
            self.extension_tools_for(&groups)
                .into_iter()
                .map(|t| ToolInfo {
                    name: t.name,
                    description: t.description,
                    input_schema: t.input_schema,
                }),
        );
        tools
    }

    /// The enabled extensions' tools whose groups include one of `groups`. The list is only
    /// what is shown; the host checks again when the tool is called.
    pub(crate) fn extension_tools_for(
        &self,
        groups: &[Group],
    ) -> Vec<crate::host::ExtensionToolSpec> {
        let Some(host) = self.extensions.get() else {
            return Vec::new();
        };
        host.tools()
            .into_iter()
            .filter(|t| {
                t.groups
                    .iter()
                    .any(|g| groups.iter().any(|have| have.name() == g))
            })
            .collect()
    }

    /// Calls an extension tool as `caller`: who they are and their groups are read here, at
    /// call time, not taken from the request.
    async fn call_extension(
        &self,
        caller: &Agent,
        name: &str,
        input: Value,
    ) -> Result<Value, CoreError> {
        let host = self.extensions.get().ok_or_else(|| CoreError::NotFound {
            kind: "tool",
            id: name.to_owned(),
        })?;
        let groups = self.groups(caller).await;
        let claim = match self.store.claim_of(caller.id).await {
            Ok(claim) => claim,
            Err(err) => {
                tracing::warn!(agent = %caller.id, error = %err, "cannot read the agent's claim");
                None
            }
        };
        host.call(
            crate::host::ExtensionCaller {
                agent: caller.clone(),
                groups: groups.iter().map(|g| g.name().to_owned()).collect(),
                run: claim.as_ref().map(|c| c.run_id),
                task: claim.and_then(|c| c.task_id).map(|t| t.to_string()),
            },
            name,
            input,
        )
        .await
    }

    /// The extra instructions enabled extensions declare for `groups`, joined; empty when
    /// there are none. The caller appends them after everything the hub says.
    pub fn extension_prompts(&self, groups: &[Group]) -> String {
        let Some(host) = self.extensions.get() else {
            return String::new();
        };
        let names: Vec<&str> = groups.iter().map(|g| g.name()).collect();
        host.prompts(&names).join("\n\n")
    }

    pub async fn call_tool(
        self: &Arc<Self>,
        caller: &Agent,
        name: &str,
        input: Value,
    ) -> Result<Value, CoreError> {
        // `<extension>__<tool>`: no built-in name contains `__`.
        if name.contains("__") {
            return self.call_extension(caller, name, input).await;
        }
        let tool = TOOLS
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| CoreError::NotFound {
                kind: "tool",
                id: name.to_owned(),
            })?;
        let groups = self.groups(caller).await;
        if !tool.groups.iter().any(|g| groups.contains(g)) {
            return Err(CoreError::Conflict(format!(
                "{name} is not available to you (your groups: {})",
                groups
                    .iter()
                    .map(|g| g.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        match name {
            "whoami" => {
                let tools = self
                    .tools_for(caller)
                    .await
                    .into_iter()
                    .map(|t| t.name)
                    .collect();
                self.whoami(caller, tools).await
            }
            "agent_list" => self.agent_list(caller).await,
            "agent_get" => {
                #[derive(Deserialize)]
                struct A {
                    name: String,
                }
                let a: A = args(input)?;
                self.agent_get(&a.name).await
            }
            "agent_send" => self.agent_send(caller, input).await,
            "message_withdraw" => self.message_withdraw(caller, input).await,
            "remind_set" => self.remind_set(caller, input).await,
            "remind_list" => self.remind_list(caller).await,
            "remind_cancel" => self.remind_cancel(caller, input).await,
            "watch" => self.watch_add(caller, input).await,
            "watch_list" => self.watch_list(caller).await,
            "watch_cancel" => self.watch_cancel(caller, input).await,
            "note_write" => self.note_write(caller, input).await,
            "note_read" => self.note_read(caller, input).await,
            "note_list" => self.note_list(caller, input).await,
            "issue_pending" => self.issue_pending(caller, input).await,
            "lesson_create" | "lesson_merge" | "issue_keep" => {
                self.issue_sort(caller, name, input).await
            }
            "lesson_search" => self.lesson_search(caller, input).await,
            "lesson_get" => self.lesson_get(caller, input).await,
            "expertise_search" => self.expertise_search(caller, input).await,
            "expertise_get" => self.expertise_get(caller, input).await,
            "expertise_pending" => self.expertise_pending(caller).await,
            "expertise_triage" => self.expertise_triage(caller, input).await,
            "lesson_report" => self.lesson_report(caller, input).await,
            "usage_get" => self.usage_get(caller).await,
            "inbox_read" => self.inbox_read(caller).await,
            "human_ask" => self.human_ask(caller, input).await,
            "human_notify" => self.human_notify(caller, input).await,
            "work_start" => self.work_start(caller, input).await,
            "run_next" => self.run_next(caller, input).await,
            "plan_submit" => {
                #[derive(Deserialize)]
                struct A {
                    tasks: Vec<PlannedTask>,
                }
                let a: A = args(input)?;
                let run = self.submit_plan(caller, a.tasks).await?;
                let next = if run.status == RunStatus::AwaitingApproval {
                    "The plan waits for the person's approval. End your turn now."
                } else {
                    "The plan started at once (the run's policy). End your turn now."
                };
                Ok(
                    json!({ "run_id": run.id, "status": run.status, "tasks": run.plan.len(), "next": next }),
                )
            }
            "task_create" => self.task_create(caller, input).await,
            "task_list" => self.task_list(caller, input).await,
            "task_get" => self.task_get(caller, input).await,
            "task_update" => self.task_update(caller, input).await,
            "task_assign" => self.task_assign(caller, input).await,
            "task_cancel" => self.task_cancel(caller, input).await,
            "task_answer" => {
                #[derive(Deserialize)]
                struct A {
                    answer: String,
                }
                let task = self.task_ref(caller, &input).await?;
                let a: A = args(input)?;
                if self.answer(caller, task, a.answer).await? {
                    Ok(json!({
                        "delivered": true,
                        "read": "after_current_turn",
                        "note": "The worker is in a turn: your answer waits in its line and is read when that turn ends. Interrupting it (agent_send now) puts other messages ahead of the answer."
                    }))
                } else {
                    Ok(json!({ "delivered": true, "read": "now" }))
                }
            }
            "workspace_diff" => self.workspace_diff(caller, input).await,
            "workspace_read" => self.workspace_read(caller, input).await,
            "workspace_run" => self.workspace_run(caller, input).await,
            "checks_run" => self.checks_run(caller, input).await,
            "gui_screenshot" => self.gui_screenshot(caller, input).await,
            "review_request" => self.review_request(caller, input).await,
            "task_accept" => self.task_accept(caller, input).await,
            "task_verify" => self.task_verify(caller, input).await,
            "task_rework" => self.task_rework(caller, input).await,
            "task_drop" => self.task_drop(caller, input).await,
            "branch_sync" => self.branch_sync(caller, input).await,
            "task_progress" => self.task_progress(caller, input).await,
            "run_land" => self.run_land_tool(caller, input).await,
            "agent_spawn" => self.agent_spawn(caller, input).await,
            "agent_retire" => self.agent_retire(caller, input).await,
            "agent_set_model" => self.agent_set_model(caller, input).await,
            "agent_interrupt" => self.agent_interrupt(caller, input).await,
            "run_finish" => self.run_finish_tool(caller, input).await,
            "task_complete" => {
                #[derive(Deserialize)]
                struct A {
                    #[serde(default)]
                    summary: String,
                    /// The expertise entries the worker went by (EXPERTISE §4.5).
                    #[serde(default)]
                    used: Vec<String>,
                }
                let task = self.task_ref(caller, &input).await?;
                let result = input.get("result").cloned();
                let a: A = args(input)?;
                self.complete_with_result(caller, task, a.summary, result)
                    .await?;
                // Tags that named no entry of this project or the person come back, so the
                // worker can name them again properly. The report is already in: a store that
                // cannot record them must not make task_complete fail and be retried, so they
                // all count as unplaced.
                let (used, unrecognized) = match self.declare_used(caller, task, &a.used).await {
                    Ok(placed) => placed,
                    Err(err) => {
                        tracing::warn!(task = %task, error = %err, "cannot record declared expertise usage");
                        (0, a.used.clone())
                    }
                };
                Ok(json!({
                    "status": "verifying",
                    "used": used,
                    "unrecognized_used": unrecognized,
                    "next": "The hub commits your work and runs the task's checks; your summary reaches the task's issuer once they are done. End your turn; if it comes back with notes, continue and report again."
                }))
            }
            "task_fail" => {
                #[derive(Deserialize)]
                struct A {
                    reason: String,
                }
                let task = self.task_ref(caller, &input).await?;
                let a: A = args(input)?;
                self.give_up(caller, task, a.reason).await?;
                Ok(json!({ "status": "stalled", "next": "End your turn." }))
            }
            "task_ask" => {
                #[derive(Deserialize)]
                struct A {
                    question: String,
                }
                let task = self.task_ref(caller, &input).await?;
                let a: A = args(input)?;
                self.ask(caller, task, a.question).await
            }
            "review_submit" => {
                #[derive(Deserialize)]
                struct A {
                    approve: bool,
                    #[serde(default)]
                    notes: String,
                    #[serde(default)]
                    findings: Vec<crossagents_core::model::ReviewFinding>,
                    #[serde(default)]
                    verdict: Option<crossagents_core::model::ReviewVerdict>,
                    #[serde(default)]
                    checklist: Vec<crate::expertise::review::ChecklistItem>,
                }
                let id = self.task_ref(caller, &input).await?;
                let a: A = args(input)?;
                // Refuse a call whose approve and verdict disagree before anything is
                // recorded (docs/TODO.md §二十五 第 6 条).
                let verdict = crate::review::effective_verdict(a.approve, a.verdict, &a.findings)?;
                let findings = crate::review::sorted_findings(&a.findings);
                // The notes the issuer reads: the reviewer's words, then the verdict and
                // the findings, P0 first with the blocking ones (P0/P1) on top.
                let notes = crate::review::review_notes(
                    &a.notes,
                    &findings,
                    a.verdict.or((!findings.is_empty()).then_some(verdict)),
                );
                // Record the structured result while the task is still Reviewing: nobody
                // can accept it then, so this write cannot race an accept. `review` below
                // owns the status and reviewer checks and refuses a caller that may not
                // review; only touch the report when those checks would pass.
                let mut stored = self.store.get_task(id).await?;
                if stored.status == crossagents_core::TaskStatus::Reviewing
                    && stored.reviewer == Some(caller.id)
                {
                    stored.report.findings = findings;
                    stored.report.verdict = Some(verdict);
                    self.store.update_task(&stored).await?;
                }
                self.review(caller, id, a.approve, notes).await?;
                // A failed item proposes a replacement to the person (EXPERTISE §6.3). The
                // review is already in, so a store that cannot keep candidates must not make
                // this call fail and be retried.
                let candidates = match self.store.get_task(id).await {
                    Ok(task) => match self.review_candidates(caller, &task, &a.checklist).await {
                        Ok(candidates) => candidates.len(),
                        Err(err) => {
                            tracing::warn!(task = %id, error = %err, "cannot propose expertise candidates");
                            0
                        }
                    },
                    Err(err) => {
                        tracing::warn!(task = %id, error = %err, "cannot read the reviewed task");
                        0
                    }
                };
                Ok(json!({
                    "recorded": true,
                    "candidates": candidates,
                    "next": "End your turn."
                }))
            }
            "extension_api" | "extension_check" | "extension_request_enable" | "extension_logs" => {
                self.call_extension(caller, name, input).await
            }
            "extension_tools" => {
                let tools: Vec<Value> = self
                    .extension_tools_for(&groups)
                    .into_iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.input_schema,
                        })
                    })
                    .collect();
                Ok(json!({ "tools": tools }))
            }
            "extension_call" => {
                #[derive(Deserialize)]
                struct A {
                    tool: String,
                    #[serde(default)]
                    input: Value,
                }
                let a: A = args(input)?;
                if !a.tool.contains("__") {
                    return Err(CoreError::InvalidName(format!(
                        "{:?} is not an extension tool: the name is <extension>__<tool> (see extension_tools)",
                        a.tool
                    )));
                }
                self.call_extension(caller, &a.tool, a.input).await
            }
            _ => unreachable!("every tool in TOOLS is handled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_filtered_by_group() {
        let names = |group: Group| -> Vec<&str> {
            TOOLS
                .iter()
                .filter(|t| t.groups.contains(&Group::Everyone) || t.groups.contains(&group))
                .map(|t| t.name)
                .collect()
        };
        assert!(names(Group::Lead).contains(&"plan_submit"));
        assert!(!names(Group::Assignee).contains(&"plan_submit"));
        assert!(names(Group::Assignee).contains(&"task_complete"));
        let reviewer = names(Group::Reviewer);
        assert!(reviewer.contains(&"review_submit") && reviewer.contains(&"whoami"));
        assert!(!reviewer.contains(&"task_complete"));
        for tool in TOOLS {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
        }
    }
}

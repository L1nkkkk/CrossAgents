//! Collaboration tools (docs/DESIGN.md §9, docs/tasks/H1.md §5): definitions by permission
//! group and dispatch.

use std::sync::Arc;

use crossagents_core::{Agent, CoreError, PlannedTask, RunStatus, ToolInfo};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::messaging::{Group, groups_of};

const ALL: &[Group] = &[Group::Everyone];
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
            "assignee": { "type": "string", "description": "Name of the worker on your team whose specialty fits this task; omit for the default worker" },
            "reviewer": { "type": "string", "description": "Name of a reviewer on your team (e.g. a security reviewer)" }
        },
        "required": ["key", "title", "spec"]
    })
}

fn spec_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "properties": {
            "goal": { "type": "string" },
            "scope": {
                "type": "object",
                "properties": { "allow": strings, "deny": strings }
            },
            "interfaces": strings,
            "acceptance": { "type": "array", "items": { "type": "string" }, "description": "Optional shell commands the hub runs when the worker reports done." },
            "references": strings
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
        description: "Your teammates: name, title, specialty, role, status, model and current tasks.",
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
        description: "Send a message. `to` is a name, a list of names, or \"@team\" (everyone in your run). It arrives as a new turn, or after the recipient's current turn (`now: true` interrupts it instead). Use `reply_to` with a message id to answer a message; `task` names the task it is about. Returns the message ids. Retrying the same call is safe: identical messages within a minute, or the same idempotency_key, are sent once.",
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
                &["to", "text"],
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
        description: "Write a shared note under a key (replacing what was there): conventions, decisions, interfaces, pitfalls. scope \"run\" (default, this run) or \"team\" (lasts across runs). text null deletes it (author or a lead only).",
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
                    "title": { "type": "string", "description": "A short title for the card and the run." }
                }),
                &["request"],
            )
        },
    },
    ToolDef {
        name: "plan_submit",
        description: "Submit the plan for your current run: the full list of tasks for workers. Depending on the run's policy the person approves it first, or it starts at once.",
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
        description: "Add tasks to your running run (same fields as plan_submit's tasks). They start once their dependencies are merged. Keys already in the run are left as they are. If the run lets workers delegate, a worker can split parts off its own task this way: they branch from it and merge back into it, and the worker accepts them.",
        groups: ALL,
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
        groups: ALL,
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
        groups: ALL,
        schema: || task_schema(json!({})),
    },
    ToolDef {
        name: "task_update",
        description: "Change a task you issued: title, spec, depends_on. Its worker gets the new version.",
        groups: ALL,
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
        description: "Give a task you issued to another worker on the team (by name). A started task keeps its worktree; the new worker continues from it and gets a handoff card (files changed, last word, rework notes, open questions). reason says why it changes hands.",
        groups: ALL,
        schema: || {
            task_schema(json!({ "to": { "type": "string" }, "reason": { "type": "string" } }))
        },
    },
    ToolDef {
        name: "task_cancel",
        description: "Cancel a task you issued. keep_branch keeps its branch for inspection.",
        groups: ALL,
        schema: || task_schema(json!({ "keep_branch": { "type": "boolean" } })),
    },
    ToolDef {
        name: "task_answer",
        description: "Answer the open question on a task you issued (replying to the question message with agent_send works too).",
        groups: ALL,
        schema: || task_schema(json!({ "answer": { "type": "string" } })),
    },
    ToolDef {
        name: "workspace_diff",
        description: "What a task changed against the run branch, committed or not: a unified diff (paths to narrow it; stat_only for a summary).",
        groups: ALL,
        schema: || {
            task_schema(
                json!({ "paths": { "type": "array", "items": { "type": "string" } }, "stat_only": { "type": "boolean" } }),
            )
        },
    },
    ToolDef {
        name: "workspace_read",
        description: "Read a file in a task's worktree (path relative to it), optionally a line range.",
        groups: ALL,
        schema: || {
            task_schema(
                json!({ "path": { "type": "string" }, "from_line": { "type": "integer" }, "to_line": { "type": "integer" } }),
            )
        },
    },
    ToolDef {
        name: "workspace_run",
        description: "Run a shell command in a task's worktree (tests, builds) and get exit code and output. Default limit 300 s, at most 1800.",
        groups: ALL,
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
        groups: ALL,
        schema: || task_schema(json!({})),
    },
    ToolDef {
        name: "review_request",
        description: "Ask a reviewer (by name, else the task's or run's reviewer) for a verdict on a task reported done; its verdict comes back to you.",
        groups: ALL,
        schema: || {
            task_schema(json!({ "reviewer": { "type": "string" }, "focus": { "type": "string" } }))
        },
    },
    ToolDef {
        name: "task_accept",
        description: "Merge a task you issued into the run branch. On a conflict nothing changes and you get the conflicting files and hunks. message is the commit message, written the way the project's rules ask (default: \"<key>: <title>\").",
        groups: ALL,
        schema: || task_schema(json!({ "message": { "type": "string" } })),
    },
    ToolDef {
        name: "task_rework",
        description: "Send a task you issued back to its worker with notes on what to fix.",
        groups: ALL,
        schema: || task_schema(json!({ "notes": { "type": "string" } })),
    },
    ToolDef {
        name: "task_drop",
        description: "Discard a task's result and branch. requeue starts it over (task_assign first for another worker); otherwise it is cancelled.",
        groups: ALL,
        schema: || {
            task_schema(json!({ "requeue": { "type": "boolean" }, "reason": { "type": "string" } }))
        },
    },
    ToolDef {
        name: "branch_sync",
        description: "Merge the run branch (work merged since the task started) into a task's worktree; returns conflicts if any.",
        groups: ALL,
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
        description: "Close your run with a summary for the person. What you changed yourself and did not commit is committed on the run branch with message (written the way the project's rules ask; default: the summary's first line). Tasks still open are refused unless cancel_open. A run that changed something must first pass the project's checks (crossagents.toml), run by the hub in your checkout; a failure is refused with their output. The person also sees every decision taken by default. A run that changed something can then be landed (`run_land`).",
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
        description: "Report your task done with a summary of what you changed. The hub commits your work and passes your summary to the task's issuer, who checks it.",
        groups: WORKER,
        schema: || {
            task_id_schema(
                json!({ "summary": { "type": "string", "description": "What you changed." } }),
            )
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
        description: "Give your verdict on the change you are reviewing; it goes to whoever asked for the review.",
        groups: REVIEWER,
        schema: || {
            task_id_schema(json!({
                "approve": { "type": "boolean" },
                "notes": { "type": "string", "description": "For a rejection: exactly what to fix." }
            }))
        },
    },
];

pub(crate) fn args<T: DeserializeOwned>(value: Value) -> Result<T, CoreError> {
    serde_json::from_value(if value.is_null() { json!({}) } else { value })
        .map_err(|e| CoreError::Conflict(format!("invalid arguments: {e}")))
}

impl Orchestrator {
    /// The tools `agent` may call, by its permission groups.
    pub fn tools_for(&self, agent: &Agent) -> Vec<ToolInfo> {
        let groups = groups_of(agent);
        TOOLS
            .iter()
            .filter(|t| t.groups.iter().any(|g| groups.contains(g)))
            .map(|t| ToolInfo {
                name: t.name.to_owned(),
                description: t.description.to_owned(),
                input_schema: (t.schema)(),
            })
            .collect()
    }

    pub async fn call_tool(
        self: &Arc<Self>,
        caller: &Agent,
        name: &str,
        input: Value,
    ) -> Result<Value, CoreError> {
        let tool = TOOLS
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| CoreError::NotFound {
                kind: "tool",
                id: name.to_owned(),
            })?;
        let groups = groups_of(caller);
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
                let tools = self.tools_for(caller).into_iter().map(|t| t.name).collect();
                self.whoami(caller, tools).await
            }
            "agent_list" => self.agent_list().await,
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
            "usage_get" => self.usage_get(caller).await,
            "inbox_read" => self.inbox_read(caller).await,
            "human_ask" => self.human_ask(caller, input).await,
            "human_notify" => self.human_notify(caller, input).await,
            "work_start" => self.work_start(caller, input).await,
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
                self.answer(caller, task, a.answer).await?;
                Ok(json!({ "delivered": true }))
            }
            "workspace_diff" => self.workspace_diff(caller, input).await,
            "workspace_read" => self.workspace_read(caller, input).await,
            "workspace_run" => self.workspace_run(caller, input).await,
            "checks_run" => self.checks_run(caller, input).await,
            "gui_screenshot" => self.gui_screenshot(caller, input).await,
            "review_request" => self.review_request(caller, input).await,
            "task_accept" => self.task_accept(caller, input).await,
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
                }
                let task = self.task_ref(caller, &input).await?;
                let a: A = args(input)?;
                self.complete(caller, task, a.summary).await?;
                Ok(json!({
                    "status": "passed",
                    "next": "Your summary goes to the task's issuer, who checks the work. End your turn; if it comes back with notes, continue and report again."
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
                }
                let task = self.task_ref(caller, &input).await?;
                let a: A = args(input)?;
                self.review(caller, task, a.approve, a.notes).await?;
                Ok(json!({ "recorded": true, "next": "End your turn." }))
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

//! Collaboration tools (docs/DESIGN.md §9, docs/tasks/H1.md §5): definitions by permission
//! group and dispatch.

use std::sync::Arc;

use crossagents_core::{Agent, CoreError, PlannedTask, RunStatus, Task, TaskId, ToolInfo};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::messaging::{Group, groups_of};

const ALL: &[Group] = &[Group::Everyone];
const PLANNER: &[Group] = &[Group::Lead];
const WORKER: &[Group] = &[Group::Assignee];
const REVIEWER: &[Group] = &[Group::Reviewer];

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
            "acceptance": strings,
            "references": strings
        },
        "required": ["goal", "acceptance"]
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
        name: "plan_submit",
        description: "Submit the plan for your current run: the full list of tasks for workers. The human approves it before work starts.",
        groups: PLANNER,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "key": { "type": "string", "description": "Short unique id: a-z, 0-9, -" },
                                "title": { "type": "string" },
                                "spec": spec_schema(),
                                "depends_on": { "type": "array", "items": { "type": "string" } },
                                "assignee": { "type": "string", "description": "Name of the worker on your team whose specialty fits this task; omit for the default worker" },
                                "reviewer": { "type": "string", "description": "Name of a reviewer on your team to review this task (e.g. a security reviewer); omit for the run's reviewer" }
                            },
                            "required": ["key", "title", "spec"]
                        }
                    }
                },
                "required": ["tasks"]
            })
        },
    },
    ToolDef {
        name: "task_list",
        description: "List the tasks of your current run with their status.",
        groups: PLANNER,
        schema: || json!({ "type": "object", "properties": {} }),
    },
    ToolDef {
        name: "task_status",
        description: "Status, attempts and assignee of one task.",
        groups: PLANNER,
        schema: || task_id_schema(json!({})),
    },
    ToolDef {
        name: "task_read",
        description: "Full report of a task: spec, worker summary, acceptance output, diff stat, failure reason.",
        groups: PLANNER,
        schema: || task_id_schema(json!({})),
    },
    ToolDef {
        name: "task_answer",
        description: "Answer a worker's question about a task.",
        groups: PLANNER,
        schema: || task_id_schema(json!({ "answer": { "type": "string" } })),
    },
    ToolDef {
        name: "task_complete",
        description: "Report that your task is done. CrossAgents then commits your changes and runs the acceptance commands; you get a message if they fail.",
        groups: WORKER,
        schema: || {
            task_id_schema(
                json!({ "summary": { "type": "string", "description": "What you changed." } }),
            )
        },
    },
    ToolDef {
        name: "task_fail",
        description: "Give up on your task because it is impossible or the spec is wrong.",
        groups: WORKER,
        schema: || task_id_schema(json!({ "reason": { "type": "string" } })),
    },
    ToolDef {
        name: "task_ask",
        description: "Ask the planner a question about your task; the answer arrives as a message. End your turn after asking.",
        groups: WORKER,
        schema: || task_id_schema(json!({ "question": { "type": "string" } })),
    },
    ToolDef {
        name: "review_submit",
        description: "Approve or reject the change you are reviewing.",
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

fn task_id(raw: &str) -> Result<TaskId, CoreError> {
    raw.trim()
        .parse()
        .map_err(|_| CoreError::Conflict(format!("invalid task_id {raw:?}")))
}

#[derive(Deserialize)]
struct TaskArgs {
    task_id: String,
}

fn task_json(task: &Task) -> Value {
    json!({
        "task_id": task.id,
        "key": task.key,
        "title": task.title,
        "status": task.status,
        "attempts": task.attempts,
        "escalated": task.escalated,
        "depends_on": task.depends_on,
        "open_question": task.report.question,
    })
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
            "plan_submit" => {
                #[derive(Deserialize)]
                struct A {
                    tasks: Vec<PlannedTask>,
                }
                let a: A = args(input)?;
                let run = self.submit_plan(caller, a.tasks).await?;
                let next = if run.status == RunStatus::AwaitingApproval {
                    "The plan is waiting for the human's approval. End your turn now."
                } else {
                    "The plan was approved automatically and tasks are being dispatched. End your turn now."
                };
                Ok(
                    json!({ "run_id": run.id, "status": run.status, "tasks": run.plan.len(), "next": next }),
                )
            }
            "task_list" => {
                let runs = self.store().list_runs().await?;
                let run = runs
                    .into_iter()
                    .find(|r| r.planner == caller.id)
                    .ok_or_else(|| CoreError::Conflict("you lead no run".into()))?;
                let tasks: Vec<Value> = self
                    .store()
                    .list_tasks(run.id)
                    .await?
                    .iter()
                    .map(task_json)
                    .collect();
                Ok(json!({ "run_id": run.id, "run_status": run.status, "tasks": tasks }))
            }
            "task_status" | "task_read" => {
                let a: TaskArgs = args(input)?;
                let (run, task) = self.task_of(task_id(&a.task_id)?).await?;
                if run.planner != caller.id {
                    return Err(CoreError::Conflict(
                        "this task belongs to another planner's run".into(),
                    ));
                }
                if name == "task_status" {
                    Ok(task_json(&task))
                } else {
                    Ok(
                        json!({ "task": task_json(&task), "spec": task.spec, "report": task.report, "branch": task.branch }),
                    )
                }
            }
            "task_answer" => {
                #[derive(Deserialize)]
                struct A {
                    task_id: String,
                    answer: String,
                }
                let a: A = args(input)?;
                self.answer(caller, task_id(&a.task_id)?, a.answer).await?;
                Ok(json!({ "delivered": true }))
            }
            "task_complete" => {
                #[derive(Deserialize)]
                struct A {
                    task_id: String,
                    #[serde(default)]
                    summary: String,
                }
                let a: A = args(input)?;
                self.complete(caller, task_id(&a.task_id)?, a.summary)
                    .await?;
                Ok(json!({
                    "status": "verifying",
                    "next": "CrossAgents is running the acceptance commands. End your turn; if they fail you will get a message with the output."
                }))
            }
            "task_fail" => {
                #[derive(Deserialize)]
                struct A {
                    task_id: String,
                    reason: String,
                }
                let a: A = args(input)?;
                self.give_up(caller, task_id(&a.task_id)?, a.reason).await?;
                Ok(json!({ "status": "reported", "next": "End your turn." }))
            }
            "task_ask" => {
                #[derive(Deserialize)]
                struct A {
                    task_id: String,
                    question: String,
                }
                let a: A = args(input)?;
                self.ask(caller, task_id(&a.task_id)?, a.question).await?;
                Ok(
                    json!({ "status": "waiting_answer", "next": "End your turn; the answer will arrive as a message." }),
                )
            }
            "review_submit" => {
                #[derive(Deserialize)]
                struct A {
                    task_id: String,
                    approve: bool,
                    #[serde(default)]
                    notes: String,
                }
                let a: A = args(input)?;
                self.review(caller, task_id(&a.task_id)?, a.approve, a.notes)
                    .await?;
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

//! Collaboration tools (docs/DESIGN.md §9): definitions by role and dispatch.

use std::sync::Arc;

use crossagents_core::{
    Agent, AgentKey, CoreError, PlannedTask, Role, RunStatus, Task, TaskId, ToolInfo,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::engine::Orchestrator;

const ALL: &[Role] = &[Role::Planner, Role::Worker, Role::Reviewer];
const PLANNER: &[Role] = &[Role::Planner];
const WORKER: &[Role] = &[Role::Worker];
const REVIEWER: &[Role] = &[Role::Reviewer];

struct ToolDef {
    name: &'static str,
    description: &'static str,
    roles: &'static [Role],
    schema: fn() -> Value,
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
        name: "agent_list",
        description: "List the agents in this CrossAgents hub with their role and status.",
        roles: ALL,
        schema: || json!({ "type": "object", "properties": {} }),
    },
    ToolDef {
        name: "agent_send",
        description: "Send a message to another agent by name. It arrives as a new turn, or after the agent's current turn.",
        roles: ALL,
        schema: || {
            json!({
                "type": "object",
                "properties": { "to": { "type": "string" }, "text": { "type": "string" } },
                "required": ["to", "text"]
            })
        },
    },
    ToolDef {
        name: "plan_submit",
        description: "Submit the plan for your current run: the full list of tasks for workers. The human approves it before work starts.",
        roles: PLANNER,
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
                                "depends_on": { "type": "array", "items": { "type": "string" } }
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
        roles: PLANNER,
        schema: || json!({ "type": "object", "properties": {} }),
    },
    ToolDef {
        name: "task_status",
        description: "Status, attempts and assignee of one task.",
        roles: PLANNER,
        schema: || task_id_schema(json!({})),
    },
    ToolDef {
        name: "task_read",
        description: "Full report of a task: spec, worker summary, acceptance output, diff stat, failure reason.",
        roles: PLANNER,
        schema: || task_id_schema(json!({})),
    },
    ToolDef {
        name: "task_answer",
        description: "Answer a worker's question about a task.",
        roles: PLANNER,
        schema: || task_id_schema(json!({ "answer": { "type": "string" } })),
    },
    ToolDef {
        name: "task_complete",
        description: "Report that your task is done. CrossAgents then commits your changes and runs the acceptance commands; you get a message if they fail.",
        roles: WORKER,
        schema: || {
            task_id_schema(
                json!({ "summary": { "type": "string", "description": "What you changed." } }),
            )
        },
    },
    ToolDef {
        name: "task_fail",
        description: "Give up on your task because it is impossible or the spec is wrong.",
        roles: WORKER,
        schema: || task_id_schema(json!({ "reason": { "type": "string" } })),
    },
    ToolDef {
        name: "task_ask",
        description: "Ask the planner a question about your task; the answer arrives as a message. End your turn after asking.",
        roles: WORKER,
        schema: || task_id_schema(json!({ "question": { "type": "string" } })),
    },
    ToolDef {
        name: "review_submit",
        description: "Approve or reject the change you are reviewing.",
        roles: REVIEWER,
        schema: || {
            task_id_schema(json!({
                "approve": { "type": "boolean" },
                "notes": { "type": "string", "description": "For a rejection: exactly what to fix." }
            }))
        },
    },
];

fn args<T: DeserializeOwned>(value: Value) -> Result<T, CoreError> {
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
    pub fn tools_for(&self, role: Role) -> Vec<ToolInfo> {
        TOOLS
            .iter()
            .filter(|t| t.roles.contains(&role))
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
        if !tool.roles.contains(&caller.role) {
            return Err(CoreError::Conflict(format!(
                "{name} is not available to the {:?} role",
                caller.role
            )));
        }
        match name {
            "agent_list" => {
                let agents: Vec<Value> = self
                    .store()
                    .list_agents()
                    .await?
                    .into_iter()
                    .map(|a| json!({ "name": a.name, "role": a.role, "status": a.status, "plugin": a.plugin, "model": a.model }))
                    .collect();
                Ok(json!({ "agents": agents }))
            }
            "agent_send" => {
                #[derive(Deserialize)]
                struct A {
                    to: String,
                    text: String,
                }
                let a: A = args(input)?;
                let target = self.store().get_agent(&AgentKey::Name(a.to)).await?;
                self.deliver(
                    target.id,
                    format!("Message from {}:\n{}", caller.name, a.text),
                );
                Ok(json!({ "sent": true }))
            }
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
    fn tools_are_filtered_by_role() {
        let names = |role: Role| -> Vec<&str> {
            TOOLS
                .iter()
                .filter(|t| t.roles.contains(&role))
                .map(|t| t.name)
                .collect()
        };
        assert!(names(Role::Planner).contains(&"plan_submit"));
        assert!(!names(Role::Worker).contains(&"plan_submit"));
        assert!(names(Role::Worker).contains(&"task_complete"));
        assert_eq!(
            names(Role::Reviewer),
            ["agent_list", "agent_send", "review_submit"]
        );
        for tool in TOOLS {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
        }
    }
}

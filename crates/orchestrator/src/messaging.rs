//! Identity, messaging, reminders and watches (docs/tasks/H1.md §5.1–5.3): the tools every
//! agent has. The hub only delivers and records; what a message says is the sender's.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossagents_core::{
    Agent, AgentId, AgentKey, CoreError, Envelope, MessageId, MessageSource, Reminder, Role, Run,
    Task, TaskId, TaskStatus, Watch, WatchOn, WatchTarget,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;

/// Identical messages to the same recipient within this window are sent once.
const DEDUP_WINDOW: Duration = Duration::from_secs(60);
/// How long idempotency keys and sent-message records are kept.
const KEEP: Duration = Duration::from_secs(3600);

/// Permission groups (docs/tasks/H1.md §3). Until tasks name their issuers (H1-3) they
/// follow the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Everyone,
    Lead,
    Assignee,
    Reviewer,
}

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::Lead => "lead",
            Self::Assignee => "assignee",
            Self::Reviewer => "reviewer",
        }
    }
}

pub fn groups_of(agent: &Agent) -> Vec<Group> {
    let mut groups = vec![Group::Everyone];
    groups.push(match agent.role {
        Role::Planner => Group::Lead,
        Role::Worker => Group::Assignee,
        Role::Reviewer => Group::Reviewer,
    });
    groups
}

/// Memory for retries: idempotency keys, recent messages, and who sent what.
#[derive(Default)]
pub(crate) struct MessagingState {
    /// (sender, key) -> the first call's result.
    keys: HashMap<(AgentId, String), (Instant, Value)>,
    /// (sender, recipient, text) -> the message sent.
    recent: HashMap<(AgentId, AgentId, String), (Instant, MessageId)>,
    /// message -> (sender, recipient), for withdrawing.
    sent: HashMap<MessageId, (Instant, AgentId, AgentId)>,
}

impl MessagingState {
    fn prune(&mut self) {
        let now = Instant::now();
        self.keys.retain(|_, (at, _)| now - *at < KEEP);
        self.recent.retain(|_, (at, _)| now - *at < DEDUP_WINDOW);
        self.sent.retain(|_, (at, _, _)| now - *at < KEEP);
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum To {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct SendArgs {
    to: To,
    text: String,
    #[serde(default)]
    reply_to: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    now: bool,
    #[serde(default)]
    idempotency_key: Option<String>,
}

fn parse_message_id(raw: &str) -> Result<MessageId, CoreError> {
    raw.trim()
        .parse()
        .map_err(|_| CoreError::Conflict(format!("invalid message id {raw:?}")))
}

fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let mut out: String = line.chars().take(max).collect();
    if out.len() < line.len() {
        out.push('…');
    }
    out
}

fn task_brief(task: &Task) -> Value {
    json!({ "task_id": task.id, "key": task.key, "title": task.title, "status": task.status })
}

fn ended(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Merged | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

impl Orchestrator {
    /// The run an agent belongs to now: the one it plans, else the one with a task it works
    /// on or reviews. Active runs first.
    pub(crate) async fn run_of(
        &self,
        agent: AgentId,
    ) -> Result<Option<(Run, Vec<Task>)>, CoreError> {
        let mut found = None;
        for run in self.store().list_runs().await? {
            let tasks = self.store().list_tasks(run.id).await?;
            let member = run.planner == agent
                || tasks
                    .iter()
                    .any(|t| t.assignee == Some(agent) || t.reviewer == Some(agent));
            if !member {
                continue;
            }
            let active = !matches!(
                run.status,
                crossagents_core::RunStatus::Completed
                    | crossagents_core::RunStatus::Failed
                    | crossagents_core::RunStatus::Cancelled
            );
            if active {
                return Ok(Some((run, tasks)));
            }
            found.get_or_insert((run, tasks));
        }
        Ok(found)
    }

    pub(crate) async fn whoami(
        &self,
        caller: &Agent,
        tools: Vec<String>,
    ) -> Result<Value, CoreError> {
        let run = self.run_of(caller.id).await?;
        let (run_json, assigned, issued) = match &run {
            Some((run, tasks)) => (
                json!({ "run_id": run.id, "title": run.title, "status": run.status }),
                tasks
                    .iter()
                    .filter(|t| t.assignee == Some(caller.id) || t.reviewer == Some(caller.id))
                    .map(task_brief)
                    .collect::<Vec<_>>(),
                if run.planner == caller.id {
                    tasks.iter().map(task_brief).collect()
                } else {
                    Vec::new()
                },
            ),
            None => (Value::Null, Vec::new(), Vec::new()),
        };
        Ok(json!({
            "name": caller.name,
            "display_name": caller.profile.display_name,
            "title": caller.profile.title,
            "role": caller.role,
            "groups": groups_of(caller).iter().map(|g| g.name()).collect::<Vec<_>>(),
            "plugin": caller.plugin,
            "model": caller.model,
            "cwd": caller.cwd,
            "run": run_json,
            "tasks_assigned": assigned,
            "tasks_issued": issued,
            "tools": tools,
        }))
    }

    /// Current task keys by agent, across active runs.
    async fn current_tasks(&self) -> Result<HashMap<AgentId, Vec<String>>, CoreError> {
        let mut by_agent: HashMap<AgentId, Vec<String>> = HashMap::new();
        for run in self.store().list_runs().await? {
            for task in self.store().list_tasks(run.id).await? {
                if ended(task.status) {
                    continue;
                }
                for agent in [task.assignee, task.reviewer].into_iter().flatten() {
                    by_agent.entry(agent).or_default().push(task.key.clone());
                }
            }
        }
        Ok(by_agent)
    }

    pub(crate) async fn agent_list(&self) -> Result<Value, CoreError> {
        let tasks = self.current_tasks().await?;
        let agents: Vec<Value> = self
            .store()
            .list_agents()
            .await?
            .into_iter()
            .map(|a| {
                json!({
                    "name": a.name,
                    "display_name": a.profile.display_name,
                    "title": a.profile.title,
                    "specialty": a.profile.persona.as_deref().map(|p| first_line(p, 160)),
                    "role": a.role,
                    "status": a.status,
                    "plugin": a.plugin,
                    "model": a.model,
                    "current_tasks": tasks.get(&a.id).cloned().unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({ "agents": agents }))
    }

    pub(crate) async fn agent_get(&self, name: &str) -> Result<Value, CoreError> {
        let agent = self.store().get_agent(&AgentKey::parse(name)).await?;
        let tasks = self.current_tasks().await?;
        Ok(json!({
            "name": agent.name,
            "profile": agent.profile,
            "role": agent.role,
            "status": agent.status,
            "plugin": agent.plugin,
            "driver": agent.driver,
            "model": agent.model,
            "cwd": agent.cwd,
            "current_tasks": tasks.get(&agent.id).cloned().unwrap_or_default(),
            "created_at": agent.created_at,
        }))
    }

    /// `agent_send`: one or more recipients, `@team`, replies, retries collapsed.
    pub(crate) async fn agent_send(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let a: SendArgs = crate::tools::args(input)?;
        let mut state = self.messaging.lock().await;
        state.prune();
        if let Some(key) = &a.idempotency_key
            && let Some((_, result)) = state.keys.get(&(caller.id, key.clone()))
        {
            let mut result = result.clone();
            result["repeated"] = json!(true);
            return Ok(result);
        }
        drop(state);

        let names = match a.to {
            To::One(name) if name == "@team" => self.team_names(caller).await?,
            To::One(name) => vec![name],
            To::Many(names) => names,
        };
        if names.is_empty() {
            return Err(CoreError::Conflict(
                "no recipients (@team is empty outside a run)".into(),
            ));
        }
        let reply_to = a.reply_to.as_deref().map(parse_message_id).transpose()?;
        // A reply to the message that carried a task question answers it.
        if let Some(parent) = reply_to {
            self.reply_closes_question(caller, parent).await;
        }
        let task = match &a.task {
            Some(key) => Some(self.find_task(caller, key).await?),
            None => None,
        };
        // A message about a task still comes from the agent; the task is context.
        let from = MessageSource::Agent {
            agent_id: caller.id,
        };
        let header = match &task {
            Some(task) => format!("Message from {} about #{}:", caller.name, task.key),
            None => format!("Message from {}:", caller.name),
        };
        let mut sent = Vec::new();
        let mut repeated = true;
        for name in names {
            let target = self
                .store()
                .get_agent(&AgentKey::Name(name.clone()))
                .await?;
            if target.id == caller.id {
                return Err(CoreError::Conflict("you cannot message yourself".into()));
            }
            let mut state = self.messaging.lock().await;
            let dedup = (caller.id, target.id, a.text.clone());
            if let Some((_, id)) = state.recent.get(&dedup) {
                sent.push(json!({ "to": name, "message_id": id }));
                continue;
            }
            repeated = false;
            let envelope = Envelope {
                id: MessageId::new(),
                from,
                text: format!("{header}\n{}", a.text),
                reply_to,
            };
            let id = envelope.id;
            state.recent.insert(dedup, (Instant::now(), id));
            state
                .sent
                .insert(id, (Instant::now(), caller.id, target.id));
            drop(state);
            let host = self.host.clone();
            let now = a.now;
            tokio::spawn(async move {
                if let Err(err) = host.send_envelope(target.id, envelope, now).await {
                    tracing::warn!(agent = %target.id, error = %err, "cannot deliver message");
                }
            });
            sent.push(json!({ "to": name, "message_id": id }));
        }
        let result = json!({ "messages": sent, "repeated": repeated });
        if let Some(key) = a.idempotency_key {
            self.messaging
                .lock()
                .await
                .keys
                .insert((caller.id, key), (Instant::now(), result.clone()));
        }
        Ok(result)
    }

    /// Everyone in the caller's run but the caller, by name.
    async fn team_names(&self, caller: &Agent) -> Result<Vec<String>, CoreError> {
        let Some((run, tasks)) = self.run_of(caller.id).await? else {
            return Ok(Vec::new());
        };
        let mut ids = vec![run.planner];
        for task in &tasks {
            ids.extend([task.assignee, task.reviewer].into_iter().flatten());
        }
        let mut names = Vec::new();
        for id in ids {
            if id == caller.id {
                continue;
            }
            if let Ok(agent) = self.store().get_agent(&AgentKey::Id(id)).await
                && !names.contains(&agent.name)
            {
                names.push(agent.name);
            }
        }
        Ok(names)
    }

    /// A task by id, or by key (`auth` or `#auth`) in the caller's run.
    pub(crate) async fn find_task(&self, caller: &Agent, key: &str) -> Result<Task, CoreError> {
        let key = key.trim().trim_start_matches('#');
        if let Ok(id) = key.parse::<TaskId>() {
            return self.store().get_task(id).await;
        }
        let Some((_, tasks)) = self.run_of(caller.id).await? else {
            return Err(CoreError::NotFound {
                kind: "task",
                id: key.to_owned(),
            });
        };
        tasks
            .into_iter()
            .find(|t| t.key == key)
            .ok_or_else(|| CoreError::NotFound {
                kind: "task",
                id: key.to_owned(),
            })
    }

    pub(crate) async fn message_withdraw(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            message_id: String,
        }
        let a: A = crate::tools::args(input)?;
        let id = parse_message_id(&a.message_id)?;
        let recipient = match self.messaging.lock().await.sent.get(&id) {
            Some((_, sender, recipient)) if *sender == caller.id => *recipient,
            Some(_) => return Err(CoreError::Conflict("that message is not yours".into())),
            None => {
                return Err(CoreError::Conflict(
                    "unknown message (only messages you sent in the last hour can be withdrawn)"
                        .into(),
                ));
            }
        };
        self.host.withdraw(recipient, id).await?;
        Ok(json!({ "withdrawn": true }))
    }

    pub(crate) async fn remind_set(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            after_minutes: Option<f64>,
            #[serde(default)]
            at: Option<DateTime<Utc>>,
            note: String,
            #[serde(default)]
            task: Option<String>,
            #[serde(default)]
            only_if_open: bool,
        }
        let a: A = crate::tools::args(input)?;
        let due_at = match (a.after_minutes, a.at) {
            (Some(m), None) if m > 0.0 => Utc::now() + chrono::Duration::seconds((m * 60.0) as i64),
            (None, Some(at)) => at,
            _ => {
                return Err(CoreError::Conflict(
                    "give exactly one of after_minutes (> 0) or at (RFC 3339)".into(),
                ));
            }
        };
        let existing = self.store().list_reminders(Some(caller.id)).await?;
        if existing.len() >= crossagents_core::notify::MAX_PER_AGENT {
            return Err(CoreError::Conflict(format!(
                "you already have {} reminders; cancel some first: {}",
                existing.len(),
                existing
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let task = match &a.task {
            Some(key) => Some(self.find_task(caller, key).await?.id),
            None => None,
        };
        // The same reminder set twice (a retried call) is one reminder.
        if let Some(same) = existing.iter().find(|r| {
            r.note == a.note && r.task == task && (r.due_at - due_at).num_seconds().abs() < 60
        }) {
            return Ok(json!({ "reminder": same, "repeated": true }));
        }
        let reminder = Reminder {
            id: format!("r-{}", short_id()),
            owner: caller.id,
            due_at,
            note: a.note,
            task,
            only_if_open: a.only_if_open && task.is_some(),
            created_at: Utc::now(),
        };
        self.store().insert_reminder(&reminder).await?;
        Ok(json!({ "reminder": reminder }))
    }

    pub(crate) async fn remind_list(&self, caller: &Agent) -> Result<Value, CoreError> {
        Ok(json!({ "reminders": self.store().list_reminders(Some(caller.id)).await? }))
    }

    pub(crate) async fn remind_cancel(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            id: String,
        }
        let a: A = crate::tools::args(input)?;
        let mine = self.store().list_reminders(Some(caller.id)).await?;
        if !mine.iter().any(|r| r.id == a.id) {
            // Already gone (fired, or a retried cancel): nothing to do.
            return Ok(json!({ "cancelled": false, "repeated": true }));
        }
        self.store().delete_reminder(&a.id).await?;
        Ok(json!({ "cancelled": true }))
    }

    pub(crate) async fn watch_add(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            task: Option<String>,
            #[serde(default)]
            agent: Option<String>,
            #[serde(default)]
            on: Vec<WatchOn>,
        }
        let a: A = crate::tools::args(input)?;
        let target = match (a.task, a.agent) {
            (Some(task), None) => WatchTarget::Task {
                task_id: self.find_task(caller, &task).await?.id,
            },
            (None, Some(name)) => WatchTarget::Agent {
                agent_id: self.store().get_agent(&AgentKey::parse(&name)).await?.id,
            },
            _ => {
                return Err(CoreError::Conflict(
                    "give exactly one of task or agent".into(),
                ));
            }
        };
        let on = if a.on.is_empty() {
            vec![WatchOn::Any]
        } else {
            a.on
        };
        let existing = self.store().list_watches(Some(caller.id)).await?;
        if let Some(same) = existing.iter().find(|w| w.target == target && w.on == on) {
            return Ok(json!({ "watch": same, "repeated": true }));
        }
        if existing.len() >= crossagents_core::notify::MAX_PER_AGENT {
            return Err(CoreError::Conflict(format!(
                "you already watch {} things; remove some with watch_cancel first",
                existing.len()
            )));
        }
        let watch = Watch {
            id: format!("w-{}", short_id()),
            owner: caller.id,
            target,
            on,
            created_at: Utc::now(),
        };
        self.store().insert_watch(&watch).await?;
        Ok(json!({ "watch": watch }))
    }

    pub(crate) async fn watch_list(&self, caller: &Agent) -> Result<Value, CoreError> {
        Ok(json!({ "watches": self.store().list_watches(Some(caller.id)).await? }))
    }

    pub(crate) async fn watch_cancel(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            id: String,
        }
        let a: A = crate::tools::args(input)?;
        let mine = self.store().list_watches(Some(caller.id)).await?;
        if !mine.iter().any(|w| w.id == a.id) {
            return Ok(json!({ "cancelled": false, "repeated": true }));
        }
        self.store().delete_watch(&a.id).await?;
        Ok(json!({ "cancelled": true }))
    }
}

fn short_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    id[..10].to_owned()
}

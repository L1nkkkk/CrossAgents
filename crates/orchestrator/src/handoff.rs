//! Handoff cards (docs/research/magpie-lancer.md §2.1): each time a task changes hands — its
//! first worker, `task_assign`, a quota reroute, an escalation — the hub records what went
//! with it on the task and in the event stream, before the new worker hears of it. What the
//! worker before left is read before it goes; a part that cannot be read stays empty and
//! never holds up the handoff.
//!
//! A task may also say where its work goes (docs/research/multica.md §2.12): `spec.next` is
//! the step after it — a task key the hub releases once the task is accepted, or a teammate
//! for whom the hub opens that step — and `spec.on_reject` is where work goes back when it is
//! sent back: a step, whose worker takes it again, or a teammate. Both are optional and a
//! task without them changes hands exactly as it did before.

use std::collections::HashMap;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, CoreError, EventKind, Handoff, HandoffId, HandoffKind, PlannedTask,
    Role, Run, Task, TaskId, TaskReport, TaskStatus,
};
use serde_json::{Value, json};

use crate::brief;
use crate::engine::{Orchestrator, worker_name};

/// Where a send-back would go, worked out before it happens.
enum Route {
    /// To a fresh execution session of this teammate: the step's worker, or the named one.
    Elsewhere(Box<Agent>, Target),
    /// To the worker the task already has.
    Own(Target),
    /// No route is set, or the run has no such step and no such teammate.
    None,
}

/// What the one handing the task over leaves with it.
#[derive(Debug, Default)]
pub(crate) struct Left {
    from: Option<AgentId>,
    from_name: Option<String>,
    files: Vec<String>,
    summary: Option<String>,
    open_questions: Vec<String>,
    context_tokens: Option<u64>,
}

/// A place a handoff goes, as `spec.next` / `spec.on_reject` write it: a step of the run's
/// plan (a task key) or a teammate (a name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A step of the run's plan, by task key.
    Step(String),
    /// A teammate of the run, by name.
    Teammate(String),
}

impl Target {
    /// Reads `raw` against `tasks`: `#b` always names step b; a bare name is a step when the
    /// run has one with that key, and a teammate otherwise (the plan's own keys are what a
    /// workflow is written in, so they win).
    pub fn of(raw: &str, tasks: &[Task]) -> Option<Self> {
        let raw = raw.trim();
        let (key, forced) = match raw.strip_prefix('#') {
            Some(key) => (key.trim(), true),
            None => (raw, false),
        };
        if key.is_empty() {
            return None;
        }
        if forced || tasks.iter().any(|t| t.key == key) {
            return Some(Self::Step(key.to_owned()));
        }
        Some(Self::Teammate(key.to_owned()))
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Step(name) | Self::Teammate(name) => name,
        }
    }
}

/// The step `spec.next` names, as written; `None` when it sets none.
pub fn next_of(task: &Task) -> Option<&str> {
    task.spec
        .next
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Where `spec.on_reject` sends a task back, as written; `None` when it sets none.
pub fn on_reject_of(task: &Task) -> Option<&str> {
    task.spec
        .on_reject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// What `spec.next` came to once a task was accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextStep {
    /// The step that is next, by task key.
    pub step: String,
    /// Who works it, as a person reads it.
    pub to: String,
    /// `true` when the hub opened the step: it was not in the plan.
    pub opened: bool,
    /// What the hub did about it, one line.
    pub about: String,
}

impl NextStep {
    pub fn json(&self) -> Value {
        json!({
            "step": self.step,
            "to": self.to,
            "opened": self.opened,
            "about": self.about,
        })
    }
}

/// `base`, or `base-2`, `base-3`… when the run already has a task with that key. Cut to the
/// longest key a plan allows, as `plan::validate` has it.
fn free_key(tasks: &[Task], base: &str) -> String {
    const KEY_MAX: usize = 24;
    let mut base = base.to_owned();
    base.truncate(KEY_MAX);
    if !tasks.iter().any(|t| t.key == base) {
        return base;
    }
    for n in 2..1000 {
        let suffix = format!("-{n}");
        let mut head = base.clone();
        head.truncate(KEY_MAX - suffix.len());
        let candidate = format!("{}{suffix}", head.trim_end_matches('-'));
        if !tasks.iter().any(|t| t.key == candidate) {
            return candidate;
        }
    }
    base
}

/// How many times the same work may go back to the same step or teammate before the hub
/// stops: a bounce that would go on for ever is left with the lead
/// (docs/research/multica.md §2.12).
pub const MAX_BOUNCES: u32 = 3;

/// A task as a plan would have written it, for checking the routing of a whole run.
pub(crate) fn planned(task: &Task) -> PlannedTask {
    PlannedTask {
        key: task.key.clone(),
        title: task.title.clone(),
        spec: task.spec.clone(),
        depends_on: task.depends_on.clone(),
        assignee: None,
        reviewer: None,
        fresh: true,
        restore: false,
        reviewer_fresh: true,
    }
}

impl Orchestrator {
    /// A first assignment: from the issuer, with the files the spec allows.
    pub(crate) async fn left_by_issuer(&self, run: &Run, task: &Task) -> Left {
        let issuer = task.issuer_or(run.planner);
        Left {
            from: Some(issuer),
            from_name: self.name_of(issuer).await,
            files: task.spec.scope.allow.clone(),
            ..Left::default()
        }
    }

    /// What the task's worker until now (`worker`) leaves: the files it changed, its last
    /// word, the question it is still waiting on. Read before the worker is removed and
    /// before the task is changed for the next one.
    pub(crate) async fn left_by_worker(
        &self,
        run: &Run,
        task: &Task,
        worker: Option<AgentId>,
    ) -> Left {
        let files = match self.changed_files(run, task).await {
            Ok(files) => files,
            Err(err) => {
                tracing::warn!(task = %task.key, error = %err, "cannot list the files a handoff carries");
                Vec::new()
            }
        };
        // Who the person knew the worker as: the session the last handoff named, else the
        // one it was cloned from.
        let from_name = match task.handoffs.last() {
            Some(last) => Some(last.to_name.clone()),
            None => {
                self.name_of(task.worker_template.unwrap_or(run.config.worker))
                    .await
            }
        };
        // Its own last word: a handoff clears what the worker before said, and the progress
        // it leaves (why the task changed hands) is the hub's, not this worker's.
        let said_by_hub = task.handoffs.last().and_then(|h| h.reason.as_ref());
        let progress = task.progress.clone().filter(|p| Some(p) != said_by_hub);
        Left {
            from: worker,
            from_name,
            files,
            summary: task.report.summary.clone().or(progress),
            open_questions: task.report.question.iter().cloned().collect(),
            context_tokens: worker.and_then(|w| self.context_of(w)),
        }
    }

    async fn changed_files(&self, run: &Run, task: &Task) -> Result<Vec<String>, CoreError> {
        let dir = task
            .worktree
            .clone()
            .ok_or_else(|| CoreError::Conflict(format!("#{} has no worktree", task.key)))?;
        if !dir.exists() {
            return Err(CoreError::Conflict(format!(
                "the worktree of #{} is gone: {}",
                task.key,
                dir.display()
            )));
        }
        let (base, _) = self.base_of(run, task).await?;
        self.ws_for(run).changed_files(&dir, &base).await
    }

    /// Records the card on `task` and announces it. Called once `worker` has the task and
    /// before its first message is sent. `template` is the teammate `worker` was cloned
    /// from, `None` when `worker` took the task itself.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn hand_off(
        &self,
        run: &Run,
        task: &mut Task,
        kind: HandoffKind,
        left: Left,
        worker: &Agent,
        template: Option<String>,
        reason: Option<String>,
    ) -> Result<Handoff, CoreError> {
        let decisions = task
            .spec
            .interfaces
            .iter()
            .chain(&task.report.rework)
            .cloned()
            .collect();
        let card = Handoff {
            id: HandoffId::new(),
            run_id: run.id,
            task_id: task.id,
            task_key: task.key.clone(),
            kind,
            from: left.from,
            from_name: left.from_name,
            to: worker.id,
            // The name to message it by; where it came from goes alongside.
            to_name: worker.name.clone(),
            template: template.filter(|t| *t != worker.name),
            reason,
            goal: task.spec.goal.clone(),
            files: left.files,
            summary: left.summary,
            decisions,
            open_questions: left.open_questions,
            context_tokens: left.context_tokens,
            // What holds it says how it was taken: started for this, or itself.
            fresh: self
                .store
                .claim_of(worker.id)
                .await?
                .is_none_or(|c| c.fresh),
            outcome: None,
            at: Utc::now(),
        };
        task.handoffs.push(card.clone());
        // What the worker before reported is on the card now; left on the task, it would read
        // as the new worker's (and travel on the next card under its name). Until the new
        // one says something, the task's line says why it changed hands.
        task.report.summary = None;
        task.report.failure = None;
        task.progress = card.reason.clone();
        task.updated_at = Utc::now();
        self.store.update_task(task).await?;
        self.publish(EventKind::TaskHandedOff {
            run_id: run.id,
            handoff: card.clone(),
        })
        .await;
        Ok(card)
    }

    pub(crate) async fn name_of(&self, agent: AgentId) -> Option<String> {
        self.store
            .get_agent(&AgentKey::Id(agent))
            .await
            .ok()
            .map(|a| a.name)
    }

    // -----------------------------------------------------------------------------------
    // Workflow handoffs (docs/research/multica.md §2.12)

    /// Checks the routing a plan or a task sets, before anything it says takes effect: a
    /// `next` or `on_reject` that names no step of the run (or of the plan) and no session of
    /// the workspace, one that names the task itself, and a `next` step in an earlier stage —
    /// which the stage gate would never release. `Err` lists every problem, so one answer
    /// fixes them all.
    pub(crate) async fn check_routes(&self, tasks: &[PlannedTask]) -> Result<(), String> {
        let stages: HashMap<&str, Option<u32>> = tasks
            .iter()
            .map(|t| (t.key.as_str(), t.spec.stage))
            .collect();
        let mut problems = Vec::new();
        for task in tasks {
            for (field, raw) in [
                ("next", task.spec.next.as_deref()),
                ("on_reject", task.spec.on_reject.as_deref()),
            ] {
                let Some(raw) = raw.map(str::trim).filter(|n| !n.is_empty()) else {
                    continue;
                };
                let name = raw.strip_prefix('#').map(str::trim).unwrap_or(raw);
                if name == task.key {
                    problems.push(format!(
                        "task {:?}: {field} names the task itself",
                        task.key
                    ));
                    continue;
                }
                match stages.get(name) {
                    Some(step_stage) => {
                        if field == "next"
                            && let (Some(own), Some(step)) = (task.spec.stage, *step_stage)
                            && step < own
                        {
                            problems.push(format!(
                                "task {:?} (stage {own}): next {name:?} is in stage {step}, which starts only before stage {own}",
                                task.key
                            ));
                        }
                    }
                    None => {
                        if self
                            .store
                            .get_agent(&AgentKey::Name(name.to_owned()))
                            .await
                            .is_err()
                        {
                            problems.push(format!(
                                "task {:?}: {field} names no task and no session {name:?} (agent_list lists the sessions)",
                                task.key
                            ));
                        }
                    }
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("\n"))
        }
    }

    /// Whether the same work has gone back to `target` [`MAX_BOUNCES`] times already: then
    /// it stops there, with the lead told why.
    async fn bounce_stopped(&self, run: &Run, task: &Task, target: &Target) -> bool {
        let key = (task.id, target.name().to_owned());
        let count = self
            .bounces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .copied()
            .unwrap_or(0);
        if count < MAX_BOUNCES {
            return false;
        }
        self.notice(
            task.issuer_or(run.planner),
            &format!("task:{}:bounce", task.id),
            &format!(
                "#{} has gone back to {} {count} times already; the hub stops here and it stays as it is. Change what is asked of it, or task_assign it elsewhere.",
                task.key,
                target.name()
            ),
        )
        .await;
        true
    }

    fn record_bounce(&self, task: &Task, target: &Target) {
        let key = (task.id, target.name().to_owned());
        *self
            .bounces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_default() += 1;
    }

    /// Where a send-back would go, worked out before anything moves.
    async fn route_back(&self, run: &Run, task: &Task) -> Result<Route, CoreError> {
        let Some(raw) = on_reject_of(task) else {
            return Ok(Route::None);
        };
        let tasks = self.store.list_tasks(run.id).await?;
        let Some(target) = Target::of(raw, &tasks) else {
            return Ok(Route::None);
        };
        let Some(agent) = self.reject_route(&tasks, &target).await else {
            return Ok(Route::None);
        };
        if task.worker_template == Some(agent.id) {
            // It is already where it would go: its own worker takes it again.
            return Ok(Route::Own(target));
        }
        Ok(Route::Elsewhere(Box::new(agent), target))
    }

    /// Whether `spec.on_reject` has taken this work back as often as it may: the hub stops
    /// and leaves it as it is, rather than send it round again (multica §2.12).
    pub(crate) async fn reject_stopped(&self, run: &Run, task: &Task) -> Result<bool, CoreError> {
        match self.route_back(run, task).await? {
            Route::None => Ok(false),
            Route::Own(target) | Route::Elsewhere(_, target) => {
                Ok(self.bounce_stopped(run, task, &target).await)
            }
        }
    }

    /// Records the edge `spec.next` promises (docs/research/multica.md §2.12): a step that
    /// another task names as its next waits for that task, so a plan writes the workflow
    /// once — in `next` — and not again in `depends_on`.
    pub(crate) async fn link_next(&self, run: &Run) -> Result<(), CoreError> {
        let tasks = self.store.list_tasks(run.id).await?;
        for task in &tasks {
            let Some(raw) = next_of(task) else {
                continue;
            };
            let Some(Target::Step(key)) = Target::of(raw, &tasks) else {
                continue;
            };
            let Some(step) = tasks.iter().find(|t| t.key == key) else {
                continue;
            };
            if step.id == task.id || step.depends_on.contains(&task.key) {
                continue;
            }
            let mut step = step.clone();
            step.depends_on.push(task.key.clone());
            step.updated_at = Utc::now();
            self.store.update_task(&step).await?;
        }
        Ok(())
    }

    /// Puts `spec.next` into effect once `task` is accepted (merged): the step it names is
    /// released — it starts as its other dependencies allow, in the schedule pass that
    /// follows the merge — or, when it names a teammate, the hub opens the step for it. `None`
    /// when the task sets no `next`, or names a step the run does not have: nothing changes.
    pub(crate) async fn after_accept(
        &self,
        run: &Run,
        task: &Task,
    ) -> Result<Option<NextStep>, CoreError> {
        let Some(raw) = next_of(task) else {
            return Ok(None);
        };
        let tasks = self.store.list_tasks(run.id).await?;
        let Some(target) = Target::of(raw, &tasks) else {
            return Ok(None);
        };
        match target {
            Target::Step(key) => {
                let Some(step) = tasks.iter().find(|t| t.key == key) else {
                    tracing::warn!(
                        task = %task.key, next = %key,
                        "spec.next names no step and no teammate of the run"
                    );
                    return Ok(None);
                };
                let to = self.worker_of(step).await;
                if step.status != TaskStatus::Pending {
                    return Ok(Some(NextStep {
                        step: key.clone(),
                        to,
                        opened: false,
                        about: format!(
                            "#{key} is already {:?}: there is nothing to release",
                            step.status
                        ),
                    }));
                }
                // Its step waits for this one: released here, started by the schedule pass
                // that runs once the merge is recorded.
                let mut step = step.clone();
                if step.id != task.id && !step.depends_on.contains(&task.key) {
                    step.depends_on.push(task.key.clone());
                }
                step.updated_at = Utc::now();
                self.store.update_task(&step).await?;
                Ok(Some(NextStep {
                    step: key.clone(),
                    to,
                    opened: false,
                    about: format!("#{key} is released: it starts as its other dependencies allow"),
                }))
            }
            Target::Teammate(name) => {
                let Some(agent) = self.teammate(run, &name).await else {
                    tracing::warn!(
                        task = %task.key, next = %name,
                        "spec.next names no step and no teammate of the run"
                    );
                    return Ok(None);
                };
                // The step does not exist yet: the hub opens it from this task's contract —
                // the same work goes on with someone else — and the handoff it starts with
                // carries what this one left.
                let step = self.opened_step(run, task, &tasks, &agent);
                let key = step.key.clone();
                // What the hub opens passes the checks a plan does; a step it cannot open
                // leaves the merge as it is and tells the lead.
                let mut check: Vec<PlannedTask> = tasks.iter().map(planned).collect();
                check.push(planned(&step));
                if let Err(why) = crate::plan::validate(&check) {
                    tracing::warn!(task = %task.key, error = %why, "cannot open the next step");
                    self.notice(
                        task.issuer_or(run.planner),
                        &format!("task:{}:next", task.id),
                        &format!(
                            "#{} was accepted, but the next step for {} cannot be opened: {why}. Add it with task_create.",
                            task.key, agent.name
                        ),
                    )
                    .await;
                    return Ok(None);
                }
                self.store.insert_task(&step).await?;
                self.publish(EventKind::TaskStatusChanged {
                    run_id: run.id,
                    id: step.id,
                    key: step.key.clone(),
                    from: TaskStatus::Pending,
                    to: TaskStatus::Pending,
                    assignee: None,
                })
                .await;
                let about = format!("#{key} is a new step for {}", agent.name);
                self.notice(
                    task.issuer_or(run.planner),
                    &format!("task:{}:next", task.id),
                    &format!(
                        "#{} was accepted; {about}. It starts as its dependencies allow.",
                        task.key
                    ),
                )
                .await;
                Ok(Some(NextStep {
                    step: key,
                    to: agent.name,
                    opened: true,
                    about,
                }))
            }
        }
    }

    /// Whether `spec.on_reject` would hand `task` to someone else: a step or teammate of the
    /// run, not the one its worker was cloned from, and not one that has taken it back
    /// [`MAX_BOUNCES`] times already.
    pub(crate) async fn rejects_elsewhere(
        &self,
        run: &Run,
        task: &Task,
    ) -> Result<bool, CoreError> {
        match self.route_back(run, task).await? {
            Route::Elsewhere(_, target) => Ok(!self.bounce_stopped(run, task, &target).await),
            Route::Own(_) | Route::None => Ok(false),
        }
    }

    /// Sends `task` back where `spec.on_reject` says, after `why` sent it back (its issuer's
    /// `task_rework`, a review that failed): a fresh execution session of the teammate it
    /// names — or of the worker of the step it names — takes it over with `notes`. `None`
    /// when nothing is set, it names something the run does not have, or it names the task's
    /// own worker: then it goes back exactly as before.
    ///
    /// Every send-back along a route is counted: [`MAX_BOUNCES`] of them is all one does.
    pub(crate) async fn send_back(
        &self,
        run: &Run,
        task: &mut Task,
        notes: &str,
        why: &str,
    ) -> Result<Option<String>, CoreError> {
        let route = self.route_back(run, task).await?;
        match route {
            Route::None => return Ok(None),
            Route::Own(target) => {
                self.record_bounce(task, &target);
                return Ok(None);
            }
            Route::Elsewhere(_, ref target) => {
                if self.bounce_stopped(run, task, target).await {
                    return Ok(None);
                }
            }
        }
        let Route::Elsewhere(template, target) = route else {
            return Ok(None);
        };
        let Some(cwd) = task.worktree.clone() else {
            tracing::warn!(task = %task.key, "spec.on_reject has no worktree to hand back into");
            return Ok(None);
        };
        let issuer = task.issuer_or(run.planner);
        let left = self.left_by_worker(run, task, task.assignee).await;
        crossagents_core::Handoff::end(
            &mut task.handoffs,
            task.assignee,
            crossagents_core::HandoffOutcome::Reassigned,
        );
        if let Some(old) = task.assignee.take()
            && let Err(err) = self.let_go(run.id, old).await
        {
            tracing::warn!(task = %task.key, error = %err, "cannot let go of the worker before");
        }
        let worker = self
            .fresh_agent(
                task,
                template.id,
                &worker_name(run, task, template.id),
                Role::Worker,
                &cwd,
            )
            .await?;
        task.worker_template = Some(template.id);
        task.worker_fresh = true;
        task.worker_restore = false;
        task.assignee = Some(worker.id);
        task.report.question = None;
        if task.status != TaskStatus::Running {
            self.transition(task, TaskStatus::Running).await?;
        } else {
            task.updated_at = Utc::now();
            self.store.update_task(task).await?;
        }
        let reason = match notes.trim() {
            "" => why.to_owned(),
            notes => format!("{why}: {notes}"),
        };
        let card = self
            .hand_off(
                run,
                task,
                HandoffKind::Reassign,
                left,
                &worker,
                Some(template.name.clone()),
                Some(reason),
            )
            .await?;
        self.notify_dispatched(issuer, task, &card).await;
        let mut opening = brief::handover(task, &card);
        if let Some(pushed) = self.expertise_brief(&worker, task).await {
            opening = format!("{opening}\n\n{pushed}");
        }
        let envelope = self.relay_envelope(issuer, opening);
        crate::attempts::begin(task, Some(worker.id), Some(envelope.id));
        task.updated_at = Utc::now();
        self.store.update_task(task).await?;
        self.send_in_background(worker.id, envelope);
        self.record_bounce(task, &target);
        Ok(Some(worker.name))
    }

    /// What `spec.next` and `spec.on_reject` would send, before anything moves: where each
    /// goes, who receives it, and the words it would be started with
    /// (`handoff_preview`, and `task_get`'s `handoff`).
    pub async fn handoff_preview(&self, run: &Run, task: &Task) -> Result<Value, CoreError> {
        let tasks = self.store.list_tasks(run.id).await?;
        let mut out = json!({
            "task": format!("#{}", task.key),
            "status": task.status,
            "next": Value::Null,
            "on_reject": Value::Null,
        });
        match next_of(task) {
            Some(raw) => {
                out["next"] = match Target::of(raw, &tasks) {
                    Some(target) => self.next_preview(run, task, &target, &tasks).await,
                    None => json!({ "name": raw, "note": "empty" }),
                };
            }
            None => {
                out["note"] = json!(format!(
                    "no workflow handoff is set: #{} stays with the hub's usual rules",
                    task.key
                ));
            }
        }
        if let Some(raw) = on_reject_of(task) {
            out["on_reject"] = match Target::of(raw, &tasks) {
                Some(target) => self.reject_preview(task, &target, &tasks).await,
                None => json!({ "name": raw, "note": "empty" }),
            };
        }
        Ok(out)
    }

    /// The `handoff_preview` tool: `task` names the task, and its issuer (or the run's lead)
    /// looks at what would be sent.
    pub(crate) async fn handoff_preview_tool(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let id = self.task_ref(caller, &input).await?;
        let (run, task) = self.task_of(id).await?;
        if task.issuer_or(run.planner) != caller.id && run.planner != caller.id {
            return Err(CoreError::Conflict(format!(
                "only the issuer of #{} or the run's lead previews its handoffs",
                task.key
            )));
        }
        self.handoff_preview(&run, &task).await
    }

    async fn next_preview(&self, run: &Run, task: &Task, target: &Target, tasks: &[Task]) -> Value {
        let when = "when this task is accepted";
        match target {
            Target::Step(key) => match tasks.iter().find(|t| t.key == *key) {
                Some(step) => json!({
                    "goes_to": format!("#{key}"),
                    "who": self.worker_of(step).await,
                    "when": when,
                    "state": step.status,
                    // What the step's own worker is started with.
                    "brief": brief::assignment(step),
                }),
                None => json!({
                    "goes_to": format!("#{key}"),
                    "who": Value::Null,
                    "when": when,
                    "note": format!("the run has no step #{key}, and no teammate of that name"),
                }),
            },
            Target::Teammate(name) => match self.teammate(run, name).await {
                Some(agent) => {
                    let step = self.opened_step(run, task, tasks, &agent);
                    json!({
                        "goes_to": format!("#{}", step.key),
                        "who": agent.name,
                        "when": format!("{when}: the hub opens the step"),
                        "opened": true,
                        "brief": brief::assignment(&step),
                    })
                }
                None => json!({
                    "goes_to": name,
                    "who": Value::Null,
                    "when": when,
                    "note": format!("the run has no step #{name}, and no teammate called {name}"),
                }),
            },
        }
    }

    async fn reject_preview(&self, task: &Task, target: &Target, tasks: &[Task]) -> Value {
        let when = "when it is sent back (task_rework, or a review that fails)";
        match self.reject_route(tasks, target).await {
            Some(agent) => json!({
                "goes_to": target.name(),
                "who": format!("a new execution session of {}", agent.name),
                "when": when,
                // What that session is started with: the notes, then the task's facts.
                "brief": format!(
                    "<the notes it is sent back with>\n\n---\nrework of task #{}\ntask_id: {}\nattempt: {}",
                    task.key,
                    task.id,
                    task.attempts + 1
                ),
            }),
            None => json!({
                "goes_to": target.name(),
                "who": Value::Null,
                "when": when,
                "note": "spec.on_reject names no step and no teammate of the run, so it would go back to its own worker",
            }),
        }
    }

    /// The step `spec.next` opens for a teammate: this task's contract, with the routing
    /// cleared (the next step has no onward routing of its own) and this one named as the
    /// step before it. Built, not stored, so a preview can show it.
    fn opened_step(&self, run: &Run, task: &Task, tasks: &[Task], agent: &Agent) -> Task {
        let mut spec = task.spec.clone();
        spec.next = None;
        spec.on_reject = None;
        spec.references
            .push(format!("#{} (the step before this one, merged)", task.key));
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            run_id: run.id,
            key: free_key(tasks, &format!("{}-next", task.key)),
            title: format!("{} (next)", task.title),
            spec,
            depends_on: vec![task.key.clone()],
            issuer: Some(task.issuer_or(run.planner)),
            assignee: None,
            reviewer: None,
            worker_template: Some(agent.id),
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Pending,
            attempts: 0,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            // A step of the workflow, not a part split off this task: it starts from the run
            // branch, which holds this task's merge, and does not merge back into it.
            parent: None,
            progress: Some(format!("the step after #{}, which is merged", task.key)),
            handoffs: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Who the work of `spec.on_reject` goes back to: the teammate it names, or the teammate
    /// the step it names is worked by.
    async fn reject_route(&self, tasks: &[Task], target: &Target) -> Option<Agent> {
        let id = match target {
            Target::Teammate(name) => self.store.get_agent(&AgentKey::parse(name)).await.ok()?.id,
            // A step goes back to the teammate its workers are cloned from.
            Target::Step(key) => tasks.iter().find(|t| t.key == *key)?.worker_template?,
        };
        self.store.get_agent(&AgentKey::Id(id)).await.ok()
    }

    /// Who works a step, as a person reads it.
    async fn worker_of(&self, step: &Task) -> String {
        match step.worker_template {
            Some(template) => self
                .name_of(template)
                .await
                .unwrap_or_else(|| "a new execution session".into()),
            None => "the run's default worker".into(),
        }
    }

    /// The teammate `name` of the run, if it has one: its worker, reviewer, escalation
    /// teammate, or a member of its team.
    async fn teammate(&self, run: &Run, name: &str) -> Option<Agent> {
        let agent = self.store.get_agent(&AgentKey::parse(name)).await.ok()?;
        let in_run = run.config.worker == agent.id
            || run.config.reviewer == Some(agent.id)
            || run.config.escalate == Some(agent.id)
            || run.config.team.contains(&agent.id);
        in_run.then_some(agent)
    }

    /// Context tokens `agent` last reported in use (`agent.usage`), if it ever did.
    fn context_of(&self, agent: AgentId) -> Option<u64> {
        self.contexts.lock().unwrap().get(&agent).copied()
    }

    pub(crate) fn record_context(&self, agent: AgentId, used: u64) {
        if used > 0 {
            self.contexts.lock().unwrap().insert(agent, used);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{RunId, TaskReport};

    use super::*;

    fn task(key: &str) -> Task {
        Task {
            id: TaskId::new(),
            run_id: RunId::new(),
            key: key.into(),
            title: key.into(),
            spec: crossagents_core::TaskSpec {
                goal: "g".into(),
                ..crossagents_core::TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: None,
            assignee: None,
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Pending,
            attempts: 0,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            parent: None,
            progress: None,
            handoffs: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_name_is_a_step_when_the_run_has_one() {
        let tasks = vec![task("b"), task("ada")];
        assert_eq!(Target::of("b", &tasks), Some(Target::Step("b".into())));
        // `#` says step, whatever the run has.
        assert_eq!(Target::of("#b", &[]), Some(Target::Step("b".into())));
        assert_eq!(Target::of("#ada", &tasks), Some(Target::Step("ada".into())));
        // The plan's keys win over a teammate of the same name.
        assert_eq!(Target::of("ada", &tasks), Some(Target::Step("ada".into())));
        // Not a key: a teammate.
        assert_eq!(
            Target::of("rex", &tasks),
            Some(Target::Teammate("rex".into()))
        );
        assert_eq!(Target::of("  ", &tasks), None);
        assert_eq!(Target::of("  #  ", &tasks), None);
    }

    #[test]
    fn a_routing_that_is_blank_or_unset_names_nothing() {
        let mut task = task("a");
        assert_eq!(next_of(&task), None);
        assert_eq!(on_reject_of(&task), None);
        task.spec.next = Some("   ".into());
        assert_eq!(next_of(&task), None);
        task.spec.next = Some(" b ".into());
        assert_eq!(next_of(&task), Some("b"));
        task.spec.on_reject = Some(" ada ".into());
        assert_eq!(on_reject_of(&task), Some("ada"));
    }

    #[test]
    fn an_opened_step_gets_a_key_of_its_own() {
        let tasks = vec![task("a"), task("a-next")];
        assert_eq!(free_key(&[], "a-next"), "a-next");
        assert_eq!(free_key(&tasks, "a-next"), "a-next-2");
        let mut tasks = tasks;
        tasks.push(task("a-next-2"));
        assert_eq!(free_key(&tasks, "a-next"), "a-next-3");
    }
}

//! Who a landing commit is co-authored by (docs/tasks/SELF.md B2).
//!
//! The hub makes every commit as one identity (`CrossAgents <crossagents@localhost>`,
//! `workspace::AUTHOR`), which says nothing about the agents the work came from. So the run's
//! landing commit ends with one `Co-authored-by:` trailer per agent that took part — the lead,
//! whoever issued, worked on or reviewed a task, and whoever held one before — and `git log`
//! or a forge then shows them beside the commit.
//!
//! The address is made from the agent's name. `CROSSAGENTS_COAUTHOR_EMAIL` says how, with
//! `{slug}` (the name lowercased, every run of other characters as one `-`) and `{name}` (the
//! name as it is), e.g. `{slug}@users.noreply.github.com`. Unset, or blank, it is
//! [`DEFAULT_EMAIL`]; `off` or `none` adds no trailers, and the commits keep the hub's author
//! alone — the same as giving the hub a person's git identity instead.

use crossagents_core::{Agent, AgentId, Run, Task};

use crate::engine::Orchestrator;

/// How a co-author's address is made, if the person sets it in the hub's environment.
pub const EMAIL_ENV: &str = "CROSSAGENTS_COAUTHOR_EMAIL";

/// What an address is made from when the variable is unset.
pub const DEFAULT_EMAIL: &str = "{slug}@agents.crossagents.local";

/// One `Co-authored-by:` line: the name to credit, and the address it goes with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coauthor {
    pub name: String,
    pub email: String,
}

/// How an agent's name becomes an address; no pattern means no trailers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pattern: Option<String>,
}

impl Format {
    /// The format the hub runs with (`EMAIL_ENV`).
    pub fn from_env() -> Format {
        Self::new(std::env::var(EMAIL_ENV).ok().as_deref())
    }

    /// `pattern` as the person wrote it: unset or blank is [`DEFAULT_EMAIL`], `off`/`none` is
    /// nothing, and a pattern that cannot be an address is said in a warning, not a commit
    /// that quietly credits nobody.
    pub fn new(pattern: Option<&str>) -> Format {
        let pattern = match pattern.map(str::trim) {
            None | Some("") => Some(DEFAULT_EMAIL.to_owned()),
            Some(text) if text.eq_ignore_ascii_case("off") || text.eq_ignore_ascii_case("none") => {
                None
            }
            Some(text) => Some(text.to_owned()),
        };
        if let Some(text) = &pattern
            && !text.contains('@')
        {
            tracing::warn!(
                value = %text,
                "{EMAIL_ENV} is not an address (it has no `@`): landing commits get no co-authors"
            );
            return Format { pattern: None };
        }
        Format { pattern }
    }

    /// Whether trailers are turned off.
    pub fn off(&self) -> bool {
        self.pattern.is_none()
    }

    /// The address for an agent called `name`, by `id` when the name leaves nothing to build
    /// one from (a name in a script this makes no slug of).
    pub fn email(&self, name: &str, id: AgentId) -> Option<String> {
        let pattern = self.pattern.as_deref()?;
        let mut slug = slug(name);
        if slug.is_empty() {
            slug = id.to_string().chars().take(8).collect();
        }
        Some(
            pattern
                .replace("{slug}", &slug)
                .replace("{name}", name.trim()),
        )
    }
}

/// The agents that took part in `run`, in the order they came in: its lead, then whoever
/// issued, worked on or reviewed a task, and whoever held one before. Each once.
pub fn participants(run: &Run, tasks: &[Task]) -> Vec<AgentId> {
    let mut ids = vec![run.planner];
    for task in tasks {
        ids.extend(task.issuer);
        ids.extend(task.assignee);
        ids.extend(task.reviewer);
        // A task's assignee is let go once it is merged; the cards keep everyone it passed
        // through, and a worker whose task was merged is only there.
        for card in &task.handoffs {
            ids.extend(card.from);
            ids.push(card.to);
        }
    }
    let mut unique: Vec<AgentId> = Vec::new();
    for id in ids {
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    unique
}

/// The trailers for the agents `ids` names, skipping the ones `agents` does not know (an
/// agent deleted since) and the ones the format gives no address for.
pub fn coauthors(ids: &[AgentId], agents: &[Agent], format: &Format) -> Vec<Coauthor> {
    ids.iter()
        .filter_map(|id| agents.iter().find(|agent| agent.id == *id))
        .filter_map(|agent| {
            let name = credited(agent);
            // The handle makes the address (it is unique and stable), the credited name is
            // what a reader sees.
            format
                .email(&agent.name, agent.id)
                .map(|email| Coauthor { name, email })
        })
        .collect()
}

/// What a commit credits an agent by: the name a person gave it, else its handle.
fn credited(agent: &Agent) -> String {
    let chosen = agent
        .profile
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    chosen.unwrap_or(&agent.name).to_owned()
}

/// A name as an address's local part: lower case, letters and digits kept, every other run of
/// characters a single `-`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// `message` with a `Co-authored-by:` line per co-author, below the body. A trailer already
/// there for the same address (the lead wrote one itself) is kept and not repeated.
pub fn with_trailers(message: &str, coauthors: &[Coauthor]) -> String {
    let mut out = message.trim_end().to_owned();
    let mut have: Vec<String> = out.lines().filter_map(trailer_email).collect();
    let mut added: Vec<String> = Vec::new();
    for coauthor in coauthors {
        let (name, email) = (coauthor.name.trim(), coauthor.email.trim());
        if name.is_empty() || email.is_empty() {
            continue;
        }
        if have.iter().any(|have| have.eq_ignore_ascii_case(email)) {
            continue;
        }
        have.push(email.to_owned());
        added.push(format!("Co-authored-by: {name} <{email}>"));
    }
    if !added.is_empty() {
        out.push_str("\n\n");
        out.push_str(&added.join("\n"));
    }
    out
}

/// The address in a `Co-authored-by:` line, if `line` is one.
fn trailer_email(line: &str) -> Option<String> {
    let (key, rest) = line.split_once(':')?;
    if !key.trim().eq_ignore_ascii_case("co-authored-by") {
        return None;
    }
    let start = rest.rfind('<')?;
    let end = rest[start..].find('>')? + start;
    Some(rest[start + 1..end].trim().to_owned())
}

impl Orchestrator {
    /// `message`, the landing commit's, with a `Co-authored-by:` trailer for each agent that
    /// took part in the run (docs/tasks/SELF.md B2). A landing is not refused over credit:
    /// when the run's tasks or the agents cannot be read, the message goes as it is.
    pub(crate) async fn coauthored(&self, run: &Run, message: String) -> String {
        let format = Format::from_env();
        if format.off() {
            return message;
        }
        let tasks = match self.store.list_tasks(run.id).await {
            Ok(tasks) => tasks,
            Err(err) => {
                tracing::warn!(run = %run.id, error = %err, "cannot read the run's tasks for co-authors");
                return message;
            }
        };
        let agents = match self.store.list_agents().await {
            Ok(agents) => agents,
            Err(err) => {
                tracing::warn!(run = %run.id, error = %err, "cannot read the agents for co-authors");
                return message;
            }
        };
        let coauthors = coauthors(&participants(run, &tasks), &agents, &format);
        with_trailers(&message, &coauthors)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{
        AgentProfile, AgentStatus, HandoffId, HandoffKind, PlanApproval, Role, RunConfig, RunId,
        RunPolicy, RunStatus, TaskId, TaskReport, TaskSpec, TaskStatus,
    };

    use super::*;

    fn agent(name: &str, display: Option<&str>) -> Agent {
        Agent {
            id: AgentId::new(),
            name: name.into(),
            name_source: Default::default(),
            plugin: "mock".into(),
            driver: None,
            model: None,
            role: Role::Worker,
            home: "home".into(),
            cwd: "home".into(),
            config: serde_json::json!({}),
            status: AgentStatus::Idle,
            created_at: Utc::now(),
            profile: AgentProfile {
                display_name: display.map(str::to_owned),
                ..AgentProfile::default()
            },
            topic: None,
            archived_at: None,
        }
    }

    fn a_run(planner: AgentId) -> Run {
        let now = Utc::now();
        Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "title".into(),
            request: "do it".into(),
            planner,
            status: RunStatus::Running,
            config: RunConfig {
                worker: AgentId::new(),
                escalate: None,
                reviewer: None,
                auto_approve: true,
                max_parallel: 1,
                max_attempts: 1,
                team: vec![],
                policy: RunPolicy {
                    plan_approval: PlanApproval::Auto,
                    ..RunPolicy::default()
                },
            },
            repo: "repo".into(),
            base_branch: "main".into(),
            branch: "run".into(),
            in_place: false,
            note: None,
            plan: vec![],
            lead_note: None,
            landed: None,
            summary: None,
            checks: vec![],
            checks_state: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn a_task(run: &Run, key: &str) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            run_id: run.id,
            key: key.into(),
            title: format!("task {key}"),
            spec: TaskSpec::default(),
            depends_on: Vec::new(),
            issuer: Some(run.planner),
            assignee: None,
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Merged,
            attempts: 1,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            parent: None,
            progress: None,
            handoffs: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn handoff(run: &Run, task: &Task, from: AgentId, to: AgentId) -> crossagents_core::Handoff {
        crossagents_core::Handoff {
            id: HandoffId::new(),
            run_id: run.id,
            task_id: task.id,
            task_key: task.key.clone(),
            kind: HandoffKind::Reassign,
            from: Some(from),
            from_name: None,
            to,
            to_name: "next".into(),
            template: None,
            reason: None,
            goal: String::new(),
            files: Vec::new(),
            summary: None,
            decisions: Vec::new(),
            open_questions: Vec::new(),
            context_tokens: None,
            fresh: true,
            outcome: None,
            at: Utc::now(),
        }
    }

    #[test]
    fn the_address_is_made_from_the_name_and_can_be_configured() {
        let id = AgentId::new();
        assert_eq!(
            Format::new(None).email("Ada Lovelace", id).unwrap(),
            "ada-lovelace@agents.crossagents.local"
        );
        // A name that makes no slug falls back to the id, so the address is still one.
        let fallback = Format::new(Some("{slug}@example.com"))
            .email("小美", id)
            .unwrap();
        let short: String = id.to_string().chars().take(8).collect();
        assert_eq!(fallback, format!("{short}@example.com"));
        // `{name}` and `{slug}` both, as a person may want them.
        assert_eq!(
            Format::new(Some("{slug}+{name}@example.com"))
                .email("Rex", id)
                .unwrap(),
            "rex+Rex@example.com"
        );
        // Off, and a pattern that is no address at all (the hub says so, once, and goes on).
        assert!(Format::new(Some("off")).off());
        assert!(Format::new(Some(" none ")).off());
        assert!(!Format::new(Some("{slug}@example.com")).off());
        assert!(Format::new(Some("{name}")).off());
    }

    #[test]
    fn everyone_who_touched_a_task_is_credited_once() {
        let lead = AgentId::new();
        let worker = AgentId::new();
        let reviewer = AgentId::new();
        let before = AgentId::new();
        let run = a_run(lead);
        let mut first = a_task(&run, "a");
        first.assignee = Some(worker);
        first.reviewer = Some(reviewer);
        // A merged task's assignee is let go: its card is what still names the worker.
        let mut second = a_task(&run, "b");
        second.handoffs = vec![handoff(&run, &second, before, worker)];
        // The lead issues both tasks (issuer), so it is there already; nothing repeats.
        assert_eq!(
            participants(&run, &[first, second]),
            [lead, worker, reviewer, before]
        );
    }

    #[test]
    fn a_name_a_person_gave_is_what_the_commit_credits() {
        let lead = agent("boss", Some("Rex"));
        let worker = agent("coder", None);
        let ids = [lead.id, worker.id];
        let coauthors = coauthors(&ids, &[lead, worker], &Format::new(None));
        assert_eq!(
            coauthors,
            [
                Coauthor {
                    name: "Rex".into(),
                    email: "boss@agents.crossagents.local".into()
                },
                Coauthor {
                    name: "coder".into(),
                    email: "coder@agents.crossagents.local".into()
                },
            ]
        );
    }

    #[test]
    fn the_trailers_close_the_landing_message() {
        let coauthors = [
            Coauthor {
                name: "Rex".into(),
                email: "boss@agents.crossagents.local".into(),
            },
            Coauthor {
                name: "coder".into(),
                email: "coder@agents.crossagents.local".into(),
            },
        ];
        let message = with_trailers("crossagents: do it\n\n连带落地：第 1 轮（x）", &coauthors);
        assert_eq!(
            message,
            "crossagents: do it\n\n连带落地：第 1 轮（x）\n\n\
             Co-authored-by: Rex <boss@agents.crossagents.local>\n\
             Co-authored-by: coder <coder@agents.crossagents.local>"
        );

        // A message that already credits someone is not credited twice, whatever its case.
        let again = with_trailers(&message, &coauthors);
        assert_eq!(again, message);
        let written = with_trailers(
            "crossagents: do it\n\nCo-authored-by: Rex <BOSS@agents.crossagents.local>",
            &coauthors,
        );
        assert_eq!(
            written
                .to_lowercase()
                .matches("boss@agents.crossagents.local")
                .count(),
            1,
            "{written}"
        );
        assert!(written.contains("Co-authored-by: coder <coder@agents.crossagents.local>"));

        // Nothing to credit: the message is left as it is, without a trailing newline.
        assert_eq!(
            with_trailers("crossagents: do it\n", &[]),
            "crossagents: do it"
        );
    }
}

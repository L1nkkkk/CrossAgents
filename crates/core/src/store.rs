//! Persistence abstraction. See docs/DESIGN.md §5.

use async_trait::async_trait;

use crate::account::Account;
use crate::claim::AgentClaim;
use crate::error::CoreError;
use crate::event::{Event, EventKind};
use crate::expertise::{
    Candidate, CandidateStatus, ExpertiseEntry, OwnerKind, UsageOutcome, UsageRecord,
};
use crate::human::HumanItem;
use crate::ids::{AgentId, MessageId, RunId, TaskId};
use crate::lesson::{IssueRef, Lesson, Project, TriageRecord, TriageStep};
use crate::model::{Agent, AgentProfile, AgentStatus, Run, Task};
use crate::note::{Note, NoteScope};
use crate::notify::{Reminder, Watch};

/// Look an agent up by id or by its unique name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentKey {
    Id(AgentId),
    Name(String),
}

impl AgentKey {
    /// Parses a user-supplied reference: a UUID is an id, anything else a name.
    pub fn parse(s: &str) -> Self {
        s.parse()
            .map_or_else(|_| Self::Name(s.to_owned()), Self::Id)
    }
}

#[async_trait]
pub trait Store: Send + Sync {
    /// Latest failure proving this account cannot use this model.
    async fn unavailable_model(
        &self,
        plugin: &str,
        account: &str,
        model: &str,
    ) -> Result<Option<String>, CoreError> {
        let _ = (plugin, account, model);
        Ok(None)
    }
    async fn record_unavailable_model(
        &self,
        plugin: &str,
        account: &str,
        model: &str,
        error: &str,
    ) -> Result<(), CoreError> {
        let _ = (plugin, account, model, error);
        Err(CoreError::Unsupported("unavailable models".into()))
    }
    async fn clear_unavailable_model(
        &self,
        plugin: &str,
        account: &str,
        model: &str,
    ) -> Result<(), CoreError> {
        let _ = (plugin, account, model);
        Err(CoreError::Unsupported("unavailable models".into()))
    }

    /// Fails with [`CoreError::Conflict`] if the name is taken.
    async fn insert_agent(&self, agent: &Agent) -> Result<(), CoreError>;
    async fn get_agent(&self, key: &AgentKey) -> Result<Agent, CoreError>;
    async fn list_agents(&self) -> Result<Vec<Agent>, CoreError>;
    async fn delete_agent(&self, id: AgentId) -> Result<(), CoreError>;
    async fn update_agent_status(&self, id: AgentId, status: AgentStatus) -> Result<(), CoreError>;
    /// Renames the agent; fails with [`CoreError::Conflict`] if another has the name
    /// (ignoring case).
    async fn rename_agent(
        &self,
        id: AgentId,
        name: &str,
        source: crate::NameSource,
    ) -> Result<(), CoreError> {
        let _ = (id, name, source);
        Err(CoreError::Unsupported("renaming agents".into()))
    }
    /// Sets or clears a conversation's title.
    async fn update_agent_topic(&self, id: AgentId, topic: Option<&str>) -> Result<(), CoreError> {
        let _ = (id, topic);
        Err(CoreError::Unsupported("conversation titles".into()))
    }
    /// Archives the conversation (`Some`) or restores it (`None`).
    async fn set_agent_archived(
        &self,
        id: AgentId,
        at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), CoreError> {
        let _ = (id, at);
        Err(CoreError::Unsupported("archiving agents".into()))
    }
    /// Replaces the agent's profile.
    async fn update_agent_profile(
        &self,
        id: AgentId,
        profile: &AgentProfile,
    ) -> Result<(), CoreError> {
        let _ = (id, profile);
        Err(CoreError::Unsupported("updating agent profiles".into()))
    }
    /// Replaces the agent's model; `None` leaves it to the agent's own default.
    async fn update_agent_model(&self, id: AgentId, model: Option<&str>) -> Result<(), CoreError> {
        let _ = (id, model);
        Err(CoreError::Unsupported("updating agent models".into()))
    }

    /// Moves the agent to another working directory. A live session has to stop first
    /// (`Runtime::place`): the driver resumes it there.
    async fn update_agent_cwd(&self, id: AgentId, cwd: &std::path::Path) -> Result<(), CoreError> {
        let _ = (id, cwd);
        Err(CoreError::Unsupported("moving agents".into()))
    }

    /// Sets the workspace the agent belongs to (docs/DESIGN.md §4.7).
    async fn update_agent_home(
        &self,
        id: AgentId,
        home: &std::path::Path,
    ) -> Result<(), CoreError> {
        let _ = (id, home);
        Err(CoreError::Unsupported("agent homes".into()))
    }

    /// Holds `claim.agent_id` for a run, in one atomic insert (docs/tasks/S1.md §3.1):
    /// `None` when it is now held, else the claim already holding it, which stays.
    async fn claim_agent(&self, claim: &AgentClaim) -> Result<Option<AgentClaim>, CoreError> {
        let _ = claim;
        Err(CoreError::Unsupported("claims".into()))
    }
    /// Lets go of `agent` if `run` holds it; `false` when it did not (another run's claim is
    /// left alone).
    async fn release_agent(&self, agent: AgentId, run: RunId) -> Result<bool, CoreError> {
        let _ = (agent, run);
        Ok(false)
    }
    /// What holds `agent`, if anything.
    async fn claim_of(&self, agent: AgentId) -> Result<Option<AgentClaim>, CoreError> {
        let _ = agent;
        Ok(None)
    }
    /// Every claim, oldest first; only `run`'s when given.
    async fn list_claims(&self, run: Option<RunId>) -> Result<Vec<AgentClaim>, CoreError> {
        let _ = run;
        Ok(Vec::new())
    }

    /// Replaces the agent's plugin-private config (e.g. its reasoning `effort`).
    async fn update_agent_config(
        &self,
        id: AgentId,
        config: &serde_json::Value,
    ) -> Result<(), CoreError> {
        let _ = (id, config);
        Err(CoreError::Unsupported("updating agent config".into()))
    }

    /// Persists the event, assigning its `seq`, `id` and `ts`.
    async fn append_event(&self, kind: EventKind) -> Result<Event, CoreError>;
    /// Events with `seq > after`, oldest first.
    async fn events_since(&self, after: u64, limit: u32) -> Result<Vec<Event>, CoreError>;
    /// The highest `seq` stored, or 0.
    async fn latest_event_seq(&self) -> Result<u64, CoreError>;
    /// The vendor session id the agent's last session reported (`agent.session_started`).
    /// The default scans the agent's events; stores should override it.
    async fn last_session_id(&self, agent: AgentId) -> Result<Option<String>, CoreError> {
        let mut last = None;
        let mut after = 0;
        loop {
            let page = self.agent_events(agent, after, 500).await?;
            let Some(end) = page.last().map(|e| e.seq) else {
                return Ok(last);
            };
            after = end;
            for event in page {
                if let EventKind::AgentSessionStarted { session_id, .. } = event.kind {
                    last = Some(session_id);
                }
            }
        }
    }

    /// Events about `agent` (see [`EventKind::agent_id`]) with `seq > after`, oldest first.
    /// The default scans every event; stores should override it with an indexed lookup.
    async fn agent_events(
        &self,
        agent: AgentId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<Event>, CoreError> {
        let mut found = Vec::new();
        let mut last = after;
        loop {
            let page = self.events_since(last, 500).await?;
            let Some(end) = page.last().map(|e| e.seq) else {
                return Ok(found);
            };
            last = end;
            for event in page {
                if event.kind.agent_id() == Some(agent) {
                    found.push(event);
                    if found.len() >= limit as usize {
                        return Ok(found);
                    }
                }
            }
        }
    }

    async fn insert_work(&self, work: &crate::Work) -> Result<(), CoreError> {
        let _ = work;
        Err(CoreError::Unsupported("works".into()))
    }
    async fn get_work(&self, id: crate::WorkId) -> Result<crate::Work, CoreError> {
        Err(CoreError::NotFound {
            kind: "work",
            id: id.to_string(),
        })
    }
    async fn list_works(&self) -> Result<Vec<crate::Work>, CoreError> {
        Ok(Vec::new())
    }
    async fn update_work(&self, work: &crate::Work) -> Result<(), CoreError> {
        let _ = work;
        Err(CoreError::Unsupported("works".into()))
    }

    async fn insert_run(&self, run: &Run) -> Result<(), CoreError>;
    async fn get_run(&self, id: RunId) -> Result<Run, CoreError>;
    /// Newest first.
    async fn list_runs(&self) -> Result<Vec<Run>, CoreError>;
    /// Replaces the stored run with `run` (matched by id).
    async fn update_run(&self, run: &Run) -> Result<(), CoreError>;
    /// Deletes the run and everything the store keeps for it: its tasks, the human items and
    /// notes that name it (docs/tasks/R1.md §3.3). Events stay, and so does its record in the
    /// workspace's history ([`Store::upsert_run_history`]), marked deleted. Fails with
    /// [`CoreError::NotFound`] when there is no such run.
    async fn delete_run(&self, id: RunId) -> Result<(), CoreError> {
        let _ = id;
        Err(CoreError::Unsupported("deleting runs".into()))
    }

    /// Saves what the run left behind in its workspace's history, or updates the record a
    /// previous write made (idempotent, matched by run id). History outlives the run:
    /// [`Store::delete_run`] never deletes it.
    async fn upsert_run_history(&self, history: &crate::RunHistory) -> Result<(), CoreError> {
        let _ = history;
        Err(CoreError::Unsupported("run history".into()))
    }
    /// What runs left in their workspaces, newest first: one workspace's when `repo` is given,
    /// every workspace's otherwise. Includes runs since deleted.
    async fn list_run_history(
        &self,
        repo: Option<&std::path::Path>,
    ) -> Result<Vec<crate::RunHistory>, CoreError> {
        let _ = repo;
        Ok(Vec::new())
    }

    async fn insert_task(&self, task: &Task) -> Result<(), CoreError>;
    async fn get_task(&self, id: TaskId) -> Result<Task, CoreError>;
    /// In creation order.
    async fn list_tasks(&self, run_id: RunId) -> Result<Vec<Task>, CoreError>;
    async fn update_task(&self, task: &Task) -> Result<(), CoreError>;

    /// Stores only a credential hash; an active duplicate name is a conflict.
    async fn insert_device(
        &self,
        device: &crate::Device,
        token_hash: &str,
    ) -> Result<(), CoreError> {
        let _ = (device, token_hash);
        Err(CoreError::Unsupported("devices".into()))
    }
    async fn list_devices(&self) -> Result<Vec<crate::Device>, CoreError> {
        Err(CoreError::Unsupported("devices".into()))
    }
    async fn device_by_hash(&self, hash: &str) -> Result<Option<crate::Device>, CoreError> {
        let _ = hash;
        Err(CoreError::Unsupported("devices".into()))
    }
    async fn revoke_device(&self, id: &str) -> Result<crate::Device, CoreError> {
        let _ = id;
        Err(CoreError::Unsupported("devices".into()))
    }
    async fn touch_device(&self, id: &str) -> Result<(), CoreError> {
        let _ = id;
        Err(CoreError::Unsupported("devices".into()))
    }

    async fn insert_account(&self, account: &Account) -> Result<(), CoreError>;
    async fn get_account(&self, id: &str) -> Result<Account, CoreError>;
    async fn list_accounts(&self) -> Result<Vec<Account>, CoreError>;
    async fn delete_account(&self, id: &str) -> Result<(), CoreError>;

    async fn insert_reminder(&self, reminder: &Reminder) -> Result<(), CoreError> {
        let _ = reminder;
        Err(CoreError::Unsupported("reminders".into()))
    }
    /// `owner`'s reminders, or everyone's; soonest first.
    async fn list_reminders(&self, owner: Option<AgentId>) -> Result<Vec<Reminder>, CoreError> {
        let _ = owner;
        Ok(Vec::new())
    }
    /// Fails with [`CoreError::NotFound`] if there is no such reminder.
    async fn delete_reminder(&self, id: &str) -> Result<(), CoreError> {
        Err(CoreError::NotFound {
            kind: "reminder",
            id: id.to_owned(),
        })
    }
    async fn insert_watch(&self, watch: &Watch) -> Result<(), CoreError> {
        let _ = watch;
        Err(CoreError::Unsupported("watches".into()))
    }
    async fn list_watches(&self, owner: Option<AgentId>) -> Result<Vec<Watch>, CoreError> {
        let _ = owner;
        Ok(Vec::new())
    }
    async fn delete_watch(&self, id: &str) -> Result<(), CoreError> {
        Err(CoreError::NotFound {
            kind: "watch",
            id: id.to_owned(),
        })
    }
    /// Inserts or replaces a note (by scope, run and key).
    async fn put_note(&self, note: &Note) -> Result<(), CoreError> {
        let _ = note;
        Err(CoreError::Unsupported("notes".into()))
    }
    async fn get_note(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        key: &str,
    ) -> Result<Option<Note>, CoreError> {
        let _ = (scope, run_id, key);
        Ok(None)
    }
    /// Notes of a scope (and run), with keys starting with `prefix`, by key.
    async fn list_notes(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        prefix: &str,
    ) -> Result<Vec<Note>, CoreError> {
        let _ = (scope, run_id, prefix);
        Ok(Vec::new())
    }
    async fn delete_note(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        key: &str,
    ) -> Result<(), CoreError> {
        let _ = (scope, run_id, key);
        Ok(())
    }
    /// Inserts or replaces a project (by id).
    async fn put_project(&self, project: &Project) -> Result<(), CoreError> {
        let _ = project;
        Err(CoreError::Unsupported("projects".into()))
    }
    async fn list_projects(&self) -> Result<Vec<Project>, CoreError> {
        Ok(Vec::new())
    }
    /// Sorts `issues` in one transaction (docs/tasks/LESSONS.md §4.3; see [`TriageStep`]):
    /// returns every issue's record, the earlier one for those already sorted, and the lesson
    /// written, if any.
    async fn triage(
        &self,
        issues: &[IssueRef],
        step: TriageStep,
        by: crate::ids::AgentId,
    ) -> Result<(Vec<TriageRecord>, Option<Lesson>), CoreError> {
        let _ = (issues, step, by);
        Err(CoreError::Unsupported("lessons".into()))
    }
    /// How the issues of `run` were sorted so far.
    async fn list_triage(&self, run: RunId) -> Result<Vec<TriageRecord>, CoreError> {
        let _ = run;
        Ok(Vec::new())
    }
    async fn get_lesson(&self, id: &str) -> Result<Lesson, CoreError> {
        Err(CoreError::NotFound {
            kind: "lesson",
            id: id.to_owned(),
        })
    }
    /// Replaces a stored lesson (by id) and its search entry.
    async fn update_lesson(&self, lesson: &Lesson) -> Result<(), CoreError> {
        let _ = lesson;
        Err(CoreError::Unsupported("lessons".into()))
    }
    /// The project's lessons, newest first.
    async fn list_lessons(&self, project: &str) -> Result<Vec<Lesson>, CoreError> {
        let _ = project;
        Ok(Vec::new())
    }
    /// The project's lessons matching any of `terms`, best first (FTS5 `bm25`), at most
    /// `limit`. Superseded ones only with `superseded`.
    async fn search_lessons(
        &self,
        project: &str,
        terms: &[String],
        superseded: bool,
        limit: usize,
    ) -> Result<Vec<Lesson>, CoreError> {
        let _ = (project, terms, superseded, limit);
        Ok(Vec::new())
    }
    /// Inserts or replaces (by id) an expertise entry and its search entry.
    async fn expertise_put(&self, entry: &ExpertiseEntry) -> Result<(), CoreError> {
        let _ = entry;
        Err(CoreError::Unsupported("expertise".into()))
    }
    async fn expertise_get(&self, id: &str) -> Result<ExpertiseEntry, CoreError> {
        Err(CoreError::NotFound {
            kind: "expertise entry",
            id: id.to_owned(),
        })
    }
    /// The owner's entries, newest first; only `domain`'s when given.
    async fn expertise_list(
        &self,
        owner_kind: OwnerKind,
        owner_id: &str,
        domain: Option<&str>,
    ) -> Result<Vec<ExpertiseEntry>, CoreError> {
        let _ = (owner_kind, owner_id, domain);
        Ok(Vec::new())
    }
    /// Entries of `owners` matching any of `terms`, best first (FTS5 `bm25`), at most `limit`.
    /// Superseded entries are left out: they are history (`expertise_list` has them).
    async fn expertise_search(
        &self,
        owners: &[(OwnerKind, String)],
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<ExpertiseEntry>, CoreError> {
        let _ = (owners, terms, limit);
        Ok(Vec::new())
    }
    /// Fails with [`CoreError::NotFound`] when there is no such entry; its usage records stay.
    async fn expertise_delete(&self, id: &str) -> Result<(), CoreError> {
        let _ = id;
        Err(CoreError::Unsupported("expertise".into()))
    }

    /// Inserts or replaces (by id) a candidate.
    async fn candidate_put(&self, candidate: &Candidate) -> Result<(), CoreError> {
        let _ = candidate;
        Err(CoreError::Unsupported("expertise candidates".into()))
    }
    /// Candidates, oldest first; only one owner's and/or one status' when given.
    async fn candidate_list(
        &self,
        owner: Option<(OwnerKind, String)>,
        status: Option<CandidateStatus>,
    ) -> Result<Vec<Candidate>, CoreError> {
        let _ = (owner, status);
        Ok(Vec::new())
    }
    /// Fails with [`CoreError::NotFound`] when there is no such candidate.
    async fn candidate_set_status(
        &self,
        id: &str,
        status: CandidateStatus,
    ) -> Result<(), CoreError> {
        let _ = (id, status);
        Err(CoreError::Unsupported("expertise candidates".into()))
    }

    /// Appends one usage record.
    async fn usage_record(&self, usage: &UsageRecord) -> Result<(), CoreError> {
        let _ = usage;
        Err(CoreError::Unsupported("expertise usage".into()))
    }
    /// Sets the outcome on every usage record of `task` that has none yet; returns how many.
    async fn usage_set_outcome(
        &self,
        task: TaskId,
        outcome: UsageOutcome,
    ) -> Result<usize, CoreError> {
        let _ = (task, outcome);
        Err(CoreError::Unsupported("expertise usage".into()))
    }
    /// Usage records of one entry and/or one task, oldest first.
    async fn usage_list(
        &self,
        entry: Option<&str>,
        task: Option<TaskId>,
    ) -> Result<Vec<UsageRecord>, CoreError> {
        let _ = (entry, task);
        Ok(Vec::new())
    }

    /// Inserts or replaces (by id) an item for the person.
    async fn put_human_item(&self, item: &HumanItem) -> Result<(), CoreError> {
        let _ = item;
        Err(CoreError::Unsupported("human items".into()))
    }
    async fn get_human_item(&self, id: &str) -> Result<HumanItem, CoreError> {
        Err(CoreError::NotFound {
            kind: "question",
            id: id.to_owned(),
        })
    }
    /// Newest first; only open ones with `open_only`.
    async fn list_human_items(&self, open_only: bool) -> Result<Vec<HumanItem>, CoreError> {
        let _ = open_only;
        Ok(Vec::new())
    }
    /// The thread and text of a message delivered earlier, if it is known.
    async fn find_message(
        &self,
        id: MessageId,
    ) -> Result<Option<(Option<MessageId>, String)>, CoreError> {
        let _ = id;
        Ok(None)
    }
}

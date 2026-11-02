//! The expertise tools (docs/tasks/EXPERTISE.md §6.2, task exp-tools): `expertise_search` and
//! `expertise_get`, the entries a worker names in `task_complete.used`, and how a decided
//! task's result lands on the entries it went by.
//!
//! What an agent may look at is always its own project's profile plus the person's: one
//! project never reads another's.

use chrono::Utc;
use crossagents_core::{
    Agent, CoreError, EntryKind, ExpertiseEntry, OwnerKind, PERSON_OWNER_ID, RunId, Task, TaskId,
    UsageOutcome, UsageRecord, UsageSource,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::engine::Orchestrator;
use crate::lessons::search_terms;
use crate::tools::args;

/// Longest excerpt of an entry's body in a search hit, in bytes (EXPERTISE §4.5).
const EXCERPT_MAX: usize = 300;
/// Hits returned unless asked for another number, and the most one call returns.
const SEARCH_DEFAULT: usize = 5;
const SEARCH_LIMIT: usize = 10;
/// Hits fetched per wanted hit, so that `domain` / `kind` can narrow them and still fill up.
const FETCH_FACTOR: usize = 4;

/// The start of `text` for a search hit: its first paragraph, at most [`EXCERPT_MAX`] bytes,
/// and whether anything was left out.
fn excerpt(text: &str) -> (String, bool) {
    let text = text.trim();
    let mut cut = text.find("\n\n").unwrap_or(text.len()).min(EXCERPT_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let short = text[..cut].trim_end();
    if short.len() == text.len() {
        (text.to_owned(), false)
    } else {
        (format!("{short}…"), true)
    }
}

/// `kind` as asked for in a search, or a refusal naming the three.
fn entry_kind(kind: &str) -> Result<EntryKind, CoreError> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "judgment" | "judgement" => Ok(EntryKind::Judgment),
        "example" => Ok(EntryKind::Example),
        "limit" => Ok(EntryKind::Limit),
        other => Err(CoreError::Conflict(format!(
            "kind is judgment, example or limit, not {other}"
        ))),
    }
}

/// The short name an entry goes by in a brief and in `task_complete.used`: its id's first
/// eight characters.
fn tag(entry: &ExpertiseEntry) -> String {
    entry.id.chars().take(8).collect()
}

/// One search hit: enough to tell whether it is worth reading in full (EXPERTISE §4.5).
fn search_hit(entry: &ExpertiseEntry) -> Value {
    let (text, truncated) = excerpt(&entry.text);
    json!({
        "id": entry.id,
        "tag": tag(entry),
        "title": entry.title,
        "domain": entry.domain,
        "kind": entry.kind,
        "status": entry.status,
        "applies_when": entry.applies_when,
        "text": text,
        "truncated": truncated,
    })
}

/// Whether `task` was sent back at least once: its result then counts as reworked (§4.6).
pub(crate) fn was_reworked(task: &Task) -> bool {
    task.attempts > 1 || !task.report.rework.is_empty()
}

impl Orchestrator {
    /// The profiles the caller may look at: its project's, and the person's.
    async fn expertise_owners(
        &self,
        caller: &Agent,
    ) -> Result<Vec<(OwnerKind, String)>, CoreError> {
        let project = self.project_for(&caller.cwd).await?;
        Ok(vec![
            (OwnerKind::Project, project.id),
            (OwnerKind::Person, PERSON_OWNER_ID.to_owned()),
        ])
    }

    /// An entry the caller may read, by its id or by the tag a brief showed; `None` outside
    /// its project and the person's profiles, or when the tag names none or several.
    async fn own_entry(
        &self,
        caller: &Agent,
        id: &str,
    ) -> Result<Option<ExpertiseEntry>, CoreError> {
        let id = id.trim();
        if id.is_empty() {
            return Ok(None);
        }
        let owners = self.expertise_owners(caller).await?;
        if let Ok(entry) = self.store.expertise_get(id).await
            && owners.contains(&(entry.owner_kind, entry.owner_id.clone()))
        {
            return Ok(Some(entry));
        }
        let mut found: Vec<ExpertiseEntry> = Vec::new();
        for (kind, owner) in &owners {
            for entry in self.store.expertise_list(*kind, owner, None).await? {
                if entry.id.starts_with(id) && !found.iter().any(|e| e.id == entry.id) {
                    found.push(entry);
                }
            }
        }
        Ok(if found.len() == 1 { found.pop() } else { None })
    }

    /// Where a pull belongs: the caller's run, and the task it works on or reviews there.
    async fn usage_place(&self, caller: &Agent) -> (Option<RunId>, Option<TaskId>) {
        let Ok(Some((run, tasks))) = self.run_of(caller.id).await else {
            return (None, None);
        };
        let task = tasks
            .iter()
            .find(|t| t.assignee == Some(caller.id) && !t.status.is_terminal())
            .or_else(|| tasks.iter().find(|t| t.reviewer == Some(caller.id)));
        (Some(run.id), task.map(|t| t.id))
    }

    /// One usage record; a store without expertise is a warning, never a broken tool call.
    async fn record_usage(
        &self,
        entry_id: &str,
        caller: &Agent,
        run_id: Option<RunId>,
        task_id: Option<TaskId>,
        source: UsageSource,
    ) {
        let usage = UsageRecord {
            id: Uuid::new_v4().to_string(),
            entry_id: entry_id.to_owned(),
            run_id,
            task_id,
            agent: Some(caller.id),
            source,
            outcome: None,
            at: Utc::now(),
        };
        if let Err(err) = self.store.usage_record(&usage).await {
            tracing::warn!(entry = %entry_id, error = %err, "cannot record an expertise usage");
        }
    }

    /// This project's and the person's entries matching a query, best first, as hits: title,
    /// domain, status, `applies_when` and the start of the body. Each hit is a pull.
    pub(crate) async fn expertise_search(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            query: String,
            #[serde(default)]
            domain: Option<String>,
            #[serde(default)]
            kind: Option<String>,
            #[serde(default)]
            limit: Option<usize>,
        }
        let a: A = args(input)?;
        let kind = match a.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            Some(kind) => Some(entry_kind(kind)?),
            None => None,
        };
        let domain = a
            .domain
            .map(|d| d.trim().to_owned())
            .filter(|d| !d.is_empty());
        let limit = a.limit.unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_LIMIT);
        let owners = self.expertise_owners(caller).await?;
        let terms = search_terms(&a.query);
        let found = self
            .store
            .expertise_search(&owners, &terms, limit * FETCH_FACTOR)
            .await?;
        let mut hits: Vec<ExpertiseEntry> = Vec::new();
        for entry in found {
            if let Some(domain) = &domain
                && &entry.domain != domain
            {
                continue;
            }
            if let Some(kind) = kind
                && entry.kind != kind
            {
                continue;
            }
            hits.push(entry);
            if hits.len() == limit {
                break;
            }
        }
        let (run_id, task_id) = self.usage_place(caller).await;
        for entry in &hits {
            self.record_usage(&entry.id, caller, run_id, task_id, UsageSource::Pulled)
                .await;
        }
        let entries: Vec<Value> = hits.iter().map(search_hit).collect();
        Ok(json!({
            "entries": entries,
            "note": "A match is a lead, not a rule: check its status and applies_when, and read it in full with expertise_get. Text ending in … is cut. Name the entries you went by in task_complete.used.",
        }))
    }

    /// One entry in full, by its id or the tag a brief showed; a pull, like a search.
    pub(crate) async fn expertise_get(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            id: String,
        }
        let a: A = args(input)?;
        let entry = self
            .own_entry(caller, &a.id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "expertise entry",
                id: a.id.clone(),
            })?;
        let (run_id, task_id) = self.usage_place(caller).await;
        self.record_usage(&entry.id, caller, run_id, task_id, UsageSource::Pulled)
            .await;
        Ok(json!({ "entry": entry, "tag": tag(&entry) }))
    }

    /// `task_complete.used`: the entries the worker says it went by, each a full id or the
    /// short tag a brief showed. A tag that names none or several entries, or one of another
    /// project, is not recorded: it comes back in the list of what could not be placed.
    /// Returns how many were recorded, and what could not be placed.
    pub(crate) async fn declare_used(
        &self,
        caller: &Agent,
        task_id: TaskId,
        used: &[String],
    ) -> Result<(usize, Vec<String>), CoreError> {
        if used.is_empty() {
            return Ok((0, Vec::new()));
        }
        let task = self.store.get_task(task_id).await?;
        let mut declared: Vec<String> = self
            .store
            .usage_list(None, Some(task.id))
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|u| u.source == UsageSource::Declared)
            .map(|u| u.entry_id)
            .collect();
        let mut unrecognized: Vec<String> = Vec::new();
        let mut recorded = 0usize;
        for id in used {
            let given = id.trim();
            if given.is_empty() {
                continue;
            }
            let Some(entry) = self.own_entry(caller, given).await? else {
                if !unrecognized.iter().any(|u| u == given) {
                    unrecognized.push(given.to_owned());
                }
                continue;
            };
            if declared.contains(&entry.id) {
                continue;
            }
            declared.push(entry.id.clone());
            recorded += 1;
            self.record_usage(
                &entry.id,
                caller,
                Some(task.run_id),
                Some(task.id),
                UsageSource::Declared,
            )
            .await;
        }
        Ok((recorded, unrecognized))
    }

    /// The task's result lands on the entries it used: its undecided usage records get
    /// `outcome`, and each entry, counted once per task, gets its stats and evidence
    /// (EXPERTISE §4.6). A repeated accept finds nothing to decide and changes nothing.
    pub(crate) async fn settle_usage(
        &self,
        task: &Task,
        outcome: UsageOutcome,
    ) -> Result<(), CoreError> {
        let settled = match self.store.usage_set_outcome(task.id, outcome).await {
            Ok(settled) => settled,
            // A store without expertise: the run goes on as if there were no profile.
            Err(CoreError::Unsupported(_)) => return Ok(()),
            Err(err) => return Err(err),
        };
        if settled == 0 {
            return Ok(());
        }
        let usages = self
            .store
            .usage_list(None, Some(task.id))
            .await
            .unwrap_or_default();
        let now = Utc::now();
        let mut counted: Vec<String> = Vec::new();
        for usage in usages {
            if counted.contains(&usage.entry_id) {
                continue;
            }
            counted.push(usage.entry_id.clone());
            let Ok(mut entry) = self.store.expertise_get(&usage.entry_id).await else {
                continue;
            };
            entry.stats.used += 1;
            match outcome {
                UsageOutcome::MergedFirstTry => entry.stats.merged_first_try += 1,
                UsageOutcome::Reworked => entry.stats.reworked += 1,
                UsageOutcome::Dropped => entry.stats.dropped += 1,
            }
            // The judgement held or did not on a real task: one more piece of evidence.
            if outcome != UsageOutcome::Dropped {
                entry.evidence += 1;
                entry.last_confirmed_at = Some(now);
            }
            if let Err(err) = self.store.expertise_put(&entry).await {
                tracing::warn!(entry = %entry.id, error = %err, "cannot count an expertise result");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hit_is_cut_at_the_first_paragraph_or_the_byte_limit() {
        let (text, truncated) = excerpt("first line\n\nsecond paragraph");
        assert_eq!(text, "first line…");
        assert!(truncated);
        let long = "测".repeat(EXCERPT_MAX);
        let (text, truncated) = excerpt(&long);
        assert!(truncated);
        assert_eq!(text, format!("{}…", "测".repeat(EXCERPT_MAX / 3)));
        let (text, truncated) = excerpt("short");
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    #[test]
    fn a_kind_is_judgment_example_or_limit() {
        assert_eq!(entry_kind("judgment").unwrap(), EntryKind::Judgment);
        assert_eq!(entry_kind(" Example ").unwrap(), EntryKind::Example);
        assert_eq!(entry_kind("limit").unwrap(), EntryKind::Limit);
        assert!(entry_kind("rule").is_err());
    }

    #[test]
    fn an_entry_goes_by_the_first_eight_characters_of_its_id() {
        let now = Utc::now();
        let entry = ExpertiseEntry {
            id: "01a19b48-7c41-7638-9344-5b7c982ac8a0".into(),
            owner_kind: OwnerKind::Project,
            owner_id: "p1".into(),
            domain: "rust".into(),
            kind: EntryKind::Judgment,
            title: "先写测试".into(),
            text: "先写一个会失败的用例\n\n再实现".into(),
            example: None,
            applies_when: "crates/**".into(),
            evidence: 0,
            last_confirmed_at: None,
            status: crossagents_core::EntryStatus::Active,
            superseded_by: None,
            stats: Default::default(),
            sources: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        assert_eq!(tag(&entry), "01a19b48");
        let hit = search_hit(&entry);
        assert_eq!(hit["tag"], "01a19b48");
        // Only the first paragraph, and it says when it was cut.
        assert_eq!(hit["text"], "先写一个会失败的用例…");
        assert_eq!(hit["truncated"], true);
        assert_eq!(hit["kind"], "judgment");
        assert_eq!(hit["status"], "active");
        assert_eq!(hit["applies_when"], "crates/**");
    }
}

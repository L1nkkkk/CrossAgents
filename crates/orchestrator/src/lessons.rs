//! Troubleshooting records (docs/tasks/LESSONS.md): problems recorded during a run as run
//! notes under `issue/`, sorted afterwards into lessons of the run's project, which any agent
//! of that project searches when it runs into trouble. Reference, never rules: nothing here
//! goes into an agent's instructions beyond one line saying the records exist.

use std::path::Path;

use chrono::Utc;
use crossagents_core::{
    Agent, CoreError, Evidence, ISSUE_PREFIX, IssueRef, Lesson, LessonOutcome, LessonStatus,
    NoteScope, Project, Run, RunId, Triage, TriageStep,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::projects::{LOCAL_NODE, main_repo, resolve};
use crate::tools::args;

/// Longest title, and longest of the other text fields, in bytes.
const TITLE_MAX: usize = 200;
const FIELD_MAX: usize = 4 * 1024;
const KEYWORDS_MAX: usize = 20;
const SEARCH_LIMIT: usize = 10;

pub(crate) fn outcome_schema() -> Value {
    json!({ "type": "string", "enum": ["resolved", "workaround", "unresolved"] })
}

fn checked(field: &str, text: &str, max: usize) -> Result<String, CoreError> {
    let text = text.trim();
    if text.len() > max {
        return Err(CoreError::Conflict(format!(
            "{field} holds at most {max} bytes"
        )));
    }
    Ok(text.to_owned())
}

/// The issue keys of a call, with the prefix added when left out.
fn issue_keys(keys: Vec<String>) -> Result<Vec<String>, CoreError> {
    if keys.is_empty() {
        return Err(CoreError::Conflict("name at least one issue".into()));
    }
    Ok(keys
        .into_iter()
        .map(|k| {
            let k = k.trim().to_owned();
            if k.starts_with(ISSUE_PREFIX) {
                k
            } else {
                format!("{ISSUE_PREFIX}{k}")
            }
        })
        .collect())
}

/// Words worth searching for in a query: split on whitespace and on punctuation that is not
/// part of an identifier, error code or path.
pub fn search_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in query.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';'
                    | '"'
                    | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '，'
                    | '。'
                    | '：'
                    | '；'
            )
    }) {
        let word = word.trim_matches(|c: char| matches!(c, ':' | '.' | '!' | '?' | '`'));
        if !word.is_empty() && !terms.iter().any(|t| t == word) {
            terms.push(word.to_owned());
        }
    }
    terms.truncate(12);
    terms
}

impl Orchestrator {
    /// The project of `dir` if the hub knows it already; nothing is made or bound.
    async fn known_project(&self, dir: &Path) -> Result<Option<Project>, CoreError> {
        let root = main_repo(dir).unwrap_or_else(|| dir.to_path_buf());
        let projects = self.store.list_projects().await?;
        let (project, _) = resolve(&projects, &root, None);
        Ok(projects.into_iter().find(|p| p.id == project.id))
    }

    /// The one line an agent's instructions get about its project's records, when there are
    /// any (LESSONS §4.5): how many, and how to search them. Never their text.
    pub async fn lessons_hint(&self, agent: &Agent) -> Option<String> {
        let project = self.known_project(&agent.cwd).await.ok()??;
        let lessons = self.store.list_lessons(&project.id).await.ok()?;
        let usable = lessons
            .iter()
            .filter(|l| l.status != LessonStatus::Superseded)
            .count();
        (usable > 0).then(|| {
            format!(
                "# Troubleshooting records\n\nThis project has {usable} troubleshooting record(s) from earlier runs: problems agents hit here and what they did. When you hit an error, `lesson_search` it before digging in; a match is a lead to check, not a rule. If you use one, say how it went with `lesson_report`."
            )
        })
    }

    /// How many problems recorded in `run` nobody has sorted yet.
    pub async fn unsorted_issues(&self, run: RunId) -> usize {
        let Ok(notes) = self
            .store
            .list_notes(NoteScope::Run, Some(run), ISSUE_PREFIX)
            .await
        else {
            return 0;
        };
        let sorted = self.store.list_triage(run).await.unwrap_or_default();
        notes
            .iter()
            .filter(|n| !sorted.iter().any(|t| t.issue.key == n.key))
            .count()
    }

    /// The run a call names (an id or the start or end of one), else the caller's own, and
    /// only one of the caller's project (LESSONS §4.3).
    async fn issue_run(&self, caller: &Agent, run: Option<String>) -> Result<Run, CoreError> {
        let run = match run.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            None => match self.run_of(caller.id).await? {
                Some((run, _)) => run,
                None => {
                    return Err(CoreError::Conflict(
                        "you are in no run; name the run whose issues you sort".into(),
                    ));
                }
            },
            Some(key) => self.run_by_key(key).await?,
        };
        let theirs = self.project_for(&run.repo).await?;
        let mine = self.project_for(&caller.cwd).await?;
        if theirs.id != mine.id {
            return Err(CoreError::Conflict(
                "that run belongs to another project".into(),
            ));
        }
        Ok(run)
    }

    async fn run_by_key(&self, key: &str) -> Result<Run, CoreError> {
        if let Ok(id) = key.parse::<RunId>() {
            return self.store.get_run(id).await;
        }
        let key = key.to_lowercase().replace('-', "");
        if key.len() < 4 {
            return Err(CoreError::Conflict(
                "give at least 4 characters of the run id".into(),
            ));
        }
        let mut found = self.store.list_runs().await?.into_iter().filter(|r| {
            let hex = r.id.to_string().replace('-', "");
            hex.starts_with(&key) || hex.ends_with(&key)
        });
        match (found.next(), found.next()) {
            (Some(run), None) => Ok(run),
            (None, _) => Err(CoreError::NotFound {
                kind: "run",
                id: key,
            }),
            _ => Err(CoreError::Conflict(format!(
                "{key} matches more than one run"
            ))),
        }
    }

    /// A run's recorded problems not sorted yet. Read straight from the store by run, so a
    /// run that ended (or failed, or was cancelled) can still be sorted by anyone of its
    /// project; read only.
    pub(crate) async fn issue_pending(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            run: Option<String>,
        }
        let a: A = args(input)?;
        let run = self.issue_run(caller, a.run).await?;
        let sorted = self.store.list_triage(run.id).await?;
        let notes = self
            .store
            .list_notes(NoteScope::Run, Some(run.id), ISSUE_PREFIX)
            .await?;
        let pending: Vec<Value> = notes
            .into_iter()
            .filter(|n| !sorted.iter().any(|t| t.issue.key == n.key))
            .map(|n| json!({ "key": n.key, "author": n.author, "updated_at": n.updated_at, "text": n.text }))
            .collect();
        Ok(json!({
            "run": run.id,
            "status": run.status,
            "pending": pending,
            "sorted": sorted.len(),
        }))
    }

    /// Sorts issues of a run: into a new lesson, into an existing one, or kept in the run.
    pub(crate) async fn issue_sort(
        &self,
        caller: &Agent,
        how: &str,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            run: Option<String>,
            issues: Vec<String>,
            // lesson_create
            #[serde(default)]
            title: String,
            #[serde(default)]
            symptom: String,
            #[serde(default)]
            keywords: Vec<String>,
            #[serde(default)]
            conditions: String,
            #[serde(default)]
            resolution: Option<String>,
            #[serde(default)]
            outcome: Option<LessonOutcome>,
            // lesson_merge
            #[serde(default)]
            lesson: Option<String>,
            #[serde(default)]
            note: String,
            // issue_keep
            #[serde(default)]
            why: String,
        }
        let a: A = args(input)?;
        let run = self.issue_run(caller, a.run).await?;
        let keys = issue_keys(a.issues)?;
        let notes = self
            .store
            .list_notes(NoteScope::Run, Some(run.id), ISSUE_PREFIX)
            .await?;
        for key in &keys {
            if !notes.iter().any(|n| &n.key == key) {
                return Err(CoreError::NotFound {
                    kind: "issue",
                    id: key.clone(),
                });
            }
        }
        let issues: Vec<IssueRef> = keys
            .into_iter()
            .map(|key| IssueRef {
                run_id: run.id,
                key,
            })
            .collect();
        let project = self.project_for(&run.repo).await?;
        let now = Utc::now();
        let evidence = |outcome: LessonOutcome, note: String, commit: Option<String>| Evidence {
            run_id: Some(run.id),
            agent: caller.id,
            node: LOCAL_NODE.into(),
            at: now,
            commit,
            dirty: None,
            outcome,
            note,
        };
        // Where the run's work is, if its checkout is still there.
        let checkout = self.integration_path(&run);
        let commit = if checkout.is_dir() {
            self.workspaces
                .head(&checkout)
                .await
                .ok()
                .filter(|c| !c.is_empty())
        } else {
            None
        };
        let step = match how {
            "lesson_create" => {
                let title = checked("title", &a.title, TITLE_MAX)?;
                let resolution = checked(
                    "resolution",
                    a.resolution.as_deref().unwrap_or(""),
                    FIELD_MAX,
                )?;
                if title.is_empty() || resolution.is_empty() {
                    return Err(CoreError::Conflict(
                        "a lesson needs a title and a resolution".into(),
                    ));
                }
                let outcome = a.outcome.ok_or_else(|| {
                    CoreError::Conflict(
                        "say how it ended: outcome resolved, workaround or unresolved".into(),
                    )
                })?;
                let mut keywords: Vec<String> = a
                    .keywords
                    .iter()
                    .map(|k| k.trim().to_owned())
                    .filter(|k| !k.is_empty())
                    .collect();
                keywords.truncate(KEYWORDS_MAX);
                TriageStep::Create(Lesson {
                    id: uuid::Uuid::new_v4().to_string(),
                    project_id: project.id.clone(),
                    title,
                    symptom: checked("symptom", &a.symptom, FIELD_MAX)?,
                    keywords,
                    conditions: checked("conditions", &a.conditions, FIELD_MAX)?,
                    resolution,
                    outcome,
                    status: LessonStatus::Active,
                    superseded_by: None,
                    evidence: vec![evidence(outcome, String::new(), commit)],
                    sources: Vec::new(),
                    created_at: now,
                    updated_at: now,
                })
            }
            "lesson_merge" => {
                let id = a
                    .lesson
                    .ok_or_else(|| CoreError::Conflict("name the lesson to merge into".into()))?;
                let mut lesson = self.store.get_lesson(&id).await?;
                if lesson.project_id != project.id {
                    return Err(CoreError::Conflict(
                        "that lesson belongs to another project".into(),
                    ));
                }
                if let Some(resolution) = a.resolution {
                    let resolution = checked("resolution", &resolution, FIELD_MAX)?;
                    if !resolution.is_empty() {
                        lesson.resolution = resolution;
                    }
                }
                if let Some(outcome) = a.outcome {
                    lesson.outcome = outcome;
                }
                lesson.evidence.push(evidence(
                    a.outcome.unwrap_or(lesson.outcome),
                    checked("note", &a.note, FIELD_MAX)?,
                    commit,
                ));
                lesson.updated_at = now;
                TriageStep::Merge(lesson)
            }
            _ => TriageStep::Keep {
                why: checked("why", &a.why, FIELD_MAX)?,
            },
        };
        let (records, lesson) = self.store.triage(&issues, step, caller.id).await?;
        let sorted: Vec<Value> = records
            .iter()
            .map(|r| {
                let result = match &r.triage {
                    Triage::Lesson { lesson_id } => json!({ "lesson": lesson_id }),
                    Triage::Merged { lesson_id } => json!({ "merged": lesson_id }),
                    Triage::Kept { .. } => json!("kept"),
                };
                json!({ "issue": r.issue.key, "result": result })
            })
            .collect();
        Ok(json!({ "sorted": sorted, "lesson": lesson.map(|l| l.id) }))
    }

    /// The caller's project's lessons matching a query, best first.
    pub(crate) async fn lesson_search(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            query: String,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            include_superseded: bool,
        }
        let a: A = args(input)?;
        let project = self.project_for(&caller.cwd).await?;
        let terms = search_terms(&a.query);
        let limit = a.limit.unwrap_or(5).clamp(1, SEARCH_LIMIT);
        let found = self
            .store
            .search_lessons(&project.id, &terms, a.include_superseded, limit)
            .await?;
        let lessons: Vec<Value> = found
            .iter()
            .map(|l| {
                json!({
                    "id": l.id,
                    "title": l.title,
                    "status": l.status,
                    "outcome": l.outcome,
                    "symptom": l.symptom,
                    "resolution": l.resolution,
                    "last_checked": l.evidence.last().map(|e| e.at),
                })
            })
            .collect();
        Ok(
            json!({ "lessons": lessons, "note": "A match may not apply here: check its conditions and evidence (lesson_get) before relying on it." }),
        )
    }

    pub(crate) async fn lesson_get(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            id: String,
        }
        let a: A = args(input)?;
        let lesson = self.own_lesson(caller, &a.id).await?;
        Ok(json!({ "lesson": lesson }))
    }

    /// How a lesson went when the caller used it: one more piece of evidence; one that did
    /// not work puts the lesson up for review, its history kept.
    pub(crate) async fn lesson_report(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            id: String,
            worked: bool,
            #[serde(default)]
            note: String,
        }
        let a: A = args(input)?;
        let mut lesson = self.own_lesson(caller, &a.id).await?;
        let (commit, dirty) = if self.workspaces.versioned(&caller.cwd).await {
            let commit = self
                .workspaces
                .head(&caller.cwd)
                .await
                .ok()
                .filter(|c| !c.is_empty());
            let dirty = self
                .workspaces
                .uncommitted(&caller.cwd)
                .await
                .ok()
                .map(|f| !f.is_empty());
            (commit, dirty)
        } else {
            (None, None)
        };
        let now = Utc::now();
        lesson.evidence.push(Evidence {
            run_id: self.run_of(caller.id).await?.map(|(run, _)| run.id),
            agent: caller.id,
            node: LOCAL_NODE.into(),
            at: now,
            commit,
            dirty,
            outcome: if a.worked {
                lesson.outcome
            } else {
                LessonOutcome::Unresolved
            },
            note: checked("note", &a.note, FIELD_MAX)?,
        });
        if !a.worked && lesson.status == LessonStatus::Active {
            lesson.status = LessonStatus::Review;
        }
        lesson.updated_at = now;
        self.store.update_lesson(&lesson).await?;
        Ok(json!({ "id": lesson.id, "status": lesson.status, "evidence": lesson.evidence.len() }))
    }

    async fn own_lesson(&self, caller: &Agent, id: &str) -> Result<Lesson, CoreError> {
        let lesson = self.store.get_lesson(id.trim()).await?;
        let project = self.project_for(&caller.cwd).await?;
        if lesson.project_id != project.id {
            // Other projects' records are not there for this caller.
            return Err(CoreError::NotFound {
                kind: "lesson",
                id: id.to_owned(),
            });
        }
        Ok(lesson)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_error_becomes_search_terms() {
        assert_eq!(
            search_terms("error[E0277]: `python` exited with code 9009 (WindowsApps)"),
            vec![
                "error",
                "E0277",
                "python",
                "exited",
                "with",
                "code",
                "9009",
                "WindowsApps"
            ]
        );
        assert_eq!(search_terms("连接超时，走代理"), vec!["连接超时", "走代理"]);
        assert!(search_terms("  ,; ").is_empty());
    }
}

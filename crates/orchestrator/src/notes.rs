//! Shared notes, usage and the inbox of agents outside the hub (docs/tasks/H1.md §5.5,
//! §5.12, H1-6).

use chrono::Utc;
use crossagents_core::{Agent, CoreError, EventKind, Note, NoteScope, RunId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::tools::args;

/// Longest note text.
const NOTE_MAX: usize = 16 * 1024;

impl Orchestrator {
    /// The run a run-scoped note belongs to: the caller's.
    async fn note_run(&self, caller: &Agent, scope: NoteScope) -> Result<Option<RunId>, CoreError> {
        match scope {
            NoteScope::Team => Ok(None),
            NoteScope::Run => match self.run_of(caller.id).await? {
                Some((run, _)) => Ok(Some(run.id)),
                None => Err(CoreError::Conflict(
                    "you are in no run; use scope \"team\" for notes that outlast runs".into(),
                )),
            },
        }
    }

    pub(crate) async fn note_write(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            key: String,
            /// `null` deletes the note.
            text: Option<String>,
            #[serde(default)]
            scope: NoteScope,
        }
        let a: A = args(input)?;
        let key = a.key.trim().to_owned();
        if key.is_empty() || key.len() > 120 {
            return Err(CoreError::Conflict("a note key is 1-120 characters".into()));
        }
        let run_id = self.note_run(caller, a.scope).await?;
        let existing = self.store.get_note(a.scope, run_id, &key).await?;
        match a.text {
            None => {
                let Some(note) = existing else {
                    return Ok(json!({ "deleted": false, "repeated": true }));
                };
                // A run's lead may remove what a teammate left; a team note has no lead, so
                // only its author may delete it — the role says nothing about who may.
                let lead = match run_id {
                    Some(run) => self.store.get_run(run).await?.planner == caller.id,
                    None => false,
                };
                if note.author != caller.id && !lead {
                    return Err(CoreError::Conflict(
                        "only the note's author or a lead can delete it".into(),
                    ));
                }
                self.store.delete_note(a.scope, run_id, &key).await?;
                self.publish(EventKind::NoteWritten {
                    scope: a.scope,
                    run_id,
                    key,
                    author: caller.id,
                    text: None,
                })
                .await;
                Ok(json!({ "deleted": true }))
            }
            Some(text) => {
                if text.len() > NOTE_MAX {
                    return Err(CoreError::Conflict(format!(
                        "notes hold at most {NOTE_MAX} bytes; split it over several keys"
                    )));
                }
                if existing.as_ref().is_some_and(|n| n.text == text) {
                    return Ok(json!({ "written": true, "repeated": true }));
                }
                let note = Note {
                    scope: a.scope,
                    run_id,
                    key: key.clone(),
                    text: text.clone(),
                    author: caller.id,
                    updated_at: Utc::now(),
                };
                self.store.put_note(&note).await?;
                self.issue_recorded(a.scope, run_id, &key, caller.id).await;
                self.publish(EventKind::NoteWritten {
                    scope: a.scope,
                    run_id,
                    key,
                    author: caller.id,
                    text: Some(text),
                })
                .await;
                Ok(json!({ "written": true }))
            }
        }
    }

    pub(crate) async fn note_read(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            key: String,
            #[serde(default)]
            scope: Option<NoteScope>,
        }
        let a: A = args(input)?;
        let scopes = match a.scope {
            Some(scope) => vec![scope],
            None => vec![NoteScope::Run, NoteScope::Team],
        };
        for scope in scopes {
            let Ok(run_id) = self.note_run(caller, scope).await else {
                continue;
            };
            if let Some(note) = self.store.get_note(scope, run_id, a.key.trim()).await? {
                return Ok(json!({ "note": note }));
            }
        }
        Err(CoreError::NotFound {
            kind: "note",
            id: a.key,
        })
    }

    pub(crate) async fn note_list(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            prefix: String,
            #[serde(default)]
            scope: Option<NoteScope>,
        }
        let a: A = args(input)?;
        let scopes = match a.scope {
            Some(scope) => vec![scope],
            None => vec![NoteScope::Run, NoteScope::Team],
        };
        let mut notes = Vec::new();
        for scope in scopes {
            let Ok(run_id) = self.note_run(caller, scope).await else {
                continue;
            };
            for note in self.store.list_notes(scope, run_id, &a.prefix).await? {
                // A listing shows the start; note_read gives the whole text.
                let preview: String = note.text.chars().take(200).collect();
                notes.push(json!({ "scope": note.scope, "key": note.key, "preview": preview, "author": note.author, "updated_at": note.updated_at }));
            }
        }
        Ok(json!({ "notes": notes }))
    }

    /// What the caller's run (or, outside a run, the hub) has spent, and the accounts' quota.
    pub(crate) async fn usage_get(&self, caller: &Agent) -> Result<Value, CoreError> {
        let run = self.run_of(caller.id).await?.map(|(run, _)| run);
        let mut report = self.host.usage_report(run.as_ref().map(|r| r.id)).await?;
        if let Some(run) = run {
            report["budget"] = json!(run.config.policy.budget);
            report["minutes_elapsed"] = json!((Utc::now() - run.created_at).num_minutes());
        }
        Ok(report)
    }

    /// Messages waiting for an agent that runs outside the hub (it has no session to deliver
    /// them into).
    pub(crate) async fn inbox_read(&self, caller: &Agent) -> Result<Value, CoreError> {
        let messages = self.host.take_inbox(caller.id).await?;
        Ok(json!({ "messages": messages }))
    }
}

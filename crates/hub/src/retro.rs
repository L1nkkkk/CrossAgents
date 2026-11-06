//! `crossagents run retro` (docs/tasks/SELF.md C1): a finished run's retro as a document.
//!
//! The generator belongs to the orchestrator — it folds only core events and notes, so the
//! hub's collaboration tool `run_retro` and this command line write the same document from
//! the same run. This module exposes it to the command line, which depends on the hub and not
//! on the orchestrator.

pub use crossagents_orchestrator::retro::{RetroDoc, document, file_name, render, write};

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use crossagents_core::{
        AgentId, CheckResult, EVENT_SCHEMA_VERSION, Event, EventKind, MessageId, Note, NoteScope,
        RunConfig, RunId, RunPolicy, RunStatus, StopReason, Task, TaskId, TaskReport, TaskSpec,
        TaskStatus, TokenUsage, ToolDetail, ToolStatus,
    };

    use super::*;

    /// One finished run, fixed: a task whose command fails twice, a failed acceptance check,
    /// one issue note, and the run ending without landing.
    fn fixture() -> (crossagents_core::Run, Vec<Task>, Vec<Event>, Vec<Note>) {
        let start = Utc::now();
        let planner = AgentId::new();
        let worker = AgentId::new();
        let run = crossagents_core::Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "给 send 加附件".into(),
            request: "给 crossagents send 加 --file".into(),
            planner,
            status: RunStatus::Completed,
            config: RunConfig {
                worker,
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: RunConfig::DEFAULT_MAX_PARALLEL,
                max_attempts: RunConfig::DEFAULT_MAX_ATTEMPTS,
                team: Vec::new(),
                policy: RunPolicy::default(),
            },
            repo: std::path::PathBuf::from("/repo"),
            base_branch: "main".into(),
            branch: "crossagents/a1b2c3d4".into(),
            in_place: false,
            mode: None,
            note: None,
            plan: Vec::new(),
            lead_note: None,
            landed: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            checks_snapshot: None,
            created_at: start,
            updated_at: start + Duration::minutes(12),
        };
        let task = Task {
            id: TaskId::new(),
            run_id: run.id,
            key: "api".into(),
            title: "api".into(),
            spec: TaskSpec {
                goal: "g".into(),
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: None,
            assignee: Some(worker),
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Failed,
            attempts: 2,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport {
                failure: Some("checks did not pass".into()),
                ..TaskReport::default()
            },
            parent: None,
            progress: None,
            handoffs: Vec::new(),
            created_at: start,
            updated_at: start + Duration::minutes(10),
        };

        let mut at = start;
        let mut events: Vec<Event> = Vec::new();
        let mut push = |kind: EventKind| {
            at += Duration::seconds(30);
            events.push(Event {
                seq: events.len() as u64 + 1,
                id: uuid::Uuid::now_v7(),
                ts: at,
                schema: EVENT_SCHEMA_VERSION,
                kind,
            });
        };
        push(EventKind::RunCreated { run: Box::new(run.clone()) });
        push(EventKind::TaskStatusChanged {
            run_id: run.id,
            id: task.id,
            key: "api".into(),
            from: TaskStatus::Pending,
            to: TaskStatus::Running,
            assignee: Some(worker),
        });
        for i in 0..2 {
            let call = format!("t{i}");
            push(EventKind::AgentToolCall {
                id: worker,
                tool_call_id: call.clone(),
                title: "cargo test".into(),
                kind: Some("execute".into()),
                status: ToolStatus::InProgress,
            });
            push(EventKind::AgentToolDetail {
                id: worker,
                tool_call_id: call,
                detail: ToolDetail {
                    command: Some("cargo test -p auth".into()),
                    exit_code: Some(101),
                    duration_ms: Some(9000),
                    output: Some("test result: FAILED".into()),
                    ..ToolDetail::default()
                },
            });
        }
        push(EventKind::AgentUsage {
            id: worker,
            used: 150_000,
            size: 200_000,
            cost_usd: Some(0.30),
            tokens: Some(TokenUsage {
                total: 3_000,
                ..TokenUsage::default()
            }),
        });
        push(EventKind::AgentUsage {
            id: worker,
            used: 40_000,
            size: 200_000,
            cost_usd: Some(0.90),
            tokens: Some(TokenUsage {
                total: 9_000,
                ..TokenUsage::default()
            }),
        });
        push(EventKind::AgentTurnCompleted {
            id: worker,
            message_id: MessageId::new(),
            stop_reason: StopReason::Error,
        });
        push(EventKind::TaskChecked {
            run_id: run.id,
            id: task.id,
            key: "api".into(),
            passed: false,
            checks: vec![CheckResult {
                command: "cargo test -p crossagents-cli".into(),
                exit_code: Some(101),
                passed: false,
                output: String::new(),
            }],
        });
        push(EventKind::TaskHandedOff {
            run_id: run.id,
            handoff: crossagents_core::Handoff {
                id: crossagents_core::HandoffId::new(),
                run_id: run.id,
                task_id: task.id,
                task_key: "api".into(),
                kind: crossagents_core::HandoffKind::Reassign,
                from: Some(worker),
                from_name: Some("rex".into()),
                to: AgentId::new(),
                to_name: "w-a1b2c3d4-api".into(),
                template: Some("ada".into()),
                reason: Some("it stalled".into()),
                goal: "g".into(),
                files: vec!["crates/cli/src/cmd/session.rs".into()],
                summary: None,
                decisions: Vec::new(),
                open_questions: Vec::new(),
                context_tokens: None,
                fresh: true,
                outcome: None,
                at: start,
            },
        });
        let issue = Note {
            scope: NoteScope::Run,
            run_id: Some(run.id),
            key: "issue/1".into(),
            text: "CodeBuddy worker 的写权限被拒".into(),
            author: worker,
            updated_at: at,
        };
        (run, vec![task], events, vec![issue])
    }

    /// The fixed trace becomes the document the person reads: a 现象 / 影响 / 方向 table with
    /// one row per thing that went wrong, the issue note included.
    #[test]
    fn a_fixed_trace_becomes_the_three_column_table() {
        let (run, tasks, events, issues) = fixture();
        let doc = render(&run, &tasks, &events, &issues);
        let md = &doc.markdown;

        assert!(md.starts_with("# RETRO-"), "{md}");
        assert!(md.contains("## 怎么跑的"));
        assert!(md.contains("- 分支：`crossagents/a1b2c3d4`，从 `main`"));
        assert!(md.contains("- 花费：9,000 tokens，$0.90"));
        assert!(md.contains("## 结果"));
        assert!(md.contains("- #api「失败」— 没有留 summary"));
        assert!(md.contains("| # | 现象 | 影响 | 方向 |"));
        // Failed task, failed check, the twice-failed command, the failed turn, the handoff,
        // the compaction, the unlanded run, and the issue note.
        assert_eq!(doc.findings, 8, "{md}");
        assert_eq!(doc.issues, 1);
        assert!(md.contains("| 1 | #api 以「失败」结束：checks did not pass |"));
        assert!(md.contains("`cargo test -p auth` 失败 2 次（最近 exit 101）"));
        assert!(md.contains("同一处白跑 2 轮"));
        assert!(md.contains("#api：1 个回合失败或中断"));
        assert!(md.contains("#api 的验收 1/1 次没过"));
        assert!(md.contains("#api 换了 1 次手：rex → w-a1b2c3d4-api"));
        assert!(md.contains("#api 的上下文压缩了 1 次"));
        assert!(md.contains("run：结束了但没有落地"));
        assert!(md.contains("[issue/1] CodeBuddy worker 的写权限被拒"));
        assert!(md.contains("- 事件：11 条（按 run 折叠）"));
        assert!(md.contains("- 重新生成：`crossagents run retro"));

        // The file the command writes is named for the run's short id.
        let hex = run.id.to_string().replace('-', "");
        assert_eq!(
            file_name(run.id),
            format!("RETRO-{}.md", &hex[hex.len() - 8..])
        );
    }

    #[test]
    fn the_document_lands_under_docs_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let (run, ..) = fixture();
        let doc = RetroDoc {
            markdown: "# 一句话\n".into(),
            findings: 0,
            issues: 0,
        };
        let path = write(dir.path(), run.id, &doc, false).unwrap();
        assert_eq!(
            path,
            dir.path()
                .join("docs")
                .join("tasks")
                .join(file_name(run.id))
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "# 一句话\n");
    }

    /// A second look at the same run does not silently replace the document: only `force`
    /// writes over it.
    #[test]
    fn an_existing_retro_is_kept_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (run, ..) = fixture();
        let first = RetroDoc {
            markdown: "# first\n".into(),
            findings: 0,
            issues: 0,
        };
        let path = write(dir.path(), run.id, &first, false).unwrap();
        let second = RetroDoc {
            markdown: "# second\n".into(),
            findings: 0,
            issues: 0,
        };
        let refused = write(dir.path(), run.id, &second, false).unwrap_err();
        assert!(
            matches!(&refused, crossagents_core::CoreError::Conflict(message) if message.contains("already exists")),
            "{refused}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# first\n");
        write(dir.path(), run.id, &second, true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# second\n");
    }

    /// `docs` is a junction/symlink pointing out of the checkout: the retro is refused there,
    /// so a run cannot write a file into a directory outside the repository. Skipped where
    /// the link cannot be made (a Windows account without the privilege).
    #[test]
    fn docs_tasks_outside_the_checkout_is_refused() {
        let checkout = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("tasks")).unwrap();
        if link_dir(outside.path(), &checkout.path().join("docs")).is_err() {
            eprintln!("skipped: cannot link a directory here");
            return;
        }
        let (run, ..) = fixture();
        let doc = RetroDoc {
            markdown: "# outside\n".into(),
            findings: 0,
            issues: 0,
        };
        let refused = write(checkout.path(), run.id, &doc, false).unwrap_err();
        assert!(
            matches!(&refused, crossagents_core::CoreError::Conflict(message) if message.contains("outside")),
            "{refused}"
        );
        assert!(
            !outside
                .path()
                .join("tasks")
                .join(file_name(run.id))
                .exists()
        );
    }

    /// A directory link: a junction on Windows (no privilege needed), a symlink elsewhere.
    #[cfg(windows)]
    fn link_dir(original: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(original)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("mklink /J failed"))
        }
    }

    #[cfg(unix)]
    fn link_dir(original: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }
}

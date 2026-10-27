//! How relayed content is laid out (docs/tasks/H1.md §2: relay, do not speak). Everything
//! here formats what someone else wrote — the person's request, a task's spec, a worker's
//! summary — plus the facts the hub knows (ids, branches, diff stats, check results).
//! How to act on it is in each role's guide (`protocol.rs`), not in these messages.

use std::fmt::Write;

use crossagents_core::{Agent, AgentId, CheckResult, Handoff, HandoffKind, Role, Run, Task};

use crate::project::{self, RunChecks};

/// The run's request as the person wrote it, then the facts the lead needs.
pub fn kickoff(
    run: &Run,
    team: &[Agent],
    default_worker: AgentId,
    checks: &Result<RunChecks, String>,
    sessions: &str,
) -> String {
    let mut out = run.request.trim().to_owned();
    if run.in_place {
        let _ = write!(
            out,
            "\n\n---\nrun: {} (run_id: {}) · {} is not under version control: tasks run one at a \
             time directly in it, with no branches, diffs or merges, and nothing can be rolled back",
            run.title,
            run.id,
            run.repo.display()
        );
    } else {
        let _ = write!(
            out,
            "\n\n---\nrun: {} (run_id: {}) · base branch {} · run branch {}",
            run.title, run.id, run.base_branch, run.branch
        );
        match checks {
            Ok(c) if c.commands.is_empty() => {}
            Ok(c) => {
                let list: Vec<String> = c.commands.iter().map(|c| format!("`{c}`")).collect();
                let _ = write!(
                    out,
                    "\nchecks ({}): before you land or finish with changes, the hub runs {} in \
                     your checkout; they must pass",
                    project::FILE,
                    list.join(", ")
                );
            }
            Err(e) => {
                let _ = write!(
                    out,
                    "\nchecks: {e}; landing and finishing are refused until it is fixed"
                );
            }
        }
    }
    out.push_str(&roster(team, default_worker));
    if !team.iter().any(|a| a.role == Role::Worker) {
        // No worker was picked: tasks, if any, run on the lead's model.
        out.push_str(
            "\nworkers: none on the team; a task you assign to nobody goes to a copy of you",
        );
    }
    // The workspace's sessions, for working with one of them itself (docs/tasks/S1.md §3.6).
    out.push_str(sessions);
    out
}

/// The team: who is good at what, for picking assignees.
pub fn roster(team: &[Agent], default_worker: AgentId) -> String {
    let line = |a: &Agent| {
        let who = match (&a.profile.display_name, &a.profile.title) {
            (Some(name), Some(title)) => format!("{name}, {title}"),
            (Some(name), None) => name.clone(),
            (None, Some(title)) => title.clone(),
            (None, None) => "no title".to_owned(),
        };
        let model = a.model.as_deref().unwrap_or(&a.plugin);
        let mut s = format!("- {} — {who} · {model}", a.name);
        if let Some(persona) = &a.profile.persona {
            let _ = write!(s, ". {}", first_sentence(persona, 160));
        }
        if a.id == default_worker {
            s.push_str(" (default)");
        }
        s
    };
    let mut out = String::new();
    for (role, title) in [(Role::Worker, "workers"), (Role::Reviewer, "reviewers")] {
        let lines: Vec<String> = team.iter().filter(|a| a.role == role).map(line).collect();
        if !lines.is_empty() {
            let _ = write!(out, "\n{title}:\n{}", lines.join("\n"));
        }
    }
    out
}

fn list(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(out, "{title}:");
    for item in items {
        let _ = writeln!(out, "  - {item}");
    }
}

/// A task as its issuer specified it. The `task_id:` line is what task tools take.
pub fn task(task: &Task) -> String {
    let spec = &task.spec;
    let mut out = format!(
        "Task #{} — {}\ntask_id: {}\n\n{}\n\n",
        task.key,
        task.title,
        task.id,
        spec.goal.trim()
    );
    list(&mut out, "Files you may change", &spec.scope.allow);
    list(&mut out, "Files you must not change", &spec.scope.deny);
    list(&mut out, "Interfaces", &spec.interfaces);
    list(&mut out, "Read first", &spec.references);
    if !spec.acceptance.is_empty() {
        out.push_str("Checks (run by the hub when you report done):\n");
        for command in &spec.acceptance {
            let _ = writeln!(out, "  $ {command}");
        }
    }
    if !task.depends_on.is_empty() {
        let _ = writeln!(
            out,
            "After: {}",
            task.depends_on
                .iter()
                .map(|d| format!("#{d}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(branch) = &task.branch {
        let _ = writeln!(out, "Branch: {branch}");
    }
    out.trim_end().to_owned()
}

/// A task as its worker gets it: the spec, and how the work ends. Said here too because a
/// worker that learned it only from its session instructions may forget it after a long
/// turn.
pub fn assignment(task: &Task) -> String {
    format!(
        "{}\n\nWhen done, report with `task_complete` (this task_id and a summary); a turn that \
         ends without a report leaves the task stalled.",
        self::task(task)
    )
}

/// A task as the worker it was handed to gets it: the assignment, then the handoff card —
/// who had it, why it moved, what they changed and left open. A first assignment is the
/// assignment alone.
pub fn handover(task: &Task, card: &Handoff) -> String {
    let mut out = assignment(task);
    if card.kind == HandoffKind::Assign && card.reason.is_none() {
        return out;
    }
    let kind = match card.kind {
        HandoffKind::Assign => "assigned",
        HandoffKind::Reassign => "reassigned",
        HandoffKind::Reroute => "rerouted",
        HandoffKind::Escalate => "escalated",
    };
    let _ = write!(
        out,
        "\n\n---\nhandoff: {} → {} ({kind})\n",
        card.from_name.as_deref().unwrap_or("?"),
        card.to_name
    );
    if let Some(reason) = &card.reason {
        let _ = writeln!(out, "why: {}", reason.trim());
    }
    if card.kind != HandoffKind::Assign {
        out.push_str("Someone already worked on this; their work is in your working directory.\n");
        if card.files.is_empty() {
            out.push_str("Files they changed: none that the hub could see\n");
        } else {
            list(&mut out, "Files they changed", &card.files);
        }
    }
    if let Some(summary) = &card.summary {
        let _ = writeln!(out, "Their last word: {}", summary.trim());
    }
    // The spec's interfaces are in the assignment already.
    let decisions: Vec<String> = card
        .decisions
        .iter()
        .filter(|d| !task.spec.interfaces.contains(d))
        .cloned()
        .collect();
    list(&mut out, "Decided since (keep to it)", &decisions);
    list(
        &mut out,
        "Open questions (asked, not answered; task_ask again if you need them)",
        &card.open_questions,
    );
    out.trim_end().to_owned()
}

/// `#key (task id …)`, how task messages to an issuer name the task.
pub fn task_ref(task: &Task) -> String {
    format!("#{} ({})", task.key, task.id)
}

/// What the hub knows about a finished attempt, after the worker's own words.
pub fn facts(task: &Task) -> String {
    let mut out = format!("---\ntask {}", task_ref(task));
    if let Some(branch) = &task.branch {
        let _ = write!(out, " · branch {branch}");
    }
    if let Some(dir) = &task.worktree {
        let _ = write!(out, "\nworktree: {}", dir.display());
    }
    match task.report.diff_stat.as_deref().map(str::trim) {
        Some(stat) if !stat.is_empty() => {
            let _ = write!(out, "\nchanges:\n{stat}");
        }
        _ => out.push_str("\nchanges: none"),
    }
    if !task.report.checks.is_empty() {
        let _ = write!(out, "\nchecks: {}", checks(&task.report.checks));
    }
    out
}

/// "2/2 passed", or the first failure with its output.
pub fn checks(checks: &[CheckResult]) -> String {
    let passed = checks.iter().filter(|c| c.passed).count();
    match checks.iter().find(|c| !c.passed) {
        None => format!("{passed}/{} passed", checks.len()),
        Some(bad) => format!(
            "{passed}/{} passed; failed: $ {} (exit {})\n{}",
            checks.len(),
            bad.command,
            bad.exit_code.map_or("none".into(), |c| c.to_string()),
            bad.output
        ),
    }
}

/// Up to the first sentence of `text`, at most `max` characters.
fn first_sentence(text: &str, max: usize) -> String {
    let text = text.trim();
    let end = text
        .char_indices()
        .find(|&(_, c)| matches!(c, '.' | '。' | '\n'))
        .map_or(text.len(), |(i, c)| i + c.len_utf8());
    let sentence = text[..end].trim_end_matches('\n');
    if sentence.chars().count() > max {
        let cut: String = sentence.chars().take(max).collect();
        format!("{}…", cut.trim_end())
    } else {
        sentence.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{HandoffId, RunId, TaskId, TaskReport, TaskSpec, TaskStatus};

    use super::*;

    fn task() -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            run_id: RunId::new(),
            key: "a".into(),
            title: "task a".into(),
            spec: TaskSpec {
                goal: "do a".into(),
                interfaces: vec!["fn a()".into()],
                ..TaskSpec::default()
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
            status: TaskStatus::Running,
            attempts: 1,
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

    fn card(kind: HandoffKind) -> Handoff {
        Handoff {
            id: HandoffId::new(),
            run_id: RunId::new(),
            task_id: TaskId::new(),
            task_key: "a".into(),
            kind,
            from: None,
            from_name: Some("Rex".into()),
            to: AgentId::new(),
            to_name: "Ada".into(),
            reason: Some("Rex is stuck".into()),
            goal: "do a".into(),
            files: vec!["src/a.rs".into()],
            summary: Some("half done".into()),
            decisions: vec!["fn a()".into(), "keep the old API".into()],
            open_questions: vec!["which way?".into()],
            context_tokens: Some(1200),
            at: Utc::now(),
        }
    }

    #[test]
    fn a_handover_carries_the_card() {
        let task = task();
        let text = handover(&task, &card(HandoffKind::Reassign));
        assert!(text.starts_with(&assignment(&task)), "{text}");
        for part in [
            "handoff: Rex → Ada (reassigned)",
            "why: Rex is stuck",
            "  - src/a.rs",
            "Their last word: half done",
            "  - keep the old API",
            "  - which way?",
        ] {
            assert!(text.contains(part), "{part} missing:\n{text}");
        }
        // The interfaces are in the assignment once, not again among the decisions.
        assert_eq!(text.matches("fn a()").count(), 1, "{text}");
    }

    #[test]
    fn a_first_assignment_is_the_assignment() {
        let task = task();
        let mut first = card(HandoffKind::Assign);
        first.reason = None;
        assert_eq!(handover(&task, &first), assignment(&task));
    }
}

//! How relayed content is laid out (docs/tasks/H1.md §2: relay, do not speak). Everything
//! here formats what someone else wrote — the person's request, a task's spec, a worker's
//! summary — plus the facts the hub knows (ids, branches, diff stats, check results).
//! How to act on it is in each role's guide (`protocol.rs`), not in these messages.

use std::fmt::Write;

use crossagents_core::{
    Agent, AgentId, CheckResult, ChecksState, Handoff, HandoffKind, LandPolicy, Run, Task,
};

use crate::project::{self, RunChecks, RunEnv};

// Kept in sync with the GUI kickoff recognizer.
const KICKOFF_SEPARATOR: &str = "\n\n---\nrun: ";

/// A run whose plan waits for the person: the lead writes the tasks, the person staffs them.
/// Without this the lead reads its guide's "do it yourself" as permission to skip the plan,
/// and no plan ever reaches the person (docs/AGENT-WORKSPACE.md §6.1).
const PLAN_WAITS: &str = "\nplan: the person approves it and picks who runs each task. Before you \
     change anything, call `plan_submit` with every task and what its session must do — write \
     each spec.goal so a worker who sees only it and the repository can do the work. Do not \
     start the work yourself. Answering a question, or reporting that nothing needs changing, \
     needs no plan.";

pub fn previous_rounds(rounds: &[Run]) -> String {
    let mut out = String::new();
    if !rounds.is_empty() {
        out.push_str("\nPrevious rounds in this work group:");
    }
    for run in rounds.iter().take(5) {
        let _ = write!(
            out,
            "\n- {} · {:?} · {} · {}",
            run.title,
            run.status,
            if run.landed.is_some() {
                "landed"
            } else {
                "not landed"
            },
            run.summary
                .as_deref()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
        );
    }
    out
}

/// The run's request as the person wrote it, then the facts the lead needs.
pub fn kickoff(
    run: &Run,
    team: &[Agent],
    default_worker: AgentId,
    checks: &Result<RunChecks, String>,
    env: Option<&RunEnv>,
    sessions: &str,
) -> String {
    kickoff_with(
        run,
        team,
        default_worker,
        checks,
        env,
        sessions,
        &crate::machine::current(),
    )
}

/// [`kickoff`] with the machine its toolchain line describes (docs/tasks/SELF.md B4): a test
/// gives a fake one, and the hub gives what it probed.
pub fn kickoff_with(
    run: &Run,
    team: &[Agent],
    default_worker: AgentId,
    checks: &Result<RunChecks, String>,
    env: Option<&RunEnv>,
    sessions: &str,
    machine: &crate::machine::Machine,
) -> String {
    let mut out = run.request.trim().to_owned();
    out.push_str(KICKOFF_SEPARATOR);
    if run.in_place {
        let _ = write!(
            out,
            "{} (run_id: {}) · {} is not under version control: tasks run one at a \
             time directly in it, with no branches, diffs or merges, and nothing can be rolled back",
            run.title,
            run.id,
            run.repo.display()
        );
        if !run.config.auto_approves() {
            out.push_str(PLAN_WAITS);
        }
    } else {
        let _ = write!(
            out,
            "{} (run_id: {}) · base branch {} · run branch {}",
            run.title, run.id, run.base_branch, run.branch
        );
        out.push_str(match run.config.policy.land {
            LandPolicy::Manual => {
                "\nlanding: the person lands it; when every task has ended, close with run_finish and \
                 do not merge into the base branch yourself"
            }
            LandPolicy::Auto => "\nlanding: you land it yourself with run_land, then run_finish",
            LandPolicy::Pr => {
                "\nlanding: you may only open a pull request (run_land mode pr), then run_finish"
            }
        });
        if !run.config.auto_approves() {
            out.push_str(PLAN_WAITS);
        }
        match checks {
            Ok(c) if c.checks.is_empty() => {
                let _ = write!(
                    out,
                    "\nchecks: none declared in {}; the hub will not verify this run",
                    project::FILE
                );
            }
            Ok(c) => {
                let list: Vec<String> = c
                    .checks
                    .iter()
                    .map(|c| match c.paths.as_slice() {
                        [] => format!("`{}`", c.command),
                        paths => format!("`{}` (when {} changes)", c.command, paths.join(", ")),
                    })
                    .collect();
                let _ = write!(
                    out,
                    "\nchecks ({}): before you land or finish with changes, the hub runs {} in \
                     your checkout; they must pass. It runs them itself, in your build \
                     directory: run only the tests your change needs, and let any build or test \
                     you started finish before you call run_finish or run_land",
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
        if let Some(env) = env {
            out.push_str(&environment(env));
        }
    }
    // What the machine the session runs on offers: a fact, so that a turn is not spent finding
    // out that `python` here is Windows' Store placeholder (docs/tasks/SELF.md B4).
    if let Some(line) = machine.line() {
        let _ = write!(out, "\n{line}");
    }
    out.push_str(&roster(team, default_worker));
    if !team.iter().any(|a| a.id == default_worker) {
        // No worker was picked: tasks, if any, run on the lead's model.
        out.push_str(
            "\nno default worker on the team: a task you assign to nobody goes to a copy of you",
        );
    }
    // The workspace's sessions, for working with one of them itself (docs/tasks/S1.md §3.6).
    out.push_str(sessions);
    out
}

/// What the run's checkout sets in its agents' and checks' environment, and from where; a
/// variable presets disagree on is named, since it is left unset.
fn environment(env: &RunEnv) -> String {
    let mut out = String::new();
    if !env.vars.is_empty() {
        let vars: Vec<String> = env
            .vars
            .iter()
            .map(|v| format!("{}={} ({})", v.name, v.value, v.source))
            .collect();
        let _ = write!(out, "\nbuild env: {}", vars.join(", "));
    }
    for c in &env.conflicts {
        let _ = write!(
            out,
            "\nbuild env: {} left unset: presets {} set it differently ({} can settle it in [run.env])",
            c.name,
            c.presets.join(", "),
            project::FILE
        );
    }
    out
}

/// How far the project's checks verified `run`, for the lead's landing and finishing.
pub fn verification(run: &Run) -> Option<String> {
    Some(match run.checks_state? {
        ChecksState::NotConfigured => {
            format!("not configured in {}: nothing was verified", project::FILE)
        }
        ChecksState::NotApplicable { declared } => format!(
            "none of the {declared} declared in {} applies to this change: nothing was verified",
            project::FILE
        ),
        ChecksState::Ran => checks(&run.checks),
    })
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
    // One list: who works on a task and who reviews one is decided per task, not by how a
    // session was made.
    if team.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = team.iter().map(line).collect();
    format!(
        "\nteam (name one as a task's assignee or reviewer):\n{}",
        lines.join("\n")
    )
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
    if let Some(schema) = &spec.result_schema {
        let _ = writeln!(
            out,
            "Result schema (task_complete requires result matching this JSON Schema):\n{schema}\n"
        );
    }
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
    assignment_with(task, &crate::machine::current())
}

/// [`assignment`] with the machine its toolchain line describes (docs/tasks/SELF.md B4): a
/// test gives a fake one, and the hub gives what it probed.
pub fn assignment_with(task: &Task, machine: &crate::machine::Machine) -> String {
    let mut out = format!(
        "{}\n\nWhen done, report with `task_complete` (this task_id and a summary); a turn that \
         ends without a report leaves the task stalled.",
        self::task(task)
    );
    // What the machine the worker runs on offers (docs/tasks/SELF.md B4).
    if let Some(line) = machine.line() {
        let _ = write!(out, "\n\n{line}");
    }
    out
}

/// A task as the worker it was handed to gets it: the assignment, then the handoff card —
/// who had it, why it moved, what they changed and left open. A first assignment is the
/// assignment alone.
pub fn handover(task: &Task, card: &Handoff) -> String {
    handover_with(task, card, &crate::machine::current())
}

/// [`handover`] with the machine its toolchain line describes (docs/tasks/SELF.md B4).
pub fn handover_with(task: &Task, card: &Handoff, machine: &crate::machine::Machine) -> String {
    let mut out = assignment_with(task, machine);
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
        card.to_label()
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
            template: None,
            reason: Some("Rex is stuck".into()),
            goal: "do a".into(),
            files: vec!["src/a.rs".into()],
            summary: Some("half done".into()),
            decisions: vec!["fn a()".into(), "keep the old API".into()],
            open_questions: vec!["which way?".into()],
            context_tokens: Some(1200),
            fresh: true,
            outcome: None,
            at: Utc::now(),
        }
    }

    #[test]
    fn kickoff_keeps_request_and_context_boundary() {
        use crossagents_core::{PlanApproval, RunConfig, RunPolicy, RunStatus};
        let now = Utc::now();
        let worker = AgentId::new();
        let mut run = Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "title".into(),
            request: "  hello\nworld  ".into(),
            planner: AgentId::new(),
            status: RunStatus::Planning,
            config: RunConfig {
                worker,
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: 1,
                max_attempts: 1,
                team: vec![],
                policy: RunPolicy::default(),
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
        };
        for in_place in [false, true] {
            run.in_place = in_place;
            let text = kickoff(
                &run,
                &[],
                worker,
                &Err("test".into()),
                None,
                "\nsessions: test",
            );
            assert!(
                text.starts_with("hello\nworld\n\n---\nrun: title (run_id: "),
                "{text}"
            );
            assert!(text.contains("\nsessions: test"));
            // The plan waits for the person: the lead is told to write the tasks and stop,
            // not to do the work itself.
            assert!(
                text.contains("plan_submit") && text.contains("picks who runs each task"),
                "{text}"
            );
            // Under automatic approval nothing is added: the guide already covers it.
            run.config.policy.plan_approval = PlanApproval::Auto;
            let auto = kickoff(&run, &[], worker, &Err("test".into()), None, "");
            assert!(!auto.contains("plan_submit"), "{auto}");
            run.config.policy.plan_approval = PlanApproval::Required;
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

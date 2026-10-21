//! The messages CrossAgents sends to planners and workers.

use std::fmt::Write;

use crossagents_core::{Agent, AgentId, CheckResult, Role, Run, Task};

pub fn planner_kickoff(run: &Run, team: &[Agent], default_worker: AgentId) -> String {
    let mut out = kickoff_body(run);
    out.push_str(&team_section(team, default_worker));
    out
}

/// The roster, so the planner can match tasks to specialties.
fn team_section(team: &[Agent], default_worker: AgentId) -> String {
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
    let workers: Vec<String> = team
        .iter()
        .filter(|a| a.role == Role::Worker)
        .map(line)
        .collect();
    let reviewers: Vec<String> = team
        .iter()
        .filter(|a| a.role == Role::Reviewer)
        .map(line)
        .collect();
    let mut out = String::from(
        "\n\nYour team. Match each task to the teammate whose specialty fits it: set the task's \
         `assignee` to a worker's name below, and optionally `reviewer` to a reviewer's name \
         (for example security-sensitive changes to a security reviewer). Tasks without an \
         assignee go to the default worker.\n",
    );
    out.push_str("Workers:\n");
    out.push_str(&workers.join("\n"));
    out.push('\n');
    if !reviewers.is_empty() {
        out.push_str("Reviewers:\n");
        out.push_str(&reviewers.join("\n"));
        out.push('\n');
    }
    out
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

fn kickoff_body(run: &Run) -> String {
    format!(
        r#"You are the PLANNER of a CrossAgents run (run id: {id}).

Request from the human:
{request}

Your job: understand the code in your working directory, design the change, and split it
into small tasks that cheaper, faster worker models will implement. Do NOT implement the
change yourself.

When the plan is ready, call the `plan_submit` tool once with all tasks. Each task has:
- key: short unique id, lowercase letters, digits and dashes (e.g. "db-schema")
- title: one line
- spec.goal: exactly what to do and why. Workers see only this spec and the repository,
  so name files, functions and expected behaviour explicitly.
- spec.scope.allow / spec.scope.deny: glob patterns of files the task may / must not change
- spec.interfaces: signatures and data structures the task must follow
- spec.acceptance: shell commands, run from the repository root, that must all succeed
  (at least one, e.g. "cargo test auth::")
- spec.references: files to read first
- depends_on: keys of tasks that must be merged before this one starts
- assignee / reviewer: names of teammates (see "Your team" below)

Keep each task small enough to finish in one session. Tasks with overlapping scopes run
one after another; independent tasks run in parallel.

After `plan_submit`, end your turn. You will get messages when the plan is approved, when
tasks are merged or fail, and when workers ask questions (answer with `task_answer`).
Use `task_list`, `task_status` and `task_read` to inspect progress."#,
        id = run.id,
        request = run.request.trim(),
    )
}

pub fn plan_rejected(reason: &str) -> String {
    format!(
        "The human rejected your plan:\n{}\n\nRevise it and call `plan_submit` again.",
        reason.trim()
    )
}

pub fn plan_approved(task_count: usize) -> String {
    format!(
        "Your plan was approved; {task_count} task(s) are being dispatched to workers. \
         End your turn and wait for updates."
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

pub fn worker_task(task: &Task) -> String {
    let spec = &task.spec;
    let mut out = format!(
        "You are a WORKER in a CrossAgents run. Complete exactly this task in your working \
         directory (a dedicated git worktree). Do not commit; CrossAgents commits for you.\n\n\
         task_id: {}\nattempt: {}\n\nTitle: {}\n\nGoal:\n{}\n\n",
        task.id,
        task.attempts,
        task.title,
        spec.goal.trim()
    );
    if spec.scope.allow.is_empty() {
        out.push_str("Files you may change: any\n");
    } else {
        list(&mut out, "Files you may change", &spec.scope.allow);
    }
    list(&mut out, "Files you must not change", &spec.scope.deny);
    list(&mut out, "Interfaces to follow", &spec.interfaces);
    list(&mut out, "Read first", &spec.references);
    out.push_str("\nAcceptance (CrossAgents runs these after you report; all must pass):\n");
    for command in &spec.acceptance {
        let _ = writeln!(out, "  $ {command}");
    }
    out.push_str(
        "\nWhen done, call `task_complete` with the task_id and a short summary of what you \
         changed. If the task is impossible or the spec is wrong, call `task_fail` with the \
         reason. If something is unclear, call `task_ask` and wait for the answer.",
    );
    out
}

pub fn checks_failed(task: &Task, checks: &[CheckResult], max_attempts: u32) -> String {
    let mut out = format!(
        "Acceptance failed (attempt {} of {max_attempts}).\n\n",
        task.attempts.saturating_sub(1),
    );
    for check in checks {
        let code = check
            .exit_code
            .map_or_else(|| "no exit code".to_owned(), |c| format!("exit {c}"));
        let _ = writeln!(
            out,
            "$ {} ({code})\n{}\n",
            check.command,
            check.output.trim()
        );
    }
    let _ = write!(
        out,
        "Fix the problem, then call `task_complete` again.\n\ntask_id: {}\nattempt: {}",
        task.id, task.attempts
    );
    out
}

pub fn nudge(task: &Task) -> String {
    format!(
        "You ended your turn without calling `task_complete`, `task_fail` or `task_ask`. \
         Continue the task and report with one of these tools.\n\ntask_id: {}\nattempt: {}",
        task.id, task.attempts
    )
}

pub fn worker_question(task: &Task, question: &str) -> String {
    format!(
        "The worker on task {:?} ({}) asks:\n{}\n\nAnswer with `task_answer`.\n\ntask_id: {}",
        task.key,
        task.title,
        question.trim(),
        task.id
    )
}

pub fn planner_answer(task: &Task, answer: &str) -> String {
    format!(
        "The planner answered your question:\n{}\n\nContinue the task.\n\ntask_id: {}\nattempt: {}",
        answer.trim(),
        task.id,
        task.attempts
    )
}

pub fn task_merged(task: &Task) -> String {
    format!(
        "Task {:?} ({}) passed acceptance and was merged.\nWorker summary: {}",
        task.key,
        task.title,
        task.report.summary.as_deref().unwrap_or("(none)")
    )
}

pub fn task_failed(task: &Task) -> String {
    format!(
        "Task {:?} ({}) failed: {}\nUse `task_read` for details.",
        task.key,
        task.title,
        task.report.failure.as_deref().unwrap_or("unknown reason")
    )
}

pub fn run_finished(run: &Run, tasks: &[Task]) -> String {
    let mut out = format!(
        "The run has finished with status {:?}. Results are on branch `{}`.\n\n",
        run.status, run.branch
    );
    for task in tasks {
        let _ = writeln!(out, "- {} ({}): {:?}", task.key, task.title, task.status);
    }
    out.push_str(
        "\nWrite a short summary for the human: what was done, what failed, and what to check.",
    );
    out
}

pub fn review_request(task: &Task, integration_branch: &str) -> String {
    let mut out = format!(
        "You are a REVIEWER in a CrossAgents run. A worker finished the task below and its \
         acceptance checks passed. Review the change in your working directory: run \
         `git diff {integration_branch}...HEAD` to see it. Do not modify files.\n\n\
         task_id: {}\nattempt: {}\n\nTitle: {}\n\nGoal:\n{}\n\n",
        task.id,
        task.attempts,
        task.title,
        task.spec.goal.trim()
    );
    list(&mut out, "Interfaces to follow", &task.spec.interfaces);
    if let Some(summary) = &task.report.summary {
        let _ = writeln!(out, "Worker summary: {summary}\n");
    }
    out.push_str(
        "Check correctness, that the goal is fully met, that the scope and interfaces were \
         respected, and code quality. Then call `review_submit` with the task_id, \
         `approve` true or false, and notes (for a rejection: exactly what to fix).",
    );
    out
}

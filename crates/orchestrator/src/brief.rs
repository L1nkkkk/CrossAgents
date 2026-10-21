//! How relayed content is laid out (docs/tasks/H1.md §2: relay, do not speak). Everything
//! here formats what someone else wrote — the person's request, a task's spec, a worker's
//! summary — plus the facts the hub knows (ids, branches, diff stats, check results).
//! How to act on it is in each role's guide (`protocol.rs`), not in these messages.

use std::fmt::Write;

use crossagents_core::{Agent, AgentId, CheckResult, Role, Run, Task};

/// The run's request as the person wrote it, then the facts the lead needs.
pub fn kickoff(run: &Run, team: &[Agent], default_worker: AgentId) -> String {
    let mut out = run.request.trim().to_owned();
    let _ = write!(
        out,
        "\n\n---\nrun: {} (run_id: {}) · base branch {} · run branch {}",
        run.title, run.id, run.base_branch, run.branch
    );
    out.push_str(&roster(team, default_worker));
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

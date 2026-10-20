//! Plan validation and scheduling rules.

use std::collections::{HashMap, HashSet};

use crossagents_core::{PlannedTask, Scope, Task, TaskStatus};

const KEY_MAX: usize = 24;

/// Checks a submitted plan; the error lists every problem so the planner can fix them in
/// one go.
pub fn validate(tasks: &[PlannedTask]) -> Result<(), String> {
    let mut problems = Vec::new();
    if tasks.is_empty() {
        problems.push("the plan has no tasks".to_owned());
    }
    let mut keys = HashSet::new();
    for task in tasks {
        let key = &task.key;
        if !valid_key(key) {
            problems.push(format!(
                "task key {key:?} must be 1-{KEY_MAX} characters of a-z, 0-9 and -"
            ));
        }
        if !keys.insert(key.as_str()) {
            problems.push(format!("duplicate task key {key:?}"));
        }
        if task.title.trim().is_empty() {
            problems.push(format!("task {key:?} needs a title"));
        }
        if task.spec.goal.trim().is_empty() {
            problems.push(format!("task {key:?} needs spec.goal"));
        }
        if task.spec.acceptance.iter().all(|c| c.trim().is_empty()) {
            problems.push(format!(
                "task {key:?} needs at least one spec.acceptance command"
            ));
        }
    }
    for task in tasks {
        for dep in &task.depends_on {
            if dep == &task.key {
                problems.push(format!("task {:?} depends on itself", task.key));
            } else if !keys.contains(dep.as_str()) {
                problems.push(format!(
                    "task {:?} depends on unknown task {dep:?}",
                    task.key
                ));
            }
        }
    }
    if problems.is_empty()
        && let Some(cycle) = find_cycle(tasks)
    {
        problems.push(format!("dependency cycle: {}", cycle.join(" -> ")));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= KEY_MAX
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn find_cycle(tasks: &[PlannedTask]) -> Option<Vec<String>> {
    let deps: HashMap<&str, &[String]> = tasks
        .iter()
        .map(|t| (t.key.as_str(), t.depends_on.as_slice()))
        .collect();
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit<'a>(
        key: &'a str,
        deps: &HashMap<&'a str, &'a [String]>,
        marks: &mut HashMap<&'a str, Mark>,
        path: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match marks.get(key) {
            Some(Mark::Done) => return None,
            Some(Mark::Visiting) => {
                let start = path.iter().position(|k| *k == key).unwrap_or(0);
                let mut cycle: Vec<String> = path[start..].iter().map(|k| k.to_string()).collect();
                cycle.push(key.to_owned());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(key, Mark::Visiting);
        path.push(key);
        for dep in deps.get(key).copied().unwrap_or_default() {
            if let Some(cycle) = visit(dep, deps, marks, path) {
                return Some(cycle);
            }
        }
        path.pop();
        marks.insert(key, Mark::Done);
        None
    }
    let mut marks = HashMap::new();
    for task in tasks {
        if let Some(cycle) = visit(&task.key, &deps, &mut marks, &mut Vec::new()) {
            return Some(cycle);
        }
    }
    None
}

/// The literal directory/file prefix of a glob pattern.
fn literal_prefix(pattern: &str) -> &str {
    let end = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    pattern[..end].trim_start_matches("./")
}

/// Whether two tasks may touch the same files. An empty `allow` means "anything".
pub fn scopes_overlap(a: &Scope, b: &Scope) -> bool {
    if a.allow.is_empty() || b.allow.is_empty() {
        return true;
    }
    a.allow.iter().any(|pa| {
        b.allow.iter().any(|pb| {
            let (x, y) = (literal_prefix(pa), literal_prefix(pb));
            x.starts_with(y) || y.starts_with(x)
        })
    })
}

/// Tasks that can start now: pending, every dependency merged, no scope overlap with work in
/// progress or with each other, and within the parallelism limit.
pub fn ready_tasks(tasks: &[Task], max_parallel: u32) -> Vec<&Task> {
    let merged: HashSet<&str> = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Merged)
        .map(|t| t.key.as_str())
        .collect();
    let mut active: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.status != TaskStatus::Pending && !t.status.is_terminal())
        .collect();
    let mut ready = Vec::new();
    for task in tasks.iter().filter(|t| t.status == TaskStatus::Pending) {
        if active.len() >= max_parallel.max(1) as usize {
            break;
        }
        let deps_done = task.depends_on.iter().all(|d| merged.contains(d.as_str()));
        let clashes = active
            .iter()
            .any(|other| scopes_overlap(&task.spec.scope, &other.spec.scope));
        if deps_done && !clashes {
            active.push(task);
            ready.push(task);
        }
    }
    ready
}

/// Pending tasks that can never run because a dependency failed or was cancelled.
pub fn blocked_tasks(tasks: &[Task]) -> Vec<&Task> {
    let dead: HashSet<&str> = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::Cancelled))
        .map(|t| t.key.as_str())
        .collect();
    tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .filter(|t| t.depends_on.iter().any(|d| dead.contains(d.as_str())))
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{RunId, TaskId, TaskReport, TaskSpec};

    use super::*;

    fn planned(key: &str, deps: &[&str]) -> PlannedTask {
        PlannedTask {
            key: key.into(),
            title: key.into(),
            spec: TaskSpec {
                goal: "g".into(),
                acceptance: vec!["true".into()],
                ..Default::default()
            },
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn task(key: &str, status: TaskStatus, allow: &[&str], deps: &[&str]) -> Task {
        Task {
            id: TaskId::new(),
            run_id: RunId::new(),
            key: key.into(),
            title: key.into(),
            spec: TaskSpec {
                goal: "g".into(),
                scope: Scope {
                    allow: allow.iter().map(|a| a.to_string()).collect(),
                    deny: vec![],
                },
                ..Default::default()
            },
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            assignee: None,
            reviewer: None,
            status,
            attempts: 0,
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn validates_plans() {
        assert!(validate(&[planned("a", &[]), planned("b", &["a"])]).is_ok());
        assert!(validate(&[]).is_err());
        let err =
            validate(&[planned("a", &["x"]), planned("a", &[]), planned("Bad", &[])]).unwrap_err();
        assert!(err.contains("unknown task \"x\""), "{err}");
        assert!(err.contains("duplicate task key"), "{err}");
        assert!(err.contains("\"Bad\""), "{err}");
        let mut no_check = planned("c", &[]);
        no_check.spec.acceptance.clear();
        assert!(validate(&[no_check]).unwrap_err().contains("acceptance"));
    }

    #[test]
    fn detects_cycles() {
        let err = validate(&[
            planned("a", &["c"]),
            planned("b", &["a"]),
            planned("c", &["b"]),
        ])
        .unwrap_err();
        assert!(err.contains("cycle"), "{err}");
    }

    #[test]
    fn scope_overlap() {
        let s = |allow: &[&str]| Scope {
            allow: allow.iter().map(|a| a.to_string()).collect(),
            deny: vec![],
        };
        assert!(scopes_overlap(&s(&[]), &s(&["src/a.rs"])));
        assert!(scopes_overlap(&s(&["src/**"]), &s(&["src/auth/mod.rs"])));
        assert!(!scopes_overlap(&s(&["src/auth/**"]), &s(&["docs/**"])));
        assert!(!scopes_overlap(&s(&["./web/*.ts"]), &s(&["server/*.rs"])));
    }

    #[test]
    fn schedules_by_deps_scope_and_limit() {
        let tasks = vec![
            task("a", TaskStatus::Merged, &["a/**"], &[]),
            task("b", TaskStatus::Pending, &["b/**"], &["a"]),
            task("c", TaskStatus::Pending, &["c/**"], &["b"]),
            task("d", TaskStatus::Pending, &["b/x"], &[]),
            task("e", TaskStatus::Pending, &["e/**"], &[]),
        ];
        let keys = |v: Vec<&Task>| v.iter().map(|t| t.key.clone()).collect::<Vec<_>>();
        // d overlaps b, which comes first.
        assert_eq!(keys(ready_tasks(&tasks, 3)), ["b", "e"]);
        assert_eq!(keys(ready_tasks(&tasks, 1)), ["b"]);

        let mut running = tasks.clone();
        running[1].status = TaskStatus::Running;
        assert_eq!(keys(ready_tasks(&running, 3)), ["e"]);
    }

    #[test]
    fn failed_dependencies_block() {
        let tasks = vec![
            task("a", TaskStatus::Failed, &["a"], &[]),
            task("b", TaskStatus::Pending, &["b"], &["a"]),
            task("c", TaskStatus::Pending, &["c"], &[]),
        ];
        let blocked: Vec<_> = blocked_tasks(&tasks)
            .iter()
            .map(|t| t.key.clone())
            .collect();
        assert_eq!(blocked, ["b"]);
    }
}

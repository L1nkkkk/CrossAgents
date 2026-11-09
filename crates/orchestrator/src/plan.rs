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
        // Its worktree would be `<run>/targets`, where every worktree of the run builds.
        if key == crate::workspace::SHARED_TARGETS {
            problems.push(format!(
                "task key {key:?} is taken by the build directories; choose another"
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
    // A later stage starts only once this one has ended, so waiting for a task in it would
    // never end (docs/research/multica.md §2.2).
    let stages: HashMap<&str, Option<u32>> = tasks
        .iter()
        .map(|t| (t.key.as_str(), t.spec.stage))
        .collect();
    for task in tasks {
        let Some(own) = task.spec.stage else {
            continue;
        };
        for dep in &task.depends_on {
            let Some(Some(dep_stage)) = stages.get(dep.as_str()).copied() else {
                continue;
            };
            if dep_stage > own {
                problems.push(format!(
                    "task {:?} (stage {own}) depends on {dep:?} (stage {dep_stage}), which starts only after stage {own} has ended",
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

/// Keys of planned tasks whose `scope.allow` is empty. The scheduler reads an empty allow as
/// "may touch any file" ([`scopes_overlap`]), so such a task never runs beside another one:
/// a plan that wants parallelism lists the files each task will change.
pub fn empty_scope_keys(tasks: &[PlannedTask]) -> Vec<&str> {
    tasks
        .iter()
        .filter(|t| t.spec.scope.allow.is_empty())
        .map(|t| t.key.as_str())
        .collect()
}

/// Tasks that can start now: pending, every dependency merged, every earlier stage ended (see
/// [`crate::stage`]), no scope overlap with work in progress or with each other, and within the
/// parallelism limit.
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
        // A task split off another works in its own worktree and merges back into it: the
        // tasks it came from neither block it nor count against the limit (they wait for it).
        let others: Vec<&&Task> = active
            .iter()
            .filter(|other| !is_ancestor(tasks, other, task))
            .collect();
        if others.len() >= max_parallel.max(1) as usize {
            continue;
        }
        let deps_done = task.depends_on.iter().all(|d| merged.contains(d.as_str()));
        let clashes = others
            .iter()
            .any(|other| scopes_overlap(&task.spec.scope, &other.spec.scope));
        if deps_done && !clashes && crate::stage::released(tasks, task.spec.stage) {
            active.push(task);
            ready.push(task);
        }
    }
    ready
}

/// Whether `ancestor` is `task`'s parent, or its parent's, and so on.
fn is_ancestor(tasks: &[Task], ancestor: &Task, task: &Task) -> bool {
    let mut current = task.parent;
    let mut steps = 0;
    while let Some(id) = current {
        if id == ancestor.id {
            return true;
        }
        current = tasks.iter().find(|t| t.id == id).and_then(|t| t.parent);
        steps += 1;
        if steps > tasks.len() {
            return false;
        }
    }
    false
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
            assignee: None,
            reviewer: None,
            fresh: true,
            restore: false,
            reviewer_fresh: true,
        }
    }

    fn task(key: &str, status: TaskStatus, allow: &[&str], deps: &[&str]) -> Task {
        Task {
            parent: None,
            issuer: None,
            progress: None,
            handoffs: Vec::new(),
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
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status,
            attempts: 0,
            runs: Vec::new(),
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
        // Acceptance commands are optional: the issuer checks the result (H1).
        let mut no_check = planned("c", &[]);
        no_check.spec.acceptance.clear();
        assert!(validate(&[no_check]).is_ok());
        // The build directories live in `<run>/targets`, so no task may be called that.
        let err = validate(&[planned("targets", &[])]).unwrap_err();
        assert!(err.contains("build directories"), "{err}");
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
    fn empty_scopes_are_listed_for_the_planner() {
        let mut scoped = planned("b", &[]);
        scoped.spec.scope = Scope {
            allow: vec!["src/b/**".into()],
            deny: vec![],
        };
        let tasks = [planned("a", &[]), scoped, planned("c", &[])];
        assert_eq!(empty_scope_keys(&tasks), ["a", "c"]);
        // An empty allow still overlaps everything, which is why the planner is warned.
        assert!(scopes_overlap(&tasks[0].spec.scope, &tasks[1].spec.scope));
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
    fn later_stages_wait_for_earlier_ones() {
        let mut tasks = vec![
            task("a", TaskStatus::Pending, &["a/**"], &[]),
            task("b", TaskStatus::Pending, &["b/**"], &[]),
            task("c", TaskStatus::Pending, &["c/**"], &[]),
        ];
        tasks[0].spec.stage = Some(1);
        tasks[1].spec.stage = Some(1);
        tasks[2].spec.stage = Some(2);
        let keys = |v: Vec<&Task>| v.iter().map(|t| t.key.clone()).collect::<Vec<_>>();
        // c depends on nothing and shares no files, but stage 1 has not ended.
        assert_eq!(keys(ready_tasks(&tasks, 3)), ["a", "b"]);
        tasks[0].status = TaskStatus::Merged;
        assert_eq!(keys(ready_tasks(&tasks, 3)), ["b"]);
        // Ended is enough, whatever it came to.
        tasks[1].status = TaskStatus::Cancelled;
        assert_eq!(keys(ready_tasks(&tasks, 3)), ["c"]);
    }

    #[test]
    fn rejects_a_dependency_on_a_later_stage() {
        let mut first = planned("a", &["b"]);
        first.spec.stage = Some(1);
        let mut second = planned("b", &[]);
        second.spec.stage = Some(2);
        let err = validate(&[first, second]).unwrap_err();
        assert!(err.contains("stage 1") && err.contains("stage 2"), "{err}");

        let mut first = planned("a", &[]);
        first.spec.stage = Some(1);
        let mut second = planned("b", &["a"]);
        second.spec.stage = Some(2);
        assert!(validate(&[first, second]).is_ok());
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

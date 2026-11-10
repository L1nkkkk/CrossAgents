//! What this machine offers an agent that is about to work in it (docs/tasks/SELF.md B4):
//! the version each toolchain answers with, and the trap Windows leaves in `PATH`, where
//! `python` is often the Store's App Execution Alias — it prints nothing and opens the Store
//! instead of running Python.
//!
//! The lead's kickoff and a worker's assignment end with what was probed:
//!
//! ```text
//! machine: python 3.12.10 · node v24.21.0 · cargo 1.99.1
//! ```
//!
//! These are facts the hub learned, not instructions: a session reads them before it spends a
//! turn finding out that `python` is not Python here. Probing runs three short commands, so
//! the answer is kept for a while ([`current`]); a test builds a [`Machine`] by hand and asks
//! what [`Machine::line`] says about it, without starting anything.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossagents_core::process::NoWindow;

/// The programs the line covers, in the order it lists them, and what may answer for each: a
/// box with `python3` and no `python` still has Python.
const TOOLS: [(&str, &[&str]); 3] = [
    ("python", &["python", "python3"]),
    ("node", &["node"]),
    ("cargo", &["cargo"]),
];

/// How long a probe is good. A hub runs for days and a toolchain may be installed while it
/// does; a few minutes of staleness costs nothing, and probing on every kickoff would start
/// three processes each time.
const PROBE_TTL: Duration = Duration::from_secs(300);

/// What the hub found on this machine. Its [`Default`] (nothing probed) says nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Machine {
    /// What each program answered, in [`TOOLS`] order.
    pub tools: Vec<Tool>,
}

/// One program and what came of asking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// `python`, `node` or `cargo`.
    pub name: &'static str,
    pub state: State,
}

/// What asking a program did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// It answered: the version, as it printed it (`3.12.10`, `v24.21.0`).
    Ready(String),
    /// Windows' App Execution Alias stands behind the name: nothing is installed.
    StoreStub,
    /// Nothing on `PATH` answers to it.
    Missing,
    /// It is there but did not say a version.
    Unclear,
}

impl Machine {
    /// Asks each program in [`TOOLS`], in the hub's own environment.
    pub fn probe() -> Machine {
        Machine {
            tools: TOOLS
                .iter()
                .map(|(name, commands)| Tool::probe(name, commands))
                .collect(),
        }
    }

    /// The line a kickoff or an assignment ends with; `None` when nothing is known.
    pub fn line(&self) -> Option<String> {
        if self.tools.is_empty() {
            return None;
        }
        let parts: Vec<String> = self.tools.iter().map(Tool::phrase).collect();
        Some(format!("machine: {}", parts.join(" · ")))
    }
}

/// What this machine offers, as asked not long ago ([`PROBE_TTL`]).
pub fn current() -> Machine {
    static CACHE: OnceLock<Mutex<Option<(Instant, Machine)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((at, machine)) = cache.as_ref()
        && at.elapsed() < PROBE_TTL
    {
        return machine.clone();
    }
    let machine = Machine::probe();
    *cache = Some((Instant::now(), machine.clone()));
    machine
}

impl Tool {
    /// Asks the first of `commands` that is there for its version, the way a shell in this
    /// checkout would find it.
    fn probe(program: &'static str, commands: &[&str]) -> Tool {
        let Some((command, path)) = commands
            .iter()
            .find_map(|command| find(command).map(|path| (*command, path)))
        else {
            return Tool {
                name: program,
                state: State::Missing,
            };
        };
        // The alias is decided by where the name led, not by asking: the stub answers nothing
        // (exit 9009), which is the same shape as a program that failed.
        if store_alias(&path) {
            return Tool {
                name: program,
                state: State::StoreStub,
            };
        }
        Tool {
            name: program,
            state: match version(command, &path) {
                Some(version) => State::Ready(version),
                None => State::Unclear,
            },
        }
    }

    /// How the tool reads in the line.
    fn phrase(&self) -> String {
        match &self.state {
            State::Ready(version) => format!("{} {version}", self.name),
            State::StoreStub => format!(
                "{} is the Windows Store placeholder, and nothing is installed behind it",
                self.name
            ),
            State::Missing => format!("{}: not found", self.name),
            State::Unclear => format!("{}: found, no version", self.name),
        }
    }
}

/// The first file `program` names on `PATH`.
fn find(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in names(program) {
            let file = dir.join(name);
            if file.is_file() {
                return Some(file);
            }
        }
    }
    None
}

/// What a name may be on this platform: Windows finds a program by an extension the file
/// carries, elsewhere by the name itself.
fn names(program: &str) -> Vec<String> {
    if cfg!(windows) {
        ["exe", "cmd", "bat", ""]
            .iter()
            .map(|ext| {
                if ext.is_empty() {
                    program.to_owned()
                } else {
                    format!("{program}.{ext}")
                }
            })
            .collect()
    } else {
        vec![program.to_owned()]
    }
}

/// The version `path` prints for `--version`.
fn version(program: &str, path: &Path) -> Option<String> {
    let output = Command::new(path)
        .arg("--version")
        .no_window()
        .stdin(Stdio::null())
        .output()
        .ok()?;
    // Python prints to stdout, some tools to stderr; whichever said something.
    let text = if output.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    printed(program, &text)
}

/// The version in what `--version` printed: the first line, minus the program's own name and
/// whatever follows the version — `Python 3.12.10` and `cargo 1.99.1 (5f94df478 2026-08-27)`
/// read as `3.12.10` and `1.99.1`.
fn printed(program: &str, text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut words = line.split_whitespace();
    let first = words.next()?;
    let version = if first.eq_ignore_ascii_case(program) {
        words.next().unwrap_or(first)
    } else {
        first
    };
    Some(version.to_owned())
}

/// Whether `path` leads into Windows' App Execution Aliases (`…\Microsoft\WindowsApps`),
/// where every name is a stub for an app the Store would install.
fn store_alias(path: &Path) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    let dir = dir.to_string_lossy().to_ascii_lowercase();
    dir.ends_with("microsoft\\windowsapps") || dir.ends_with("microsoft/windowsapps")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{
        AgentId, Run, RunConfig, RunId, RunPolicy, RunStatus, Task, TaskId, TaskReport, TaskSpec,
        TaskStatus,
    };

    use super::*;

    fn tool(name: &'static str, state: State) -> Tool {
        Tool { name, state }
    }

    fn fake() -> Machine {
        Machine {
            tools: vec![
                tool("python", State::StoreStub),
                tool("node", State::Ready("v24.21.0".into())),
                tool("cargo", State::Ready("1.99.1".into())),
            ],
        }
    }

    fn a_run() -> Run {
        let now = Utc::now();
        Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "title".into(),
            request: "do it".into(),
            planner: AgentId::new(),
            status: RunStatus::Running,
            config: RunConfig {
                worker: AgentId::new(),
                escalate: None,
                reviewer: None,
                auto_approve: true,
                max_parallel: 1,
                max_attempts: 1,
                team: vec![],
                policy: RunPolicy::default(),
            },
            repo: "repo".into(),
            base_branch: "main".into(),
            branch: "run".into(),
            in_place: false,
            mode: None,
            note: None,
            plan: vec![],
            lead_note: None,
            landed: None,
            summary: None,
            checks: vec![],
            checks_state: None,
            checks_snapshot: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn a_task(run: &Run) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            run_id: run.id,
            key: "a".into(),
            title: "task a".into(),
            spec: TaskSpec {
                goal: "do a".into(),
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: Some(run.planner),
            assignee: Some(run.config.worker),
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Running,
            attempts: 1,
            runs: Vec::new(),
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

    #[test]
    fn what_was_probed_reads_as_one_line() {
        assert_eq!(
            fake().line().unwrap(),
            "machine: python is the Windows Store placeholder, and nothing is installed behind \
             it · node v24.21.0 · cargo 1.99.1"
        );
        // Nothing probed (a test's default) adds nothing to a message.
        assert!(Machine::default().line().is_none());
        // A program that is not there, and one that is there but says nothing, are told apart.
        let machine = Machine {
            tools: vec![
                tool("python", State::Missing),
                tool("node", State::Unclear),
                tool("cargo", State::Ready("1.99.1".into())),
            ],
        };
        let line = machine.line().unwrap();
        assert!(line.contains("python: not found"), "{line}");
        assert!(line.contains("node: found, no version"), "{line}");
    }

    #[test]
    fn a_version_is_the_first_line_without_the_programs_name() {
        // What the programs print, as the hub reads it.
        let cases = [
            ("python", "Python 3.12.10\n", "3.12.10"),
            ("node", "v24.21.0\n", "v24.21.0"),
            ("cargo", "cargo 1.99.1 (5f94df478 2026-08-27)\n", "1.99.1"),
            // Nothing on stdout: no version to report.
            ("cargo", "\n  \n", ""),
        ];
        for (program, printed_text, want) in cases {
            assert_eq!(
                printed(program, printed_text).unwrap_or_default(),
                want,
                "{printed_text:?}"
            );
        }
    }

    #[test]
    fn the_store_alias_is_recognized_by_where_the_name_led() {
        assert!(store_alias(Path::new(
            r"C:\Users\ada\AppData\Local\Microsoft\WindowsApps\python.exe"
        )));
        assert!(store_alias(Path::new(
            "/home/ada/Microsoft/WindowsApps/python"
        )));
        // A real install beside it is not the placeholder.
        assert!(!store_alias(Path::new(
            r"C:\Users\ada\AppData\Local\Programs\Python\Python312\python.exe"
        )));
        assert!(!store_alias(Path::new("/usr/bin/python3")));
    }

    #[test]
    fn a_fake_machine_reaches_the_kickoff_and_a_workers_assignment() {
        let machine = fake();
        let line = machine.line().unwrap();
        let run = a_run();
        let task = a_task(&run);

        let kickoff = crate::brief::kickoff_with(
            &run,
            &[],
            run.config.worker,
            &Err("test".into()),
            None,
            "",
            &Default::default(),
            &machine,
        );
        assert!(kickoff.contains(&line), "{kickoff}");

        let opening = crate::brief::assignment_with(&task, &machine);
        assert!(opening.contains(&line), "{opening}");
        // The line closes the opening, after what to do.
        assert!(opening.ends_with(&line), "{opening}");
    }
}

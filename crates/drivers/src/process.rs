//! Child-process helpers shared by the drivers.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

const STDERR_TAIL: usize = 40;

/// The last lines an agent wrote to stderr, for error reports.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    /// Collects `stderr` in the background.
    pub fn spawn(stderr: tokio::process::ChildStderr) -> Self {
        let tail = Self::default();
        let lines = tail.0.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                tracing::debug!(target: "crossagents::agent_stderr", "{line}");
                let mut lines = lines.lock().await;
                if lines.len() == STDERR_TAIL {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        });
        tail
    }

    /// `"<what>"`, or `"<what>:\n<tail>"` when stderr said something.
    pub async fn describe(&self, what: &str) -> String {
        let lines: Vec<String> = self.0.lock().await.iter().cloned().collect();
        if lines.is_empty() {
            what.to_owned()
        } else {
            format!("{what}:\n{}", lines.join("\n"))
        }
    }
}

/// On Windows a bare program name does not find `npx.cmd` and friends; search `PATH`.
pub fn resolve_program(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if !cfg!(windows) || path.extension().is_some() || path.components().count() > 1 {
        return path;
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&dirs) {
        for ext in ["exe", "cmd", "bat"] {
            let candidate = dir.join(program).with_extension(ext);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    path
}

#[cfg(windows)]
pub fn hide_console(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_: &mut Command) {}

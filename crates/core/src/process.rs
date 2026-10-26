//! Child processes: without a console window, and as whole trees.
//!
//! The hub runs detached, without a console of its own. On Windows every console program it
//! starts (git, cmd, reg, a login tool) would then open a new console window that flashes on
//! the user's screen. `no_window` starts them without one; elsewhere it does nothing.
//!
//! [`spawn_tree`] starts a command so that it and everything it starts can be ended together
//! ([`Tree`]): killing a shell alone leaves its children running.

mod job;

pub use job::Job;

use std::io;

/// Starts the command without a console window (Windows' `CREATE_NO_WINDOW`).
pub trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

impl NoWindow for std::process::Command {
    #[cfg(windows)]
    fn no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_window(&mut self) -> &mut Self {
        self
    }
}

impl NoWindow for tokio::process::Command {
    #[cfg(windows)]
    fn no_window(&mut self) -> &mut Self {
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_window(&mut self) -> &mut Self {
        self
    }
}

/// A started command's process tree. Dropping it, or [`Tree::kill`], ends every process in
/// it that is still running, the command's own process included.
///
/// On Windows that is a [`Job`]; elsewhere the command leads its own process group.
pub struct Tree {
    #[cfg(windows)]
    job: Option<Job>,
    #[cfg(unix)]
    group: Option<u32>,
}

impl Tree {
    /// Ends the tree now.
    pub fn kill(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            job::kill_group(group);
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Spawns `cmd` as the root of a [`Tree`].
///
/// Windows: the process starts suspended (and without a console window, replacing any
/// creation flags set before), goes in a new job, and only then runs, so nothing it starts
/// can escape the job. Should the job fail, it runs anyway and only it can be killed.
/// Unix: it starts in a new process group; a child that moves to a group of its own
/// (`setsid`, a daemon) escapes it.
pub fn spawn_tree(cmd: &mut tokio::process::Command) -> io::Result<(tokio::process::Child, Tree)> {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = cmd.spawn()?;
        let Some(pid) = child.id() else {
            return Ok((child, Tree { job: None }));
        };
        let job = Job::new().and_then(|job| job.assign(pid).map(|()| job));
        if let Err(e) = &job {
            tracing::warn!(
                pid,
                "cannot put the process in a job, its children may outlive it: {e}"
            );
        }
        if let Err(e) = job::resume(pid) {
            let _ = child.start_kill();
            return Err(e);
        }
        Ok((child, Tree { job: job.ok() }))
    }
    #[cfg(unix)]
    {
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let group = child.id();
        Ok((child, Tree { group }))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok((cmd.spawn()?, Tree {}))
    }
}

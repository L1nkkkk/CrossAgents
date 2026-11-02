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
#[cfg(unix)]
pub use job::kill_group;

use std::io;

/// Lets programs started from now on stop on Ctrl+C. A process started in a new process group,
/// as a detached hub is, ignores it, and its children inherit that: a person's `ping -t` in a
/// terminal could then not be stopped. Harmless to the hub itself: it has its own handler, and
/// mostly no console.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn children_stop_on_ctrl_c() {
    // SAFETY: a plain Win32 call with no pointers: a null handler means "the default one".
    let ok = unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) };
    if ok == 0 {
        tracing::debug!(error = %io::Error::last_os_error(), "cannot restore Ctrl+C for children");
    }
}

#[cfg(not(windows))]
pub fn children_stop_on_ctrl_c() {}

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
    pid: Option<u32>,
}

impl Tree {
    /// The root process's PID, if it could be read.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// How many processes in the tree are alive right now.
    ///
    /// Windows counts them in the job. Unix cannot count a process group, so this is `1` while
    /// anything is left in it and `0` once it is gone — enough for [`Tree::is_empty`].
    pub fn active_processes(&self) -> io::Result<u32> {
        #[cfg(windows)]
        {
            match &self.job {
                Some(job) => job.active_processes(),
                None => Ok(0),
            }
        }
        #[cfg(unix)]
        {
            Ok(match self.group {
                Some(group) if job::group_alive(group) => 1,
                _ => 0,
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            Ok(0)
        }
    }

    /// Whether no process of the tree is left.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.active_processes()? == 0)
    }

    /// The value written to a holder record: the job (which has no name) on Windows, the
    /// process group number elsewhere.
    pub fn record(&self) -> String {
        #[cfg(windows)]
        {
            "job".to_owned()
        }
        #[cfg(unix)]
        {
            self.group
                .map(|group| group.to_string())
                .unwrap_or_else(|| "group".to_owned())
        }
        #[cfg(not(any(windows, unix)))]
        {
            "tree".to_owned()
        }
    }

    /// Ends the tree now.
    pub fn kill(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            job::kill_group(group);
        }
    }

    /// Ends every process in the tree now and keeps watching it: [`Tree::is_empty`] then
    /// says when they are really gone, which [`Tree::kill`] (it lets go of the job or group)
    /// cannot.
    pub fn end(&mut self) {
        #[cfg(windows)]
        if let Some(job) = &self.job
            && let Err(error) = job.terminate()
        {
            tracing::warn!(%error, "cannot end the job's processes; closing it instead");
            self.kill();
        }
        #[cfg(unix)]
        if let Some(group) = self.group {
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
            return Ok((
                child,
                Tree {
                    job: None,
                    pid: None,
                },
            ));
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
        Ok((
            child,
            Tree {
                job: job.ok(),
                pid: Some(pid),
            },
        ))
    }
    #[cfg(unix)]
    {
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let group = child.id();
        Ok((child, Tree { group, pid: group }))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok((cmd.spawn()?, Tree { pid: None }))
    }
}

/// Spawns `cmd` as the root of a [`Tree`], and **refuses to run it** when there is no tree.
///
/// [`spawn_tree`] warns and runs the command anyway when the platform will not give it a
/// tree; the executor cannot accept that, because "the lock is released once the tree ends"
/// would then be a claim about a process nobody can see (`docs/tasks/LOCKS.md` §4.3). Here a
/// failure to join a job (Windows) or to lead a process group (Unix) ends the process and is
/// returned as an error. Creation flags are replaced just as in [`spawn_tree`].
pub fn spawn_tree_strict(
    cmd: &mut tokio::process::Command,
) -> io::Result<(tokio::process::Child, Tree)> {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = cmd.spawn()?;
        let Some(pid) = child.id() else {
            let _ = child.start_kill();
            return Err(io::Error::other("the started process has no pid"));
        };
        let job = match Job::new().and_then(|job| job.assign(pid).map(|()| job)) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.start_kill();
                return Err(io::Error::new(
                    error.kind(),
                    format!("cannot put the process in a job, refusing to run it: {error}"),
                ));
            }
        };
        if let Err(error) = job::resume(pid) {
            let _ = child.start_kill();
            return Err(error);
        }
        Ok((
            child,
            Tree {
                job: Some(job),
                pid: Some(pid),
            },
        ))
    }
    #[cfg(unix)]
    {
        // `process_group(0)` is a `setpgid` before `exec`: when it cannot be done the spawn
        // itself fails, so a child that comes back at all leads its own group.
        cmd.process_group(0);
        let mut child = cmd.spawn()?;
        let Some(group) = child.id() else {
            let _ = child.start_kill();
            return Err(io::Error::other("the started process has no pid"));
        };
        Ok((
            child,
            Tree {
                group: Some(group),
                pid: Some(group),
            },
        ))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform has no process trees",
        ))
    }
}

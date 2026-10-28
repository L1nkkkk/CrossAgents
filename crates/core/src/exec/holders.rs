//! Holder records: who holds a lock right now (`docs/tasks/LOCKS.md` §4.4).
//!
//! The lock file carries no information, so each `exec` writes its own record next to it,
//! `<hash>.holders/<exec-id>.json`. Several shared holders each write their own file and
//! never overwrite one another. A record can only make a newcomer wait longer — the lock
//! itself decides who runs — so a stale one is a display problem, not a correctness one:
//! records whose process is gone are skipped when read and deleted by [`cleanup`].

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::lock::LockFile;
use super::{Access, ExecError};

/// One `exec` holding one lock.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Holder {
    pub exec_id: String,
    /// The resource's name (`build`), as the caller named it.
    pub resource: String,
    /// The path as the caller gave it; the lock file's name comes from [`super::identity`].
    pub path: PathBuf,
    pub access: Access,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The run the caller is part of, if it said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The root process of the command tree; empty until `exec-run` has started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_pid: Option<u32>,
    /// The root process's creation time, so a reused PID is not mistaken for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_created: Option<u64>,
    /// Job name or process group number of the tree (`exec-run` fills it in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    pub started_at: DateTime<Utc>,
}

impl Holder {
    /// Whether the process this record names is still the one that wrote it.
    ///
    /// A record without a PID was written between taking the lock and starting the command;
    /// it counts as live.
    pub fn alive(&self) -> bool {
        match self.root_pid {
            Some(pid) => alive_since(pid, self.root_created),
            None => true,
        }
    }

    /// How long ago the holder took the lock.
    pub fn held_for(&self) -> Duration {
        Utc::now()
            .signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }

    /// Writes the record: a temporary file and a rename, so a reader never sees half of it.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, path)
    }

    /// Reads one record.
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

/// The records of everybody holding the lock for `path`, oldest first.
///
/// Records whose process is gone are left out: they only ever make a waiter wait longer.
pub fn holders(locks_dir: &Path, path: &Path) -> Vec<Holder> {
    let mut found = read_records(&super::holders_dir(locks_dir, path));
    found.retain(Holder::alive);
    found.sort_by_key(|holder| holder.started_at);
    found
}

/// What [`cleanup`] removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cleanup {
    pub locks: usize,
    pub records: usize,
}

/// Deletes lock files nobody has touched for `older_than` and can be locked exclusively right
/// now, plus holder records whose process is gone.
///
/// A lock file that is in use is never deleted: it is only removed after taking the exclusive
/// lock on it, and the file is unlinked before the lock is dropped.
pub fn cleanup(locks_dir: &Path, older_than: Duration) -> Result<Cleanup, ExecError> {
    let mut done = Cleanup::default();
    let now = SystemTime::now();
    let entries = match std::fs::read_dir(locks_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(done),
        Err(err) => return Err(err.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(hash) = name.strip_suffix(".lock") {
            if !older_than_for(entry.metadata().ok(), older_than, now) {
                continue;
            }
            let Ok(lock) = LockFile::open(&path) else {
                continue;
            };
            if !matches!(lock.try_lock(Access::Exclusive), Ok(true)) {
                continue;
            }
            // Both while the exclusive lock is held: nobody can have taken the lock and
            // written a record in between, so the records this removes are all orphans.
            let _ = std::fs::remove_dir_all(locks_dir.join(format!("{hash}.holders")));
            // Unlink while the lock is held: whoever takes the lock next gets a new file.
            if std::fs::remove_file(&path).is_ok() {
                done.locks += 1;
            }
            lock.unlock();
            drop(lock);
        } else if name.ends_with(".holders") {
            done.records += clean_records(&path, older_than, now);
        }
    }
    Ok(done)
}

/// Removes the records in `dir` that belong to processes which are gone.
fn clean_records(dir: &Path, older_than: Duration, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if !name.to_string_lossy().ends_with(".json") {
            continue;
        }
        let Ok(holder) = Holder::read(&path) else {
            // Unreadable and old: nothing can use it.
            if older_than_for(entry.metadata().ok(), older_than, now) {
                removed += usize::from(std::fs::remove_file(&path).is_ok());
            }
            continue;
        };
        let stale = match holder.root_pid {
            // A record without a PID is one just written; only age makes it stale.
            None => older_than_for(entry.metadata().ok(), older_than, now),
            Some(_) => !holder.alive(),
        };
        if stale {
            removed += usize::from(std::fs::remove_file(&path).is_ok());
        }
    }
    removed
}

/// Whether the file's last change is at least `older_than` ago.
fn older_than_for(
    metadata: Option<std::fs::Metadata>,
    older_than: Duration,
    now: SystemTime,
) -> bool {
    metadata
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= older_than)
}

/// Every record in `dir`, in whatever order the directory lists them; unreadable files are
/// skipped.
fn read_records(dir: &Path) -> Vec<Holder> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|entry| Holder::read(&entry.path()).ok())
        .collect()
}

/// Process liveness: "that PID is still the process that wrote the record".
#[cfg(windows)]
#[allow(unsafe_code)]
mod proc {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };

    /// Whether `pid` is running: a zero-timeout wait for it to exit.
    pub(super) fn alive(pid: u32) -> bool {
        // SAFETY: the process handle is checked and closed; a zero timeout never blocks.
        unsafe {
            let process = OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            );
            if process.is_null() {
                return false;
            }
            let state = WaitForSingleObject(process, 0);
            CloseHandle(process);
            state == WAIT_TIMEOUT
        }
    }

    /// The process's creation time (a `FILETIME`, 100 ns units).
    pub(super) fn created(pid: u32) -> Option<u64> {
        // SAFETY: the process handle is checked and closed; the FILETIMEs are plain out-params.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut creation: FILETIME = std::mem::zeroed();
            let mut exit: FILETIME = std::mem::zeroed();
            let mut kernel: FILETIME = std::mem::zeroed();
            let mut user: FILETIME = std::mem::zeroed();
            let ok = GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user);
            CloseHandle(process);
            if ok == 0 {
                return None;
            }
            Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
        }
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod proc {
    /// Whether `pid` exists. A process we may not signal still exists (`EPERM`).
    pub(super) fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// `starttime` from `/proc/<pid>/stat`, in clock ticks since boot: the closest thing to a
    /// Windows creation time. Only Linux has it; elsewhere a reused PID cannot be told apart.
    #[cfg(target_os = "linux")]
    pub(super) fn created(pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Field 2 (the command) may contain spaces and parentheses, so count from the last
        // `)`: the next field is 3 (state), and `starttime` is field 22.
        let rest = &stat[stat.rfind(')')? + 1..];
        rest.split_whitespace().nth(19)?.parse().ok()
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn created(_pid: u32) -> Option<u64> {
        None
    }
}

#[cfg(not(any(windows, unix)))]
mod proc {
    pub(super) fn alive(_pid: u32) -> bool {
        true
    }

    pub(super) fn created(_pid: u32) -> Option<u64> {
        None
    }
}

/// `pid` is still the process recorded (creation time included) and still running.
pub(super) fn alive_since(pid: u32, created: Option<u64>) -> bool {
    match created {
        Some(created) => proc::created(pid) == Some(created) && proc::alive(pid),
        None => proc::alive(pid),
    }
}

/// The creation time to record for `pid`, if the platform can tell.
pub(super) fn created(pid: u32) -> Option<u64> {
    proc::created(pid)
}

//! Local resource locks: one path, many processes, shared or exclusive.
//!
//! (`docs/tasks/LOCKS.md` §3, §4.1, §4.2, §4.4)
//!
//! A resource is a path — a build directory, a port's runtime directory — and two requests for
//! the same resource are requests for the same lock, whatever they call it. The lock itself
//! is an OS file lock in one fixed directory, never inside the resource:
//!
//! ```text
//! <locks_dir>/<hash>.lock                 the lock; an empty file, nothing is written in it
//! <locks_dir>/<hash>.holders/<exec>.json  one record per holder
//! ```
//!
//! `<hash>` is the first 32 hex digits of the SHA-256 of the normalized path
//! ([`identity`]). Because the lock file does not live in the resource, `cargo clean` or a
//! script deleting the build directory cannot make the next process lock a different file
//! and run alongside a holder that is still busy.
//!
//! # Taking every lock at once
//!
//! [`acquire`] sorts the requested locks by file name, tries each one without blocking, and
//! as soon as one is busy drops all the ones it took, backs off and starts over. It never
//! holds part of a request while waiting for the rest, so two requests that want the same
//! resources in different orders cannot deadlock — the worst case is both waiting.
//!
//! While waiting it calls the `on_wait` callback with one line per report period
//! ([`Request::notice_every`]), naming who holds the lock, from the holder records:
//!
//! ```text
//! waiting for build (C:\…\cargo): held by Kai · cargo test --workspace · since 02:43 (4m 12s)
//! ```
//!
//! When [`Request::timeout`] runs out the call fails with [`ExecError::Timeout`], which says
//! who was holding it and for how long. A CLI should exit with [`EXIT_TEMPFAIL`] (`75`, BSD
//! `EX_TEMPFAIL`) so that "could not get the lock" is not confused with "the command failed".
//!
//! # Releasing
//!
//! [`Held`] owns the locks: dropping it deletes its holder records and releases the locks.
//! `exec-run` waits for the whole process tree to end before dropping it. Between `acquire`
//! and starting the command there is no PID to record; [`Held::set_process`] fills it in.
//!
//! # Holder records
//!
//! They only ever make a waiter wait longer, never let it through: the OS lock decides. A
//! record whose process is gone is ignored by [`holders`] and deleted by [`cleanup`].
//!
//! # Nothing here needs the hub
//!
//! The locks are ordinary file locks on the local machine; a CLI run with the hub stopped
//! is excluded from a hub check exactly the same way.
//!
//! # What to call
//!
//! * [`Access`], [`Resource`], [`Request`] — what to lock, in which mode, and how long to wait
//!   (`Request::new` has the defaults: 10 minutes, a report every 10 seconds).
//! * [`acquire`] — take them all. The [`Held`] it returns releases them on drop;
//!   [`Held::set_process`] records the command's root process once `exec-run` started it, and
//!   [`Held::locks`] hands `exec-run` the file names, the access and the raw handle or
//!   descriptor to pass on to the process tree.
//! * [`holders`] — who holds a resource now, for a notice or the run page.
//! * [`cleanup`] — delete lock files nobody touched for a while and records of gone processes.
//! * [`identity`], [`lock_name`], [`lock_path`], [`holders_dir`] — the names, for display and
//!   for tests.
//! * [`ExecError`] and [`EXIT_TEMPFAIL`] — how to report that the lock did not come.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod holders;
mod lock;

pub use holders::{Cleanup, Holder, cleanup, holders};
pub use lock::RawLock;

/// Exit code for "could not get the lock in time" (BSD `EX_TEMPFAIL`).
pub const EXIT_TEMPFAIL: i32 = 75;

/// How long `acquire` waits by default: ten minutes, the same as `exec --timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// How often `acquire` reports the holder by default.
pub const DEFAULT_NOTICE_EVERY: Duration = Duration::from_secs(10);

/// The first backoff after a failed attempt; it doubles up to [`BACKOFF_MAX`].
const BACKOFF_START: Duration = Duration::from_millis(100);

/// The longest pause between attempts: a lock that frees up is noticed within this.
const BACKOFF_MAX: Duration = Duration::from_secs(2);

/// The shortest pause, so a very small `notice_every` cannot turn into a spin.
const MIN_POLL: Duration = Duration::from_millis(5);

/// How a request wants to use a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Others may hold it at the same time; nothing may hold it exclusively.
    Shared,
    /// Nobody else, in any way.
    Exclusive,
}

impl Access {
    /// The name used in holder records, `CROSSAGENTS_HELD` and `--resource build:shared`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Exclusive => "exclusive",
        }
    }
}

/// One resource a command needs.
#[derive(Clone, Debug)]
pub struct Resource {
    /// The name the caller knows it by (`build`); used in notices and records.
    pub name: String,
    /// The actual path; its [`identity`] is the resource.
    pub path: PathBuf,
    pub access: Access,
}

impl Resource {
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>, access: Access) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            access,
        }
    }
}

/// What one `exec` asks for.
#[derive(Clone, Debug)]
pub struct Request {
    pub resources: Vec<Resource>,
    /// The command line, for the holder records and notices.
    pub command: String,
    /// The agent that asked, if it can be told (`CROSSAGENTS_AGENT`).
    pub agent: Option<String>,
    /// The run this is part of, if any (`CROSSAGENTS_RUN`).
    pub run: Option<String>,
    pub timeout: Duration,
    /// How often to report the holder while waiting.
    pub notice_every: Duration,
}

impl Request {
    /// A request with the default timeout and report period.
    pub fn new(command: impl Into<String>, resources: Vec<Resource>) -> Self {
        Self {
            resources,
            command: command.into(),
            agent: None,
            run: None,
            timeout: DEFAULT_TIMEOUT,
            notice_every: DEFAULT_NOTICE_EVERY,
        }
    }
}

/// Why a request could not be served.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The timeout ran out while somebody else held the resource.
    #[error("timed out after {waited_secs:.1}s waiting for {resource} ({path}): {holder}")]
    Timeout {
        resource: String,
        /// The path as the caller gave it.
        path: String,
        waited_secs: f64,
        /// Who held it, and since when — from the holder records.
        holder: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// The normalized identity of a resource path.
///
/// Absolute, symlinks resolved, `.` and `..` gone. A path that does not exist yet is
/// resolved as far as it does exist and the rest is appended, so deleting a build directory
/// does not rename its lock. On Windows the result is lowercase with `\` separators, because
/// `C:\Build` and `c:\build` are the same directory.
pub fn identity(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(dir) => dir.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    normalize(lexical(&resolve_existing_prefix(&absolute)))
}

/// The 32 hex digits that name the lock file for `path`
/// (`sha256(identity(path))`, first 16 bytes).
pub fn lock_name(path: &Path) -> String {
    let digest = Sha256::digest(identity(path).to_string_lossy().as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The lock file for `path`.
pub fn lock_path(locks_dir: &Path, path: &Path) -> PathBuf {
    locks_dir.join(format!("{}.lock", lock_name(path)))
}

/// The directory holding the records of everybody holding `path`'s lock.
pub fn holders_dir(locks_dir: &Path, path: &Path) -> PathBuf {
    locks_dir.join(format!("{}.holders", lock_name(path)))
}

/// Takes every lock in `req`, or none.
///
/// Returns once all of them are held; until then it reports the holder through `on_wait` and
/// retries with a growing backoff (100 ms up to 2 s, with jitter). It fails with
/// [`ExecError::Timeout`] when `req.timeout` is up.
///
/// The file calls are short system calls (an open and a non-blocking lock attempt); only the
/// pause between attempts is awaited.
pub async fn acquire(
    locks_dir: &Path,
    req: &Request,
    mut on_wait: impl FnMut(&str),
) -> Result<Held, ExecError> {
    let wants = wants(&req.resources);
    std::fs::create_dir_all(locks_dir)?;
    let started = Instant::now();
    let notice_every = req.notice_every.max(MIN_POLL);
    let mut backoff = BACKOFF_START;
    // The first failed attempt reports at once: a person should not wait ten seconds to learn
    // who they are waiting for.
    let mut next_notice = started;
    loop {
        let blocked = match try_all(locks_dir, &wants)? {
            Ok(taken) => return Held::take(locks_dir, req, taken),
            Err(blocked) => blocked,
        };
        let waited = started.elapsed();
        if waited >= req.timeout {
            return Err(ExecError::Timeout {
                resource: blocked.want.name.clone(),
                path: blocked.want.path.display().to_string(),
                waited_secs: waited.as_secs_f64(),
                holder: held_by(locks_dir, &blocked.want),
            });
        }
        let now = Instant::now();
        if now >= next_notice {
            on_wait(&describe(locks_dir, &blocked.want));
            next_notice = now + notice_every;
        }
        let pause = backoff
            .min(
                next_notice
                    .saturating_duration_since(Instant::now())
                    .max(MIN_POLL),
            )
            .min(req.timeout.saturating_sub(started.elapsed()))
            .max(MIN_POLL);
        tokio::time::sleep(pause).await;
        backoff = jitter((backoff * 2).min(BACKOFF_MAX));
    }
}

/// Everything `acquire` took: releasing them is dropping it.
pub struct Held {
    exec_id: String,
    locks: Vec<HeldLock>,
}

struct HeldLock {
    want: Want,
    file: lock::LockFile,
    record: PathBuf,
    holder: Holder,
}

/// A held lock as `exec-run` needs it: the lock file's name, the access it was taken with and
/// the raw handle or descriptor to pass on (`docs/tasks/LOCKS.md` §4.5).
#[derive(Clone, Debug)]
pub struct LockRef {
    /// The lock file's name, `<hash>.lock`.
    pub name: String,
    pub access: Access,
    pub raw: RawLock,
}

impl Held {
    /// This run's id, the name of its holder records.
    pub fn exec_id(&self) -> &str {
        &self.exec_id
    }

    /// The held locks, in the order they were taken.
    pub fn locks(&self) -> Vec<LockRef> {
        self.locks
            .iter()
            .map(|held| LockRef {
                name: format!("{}.lock", held.want.hash),
                access: held.want.access,
                raw: held.file.raw(),
            })
            .collect()
    }

    /// Records the command's root process and its tree, once `exec-run` has started them.
    ///
    /// Until this is called the record names no process, so nobody can check that the holder
    /// is still alive; waiters go on reporting the lock as held, which is the safe direction.
    pub fn set_process(&self, pid: u32, tree: &str) -> std::io::Result<()> {
        let created = holders::created(pid);
        for held in &self.locks {
            let mut holder = held.holder.clone();
            holder.root_pid = Some(pid);
            holder.root_created = created;
            holder.tree = Some(tree.to_owned());
            holder.write(&held.record)?;
        }
        Ok(())
    }

    /// Takes the locks and writes a record for each; on any failure the records already
    /// written are removed and the locks released by dropping the half-built value.
    fn take(
        locks_dir: &Path,
        req: &Request,
        taken: Vec<(Want, lock::LockFile)>,
    ) -> Result<Self, ExecError> {
        let mut held = Self {
            exec_id: uuid::Uuid::now_v7().simple().to_string(),
            locks: Vec::with_capacity(taken.len()),
        };
        for (want, file) in taken {
            let dir = holders_dir(locks_dir, &want.path);
            std::fs::create_dir_all(&dir)?;
            let record = dir.join(format!("{}.json", held.exec_id));
            let holder = Holder {
                exec_id: held.exec_id.clone(),
                resource: want.name.clone(),
                path: want.path.clone(),
                access: want.access,
                command: req.command.clone(),
                agent: req.agent.clone(),
                run: req.run.clone(),
                root_pid: None,
                root_created: None,
                tree: None,
                started_at: chrono::Utc::now(),
            };
            file.touch();
            holder.write(&record)?;
            held.locks.push(HeldLock {
                want,
                file,
                record,
                holder,
            });
        }
        Ok(held)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        for held in &self.locks {
            // Record first, lock second: the other order would let a newcomer take the lock
            // while a record still claims it is held, which only ever makes people wait.
            let _ = std::fs::remove_file(&held.record);
            if let Some(dir) = held.record.parent() {
                // Fails while other holders are still in it, which is what we want.
                let _ = std::fs::remove_dir(dir);
            }
            held.file.unlock();
        }
    }
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Held")
            .field("exec_id", &self.exec_id)
            .field(
                "locks",
                &self
                    .locks
                    .iter()
                    .map(|held| (&held.want.name, held.want.access))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// One lock to take: a resource and the lock file it maps to.
#[derive(Clone)]
struct Want {
    name: String,
    path: PathBuf,
    access: Access,
    hash: String,
}

/// The lock of the request that could not be taken.
struct Blocked {
    want: Want,
}

/// The resources of a request, one entry per lock file, in lock-file order.
///
/// A resource listed twice (`build:shared` and `build:exclusive` by mistake) is one lock taken
/// with the stronger access.
fn wants(resources: &[Resource]) -> Vec<Want> {
    let mut wants: Vec<Want> = resources
        .iter()
        .map(|resource| Want {
            name: resource.name.clone(),
            path: resource.path.clone(),
            access: resource.access,
            hash: lock_name(&resource.path),
        })
        .collect();
    wants.sort_by(|left, right| left.hash.cmp(&right.hash));
    let mut unique: Vec<Want> = Vec::with_capacity(wants.len());
    for want in wants {
        match unique.last_mut() {
            Some(last) if last.hash == want.hash => {
                if want.access > last.access {
                    last.access = want.access;
                    last.name = want.name;
                }
            }
            _ => unique.push(want),
        }
    }
    unique
}

/// Tries every lock once, in order. On the first conflict the locks already taken are
/// released by dropping them, and the conflicted one is returned.
fn try_all(
    locks_dir: &Path,
    wants: &[Want],
) -> Result<Result<Vec<(Want, lock::LockFile)>, Blocked>, ExecError> {
    let mut taken = Vec::with_capacity(wants.len());
    for want in wants {
        let file = lock::LockFile::open(&locks_dir.join(format!("{}.lock", want.hash)))?;
        if !file.try_lock(want.access)? {
            return Ok(Err(Blocked { want: want.clone() }));
        }
        taken.push((want.clone(), file));
    }
    Ok(Ok(taken))
}

/// The line reported while waiting: one per report period, naming the holder.
fn describe(locks_dir: &Path, want: &Want) -> String {
    format!(
        "waiting for {} ({}): {}",
        want.name,
        want.path.display(),
        held_by(locks_dir, want)
    )
}

/// Who is holding the lock, as far as the records know.
fn held_by(locks_dir: &Path, want: &Want) -> String {
    let held = holders(locks_dir, &want.path);
    if held.is_empty() {
        return "held by an unknown process".to_owned();
    }
    held.iter()
        .map(|holder| {
            let agent = holder.agent.as_deref().unwrap_or("an unknown agent");
            format!(
                "held by {agent} · {} · since {}",
                holder.command,
                since(holder)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `02:43 (4m 12s)`: when the holder started and how long ago that was.
fn since(holder: &Holder) -> String {
    let at = holder.started_at.with_timezone(&chrono::Local);
    format!("{} ({})", at.format("%H:%M"), human(holder.held_for()))
}

/// A duration as a person reads it: `12s`, `4m 12s`, `1h 03m`.
fn human(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

/// Spreads retries out by ±10%, so two waiters that started together do not take turns
/// hammering the lock in step.
fn jitter(duration: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    let percent = 90 + (nanos % 21);
    duration.mul_f64(f64::from(percent) / 100.0)
}

/// Drops `.` and `..` components without touching the file system. Only used on the part of a
/// path that does not exist — whatever does exist went through `canonicalize`, which already
/// resolved it.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !cleaned.pop() && !path.is_absolute() {
                    cleaned.push(component.as_os_str());
                }
            }
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

/// Resolves symlinks in the longest prefix of `absolute` that exists.
fn resolve_existing_prefix(absolute: &Path) -> PathBuf {
    let mut anchor = absolute;
    let mut tail: Vec<OsString> = Vec::new();
    loop {
        if let Ok(resolved) = std::fs::canonicalize(anchor) {
            let mut path = resolved;
            for part in tail.iter().rev() {
                path.push(part);
            }
            return path;
        }
        match (anchor.parent(), anchor.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                anchor = parent;
            }
            _ => return absolute.to_path_buf(),
        }
    }
}

/// Windows: one case, one separator. Elsewhere the path is already the identity.
#[cfg(windows)]
fn normalize(path: PathBuf) -> PathBuf {
    let mut text: String = path.to_string_lossy().replace('/', "\\").to_lowercase();
    while text.len() > 3 && text.ends_with('\\') {
        text.pop();
    }
    PathBuf::from(text)
}

#[cfg(not(windows))]
fn normalize(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_of_a_missing_path_keeps_its_parent() {
        let dir = std::env::temp_dir().join("crossagents-identity-missing");
        std::fs::create_dir_all(&dir).expect("create the directory");
        let deep = dir.join("build").join("cargo");
        let identity = identity(&deep);
        assert!(identity.is_absolute());
        assert!(
            identity.ends_with(Path::new("build").join("cargo")),
            "{}",
            identity.display()
        );
    }

    #[test]
    fn identity_ignores_dots_and_separators() {
        let dir = std::env::temp_dir().join("crossagents-identity-dots");
        std::fs::create_dir_all(&dir).expect("create the directory");
        let plain = identity(&dir);
        let dotted = identity(&dir.join(".").join("build").join(".."));
        assert_eq!(plain, dotted);
    }

    #[test]
    fn lock_names_are_32_hex_digits() {
        let name = lock_name(Path::new("/tmp/whatever"));
        assert_eq!(name.len(), 32);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn durations_read_like_a_person_wrote_them() {
        assert_eq!(human(Duration::from_secs(12)), "12s");
        assert_eq!(human(Duration::from_secs(252)), "4m 12s");
        assert_eq!(human(Duration::from_secs(3780)), "1h 03m");
    }
}

//! Running a command under the locks: start the tree, record its root process, wait for the
//! whole tree to end, and pass the locks on to nested `exec`s
//! (`docs/tasks/LOCKS.md` §4.3, §4.4, §4.5).
//!
//! The order is the one in §4:
//!
//! ```text
//! take every lock → write the holder records → start the tree → wait for the root and then
//! for the whole tree → delete the records → release the locks
//! ```
//!
//! [`acquire`](super::acquire) writes the records with no process in them; [`spawn`] fills in
//! the root process once it exists, because the PID cannot be known before `spawn`. Between
//! the two, the record names no process and only makes waiters more careful.
//!
//! # Nesting
//!
//! The child environment carries `CROSSAGENTS_HELD` (`<exec-id>;<lock file>:<access>:<handle
//! or fd>,…`) and the lock handles are made inheritable. An inner `exec` that sees its whole
//! request covered does not take the locks again — it already runs inside the tree that holds
//! them. A request the outer exec does not cover, or a shared → exclusive upgrade, fails at
//! once instead of waiting for a lock its own ancestor holds.
//!
//! An inherited name is not proof. Before skipping the locks, [`inherited_holds`] checks that
//! the handle still points at the lock file (`docs/tasks/LOCKS.md` §4.5) and that the outer
//! exec's record names a live root process. Anything that does not check out is treated as
//! "not held" — the request takes the locks like any other.

use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use super::{
    Access, ExecError, Held, Holder, RawLock, Request, Resource, acquire, file_id_raw, lock_name,
    path_id,
};
use crate::process::{Tree, spawn_tree_strict};

/// The environment variable that carries the outer `exec`'s holds into its children
/// (`docs/tasks/LOCKS.md` §4.5).
pub const HELD_ENV: &str = "CROSSAGENTS_HELD";

/// How often [`wait_tree_empty`] looks again.
const TREE_POLL: Duration = Duration::from_millis(25);

/// One lock an outer `exec` says it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The lock file's name, `<hash>.lock`.
    pub name: String,
    pub access: Access,
    /// The inherited handle or descriptor.
    pub raw: RawLock,
}

/// The value of [`HELD_ENV`], parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inherited {
    pub exec_id: String,
    pub claims: Vec<Claim>,
}

impl Inherited {
    /// Parses `<exec-id>;<name>:<access>:<raw>,…`; `None` for anything malformed.
    ///
    /// A malformed value means the request simply takes the locks: a claim that cannot be
    /// read is not evidence of anything.
    pub fn parse(value: &str) -> Option<Self> {
        let (exec_id, rest) = value.split_once(';')?;
        if exec_id.is_empty() {
            return None;
        }
        let claims = rest
            .split(',')
            .map(Claim::parse)
            .collect::<Option<Vec<_>>>()?;
        if claims.is_empty() {
            return None;
        }
        Some(Self {
            exec_id: exec_id.to_owned(),
            claims,
        })
    }

    /// The claim for the lock file `name`, if the outer exec made one.
    pub fn claim(&self, name: &str) -> Option<&Claim> {
        self.claims.iter().find(|claim| claim.name == name)
    }
}

impl Claim {
    fn parse(part: &str) -> Option<Self> {
        let mut fields = part.splitn(3, ':');
        let name = fields.next()?;
        let access = match fields.next()? {
            "shared" => Access::Shared,
            "exclusive" => Access::Exclusive,
            _ => return None,
        };
        let raw: i64 = fields.next()?.parse().ok()?;
        if name.is_empty() {
            return None;
        }
        Some(Self {
            name: name.to_owned(),
            access,
            raw: raw as RawLock,
        })
    }
}

/// Whether `value` (a `CROSSAGENTS_HELD` value) still covers every resource in `resources`.
///
/// The value is proved real first: every claim must be a handle to the lock file at its path,
/// and the outer exec's record must name a root process that is still running. A leftover
/// value in a long-lived shell — the outer exec exited long ago — or a forged one is not
/// evidence of anything, so it gives `Ok(None)` and the request takes the locks normally.
///
/// Only a **real** outer hold is then compared with the request (§4.5's table):
///
/// * `Ok(Some(_))` — it covers every resource: the caller may run without taking the locks.
/// * `Ok(None)` — no usable inherited hold (absent, malformed, a closed handle, a handle to
///   another file, or a record of a process that is gone): take the locks normally.
/// * `Err(`[`ExecError::Nesting`]`)` — the outer exec really holds the resource, but at a
///   lower access than asked, or does not hold it at all. This is a mistake in the request,
///   and waiting could never fix it.
pub fn inherited_holds(
    locks_dir: &Path,
    value: &str,
    resources: &[Resource],
) -> Result<Option<Inherited>, ExecError> {
    let Some(inherited) = Inherited::parse(value) else {
        return Ok(None);
    };

    // A name in the environment is not a lock. Until every claim checks out, treat the value
    // as absent: the table below must not turn a stale or forged value into an error.
    let real = inherited
        .claims
        .iter()
        .all(|claim| claim_still_holds(locks_dir, &inherited.exec_id, claim));
    if !real {
        return Ok(None);
    }

    // It is real: what the request wants versus what the outer exec declared (§4.5).
    for resource in resources {
        let name = format!("{}.lock", lock_name(&resource.path));
        let Some(claim) = inherited.claim(&name) else {
            return Err(ExecError::Nesting(format!(
                "resource {} ({}) is not held by the outer exec; \
                 declare it in the outer exec: --resource {}",
                resource.name,
                resource.path.display(),
                resource.name
            )));
        };
        if claim.access < resource.access {
            return Err(ExecError::Nesting(format!(
                "resource {} ({}) is held {} by the outer exec and cannot be upgraded to {}; \
                 declare it {} in the outer exec",
                resource.name,
                resource.path.display(),
                claim.access.as_str(),
                resource.access.as_str(),
                resource.access.as_str()
            )));
        }
    }

    Ok(Some(inherited))
}

/// The outer exec's claim is real: the handle is the lock file at its path, and the outer
/// exec's record names a root process that is still running.
fn claim_still_holds(locks_dir: &Path, exec_id: &str, claim: &Claim) -> bool {
    let path = locks_dir.join(&claim.name);
    let (Ok(handle), Ok(at_path)) = (file_id_raw(claim.raw), path_id(&path)) else {
        return false;
    };
    if handle != at_path {
        return false;
    }
    let Some(hash) = claim.name.strip_suffix(".lock") else {
        return false;
    };
    let records = locks_dir.join(format!("{hash}.holders"));
    let Ok(entries) = std::fs::read_dir(records) else {
        return false;
    };
    entries
        .flatten()
        .filter_map(|entry| Holder::read(&entry.path()).ok())
        .any(|holder| holder.exec_id == exec_id && holder.alive())
}

/// Starts `cmd` as the tree that holds `held`'s locks.
///
/// Before the spawn: `CROSSAGENTS_HELD` goes into the child environment and the lock handles
/// become inheritable. After it: the holder records get the root PID, its creation time and
/// the tree (job or process group). The command runs in a strict tree ([`spawn_tree_strict`]):
/// when the platform will not give it one the process is killed and nothing is run, because
/// the locks could not then be held until the tree ends (§4.3).
///
/// The caller keeps the [`tokio::process::Child`] and decides what to do with its stdio; it
/// must wait for the child itself and then call [`wait_tree_empty`] before releasing `held`.
pub fn spawn(
    held: &mut Held,
    cmd: &mut tokio::process::Command,
) -> io::Result<(tokio::process::Child, Tree)> {
    cmd.env(HELD_ENV, held_value(held));
    held.inherit_locks()?;
    let (mut child, tree) = spawn_tree_strict(cmd)?;
    let Some(pid) = child.id() else {
        let _ = child.start_kill();
        return Err(io::Error::other("the started process has no pid"));
    };
    if let Err(error) = held.set_process(pid, &tree.record()) {
        let _ = child.start_kill();
        return Err(error);
    }
    Ok((child, tree))
}

/// The `CROSSAGENTS_HELD` value for what `held` has taken.
fn held_value(held: &Held) -> String {
    let mut value = held.exec_id().to_owned();
    value.push(';');
    value.push_str(
        &held
            .locks()
            .iter()
            .map(|lock| format!("{}:{}:{}", lock.name, lock.access.as_str(), lock.raw))
            .collect::<Vec<_>>()
            .join(","),
    );
    value
}

/// Waits until no process of the tree is left (`docs/tasks/LOCKS.md` §4.3).
///
/// The caller waits for the root process first; this covers what it started and left behind.
/// On Windows it is the job's active process count, on Unix the process group. A daemon that
/// left the group, or a child that broke out of the job, is outside the tree on Unix, as §5
/// records.
pub async fn wait_tree_empty(tree: &Tree) -> io::Result<()> {
    loop {
        if tree.is_empty()? {
            return Ok(());
        }
        tokio::time::sleep(TREE_POLL).await;
    }
}

/// Takes the locks, runs `cmd` under them and returns the command's exit status.
///
/// Waiting notices go to stderr; the CLI is the caller that wants them. A request the outer
/// exec already covers runs without taking the locks again (§4.5); everything else goes
/// through [`acquire`], [`spawn`] and [`wait_tree_empty`].
pub async fn run(
    locks_dir: &Path,
    req: &Request,
    cmd: &mut tokio::process::Command,
) -> Result<ExitStatus, ExecError> {
    if let Ok(value) = std::env::var(HELD_ENV)
        && inherited_holds(locks_dir, &value, &req.resources)?.is_some()
    {
        // The outer exec's tree already holds these locks and covers this process; the
        // child inherits the same `CROSSAGENTS_HELD` and stdio.
        let mut child = cmd.spawn()?;
        return Ok(child.wait().await?);
    }

    let mut held = acquire(locks_dir, req, |line| eprintln!("{line}")).await?;
    let (mut child, tree) = spawn(&mut held, cmd)?;
    let status = child.wait().await?;
    wait_tree_empty(&tree).await?;
    Ok(status)
}

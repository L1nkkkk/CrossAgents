//! Runtime hosts: agent processes that outlive the hub (docs/PROCESS-SUPERVISION.md).
//!
//! A runtime host is `crossagents runtime-host`, a small process the hub starts, detached,
//! for every agent process that can be taken over (a Codex app-server, a Claude Code
//! session). The host puts itself in a fresh Windows Job Object that kills on close and
//! starts the agent inside it, so the agent's whole tree (shells, MCP servers, builds)
//! lives exactly as long as the host. It relays the agent's stdio over a loopback socket
//! and keeps what the hub has not acknowledged.
//!
//! So the hub can stop, crash or be upgraded while agents keep working: the next hub finds
//! the hosts by their records (`<dir>/<id>.json`), reconnects, reads what it missed and
//! carries on ([`claim`], [`crate::launch::adopt`]). Nothing is ever killed by pid: a host
//! is told to stop over its own connection, which proves who it is.

mod client;
mod serve;
pub mod wire;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

pub use serve::serve;
pub use wire::{HostRecord, HostSpec};

pub(crate) use client::{connect, spawn};

/// How the hub starts hosts. Without settings every agent process is a direct child.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Runs a host; `--dir <dir> --id <id>` is added. E.g. `[crossagents.exe, runtime-host]`.
    pub command: Vec<String>,
    /// Where hosts keep their records.
    pub dir: PathBuf,
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);
/// Bumped by [`leave`]: connections from before no longer act for the hub.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Hosts found at start that no session has claimed yet, by owner.
static ADOPTABLE: Mutex<Vec<HostRecord>> = Mutex::new(Vec::new());

/// Starts takeover-able agent processes in hosts from now on.
pub fn enable(settings: Settings) {
    *SETTINGS.write().expect("settings lock") = Some(settings);
}

/// Starts every agent process directly again.
pub fn disable() {
    *SETTINGS.write().expect("settings lock") = None;
}

pub fn settings() -> Option<Settings> {
    SETTINGS.read().expect("settings lock").clone()
}

/// The hub is leaving its agents running: from now on, closing a hosted session neither
/// stops its process nor closes its input, and lost connections are not failures.
/// Connections stop reading, so whatever the agents say next waits for the next hub.
pub fn leave() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    LEFT.notify_waiters();
}

static LEFT: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Resolves once the hub has left since `generation`.
pub(crate) async fn until_left(generation: u64) {
    loop {
        let notified = LEFT.notified();
        if left(generation) {
            return;
        }
        notified.await;
    }
}

pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// Whether the hub left since `generation`.
pub(crate) fn left(generation: u64) -> bool {
    self::generation() != generation
}

fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// The records of every host in `dir` (running or waiting to report an exit).
pub fn records(dir: &Path) -> Vec<HostRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<HostRecord> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.ends_with(".json") && !name.ends_with(".spec.json")
        })
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    found.sort_by_key(|r| r.started_at);
    found
}

/// What [`discover`] found.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Hosts that answered, now waiting for [`claim`].
    pub live: Vec<HostRecord>,
    /// Records of hosts that are gone; removed.
    pub stale: Vec<HostRecord>,
}

/// Looks for hosts left by a previous hub and keeps the ones that answer for [`claim`].
pub async fn discover() -> Discovery {
    let Some(settings) = settings() else {
        return Discovery::default();
    };
    let mut discovery = Discovery::default();
    for record in records(&settings.dir) {
        if client::probe(&record).await {
            discovery.live.push(record);
        } else {
            let _ = std::fs::remove_file(record_path(&settings.dir, &record.id));
            discovery.stale.push(record);
        }
    }
    *ADOPTABLE.lock().expect("adoptable lock") = discovery.live.clone();
    discovery
}

/// Takes the newest host left for `owner`, if any.
pub fn claim(owner: &str) -> Option<HostRecord> {
    let mut adoptable = ADOPTABLE.lock().expect("adoptable lock");
    let index = adoptable.iter().rposition(|r| r.owner == owner)?;
    Some(adoptable.remove(index))
}

/// Stops every host no session claimed (their sessions are gone, or a newer host has the
/// same owner). Returns how many.
pub async fn release_unclaimed() -> Vec<HostRecord> {
    let left: Vec<HostRecord> = std::mem::take(&mut *ADOPTABLE.lock().expect("adoptable lock"));
    for record in &left {
        if let Ok(launched) = connect(record).await {
            launched.process.start_kill();
            let _ = launched.process.exit_within(Duration::from_secs(5)).await;
        }
    }
    left
}

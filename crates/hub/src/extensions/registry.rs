use super::{
    manifest::Manifest,
    snapshot::{self, Metadata},
};
use crossagents_api_types::*;
use crossagents_core::{AgentId, CoreError, EventBus, EventKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

type Result<T> = std::result::Result<T, CoreError>;
fn storage(e: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(e.to_string())
}
fn conflict(e: impl std::fmt::Display) -> CoreError {
    CoreError::Conflict(e.to_string())
}
fn missing(name: &str) -> CoreError {
    CoreError::NotFound {
        kind: "extension".into(),
        id: name.into(),
    }
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    auto_trust: bool,
    current: Option<String>,
    previous: Option<String>,
    pending: Option<Pending>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Pending {
    fingerprint: String,
    requested_at: chrono::DateTime<chrono::Utc>,
    requested_by: Option<AgentId>,
}
#[derive(Default)]
struct State {
    records: BTreeMap<String, Record>,
    infos: BTreeMap<String, ExtensionInfo>,
    metadata: BTreeMap<String, Metadata>,
    occupied: BTreeMap<(String, String), usize>,
    /// Resident processes of this hub's run; live state, never persisted.
    services: BTreeMap<String, ExtensionServiceStatus>,
    /// Extensions whose enabled state or current version changed while the lock was held;
    /// `on_version_changed` hooks run after it is released.
    switched: Vec<String>,
    /// Bumped whenever an extension is enabled, disabled or switched to another snapshot.
    /// A lease carries the value it was taken at, so a command cannot start on a lease that
    /// predates a disable.
    generations: BTreeMap<String, u64>,
}
/// Serializes scan, approval and acquisition so cleanup cannot race a new call.
pub struct Registry {
    home: PathBuf,
    state: Mutex<State>,
    bus: Option<Arc<EventBus>>,
    /// Told after an extension is disabled, with the lock released (docs/EXTENSIONS.md §3:
    /// calls already running are ended a few seconds later).
    disabled: Mutex<Vec<DisabledHook>>,
    /// Told after the extension is enabled, disabled, or switched to another snapshot, with
    /// the lock released. The resident-process supervisor uses it to stop the old process
    /// and start the new one.
    version_changed: Mutex<Vec<VersionHook>>,
}
type DisabledHook = Box<dyn Fn(&str) + Send + Sync>;
type VersionHook = Box<dyn Fn(&str) + Send + Sync>;
#[derive(Debug, Clone)]
pub struct EnabledVersion {
    pub path: PathBuf,
    pub data: PathBuf,
    pub version: ExtensionVersion,
    pub manifest: Manifest,
}
/// Pins a snapshot until dropped. Explicit release is also available for runners.
pub struct SnapshotLease {
    pub enabled: EnabledVersion,
    /// The extension's generation when this lease was taken; see [`Registry::lease_is_current`].
    pub generation: u64,
    registry: Weak<Registry>,
    name: String,
    fingerprint: String,
}
impl SnapshotLease {
    /// Releases this pin immediately; dropping it has the same effect.
    pub fn release(self) {
        drop(self);
    }
}
impl Drop for SnapshotLease {
    fn drop(&mut self) {
        if let Some(r) = self.registry.upgrade() {
            r.unpin(&self.name, &self.fingerprint);
        }
    }
}
impl Registry {
    pub fn load(home: impl Into<PathBuf>) -> Result<Self> {
        Self::load_with_bus(home, None)
    }
    pub fn load_with_bus(home: impl Into<PathBuf>, bus: Option<Arc<EventBus>>) -> Result<Self> {
        let home = home.into();
        for dir in ["extensions", "extension-versions", "extension-data"] {
            fs::create_dir_all(home.join(dir)).map_err(storage)?;
        }
        let records: BTreeMap<String, Record> = fs::read(home.join("extensions.json"))
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or_default();
        let r = Self {
            home,
            state: Mutex::new(State {
                records,
                ..State::default()
            }),
            bus,
            disabled: Mutex::default(),
            version_changed: Mutex::default(),
        };
        {
            let mut s = r.state.lock().unwrap();
            // Never interpolate unvalidated persisted names/fingerprints into filesystem paths.
            s.records.retain(|name, rec| {
                check_extension_name(name).is_ok()
                    && rec
                        .current
                        .iter()
                        .chain(rec.previous.iter())
                        .chain(rec.pending.iter().map(|p| &p.fingerprint))
                        .all(|f| valid_fingerprint(f))
            });
            for (name, record) in s.records.clone() {
                let info = r.info(&name, &record, &s.services);
                s.infos.insert(name, info);
            }
        }
        if let Err(error) = r.scan() {
            tracing::warn!(%error, "initial extension scan failed");
        }
        Ok(r)
    }
    fn version_path(&self, name: &str, fp: &str) -> PathBuf {
        snapshot::path(&self.home.join("extension-versions"), name, fp)
    }
    fn read_version(&self, name: &str, fp: &str) -> Option<ExtensionVersion> {
        let (_, v) = snapshot::inspect(&self.version_path(name, fp), name).ok()?;
        (v.fingerprint == fp).then_some(v)
    }
    fn info(
        &self,
        name: &str,
        rec: &Record,
        services: &BTreeMap<String, ExtensionServiceStatus>,
    ) -> ExtensionInfo {
        let current = rec
            .current
            .as_ref()
            .and_then(|f| self.read_version(name, f));
        let pending = rec.pending.as_ref().and_then(|p| {
            self.read_version(name, &p.fingerprint)
                .map(|version| ExtensionPending {
                    version,
                    requested_at: p.requested_at,
                    requested_by: p.requested_by,
                })
        });
        ExtensionInfo {
            name: name.into(),
            source: self
                .home
                .join("extensions")
                .join(name)
                .to_string_lossy()
                .into(),
            enabled: rec.enabled && current.is_some(),
            auto_trust: rec.auto_trust,
            error: if rec.current.is_some() && current.is_none() {
                Some("approved snapshot missing or corrupt".into())
            } else {
                None
            },
            current,
            previous_fingerprint: rec.previous.clone(),
            pending,
            service: services.get(name).cloned(),
        }
    }
    fn save(&self, records: &BTreeMap<String, Record>) -> Result<()> {
        let tmp = self.home.join("extensions.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(records).map_err(storage)?)
            .and_then(|_| fs::rename(tmp, self.home.join("extensions.json")))
            .map_err(storage)
    }
    fn notify(&self, name: String) {
        if let Some(bus) = &self.bus {
            let bus = bus.clone();
            tokio::spawn(async move {
                if let Err(e) = bus.publish(EventKind::ExtensionChanged { name }).await {
                    tracing::warn!("extension event: {e}");
                }
            });
        }
    }
    pub fn list(&self) -> Vec<ExtensionInfo> {
        self.state.lock().unwrap().infos.values().cloned().collect()
    }
    pub fn get(&self, name: &str) -> Result<ExtensionInfo> {
        self.state
            .lock()
            .unwrap()
            .infos
            .get(name)
            .cloned()
            .ok_or_else(|| missing(name))
    }
    /// Poll metadata only; copy and hash only when metadata changes.
    pub fn scan(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let mut names: BTreeSet<String> = s.records.keys().cloned().collect();
        for e in fs::read_dir(self.home.join("extensions")).map_err(storage)? {
            let e = match e {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(%error, "cannot read extension directory entry");
                    continue;
                }
            };
            if let Some(n) = e.file_name().to_str() {
                names.insert(n.into());
            }
        }
        for name in names {
            if let Err(error) = self.scan_one(&mut s, &name, false) {
                tracing::warn!(extension = %name, %error, "extension scan failed");
            }
        }
        let switched = std::mem::take(&mut s.switched);
        drop(s);
        self.fire_version_changed(switched);
        Ok(())
    }
    fn scan_one(&self, s: &mut State, name: &str, force: bool) -> Result<()> {
        if let Err(error) = check_extension_name(name) {
            let info = ExtensionInfo {
                name: name.into(),
                source: self
                    .home
                    .join("extensions")
                    .join(name)
                    .to_string_lossy()
                    .into(),
                enabled: false,
                auto_trust: false,
                current: None,
                previous_fingerprint: None,
                pending: None,
                service: s.services.get(name).cloned(),
                error: Some(error),
            };
            if s.infos.get(name) != Some(&info) {
                s.infos.insert(name.into(), info);
                self.notify(name.into());
            }
            return Ok(());
        }
        let source = self.home.join("extensions").join(name);
        let meta = snapshot::metadata(&source);
        if !force
            && meta
                .as_ref()
                .ok()
                .is_some_and(|m| s.metadata.get(name) == Some(m))
        {
            self.try_cleanup(s, name);
            return Ok(());
        }
        let old = s.infos.get(name).cloned();
        let mut rec = s.records.get(name).cloned().unwrap_or_default();
        let result = meta.as_ref().map_err(|e| e.to_string()).and_then(|_| {
            snapshot::create(
                &source,
                &self.home.join("extension-versions").join(name),
                name,
            )
            .map_err(|e| e.to_string())
        });
        let error = match result {
            Ok(v) => {
                if rec.current.as_ref() == Some(&v.fingerprint) {
                    rec.pending = None;
                } else if rec.auto_trust {
                    let enabled = rec.enabled;
                    Self::switch(&mut rec, v.fingerprint);
                    rec.enabled = enabled;
                } else if rec.pending.as_ref().map(|p| &p.fingerprint) != Some(&v.fingerprint) {
                    rec.pending = Some(Pending {
                        fingerprint: v.fingerprint,
                        requested_at: chrono::Utc::now(),
                        requested_by: None,
                    });
                }
                None
            }
            Err(e) => {
                rec.pending = None;
                Some(e)
            }
        };
        let mut records = s.records.clone();
        records.insert(name.into(), rec.clone());
        if records != s.records {
            self.save(&records)?;
        }
        let mut info = self.info(name, &rec, &s.services);
        // Preserve snapshot timestamps across polls when the fingerprint is unchanged.
        if let Some(old) = &old {
            if info.current.as_ref().map(|v| &v.fingerprint)
                == old.current.as_ref().map(|v| &v.fingerprint)
            {
                info.current = old.current.clone();
            }
            if info.pending.as_ref().map(|p| &p.version.fingerprint)
                == old.pending.as_ref().map(|p| &p.version.fingerprint)
            {
                info.pending = old.pending.clone();
            }
        }
        let switched = old.as_ref().is_none_or(|old| {
            old.enabled != info.enabled
                || old.current.as_ref().map(|v| &v.fingerprint)
                    != info.current.as_ref().map(|v| &v.fingerprint)
        });
        if error.is_some() {
            info.error = error;
        }
        s.records = records;
        if let Ok(meta) = meta {
            s.metadata.insert(name.into(), meta);
        } else {
            s.metadata.remove(name);
        }
        let changed = old.as_ref() != Some(&info);
        s.infos.insert(name.into(), info);
        if switched {
            s.switched.push(name.into());
            *s.generations.entry(name.into()).or_default() += 1;
        }
        self.try_cleanup(s, name);
        if changed {
            self.notify(name.into());
        }
        Ok(())
    }
    fn switch(rec: &mut Record, fp: String) {
        if rec.current.as_ref() != Some(&fp) {
            rec.previous = rec.current.replace(fp);
        }
        rec.pending = None;
        rec.enabled = true;
    }
    /// Forces a fresh snapshot before approval, invalidating stale approval requests even between polls.
    pub fn enable(&self, name: &str, fingerprint: &str) -> Result<ExtensionInfo> {
        check_extension_name(name).map_err(CoreError::InvalidName)?;
        let mut s = self.state.lock().unwrap();
        if !s.records.contains_key(name) {
            return Err(missing(name));
        }
        let mut rec = s.records.get(name).cloned().ok_or_else(|| missing(name))?;
        if rec.current.as_deref() == Some(fingerprint) {
            if self.read_version(name, fingerprint).is_none() {
                return Err(conflict("approved snapshot is unavailable"));
            }
            rec.enabled = true;
            let info = self.update(&mut s, name, rec)?;
            let switched = std::mem::take(&mut s.switched);
            drop(s);
            self.fire_version_changed(switched);
            return Ok(info);
        }
        self.scan_one(&mut s, name, true)?;
        let mut rec = s.records.get(name).cloned().ok_or_else(|| missing(name))?;
        let candidate = rec.pending.as_ref().map(|p| p.fingerprint.as_str());
        if candidate != Some(fingerprint) || s.infos[name].error.is_some() {
            return Err(conflict(
                "fingerprint does not match the pending snapshot; refresh before approving",
            ));
        }
        Self::switch(&mut rec, fingerprint.into());
        let info = self.update(&mut s, name, rec)?;
        let switched = std::mem::take(&mut s.switched);
        drop(s);
        self.fire_version_changed(switched);
        Ok(info)
    }
    pub fn disable(&self, name: &str) -> Result<ExtensionInfo> {
        check_extension_name(name).map_err(CoreError::InvalidName)?;
        let mut s = self.state.lock().unwrap();
        let mut rec = s.records.get(name).cloned().ok_or_else(|| missing(name))?;
        rec.enabled = false;
        let info = self.update(&mut s, name, rec)?;
        let switched = std::mem::take(&mut s.switched);
        drop(s);
        for hook in self.disabled.lock().unwrap().iter() {
            hook(name);
        }
        self.fire_version_changed(switched);
        Ok(info)
    }
    /// Calls `hook` with the extension's name whenever one is disabled, after the change is
    /// saved and the registry's lock is released. The runner uses it to end running calls.
    pub fn on_disabled(&self, hook: impl Fn(&str) + Send + Sync + 'static) {
        self.disabled.lock().unwrap().push(Box::new(hook));
    }
    /// Calls `hook` with the extension's name after it is enabled, disabled or switched to
    /// another snapshot, with the lock released. Same shape as [`Self::on_disabled`]; the
    /// resident-process supervisor uses it to stop the old process and start the new one.
    pub fn on_version_changed(&self, hook: impl Fn(&str) + Send + Sync + 'static) {
        self.version_changed.lock().unwrap().push(Box::new(hook));
    }
    fn fire_version_changed(&self, names: Vec<String>) {
        if names.is_empty() {
            return;
        }
        let hooks = self.version_changed.lock().unwrap();
        for name in names {
            for hook in hooks.iter() {
                hook(&name);
            }
        }
    }
    /// Records what one extension's resident process is doing. Live state of this run only:
    /// it reaches clients through `ExtensionInfo.service` and is never written to disk.
    pub fn set_service_status(&self, name: &str, status: Option<ExtensionServiceStatus>) {
        let mut s = self.state.lock().unwrap();
        let Some(info) = s.infos.get_mut(name) else {
            return;
        };
        info.service = status.clone();
        match status {
            Some(status) => s.services.insert(name.into(), status),
            None => s.services.remove(name),
        };
    }
    /// Setting this explicit human-controlled flag also accepts the current pending update.
    pub fn set_auto_trust(&self, name: &str, value: bool) -> Result<ExtensionInfo> {
        check_extension_name(name).map_err(CoreError::InvalidName)?;
        let mut s = self.state.lock().unwrap();
        if !s.records.contains_key(name) {
            return Err(missing(name));
        }
        self.scan_one(&mut s, name, true)?;
        let mut rec = s.records[name].clone();
        rec.auto_trust = value;
        if value {
            if let Some(p) = &rec.pending {
                let fp = p.fingerprint.clone();
                Self::switch(&mut rec, fp);
            }
        }
        let info = self.update(&mut s, name, rec)?;
        let switched = std::mem::take(&mut s.switched);
        drop(s);
        self.fire_version_changed(switched);
        Ok(info)
    }
    /// Where this hub reads extension sources from.
    pub fn sources(&self) -> PathBuf {
        self.home.join("extensions")
    }
    /// Static inspection of source files; no subprocess, import, approval or state change.
    pub fn check(&self, name: &str) -> Result<ExtensionVersion> {
        check_extension_name(name).map_err(CoreError::InvalidName)?;
        snapshot::inspect(&self.home.join("extensions").join(name), name)
            .map(|(_, version)| version)
            .map_err(|e| CoreError::InvalidName(format!("{e:#}")))
    }
    pub fn request_enable(
        &self,
        name: &str,
        requested_by: Option<AgentId>,
    ) -> Result<ExtensionInfo> {
        check_extension_name(name).map_err(CoreError::InvalidName)?;
        let mut s = self.state.lock().unwrap();
        match fs::symlink_metadata(self.home.join("extensions").join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(missing(name)),
            Err(e) => return Err(storage(e)),
            Ok(_) => {}
        }
        let version = snapshot::create(
            &self.home.join("extensions").join(name),
            &self.home.join("extension-versions").join(name),
            name,
        )
        .map_err(|e| CoreError::InvalidName(format!("{e:#}")))?;
        let mut rec = s.records.get(name).cloned().unwrap_or_default();
        rec.pending = if rec.current.as_ref() == Some(&version.fingerprint) {
            None
        } else {
            Some(Pending {
                fingerprint: version.fingerprint,
                requested_at: chrono::Utc::now(),
                requested_by,
            })
        };
        // A valid fresh snapshot supersedes an earlier source validation error.
        if let Some(info) = s.infos.get_mut(name) {
            info.error = None;
        }
        self.update(&mut s, name, rec)
    }
    fn update(&self, s: &mut State, name: &str, rec: Record) -> Result<ExtensionInfo> {
        let mut records = s.records.clone();
        records.insert(name.into(), rec.clone());
        if records != s.records {
            self.save(&records)?;
        }
        let mut info = self.info(name, &rec, &s.services);
        if let Some(old) = s.infos.get(name) {
            if old.error.is_some() {
                info.error = old.error.clone();
            }
            if old.enabled != info.enabled
                || old.current.as_ref().map(|v| &v.fingerprint)
                    != info.current.as_ref().map(|v| &v.fingerprint)
            {
                s.switched.push(name.into());
                *s.generations.entry(name.into()).or_default() += 1;
            }
        }
        s.records = records;
        s.infos.insert(name.into(), info.clone());
        self.try_cleanup(s, name);
        self.notify(name.into());
        Ok(info)
    }
    fn try_cleanup(&self, s: &State, name: &str) {
        if let Err(error) = self.cleanup(s, name) {
            tracing::warn!(extension = %name, %error, "snapshot cleanup failed; will retry");
        }
    }
    fn cleanup(&self, s: &State, name: &str) -> Result<()> {
        let Some(rec) = s.records.get(name) else {
            return Ok(());
        };
        let root = self.home.join("extension-versions").join(name);
        if !root.exists() {
            return Ok(());
        }
        for e in fs::read_dir(root).map_err(storage)? {
            let e = e.map_err(storage)?;
            let fp = e.file_name().to_string_lossy().into_owned();
            if rec.current.as_ref() == Some(&fp)
                || rec.previous.as_ref() == Some(&fp)
                || rec.pending.as_ref().is_some_and(|p| p.fingerprint == fp)
                || s.occupied.get(&(name.into(), fp)).copied().unwrap_or(0) > 0
            {
                continue;
            }
            snapshot::remove(&e.path()).map_err(storage)?;
        }
        Ok(())
    }
    fn enabled_locked(&self, s: &State, name: &str) -> Result<EnabledVersion> {
        let info = s.infos.get(name).ok_or_else(|| missing(name))?;
        if !info.enabled {
            return Err(conflict("extension is not enabled"));
        }
        let version = info
            .current
            .clone()
            .ok_or_else(|| conflict("approved snapshot is unavailable"))?;
        let path = self.version_path(name, &version.fingerprint);
        let manifest = Manifest::read(&path, name).map_err(storage)?;
        let data = self.home.join("extension-data").join(name);
        fs::create_dir_all(&data).map_err(storage)?;
        Ok(EnabledVersion {
            path,
            data,
            version,
            manifest,
        })
    }
    /// For inspection. Runners must use acquire() to pin the version before use.
    pub fn enabled(&self, name: &str) -> Result<EnabledVersion> {
        self.enabled_locked(&self.state.lock().unwrap(), name)
    }
    pub fn acquire(self: &Arc<Self>, name: &str) -> Result<SnapshotLease> {
        let mut s = self.state.lock().unwrap();
        let enabled = self.enabled_locked(&s, name)?;
        let fingerprint = enabled.version.fingerprint.clone();
        let generation = s.generations.get(name).copied().unwrap_or(0);
        *s.occupied
            .entry((name.into(), fingerprint.clone()))
            .or_default() += 1;
        Ok(SnapshotLease {
            enabled,
            generation,
            registry: Arc::downgrade(self),
            name: name.into(),
            fingerprint,
        })
    }
    /// Whether a lease still matches what is enabled now: the same snapshot, and no enable,
    /// disable or version change since it was taken. A hook takes its lease before the runner
    /// registers the command, and a disable in that window must not let the process start
    /// (docs/EXTENSIONS.md §3).
    pub fn lease_is_current(&self, name: &str, fingerprint: &str, generation: u64) -> bool {
        let s = self.state.lock().unwrap();
        s.generations.get(name).copied().unwrap_or(0) == generation
            && s.infos.get(name).is_some_and(|info| {
                info.enabled
                    && info
                        .current
                        .as_ref()
                        .is_some_and(|version| version.fingerprint == fingerprint)
            })
    }
    fn unpin(&self, name: &str, fingerprint: &str) {
        let mut s = self.state.lock().unwrap();
        let key = (name.into(), fingerprint.into());
        if let Some(count) = s.occupied.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                s.occupied.remove(&key);
            }
        }
        self.try_cleanup(&s, name);
    }
    pub fn start(
        self: &Arc<Self>,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let r = self.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tokio::select! { _ = shutdown.cancelled() => break, _ = timer.tick() => {
                    let r = r.clone(); match tokio::task::spawn_blocking(move || r.scan()).await { Ok(Ok(())) => {}, other => tracing::warn!("extension scan failed: {other:?}") }
                } }
            }
        })
    }
    pub fn pending_diff(&self, name: &str) -> Result<ExtensionDiffResponse> {
        let s = self.state.lock().unwrap();
        let info = s.infos.get(name).ok_or_else(|| missing(name))?;
        let pending = info
            .pending
            .as_ref()
            .ok_or_else(|| missing("pending snapshot"))?;
        let current = info.current.as_ref();
        let b = self.version_path(name, &pending.version.fingerprint);
        let a = current.map(|v| self.version_path(name, &v.fingerprint));
        diff(name, &b, a.as_deref(), &pending.version, current).map_err(storage)
    }
}
fn valid_fingerprint(fp: &str) -> bool {
    fp.len() == 64 && fp.bytes().all(|c| c.is_ascii_hexdigit())
}
fn diff(
    name: &str,
    b: &Path,
    a: Option<&Path>,
    pending: &ExtensionVersion,
    current: Option<&ExtensionVersion>,
) -> anyhow::Result<ExtensionDiffResponse> {
    let bm = snapshot::metadata(b)?;
    let am = a.map(snapshot::metadata).transpose()?.unwrap_or_default();
    let paths: BTreeSet<_> = bm.keys().chain(am.keys()).collect();
    let mut out = ExtensionDiffResponse {
        name: name.into(),
        fingerprint: pending.fingerprint.clone(),
        against: current.map(|v| v.fingerprint.clone()),
        diff: String::new(),
        truncated: false,
        files: vec![],
        access: pending.access,
        access_changed: current.is_some_and(|v| v.access != pending.access),
    };
    for path in paths {
        let old = if am.contains_key(path) {
            fs::read(a.unwrap().join(path))?
        } else {
            vec![]
        };
        let new = if bm.contains_key(path) {
            fs::read(b.join(path))?
        } else {
            vec![]
        };
        if am.contains_key(path) && bm.contains_key(path) && old == new {
            continue;
        }
        let change = if !am.contains_key(path) {
            ExtensionFileChangeKind::Added
        } else if !bm.contains_key(path) {
            ExtensionFileChangeKind::Removed
        } else {
            ExtensionFileChangeKind::Modified
        };
        let text = if old.len().max(new.len()) > 1024 * 1024 || old.contains(&0) || new.contains(&0)
        {
            None
        } else {
            std::str::from_utf8(&old)
                .ok()
                .zip(std::str::from_utf8(&new).ok())
        };
        let (mut added, mut removed) = (0, 0);
        if let Some((old, new)) = text {
            let d = similar::TextDiff::from_lines(old, new);
            for c in d.iter_all_changes() {
                match c.tag() {
                    similar::ChangeTag::Insert => added += 1,
                    similar::ChangeTag::Delete => removed += 1,
                    _ => {}
                }
            }
            let unified = d
                .unified_diff()
                .header(&format!("a/{path}"), &format!("b/{path}"))
                .to_string();
            const CAP: usize = 2 * 1024 * 1024;
            if out.diff.len() + unified.len() <= CAP {
                out.diff.push_str(&unified);
            } else {
                out.truncated = true;
            }
        }
        out.files.push(ExtensionFileChange {
            path: path.clone(),
            change,
            added,
            removed,
            binary: text.is_none(),
        });
    }
    Ok(out)
}

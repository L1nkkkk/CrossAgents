//! What each CLI version did when it ran, kept in `<home>/cli-runs.json`: the evidence
//! behind "this version works with CrossAgents" and "this version broke on its first run".
//!
//! A run counts as ok once the driver parsed an event the CLI itself sent (the handshake);
//! it counts as a protocol failure only when the CLI could not be talked to at all (it left
//! without a parsable event, a flag was refused, its output could not be read). Quotas,
//! networks and models are not the version's fault and are never recorded here. The newer
//! of the two records decides.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use crossagents_core::{ExitCause, SessionEvent, SessionEventKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub plugin: String,
    pub path: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_protocol_fail_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_message: Option<String>,
}

impl Record {
    /// Whether the latest word on this version is a protocol failure.
    pub fn failing(&self) -> bool {
        match (self.last_ok_at, self.last_protocol_fail_at) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(ok), Some(fail)) => fail > ok,
        }
    }

    pub fn working(&self) -> bool {
        self.last_ok_at.is_some() && !self.failing()
    }
}

/// How a CLI version stands, for the GUI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VersionStanding {
    /// `verified`, `known_good`, `untried` or `protocol_failed`.
    pub status: &'static str,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub last_protocol_fail_at: Option<DateTime<Utc>>,
    pub fail_message: Option<String>,
    /// Another version of the plugin that last worked here, when this one fails.
    pub last_good_version: Option<String>,
}

pub struct CliRuns {
    path: Option<PathBuf>,
    records: RwLock<BTreeMap<String, Record>>,
}

fn key(plugin: &str, path: &str, version: &str) -> String {
    format!("{plugin}|{path}|{version}")
}

impl CliRuns {
    /// Reads `path`; a missing or unreadable file starts with no evidence.
    pub fn load(path: PathBuf) -> Self {
        let records = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!("ignoring {}: {err}", path.display());
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        };
        Self {
            path: Some(path),
            records: RwLock::new(records),
        }
    }

    /// Evidence kept in memory only, for tests and hubs without a home.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            records: RwLock::new(BTreeMap::new()),
        }
    }

    pub fn get(&self, plugin: &str, path: &str, version: &str) -> Option<Record> {
        self.records
            .read()
            .unwrap()
            .get(&key(plugin, path, version))
            .cloned()
    }

    pub fn record_ok(&self, plugin: &str, path: &str, version: &str) {
        self.update(plugin, path, version, |record| {
            record.last_ok_at = Some(Utc::now());
        });
    }

    pub fn record_protocol_fail(&self, plugin: &str, path: &str, version: &str, message: &str) {
        let message: String = message.chars().take(500).collect();
        self.update(plugin, path, version, |record| {
            record.last_protocol_fail_at = Some(Utc::now());
            record.fail_message = Some(message);
        });
    }

    fn update(&self, plugin: &str, path: &str, version: &str, change: impl FnOnce(&mut Record)) {
        let mut records = self.records.write().unwrap();
        let record = records
            .entry(key(plugin, path, version))
            .or_insert_with(|| Record {
                plugin: plugin.to_owned(),
                path: path.to_owned(),
                version: version.to_owned(),
                ..Record::default()
            });
        change(record);
        let Some(file) = &self.path else { return };
        match serde_json::to_vec_pretty(&*records) {
            Ok(bytes) => {
                if let Err(err) = crate::paths::write_atomic(file, &bytes) {
                    tracing::warn!("cannot write {}: {err}", file.display());
                }
            }
            Err(err) => tracing::warn!("cannot serialize CLI runs: {err}"),
        }
    }

    /// How `version` of `plugin` at `path` stands. `verified` says the driver was verified
    /// on this version's series: it counts as working until a run says otherwise.
    pub fn standing(
        &self,
        plugin: &str,
        path: &str,
        version: &str,
        verified: bool,
    ) -> VersionStanding {
        let record = self.get(plugin, path, version).unwrap_or_default();
        let status = if record.failing() {
            "protocol_failed"
        } else if verified {
            "verified"
        } else if record.working() {
            "known_good"
        } else {
            "untried"
        };
        let last_good_version = (status == "protocol_failed")
            .then(|| self.last_good(plugin, version))
            .flatten();
        VersionStanding {
            status,
            last_ok_at: record.last_ok_at,
            last_protocol_fail_at: record.last_protocol_fail_at,
            fail_message: record.fail_message.filter(|_| status == "protocol_failed"),
            last_good_version,
        }
    }

    /// The other version of `plugin` that worked most recently.
    fn last_good(&self, plugin: &str, except: &str) -> Option<String> {
        self.records
            .read()
            .unwrap()
            .values()
            .filter(|r| r.plugin == plugin && r.version != except && r.working())
            .max_by_key(|r| r.last_ok_at)
            .map(|r| r.version.clone())
    }
}

/// Whether a session's error or exit message says the CLI could not be talked to, rather
/// than that the vendor refused the work (quota, rate limit, network, model). Only judged
/// for sessions in which the driver never parsed an event of the CLI's own.
pub fn protocol_failure(message: &str) -> bool {
    let lower = message.to_lowercase();
    const NOT_THE_VERSION: &[&str] = &[
        "429",
        "rate limit",
        "rate_limit",
        "quota",
        "usage limit",
        "credit",
        "billing",
        "network",
        "timed out",
        "timeout",
        "connection",
        "dns",
        "econn",
        "proxy",
        "certificate",
        "unauthorized",
        "401",
        "403",
        "login",
        "not logged in",
        "api key",
        "model",
        "overloaded",
        "503",
        "502",
        "killed",
        "cancel",
    ];
    if NOT_THE_VERSION.iter().any(|word| lower.contains(word)) {
        return false;
    }
    const PROTOCOL: &[&str] = &[
        "before the turn ended",
        "unknown flag",
        "unknown option",
        "unrecognized option",
        "unrecognized argument",
        "unexpected argument",
        "unknown argument",
        "invalid option",
        "cannot parse",
        "could not parse",
        "failed to parse",
        "parse error",
        "unexpected token",
        "crashed",
    ];
    PROTOCOL.iter().any(|word| lower.contains(word))
}

/// Watches one session's events for the evidence: the first event the driver parsed from
/// the CLI's own output (the handshake) records the version as working; a protocol failure
/// before any such event records it as broken. Each at most once per session.
pub struct Witness {
    runs: Arc<CliRuns>,
    plugin: String,
    path: String,
    version: String,
    done: bool,
}

impl Witness {
    pub fn new(runs: Arc<CliRuns>, plugin: String, path: String, version: String) -> Self {
        Self {
            runs,
            plugin,
            path,
            version,
            done: false,
        }
    }

    /// Looks at one event; returns what it recorded, if anything.
    pub fn observe(&mut self, event: &SessionEvent) -> Option<bool> {
        if self.done {
            return None;
        }
        match &event.kind {
            SessionEventKind::Error { message } if protocol_failure(message) => {
                self.fail(message);
                Some(false)
            }
            SessionEventKind::Exited {
                message,
                cause: Some(ExitCause::Crashed),
                ..
            } if protocol_failure(message) => {
                self.fail(message);
                Some(false)
            }
            // Drivers that synthesize these without the CLI (With's `Started`, every
            // driver's `TurnStarted`/`TurnCompleted`) prove nothing by themselves.
            SessionEventKind::Started { .. }
            | SessionEventKind::TurnStarted { .. }
            | SessionEventKind::TurnCompleted { .. }
            | SessionEventKind::PromptViaFile
            | SessionEventKind::Warning { .. }
            | SessionEventKind::Error { .. }
            | SessionEventKind::Exited { .. }
                if event.raw.is_none() =>
            {
                None
            }
            _ => {
                self.done = true;
                self.runs.record_ok(&self.plugin, &self.path, &self.version);
                Some(true)
            }
        }
    }

    /// Called with what ended a session that never got going (an open or a turn that
    /// failed): records a protocol failure when nothing was recorded yet and the message
    /// says the CLI could not be talked to. Returns whether it recorded one.
    pub fn conclude(&mut self, message: &str) -> bool {
        if self.done || !protocol_failure(message) {
            return false;
        }
        self.fail(message);
        true
    }

    /// Whether this session already recorded its evidence.
    pub fn recorded(&self) -> bool {
        self.done
    }

    fn fail(&mut self, message: &str) {
        self.done = true;
        tracing::warn!(
            plugin = %self.plugin,
            version = %self.version,
            message,
            "the CLI failed before it said anything the driver could read"
        );
        self.runs
            .record_protocol_fail(&self.plugin, &self.path, &self.version, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newer_record_decides_and_is_kept_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cli-runs.json");
        let runs = CliRuns::load(file.clone());
        assert_eq!(runs.standing("with", "/w", "1.0.0", false).status, "untried");
        assert_eq!(runs.standing("with", "/w", "1.0.0", true).status, "verified");

        runs.record_ok("with", "/w", "1.0.0");
        assert_eq!(
            runs.standing("with", "/w", "1.0.0", false).status,
            "known_good"
        );

        std::thread::sleep(std::time::Duration::from_millis(5));
        runs.record_protocol_fail("with", "/w", "1.1.0", "with-cli exited before the turn ended");
        let failed = runs.standing("with", "/w", "1.1.0", false);
        assert_eq!(failed.status, "protocol_failed");
        assert_eq!(failed.last_good_version.as_deref(), Some("1.0.0"));
        assert!(failed.fail_message.unwrap().contains("before the turn ended"));
        // A failure beats the seed list.
        assert_eq!(
            runs.standing("with", "/w", "1.1.0", true).status,
            "protocol_failed"
        );

        std::thread::sleep(std::time::Duration::from_millis(5));
        runs.record_ok("with", "/w", "1.1.0");
        let again = CliRuns::load(file);
        let fixed = again.standing("with", "/w", "1.1.0", false);
        assert_eq!(fixed.status, "known_good");
        assert_eq!(fixed.fail_message, None);
        assert_eq!(fixed.last_good_version, None);
    }

    #[test]
    fn only_talking_failures_count_against_a_version() {
        assert!(protocol_failure("with-cli exited before the turn ended"));
        assert!(protocol_failure("error: unknown flag: --output-format"));
        assert!(protocol_failure("error: unexpected argument '--foo' found"));
        assert!(protocol_failure("with-cli crashed (exit code 2)"));
        assert!(!protocol_failure("429 Too Many Requests"));
        assert!(!protocol_failure("rate limit reached before the turn ended"));
        assert!(!protocol_failure("network error: connection reset"));
        assert!(!protocol_failure("model gpt-x is not available"));
        assert!(!protocol_failure("You have hit your usage limit"));
        assert!(!protocol_failure("the agent was stopped"));
        assert!(!protocol_failure("claude exited"));
    }

    #[test]
    fn a_witness_records_the_handshake_once_and_ignores_what_drivers_make_up() {
        let runs = Arc::new(CliRuns::in_memory());
        let mut witness = Witness::new(runs.clone(), "with".into(), "/w".into(), "2.0.0".into());
        let plain = |kind| SessionEvent { kind, raw: None };
        assert_eq!(
            witness.observe(&plain(SessionEventKind::Started {
                session_id: "s".into()
            })),
            None
        );
        assert_eq!(
            witness.observe(&plain(SessionEventKind::Error {
                message: "429 rate limit".into()
            })),
            None
        );
        let parsed = SessionEvent::with_raw(
            SessionEventKind::Error {
                message: "RUN_ERROR: bad".into(),
            },
            serde_json::json!({}),
        );
        assert_eq!(witness.observe(&parsed), Some(true));
        assert_eq!(witness.observe(&parsed), None);
        assert_eq!(runs.standing("with", "/w", "2.0.0", false).status, "known_good");

        let mut broken = Witness::new(runs.clone(), "with".into(), "/w".into(), "2.1.0".into());
        assert_eq!(
            broken.observe(&plain(SessionEventKind::Error {
                message: "with-cli crashed (exit code 2) before the turn ended".into()
            })),
            Some(false)
        );
        let standing = runs.standing("with", "/w", "2.1.0", false);
        assert_eq!(standing.status, "protocol_failed");
        assert_eq!(standing.last_good_version.as_deref(), Some("2.0.0"));
    }
}

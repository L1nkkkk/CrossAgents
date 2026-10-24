//! Every published event, one JSON line each, in a file per day under `logs/`.
//!
//! `hub.log` carries what the hub itself reported (warnings, errors). This carries what
//! happened: one line per event, exactly as `GET /v1/events` returns it. It is meant for
//! reading when no window is open (or after the hub is gone): `crossagents events --log`,
//! or any `tail`/`grep` (`Get-Content -Wait` on Windows).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use crossagents_core::{Event, EventSink};

/// Lines buffered while the writer catches up. Past this they are dropped and counted: a slow
/// or full disk must not slow the hub down.
const QUEUE: usize = 8192;

/// Days of files kept; older ones go when a hub starts.
const KEEP_DAYS: i64 = 14;

/// A daily file is `events-2026-10-23.jsonl`.
const PREFIX: &str = "events-";
const SUFFIX: &str = ".jsonl";

/// Appends events to today's file. Dropping it (at shutdown) ends the writer thread.
pub struct EventLog {
    tx: SyncSender<String>,
    dropped: AtomicU64,
}

impl EventLog {
    /// Starts the writer thread and removes files past [`KEEP_DAYS`].
    pub fn spawn(dir: PathBuf) -> Arc<Self> {
        let (tx, rx) = sync_channel(QUEUE);
        let writer_dir = dir.clone();
        let spawned = std::thread::Builder::new()
            .name("event-log".into())
            .spawn(move || write_loop(&writer_dir, rx));
        if let Err(err) = spawned {
            tracing::warn!(error = %err, "cannot start the event log writer");
        }
        prune(&dir);
        Arc::new(Self {
            tx,
            dropped: AtomicU64::new(0),
        })
    }

    /// How many events were dropped because the writer could not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl EventSink for EventLog {
    async fn handle(&self, event: &Event) {
        let line = match serde_json::to_string(event) {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!(error = %err, "cannot serialize an event for the log");
                return;
            }
        };
        match self.tx.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped == 1 || dropped.is_multiple_of(1000) {
                    tracing::warn!(
                        dropped,
                        "the event log is falling behind; events are being dropped"
                    );
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                tracing::warn!("the event log writer is gone; events are no longer logged");
            }
        }
    }
}

/// The file holding `date`'s events.
pub fn file(dir: &Path, date: NaiveDate) -> PathBuf {
    dir.join(format!("{PREFIX}{}{SUFFIX}", date.format("%Y-%m-%d")))
}

/// The date in a name of ours, `events-YYYY-MM-DD.jsonl`; `None` for anything else, so a
/// person's own files in `logs/` are never touched.
pub fn date_of(name: &str) -> Option<NaiveDate> {
    let date = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

fn write_loop(dir: &Path, rx: std::sync::mpsc::Receiver<String>) {
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::warn!(error = %err, dir = %dir.display(), "cannot create the event log directory");
        return;
    }
    let mut open: Option<(NaiveDate, File)> = None;
    let mut warned = false;
    while let Ok(line) = rx.recv() {
        let today = Utc::now().date_naive();
        if open.as_ref().map(|(date, _)| *date) != Some(today) {
            match append(&file(dir, today)) {
                Ok(file) => {
                    open = Some((today, file));
                    warned = false;
                }
                Err(err) => {
                    if !warned {
                        tracing::warn!(error = %err, "cannot open today's event log");
                        warned = true;
                    }
                    open = None;
                }
            }
        }
        let Some((_, file)) = open.as_mut() else {
            continue;
        };
        // Unbuffered on purpose: a reader sees a line as soon as it is written.
        let written = file
            .write_all(line.as_bytes())
            .and_then(|()| file.write_all(b"\n"));
        if let Err(err) = written {
            if !warned {
                tracing::warn!(error = %err, "cannot write to the event log");
                warned = true;
            }
            open = None;
        }
    }
}

fn append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn prune(dir: &Path) {
    let cutoff = Utc::now().date_naive() - Duration::days(KEEP_DAYS);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(date) = date_of(name) else {
            continue;
        };
        if date >= cutoff {
            continue;
        }
        if let Err(err) = std::fs::remove_file(entry.path()) {
            tracing::warn!(error = %err, file = %entry.path().display(), "cannot remove an old event log");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration as StdDuration, Instant};

    use chrono::Utc;
    use crossagents_core::{EVENT_SCHEMA_VERSION, EventKind};
    use uuid::Uuid;

    use super::*;

    fn event(seq: u64) -> Event {
        Event {
            seq,
            id: Uuid::now_v7(),
            ts: Utc::now(),
            schema: EVENT_SCHEMA_VERSION,
            kind: EventKind::HubStarted {
                version: "0-test".into(),
            },
        }
    }

    #[test]
    fn only_our_own_names_carry_a_date() {
        assert_eq!(
            date_of("events-2026-10-23.jsonl"),
            NaiveDate::from_ymd_opt(2026, 10, 23)
        );
        assert_eq!(date_of("hub.log.2026-10-23"), None);
        assert_eq!(date_of("events-not-a-date.jsonl"), None);
        assert_eq!(date_of("events-2026-10-23.txt"), None);
    }

    #[tokio::test]
    async fn events_are_written_one_line_each() {
        let home = tempfile::tempdir().unwrap();
        let log = EventLog::spawn(home.path().to_path_buf());
        for seq in 1..=3 {
            log.handle(&event(seq)).await;
        }
        let path = file(home.path(), Utc::now().date_naive());

        // The writer thread is separate; give it a moment rather than sleeping blindly.
        let deadline = Instant::now() + StdDuration::from_secs(5);
        loop {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            if lines.len() == 3 {
                let first: Event = serde_json::from_str(lines[0]).unwrap();
                assert_eq!(first.seq, 1);
                assert_eq!(first.kind.type_name(), "hub.started");
                drop(log);
                return;
            }
            assert!(Instant::now() < deadline, "no log written: {text:?}");
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn old_files_go_and_other_files_stay() {
        let home = tempfile::tempdir().unwrap();
        let old = file(
            home.path(),
            Utc::now().date_naive() - Duration::days(KEEP_DAYS + 1),
        );
        let recent = file(home.path(), Utc::now().date_naive() - Duration::days(1));
        let foreign = home.path().join("notes.txt");
        for path in [&old, &recent, &foreign] {
            std::fs::write(path, "{}\n").unwrap();
        }

        let _log = EventLog::spawn(home.path().to_path_buf());

        assert!(!old.exists(), "an expired file should be removed");
        assert!(recent.exists(), "a file inside the window should stay");
        assert!(foreign.exists(), "someone else's file should stay");
    }
}

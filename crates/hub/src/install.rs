//! Installing and upgrading a vendor CLI, from its agent description (docs/DESIGN.md §9.3).
//!
//! The description says what to run; the hub runs exactly that, one job per plugin at a
//! time, and reports it on the event stream: `cli.install.started`, `cli.install.output`
//! (line by line, capped), `cli.install.finished`, then `plugin.changed` so clients re-read
//! `GET /v1/plugins`.
//!
//! Success is what the lookup says, not the exit code: a command that exits 0 but leaves no
//! program behind is a failure, and the reason lists everywhere the description looked.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crossagents_core::{CoreError, Driver, EventBus, EventKind};
use crossagents_drivers::install;
use tokio::sync::Mutex;

/// What was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Upgrade,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Upgrade => "upgrade",
        }
    }

    /// The past participle, for the 409 a person reads: `installed`, `upgraded`.
    pub fn past(self) -> &'static str {
        match self {
            Self::Install => "installed",
            Self::Upgrade => "upgraded",
        }
    }
}

/// One install or upgrade the API accepted: what to run, and where that came from.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub plugin: String,
    pub action: Action,
    /// The description's own command, as argv. Nothing else is ever run.
    pub argv: Vec<String>,
    /// `built-in`, a manifest's path, or `config.toml`.
    pub described_by: Option<String>,
}

/// The installs and upgrades running now, by plugin.
///
/// Only in memory, by design: an install runs in a child of this process, so after a restart
/// nothing is running and `GET /v1/plugins` reports no install in progress.
#[derive(Default)]
pub struct Installer {
    /// What each plugin's job is doing now.
    running: Mutex<HashMap<String, Action>>,
}

impl Installer {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is running for `plugin` now, or `None`: what `CliInfo.installing` reports.
    pub async fn installing(&self, plugin: &str) -> Option<Action> {
        self.running.lock().await.get(plugin).copied()
    }

    /// Starts `job`, or refuses (409) when that plugin already has one running.
    ///
    /// Returns once the job is accepted and `cli.install.started` is written; the work goes
    /// on in the background.
    pub async fn start(
        self: &Arc<Self>,
        bus: Arc<EventBus>,
        driver: Arc<dyn Driver>,
        job: JobSpec,
    ) -> Result<(), CoreError> {
        self.claim(&job.plugin, job.action).await?;
        if let Err(err) = bus
            .publish(EventKind::CliInstallStarted {
                plugin: job.plugin.clone(),
                action: job.action.as_str().to_owned(),
                argv: job.argv.clone(),
                described_by: job.described_by.clone(),
            })
            .await
        {
            self.release(&job.plugin).await;
            return Err(err);
        }
        let this = self.clone();
        tokio::spawn(async move { this.run(bus, driver, job).await });
        Ok(())
    }

    /// Reserves a plugin; `Err(Conflict)` when it already has a job running.
    async fn claim(&self, plugin: &str, action: Action) -> Result<(), CoreError> {
        let mut running = self.running.lock().await;
        if let Some(current) = running.get(plugin) {
            return Err(CoreError::Conflict(format!(
                "{plugin} is already being {}; wait for it to finish",
                current.past()
            )));
        }
        running.insert(plugin.to_owned(), action);
        Ok(())
    }

    async fn release(&self, plugin: &str) {
        self.running.lock().await.remove(plugin);
    }

    async fn run(self: Arc<Self>, bus: Arc<EventBus>, driver: Arc<dyn Driver>, job: JobSpec) {
        let started = Instant::now();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, u64)>();
        // Output is written as it arrives, from its own task: the command keeps running
        // while a slow event is written.
        let reporter = {
            let bus = bus.clone();
            let plugin = job.plugin.clone();
            let action = job.action.as_str().to_owned();
            tokio::spawn(async move {
                while let Some((line, dropped)) = rx.recv().await {
                    let _ = bus
                        .publish(EventKind::CliInstallOutput {
                            plugin: plugin.clone(),
                            action: action.clone(),
                            line,
                            dropped,
                        })
                        .await;
                }
            })
        };
        let outcome = install::run(&job.argv, |line, dropped| {
            let _ = tx.send((line, dropped));
        })
        .await;
        drop(tx);
        let _ = reporter.await;

        // Whatever the command said, the machine decides: look again, and ask the program
        // its version before reporting, so a client that reads /v1/plugins right after the
        // event sees both.
        driver.refresh_cli();
        driver.probe_cli().await;
        let status = driver.cli_status().await;
        let found = status
            .as_ref()
            .is_some_and(|status| status.located.is_some());

        let duration_ms = started.elapsed().as_millis() as u64;
        let (ok, reason) = verdict(&outcome, found, &driver.cli_places());

        let _ = bus
            .publish(EventKind::CliInstallFinished {
                plugin: job.plugin.clone(),
                action: job.action.as_str().to_owned(),
                ok,
                code: outcome.code,
                duration_ms,
                reason,
                dropped: outcome.dropped,
            })
            .await;
        let _ = bus
            .publish(EventKind::PluginChanged {
                name: job.plugin.clone(),
            })
            .await;
        self.release(&job.plugin).await;
    }
}

/// What the stream says after the command: success is what the lookup found, not the exit
/// code alone. A command that exits 0 but leaves no program behind is a failure, and the
/// reason lists everywhere the description looked.
fn verdict(outcome: &install::Run, found: bool, places: &[String]) -> (bool, Option<String>) {
    if outcome.timed_out {
        return (
            false,
            Some(format!(
                "it did not finish within {} minutes and was stopped with its process tree",
                install::TIMEOUT.as_secs() / 60
            )),
        );
    }
    if outcome.not_started {
        return (
            false,
            Some("the command could not be started; the output says why".to_owned()),
        );
    }
    if outcome.code != Some(0) {
        return (
            false,
            Some(match outcome.code {
                Some(code) => format!("it exited with code {code}"),
                None => "it was stopped before it exited".to_owned(),
            }),
        );
    }
    if !found {
        return (
            false,
            Some(format!(
                "the command succeeded, but the hub cannot find the installed program \
                 afterwards; it looked in: {}",
                places.join("; ")
            )),
        );
    }
    (true, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_job_per_plugin() {
        let installer = Installer::new();
        assert_eq!(installer.installing("claude").await, None);
        installer.claim("claude", Action::Install).await.unwrap();
        assert_eq!(installer.installing("claude").await, Some(Action::Install));
        let err = installer
            .claim("claude", Action::Upgrade)
            .await
            .expect_err("already running");
        assert!(matches!(err, CoreError::Conflict(_)), "{err:?}");
        // The conflict says what is running, and that is an install.
        assert!(err.to_string().contains("installed"), "{err}");
        // Another plugin is not held up by it.
        installer.claim("codex", Action::Install).await.unwrap();
        installer.release("claude").await;
        assert_eq!(installer.installing("claude").await, None);
        assert_eq!(installer.installing("codex").await, Some(Action::Install));
        installer.claim("claude", Action::Upgrade).await.unwrap();
        assert_eq!(installer.installing("claude").await, Some(Action::Upgrade));
    }

    #[test]
    fn a_command_that_leaves_nothing_behind_is_a_failure() {
        let exited_ok = install::Run {
            code: Some(0),
            ..install::Run::default()
        };
        assert_eq!(verdict(&exited_ok, true, &[]), (true, None));

        let (ok, reason) = verdict(
            &exited_ok,
            false,
            &[
                "PATH: demo (demo)".to_owned(),
                "~/.local/bin/demo".to_owned(),
            ],
        );
        assert!(!ok);
        let reason = reason.expect("a reason");
        assert!(reason.contains("looked in"), "{reason}");
        assert!(reason.contains("~/.local/bin/demo"), "{reason}");

        let failed = install::Run {
            code: Some(3),
            ..install::Run::default()
        };
        let (ok, reason) = verdict(&failed, true, &[]);
        assert!(!ok);
        assert!(reason.unwrap().contains("code 3"));

        let timed_out = install::Run {
            timed_out: true,
            ..install::Run::default()
        };
        assert!(!verdict(&timed_out, true, &[]).0);

        let not_started = install::Run {
            not_started: true,
            ..install::Run::default()
        };
        assert!(!verdict(&not_started, false, &[]).0);
    }
}

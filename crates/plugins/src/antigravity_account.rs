//! Antigravity CLI accounts (docs/research/antigravity-cli.md). The CLI signs in with a
//! Google account and keeps the credential in the OS keyring — not under `~/.gemini` — so a
//! machine has one Antigravity login: another account's home does not change who is signed
//! in, and `HOME` isolation cannot separate two of them.
//!
//! Signing in is `crossagents agy-login`: `agy acp --login` prints a Google link and reads
//! the pasted code from its stdin, so the helper serves a page for exactly that. The hub
//! hands the page's address to the GUI as the login page and watches the account until the
//! CLI reports a sign-in.
//!
//! Quota and plan come from the CLI's own headless `/usage` and `/credits` reports,
//! without reading or exporting the keyring credential. There are no
//! session files with token counts — per-turn tokens come from the driver's event stream.

use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, Credits, DataSource, LimitGroup,
    LoginCommand, ModelInfo, RateWindow, SessionUsage,
};
#[cfg(not(windows))]
use crossagents_drivers::antigravity_native::prepare_query;
use crossagents_drivers::antigravity_native::wire::parse_models;
use crossagents_drivers::locate::Cli;
#[cfg(not(windows))]
use crossagents_drivers::process::apply_proxy_env;
use crossagents_drivers::process::{explicit_proxy_env, resolve_program};
use serde_json::Value;

/// Live status is refreshed at most this often.
const LIVE_TTL: Duration = Duration::from_secs(60);
/// `agy models` that takes longer is treated as failed.
const CLI_TIMEOUT: Duration = Duration::from_secs(20);

const NOT_INSTALLED: &str =
    "the Antigravity CLI is not installed (irm https://antigravity.google/cli/install.ps1 | iex)";

const SIGN_IN_NOTE: &str =
    "还没有登录：点「登录」会打开一个本地页面，里面有 Google 授权链接，把授权码粘进去就完成";

enum CliLookup {
    /// Look up the CLI on demand so an install while the hub runs is picked up.
    Live(Option<Cli>),
    /// A fixed command (or `None` to simulate an uninstalled CLI) for tests.
    Fixed(Option<Vec<String>>),
}

pub struct AntigravityAccounts {
    /// How to run the CLI.
    cli: CliLookup,
    /// How to start the login helper (`<crossagents> agy-login`), when the hub has one.
    login: Option<Vec<String>>,
    /// The description's model list, for when the CLI cannot be asked.
    models: Vec<String>,
    live: Mutex<Option<(Instant, String, AccountStatus)>>,
    model_epoch: std::sync::atomic::AtomicU64,
    model_cache: tokio::sync::Mutex<Option<(Instant, String, Vec<ModelInfo>)>>,
}

impl Default for AntigravityAccounts {
    fn default() -> Self {
        Self::with_login(None)
    }
}

impl AntigravityAccounts {
    pub fn new(cli: Option<Vec<String>>, login: Option<Vec<String>>, models: Vec<String>) -> Self {
        Self {
            cli: CliLookup::Fixed(cli),
            login,
            models,
            live: Mutex::default(),
            model_epoch: std::sync::atomic::AtomicU64::new(0),
            model_cache: tokio::sync::Mutex::default(),
        }
    }

    /// Uses the built-in Antigravity description, resolving the CLI on demand so a CLI
    /// installed after the hub started is found without restarting the hub.
    pub fn with_login(login: Option<Vec<String>>) -> Self {
        Self {
            cli: CliLookup::Live(crate::builtin::cli("antigravity")),
            login,
            models: crate::builtin::config("antigravity")
                .map(|config| config.models.clone())
                .unwrap_or_default(),
            live: Mutex::default(),
            model_epoch: std::sync::atomic::AtomicU64::new(0),
            model_cache: tokio::sync::Mutex::default(),
        }
    }

    fn command(&self) -> Option<Vec<String>> {
        match &self.cli {
            CliLookup::Live(cli) => cli.as_ref().and_then(Cli::command),
            CliLookup::Fixed(cli) => cli.clone(),
        }
    }

    fn cache_key(&self, account: &Account) -> String {
        format!(
            "{}:{}",
            self.model_epoch.load(std::sync::atomic::Ordering::SeqCst),
            account_key(account)
        )
    }

    /// Captures a bounded CLI query. `models` may exit 1 when signed out; headless
    /// reports may return a structured error envelope on a non-zero exit.
    async fn run(&self, account: &Account, args: &[&str]) -> Result<(String, String), String> {
        let Some(command) = self.command() else {
            return Err(NOT_INSTALLED.into());
        };
        let (program, rest) = command
            .split_first()
            .ok_or_else(|| "empty Antigravity command".to_owned())?;
        #[cfg(windows)]
        let (stdout, stderr, success) = {
            let mut all_args: Vec<String> = rest.to_vec();
            all_args.extend(args.iter().map(|s| (*s).to_owned()));
            let mut env = explicit_proxy_env(account.proxy.as_deref());
            env.push(("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()));
            let mut launched = crossagents_drivers::launch::launch(
                crossagents_drivers::launch::LaunchSpec {
                    program: resolve_program(program),
                    args: all_args,
                    cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                    env,
                    label: format!("agy {}", args.join(" ")),
                    ..crossagents_drivers::launch::LaunchSpec::default()
                },
            )
            .await
            .map_err(|e| format!("cannot run the Antigravity CLI: {e}"))?;
            drop(launched.stdin);
            let mut out_buf = Vec::new();
            let read_res = tokio::time::timeout(CLI_TIMEOUT, async {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut launched.stdout, &mut out_buf)
                    .await;
                launched.process.wait().await
            })
            .await;
            let exit = match read_res {
                Ok(exit) => exit,
                Err(_) => {
                    launched.process.start_kill();
                    return Err(format!(
                        "`agy {}` did not answer in {} s",
                        args.join(" "),
                        CLI_TIMEOUT.as_secs()
                    ));
                }
            };
            let stderr = launched.stderr.text().await;
            let stdout = String::from_utf8_lossy(&out_buf).into_owned();
            (stdout, stderr, exit.and_then(|info| info.code) == Some(0))
        };
        #[cfg(not(windows))]
        let (stdout, stderr, success) = {
            let mut child = tokio::process::Command::new(resolve_program(program));
            child
                .args(rest)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            apply_proxy_env(&mut child, account.proxy.as_deref());
            prepare_query(&mut child);
            let output = tokio::time::timeout(CLI_TIMEOUT, child.output())
                .await
                .map_err(|_| {
                    format!(
                        "`agy {}` did not answer in {} s",
                        args.join(" "),
                        CLI_TIMEOUT.as_secs()
                    )
                })?
                .map_err(|e| format!("cannot run the Antigravity CLI: {e}"))?;
            (
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
                output.status.success(),
            )
        };
        let text = format!("{stdout}\n{stderr}");
        if !success
            && !text.to_ascii_lowercase().contains("sign in")
            && args == ["models"]
        {
            return Err(format!("Antigravity model query failed: {}", text.trim()));
        }
        Ok((stdout, stderr))
    }

    /// Whether the CLI is signed in, and what it lists. Signed out is the CLI saying so:
    /// `agy models` answers `Please sign in to view available models.`
    async fn probe(&self, account: &Account) -> (bool, Vec<ModelInfo>, Option<String>) {
        let (stdout, stderr) = match self.run(account, &["models"]).await {
            Ok(answer) => answer,
            Err(err) => return (false, Vec::new(), Some(err)),
        };
        let text = format!("{stdout}\n{stderr}");
        let signed_out = text.to_ascii_lowercase().contains("sign in");
        let models: Vec<ModelInfo> = parse_models(&stdout)
            .into_iter()
            .map(|id| ModelInfo {
                context_window: crossagents_drivers::antigravity_native::context_window_for(Some(
                    &id,
                )),
                name: id.clone(),
                id,
                description: None,
                credit_multiplier: None,
                efforts: None,
            })
            .collect();
        let note = if signed_out {
            Some(SIGN_IN_NOTE.to_owned())
        } else {
            // Not signed out and nothing listed: the CLI's own line says why (a network
            // failure, a model list it could not fetch).
            text.lines()
                .find(|line| line.trim_start().starts_with("Error:"))
                .map(|line| line.trim().to_owned())
        };
        let note = note.or_else(|| {
            models
                .is_empty()
                .then(|| format!("Antigravity did not return a model list: {}", text.trim()))
        });
        (!signed_out && !models.is_empty(), models, note)
    }

    async fn report(&self, account: &Account, command: &str) -> Result<Value, String> {
        let (stdout, stderr) = self
            .run(account, &["-p", command, "--output-format", "json"])
            .await?;
        let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
            if stderr.trim().is_empty() {
                format!("Antigravity {command}: unexpected report")
            } else {
                format!("Antigravity {command}: {}", stderr.trim())
            }
        })?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(format!("Antigravity {command}: {error}"));
        }
        Ok(value)
    }
}

#[async_trait]
impl AccountProvider for AntigravityAccounts {
    fn name(&self) -> &str {
        "antigravity"
    }

    fn default_home(&self) -> Option<PathBuf> {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home))
    }

    async fn status(&self, account: &Account) -> AccountStatus {
        let request_key = self.cache_key(account);
        if let Some((at, key, status)) = self.live.lock().unwrap().as_ref()
            && *key == request_key
            && at.elapsed() < LIVE_TTL
            && !(status.note.as_deref() == Some(NOT_INSTALLED) && self.command().is_some())
        {
            return status.clone();
        }
        let (logged_in, _, note) = self.probe(account).await;
        let mut status = AccountStatus {
            logged_in,
            auth_mode: logged_in.then(|| "google".to_owned()),
            // The credential is in the keyring and the CLI never prints the address.
            email: None,
            plan: None,
            subscription_until: None,
            rate_limits: Vec::new(),
            limit_reached: false,
            credits: None,
            limits: Vec::new(),
            reset_credits: Vec::new(),
            source: None,
            as_of: None,
            note,
        };
        if logged_in {
            let (quota, credits) = tokio::join!(
                self.report(account, "/usage"),
                self.report(account, "/credits")
            );
            let mut notes = Vec::new();
            match quota.and_then(|value| apply_quota(&mut status, &value)) {
                Ok(()) => {}
                Err(error) => notes.push(error),
            }
            match credits.and_then(|value| apply_credits(&mut status, &value)) {
                Ok(()) => {}
                Err(error) => notes.push(error),
            }
            status.note = (!notes.is_empty()).then(|| notes.join("\n"));
        }
        status.note = status.note.map(eligibility_note);
        *self.live.lock().unwrap() = Some((Instant::now(), request_key, status.clone()));
        status
    }

    /// Nothing to count locally: `agy` keeps conversations in its own store without token
    /// counts. The driver reports each turn's usage from the event stream instead.
    async fn sessions(&self, _account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
        Ok(Vec::new())
    }

    async fn models(&self, account: &Account) -> Vec<ModelInfo> {
        // Model discovery never runs headless /usage or /credits reports. Concurrent
        // pickers share a query, scoped to the account and its proxy configuration.
        let mut cache = self.model_cache.lock().await;
        let key = self.cache_key(account);
        if let Some((at, cached_key, models)) = cache.as_ref()
            && *cached_key == key
            && at.elapsed() < LIVE_TTL
        {
            return models.clone();
        }
        let (_, models, _) = self.probe(account).await;
        if !models.is_empty() {
            *cache = Some((Instant::now(), key, models.clone()));
            return models;
        }
        // The description's list stands in when the CLI listed nothing (signed out).
        let models = self
            .models
            .iter()
            .map(|id| ModelInfo {
                name: id.clone(),
                id: id.clone(),
                description: None,
                context_window: None,
                credit_multiplier: None,
                efforts: None,
            })
            .collect::<Vec<_>>();
        *cache = Some((Instant::now(), key, models.clone()));
        models
    }

    /// `<crossagents> agy-login --command <agy> --port <n>`: the helper serves the page the
    /// GUI opens, and writes the pasted code into `agy acp --login`. The hub watches the
    /// account until the CLI reports a sign-in.
    fn login_command(&self, account: &Account) -> LoginCommand {
        let port = free_port().unwrap_or(8323);
        let mut command = self.login.clone().unwrap_or_else(|| {
            vec![
                std::env::current_exe()
                    .map(|exe| exe.display().to_string())
                    .unwrap_or_else(|_| "crossagents".into()),
                "agy-login".into(),
            ]
        });
        // `--command` takes everything after it, so the port goes first.
        command.push("--port".into());
        command.push(port.to_string());
        command.push("--command".into());
        match self.command() {
            Some(cli) => command.extend(cli),
            None => command.push("agy".into()),
        }
        let proxy_env = explicit_proxy_env(account.proxy.as_deref());
        let env_remove = proxy_env
            .iter()
            .filter(|(_, v)| v.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        let env = proxy_env
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        LoginCommand {
            command,
            env,
            env_remove,
            serves: Some(format!("http://127.0.0.1:{port}/")),
            opens_browser: false,
        }
    }

    fn forget_status(&self, _account: &Account) {
        *self.live.lock().unwrap() = None;
        // A login or plugin change invalidates both query paths, including a running read.
        self.model_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// The credential is in the OS keyring, not in the account's home, so only an explicit
    /// proxy override is carried in the environment.
    fn agent_env(&self, account: &Account) -> Vec<(String, String)> {
        explicit_proxy_env(account.proxy.as_deref())
    }
}

fn account_key(account: &Account) -> String {
    serde_json::to_string(account).unwrap()
}

fn eligibility_note(original: String) -> String {
    let lower = original.to_ascii_lowercase();
    if !(lower.contains("eligibility")
        || lower.contains("ineligible")
        || lower.contains("not eligible"))
    {
        return original;
    }
    format!(
        "{original}\nAntigravity 资格由 Google 判定；美国代理出口不保证账号地区资格。请登录 https://policies.google.com/terms 查看账号关联国家；若有误，可通过 https://policies.google.com/country-association-form 申请更正。年龄及账号类型也可能影响资格。此提示未验证账号地区，也不代表拒绝已解决。官方 FAQ：https://antigravity.google/docs/faq"
    )
}

/// Unwraps `agy -p /usage --output-format json` / `/credits` printmode envelopes
/// (`{"status":"SUCCESS","command":{"name":"usage","data":{...}}}`), while still accepting a
/// bare payload object directly.
fn command_data(value: &Value) -> &Value {
    value.pointer("/command/data").unwrap_or(value)
}

/// Maps `agy`'s explicit bucket `window` token (`"5h"`, `"weekly"`, …) to seconds without
/// guessing from reset timestamps.
fn bucket_window_seconds(bucket: &Value) -> Option<u64> {
    match bucket.get("window").and_then(Value::as_str) {
        Some("5h") => Some(5 * 3600),
        Some("weekly") => Some(7 * 24 * 3600),
        Some("daily" | "24h") => Some(24 * 3600),
        _ => None,
    }
}

/// The CLI's printmode quotaJSON -> groupJSON -> bucketJSON. Missing numeric values
/// remain unknown; an amount without a total cannot be turned into a percentage.
fn apply_quota(status: &mut AccountStatus, value: &Value) -> Result<(), String> {
    let data = command_data(value);
    let groups = data
        .get("groups")
        .and_then(Value::as_array)
        .ok_or("Antigravity /usage: no quota groups in report")?;
    for (index, group) in groups.iter().enumerate() {
        let name = group
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Antigravity");
        let mut windows = Vec::new();
        for bucket in group
            .get("buckets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(fraction) = bucket
                .get("remaining_fraction")
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
            else {
                continue;
            };
            let label = bucket
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| bucket.get("window").and_then(Value::as_str))
                .unwrap_or(name);
            windows.push(RateWindow {
                name: label.to_owned(),
                used_percent: (1.0 - fraction) * 100.0,
                window_seconds: bucket_window_seconds(bucket),
                resets_at: bucket
                    .get("reset_time")
                    .and_then(Value::as_str)
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|at| at.with_timezone(&Utc)),
                remaining: None,
                total: None,
            });
        }
        if windows.is_empty() {
            continue;
        }
        let reached = windows.iter().any(|window| window.used_percent >= 100.0);
        status.limits.push(LimitGroup {
            id: format!("antigravity-{index}"),
            name: Some(name.to_owned()),
            // Groups cover several model families; their display names are not model IDs.
            model: None,
            windows,
            reached,
        });
    }
    // The summary shows the most constrained group, while details retain every group.
    if let Some(group) = status.limits.iter().max_by(|a, b| {
        let fullest = |group: &LimitGroup| {
            group
                .windows
                .iter()
                .map(|w| w.used_percent)
                .fold(0.0, f64::max)
        };
        fullest(a).total_cmp(&fullest(b))
    }) {
        status.rate_limits = group.windows.clone();
    }
    // A depleted model group does not exhaust the account's other groups.
    status.limit_reached = !status.limits.is_empty() && status.limits.iter().all(|g| g.reached);
    if status.limits.is_empty() {
        return Err("Antigravity /usage: no supported quota windows in report".into());
    }
    status.source = Some(DataSource::Live);
    status.as_of = Some(Utc::now());
    Ok(())
}

fn apply_credits(status: &mut AccountStatus, value: &Value) -> Result<(), String> {
    let data = command_data(value);
    status.plan = data
        .get("plan_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let balance = data
        .get("remaining_credits")
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or("Antigravity /credits: no credit balance in report")?;
    status.credits = Some(Credits {
        has_credits: balance > 0.0,
        unlimited: false,
        balance: Some(balance.to_string()),
    });
    Ok(())
}

/// A port that was free a moment ago.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    Some(listener.local_addr().ok()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn model_listing_does_not_run_quota_reports() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("slow-reports.mjs");
        let calls = dir.path().join("calls.txt");
        std::fs::write(
            &script,
            format!(
                r#"
import {{ appendFileSync }} from 'node:fs';
appendFileSync({:?}, process.argv.slice(2).join(' ') + '\n');
if (process.argv[2] === 'models') console.log('gemini-3.8-flash-high');
else {{ await new Promise(r => setTimeout(r, 800)); console.log('{{}}'); }}
"#,
                calls.display().to_string()
            ),
        )
        .unwrap();
        let provider = AntigravityAccounts::new(
            Some(vec!["node".into(), script.display().to_string()]),
            None,
            Vec::new(),
        );
        let started = Instant::now();
        let models = provider.models(&account(dir.path())).await;
        eprintln!("model listing elapsed: {:?}", started.elapsed());
        assert_eq!(models[0].id, "gemini-3.8-flash-high");
        assert_eq!(std::fs::read_to_string(calls).unwrap(), "models\n");
    }

    fn account(home: &std::path::Path) -> Account {
        Account {
            codebuddy_site: None,
            egress: None,
            proxy: None,
            id: "antigravity-main".into(),
            provider: "antigravity".into(),
            label: "Antigravity".into(),
            home: home.to_path_buf(),
            created_at: Utc::now(),
        }
    }

    /// A stand-in for `agy models`.
    fn fake_cli(answer: &str) -> (Vec<String>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-agy.mjs");
        std::fs::write(&script, format!(r#"
const args = process.argv.slice(2);
if (args[0] === 'models') process.stdout.write({answer:?});
else if (args[1] === '/usage') process.stdout.write(JSON.stringify({{groups:[{{name:'Gemini',buckets:[{{name:'5 hours',remaining_fraction:0.75,reset_time:'2026-11-17T00:00:00Z'}}]}}]}}));
else if (args[1] === '/credits') process.stdout.write(JSON.stringify({{remaining_credits:0,plan_name:'Google AI Pro'}}));
else process.exit(1);
"#)).unwrap();
        (vec!["node".into(), script.display().to_string()], dir)
    }

    #[tokio::test]
    async fn a_signed_out_cli_says_so_and_offers_the_login() {
        let (cli, _dir) = fake_cli("Error: Please sign in to view available models.");
        let home = tempfile::tempdir().unwrap();
        let accounts = AntigravityAccounts::new(Some(cli), None, Vec::new());
        let status = accounts.status(&account(home.path())).await;
        assert!(!status.logged_in);
        assert!(status.auth_mode.is_none());
        assert_eq!(status.note.as_deref(), Some(SIGN_IN_NOTE));
        assert!(accounts.models(&account(home.path())).await.is_empty());
    }

    #[tokio::test]
    async fn a_listing_is_a_signed_in_account_with_models() {
        let (cli, _dir) = fake_cli(
            "Gemini 3.8 Flash (High)   gemini-3.8-flash-high\n\
             Gemini 3.1 Pro (High)     gemini-3.1-pro-high\n\
             auto\n",
        );
        let home = tempfile::tempdir().unwrap();
        let accounts = AntigravityAccounts::new(Some(cli), None, Vec::new());
        let status = accounts.status(&account(home.path())).await;
        assert!(status.logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("google"));
        assert!(status.note.is_none());
        assert_eq!(status.plan.as_deref(), Some("Google AI Pro"));
        assert_eq!(status.rate_limits[0].used_percent, 25.0);
        assert_eq!(
            status.credits.as_ref().unwrap().balance.as_deref(),
            Some("0")
        );
        assert_eq!(status.source, Some(DataSource::Live));
        let models: Vec<_> = accounts
            .models(&account(home.path()))
            .await
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            models,
            ["gemini-3.8-flash-high", "gemini-3.1-pro-high", "auto"]
        );
    }

    #[tokio::test]
    async fn a_cli_that_is_not_installed_is_not_a_signed_in_account() {
        let home = tempfile::tempdir().unwrap();
        let accounts = AntigravityAccounts::new(None, None, vec!["gemini-3.8-flash".into()]);
        let status = accounts.status(&account(home.path())).await;
        assert!(!status.logged_in);
        assert_eq!(status.note.as_deref(), Some(NOT_INSTALLED));
        // The description's list stands in when the CLI cannot be asked.
        let models: Vec<_> = accounts
            .models(&account(home.path()))
            .await
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(models, ["gemini-3.8-flash"]);
    }

    #[tokio::test]
    async fn the_login_runs_the_helper_against_the_cli_and_a_local_page() {
        let home = tempfile::tempdir().unwrap();
        let accounts = AntigravityAccounts::new(
            Some(vec!["agy".into()]),
            Some(vec!["crossagents".into(), "agy-login".into()]),
            Vec::new(),
        );
        let login = accounts.login_command(&account(home.path()));
        assert_eq!(login.command[..2], ["crossagents", "agy-login"]);
        assert_eq!(login.command[2], "--port");
        let port: u16 = login.command[3].parse().expect("a port");
        assert_eq!(login.command[4..], ["--command", "agy"]);
        assert_eq!(
            login.serves.as_deref(),
            Some(format!("http://127.0.0.1:{port}/").as_str())
        );
        assert!(!login.opens_browser);
        assert!(login.env.is_empty() && login.env_remove.is_empty());
    }

    #[test]
    fn the_default_home_is_the_machines() {
        let accounts = AntigravityAccounts::default();
        let home = accounts.default_home().expect("a home");
        assert!(home.is_absolute());
    }

    #[tokio::test]
    async fn a_network_error_is_not_a_login() {
        let (cli, _dir) = fake_cli("Error: network unavailable");
        let accounts = AntigravityAccounts::new(Some(cli), None, Vec::new());
        let home = tempfile::tempdir().unwrap();
        let status = accounts.status(&account(home.path())).await;
        assert!(!status.logged_in);
        assert_eq!(status.note.as_deref(), Some("Error: network unavailable"));
    }

    #[tokio::test]
    async fn a_report_refusal_keeps_the_login_but_shows_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("refused.mjs");
        std::fs::write(&script, r#"
if (process.argv[2] === 'models') console.log('gemini-3.8-flash-high');
else { console.log(JSON.stringify({status:'ERROR',error:'Eligibility check failed: account unavailable'})); process.exitCode=1; }
"#).unwrap();
        let accounts = AntigravityAccounts::new(
            Some(vec!["node".into(), script.display().to_string()]),
            None,
            Vec::new(),
        );
        let status = accounts.status(&account(dir.path())).await;
        assert!(status.logged_in);
        assert!(status.rate_limits.is_empty());
        assert!(status.credits.is_none());
        assert!(status.source.is_none() && status.as_of.is_none());
        let note = status.note.unwrap();
        assert!(
            note.starts_with("Antigravity /usage: Eligibility check failed: account unavailable")
        );
        assert!(
            note.contains("Antigravity /credits: Eligibility check failed: account unavailable")
        );
        assert!(note.contains("美国代理出口不保证账号地区资格"));
        assert!(note.contains("https://policies.google.com/terms"));
        assert!(note.contains("https://policies.google.com/country-association-form"));
        assert!(note.contains("未验证账号地区"));
    }

    #[test]
    fn unrelated_errors_are_not_rewritten_as_eligibility_failures() {
        assert_eq!(
            eligibility_note("Error: network unavailable".into()),
            "Error: network unavailable"
        );
    }

    #[tokio::test]
    async fn model_cache_is_coalesced_and_invalidated_by_account_or_login_changes() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("models.mjs");
        let calls = dir.path().join("calls.txt");
        std::fs::write(
            &script,
            format!(
                r#"
import {{ appendFileSync }} from 'node:fs';
appendFileSync({:?}, 'models\n');
console.log('gemini-3.8-flash-high');
"#,
                calls.display().to_string()
            ),
        )
        .unwrap();
        let provider = AntigravityAccounts::new(
            Some(vec!["node".into(), script.display().to_string()]),
            None,
            Vec::new(),
        );
        let mut first = account(dir.path());
        tokio::join!(provider.models(&first), provider.models(&first));
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        first.proxy = Some("direct".into());
        provider.models(&first).await;
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 2);
        provider.forget_status(&first);
        provider.models(&first).await;
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 3);
    }

    #[tokio::test]
    async fn quota_groups_preserve_unknowns_and_only_all_depleted_means_exhausted() {
        let accounts = AntigravityAccounts::new(None, None, Vec::new());
        let home = tempfile::tempdir().unwrap();
        let mut status = accounts.status(&account(home.path())).await;
        apply_quota(&mut status, &serde_json::json!({"groups":[
            {"name":"Gemini", "buckets":[
                {"name":"5 hours", "remaining_fraction":0.0, "reset_time":"2026-11-17T00:00:00Z"},
                {"name":"Weekly", "remaining_fraction":0.2},
                {"name":"Unknown"},
                {"name":"Disabled", "remaining_fraction":0.0,"disabled":true},
                {"name":"Invalid", "remaining_fraction":2.0}
            ]},
            {"name":"Claude and GPT", "buckets":[{"name":"Weekly","remaining_fraction":1.0}]}
        ]})).unwrap();
        assert_eq!(status.limits.len(), 2);
        assert_eq!(status.limits[0].windows.len(), 2);
        assert!(status.limits[0].reached);
        assert!(!status.limit_reached);
        assert!(status.rate_limits[0].resets_at.is_some());
        assert_eq!(status.rate_limits[0].window_seconds, None);
        assert!(apply_credits(&mut status, &serde_json::json!({})).is_err());
        assert!(status.credits.is_none());
    }

    #[tokio::test]
    async fn a_cli_installed_after_start_is_picked_up_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let bin = if cfg!(windows) {
            dir.path().join("agy-late.cmd")
        } else {
            dir.path().join("agy-late")
        };
        let toml = format!(
            "[ paths ]\n{} = [{:?}]\n",
            crossagents_drivers::locate::os_name(),
            bin.display().to_string()
        );
        let locator: crossagents_drivers::locate::Locator = toml::from_str(&toml).unwrap();
        let cli = Cli::new(Vec::new(), locator, Default::default());
        let accounts = AntigravityAccounts {
            cli: CliLookup::Live(Some(cli)),
            login: Some(vec!["crossagents".into(), "agy-login".into()]),
            models: Vec::new(),
            live: Mutex::default(),
            model_epoch: std::sync::atomic::AtomicU64::new(0),
            model_cache: tokio::sync::Mutex::default(),
        };

        // Before the CLI is installed, status reports NOT_INSTALLED.
        let status = accounts.status(&account(dir.path())).await;
        assert_eq!(status.note.as_deref(), Some(NOT_INSTALLED));

        // Once installed while the hub is running, login_command and status find it at once.
        #[cfg(windows)]
        std::fs::write(
            &bin,
            "@echo off\r\necho Error: Please sign in to view available models.\r\nexit /b 1\r\n",
        )
        .unwrap();
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(
                &bin,
                "#!/bin/sh\necho 'Error: Please sign in to view available models.'\nexit 1\n",
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let login = accounts.login_command(&account(dir.path()));
        assert_eq!(
            login.command[4..],
            ["--command", &bin.display().to_string()]
        );
        let status = accounts.status(&account(dir.path())).await;
        assert_eq!(status.note.as_deref(), Some(SIGN_IN_NOTE));
    }

    #[test]
    fn parses_real_printmode_command_data_envelope() {
        let mut status = AccountStatus {
            logged_in: true,
            auth_mode: Some("google".into()),
            email: None,
            plan: None,
            subscription_until: None,
            rate_limits: Vec::new(),
            limit_reached: false,
            credits: None,
            limits: Vec::new(),
            reset_credits: Vec::new(),
            source: None,
            as_of: None,
            note: None,
        };
        let usage = serde_json::json!({
            "conversation_id": "",
            "status": "SUCCESS",
            "command": {
                "name": "usage",
                "data": {
                    "groups": [
                        {
                            "name": "Gemini Models",
                            "buckets": [
                                {
                                    "id": "gemini-weekly",
                                    "name": "Weekly Limit Remaining",
                                    "window": "weekly",
                                    "remaining_fraction": 0.92,
                                    "reset_time": "2026-12-01T14:31:34Z"
                                },
                                {
                                    "id": "gemini-5h",
                                    "name": "Five Hour Limit Remaining",
                                    "window": "5h",
                                    "remaining_fraction": 0.70,
                                    "reset_time": "2026-11-24T19:31:34Z"
                                }
                            ]
                        },
                        {
                            "name": "Claude and GPT models",
                            "buckets": [
                                {
                                    "id": "3p-weekly",
                                    "name": "Weekly Limit Remaining",
                                    "window": "weekly",
                                    "remaining_fraction": 1.0,
                                    "reset_time": "2026-12-01T17:47:09Z"
                                }
                            ]
                        }
                    ]
                }
            }
        });
        let credits = serde_json::json!({
            "conversation_id": "",
            "status": "SUCCESS",
            "command": {
                "name": "credits",
                "data": {
                    "remaining_credits": 0,
                    "upgrade_uri": "https://antigravity.google/g1-upgrade"
                }
            }
        });

        apply_quota(&mut status, &usage).unwrap();
        apply_credits(&mut status, &credits).unwrap();
        assert_eq!(status.limits.len(), 2);
        assert_eq!(status.limits[0].name.as_deref(), Some("Gemini Models"));
        assert_eq!(status.limits[0].windows[0].window_seconds, Some(7 * 24 * 3600));
        assert_eq!(status.limits[0].windows[1].window_seconds, Some(5 * 3600));
        assert!((status.limits[0].windows[1].used_percent - 30.0).abs() < 1e-6);
        assert_eq!(status.rate_limits.len(), 2);
        assert_eq!(status.credits.as_ref().unwrap().balance.as_deref(), Some("0"));
        assert_eq!(status.source, Some(DataSource::Live));
    }
}

//! Account management (docs/tasks/M5.md): accounts, live status, token usage and login.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, Utc};
use crossagents_api_types::{
    AccountUsageGroup, AccountUsageResponse, AccountUsageRow, CreateAccountRequest, LoginStarted,
};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, Agent, AgentId, CoreError, EventBus, EventKind, Role,
    RunId, Store, TokenTotals, validate_agent_name,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc};

const ORIGIN_CROSSAGENTS: &str = "crossagents";
const LOGIN_URL_WAIT: Duration = Duration::from_secs(20);

pub struct Accounts {
    bus: Arc<EventBus>,
    providers: BTreeMap<String, Arc<dyn AccountProvider>>,
    /// Where new accounts' homes go: `<crossagents home>/accounts`.
    root: PathBuf,
    /// Running login processes by account id.
    logins: Arc<Mutex<HashMap<String, tokio::process::Child>>>,
}

impl Accounts {
    pub fn new(
        bus: Arc<EventBus>,
        providers: Vec<Arc<dyn AccountProvider>>,
        root: PathBuf,
    ) -> Self {
        Self {
            bus,
            providers: providers
                .into_iter()
                .map(|p| (p.name().to_owned(), p))
                .collect(),
            root,
            logins: Arc::default(),
        }
    }

    fn store(&self) -> &Arc<dyn Store> {
        self.bus.store()
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    fn provider(&self, name: &str) -> Result<&Arc<dyn AccountProvider>, CoreError> {
        self.providers.get(name).ok_or_else(|| {
            CoreError::Conflict(format!(
                "no account support for {name:?} (available: {})",
                self.provider_names().join(", ")
            ))
        })
    }

    pub async fn create(&self, req: CreateAccountRequest) -> Result<Account, CoreError> {
        validate_agent_name(&req.id)?;
        let provider = self.provider(&req.provider)?;
        let home = match (req.home, req.import_default) {
            (Some(home), _) => PathBuf::from(home),
            (None, true) => provider.default_home().ok_or_else(|| {
                CoreError::Conflict(format!("{} has no default home to import", req.provider))
            })?,
            (None, false) => self.root.join(&req.id),
        };
        std::fs::create_dir_all(&home)
            .map_err(|e| CoreError::Storage(format!("cannot create {}: {e}", home.display())))?;
        let account = Account {
            label: req.label.unwrap_or_else(|| req.id.clone()),
            id: req.id,
            provider: req.provider,
            home,
            created_at: Utc::now(),
        };
        self.store().insert_account(&account).await?;
        self.bus
            .publish(EventKind::AccountCreated {
                account: account.clone(),
            })
            .await?;
        Ok(account)
    }

    /// Forgets the account. Its files stay on disk (they may be the user's own `~/.codex`).
    pub async fn remove(&self, id: &str) -> Result<(), CoreError> {
        self.store().delete_account(id).await?;
        if let Some(mut child) = self.logins.lock().await.remove(id) {
            let _ = child.start_kill();
        }
        self.bus
            .publish(EventKind::AccountRemoved { id: id.to_owned() })
            .await?;
        Ok(())
    }

    pub async fn status(&self, id: &str) -> Result<AccountStatus, CoreError> {
        let account = self.store().get_account(id).await?;
        Ok(self.provider(&account.provider)?.status(&account).await)
    }

    pub async fn models(&self, id: &str) -> Result<Vec<crossagents_core::ModelInfo>, CoreError> {
        let account = self.store().get_account(id).await?;
        Ok(self.provider(&account.provider)?.models(&account).await)
    }

    /// Environment for an agent bound to an account through `config.account`.
    pub async fn agent_env(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        let Some(id) = account_of(agent) else {
            return Ok(Vec::new());
        };
        let account = self.store().get_account(&id).await?;
        Ok(self.provider(&account.provider)?.agent_env(&account))
    }

    /// Checks that `agent` may use the account it names.
    pub async fn check_binding(
        &self,
        agent_plugin: &str,
        account_id: &str,
    ) -> Result<(), CoreError> {
        let account = self.store().get_account(account_id).await?;
        if account.provider != agent_plugin {
            return Err(CoreError::Conflict(format!(
                "account {account_id:?} is a {} account; it cannot run a {agent_plugin} agent",
                account.provider
            )));
        }
        Ok(())
    }

    pub async fn usage(
        &self,
        id: &str,
        by: AccountUsageGroup,
    ) -> Result<AccountUsageResponse, CoreError> {
        let account = self.store().get_account(id).await?;
        let sessions = self.provider(&account.provider)?.sessions(&account).await?;
        let owners = if matches!(
            by,
            AccountUsageGroup::Agent | AccountUsageGroup::Role | AccountUsageGroup::Run
        ) {
            session_owners(self.store().as_ref()).await?
        } else {
            HashMap::new()
        };
        let mut groups: BTreeMap<String, TokenTotals> = BTreeMap::new();
        let mut total = TokenTotals::default();
        for session in &sessions {
            let ours = session.originator.as_deref() == Some(ORIGIN_CROSSAGENTS);
            let owner = owners.get(&session.session_id);
            let key = match by {
                AccountUsageGroup::Day => session
                    .started_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d")
                    .to_string(),
                AccountUsageGroup::Model => {
                    session.model.clone().unwrap_or_else(|| "unknown".into())
                }
                AccountUsageGroup::Origin => if ours {
                    "crossagents"
                } else {
                    "outside CrossAgents"
                }
                .into(),
                AccountUsageGroup::Agent => owner.map_or_else(|| outside(ours), |o| o.name.clone()),
                AccountUsageGroup::Role => {
                    owner.map_or_else(|| outside(ours), |o| role_name(o.role))
                }
                AccountUsageGroup::Run => owner
                    .and_then(|o| o.run)
                    .map_or_else(|| outside(ours), |r| r.to_string()),
            };
            groups.entry(key).or_default().add(&session.totals);
            total.add(&session.totals);
        }
        let mut rows: Vec<AccountUsageRow> = groups
            .into_iter()
            .map(|(key, totals)| AccountUsageRow {
                cache_hit_rate: totals.cache_hit_rate(),
                key,
                totals,
            })
            .collect();
        if by == AccountUsageGroup::Day {
            rows.reverse(); // newest day first
        }
        Ok(AccountUsageResponse {
            by,
            rows,
            total_cache_hit_rate: total.cache_hit_rate(),
            total,
        })
    }

    /// Starts the provider's login tool for this account and returns the page to open.
    /// The outcome arrives later as `account.login_finished`.
    pub async fn login(&self, id: &str) -> Result<LoginStarted, CoreError> {
        let account = self.store().get_account(id).await?;
        let login = self.provider(&account.provider)?.login_command(&account);
        let (program, args) = login
            .command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty login command".into()))?;
        let display = login.command.join(" ");
        if let Some(mut old) = self.logins.lock().await.remove(id) {
            let _ = old.start_kill();
        }
        let mut child = tokio::process::Command::new(resolve_program(program))
            .args(args)
            .envs(&login.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| CoreError::Agent(format!("cannot start `{display}`: {e}")))?;

        // Scan both streams for the first URL; keep the tail for error reports.
        let (lines_tx, mut lines_rx) = mpsc::unbounded_channel::<String>();
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tx = lines_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        drop(lines_tx);
        self.logins.lock().await.insert(id.to_owned(), child);

        let mut url = None;
        let mut tail = Vec::new();
        let deadline = tokio::time::Instant::now() + LOGIN_URL_WAIT;
        while url.is_none() {
            match tokio::time::timeout_at(deadline, lines_rx.recv()).await {
                Ok(Some(line)) => {
                    url = find_url(&line);
                    tail.push(line);
                }
                _ => break,
            }
        }
        self.bus
            .publish(EventKind::AccountLoginPending {
                id: id.to_owned(),
                url: url.clone(),
            })
            .await?;

        // Wait for the login to finish in the background.
        let bus = self.bus.clone();
        let id_owned = id.to_owned();
        let provider = self.provider(&account.provider)?.clone();
        let logins = self.logins.clone();
        tokio::spawn(async move {
            let mut rest = Vec::new();
            while let Some(line) = lines_rx.recv().await {
                rest.push(line);
            }
            let exit = match logins.lock().await.remove(&id_owned) {
                Some(mut child) => child.wait().await.ok(),
                None => None, // replaced or cancelled
            };
            let status = provider.status(&account).await;
            let ok = exit.is_some_and(|e| e.success()) && status.logged_in;
            let message = if ok {
                None
            } else {
                let all: Vec<&str> = tail.iter().chain(rest.iter()).map(String::as_str).collect();
                Some(all[all.len().saturating_sub(5)..].join("\n"))
            };
            let _ = bus
                .publish(EventKind::AccountLoginFinished {
                    id: id_owned,
                    ok,
                    message,
                })
                .await;
        });
        Ok(LoginStarted {
            url,
            command: display,
        })
    }
}

pub fn account_of(agent: &Agent) -> Option<String> {
    agent
        .config
        .get("account")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn outside(ours: bool) -> String {
    if ours {
        "(unknown CrossAgents agent)".into()
    } else {
        "outside CrossAgents".into()
    }
}

fn role_name(role: Role) -> String {
    match role {
        Role::Planner => "planner",
        Role::Worker => "worker",
        Role::Reviewer => "reviewer",
    }
    .into()
}

/// The agent behind a vendor session, from the event log.
struct Owner {
    name: String,
    role: Role,
    run: Option<RunId>,
}

async fn session_owners(store: &dyn Store) -> Result<HashMap<String, Owner>, CoreError> {
    let mut agents: HashMap<AgentId, (String, Role)> = HashMap::new();
    let mut runs: HashMap<AgentId, RunId> = HashMap::new();
    let mut sessions: Vec<(String, AgentId, Option<RunId>)> = Vec::new();
    let mut after = 0;
    loop {
        let page = store.events_since(after, 1000).await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        for event in page {
            match event.kind {
                EventKind::AgentCreated { agent } => {
                    agents.insert(agent.id, (agent.name, agent.role));
                }
                EventKind::RunCreated { run } => {
                    runs.insert(run.planner, run.id);
                }
                EventKind::TaskStatusChanged {
                    run_id,
                    assignee: Some(agent),
                    ..
                } => {
                    runs.insert(agent, run_id);
                }
                EventKind::AgentSessionStarted { id, session_id } => {
                    sessions.push((session_id, id, None));
                }
                _ => {}
            }
        }
    }
    Ok(sessions
        .into_iter()
        .filter_map(|(session_id, agent, _)| {
            let (name, role) = agents.get(&agent)?.clone();
            Some((
                session_id,
                Owner {
                    name,
                    role,
                    run: runs.get(&agent).copied(),
                },
            ))
        })
        .collect())
}

/// The first https URL in a line of tool output. Plain http links are local callback
/// servers (e.g. `http://localhost:1455`), not the page to open.
fn find_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\x27' | '<' | '>' | ')'))
        .collect();
    Some(url.trim_end_matches(['.', ',', ';']).to_owned())
}

/// On Windows a bare program name does not find `npx.cmd`; search `PATH`.
fn resolve_program(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if !cfg!(windows) || path.extension().is_some() || path.components().count() > 1 {
        return path;
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&dirs) {
        for ext in ["exe", "cmd", "bat"] {
            let candidate: &Path = &dir.join(program).with_extension(ext);
            if candidate.is_file() {
                return candidate.to_path_buf();
            }
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_urls_in_tool_output() {
        assert_eq!(
            find_url("Open this link: https://auth.openai.com/oauth/authorize?x=1&y=2 to log in")
                .as_deref(),
            Some("https://auth.openai.com/oauth/authorize?x=1&y=2")
        );
        assert!(find_url("Starting local login server on http://localhost:1455.").is_none());
        assert_eq!(
            find_url("go to https://x.test/a.").as_deref(),
            Some("https://x.test/a")
        );
        assert!(find_url("no link here").is_none());
    }
}

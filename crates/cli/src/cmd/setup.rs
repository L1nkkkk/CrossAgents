//! `crossagents setup codex`: guided onboarding (docs/tasks/M5.md).
//!
//! hub → environment → account → login → status → models → agents → test.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use crossagents_api_types::{
    Account, AccountStatus, Agent, CreateAccountRequest, CreateAgentRequest, ListAccountsResponse,
    ListAgentsResponse, ModelInfo, Role, SendMessageRequest,
};

use super::Context;
use super::account::{login, print_models, print_status};
use crate::client::Client;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Provider {
    Codex,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    pub provider: Provider,
    /// Use this existing account.
    #[arg(long)]
    pub account: Option<String>,
    /// Project the new agents work in (default: the current directory).
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long)]
    pub planner_model: Option<String>,
    #[arg(long)]
    pub worker_model: Option<String>,
    /// Accept every default without asking.
    #[arg(long, short)]
    pub yes: bool,
    /// Skip the final test message.
    #[arg(long)]
    pub skip_test: bool,
}

struct Prompt {
    interactive: bool,
}

impl Prompt {
    fn ask(&self, question: &str, default: &str) -> Result<String> {
        if !self.interactive {
            println!("{question} [{default}]: {default}");
            return Ok(default.to_owned());
        }
        print!("{question} [{default}]: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let answer = line.trim();
        Ok(if answer.is_empty() { default } else { answer }.to_owned())
    }

    fn confirm(&self, question: &str) -> Result<bool> {
        let answer = self.ask(&format!("{question} (y/n)"), "y")?;
        Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
    }
}

fn step(n: u32, title: &str) {
    println!("\n[{n}/7] {title}");
}

fn on_path(program: &str) -> bool {
    let Some(dirs) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&dirs).any(|dir| {
        ["", ".exe", ".cmd", ".bat"]
            .iter()
            .any(|ext| dir.join(format!("{program}{ext}")).is_file())
    })
}

/// Picks a model by number or id from the list.
fn pick(models: &[ModelInfo], answer: &str) -> String {
    answer
        .parse::<usize>()
        .ok()
        .and_then(|i| models.get(i.wrapping_sub(1)))
        .map_or_else(|| answer.to_owned(), |m| m.id.clone())
}

pub async fn run(ctx: &Context, args: SetupArgs) -> Result<()> {
    let Provider::Codex = args.provider;
    let prompt = Prompt {
        interactive: !args.yes && std::io::stdin().is_terminal(),
    };
    println!("Setting up Codex for CrossAgents.");

    step(1, "Hub");
    super::hub::ensure_running(ctx).await?;
    let client = ctx.client()?;
    println!("hub is running");

    step(2, "Environment");
    if on_path("npx") {
        println!("Node.js (npx) found; the Codex adapter is downloaded on first use");
    } else {
        bail!(
            "Node.js is required (npx runs the Codex adapter). Install it from https://nodejs.org and run setup again."
        );
    }

    step(3, "Account");
    let account = choose_account(&client, &prompt, args.account.as_deref()).await?;
    println!("using account {} ({})", account.id, account.home.display());

    step(4, "Login");
    let mut status: AccountStatus = client
        .get(&format!("/v1/accounts/{}/status", account.id))
        .await?;
    if status.logged_in {
        println!("already logged in");
    } else {
        println!("this account is not logged in yet");
        if !login(&client, &account.id).await? {
            bail!(
                "login did not complete; run `crossagents account login {}` to retry",
                account.id
            );
        }
        status = client
            .get(&format!("/v1/accounts/{}/status", account.id))
            .await?;
    }
    print_status(&status);

    step(5, "Models");
    let models: Vec<ModelInfo> = client
        .get(&format!("/v1/accounts/{}/models", account.id))
        .await?;
    print_models(&models);
    let default_planner = models.first().map_or("", |m| m.id.as_str()).to_owned();
    let default_worker = models.last().map_or("", |m| m.id.as_str()).to_owned();
    if models.is_empty() && (args.planner_model.is_none() || args.worker_model.is_none()) {
        println!(
            "no model list yet; the agents will use Codex's default model (change later with --model)"
        );
    } else {
        println!("tip: give the planner the strongest model and workers a fast, cheap one");
    }
    let planner_model = match args.planner_model {
        Some(m) => m,
        None if models.is_empty() => String::new(),
        None => pick(
            &models,
            &prompt.ask("Planner model (number or id)", &default_planner)?,
        ),
    };
    let worker_model = match args.worker_model {
        Some(m) => m,
        None if models.is_empty() => String::new(),
        None => pick(
            &models,
            &prompt.ask("Worker model (number or id)", &default_worker)?,
        ),
    };

    step(6, "Agents");
    let repo = match args.repo {
        Some(repo) => repo,
        None => std::env::current_dir().context("cannot read the current directory")?,
    };
    let repo = prompt.ask(
        "Project directory for the agents",
        &repo.display().to_string(),
    )?;
    let existing: ListAgentsResponse = client.get("/v1/agents").await?;
    let mut created = Vec::new();
    for (name, role, model) in [
        (
            format!("{}-planner", account.id),
            Role::Planner,
            planner_model,
        ),
        (format!("{}-worker", account.id), Role::Worker, worker_model),
    ] {
        if existing.agents.iter().any(|a| a.name == name) {
            println!("{name} already exists; keeping it");
            continue;
        }
        let agent: Agent = client
            .post(
                "/v1/agents",
                &CreateAgentRequest {
                    name: name.clone(),
                    plugin: "codex".into(),
                    driver: None,
                    model: (!model.is_empty()).then_some(model.clone()),
                    role,
                    cwd: repo.clone(),
                    config: serde_json::json!({ "account": account.id }),
                },
            )
            .await?;
        println!(
            "created {} ({}, {})",
            agent.name,
            super::agent::enum_name(&agent.role),
            agent.model.as_deref().unwrap_or("default model")
        );
        created.push(agent.name);
    }

    step(7, "Test");
    let worker = format!("{}-worker", account.id);
    if args.skip_test || !prompt.confirm(&format!("Send {worker} a test message"))? {
        println!("skipped");
    } else {
        let resp: serde_json::Value = client
            .post(
                &format!("/v1/agents/{worker}/messages"),
                &SendMessageRequest {
                    text: "Reply with exactly: READY. Do not run any tools.".into(),
                },
            )
            .await?;
        let _ = resp;
        println!("sent; the first start downloads the Codex adapter and can take a minute");
        println!("watch it with: crossagents attach {worker}");
    }

    println!("\nDone. Next:");
    println!(
        "  crossagents account status {}      # plan and quota",
        account.id
    );
    println!("  crossagents account usage {} --by model", account.id);
    println!(
        "  crossagents run \"<what you want>\" --planner {}-planner --worker {}-worker",
        account.id, account.id
    );
    Ok(())
}

async fn choose_account(client: &Client, prompt: &Prompt, wanted: Option<&str>) -> Result<Account> {
    let list: ListAccountsResponse = client.get("/v1/accounts").await?;
    let codex: Vec<&Account> = list
        .accounts
        .iter()
        .filter(|a| a.provider == "codex")
        .collect();
    if let Some(id) = wanted {
        return codex
            .into_iter()
            .find(|a| a.id == id)
            .cloned()
            .with_context(|| format!("no codex account {id:?}; see `crossagents account ls`"));
    }
    if let Some(first) = codex.first() {
        if codex.len() > 1 {
            let ids: Vec<&str> = codex.iter().map(|a| a.id.as_str()).collect();
            println!("codex accounts: {}", ids.join(", "));
            let id = prompt.ask("Which account", &first.id)?;
            return codex
                .iter()
                .find(|a| a.id == id)
                .map(|a| (*a).clone())
                .with_context(|| format!("no codex account {id:?}"));
        }
        return Ok((*first).clone());
    }

    let default_home = default_codex_home();
    let has_login = default_home
        .as_ref()
        .is_some_and(|h| h.join("auth.json").is_file());
    let id = prompt.ask("Name for this account", "codex-main")?;
    let import = has_login
        && prompt.confirm(&format!(
            "Found an existing Codex login in {}. Use it",
            default_home
                .as_ref()
                .map(|h| h.display().to_string())
                .unwrap_or_default()
        ))?;
    let account: Account = client
        .post(
            "/v1/accounts",
            &CreateAccountRequest {
                id,
                provider: "codex".into(),
                label: None,
                home: None,
                import_default: import,
            },
        )
        .await?;
    Ok(account)
}

fn default_codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".codex"))
}

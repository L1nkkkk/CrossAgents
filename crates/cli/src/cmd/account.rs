//! `crossagents account`: vendor accounts behind agents (docs/tasks/M5.md).

use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Local, Utc};
use clap::{Args, Subcommand, ValueEnum};
use crossagents_api_types::{
    Account, AccountStatus, AccountUsageResponse, CreateAccountRequest, DataSource, EventKind,
    ListAccountsResponse, LoginStarted, ModelInfo,
};
use futures::StreamExt;

use super::Context;
use crate::client::Client;
use crate::output::{print_json, print_table};

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// List accounts.
    Ls,
    /// Add an account.
    Add(AddArgs),
    /// Forget an account (its files stay on disk).
    Rm { id: String },
    /// Login state, plan, quota and credits.
    Status { id: String },
    /// Token use and cache hits from the vendor's session logs.
    Usage {
        id: String,
        #[arg(long, value_enum, default_value_t = UsageBy::Day)]
        by: UsageBy,
    },
    /// Models this account may use.
    Models { id: String },
    /// Log the account in (opens the vendor's login page).
    Login { id: String },
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// New independent CodeBuddy site login; cannot import the machine login.
    #[arg(long, value_parser = ["china", "international"], conflicts_with = "import_default")]
    pub codebuddy_site: Option<String>,
    /// Short id, e.g. `codex-work`.
    pub id: String,
    #[arg(long, default_value = "codex")]
    pub provider: String,
    #[arg(long)]
    pub label: Option<String>,
    /// Use this directory as the account's home.
    #[arg(long, conflicts_with = "import_default")]
    pub home: Option<String>,
    /// Use the vendor's existing default home (e.g. ~/.codex) and its login.
    #[arg(long)]
    pub import_default: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum UsageBy {
    Day,
    Model,
    Origin,
    Agent,
    Role,
    Run,
}

impl UsageBy {
    fn as_query(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Model => "model",
            Self::Origin => "origin",
            Self::Agent => "agent",
            Self::Role => "role",
            Self::Run => "run",
        }
    }
}

pub async fn run(ctx: &Context, cmd: AccountCommand) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        AccountCommand::Ls => {
            let list: ListAccountsResponse = client.get("/v1/accounts").await?;
            if ctx.json {
                return print_json(&list);
            }
            if list.accounts.is_empty() {
                println!("no accounts; run `crossagents setup codex`");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = list
                .accounts
                .iter()
                .map(|a| {
                    vec![
                        a.id.clone(),
                        a.provider.clone(),
                        a.label.clone(),
                        a.home.display().to_string(),
                    ]
                })
                .collect();
            print_table(&["ACCOUNT", "PROVIDER", "LABEL", "HOME"], &rows);
        }
        AccountCommand::Add(args) => {
            let req = CreateAccountRequest {
                codebuddy_site: args
                    .codebuddy_site
                    .map(|s| serde_json::from_value(serde_json::Value::String(s)))
                    .transpose()?,
                id: args.id,
                provider: args.provider,
                label: args.label,
                home: args.home,
                import_default: args.import_default,
            };
            let account: Account = client.post("/v1/accounts", &req).await?;
            if req.codebuddy_site.is_some() && account.codebuddy_site != req.codebuddy_site {
                client
                    .delete(&format!("/v1/accounts/{}", account.id))
                    .await?;
                anyhow::bail!(
                    "the hub does not support independent CodeBuddy sites; update the hub and CLI together. The new registration was removed and no login was started"
                );
            }
            if ctx.json {
                return print_json(&account);
            }
            println!("added account {} ({})", account.id, account.home.display());
        }
        AccountCommand::Rm { id } => {
            client.delete(&format!("/v1/accounts/{id}")).await?;
            if !ctx.json {
                println!("removed account {id} (files left in place)");
            }
        }
        AccountCommand::Status { id } => {
            let status: AccountStatus = client.get(&format!("/v1/accounts/{id}/status")).await?;
            if ctx.json {
                return print_json(&status);
            }
            print_status(&status);
        }
        AccountCommand::Usage { id, by } => {
            let usage: AccountUsageResponse = client
                .get(&format!("/v1/accounts/{id}/usage?by={}", by.as_query()))
                .await?;
            if ctx.json {
                return print_json(&usage);
            }
            print_usage(&usage, by);
        }
        AccountCommand::Models { id } => {
            let models: Vec<ModelInfo> = client.get(&format!("/v1/accounts/{id}/models")).await?;
            if ctx.json {
                return print_json(&models);
            }
            print_models(&models);
        }
        AccountCommand::Login { id } => {
            let ok = login(&client, &id).await?;
            if !ok {
                bail!("login did not complete");
            }
        }
    }
    Ok(())
}

/// A login the hub cannot run: shows what to type in another terminal, then waits until the
/// account reports itself logged in.
async fn manual_login(client: &Client, id: &str, steps: &[String]) -> Result<bool> {
    println!("log in from another terminal:");
    for (n, step) in steps.iter().enumerate() {
        println!("  {}. {step}", n + 1);
    }
    println!("waiting for the login…");
    let wait = tokio::time::timeout(Duration::from_secs(15 * 60), async {
        loop {
            let status: AccountStatus = client.get(&format!("/v1/accounts/{id}/status")).await?;
            if status.logged_in {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
    match wait.await {
        Ok(Ok(())) => {
            println!("logged in");
            Ok(true)
        }
        Ok(Err(err)) => Err(err),
        Err(_) => bail!("timed out waiting for the login to finish"),
    }
}

/// Starts a login, shows the link and waits until it finishes. Returns whether it worked.
pub async fn login(client: &Client, id: &str) -> Result<bool> {
    let events = client.live_events().await?;
    let started: LoginStarted = client
        .post(&format!("/v1/accounts/{id}/login"), &serde_json::json!({}))
        .await?;
    if let Some(steps) = &started.manual {
        return manual_login(client, id, steps).await;
    }
    println!("running `{}`", started.command);
    match &started.url {
        Some(url) => {
            println!("open this page to log in (it may already be open in your browser):\n  {url}")
        }
        None => println!("follow the login page that opens in your browser"),
    }
    let mut events = std::pin::pin!(events);
    let wait = tokio::time::timeout(Duration::from_secs(15 * 60), async {
        while let Some(event) = events.next().await {
            if let EventKind::AccountLoginFinished {
                id: done,
                ok,
                message,
            } = event?.kind
                && done == id
            {
                return Ok::<_, anyhow::Error>((ok, message));
            }
        }
        bail!("event stream ended")
    });
    match wait.await {
        Ok(Ok((true, _))) => {
            println!("logged in");
            Ok(true)
        }
        Ok(Ok((false, message))) => {
            println!(
                "login failed{}",
                message.map(|m| format!(":\n{m}")).unwrap_or_default()
            );
            Ok(false)
        }
        Ok(Err(err)) => Err(err),
        Err(_) => bail!("timed out waiting for the login to finish"),
    }
}

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn window_name(seconds: Option<u64>, fallback: &str) -> String {
    match seconds {
        Some(s) if s % 86400 == 0 => format!("{} day", s / 86400),
        Some(s) if s % 3600 == 0 => format!("{} h", s / 3600),
        Some(s) => format!("{} min", s / 60),
        None => fallback.to_owned(),
    }
}

/// `[#####-----]` for a percentage.
fn bar(percent: f64) -> String {
    let filled = ((percent / 10.0).round() as usize).min(10);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(10 - filled))
}

pub fn print_status(s: &AccountStatus) {
    if !s.logged_in {
        println!("login:    not logged in");
        if let Some(note) = &s.note {
            println!("note:     {note}");
        }
        return;
    }
    println!(
        "login:    {}{}",
        s.auth_mode.as_deref().unwrap_or("yes"),
        s.email
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default()
    );
    println!("plan:     {}", s.plan.as_deref().unwrap_or("unknown"));
    if let Some(until) = s.subscription_until {
        println!("renews:   {}", local(until));
    }
    for w in &s.rate_limits {
        println!(
            "quota:    {} {:>5.1}% used of the {} window{}",
            bar(w.used_percent),
            w.used_percent,
            window_name(w.window_seconds, &w.name),
            w.resets_at
                .map(|t| format!(", resets {}", local(t)))
                .unwrap_or_default()
        );
    }
    if s.limit_reached {
        println!("          LIMIT REACHED");
    }
    if let Some(c) = &s.credits {
        let balance = if c.unlimited {
            "unlimited".to_owned()
        } else {
            c.balance.clone().unwrap_or_else(|| "-".into())
        };
        println!("credits:  {balance}");
    }
    if let (Some(source), Some(at)) = (s.source, s.as_of) {
        let kind = match source {
            DataSource::Live => "live",
            DataSource::Snapshot => "last recorded",
        };
        println!("as of:    {} ({kind})", local(at));
    }
    if let Some(note) = &s.note {
        println!("note:     {note}");
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn print_usage(usage: &AccountUsageResponse, by: UsageBy) {
    if usage.rows.is_empty() {
        println!("no sessions recorded yet");
        return;
    }
    let row = |key: &str, t: &crossagents_api_types::TokenTotals, rate: f64| {
        vec![
            key.to_owned(),
            t.sessions.to_string(),
            thousands(t.input_tokens),
            thousands(t.cached_input_tokens),
            format!("{:.0}%", rate * 100.0),
            thousands(t.output_tokens),
            thousands(t.reasoning_output_tokens),
        ]
    };
    let mut rows: Vec<Vec<String>> = usage
        .rows
        .iter()
        .map(|r| row(&r.key, &r.totals, r.cache_hit_rate))
        .collect();
    rows.push(row("TOTAL", &usage.total, usage.total_cache_hit_rate));
    let header = by.as_query().to_uppercase();
    print_table(
        &[
            &header,
            "SESSIONS",
            "INPUT",
            "CACHED",
            "HIT",
            "OUTPUT",
            "REASONING",
        ],
        &rows,
    );
}

pub fn print_models(models: &[ModelInfo]) {
    if models.is_empty() {
        println!("no model list yet (Codex writes it after its first session)");
        return;
    }
    for (i, m) in models.iter().enumerate() {
        println!(
            "{:>2}. {:<16} {}",
            i + 1,
            m.id,
            m.description.as_deref().unwrap_or("")
        );
    }
}

//! `crossagents run`: start and steer orchestrated runs.

use anyhow::Result;
use clap::{Args, Subcommand};
use crossagents_api_types::{
    CreateRunRequest, ListRunsResponse, RejectRunRequest, Run, RunDetail, Task, TaskStatus,
};

use super::Context;
use super::agent::enum_name;
use super::events::short_tail;
use crate::output::{local_time, print_json, print_table};

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RunArgs {
    #[command(subcommand)]
    pub command: Option<RunCommand>,
    #[command(flatten)]
    pub start: StartArgs,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// What you want done.
    pub request: Option<String>,
    /// Agent that plans the work (a strong model).
    #[arg(long)]
    pub planner: Option<String>,
    /// Agent used as the template for workers (a fast, cheap model).
    #[arg(long)]
    pub worker: Option<String>,
    /// Stronger template a task moves to after repeated failures.
    #[arg(long)]
    pub escalate: Option<String>,
    /// Agent that reviews each change before it is merged.
    #[arg(long)]
    pub reviewer: Option<String>,
    /// Start working without waiting for you to approve the plan.
    #[arg(long)]
    pub no_approve: bool,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub max_parallel: Option<u32>,
    #[arg(long)]
    pub max_attempts: Option<u32>,
    /// Teammates the planner may assign tasks to (repeat or comma-separate); default:
    /// every worker and reviewer registered in the planner's repository.
    #[arg(long, value_delimiter = ',')]
    pub team: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum RunCommand {
    /// List runs.
    Ls,
    /// Show a run, its plan and its tasks.
    Show {
        run: String,
        /// Include each task's spec and full report.
        #[arg(long, short)]
        verbose: bool,
    },
    /// Approve the submitted plan and start dispatching tasks.
    Approve { run: String },
    /// Send the plan back to the planner with a reason.
    Reject { run: String, reason: String },
    /// Stop all work on a run.
    Cancel { run: String },
}

pub async fn run(ctx: &Context, args: RunArgs) -> Result<()> {
    let client = ctx.client()?;
    let Some(command) = args.command else {
        let start = args.start;
        let (Some(request), Some(planner), Some(worker)) =
            (start.request, start.planner, start.worker)
        else {
            anyhow::bail!(
                "usage: crossagents run \"<request>\" --planner <agent> --worker <agent> \
                 (or a subcommand: ls, show, approve, reject, cancel)"
            );
        };
        let req = CreateRunRequest {
            request,
            title: start.title,
            planner,
            worker,
            escalate: start.escalate,
            reviewer: start.reviewer,
            auto_approve: start.no_approve,
            max_parallel: start.max_parallel,
            max_attempts: start.max_attempts,
            team: (!start.team.is_empty()).then_some(start.team),
            policy: None,
        };
        let run: Run = client.post("/v1/runs", &req).await?;
        if ctx.json {
            return print_json(&run);
        }
        println!(
            "started run {} on branch {}",
            short_tail(&run.id),
            run.branch
        );
        println!(
            "the planner is working on a plan; follow with `crossagents run show {}`",
            short_tail(&run.id)
        );
        return Ok(());
    };
    match command {
        RunCommand::Ls => {
            let list: ListRunsResponse = client.get("/v1/runs").await?;
            if ctx.json {
                return print_json(&list);
            }
            if list.runs.is_empty() {
                println!(
                    "no runs; start one with `crossagents run \"<request>\" --planner <agent> --worker <agent>`"
                );
                return Ok(());
            }
            let rows: Vec<Vec<String>> = list
                .runs
                .iter()
                .map(|r| {
                    vec![
                        short_tail(&r.id),
                        enum_name(&r.status),
                        local_time(r.created_at),
                        r.title.clone(),
                    ]
                })
                .collect();
            print_table(&["RUN", "STATUS", "CREATED", "TITLE"], &rows);
        }
        RunCommand::Show { run, verbose } => {
            let detail: RunDetail = client.get(&format!("/v1/runs/{run}")).await?;
            if ctx.json {
                return print_json(&detail);
            }
            print_detail(&detail, verbose);
        }
        RunCommand::Approve { run } => {
            let detail: RunDetail = client
                .post(&format!("/v1/runs/{run}/approve"), &serde_json::json!({}))
                .await?;
            if ctx.json {
                return print_json(&detail);
            }
            println!("approved; {} task(s) dispatching", detail.tasks.len());
            print_tasks(&detail.tasks);
        }
        RunCommand::Reject { run, reason } => {
            let detail: RunDetail = client
                .post(
                    &format!("/v1/runs/{run}/reject"),
                    &RejectRunRequest { reason },
                )
                .await?;
            if ctx.json {
                return print_json(&detail);
            }
            println!("plan rejected; the planner will revise it");
        }
        RunCommand::Cancel { run } => {
            let detail: RunDetail = client
                .post(&format!("/v1/runs/{run}/cancel"), &serde_json::json!({}))
                .await?;
            if ctx.json {
                return print_json(&detail);
            }
            println!("run {} cancelled", short_tail(&detail.run.id));
        }
    }
    Ok(())
}

fn print_detail(detail: &RunDetail, verbose: bool) {
    let run = &detail.run;
    println!("run:      {} ({})", short_tail(&run.id), run.id);
    println!("title:    {}", run.title);
    println!("status:   {}", enum_name(&run.status));
    println!(
        "repo:     {} (from {})",
        run.repo.display(),
        run.base_branch
    );
    println!("branch:   {}", run.branch);
    if let Some(note) = &run.note {
        println!("note:     {note}");
    }
    println!("created:  {}", local_time(run.created_at));
    if detail.tasks.is_empty() && !run.plan.is_empty() {
        println!("\nplan ({} tasks, waiting for approval):", run.plan.len());
        for task in &run.plan {
            let deps = if task.depends_on.is_empty() {
                String::new()
            } else {
                format!("  (after {})", task.depends_on.join(", "))
            };
            println!("\n  [{}] {}{deps}", task.key, task.title);
            for line in task.spec.goal.lines() {
                println!("      {line}");
            }
            if !task.spec.scope.allow.is_empty() {
                println!("      scope: {}", task.spec.scope.allow.join(", "));
            }
            for check in &task.spec.acceptance {
                println!("      check: $ {check}");
            }
        }
        println!(
            "\napprove with `crossagents run approve {}` or reject with `crossagents run reject {} \"<reason>\"`",
            short_tail(&run.id),
            short_tail(&run.id)
        );
        return;
    }
    if !detail.tasks.is_empty() {
        println!();
        print_tasks(&detail.tasks);
    }
    if verbose {
        for task in &detail.tasks {
            print_task_report(task);
        }
    }
}

fn print_tasks(tasks: &[Task]) {
    let rows: Vec<Vec<String>> = tasks
        .iter()
        .map(|t| {
            let note = match t.status {
                TaskStatus::Failed | TaskStatus::Cancelled => {
                    t.report.failure.clone().unwrap_or_default()
                }
                TaskStatus::WaitingAnswer => t.report.question.clone().unwrap_or_default(),
                _ => String::new(),
            };
            vec![
                t.key.clone(),
                enum_name(&t.status),
                format!(
                    "{}{}",
                    t.attempts,
                    if t.escalated { " (escalated)" } else { "" }
                ),
                t.title.clone(),
                note.lines().next().unwrap_or_default().to_owned(),
            ]
        })
        .collect();
    print_table(&["TASK", "STATUS", "ATTEMPT", "TITLE", "NOTE"], &rows);
}

fn print_task_report(task: &Task) {
    println!("\n── {} ({}) ──", task.key, enum_name(&task.status));
    println!("goal: {}", task.spec.goal.trim());
    if let Some(branch) = &task.branch {
        println!("branch: {branch}");
    }
    if let Some(summary) = &task.report.summary {
        println!("summary: {summary}");
    }
    if let Some(stat) = &task.report.diff_stat {
        println!("diff:\n{stat}");
    }
    for check in &task.report.checks {
        println!(
            "check {} $ {}\n{}",
            if check.passed { "ok  " } else { "FAIL" },
            check.command,
            check.output.trim()
        );
    }
    if let Some(review) = &task.report.review {
        println!("review: {review}");
    }
    if let Some(failure) = &task.report.failure {
        println!("failure: {failure}");
    }
}

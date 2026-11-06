//! `crossagents run`: start and steer orchestrated runs.

use anyhow::Result;
use clap::{Args, Subcommand};
use crossagents_api_types::{
    ContinuationUpdate, CreateRunRequest, DeleteRunResponse, Event, EventKind, ListRunsResponse,
    RejectRunRequest, Run, RunDetail, RunTrace, Task, TaskStatus, UpdateRunPolicyRequest, handoff,
};

use super::Context;
use super::agent::enum_name;
use super::events::{one_line, short_tail};
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
    /// Enable bounded lead wakeups when the run stops making progress.
    #[arg(long)]
    pub continue_run: bool,
    #[arg(long, default_value_t = 3, requires = "continue_run")]
    pub continuation_attempts: u32,
    #[arg(long, default_value_t = 60, requires = "continue_run")]
    pub continuation_interval: u32,
    /// What you want done.
    pub request: Option<String>,
    /// Agent that leads the run: works on the run branch, and splits the work when it pays.
    #[arg(long)]
    pub planner: Option<String>,
    /// Template for workers when the lead splits the work (a fast, cheap model); the lead's
    /// own model when omitted.
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
    /// Have the lead work in your own directory, with the session and branch you have, and
    /// no commit, checkout or branch switch of the hub's (docs/tasks/LEAD.md §2). The
    /// default is a checkout of the run's own.
    #[arg(long)]
    pub in_place: bool,
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
    /// Change how an existing run continues by itself, and hand its allowance out again.
    Policy {
        run: String,
        /// Wake the lead when nothing is executing, if the run does not already.
        #[arg(long)]
        continue_run: bool,
        /// Stop the wakeups; the run itself carries on.
        #[arg(long, conflicts_with = "continue_run")]
        no_continue: bool,
        /// Wakeups the run may have from now on, in all (default: what it had, or 3).
        #[arg(long, requires = "continue_run")]
        attempts: Option<u32>,
        /// Seconds between wakeups (default: what the run had, or 60).
        #[arg(long, requires = "continue_run")]
        interval: Option<u32>,
        /// Put the allowance back: a run whose wakeups ran out is woken again.
        #[arg(long, requires = "continue_run")]
        reset: bool,
        /// Report the run to its lead when its own records go quiet for this many minutes
        /// while it waits on a busy agent (default: what it had, or off). Only a report:
        /// no turn is interrupted.
        #[arg(long, requires = "continue_run")]
        stuck_minutes: Option<u32>,
        /// Stop reporting a quiet wait; the run keeps its other limits.
        #[arg(long, requires = "continue_run", conflicts_with = "stuck_minutes")]
        no_stuck: bool,
    },
    /// Delete a run that ended, with its tasks and the agents it started.
    Rm {
        run: String,
        /// Delete its branches too. A landed run's go by default; an unlanded run's stay,
        /// since its changes are nowhere else.
        #[arg(long)]
        branches: bool,
        /// Delete even when the run's checkout has uncommitted changes.
        #[arg(long)]
        force: bool,
    },
    /// What happened, by task: turns, commands, checks, spend, the first failure.
    Trace {
        run: String,
        /// Read the event log on disk (`logs/events-*.jsonl`) instead of asking the hub,
        /// which then need not be running.
        #[arg(long)]
        log: bool,
    },
    /// Review a finished run: write its 现象 / 影响 / 方向 table (DOGFOOD §9) to
    /// `docs/tasks/RETRO-<run>.md` in this directory's repository, from its trace and its
    /// issue notes.
    Retro {
        run: String,
        /// Write again over a retro that is already there.
        #[arg(long)]
        force: bool,
    },
}

pub async fn run(ctx: &Context, args: RunArgs) -> Result<()> {
    if let Some(RunCommand::Trace { run, log: true }) = &args.command {
        let trace = trace_from_log(ctx, run)?;
        return print_trace(ctx, &trace);
    }
    let client = ctx.client()?;
    let Some(command) = args.command else {
        let start = args.start;
        let (Some(request), Some(planner)) = (start.request, start.planner) else {
            anyhow::bail!(
                "usage: crossagents run \"<request>\" --planner <agent> [--worker <agent>] \
                 (or a subcommand: ls, show, approve, reject, cancel, policy, rm)"
            );
        };
        let req = CreateRunRequest {
            request,
            title: start.title,
            planner,
            worker: start.worker,
            escalate: start.escalate,
            reviewer: start.reviewer,
            auto_approve: start.no_approve,
            max_parallel: start.max_parallel,
            max_attempts: start.max_attempts,
            team: (!start.team.is_empty()).then_some(start.team),
            policy: start
                .continue_run
                .then(|| crossagents_api_types::RunPolicy {
                    continuation: Some(crossagents_api_types::ContinuationPolicy {
                        max_attempts: start.continuation_attempts,
                        interval_seconds: start.continuation_interval,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            attachments: None,
            lead_place: start
                .in_place
                .then_some(crossagents_api_types::LeadPlace::InPlace),
            fresh: None,
            restore: false,
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
            "the lead is on it; follow with `crossagents run show {}`",
            short_tail(&run.id)
        );
        return Ok(());
    };
    match command {
        RunCommand::Trace { run, .. } => {
            let trace: RunTrace = client.get(&format!("/v1/runs/{run}/trace")).await?;
            print_trace(ctx, &trace)?;
        }
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
        RunCommand::Policy {
            run,
            continue_run,
            no_continue,
            attempts,
            interval,
            reset,
            stuck_minutes,
            no_stuck,
        } => {
            if !continue_run && !no_continue && !reset {
                anyhow::bail!(
                    "say what to change: --continue-run [--attempts N] [--interval S] \
                     [--stuck-minutes M | --no-stuck] [--reset], or --no-continue"
                );
            }
            let req = UpdateRunPolicyRequest {
                continuation: (continue_run || no_continue).then_some(ContinuationUpdate {
                    enabled: continue_run,
                    max_attempts: attempts,
                    interval_seconds: interval,
                    // Said only when asked for: `--no-stuck` states null, a bare command
                    // leaves the run's own setting alone.
                    stuck_minutes: match (stuck_minutes, no_stuck) {
                        (Some(minutes), _) => Some(Some(minutes)),
                        (None, true) => Some(None),
                        (None, false) => None,
                    },
                }),
                reset_attempts: reset,
            };
            let detail: RunDetail = client.post(&format!("/v1/runs/{run}/policy"), &req).await?;
            if ctx.json {
                return print_json(&detail);
            }
            print_policy(&detail);
        }
        RunCommand::Rm {
            run,
            branches,
            force,
        } => {
            let done: DeleteRunResponse = client
                .delete_json(&format!("/v1/runs/{run}?branches={branches}&force={force}"))
                .await?;
            if ctx.json {
                return print_json(&done);
            }
            println!("run {run} deleted");
            print_cleanup(&done);
        }
        RunCommand::Retro { run, force } => super::retro::run(ctx, &run, force).await?,
    }
    Ok(())
}

/// What a run continues by itself now, after a policy change: the limits it is woken under,
/// and how much of the allowance is used.
fn print_policy(detail: &RunDetail) {
    let run = &detail.run;
    let Some(policy) = &run.config.policy.continuation else {
        println!(
            "continuation off for run {}; it is no longer woken (the run itself goes on)",
            short_tail(&run.id)
        );
        return;
    };
    let used = run.continuation.as_ref().map(|s| s.attempts).unwrap_or(0);
    println!(
        "continuation on for run {}: up to {} wakeup(s) every {}s, {used} used",
        short_tail(&run.id),
        policy.max_attempts,
        policy.interval_seconds
    );
    match policy.stuck_minutes {
        Some(minutes) => println!("stuck:    reported after {minutes} quiet minute(s)"),
        None => println!("stuck:    a quiet wait is not reported"),
    }
    if let Some(state) = &run.continuation {
        println!("phase:    {}", state.phase);
        if let Some(at) = state.next_at {
            println!("due:      {}", local_time(at));
        }
    }
}

/// What deleting a run removed, and what it could not (docs/tasks/R1.md §3.3).
fn print_cleanup(done: &DeleteRunResponse) {
    let list = |what: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        println!("{what}:");
        for item in items {
            println!("  {item}");
        }
    };
    list("back in their workspace", &done.agents.returned);
    list("archived (can be restored)", &done.agents.archived);
    list("worktrees", &done.worktrees);
    list("branches", &done.branches);
    if !done.warnings.is_empty() {
        println!("not cleaned up:");
        for warning in &done.warnings {
            println!("  {warning}");
        }
    }
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
    match &run.config.policy.continuation {
        Some(policy) => println!(
            "policy:   continuation, up to {} wakeup(s) every {}s{}",
            policy.max_attempts,
            policy.interval_seconds,
            match policy.stuck_minutes {
                Some(minutes) => format!(", quiet for {minutes}min is reported"),
                None => String::new(),
            }
        ),
        None => println!("policy:   no automatic continuation"),
    }
    if let Some(state) = &run.continuation {
        println!(
            "continuation: {} ({} wakeups, owner {})",
            state.phase, state.attempts, state.owner
        );
        println!("next:     {}", state.next_step);
        if let Some(at) = state.next_at {
            println!("due:      {}", local_time(at));
        }
        if let Some(error) = &state.delivery_error {
            println!("delivery error: {error}");
        }
    }
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
        print_handoffs(&detail.tasks);
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

/// Who had each task, in turn: `#a Pia → Rex (assign) → Ada (reroute: …)`.
fn print_handoffs(tasks: &[Task]) {
    let chains: Vec<String> = tasks
        .iter()
        .filter(|t| !t.handoffs.is_empty())
        .map(|t| format!("#{} {}", t.key, one_line(&handoff::chain(&t.handoffs))))
        .collect();
    if !chains.is_empty() {
        println!("\nhandoffs:");
        for chain in chains {
            println!("  {chain}");
        }
    }
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

/// A run's trace from the event log on disk (docs/tasks/DOGFOOD.md §8); `run` is its id or
/// the start or end of it.
fn trace_from_log(ctx: &Context, run: &str) -> Result<RunTrace> {
    let events: Vec<Event> = crossagents_hub::trace::read_log(&ctx.paths.logs()).map_err(|e| {
        anyhow::anyhow!(
            "cannot read {}: {e}; is event logging on (`[hub] event_log`)?",
            ctx.paths.logs().display()
        )
    })?;
    let wanted = run.trim().to_lowercase().replace('-', "");
    let ids: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::RunCreated { run } => Some(run.id),
            _ => None,
        })
        .filter(|id| {
            let hex = id.to_string().replace('-', "");
            hex.starts_with(&wanted) || hex.ends_with(&wanted)
        })
        .collect();
    let id = match ids.as_slice() {
        [id] => *id,
        [] => anyhow::bail!("no run {run} in the event log"),
        _ => anyhow::bail!("{run} matches several runs; give more of its id"),
    };
    let mut tracer = crossagents_hub::trace::Tracer::new(id);
    for event in &events {
        tracer.push(event);
    }
    Ok(tracer.finish())
}

fn print_trace(ctx: &Context, trace: &RunTrace) -> Result<()> {
    if ctx.json {
        return print_json(trace);
    }
    let spent =
        |s: &crossagents_api_types::Spent| format!("{} tokens, ${:.2}", s.tokens, s.cost_usd);
    println!("spent: {}", spent(&trace.spent));
    let lane = |l: &crossagents_api_types::LaneTrace| {
        let failed = l
            .commands
            .iter()
            .filter(|c| c.exit_code.is_some_and(|x| x != 0))
            .count();
        format!(
            "{} turn(s), {} command(s) ({failed} failed), {}",
            l.turns.len(),
            l.commands.len(),
            spent(&l.spent)
        )
    };
    println!("lead: {}", lane(&trace.lead));
    for task in &trace.tasks {
        let status = task
            .status
            .map(|s| enum_name(&s))
            .unwrap_or_else(|| "?".into());
        println!(
            "#{} {status} — {}",
            task.key,
            task.title.as_deref().unwrap_or_default()
        );
        println!("  {}", lane(&task.lane));
        if let Some(first) = &task.first_failure {
            println!("  first failure: {first}");
        }
        if let Some(failure) = &task.failure {
            println!("  failed: {failure}");
        }
        for reason in &task.rerouted {
            println!("  rerouted: {reason}");
        }
        // Each dispatch and the accounts weighed for it, best first (magpie-lancer §2.8).
        for dispatch in &task.dispatches {
            println!(
                "  account: {}",
                dispatch.account.as_deref().unwrap_or("(its own)")
            );
            for candidate in &dispatch.candidates {
                println!("    {} — {}", candidate.account, candidate.reason);
            }
        }
        if !task.handoffs.is_empty() {
            println!("  handoffs: {}", handoff::chain(&task.handoffs));
        }
        for c in task
            .lane
            .commands
            .iter()
            .filter(|c| c.exit_code.is_some_and(|x| x != 0))
        {
            println!(
                "  ✗ {} (exit {}) {}",
                c.command,
                c.exit_code.unwrap_or_default(),
                local_time(c.at)
            );
        }
    }
    Ok(())
}

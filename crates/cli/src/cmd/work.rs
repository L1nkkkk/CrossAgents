//! `crossagents work`: work groups, and the rounds (runs) each one has (docs/tasks/G1.md).

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Subcommand;
use crossagents_api_types::{ListWorksResponse, WorkDetail};
use crossagents_core::{Landing, RunId, RunStatus, Work};
use serde::Serialize;

use super::Context;
use super::agent::enum_name;
use super::events::short_tail;
use crate::client::Client;
use crate::output::{local_time, print_json, print_table};

#[derive(Debug, Subcommand)]
pub enum WorkCommand {
    /// List work groups: id, title, number of rounds, and how the latest round ended.
    Ls,
    /// Show a work group's rounds: run, title, status, branch, and whether it landed.
    Show {
        /// The work group's id, or the end of it as `work ls` shows it.
        work: String,
    },
}

pub async fn run(ctx: &Context, command: WorkCommand) -> Result<()> {
    let client = ctx.client()?;
    match command {
        WorkCommand::Ls => ls(ctx, &client).await,
        WorkCommand::Show { work } => show(ctx, &client, &work).await,
    }
}

/// One round of a work group: a run, or one that ended and was since deleted (its history
/// outlives it, `RunHistory.deleted`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Round {
    pub run: RunId,
    pub title: String,
    pub status: RunStatus,
    pub branch: String,
    pub landed: Option<Landing>,
    pub created_at: DateTime<Utc>,
    pub deleted: bool,
}

/// The rounds of a work group, oldest first: its live runs, then the ended ones since deleted.
fn rounds(detail: &WorkDetail) -> Vec<Round> {
    let mut rounds: Vec<Round> = detail
        .runs
        .iter()
        .map(|r| Round {
            run: r.id,
            title: r.title.clone(),
            status: r.status,
            branch: r.branch.clone(),
            landed: r.landed.clone(),
            created_at: r.created_at,
            deleted: false,
        })
        .collect();
    rounds.extend(
        detail
            .history
            .iter()
            .filter(|h| h.deleted && !detail.runs.iter().any(|r| r.id == h.run_id))
            .map(|h| Round {
                run: h.run_id,
                title: h.title.clone(),
                status: h.status,
                branch: h.branch.clone(),
                landed: h.landed.clone(),
                created_at: h.created_at,
                deleted: true,
            }),
    );
    rounds.sort_by_key(|r| r.created_at);
    rounds
}

/// The round that came last.
fn latest(rounds: &[Round]) -> Option<&Round> {
    rounds.iter().max_by_key(|r| r.created_at)
}

/// A work group as `work ls` shows it, with its rounds counted.
#[derive(Debug, Serialize)]
pub struct WorkRow {
    pub work: Work,
    pub rounds: usize,
    pub latest: Option<Round>,
}

async fn ls(ctx: &Context, client: &Client) -> Result<()> {
    let list: ListWorksResponse = client.get("/v1/works").await?;
    let mut rows = Vec::with_capacity(list.works.len());
    for work in list.works {
        let detail: WorkDetail = client.get(&format!("/v1/works/{}", work.id)).await?;
        let all = rounds(&detail);
        rows.push(WorkRow {
            rounds: all.len(),
            latest: latest(&all).cloned(),
            work,
        });
    }
    if ctx.json {
        return print_json(&rows);
    }
    if rows.is_empty() {
        println!("no work groups; a run started from the app or `work_start` makes one");
        return Ok(());
    }
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                short_tail(&row.work.id),
                row.work.title.clone(),
                row.rounds.to_string(),
                row.latest
                    .as_ref()
                    .map(|r| status_text(r))
                    .unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    print_table(&["WORK", "TITLE", "ROUNDS", "LATEST"], &table);
    Ok(())
}

async fn show(ctx: &Context, client: &Client, wanted: &str) -> Result<()> {
    let work = resolve(client, wanted).await?;
    let detail: WorkDetail = client.get(&format!("/v1/works/{}", work.id)).await?;
    let all = rounds(&detail);
    if ctx.json {
        #[derive(Serialize)]
        struct Shown<'a> {
            work: &'a Work,
            rounds: &'a [Round],
        }
        return print_json(&Shown {
            work: &detail.work,
            rounds: &all,
        });
    }
    println!("work:    {} ({})", short_tail(&work.id), work.id);
    println!("title:   {}", work.title);
    println!("repo:    {}", work.repo.display());
    println!("lead:    {}", work.lead);
    println!("created: {}", local_time(work.created_at));
    println!("updated: {}", local_time(work.updated_at));
    if all.is_empty() {
        println!("\nno rounds yet");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|r| {
            vec![
                short_tail(&r.run),
                status_text(r),
                r.branch.clone(),
                match &r.landed {
                    Some(landing) => format!("yes ({})", enum_name(&landing.mode)),
                    None => "no".into(),
                },
                r.title.clone(),
            ]
        })
        .collect();
    println!();
    print_table(&["RUN", "STATUS", "BRANCH", "LANDED", "TITLE"], &rows);
    Ok(())
}

/// The status of a round, with `(deleted)` when its run is gone.
fn status_text(round: &Round) -> String {
    enum_name(&round.status) + if round.deleted { " (deleted)" } else { "" }
}

/// The work group `wanted` names: its full id, or the end of it as `work ls` prints it.
async fn resolve(client: &Client, wanted: &str) -> Result<Work> {
    let list: ListWorksResponse = client.get("/v1/works").await?;
    let works = list.works;
    let found = matching(&works, wanted)?;
    Ok(found.clone())
}

/// The one work whose id starts or ends with `wanted` (dashes and case ignored).
fn matching<'a>(works: &'a [Work], wanted: &str) -> Result<&'a Work> {
    let wanted = wanted.trim().to_lowercase().replace('-', "");
    let found: Vec<&Work> = works
        .iter()
        .filter(|w| {
            let hex = w.id.to_string().replace('-', "");
            !wanted.is_empty() && (hex.starts_with(&wanted) || hex.ends_with(&wanted))
        })
        .collect();
    match found.as_slice() {
        [work] => Ok(work),
        [] => anyhow::bail!("no work group {wanted}; see `crossagents work ls`"),
        _ => anyhow::bail!("{wanted} matches several work groups; give more of its id"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use crossagents_core::{AgentId, WorkId};

    use super::*;

    fn work(title: &str) -> Work {
        let now = Utc::now();
        Work {
            id: WorkId::new(),
            title: title.into(),
            repo: "C:/repo".into(),
            lead: AgentId::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn round(day: u32, status: RunStatus, deleted: bool) -> Round {
        Round {
            run: RunId::new(),
            title: format!("round {day}"),
            status,
            branch: format!("crossagents/run-{day}"),
            landed: None,
            created_at: Utc.with_ymd_and_hms(2026, 11, day, 9, 0, 0).unwrap(),
            deleted,
        }
    }

    #[test]
    fn latest_is_the_round_created_last() {
        let rounds = vec![
            round(3, RunStatus::Running, false),
            round(1, RunStatus::Completed, true),
            round(2, RunStatus::Failed, false),
        ];
        let last = latest(&rounds).unwrap();
        assert_eq!(last.title, "round 3");
        assert_eq!(last.status, RunStatus::Running);
        assert!(latest(&[]).is_none());
    }

    #[test]
    fn a_deleted_latest_round_says_so() {
        let deleted = round(4, RunStatus::Cancelled, true);
        assert_eq!(status_text(&deleted), "cancelled (deleted)");
        let live = round(4, RunStatus::Completed, false);
        assert_eq!(status_text(&live), "completed");
    }

    #[test]
    fn a_work_matches_by_the_end_of_its_id() {
        let works = vec![work("one"), work("two")];
        let target = &works[1];
        let short = short_tail(&target.id);
        assert_eq!(matching(&works, &short).unwrap().id, target.id);
        assert_eq!(
            matching(&works, &target.id.to_string().to_uppercase())
                .unwrap()
                .id,
            target.id
        );
        assert!(matching(&works, "zzzz-no-such-work").is_err());
        assert!(matching(&works, "").is_err());
    }
}

//! `crossagents run retro <run>`: a finished run's retro, written as a document.

use anyhow::Result;
use serde::Deserialize;

use super::Context;
use crate::output::print_json;

#[derive(Debug, Deserialize)]
struct Done {
    run: crossagents_core::RunId,
    path: String,
    findings: usize,
    issues: usize,
}

pub async fn run(ctx: &Context, run: &str) -> Result<()> {
    let client = ctx.client()?;
    // The hub reads the run's trace and issue notes and writes the document, so a big event
    // store stays on the hub's side.
    let done: Done = client
        .post(&format!("/v1/runs/{run}/retro"), &serde_json::json!({}))
        .await?;
    if ctx.json {
        return print_json(&serde_json::json!({
            "run": done.run,
            "path": done.path,
            "findings": done.findings,
            "issues": done.issues,
        }));
    }
    println!("wrote {}", done.path);
    println!(
        "{} 条现象 / {} 条 issue 笔记；拿去挑一条影响大、改动小的开下一轮",
        done.findings, done.issues
    );
    Ok(())
}

//! `crossagents run retro <run>`: a finished run's retro, written as a document.
//!
//! The document goes into the repository the current directory belongs to, never into the
//! run's own checkout on some other machine: the person reads it where they ran the command.

use std::path::{Path, PathBuf};

use anyhow::Result;
use crossagents_hub::retro::RetroDoc;
use serde::Deserialize;

use super::Context;
use crate::output::print_json;

#[derive(Debug, Deserialize)]
struct Retro {
    run: crossagents_core::RunId,
    markdown: String,
    findings: usize,
    issues: usize,
}

pub async fn run(ctx: &Context, run: &str, force: bool) -> Result<()> {
    let client = ctx.client()?;
    // The hub folds the trace and the issue notes; the command writes the file where the
    // person is, so a big event store stays on the hub's side.
    let retro: Retro = client.get(&format!("/v1/runs/{run}/retro")).await?;
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("cannot read the current directory: {e}"))?;
    let doc = RetroDoc {
        markdown: retro.markdown,
        findings: retro.findings,
        issues: retro.issues,
    };
    let path = crossagents_hub::retro::write(&repo_root(&cwd), retro.run, &doc, force)?;
    if ctx.json {
        return print_json(&serde_json::json!({
            "run": retro.run,
            "path": path.display().to_string(),
            "findings": doc.findings,
            "issues": doc.issues,
        }));
    }
    println!("wrote {}", path.display());
    println!(
        "{} 条现象 / {} 条 issue 笔记；拿去挑一条影响大、改动小的开下一轮",
        doc.findings, doc.issues
    );
    Ok(())
}

/// The repository `dir` belongs to: the nearest ancestor holding a `.git` entry (a directory,
/// or a worktree's file), or `dir` itself when there is none.
fn repo_root(dir: &Path) -> PathBuf {
    for at in dir.ancestors() {
        if at.join(".git").exists() {
            return at.to_path_buf();
        }
    }
    dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subdirectory_lands_in_its_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        let sub = root.join("crates").join("x");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(repo_root(&sub), root);
        // A worktree's `.git` is a file, not a directory.
        let worktree = root.join("wt");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: ../.git/worktrees/wt").unwrap();
        assert_eq!(repo_root(&worktree), worktree);
    }

    #[test]
    fn a_directory_without_a_repository_is_its_own_root() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(repo_root(dir.path()), dir.path());
    }
}

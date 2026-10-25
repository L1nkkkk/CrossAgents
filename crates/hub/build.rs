// The commit the hub is built from, for `/v1/info` (`HubInfo::commit`): the GUI compares it
// with its own, so a page and a hub from different builds can tell. No git, no commit.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn main() {
    // `logs/HEAD` moves on every commit, checkout and reset, also in a worktree.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/logs/HEAD");
    }
    println!("cargo:rerun-if-env-changed=CROSSAGENTS_COMMIT");
    let commit = std::env::var("CROSSAGENTS_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]));
    if let Some(commit) = commit {
        println!("cargo:rustc-env=CROSSAGENTS_COMMIT={commit}");
    }
}

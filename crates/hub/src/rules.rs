//! One set of project rules for every vendor (docs/tasks/DOGFOOD.md §6): `CROSSAGENTS.md`,
//! else `AGENTS.md`, else `CLAUDE.md`, put into the opening instructions of the agents that
//! would not read that file themselves.

use std::path::{Path, PathBuf};

use crossagents_core::Agent;

/// Bytes of a rules file put into the instructions.
const LIMIT: usize = 64 * 1024;

const FILES: [&str; 3] = ["CROSSAGENTS.md", "AGENTS.md", "CLAUDE.md"];

/// The rules for `agent`, as a section of its opening instructions, or `None` when there
/// are none or it reads them itself.
pub fn for_agent(agent: &Agent) -> Option<String> {
    let (path, name) = find(&agent.cwd)?;
    if reads_itself(agent, name) {
        return None;
    }
    let mut text = std::fs::read_to_string(&path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let cut = text.len() > LIMIT;
    if cut {
        let mut end = LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Some(format!(
        "# Project rules\n\nFrom {} (the person keeps these for every agent working here):\n\n{}{}",
        path.display(),
        text.trim_end(),
        if cut {
            "\n\n[cut: the file is longer; read it for the rest]"
        } else {
            ""
        }
    ))
}

/// The first rules file from `cwd` up to its repository's root.
fn find(cwd: &Path) -> Option<(PathBuf, &'static str)> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        for name in FILES {
            let path = d.join(name);
            if path.is_file() {
                return Some((path, name));
            }
        }
        if d.join(".git").exists() {
            return None;
        }
        dir = d.parent();
    }
    None
}

/// Whether the agent's own CLI already loads `file`.
fn reads_itself(agent: &Agent, file: &str) -> bool {
    match file {
        // with-cli puts the workspace's AGENTS.md into its own rules.
        "AGENTS.md" => matches!(agent.plugin.as_str(), "codex" | "with"),
        "CLAUDE.md" => {
            agent.plugin == "claude"
                && agent
                    .config
                    .get("setting_sources")
                    .and_then(|v| v.as_str())
                    .is_none_or(|s| s.split(',').any(|p| p.trim() == "project"))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{AgentId, AgentProfile, AgentStatus, NameSource, Role};
    use serde_json::json;

    fn agent(plugin: &str, cwd: &Path, config: serde_json::Value) -> Agent {
        Agent {
            node: None,
            id: AgentId::new(),
            name: "Luna".into(),
            name_source: NameSource::Chosen,
            plugin: plugin.into(),
            driver: None,
            model: None,
            role: Role::Worker,
            cwd: cwd.to_path_buf(),
            config,
            status: AgentStatus::Stopped,
            created_at: chrono::Utc::now(),
            profile: AgentProfile::default(),
            topic: None,
            archived_at: None,
            home: PathBuf::new(),
        }
    }

    #[test]
    fn each_vendor_gets_what_it_does_not_read() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let sub = repo.path().join("crates").join("hub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(repo.path().join("CLAUDE.md"), "Commit messages in Chinese.").unwrap();

        // Only CLAUDE.md: Claude reads it, the others are told, from a subdirectory too.
        assert_eq!(for_agent(&agent("claude", &sub, json!({}))), None);
        let codex = for_agent(&agent("codex", &sub, json!({}))).unwrap();
        assert!(codex.contains("Commit messages in Chinese.") && codex.contains("CLAUDE.md"));
        // A Claude that reads no project settings is told as well.
        assert!(for_agent(&agent("claude", &sub, json!({ "setting_sources": "" }))).is_some());

        // AGENTS.md wins over CLAUDE.md; Codex and With read it themselves.
        std::fs::write(repo.path().join("AGENTS.md"), "Run cargo test.").unwrap();
        assert_eq!(for_agent(&agent("codex", &sub, json!({}))), None);
        assert_eq!(for_agent(&agent("with", &sub, json!({}))), None);
        assert!(
            for_agent(&agent("claude", &sub, json!({})))
                .unwrap()
                .contains("Run cargo test.")
        );

        // CROSSAGENTS.md goes to everyone.
        std::fs::write(repo.path().join("CROSSAGENTS.md"), "One rule for all.").unwrap();
        for plugin in ["claude", "codex", "codebuddy"] {
            assert!(
                for_agent(&agent(plugin, &sub, json!({})))
                    .unwrap()
                    .contains("One rule for all.")
            );
        }
        // Outside the repository nothing is looked for above it.
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir(elsewhere.path().join(".git")).unwrap();
        assert_eq!(
            for_agent(&agent("codex", elsewhere.path(), json!({}))),
            None
        );
    }
}

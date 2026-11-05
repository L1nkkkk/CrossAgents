use anyhow::{Context, Result, ensure};
use crossagents_api_types::*;
use crossagents_orchestrator::messaging::Group;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

#[derive(Debug, Clone, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub command: Vec<String>,
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default = "timeout")]
    pub timeout_secs: u32,
}
fn timeout() -> u32 {
    60
}
#[derive(Debug, Clone, Deserialize)]
pub struct Prompt {
    pub file: String,
    #[serde(default)]
    pub groups: Vec<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Gui {
    pub entry: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub api: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub access: ExtensionAccess,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub prompts: Vec<Prompt>,
    pub gui: Option<Gui>,
}
fn entry(root: &Path, file: &str) -> Result<u64> {
    ensure!(
        !file.is_empty()
            && !file.contains('\\')
            && Path::new(file)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "entry must be a relative path: {file}"
    );
    let meta = std::fs::symlink_metadata(root.join(file))
        .with_context(|| format!("missing entry: {file}"))?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "entry must be a regular file: {file}"
    );
    Ok(meta.len())
}
fn check_groups(groups: &[String], human: bool) -> Result<()> {
    let mut accepted: Vec<_> = [
        Group::Everyone,
        Group::Lead,
        Group::Assignee,
        Group::Reviewer,
        Group::Conversation,
    ]
    .into_iter()
    .map(Group::name)
    .collect();
    if human {
        accepted.push("human");
    }
    for group in groups {
        ensure!(
            accepted.contains(&group.as_str()),
            "unknown group {group:?}; accepted groups: {}",
            accepted.join(", ")
        );
    }
    Ok(())
}
impl Manifest {
    pub fn read(root: &Path, name: &str) -> Result<Self> {
        let m: Self = toml::from_str(&std::fs::read_to_string(root.join("extension.toml"))?)?;
        check_extension_name(&m.name).map_err(anyhow::Error::msg)?;
        // `/v1/extensions/calls/{id}` is a route of its own, and an extension of that name
        // would be unreachable beside it (`/v1/extensions/{name}/enable`).
        ensure!(
            m.name != "calls",
            "`calls` is reserved and cannot name an extension"
        );
        ensure!(m.name == name, "manifest name must match directory name");
        ensure!(
            m.api == 1,
            "unsupported extension api {}; expected 1",
            m.api
        );
        let mut names = BTreeSet::new();
        for t in &m.tools {
            extension_tool_name(name, &t.name).map_err(anyhow::Error::msg)?;
            check_groups(&t.groups, true)
                .map_err(|e| anyhow::anyhow!("tool {} groups: {e}", t.name))?;
            ensure!(names.insert(&t.name), "duplicate tool {}", t.name);
            ensure!(
                !t.command.is_empty() && !t.command[0].is_empty(),
                "tool command cannot be empty"
            );
            ensure!(
                (1..=600).contains(&t.timeout_secs),
                "timeout_secs must be 1..600"
            );
            // Compiling checks the schema itself. External references have no retriever.
            jsonschema::validator_for(&t.input_schema)
                .map_err(|e| anyhow::anyhow!("invalid input_schema: {e}"))?;
        }
        for p in &m.prompts {
            check_groups(&p.groups, false)
                .map_err(|e| anyhow::anyhow!("prompt {} groups: {e}", p.file))?;
            entry(root, &p.file)?;
        }
        if let Some(gui) = &m.gui {
            entry(root, &gui.entry)?;
        }
        Ok(m)
    }
    pub fn version(
        &self,
        root: &Path,
        fingerprint: String,
        files: u32,
        bytes: u64,
    ) -> Result<ExtensionVersion> {
        Ok(ExtensionVersion {
            fingerprint,
            api: self.api,
            description: self.description.clone(),
            access: self.access,
            tools: self
                .tools
                .iter()
                .map(|t| ExtensionToolInfo {
                    name: t.name.clone(),
                    qualified_name: extension_tool_name(&self.name, &t.name).unwrap(),
                    description: t.description.clone(),
                    input_schema: t.input_schema.clone(),
                    groups: t.groups.clone(),
                    timeout_secs: t.timeout_secs,
                })
                .collect(),
            prompts: self
                .prompts
                .iter()
                .map(|p| {
                    Ok(ExtensionPromptInfo {
                        file: p.file.clone(),
                        groups: p.groups.clone(),
                        bytes: entry(root, &p.file)?,
                    })
                })
                .collect::<Result<_>>()?,
            gui_entry: self.gui.as_ref().map(|g| g.entry.clone()),
            files,
            bytes,
            taken_at: std::fs::metadata(root)?
                .created()
                .or_else(|_| std::fs::metadata(root)?.modified())?
                .into(),
        })
    }
}

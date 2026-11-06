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
/// `[[rules]]`: a stream rule file in the snapshot (docs/EXTENSIONS.md §4).
#[derive(Debug, Clone, Deserialize)]
pub struct RuleFile {
    pub file: String,
}
/// `[[on]]`: one process per event, observe only (docs/EXTENSIONS.md §3).
#[derive(Debug, Clone, Deserialize)]
pub struct Hook {
    pub event: String,
    pub command: Vec<String>,
    #[serde(default = "timeout")]
    pub timeout_secs: u32,
}
/// `[service]`: one resident process per enabled extension (docs/EXTENSIONS.md §3).
#[derive(Debug, Clone, Deserialize)]
pub struct Service {
    pub command: Vec<String>,
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
    /// `[[on]]`, in file order.
    #[serde(default)]
    pub on: Vec<Hook>,
    pub service: Option<Service>,
    pub gui: Option<Gui>,
    /// `[[rules]]`: stream rule files, in file order.
    #[serde(default)]
    pub rules: Vec<RuleFile>,
}
/// The event wire names the hub can emit, read from the core source at compile time so the
/// list cannot drift. `creator.rs` builds `extension_api` from the same list.
pub fn event_names() -> Vec<&'static str> {
    include_str!("../../../core/src/event.rs")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("#[serde(rename = \""))
        .filter_map(|line| line.strip_suffix("\")]"))
        .collect()
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
        let events = event_names();
        let mut hooked = BTreeSet::new();
        for hook in &m.on {
            // Refused, not merely ignored: an extension that could react to another one
            // would let two of them trigger each other forever (docs/EXTENSIONS.md §3).
            ensure!(
                !hook.event.starts_with("extension."),
                "hook {}: extension.* events are refused; extensions cannot react to each other",
                hook.event
            );
            ensure!(
                events.contains(&hook.event.as_str()),
                "hook {}: unknown event; accepted events are {}",
                hook.event,
                events.join(", ")
            );
            ensure!(
                hooked.insert(&hook.event),
                "duplicate hook for event {}",
                hook.event
            );
            ensure!(
                !hook.command.is_empty() && !hook.command[0].is_empty(),
                "hook {} command cannot be empty",
                hook.event
            );
            ensure!(
                (1..=600).contains(&hook.timeout_secs),
                "hook {} timeout_secs must be 1..600",
                hook.event
            );
        }
        if let Some(service) = &m.service {
            ensure!(
                !service.command.is_empty() && !service.command[0].is_empty(),
                "service command cannot be empty"
            );
        }
        if let Some(gui) = &m.gui {
            entry(root, &gui.entry)?;
        }
        // A rule that does not parse is skipped at session start (with a warning), as a
        // broken project rule is; only a missing or unsafe path is refused here.
        for rule in &m.rules {
            entry(root, &rule.file)
                .map_err(|e| anyhow::anyhow!("rule {}: {e}", rule.file))?;
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
                    command: t.command.clone(),
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
            hooks: self
                .on
                .iter()
                .map(|h| ExtensionHookInfo {
                    event: h.event.clone(),
                    command: h.command.clone(),
                    timeout_secs: h.timeout_secs,
                })
                .collect(),
            service: self.service.as_ref().map(|s| ExtensionServiceInfo { command: s.command.clone() }),
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

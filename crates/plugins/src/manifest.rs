//! Agents as description files: `<home>/plugins/<name>/plugin.toml`, and the built-ins in
//! `agents/*.toml` (docs/DESIGN.md §9.3).
//!
//! ```toml
//! name = "gemini"
//! protocol = "acp"                  # or "native"
//! command = ["gemini", "--experimental-acp"]     # ACP launch command
//! # driver = "acp-generic"          # the default
//! # native_command = [...]           # launch command, overriding everything below
//! # models = ["gemini-3-pro"]        # or the [models] section
//! # efforts = ["low", "high"]        # reasoning effort levels
//!
//! [models]
//! fallback = ["gemini-3-pro", "gemini-3-flash"]
//! efforts = ["low", "high"]
//!
//! [locate]
//! env = "GEMINI_CLI"                # checked first, after native_command
//! commands = ["gemini"]             # then PATH
//! paths = ["~/.local/bin/gemini"]   # then the vendor's own install locations
//! bundled = ["%LOCALAPPDATA%/Gemini/bin/*/gemini.exe"]   # then another app's copy
//! shim = "node-script"              # how to see through an npm wrapper
//! version = ["--version"]
//!
//! [locate.paths]                    # any of them per system
//! windows = ["%LOCALAPPDATA%/Gemini/bin/gemini.exe"]
//! macos = ["/usr/local/bin/gemini"]
//!
//! [install]
//! hint = "npm install -g @google/gemini-cli"
//! docs = "https://..."
//! ```
//!
//! Without a `driver` the plugin protocol is ACP: any executable that speaks it can be a
//! plugin. A native driver needs no ACP command, but its `locate` section then says where its
//! CLI is.

use std::path::{Path, PathBuf};

use crossagents_core::DriverKind;
use crossagents_drivers::locate::{Install, Locator};
use serde::Deserialize;

use crate::PluginConfig;

pub const MANIFEST: &str = "plugin.toml";

#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    #[serde(default = "default_protocol")]
    protocol: String,
    #[serde(default)]
    command: Vec<String>,
    /// The model list: `models = [...]`, or the `[models]` section below.
    #[serde(default)]
    models: Models,
    #[serde(default)]
    model_env: Option<String>,
    #[serde(default)]
    driver: Option<DriverKind>,
    #[serde(default)]
    native_command: Vec<String>,
    /// Effort levels, when they are given beside `models` rather than in its section.
    #[serde(default)]
    efforts: Vec<String>,
    #[serde(default)]
    locate: Locator,
    #[serde(default)]
    install: Install,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Models {
    List(Vec<String>),
    Section(ModelSection),
}

impl Default for Models {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelSection {
    /// What the CLI accepts; the same as the flat `models` list.
    fallback: Vec<String>,
    efforts: Vec<String>,
}

fn default_protocol() -> String {
    "acp".into()
}

/// Plugins found in `dir` (with the manifest each came from), plus a warning for every
/// manifest that could not be used.
pub fn load_dir(dir: &Path) -> (Vec<(String, PluginConfig, PathBuf)>, Vec<String>) {
    let mut plugins = Vec::new();
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (plugins, warnings); // no plugins directory is fine
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join(MANIFEST).is_file())
        .collect();
    dirs.sort();
    for plugin_dir in dirs {
        let path = plugin_dir.join(MANIFEST);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                warnings.push(format!("{}: {err}", path.display()));
                continue;
            }
        };
        match parse_in(&text, &path.display().to_string(), Some(&plugin_dir)) {
            Ok(entry) => plugins.push((entry.0, entry.1, path)),
            Err(err) => warnings.push(format!("{}: {err}", path.display())),
        }
    }
    (plugins, warnings)
}

/// One manifest, without a directory of its own (the built-in descriptions).
pub(crate) fn parse(text: &str, origin: &str) -> Result<(String, PluginConfig), String> {
    parse_in(text, origin, None)
}

fn parse_in(
    text: &str,
    origin: &str,
    plugin_dir: Option<&Path>,
) -> Result<(String, PluginConfig), String> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| e.to_string())?;
    if !matches!(manifest.protocol.as_str(), "acp" | "native") {
        return Err(format!(
            "unsupported protocol {:?} (only \"acp\" and \"native\")",
            manifest.protocol
        ));
    }
    if crossagents_core::validate_slug(&manifest.name).is_err() {
        return Err(format!("invalid plugin name {:?}", manifest.name));
    }
    let driver = manifest.driver.unwrap_or_default();
    if driver == DriverKind::AcpGeneric && manifest.command.is_empty() {
        return Err("command is empty".into());
    }
    let (models, efforts) = match manifest.models {
        Models::List(models) => (models, manifest.efforts),
        Models::Section(section) => {
            let efforts = if section.efforts.is_empty() {
                manifest.efforts
            } else {
                section.efforts
            };
            (section.fallback, efforts)
        }
    };
    let mut command = manifest.command;
    let mut native_command = manifest.native_command;
    for program in [command.first_mut(), native_command.first_mut()]
        .into_iter()
        .flatten()
    {
        // A relative path with a directory part points inside the plugin.
        if (program.contains('/') || program.contains('\\'))
            && Path::new(program).is_relative()
            && let Some(dir) = plugin_dir
        {
            *program = dir.join(&*program).display().to_string();
        }
    }
    tracing::debug!(origin, name = manifest.name, "agent description");
    Ok((
        manifest.name,
        PluginConfig {
            command,
            models,
            model_env: manifest.model_env,
            driver: manifest.driver,
            native_command,
            efforts,
            locate: manifest.locate,
            install: manifest.install,
            described_by: None,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MANIFEST), body).unwrap();
    }

    #[test]
    fn loads_valid_manifests_and_reports_bad_ones() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "good",
            "name = \"good\"\ncommand = [\"./bin/agent\", \"--acp\"]\nmodels = [\"m\"]\n",
        );
        write(
            dir.path(),
            "global",
            "name = \"global\"\ncommand = [\"node\", \"x.js\"]\n",
        );
        write(
            dir.path(),
            "wrong",
            "name = \"wrong\"\nprotocol = \"grpc\"\ncommand = [\"x\"]\n",
        );
        write(dir.path(), "broken", "this is not toml [");
        std::fs::create_dir_all(dir.path().join("no-manifest")).unwrap();

        let (plugins, warnings) = load_dir(dir.path());
        let names: Vec<&str> = plugins.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, ["global", "good"]);
        let good = &plugins[1].1;
        assert!(
            Path::new(&good.command[0]).is_absolute(),
            "{:?}",
            good.command
        );
        assert!(good.command[0].contains("good"));
        assert_eq!(plugins[0].1.command[0], "node");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn missing_directory_is_empty() {
        let (plugins, warnings) = load_dir(Path::new("/definitely/not/here"));
        assert!(plugins.is_empty() && warnings.is_empty());
    }

    #[test]
    fn native_plugins_need_no_acp_command() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "cx",
            "name = \"cx\"\nprotocol = \"native\"\ndriver = \"codex-native\"\nnative_command = [\"./bin/codex\", \"app-server\"]\nefforts = [\"low\", \"high\"]\n",
        );
        write(dir.path(), "acp", "name = \"acp\"\n");
        let (plugins, warnings) = load_dir(dir.path());
        assert_eq!(plugins.len(), 1, "{warnings:?}");
        let (name, config, manifest_path) = &plugins[0];
        assert_eq!(name, "cx");
        assert_eq!(config.driver, Some(DriverKind::CodexNative));
        assert!(manifest_path.ends_with("plugin.toml"), "{manifest_path:?}");
        assert!(Path::new(&config.native_command[0]).is_absolute());
        assert_eq!(config.efforts, ["low", "high"]);
        assert!(warnings[0].contains("command is empty"), "{warnings:?}");
    }

    #[test]
    fn the_models_section_holds_the_list_and_the_efforts() {
        let config: Manifest = toml::from_str(
            "name = \"m\"\n[models]\nfallback = [\"a\", \"b\"]\nefforts = [\"low\"]\n",
        )
        .unwrap();
        let (models, efforts) = match config.models {
            Models::Section(section) => (section.fallback, section.efforts),
            Models::List(_) => panic!("a section"),
        };
        assert_eq!(models, ["a", "b"]);
        assert_eq!(efforts, ["low"]);
        // The flat list still works, and so do efforts beside it.
        let flat: Manifest =
            toml::from_str("name = \"m\"\nmodels = [\"a\"]\nefforts = [\"high\"]\n").unwrap();
        assert!(matches!(flat.models, Models::List(_)));
        assert_eq!(flat.efforts, ["high"]);
    }

    #[test]
    fn a_description_can_say_where_its_cli_is() {
        let (name, config) = parse(
            r#"
            name = "demo"
            driver = "codebuddy-native"
            [locate]
            env = "DEMO_CLI"
            commands = ["demo"]
            shim = "node-script"
            version = ["--version"]
            [locate.paths]
            windows = ["%LOCALAPPDATA%/demo/demo.exe"]
            macos = ["/usr/local/bin/demo"]
            [install]
            hint = "npm install -g demo"
            "#,
            "test",
        )
        .unwrap();
        assert_eq!(name, "demo");
        assert_eq!(config.locate.shim.as_deref(), Some("node-script"));
        assert_eq!(
            config.locate.env.get().map(String::as_str),
            Some("DEMO_CLI")
        );
        assert!(config.locate.paths.get_for("windows").is_some());
        assert!(config.locate.paths.get_for("linux").is_none());
        assert_eq!(config.install.hint.as_deref(), Some("npm install -g demo"));
    }
}

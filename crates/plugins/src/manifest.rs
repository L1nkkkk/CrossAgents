//! External plugins: `<home>/plugins/<name>/plugin.toml` (docs/DESIGN.md §9.3).
//!
//! ```toml
//! name = "gemini"
//! protocol = "acp"
//! command = ["gemini", "--experimental-acp"]
//! models = ["gemini-3-pro", "gemini-3-flash"]
//! # driver = "acp-generic"          # the default
//! # native_command = [...]          # launch command for a native driver
//! ```
//!
//! Without a `driver` the plugin protocol is ACP: any executable that speaks it can be a
//! plugin.

use std::path::{Path, PathBuf};

use crossagents_core::DriverKind;
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
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    model_env: Option<String>,
    #[serde(default)]
    driver: Option<DriverKind>,
    #[serde(default)]
    native_command: Vec<String>,
}

fn default_protocol() -> String {
    "acp".into()
}

/// Plugins found in `dir`, plus a warning for every manifest that could not be used.
pub fn load_dir(dir: &Path) -> (Vec<(String, PluginConfig)>, Vec<String>) {
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
        match load_one(&plugin_dir, &path) {
            Ok(entry) => plugins.push(entry),
            Err(err) => warnings.push(format!("{}: {err}", path.display())),
        }
    }
    (plugins, warnings)
}

fn load_one(plugin_dir: &Path, path: &Path) -> Result<(String, PluginConfig), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let manifest: Manifest = toml::from_str(&text).map_err(|e| e.to_string())?;
    if manifest.protocol != "acp" {
        return Err(format!(
            "unsupported protocol {:?} (only \"acp\")",
            manifest.protocol
        ));
    }
    if crossagents_core::validate_agent_name(&manifest.name).is_err() {
        return Err(format!("invalid plugin name {:?}", manifest.name));
    }
    let driver = manifest.driver.unwrap_or_default();
    if driver == DriverKind::AcpGeneric && manifest.command.is_empty() {
        return Err("command is empty".into());
    }
    let mut command = manifest.command;
    let mut native_command = manifest.native_command;
    for program in [command.first_mut(), native_command.first_mut()]
        .into_iter()
        .flatten()
    {
        // A relative path with a directory part points inside the plugin.
        if (program.contains('/') || program.contains('\\')) && Path::new(program).is_relative() {
            *program = plugin_dir.join(&*program).display().to_string();
        }
    }
    Ok((
        manifest.name,
        PluginConfig {
            command,
            models: manifest.models,
            model_env: manifest.model_env,
            driver: manifest.driver,
            native_command,
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
        let names: Vec<&str> = plugins.iter().map(|(n, _)| n.as_str()).collect();
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
}

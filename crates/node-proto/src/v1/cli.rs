use crossagents_core::{CliSource, CliStatus, Located};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// [`CliStatus`] as it crosses the wire: the core type is not serializable (it is local
/// state, not a message), so the fields travel in their own shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCliStatus {
    pub located: Option<WireLocated>,
    pub probing: bool,
    pub install_hint: Option<String>,
    pub docs: Option<String>,
    pub installed_via: Option<String>,
    pub install_command: Option<Vec<String>>,
    pub upgrade_command: Option<Vec<String>>,
    pub can_upgrade: bool,
    pub described_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLocated {
    pub argv: Vec<String>,
    pub path: PathBuf,
    pub source: String,
    pub version: Option<String>,
}

impl From<CliStatus> for WireCliStatus {
    fn from(status: CliStatus) -> Self {
        Self {
            located: status.located.map(|located| WireLocated {
                argv: located.argv,
                path: located.path,
                source: located.source.as_str().to_owned(),
                version: located.version,
            }),
            probing: status.probing,
            install_hint: status.install_hint,
            docs: status.docs,
            installed_via: status.installed_via,
            install_command: status.install_command,
            upgrade_command: status.upgrade_command,
            can_upgrade: status.can_upgrade,
            described_by: status.described_by,
        }
    }
}

impl From<WireCliStatus> for CliStatus {
    fn from(status: WireCliStatus) -> Self {
        Self {
            located: status.located.map(|located| Located {
                argv: located.argv,
                path: located.path,
                source: match located.source.as_str() {
                    "config" => CliSource::Config,
                    "env" => CliSource::Env,
                    "path" => CliSource::Path,
                    "native" => CliSource::Native,
                    "bundled" => CliSource::Bundled,
                    _ => CliSource::Path,
                },
                version: located.version,
            }),
            probing: status.probing,
            install_hint: status.install_hint,
            docs: status.docs,
            installed_via: status.installed_via,
            install_command: status.install_command,
            upgrade_command: status.upgrade_command,
            can_upgrade: status.can_upgrade,
            described_by: status.described_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_status_round_trips() {
        let status = CliStatus {
            located: Some(Located {
                argv: vec!["node".into(), "cli.js".into()],
                path: PathBuf::from("/usr/bin/node"),
                source: CliSource::Path,
                version: Some("1.2.3".into()),
            }),
            probing: false,
            install_hint: Some("npm i -g x".into()),
            docs: None,
            installed_via: Some("npm".into()),
            install_command: Some(vec!["npm".into()]),
            upgrade_command: None,
            can_upgrade: true,
            described_by: Some("built-in".into()),
        };
        let wire = WireCliStatus::from(status.clone());
        assert_eq!(CliStatus::from(wire), status);
    }
}

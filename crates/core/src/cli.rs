//! Where a native driver's CLI was found on this machine (docs/DESIGN.md §9.3).
//!
//! A driver looks its program up by a description (an agent description file), not by its own
//! rules: `native_command` first, then an environment variable, `PATH`, the vendor's own
//! install directory, and the copy another app bundles. [`Located`] is the answer, shared by
//! all of them, and what `PluginInfo::cli` reports.

use std::path::PathBuf;

/// Where the program came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliSource {
    /// `native_command` in the description or `config.toml`.
    Config,
    /// The description's `locate.env`.
    Env,
    /// A `locate.commands` name found on `PATH`.
    Path,
    /// One of the vendor's own install locations (`locate.paths`).
    Native,
    /// The copy another app keeps for itself (`locate.bundled`), newest first.
    Bundled,
}

impl CliSource {
    pub const ALL: [CliSource; 5] = [
        Self::Config,
        Self::Env,
        Self::Path,
        Self::Native,
        Self::Bundled,
    ];

    /// As the API names it (`PluginInfo.cli.source`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Env => "env",
            Self::Path => "path",
            Self::Native => "native",
            Self::Bundled => "bundled",
        }
    }
}

/// A CLI as it would be launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The program and the arguments that launch it: `node <script>` for an npm wrapper the
    /// description knows how to see through, the vendor binary itself otherwise.
    pub argv: Vec<String>,
    /// The file that was found: the CLI's own entry point, even when `argv` runs it through
    /// `node`.
    pub path: PathBuf,
    pub source: CliSource,
    /// What `<argv> <version args>` answered, when it answered in time.
    pub version: Option<String>,
}

/// A native driver's CLI on this machine: where it is, and how to install it when it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliStatus {
    /// `None`: not found, so nothing can be launched.
    pub located: Option<Located>,
    /// The command that installs it (shown, never run).
    pub install_hint: Option<String>,
    /// Where to read more about installing and signing in.
    pub docs: Option<String>,
    /// The install method that put what was found there (`npm`, `native`), when the path
    /// says which one did (docs/DESIGN.md §9.3).
    pub installed_via: Option<String>,
    /// The command this machine would run to install it, as argv: what the confirmation
    /// box shows.
    pub install_command: Option<Vec<String>>,
    /// The command this machine would run to upgrade it; `None` when it cannot be upgraded.
    pub upgrade_command: Option<Vec<String>>,
    /// Whether upgrading in place is something the description knows how to do: false for a
    /// copy another app keeps, and for a program the person pointed at themselves.
    pub can_upgrade: bool,
    /// Where the description came from: `built-in`, a manifest's path, or `config.toml`.
    pub described_by: Option<String>,
}

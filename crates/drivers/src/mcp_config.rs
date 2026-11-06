//! The MCP servers a CLI's own user configuration already has, and the ones the hub passes
//! (docs/research/multica.md §2.10).
//!
//! Both CLIs take `--mcp-config` for the servers a session must have, and both read a
//! user-level file of their own (`~/.claude.json` for Claude Code, `~/.codebuddy/mcp.json`
//! for CodeBuddy; each moves with `CLAUDE_CONFIG_DIR` / `CODEBUDDY_CONFIG_DIR`). What
//! differs is what `--strict-mcp-config` does next:
//!
//! - **Claude Code** is started with it: only `--mcp-config` counts, so every server the
//!   person configured for their own `claude` would silently be gone inside a session. We
//!   therefore read that file here and hand the servers over in `--mcp-config` too.
//! - **CodeBuddy** is started without it: it loads its own `mcp.json` by itself, so merging
//!   it here as well would start each of those servers twice. Checked against CodeBuddy
//!   2.173.0 -- with the flag, `system/init` lists none of the user's servers; without it,
//!   it lists them beside the ones from `--mcp-config`, each once.
//!
//! The hub's own servers (`crossagents` and the `[mcp.*]` entries of `config.toml`) always
//! win on a name: a user file cannot replace the collaboration tools.
//!
//! Only the user-level file is read. Project-scope servers (`.mcp.json`, Claude Code's
//! `projects.<cwd>` section) belong to whatever directory the session happens to run in, so
//! they are left to the CLI: a session in a worktree can find the repository's own file when
//! it is started without strict mode, and never inherits one from the hub.

use std::path::{Path, PathBuf};

use crossagents_core::McpServerSpec;
use serde_json::{Map, Value, json};

use crate::claude_native::wire::Flavor;

/// Where the CLI keeps the MCP servers its user configured, for the session's environment:
/// `CLAUDE_CONFIG_DIR` / `CODEBUDDY_CONFIG_DIR` when the account moves the config dir, the
/// user's home otherwise.
pub fn user_config(flavor: Flavor, env: &[(String, String)]) -> Option<PathBuf> {
    match flavor {
        Flavor::Claude => match var(env, "CLAUDE_CONFIG_DIR") {
            // Claude Code keeps `.claude.json` *inside* `CLAUDE_CONFIG_DIR`, beside `~/.claude`
            // when that is unset (crates/plugins/src/claude_account.rs).
            Some(dir) => Some(dir.join(".claude.json")),
            None => Some(home()?.join(".claude.json")),
        },
        Flavor::Codebuddy => match var(env, "CODEBUDDY_CONFIG_DIR") {
            Some(dir) => Some(dir.join("mcp.json")),
            None => Some(home()?.join(".codebuddy").join("mcp.json")),
        },
    }
}

/// What goes in `--mcp-config`, as the JSON string both CLIs accept: the hub's servers,
/// plus (Claude Code only) the user's own that the hub does not define. `None` when there is
/// nothing to pass.
pub fn config_arg(
    flavor: Flavor,
    env: &[(String, String)],
    hub: &[McpServerSpec],
) -> Option<String> {
    let mut servers = hub_servers(hub);
    if flavor == Flavor::Claude
        && let Some(path) = user_config(flavor, env)
    {
        for (name, server) in read_servers(&path) {
            servers.entry(name).or_insert(server);
        }
    }
    if servers.is_empty() {
        return None;
    }
    Some(json!({ "mcpServers": servers }).to_string())
}

/// The servers the hub itself adds, in the shape both CLIs read.
pub fn hub_servers(hub: &[McpServerSpec]) -> Map<String, Value> {
    hub.iter()
        .map(|server| {
            (
                server.name.clone(),
                json!({
                    "type": "stdio",
                    "command": server.command.display().to_string(),
                    "args": server.args,
                    "env": server.env,
                }),
            )
        })
        .collect()
}

/// The `mcpServers` object of a CLI config file: empty when the file, the key or the whole
/// JSON is not there (a config file the CLI itself cannot read is one it would not load
/// either, so there is nothing to report).
pub fn read_servers(path: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Map::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Map::new();
    };
    value
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// A variable from the session's environment: the last one set wins, as for the process.
fn var(env: &[(String, String)], key: &str) -> Option<PathBuf> {
    env.iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| PathBuf::from(value))
        .filter(|value| !value.as_os_str().is_empty())
}

/// The user's home, for a session that does not move its config dir.
fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn hub(name: &str) -> McpServerSpec {
        McpServerSpec {
            name: name.to_owned(),
            command: "/bin/crossagents".into(),
            args: vec!["mcp".into()],
            env: BTreeMap::from([("TOKEN".to_owned(), "t".to_owned())]),
        }
    }

    fn env_with(dir: &Path) -> Vec<(String, String)> {
        vec![("CLAUDE_CONFIG_DIR".to_owned(), dir.display().to_string())]
    }

    /// A `~/.claude.json`: mostly Claude Code's own bookkeeping, with the servers inside.
    fn claude_config(dir: &Path, servers: &str) {
        std::fs::write(
            dir.join(".claude.json"),
            format!(r#"{{"numStartups":3,"mcpServers":{servers}}}"#),
        )
        .unwrap();
    }

    fn parsed(arg: &str) -> Map<String, Value> {
        let value: Value = serde_json::from_str(arg).unwrap();
        value["mcpServers"].as_object().unwrap().clone()
    }

    #[test]
    fn claude_gets_the_users_servers_merged_in() {
        let dir = tempfile::tempdir().unwrap();
        claude_config(
            dir.path(),
            r#"{"playwright":{"type":"stdio","command":"npx","args":["playwright"]}}"#,
        );
        let arg = config_arg(Flavor::Claude, &env_with(dir.path()), &[hub("crossagents")]).unwrap();
        let servers = parsed(&arg);
        assert!(servers.contains_key("crossagents"));
        assert_eq!(servers["playwright"]["command"], "npx");
        assert_eq!(servers.len(), 2);
    }

    #[test]
    fn the_hubs_own_server_wins_over_the_users() {
        let dir = tempfile::tempdir().unwrap();
        claude_config(
            dir.path(),
            r#"{"crossagents":{"type":"stdio","command":"mine","args":[]}}"#,
        );
        let arg = config_arg(Flavor::Claude, &env_with(dir.path()), &[hub("crossagents")]).unwrap();
        let servers = parsed(&arg);
        assert_eq!(servers["crossagents"]["command"], "/bin/crossagents");
        assert_eq!(servers.len(), 1);
    }

    #[test]
    fn codebuddy_is_left_to_load_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let env = vec![("CODEBUDDY_CONFIG_DIR".to_owned(), dir.path().display().to_string())];
        std::fs::write(
            dir.path().join("mcp.json"),
            r#"{"mcpServers":{"playwright":{"type":"stdio","command":"npx"}}}"#,
        )
        .unwrap();
        // Present in the file, absent from what we pass: CodeBuddy would load it itself,
        // and passing it too would start it twice.
        let arg = config_arg(Flavor::Codebuddy, &env, &[hub("crossagents")]).unwrap();
        let servers = parsed(&arg);
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["crossagents"]);
        // And with nothing of the hub's, there is no argument at all.
        assert!(config_arg(Flavor::Codebuddy, &env, &[]).is_none());
    }

    #[test]
    fn a_missing_or_broken_config_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let arg = config_arg(Flavor::Claude, &env_with(dir.path()), &[hub("crossagents")]).unwrap();
        assert_eq!(parsed(&arg).len(), 1, "{arg}");
        std::fs::write(dir.path().join(".claude.json"), "not json").unwrap();
        let arg = config_arg(Flavor::Claude, &env_with(dir.path()), &[hub("crossagents")]).unwrap();
        assert_eq!(parsed(&arg).len(), 1, "{arg}");
        // No hub servers and no user file: nothing to pass, and `--mcp-config` is left out.
        let empty = tempfile::tempdir().unwrap();
        assert!(config_arg(Flavor::Claude, &env_with(empty.path()), &[]).is_none());
    }

    #[test]
    fn the_default_directory_is_the_users_home() {
        let claude = user_config(Flavor::Claude, &[]).unwrap();
        assert_eq!(claude.file_name().unwrap(), ".claude.json");
        assert!(!claude.parent().unwrap().as_os_str().is_empty());
        let codebuddy = user_config(Flavor::Codebuddy, &[]).unwrap();
        assert!(codebuddy.ends_with(".codebuddy/mcp.json"), "{codebuddy:?}");
        // The account's directory wins, whichever case the variable is written in.
        let dir = tempfile::tempdir().unwrap();
        let env = vec![("claude_config_dir".to_owned(), dir.path().display().to_string())];
        assert_eq!(
            user_config(Flavor::Claude, &env).unwrap(),
            dir.path().join(".claude.json")
        );
    }
}

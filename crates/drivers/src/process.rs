//! Child-process helpers shared by the drivers.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use crossagents_core::process::NoWindow;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

const STDERR_TAIL: usize = 40;

/// The last lines an agent wrote to stderr, for error reports.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    /// Collects `stderr` in the background.
    pub fn spawn(stderr: tokio::process::ChildStderr) -> Self {
        let tail = Self::default();
        let lines = tail.0.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                tracing::debug!(target: "crossagents::agent_stderr", "{line}");
                let mut lines = lines.lock().await;
                if lines.len() == STDERR_TAIL {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        });
        tail
    }

    /// `"<what>"`, or `"<what>:\n<tail>"` when stderr said something.
    pub async fn describe(&self, what: &str) -> String {
        let lines: Vec<String> = self.0.lock().await.iter().cloned().collect();
        if lines.is_empty() {
            what.to_owned()
        } else {
            format!("{what}:\n{}", lines.join("\n"))
        }
    }
}

/// On Windows a bare program name does not find `npx.cmd` and friends; search `PATH`.
pub fn resolve_program(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if !cfg!(windows) || path.extension().is_some() || path.components().count() > 1 {
        return path;
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&dirs) {
        for ext in ["exe", "cmd", "bat"] {
            let candidate = dir.join(program).with_extension(ext);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    path
}

/// `program` as found on `PATH` (with the Windows executable extensions), else as given.
pub fn find_in_path(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if path.components().count() > 1 {
        return path;
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return path;
    };
    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&dirs) {
        for ext in exts {
            let candidate = if ext.is_empty() {
                dir.join(program)
            } else {
                dir.join(program).with_extension(ext)
            };
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    path
}

#[cfg(windows)]
pub fn hide_console(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_: &mut Command) {}

/// Proxy settings for vendor CLIs that do not read the system proxy themselves (Claude
/// Code): the Windows system proxy as `HTTPS_PROXY` / `HTTP_PROXY`, with local addresses
/// exempt so agents still reach the hub. Empty when the hub's own environment already names
/// a proxy (children inherit it) or there is no system proxy.
pub fn system_proxy_env() -> Vec<(String, String)> {
    static PROXY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let named = [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
    ]
    .iter()
    .any(|v| std::env::var_os(v).is_some_and(|p| !p.is_empty()));
    if named {
        return Vec::new();
    }
    let Some(proxy) = PROXY.get_or_init(windows_proxy) else {
        return Vec::new();
    };
    vec![
        ("HTTPS_PROXY".into(), proxy.clone()),
        ("HTTP_PROXY".into(), proxy.clone()),
        ("NO_PROXY".into(), "localhost,127.0.0.1,::1".into()),
    ]
}

/// `ProxyServer` from the user's Internet Settings when `ProxyEnable` is on, as a URL.
fn windows_proxy() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let output = std::process::Command::new("reg")
        .no_window()
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
        ])
        .output()
        .ok()?;
    parse_proxy_settings(&String::from_utf8_lossy(&output.stdout))
}

fn parse_proxy_settings(reg: &str) -> Option<String> {
    let value = |name: &str| {
        reg.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next()? == name).then(|| parts.nth(1).map(str::to_owned))?
        })
    };
    if value("ProxyEnable")? != "0x1" {
        return None;
    }
    let server = value("ProxyServer")?;
    // Either `host:port` for every protocol or `http=host:port;https=host:port`.
    let address = if server.contains('=') {
        let by_scheme = |scheme: &str| {
            server.split(';').find_map(|part| {
                let (s, addr) = part.split_once('=')?;
                (s.eq_ignore_ascii_case(scheme)).then(|| addr.to_owned())
            })
        };
        by_scheme("https").or_else(|| by_scheme("http"))?
    } else {
        server
    };
    Some(if address.contains("://") {
        address
    } else {
        format!("http://{address}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_windows_proxy_settings() {
        let reg =
            "\n    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    127.0.0.1:7890\n";
        assert_eq!(
            parse_proxy_settings(reg).as_deref(),
            Some("http://127.0.0.1:7890")
        );
        let split = "ProxyEnable REG_DWORD 0x1\nProxyServer REG_SZ http=p:80;https=s:443\n";
        assert_eq!(parse_proxy_settings(split).as_deref(), Some("http://s:443"));
        assert!(
            parse_proxy_settings("ProxyEnable REG_DWORD 0x0\nProxyServer REG_SZ p:1\n").is_none()
        );
    }
}

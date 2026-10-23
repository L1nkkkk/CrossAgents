//! Child-process helpers shared by the drivers.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossagents_core::CoreError;
use crossagents_core::process::NoWindow;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::Mutex;

const STDERR_TAIL: usize = 40;

/// The last lines an agent wrote to stderr, for error reports.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    /// Collects `stderr` in the background.
    pub fn spawn(stderr: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> Self {
        let tail = Self::default();
        let this = tail.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                this.push(line).await;
            }
        });
        tail
    }

    /// Records one line the agent wrote to stderr.
    pub async fn push(&self, line: String) {
        tracing::debug!(target: "crossagents::agent_stderr", "{line}");
        let mut lines = self.0.lock().await;
        if lines.len() == STDERR_TAIL {
            lines.pop_front();
        }
        lines.push_back(line);
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

/// Windows children inherit every inheritable handle of their parent, not just the ones
/// given as their stdio, and this process's own stdin, stdout and stderr are inheritable
/// when it was started with them. A long-lived child (the hub started by `hub start`, a
/// runtime host started by the hub) would then hold this process's output open: whoever
/// reads it (`hub run | tee`, a GUI, `$(crossagents hub start)`) sees no end until that
/// child exits, long after this process did.
///
/// Clears the inherit flag on this process's std handles, once. Children started with
/// [`std::process::Stdio::inherit`] still get them: the standard library duplicates the
/// handles it passes. Does nothing elsewhere: Unix descriptors are close-on-exec and a
/// child's 0, 1 and 2 are the ones it was given.
pub fn stop_inheriting_std_handles() {
    #[cfg(windows)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(windows_no_inherit);
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_no_inherit() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain Win32 calls on handles owned by this process; failures (no such
        // handle, a console pseudo handle) are harmless and ignored.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle as isize != -1 {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Proxy settings for vendor CLIs that do not read the system proxy themselves (Claude
/// Code): the Windows system proxy as `HTTPS_PROXY` / `HTTP_PROXY`, with local addresses
/// exempt so agents still reach the hub. Empty when there is no system proxy and the hub's
/// own environment names none; when it does name one, children inherit it and only the
/// loopback exemption is added.
pub fn system_proxy_env() -> Vec<(String, String)> {
    if names_a_proxy() {
        // The proxy is inherited, but loopback still has to bypass it: the agent's MCP
        // server is `crossagents mcp` talking to the hub on 127.0.0.1.
        return no_proxy_env();
    }
    let Some(proxy) = SYSTEM_PROXY.get_or_init(windows_proxy) else {
        return Vec::new();
    };
    let mut env = vec![
        ("HTTPS_PROXY".into(), proxy.clone()),
        ("HTTP_PROXY".into(), proxy.clone()),
    ];
    env.extend(no_proxy_env());
    env
}

/// Whether the hub's own environment already names a proxy for the children to inherit.
fn names_a_proxy() -> bool {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Addresses that never go through a proxy: agents talk to the hub on loopback.
const LOCAL_BYPASS: &str = "localhost,127.0.0.1,::1";

/// `NO_PROXY` for a child: [`LOCAL_BYPASS`], kept alongside whatever the hub was given.
/// The key keeps the case the hub's environment used, so a client that reads only one of
/// `NO_PROXY` / `no_proxy` still sees the merged value.
fn no_proxy_env() -> Vec<(String, String)> {
    let inherited = ["NO_PROXY", "no_proxy"]
        .iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| (*key, value))
        });
    match inherited {
        Some((key, value)) => vec![(key.to_owned(), no_proxy_value(Some(&value)))],
        None => vec![("NO_PROXY".to_owned(), no_proxy_value(None))],
    }
}

fn no_proxy_value(inherited: Option<&str>) -> String {
    match inherited {
        Some(inherited) if !inherited.contains("localhost") && !inherited.contains("127.0.0.1") => {
            format!("{LOCAL_BYPASS},{inherited}")
        }
        Some(inherited) => inherited.to_owned(),
        None => LOCAL_BYPASS.to_owned(),
    }
}

/// How long a proxy gets to accept a connection before it counts as down.
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Fails when the proxy a vendor CLI would be pointed at is not accepting connections.
///
/// A CLI started against a dead proxy runs fine and then cannot reach its API: the turn
/// stalls, or fails much later with something that never mentions the proxy. A connection
/// test at launch is the check that is unambiguous — a refused port is a dead proxy,
/// whatever the CLI would have made of it.
///
/// `env` is what the caller adds to the CLI's environment after [`system_proxy_env`]: if it
/// names a proxy, that is the one the CLI ends up using.
pub async fn check_proxy(env: &[(String, String)]) -> Result<(), CoreError> {
    match effective_proxy(env) {
        Some(proxy) => probe(&proxy).await,
        None => Ok(()),
    }
}

/// The proxy a vendor CLI would use: `env` first, then the hub's own `HTTPS_PROXY` /
/// `ALL_PROXY` (children inherit them), then the Windows system proxy. `None` when traffic
/// goes out directly.
fn effective_proxy(overrides: &[(String, String)]) -> Option<ProxyUrl> {
    let lookup = |key: &str| -> Option<String> {
        overrides
            .iter()
            .find(|(name, value)| name == key && !value.is_empty())
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(key).ok().filter(|value| !value.is_empty()))
    };
    if let Some(raw) = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .find_map(|key| lookup(key))
    {
        return ProxyUrl::parse(&raw);
    }
    ProxyUrl::parse(SYSTEM_PROXY.get_or_init(windows_proxy).as_deref()?)
}

async fn probe(proxy: &ProxyUrl) -> Result<(), CoreError> {
    let target = (proxy.host.as_str(), proxy.port);
    let failed = |detail: String| {
        CoreError::Agent(format!(
            "the proxy {} is not usable ({detail}); start it, or set HTTPS_PROXY (or the system \
             proxy) to a working one",
            proxy.url
        ))
    };
    match tokio::time::timeout(PROXY_CONNECT_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(failed(err.to_string())),
        Err(_) => Err(failed(format!(
            "no connection within {}s",
            PROXY_CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

/// A proxy URL as the CLIs are given it, split up so it can be probed.
#[derive(Debug)]
struct ProxyUrl {
    /// As the environment or the registry wrote it, for error messages.
    url: String,
    host: String,
    port: u16,
}

impl ProxyUrl {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (scheme, rest) = raw.split_once("://").unwrap_or(("http", raw));
        // `user:pass@host:port/path` → `host:port`
        let authority = rest.split(['/', '?', '#']).next()?;
        let authority = authority.rsplit('@').next()?;
        let default_port = match scheme.to_ascii_lowercase().as_str() {
            "https" => 443,
            "socks" | "socks4" | "socks4a" | "socks5" | "socks5h" => 1080,
            _ => 80,
        };
        let (host, port) = match authority.strip_prefix('[') {
            // `[::1]:7890`
            Some(rest) => {
                let (host, after) = rest.split_once(']')?;
                let port = after
                    .strip_prefix(':')
                    .and_then(|port| port.parse().ok())
                    .unwrap_or(default_port);
                (host.to_owned(), port)
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) if port.parse::<u16>().is_ok() => {
                    (host.to_owned(), port.parse().ok()?)
                }
                _ => (authority.to_owned(), default_port),
            },
        };
        (!host.is_empty()).then(|| Self {
            url: raw.to_owned(),
            host,
            port,
        })
    }
}

/// The Windows system proxy, read once per hub: restarting it picks up a change, as it does
/// for the CLIs the hub starts.
static SYSTEM_PROXY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

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

    #[test]
    fn reads_proxy_urls() {
        let parse = |raw: &str| ProxyUrl::parse(raw).map(|p| (p.host, p.port));
        assert_eq!(
            parse("http://127.0.0.1:7890"),
            Some(("127.0.0.1".into(), 7890))
        );
        assert_eq!(parse("127.0.0.1:7890"), Some(("127.0.0.1".into(), 7890)));
        assert_eq!(parse("socks5://10.0.0.1"), Some(("10.0.0.1".into(), 1080)));
        assert_eq!(
            parse("https://proxy.corp:8443/"),
            Some(("proxy.corp".into(), 8443))
        );
        assert_eq!(
            parse("http://user:pass@127.0.0.1:1080"),
            Some(("127.0.0.1".into(), 1080))
        );
        assert_eq!(parse("http://[::1]:7890"), Some(("::1".into(), 7890)));
        assert_eq!(parse(""), None);
        assert_eq!(parse("http://"), None);
    }

    #[test]
    fn loopback_always_bypasses_the_proxy() {
        assert_eq!(no_proxy_value(None), "localhost,127.0.0.1,::1");
        assert_eq!(
            no_proxy_value(Some("*.corp.example")),
            "localhost,127.0.0.1,::1,*.corp.example"
        );
        // Already exempt: left alone rather than repeated.
        assert_eq!(
            no_proxy_value(Some("localhost,10.0.0.0/8")),
            "localhost,10.0.0.0/8"
        );
    }

    #[test]
    fn an_agents_own_proxy_wins_over_the_hubs() {
        let env = vec![("HTTPS_PROXY".to_owned(), "http://10.0.0.9:8080".to_owned())];
        assert_eq!(
            effective_proxy(&env).map(|proxy| proxy.url),
            Some("http://10.0.0.9:8080".to_owned())
        );
    }

    #[tokio::test]
    async fn a_dead_proxy_is_reported_before_a_cli_starts() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bound");
        let port = listener.local_addr().expect("address").port();
        let proxy = ProxyUrl::parse(&format!("http://127.0.0.1:{port}")).expect("parsed");
        assert!(probe(&proxy).await.is_ok());
        drop(listener);
        let err = probe(&proxy).await.expect_err("nothing listens there now");
        assert!(err.to_string().contains(&proxy.url), "{err}");
    }
}

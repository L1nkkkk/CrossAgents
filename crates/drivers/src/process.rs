//! Child-process helpers shared by the drivers.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossagents_core::CoreError;
use crossagents_core::EgressPolicy;
use crossagents_core::process::NoWindow;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
    find_in_path_in(program, None)
}

/// [`find_in_path`], looking in `dirs` instead of this process's `PATH` when one is given
/// (tests run in parallel and must not touch the real environment).
pub fn find_in_path_in(program: &str, dirs: Option<&[PathBuf]>) -> PathBuf {
    let path = PathBuf::from(program);
    if path.components().count() > 1 {
        return path;
    }
    let inherited;
    let dirs: &[PathBuf] = match dirs {
        Some(dirs) => dirs,
        None => {
            let Some(value) = std::env::var_os("PATH") else {
                return path;
            };
            inherited = std::env::split_paths(&value).collect::<Vec<_>>();
            &inherited
        }
    };
    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    for dir in dirs {
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
    let Some(proxy) = system_proxy() else {
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
    let inherited = ["NO_PROXY", "no_proxy"].iter().find_map(|key| {
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

/// How long the proxy gets to carry a TLS handshake to the vendor's API.
///
/// Generous on purpose: a working node took 6.6s for one API host on this machine. The
/// check only separates "the tunnel carries bytes" from "it carries none", so waiting is
/// cheaper than calling a working proxy dead.
const REACH_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of the proxy's `CONNECT` answer is read before giving up on it.
const CONNECT_ANSWER_LIMIT: usize = 8 * 1024;

/// Fails when the proxy a vendor CLI would be pointed at cannot carry the CLI's traffic.
///
/// A CLI started against a dead proxy runs fine and then cannot reach its API: the turn
/// stalls, or fails much later with something that never mentions the proxy. A connection
/// test at launch is the check that is unambiguous — a refused port is a dead proxy,
/// whatever the CLI would have made of it.
///
/// An open port is not enough on its own: a proxy can accept `CONNECT`, answer `200
/// Connection established`, and still carry nothing because its route to that one host is
/// dead. The CLI then fails minutes into a turn with a TLS error that names no proxy (seen:
/// a rule that sent the vendor's API through a node that could not reach it). `reach` is
/// the host the CLI has to get to, which [`reach_through`] checks with a real handshake;
/// `None` when the caller cannot name it, and only the port is checked.
///
/// `env` is what the caller adds to the CLI's environment after [`system_proxy_env`]: if it
/// names a proxy, that is the one the CLI ends up using.
///
/// `policy` decides what happens when the proxy is up but its route to `reach` is dead
/// (docs/ACCOUNT-RISK.md):
///
/// * [`EgressPolicy::ProxyOnly`] fails with an error that names the host and the policy to
///   change. No direct connection is attempted, so the vendor never sees this machine's
///   own address instead of the proxy node, and nothing is added to [`DIRECT_HOSTS`] — the
///   fast path that would send a later launch straight out does not apply either. The
///   account's login is what the proxy is there to protect; a session that cannot use the
///   proxy is a session that does not run.
/// * [`EgressPolicy::AllowDirect`] keeps the old behaviour byte for byte: a host that
///   answers directly is exempted from the proxy for this launch, the returned variables
///   go into the CLI's environment last, and that host is remembered in [`DIRECT_HOSTS`]
///   for [`DIRECT_TTL`] so later launches skip the wait. This is the right choice for a
///   vendor or gateway that is reachable without the proxy anyway, where a direct
///   connection exposes nothing extra.
pub async fn check_proxy(
    env: &[(String, String)],
    reach: Option<&str>,
    policy: EgressPolicy,
) -> Result<Vec<(String, String)>, CoreError> {
    let Some(proxy) = effective_proxy(env) else {
        return Ok(Vec::new());
    };
    probe(&proxy).await?;
    // A SOCKS proxy needs its own handshake before TLS; the port test is all there is.
    let Some(host) = reach.filter(|_| proxy.scheme == "http") else {
        return Ok(Vec::new());
    };
    if policy == EgressPolicy::AllowDirect && goes_direct(host) {
        return Ok(bypass_env(env, host));
    }
    let Err(err) = reach_through(&proxy, host).await else {
        return Ok(Vec::new());
    };
    if policy == EgressPolicy::ProxyOnly {
        return Err(CoreError::Agent(format!(
            "to protect this account, refusing to reach {host} over a bare-IP direct \
             connection: the proxy {} is up but cannot carry the traffic ({err}) — fix the \
             proxy node, or set the account's egress policy to allow_direct",
            proxy.url
        )));
    }
    if reach_directly(host).await {
        DIRECT_HOSTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(host.to_owned(), std::time::Instant::now());
        return Ok(bypass_env(env, host));
    }
    Err(err)
}

/// `env` without the hub-internal [`crossagents_core::EGRESS_ENV`] entries.
///
/// The hub passes an account's egress policy to drivers in that variable; it is not meant
/// for the vendor CLI, so every path that hands `env` to a process strips it here.
pub fn without_internal_env(env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case(crossagents_core::EGRESS_ENV))
        .cloned()
        .collect()
}

/// Hosts the proxy could not reach but a direct connection could, with when that was
/// found: later launches go direct at once instead of waiting out [`REACH_TIMEOUT`] again.
static DIRECT_HOSTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(Default::default);

/// How long a host found reachable only directly keeps skipping the proxy check: the proxy
/// may be fixed (a rule added, a node switched) in the meantime.
const DIRECT_TTL: Duration = Duration::from_secs(10 * 60);

fn goes_direct(host: &str) -> bool {
    DIRECT_HOSTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(host)
        .is_some_and(|found| found.elapsed() < DIRECT_TTL)
}

/// `NO_PROXY` for a CLI that has to reach `host` directly: what it would have had anyway
/// (the caller's own value first, else the hub's merged with loopback), plus the host's
/// domain, so its sibling hosts (`api.` next to `www.`) go direct as well.
fn bypass_env(overrides: &[(String, String)], host: &str) -> Vec<(String, String)> {
    let domain = host.strip_prefix("www.").unwrap_or(host);
    let own = overrides
        .iter()
        .find(|(key, value)| key.eq_ignore_ascii_case("NO_PROXY") && !value.is_empty())
        .cloned();
    let (key, value) = own.unwrap_or_else(|| {
        no_proxy_env()
            .into_iter()
            .next()
            .expect("no_proxy_env names one variable")
    });
    if value.split(',').any(|entry| entry.trim() == domain) {
        return vec![(key, value)];
    }
    vec![(key, format!("{value},{domain}"))]
}

/// Whether `host` answers a TLS hello on port 443 without any proxy.
async fn reach_directly(host: &str) -> bool {
    let Ok(Ok(mut stream)) =
        tokio::time::timeout(PROXY_CONNECT_TIMEOUT, TcpStream::connect((host, 443))).await
    else {
        return false;
    };
    if stream.write_all(&client_hello(host)).await.is_err() {
        return false;
    }
    let mut byte = [0u8; 1];
    matches!(
        tokio::time::timeout(REACH_TIMEOUT, stream.read(&mut byte)).await,
        Ok(Ok(n)) if n > 0
    )
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
    ProxyUrl::parse(system_proxy().as_deref()?)
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

/// Fails when the proxy cannot carry a TLS handshake to `host`.
///
/// `CONNECT` is answered before the proxy has its upstream: a `200` only means the request
/// was understood, not that anything is behind it. The first byte the far end sends is the
/// first proof the tunnel reaches it, so a `ClientHello` goes in and a byte has to come
/// back.
async fn reach_through(proxy: &ProxyUrl, host: &str) -> Result<(), CoreError> {
    let failed = |detail: String| {
        CoreError::Agent(format!(
            "the proxy {} is up but cannot reach {host} ({detail}); its route to that host \
             looks dead — give the proxy a direct rule for it, switch its node, or set \
             HTTPS_PROXY to a working proxy",
            proxy.url
        ))
    };
    let mut stream = match tokio::time::timeout(
        PROXY_CONNECT_TIMEOUT,
        TcpStream::connect((proxy.host.as_str(), proxy.port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => return Err(failed(err.to_string())),
        Err(_) => {
            return Err(failed(format!(
                "no connection within {}s",
                PROXY_CONNECT_TIMEOUT.as_secs()
            )));
        }
    };
    let connect = format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n");
    if let Err(err) = stream.write_all(connect.as_bytes()).await {
        return Err(failed(err.to_string()));
    }
    let answer = match tokio::time::timeout(PROXY_CONNECT_TIMEOUT, read_answer(&mut stream)).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(err)) => return Err(failed(err.to_string())),
        Err(_) => {
            return Err(failed(format!(
                "no answer to CONNECT within {}s",
                PROXY_CONNECT_TIMEOUT.as_secs()
            )));
        }
    };
    let status = answer.lines().next().unwrap_or_default();
    if !status.contains(" 200") {
        return Err(failed(format!("CONNECT was answered `{status}`")));
    }
    if let Err(err) = stream.write_all(&client_hello(host)).await {
        return Err(failed(err.to_string()));
    }
    let mut byte = [0u8; 1];
    match tokio::time::timeout(REACH_TIMEOUT, stream.read(&mut byte)).await {
        Ok(Ok(0)) => Err(failed("the tunnel closed without a byte of TLS".to_owned())),
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(failed(err.to_string())),
        Err(_) => Err(failed(format!(
            "no TLS answer within {}s",
            REACH_TIMEOUT.as_secs()
        ))),
    }
}

/// The proxy's `CONNECT` answer, up to and including its blank line: a header left in the
/// socket would be read back as if the far end had sent it.
async fn read_answer(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while answer.len() < CONNECT_ANSWER_LIMIT {
        if stream.read(&mut byte).await? == 0 {
            break;
        }
        answer.push(byte[0]);
        if answer.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&answer).into_owned())
}

/// A minimal TLS 1.2 `ClientHello` for `host`.
///
/// Nothing is verified and nothing is derived from it: the only question is whether the far
/// end answers at all, and any server answers a well-formed hello with a `ServerHello` or an
/// alert. Written out by hand to keep a TLS stack out of the dependency tree of a health
/// check — a server that got this far is reachable, which is all [`reach_through`] reports.
fn client_hello(host: &str) -> Vec<u8> {
    let name = host.as_bytes();
    let mut server_name = Vec::new();
    server_name.extend_from_slice(&((name.len() + 3) as u16).to_be_bytes());
    server_name.push(0); // host_name
    server_name.extend_from_slice(&(name.len() as u16).to_be_bytes());
    server_name.extend_from_slice(name);

    let mut extensions = Vec::new();
    extensions.extend_from_slice(&[0x00, 0x00]); // server_name
    extensions.extend_from_slice(&(server_name.len() as u16).to_be_bytes());
    extensions.extend_from_slice(&server_name);

    let suites: [u16; 6] = [0xc02b, 0xc02f, 0x009c, 0x009d, 0x002f, 0x0035];
    let mut body = Vec::new();
    body.extend_from_slice(&[0x03, 0x03]); // TLS 1.2
    body.extend_from_slice(&[0x5a; 32]); // random: fixed, nothing is derived from it
    body.push(0); // no session id
    body.extend_from_slice(&((suites.len() * 2) as u16).to_be_bytes());
    for suite in suites {
        body.extend_from_slice(&suite.to_be_bytes());
    }
    body.extend_from_slice(&[0x01, 0x00]); // one compression method: none
    body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
    body.extend_from_slice(&extensions);

    let mut handshake = vec![0x01];
    handshake.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]); // 3-byte length
    handshake.extend_from_slice(&body);

    let mut record = vec![0x16, 0x03, 0x01]; // handshake, TLS 1.0 record version
    record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
    record.extend_from_slice(&handshake);
    record
}

/// A proxy URL as the CLIs are given it, split up so it can be probed.
#[derive(Debug)]
struct ProxyUrl {
    /// As the environment or the registry wrote it, for error messages.
    url: String,
    /// Lowercased: `http` can be asked to `CONNECT`, the others need their own handshake.
    scheme: String,
    host: String,
    port: u16,
}

impl ProxyUrl {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (scheme, rest) = raw.split_once("://").unwrap_or(("http", raw));
        let scheme = scheme.to_ascii_lowercase();
        // `user:pass@host:port/path` → `host:port`
        let authority = rest.split(['/', '?', '#']).next()?;
        let authority = authority.rsplit('@').next()?;
        let default_port = match scheme.as_str() {
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
            scheme,
            host,
            port,
        })
    }
}

/// How long the Windows system proxy is trusted before the registry is read again: someone
/// switching nodes while the hub runs is picked up without restarting it.
const SYSTEM_PROXY_TTL: Duration = Duration::from_secs(45);

/// The Windows system proxy, cached for [`SYSTEM_PROXY_TTL`] with when it was read.
static SYSTEM_PROXY: std::sync::LazyLock<
    std::sync::Mutex<Option<(std::time::Instant, Option<String>)>>,
> = std::sync::LazyLock::new(Default::default);

/// `ProxyServer` from the user's Internet Settings when `ProxyEnable` is on, as a URL, read
/// at most every [`SYSTEM_PROXY_TTL`].
fn system_proxy() -> Option<String> {
    let now = std::time::Instant::now();
    {
        let cached = SYSTEM_PROXY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((read_at, value)) = cached.as_ref()
            && now.duration_since(*read_at) < SYSTEM_PROXY_TTL
        {
            return value.clone();
        }
    }
    let value = windows_proxy();
    *SYSTEM_PROXY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((now, value.clone()));
    value
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

    /// The length the TLS record at the front of `hello` announces, once its five-byte
    /// header is there.
    fn hello_record_len(hello: &[u8]) -> usize {
        match hello {
            [_, _, _, hi, lo, ..] => u16::from_be_bytes([*hi, *lo]) as usize,
            _ => 0,
        }
    }

    /// A listener standing in for a proxy: `answer` goes back once the `CONNECT` line has
    /// been read, then `payload` once the client's hello arrives. Without a payload the
    /// connection is dropped where a proxy with a dead route would leave it.
    ///
    /// It serves one connection after another: [`check_proxy`] first opens a port probe and
    /// only then the `CONNECT` the payload answers, so a single-accept stand-in would drop
    /// the second one.
    async fn fake_proxy(answer: &'static str, payload: Option<&'static [u8]>) -> ProxyUrl {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bound");
        let port = listener.local_addr().expect("address").port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut seen = Vec::new();
                    let mut byte = [0u8; 1];
                    while !seen.ends_with(b"\r\n\r\n") {
                        if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                            return;
                        }
                        seen.push(byte[0]);
                    }
                    socket.write_all(answer.as_bytes()).await.expect("answered");
                    let Some(payload) = payload else {
                        return;
                    };
                    // Drain the whole hello first: closing with bytes still unread resets the
                    // connection on Windows, and the payload would be lost with it.
                    let mut hello = Vec::new();
                    let mut buffer = [0u8; 256];
                    while hello.len() < 5 || hello.len() < 5 + hello_record_len(&hello) {
                        let read = socket.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        hello.extend_from_slice(&buffer[..read]);
                    }
                    socket.write_all(payload).await.expect("payload");
                    let _ = socket.shutdown().await;
                });
            }
        });
        ProxyUrl::parse(&format!("http://127.0.0.1:{port}")).expect("parsed")
    }

    #[tokio::test]
    async fn a_proxy_that_carries_tls_to_the_api_is_accepted() {
        let proxy = fake_proxy("HTTP/1.1 200 Connection established\r\n\r\n", Some(&[0x16])).await;
        let result = reach_through(&proxy, "api.example.test").await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn a_proxy_that_answers_connect_but_carries_nothing_is_reported() {
        // The failure that started this: `200 Connection established`, then silence.
        let proxy = fake_proxy("HTTP/1.1 200 Connection established\r\n\r\n", None).await;
        let err = reach_through(&proxy, "api.example.test")
            .await
            .expect_err("nothing came back");
        let message = err.to_string();
        assert!(message.contains("api.example.test"), "{message}");
        assert!(message.contains(&proxy.url), "{message}");
    }

    #[tokio::test]
    async fn a_proxy_that_refuses_connect_is_reported() {
        let proxy = fake_proxy("HTTP/1.1 403 Forbidden\r\n\r\n", None).await;
        let err = reach_through(&proxy, "api.example.test")
            .await
            .expect_err("refused");
        assert!(err.to_string().contains("403"), "{err}");
    }

    #[tokio::test]
    async fn a_proxy_only_account_refuses_the_direct_fallback() {
        let proxy = fake_proxy("HTTP/1.1 403 Forbidden\r\n\r\n", None).await;
        let env = [("HTTPS_PROXY".to_owned(), proxy.url.clone())];
        let host = "proxy-only.example.test";
        let err = check_proxy(&env, Some(host), EgressPolicy::ProxyOnly)
            .await
            .expect_err("proxy_only never falls back");
        let message = err.to_string();
        assert!(message.contains(host), "{message}");
        assert!(message.contains("allow_direct"), "{message}");
        assert!(message.contains("protect this account"), "{message}");
        // Nothing was remembered that would send a later launch straight out.
        assert!(!goes_direct(host), "DIRECT_HOSTS must stay untouched");
    }

    #[tokio::test]
    async fn a_remembered_direct_host_is_skipped_only_when_allowed() {
        let host = "fast-path.example.test";
        DIRECT_HOSTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(host.to_owned(), std::time::Instant::now());
        let carried = "HTTP/1.1 200 Connection established\r\n\r\n";
        let allowed = fake_proxy(carried, Some(&[0x16])).await;
        let env = [("HTTPS_PROXY".to_owned(), allowed.url.clone())];
        let extra = check_proxy(&env, Some(host), EgressPolicy::AllowDirect)
            .await
            .expect("the fast path bypasses the proxy");
        assert!(
            extra.iter().any(|(_, value)| value.contains(host)),
            "{extra:?}"
        );
        // proxy_only does not take the fast path: the proxy carries the traffic, so the
        // session runs through it and no direct exemption is handed out.
        let only = fake_proxy(carried, Some(&[0x16])).await;
        let env = [("HTTPS_PROXY".to_owned(), only.url.clone())];
        let extra = check_proxy(&env, Some(host), EgressPolicy::ProxyOnly)
            .await
            .expect("the proxy carries it");
        assert!(extra.is_empty(), "{extra:?}");
    }

    #[test]
    fn a_host_reached_directly_is_added_to_no_proxy() {
        let env = [("NO_PROXY".to_owned(), "localhost,127.0.0.1".to_owned())];
        assert_eq!(
            bypass_env(&env, "www.codebuddy.ai"),
            [(
                "NO_PROXY".to_owned(),
                "localhost,127.0.0.1,codebuddy.ai".to_owned()
            )]
        );
        let env = [("NO_PROXY".to_owned(), "localhost,codebuddy.ai".to_owned())];
        assert_eq!(
            bypass_env(&env, "www.codebuddy.ai")[0].1,
            "localhost,codebuddy.ai"
        );
    }

    /// Live: a proxy at 127.0.0.1:7890 whose route to the host is dead, and a host that
    /// answers directly. Run by hand with `--ignored` on a machine set up that way.
    #[tokio::test]
    #[ignore]
    async fn a_dead_proxy_route_falls_back_to_direct() {
        let env = [("HTTPS_PROXY".to_owned(), "http://127.0.0.1:7890".to_owned())];
        let extra = check_proxy(&env, Some("www.codebuddy.ai"), EgressPolicy::AllowDirect)
            .await
            .expect("falls back to direct");
        assert!(
            extra
                .iter()
                .any(|(_, value)| value.contains("codebuddy.ai")),
            "{extra:?}"
        );
    }

    #[test]
    fn the_hello_names_the_host_and_is_well_formed() {
        let hello = client_hello("api.example.test");
        assert_eq!(&hello[..3], &[0x16, 0x03, 0x01]);
        let record = u16::from_be_bytes([hello[3], hello[4]]) as usize;
        assert_eq!(record, hello.len() - 5);
        assert_eq!(hello[5], 0x01); // ClientHello
        let handshake = u32::from_be_bytes([0, hello[6], hello[7], hello[8]]) as usize;
        assert_eq!(handshake, record - 4);
        assert!(
            hello
                .windows(16)
                .any(|window| window == b"api.example.test")
        );
    }
}

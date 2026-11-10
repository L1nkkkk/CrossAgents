//! Hole-punched connections to a hub (docs/MULTI-NODE.md §14「内置打洞」), over iroh: a QUIC
//! connection dialled by the hub's public key, hole-punched when it can be and relayed when it
//! cannot.
//!
//! - The hub's side ([`serve`]) accepts connections on [`ALPN`]. Every stream starts with one
//!   JSON line, a [`Hello`]: `tunnel` carries a device token and, once the [`Gate`] accepts
//!   it, the stream becomes a plain byte pipe to the hub's `127.0.0.1` port; `redeem` turns a
//!   one-time pairing code into a new device and its token.
//! - The client's side ([`forward`]) listens on `127.0.0.1` and carries each local TCP
//!   connection over its own stream, so the GUI keeps talking to a loopback port, as it does
//!   through the SSH tunnel.
//!
//! The hub is never reachable without a token the hub itself issued: a stream whose first line
//! is not accepted is closed before any byte reaches the hub's port.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey, Watcher as _};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// The protocol on the wire. A new incompatible version gets a new ALPN.
pub const ALPN: &[u8] = b"crossagents/link/1";
/// The longest first line either side reads.
const MAX_LINE: usize = 8 * 1024;
/// How long a dial may take before the client gives up on it.
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the hub waits for the first line of a stream.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);

/// What a pairing code starts with: `ca1-` and base64url of an [`Invite`].
pub const CODE_PREFIX: &str = "ca1-";

/// What a pairing code carries. It is a credential until it is redeemed or expires: whoever
/// has it can pair one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// The hub's endpoint id (its public key).
    #[serde(rename = "e")]
    pub endpoint: String,
    /// The hub's home relay when the code was made: a hint, the id is enough to dial.
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// The hub uses its own relay (`relay`) instead of n0's: the client must use it too.
    #[serde(rename = "c", default, skip_serializing_if = "std::ops::Not::not")]
    pub custom_relay: bool,
    /// The hub's direct addresses when the code was made (its LAN addresses among them): on
    /// the same network these connect without any relay.
    #[serde(rename = "a", default, skip_serializing_if = "Vec::is_empty")]
    pub addrs: Vec<String>,
    /// The one-time secret.
    #[serde(rename = "s")]
    pub secret: String,
    /// The hub machine's host name, to name it on the client.
    #[serde(rename = "h", default)]
    pub host: String,
    /// When the code stops working (unix seconds).
    #[serde(rename = "x")]
    pub expires: i64,
}

impl Invite {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("an invite serializes");
        format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Reads a pasted code. Whitespace (a code broken over lines) is ignored.
    pub fn decode(code: &str) -> Result<Self> {
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let Some(body) = code.strip_prefix(CODE_PREFIX) else {
            bail!("not a CrossAgents pairing code (it starts with {CODE_PREFIX})");
        };
        let json = URL_SAFE_NO_PAD
            .decode(body)
            .context("the pairing code is damaged (not base64url)")?;
        let invite: Invite =
            serde_json::from_slice(&json).context("the pairing code is damaged")?;
        invite
            .endpoint
            .parse::<EndpointId>()
            .map_err(|e| anyhow!("the pairing code names no valid hub: {e}"))?;
        Ok(invite)
    }

    /// Where to dial.
    pub fn target(&self) -> Result<Target> {
        let mut target = Target::new(&self.endpoint, self.relay.as_deref())?;
        target.custom_relay = self.custom_relay && target.relay.is_some();
        target.addrs = self.addrs.iter().filter_map(|a| a.parse().ok()).collect();
        Ok(target)
    }
}

/// A hub to dial: its id and, as a hint, its relay.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: EndpointId,
    pub relay: Option<RelayUrl>,
    /// `relay` is the hub's own relay: dial with it as this side's relay too.
    pub custom_relay: bool,
    /// Direct addresses to try besides the relay.
    pub addrs: Vec<SocketAddr>,
}

impl Target {
    pub fn new(endpoint: &str, relay: Option<&str>) -> Result<Self> {
        let id = endpoint
            .trim()
            .parse::<EndpointId>()
            .map_err(|e| anyhow!("invalid endpoint id {endpoint:?}: {e}"))?;
        let relay = match relay.map(str::trim).filter(|r| !r.is_empty()) {
            Some(url) => Some(
                url.parse::<RelayUrl>()
                    .map_err(|e| anyhow!("invalid relay url {url:?}: {e}"))?,
            ),
            None => None,
        };
        Ok(Self {
            id,
            relay,
            custom_relay: false,
            addrs: Vec::new(),
        })
    }

    /// How a client reaching this hub binds.
    pub fn network(&self, proxy: Option<String>) -> Network {
        Network {
            proxy,
            relay: self.relay.clone().filter(|_| self.custom_relay),
        }
    }

    fn addr(&self) -> EndpointAddr {
        let mut addr = EndpointAddr::new(self.id);
        if let Some(relay) = &self.relay {
            addr = addr.with_relay_url(relay.clone());
        }
        for ip in &self.addrs {
            addr = addr.with_ip_addr(*ip);
        }
        addr
    }
}

/// The first line of every stream, client to hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Hello {
    Tunnel {
        token: String,
    },
    Redeem {
        secret: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        os: Option<String>,
    },
}

/// The hub's answer to a [`Hello`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    /// A short machine word for a refusal: `unauthorized`, `expired`, `bad_request`, `unavailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What a redeem gives: `{ device, token }` as `POST /v1/devices` answers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired: Option<serde_json::Value>,
}

/// Why the hub refused a stream.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// What the hub decides: which tokens may tunnel, and redeeming pairing codes.
#[async_trait::async_trait]
pub trait Gate: Send + Sync + 'static {
    async fn tunnel(&self, token: &str) -> Result<(), Refusal>;
    async fn redeem(
        &self,
        secret: &str,
        name: &str,
        os: Option<&str>,
    ) -> Result<serde_json::Value, Refusal>;
}

/// How an endpoint reaches the outside.
#[derive(Debug, Clone, Default)]
pub struct Network {
    /// An HTTP proxy for the relay (`http://127.0.0.1:7890`); none: `HTTPS_PROXY` and the
    /// like from the environment, when set.
    pub proxy: Option<String>,
    /// A self-hosted relay to use instead of n0's.
    pub relay: Option<RelayUrl>,
}

/// An endpoint with n0's relays (or `network.relay`) and address lookup, through the proxy.
pub async fn bind(secret: Option<SecretKey>, accept: bool, network: &Network) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0);
    builder = match network
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(proxy) => builder.proxy_url(
            proxy
                .parse()
                .map_err(|e| anyhow!("invalid proxy {proxy:?}: {e}"))?,
        ),
        None => builder.proxy_from_env(),
    };
    if let Some(relay) = &network.relay {
        builder = builder.relay_mode(iroh::RelayMode::custom([relay.clone()]));
    }
    if let Some(secret) = secret {
        builder = builder.secret_key(secret);
    }
    if accept {
        builder = builder.alpns(vec![ALPN.to_vec()]);
    }
    builder
        .bind()
        .await
        .map_err(|e| anyhow!("cannot open the hole-punching endpoint: {e}"))
}

/// The endpoint's direct addresses (LAN and, once known, public ones).
pub fn direct_addrs(endpoint: &Endpoint) -> Vec<String> {
    endpoint.addr().ip_addrs().map(|a| a.to_string()).collect()
}

/// The hub's relay as it stands: its url and whether the endpoint is connected to it.
pub fn relay_status(endpoint: &Endpoint) -> Option<(String, bool)> {
    endpoint
        .home_relay_status()
        .get()
        .first()
        .map(|status| (status.url().to_string(), status.is_connected()))
}

// ------------------------------------------------------------------------------------------
// The hub's side.

/// Accepts connections until `cancel` fires, piping accepted `tunnel` streams to `hub`.
pub async fn serve(
    endpoint: Endpoint,
    hub: SocketAddr,
    gate: Arc<dyn Gate>,
    cancel: CancellationToken,
) {
    loop {
        let incoming = tokio::select! {
            _ = cancel.cancelled() => break,
            incoming = endpoint.accept() => incoming,
        };
        let Some(incoming) = incoming else { break };
        let gate = gate.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let connection = match incoming.accept() {
                Ok(accepting) => match accepting.await {
                    Ok(connection) => connection,
                    Err(err) => {
                        tracing::debug!(error = %err, "link: handshake failed");
                        return;
                    }
                },
                Err(err) => {
                    tracing::debug!(error = %err, "link: incoming refused");
                    return;
                }
            };
            let peer = connection.remote_id();
            tracing::info!(%peer, "link: peer connected");
            loop {
                let stream = tokio::select! {
                    _ = cancel.cancelled() => {
                        connection.close(0u32.into(), b"hub stopping");
                        break;
                    }
                    stream = connection.accept_bi() => stream,
                };
                let Ok((send, recv)) = stream else { break };
                let gate = gate.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_to_hub(send, recv, hub, gate.as_ref()).await {
                        tracing::debug!(%peer, error = %err, "link: stream ended");
                    }
                });
            }
            tracing::info!(%peer, "link: peer gone");
        });
    }
    endpoint.close().await;
}

async fn stream_to_hub(
    mut send: SendStream,
    recv: RecvStream,
    hub: SocketAddr,
    gate: &dyn Gate,
) -> Result<()> {
    let mut recv = BufReader::new(recv);
    let line = tokio::time::timeout(HELLO_TIMEOUT, read_line(&mut recv))
        .await
        .map_err(|_| anyhow!("no hello"))??;
    let hello: Hello = match serde_json::from_str(&line) {
        Ok(hello) => hello,
        Err(err) => {
            refuse(
                &mut send,
                Refusal::new("bad_request", format!("bad hello: {err}")),
            )
            .await;
            return Ok(());
        }
    };
    match hello {
        Hello::Redeem { secret, name, os } => {
            let reply = match gate.redeem(&secret, &name, os.as_deref()).await {
                Ok(paired) => Reply {
                    ok: true,
                    code: None,
                    error: None,
                    paired: Some(paired),
                },
                Err(refusal) => refusal_reply(refusal),
            };
            write_line(&mut send, &reply).await?;
            send.finish().ok();
            // Let the reply reach the client before the stream is dropped.
            let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
            Ok(())
        }
        Hello::Tunnel { token } => {
            if let Err(refusal) = gate.tunnel(&token).await {
                refuse(&mut send, refusal).await;
                return Ok(());
            }
            let mut tcp = match TcpStream::connect(hub).await {
                Ok(tcp) => tcp,
                Err(err) => {
                    refuse(
                        &mut send,
                        Refusal::new("unavailable", format!("the hub does not answer: {err}")),
                    )
                    .await;
                    return Ok(());
                }
            };
            write_line(
                &mut send,
                &Reply {
                    ok: true,
                    code: None,
                    error: None,
                    paired: None,
                },
            )
            .await?;
            let mut pipe = tokio::io::join(recv, send);
            tokio::io::copy_bidirectional(&mut pipe, &mut tcp).await?;
            Ok(())
        }
    }
}

fn refusal_reply(refusal: Refusal) -> Reply {
    Reply {
        ok: false,
        code: Some(refusal.code.into()),
        error: Some(refusal.message),
        paired: None,
    }
}

async fn refuse(send: &mut SendStream, refusal: Refusal) {
    tracing::info!(code = refusal.code, why = %refusal.message, "link: stream refused");
    if write_line(send, &refusal_reply(refusal)).await.is_ok() {
        send.finish().ok();
        let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
    }
}

async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<String> {
    let mut line = Vec::new();
    let mut limited = (&mut *reader).take(MAX_LINE as u64);
    limited.read_until(b'\n', &mut line).await?;
    if line.last() != Some(&b'\n') {
        bail!(if line.len() >= MAX_LINE {
            "first line too long"
        } else {
            "stream closed before the first line"
        });
    }
    String::from_utf8(line).context("first line is not UTF-8")
}

async fn write_line<T: Serialize>(send: &mut SendStream, value: &T) -> Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    send.write_all(&line).await?;
    Ok(())
}

// ------------------------------------------------------------------------------------------
// The client's side.

/// What the client tells whoever started it, one JSON line each on stdout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The local port is open.
    Listening { port: u16 },
    /// A connection to the hub is up; `direct` once hole punching won, else through a relay.
    Connected { direct: bool },
    /// The path changed (a relayed connection went direct, or back).
    Path { direct: bool },
    /// Dialling the hub failed; the next local connection tries again.
    Unreachable { error: String },
    /// The hub refused the token: the device was revoked. The client exits after this.
    Unauthorized { error: String },
}

/// Opens a connection and asks the hub to redeem a pairing code. Returns `{ device, token }`.
pub async fn redeem(
    invite: &Invite,
    name: &str,
    os: Option<&str>,
    proxy: Option<String>,
) -> Result<serde_json::Value> {
    let target = invite.target()?;
    let endpoint = bind(None, false, &target.network(proxy)).await?;
    let result = async {
        let connection = dial(&endpoint, &target).await?;
        let (mut send, recv) = connection.open_bi().await.context("cannot open a stream")?;
        write_line(
            &mut send,
            &Hello::Redeem {
                secret: invite.secret.clone(),
                name: name.into(),
                os: os.map(str::to_owned),
            },
        )
        .await?;
        let mut recv = BufReader::new(recv);
        let reply: Reply = serde_json::from_str(&read_line(&mut recv).await?)
            .context("the hub's reply is not JSON")?;
        connection.close(0u32.into(), b"done");
        if !reply.ok {
            bail!(
                "{}",
                reply
                    .error
                    .unwrap_or_else(|| "the hub refused the pairing code".into())
            );
        }
        reply.paired.context("the hub's reply has no token")
    }
    .await;
    endpoint.close().await;
    result
}

async fn dial(endpoint: &Endpoint, target: &Target) -> Result<Connection> {
    tokio::time::timeout(DIAL_TIMEOUT, endpoint.connect(target.addr(), ALPN))
        .await
        .map_err(|_| anyhow!("timed out reaching the hub ({}s)", DIAL_TIMEOUT.as_secs()))?
        .map_err(|e| anyhow!("cannot reach the hub: {e}"))
}

fn direct(connection: &Connection) -> bool {
    connection
        .paths()
        .iter()
        .any(|path| path.is_selected() && path.is_ip())
}

/// Listens on `listen` and carries every local connection to the hub, presenting `token`.
/// Returns when the hub refuses the token (the caller exits), or with an error when the
/// local port cannot be opened.
pub async fn forward(
    target: Target,
    token: String,
    listen: SocketAddr,
    proxy: Option<String>,
    events: impl Fn(Event) + Send + Sync + 'static,
) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("cannot listen on {listen}"))?;
    let port = listener.local_addr()?.port();
    let events: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(events);
    events(Event::Listening { port });
    let endpoint = bind(None, false, &target.network(proxy)).await?;
    let link = Arc::new(Link {
        endpoint,
        target,
        token,
        connection: Mutex::new(None),
        events: events.clone(),
        revoked: CancellationToken::new(),
    });
    // Dial once now, so a hub that cannot be reached shows before the first request.
    let _ = link.connection().await;
    loop {
        let accepted = tokio::select! {
            _ = link.revoked.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let Ok((tcp, _)) = accepted else { continue };
        let link = link.clone();
        tokio::spawn(async move { link.carry(tcp).await });
    }
    link.endpoint.close().await;
    Ok(())
}

struct Link {
    endpoint: Endpoint,
    target: Target,
    token: String,
    connection: Mutex<Option<Connection>>,
    events: Arc<dyn Fn(Event) + Send + Sync>,
    revoked: CancellationToken,
}

impl Link {
    /// The open connection, or a new one.
    async fn connection(&self) -> Option<Connection> {
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_ref()
            && connection.close_reason().is_none()
        {
            return Some(connection.clone());
        }
        match dial(&self.endpoint, &self.target).await {
            Ok(connection) => {
                (self.events)(Event::Connected {
                    direct: direct(&connection),
                });
                self.watch_path(connection.clone());
                *slot = Some(connection.clone());
                Some(connection)
            }
            Err(err) => {
                *slot = None;
                (self.events)(Event::Unreachable {
                    error: format!("{err:#}"),
                });
                None
            }
        }
    }

    fn watch_path(&self, connection: Connection) {
        let events = self.events.clone();
        tokio::spawn(async move {
            let mut last = direct(&connection);
            loop {
                tokio::select! {
                    _ = connection.closed() => break,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
                let now = direct(&connection);
                if now != last {
                    last = now;
                    events(Event::Path { direct: now });
                }
            }
        });
    }

    async fn carry(&self, mut tcp: TcpStream) {
        let Some(connection) = self.connection().await else {
            return;
        };
        let Ok((mut send, recv)) = connection.open_bi().await else {
            // The connection broke under us: forget it, the next local connection dials again.
            let mut slot = self.connection.lock().await;
            if slot
                .as_ref()
                .is_some_and(|c| c.stable_id() == connection.stable_id())
            {
                *slot = None;
            }
            return;
        };
        let hello = Hello::Tunnel {
            token: self.token.clone(),
        };
        if write_line(&mut send, &hello).await.is_err() {
            return;
        }
        let mut recv = BufReader::new(recv);
        let reply: Reply = match read_line(&mut recv).await.map(|l| serde_json::from_str(&l)) {
            Ok(Ok(reply)) => reply,
            _ => return,
        };
        if !reply.ok {
            if reply.code.as_deref() == Some("unauthorized") {
                (self.events)(Event::Unauthorized {
                    error: reply.error.unwrap_or_default(),
                });
                self.revoked.cancel();
            }
            return;
        }
        let mut pipe = tokio::io::join(recv, send);
        let _ = tokio::io::copy_bidirectional(&mut pipe, &mut tcp).await;
    }
}

/// Reads the hub's secret key from `path`, or makes one and writes it there.
pub fn load_or_create_key(path: &std::path::Path) -> Result<SecretKey> {
    if let Ok(text) = std::fs::read_to_string(path) {
        return text
            .trim()
            .parse::<SecretKey>()
            .map_err(|e| anyhow!("{} is not a key: {e}", path.display()));
    }
    let key = SecretKey::generate();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("partial");
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&partial, hex)?;
    std::fs::rename(&partial, path)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn invite() -> Invite {
        Invite {
            endpoint: SecretKey::generate().public().to_string(),
            relay: Some("https://relay.example./".into()),
            custom_relay: false,
            addrs: vec!["192.0.2.1:4433".into()],
            secret: "s3cret".into(),
            host: "CENTRE".into(),
            expires: 1_900_000_000,
        }
    }

    #[test]
    fn codes_round_trip_and_survive_line_breaks() {
        let invite = invite();
        let code = invite.encode();
        assert!(code.starts_with(CODE_PREFIX));
        assert_eq!(Invite::decode(&code).unwrap(), invite);
        let broken = format!("  {}\n{}  ", &code[..20], &code[20..]);
        assert_eq!(Invite::decode(&broken).unwrap(), invite);
        let target = invite.target().unwrap();
        assert!(target.relay.is_some());
        assert_eq!(target.addrs.len(), 1);
    }

    #[test]
    fn damaged_codes_are_refused() {
        assert!(Invite::decode("hello").is_err());
        assert!(Invite::decode("ca1-***").is_err());
        let mut bad = invite();
        bad.endpoint = "not-a-key".into();
        assert!(Invite::decode(&bad.encode()).is_err());
    }

    #[test]
    fn keys_persist() {
        let dir = std::env::temp_dir().join(format!("ca-link-key-{}", std::process::id()));
        let path = dir.join("link.key");
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create_key(&path).unwrap();
        let again = load_or_create_key(&path).unwrap();
        assert_eq!(first.public(), again.public());
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct Accepting;

    #[async_trait::async_trait]
    impl Gate for Accepting {
        async fn tunnel(&self, token: &str) -> Result<(), Refusal> {
            if token == "good" {
                Ok(())
            } else {
                Err(Refusal::new("unauthorized", "revoked"))
            }
        }
        async fn redeem(
            &self,
            secret: &str,
            name: &str,
            _os: Option<&str>,
        ) -> Result<serde_json::Value, Refusal> {
            if secret == "s3cret" {
                Ok(serde_json::json!({ "device": { "id": "d1", "name": name }, "token": "good" }))
            } else {
                Err(Refusal::new("expired", "used"))
            }
        }
    }

    /// Both sides on this machine, through the real endpoint (direct paths on loopback; no
    /// relay needed). Ignored by default: binding reaches n0's relays and DNS.
    #[tokio::test]
    #[ignore = "uses the network (n0 relays)"]
    async fn tunnel_end_to_end() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub = echo.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = tcp.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        let server = bind(Some(SecretKey::generate()), true, &Network::default())
            .await
            .unwrap();
        server.online().await;
        let id = server.id().to_string();
        let cancel = CancellationToken::new();
        tokio::spawn(serve(server, hub, Arc::new(Accepting), cancel.clone()));
        let mut invite = invite();
        invite.endpoint = id.clone();
        invite.relay = None;
        let paired = redeem(&invite, "laptop", None, None).await.unwrap();
        assert_eq!(paired["token"], "good");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(forward(
            Target::new(&id, None).unwrap(),
            "good".into(),
            "127.0.0.1:0".parse().unwrap(),
            None,
            move |e| {
                let _ = tx.send(e);
            },
        ));
        let Some(Event::Listening { port }) = rx.recv().await else {
            panic!()
        };
        let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        tcp.write_all(b"ping\n").await.unwrap();
        let mut buf = [0u8; 5];
        tcp.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping\n");
        cancel.cancel();
    }
}

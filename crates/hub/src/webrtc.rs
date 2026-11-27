//! Pure-Rust WebRTC DataChannel gateway (`str0m`) for low-latency P2P mobile connections.
//!
//! Signaling piggybacks on the authenticated hub API (`POST /v1/link/webrtc/offer`) — normally
//! reached over `iroh` E2EE relay — so no extra signaling server is required. Once ICE + DTLS +
//! SCTP succeed, the mobile PWA upgrades REST calls and SSE live events onto the ordered
//! `"crossagents-rpc"` DataChannel and transparently falls back to `iroh` on any drop.
//!
//! Security:
//! - Only human credentials (`Grant::is_person()`, `Level::Read`+) can open a WebRTC session;
//!   agent and node tokens are refused.
//! - Every DataChannel request is forwarded to the hub's own loopback HTTP listener with the
//!   caller's exact `Authorization` and `x-crossagents-client-os` headers, preserving 100% of
//!   existing route-level access control and audit logic.
//! - Paired device revocation cancels the session's `CancellationToken` immediately, closing the
//!   UDP loop and every active SSE stream.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, Method, header};
use axum::routing::post;
use axum::{Extension, Json, Router};
use futures::StreamExt;
use netdev::interface::state::OperState;
use netdev::interface::types::InterfaceType;
use serde::{Deserialize, Serialize};
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcError};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::access::Grant;
use crate::api::{ApiResult, AppError, AppState};

/// DataChannel label opened by the mobile PWA.
pub const CHANNEL_LABEL: &str = "crossagents-rpc";

/// Maximum concurrent active WebRTC sessions on the hub.
const MAX_SESSIONS: usize = 16;

/// Maximum local UDP sockets bound per session (2 IPv4 + 2 IPv6).
const MAX_V4_SOCKETS: usize = 2;
const MAX_V6_SOCKETS: usize = 2;

/// Maximum SCTP chunk payload size kept safely below Safari/WebKit's 16 KiB–64 KiB limits.
const CHUNK_BYTES: usize = 14_000;

/// Maximum reassembled inbound request body (matches 40 MiB attachment upload limits in base64).
const MAX_INBOUND_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Maximum concurrent SSE streams per WebRTC session.
const MAX_SSE_PER_SESSION: usize = 4;

/// Maximum concurrent in-flight chunked uploads per WebRTC session.
const MAX_PARTIAL_UPLOADS: usize = 8;

/// Idle timeout when no traffic flows and no SSE stream is open.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Grace period after ICE disconnects before tearing down the session.
const DISCONNECT_GRACE: Duration = Duration::from_secs(10);

/// Time budget for STUN server-reflexive discovery.
const STUN_TIMEOUT: Duration = Duration::from_millis(650);

/// Time budget for resolving LAN mDNS `.local` ICE candidates emitted by Safari/Chrome.
const MDNS_TIMEOUT: Duration = Duration::from_millis(700);

/// Verified dual-stack IPv4 + IPv6 STUN servers reachable from China Telecom/Unicom/Mobile
/// without VPN (tried in parallel; first valid reply wins).
const STUN_SERVERS: &[&str] = &[
    "stun.cloudflare.com:3478",
    "stun.nextcloud.com:443",
    "stun.hot-chilli.net:3478",
];

/// RFC 5389 STUN magic cookie.
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

#[derive(Debug, Deserialize)]
pub struct OfferRequest {
    pub sdp: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OfferResponse {
    pub sdp: String,
    /// Human-readable summary of local candidate kinds advertised in the SDP answer, e.g.
    /// `["host/ipv4", "host/ipv6", "srflx/ipv4"]` for client diagnostics.
    pub candidates: Vec<String>,
    /// Summary of remote candidate kinds accepted from the client's SDP offer (including
    /// any LAN mDNS `.local` hostnames resolved by the hub).
    #[serde(default)]
    pub remote_candidates: Vec<String>,
    /// True when the client's `srflx` IPv4 matches the hub's own public `srflx` IPv4 (same NAT/LAN).
    #[serde(default)]
    pub same_nat: bool,
}

struct SessionEntry {
    id: u64,
    cancel: CancellationToken,
}

/// Manages active WebRTC DataChannel sessions and provides the loopback HTTP client.
pub struct WebRtcService {
    sessions: Mutex<VecDeque<SessionEntry>>,
    next_id: AtomicU64,
    http: reqwest::Client,
    enable_stun: bool,
}

impl Default for WebRtcService {
    fn default() -> Self {
        Self::new()
    }
}

impl WebRtcService {
    pub fn new() -> Self {
        let enable_stun = std::env::var("CROSSAGENTS_WEBRTC_NO_STUN")
            .map(|v| v != "1" && !v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("loopback reqwest client");
        Self {
            sessions: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
            http,
            enable_stun,
        }
    }

    async fn register(&self, cancel: CancellationToken) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|s| !s.cancel.is_cancelled());
        while sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions.pop_front() {
                oldest.cancel.cancel();
            }
        }
        sessions.push_back(SessionEntry { id, cancel });
        id
    }

    async fn unregister(&self, id: u64) {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|s| s.id != id && !s.cancel.is_cancelled());
    }

    pub async fn stop_all(&self) {
        let mut sessions = self.sessions.lock().await;
        for entry in sessions.drain(..) {
            entry.cancel.cancel();
        }
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v1/link/webrtc/offer", post(handle_offer))
}

async fn handle_offer(
    State(state): State<Arc<AppState>>,
    Extension(grant): Extension<Grant>,
    headers: HeaderMap,
    Json(req): Json<OfferRequest>,
) -> ApiResult<Json<OfferResponse>> {
    if !grant.is_person() {
        return Err(AppError::forbidden(
            "only human or paired client credentials may open a WebRTC session",
        ));
    }
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| AppError::bad_request("missing Authorization header"))?;
    let client_os = headers
        .get(crossagents_core::DeviceOs::HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // Resolve any RFC 6762 mDNS `.local` candidates emitted by Safari/Chrome on LAN into real
    // IPv4/IPv6 addresses so Full-ICE can send outbound UDP probes (which also punch stateful
    // pinholes through Windows Defender Firewall without needing manual inbound firewall rules).
    let (resolved_sdp, remote_summary, remote_srflx_v4) =
        resolve_mdns_candidates_in_sdp(&req.sdp).await;

    let offer = SdpOffer::from_sdp_string(&resolved_sdp)
        .map_err(|err| AppError::bad_request(format!("invalid WebRTC SDP offer: {err}")))?;

    let device_cancel = match &grant {
        Grant::Device { id, .. } => state.devices.cancellation(id).await,
        _ => None,
    };

    let gathered = gather_sockets_and_candidates(state.webrtc.enable_stun).await;
    if gathered.sockets.is_empty() || gathered.candidates.is_empty() {
        return Err(AppError::internal(
            "no usable local UDP interfaces available for WebRTC",
        ));
    }

    let same_nat = gathered
        .srflx_v4
        .is_some_and(|hub_ip| remote_srflx_v4.contains(&hub_ip));

    let now = Instant::now();
    let mut rtc = Rtc::builder()
        .set_sctp_max_message_size(256 * 1024)
        .set_sctp_max_buffered_amount(4 * 1024 * 1024)
        .build(now);

    let mut summary = Vec::new();
    for (cand, label) in gathered.candidates {
        if rtc.add_local_candidate(cand).is_some() {
            summary.push(label);
        }
    }

    let answer = rtc
        .sdp_api()
        .accept_offer(offer)
        .map_err(|err| AppError::bad_request(format!("cannot accept WebRTC offer: {err}")))?;
    let answer_sdp = answer.to_sdp_string();

    let session_cancel = CancellationToken::new();
    let session_id = state.webrtc.register(session_cancel.clone()).await;

    let ctx = SessionContext {
        id: session_id,
        service: state.webrtc.clone(),
        hub_url: state.url.clone(),
        auth_header,
        client_os,
        shutdown: state.shutdown.clone(),
        device_cancel,
        session_cancel,
        stun_servers: gathered.stun_servers,
    };

    tracing::info!(
        session = session_id,
        local = ?summary,
        remote = ?remote_summary,
        same_nat,
        "webrtc: accepted offer"
    );

    tokio::spawn(async move {
        run_session(rtc, gathered.sockets, ctx).await;
    });

    Ok(Json(OfferResponse {
        sdp: answer_sdp,
        candidates: summary,
        remote_candidates: remote_summary,
        same_nat,
    }))
}

// ---------------------------------------------------------------------------
// Local interface enumeration + domestic STUN srflx discovery

struct Gathered {
    sockets: HashMap<SocketAddr, Arc<UdpSocket>>,
    candidates: Vec<(Candidate, String)>,
    srflx_v4: Option<Ipv4Addr>,
    stun_servers: HashSet<SocketAddr>,
}

async fn gather_sockets_and_candidates(enable_stun: bool) -> Gathered {
    let local_ips = tokio::task::spawn_blocking(select_local_ips)
        .await
        .unwrap_or_default();

    let mut sockets: HashMap<SocketAddr, Arc<UdpSocket>> = HashMap::new();
    let mut candidates: Vec<(Candidate, String)> = Vec::new();
    let mut primary_v4: Option<(SocketAddr, Arc<UdpSocket>)> = None;

    for ip in local_ips {
        match UdpSocket::bind(SocketAddr::new(ip, 0)).await {
            Ok(sock) => {
                if let Ok(local_addr) = sock.local_addr()
                    && let Ok(cand) = Candidate::host(local_addr, Protocol::Udp)
                {
                    let sock = Arc::new(sock);
                    if primary_v4.is_none() && local_addr.is_ipv4() && !local_addr.ip().is_loopback()
                    {
                        primary_v4 = Some((local_addr, sock.clone()));
                    }
                    let label = if local_addr.is_ipv6() {
                        "host/ipv6"
                    } else {
                        "host/ipv4"
                    };
                    sockets.insert(local_addr, sock);
                    candidates.push((cand, label.to_owned()));
                }
            }
            Err(err) => {
                tracing::debug!(%ip, error = %err, "webrtc: failed to bind candidate socket");
            }
        }
    }

    // Fallback to loopback when running in an isolated environment without routable NICs.
    if sockets.is_empty()
        && let Ok(sock) = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await
        && let Ok(local_addr) = sock.local_addr()
        && let Ok(cand) = Candidate::host(local_addr, Protocol::Udp)
    {
        sockets.insert(local_addr, Arc::new(sock));
        candidates.push((cand, "host/loopback".to_owned()));
    }

    let mut srflx_v4 = None;
    let mut stun_servers = HashSet::new();
    if enable_stun
        && let Some((base_addr, v4_sock)) = primary_v4
    {
        let (mapped, probed) = stun_discover_v4(&v4_sock).await;
        stun_servers = probed;
        if let Some(ext_addr) = mapped
            && ext_addr != base_addr
            && let Ok(cand) = Candidate::server_reflexive(ext_addr, base_addr, Protocol::Udp)
        {
            if let IpAddr::V4(v4) = ext_addr.ip() {
                srflx_v4 = Some(v4);
            }
            tracing::debug!(%base_addr, %ext_addr, "webrtc: discovered srflx candidate via STUN");
            candidates.push((cand, "srflx/ipv4".to_owned()));
        }
    }

    Gathered {
        sockets,
        candidates,
        srflx_v4,
        stun_servers,
    }
}

/// Resolves RFC 6762 `.local` mDNS hostnames in SDP `a=candidate:` lines via OS mDNS
/// (`tokio::net::lookup_host`) so `str0m` can send outbound ICE connectivity checks directly
/// to Safari/Chrome peers on the same LAN. Also returns a diagnostic summary of remote
/// candidate kinds and any `srflx` IPv4 addresses present in the offer.
async fn resolve_mdns_candidates_in_sdp(sdp: &str) -> (String, Vec<String>, HashSet<Ipv4Addr>) {
    // Collect unique `.local` hostnames first and resolve them in parallel.
    let mut mdns_hosts: Vec<String> = Vec::new();
    let mut seen_hosts: HashSet<String> = HashSet::new();
    for line in sdp.lines() {
        let trimmed = line.trim_end_matches('\r');
        if let Some(rest) = trimmed.strip_prefix("a=candidate:") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 8 {
                let addr = parts[4];
                if addr.ends_with(".local") && seen_hosts.insert(addr.to_owned()) {
                    mdns_hosts.push(addr.to_owned());
                }
            }
        }
    }

    let mut resolved_map: HashMap<String, Vec<IpAddr>> = HashMap::new();
    if !mdns_hosts.is_empty() {
        let futs = mdns_hosts.into_iter().map(|host| async move {
            let ips = tokio::time::timeout(
                MDNS_TIMEOUT,
                tokio::net::lookup_host((host.as_str(), 0u16)),
            )
            .await
            .ok()
            .and_then(|res| res.ok())
            .map(|iter| {
                let mut out = Vec::new();
                let mut seen = HashSet::new();
                for sa in iter {
                    let ip = sa.ip();
                    let ok = match ip {
                        IpAddr::V4(v4) => is_usable_ipv4(v4),
                        IpAddr::V6(v6) => is_global_unicast_ipv6(v6),
                    };
                    if ok && seen.insert(ip) {
                        out.push(ip);
                    }
                }
                out
            })
            .unwrap_or_default();
            (host, ips)
        });
        for (host, ips) in futures::future::join_all(futs).await {
            if !ips.is_empty() {
                resolved_map.insert(host, ips);
            }
        }
    }

    let line_ending = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out_lines: Vec<String> = Vec::new();
    let mut summary: Vec<String> = Vec::new();
    let mut srflx_v4: HashSet<Ipv4Addr> = HashSet::new();

    for line in sdp.lines() {
        let trimmed = line.trim_end_matches('\r');
        if let Some(rest) = trimmed.strip_prefix("a=candidate:") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 8 {
                let proto = parts[2].to_ascii_lowercase();
                let addr = parts[4];
                let kind = parts[7];
                if proto != "udp" {
                    out_lines.push(trimmed.to_owned());
                    continue;
                }
                if addr.ends_with(".local") {
                    if let Some(ips) = resolved_map.get(addr) {
                        for ip in ips {
                            let mut rewritten = parts.clone();
                            let ip_str = ip.to_string();
                            rewritten[4] = &ip_str;
                            out_lines.push(format!("a=candidate:{}", rewritten.join(" ")));
                            summary.push(format!(
                                "mdns->{}",
                                if ip.is_ipv6() { "ipv6" } else { "ipv4" }
                            ));
                        }
                    } else {
                        summary.push("mdns/unresolved".to_owned());
                        out_lines.push(trimmed.to_owned());
                    }
                    continue;
                } else if let Ok(ip) = addr.parse::<IpAddr>() {
                    if kind == "srflx"
                        && let IpAddr::V4(v4) = ip
                    {
                        srflx_v4.insert(v4);
                    }
                    summary.push(format!(
                        "{kind}/{}",
                        if ip.is_ipv6() { "ipv6" } else { "ipv4" }
                    ));
                }
            }
        }
        out_lines.push(trimmed.to_owned());
    }

    let mut rebuilt = out_lines.join(line_ending);
    if sdp.ends_with('\n') {
        rebuilt.push_str(line_ending);
    }
    (rebuilt, summary, srflx_v4)
}

/// Returns prioritized local IPv4 (up to `MAX_V4_SOCKETS`) and global IPv6 (up to
/// `MAX_V6_SOCKETS`) addresses for ICE host candidates.
fn select_local_ips() -> Vec<IpAddr> {
    let mut phys_v4: Vec<(u8, Ipv4Addr)> = Vec::new();
    let mut virt_v4: Vec<(u8, Ipv4Addr)> = Vec::new();
    let mut phys_v6: Vec<(u8, Ipv6Addr)> = Vec::new();
    let mut virt_v6: Vec<(u8, Ipv6Addr)> = Vec::new();
    let mut seen_v4: HashSet<Ipv4Addr> = HashSet::new();
    let mut seen_v6: HashSet<Ipv6Addr> = HashSet::new();

    for iface in netdev::get_interfaces() {
        if iface.oper_state != OperState::Up || iface.if_type == InterfaceType::Loopback {
            continue;
        }
        let virtual_nic = is_virtual_interface(&iface);
        let rank = if iface.default { 0u8 } else { 1u8 };

        for net in &iface.ipv4 {
            let ip = net.addr();
            if is_usable_ipv4(ip) && seen_v4.insert(ip) {
                if virtual_nic {
                    virt_v4.push((rank, ip));
                } else {
                    phys_v4.push((rank, ip));
                }
            }
        }
        for net in &iface.ipv6 {
            let ip = net.addr();
            if is_global_unicast_ipv6(ip) && seen_v6.insert(ip) {
                if virtual_nic {
                    virt_v6.push((rank, ip));
                } else {
                    phys_v6.push((rank, ip));
                }
            }
        }
    }

    let mut v4 = if !phys_v4.is_empty() { phys_v4 } else { virt_v4 };
    let mut v6 = if !phys_v6.is_empty() { phys_v6 } else { virt_v6 };
    v4.sort_by_key(|(rank, _)| *rank);
    v6.sort_by_key(|(rank, _)| *rank);

    let mut out = Vec::new();
    for (_, ip) in v4.into_iter().take(MAX_V4_SOCKETS) {
        out.push(IpAddr::V4(ip));
    }
    for (_, ip) in v6.into_iter().take(MAX_V6_SOCKETS) {
        out.push(IpAddr::V6(ip));
    }
    out
}

fn is_virtual_interface(iface: &netdev::Interface) -> bool {
    if matches!(
        iface.if_type,
        InterfaceType::Tunnel | InterfaceType::ProprietaryVirtual | InterfaceType::Bridge
    ) {
        return true;
    }
    let hay = format!(
        "{} {} {}",
        iface.name,
        iface.friendly_name.as_deref().unwrap_or(""),
        iface.description.as_deref().unwrap_or("")
    )
    .to_ascii_lowercase();
    [
        "vethernet",
        "wsl",
        "hyper-v",
        "docker",
        "vmware",
        "virtualbox",
        "vbox",
        "tailscale",
        "zerotier",
        "clash",
        "mihomo",
        "wintun",
        "utun",
        "tun0",
        "tap",
        "loopback",
        "teredo",
        "isatap",
    ]
    .iter()
    .any(|kw| hay.contains(kw))
}

fn is_usable_ipv4(ip: Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
    {
        return false;
    }
    // Skip RFC 2544 benchmarking range 198.18.0.0/15 (commonly used by Clash/Mihomo Fake-IP TUN).
    let octets = ip.octets();
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return false;
    }
    true
}

fn is_global_unicast_ipv6(ip: Ipv6Addr) -> bool {
    // Global unicast IPv6 is 2000::/3 (excludes loopback ::1, link-local fe80::/10, ULA fc00::/7,
    // multicast ff00::/8).
    let first = ip.segments()[0];
    (first & 0xe000) == 0x2000
}

/// Sends a lightweight RFC 5389 STUN Binding Request from `sock` to verified STUN servers in
/// parallel and returns `(mapped_addr, probed_servers)` within `STUN_TIMEOUT`.
async fn stun_discover_v4(sock: &UdpSocket) -> (Option<SocketAddr>, HashSet<SocketAddr>) {
    let Ok(tx_id): Result<[u8; 12], _> = uuid::Uuid::new_v4().as_bytes()[..12].try_into() else {
        return (None, HashSet::new());
    };
    let req = build_stun_binding_request(&tx_id);

    let mut targets = Vec::new();
    let resolve_futs = STUN_SERVERS.iter().map(|host| async move {
        tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::lookup_host(*host),
        )
        .await
        .ok()
        .and_then(|res| res.ok())
        .map(|iter| iter.filter(SocketAddr::is_ipv4).take(1).collect::<Vec<_>>())
        .unwrap_or_default()
    });
    for addrs in futures::future::join_all(resolve_futs).await {
        targets.extend(addrs);
    }
    let probed: HashSet<SocketAddr> = targets.iter().copied().collect();
    if targets.is_empty() {
        return (None, probed);
    }

    for target in &targets {
        let _ = sock.send_to(&req, target).await;
    }

    let deadline = tokio::time::Instant::now() + STUN_TIMEOUT;
    let mut buf = [0u8; 512];
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _src))) => {
                if let Some(mapped) = parse_stun_binding_response(&buf[..n], &tx_id)
                    && mapped.is_ipv4()
                {
                    return (Some(mapped), probed);
                }
            }
            _ => break,
        }
    }
    (None, probed)
}

fn build_stun_binding_request(tx_id: &[u8; 12]) -> [u8; 20] {
    let mut pkt = [0u8; 20];
    pkt[0..2].copy_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
    pkt[2..4].copy_from_slice(&0x0000u16.to_be_bytes()); // Length = 0
    pkt[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    pkt[8..20].copy_from_slice(tx_id);
    pkt
}

fn parse_stun_binding_response(pkt: &[u8], expected_tx_id: &[u8; 12]) -> Option<SocketAddr> {
    if pkt.len() < 20 {
        return None;
    }
    let msg_type = u16::from_be_bytes([pkt[0], pkt[1]]);
    if msg_type != 0x0101 {
        // Not Binding Success Response.
        return None;
    }
    let msg_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    let cookie = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
    if cookie != STUN_MAGIC_COOKIE || &pkt[8..20] != expected_tx_id {
        return None;
    }
    let end = (20 + msg_len).min(pkt.len());
    let mut pos = 20;
    let mut fallback_mapped: Option<SocketAddr> = None;

    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([pkt[pos], pkt[pos + 1]]);
        let attr_len = u16::from_be_bytes([pkt[pos + 2], pkt[pos + 3]]) as usize;
        pos += 4;
        if pos + attr_len > end {
            break;
        }
        let val = &pkt[pos..pos + attr_len];
        if attr_type == 0x0020 && attr_len >= 8 && val[1] == 0x01 {
            // XOR-MAPPED-ADDRESS IPv4
            let port = u16::from_be_bytes([val[2], val[3]]) ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
            let cookie_bytes = STUN_MAGIC_COOKIE.to_be_bytes();
            let ip = Ipv4Addr::new(
                val[4] ^ cookie_bytes[0],
                val[5] ^ cookie_bytes[1],
                val[6] ^ cookie_bytes[2],
                val[7] ^ cookie_bytes[3],
            );
            return Some(SocketAddr::new(IpAddr::V4(ip), port));
        } else if attr_type == 0x0001 && attr_len >= 8 && val[1] == 0x01 {
            // MAPPED-ADDRESS IPv4
            let port = u16::from_be_bytes([val[2], val[3]]);
            let ip = Ipv4Addr::new(val[4], val[5], val[6], val[7]);
            fallback_mapped = Some(SocketAddr::new(IpAddr::V4(ip), port));
        }
        // STUN attributes are padded to 4-byte boundaries.
        let padded = (attr_len + 3) & !3;
        pos += padded;
    }
    fallback_mapped
}

// ---------------------------------------------------------------------------
// Wire framing over the `"crossagents-rpc"` DataChannel

#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ClientFrame {
    #[serde(rename = "ping")]
    Ping { n: u64 },
    #[serde(rename = "req")]
    Req {
        id: u64,
        m: String,
        p: String,
        #[serde(default)]
        b: Option<String>,
    },
    #[serde(rename = "req_s")]
    ReqStart { id: u64, m: String, p: String },
    #[serde(rename = "req_c")]
    ReqChunk { id: u64, d: String },
    #[serde(rename = "req_e")]
    ReqEnd { id: u64 },
    #[serde(rename = "sse_open")]
    SseOpen {
        id: u64,
        #[serde(default)]
        since: Option<u64>,
    },
    #[serde(rename = "sse_close")]
    SseClose { id: u64 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "t")]
enum ServerFrame<'a> {
    #[serde(rename = "pong")]
    Pong { n: u64 },
    #[serde(rename = "res")]
    Res { id: u64, s: u16, b: &'a str },
    #[serde(rename = "res_s")]
    ResStart { id: u64, s: u16 },
    #[serde(rename = "res_c")]
    ResChunk { id: u64, d: &'a str },
    #[serde(rename = "res_e")]
    ResEnd { id: u64 },
    #[serde(rename = "err")]
    Err {
        id: u64,
        c: &'a str,
        m: &'a str,
    },
    #[serde(rename = "sse_data")]
    SseData { id: u64, d: &'a str },
    #[serde(rename = "sse_ds")]
    SseDataStart { id: u64 },
    #[serde(rename = "sse_dc")]
    SseDataChunk { id: u64, d: &'a str },
    #[serde(rename = "sse_de")]
    SseDataEnd { id: u64 },
    #[serde(rename = "sse_err")]
    SseErr {
        id: u64,
        c: &'a str,
        s: u16,
        m: &'a str,
    },
}

struct PartialRequest {
    method: String,
    path: String,
    body: String,
}

// ---------------------------------------------------------------------------
// Sans-I/O `str0m` session event loop

struct SessionContext {
    id: u64,
    service: Arc<WebRtcService>,
    hub_url: String,
    auth_header: String,
    client_os: Option<String>,
    shutdown: CancellationToken,
    device_cancel: Option<CancellationToken>,
    session_cancel: CancellationToken,
    stun_servers: HashSet<SocketAddr>,
}

struct IncomingPacket {
    data: Vec<u8>,
    source: SocketAddr,
    destination: SocketAddr,
}

/// Drains `rtc.poll_output()` until `Output::Timeout` strictly after every mutation.
fn drain_rtc(
    rtc: &mut Rtc,
) -> Result<(Vec<str0m::net::Transmit>, Vec<Event>, Instant), RtcError> {
    let mut transmits = Vec::new();
    let mut events = Vec::new();
    loop {
        match rtc.poll_output()? {
            Output::Timeout(timeout) => return Ok((transmits, events, timeout)),
            Output::Transmit(t) => transmits.push(t),
            Output::Event(e) => events.push(e),
        }
    }
}

async fn send_transmits(
    session_id: u64,
    sockets: &HashMap<SocketAddr, Arc<UdpSocket>>,
    transmits: Vec<str0m::net::Transmit>,
    tx_count: &mut u64,
    logged_send_err: &mut bool,
) {
    for t in transmits {
        let sock = sockets.get(&t.source).or_else(|| {
            sockets
                .iter()
                .find(|(addr, _)| addr.is_ipv4() == t.destination.is_ipv4())
                .map(|(_, s)| s)
        });
        if let Some(sock) = sock {
            match sock.send_to(&t.contents, t.destination).await {
                Ok(_) => {
                    *tx_count += 1;
                    if *tx_count == 1 {
                        tracing::info!(
                            session = session_id,
                            source = %t.source,
                            dest = %t.destination,
                            bytes = t.contents.len(),
                            "webrtc: first UDP packet sent"
                        );
                    }
                }
                Err(err) => {
                    if !*logged_send_err {
                        *logged_send_err = true;
                        tracing::warn!(
                            session = session_id,
                            source = %t.source,
                            dest = %t.destination,
                            error = %err,
                            "webrtc: UDP send_to failed"
                        );
                    }
                }
            }
        }
    }
}

async fn run_session(
    mut rtc: Rtc,
    sockets: HashMap<SocketAddr, Arc<UdpSocket>>,
    ctx: SessionContext,
) {
    let (udp_tx, mut udp_rx) = mpsc::channel::<IncomingPacket>(512);
    let mut recv_tasks = Vec::with_capacity(sockets.len());
    for (&local_addr, sock) in &sockets {
        let sock = sock.clone();
        let udp_tx = udp_tx.clone();
        let cancel = ctx.session_cancel.clone();
        let session_id = ctx.id;
        recv_tasks.push(tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    res = sock.recv_from(&mut buf) => {
                        match res {
                            Ok((n, source)) => {
                                if udp_tx.try_send(IncomingPacket {
                                    data: buf[..n].to_vec(),
                                    source,
                                    destination: local_addr,
                                }).is_err() {
                                    // Drop packet under extreme UDP burst rather than blocking recv.
                                }
                            }
                            Err(err) => {
                                // CRITICAL on Windows Winsock: any prior `send_to` to an unreachable
                                // ICE candidate returns ICMP Port Unreachable as WSAECONNRESET (10054)
                                // on the next `recv_from`. Never break the recv loop on UDP errors!
                                tracing::debug!(
                                    session = session_id,
                                    %local_addr,
                                    error = %err,
                                    "webrtc: ignoring transient UDP recv_from error"
                                );
                                if err.kind() == std::io::ErrorKind::InvalidInput {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }));
    }
    drop(udp_tx);

    let (out_tx, mut out_rx) = mpsc::channel::<String>(512);
    let mut out_queue: VecDeque<String> = VecDeque::new();
    let mut rpc_channel: Option<ChannelId> = None;
    let mut partials: HashMap<u64, PartialRequest> = HashMap::new();
    let mut sse_streams: HashMap<u64, CancellationToken> = HashMap::new();

    let mut last_activity = Instant::now();
    let mut disconnected_since: Option<Instant> = None;
    let mut udp_rx_count: u64 = 0;
    let mut udp_tx_count: u64 = 0;
    let mut logged_send_err = false;

    // 1. Drain output from `accept_offer` (maintaining str0m's single-mutation invariant).
    let (transmits, initial_events, _) = match drain_rtc(&mut rtc) {
        Ok(res) => res,
        Err(err) => {
            tracing::warn!(session = ctx.id, error = %err, "webrtc: initial poll failed");
            cleanup_session(&ctx, recv_tasks, sse_streams, udp_rx_count, udp_tx_count).await;
            return;
        }
    };
    send_transmits(
        ctx.id,
        &sockets,
        transmits,
        &mut udp_tx_count,
        &mut logged_send_err,
    )
    .await;

    // 2. CRITICAL: `str0m`'s `IceAgent::poll_timeout()` returns `None` (`not_happening()`, +24h)
    // until the VERY FIRST `Input::Timeout(now)` is fed! Feed an immediate tick now so Full-ICE
    // transitions `New -> Checking` and immediately begins sending outbound UDP STUN checks.
    if let Err(err) = rtc.handle_input(Input::Timeout(Instant::now())) {
        tracing::warn!(session = ctx.id, error = %err, "webrtc: initial timeout tick failed");
        cleanup_session(&ctx, recv_tasks, sse_streams, udp_rx_count, udp_tx_count).await;
        return;
    }
    let (kick_txs, kick_events, mut next_timeout) = match drain_rtc(&mut rtc) {
        Ok(res) => res,
        Err(err) => {
            tracing::warn!(session = ctx.id, error = %err, "webrtc: post-tick poll failed");
            cleanup_session(&ctx, recv_tasks, sse_streams, udp_rx_count, udp_tx_count).await;
            return;
        }
    };
    send_transmits(
        ctx.id,
        &sockets,
        kick_txs,
        &mut udp_tx_count,
        &mut logged_send_err,
    )
    .await;

    let mut boot_events = initial_events;
    boot_events.extend(kick_events);
    if !handle_events(
        boot_events,
        &mut rtc,
        &sockets,
        &mut next_timeout,
        &mut rpc_channel,
        &mut out_queue,
        &out_tx,
        &mut partials,
        &mut sse_streams,
        &mut last_activity,
        &mut disconnected_since,
        &mut udp_tx_count,
        &mut logged_send_err,
        &ctx,
    )
    .await
    {
        cleanup_session(&ctx, recv_tasks, sse_streams, udp_rx_count, udp_tx_count).await;
        return;
    }

    let mut idle_tick = tokio::time::interval(Duration::from_secs(5));
    idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let now = Instant::now();
        let sleep_dur = next_timeout.saturating_duration_since(now);
        let timer = tokio::time::sleep(sleep_dur);
        tokio::pin!(timer);

        let device_cancelled = async {
            match &ctx.device_cancel {
                Some(tok) => tok.cancelled().await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {
                send_graceful_close(ctx.id, &mut rtc, &sockets, &mut udp_tx_count, &mut logged_send_err).await;
                break;
            }
            _ = ctx.session_cancel.cancelled() => {
                send_graceful_close(ctx.id, &mut rtc, &sockets, &mut udp_tx_count, &mut logged_send_err).await;
                break;
            }
            _ = device_cancelled => {
                tracing::info!(session = ctx.id, "webrtc: device revoked, closing session");
                // Notify any open SSE streams with 401 immediately before sending DTLS CloseNotify.
                for &sse_id in sse_streams.keys() {
                    if let Ok(json) = serde_json::to_string(&ServerFrame::SseErr {
                        id: sse_id,
                        c: "unauthorized",
                        s: 401,
                        m: "设备授权已失效或被中心吊销",
                    }) {
                        out_queue.push_back(json);
                    }
                }
                let _ = flush_outbound(
                    ctx.id,
                    &mut rtc,
                    &sockets,
                    &mut next_timeout,
                    rpc_channel,
                    &mut out_queue,
                    &mut udp_tx_count,
                    &mut logged_send_err,
                )
                .await;
                send_graceful_close(ctx.id, &mut rtc, &sockets, &mut udp_tx_count, &mut logged_send_err).await;
                break;
            }
            Some(pkt) = udp_rx.recv() => {
                // Ignore late STUN replies from our own initial STUN discovery probe.
                if ctx.stun_servers.contains(&pkt.source) {
                    continue;
                }
                udp_rx_count += 1;
                if udp_rx_count == 1 {
                    tracing::info!(
                        session = ctx.id,
                        source = %pkt.source,
                        dest = %pkt.destination,
                        bytes = pkt.data.len(),
                        "webrtc: first peer UDP packet received"
                    );
                }
                let Ok(recv) = Receive::new(Protocol::Udp, pkt.source, pkt.destination, &pkt.data) else {
                    continue;
                };
                if let Err(err) = rtc.handle_input(Input::Receive(Instant::now(), recv)) {
                    tracing::debug!(session = ctx.id, error = %err, "webrtc: handle_input error");
                    break;
                }
                match drain_rtc(&mut rtc) {
                    Ok((txs, evs, t)) => {
                        next_timeout = t;
                        send_transmits(ctx.id, &sockets, txs, &mut udp_tx_count, &mut logged_send_err).await;
                        if !handle_events(
                            evs,
                            &mut rtc,
                            &sockets,
                            &mut next_timeout,
                            &mut rpc_channel,
                            &mut out_queue,
                            &out_tx,
                            &mut partials,
                            &mut sse_streams,
                            &mut last_activity,
                            &mut disconnected_since,
                            &mut udp_tx_count,
                            &mut logged_send_err,
                            &ctx,
                        ).await {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::debug!(session = ctx.id, error = %err, "webrtc: poll error");
                        break;
                    }
                }
            }
            Some(msg) = out_rx.recv() => {
                last_activity = Instant::now();
                out_queue.push_back(msg);
                while let Ok(more) = out_rx.try_recv() {
                    out_queue.push_back(more);
                    if out_queue.len() >= 256 {
                        break;
                    }
                }
                if !flush_outbound(
                    ctx.id,
                    &mut rtc,
                    &sockets,
                    &mut next_timeout,
                    rpc_channel,
                    &mut out_queue,
                    &mut udp_tx_count,
                    &mut logged_send_err,
                ).await {
                    break;
                }
            }
            () = &mut timer => {
                if let Err(err) = rtc.handle_input(Input::Timeout(Instant::now())) {
                    tracing::debug!(session = ctx.id, error = %err, "webrtc: timeout error");
                    break;
                }
                match drain_rtc(&mut rtc) {
                    Ok((txs, evs, t)) => {
                        next_timeout = t;
                        send_transmits(ctx.id, &sockets, txs, &mut udp_tx_count, &mut logged_send_err).await;
                        if !handle_events(
                            evs,
                            &mut rtc,
                            &sockets,
                            &mut next_timeout,
                            &mut rpc_channel,
                            &mut out_queue,
                            &out_tx,
                            &mut partials,
                            &mut sse_streams,
                            &mut last_activity,
                            &mut disconnected_since,
                            &mut udp_tx_count,
                            &mut logged_send_err,
                            &ctx,
                        ).await {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::debug!(session = ctx.id, error = %err, "webrtc: timeout poll error");
                        break;
                    }
                }
            }
            _ = idle_tick.tick() => {
                if let Some(since) = disconnected_since
                    && since.elapsed() >= DISCONNECT_GRACE
                {
                    tracing::info!(session = ctx.id, "webrtc: ICE disconnected past grace window");
                    break;
                }
                if sse_streams.is_empty() && last_activity.elapsed() >= IDLE_TIMEOUT {
                    tracing::info!(session = ctx.id, "webrtc: idle timeout");
                    break;
                }
            }
        }
    }

    cleanup_session(&ctx, recv_tasks, sse_streams, udp_rx_count, udp_tx_count).await;
}

async fn send_graceful_close(
    session_id: u64,
    rtc: &mut Rtc,
    sockets: &HashMap<SocketAddr, Arc<UdpSocket>>,
    tx_count: &mut u64,
    logged_send_err: &mut bool,
) {
    if rtc.close().is_ok()
        && let Ok((txs, _, _)) = drain_rtc(rtc)
    {
        send_transmits(session_id, sockets, txs, tx_count, logged_send_err).await;
    }
}

async fn cleanup_session(
    ctx: &SessionContext,
    recv_tasks: Vec<tokio::task::JoinHandle<()>>,
    sse_streams: HashMap<u64, CancellationToken>,
    udp_rx_count: u64,
    udp_tx_count: u64,
) {
    ctx.session_cancel.cancel();
    for (_, cancel) in sse_streams {
        cancel.cancel();
    }
    for task in recv_tasks {
        task.abort();
    }
    ctx.service.unregister(ctx.id).await;
    tracing::info!(
        session = ctx.id,
        udp_rx = udp_rx_count,
        udp_tx = udp_tx_count,
        "webrtc: session closed"
    );
}

#[allow(clippy::too_many_arguments)]
async fn handle_events(
    events: Vec<Event>,
    rtc: &mut Rtc,
    sockets: &HashMap<SocketAddr, Arc<UdpSocket>>,
    next_timeout: &mut Instant,
    rpc_channel: &mut Option<ChannelId>,
    out_queue: &mut VecDeque<String>,
    out_tx: &mpsc::Sender<String>,
    partials: &mut HashMap<u64, PartialRequest>,
    sse_streams: &mut HashMap<u64, CancellationToken>,
    last_activity: &mut Instant,
    disconnected_since: &mut Option<Instant>,
    udp_tx_count: &mut u64,
    logged_send_err: &mut bool,
    ctx: &SessionContext,
) -> bool {
    for ev in events {
        match ev {
            Event::Connected => {
                *last_activity = Instant::now();
                *disconnected_since = None;
                tracing::info!(session = ctx.id, "webrtc: DTLS connected");
            }
            Event::IceConnectionStateChange(state) => {
                tracing::info!(session = ctx.id, ?state, "webrtc: ICE state");
                match state {
                    IceConnectionState::Connected | IceConnectionState::Completed => {
                        *disconnected_since = None;
                    }
                    IceConnectionState::Disconnected => {
                        disconnected_since.get_or_insert_with(Instant::now);
                    }
                    IceConnectionState::New | IceConnectionState::Checking => {}
                }
            }
            Event::ChannelOpen(cid, label) => {
                *last_activity = Instant::now();
                tracing::info!(session = ctx.id, %label, "webrtc: DataChannel opened");
                if label == CHANNEL_LABEL || rpc_channel.is_none() {
                    *rpc_channel = Some(cid);
                    if let Some(mut ch) = rtc.channel(cid) {
                        ch.set_buffered_amount_low_threshold(64 * 1024);
                    }
                    match drain_rtc(rtc) {
                        Ok((txs, _, t)) => {
                            *next_timeout = t;
                            send_transmits(ctx.id, sockets, txs, udp_tx_count, logged_send_err)
                                .await;
                        }
                        Err(_) => return false,
                    }
                    if !flush_outbound(
                        ctx.id,
                        rtc,
                        sockets,
                        next_timeout,
                        *rpc_channel,
                        out_queue,
                        udp_tx_count,
                        logged_send_err,
                    )
                    .await
                    {
                        return false;
                    }
                }
            }
            Event::ChannelBufferedAmountLow(cid) => {
                if *rpc_channel == Some(cid)
                    && !flush_outbound(
                        ctx.id,
                        rtc,
                        sockets,
                        next_timeout,
                        *rpc_channel,
                        out_queue,
                        udp_tx_count,
                        logged_send_err,
                    )
                    .await
                {
                    return false;
                }
            }
            Event::ChannelData(data) => {
                *last_activity = Instant::now();
                if *rpc_channel == Some(data.id)
                    && let Ok(text) = std::str::from_utf8(&data.data)
                {
                    handle_client_frame(text, out_tx, partials, sse_streams, ctx);
                }
            }
            Event::ChannelClose(cid) => {
                if *rpc_channel == Some(cid) {
                    tracing::info!(session = ctx.id, "webrtc: RPC DataChannel closed by peer");
                    return false;
                }
            }
            Event::Closed => {
                return false;
            }
            _ => {}
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
async fn flush_outbound(
    session_id: u64,
    rtc: &mut Rtc,
    sockets: &HashMap<SocketAddr, Arc<UdpSocket>>,
    next_timeout: &mut Instant,
    rpc_channel: Option<ChannelId>,
    out_queue: &mut VecDeque<String>,
    udp_tx_count: &mut u64,
    logged_send_err: &mut bool,
) -> bool {
    let Some(cid) = rpc_channel else {
        return true;
    };
    while let Some(front) = out_queue.front() {
        let write_res = {
            let Some(mut ch) = rtc.channel(cid) else {
                return false;
            };
            ch.write(false, front.as_bytes())
        };
        match write_res {
            Ok(true) => {
                out_queue.pop_front();
                match drain_rtc(rtc) {
                    Ok((txs, _, t)) => {
                        *next_timeout = t;
                        send_transmits(session_id, sockets, txs, udp_tx_count, logged_send_err)
                            .await;
                    }
                    Err(_) => return false,
                }
            }
            Ok(false) => {
                // SCTP send buffer full: wait for `ChannelBufferedAmountLow`.
                match drain_rtc(rtc) {
                    Ok((txs, _, t)) => {
                        *next_timeout = t;
                        send_transmits(session_id, sockets, txs, udp_tx_count, logged_send_err)
                            .await;
                    }
                    Err(_) => return false,
                }
                break;
            }
            Err(err) => {
                tracing::debug!(error = %err, "webrtc: channel write error");
                return false;
            }
        }
    }
    true
}

fn handle_client_frame(
    raw: &str,
    out_tx: &mpsc::Sender<String>,
    partials: &mut HashMap<u64, PartialRequest>,
    sse_streams: &mut HashMap<u64, CancellationToken>,
    ctx: &SessionContext,
) {
    let frame: ClientFrame = match serde_json::from_str(raw) {
        Ok(f) => f,
        Err(err) => {
            tracing::debug!(session = ctx.id, error = %err, "webrtc: ignoring malformed frame");
            return;
        }
    };

    match frame {
        ClientFrame::Ping { n } => {
            if let Ok(json) = serde_json::to_string(&ServerFrame::Pong { n }) {
                let _ = out_tx.try_send(json);
            }
        }
        ClientFrame::Req { id, m, p, b } => {
            spawn_rpc_request(id, m, p, b, out_tx.clone(), ctx);
        }
        ClientFrame::ReqStart { id, m, p } => {
            if partials.len() >= MAX_PARTIAL_UPLOADS {
                send_err_frame(out_tx, id, "transport", "too many concurrent uploads");
                return;
            }
            partials.insert(
                id,
                PartialRequest {
                    method: m,
                    path: p,
                    body: String::new(),
                },
            );
        }
        ClientFrame::ReqChunk { id, d } => {
            let overflow = if let Some(entry) = partials.get_mut(&id) {
                if entry.body.len() + d.len() > MAX_INBOUND_BODY_BYTES {
                    true
                } else {
                    entry.body.push_str(&d);
                    false
                }
            } else {
                false
            };
            if overflow {
                partials.remove(&id);
                send_err_frame(out_tx, id, "http", "request body exceeds size limit");
            }
        }
        ClientFrame::ReqEnd { id } => {
            if let Some(entry) = partials.remove(&id) {
                spawn_rpc_request(
                    id,
                    entry.method,
                    entry.path,
                    Some(entry.body),
                    out_tx.clone(),
                    ctx,
                );
            }
        }
        ClientFrame::SseOpen { id, since } => {
            if let Some(prev) = sse_streams.remove(&id) {
                prev.cancel();
            }
            while sse_streams.len() >= MAX_SSE_PER_SESSION {
                if let Some((&oldest_id, _)) = sse_streams.iter().next()
                    && let Some(old) = sse_streams.remove(&oldest_id)
                {
                    old.cancel();
                }
            }
            let stream_cancel = ctx.session_cancel.child_token();
            sse_streams.insert(id, stream_cancel.clone());
            spawn_sse_proxy(id, since, stream_cancel, out_tx.clone(), ctx);
        }
        ClientFrame::SseClose { id } => {
            if let Some(cancel) = sse_streams.remove(&id) {
                cancel.cancel();
            }
        }
    }
}

fn validate_rpc_target(method: &str, path: &str) -> Result<Method, &'static str> {
    let m = match method {
        "GET" | "get" => Method::GET,
        "POST" | "post" => Method::POST,
        "DELETE" | "delete" => Method::DELETE,
        _ => return Err("unsupported HTTP method"),
    };
    if !path.starts_with("/v1/")
        || path.contains('\r')
        || path.contains('\n')
        || path.starts_with("/v1/link/webrtc/")
    {
        return Err("invalid API path");
    }
    Ok(m)
}

fn spawn_rpc_request(
    id: u64,
    method: String,
    path: String,
    body: Option<String>,
    out_tx: mpsc::Sender<String>,
    ctx: &SessionContext,
) {
    let parsed_method = match validate_rpc_target(&method, &path) {
        Ok(m) => m,
        Err(msg) => {
            send_err_frame(&out_tx, id, "http", msg);
            return;
        }
    };

    let http = ctx.service.http.clone();
    let url = format!("{}{}", ctx.hub_url, path);
    let auth = ctx.auth_header.clone();
    let os = ctx.client_os.clone();
    let cancel = ctx.session_cancel.clone();

    tokio::spawn(async move {
        let mut req = http
            .request(parsed_method, &url)
            .header(header::AUTHORIZATION, auth);
        if let Some(os) = os {
            req = req.header(crossagents_core::DeviceOs::HEADER, os);
        }
        if let Some(body) = body {
            req = req
                .header(header::CONTENT_TYPE, "application/json")
                .body(body);
        }

        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            res = req.send() => res,
        };

        match resp {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    res = resp.text() => res.unwrap_or_default(),
                };
                send_response_frames(&out_tx, id, status, &text).await;
            }
            Err(err) => {
                let msg = format!("loopback request failed: {err}");
                if let Ok(json) = serde_json::to_string(&ServerFrame::Err {
                    id,
                    c: "hub_offline",
                    m: &msg,
                }) {
                    let _ = out_tx.send(json).await;
                }
            }
        }
    });
}

fn spawn_sse_proxy(
    id: u64,
    since: Option<u64>,
    cancel: CancellationToken,
    out_tx: mpsc::Sender<String>,
    ctx: &SessionContext,
) {
    let http = ctx.service.http.clone();
    let query = match since {
        Some(n) if n > 0 => format!("since={n}"),
        _ => "live=true".to_owned(),
    };
    let url = format!("{}/v1/events?{}", ctx.hub_url, query);
    let auth = ctx.auth_header.clone();
    let os = ctx.client_os.clone();

    tokio::spawn(async move {
        let mut req = http
            .get(&url)
            .header(header::AUTHORIZATION, auth)
            .header(header::ACCEPT, "text/event-stream");
        if let Some(os) = os {
            req = req.header(crossagents_core::DeviceOs::HEADER, os);
        }

        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            res = req.send() => res,
        };

        let resp = match resp {
            Ok(r) => r,
            Err(err) => {
                send_sse_err(&out_tx, id, "transport", 0, &err.to_string()).await;
                return;
            }
        };

        let status = resp.status().as_u16();
        if status == 401 {
            send_sse_err(&out_tx, id, "unauthorized", 401, "设备授权已失效或被中心吊销").await;
            return;
        }
        if !resp.status().is_success() {
            send_sse_err(&out_tx, id, "http", status, &format!("事件流 HTTP {status}")).await;
            return;
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                next = stream.next() => next,
            };
            let Some(chunk) = chunk else {
                if !cancel.is_cancelled() {
                    send_sse_err(&out_tx, id, "transport", 0, "事件流已断开").await;
                }
                return;
            };
            let Ok(bytes) = chunk else {
                if !cancel.is_cancelled() {
                    send_sse_err(&out_tx, id, "transport", 0, "事件流读取中断").await;
                }
                return;
            };
            buf.extend_from_slice(&bytes);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let mut line = buf.drain(..=nl).collect::<Vec<u8>>();
                if line.ends_with(b"\n") {
                    line.pop();
                }
                if line.ends_with(b"\r") {
                    line.pop();
                }
                if let Ok(text) = std::str::from_utf8(&line)
                    && let Some(rest) = text.strip_prefix("data:")
                {
                    let payload = rest.trim_start();
                    if !payload.is_empty() && !send_sse_data_frames(&out_tx, id, payload).await {
                        return;
                    }
                }
            }
        }
    });
}

fn send_err_frame(out_tx: &mpsc::Sender<String>, id: u64, code: &str, msg: &str) {
    if let Ok(json) = serde_json::to_string(&ServerFrame::Err { id, c: code, m: msg }) {
        let _ = out_tx.try_send(json);
    }
}

async fn send_sse_err(
    out_tx: &mpsc::Sender<String>,
    id: u64,
    code: &str,
    status: u16,
    msg: &str,
) {
    if let Ok(json) = serde_json::to_string(&ServerFrame::SseErr {
        id,
        c: code,
        s: status,
        m: msg,
    }) {
        let _ = out_tx.send(json).await;
    }
}

async fn send_response_frames(out_tx: &mpsc::Sender<String>, id: u64, status: u16, body: &str) {
    if body.len() <= CHUNK_BYTES {
        if let Ok(json) = serde_json::to_string(&ServerFrame::Res {
            id,
            s: status,
            b: body,
        }) {
            let _ = out_tx.send(json).await;
        }
        return;
    }
    if let Ok(start) = serde_json::to_string(&ServerFrame::ResStart { id, s: status })
        && out_tx.send(start).await.is_err()
    {
        return;
    }
    for piece in split_utf8_chunks(body, CHUNK_BYTES) {
        if let Ok(chunk) = serde_json::to_string(&ServerFrame::ResChunk { id, d: piece })
            && out_tx.send(chunk).await.is_err()
        {
            return;
        }
    }
    if let Ok(end) = serde_json::to_string(&ServerFrame::ResEnd { id }) {
        let _ = out_tx.send(end).await;
    }
}

async fn send_sse_data_frames(out_tx: &mpsc::Sender<String>, id: u64, payload: &str) -> bool {
    if payload.len() <= CHUNK_BYTES {
        if let Ok(json) = serde_json::to_string(&ServerFrame::SseData { id, d: payload }) {
            return out_tx.send(json).await.is_ok();
        }
        return true;
    }
    if let Ok(start) = serde_json::to_string(&ServerFrame::SseDataStart { id })
        && out_tx.send(start).await.is_err()
    {
        return false;
    }
    for piece in split_utf8_chunks(payload, CHUNK_BYTES) {
        if let Ok(chunk) = serde_json::to_string(&ServerFrame::SseDataChunk { id, d: piece })
            && out_tx.send(chunk).await.is_err()
        {
            return false;
        }
    }
    if let Ok(end) = serde_json::to_string(&ServerFrame::SseDataEnd { id }) {
        return out_tx.send(end).await.is_ok();
    }
    true
}

/// Splits `s` into non-empty slices whose byte lengths are each `<= max_bytes`, always splitting
/// cleanly on UTF-8 character boundaries.
fn split_utf8_chunks(s: &str, max_bytes: usize) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let limit = max_bytes.max(4);
    let mut out = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let mut end = (start + limit).min(s.len());
        while end > start && !s.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            // Single char larger than limit cannot happen because limit >= 4, but stay safe.
            end = s.len();
        }
        out.push(&s[start..end]);
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xor_mapped_address_ipv4() {
        let tx_id = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let ext_ip = Ipv4Addr::new(183, 16, 14, 57);
        let ext_port: u16 = 54321;

        let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
        let xport = ext_port ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
        let ip_octets = ext_ip.octets();

        let mut pkt = Vec::new();
        pkt.extend_from_slice(&0x0101u16.to_be_bytes());
        pkt.extend_from_slice(&12u16.to_be_bytes());
        pkt.extend_from_slice(&cookie);
        pkt.extend_from_slice(&tx_id);
        // XOR-MAPPED-ADDRESS attribute
        pkt.extend_from_slice(&0x0020u16.to_be_bytes());
        pkt.extend_from_slice(&8u16.to_be_bytes());
        pkt.push(0x00);
        pkt.push(0x01); // IPv4
        pkt.extend_from_slice(&xport.to_be_bytes());
        for i in 0..4 {
            pkt.push(ip_octets[i] ^ cookie[i]);
        }

        let parsed = parse_stun_binding_response(&pkt, &tx_id);
        assert_eq!(parsed, Some(SocketAddr::new(IpAddr::V4(ext_ip), ext_port)));
        assert_eq!(parse_stun_binding_response(&pkt, &[0u8; 12]), None);
    }

    #[test]
    fn splits_multibyte_utf8_chunks_safely() {
        let s = "你好世界🌍CrossAgents低延迟直连测试".repeat(50);
        let chunks = split_utf8_chunks(&s, 23);
        assert!(chunks.len() > 10);
        for c in &chunks {
            assert!(c.len() <= 23);
        }
        assert_eq!(chunks.concat(), s);
    }

    #[test]
    fn rejects_invalid_rpc_paths_and_methods() {
        assert!(validate_rpc_target("GET", "/v1/agents").is_ok());
        assert!(validate_rpc_target("POST", "/v1/agents/a/messages").is_ok());
        assert!(validate_rpc_target("DELETE", "/v1/agents/a").is_ok());
        assert!(validate_rpc_target("PUT", "/v1/agents").is_err());
        assert!(validate_rpc_target("GET", "/etc/passwd").is_err());
        assert!(validate_rpc_target("POST", "/v1/link/webrtc/offer").is_err());
        assert!(validate_rpc_target("GET", "/v1/info\r\nX-Evil: 1").is_err());
    }

    #[test]
    fn filters_fake_ip_and_non_global_v6() {
        assert!(!is_usable_ipv4(Ipv4Addr::new(198, 18, 0, 1)));
        assert!(!is_usable_ipv4(Ipv4Addr::new(169, 254, 1, 2)));
        assert!(!is_usable_ipv4(Ipv4Addr::LOCALHOST));
        assert!(is_usable_ipv4(Ipv4Addr::new(192, 168, 1, 6)));

        assert!(is_global_unicast_ipv6("240e:3bd:1234::1".parse().unwrap()));
        assert!(!is_global_unicast_ipv6("fe80::1".parse().unwrap()));
        assert!(!is_global_unicast_ipv6("fd00::1".parse().unwrap()));
        assert!(!is_global_unicast_ipv6("::1".parse().unwrap()));
    }

    #[tokio::test]
    async fn summarizes_remote_sdp_candidates_and_handles_mdns() {
        let sdp = "v=0\r\n\
a=candidate:1 1 udp 2122260223 192.168.1.9 50000 typ host\r\n\
a=candidate:2 1 udp 1686052607 240e:3bd::9 50001 typ srflx raddr 0.0.0.0 rport 0\r\n\
a=candidate:3 1 udp 1686052606 183.16.14.57 50003 typ srflx raddr 0.0.0.0 rport 0\r\n\
a=candidate:4 1 udp 2122194687 nonexistent-test-host-xyz.local 50002 typ host\r\n";
        let (rebuilt, summary, srflx_v4) = resolve_mdns_candidates_in_sdp(sdp).await;
        assert!(rebuilt.contains("192.168.1.9 50000"));
        assert!(rebuilt.contains("240e:3bd::9 50001"));
        assert!(summary.contains(&"host/ipv4".to_owned()));
        assert!(summary.contains(&"srflx/ipv6".to_owned()));
        assert!(summary.contains(&"srflx/ipv4".to_owned()));
        assert!(summary.contains(&"mdns/unresolved".to_owned()));
        assert!(srflx_v4.contains(&Ipv4Addr::new(183, 16, 14, 57)));
    }

    /// Regression test: after `accept_offer`, feeding the initial `Input::Timeout(now)` tick MUST
    /// transition ICE out of `New` and immediately emit outbound STUN connectivity checks without
    /// waiting for any inbound UDP packet from the peer!
    #[test]
    fn initial_timeout_tick_emits_outbound_ice_checks_without_inbound_packets() {
        let now = Instant::now();
        let mut client = Rtc::builder().build(now);
        client
            .add_local_candidate(Candidate::host("198.51.100.10:50000".parse().unwrap(), Protocol::Udp).unwrap())
            .unwrap();
        let mut change = client.sdp_api();
        change.add_channel(CHANNEL_LABEL.to_owned());
        let (offer, _pending) = change.apply().unwrap();

        let mut server = Rtc::builder().build(now);
        server
            .add_local_candidate(Candidate::host("192.0.2.20:51000".parse().unwrap(), Protocol::Udp).unwrap())
            .unwrap();
        let _answer = server.sdp_api().accept_offer(offer).unwrap();
        let _ = drain_rtc(&mut server).unwrap();

        server.handle_input(Input::Timeout(now)).unwrap();
        let (txs, evs, next_timeout) = drain_rtc(&mut server).unwrap();

        assert!(
            !txs.is_empty(),
            "Full-ICE server must actively send outbound STUN checks after initial tick"
        );
        assert!(
            txs.iter().any(|t| t.destination == "198.51.100.10:50000".parse::<SocketAddr>().unwrap()),
            "expected outbound STUN probe to remote candidate, got: {:?}",
            txs.iter().map(|t| t.destination).collect::<Vec<_>>()
        );
        assert!(
            evs.iter().any(|e| matches!(e, Event::IceConnectionStateChange(IceConnectionState::Checking))),
            "expected ICE state transition to Checking"
        );
        assert!(
            next_timeout.saturating_duration_since(now) < Duration::from_secs(5),
            "next_timeout should be short ICE pacing interval, not 24h not_happening()"
        );
    }

    /// End-to-end real UDP `str0m` client ↔ real `crossagents-hub` `POST /v1/link/webrtc/offer`
    /// verifying: ping/pong, GET /v1/info RPC, large chunked response, SSE live event, and
    /// immediate disconnect upon paired device revocation!
    #[tokio::test]
    async fn str0m_client_roundtrips_rpc_sse_and_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::HubConfig::new(crate::Paths::new(dir.path().to_path_buf()), 0);
        cfg.detect_accounts = false;
        cfg.event_log = false;
        cfg.builtin_node = false;
        let hub = crate::start(cfg).await.unwrap();

        // Create a paired Read-level client device to verify both Level::Read signaling access
        // and instant teardown when revoked.
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let dev_resp: serde_json::Value = http
            .post(format!("http://{}/v1/devices", hub.addr))
            .bearer_auth(&hub.token)
            .json(&serde_json::json!({ "name": "test-iphone", "level": "read" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let dev_id = dev_resp["device"]["id"].as_str().unwrap().to_owned();
        let dev_token = dev_resp["token"].as_str().unwrap().to_owned();

        // Bind a real local UDP socket for the str0m test client (preferring a local IP from the
        // same selection pool as the hub, falling back to 127.0.0.1).
        let client_ip = select_local_ips()
            .into_iter()
            .find(IpAddr::is_ipv4)
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let client_sock = Arc::new(
            UdpSocket::bind(SocketAddr::new(client_ip, 0))
                .await
                .unwrap(),
        );
        let client_addr = client_sock.local_addr().unwrap();

        let mut client = Rtc::builder()
            .set_sctp_max_message_size(256 * 1024)
            .set_sctp_max_buffered_amount(4 * 1024 * 1024)
            .build(Instant::now());
        client
            .add_local_candidate(Candidate::host(client_addr, Protocol::Udp).unwrap())
            .unwrap();
        let mut change = client.sdp_api();
        let cid = change.add_channel(CHANNEL_LABEL.to_owned());
        let (offer, pending) = change.apply().unwrap();

        // Exchange offer/answer over HTTP with the read-only device token.
        let offer_res = http
            .post(format!("http://{}/v1/link/webrtc/offer", hub.addr))
            .bearer_auth(&dev_token)
            .json(&serde_json::json!({ "sdp": offer.to_sdp_string() }))
            .send()
            .await
            .unwrap();
        assert_eq!(offer_res.status(), 200);
        let answer_json: OfferResponse = offer_res.json().await.unwrap();
        assert!(!answer_json.candidates.is_empty());

        let answer = str0m::change::SdpAnswer::from_sdp_string(&answer_json.sdp).unwrap();
        client.sdp_api().accept_answer(pending, answer).unwrap();

        // Drive the client str0m state machine over real UDP sockets until DataChannel opens,
        // send ping + GET /v1/info + sse_open, and verify replies!
        let mut channel_open = false;
        let mut got_pong = false;
        let mut got_info = false;
        let mut got_sse = false;
        let mut sent_requests = false;
        let mut buf = vec![0u8; 4096];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "WebRTC test timed out: open={channel_open} pong={got_pong} info={got_info} sse={got_sse}"
            );

            let (txs, evs, next_t) = drain_rtc(&mut client).unwrap();
            for t in txs {
                let _ = client_sock.send_to(&t.contents, t.destination).await;
            }
            for ev in evs {
                match ev {
                    Event::ChannelOpen(id, _) if id == cid => {
                        channel_open = true;
                    }
                    Event::ChannelData(data) => {
                        let v: serde_json::Value =
                            serde_json::from_slice(&data.data).expect("valid JSON frame");
                        match v["t"].as_str() {
                            Some("pong") if v["n"].as_u64() == Some(42) => got_pong = true,
                            Some("res") if v["id"].as_u64() == Some(1) => {
                                assert_eq!(v["s"].as_u64(), Some(200));
                                let info: serde_json::Value =
                                    serde_json::from_str(v["b"].as_str().unwrap()).unwrap();
                                assert_eq!(info["version"].as_str(), Some(crate::VERSION));
                                got_info = true;
                            }
                            Some("sse_data") if v["id"].as_u64() == Some(2) => {
                                let ev_json: serde_json::Value =
                                    serde_json::from_str(v["d"].as_str().unwrap()).unwrap();
                                if ev_json["type"].as_str() == Some("agent.created")
                                    || ev_json["seq"].as_u64().is_some()
                                {
                                    got_sse = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            if channel_open && !sent_requests {
                sent_requests = true;
                for payload in [
                    r#"{"t":"ping","n":42}"#,
                    r#"{"t":"req","id":1,"m":"GET","p":"/v1/info"}"#,
                    r#"{"t":"sse_open","id":2,"since":0}"#,
                ] {
                    let ok = client
                        .channel(cid)
                        .unwrap()
                        .write(false, payload.as_bytes())
                        .unwrap();
                    assert!(ok);
                    let (txs, _, _) = drain_rtc(&mut client).unwrap();
                    for t in txs {
                        let _ = client_sock.send_to(&t.contents, t.destination).await;
                    }
                }
                // Trigger a real event on the hub bus after opening SSE.
                let bus = hub.bus.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    let _ = bus
                        .publish(crossagents_core::EventKind::HubStarted {
                            version: "webrtc-test".into(),
                        })
                        .await;
                });
            }

            if got_pong && got_info && got_sse {
                break;
            }

            let wait = next_t
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50));
            tokio::select! {
                res = client_sock.recv_from(&mut buf) => {
                    if let Ok((n, src)) = res
                        && let Ok(recv) = Receive::new(Protocol::Udp, src, client_addr, &buf[..n])
                    {
                        let _ = client.handle_input(Input::Receive(Instant::now(), recv));
                    }
                }
                () = tokio::time::sleep(wait) => {
                    let _ = client.handle_input(Input::Timeout(Instant::now()));
                }
            }
        }

        // Revoking the paired device must reject new offers and cancel the live session.
        let revoke_res = http
            .delete(format!("http://{}/v1/devices/{dev_id}", hub.addr))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap();
        assert_eq!(revoke_res.status(), 200);

        let denied = http
            .post(format!("http://{}/v1/link/webrtc/offer", hub.addr))
            .bearer_auth(&dev_token)
            .json(&serde_json::json!({ "sdp": "v=0" }))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 401);

        hub.shutdown();
        let _ = hub.wait().await;
    }
}

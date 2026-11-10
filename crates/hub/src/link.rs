//! The hub's built-in hole-punching entrance (docs/MULTI-NODE.md §14「内置打洞」): an iroh
//! endpoint that carries a paired client's streams to the hub's loopback port, and the
//! one-time pairing codes that pair such a client.
//!
//! Off until the person turns it on (`<home>/link.json`); the endpoint's key is
//! `<home>/link.key`, so the hub keeps its id across restarts and codes made before a restart
//! still name it (the codes themselves live in memory and do not survive one).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use chrono::{DateTime, Duration, Utc};
use crossagents_api_types::{LinkInvite, LinkState, LinkStatus};
use crossagents_core::{DeviceKind, DeviceLevel, DeviceOs};
use crossagents_link::{Gate, Invite, Network, Refusal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::api::AppState;

/// How long a pairing code works.
pub const INVITE_TTL_MINUTES: i64 = 30;
/// Codes waiting at once; making another drops the oldest.
const MAX_INVITES: usize = 16;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Config {
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay: Option<String>,
}

pub struct LinkService {
    home: PathBuf,
    inner: Mutex<Inner>,
    /// sha256(secret) → when it expires.
    invites: Mutex<HashMap<String, DateTime<Utc>>>,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    error: Option<String>,
}

struct Running {
    endpoint: iroh::Endpoint,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    proxy: Option<String>,
}

impl LinkService {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            inner: Mutex::default(),
            invites: Mutex::default(),
        }
    }

    fn config_file(&self) -> PathBuf {
        self.home.join("link.json")
    }

    fn key_file(&self) -> PathBuf {
        self.home.join("link.key")
    }

    fn config(&self) -> Config {
        std::fs::read_to_string(self.config_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, config: &Config) -> anyhow::Result<()> {
        let file = self.config_file();
        let partial = file.with_extension("json.partial");
        std::fs::write(&partial, serde_json::to_vec_pretty(config)?)?;
        std::fs::rename(&partial, &file)?;
        Ok(())
    }

    /// At hub start: brings the entrance up when the person left it on.
    pub async fn resume(&self, state: &Arc<AppState>, hub: SocketAddr) {
        let config = self.config();
        if config.enabled {
            self.start(state, hub, &config).await;
        }
    }

    /// Turns the entrance on or off and keeps the choice.
    pub async fn set(
        &self,
        state: &Arc<AppState>,
        hub: SocketAddr,
        enabled: bool,
        relay: Option<String>,
    ) -> anyhow::Result<LinkStatus> {
        let relay = relay.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
        if let Some(url) = &relay {
            url.parse::<iroh::RelayUrl>()
                .map_err(|e| anyhow::anyhow!("中继地址无效（{url}）：{e}"))?;
        }
        let config = Config { enabled, relay };
        self.save(&config)?;
        self.stop().await;
        if enabled {
            self.start(state, hub, &config).await;
        } else {
            self.invites.lock().await.clear();
        }
        Ok(self.status().await)
    }

    async fn start(&self, state: &Arc<AppState>, hub: SocketAddr, config: &Config) {
        let mut inner = self.inner.lock().await;
        inner.error = None;
        let proxy = hub_proxy();
        let network = Network {
            proxy: proxy.clone(),
            relay: config.relay.as_deref().and_then(|r| r.parse().ok()),
        };
        let endpoint = match crossagents_link::load_or_create_key(&self.key_file()) {
            Ok(key) => crossagents_link::bind(Some(key), true, &network).await,
            Err(err) => Err(err),
        };
        let endpoint = match endpoint {
            Ok(endpoint) => endpoint,
            Err(err) => {
                tracing::warn!(error = %err, "link: cannot start");
                inner.error = Some(format!("{err:#}"));
                return;
            }
        };
        tracing::info!(endpoint = %endpoint.id(), proxy = ?proxy.as_deref().map(redact), "link: entrance open");
        let cancel = CancellationToken::new();
        let gate: Arc<dyn Gate> = Arc::new(HubGate {
            state: Arc::downgrade(state),
        });
        let task = tokio::spawn(crossagents_link::serve(
            endpoint.clone(),
            hub,
            gate,
            cancel.clone(),
        ));
        inner.running = Some(Running {
            endpoint,
            cancel,
            task,
            proxy,
        });
    }

    /// Closes the entrance (hub stopping, or turned off).
    pub async fn stop(&self) {
        let running = self.inner.lock().await.running.take();
        if let Some(running) = running {
            running.cancel.cancel();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), running.task).await;
        }
    }

    pub async fn status(&self) -> LinkStatus {
        let config = self.config();
        let pending = {
            let mut invites = self.invites.lock().await;
            let now = Utc::now();
            invites.retain(|_, expires| *expires > now);
            invites.len() as u32
        };
        let inner = self.inner.lock().await;
        let mut status = LinkStatus {
            enabled: config.enabled,
            state: LinkState::Off,
            endpoint: None,
            relay: None,
            relay_connected: false,
            custom_relay: config.relay.clone(),
            proxy: None,
            error: inner.error.clone(),
            pending_invites: pending,
        };
        if let Some(running) = &inner.running {
            status.endpoint = Some(running.endpoint.id().to_string());
            status.proxy = running.proxy.as_deref().map(redact);
            if let Some((url, connected)) = crossagents_link::relay_status(&running.endpoint) {
                status.relay = Some(url);
                status.relay_connected = connected;
            }
            status.state = if status.relay_connected {
                LinkState::Online
            } else {
                LinkState::Starting
            };
        } else if config.enabled {
            status.state = LinkState::Failed;
        }
        status
    }

    /// A new one-time pairing code. Refused while the entrance is not running.
    pub async fn invite(&self, host: String) -> anyhow::Result<LinkInvite> {
        let endpoint = {
            let inner = self.inner.lock().await;
            let Some(running) = &inner.running else {
                anyhow::bail!("内置打洞没有开启：先在 设置 →「连接」里打开它");
            };
            running.endpoint.clone()
        };
        // Right after it opens the endpoint has no relay yet: give it a moment, so the code
        // carries where to find the hub. Without one the LAN addresses still work.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), endpoint.online()).await;
        let relay = crossagents_link::relay_status(&endpoint).map(|(url, _)| url);
        let addrs = crossagents_link::direct_addrs(&endpoint);
        let endpoint = endpoint.id().to_string();
        let config = self.config();
        let custom = config.relay.is_some();
        // 122 random bits: it only has to outlast 30 minutes of guessing over the relay.
        let secret = uuid::Uuid::new_v4().simple().to_string();
        let expires_at = Utc::now() + Duration::minutes(INVITE_TTL_MINUTES);
        {
            let mut invites = self.invites.lock().await;
            let now = Utc::now();
            invites.retain(|_, expires| *expires > now);
            while invites.len() >= MAX_INVITES {
                let oldest = invites
                    .iter()
                    .min_by_key(|(_, expires)| **expires)
                    .map(|(k, _)| k.clone());
                match oldest {
                    Some(key) => invites.remove(&key),
                    None => break,
                };
            }
            invites.insert(digest(&secret), expires_at);
        }
        let code = Invite {
            endpoint,
            relay: if custom { config.relay.clone() } else { relay },
            custom_relay: custom,
            addrs,
            secret,
            host,
            expires: expires_at.timestamp(),
        }
        .encode();
        Ok(LinkInvite { code, expires_at })
    }

    /// Takes a pairing code's secret: true once, while it has not expired.
    async fn take(&self, secret: &str) -> bool {
        let mut invites = self.invites.lock().await;
        match invites.remove(&digest(secret)) {
            Some(expires) => expires > Utc::now(),
            None => false,
        }
    }
}

fn digest(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

/// The proxy the hub reaches the relay through: its own environment's, else the system's
/// (the same one it hands the CLIs, `crossagents_drivers::process::system_proxy_env`).
fn hub_proxy() -> Option<String> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return Some(value.trim().to_owned());
        }
    }
    crossagents_drivers::process::system_proxy_env()
        .into_iter()
        .find(|(k, _)| k == "HTTPS_PROXY")
        .map(|(_, v)| v)
}

/// `scheme://host:port` without a password.
fn redact(proxy: &str) -> String {
    match proxy.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            format!("{scheme}://{}", host.trim_end_matches('/'))
        }
        None => proxy
            .rsplit_once('@')
            .map_or(proxy, |(_, host)| host)
            .to_owned(),
    }
}

struct HubGate {
    state: Weak<AppState>,
}

impl HubGate {
    fn state(&self) -> Result<Arc<AppState>, Refusal> {
        self.state
            .upgrade()
            .ok_or_else(|| Refusal::new("unavailable", "the hub is stopping"))
    }
}

#[async_trait::async_trait]
impl Gate for HubGate {
    async fn tunnel(&self, token: &str) -> Result<(), Refusal> {
        let state = self.state()?;
        match state
            .devices
            .authenticate(state.bus.store().as_ref(), token)
            .await
        {
            Ok(Some((device, _))) if device.kind == DeviceKind::Client => Ok(()),
            Ok(Some(_)) => Err(Refusal::new(
                "unauthorized",
                "only client devices connect this way",
            )),
            Ok(None) => Err(Refusal::new(
                "unauthorized",
                "this device was revoked or its token is not valid",
            )),
            Err(err) => Err(Refusal::new("unavailable", err.to_string())),
        }
    }

    async fn redeem(
        &self,
        secret: &str,
        name: &str,
        os: Option<&str>,
    ) -> Result<serde_json::Value, Refusal> {
        let state = self.state()?;
        if !state.link.take(secret).await {
            return Err(Refusal::new(
                "expired",
                "这个配对码已经用过、过期，或中心的 hub 重启过：在中心上重新生成一个",
            ));
        }
        let created = crate::api::issue_device(
            &state,
            name,
            DeviceKind::Client,
            DeviceLevel::Write,
            os.and_then(DeviceOs::parse_reported),
        )
        .await
        .map_err(|err| Refusal::new("bad_request", err.to_string()))?;
        tracing::info!(device = %created.device.name, "link: device paired by code");
        serde_json::to_value(&created).map_err(|err| Refusal::new("unavailable", err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_lose_their_password() {
        assert_eq!(
            redact("http://u:p@127.0.0.1:7890/"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(redact("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(redact("u:p@h:1"), "h:1");
    }

    #[tokio::test]
    async fn codes_are_taken_once_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let link = LinkService::new(dir.path());
        assert!(link.invite("x".into()).await.is_err(), "not running");
        link.invites
            .lock()
            .await
            .insert(digest("live"), Utc::now() + Duration::minutes(1));
        link.invites
            .lock()
            .await
            .insert(digest("old"), Utc::now() - Duration::minutes(1));
        assert!(link.take("live").await);
        assert!(!link.take("live").await, "a code works once");
        assert!(!link.take("old").await, "an expired code does not work");
        assert!(!link.take("never").await);
        assert_eq!(link.status().await.state, LinkState::Off);
    }
}

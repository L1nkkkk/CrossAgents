//! Cached device authentication. Database reads run outside the cache lock.
use crossagents_core::{CoreError, Device, Store};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
pub struct Devices {
    cache: Mutex<Cache>,
    ttl: Duration,
}
impl Default for Devices {
    fn default() -> Self {
        Self::with_ttl(Duration::from_secs(60))
    }
}
#[derive(Default)]
struct Cache {
    entries: HashMap<String, Cached>,
    generation: u64,
}
struct Cached {
    device: Device,
    seen: Instant,
    cancel: CancellationToken,
}
pub fn hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
impl Devices {
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            cache: Mutex::default(),
            ttl,
        }
    }

    pub async fn authenticate(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Option<(Device, CancellationToken)>, CoreError> {
        if !token.starts_with("cad_") {
            return Ok(None);
        }
        let hash = hash(token);
        loop {
            let generation = {
                let cache = self.cache.lock().await;
                if let Some(entry) = cache.entries.get(&hash) {
                    if entry.seen.elapsed() < self.ttl {
                        return Ok(Some((entry.device.clone(), entry.cancel.clone())));
                    }
                }
                cache.generation
            };
            let checked_at = Instant::now();
            let device = store.device_by_hash(&hash).await?;
            if let Some(device) = &device {
                store.touch_device(&device.id).await?;
            }
            let mut cache = self.cache.lock().await;
            if cache.generation != generation {
                continue;
            }
            let Some(device) = device else {
                if let Some(old) = cache.entries.remove(&hash) {
                    old.cancel.cancel();
                }
                return Ok(None);
            };
            let cancel = cache
                .entries
                .get(&hash)
                .map(|e| e.cancel.clone())
                .unwrap_or_default();
            cache.entries.insert(
                hash.clone(),
                Cached {
                    device: device.clone(),
                    seen: checked_at,
                    cancel: cancel.clone(),
                },
            );
            return Ok(Some((device, cancel)));
        }
    }
    pub async fn revoke(&self, store: &dyn Store, id: &str) -> Result<(Device, bool), CoreError> {
        let mut cache = self.cache.lock().await;
        let device = store.revoke_device(id).await?;
        cache.generation += 1;
        cache.entries.retain(|_, entry| {
            if entry.device.id == id {
                entry.cancel.cancel();
                false
            } else {
                true
            }
        });
        Ok(device)
    }
    pub async fn cancellation(&self, id: &str) -> Option<CancellationToken> {
        self.cache
            .lock()
            .await
            .entries
            .values()
            .find(|e| e.device.id == id)
            .map(|e| e.cancel.clone())
    }
}

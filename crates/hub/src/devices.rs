//! Cached device authentication. A revoke and a lookup serialize on the same lock.
use crossagents_core::{CoreError, Device, Store};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
#[derive(Default)]
pub struct Devices {
    cache: Mutex<HashMap<String, Cached>>,
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
    pub async fn authenticate(
        &self,
        store: &dyn Store,
        token: &str,
    ) -> Result<Option<(Device, CancellationToken)>, CoreError> {
        if !token.starts_with("cad_") {
            return Ok(None);
        }
        let hash = hash(token);
        let mut cache = self.cache.lock().await;
        if let Some(entry) = cache.get_mut(&hash) {
            if entry.seen.elapsed() >= Duration::from_secs(60) {
                store.touch_device(&entry.device.id).await?;
                entry.seen = Instant::now();
            }
            return Ok(Some((entry.device.clone(), entry.cancel.clone())));
        }
        let Some(device) = store.device_by_hash(&hash).await? else {
            return Ok(None);
        };
        store.touch_device(&device.id).await?;
        let cancel = CancellationToken::new();
        cache.insert(
            hash,
            Cached {
                device: device.clone(),
                seen: Instant::now(),
                cancel: cancel.clone(),
            },
        );
        Ok(Some((device, cancel)))
    }
    pub async fn revoke(&self, store: &dyn Store, id: &str) -> Result<Device, CoreError> {
        let mut cache = self.cache.lock().await;
        let device = store.revoke_device(id).await?;
        cache.retain(|_, entry| {
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
            .values()
            .find(|e| e.device.id == id)
            .map(|e| e.cancel.clone())
    }
}

//! Event bus: persist, then broadcast, then notify sinks.

use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};

use crate::error::CoreError;
use crate::event::{Event, EventKind, EventSink};
use crate::store::Store;

const CHANNEL_CAPACITY: usize = 1024;

pub struct EventBus {
    store: Arc<dyn Store>,
    tx: broadcast::Sender<Event>,
    sinks: Vec<Arc<dyn EventSink>>,
    /// Serializes append + broadcast so subscribers always see events in `seq` order.
    publish_lock: Mutex<()>,
}

impl EventBus {
    pub fn new(store: Arc<dyn Store>) -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            store,
            tx,
            sinks: Vec::new(),
            publish_lock: Mutex::new(()),
        }
    }

    pub fn with_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sinks.push(sink);
        self
    }

    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    pub async fn publish(&self, kind: EventKind) -> Result<Event, CoreError> {
        let event = {
            let _guard = self.publish_lock.lock().await;
            let event = self.store.append_event(kind).await?;
            // Having no subscribers is fine.
            let _ = self.tx.send(event.clone());
            event
        };
        for sink in &self.sinks {
            sink.handle(&event).await;
        }
        Ok(event)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::event::EVENT_SCHEMA_VERSION;
    use crate::ids::{AgentId, RunId, TaskId};
    use crate::model::{Agent, AgentStatus, Run, Task};
    use crate::store::AgentKey;

    #[derive(Default)]
    struct MemStore {
        events: StdMutex<Vec<Event>>,
    }

    #[async_trait]
    impl Store for MemStore {
        async fn insert_agent(&self, _: &Agent) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn get_agent(&self, _: &AgentKey) -> Result<Agent, CoreError> {
            unimplemented!()
        }
        async fn list_agents(&self) -> Result<Vec<Agent>, CoreError> {
            unimplemented!()
        }
        async fn delete_agent(&self, _: AgentId) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn update_agent_status(&self, _: AgentId, _: AgentStatus) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn append_event(&self, kind: EventKind) -> Result<Event, CoreError> {
            let mut events = self.events.lock().unwrap();
            let event = Event {
                seq: events.len() as u64 + 1,
                id: Uuid::now_v7(),
                ts: Utc::now(),
                schema: EVENT_SCHEMA_VERSION,
                kind,
            };
            events.push(event.clone());
            Ok(event)
        }
        async fn events_since(&self, after: u64, _: u32) -> Result<Vec<Event>, CoreError> {
            let events = self.events.lock().unwrap();
            Ok(events.iter().filter(|e| e.seq > after).cloned().collect())
        }
        async fn latest_event_seq(&self) -> Result<u64, CoreError> {
            Ok(self.events.lock().unwrap().len() as u64)
        }
        async fn insert_run(&self, _: &Run) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn get_run(&self, _: RunId) -> Result<Run, CoreError> {
            unimplemented!()
        }
        async fn list_runs(&self) -> Result<Vec<Run>, CoreError> {
            unimplemented!()
        }
        async fn update_run(&self, _: &Run) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn insert_task(&self, _: &Task) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn get_task(&self, _: TaskId) -> Result<Task, CoreError> {
            unimplemented!()
        }
        async fn list_tasks(&self, _: RunId) -> Result<Vec<Task>, CoreError> {
            unimplemented!()
        }
        async fn update_task(&self, _: &Task) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn insert_account(&self, _: &crate::Account) -> Result<(), CoreError> {
            unimplemented!()
        }
        async fn get_account(&self, _: &str) -> Result<crate::Account, CoreError> {
            unimplemented!()
        }
        async fn list_accounts(&self) -> Result<Vec<crate::Account>, CoreError> {
            unimplemented!()
        }
        async fn delete_account(&self, _: &str) -> Result<(), CoreError> {
            unimplemented!()
        }
    }

    #[derive(Default)]
    struct CountingSink(AtomicUsize);

    #[async_trait]
    impl EventSink for CountingSink {
        async fn handle(&self, _: &Event) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn publish_persists_broadcasts_and_notifies() {
        let store = Arc::new(MemStore::default());
        let sink = Arc::new(CountingSink::default());
        let bus = EventBus::new(store.clone()).with_sink(sink.clone());
        let mut rx = bus.subscribe();

        let first = bus
            .publish(EventKind::HubStarted {
                version: "0".into(),
            })
            .await
            .unwrap();
        let second = bus.publish(EventKind::HubStopping).await.unwrap();

        assert_eq!((first.seq, second.seq), (1, 2));
        assert_eq!(store.events_since(0, 10).await.unwrap().len(), 2);
        assert_eq!(rx.recv().await.unwrap(), first);
        assert_eq!(rx.recv().await.unwrap(), second);
        assert_eq!(sink.0.load(Ordering::SeqCst), 2);
    }
}

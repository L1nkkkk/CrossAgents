//! How frames travel (docs/tasks/B1-NODE.md §4).
//!
//! [`Transport`] is one connection: JSON lines in both directions, `None` when it closed.
//! The node accepts connections with a [`TransportServer`]. This step only ships the
//! in-memory pair; B1.3 adds a QUIC-stream implementation of the same two traits, so
//! neither [`crate::NodeService`] nor [`crate::RemoteDriver`] changes.
//!
//! An in-memory link can be dropped and made again: the hub side's connection is closed from
//! the test with [`MemoryHub::disconnect`], and [`MemoryHub`] refuses to connect while it is
//! offline, so the reconnection is the hub's decision, as it will be over the tunnel.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc, watch};

use crate::remote::HubLink;

/// A transport could not carry a frame, or has no connection to carry it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

/// One connection. Both methods take `&self` so a reader and a writer can share it.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send_line(&self, line: String) -> Result<(), TransportError>;
    /// The next line, or `None` once the other end went away.
    async fn recv_line(&self) -> Option<String>;
}

/// The node's side of a link: hands out one [`Transport`] per accepted connection.
#[async_trait]
pub trait TransportServer: Send + Sync {
    async fn accept(&self) -> Option<Arc<dyn Transport>>;
}

/// One end of an in-memory connection.
struct MemoryConn {
    tx: mpsc::UnboundedSender<String>,
    rx: Mutex<mpsc::UnboundedReceiver<String>>,
    closed: watch::Receiver<bool>,
}

#[async_trait]
impl Transport for MemoryConn {
    async fn send_line(&self, line: String) -> Result<(), TransportError> {
        if *self.closed.borrow() {
            return Err(TransportError("the connection is closed".into()));
        }
        self.tx
            .send(line)
            .map_err(|_| TransportError("the other end went away".into()))
    }

    async fn recv_line(&self) -> Option<String> {
        let mut rx = self.rx.lock().await;
        let mut closed = self.closed.clone();
        tokio::select! {
            line = rx.recv() => line,
            _ = closed.wait_for(|closed| *closed) => None,
        }
    }
}

struct MemoryNetwork {
    online: AtomicBool,
    /// The node's accepted connections; held here so the node also stops when the hub
    /// endpoint is dropped.
    pending: mpsc::UnboundedSender<Arc<dyn Transport>>,
    /// Closes the current connection on request (see [`MemoryHub::disconnect`]).
    close: Mutex<Option<watch::Sender<bool>>>,
}

/// The hub's end of an in-memory link. Cloneable; one link per node.
#[derive(Clone)]
pub struct MemoryHub {
    net: Arc<MemoryNetwork>,
}

/// The node's end of an in-memory link.
pub struct MemoryNode {
    accepted: Mutex<mpsc::UnboundedReceiver<Arc<dyn Transport>>>,
    /// Keeps the network (and so the pending sender) alive as long as the node runs.
    _net: Arc<MemoryNetwork>,
}

/// A hub link and the node it talks to.
pub fn memory_pair() -> (MemoryHub, MemoryNode) {
    let (pending, accepted) = mpsc::unbounded_channel();
    let net = Arc::new(MemoryNetwork {
        online: AtomicBool::new(true),
        pending,
        close: Mutex::new(None),
    });
    (
        MemoryHub { net: net.clone() },
        MemoryNode {
            accepted: Mutex::new(accepted),
            _net: net,
        },
    )
}

impl MemoryHub {
    /// A connection to the node. Fails while the link is offline, which is what makes
    /// "a call made while disconnected fails at once" testable.
    pub async fn connect_memory(&self) -> Result<Arc<dyn Transport>, TransportError> {
        if !self.net.online.load(Ordering::SeqCst) {
            return Err(TransportError("the memory link is offline".into()));
        }
        let (hub_tx, node_rx) = mpsc::unbounded_channel();
        let (node_tx, hub_rx) = mpsc::unbounded_channel();
        let (close_tx, close_rx) = watch::channel(false);
        let hub = Arc::new(MemoryConn {
            tx: hub_tx,
            rx: Mutex::new(hub_rx),
            closed: close_rx,
        });
        let node = Arc::new(MemoryConn {
            tx: node_tx,
            rx: Mutex::new(node_rx),
            closed: close_tx.subscribe(),
        });
        self.net
            .pending
            .send(node)
            .map_err(|_| TransportError("the node is gone".into()))?;
        *self.net.close.lock().await = Some(close_tx);
        Ok(hub)
    }

    /// Ends the current connection and makes further connects fail until
    /// [`Self::set_online`] (the test's "the node dropped off the network").
    pub async fn disconnect(&self) {
        self.net.online.store(false, Ordering::SeqCst);
        if let Some(close) = self.net.close.lock().await.take() {
            let _ = close.send(true);
        }
    }

    pub fn set_online(&self, online: bool) {
        self.net.online.store(online, Ordering::SeqCst);
    }
}

#[async_trait]
impl HubLink for MemoryHub {
    async fn connect(&self) -> Result<Arc<dyn Transport>, TransportError> {
        self.connect_memory().await
    }
}

#[async_trait]
impl TransportServer for MemoryNode {
    async fn accept(&self) -> Option<Arc<dyn Transport>> {
        self.accepted.lock().await.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_cross_the_memory_link() {
        let (hub, node) = memory_pair();
        let client = hub.connect_memory().await.unwrap();
        let server = node.accept().await.unwrap();
        client.send_line("hello\n".into()).await.unwrap();
        assert_eq!(server.recv_line().await.unwrap(), "hello\n");
        server.send_line("there\n".into()).await.unwrap();
        assert_eq!(client.recv_line().await.unwrap(), "there\n");
    }

    #[tokio::test]
    async fn disconnecting_ends_the_connection_and_refuses_new_ones() {
        let (hub, node) = memory_pair();
        let client = hub.connect_memory().await.unwrap();
        let _server = node.accept().await.unwrap();
        hub.disconnect().await;
        assert!(client.recv_line().await.is_none());
        assert!(hub.connect_memory().await.is_err());
        hub.set_online(true);
        let again = hub.connect_memory().await.unwrap();
        assert!(node.accept().await.is_some());
        drop(again);
    }
}

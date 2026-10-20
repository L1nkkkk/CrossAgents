//! JSON-RPC 2.0 over newline-delimited JSON, in both directions: we send requests and
//! notifications, and answer the peer's requests through an [`RpcHandler`].

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// Not a JSON-RPC code: the connection closed before a response arrived.
pub const CONNECTION_CLOSED: i64 = -32099;

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("method not found: {method}"))
    }

    pub fn invalid_params(err: impl fmt::Display) -> Self {
        Self::new(INVALID_PARAMS, format!("invalid params: {err}"))
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)?;
        if let Some(data) = &self.data {
            write!(f, ": {data}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RpcError {}

/// Answers requests and notifications coming from the peer.
#[async_trait]
pub trait RpcHandler: Send + Sync + 'static {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError>;
    /// Called in arrival order, before any later message is processed.
    async fn notification(&self, method: &str, params: Value);
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// One side of a JSON-RPC connection. Cheap to clone.
#[derive(Clone)]
pub struct RpcPeer {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: Arc<AtomicI64>,
}

impl RpcPeer {
    /// Starts reading from `reader` and writing to `writer`. The returned task ends when the
    /// peer closes its side; pending requests then fail with [`CONNECTION_CLOSED`].
    pub fn spawn<R, W>(reader: R, writer: W, handler: Arc<dyn RpcHandler>) -> (Self, JoinHandle<()>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out, out_rx) = mpsc::unbounded_channel();
        let peer = Self {
            out,
            pending: Arc::default(),
            next_id: Arc::new(AtomicI64::new(1)),
        };
        tokio::spawn(write_loop(writer, out_rx));
        let task = tokio::spawn(read_loop(reader, handler, peer.clone()));
        (peer, task)
    }

    pub async fn request<P: Serialize, T: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<T, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.out.send(message.to_string()).is_err() {
            self.pending.lock().await.remove(&id);
            return Err(closed());
        }
        let result = rx.await.map_err(|_| closed())??;
        serde_json::from_value(result)
            .map_err(|e| RpcError::new(INTERNAL_ERROR, format!("invalid {method} result: {e}")))
    }

    pub fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<(), RpcError> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.out.send(message.to_string()).map_err(|_| closed())
    }

    fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        let message = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        let _ = self.out.send(message.to_string());
    }
}

fn closed() -> RpcError {
    RpcError::new(CONNECTION_CLOSED, "connection closed")
}

async fn write_loop<W: AsyncWrite + Unpin>(mut writer: W, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        let written = async {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        };
        if let Err(err) = written.await {
            tracing::debug!(%err, "json-rpc write failed");
            return;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(reader: R, handler: Arc<dyn RpcHandler>, peer: RpcPeer) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                tracing::debug!(%err, "json-rpc read failed");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(err) => {
                tracing::warn!(%err, line, "ignoring malformed json-rpc message");
                continue;
            }
        };
        dispatch(message, &handler, &peer).await;
    }
    // Fail everything still waiting.
    for (_, tx) in peer.pending.lock().await.drain() {
        let _ = tx.send(Err(closed()));
    }
}

async fn dispatch(mut message: Value, handler: &Arc<dyn RpcHandler>, peer: &RpcPeer) {
    let id = message.get("id").cloned().filter(|id| !id.is_null());
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let params = message
        .get_mut("params")
        .map(Value::take)
        .unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => {
            // Requests may take a while (e.g. waiting for a human); never block the reader.
            let handler = handler.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                let result = handler.request(&method, params).await;
                peer.respond(id, result);
            });
        }
        (Some(method), None) => handler.notification(&method, params).await,
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(tx) = peer.pending.lock().await.remove(&id) else {
                tracing::debug!(id, "response to unknown request");
                return;
            };
            let result = match message.get_mut("error").map(Value::take) {
                Some(error) => Err(serde_json::from_value(error)
                    .unwrap_or_else(|e| RpcError::new(INTERNAL_ERROR, format!("bad error: {e}")))),
                None => Ok(message
                    .get_mut("result")
                    .map(Value::take)
                    .unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        (None, None) => tracing::warn!("ignoring json-rpc message without id or method"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echoes requests back; records notifications.
    #[derive(Default)]
    struct Echo {
        notes: Mutex<Vec<(String, Value)>>,
    }

    #[async_trait]
    impl RpcHandler for Echo {
        async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
            match method {
                "echo" => Ok(params),
                _ => Err(RpcError::method_not_found(method)),
            }
        }
        async fn notification(&self, method: &str, params: Value) {
            self.notes.lock().await.push((method.into(), params));
        }
    }

    fn pair() -> (RpcPeer, RpcPeer, Arc<Echo>, Arc<Echo>) {
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let ha = Arc::new(Echo::default());
        let hb = Arc::new(Echo::default());
        let (pa, _) = RpcPeer::spawn(ar, aw, ha.clone());
        let (pb, _) = RpcPeer::spawn(br, bw, hb.clone());
        (pa, pb, ha, hb)
    }

    #[tokio::test]
    async fn requests_round_trip_both_ways() {
        let (a, b, _, _) = pair();
        let r: Value = a.request("echo", json!({ "x": 1 })).await.unwrap();
        assert_eq!(r, json!({ "x": 1 }));
        let r: Value = b.request("echo", json!([2])).await.unwrap();
        assert_eq!(r, json!([2]));
    }

    #[tokio::test]
    async fn errors_are_returned() {
        let (a, _b, _, _) = pair();
        let err = a.request::<_, Value>("nope", json!({})).await.unwrap_err();
        assert_eq!(err.code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn notifications_arrive_in_order() {
        let (a, b, _, hb) = pair();
        for i in 0..20 {
            a.notify("note", json!(i)).unwrap();
        }
        // A request after the notifications forces them all through.
        let _: Value = a.request("echo", json!(null)).await.unwrap();
        let _: Value = b.request("echo", json!(null)).await.unwrap();
        let notes = hb.notes.lock().await;
        let values: Vec<_> = notes.iter().map(|(_, v)| v.as_i64().unwrap()).collect();
        assert_eq!(values, (0..20).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn pending_requests_fail_when_peer_closes() {
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (pa, _) = RpcPeer::spawn(ar, aw, Arc::new(Echo::default()));
        let request = tokio::spawn(async move { pa.request::<_, Value>("slow", json!({})).await });
        tokio::task::yield_now().await;
        drop(b);
        let err = request.await.unwrap().unwrap_err();
        assert_eq!(err.code, CONNECTION_CLOSED);
    }
}

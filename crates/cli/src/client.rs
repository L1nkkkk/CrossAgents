//! HTTP client for the hub's local API.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use crossagents_api_types::{ApiError, Event};
use crossagents_hub::{HubFile, Paths};
use futures::{Stream, StreamExt};
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub const NOT_RUNNING: &str = "hub is not running; start it with `crossagents hub start`";

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// Connects to the hub described by `hub.json`.
    pub fn connect(paths: &Paths) -> Result<Self> {
        let hub = paths.read_hub_file()?.ok_or_else(|| anyhow!(NOT_RUNNING))?;
        Self::for_hub(paths, &hub)
    }

    pub fn for_hub(paths: &Paths, hub: &HubFile) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .build()?,
            base: format!("http://127.0.0.1:{}", hub.port),
            token: paths.read_token()?,
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    async fn send(&self, req: RequestBuilder) -> Result<Response> {
        let resp = req.send().await.map_err(|e| {
            if e.is_connect() {
                anyhow!(NOT_RUNNING)
            } else {
                anyhow!(e)
            }
        })?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        match resp.json::<ApiError>().await {
            Ok(err) => bail!("{}", err.error.message),
            Err(_) => bail!("hub returned {status}"),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.send(self.request(Method::GET, path)).await?;
        resp.json().await.context("invalid response from hub")
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let resp = self
            .send(self.request(Method::POST, path).json(body))
            .await?;
        resp.json().await.context("invalid response from hub")
    }

    pub async fn post_empty(&self, path: &str) -> Result<()> {
        self.send(self.request(Method::POST, path)).await?;
        Ok(())
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        self.send(self.request(Method::DELETE, path)).await?;
        Ok(())
    }

    /// Streams events with `seq > since`; with `follow`, keeps streaming live events.
    pub async fn events(
        &self,
        since: u64,
        follow: bool,
    ) -> Result<impl Stream<Item = Result<Event>> + use<>> {
        let path = format!("/v1/events?since={since}&follow={follow}");
        let resp = self.send(self.request(Method::GET, &path)).await?;
        Ok(sse_events(resp.bytes_stream()))
    }
}

/// Parses `data:` lines of a server-sent event stream into events.
fn sse_events<S, B, E>(bytes: S) -> impl Stream<Item = Result<Event>>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: Into<anyhow::Error>,
{
    futures::stream::unfold(
        (bytes, String::new(), Vec::<Event>::new()),
        |(mut bytes, mut buf, mut ready)| async move {
            loop {
                if !ready.is_empty() {
                    let event = ready.remove(0);
                    return Some((Ok(event), (bytes, buf, ready)));
                }
                match bytes.next().await {
                    None => return None,
                    Some(Err(e)) => return Some((Err(e.into()), (bytes, buf, ready))),
                    Some(Ok(chunk)) => {
                        buf.push_str(
                            &String::from_utf8_lossy(chunk.as_ref()).replace("\r\n", "\n"),
                        );
                        while let Some(end) = buf.find("\n\n") {
                            let block: String = buf.drain(..end + 2).collect();
                            let data: Vec<&str> = block
                                .lines()
                                .filter_map(|l| l.strip_prefix("data:"))
                                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                                .collect();
                            if data.is_empty() {
                                continue; // keep-alive comment
                            }
                            match serde_json::from_str::<Event>(&data.join("\n")) {
                                Ok(event) => ready.push(event),
                                // Unknown event types from a newer hub are skipped.
                                Err(err) => tracing::debug!(%err, "skipping unparsable event"),
                            }
                        }
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_split_chunks_and_skips_keepalives() {
        let event = r#"{"seq":3,"id":"0192a0d4-0000-7000-8000-000000000000","ts":"2026-10-21T00:00:00Z","schema":1,"type":"hub.stopping"}"#;
        let raw = format!(":\n\nid: 3\nevent: hub.stopping\ndata: {event}\n\n");
        let (a, b) = raw.split_at(20);
        let chunks = vec![
            Ok::<_, std::io::Error>(a.as_bytes().to_vec()),
            Ok(b.as_bytes().to_vec()),
        ];
        let events: Vec<_> = sse_events(futures::stream::iter(chunks)).collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap().seq, 3);
    }
}

//! The terminal routes against real shells: typing and reading, coming back to a terminal, who may
//! type, what is reclaimed, and who may reach them at all.

use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use crossagents_api_types::{ListTerminalsResponse, TerminalFrame, TerminalInfo};
use crossagents_hub::{HubConfig, Paths, RunningHub};
use reqwest::{Client, Response, StatusCode};
use serde_json::json;
use tempfile::TempDir;

const LONG: Duration = Duration::from_secs(60);

fn shell() -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd.exe".into(), "/d".into()]
    } else {
        vec!["/bin/sh".into()]
    }
}

/// Typing a line runs it.
const ENTER: &str = if cfg!(windows) { "\r" } else { "\n" };

struct TestHub {
    hub: RunningHub,
    client: Client,
    base: String,
    home: TempDir,
}

impl TestHub {
    async fn start(idle: Duration) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut config = HubConfig::new(Paths::new(home.path()), 0);
        config.terminal_shell = Some(shell());
        config.terminal_idle = idle;
        let hub = crossagents_hub::start(config).await.unwrap();
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            client: Client::new(),
            base,
            home,
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.hub.token)
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.hub.token)
    }

    async fn create(&self) -> TerminalInfo {
        let resp = self
            .post("/v1/terminals")
            .json(&json!({ "cwd": self.home.path(), "project": "demo", "cols": 80, "rows": 24 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        resp.json().await.unwrap()
    }

    async fn read(&self, id: &str, connection: &str) -> Reader {
        let resp = self
            .get(&format!(
                "/v1/terminals/{id}/output?connection={connection}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        Reader {
            resp,
            pending: String::new(),
        }
    }

    async fn type_line(&self, id: &str, connection: &str, seq: u32, line: &str) -> StatusCode {
        self.post(&format!("/v1/terminals/{id}/input"))
            .json(
                &json!({ "connection": connection, "seq": seq, "data": format!("{line}{ENTER}") }),
            )
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn list(&self) -> Vec<TerminalInfo> {
        let list: ListTerminalsResponse = self
            .get("/v1/terminals")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        list.terminals
    }
}

/// One connection's frames.
struct Reader {
    resp: Response,
    pending: String,
}

impl Reader {
    async fn frame(&mut self) -> Option<TerminalFrame> {
        loop {
            if let Some(end) = self.pending.find("\n\n") {
                let block: String = self.pending.drain(..end + 2).collect();
                let data = block.lines().find_map(|l| l.strip_prefix("data: "));
                match data {
                    Some(data) => return Some(serde_json::from_str(data).unwrap()),
                    None => continue,
                }
            }
            let chunk = self.resp.chunk().await.unwrap()?;
            self.pending.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// Reads until the text seen so far has `needle`; returns all of it.
    async fn until(&mut self, needle: &str) -> String {
        let mut seen = String::new();
        let found = tokio::time::timeout(LONG, async {
            while let Some(frame) = self.frame().await {
                match frame {
                    TerminalFrame::Snapshot { data, .. } | TerminalFrame::Output { data } => {
                        seen.push_str(&String::from_utf8_lossy(&STANDARD.decode(data).unwrap()));
                    }
                    _ => {}
                }
                if seen.contains(needle) {
                    return true;
                }
            }
            false
        })
        .await;
        assert_eq!(found, Ok(true), "never saw {needle:?} in {seen:?}");
        seen
    }
}

async fn eventually(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let start = Instant::now();
    while !done().await {
        assert!(start.elapsed() < LONG, "{what} did not happen");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn typing_runs_in_the_shell_and_the_output_comes_back() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    assert_eq!(term.project.as_deref(), Some("demo"));
    assert_eq!((term.cols, term.rows), (80, 24));
    let mut out = hub.read(&term.id, "win-1").await;
    assert!(matches!(
        out.frame().await,
        Some(TerminalFrame::Snapshot { .. })
    ));
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "echo out-put-$((6*7))%OS%")
            .await,
        StatusCode::OK
    );
    out.until("out-put-").await;

    let resp = hub
        .post(&format!("/v1/terminals/{}/resize", term.id))
        .json(&json!({ "connection": "win-1", "cols": 100, "rows": 30 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let listed = hub.list().await;
    assert_eq!((listed[0].cols, listed[0].rows), (100, 30));
    assert_eq!(listed[0].connections, 1);
}

#[tokio::test]
async fn coming_back_shows_what_was_printed_while_away() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    let mut first = hub.read(&term.id, "win-1").await;
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "echo away-marker")
            .await,
        StatusCode::OK
    );
    first.until("away-marker").await;
    drop(first);
    eventually("the connection to go", async || {
        hub.list().await[0].connections == 0
    })
    .await;
    assert_eq!(
        hub.list().await.len(),
        1,
        "closing a window leaves the terminal"
    );

    let mut back = hub.read(&term.id, "win-2").await;
    let Some(TerminalFrame::Snapshot { data, .. }) = back.frame().await else {
        panic!("a connection starts with the screen");
    };
    let screen = String::from_utf8_lossy(&STANDARD.decode(data).unwrap()).into_owned();
    assert!(screen.contains("away-marker"), "{screen:?}");
    // And it is live.
    assert_eq!(
        hub.type_line(&term.id, "win-2", 1, "echo back-marker")
            .await,
        StatusCode::OK
    );
    back.until("back-marker").await;
}

#[tokio::test]
async fn the_newest_connection_types_until_another_takes_over() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    let mut first = hub.read(&term.id, "win-1").await;
    let _second = hub.read(&term.id, "win-2").await;
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "echo refused").await,
        StatusCode::CONFLICT
    );
    let resize = hub
        .post(&format!("/v1/terminals/{}/resize", term.id))
        .json(&json!({ "connection": "win-1", "cols": 90, "rows": 20 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resize.status(), StatusCode::CONFLICT);

    let taken = hub
        .post(&format!("/v1/terminals/{}/takeover", term.id))
        .json(&json!({ "connection": "win-1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(taken.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "echo taken-marker")
            .await,
        StatusCode::OK
    );
    let seen = first.until("taken-marker").await;
    assert!(
        !seen.contains("refused"),
        "refused input stays untyped: {seen:?}"
    );
    assert_eq!(
        hub.type_line(&term.id, "win-2", 2, "echo also-refused")
            .await,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn a_batch_sent_twice_is_typed_once() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    let mut out = hub.read(&term.id, "win-1").await;
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "echo first>>once.txt")
            .await,
        StatusCode::OK
    );
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "echo first>>once.txt")
            .await,
        StatusCode::OK
    );
    assert_eq!(
        hub.type_line(&term.id, "win-1", 2, "echo done-marker")
            .await,
        StatusCode::OK
    );
    out.until("done-marker").await;
    let path = hub.home.path().join("once.txt");
    eventually("the file", async || path.exists()).await;
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 1, "{text:?}");
}

#[tokio::test]
async fn a_shell_that_exits_keeps_its_output_and_code() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    let mut out = hub.read(&term.id, "win-1").await;
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "exit 3").await,
        StatusCode::OK
    );
    let code = tokio::time::timeout(LONG, async {
        while let Some(frame) = out.frame().await {
            if let TerminalFrame::Exit { code } = frame {
                return code;
            }
        }
        panic!("the stream ended without saying why");
    })
    .await
    .unwrap();
    assert_eq!(code, Some(3));
    let listed = hub.list().await;
    assert!(listed[0].exited);
    assert_eq!(listed[0].exit_code, Some(3));
    assert_eq!(
        hub.type_line(&term.id, "win-1", 2, "echo late").await,
        StatusCode::CONFLICT
    );

    let mut back = hub.read(&term.id, "win-2").await;
    assert!(matches!(
        back.frame().await,
        Some(TerminalFrame::Snapshot { .. })
    ));
    assert_eq!(
        back.frame().await,
        Some(TerminalFrame::Exit { code: Some(3) })
    );
    assert_eq!(back.frame().await, None);
}

#[tokio::test]
async fn closing_ends_a_terminal() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    let mut out = hub.read(&term.id, "win-1").await;
    let url = format!("{}/v1/terminals/{}", hub.base, term.id);
    let closed = hub
        .client
        .delete(&url)
        .bearer_auth(&hub.hub.token)
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), StatusCode::NO_CONTENT);
    assert!(hub.list().await.is_empty());
    tokio::time::timeout(LONG, async {
        while let Some(frame) = out.frame().await {
            if matches!(frame, TerminalFrame::Exit { .. }) {
                return;
            }
        }
    })
    .await
    .unwrap();
    let again = hub
        .client
        .delete(&url)
        .bearer_auth(&hub.hub.token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn left_alone_and_idle_is_reclaimed_but_watched_or_busy_is_not() {
    let hub = TestHub::start(Duration::from_secs(1)).await;

    // A silent command keeps its terminal though nobody is connected and nothing is printed.
    let busy = hub.create().await;
    let mut out = hub.read(&busy.id, "win-1").await;
    let sleep = if cfg!(windows) {
        "powershell -NoProfile -Command Start-Sleep 25"
    } else {
        "sleep 25"
    };
    assert_eq!(
        hub.type_line(&busy.id, "win-1", 1, sleep).await,
        StatusCode::OK
    );
    // Long enough for the command to have started before the connection goes.
    let _ = tokio::time::timeout(Duration::from_secs(5), out.until("never-printed")).await;
    drop(out);

    // A terminal someone is connected to stays too.
    let watched = hub.create().await;
    let _keep = hub.read(&watched.id, "win-2").await;

    let idle = hub.create().await;
    eventually("the idle terminal to be reclaimed", async || {
        !hub.list().await.iter().any(|t| t.id == idle.id)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let left: Vec<_> = hub.list().await.into_iter().map(|t| t.id).collect();
    assert!(left.contains(&busy.id), "something is running in it");
    assert!(left.contains(&watched.id), "someone is connected");
}

#[tokio::test]
async fn only_the_admin_token_reaches_terminals() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    for (method, path) in [
        ("GET", "/v1/terminals".to_string()),
        ("POST", "/v1/terminals".to_string()),
        ("DELETE", format!("/v1/terminals/{}", term.id)),
        (
            "GET",
            format!("/v1/terminals/{}/output?connection=x", term.id),
        ),
        ("POST", format!("/v1/terminals/{}/input", term.id)),
        ("POST", format!("/v1/terminals/{}/resize", term.id)),
        ("POST", format!("/v1/terminals/{}/takeover", term.id)),
    ] {
        let url = format!("{}{path}", hub.base);
        let request = |token: Option<&str>| {
            let builder = hub.client.request(method.parse().unwrap(), &url);
            match token {
                Some(token) => builder.bearer_auth(token),
                None => builder,
            }
        };
        let read = request(Some(&hub.hub.read_token)).send().await.unwrap();
        assert_eq!(
            read.status(),
            StatusCode::FORBIDDEN,
            "read token: {method} {path}"
        );
        let none = request(None).send().await.unwrap();
        assert_eq!(
            none.status(),
            StatusCode::UNAUTHORIZED,
            "no token: {method} {path}"
        );
    }
}

#[tokio::test]
async fn requests_outside_the_limits_are_refused() {
    let hub = TestHub::start(LONG).await;
    let home = hub.home.path().display().to_string();
    for body in [
        json!({ "cwd": home, "cols": 0, "rows": 24 }),
        json!({ "cwd": home, "cols": 80, "rows": 9999 }),
        json!({ "cwd": hub.home.path().join("no-such-dir"), "cols": 80, "rows": 24 }),
        json!({ "cwd": home, "cols": 80, "rows": 24, "node": "other" }),
    ] {
        let resp = hub.post("/v1/terminals").json(&body).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
    }
    let term = hub.create().await;
    let _out = hub.read(&term.id, "win-1").await;
    let big = hub
        .post(&format!("/v1/terminals/{}/input", term.id))
        .json(&json!({ "connection": "win-1", "seq": 1, "data": "x".repeat(70_000) }))
        .send()
        .await
        .unwrap();
    assert_eq!(big.status(), StatusCode::BAD_REQUEST);
    for n in 1..16 {
        let more = hub.create().await;
        assert_ne!(more.id, term.id, "{n}");
    }
    let over = hub
        .post("/v1/terminals")
        .json(&json!({ "cwd": home, "cols": 80, "rows": 24 }))
        .send()
        .await
        .unwrap();
    assert_eq!(over.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn stopping_the_hub_ends_its_terminals() {
    let hub = TestHub::start(LONG).await;
    let term = hub.create().await;
    let mut out = hub.read(&term.id, "win-1").await;
    assert_eq!(
        hub.type_line(&term.id, "win-1", 1, "echo up-marker").await,
        StatusCode::OK
    );
    out.until("up-marker").await;
    hub.hub.shutdown();
    tokio::time::timeout(LONG, async { while out.frame().await.is_some() {} })
        .await
        .expect("the stream ends with the hub");
}

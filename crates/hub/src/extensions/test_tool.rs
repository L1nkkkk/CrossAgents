//! A stand-in extension tool for the runner's tests (`tests/extensions_runner.rs`). It speaks
//! the protocol of docs/EXTENSIONS.md §3: one JSON on stdin, one JSON on stdout. What it does
//! is chosen by `input.mode`, so a test needs no interpreter on the machine. Not shipped.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("CROSSAGENTS_EXTENSION_DATA").unwrap_or_default())
}

fn main() {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).unwrap();
    let request: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let input = &request["input"];
    let mode = input["mode"].as_str().unwrap_or("echo");
    let env = |name: &str| std::env::var(name).ok();
    match mode {
        "echo" => {
            let out = json!({
                "request": request,
                "cwd": std::env::current_dir().unwrap().display().to_string(),
                "env": {
                    "url": env("CROSSAGENTS_URL"),
                    "token": env("CROSSAGENTS_TOKEN"),
                    "data": env("CROSSAGENTS_EXTENSION_DATA"),
                    "https_proxy": env("HTTPS_PROXY"),
                    "no_proxy": env("NO_PROXY"),
                    "agent_token": env("CROSSAGENTS_AGENT_TOKEN"),
                },
            });
            println!("{out}");
        }
        "version" => {
            let text = std::fs::read_to_string("version.txt").unwrap_or_default();
            println!("{}", json!({ "version": text.trim() }));
        }
        "sleep" => {
            let ms = input["ms"].as_u64().unwrap_or(1000);
            std::thread::sleep(Duration::from_millis(ms));
            println!("{}", json!({ "slept": ms }));
        }
        "big" => {
            let chunk = vec![b'a'; 64 * 1024];
            let mut out = std::io::stdout().lock();
            for _ in 0..64 {
                if out.write_all(&chunk).is_err() {
                    break;
                }
            }
            let _ = out.flush();
            std::thread::sleep(Duration::from_secs(30));
        }
        "fail" => {
            eprintln!("boom: {}", input["why"].as_str().unwrap_or("it broke"));
            std::process::exit(3);
        }
        "stderr" => {
            let mut err = std::io::stderr().lock();
            for n in 0..20_000 {
                let _ = writeln!(err, "line {n:06} xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
            }
            let _ = err.flush();
            drop(err);
            println!("{}", json!({ "ok": true }));
        }
        "badjson" => println!("this is not json"),
        "empty" => {}
        "tree" => {
            // A child that outlives nobody: it writes a heartbeat until it is killed.
            #[allow(clippy::zombie_processes)] // it is meant to outlive this block
            let mut child = Command::new(std::env::current_exe().unwrap())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let _ = stdin.write_all(br#"{"input":{"mode":"heartbeat"}}"#);
            drop(stdin);
            std::thread::sleep(Duration::from_secs(120));
        }
        "heartbeat" => loop {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(data_dir().join("heartbeat"))
            {
                let _ = file.write_all(b"x");
            }
            std::thread::sleep(Duration::from_millis(100));
        },
        "hold" => {
            // Leave the token where the test can read it, then wait to be released.
            let dir = data_dir();
            std::fs::write(
                dir.join("token.txt"),
                env("CROSSAGENTS_TOKEN").unwrap_or_default(),
            )
            .unwrap();
            for _ in 0..600 {
                if dir.join("release").exists() {
                    println!("{}", json!({ "held": true }));
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            std::process::exit(9);
        }
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}

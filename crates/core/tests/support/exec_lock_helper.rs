//! A second process for `tests/exec_locks.rs`.
//!
//! ```text
//! exec_lock_helper <locks dir> <resource path> <shared|exclusive>
//! ```
//!
//! Takes the lock, prints `ready <exec-id>`, and holds it until its stdin closes (or the test
//! kills it). The test uses it to prove that the locks are file locks and exclude across
//! processes, with no hub anywhere in the picture (`docs/tasks/LOCKS.md` §7 item 9).

use std::io::BufRead;
use std::path::PathBuf;

use crossagents_core::exec::{Access, Request, Resource, acquire};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let locks_dir = PathBuf::from(arguments.first().expect("the locks directory"));
    let path = PathBuf::from(arguments.get(1).expect("the resource path"));
    let access = match arguments.get(2).map(String::as_str) {
        Some("shared") => Access::Shared,
        _ => Access::Exclusive,
    };
    let request = Request {
        agent: Some("helper".to_owned()),
        ..Request::new("sleep 30", vec![Resource::new("build", path, access)])
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let held = runtime
        .block_on(acquire(&locks_dir, &request, |line| eprintln!("{line}")))
        .expect("take the lock");
    println!("ready {}", held.exec_id());

    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    drop(held);
}

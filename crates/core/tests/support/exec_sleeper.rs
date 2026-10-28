//! `exec_sleeper <milliseconds>`: a process that lives for a while.
//!
//! `tests/exec_tree.rs` uses it as a predecessor whose root process is still running while its
//! lock is gone (`docs/tasks/LOCKS.md` §5, conclusion 3).

fn main() {
    let millis: u64 = std::env::args()
        .nth(1)
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(millis));
}

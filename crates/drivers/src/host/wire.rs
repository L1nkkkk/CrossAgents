//! What the hub and a runtime host say to each other: one JSON object per line over a
//! loopback TCP connection. The host numbers what it sends (`seq`) and keeps it until the
//! hub acknowledges it, so a hub that reconnects gets what it missed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Sent by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToHost {
    /// First line of every connection.
    Hello { id: String, token: String },
    /// A line for the agent's stdin.
    In { line: String },
    /// Everything up to `seq` arrived; the host may forget it.
    Ack { seq: u64 },
    /// Closes the agent's stdin (how Claude Code is asked to finish).
    CloseStdin,
    /// Stops the agent's process tree.
    Kill,
}

/// Sent by the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromHost {
    /// Answers [`ToHost::Hello`]; frames not yet acknowledged follow.
    Hello { child_pid: Option<u32> },
    /// The hello was not accepted (wrong id or token).
    Refused { reason: String },
    /// A line the agent wrote to stdout.
    Out { seq: u64, line: String },
    /// A line the agent wrote to stderr.
    Err { seq: u64, line: String },
    /// The agent's process ended; nothing follows.
    Exit {
        seq: u64,
        code: Option<i32>,
        /// Ended by a [`ToHost::Kill`].
        killed: bool,
    },
}

impl FromHost {
    pub fn seq(&self) -> Option<u64> {
        match self {
            Self::Out { seq, .. } | Self::Err { seq, .. } | Self::Exit { seq, .. } => Some(*seq),
            Self::Hello { .. } | Self::Refused { .. } => None,
        }
    }
}

/// What to run, written by the hub to `<dir>/<id>.spec.json` for the host to read (and
/// delete) at start: it holds the agent's environment, which stays off the command line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSpec {
    pub id: String,
    /// Who may take the process over, e.g. `claude-native:claude:<session id>`.
    pub owner: String,
    /// For people: what runs, e.g. `codex app-server`.
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Variables of the host's own environment the agent must not inherit.
    pub env_remove: Vec<String>,
}

/// `<dir>/<id>.json`, written by a running host: how to reach it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostRecord {
    pub id: String,
    pub owner: String,
    pub label: String,
    pub host_pid: u32,
    pub child_pid: Option<u32>,
    pub port: u16,
    pub token: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// `windows_job_object`, or `direct:<why not>`.
    pub isolation: String,
    /// The agent's process has ended; the host waits for a hub to read how.
    #[serde(default)]
    pub exited: bool,
    /// The agent could not be started (the host then exits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn encode<T: Serialize>(frame: &T) -> String {
    let mut line = serde_json::to_string(frame).expect("frames serialize");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let frame = FromHost::Exit {
            seq: 4,
            code: Some(3),
            killed: false,
        };
        let line = encode(&frame);
        assert_eq!(
            line,
            "{\"t\":\"exit\",\"seq\":4,\"code\":3,\"killed\":false}\n"
        );
        assert_eq!(serde_json::from_str::<FromHost>(&line).unwrap(), frame);
        let hello: ToHost = serde_json::from_str(r#"{"t":"hello","id":"a","token":"b"}"#).unwrap();
        assert_eq!(
            hello,
            ToHost::Hello {
                id: "a".into(),
                token: "b".into()
            }
        );
    }
}

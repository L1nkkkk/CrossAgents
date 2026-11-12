use crossagents_core::{CoreError, TaskStatus};
use serde::{Deserialize, Serialize};

/// [`CoreError`], one variant each, so callers can keep matching variants across the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WireError {
    InvalidName { message: String },
    InvalidTransition { from: String, to: String },
    NotFound { kind: String, id: String },
    Conflict { message: String },
    ConflictFiles { message: String, files: Vec<String> },
    Busy { message: String },
    Storage { message: String },
    Agent { message: String },
    Unsupported { message: String },
    OutcomeUnknown { message: String },
}

/// The `NotFound` kinds the workspace's drivers and services use (`grep 'kind: "'`); a
/// `&'static str` cannot be deserialized, so the wire carries the word and this table gives
/// the literal back. Every literal a driver hands out has to be here, or the hub's message
/// says "record" instead of what was missing.
const NOT_FOUND_KINDS: [&str; 34] = [
    "account",
    "add",
    "agent",
    "attachment",
    "avatar",
    "browser request",
    "codebuddy session",
    "commit",
    "device",
    "example",
    "expertise candidate",
    "expertise entry",
    "extension",
    "git repository",
    "human",
    "issue",
    "lesson",
    "login file",
    "message",
    "note",
    "plugin",
    "project",
    "question",
    "reminder",
    "run",
    "session",
    "task",
    "terminal",
    "thinking",
    "tool",
    "turn",
    "update",
    "watch",
    "work",
];

/// The word a kind this build does not know becomes; it reads as "the thing asked for".
const UNKNOWN_KIND: &str = "record";

fn not_found_kind(kind: &str) -> &'static str {
    NOT_FOUND_KINDS
        .into_iter()
        .find(|known| *known == kind)
        .unwrap_or(UNKNOWN_KIND)
}

fn status_name(status: TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{status:?}").to_lowercase())
}

fn status_from(name: &str) -> TaskStatus {
    serde_json::from_value::<TaskStatus>(serde_json::Value::String(name.to_owned()))
        .unwrap_or(TaskStatus::Pending)
}

impl From<&CoreError> for WireError {
    fn from(err: &CoreError) -> Self {
        match err {
            CoreError::InvalidName(message) => Self::InvalidName {
                message: message.clone(),
            },
            CoreError::InvalidTransition { from, to } => Self::InvalidTransition {
                from: status_name(*from),
                to: status_name(*to),
            },
            CoreError::NotFound { kind, id } => Self::NotFound {
                kind: (*kind).to_owned(),
                id: id.clone(),
            },
            CoreError::Conflict(message) => Self::Conflict {
                message: message.clone(),
            },
            CoreError::ConflictFiles { message, files } => Self::ConflictFiles {
                message: message.clone(),
                files: files.clone(),
            },
            CoreError::Busy(message) => Self::Busy {
                message: message.clone(),
            },
            CoreError::Storage(message) => Self::Storage {
                message: message.clone(),
            },
            CoreError::Agent(message) => Self::Agent {
                message: message.clone(),
            },
            CoreError::Unsupported(message) => Self::Unsupported {
                message: message.clone(),
            },
            CoreError::OutcomeUnknown(message) => Self::OutcomeUnknown {
                message: message.clone(),
            },
        }
    }
}

impl From<WireError> for CoreError {
    fn from(err: WireError) -> Self {
        match err {
            WireError::InvalidName { message } => Self::InvalidName(message),
            WireError::InvalidTransition { from, to } => Self::InvalidTransition {
                from: status_from(&from),
                to: status_from(&to),
            },
            WireError::NotFound { kind, id } => Self::NotFound {
                kind: not_found_kind(&kind),
                id,
            },
            WireError::Conflict { message } => Self::Conflict(message),
            WireError::ConflictFiles { message, files } => Self::ConflictFiles { message, files },
            WireError::Busy { message } => Self::Busy(message),
            WireError::Storage { message } => Self::Storage(message),
            WireError::Agent { message } => Self::Agent(message),
            WireError::Unsupported { message } => Self::Unsupported(message),
            WireError::OutcomeUnknown { message } => Self::OutcomeUnknown(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::{decode, encode};

    #[test]
    fn errors_keep_their_variant() {
        let cases = [
            CoreError::InvalidName("x".into()),
            CoreError::InvalidTransition {
                from: TaskStatus::Running,
                to: TaskStatus::Merged,
            },
            CoreError::NotFound {
                kind: "agent",
                id: "a1".into(),
            },
            CoreError::Conflict("why".into()),
            CoreError::ConflictFiles {
                message: "why".into(),
                files: vec!["a".into()],
            },
            CoreError::Busy("turn running".into()),
            CoreError::Storage("disk".into()),
            CoreError::Agent("died".into()),
            CoreError::Unsupported("no pivot".into()),
            CoreError::OutcomeUnknown("timeout".into()),
        ];
        for err in cases {
            let wire = WireError::from(&err);
            let line = encode(&wire);
            let back: CoreError = decode::<WireError>(&line).unwrap().into();
            match (&err, &back) {
                (
                    CoreError::InvalidTransition { from, to },
                    CoreError::InvalidTransition { from: f2, to: t2 },
                ) => {
                    assert_eq!(from, f2);
                    assert_eq!(to, t2);
                }
                _ => assert_eq!(err.to_string(), back.to_string()),
            }
        }
        // An unknown kind becomes a literal rather than being lost.
        let back: CoreError = WireError::NotFound {
            kind: "whatever".into(),
            id: "x".into(),
        }
        .into();
        assert!(matches!(back, CoreError::NotFound { kind: "record", .. }));
    }

    #[test]
    fn not_found_kinds_keep_the_words_drivers_use() {
        // These are what drivers actually return (`grep 'kind: "'`); a missing entry
        // would turn the hub's message into "record".
        for kind in [
            "turn",
            "codebuddy session",
            "attachment",
            "tool",
            "login file",
            "git repository",
            "thinking",
            "terminal",
        ] {
            let wire = WireError::from(&CoreError::NotFound {
                kind,
                id: "x".into(),
            });
            let back: CoreError = decode::<WireError>(&encode(&wire)).unwrap().into();
            match back {
                CoreError::NotFound { kind: back, .. } => assert_eq!(back, kind),
                other => panic!("{kind} came back as {other:?}"),
            }
        }
    }
}

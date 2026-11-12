use super::{
    Op, WireAgentCommand, WirePermissionDecision, WirePermissionRequest, WireResult,
    WireSessionEvent,
};
use serde::{Deserialize, Serialize};

/// Sent by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToNode {
    /// A `Driver` / `DriverSession` call; the answer is a [`ToHub::Return`] with this `id`.
    Call { id: String, op: Op },
    /// Everything up to `seq` arrived; the node may forget it.
    Ack { seq: u64 },
    /// The hub's answer to one of the node's [`ToHub::Ask`]s.
    Answer {
        id: String,
        decision: WirePermissionDecision,
    },
    /// Liveness; answered with [`ToHub::Pong`].
    Ping,
}

/// Sent by the node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToHub {
    /// First frame of every connection, before anything is replayed.
    Register {
        /// Which run of the node this is; a new one means `seq` starts over.
        #[serde(default)]
        epoch: String,
        node_version: String,
        os: String,
        hostname: String,
        plugins: Vec<PluginSnapshot>,
    },
    /// The result of a [`ToNode::Call`].
    Return {
        seq: u64,
        id: String,
        result: WireResult,
        /// The session's commands, when they changed while the call ran (`prompt`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<WireAgentCommand>>,
    },
    /// One `SessionEvent`, unchanged, for the session the node named `session`.
    Event {
        seq: u64,
        session: String,
        event: WireSessionEvent,
        /// The session's commands, when they changed with this event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<WireAgentCommand>>,
    },
    /// A permission request the node needs the hub to decide.
    Ask {
        seq: u64,
        id: String,
        session: String,
        request: WirePermissionRequest,
    },
    Pong,
}

impl ToHub {
    /// The sequence number, for the frames that are kept until acknowledged.
    pub fn seq(&self) -> Option<u64> {
        match self {
            Self::Return { seq, .. } | Self::Event { seq, .. } | Self::Ask { seq, .. } => {
                Some(*seq)
            }
            Self::Register { .. } | Self::Pong => None,
        }
    }

    /// The session a reliable frame belongs to (the one a dropped-frame warning names).
    pub fn session(&self) -> Option<&str> {
        match self {
            Self::Return { .. } => None,
            Self::Event { session, .. } | Self::Ask { session, .. } => Some(session),
            Self::Register { .. } | Self::Pong => None,
        }
    }
}

/// A node's report of one plugin, as [`ToHub::Register`] carries it. B1.4 fills it in; B1.1
/// sends an empty list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSnapshot {
    pub name: String,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Who the node is, as [`ToHub::Register`] says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterInfo {
    /// This run of the node. A restart makes a new one, so the hub knows `seq` starts over.
    pub epoch: String,
    pub node_version: String,
    pub os: String,
    pub hostname: String,
    pub plugins: Vec<PluginSnapshot>,
}

impl RegisterInfo {
    /// This process's answer: this run's epoch, the crate version and the machine's name.
    pub fn local() -> Self {
        Self {
            epoch: new_epoch(),
            node_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            hostname: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "unknown".to_owned()),
            plugins: Vec::new(),
        }
    }
}

/// A name for this run of the node: the process, the time it was built up, and a counter, so
/// two nodes started in the same nanosecond still differ.
fn new_epoch() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{}-{nanos}-{nth}", std::process::id())
}

/// One frame as a line, newline included.
pub fn encode<T: Serialize>(frame: &T) -> String {
    let mut line = serde_json::to_string(frame).expect("frames serialize");
    line.push('\n');
    line
}

pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::{WireReply, WireSpec};
    use crossagents_core::{
        Agent, OutputStream, PermissionPolicy, SessionEvent, StopReason, TurnOutcome,
    };
    use std::path::PathBuf;

    #[test]
    fn events_round_trip() {
        let event = SessionEvent::from(crossagents_core::SessionEventKind::Output {
            stream: OutputStream::Text,
            text: "hi".into(),
        });
        let frame = ToHub::Event {
            seq: 7,
            session: "s1".into(),
            event: (&event).into(),
            commands: None,
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
        assert_eq!(frame.seq(), Some(7));
    }

    #[test]
    fn steer_read_preserves_uuid_wire_format_and_event() {
        let id = "018f0c12-3456-789a-abcd-0123456789ab";
        let event = SessionEvent::from(crossagents_core::SessionEventKind::SteerRead {
            message_id: id.parse().unwrap(),
        });
        let frame = ToHub::Event {
            seq: 17,
            session: "s1".into(),
            event: (&event).into(),
            commands: None,
        };
        let line = encode(&frame);
        assert_eq!(
            line,
            format!(
                "{{\"t\":\"event\",\"seq\":17,\"session\":\"s1\",\"event\":{{\"kind\":{{\"t\":\"steer_read\",\"message_id\":\"{id}\"}},\"raw\":null}}}}\n"
            )
        );
        let ToHub::Event { event: decoded, .. } = decode(&line).unwrap() else {
            panic!("expected event");
        };
        assert_eq!(Option::<SessionEvent>::from(decoded), Some(event));
    }

    #[test]
    fn ask_round_trips() {
        let frame = ToHub::Ask {
            seq: 9,
            id: "a1".into(),
            session: "s1".into(),
            request: WirePermissionRequest {
                id: "r1".into(),
                turn_id: Some("t1".into()),
                kind: "command".into(),
                title: "rm -rf /tmp/x".into(),
            },
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
    }

    #[test]
    fn calls_round_trip() {
        let spec = WireSpec {
            agent: (&agent()).into(),
            mcp_servers: vec![],
            permission: (&PermissionPolicy::Deny).into(),
            command: Some(vec!["agent".into(), "acp".into()]),
            instructions: Some("be kind".into()),
            env: vec![("A".into(), "B".into())],
            permissions: true,
        };
        let call = ToNode::Call {
            id: "c1".into(),
            op: Op::Open {
                plugin: "mock".into(),
                driver: Some("acp-generic".into()),
                spec: Box::new(spec),
            },
        };
        let line = encode(&call);
        assert_eq!(decode::<ToNode>(&line).unwrap(), call);
    }

    #[test]
    fn open_accepts_unknown_agent_fields() {
        let mut expected = agent();
        expected.name_source = crossagents_core::NameSource::Generated;
        expected.role = crossagents_core::Role::Reviewer;
        expected.driver = Some("native".into());
        expected.model = Some("model".into());
        expected.config = serde_json::json!({"nested": [1, true]});
        expected.profile.title = Some("reviewer".into());
        expected.topic = Some("topic".into());
        expected.archived_at = Some(expected.created_at);
        expected.node = Some("node".into());
        assert_eq!(
            serde_json::to_value(&expected).unwrap(),
            serde_json::to_value(super::super::WireAgent::from(&expected)).unwrap()
        );
        let spec = WireSpec {
            agent: (&expected).into(),
            mcp_servers: vec![],
            permission: (&PermissionPolicy::Deny).into(),
            command: None,
            instructions: None,
            env: vec![],
            permissions: false,
        };
        let call = ToNode::Call {
            id: "c1".into(),
            op: Op::Open {
                plugin: "mock".into(),
                driver: None,
                spec: Box::new(spec),
            },
        };
        let mut json = serde_json::to_value(call).unwrap();
        json["op"]["spec"]["agent"]["future_field"] = serde_json::json!({"new": true});
        let ToNode::Call {
            op: Op::Open { spec, .. },
            ..
        } = serde_json::from_value(json).unwrap()
        else {
            panic!("expected open call");
        };
        assert_eq!(Agent::from(spec.agent), expected);
    }

    #[test]
    fn returns_round_trip() {
        let frame = ToHub::Return {
            seq: 12,
            id: "c1".into(),
            result: WireResult::Ok {
                reply: WireReply::Turn {
                    outcome: (&TurnOutcome {
                        stop_reason: StopReason::EndTurn,
                        turn_id: None,
                    })
                        .into(),
                },
            },
            commands: None,
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
    }

    #[test]
    fn answers_round_trip() {
        let frame = ToNode::Answer {
            id: "a1".into(),
            decision: WirePermissionDecision::AllowForSession,
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToNode>(&line).unwrap(), frame);
    }

    #[test]
    fn register_carries_the_epoch() {
        let frame = ToHub::Register {
            epoch: "run-1".into(),
            node_version: "0.2.20".into(),
            os: "linux".into(),
            hostname: "box".into(),
            plugins: vec![PluginSnapshot {
                name: "mock".into(),
                installed: true,
                version: Some("1.0.0".into()),
            }],
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
    }

    fn agent() -> Agent {
        use crossagents_core::{AgentId, AgentStatus, NameSource, Role};
        Agent {
            id: AgentId::new(),
            name: "mocky".into(),
            name_source: NameSource::Chosen,
            plugin: "mock".into(),
            driver: None,
            model: None,
            node: None,
            role: Role::Worker,
            home: PathBuf::from("/tmp"),
            cwd: PathBuf::from("/tmp"),
            config: serde_json::Value::Null,
            status: AgentStatus::Idle,
            created_at: chrono::Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        }
    }
    #[test]
    fn unknown_operation_preserves_call_id() {
        let frame: ToNode =
            decode(r#"{"t":"call","id":"c1","op":{"op":"future","extra":42}}"#).unwrap();
        assert_eq!(
            frame,
            ToNode::Call {
                id: "c1".into(),
                op: Op::Unknown
            }
        );
        assert_eq!(Op::Unknown.session(), None);
    }

    #[test]
    fn event_decoding_tolerates_future_kinds_and_fields() {
        let unknown: ToHub = decode(r#"{"t":"event","seq":41,"session":"s","event":{"kind":{"t":"future_kind","extra":1},"raw":null}}"#).unwrap();
        assert_eq!(unknown.seq(), Some(41));
        let ToHub::Event { event, .. } = unknown else {
            panic!("expected event")
        };
        assert!(Option::<SessionEvent>::from(event).is_none());
        let known: ToHub = decode(r#"{"t":"event","seq":42,"session":"s","event":{"kind":{"t":"output","stream":"text","text":"hi","extra":1},"raw":null}}"#).unwrap();
        assert_eq!(known.seq(), Some(42));
        let ToHub::Event { event, .. } = known else {
            panic!("expected event")
        };
        assert!(Option::<SessionEvent>::from(event).is_some());
    }
}

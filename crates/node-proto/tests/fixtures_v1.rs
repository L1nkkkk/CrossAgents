//! Wire-format fixtures for protocol v1 (docs/tasks/B1-NODE.md §4, §8).
//!
//! Every message of the v1 contract has at least one sample under
//! `tests/fixtures/v1/`, recorded from this build: domain objects are built, converted to
//! their `Wire*` DTO and encoded to one line. Two tests guard the directory:
//!
//! - [`fixtures_decode_to_their_recorded_meaning`] decodes each sample with the current code
//!   and checks what it says (seq, session, an op's arguments, an event's kind and content),
//!   not merely that decoding succeeded.
//! - [`fixtures_match_current_encoding`] re-encodes the same domain objects and compares the
//!   line with the stored file. Renaming or dropping a field that crosses the wire turns
//!   this red instead of waiting for a Mac node to connect.
//!
//! The samples are recorded, never hand-written:
//!
//! ```text
//! cargo test -p crossagents-node-proto --test fixtures_v1 -- --ignored record_fixtures
//! ```
//!
//! See `tests/fixtures/README.md` for the convention.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use crossagents_core as core;
use crossagents_node_proto::v1::{
    Op, PROTOCOL_V1, PluginSnapshot, ProtocolVersion, SessionInfo, ToHub, ToNode, WireAgentCommand,
    WireAgentCommandKind, WireAttachment, WireAttachmentSupport, WireCliStatus, WireContextPart,
    WireCredits, WireError, WireFileChange, WireLimitGroup, WireLocated, WireMessageId,
    WireOutputStream, WirePermissionDecision, WirePermissionRequest, WirePrompt, WireRateLimits,
    WireRateWindow, WireReply, WireResetCredit, WireResult, WireSessionEvent, WireSessionEventKind,
    WireSpec, WireTokenUsage, WireToolStatus, WireTurnOutcome, decode, encode,
};

// ---------------------------------------------------------------------------------------
// Cases: the frames a v1 node and hub exchange, built from domain objects.
// ---------------------------------------------------------------------------------------

enum Frame {
    ToHub(ToHub),
    ToNode(ToNode),
}

struct Case {
    /// The file name without `.json`: the message or op it holds.
    name: &'static str,
    frame: Frame,
}

fn cases() -> Vec<Case> {
    let mut cases = vec![
        // node -> hub
        Case {
            name: "register",
            frame: Frame::ToHub(register()),
        },
        Case {
            name: "return",
            frame: Frame::ToHub(ToHub::Return {
                seq: 12,
                id: "call-7".into(),
                result: WireResult::Ok {
                    reply: WireReply::Turn {
                        outcome: (&core::TurnOutcome {
                            stop_reason: core::StopReason::EndTurn,
                            turn_id: Some("turn-3".into()),
                        })
                            .into(),
                    },
                },
                commands: Some(vec![(&command("review", "Review the change")).into()]),
            }),
        },
        Case {
            name: "return_session",
            frame: Frame::ToHub(ToHub::Return {
                seq: 8,
                id: "call-4".into(),
                result: WireResult::Ok {
                    reply: WireReply::Session {
                        session: SessionInfo {
                            handle: "h-1".into(),
                            session_id: "sess-123".into(),
                            attachment_support: Some((&core::AttachmentSupport::ALL).into()),
                            outlives_hub: true,
                            commands: Some(vec![
                                (&command("compact", "Compact the context")).into(),
                            ]),
                        },
                    },
                },
                commands: None,
            }),
        },
        Case {
            name: "return_error",
            frame: Frame::ToHub(ToHub::Return {
                seq: 14,
                id: "call-9".into(),
                result: WireResult::Err {
                    error: (&core::CoreError::NotFound {
                        kind: "turn",
                        id: "turn-3".into(),
                    })
                        .into(),
                },
                commands: None,
            }),
        },
        Case {
            name: "return_cli_status",
            frame: Frame::ToHub(ToHub::Return {
                seq: 15,
                id: "call-10".into(),
                result: WireResult::Ok {
                    reply: WireReply::CliStatus {
                        status: Some(cli_status()),
                    },
                },
                commands: None,
            }),
        },
        Case {
            name: "ask",
            frame: Frame::ToHub(ToHub::Ask {
                seq: 13,
                id: "ask-1".into(),
                session: "h-1".into(),
                request: (&permission_request()).into(),
            }),
        },
        Case {
            name: "pong",
            frame: Frame::ToHub(ToHub::Pong),
        },
        // hub -> node
        Case {
            name: "call_open",
            frame: Frame::ToNode(call(Op::Open {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
                spec: Box::new(spec()),
            })),
        },
        Case {
            name: "call_resume",
            frame: Frame::ToNode(call(Op::Resume {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
                session_id: "sess-123".into(),
                spec: Box::new(spec()),
            })),
        },
        Case {
            name: "call_adopt",
            frame: Frame::ToNode(call(Op::Adopt {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
                session_id: "sess-123".into(),
                turn_running: true,
                spec: Box::new(spec()),
            })),
        },
        Case {
            name: "call_adopted_turn",
            frame: Frame::ToNode(call(Op::AdoptedTurn {
                session: "h-1".into(),
            })),
        },
        Case {
            name: "call_prompt",
            frame: Frame::ToNode(call(Op::Prompt {
                session: "h-1".into(),
                prompt: prompt(),
            })),
        },
        Case {
            name: "call_steer",
            frame: Frame::ToNode(call(Op::Steer {
                session: "h-1".into(),
                message_id: MESSAGE_ID.into(),
                prompt: prompt(),
            })),
        },
        Case {
            name: "call_interrupt",
            frame: Frame::ToNode(call(Op::Interrupt {
                session: "h-1".into(),
            })),
        },
        Case {
            name: "call_suspend",
            frame: Frame::ToNode(call(Op::Suspend {
                session: "h-1".into(),
            })),
        },
        Case {
            name: "call_fork",
            frame: Frame::ToNode(call(Op::Fork {
                session: "h-1".into(),
                at_turn: Some("turn-3".into()),
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
                spec: Box::new(spec()),
            })),
        },
        Case {
            name: "call_rollback",
            frame: Frame::ToNode(call(Op::Rollback {
                session: "h-1".into(),
                before_turn: "turn-3".into(),
                revert_files: true,
            })),
        },
        Case {
            name: "call_set_model",
            frame: Frame::ToNode(call(Op::SetModel {
                session: "h-1".into(),
                model: Some("claude-opus-4-6".into()),
                effort: Some("high".into()),
            })),
        },
        Case {
            name: "call_compact",
            frame: Frame::ToNode(call(Op::Compact {
                session: "h-1".into(),
            })),
        },
        Case {
            name: "call_shutdown",
            frame: Frame::ToNode(call(Op::Shutdown {
                session: "h-1".into(),
            })),
        },
        Case {
            name: "call_probe_cli",
            frame: Frame::ToNode(call(Op::ProbeCli {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
            })),
        },
        Case {
            name: "call_refresh_cli",
            frame: Frame::ToNode(call(Op::RefreshCli {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
            })),
        },
        Case {
            name: "call_cli_status",
            frame: Frame::ToNode(call(Op::CliStatus {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
            })),
        },
        Case {
            name: "ack",
            frame: Frame::ToNode(ToNode::Ack { seq: 42 }),
        },
        Case {
            name: "answer",
            frame: Frame::ToNode(ToNode::Answer {
                id: "ask-1".into(),
                decision: WirePermissionDecision::AllowForSession,
            }),
        },
        Case {
            name: "ping",
            frame: Frame::ToNode(ToNode::Ping),
        },
    ];

    // Every SessionEvent variant the node can send, one sample each.
    cases.extend([
        event("event_started", 20, core::SessionEventKind::Started {
            session_id: "sess-123".into(),
        }),
        event("event_turn_started", 21, core::SessionEventKind::TurnStarted {
            turn_id: Some("turn-3".into()),
        }),
        event("event_output", 22, core::SessionEventKind::Output {
            stream: core::OutputStream::Text,
            text: "I'll fix the failing test.".into(),
        }),
        event("event_output_thought", 23, core::SessionEventKind::Output {
            stream: core::OutputStream::Thought,
            text: "The assertion is off by one.".into(),
        }),
        event("event_tool_call", 24, core::SessionEventKind::ToolCall {
            tool_call_id: "tool-1".into(),
            title: "Run cargo test".into(),
            kind: Some("execute".into()),
            status: core::ToolStatus::InProgress,
        }),
        event("event_tool_call_updated", 25, core::SessionEventKind::ToolCallUpdated {
            tool_call_id: "tool-1".into(),
            title: None,
            status: Some(core::ToolStatus::Completed),
        }),
        event("event_tool_output", 26, core::SessionEventKind::ToolOutput {
            tool_call_id: "tool-1".into(),
            text: "test result: ok. 42 passed".into(),
        }),
        event("event_tool_detail", 27, core::SessionEventKind::ToolDetail {
            tool_call_id: "tool-1".into(),
            detail: core::ToolDetail {
                command: Some("cargo test -p crossagents-node-proto".into()),
                cwd: Some("/crossagents/work/repo".into()),
                exit_code: Some(0),
                duration_ms: Some(812),
                output: Some("test result: ok".into()),
                changes: vec![core::FileChange {
                    path: "crates/node-proto/src/v1/session.rs".into(),
                    kind: "update".into(),
                    diff: "@@ -1 +1 @@\n-old\n+new\n".into(),
                }],
                arguments: None,
                result: None,
                error: None,
            },
        }),
        event("event_turn_diff", 28, core::SessionEventKind::TurnDiff {
            turn_id: Some("turn-3".into()),
            diff: "--- a/src/login.rs\n+++ b/src/login.rs\n@@ -1 +1 @@\n-let x = 1;\n+let x = 2;\n".into(),
        }),
        event("event_usage", 29, core::SessionEventKind::Usage {
            used: 48_000,
            size: 200_000,
            cost_usd: Some(0.42),
            tokens: Some(core::TokenUsage {
                input: 41_000,
                cached_input: 30_000,
                cache_write_input: 2_000,
                output: 7_000,
                reasoning_output: 1_500,
                total: 48_000,
                requests: Some(6),
            }),
        }),
        event("event_rate_limits", 30, core::SessionEventKind::RateLimits {
            limits: rate_limits(),
        }),
        event("event_context_breakdown", 31, core::SessionEventKind::ContextBreakdown {
            parts: vec![
                core::ContextPart {
                    id: "system_prompt".into(),
                    name: "System prompt".into(),
                    tokens: 1_800,
                    kind: "used".into(),
                },
                core::ContextPart {
                    id: "messages".into(),
                    name: "Messages".into(),
                    tokens: 12_400,
                    kind: "used".into(),
                },
                core::ContextPart {
                    id: "free_space".into(),
                    name: "Free space".into(),
                    tokens: 185_800,
                    kind: "free".into(),
                },
            ],
        }),
        event(
            "event_permission_requested",
            32,
            core::SessionEventKind::PermissionRequested {
                request: permission_request(),
                decision: core::PermissionDecision::Allow,
            },
        ),
        event("event_turn_completed", 33, core::SessionEventKind::TurnCompleted {
            turn_id: Some("turn-3".into()),
            stop_reason: core::StopReason::EndTurn,
        }),
        event("event_steer_read", 34, core::SessionEventKind::SteerRead {
            message_id: MESSAGE_ID.parse().unwrap(),
        }),
        event("event_compacted", 35, core::SessionEventKind::Compacted),
        event("event_prompt_via_file", 36, core::SessionEventKind::PromptViaFile),
        event("event_warning", 37, core::SessionEventKind::Warning {
            message: "model claude-opus-4-6 not found; kept the previous one".into(),
        }),
        event("event_error", 38, core::SessionEventKind::Error {
            message: "the agent exited before answering".into(),
        }),
        event("event_exited", 39, core::SessionEventKind::Exited {
            message: "agent process crashed (exit code 1)".into(),
            cause: Some(core::ExitCause::Crashed),
            code: Some(1),
        }),
    ]);

    // One event carrying the vendor's own message, which consumers may read.
    cases.push(Case {
        name: "event_output_with_raw",
        frame: Frame::ToHub(ToHub::Event {
            seq: 40,
            session: "h-1".into(),
            event: (&core::SessionEvent::with_raw(
                core::SessionEventKind::Output {
                    stream: core::OutputStream::Text,
                    text: "Done.".into(),
                },
                serde_json::json!({"type": "assistant", "vendor": "claude"}),
            ))
                .into(),
            commands: None,
        }),
    });

    cases
}

fn call(op: Op) -> ToNode {
    ToNode::Call {
        id: "call-7".into(),
        op,
    }
}

fn event(name: &'static str, seq: u64, kind: core::SessionEventKind) -> Case {
    Case {
        name,
        frame: Frame::ToHub(ToHub::Event {
            seq,
            session: "h-1".into(),
            event: (&core::SessionEvent::from(kind)).into(),
            commands: None,
        }),
    }
}

fn register() -> ToHub {
    ToHub::Register {
        epoch: "run-7f3a".into(),
        protocol: PROTOCOL_V1,
        capabilities: vec![
            crossagents_node_proto::v1::CAPABILITY_TERMINAL.into(),
            crossagents_node_proto::v1::CAPABILITY_ATTACHMENTS.into(),
        ],
        node_version: "0.2.20".into(),
        os: "macos".into(),
        hostname: "mac-studio".into(),
        plugins: vec![
            PluginSnapshot {
                name: "claude".into(),
                installed: true,
                version: Some("2.1.0".into()),
            },
            PluginSnapshot {
                name: "codex".into(),
                installed: false,
                version: None,
            },
        ],
    }
}

const MESSAGE_ID: &str = "018f0c12-3456-789a-abcd-0123456789ab";

fn ts() -> DateTime<Utc> {
    "2026-11-13T08:00:00Z".parse().unwrap()
}

fn agent() -> core::Agent {
    core::Agent {
        id: core::AgentId::from(
            uuid::Uuid::parse_str("018f0c12-0000-7000-8000-000000000001").unwrap(),
        ),
        name: "Luna".into(),
        name_source: core::NameSource::Chosen,
        plugin: "claude".into(),
        driver: Some("claude-native".into()),
        model: Some("claude-sonnet-4-6".into()),
        role: core::Role::Worker,
        home: PathBuf::from("/crossagents/work/agents/luna"),
        cwd: PathBuf::from("/crossagents/work/repo"),
        config: serde_json::json!({"account": "claude-main"}),
        status: core::AgentStatus::Idle,
        created_at: ts(),
        profile: core::AgentProfile {
            display_name: Some("Luna".into()),
            title: Some("Worker".into()),
            persona: Some("Careful, tests first.".into()),
            ..Default::default()
        },
        topic: Some("Fix the login flow".into()),
        archived_at: None,
        node: Some("mac-studio".into()),
    }
}

fn spec() -> WireSpec {
    WireSpec {
        agent: (&agent()).into(),
        mcp_servers: vec![
            (&core::McpServerSpec {
                name: "crossagents".into(),
                command: PathBuf::from("/usr/local/bin/crossagents"),
                args: vec!["mcp".into(), "--url".into(), "http://127.0.0.1:8787".into()],
                env: BTreeMap::from([(
                    "CROSSAGENTS_URL".to_string(),
                    "http://127.0.0.1:8787".to_string(),
                )]),
            })
                .into(),
        ],
        permission: (&core::PermissionPolicy::Guard).into(),
        command: Some(vec!["claude".into(), "--acp".into()]),
        instructions: Some("You are Luna, a worker in this repo.".into()),
        env: vec![(
            "CLAUDE_CONFIG_DIR".into(),
            "/crossagents/accounts/claude".into(),
        )],
        permissions: true,
    }
}

fn prompt() -> WirePrompt {
    (&core::Prompt {
        text: "Fix the failing login test".into(),
        attachments: vec![core::Attachment {
            id: "9f2c1e".into(),
            name: "failure.log".into(),
            mime: "text/plain".into(),
            size: 1_204,
            path: PathBuf::from("/crossagents/attachments/9f2c1e/failure.log"),
        }],
    })
        .into()
}

fn permission_request() -> core::PermissionRequest {
    core::PermissionRequest {
        id: "perm-1".into(),
        turn_id: Some("turn-3".into()),
        kind: "command".into(),
        title: "cargo test -p crossagents-node-proto".into(),
    }
}

fn command(name: &str, description: &str) -> core::AgentCommand {
    core::AgentCommand {
        name: name.into(),
        description: Some(description.into()),
        kind: core::AgentCommandKind::Command,
    }
}

fn rate_limits() -> core::RateLimits {
    core::RateLimits {
        windows: vec![core::RateWindow {
            name: "five_hour".into(),
            used_percent: 37.5,
            window_seconds: Some(18_000),
            resets_at: Some(ts()),
            remaining: Some(62.5),
            total: Some(100.0),
        }],
        credits: Some(core::Credits {
            has_credits: true,
            unlimited: false,
            balance: Some("12.50".into()),
        }),
        plan: Some("max".into()),
        reached: None,
        by_limit: vec![core::LimitGroup {
            id: "claude".into(),
            name: Some("Claude".into()),
            model: Some("claude-sonnet-4-6".into()),
            windows: vec![core::RateWindow {
                name: "weekly".into(),
                used_percent: 12.0,
                window_seconds: Some(604_800),
                resets_at: None,
                remaining: Some(88.0),
                total: Some(100.0),
            }],
            reached: false,
        }],
        reset_credits: Some(vec![core::ResetCredit {
            id: "rc-1".into(),
            title: "One free reset".into(),
            description: None,
            status: "available".into(),
            expires_at: Some(ts()),
        }]),
    }
}

fn cli_status() -> WireCliStatus {
    WireCliStatus::from(core::CliStatus {
        located: Some(core::Located {
            argv: vec!["claude".into()],
            path: PathBuf::from("/usr/local/bin/claude"),
            source: core::CliSource::Path,
            version: Some("2.1.0".into()),
        }),
        probing: false,
        install_hint: Some("npm i -g @anthropic-ai/claude-code".into()),
        docs: Some("https://docs.anthropic.com/claude-code".into()),
        installed_via: Some("npm".into()),
        install_command: Some(vec![
            "npm".into(),
            "install".into(),
            "-g".into(),
            "@anthropic-ai/claude-code".into(),
        ]),
        upgrade_command: None,
        can_upgrade: true,
        described_by: Some("built-in".into()),
    })
}

// ---------------------------------------------------------------------------------------
// Recording and files
// ---------------------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1")
}

fn fixture_path(name: &str) -> PathBuf {
    fixture_dir().join(format!("{name}.json"))
}

fn line(case: &Case) -> String {
    match &case.frame {
        Frame::ToHub(frame) => encode(frame),
        Frame::ToNode(frame) => encode(frame),
    }
}

/// Read a sample as it is on disk, with line endings normalized: git may check it out as
/// CRLF on Windows, which is not a protocol change.
fn read(name: &str) -> String {
    fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|err| panic!("read {}: {err}", fixture_path(name).display()))
        .replace("\r\n", "\n")
}

fn hub(name: &str) -> ToHub {
    decode(&read(name)).unwrap_or_else(|err| panic!("decode {name} as ToHub: {err}"))
}

fn node(name: &str) -> ToNode {
    decode(&read(name)).unwrap_or_else(|err| panic!("decode {name} as ToNode: {err}"))
}

fn event_of(name: &str) -> WireSessionEvent {
    let ToHub::Event { event, .. } = hub(name) else {
        panic!("{name} is not an Event frame")
    };
    event
}

/// Rewrites every sample from the current code. Run it when recording a release; the
/// output is meant to be committed as it is.
#[test]
#[ignore = "rewrites tests/fixtures/v1 from the current build"]
fn record_fixtures() {
    let dir = fixture_dir();
    fs::create_dir_all(&dir).unwrap();
    for case in cases() {
        let path = fixture_path(case.name);
        fs::write(&path, line(&case)).unwrap();
        println!("recorded {}", path.display());
    }
}

#[test]
fn every_case_has_exactly_one_sample_and_nothing_else() {
    let mut recorded: Vec<String> = fs::read_dir(fixture_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();
    recorded.sort();

    let mut expected: Vec<String> = cases()
        .iter()
        .map(|case| format!("{}.json", case.name))
        .collect();
    expected.sort();

    assert_eq!(
        recorded, expected,
        "tests/fixtures/v1 must hold one sample per case; re-record with \
         `cargo test -p crossagents-node-proto --test fixtures_v1 -- --ignored record_fixtures`"
    );
}

// ---------------------------------------------------------------------------------------
// Golden encoding: the guard against a silent wire change.
// ---------------------------------------------------------------------------------------

#[test]
fn fixtures_match_current_encoding() {
    for case in cases() {
        assert_eq!(
            read(case.name),
            line(&case),
            "tests/fixtures/v1/{}.json no longer matches what this build encodes; if the wire \
             format really changed, record a new v<major> directory instead of editing this one",
            case.name
        );
    }
}

// ---------------------------------------------------------------------------------------
// Decoding: what the samples say, not merely that they decode.
// ---------------------------------------------------------------------------------------

#[test]
fn fixtures_decode_to_the_frames_that_were_recorded() {
    for case in cases() {
        let text = read(case.name);
        match &case.frame {
            Frame::ToHub(expected) => assert_eq!(
                &decode::<ToHub>(&text).unwrap_or_else(|err| panic!("{}: {err}", case.name)),
                expected,
                "{}",
                case.name
            ),
            Frame::ToNode(expected) => assert_eq!(
                &decode::<ToNode>(&text).unwrap_or_else(|err| panic!("{}: {err}", case.name)),
                expected,
                "{}",
                case.name
            ),
        }
    }
}

#[test]
fn frames_decode_to_their_recorded_meaning() {
    // Register names the node and its protocol.
    let ToHub::Register {
        epoch,
        protocol,
        capabilities,
        node_version,
        os,
        hostname,
        plugins,
    } = hub("register")
    else {
        panic!("register is not a Register frame")
    };
    assert_eq!(epoch, "run-7f3a");
    assert_eq!(protocol, ProtocolVersion { major: 1, minor: 0 });
    assert_eq!(capabilities, ["terminal", "attachments"]);
    assert_eq!(node_version, "0.2.20");
    assert_eq!(os, "macos");
    assert_eq!(hostname, "mac-studio");
    assert_eq!(plugins.len(), 2);
    assert_eq!(plugins[0].name, "claude");
    assert!(plugins[0].installed);
    assert_eq!(plugins[0].version.as_deref(), Some("2.1.0"));
    assert_eq!(plugins[1].name, "codex");
    assert!(!plugins[1].installed);
    assert_eq!(plugins[1].version, None);

    // Return carries the call id, the turn outcome and the session's commands.
    let ToHub::Return {
        seq,
        id,
        result,
        commands,
    } = hub("return")
    else {
        panic!("return is not a Return frame")
    };
    assert_eq!(seq, 12);
    assert_eq!(id, "call-7");
    let WireResult::Ok {
        reply: WireReply::Turn { outcome },
    } = result
    else {
        panic!("return does not carry a turn")
    };
    assert_eq!(
        outcome,
        WireTurnOutcome {
            stop_reason: crossagents_node_proto::v1::WireStopReason::EndTurn,
            turn_id: Some("turn-3".into()),
        }
    );
    let commands = commands.expect("commands travel with the return");
    assert_eq!(
        commands,
        [WireAgentCommand {
            name: "review".into(),
            description: Some("Review the change".into()),
            kind: WireAgentCommandKind::Command,
        }]
    );

    // A session reply names the handle every later call uses.
    let ToHub::Return {
        result: WireResult::Ok {
            reply: WireReply::Session { session },
        },
        ..
    } = hub("return_session")
    else {
        panic!("return_session does not carry a session")
    };
    assert_eq!(session.handle, "h-1");
    assert_eq!(session.session_id, "sess-123");
    assert_eq!(
        session.attachment_support,
        Some(WireAttachmentSupport {
            image: true,
            pdf: true,
            text: true,
        })
    );
    assert!(session.outlives_hub);

    // An error keeps its variant and the word that names what was missing.
    let ToHub::Return {
        result: WireResult::Err { error },
        ..
    } = hub("return_error")
    else {
        panic!("return_error is not an error")
    };
    assert_eq!(
        error,
        WireError::NotFound {
            kind: "turn".into(),
            id: "turn-3".into(),
        }
    );
    let back: core::CoreError = error.into();
    assert_eq!(back.to_string(), "turn not found: turn-3");

    // Ask asks the hub to decide, for a named session.
    let ToHub::Ask {
        seq,
        id,
        session,
        request,
    } = hub("ask")
    else {
        panic!("ask is not an Ask frame")
    };
    assert_eq!(seq, 13);
    assert_eq!(id, "ask-1");
    assert_eq!(session, "h-1");
    assert_eq!(request.id, "perm-1");
    assert_eq!(request.turn_id.as_deref(), Some("turn-3"));
    assert_eq!(request.kind, "command");
    assert_eq!(request.title, "cargo test -p crossagents-node-proto");

    // Liveness and acknowledgement.
    assert_eq!(hub("pong"), ToHub::Pong);
    assert_eq!(node("ping"), ToNode::Ping);
    assert_eq!(node("ack"), ToNode::Ack { seq: 42 });
    assert_eq!(
        node("answer"),
        ToNode::Answer {
            id: "ask-1".into(),
            decision: WirePermissionDecision::AllowForSession,
        }
    );

    // Every Call op names what it asks for.
    let open = op_of("call_open");
    let Op::Open {
        plugin,
        driver,
        spec,
    } = &open
    else {
        panic!("call_open is not Open")
    };
    assert_eq!(plugin, "claude");
    assert_eq!(driver.as_deref(), Some("claude-native"));
    assert_eq!(spec.agent.name, "Luna");
    assert_eq!(spec.agent.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(spec.agent.node.as_deref(), Some("mac-studio"));
    assert_eq!(spec.agent.cwd, PathBuf::from("/crossagents/work/repo"));
    assert_eq!(
        spec.permission,
        crossagents_node_proto::v1::WirePermissionPolicy::Guard
    );
    assert!(spec.permissions);
    assert_eq!(
        spec.command.as_deref(),
        Some(&["claude".to_string(), "--acp".into()][..])
    );
    assert_eq!(spec.mcp_servers.len(), 1);
    assert_eq!(spec.mcp_servers[0].name, "crossagents");
    assert_eq!(
        spec.mcp_servers[0]
            .env
            .get("CROSSAGENTS_URL")
            .map(String::as_str),
        Some("http://127.0.0.1:8787")
    );

    let Op::Resume {
        session_id, spec, ..
    } = op_of("call_resume")
    else {
        panic!("call_resume is not Resume")
    };
    assert_eq!(session_id, "sess-123");
    assert_eq!(spec.agent.name, "Luna");

    let Op::Adopt {
        session_id,
        turn_running,
        ..
    } = op_of("call_adopt")
    else {
        panic!("call_adopt is not Adopt")
    };
    assert_eq!(session_id, "sess-123");
    assert!(turn_running);

    assert_eq!(
        op_of("call_adopted_turn"),
        Op::AdoptedTurn {
            session: "h-1".into()
        }
    );

    let Op::Prompt { session, prompt } = op_of("call_prompt") else {
        panic!("call_prompt is not Prompt")
    };
    assert_eq!(session, "h-1");
    assert_eq!(prompt.text, "Fix the failing login test");
    assert_eq!(
        prompt.attachments,
        [WireAttachment {
            id: "9f2c1e".into(),
            name: "failure.log".into(),
            mime: "text/plain".into(),
            size: 1_204,
            path: PathBuf::from("/crossagents/attachments/9f2c1e/failure.log"),
        }]
    );

    let Op::Steer {
        session,
        message_id,
        prompt,
    } = op_of("call_steer")
    else {
        panic!("call_steer is not Steer")
    };
    assert_eq!(session, "h-1");
    assert_eq!(message_id, MESSAGE_ID);
    assert_eq!(
        WireMessageId::from(&MESSAGE_ID.parse::<core::MessageId>().unwrap()),
        WireMessageId(MESSAGE_ID.parse().unwrap())
    );
    assert_eq!(prompt.text, "Fix the failing login test");

    assert_eq!(
        op_of("call_interrupt"),
        Op::Interrupt {
            session: "h-1".into()
        }
    );
    assert_eq!(
        op_of("call_suspend"),
        Op::Suspend {
            session: "h-1".into()
        }
    );

    let Op::Fork {
        session,
        at_turn,
        plugin,
        ..
    } = op_of("call_fork")
    else {
        panic!("call_fork is not Fork")
    };
    assert_eq!(session, "h-1");
    assert_eq!(at_turn.as_deref(), Some("turn-3"));
    assert_eq!(plugin, "claude");

    assert_eq!(
        op_of("call_rollback"),
        Op::Rollback {
            session: "h-1".into(),
            before_turn: "turn-3".into(),
            revert_files: true,
        }
    );

    let Op::SetModel {
        session,
        model,
        effort,
    } = op_of("call_set_model")
    else {
        panic!("call_set_model is not SetModel")
    };
    assert_eq!(session, "h-1");
    assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(effort.as_deref(), Some("high"));

    assert_eq!(
        op_of("call_compact"),
        Op::Compact {
            session: "h-1".into()
        }
    );
    assert_eq!(
        op_of("call_shutdown"),
        Op::Shutdown {
            session: "h-1".into()
        }
    );
    for (name, expected) in [
        (
            "call_probe_cli",
            Op::ProbeCli {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
            },
        ),
        (
            "call_refresh_cli",
            Op::RefreshCli {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
            },
        ),
        (
            "call_cli_status",
            Op::CliStatus {
                plugin: "claude".into(),
                driver: Some("claude-native".into()),
            },
        ),
    ] {
        assert_eq!(op_of(name), expected, "{name}");
    }
}

fn op_of(name: &str) -> Op {
    let ToNode::Call { id, op } = node(name) else {
        panic!("{name} is not a Call frame")
    };
    assert_eq!(id, "call-7", "{name} lost its call id");
    op
}

#[test]
fn events_decode_to_their_recorded_kind_and_content() {
    let seqs: Vec<u64> = [
        "event_started",
        "event_turn_started",
        "event_output",
        "event_output_thought",
        "event_tool_call",
        "event_tool_call_updated",
        "event_tool_output",
        "event_tool_detail",
        "event_turn_diff",
        "event_usage",
        "event_rate_limits",
        "event_context_breakdown",
        "event_permission_requested",
        "event_turn_completed",
        "event_steer_read",
        "event_compacted",
        "event_prompt_via_file",
        "event_warning",
        "event_error",
        "event_exited",
        "event_output_with_raw",
    ]
    .into_iter()
    .map(|name| {
        let ToHub::Event { seq, session, .. } = hub(name) else {
            panic!("{name} is not an Event frame")
        };
        assert_eq!(session, "h-1", "{name}");
        seq
    })
    .collect();
    assert_eq!(seqs, (20..=40).collect::<Vec<_>>());

    assert_eq!(
        event_of("event_started").kind,
        WireSessionEventKind::Started {
            session_id: "sess-123".into()
        }
    );
    assert_eq!(
        event_of("event_turn_started").kind,
        WireSessionEventKind::TurnStarted {
            turn_id: Some("turn-3".into())
        }
    );
    assert_eq!(
        event_of("event_output").kind,
        WireSessionEventKind::Output {
            stream: WireOutputStream::Text,
            text: "I'll fix the failing test.".into(),
        }
    );
    assert_eq!(
        event_of("event_output_thought").kind,
        WireSessionEventKind::Output {
            stream: WireOutputStream::Thought,
            text: "The assertion is off by one.".into(),
        }
    );
    assert_eq!(
        event_of("event_tool_call").kind,
        WireSessionEventKind::ToolCall {
            tool_call_id: "tool-1".into(),
            title: "Run cargo test".into(),
            kind: Some("execute".into()),
            status: WireToolStatus::InProgress,
        }
    );
    assert_eq!(
        event_of("event_tool_call_updated").kind,
        WireSessionEventKind::ToolCallUpdated {
            tool_call_id: "tool-1".into(),
            title: None,
            status: Some(WireToolStatus::Completed),
        }
    );
    assert_eq!(
        event_of("event_tool_output").kind,
        WireSessionEventKind::ToolOutput {
            tool_call_id: "tool-1".into(),
            text: "test result: ok. 42 passed".into(),
        }
    );
    let WireSessionEventKind::ToolDetail {
        tool_call_id,
        detail,
    } = event_of("event_tool_detail").kind
    else {
        panic!("event_tool_detail is not a ToolDetail")
    };
    assert_eq!(tool_call_id, "tool-1");
    assert_eq!(
        detail.command.as_deref(),
        Some("cargo test -p crossagents-node-proto")
    );
    assert_eq!(detail.exit_code, Some(0));
    assert_eq!(detail.duration_ms, Some(812));
    assert_eq!(
        detail.changes,
        [WireFileChange {
            path: "crates/node-proto/src/v1/session.rs".into(),
            kind: "update".into(),
            diff: "@@ -1 +1 @@\n-old\n+new\n".into(),
        }]
    );
    assert_eq!(detail.arguments, None);
    assert_eq!(detail.error, None);

    let WireSessionEventKind::TurnDiff { turn_id, diff } = event_of("event_turn_diff").kind else {
        panic!("event_turn_diff is not a TurnDiff")
    };
    assert_eq!(turn_id.as_deref(), Some("turn-3"));
    assert!(diff.contains("-let x = 1;"));

    let WireSessionEventKind::Usage {
        used,
        size,
        cost_usd,
        tokens,
    } = event_of("event_usage").kind
    else {
        panic!("event_usage is not a Usage")
    };
    assert_eq!((used, size, cost_usd), (48_000, 200_000, Some(0.42)));
    assert_eq!(
        tokens,
        Some(WireTokenUsage {
            input: 41_000,
            cached_input: 30_000,
            cache_write_input: 2_000,
            output: 7_000,
            reasoning_output: 1_500,
            total: 48_000,
            requests: Some(6),
        })
    );

    let WireSessionEventKind::RateLimits { limits } = event_of("event_rate_limits").kind else {
        panic!("event_rate_limits is not a RateLimits")
    };
    assert_eq!(
        limits,
        WireRateLimits {
            windows: vec![WireRateWindow {
                name: "five_hour".into(),
                used_percent: 37.5,
                window_seconds: Some(18_000),
                resets_at: Some(ts()),
                remaining: Some(62.5),
                total: Some(100.0),
            }],
            credits: Some(WireCredits {
                has_credits: true,
                unlimited: false,
                balance: Some("12.50".into()),
            }),
            plan: Some("max".into()),
            reached: None,
            by_limit: vec![WireLimitGroup {
                id: "claude".into(),
                name: Some("Claude".into()),
                model: Some("claude-sonnet-4-6".into()),
                windows: vec![WireRateWindow {
                    name: "weekly".into(),
                    used_percent: 12.0,
                    window_seconds: Some(604_800),
                    resets_at: None,
                    remaining: Some(88.0),
                    total: Some(100.0),
                }],
                reached: false,
            }],
            reset_credits: Some(vec![WireResetCredit {
                id: "rc-1".into(),
                title: "One free reset".into(),
                description: None,
                status: "available".into(),
                expires_at: Some(ts()),
            }]),
        }
    );

    let WireSessionEventKind::ContextBreakdown { parts } = event_of("event_context_breakdown").kind
    else {
        panic!("event_context_breakdown is not a ContextBreakdown")
    };
    assert_eq!(parts.len(), 3);
    assert_eq!(
        parts[0],
        WireContextPart {
            id: "system_prompt".into(),
            name: "System prompt".into(),
            tokens: 1_800,
            kind: "used".into(),
        }
    );
    assert_eq!(parts[2].id, "free_space");
    assert_eq!(parts[2].tokens, 185_800);

    let WireSessionEventKind::PermissionRequested { request, decision } =
        event_of("event_permission_requested").kind
    else {
        panic!("event_permission_requested is not a PermissionRequested")
    };
    assert_eq!(
        request,
        WirePermissionRequest {
            id: "perm-1".into(),
            turn_id: Some("turn-3".into()),
            kind: "command".into(),
            title: "cargo test -p crossagents-node-proto".into(),
        }
    );
    assert_eq!(decision, WirePermissionDecision::Allow);

    assert_eq!(
        event_of("event_turn_completed").kind,
        WireSessionEventKind::TurnCompleted {
            turn_id: Some("turn-3".into()),
            stop_reason: crossagents_node_proto::v1::WireStopReason::EndTurn,
        }
    );
    assert_eq!(
        event_of("event_steer_read").kind,
        WireSessionEventKind::SteerRead {
            message_id: WireMessageId(MESSAGE_ID.parse().unwrap()),
        }
    );
    assert_eq!(
        event_of("event_compacted").kind,
        WireSessionEventKind::Compacted
    );
    assert_eq!(
        event_of("event_prompt_via_file").kind,
        WireSessionEventKind::PromptViaFile
    );
    assert_eq!(
        event_of("event_warning").kind,
        WireSessionEventKind::Warning {
            message: "model claude-opus-4-6 not found; kept the previous one".into(),
        }
    );
    assert_eq!(
        event_of("event_error").kind,
        WireSessionEventKind::Error {
            message: "the agent exited before answering".into(),
        }
    );
    assert_eq!(
        event_of("event_exited").kind,
        WireSessionEventKind::Exited {
            message: "agent process crashed (exit code 1)".into(),
            cause: Some(crossagents_node_proto::v1::WireExitCause::Crashed),
            code: Some(1),
        }
    );

    // The vendor's own message travels unchanged next to the unified kind.
    let raw = event_of("event_output_with_raw");
    assert_eq!(
        raw.raw,
        Some(serde_json::json!({"type": "assistant", "vendor": "claude"}))
    );
    assert!(matches!(
        raw.kind,
        WireSessionEventKind::Output {
            stream: WireOutputStream::Text,
            ..
        }
    ));
}

#[test]
fn cli_status_reply_keeps_the_driver_report() {
    // A Return carrying `cli_status` is covered by the DTO itself; the conversion from the
    // core type is the part worth pinning here, since it maps a local type to a message.
    let wire = cli_status();
    assert_eq!(
        wire.located,
        Some(WireLocated {
            argv: vec!["claude".into()],
            path: PathBuf::from("/usr/local/bin/claude"),
            source: "path".into(),
            version: Some("2.1.0".into()),
        })
    );
    assert_eq!(wire.installed_via.as_deref(), Some("npm"));
    assert!(wire.can_upgrade);
    let back: core::CliStatus = wire.into();
    assert_eq!(back.located.unwrap().source, core::CliSource::Path);
}

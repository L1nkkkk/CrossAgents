//! A node: the machine that runs agents (docs/tasks/B1-NODE.md).
//!
//! One execution path serves both a node on another machine and the hub's built-in node:
//!
//! - [`protocol`] — the JSON-line frames (`Call`, `Return`, `Event`, `Ask`, `Ack`, ...) and
//!   the wire shapes of `SessionSpec`, `CoreError` and `CliStatus`.
//! - [`service::NodeService`] — runs `Driver` / `DriverSession` calls, numbers and caches the
//!   frames until the hub acks them, and replays them after a reconnect. It reaches drivers
//!   through a [`service::DriverSource`], so it does not depend on the hub's plugin registry.
//! - [`transport`] — the connection interface ([`transport::Transport`],
//!   [`transport::TransportServer`]) and an in-memory pair that can be disconnected and
//!   reconnected. B1.3 adds a QUIC stream behind the same traits.
//! - [`remote`] — the hub's [`crossagents_core::Driver`] /
//!   [`crossagents_core::DriverSession`] over that connection: [`remote::RemoteDriver`] and
//!   [`remote::RemoteSession`].

pub mod protocol;
pub mod remote;
pub mod service;
pub mod transport;

pub use protocol::{
    Op, PluginSnapshot, RegisterInfo, SessionInfo, ToHub, ToNode, WireCliStatus, WireError,
    WireLocated, WirePrompt, WireReply, WireResult, WireSpec,
};
pub use remote::{DriverInfo, HubLink, LocalDriverInfo, RemoteDriver, RemoteSession};
pub use service::{ANSWER_TIMEOUT, DriverSource, NodeService, SingleDriverSource, UNACKED_LIMIT};
pub use transport::{
    MemoryHub, MemoryNode, Transport, TransportError, TransportServer, memory_pair,
};

//! The hub's built-in node (docs/tasks/B1-NODE.md §3.1, §8 B1.2).
//!
//! With `[hub] builtin_node` on, every driver the hub hands out is a [`RemoteDriver`] on a
//! node that runs in this process: the node's [`NodeService`] runs the plugin registry's own
//! drivers, and the hub reaches it over an in-memory link. The hub code above
//! [`PluginRegistry`] is unchanged: it still asks the registry for a driver and gets one.
//!
//! With the switch off nothing here runs and the registry hands out its drivers directly.

use std::sync::Arc;

use crossagents_core::{Agent, CoreError, Driver, DriverKind};
use crossagents_node::{DriverSource, LocalDriverInfo, NodeService, RemoteNode, memory_pair};
use crossagents_plugins::{DriverWrap, PluginRegistry};

/// The built-in node's id: what `agents.node` holds for an agent that runs on this hub's
/// machine, and what an empty `agents.node` means.
pub const LOCAL: &str = "local";

/// Answers the node's `Call`s from the registry's own (unwrapped) drivers.
struct RegistrySource(PluginRegistry);

impl DriverSource for RegistrySource {
    fn driver(&self, plugin: &str, driver: Option<&str>) -> Result<Arc<dyn Driver>, CoreError> {
        self.0.local_driver(plugin, driver)
    }
}

/// Hands out a [`RemoteDriver`] over the built-in node for each driver the registry builds.
struct NodeWrap {
    node: Arc<RemoteNode>,
    /// The node's task. It holds a clone of the registry, which holds the drivers: a driver
    /// that is dropped stops its agent process (a codex app-server's `Drop`, for one), so the
    /// task ends with the registry that hands the wrapper out, as the drivers did before.
    _serving: Serving,
}

/// Aborts the node's task when dropped.
struct Serving(tokio::task::JoinHandle<()>);

impl Drop for Serving {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl DriverWrap for NodeWrap {
    fn wrap(&self, plugin: &str, kind: DriverKind, local: Arc<dyn Driver>) -> Arc<dyn Driver> {
        Arc::new(
            self.node
                .driver(plugin, Some(kind), Arc::new(LocalDriverInfo::new(local))),
        )
    }
}

/// Starts the built-in node serving `plugins` and returns the registry that hands out its
/// drivers. Must be called inside the tokio runtime.
pub fn serve(plugins: PluginRegistry) -> PluginRegistry {
    let (link, node_end) = memory_pair();
    // The node reads the registry through an unwrapped clone: a wrapped one would serve the
    // node's own calls back to it.
    let service = NodeService::new(Arc::new(RegistrySource(plugins.clone())));
    let serving = Serving(tokio::spawn(service.run(Arc::new(node_end))));
    let node = Arc::new(RemoteNode::new(Arc::new(link)));
    plugins.with_wrap(Arc::new(NodeWrap {
        node,
        _serving: serving,
    }))
}

/// Refuses an agent that names a node other than the built-in one: remote nodes come with
/// B1.3. An agent with no node, or `local`, runs here.
pub fn check_node(agent: &Agent) -> Result<(), CoreError> {
    check_name(agent.node.as_deref(), &agent.name)
}

fn check_name(node: Option<&str>, agent: &str) -> Result<(), CoreError> {
    match node {
        None | Some("") | Some(LOCAL) => Ok(()),
        Some(node) => Err(CoreError::Unsupported(format!(
            "{agent} runs on node {node:?}, and remote nodes are not supported yet"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_built_in_node_or_none_is_accepted() {
        assert!(check_name(None, "a").is_ok());
        assert!(check_name(Some(""), "a").is_ok());
        assert!(check_name(Some(LOCAL), "a").is_ok());
        let err = check_name(Some("mac"), "a").unwrap_err();
        assert!(
            matches!(err, CoreError::Unsupported(ref m) if m.contains("mac")),
            "{err}"
        );
    }

    /// With the node on, each plugin's driver answers the way the registry's own does, and
    /// the same kind is the same driver object (the switch-off registry is the local one).
    #[tokio::test]
    async fn the_built_in_node_answers_as_the_registry_does() {
        let local = PluginRegistry::with_config(&Default::default());
        let served = serve(local.clone());
        for name in local.names() {
            let plain = local.get(&name).unwrap();
            let remote = served.get(&name).unwrap();
            assert_eq!(remote.name(), plain.name(), "{name}");
            assert_eq!(remote.kind(), plain.kind(), "{name}");
            assert_eq!(remote.models(), plain.models(), "{name}");
            assert_eq!(remote.installed(), plain.installed(), "{name}");
            let again = served.get(&name).unwrap();
            assert!(Arc::ptr_eq(&remote, &again), "{name} is one driver");
        }
    }
}

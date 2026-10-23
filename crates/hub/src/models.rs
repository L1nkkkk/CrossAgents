//! Which models the GUI offers per plugin, chosen on the settings page and kept in
//! `<home>/models.json` (`{ "<plugin>": ["<model id>", …] }`). A plugin without an entry
//! offers every model it has; the hub's own choices (e.g. a worker's model) stay within it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use crossagents_core::CoreError;
use crossagents_plugins::PluginRegistry;

use crate::accounts::Accounts;

pub struct ModelChoice {
    path: PathBuf,
    shown: RwLock<BTreeMap<String, Vec<String>>>,
}

impl ModelChoice {
    /// Reads `path`; a missing or unreadable file means every model is offered.
    pub fn load(path: PathBuf) -> Self {
        let shown = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!("ignoring {}: {err}", path.display());
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        };
        Self {
            path,
            shown: RwLock::new(shown),
        }
    }

    /// The models picked for `plugin`, or `None` when all are offered.
    pub fn shown(&self, plugin: &str) -> Option<Vec<String>> {
        self.shown.read().unwrap().get(plugin).cloned()
    }

    /// Offers only `models` of `plugin` from now on; `None` offers all again.
    pub fn set(&self, plugin: &str, models: Option<Vec<String>>) -> Result<(), CoreError> {
        let mut shown = self.shown.write().unwrap();
        let mut next = shown.clone();
        match models {
            Some(models) if models.is_empty() => {
                return Err(CoreError::Conflict(
                    "pick at least one model, or show them all".into(),
                ));
            }
            Some(mut models) => {
                let mut seen = std::collections::HashSet::new();
                models.retain(|m| seen.insert(m.clone()));
                next.insert(plugin.to_owned(), models);
            }
            None => {
                next.remove(plugin);
            }
        }
        let text =
            serde_json::to_string_pretty(&next).map_err(|e| CoreError::Storage(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| {
                CoreError::Storage(format!("cannot write {}: {e}", self.path.display()))
            })?;
        *shown = next;
        Ok(())
    }

    /// `all` narrowed to the models picked for `plugin`, in `all`'s order. Everything when
    /// none are picked, or when none of the picked ones is in `all` any more (a login that
    /// changed its models must not leave the plugin with nothing to run).
    pub fn filter(&self, plugin: &str, all: Vec<String>) -> Vec<String> {
        let Some(shown) = self.shown(plugin) else {
            return all;
        };
        let kept: Vec<String> = all.iter().filter(|m| shown.contains(m)).cloned().collect();
        if kept.is_empty() { all } else { kept }
    }
}

/// Every model `plugin` can run: what its logins report, unless `config.toml` lists them;
/// its built-in list until a login has reported.
pub async fn available(
    registry: &PluginRegistry,
    accounts: &Accounts,
    plugin: &str,
) -> Vec<String> {
    let builtin = registry.get(plugin).map(|d| d.models()).unwrap_or_default();
    if registry.models_pinned(plugin) {
        return builtin;
    }
    let reported = accounts.offered_models(plugin).await;
    if reported.is_empty() {
        builtin
    } else {
        reported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_are_kept_on_disk_and_narrow_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        let all = || vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];

        let choice = ModelChoice::load(path.clone());
        assert_eq!(choice.filter("codebuddy", all()), all());
        choice
            .set("codebuddy", Some(vec!["c".into(), "a".into(), "a".into()]))
            .unwrap();
        // The list's own order, not the order picked.
        assert_eq!(choice.filter("codebuddy", all()), ["a", "c"]);
        assert_eq!(choice.filter("codex", all()), all());

        let again = ModelChoice::load(path.clone());
        assert_eq!(again.shown("codebuddy").unwrap(), ["c", "a"]);
        // Picks that no longer exist fall back to everything.
        assert_eq!(again.filter("codebuddy", vec!["x".into()]), ["x"]);

        assert!(again.set("codebuddy", Some(Vec::new())).is_err());
        again.set("codebuddy", None).unwrap();
        assert_eq!(ModelChoice::load(path).shown("codebuddy"), None);
    }
}

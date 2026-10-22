//! Agent names: what people and teammates call an agent, separate from its id (what the hub
//! routes by) and its model (an attribute that can change). The hub makes up a name when none
//! is given, never reuses a taken one, and keeps vendor and model words out of the names it
//! makes up.

use crossagents_core::names::{self, model_words};
use crossagents_core::{Agent, CoreError, NameSource, Store, validate_agent_name};
use std::path::Path;

/// The words of every model the plugins know: no made-up name may contain one.
pub fn denied_words(plugins: &crossagents_plugins::PluginRegistry) -> Vec<String> {
    let models: Vec<String> = plugins.defaults().iter().flat_map(|d| d.models()).collect();
    model_words(models.iter().map(String::as_str))
}

/// A free name: `wanted`, or a made-up one when none is wanted. A taken `wanted` gets an
/// ordinal (`wanted 2`, …) when `ordinal` (an agent recruiting); a person who typed the name
/// gets a conflict instead, so they choose.
pub async fn free_name(
    store: &dyn Store,
    wanted: Option<&str>,
    ordinal: bool,
    deny: &[String],
) -> Result<(String, NameSource), CoreError> {
    let names: Vec<String> = store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| a.name)
        .collect();
    match wanted.map(str::trim).filter(|w| !w.is_empty()) {
        Some(wanted) => {
            validate_agent_name(wanted)?;
            let name = names::with_ordinal(wanted, &names);
            if !ordinal && name != wanted {
                return Err(CoreError::Conflict(format!(
                    "agent name {wanted:?} is already taken"
                )));
            }
            // An ordinal can push a long name over the limit.
            validate_agent_name(&name)?;
            Ok((name, NameSource::Chosen))
        }
        None => Ok((names::generate(&names, deny, seed()), NameSource::Generated)),
    }
}

fn seed() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize)
}

/// Agents from before names were for people. A chat's old display name was its first message:
/// it becomes the chat's title. Any other display name (e.g. "Ada") becomes the name. A name
/// made up from a model ("chat-gpt-6-luna", "opus-lead") is replaced by a made-up given name.
/// Safe to run on every start: agents already in the new shape are left alone.
pub async fn fix_up(store: &dyn Store, chats: &Path, deny: &[String]) -> Result<(), CoreError> {
    for agent in store.list_agents().await? {
        let chat = agent.cwd.starts_with(chats);
        let mut profile = agent.profile.clone();
        let old = profile.display_name.take();
        let mut name: Option<String> = None;
        if chat {
            if agent.topic.is_none()
                && let Some(title) = &old
            {
                store.update_agent_topic(agent.id, Some(title)).await?;
            }
        } else if let Some(display) = &old
            && validate_agent_name(display).is_ok()
            && !names::is_vendor_word(display, deny)
        {
            name = Some(display.clone());
        }
        let made_up = agent.name_source == NameSource::Generated
            && (names::is_vendor_word(&agent.name, deny) || agent.name.starts_with("chat-"));
        if old.is_some() {
            store.update_agent_profile(agent.id, &profile).await?;
        }
        let rename = match name {
            Some(display) if display.to_lowercase() != agent.name.to_lowercase() => {
                Some((display, NameSource::Chosen))
            }
            Some(_) => None,
            None if made_up => Some((String::new(), NameSource::Generated)),
            None => None,
        };
        if let Some((wanted, source)) = rename {
            rename_free(
                store,
                &agent,
                (!wanted.is_empty()).then_some(wanted.as_str()),
                source,
                deny,
            )
            .await?;
        }
    }
    Ok(())
}

/// Renames `agent` to `wanted` (with an ordinal if taken) or a made-up name.
async fn rename_free(
    store: &dyn Store,
    agent: &Agent,
    wanted: Option<&str>,
    source: NameSource,
    deny: &[String],
) -> Result<(), CoreError> {
    let (name, _) = free_name(store, wanted, true, deny).await?;
    store.rename_agent(agent.id, &name, source).await?;
    tracing::info!(id = %agent.id, from = %agent.name, to = %name, "agent renamed");
    Ok(())
}

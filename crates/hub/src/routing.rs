//! Which account or teammate a task's worker runs on, by quota (docs/tasks/DOGFOOD.md §5).

use crossagents_core::{AgentId, AgentKey, CoreError};
use crossagents_orchestrator::Route;

use crate::accounts::Health;
use crate::collab::HubHost;

/// The template's own account while it has quota; else another account of its vendor that
/// has; else a teammate (another vendor, or on its own account) that has.
pub async fn route(
    host: &HubHost,
    template: AgentId,
    team: &[AgentId],
    limited: Option<AgentId>,
) -> Result<Route, CoreError> {
    let store = host.bus.store();
    let accounts = &host.accounts;
    let template = store.get_agent(&AgentKey::Id(template)).await?;
    if let Some(limited) = limited
        && let Ok(agent) = store.get_agent(&AgentKey::Id(limited)).await
        && let Some(id) = accounts.account_for(&agent).await
    {
        accounts.mark_reached(&id, "rate limited");
    }
    if !accounts.manages(&template.plugin) {
        return Ok(Route::Same);
    }
    let current = accounts.account_for(&template).await;
    let health = match &current {
        Some(id) => accounts.health(id, template.model.as_deref()).await,
        None => Health::Unknown,
    };
    if health.usable() {
        return Ok(Route::Same);
    }
    let why = format!(
        "account {} ({})",
        current.as_deref().unwrap_or("?"),
        health.reason().unwrap_or("out of quota")
    );

    // Another account of the vendor: one known to have quota, else one not read yet (worth
    // a try once the current one is out).
    let mut untried = None;
    for account in store.list_accounts().await? {
        if account.provider != template.plugin || Some(&account.id) == current.as_ref() {
            continue;
        }
        match accounts
            .health(&account.id, template.model.as_deref())
            .await
        {
            Health::Ok => {
                return Ok(Route::Account {
                    reason: format!("runs on account {} instead: {why}", account.id),
                    account: account.id,
                });
            }
            Health::Unknown if untried.is_none() => untried = Some(account.id),
            _ => {}
        }
    }
    if let Some(account) = untried {
        return Ok(Route::Account {
            reason: format!(
                "runs on account {account} instead (its quota is not known yet): {why}"
            ),
            account,
        });
    }

    for mate in team.iter().filter(|m| **m != template.id) {
        let Ok(mate) = store.get_agent(&AgentKey::Id(*mate)).await else {
            continue;
        };
        let mate_account = accounts.account_for(&mate).await;
        if mate_account.is_some() && mate_account == current {
            continue;
        }
        let ok = match &mate_account {
            Some(id) => accounts.health(id, mate.model.as_deref()).await == Health::Ok,
            // A plugin whose accounts the hub does not manage has no quota to run out of.
            None => !accounts.manages(&mate.plugin),
        };
        if ok {
            return Ok(Route::Teammate {
                agent: mate.id,
                reason: format!(
                    "given to {} ({} {}) instead: {why}",
                    mate.name,
                    mate.plugin,
                    mate.model.as_deref().unwrap_or("default model")
                ),
            });
        }
    }
    Ok(Route::Exhausted {
        reason: format!("{why}; no other account or teammate has quota left"),
    })
}

// Who a task's new Worker is when the person picks a model while approving a plan: every
// installed plugin's models, grouped by family, each mapped back to the plugin that offers it
// and to a login of that plugin. Pure, tested in workerModels.test.ts.

import type { AccountQuota } from "@bindings/AccountQuota";
import type { Agent } from "@bindings/Agent";
import type { CreateAgentRequest } from "@bindings/CreateAgentRequest";
import type { PluginInfo } from "@bindings/PluginInfo";
import type { Key } from "./i18n";
import { groupModels, vendorLabel } from "./models.ts";
import { modelDisplay } from "./modelDisplay.ts";

/** The lead, when one is known: its plugin decides whose login a new worker uses. */
export type LeadRef = { plugin?: string; account?: string };

/** Why a plugin's models cannot be used now; the entry stays listed and disabled. */
export type WorkerBlock = { key: Key };

export type WorkerModelOption = {
  plugin: string;
  model: string;
  /** A login of this plugin, when it has any; absent means the vendor's own machine login. */
  account?: string;
  blocked?: WorkerBlock;
};

/** One family's models, in the order the settings page uses. */
export type WorkerModelGroup = { key: string; label: string; options: WorkerModelOption[] };

/** A model plus the plugin and login a new agent for it runs on. */
export type WorkerTarget = { plugin: string; model: string; account?: string };

/**
 * The login a new agent of `plugin` uses: the lead's own when it is the same plugin and that
 * login is signed in, else the plugin's first signed-in one. No accounts at all means the
 * vendor's own machine login, which is not ours to name.
 */
export function loginFor(plugin: string, accounts: readonly AccountQuota[], lead?: LeadRef): string | undefined {
  const own = accounts.filter((a) => a.account.provider === plugin);
  if (lead?.plugin === plugin) {
    // Same plugin: the lead's own login, exactly — including its absence, which means the
    // vendor's own login rather than any managed account.
    if (!lead.account) return undefined;
    const preferred = own.find((a) => a.account.id === lead.account);
    if (preferred?.status.logged_in) return preferred.account.id;
  }
  return own.find((a) => a.status.logged_in)?.account.id;
}

/** Why this plugin cannot run a model now; absent when it can (or when only the hub knows). */
export function pluginBlock(plugin: PluginInfo, accounts: readonly AccountQuota[]): WorkerBlock | undefined {
  // A native plugin whose CLI is not on this machine cannot start an agent, whatever its
  // configured models say.
  if (plugin.cli && !plugin.cli.installed) return { key: "worker.notInstalled" };
  const own = accounts.filter((a) => a.account.provider === plugin.name);
  return own.length > 0 && !own.some((a) => a.status.logged_in) ? { key: "worker.notLoggedIn" } : undefined;
}

/**
 * Every installed plugin's offered models, by family, newest first within a family. A plugin
 * with no signed-in login keeps its models in the list, disabled and with the reason.
 */
export function workerModelGroups(plugins: readonly PluginInfo[], accounts: readonly AccountQuota[] = [], lead?: LeadRef): WorkerModelGroup[] {
  const all: WorkerModelOption[] = [];
  for (const plugin of plugins) {
    if (!plugin.models.length) continue;
    const blocked = pluginBlock(plugin, accounts);
    const account = blocked ? undefined : loginFor(plugin.name, accounts, lead);
    for (const model of plugin.models) {
      all.push({ plugin: plugin.name, model, ...(account ? { account } : {}), ...(blocked ? { blocked } : {}) });
    }
  }
  return groupModels([...new Set(all.map((o) => o.model))]).map((family) => ({
    key: family.key,
    label: family.label,
    options: family.models.flatMap((model) => all.filter((o) => o.model === model)),
  }));
}

/**
 * "Sonnet 5.5", plus the plugin when another plugin offers a model of the same name: the
 * person must be able to tell the two apart rather than have one guessed for them.
 */
export function workerModelLabel(option: WorkerModelOption, all: readonly WorkerModelOption[], accounts: readonly AccountQuota[] = []): string {
  const shared = all.some((o) => o.model === option.model && o.plugin !== option.plugin);
  const label = modelDisplay(option.plugin, option.model, accounts, option.account).label;
  return shared ? `${label} · ${vendorLabel(option.plugin)}` : label;
}

/** The `<option>` value for a model: the plugin and the model, unambiguous across vendors. */
export function modelPickValue(plugin: string, model: string): string {
  return `model:${plugin}/${model}`;
}

/** The plugin and model a pick names; `undefined` when it is not a model pick. */
export function parseModelPick(value: string): { plugin: string; model: string } | undefined {
  const prefix = "model:";
  if (!value.startsWith(prefix)) return undefined;
  const rest = value.slice(prefix.length);
  const at = rest.indexOf("/");
  if (at <= 0 || at === rest.length - 1) return undefined;
  return { plugin: rest.slice(0, at), model: rest.slice(at + 1) };
}

/** The plugin, model and login a new worker for `model` runs on; never the lead's account unless
 * the model's plugin is the lead's too. */
export function workerTarget(plugin: string, model: string, accounts: readonly AccountQuota[], lead?: LeadRef): WorkerTarget {
  const account = loginFor(plugin, accounts, lead);
  return { plugin, model, ...(account ? { account } : {}) };
}

/** The `POST /v1/agents` body for a new worker on `target`, in the run's checkout. */
export function workerAgentRequest(target: WorkerTarget, cwd: string): CreateAgentRequest {
  return {
    plugin: target.plugin,
    driver: null,
    model: target.model,
    role: "worker",
    cwd,
    config: target.account ? { account: target.account } : {},
    profile: { display_name: null, title: "Worker", avatar: null, avatar_image: null, color: null, persona: null },
  };
}

export type ReasoningChoice = { model: string | null; effort: string };

/** A task-specific template preserves the worker's persona/config without changing it. */
export function reasoningWorkerRequest(template: Agent | CreateAgentRequest, cwd: string, choice: ReasoningChoice): CreateAgentRequest {
  const config = template.config && typeof template.config === "object" && !Array.isArray(template.config) ? { ...template.config } : {};
  if (choice.effort && template.plugin !== "cursor") config.effort = choice.effort;
  else delete config.effort;
  return {
    plugin: template.plugin, driver: template.driver, model: choice.model, role: "worker", cwd, config,
    profile: { ...template.profile, display_name: null },
  };
}

import assert from "node:assert/strict";
import { test } from "node:test";
import type { AccountQuota } from "@bindings/AccountQuota";
import type { AccountStatus } from "@bindings/AccountStatus";
import type { Capabilities } from "@bindings/Capabilities";
import type { PluginInfo } from "@bindings/PluginInfo";
import { hasKey, t } from "./i18n.ts";
import { modelPickValue, parseModelPick, pluginBlock, workerAgentRequest, workerModelGroups, workerModelLabel, workerTarget } from "./workerModels.ts";

const plugin = (name: string, models: string[]): PluginInfo => ({
  name,
  models,
  all_models: models,
  shown: null,
  models_pinned: false,
  driver: "acp-generic",
  capabilities: {} as Capabilities,
  efforts: [],
});

const account = (id: string, provider: string, logged_in: boolean): AccountQuota => ({
  account: { id, provider, label: id, codebuddy_site: null, home: `/home/${id}`, created_at: "2026-01-01T00:00:00Z" },
  status: { logged_in } as AccountStatus,
  models: [],
  agents: [],
});

test("every installed plugin's models are listed, grouped by family", () => {
  const groups = workerModelGroups([plugin("claude", ["claude-opus-4-1"]), plugin("codex", ["gpt-5-codex"]), plugin("with", ["glm-5.3", "claude-sonnet-4-5"])]);
  assert.deepEqual(
    groups.map((g) => g.key),
    ["claude", "openai", "glm"],
  );
  assert.deepEqual(
    groups.flatMap((g) => g.options.map((o) => `${o.plugin}/${o.model}`)),
    ["claude/claude-opus-4-1", "with/claude-sonnet-4-5", "codex/gpt-5-codex", "with/glm-5.3"],
  );
  // A plugin with nothing on offer contributes nothing.
  assert.deepEqual(workerModelGroups([plugin("codebuddy", [])]), []);
});

test("a model from another plugin makes the worker on that plugin and login, not the lead's", () => {
  const accounts = [account("claude-max", "claude", true), account("with-main", "with", true)];
  const lead = { plugin: "claude", account: "claude-max" };
  const target = workerTarget("with", "deepseek-v4-flash", accounts, lead);
  assert.deepEqual(target, { plugin: "with", model: "deepseek-v4-flash", account: "with-main" });

  const request = workerAgentRequest(target, "C:/work/shop");
  assert.equal(request.plugin, "with");
  assert.equal(request.model, "deepseek-v4-flash");
  assert.equal(request.role, "worker");
  assert.equal(request.cwd, "C:/work/shop");
  assert.equal((request.config as Record<string, unknown>).account, "with-main");
  // The lead's login pays only for its own plugin.
  assert.notEqual((request.config as Record<string, unknown>).account, "claude-max");
});

test("the same plugin reuses the lead's login", () => {
  const accounts = [account("claude-max", "claude", true), account("claude-pro", "claude", true)];
  assert.equal(workerTarget("claude", "claude-sonnet-4-5", accounts, { plugin: "claude", account: "claude-pro" }).account, "claude-pro");
  // A lead with no explicit login uses the vendor's own login, not a managed account.
  assert.equal(workerTarget("claude", "claude-sonnet-4-5", accounts, { plugin: "claude" }).account, undefined);
  // A logged-out lead login falls back to one that is signed in.
  const mixed = [account("claude-max", "claude", false), account("claude-pro", "claude", true)];
  assert.equal(workerTarget("claude", "claude-sonnet-4-5", mixed, { plugin: "claude", account: "claude-max" }).account, "claude-pro");
  // No account of ours means the vendor's own machine login, which is not named.
  assert.equal(workerTarget("with", "glm-5.3", []).account, undefined);
});

test("a plugin with no signed-in login keeps its models, disabled with the reason", () => {
  const groups = workerModelGroups([plugin("kimi", ["kimi-k3"])], [account("kimi-work", "kimi", false)]);
  assert.equal(groups[0].options[0].blocked?.key, "worker.notLoggedIn");
  assert.equal(t(groups[0].options[0].blocked!.key), "未登录");
  assert.equal(pluginBlock(plugin("kimi", ["kimi-k3"]), [account("kimi-work", "kimi", false)])?.key, "worker.notLoggedIn");
  assert.equal(pluginBlock(plugin("kimi", ["kimi-k3"]), [account("kimi-work", "kimi", true)]), undefined);
});

test("a native plugin whose CLI is not installed is disabled with that reason", () => {
  const missing: PluginInfo = { ...plugin("codebuddy", ["hy4-preview"]), cli: { installed: false } as PluginInfo["cli"] };
  const groups = workerModelGroups([missing]);
  assert.equal(groups[0].options[0].blocked?.key, "worker.notInstalled");
  assert.equal(t(groups[0].options[0].blocked!.key), "CLI 未安装");
});

test("a model offered by two plugins carries its plugin in the label", () => {
  const options = workerModelGroups([plugin("with", ["glm-5.3"]), plugin("gateway", ["glm-5.3"])]).flatMap((g) => g.options);
  assert.equal(options.length, 2);
  assert.equal(workerModelLabel(options[0], options), "Glm 5.3 · With");
  assert.equal(workerModelLabel(options[1], options), "Glm 5.3 · Gateway");
  const single = workerModelGroups([plugin("with", ["glm-5.3"])]).flatMap((g) => g.options);
  assert.equal(workerModelLabel(single[0], single), "Glm 5.3");
});

test("picks carry plugin and model, and malformed ones are refused", () => {
  assert.deepEqual(parseModelPick(modelPickValue("with", "deepseek/v4")), { plugin: "with", model: "deepseek/v4" });
  assert.equal(parseModelPick("model:"), undefined);
  assert.equal(parseModelPick("model:with/"), undefined);
  assert.equal(parseModelPick("agent:a1"), undefined);
});

test("every key this module can return is in the dictionary", () => {
  const keys = ["worker.notLoggedIn", "worker.notInstalled", "worker.unknownPick", "plan.newWorkerFamily"];
  assert.deepEqual(keys.filter((k) => !hasKey(k)), []);
});

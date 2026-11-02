import assert from "node:assert/strict";
import { test } from "node:test";
import { agentNameFor, chatNameFor, chatTitle, effortLabel, effortLevels, effortOf, groupModels, modelFamily, onlyModels, prettyModel, switchModels, vendorLabel, workerModelFor } from "./models.ts";

test("models fall into their family, whatever gateway prefix they carry", () => {
  const family = (m: string) => modelFamily(m).key;
  assert.equal(family("claude-4.6-opus-1m-context"), "claude");
  assert.equal(family("opus"), "claude");
  assert.equal(family("gpt-6.1-sol"), "openai");
  assert.equal(family("tokenhub_deepseek-v4-flash"), "deepseek");
  assert.equal(family("ext-glm-5.2"), "glm");
  assert.equal(family("tokenhub_kimi-k3"), "kimi");
  assert.equal(family("hy3-preview"), "hunyuan");
  assert.equal(family("hunyuan-2.0-thinking"), "hunyuan");
  assert.equal(family("hy4-preview-ioa"), "hunyuan");
  assert.equal(family("minimax-m3-ioa"), "minimax");
  assert.equal(family("echo"), "other");
  assert.equal(family("mimo-v2.6-pro"), "mimo");
  assert.equal(family("grok-4.2"), "grok");
  assert.equal(family("doubao-seed-1.6"), "doubao");
});

test("groups come in a fixed order, newest model first", () => {
  const groups = groupModels(["echo", "glm-5", "claude-opus-5", "glm-5.3", "claude-4.6-sonnet", "gpt-5.4"]);
  assert.deepEqual(
    groups.map((g) => g.key),
    ["claude", "openai", "glm", "other"],
  );
  assert.deepEqual(groups.find((g) => g.key === "glm")?.models, ["glm-5.3", "glm-5"]);
  const glm = groupModels(["ext-glm-5.2", "glm-5.2", "tokenhub_glm-5.3-flash", "glm-4.7", "glm-5.3-flash"])[0].models;
  assert.deepEqual(glm, ["glm-5.3-flash", "tokenhub_glm-5.3-flash", "glm-5.2", "ext-glm-5.2", "glm-4.7"]);
  const claude = groupModels(["claude-4.8-opus", "claude-opus-5", "claude-sonnet-5.5", "claude-5-sonnet", "claude-4.5-sonnet"])[0].models;
  assert.deepEqual(claude, ["claude-sonnet-5.5", "claude-5-sonnet", "claude-opus-5", "claude-4.8-opus", "claude-4.5-sonnet"]);
});

test("models switch on and off together, and one always stays", () => {
  const all = ["a", "b", "c", "d"];
  assert.deepEqual(switchModels(all, new Set(["a"]), ["c", "b"], true), ["a", "b", "c"]);
  assert.equal(switchModels(all, new Set(["a", "b"]), ["c", "d"], true), null);
  assert.deepEqual(switchModels(all, new Set(["a", "b", "c"]), ["a"], false), ["b", "c"]);
  assert.equal(switchModels(all, new Set(["a"]), ["a"], false), undefined);
  assert.deepEqual(onlyModels(all, ["d", "b"]), ["b", "d"]);
  assert.equal(onlyModels(all, all), null);
  assert.equal(onlyModels(all, []), undefined);
});

test("model ids read like names", () => {
  assert.equal(prettyModel("claude-opus-4-1"), "Opus 4.1");
  assert.equal(prettyModel("claude-sonnet-4-5"), "Sonnet 4.5");
  assert.equal(prettyModel("gpt-5-codex"), "GPT-5 Codex");
  assert.equal(prettyModel("gpt-5-mini"), "GPT-5 Mini");
  assert.equal(prettyModel("opus"), "Opus");
  assert.equal(vendorLabel("codex"), "Codex");
});

test("workers get the cheap sibling when there is one", () => {
  assert.equal(workerModelFor(["claude-opus-4-1", "claude-sonnet-4-5", "claude-haiku-4-5"], "claude-opus-4-1"), "claude-haiku-4-5");
  assert.equal(workerModelFor(["gpt-5-codex", "gpt-5"], "gpt-5-codex"), "gpt-5-codex");
});

test("agent names are valid and free", () => {
  assert.equal(agentNameFor("planner", "claude-opus-4-1", new Set()), "lead-opus-4-1");
  assert.equal(agentNameFor("worker", "gpt-5-codex", new Set(["worker-gpt-5-codex"])), "worker-gpt-5-codex-2");
  assert.match(agentNameFor("planner", "Some Model!", new Set()), /^[a-z][a-z0-9_-]{0,31}$/);
});

test("chats are named after the model and titled by their first line", () => {
  assert.equal(chatNameFor("claude-sonnet-4-5", new Set(["chat-sonnet-4-5"])), "chat-sonnet-4-5-2");
  assert.equal(chatTitle("  修一下登录页\n细节如下……"), "修一下登录页");
  assert.equal(chatTitle("a".repeat(30), 10), "aaaaaaaaa…");
});

test("effort levels come from the plugin, and from an agent's config", () => {
  assert.deepEqual(effortLevels(["minimal", "low", "turbo"]), [
    { value: "minimal", label: "最低" },
    { value: "low", label: "低" },
    { value: "turbo", label: "turbo" },
  ]);
  assert.deepEqual(effortLevels([]), []);
  assert.deepEqual(effortLevels(undefined), []);
  assert.equal(effortLabel("xhigh"), "超高");
  assert.equal(effortOf({ effort: "high", account: "a" }), "high");
  assert.equal(effortOf(null), undefined);
  assert.equal(effortOf({ effort: "" }), undefined);
});

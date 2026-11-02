import assert from "node:assert/strict";
import { test } from "node:test";
import { familyKeys, modelFamily } from "./modelFamily.ts";

test("the model decides the family, whichever plugin runs it", () => {
  const key = (plugin: string, model: string | null) => modelFamily(plugin, model)?.key;
  assert.equal(key("claude", "claude-opus-5"), "claude");
  assert.equal(key("claude", "haiku"), "claude");
  assert.equal(key("codex", "gpt-6.1-terra"), "openai");
  assert.equal(key("claude", "deepseek-v4.1-flash"), "deepseek");
  assert.equal(key("codebuddy", "glm-5.3-flash"), "glm");
  assert.equal(key("codebuddy", "step-5-preview"), "step");
});

test("the brands behind the gateways keep their own family", () => {
  const key = (model: string) => modelFamily("claude", model)?.key;
  assert.equal(key("tokenhub_deepseek-v4-flash"), "deepseek");
  assert.equal(key("ext-glm-5.2"), "glm");
  assert.equal(key("tokenhub_kimi-k3"), "kimi");
  assert.equal(key("azure_gpt-5.4"), "openai");
  assert.equal(key("mimo-v2.6-pro"), "mimo");
  assert.equal(key("xiaomi-mimo-v2"), "mimo");
  assert.equal(key("grok-4.2"), "grok");
  assert.equal(key("xai-grok-4"), "grok");
  assert.equal(key("doubao-seed-1.6"), "doubao");
  assert.equal(key("ernie-4.5"), "wenxin");
  assert.equal(key("mistral-large-3"), "mistral");
  assert.equal(key("hy4-preview-ioa"), "hunyuan");
  assert.equal(key("qwen3-max"), "qwen");
  assert.equal(key("yi-lightning"), "yi");
  assert.equal(key("baichuan-4"), "baichuan");
  assert.equal(key("internlm-3"), "internlm");
  assert.equal(key("spark-4.0"), "spark");
  assert.equal(key("sensenova-6"), "sensenova");
  assert.equal(key("skywork-2"), "skywork");
  assert.equal(key("longcat-flash"), "longcat");
});

test("with no model the plugin's own family stands in; an unknown model claims none", () => {
  assert.equal(modelFamily("claude", null)?.key, "claude");
  assert.equal(modelFamily("codex", "")?.key, "openai");
  assert.equal(modelFamily("codebuddy", null), null);
  assert.equal(modelFamily("claude", "my-local-model"), null);
});

test("the family table is the one the marks are keyed by", () => {
  const keys = familyKeys();
  assert.equal(new Set(keys).size, keys.length);
  assert.equal(keys[0], "claude");
  // `openai` is the key of the OpenAI mark, not `gpt`.
  assert.ok(keys.includes("openai"));
  assert.ok(!keys.includes("gpt"));
  for (const key of ["mimo", "grok", "doubao", "wenxin", "mistral"]) assert.ok(keys.includes(key), key);
  assert.equal(modelFamily("claude", "mimo-v2.6-pro")?.name, "MiMo");
});

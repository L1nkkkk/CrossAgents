import assert from "node:assert/strict";
import { test } from "node:test";
import { modelFamily } from "./modelFamily.ts";

test("the model decides the family, whichever plugin runs it", () => {
  const key = (plugin: string, model: string | null) => modelFamily(plugin, model)?.key;
  assert.equal(key("claude", "claude-opus-5"), "claude");
  assert.equal(key("claude", "haiku"), "claude");
  assert.equal(key("codex", "gpt-6.1-terra"), "openai");
  assert.equal(key("claude", "deepseek-v4.1-flash"), "deepseek");
  assert.equal(key("codebuddy", "glm-5.3-flash"), "glm");
  assert.equal(key("codebuddy", "step-5-preview"), "step");
});

test("with no model the plugin's own family stands in; an unknown model claims none", () => {
  assert.equal(modelFamily("claude", null)?.key, "claude");
  assert.equal(modelFamily("codex", "")?.key, "openai");
  assert.equal(modelFamily("codebuddy", null), null);
  assert.equal(modelFamily("claude", "my-local-model"), null);
});

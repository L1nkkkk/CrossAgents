import assert from "node:assert/strict";
import { test } from "node:test";
import { cursorMode, cursorVariants, cursorWith, reasoningModels } from "./reasoning.ts";

const models = ["auto", "claude-opus-5-thinking-high", "claude-opus-5-high", "claude-opus-5-medium", "claude-opus-5-thinking-max", "claude-opus-5-thinking-high-fast", "composer-2.5", "composer-2.5-fast"];

test("Cursor mode parsing preserves model version numbers and handles suffix order", () => {
  assert.deepEqual(cursorMode("claude-opus-5-5-thinking-high-fast"), { base: "claude-opus-5-5", effort: "high", thinking: true, fast: true });
  assert.deepEqual(cursorMode("claude-4.6-sonnet-medium-thinking"), { base: "claude-4.6-sonnet", effort: "medium", thinking: true, fast: false });
  assert.equal(cursorMode("composer-2.5").base, "composer-2.5");
  assert.equal(cursorMode("deepseek-v41-flash-1017-none").effort, "none");
});

test("Cursor catalog collapses combinations and keeps selected variants without affecting other providers", () => {
  assert.deepEqual(reasoningModels("cursor", models, "claude-opus-5-medium"), ["auto", "claude-opus-5-medium", "composer-2.5"]);
  assert.deepEqual(reasoningModels("claude", models), models);
  assert.deepEqual(cursorVariants(models, "composer-2.5"), ["composer-2.5", "composer-2.5-fast"]);
});

test("controls select only offered variants and effort never silently changes Thinking or Fast", () => {
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high", { thinking: false }), "claude-opus-5-high");
  assert.equal(cursorWith(models, "claude-opus-5-medium", { thinking: true }), "claude-opus-5-thinking-high");
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high", { effort: "medium" }), undefined);
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high-fast", { effort: "max" }), undefined);
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high", { fast: true }), "claude-opus-5-thinking-high-fast");
  assert.equal(cursorWith(models, "auto", { effort: "high" }), undefined);
});

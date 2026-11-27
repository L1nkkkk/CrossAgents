import assert from "node:assert/strict";
import { test } from "node:test";
import { cursorFamilies, cursorMode, cursorModeLabel, cursorVariants, cursorVariantsOrdered, cursorWith, reasoningModels, usesVariantReasoning } from "./reasoning.ts";

const models = ["auto", "claude-opus-5-thinking-high", "claude-opus-5-high", "claude-opus-5-medium", "claude-opus-5-thinking-max", "claude-opus-5-thinking-high-fast", "composer-2.5", "composer-2.5-fast"];

test("Cursor mode parsing preserves model version numbers and handles suffix order", () => {
  assert.deepEqual(cursorMode("claude-opus-5-5-thinking-high-fast"), { base: "claude-opus-5-5", effort: "high", thinking: true, fast: true });
  assert.deepEqual(cursorMode("claude-4.6-sonnet-medium-thinking"), { base: "claude-4.6-sonnet", effort: "medium", thinking: true, fast: false });
  assert.equal(cursorMode("composer-2.5").base, "composer-2.5");
  assert.equal(cursorMode("deepseek-v41-flash-1017-none").effort, "none");
});

test("Cursor extra-high suffix is effort xhigh, not part of the base", () => {
  assert.deepEqual(cursorMode("gpt-5.5-extra-high"), { base: "gpt-5.5", effort: "xhigh", thinking: false, fast: false });
  assert.deepEqual(cursorMode("gpt-5.5-extra-high-fast"), { base: "gpt-5.5", effort: "xhigh", thinking: false, fast: true });
  assert.deepEqual(cursorMode("gpt-5.5-high"), { base: "gpt-5.5", effort: "high", thinking: false, fast: false });
  assert.equal(cursorMode("gpt-5.5").base, "gpt-5.5");
});

test("Cursor and Antigravity catalogs collapse combinations and keep selected variants without affecting other providers", () => {
  assert.equal(usesVariantReasoning("cursor"), true);
  assert.equal(usesVariantReasoning("antigravity"), true);
  assert.equal(usesVariantReasoning("claude"), false);
  assert.deepEqual(reasoningModels("cursor", models, "claude-opus-5-medium"), ["auto", "claude-opus-5-medium", "composer-2.5"]);
  assert.deepEqual(
    reasoningModels(
      "antigravity",
      ["gemini-4-neon-high", "gemini-4-neon-xhigh", "gemini-4-neon-medium", "gemini-4-neon-low", "gpt-oss-120b-medium"],
      "gemini-4-neon-xhigh",
    ),
    ["gemini-4-neon-xhigh", "gpt-oss-120b-medium"],
  );
  // Signed-out fallback bare models stay untouched.
  assert.deepEqual(reasoningModels("antigravity", ["gemini-3.8-flash", "gemini-3.1-pro"]), ["gemini-3.8-flash", "gemini-3.1-pro"]);
  assert.deepEqual(reasoningModels("claude", models), models);
  assert.deepEqual(cursorVariants(models, "composer-2.5"), ["composer-2.5", "composer-2.5-fast"]);
});

test("cursorFamilies groups every variant under its base in first-seen order", () => {
  const more = [...models, "gpt-5.5-extra-high", "gpt-5.5", "gpt-5.5-extra-high-fast", "gpt-5.5-low"];
  assert.deepEqual(cursorFamilies(more), [
    { base: "auto", variants: ["auto"] },
    { base: "claude-opus-5", variants: ["claude-opus-5-thinking-high", "claude-opus-5-high", "claude-opus-5-medium", "claude-opus-5-thinking-max", "claude-opus-5-thinking-high-fast"] },
    { base: "composer-2.5", variants: ["composer-2.5", "composer-2.5-fast"] },
    { base: "gpt-5.5", variants: ["gpt-5.5-extra-high", "gpt-5.5", "gpt-5.5-extra-high-fast", "gpt-5.5-low"] },
  ]);
});

test("cursorModeLabel names effort, then Thinking and Fast, or 默认", () => {
  assert.equal(cursorModeLabel("claude-opus-5-thinking-high-fast"), "高 · Thinking · Fast");
  assert.equal(cursorModeLabel("gpt-5.5-extra-high"), "超高");
  assert.equal(cursorModeLabel("composer-2.5-fast"), "Fast");
  assert.equal(cursorModeLabel("claude-4.6-sonnet-thinking"), "Thinking");
  assert.equal(cursorModeLabel("composer-2.5"), "默认");
});

test("cursorVariantsOrdered sorts by effort, then standard before Thinking, then Fast off before on", () => {
  const more = ["gpt-5.5-extra-high-fast", "gpt-5.5-max", "gpt-5.5-extra-high", "gpt-5.5", "gpt-5.5-low", "gpt-5.5-none", "composer-2.5"];
  assert.deepEqual(cursorVariantsOrdered(more, "gpt-5.5-low"), ["gpt-5.5", "gpt-5.5-none", "gpt-5.5-low", "gpt-5.5-extra-high", "gpt-5.5-extra-high-fast", "gpt-5.5-max"]);
  assert.deepEqual(cursorVariantsOrdered(models, "claude-opus-5-high"), [
    "claude-opus-5-medium", "claude-opus-5-high", "claude-opus-5-thinking-high", "claude-opus-5-thinking-high-fast", "claude-opus-5-thinking-max",
  ]);
  assert.deepEqual(cursorVariantsOrdered(models, "auto"), ["auto"]);
});

test("controls select only offered variants and effort never silently changes Thinking or Fast", () => {
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high", { thinking: false }), "claude-opus-5-high");
  assert.equal(cursorWith(models, "claude-opus-5-medium", { thinking: true }), "claude-opus-5-thinking-high");
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high", { effort: "medium" }), undefined);
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high-fast", { effort: "max" }), undefined);
  assert.equal(cursorWith(models, "claude-opus-5-thinking-high", { fast: true }), "claude-opus-5-thinking-high-fast");
  assert.equal(cursorWith(models, "auto", { effort: "high" }), undefined);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { agentNameFor, chatNameFor, chatTitle, prettyModel, vendorLabel, workerModelFor } from "./models.ts";

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

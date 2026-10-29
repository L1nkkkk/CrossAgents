import assert from "node:assert/strict";
import { test } from "node:test";
import { fields, plainText, toolChanges, toolError, toolFailed, toolShowsStat, type Call } from "./toolcall.ts";

const diff = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -12,1 +12,2 @@\n-## Quick start\n+## 快速开始\n+\n";
const change = { path: "README.md", kind: "update", diff };
const MISMATCH = "模型返回的待替换代码块未匹配到整行";

test("a failed edit shows no counts, and says why in the tool's words", () => {
  // What with-cli answers when the block the model meant to replace is not there: the detail still
  // carries the diff the model planned, which is why +2 −1 used to be shown for an edit that never
  // touched the file.
  const call: Call = { status: "failed", detail: { changes: [change], arguments: JSON.stringify({ path: "README.md" }), error: MISMATCH } };
  assert.equal(toolFailed(call), true);
  assert.equal(toolShowsStat(call), false);
  assert.deepEqual(toolChanges(call), []);
  assert.equal(toolError(call), MISMATCH);
});

test("a successful edit keeps its counts and has no error", () => {
  const call: Call = { status: "completed", detail: { changes: [change], result: JSON.stringify({ ok: true, changed: 1 }) } };
  assert.equal(toolFailed(call), false);
  assert.equal(toolShowsStat(call), true);
  assert.deepEqual(toolChanges(call), [change]);
  assert.equal(toolError(call), undefined);
});

test("failed but no detail.error: the result's text is the reason", () => {
  const call: Call = { status: "failed", detail: { changes: [change], result: MISMATCH } };
  assert.equal(toolFailed(call), true);
  assert.equal(toolError(call), MISMATCH);
  assert.deepEqual(toolChanges(call), []);
});

test("an error recorded on a call that finished counts as failed", () => {
  const call: Call = { status: "completed", detail: { changes: [change], error: "permission denied" } };
  assert.equal(toolFailed(call), true);
  assert.equal(toolShowsStat(call), false);
  assert.deepEqual(toolChanges(call), []);
  assert.equal(toolError(call), "permission denied");
});

test("an MCP result wrapped in content[0].text is read as the tool's words", () => {
  const wrapped = JSON.stringify({ content: [{ type: "text", text: "conflict: not recruited" }] });
  assert.equal(toolError({ status: "failed", detail: { changes: [], result: wrapped } }), "conflict: not recruited");
});

test("a JSON object result is fields, not a message", () => {
  const call: Call = { status: "completed", detail: { changes: [], result: JSON.stringify({ agent: "Iris", model: "gpt-6-luna" }) } };
  assert.equal(toolError(call), undefined);
  assert.deepEqual(fields(call.detail?.result), { agent: "Iris", model: "gpt-6-luna" });
  assert.equal(plainText(undefined), undefined);
});

test("a call with nothing reported is not a failure and has no counts", () => {
  const call: Call = { status: "in_progress" };
  assert.equal(toolFailed(call), false);
  assert.deepEqual(toolChanges(call), []);
  assert.equal(toolShowsStat(call), true);
  assert.equal(toolError(call), undefined);
});

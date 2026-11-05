import assert from "node:assert/strict";
import { test } from "node:test";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import { cardFor, cardToolName, extensionCallTarget, normalizeToolName } from "./toolcards.ts";

type Call = Extract<TranscriptItem, { type: "tool_call" }>;

const call = (title: string, args?: string): Call => ({
  type: "tool_call",
  tool_call_id: "c1",
  title,
  kind: "mcp",
  status: "completed",
  ...(args === undefined ? {} : { detail: { changes: [], arguments: args } }),
});

test("the hub's tool name is the same under the dot and the MCP prefix", () => {
  assert.equal(normalizeToolName("crossagents.agent_spawn"), "agent_spawn");
  assert.equal(normalizeToolName("mcp__crossagents__agent_spawn"), "agent_spawn");
  assert.equal(normalizeToolName("crossagents.standup__report"), "standup__report");
  assert.equal(normalizeToolName("mcp__crossagents__standup__report"), "standup__report");
  assert.equal(normalizeToolName("  mcp__crossagents__agent_spawn  "), "agent_spawn");
  // Another vendor's tool is left alone.
  assert.equal(normalizeToolName("mcp__other__thing"), "mcp__other__thing");
});

test("a card is matched after normalizing the transcript's tool name", () => {
  const cards = new Map([["standup__report", "card"]]);
  assert.equal(cardFor(call("mcp__crossagents__standup__report"), cards), "card");
  assert.equal(cardFor(call("crossagents.standup__report"), cards), "card");
  assert.equal(cardFor(call("mcp__crossagents__agent_list"), cards), undefined);
});

test("extension_call is matched by the real tool in input.tool", () => {
  const item = call("mcp__crossagents__extension_call", JSON.stringify({ tool: "standup__report", input: { days: 1 } }));
  assert.equal(extensionCallTarget(item), "standup__report");
  assert.equal(cardToolName(item), "standup__report");
  assert.equal(cardFor(item, new Map([["standup__report", "card"]])), "card");

  // Dotted prefix too. A bare tool name is not matched to someone else's card: exact only.
  const dotted = call("crossagents.extension_call", JSON.stringify({ tool: "report" }));
  assert.equal(cardToolName(dotted), "report");
  assert.equal(cardFor(dotted, new Map([["standup__report", "card"]])), undefined);
});

test("extension cards never hijack built-in tools of the same bare name", () => {
  const cards = new Map([
    ["standup__task_list", "list card"],
    ["standup__task_accept", "accept card"],
  ]);
  for (const title of [
    "crossagents.task_list",
    "mcp__crossagents__task_list",
    "crossagents.task_accept",
    "mcp__crossagents__task_accept",
  ]) {
    assert.equal(cardFor(call(title), cards), undefined, title);
  }
  // Its own tool still matches, exactly.
  assert.equal(cardFor(call("mcp__crossagents__standup__task_list"), cards), "list card");
  assert.equal(cardFor(call("mcp__crossagents__extension_call", JSON.stringify({ tool: "standup__task_accept" })), cards), "accept card");
});

test("extension_call without a usable tool falls back to its own name", () => {
  assert.equal(extensionCallTarget(call("crossagents.extension_call")), undefined);
  assert.equal(extensionCallTarget(call("crossagents.extension_call", "{not json")), undefined);
  assert.equal(extensionCallTarget(call("crossagents.extension_call", JSON.stringify({ tool: 3 }))), undefined);
  assert.equal(cardToolName(call("crossagents.extension_call")), "extension_call");
  // A plain call is never treated as an extension_call.
  assert.equal(extensionCallTarget(call("crossagents.agent_spawn", JSON.stringify({ tool: "run_land" }))), undefined);
});

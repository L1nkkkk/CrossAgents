import { test } from "node:test";
import { strict as assert } from "node:assert";
// The extension is plain JavaScript, loaded directly by Electron without a build.
// @ts-expect-error The independently loaded extension has no TypeScript declarations.
import { agentsOf, createStore } from "../../../../extensions/debug/gui/index.js";

test("groups events like the built-in log", () => {
  assert.deepEqual(agentsOf({ type: "agent.output", data: { id: "a", author: "b" } }), ["a"]);
  assert.deepEqual(agentsOf({ type: "task.changed", data: { assignee: "a", author: "b", item: { from: "c" } } }), ["a", "b", "c"]);
  assert.deepEqual(agentsOf({ type: "other" }), []);
});

test("store keeps at most 1000 events per agent and survives subscriber removal", () => {
  const store = createStore();
  let changes = 0;
  const off = store.subscribe(() => changes++);
  for (let seq = 0; seq < 1001; seq++) store.add({ seq, type: "agent.output", data: { id: "a" } });
  off();
  store.add({ seq: 1001, type: "agent.output", data: { id: "a" } });
  assert.equal(changes, 1001);
  assert.equal(store.get("a").length, 1000);
  assert.equal(store.get("a")[0].seq, 2);
  assert.equal(createStore().get("a").length, 0);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { groupRunsByOrigin } from "./sidebarTree.ts";

const run = (id: string, origin?: string | null, planner = `lead-${id}`) => ({
  run: { id, planner, origin_chat_id: origin },
});

test("multiple runs nest by stable source id, even with different leads", () => {
  const a = run("a", "chat");
  const b = run("b", "chat");
  const c = run("c", "other");
  const tree = groupRunsByOrigin([a, b, c], new Set(["chat", "other"]));
  assert.deepEqual(tree.children.get("chat"), [a, b]);
  assert.deepEqual(tree.children.get("other"), [c]);
  assert.deepEqual(tree.standalone, []);
});

test("old and Home runs stay standalone", () => {
  const old = { run: { id: "old", planner: "old-lead" } };
  const home = run("home", null);
  const tree = groupRunsByOrigin([old, home], new Set(["chat"]));
  assert.deepEqual(tree.standalone, [old, home]);
  assert.equal(tree.children.size, 0);
});

test("missing or archived sources do not hide their runs", () => {
  const missing = run("missing", "gone");
  const archived = run("archived", "archived-chat");
  assert.deepEqual(groupRunsByOrigin([missing, archived], new Set()).standalone, [missing, archived]);
});

test("a self-reference remains standalone rather than nesting the lead under itself", () => {
  const legacy = run("self", "same", "same");
  assert.deepEqual(groupRunsByOrigin([legacy], new Set(["same"])).standalone, [legacy]);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { capped, childPath, groupRunsByOrigin } from "./sidebarTree.ts";

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

const none = () => false;

test("a search that matches only a child run keeps its chat, showing just that child", () => {
  const hit = run("hit", "chat");
  const miss = run("miss", "chat");
  const path = childPath([hit, miss], { matching: (d) => d === hit, selected: none, pinned: none });
  assert.equal(path.matched, true);
  assert.deepEqual(path.shown, [hit]);
  assert.equal(childPath([miss], { matching: none, selected: none, pinned: none }).matched, false);
});

test("a selected child run (or one of its members) opens its chat", () => {
  const a = run("a", "chat");
  const b = run("b", "chat");
  const path = childPath([a, b], { matching: () => true, selected: (d) => d === b, pinned: none });
  assert.equal(path.selected, true);
  assert.deepEqual(path.shown, [a, b]);
});

test("a pinned child shows in pins, not again under its chat", () => {
  const a = run("a", "chat");
  const b = run("b", "chat");
  const path = childPath([a, b], { matching: () => true, selected: none, pinned: (d) => d === a });
  assert.deepEqual(path.shown, [b]);
});

test("the 更多 cut keeps a chat whose child is selected, however far down it is", () => {
  const rows = ["r1", "r2", "r3", "chat-with-selected-child", "r5"];
  const keep = (r: string) => r === "chat-with-selected-child";
  assert.deepEqual(capped(rows, keep, 2, false), ["r1", "r2", "chat-with-selected-child"]);
  assert.deepEqual(capped(rows, keep, 2, true), rows);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { closeTab, dropTarget, moveTab, restoreDocks, shownTab, showTab, type Docks } from "./docks.ts";

const docks = (right: string[], bottom: string[] = [], active: { right?: string; bottom?: string } = {}): Docks => ({
  right: { tabs: right, active: active.right ?? right[0] ?? null, open: right.length > 0 },
  bottom: { tabs: bottom, active: active.bottom ?? bottom[0] ?? null, open: bottom.length > 0 },
});
const known = (t: unknown): t is string => typeof t === "string" && ["changes", "files", "task", "graph"].includes(t);

test("moving a tab to the other dock shows it there and a neighbour where it left", () => {
  const next = moveTab(docks(["a", "b", "c"], [], { right: "b" }), "b", "bottom");
  assert.deepEqual(next.right, { tabs: ["a", "c"], active: "c", open: true });
  assert.deepEqual(next.bottom, { tabs: ["b"], active: "b", open: true });
});

test("moving a dock's last tab away folds that dock", () => {
  const next = moveTab(docks(["a"], ["b"]), "a", "bottom", 0);
  assert.equal(next.right.open, false);
  assert.deepEqual(next.bottom.tabs, ["a", "b"]);
});

test("reordering inside a dock counts the index without the moved tab", () => {
  assert.deepEqual(moveTab(docks(["a", "b", "c"]), "a", "right", 2).right.tabs, ["b", "c", "a"]);
  assert.deepEqual(moveTab(docks(["a", "b", "c"]), "c", "right", 0).right.tabs, ["c", "a", "b"]);
});

test("closing the shown tab shows its neighbour; the last tab folds the dock", () => {
  assert.equal(closeTab(docks(["a", "b"], [], { right: "a" }), "a").right.active, "b");
  assert.equal(closeTab(docks(["a", "b"], [], { right: "b" }), "a").right.active, "b");
  assert.deepEqual(closeTab(docks(["a"]), "a").right, { tabs: [], active: null, open: false });
});

test("a dock whose active tab left shows its first tab", () => {
  assert.equal(shownTab({ tabs: ["a", "b"], active: "gone", open: true }), "a");
  assert.equal(shownTab({ tabs: [], active: "gone", open: true }), null);
});

test("showing a tab finds it in either dock, or adds it on the right", () => {
  const folded: Docks = { right: { tabs: ["a"], active: "a", open: false }, bottom: { tabs: ["b", "c"], active: "b", open: false } };
  const below = showTab(folded, "c");
  assert.equal(below.bottom.active, "c");
  assert.equal(below.bottom.open, true);
  assert.equal(below.right.open, false);
  assert.deepEqual(showTab(folded, "d").right, { tabs: ["a", "d"], active: "d", open: true });
  assert.equal(showTab(folded, "d", false).right.open, false);
  assert.equal(showTab(folded, "a", false).right.open, false);
});

test("a layout saved before tabs keeps every kind as a tab, its first pane showing", () => {
  const got = restoreDocks({ ratio: 0.5, tools: { open: true, panes: ["graph", "files"], split: 0.5 } }, known, ["changes", "files", "task", "graph"]);
  assert.deepEqual(got.right, { tabs: ["changes", "files", "task", "graph"], active: "graph", open: true });
  assert.deepEqual(got.bottom, { tabs: [], active: null, open: false });
});

test("a saved layout drops unknown and repeated tabs, and an empty dock stays folded", () => {
  const got = restoreDocks({ right: { tabs: ["files", "events", "files"], active: "events", open: true }, bottom: { tabs: ["files", 3], active: "files", open: true } }, known, []);
  assert.deepEqual(got.right, { tabs: ["files"], active: null, open: true });
  assert.deepEqual(got.bottom, { tabs: [], active: "files", open: false });
});

test("nothing saved opens nothing, with the fresh tabs ready", () => {
  const got = restoreDocks(null, known, ["changes", "files"]);
  assert.deepEqual(got.right, { tabs: ["changes", "files"], active: "changes", open: false });
});

const center = { left: 200, top: 0, right: 800, bottom: 600 };
const strip = { dock: "right" as const, rect: { left: 810, top: 0, right: 1200, bottom: 48 }, tabs: [{ id: "a", left: 820, right: 880 }, { id: "b", left: 880, right: 940 }, { id: "c", left: 940, right: 1000 }] };

test("over a row of tabs a tab lands between them, not counting itself", () => {
  const at = { strips: [strip], right: { left: 810, top: 0, right: 1200, bottom: 600 }, bottom: null, center };
  assert.deepEqual(dropTarget(830, 20, "c", at), { dock: "right", index: 0, zone: strip.rect, marker: 820 });
  assert.equal(dropTarget(990, 20, "a", at)?.index, 2);
  assert.equal(dropTarget(930, 20, "a", at)?.index, 1);
});

test("over a dock's body a tab lands at its end", () => {
  const right = { left: 810, top: 0, right: 1200, bottom: 600 };
  assert.deepEqual(dropTarget(900, 300, "a", { strips: [strip], right, bottom: null, center }), { dock: "right", zone: right });
});

test("the conversation's edges open a dock not showing; its middle takes nothing", () => {
  const at = { strips: [], right: null, bottom: null, center };
  assert.equal(dropTarget(500, 500, "a", at)?.dock, "bottom");
  assert.equal(dropTarget(700, 100, "a", at)?.dock, "right");
  assert.equal(dropTarget(300, 100, "a", at), null);
  assert.equal(dropTarget(100, 100, "a", at), null);
});

test("a dock already showing is not offered again over the conversation", () => {
  const right = { left: 810, top: 0, right: 1200, bottom: 600 };
  assert.equal(dropTarget(500, 500, "a", { strips: [], right, bottom: { left: 200, top: 400, right: 800, bottom: 600 }, center: { ...center, bottom: 400 } })?.dock, "bottom");
  assert.equal(dropTarget(700, 100, "a", { strips: [], right, bottom: null, center }), null);
});

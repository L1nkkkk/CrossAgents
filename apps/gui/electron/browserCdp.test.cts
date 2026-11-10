import assert from "node:assert/strict";
import { test } from "node:test";
import { axSnapshot, backendNodeFor, consoleEntry, keyDefinition, quadCenter, type AxNode } from "./browserCdp.cjs";

const v = (value: unknown) => ({ type: "string", value });

test("the accessibility tree prints as indented text with refs on interactive nodes", () => {
  const nodes: AxNode[] = [
    { nodeId: "1", role: v("RootWebArea"), name: v("Page"), childIds: ["2", "3", "4", "9"] },
    { nodeId: "2", role: v("generic"), childIds: ["5"] },
    { nodeId: "5", role: v("heading"), name: v("Welcome"), childIds: ["6"] },
    { nodeId: "6", role: v("StaticText"), name: v("Welcome"), childIds: [] },
    { nodeId: "3", role: v("link"), name: v("Docs"), backendDOMNodeId: 42, childIds: ["7"] },
    { nodeId: "7", role: v("StaticText"), name: v("Docs"), childIds: [] },
    { nodeId: "4", role: v("textbox"), name: v("Search"), value: v("rust"), backendDOMNodeId: 77, childIds: [] },
    { nodeId: "8", role: v("InlineTextBox"), name: v("ignored"), ignored: true, childIds: [] },
    { nodeId: "9", role: v("none"), childIds: ["10"] },
    { nodeId: "10", role: v("paragraph"), name: v(""), childIds: ["11"] },
    { nodeId: "11", role: v("StaticText"), name: v("Plain   text\n here"), childIds: [] },
  ];
  const snap = axSnapshot(nodes);
  // Grouping nodes print nothing and do not indent; a named node's text leaves are not repeated.
  assert.equal(
    snap.text,
    [
      '- heading "Welcome"',
      '- link "Docs" [ref=1]',
      '- textbox "Search": "rust" [ref=2]',
      '- "Plain text here"',
    ].join("\n"),
  );
  assert.equal(snap.truncated, false);
  assert.deepEqual([...snap.refs], [[1, 42], [2, 77]]);
});

test("refs are rebuilt by each snapshot and an unknown one is refused", () => {
  const first = axSnapshot([{ nodeId: "1", role: v("button"), name: v("Go"), backendDOMNodeId: 5 }]);
  assert.equal(backendNodeFor(first.refs, 1), 5);
  const second = axSnapshot([{ nodeId: "1", role: v("button"), name: v("Stop"), backendDOMNodeId: 9 }]);
  assert.equal(second.text, '- button "Stop" [ref=1]');
  assert.equal(backendNodeFor(second.refs, 1), 9);
  assert.throws(() => backendNodeFor(second.refs, 2), /没有 ref=2/);
});

test("an interactive node without a DOM node gets no ref, and cycles do not loop", () => {
  const nodes: AxNode[] = [
    { nodeId: "1", role: v("list"), name: v("L"), childIds: ["2", "1"] },
    { nodeId: "2", role: v("button"), name: v("No DOM") },
  ];
  const snap = axSnapshot(nodes);
  assert.equal(snap.text, '- list "L"\n  - button "No DOM"');
  assert.equal(snap.refs.size, 0);
});

test("an empty tree gives empty text", () => {
  assert.deepEqual(axSnapshot([]), { text: "", refs: new Map(), truncated: false });
});

test("the centre of a content quad", () => {
  assert.deepEqual(quadCenter([10, 20, 30, 20, 30, 40, 10, 40]), { x: 20, y: 30 });
  assert.throws(() => quadCenter([1, 2, 3]));
});

test("CDP key names map to key, code and virtual-key code", () => {
  assert.deepEqual(keyDefinition("Enter"), { key: "Enter", code: "Enter", keyCode: 13, text: "\r" });
  assert.deepEqual(keyDefinition("ArrowDown"), { key: "ArrowDown", code: "ArrowDown", keyCode: 40 });
  assert.deepEqual(keyDefinition("Escape"), { key: "Escape", code: "Escape", keyCode: 27 });
  assert.deepEqual(keyDefinition("Space"), { key: " ", code: "Space", keyCode: 32, text: " " });
  assert.deepEqual(keyDefinition("a"), { key: "a", code: "KeyA", keyCode: 65, text: "a" });
  assert.deepEqual(keyDefinition("7"), { key: "7", code: "Digit7", keyCode: 55, text: "7" });
  assert.equal(keyDefinition("Foo"), undefined);
  assert.equal(keyDefinition("enter"), undefined);
  assert.equal(keyDefinition("constructor"), undefined);
});

test("console and exception events become entries; other events are ignored", () => {
  assert.deepEqual(
    consoleEntry("Runtime.consoleAPICalled", {
      type: "warning",
      timestamp: 1500,
      args: [{ type: "string", value: "hi" }, { type: "number", value: 3 }, { type: "object", description: "Object" }],
    }),
    { level: "warning", text: "hi 3 Object", ts: 1500 },
  );
  assert.deepEqual(
    consoleEntry("Runtime.exceptionThrown", {
      timestamp: 2,
      exceptionDetails: { text: "Uncaught", exception: { description: "TypeError: x is not a function" } },
    }),
    { level: "error", text: "TypeError: x is not a function", ts: 2 },
  );
  assert.equal(consoleEntry("Page.loadEventFired", {}), undefined);
});

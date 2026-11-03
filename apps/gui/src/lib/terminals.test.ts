import assert from "node:assert/strict";
import { test } from "node:test";
import { fromBase64, takeBatch, termLabels } from "./terminals.ts";

test("what was typed meanwhile goes together, in order", () => {
  const queue = ["a", "b", "c"];
  assert.equal(takeBatch(queue), "abc");
  assert.deepEqual(queue, []);
});

test("a batch stops before a piece that would not fit", () => {
  const queue = ["aaa", "bbbb", "c"];
  assert.equal(takeBatch(queue, 5), "aaa");
  assert.equal(takeBatch(queue, 5), "bbbbc");
  assert.deepEqual(queue, []);
});

test("a paste over the limit goes in parts", () => {
  const queue = ["abcdefgh"];
  assert.equal(takeBatch(queue, 3), "abc");
  assert.equal(takeBatch(queue, 3), "def");
  assert.equal(takeBatch(queue, 3), "gh");
});

test("a part never ends inside a surrogate pair", () => {
  const queue = ["ab😀cd"];
  assert.equal(takeBatch(queue, 3), "ab");
  assert.equal(takeBatch(queue, 3), "😀c");
  assert.equal(takeBatch(queue, 3), "d");
});

test("output keeps bytes that are not text on their own", () => {
  assert.deepEqual([...fromBase64(Buffer.from([0xe4, 0xb8]).toString("base64"))], [0xe4, 0xb8]);
});

test("terminals are numbered within their project", () => {
  const labels = termLabels([
    { id: "a", project: "p" },
    { id: "b", project: "q" },
    { id: "c", project: "p" },
  ]);
  assert.deepEqual([labels.get("a"), labels.get("b"), labels.get("c")], ["终端", "终端", "终端 2"]);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { noticeLines } from "./notices.ts";

test("a message without notices shows none", () => {
  assert.deepEqual(noticeLines(undefined), []);
  assert.deepEqual(noticeLines(null), []);
  assert.deepEqual(noticeLines([]), []);
});

test("each notice is one line, trimmed, in the order it was sent", () => {
  assert.deepEqual(noticeLines(["bob finished a turn", "  a task landed  "]), ["bob finished a turn", "a task landed"]);
});

test("blank notices are dropped, so the count under the bubble is the real one", () => {
  const lines = noticeLines(["one", "", "   ", "two"]);
  assert.deepEqual(lines, ["one", "two"]);
  // The summary under the bubble is rendered from this count (views/SessionMessage.tsx).
  assert.equal(lines.length, 2);
});

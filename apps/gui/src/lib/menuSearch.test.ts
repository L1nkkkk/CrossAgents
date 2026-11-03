import assert from "node:assert/strict";
import { test } from "node:test";
import { filterMenu } from "./menuSearch.ts";

const items = [
  { kind: "header" as const, key: "h-claude", label: "Claude · 工作账号" },
  { kind: "item" as const, key: "claude/opus", label: "Opus 5.5", sub: "opus", disabled: true },
  { kind: "item" as const, key: "claude/sonnet", label: "Sonnet 5.5", sub: "sonnet" },
  { kind: "header" as const, key: "h-cursor", label: "Cursor" },
  { kind: "item" as const, key: "cursor/claude-opus-5", label: "Opus 5", sub: "claude-opus-5" },
];

test("search matches names, IDs and section context without empty headers", () => {
  assert.deepEqual(filterMenu(items, " OPUS 5.5 "), [items[0], items[1]]);
  assert.deepEqual(filterMenu(items, "cursor opus"), [items[3], items[4]]);
  assert.deepEqual(filterMenu(items, "工作账号 sonnet"), [items[0], items[2]]);
  assert.deepEqual(filterMenu(items, "claude-opus-5"), [items[3], items[4]]);
  assert.deepEqual(filterMenu(items, "absent"), []);
  assert.deepEqual(filterMenu(items, "  "), items);
  assert.equal(filterMenu(items, "opus")[1].disabled, true);
});

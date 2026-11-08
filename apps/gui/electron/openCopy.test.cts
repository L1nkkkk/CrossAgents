import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { copyRoot, copyPrefix, copyName, staleCopies } from "./openCopy.cjs";
test("constructs temporary paths and sanitizes filenames", () => {
  assert.equal(copyPrefix(copyRoot("temp")), join("temp", "crossagents-open", "copy-"));
  assert.equal(copyName("dir/中文 file.txt"), "中文 file.txt");
  assert.equal(copyName("../CON.txt"), "_CON.txt");
  assert.equal(copyName("a/b:c?.txt"), "b_c_.txt");
  assert.equal(copyName(".."), "file");
});
test("selects only old generated directories", () => {
  const now = 2 * 86400000;
  assert.deepEqual(staleCopies([{ name: "copy-ABC123", modified: 0 }, { name: "copy-DEF456", modified: now }, { name: "other", modified: 0 }, { name: "copy-../../x", modified: 0 }], now), ["copy-ABC123"]);
});

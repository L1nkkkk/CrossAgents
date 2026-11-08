import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { copyRoot, copyPrefix, copyName, staleCopies, runsWhenOpened, safeCopyRoot, zoneIdentifier, quarantineArgs } from "./openCopy.cjs";
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
test("refuses executable names after trailing dots and spaces are removed", () => {
  for (const name of ["evil.exe.", "evil.exe ", "a.bat. . ", "EVIL.EXE", "x.terminal", "x.fileloc"]) assert.equal(runsWhenOpened(name), true, name);
  for (const name of ["a.txt", "a.exe.txt"]) assert.equal(runsWhenOpened(name), false, name);
});
test("generates Windows and macOS download origin markers", () => {
  assert.equal(zoneIdentifier(), "[ZoneTransfer]\r\nZoneId=3\r\n");
  assert.deepEqual(quarantineArgs("/tmp/a b", 1700000000), ["-w", "com.apple.quarantine", "0081;6553f100;CrossAgents;", "/tmp/a b"]);
});
test("accepts only a real directory owned by the current user", () => {
  assert.equal(safeCopyRoot({ directory: true, symlink: false, uid: 42 }, 42), true);
  assert.equal(safeCopyRoot({ directory: false, symlink: false, uid: 42 }, 42), false);
  assert.equal(safeCopyRoot({ directory: true, symlink: true, uid: 42 }, 42), false);
  assert.equal(safeCopyRoot({ directory: true, symlink: false, uid: 43 }, 42), false);
});

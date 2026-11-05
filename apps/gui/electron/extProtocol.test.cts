import assert from "node:assert/strict";
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { extensionFile, extensionRealFile } from "./extProtocol.cjs";

const versions = [{ name: "example", current: { fingerprint: "abc123" }, previous_fingerprint: "def456" }];
const url = (path: string, fingerprint = "abc123") => `crossagents-ext://example/${fingerprint}/${path}`;

test("maps current and previous snapshots, never pending or arbitrary versions", () => {
  for (const fingerprint of ["abc123", "def456"]) {
    assert.equal(extensionFile("home", url("gui/a%20b.js?v=1", fingerprint), versions).file,
      resolve("home", "extension-versions", "example", fingerprint, "gui", "a b.js"));
  }
  assert.throws(() => extensionFile("home", url("gui/index.js", "pending"), versions));
  assert.throws(() => extensionFile("home", url("gui/index.js"), []));
});

test("refuses traversal before URL normalization, encoded traversal and absolute paths", () => {
  for (const path of ["../secret", "gui/../../secret", "%2e%2e/secret", "gui/%2E%2E/secret",
    "%2e%2e%2fsecret", "%2e%2e%5csecret", "/etc/passwd", "%2fetc/passwd", "C:/secret",
    "C:secret", "C%3a%5csecret", "\\\\server\\share", "gui\\..\\secret", "gui/a.js:stream", "%00", "%zz"]) {
    assert.throws(() => extensionFile("home", url(path), versions), path);
  }
  for (const value of ["file:///secret", "crossagents-ext://../abc123/file", "crossagents-ext://example/%2e%2e/file",
    "crossagents-ext://example:90/abc123/file", "crossagents-ext://user@example/abc123/file"]) {
    assert.throws(() => extensionFile("home", value, versions), value);
  }
});

test("real path check refuses a junction escaping the snapshot", async () => {
  const home = await mkdtemp(join(tmpdir(), "ca-ext-protocol-"));
  try {
    const root = join(home, "extension-versions", "example", "abc123");
    const outside = join(home, "outside");
    await mkdir(root, { recursive: true });
    await mkdir(outside);
    await writeFile(join(outside, "secret.js"), "secret");
    await writeFile(join(root, "index.js"), "export default 1");
    await symlink(outside, join(root, "escape"), process.platform === "win32" ? "junction" : "dir");
    assert.equal(await extensionRealFile(home, url("index.js"), versions), await realpath(join(root, "index.js")));
    await assert.rejects(extensionRealFile(home, url("escape/secret.js"), versions), /escapes snapshot/);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

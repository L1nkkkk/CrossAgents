import assert from "node:assert/strict";
import { test } from "node:test";
import { diffStat, parseDiff, sumStats, tailLines, turnStat } from "./diff.ts";

test("splits git diff output into files with counts", () => {
  const text = [
    "diff --git a/a.txt b/a.txt",
    "index 45b983b..b6fc4c6 100644",
    "--- a/a.txt",
    "+++ b/a.txt",
    "@@ -1 +1 @@",
    "-hi",
    "+hello",
    "\\ No newline at end of file",
    "diff --git a/new.py b/new.py",
    "new file mode 100644",
    "--- /dev/null",
    "+++ b/new.py",
    "@@ -0,0 +1,2 @@",
    "+print(1)",
    "+print(2)",
    "",
  ].join("\n");
  const files = parseDiff(text);
  assert.deepEqual(
    files.map((f) => [f.path, f.added, f.removed]),
    [
      ["a.txt", 1, 1],
      ["new.py", 2, 0],
    ],
  );
  assert.deepEqual(files[0].lines.map((l) => l.kind), ["meta", "hunk", "del", "add", "meta"]);
  assert.deepEqual(diffStat(files), { files: 2, added: 3, removed: 1 });
});

test("a bare hunk list takes the fallback path", () => {
  const files = parseDiff("@@ -1 +1 @@\n-x\n+y\n", "src/x.rs");
  assert.equal(files.length, 1);
  assert.equal(files[0].path, "src/x.rs");
  assert.deepEqual(files[0].lines.map((l) => l.text), ["@@ -1 +1 @@", "x", "y"]);
});

test("tailLines keeps the end", () => {
  assert.deepEqual(tailLines("a\nb\nc\n", 2), { text: "b\nc", hidden: 1 });
  assert.deepEqual(tailLines("a\n", 5), { text: "a", hidden: 0 });
});

test("a removed line that looks like a header is still a line", () => {
  const files = parseDiff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1 @@\n--- old rule\n+++ new rule\n");
  assert.deepEqual(
    files.map((f) => [f.path, f.added, f.removed]),
    [["x", 1, 1]],
  );
});

test("turnStat and sumStats", () => {
  const stat = turnStat("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1,2 @@\n-x\n+y\n+z\n", "hub");
  assert.deepEqual(stat, { files: [{ path: "a", added: 2, removed: 1 }], added: 2, removed: 1, source: "hub" });
  assert.equal(turnStat(""), undefined);
  const b = turnStat("diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-1\n+2\n")!;
  assert.deepEqual(sumStats([stat!, b, stat!]), { files: ["a", "b"], added: 5, removed: 3 });
});

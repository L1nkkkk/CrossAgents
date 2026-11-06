import assert from "node:assert/strict";
import { test } from "node:test";
import type { FileGitChange } from "@bindings/FileGitChange";
import { changeIndex, changeMark, currentHeading, dirOf, filterPaths, outlineOf } from "./files.ts";

test("headings come from the document, without Markdown markers or code fences", () => {
  const md = [
    "# 标题",
    "",
    "正文",
    "",
    "```md",
    "# 代码里的井号不是标题",
    "```",
    "",
    "## **粗体** 与 [链接](https://example.com)",
    "",
    "> ### 引用里的标题",
    "",
    "#### `code`",
  ].join("\n");
  const got = outlineOf(md).map((h) => [h.level, h.text]);
  assert.deepEqual(got, [
    [1, "标题"],
    [2, "粗体 与 链接"],
    [3, "引用里的标题"],
    [4, "code"],
  ]);
});

test("indexes number the headings in reading order", () => {
  const headings = outlineOf("# a\n\n## b\n\n### c\n");
  assert.deepEqual(
    headings.map((h) => h.index),
    [0, 1, 2],
  );
});

test("the current heading is the last one at or above the view", () => {
  const tops = [0, 120, 340];
  assert.equal(currentHeading(tops, 0), 0);
  assert.equal(currentHeading(tops, 119), 0);
  assert.equal(currentHeading(tops, 120), 1);
  assert.equal(currentHeading(tops, 500), 2);
  assert.equal(currentHeading(tops, -1), -1);
  assert.equal(currentHeading([], 10), -1);
});

test("filtering is case-insensitive, whole-path and bounded", () => {
  const paths = ["docs/DONE.md", "docs/cli.md", "crates/cli/src/main.rs", "README.md"];
  assert.deepEqual(filterPaths(paths, "CLI"), ["docs/cli.md", "crates/cli/src/main.rs"]);
  assert.deepEqual(filterPaths(paths, "src/"), ["crates/cli/src/main.rs"]);
  assert.deepEqual(filterPaths(paths, "zzz"), []);
  assert.deepEqual(filterPaths(paths, "  "), []);
  assert.deepEqual(filterPaths(paths, "cli", 1), ["docs/cli.md"]);
});

test("changes roll up to every folder above the file", () => {
  const entries: { path: string; change: FileGitChange }[] = [
    { path: "docs/DONE.md", change: "modified" },
    { path: "docs/notes/new.md", change: "added" },
    { path: "README.md", change: "deleted" },
  ];
  const marks = changeIndex(entries);
  assert.equal(marks.files.get("docs/DONE.md"), "modified");
  assert.deepEqual([...marks.dirs].sort(), ["docs", "docs/notes"]);
  assert.equal(changeMark("modified"), "M");
  assert.equal(changeMark("added"), "A");
  assert.equal(changeMark("deleted"), "D");
  assert.equal(dirOf("docs/DONE.md"), "docs");
  assert.equal(dirOf("README.md"), "");
});

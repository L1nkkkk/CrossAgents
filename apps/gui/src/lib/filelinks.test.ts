import assert from "node:assert/strict";
import { test } from "node:test";
import { codeFileTarget, docLinkPath, fileLinkTarget, isAbsolutePath, splitLine } from "./filelinks.ts";

test("a document's links are relative to its folder", () => {
  assert.equal(docLinkPath("docs", "DESIGN.md"), "docs/DESIGN.md");
  assert.equal(docLinkPath("docs", "./tasks/U1.md"), "docs/tasks/U1.md");
  assert.equal(docLinkPath("docs/tasks", "../DESIGN.md"), "docs/DESIGN.md");
  assert.equal(docLinkPath("", "README.md"), "README.md");
  assert.equal(docLinkPath("docs", "DESIGN.md#goals"), "docs/DESIGN.md");
  assert.equal(docLinkPath("docs", "..\\README.md"), "README.md");
  assert.equal(docLinkPath("docs", "C:/work/x.md"), "C:/work/x.md");
  assert.equal(docLinkPath("docs", "../../secret.md"), "../secret.md");
});

test("line suffixes split off: :line, :line:col, #Lline", () => {
  assert.deepEqual(splitLine("src/main.rs:12"), { path: "src/main.rs", line: 12 });
  assert.deepEqual(splitLine("src/main.rs:12:5"), { path: "src/main.rs", line: 12, col: 5 });
  assert.deepEqual(splitLine("docs/a.md#L40"), { path: "docs/a.md", line: 40 });
  assert.deepEqual(splitLine("docs/a.md#L40-L52"), { path: "docs/a.md", line: 40 });
  assert.deepEqual(splitLine("C:/work/x.ts:3"), { path: "C:/work/x.ts", line: 3 });
  assert.deepEqual(splitLine("Cargo.toml"), { path: "Cargo.toml" });
});

test("link targets: relative and absolute paths, Windows and POSIX", () => {
  assert.deepEqual(fileLinkTarget("crates/cli/src/main.rs:12"), { path: "crates/cli/src/main.rs", line: 12 });
  assert.deepEqual(fileLinkTarget("./README.md"), { path: "./README.md" });
  assert.deepEqual(fileLinkTarget("../other/lib.rs#L7"), { path: "../other/lib.rs", line: 7 });
  assert.deepEqual(fileLinkTarget("C:\\work\\notes\\todo.md"), { path: "C:\\work\\notes\\todo.md" });
  assert.deepEqual(fileLinkTarget("C:/work/notes/todo.md:3:1"), { path: "C:/work/notes/todo.md", line: 3, col: 1 });
  assert.deepEqual(fileLinkTarget("/home/me/repo/src/lib.rs"), { path: "/home/me/repo/src/lib.rs" });
  assert.deepEqual(fileLinkTarget("docs/%E8%AF%B4%E6%98%8E.md"), { path: "docs/说明.md" });
  assert.deepEqual(fileLinkTarget("Makefile"), { path: "Makefile" });
});

test("link targets: web links, other schemes and empty targets are not files", () => {
  assert.equal(fileLinkTarget("https://example.com/a.rs"), undefined);
  assert.equal(fileLinkTarget("mailto:a@b.c"), undefined);
  assert.equal(fileLinkTarget("javascript:alert(1)"), undefined);
  assert.equal(fileLinkTarget("JavaScript:1"), undefined);
  assert.equal(fileLinkTarget("file:///etc/passwd"), undefined);
  assert.equal(fileLinkTarget("vscode://file/x"), undefined);
  assert.equal(fileLinkTarget("data:text/html,x"), undefined);
  assert.equal(fileLinkTarget(""), undefined);
  assert.equal(fileLinkTarget("   "), undefined);
  assert.equal(fileLinkTarget("#section"), undefined);
  assert.equal(fileLinkTarget("//evil.com/x"), undefined);
  assert.equal(fileLinkTarget("\\\\server\\share\\x"), undefined);
  assert.equal(fileLinkTarget("."), undefined);
});

test("absolute paths", () => {
  assert.ok(isAbsolutePath("C:\\x"));
  assert.ok(isAbsolutePath("d:/x"));
  assert.ok(isAbsolutePath("/usr/x"));
  assert.ok(!isAbsolutePath("src/x"));
  assert.ok(!isAbsolutePath("//host/x"));
});

test("inline code that reads like a path", () => {
  assert.deepEqual(codeFileTarget("src/lib.rs"), { path: "src/lib.rs" });
  assert.deepEqual(codeFileTarget("Cargo.toml"), { path: "Cargo.toml" });
  assert.deepEqual(codeFileTarget("apps/gui/src/api.ts:120"), { path: "apps/gui/src/api.ts", line: 120 });
  assert.deepEqual(codeFileTarget("docs/"), { path: "docs/" });
  assert.deepEqual(codeFileTarget("C:\\work\\shop\\main.py"), { path: "C:\\work\\shop\\main.py" });
  assert.deepEqual(codeFileTarget("index.d.ts"), { path: "index.d.ts" });
});

test("inline code that is not a path", () => {
  for (const code of [
    "npm run build",
    "claude-opus-5-5",
    "agent_list",
    "foo()",
    "1.2.3",
    "3.14",
    "https://example.com/a.rs",
    "javascript:x.js",
    "--force",
    "~/.bashrc",
    "",
    "a b/c.rs",
    "<div>.x",
    "Vec<String>",
    "/",
  ]) {
    assert.equal(codeFileTarget(code), undefined, code);
  }
});

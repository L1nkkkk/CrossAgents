import assert from "node:assert/strict";
import { test } from "node:test";
import { highlight, highlightLine, langOf } from "./highlight.ts";

const kinds = (line: string, lang: Parameters<typeof highlightLine>[1]) =>
  highlightLine(line, lang).tokens.filter((t) => t.k !== "plain" && t.k !== "punct").map((t) => `${t.k}:${t.s}`);

test("languages by extension", () => {
  assert.equal(langOf("crates/cli/src/main.rs"), "rs");
  assert.equal(langOf("a/b.tsx"), "ts");
  assert.equal(langOf("x.MJS"), "js");
  assert.equal(langOf("package.json"), "json");
  assert.equal(langOf("docs/cli.md"), "md");
  assert.equal(langOf("Cargo.toml"), "toml");
  assert.equal(langOf("scripts/build.py"), "py");
  assert.equal(langOf("LICENSE"), "text");
});

test("rust: keywords, strings, comments, macros", () => {
  assert.deepEqual(kinds('pub fn main() { println!("hi"); } // done', "rs"), [
    "kw:pub",
    "kw:fn",
    "fn:main",
    "fn:println",
    'str:"hi"',
    "com:// done",
  ]);
  assert.deepEqual(kinds("let c = 'x'; x: &'a Foo", "rs"), ["kw:let", "str:'x'", "type:'a", "type:Foo"]);
});

test("block comments and triple-quoted strings span lines", () => {
  const lines = highlight("/* one\ntwo */ let x = 1;", "ts");
  assert.deepEqual(lines[0], [{ k: "com", s: "/* one" }]);
  assert.equal(lines[1][0].k, "com");
  assert.ok(lines[1].some((t) => t.k === "kw" && t.s === "let"));
  assert.ok(lines[1].some((t) => t.k === "num" && t.s === "1"));
  const py = highlight('x = """a\nb"""\ndef f(): pass', "py");
  assert.equal(py[1][0].k, "str");
  assert.ok(py[2].some((t) => t.k === "kw" && t.s === "def"));
});

test("json keys, toml tables, markdown headings and code", () => {
  assert.deepEqual(kinds('  "name": "gui", "n": 3, "ok": true', "json"), ['key:"name"', 'str:"gui"', 'key:"n"', "num:3", 'key:"ok"', "lit:true"]);
  assert.deepEqual(kinds("[workspace]", "toml"), ["head:[workspace]"]);
  assert.deepEqual(kinds('name = "x" # c', "toml"), ["key:name", 'str:"x"', "com:# c"]);
  assert.deepEqual(kinds("## 用法", "md"), ["head:## 用法"]);
  assert.deepEqual(kinds("run `cargo test` now", "md"), ["code:`cargo test`"]);
  const fenced = highlight("```\n# not a heading\n```\n# heading", "md");
  assert.equal(fenced[1][0].k, "code");
  assert.equal(fenced[3][0].k, "head");
});

test("every character survives", () => {
  const src = 'const s = `a${b}`; // x\nfn  y() {}\n"k": [1, 2]';
  for (const lang of ["ts", "rs", "json", "py", "toml", "md", "text"] as const) {
    const joined = highlight(src, lang).map((l) => l.map((t) => t.s).join("")).join("\n");
    assert.equal(joined, src, lang);
  }
});

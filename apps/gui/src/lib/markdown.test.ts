import assert from "node:assert/strict";
import { test } from "node:test";
import { parseBlocks, parseInline, softBreaks } from "./markdown.ts";

test("a document's hard-wrapped lines join into one paragraph", () => {
  assert.equal(softBreaks("one line"), "one line");
  assert.equal(softBreaks("wrapped\n  here"), "wrapped here");
  assert.equal(softBreaks("中文换行\n接着写"), "中文换行接着写");
  assert.equal(softBreaks("中文 end\nnext"), "中文 end next");
  assert.equal(softBreaks("hard  \nbreak"), "hard\nbreak");
  assert.equal(softBreaks("hard\\\nbreak"), "hard\nbreak");
});

test("a bullet list with bold and code, like the model list in a reply", () => {
  const blocks = parseBlocks("我能确认的是：\n\n- **Fable 5.1**（`claude-fable-5-1`）\n- **Opus 5.5**（`claude-opus-5-5`）");
  assert.equal(blocks.length, 2);
  assert.deepEqual(blocks[0], { t: "p", text: "我能确认的是：" });
  const list = blocks[1];
  assert.equal(list.t, "list");
  if (list.t !== "list") return;
  assert.equal(list.ordered, false);
  assert.equal(list.items.length, 2);
  assert.deepEqual(parseInline(list.items[0].text), [
    { t: "strong", children: [{ t: "text", text: "Fable 5.1" }] },
    { t: "text", text: "（" },
    { t: "code", text: "claude-fable-5-1" },
    { t: "text", text: "）" },
  ]);
});

test("a list right after a paragraph line, without a blank line", () => {
  const blocks = parseBlocks("步骤：\n1. 拉代码\n2. 跑测试");
  assert.deepEqual(blocks.map((b) => b.t), ["p", "list"]);
  const list = blocks[1];
  if (list.t === "list") {
    assert.equal(list.ordered, true);
    assert.deepEqual(list.items.map((x) => x.text), ["拉代码", "跑测试"]);
  }
});

test("nested lists and numbered starts", () => {
  const [list] = parseBlocks("3. 外层\n   - 内层 a\n   - 内层 b\n4. 下一个");
  assert.equal(list.t, "list");
  if (list.t !== "list") return;
  assert.equal(list.start, 3);
  assert.equal(list.items.length, 2);
  const inner = list.items[0].children[0];
  assert.equal(inner.t, "list");
  if (inner.t === "list") assert.deepEqual(inner.items.map((x) => x.text), ["内层 a", "内层 b"]);
});

test("headings, fences, quotes, rules and tables", () => {
  const blocks = parseBlocks("## 结论\n\n```rust\nfn main() {}\n```\n\n> 引用\n\n---\n\n| 模型 | 价格 |\n|:--|--:|\n| Haiku | 低 |");
  assert.deepEqual(blocks.map((b) => b.t), ["h", "code", "quote", "hr", "table"]);
  assert.deepEqual(blocks[1], { t: "code", lang: "rust", text: "fn main() {}" });
  const table = blocks[4];
  if (table.t === "table") {
    assert.deepEqual(table.head, ["模型", "价格"]);
    assert.deepEqual(table.align, ["left", "right"]);
    assert.deepEqual(table.rows, [["Haiku", "低"]]);
  }
});

test("inline: italic, strikethrough, safe links, snake_case stays text", () => {
  assert.deepEqual(parseInline("*斜体* ~~删除~~"), [
    { t: "em", children: [{ t: "text", text: "斜体" }] },
    { t: "text", text: " " },
    { t: "del", children: [{ t: "text", text: "删除" }] },
  ]);
  assert.deepEqual(parseInline("[文档](https://example.com)"), [{ t: "link", href: "https://example.com", children: [{ t: "text", text: "文档" }] }]);
  assert.deepEqual(parseInline("[x](javascript:alert(1))"), [{ t: "text", text: "[x](javascript:alert(1))" }]);
  assert.deepEqual(parseInline("call agent_list now"), [{ t: "text", text: "call agent_list now" }]);
  assert.deepEqual(parseInline("2 * 3 * 4"), [{ t: "text", text: "2 * 3 * 4" }]);
});

test("inline: links to local files become file nodes, with their line", () => {
  assert.deepEqual(parseInline("见 [main.rs](crates/cli/src/main.rs:12)"), [
    { t: "text", text: "见 " },
    { t: "file", path: "crates/cli/src/main.rs", line: 12, children: [{ t: "text", text: "main.rs" }] },
  ]);
  assert.deepEqual(parseInline("[说明](docs/cli.md#L3)"), [{ t: "file", path: "docs/cli.md", line: 3, children: [{ t: "text", text: "说明" }] }]);
  assert.deepEqual(parseInline("[todo](C:/work/notes/todo.md)"), [{ t: "file", path: "C:/work/notes/todo.md", children: [{ t: "text", text: "todo" }] }]);
  assert.deepEqual(parseInline("[x](file:///etc/passwd)"), [{ t: "text", text: "[x](file:///etc/passwd)" }]);
  assert.deepEqual(parseInline("[x](vbscript:msgbox)"), [{ t: "text", text: "[x](vbscript:msgbox)" }]);
});

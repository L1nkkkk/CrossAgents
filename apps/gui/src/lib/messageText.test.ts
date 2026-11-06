import assert from "node:assert/strict";
import { test } from "node:test";
import { parseBlocks, parseInline, type Block, type Inline } from "./markdown.ts";
import { FOLD_INPUT, foldText, isKickoff, KICKOFF_SEPARATOR } from "./messageText.ts";

test("a kickoff is told from a message that only mentions a run", () => {
  assert.equal(isKickoff(`修复下${KICKOFF_SEPARATOR}01a1\nteam: …`), true);
  assert.equal(isKickoff("run: 01a1"), false);
});

/** Every word the blocks render, in order, so a check can say nothing was dropped. */
function plain(blocks: Block[]): string {
  const inline = (spans: Inline[]): string =>
    spans.map((s) => (s.t === "text" || s.t === "code" ? s.text : "children" in s ? inline(s.children) : "\n")).join("");
  return blocks
    .map((b) => {
      switch (b.t) {
        case "p":
        case "h":
          return inline(parseInline(b.text));
        case "code":
          return b.text;
        case "list":
          return b.items.map((i) => inline(parseInline(i.text)) + plain(i.children)).join("");
        case "quote":
          return plain(b.blocks);
        case "table":
          return [...b.head, ...b.rows.flat()].join(" ");
        case "hr":
          return "";
      }
    })
    .join("\n");
}

const LONG = "这是一条很长的消息。".repeat(80);

test("a message within the fold is left exactly as written", () => {
  assert.equal(foldText("短消息"), "短消息");
  const exact = "x".repeat(FOLD_INPUT);
  assert.equal(foldText(exact), exact);
});

test("a longer message is cut at the fold and marked with an ellipsis", () => {
  const folded = foldText(LONG);
  assert.equal(folded, LONG.slice(0, FOLD_INPUT) + "…");
  assert.equal(folded.length, FOLD_INPUT + 1);
  assert.ok(folded.endsWith("…"));
  // Nothing past the fold leaks in, and nothing in front of it is lost.
  assert.ok(folded.slice(0, -1).length <= FOLD_INPUT);
  assert.ok(LONG.startsWith(folded.slice(0, -1)));
});

test("whitespace at the cut is trimmed before the ellipsis", () => {
  assert.equal(foldText("abc" + " ".repeat(20) + "tail", 10), "abc…");
  assert.equal(foldText("abc\n\n" + " ".repeat(20) + "tail", 10), "abc…");
});

test("an emoji split by the fold keeps both halves", () => {
  const text = "😀".repeat(400);
  const folded = foldText(text, 5); // between the two units of the third emoji
  assert.equal(folded, "😀😀…");
  assert.ok(!/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/.test(folded), "no lone high surrogate");
  assert.ok(!/(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/.test(folded), "no lone low surrogate");
});

test("a fold inside a code block still parses, and keeps the code in front of it", () => {
  const text = "先看这段：\n\n```ts\n" + "const answer = 42;\n".repeat(80) + "```\n";
  const folded = foldText(text, 300);
  assert.ok(folded.includes("```ts"), "the cut lands inside the fence");
  const blocks = parseBlocks(folded);
  assert.deepEqual(blocks.map((b) => b.t), ["p", "code"]);
  const code = blocks[1];
  assert.equal(code.t, "code");
  if (code.t === "code") {
    assert.ok(code.text.startsWith("const answer = 42;"));
    assert.ok(code.text.endsWith("…"), "the fold marker is part of the visible tail");
  }
  const shown = plain(blocks);
  assert.ok(shown.includes("先看这段："));
  assert.ok(shown.includes("…"));
});

test("a fold that leaves the fence closed keeps the blocks after it", () => {
  const head = "```\ncode\n```\n\n";
  const text = head + "后面还有正文。".repeat(120);
  const folded = foldText(text, 200);
  const blocks = parseBlocks(folded);
  assert.deepEqual(blocks.map((b) => b.t), ["code", "p"]);
  assert.ok(plain(blocks).includes("后面还有正文。"));
});

test("a fold inside a link label stays text instead of a broken link", () => {
  const label = "a".repeat(200);
  const text = `看看 [${label}](https://example.com/x) 后面的内容`;
  const folded = foldText(text, 60);
  const blocks = parseBlocks(folded);
  assert.equal(blocks.length, 1);
  assert.equal(blocks[0].t, "p");
  assert.deepEqual(parseBlocks(folded), blocks, "parsing is stable");
  for (const span of blocks[0].t === "p" ? parseInline(blocks[0].text) : []) {
    assert.notEqual(span.t, "link", "a half-written link is not rendered as a link");
  }
  const shown = plain(blocks);
  assert.ok(shown.startsWith("看看 ["), "the text before the cut is kept");
  assert.ok(shown.endsWith("…"));
});

test("the text after the fold is still there when the message is opened", () => {
  const text = `${LONG}\n\n## 结尾\n\n最后一段在这里。`;
  const folded = foldText(text);
  assert.ok(!folded.includes("最后一段在这里。"));
  const whole = parseBlocks(text);
  assert.ok(plain(whole).includes("最后一段在这里。"));
  assert.ok(plain(whole).includes("结尾"));
});

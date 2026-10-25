import assert from "node:assert/strict";
import { test } from "node:test";
import type { AttachmentSupport } from "@bindings/AttachmentSupport";
import { MAX_FILE_BYTES, MAX_FILES, acceptFor, bytes, kindOf, mimeOf, refusal, supportNote } from "./attachments.ts";

const ALL: AttachmentSupport = { image: true, pdf: true, text: true };
const CODEX: AttachmentSupport = { image: true, pdf: false, text: true };
const f = (name: string, mime: string, size = 10) => ({ name, mime, size });

test("types come from the browser, else the extension", () => {
  assert.equal(mimeOf("shot.png", "image/png"), "image/png");
  assert.equal(mimeOf("main.rs", ""), "text/plain");
  assert.equal(mimeOf("notes.md", "application/octet-stream"), "text/markdown");
  assert.equal(mimeOf("app.exe", "application/x-msdownload"), "application/x-msdownload");
});

test("kind matches the hub's three kinds", () => {
  assert.equal(kindOf("image/png"), "image");
  assert.equal(kindOf("image/webp"), "image");
  assert.equal(kindOf("application/pdf"), "pdf");
  assert.equal(kindOf("application/json"), "text");
  assert.equal(kindOf("text/markdown"), "text");
  assert.equal(kindOf("application/x-msdownload"), undefined);
});

test("limits match the hub's and say why", () => {
  assert.equal(refusal([], [f("a.png", "image/png")]), undefined);
  assert.match(refusal([], [f("a.exe", "application/x-msdownload")])!, /发不了/);
  assert.match(refusal([], [f("big.txt", "text/plain", MAX_FILE_BYTES + 1)])!, /单个文件最多/);
  const held = Array.from({ length: MAX_FILES }, () => ({ size: 1 }));
  assert.match(refusal(held, [f("one.txt", "text/plain")])!, /最多带/);
  assert.match(refusal([], [f("a.pdf", "application/pdf", MAX_FILE_BYTES), f("b.pdf", "application/pdf", MAX_FILE_BYTES), f("c.txt", "text/plain", 5)])!, /合计/);
  assert.equal(bytes(1536), "2 KB");
});

test("the agent's kinds are checked after the hub's limits", () => {
  const why = refusal([], [f("a.pdf", "application/pdf")], CODEX, "codex-native");
  assert.ok(why);
  assert.match(why, /a\.pdf/);
  assert.match(why, /PDF/);
  assert.match(why, /codex-native/);
  assert.match(why, /图片和文本/);
  assert.equal(refusal([], [f("a.pdf", "application/pdf")], ALL, "claude-native"), undefined);
  assert.equal(refusal([], [f("a.pdf", "application/pdf")], null, "acp-generic"), undefined);
  assert.equal(refusal([], [f("a.pdf", "application/pdf")]), undefined);
  assert.equal(refusal([], [f("a.png", "image/png")], CODEX, "codex-native"), undefined);
  assert.equal(refusal([], [f("a.txt", "text/plain")], CODEX, "codex-native"), undefined);
});

test("the picker offers only the kinds the agent takes", () => {
  const codex = acceptFor(CODEX);
  assert.ok(codex.includes("image/png"));
  assert.ok(!codex.includes("application/pdf"));
  assert.ok(codex.includes("text/*"));
  assert.ok(codex.includes(".md"));
  assert.ok(acceptFor(null).includes("application/pdf"));
  assert.ok(acceptFor(undefined).includes("application/pdf"));
  assert.ok(acceptFor(ALL).includes("application/pdf"));
  assert.equal(acceptFor(ALL), acceptFor(null));
});

test("a partial agent says what it misses", () => {
  assert.equal(supportNote(CODEX, "codex-native"), "这个 agent 只收图片和文本，PDF 发不了（codex-native）");
  assert.equal(supportNote(null, "acp-generic"), undefined);
  assert.equal(supportNote(undefined), undefined);
  assert.equal(supportNote(ALL, "claude-native"), undefined);
  assert.match(supportNote({ image: false, pdf: false, text: false }, "mock")!, /什么文件都不收/);
});

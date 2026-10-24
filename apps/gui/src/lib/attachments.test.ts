import assert from "node:assert/strict";
import { test } from "node:test";
import { MAX_FILE_BYTES, MAX_FILES, bytes, mimeOf, refusal } from "./attachments.ts";

test("types come from the browser, else the extension", () => {
  assert.equal(mimeOf("shot.png", "image/png"), "image/png");
  assert.equal(mimeOf("main.rs", ""), "text/plain");
  assert.equal(mimeOf("notes.md", "application/octet-stream"), "text/markdown");
  assert.equal(mimeOf("app.exe", "application/x-msdownload"), "application/x-msdownload");
});

test("limits match the hub's and say why", () => {
  const f = (name: string, mime: string, size = 10) => ({ name, mime, size });
  assert.equal(refusal([], [f("a.png", "image/png")]), undefined);
  assert.match(refusal([], [f("a.exe", "application/x-msdownload")])!, /发不了/);
  assert.match(refusal([], [f("big.txt", "text/plain", MAX_FILE_BYTES + 1)])!, /单个文件最多/);
  const held = Array.from({ length: MAX_FILES }, () => ({ size: 1 }));
  assert.match(refusal(held, [f("one.txt", "text/plain")])!, /最多带/);
  assert.match(refusal([], [f("a.pdf", "application/pdf", MAX_FILE_BYTES), f("b.pdf", "application/pdf", MAX_FILE_BYTES), f("c.txt", "text/plain", 5)])!, /合计/);
  assert.equal(bytes(1536), "2 KB");
});

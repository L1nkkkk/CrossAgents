import assert from "node:assert/strict";
import { test } from "node:test";
import type { AttachmentSupport } from "@bindings/AttachmentSupport";
import {
  MAX_FILE_BYTES,
  MAX_FILES,
  IMAGE_MAX_SIDE,
  acceptFor,
  bytes,
  kindOf,
  mediaKindOfPath,
  mimeOf,
  refusal,
  shrinkPlan,
  shrunkName,
  supportNote,
} from "./attachments.ts";

const ALL: AttachmentSupport = { image: true, pdf: true, text: true };
const NO_PDF: AttachmentSupport = { image: true, pdf: false, text: true };
const f = (name: string, mime: string, size = 10) => ({ name, mime, size });

test("types come from the browser, else the extension", () => {
  assert.equal(mimeOf("shot.png", "image/png"), "image/png");
  assert.equal(mimeOf("main.rs", ""), "text/plain");
  assert.equal(mimeOf("notes.md", "application/octet-stream"), "text/markdown");
  assert.equal(mimeOf("tts.mp3", ""), "audio/mpeg");
  assert.equal(mimeOf("demo.mp4", ""), "video/mp4");
  assert.equal(mimeOf("report.docx", ""), "application/vnd.openxmlformats-officedocument.wordprocessingml.document");
  assert.equal(mimeOf("app.exe", "application/x-msdownload"), "application/x-msdownload");
});

test("kind matches the hub's allowed kinds", () => {
  assert.equal(kindOf("image/png"), "image");
  assert.equal(kindOf("image/webp"), "image");
  assert.equal(kindOf("application/pdf"), "pdf");
  assert.equal(kindOf("application/json"), "text");
  assert.equal(kindOf("text/markdown"), "text");
  assert.equal(kindOf("audio/mpeg"), "audio");
  assert.equal(kindOf("audio/wav"), "audio");
  assert.equal(kindOf("video/mp4"), "video");
  assert.equal(kindOf("video/webm"), "video");
  assert.equal(kindOf("application/zip"), "file");
  assert.equal(kindOf("application/x-msdownload"), undefined);
  assert.equal(mediaKindOfPath("out/speech.mp3"), "audio");
  assert.equal(mediaKindOfPath("demo.mp4"), "video");
  assert.equal(mediaKindOfPath("report.pdf"), "pdf");
  assert.equal(mediaKindOfPath("notes.txt"), "text");
});

test("limits match the hub's and say why", () => {
  assert.equal(refusal([], [f("a.png", "image/png")]), undefined);
  assert.equal(refusal([], [f("tts.mp3", "audio/mpeg")]), undefined);
  assert.equal(refusal([], [f("demo.mp4", "video/mp4")]), undefined);
  assert.match(refusal([], [f("a.exe", "application/x-msdownload")])!, /发不了/);
  assert.match(refusal([], [f("big.txt", "text/plain", MAX_FILE_BYTES + 1)])!, /单个文件最多/);
  const held = Array.from({ length: MAX_FILES }, () => ({ size: 1 }));
  assert.match(refusal(held, [f("one.txt", "text/plain")])!, /最多带/);
  assert.match(
    refusal([], [f("a.pdf", "application/pdf", MAX_FILE_BYTES), f("b.pdf", "application/pdf", MAX_FILE_BYTES)])!,
    /合计/,
  );
  assert.equal(bytes(1536), "2 KB");
});

test("the agent's kinds are checked after the hub's limits", () => {
  const why = refusal([], [f("a.pdf", "application/pdf")], NO_PDF, "acp-generic");
  assert.ok(why);
  assert.match(why, /a\.pdf/);
  assert.match(why, /PDF/);
  assert.match(why, /acp-generic/);
  assert.match(why, /图片和文本/);
  assert.equal(refusal([], [f("a.pdf", "application/pdf")], ALL, "claude-native"), undefined);
  assert.equal(refusal([], [f("a.pdf", "application/pdf")], null, "acp-generic"), undefined);
  assert.equal(refusal([], [f("a.pdf", "application/pdf")]), undefined);
  assert.equal(refusal([], [f("a.png", "image/png")], NO_PDF, "acp-generic"), undefined);
  assert.equal(refusal([], [f("a.txt", "text/plain")], NO_PDF, "acp-generic"), undefined);
  assert.equal(refusal([], [f("tts.mp3", "audio/mpeg")], NO_PDF, "acp-generic"), undefined);
  assert.equal(refusal([], [f("demo.mp4", "video/mp4")], NO_PDF, "acp-generic"), undefined);
});

test("a big picture is shrunk, a small one and a GIF are not", () => {
  assert.equal(shrinkPlan("image/png", 500_000, 1920, 1080), undefined);
  assert.equal(shrinkPlan("image/gif", 9_000_000, 6000, 4000), undefined);
  assert.deepEqual(shrinkPlan("image/png", 500_000, 5120, 2880), { width: IMAGE_MAX_SIDE, height: 1440 });
  assert.deepEqual(shrinkPlan("image/jpeg", 8_000_000, 2000, 1500), { width: 2000, height: 1500 });
  assert.deepEqual(shrinkPlan("image/webp", 100, 1000, 9000), { width: 284, height: IMAGE_MAX_SIDE });
  assert.equal(shrunkName("截图.png", "image/jpeg"), "截图.jpg");
  assert.equal(shrunkName("shot", "image/webp"), "shot.webp");
});

test("the picker offers only the kinds the agent takes", () => {
  const noPdf = acceptFor(NO_PDF);
  assert.ok(noPdf.includes("image/png"));
  assert.ok(!noPdf.includes("application/pdf"));
  assert.ok(noPdf.includes("audio/*"));
  assert.ok(noPdf.includes("video/*"));
  assert.ok(noPdf.includes("text/*"));
  assert.ok(noPdf.includes(".md"));
  assert.ok(acceptFor(null).includes("application/pdf"));
  assert.ok(acceptFor(undefined).includes("application/pdf"));
  assert.ok(acceptFor(ALL).includes("application/pdf"));
  assert.equal(acceptFor(ALL), acceptFor(null));
});

test("a partial agent says what it misses", () => {
  assert.equal(supportNote(NO_PDF, "acp-generic"), "这个 agent 只收图片和文本，PDF 发不了（acp-generic）");
  assert.equal(supportNote(null, "acp-generic"), undefined);
  assert.equal(supportNote(undefined), undefined);
  assert.equal(supportNote(ALL, "claude-native"), undefined);
  assert.match(supportNote({ image: false, pdf: false, text: false }, "mock")!, /什么文件都不收/);
});

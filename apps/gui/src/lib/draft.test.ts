import assert from "node:assert/strict";
import { test } from "node:test";

import type { Held } from "../attach";
import { MAX_DRAFT_BYTES, forget, isEmpty, keyOf, read, trim, unpack, write, type Store, type Where } from "./draft.ts";

const HOME: Where = { kind: "home" };

function stubStore(): Store & { map: Map<string, string> } {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

const held = (name: string, data: string, mime = "image/png"): Held => ({ key: 1, name, mime, data, size: data.length });

test("drafts of the home composer and of each conversation are kept apart", () => {
  assert.equal(keyOf(HOME), "crossagents.draft.home");
  assert.equal(keyOf({ kind: "session", agent: "a1" }), "crossagents.draft.session.a1");
  assert.notEqual(keyOf({ kind: "session", agent: "a1" }), keyOf({ kind: "session", agent: "a2" }));
});

test("text and files come back as they went in, and the image preview is rebuilt", () => {
  const store = stubStore();
  write(HOME, { text: "半句话", title: "标题", held: [held("a.png", "QUJD")] }, store);
  const back = read(HOME, store);
  assert.equal(back?.text, "半句话");
  assert.equal(back?.title, "标题");
  assert.equal(back?.held.length, 1);
  assert.equal(back?.held[0].name, "a.png");
  assert.equal(back?.held[0].data, "QUJD");
  // Not stored, so it cannot have come from storage: it is built from the bytes.
  assert.equal(back?.held[0].preview, "data:image/png;base64,QUJD");
  assert.ok(!JSON.parse(store.map.get(keyOf(HOME))!).files[0].preview, "the preview is not stored twice");
});

test("a restored file cannot collide with one added afterwards", () => {
  const store = stubStore();
  write(HOME, { text: "", title: "", held: [held("a.png", "QQ=="), held("b.png", "Qg==")] }, store);
  const keys = read(HOME, store)!.held.map((f) => f.key);
  assert.equal(new Set(keys).size, 2, "restored keys differ");
  // useHeld counts up from 1 for new files; restored ones must be far away from that.
  assert.ok(Math.min(...keys) > 1000, `${keys}`);
});

test("a draft too big for the budget keeps its text and says how many files it cost", () => {
  const big = "x".repeat(2000);
  const draft = { text: "留住这句", title: "", held: [held("a.png", big), held("b.png", big)] };
  const { stored, dropped } = trim(draft, 1000);
  assert.equal(stored.text, "留住这句");
  assert.deepEqual(stored.files, []);
  assert.equal(dropped, 2, "all of them, not some: you are told the images are not kept");
  // Within the budget nothing is dropped.
  assert.equal(trim({ text: "短", title: "", held: [] }, 1000).dropped, 0);
  assert.ok(MAX_DRAFT_BYTES > 1000);
});

test("write reports it when the store refuses, instead of pretending it kept it", () => {
  const full: Store = { getItem: () => null, setItem: () => { throw new Error("quota"); }, removeItem: () => {} };
  const out = write(HOME, { text: "话", title: "", held: [held("a.png", "QQ==")] }, full);
  assert.equal(out.kept, false);
  assert.equal(out.dropped, 1);
});

test("an empty draft is forgotten rather than stored", () => {
  const store = stubStore();
  write(HOME, { text: "话", title: "", held: [] }, store);
  assert.ok(store.map.has(keyOf(HOME)));
  write(HOME, { text: "   ", title: "", held: [] }, store);
  assert.equal(store.map.has(keyOf(HOME)), false, "whitespace alone is nothing to keep");
  assert.equal(read(HOME, store), undefined);
  assert.ok(isEmpty({ text: "\n ", title: "", held: [] }));
});

test("a damaged or foreign value reads as no draft, and never throws", () => {
  const store = stubStore();
  store.map.set(keyOf(HOME), "{not json");
  assert.equal(read(HOME, store), undefined);
  store.map.set(keyOf(HOME), JSON.stringify({ nope: 1 }));
  assert.equal(read(HOME, store), undefined);
  assert.equal(unpack(null), undefined);
  assert.equal(unpack({ text: 1 }), undefined);
  // A file entry that is not one is skipped, the rest of the draft still comes back.
  const mixed = unpack({ text: "话", title: "", files: [{ name: "a.png" }, { name: "b.png", mime: "image/png", data: "Qg==" }] });
  assert.equal(mixed?.held.length, 1);
  assert.equal(mixed?.held[0].name, "b.png");
});

test("reading when there is no storage at all is no draft, not a crash", () => {
  const broken: Store = { getItem: () => { throw new Error("blocked"); }, setItem: () => {}, removeItem: () => {} };
  assert.equal(read(HOME, broken), undefined);
  forget(HOME, broken);
});

test("forgetting a draft leaves the others alone", () => {
  const store = stubStore();
  write(HOME, { text: "首页", title: "", held: [] }, store);
  write({ kind: "session", agent: "a1" }, { text: "会话", title: "", held: [] }, store);
  forget(HOME, store);
  assert.equal(read(HOME, store), undefined);
  assert.equal(read({ kind: "session", agent: "a1" }, store)?.text, "会话");
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { dictionaries, hasKey, missingKeys, setLocale, t } from "./i18n.ts";

/** Files that take their strings from the dictionary. */
const MIGRATED = ["../Sidebar.tsx", "../views/Home.tsx", "../strips.tsx", "../views/Run.tsx", "./continuation.ts"];

/** Every literal key passed to `t(...)` in `file`. */
function keysIn(file: string): string[] {
  const src = readFileSync(new URL(file, import.meta.url), "utf8");
  return [...src.matchAll(/\bt\(\s*"([^"]+)"/g)].map((m) => m[1]);
}

/** The source between two anchors; a file that lost either of them fails loudly. */
function between(src: string, from: string, to: string): string {
  const start = src.indexOf(from);
  assert.ok(start >= 0, `the source no longer has ${from}`);
  const end = src.indexOf(to, start + from.length);
  assert.ok(end > start, `the source no longer has ${to} after ${from}`);
  return src.slice(start, end + to.length);
}

/** Text in those lines that is code, not a comment: a Chinese comment is not hard-coded words. */
const codeLines = (text: string) =>
  text.split("\n").filter((line) => {
    const s = line.trim();
    return !(s.startsWith("//") || s.startsWith("/*") || s.startsWith("*") || s.startsWith("{/*"));
  });

/** Chinese characters, and the punctuation that comes with them. */
const CJK = /[\u3000-\u303f\u4e00-\u9fff\uff00-\uffef]/;

/**
 * The continuation UI in the sources: the run's optional auto-advance, from the form that
 * turns it on to the strip and the meta that report on it. Its words come from the dictionary,
 * so these parts say none themselves.
 */
const CONTINUATION_SOURCE: [file: string, from: string, to: string][] = [
  ["../views/Home.tsx", 't("home.continuation")', 't("home.parallel")'],
  ["../strips.tsx", 'run.continuation?.phase === "exhausted"', 'className="rs-link"'],
  ["../views/Run.tsx", "const CONTINUATION_LABELS", "export function RunView"],
  ["../views/Run.tsx", "<ContinuationMeta", "onDone={onPolicyChange} />"],
  ["../views/Run.tsx", "function ContinuationMeta", "function Ending"],
];

test("every key the migrated files use is in the dictionary", () => {
  for (const file of MIGRATED) {
    const keys = keysIn(file);
    assert.ok(keys.length > 0, `${file} uses no keys`);
    const missing = keys.filter((k) => !hasKey(k));
    assert.deepEqual(missing, [], `${file} uses keys the dictionary lacks`);
  }
});

test("every locale has every key", () => {
  for (const [name, dict] of Object.entries(dictionaries)) {
    assert.deepEqual(missingKeys(dict), [], `${name} is missing keys`);
  }
  assert.deepEqual(missingKeys({ "common.clear": "Clear" }).includes("common.more"), true);
});

test("placeholders are filled, unknown ones are left alone", () => {
  assert.equal(t("common.more", { n: 3 }), "更多 (3)");
  assert.equal(t("sidebar.merged", { merged: 1, total: 4 }), "1/4 已合并");
  assert.equal(t("hub.offline", {}), "未连接：{reason}");
  assert.equal(t("home.run"), "Work");
});

test("an unknown locale falls back to zh-CN", () => {
  setLocale("xx");
  assert.equal(t("nav.usage"), "用量");
  setLocale("zh-CN");
});

test("terms stay untranslated", () => {
  assert.match(t("home.start"), /run/);
  assert.match(t("home.worker"), /Worker$/);
});

test("the continuation keys are in the dictionary", () => {
  const keys = [
    "home.continuation",
    "home.continuationWake",
    "home.continuationHint",
    "home.continuationMaxAttempts",
    "home.continuationInterval",
    "strip.continuationExhausted",
    "continuation.summary",
    "continuation.owner",
    "continuation.nextCheck",
    "continuation.deliveryError",
    "continuation.phase.ended",
    "continuation.phase.paused",
    "continuation.phase.waiting_approval",
    "continuation.phase.unavailable",
    "continuation.phase.waiting_human",
    "continuation.phase.working",
    "continuation.phase.waiting_tasks",
    "continuation.phase.waiting_checks",
    "continuation.phase.finalize",
    "continuation.phase.continue",
    "continuation.phase.recover",
    "continuation.phase.exhausted",
    "continuation.off",
    "continuation.title",
    "continuation.enable",
    "continuation.hint",
    "continuation.maxAttempts",
    "continuation.interval",
    "continuation.save",
    "continuation.saved",
    "continuation.saveDisabled",
    "continuation.reset",
    "continuation.resetHint",
    "continuation.resetDisabled",
    "continuation.resetDone",
    "continuation.stuck",
    "continuation.stuckOff",
    "continuation.stuckHint",
    "continuation.invalidStuck",
    "continuation.invalidMax",
    "continuation.invalidInterval",
    "continuation.forbidden",
    "continuation.saveFailed",
    "continuation.resetFailed",
  ];
  assert.deepEqual(
    keys.filter((k) => !hasKey(k)),
    [],
    "the dictionary lacks continuation keys",
  );
});

test("the continuation summary fills its numbers", () => {
  assert.equal(
    t("continuation.summary", { phase: t("continuation.phase.exhausted"), attempts: 2, max: 3 }),
    "持续推进：次数已用完，需要人工处理 · 已唤醒 2/3 次",
  );
});

test("the continuation UI has no hard-coded Chinese", () => {
  for (const [file, from, to] of CONTINUATION_SOURCE) {
    const src = readFileSync(new URL(file, import.meta.url), "utf8");
    const chinese = codeLines(between(src, from, to)).filter((line) => CJK.test(line));
    assert.deepEqual(chinese, [], `${file} hard-codes Chinese in its continuation part`);
  }
});

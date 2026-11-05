import assert from "node:assert/strict";
import { test } from "node:test";
import type { ReviewFinding } from "@bindings/ReviewFinding";
import {
  blockingFindings,
  confidencePercent,
  findingPlace,
  severityTone,
  sortFindings,
  verdictLabel,
  verdictTone,
} from "./reviewFindings.ts";

const finding = (severity: ReviewFinding["severity"], title: string): ReviewFinding => ({
  severity,
  confidence: 0.5,
  title,
});

test("findings read most severe first and keep the reviewer's order within a severity", () => {
  const input = [finding("P2", "p2 first"), finding("P0", "p0"), finding("P1", "p1"), finding("P2", "p2 second")];
  const sorted = sortFindings(input);
  assert.deepEqual(
    sorted.map((f) => f.title),
    ["p0", "p1", "p2 first", "p2 second"],
  );
  // The input is left alone: the task's stored report keeps what was recorded.
  assert.deepEqual(
    input.map((f) => f.title),
    ["p2 first", "p0", "p1", "p2 second"],
  );
  assert.deepEqual(sortFindings([]), []);
});

test("an unknown severity sorts last instead of vanishing", () => {
  const unknown = { severity: "P9" as ReviewFinding["severity"], confidence: 0.5, title: "odd" };
  assert.deepEqual(
    sortFindings([unknown, finding("P3", "p3")]).map((f) => f.title),
    ["p3", "odd"],
  );
});

test("the blocking findings are exactly P0 and P1, most severe first", () => {
  const input = [finding("P3", "p3"), finding("P1", "p1"), finding("P0", "p0"), finding("P2", "p2")];
  assert.deepEqual(
    blockingFindings(input).map((f) => f.title),
    ["p0", "p1"],
  );
});

test("a verdict reads as a conclusion and a colour", () => {
  assert.equal(verdictLabel("merge"), "可以合并");
  assert.equal(verdictLabel("merge_after_fixes"), "修完可合并");
  assert.equal(verdictLabel("do_not_merge"), "不要合并");
  assert.equal(verdictLabel(null), "未给结论");
  assert.equal(verdictLabel(undefined), "未给结论");
  assert.equal(verdictTone("merge"), "ok");
  assert.equal(verdictTone("merge_after_fixes"), "warn");
  assert.equal(verdictTone("do_not_merge"), "danger");
  assert.equal(verdictTone(undefined), "muted");
});

test("a confidence shows as a clamped whole percent", () => {
  assert.equal(confidencePercent(0.9), 90);
  assert.equal(confidencePercent(0.956), 96);
  assert.equal(confidencePercent(0), 0);
  assert.equal(confidencePercent(1), 100);
  assert.equal(confidencePercent(1.4), 100);
  assert.equal(confidencePercent(-0.2), 0);
  assert.equal(confidencePercent(Number.NaN), 0);
});

test("a finding's place is its path and line when it has one", () => {
  assert.equal(findingPlace({ file: "a.ts", line: 12 }), "a.ts:12");
  assert.equal(findingPlace({ file: "a.ts" }), "a.ts");
  assert.equal(findingPlace({}), "");
});

test("severity tone makes P0/P1 stand out and lets P2/P3 recede", () => {
  assert.equal(severityTone("P0"), "danger");
  assert.equal(severityTone("P1"), "warn");
  assert.equal(severityTone("P2"), "muted");
  assert.equal(severityTone("P3"), "muted");
});

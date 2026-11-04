import assert from "node:assert/strict";
import { test } from "node:test";
import { addSample, formatRate, RATE_WINDOW, tokenRate, type RateSample } from "./tokenRate.ts";

const feed = (points: [number, number][]) => points.reduce<RateSample[]>((s, [at, n]) => addSample(s, at, n), []);

test("no rate from fewer than two samples or no growth", () => {
  assert.equal(tokenRate([], 0), undefined);
  assert.equal(tokenRate(feed([[0, 100]]), 1000), undefined);
  assert.equal(tokenRate(feed([[0, 100], [2000, 100]]), 2000), undefined);
});

test("steady output gives its rate", () => {
  // 40 tokens a second for 5 seconds.
  const s = feed([0, 1, 2, 3, 4, 5].map((i) => [i * 1000, 1000 + i * 40]));
  assert.equal(Math.round(tokenRate(s, 5000)!), 40);
});

test("only the last window counts", () => {
  // Fast for 20s (100/s), then slow for 10s (10/s): the window sees only the slow part.
  const fast = Array.from({ length: 21 }, (_, i) => [i * 1000, i * 100] as [number, number]);
  const slow = Array.from({ length: 10 }, (_, i) => [21_000 + i * 1000, 2000 + (i + 1) * 10] as [number, number]);
  const s = feed([...fast, ...slow]);
  assert.equal(Math.round(tokenRate(s, 30_000)!), 10);
  // Old samples are dropped, keeping one at or before the window start.
  assert.ok(s[0].at <= 30_000 - RATE_WINDOW && s[1].at > 30_000 - RATE_WINDOW);
});

test("the window start is interpolated between samples", () => {
  // 0 → 1000 over 20s, sampled only at both ends: 50/s throughout.
  const s = feed([[0, 0], [20_000, 1000]]);
  assert.equal(Math.round(tokenRate(s, 20_000)!), 50);
});

test("a turn that went quiet slows down and stops", () => {
  const s = feed([[0, 0], [1000, 50], [2000, 100]]);
  assert.equal(Math.round(tokenRate(s, 2000)!), 50);
  assert.equal(Math.round(tokenRate(s, 4000)!), 25);
  assert.equal(tokenRate(s, 2000 + RATE_WINDOW + 1), undefined);
});

test("a count that goes down starts over", () => {
  const s = feed([[0, 5000], [1000, 5100], [2000, 10]]);
  assert.deepEqual(s, [{ at: 2000, tokens: 10 }]);
  assert.equal(tokenRate(s, 2000), undefined);
});

test("two close samples do not make a spike", () => {
  const s = feed([[0, 0], [10, 50]]);
  assert.equal(Math.round(tokenRate(s, 10)!), 50);
});

test("format hides nothing-to-show and abbreviates thousands", () => {
  assert.equal(formatRate(undefined), undefined);
  assert.equal(formatRate(0), undefined);
  assert.equal(formatRate(0.4), undefined);
  assert.equal(formatRate(42.3), "42 tok/s");
  assert.equal(formatRate(1234), "1.2k tok/s");
});

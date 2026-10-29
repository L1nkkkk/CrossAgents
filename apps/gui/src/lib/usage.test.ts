import assert from "node:assert/strict";
import { test } from "node:test";
import { compactUsage, comparison, niceScale, smoothPath } from "./usage.ts";

test("compact token boundaries", () => {
  assert.equal(compactUsage(999), "999");
  assert.equal(compactUsage(1000), "1K");
  assert.equal(compactUsage(999_999), "1M");
  assert.equal(compactUsage(1e9), "1B");
});

test("compare equal windows including a missing or zero baseline", () => {
  assert.equal(comparison(144, 100), "↑ 44%");
  assert.equal(comparison(75, 100), "↓ 25%");
  assert.equal(comparison(0, 100), "↓ 100%");
  assert.equal(comparison(1, 0), null);
  assert.equal(comparison(1, null), null);
});

test("rounded axis steps cover the data", () => {
  assert.deepEqual(niceScale(900_000), { top: 1_000_000, step: 250_000 });
  assert.deepEqual(niceScale(5e9), { top: 6e9, step: 2e9 });
  assert.deepEqual(niceScale(0), { top: 1, step: 0.25 });
});

test("cubic controls stay inside adjacent values on plateaus and spikes", () => {
  for (const ys of [
    [0, 0, 10, 0, 0],
    [2, 5, 5, 9, 3],
  ]) {
    const path = smoothPath(ys.map((y, x) => ({ x: x * 3, y })));
    const segments = [...path.matchAll(/ C([^ ]+) ([^ ]+) ([^ ]+)/g)];
    assert.equal(segments.length, ys.length - 1);
    segments.forEach((segment, i) => {
      const controls = [segment[1], segment[2]].map((p) =>
        Number(p.split(",")[1]),
      );
      for (const y of controls)
        assert.ok(
          y >= Math.min(ys[i], ys[i + 1]) && y <= Math.max(ys[i], ys[i + 1]),
        );
    });
  }
});

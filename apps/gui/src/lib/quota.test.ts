import assert from "node:assert/strict";
import { test } from "node:test";
import { creditText, level, planLabel, resetText, tightest, windowLabel } from "./quota.ts";

test("names windows by their length", () => {
  assert.equal(windowLabel({ name: "primary", window_seconds: 5 * 3600 }), "5 小时限额");
  assert.equal(windowLabel({ name: "secondary", window_seconds: 7 * 86400 }), "每周限额");
  assert.equal(windowLabel({ name: "primary", window_seconds: 43200 * 60 }), "30 天限额");
  assert.equal(windowLabel({ name: "primary", window_seconds: null }), "主限额");
});

test("reset times read relative when close, as a date when far", () => {
  const now = new Date(2026, 9, 22, 10, 0).getTime();
  assert.equal(resetText(null, now), undefined);
  assert.equal(resetText(new Date(now + 25 * 60_000).toISOString(), now), "25 分钟后重置");
  assert.equal(resetText(new Date(now + (2 * 60 + 14) * 60_000).toISOString(), now), "2 小时 14 分后重置");
  assert.match(resetText(new Date(2026, 9, 23, 22, 30).toISOString(), now)!, /^明天 .*重置$/);
  assert.equal(resetText(new Date(2026, 10, 21, 9, 0).toISOString(), now), "11月21日 周六 重置");
});

test("levels, plans and credits", () => {
  assert.equal(level(8), "ok");
  assert.equal(level(80), "warn");
  assert.equal(level(95), "danger");
  assert.equal(level(10, true), "danger");
  assert.equal(planLabel("free"), "Free");
  assert.equal(planLabel("team_plus"), "Team Plus");
  assert.equal(creditText("53035.2989180000"), (53035.3).toLocaleString(undefined, { maximumFractionDigits: 2 }));
});

test("tightest picks the most used window", () => {
  const w = (used_percent: number) => ({ name: "x", used_percent, window_seconds: null, resets_at: null });
  const found = tightest([
    { id: "a", windows: [w(10), w(40)] },
    { id: "b", windows: [w(30)] },
  ]);
  assert.equal(found?.account.id, "a");
  assert.equal(found?.window.used_percent, 40);
  assert.equal(tightest([]), undefined);
});

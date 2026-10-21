import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_POLICY, budgetText, presetOf, presetPolicy, remaining } from "./policy.ts";

test("presets map to policies and back", () => {
  assert.equal(presetOf(DEFAULT_POLICY), "manual");
  const full = presetPolicy("full");
  assert.equal(full.plan_approval, "auto");
  assert.equal(full.land, "auto");
  assert.equal(full.human_ask, "timeout");
  assert.equal(presetOf(full), "full");
  assert.equal(presetOf(presetPolicy("semi")), "semi");
  assert.equal(presetOf({ ...DEFAULT_POLICY, land: "pr" }), undefined);
});

test("budget and timeout text", () => {
  assert.equal(budgetText({ max_usd: 5, max_tokens: null, max_minutes: 120 }), "$5 · 2 小时");
  assert.equal(budgetText({ max_usd: null, max_tokens: 200000, max_minutes: 45 }), "200k tokens · 45 分钟");
  assert.equal(budgetText({ max_usd: null, max_tokens: null, max_minutes: null }), "");
  const now = Date.UTC(2026, 9, 22, 10, 0);
  assert.equal(remaining(new Date(now + 12 * 60_000).toISOString(), now), "12 分钟后");
  assert.equal(remaining(new Date(now - 1000).toISOString(), now), "即将");
  assert.equal(remaining(new Date(now + 90 * 60_000).toISOString(), now), "1 小时 30 分后");
});

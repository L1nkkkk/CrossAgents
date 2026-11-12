import assert from "node:assert/strict";
import { test } from "node:test";
import { creditText, egressOf, level, planLabel, resetText, tightest, windowLabel } from "./quota.ts";

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

test("an exit reads as its IP, country, ASN, route and last change", () => {
  const now = new Date(2026, 9, 22, 10, 0).getTime();
  const exit = egressOf(
    {
      ip: "203.0.113.9",
      asn: "AS13335",
      country: "US",
      via_proxy: true,
      changed_at: new Date(now - 2 * 3600_000).toISOString(),
    },
    now,
  );
  assert.deepEqual(exit, { known: true, ip: "203.0.113.9", country: "US", asn: "AS13335", route: "proxy", routeLabel: "经代理", changed: "08:00" });
});

test("a direct exit and an unconfirmed route are told apart", () => {
  const now = new Date(2026, 9, 22, 10, 0).getTime();
  const base = { ip: "203.0.113.9", asn: "AS13335", country: "US", changed_at: null };
  assert.equal(egressOf({ ...base, via_proxy: false }, now).routeLabel, "直连");
  assert.equal(egressOf({ ...base, via_proxy: true }, now).routeLabel, "经代理");
  // The hub cannot tell (allow_direct or NO_PROXY): say so, do not guess "proxy".
  assert.equal(egressOf({ ...base, via_proxy: null }, now).routeLabel, "无法确认");
});

test("an exit never recorded has no values to show, and missing ones stay missing", () => {
  const now = new Date(2026, 9, 22, 10, 0).getTime();
  const none = egressOf(undefined, now);
  assert.equal(none.known, false);
  assert.equal(none.ip, "IP 未知");
  assert.equal(none.country, "国家未知");
  assert.equal(none.asn, "ASN 未知");
  assert.equal(none.routeLabel, "无法确认");
  assert.equal(none.changed, undefined);

  const partial = egressOf({ ip: null, asn: null, country: "GB", via_proxy: true, changed_at: null }, now);
  assert.equal(partial.known, true);
  assert.equal(partial.ip, "IP 未知");
  assert.equal(partial.country, "GB");
  assert.equal(partial.changed, undefined);
});

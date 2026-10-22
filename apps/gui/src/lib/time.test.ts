import assert from "node:assert/strict";
import { test } from "node:test";
import { clock, day, full, range, when } from "./time.ts";

// Local times, so the tests do not depend on the machine's time zone.
const now = new Date(2026, 9, 22, 14, 30, 0); // 2026-10-22 14:30:00
const at = (y: number, mo: number, d: number, h: number, mi: number, s = 0) => new Date(y, mo - 1, d, h, mi, s);

test("within the hour reads relative", () => {
  assert.equal(when(at(2026, 10, 22, 14, 29, 30), now), "刚刚");
  assert.equal(when(at(2026, 10, 22, 14, 18), now), "12 分钟前");
  assert.equal(when(at(2026, 10, 22, 13, 31), now), "59 分钟前");
  // A clock slightly ahead of ours is still "just now".
  assert.equal(when(at(2026, 10, 22, 14, 30, 20), now), "刚刚");
});

test("after the hour reads as the clock", () => {
  assert.equal(when(at(2026, 10, 22, 11, 2), now), "11:02");
  assert.equal(when(at(2026, 10, 21, 18, 40), now), "昨天 18:40");
  assert.equal(when(at(2026, 10, 21, 9, 15), now), "昨天 09:15");
  assert.equal(when(at(2026, 10, 3, 9, 15), now), "10-03 09:15");
  assert.equal(when(at(2025, 12, 31, 23, 59), now), "2025-12-31 23:59");
});

test("clock ignores how recent it is", () => {
  assert.equal(clock(at(2026, 10, 22, 14, 29), now), "14:29");
  // Yesterday is by calendar date, not 24 hours.
  assert.equal(clock(at(2026, 10, 21, 23, 59), new Date(2026, 9, 22, 0, 1)), "昨天 23:59");
});

test("day groups by calendar date", () => {
  assert.equal(day(at(2026, 10, 22, 0, 0), now), "今天");
  assert.equal(day(at(2026, 10, 21, 23, 0), now), "昨天");
  assert.equal(day(at(2026, 10, 1, 8, 0), now), "10-01");
  assert.equal(day(at(2025, 10, 1, 8, 0), now), "2025-10-01");
});

test("tooltips show the full time and ranges", () => {
  assert.equal(full(at(2026, 10, 22, 9, 5, 7)), "2026-10-22 09:05:07");
  assert.equal(range(at(2026, 10, 22, 11, 2, 13), at(2026, 10, 22, 11, 5, 40)), "2026-10-22 11:02:13 → 11:05:40");
  assert.equal(range(at(2026, 10, 21, 23, 50), at(2026, 10, 22, 0, 10)), "2026-10-21 23:50:00 → 2026-10-22 00:10:00");
  assert.equal(range(at(2026, 10, 22, 11, 2, 13), null), "2026-10-22 11:02:13 →");
});

test("bad input comes back as it was", () => {
  assert.equal(when("not a time", now), "not a time");
  assert.equal(full("nope"), "nope");
});

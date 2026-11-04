import assert from "node:assert/strict";
import { test } from "node:test";
import type { BackgroundResponse } from "@bindings/BackgroundResponse";
import { board, countdown, elapsed, watchOn, withoutReminder } from "./background.ts";

const reminder = (id: string, owner: string, run: string | null, task: string | null = null) => ({
  reminder: { id, owner, due_at: "2026-11-04T10:30:00Z", note: id, task, only_if_open: false, created_at: "2026-11-04T10:00:00Z" },
  owner_name: owner,
  task_key: task,
  run_id: run,
});

const bg: BackgroundResponse = {
  reminders: [reminder("r-luna", "luna", "run-a"), reminder("r-vega", "vega", "run-b"), reminder("r-solo", "solo", null)],
  watches: [
    { watch: { id: "w-1", owner: "vega", target: { kind: "agent", agent_id: "luna" }, on: ["done"], created_at: "2026-11-04T10:00:00Z" }, owner_name: "vega", target_name: "luna", run_id: null },
    { watch: { id: "w-2", owner: "solo", target: { kind: "task", task_id: "t-9" }, on: [], created_at: "2026-11-04T10:00:00Z" }, owner_name: "solo", target_name: "#auth", run_id: "run-b" },
  ],
  verifying: [
    { task_id: "t-1", run_id: "run-a", run_title: "A", key: "api", title: "API", assignee: "mia", assignee_name: "mia", checks: 2, since: "2026-11-04T10:00:00Z" },
    { task_id: "t-2", run_id: "run-c", run_title: "C", key: "ui", title: "UI", assignee: "rex", assignee_name: "rex", checks: 1, since: "2026-11-04T10:00:00Z" },
  ],
};

const ids = <T>(xs: T[], id: (x: T) => string) => xs.map(id);

test("with nothing in front, everything is near", () => {
  const b = board(bg, {});
  assert.equal(b.reminders.other.length + b.watches.other.length + b.verifying.other.length, 0);
});

test("the followed agent's items and its runs' come first", () => {
  const b = board(bg, { agent: "luna" });
  assert.deepEqual(ids(b.reminders.near, (r) => r.reminder.id), ["r-luna"]);
  assert.deepEqual(ids(b.reminders.other, (r) => r.reminder.id), ["r-vega", "r-solo"]);
  // Vega watches Luna: that concerns Luna.
  assert.deepEqual(ids(b.watches.near, (w) => w.watch.id), ["w-1"]);
  // Luna's reminder belongs to run-a, so run-a's verification is near too.
  assert.deepEqual(ids(b.verifying.near, (v) => v.key), ["api"]);
});

test("a routed run pulls in its items", () => {
  const b = board(bg, { run: "run-b" });
  assert.deepEqual(ids(b.reminders.near, (r) => r.reminder.id), ["r-vega"]);
  assert.deepEqual(ids(b.watches.near, (w) => w.watch.id), ["w-2"]);
  assert.deepEqual(b.verifying.near, []);
});

test("a fired reminder leaves the board", () => {
  assert.deepEqual(ids(withoutReminder(bg, "r-vega").reminders, (r) => r.reminder.id), ["r-luna", "r-solo"]);
});

test("countdowns and durations", () => {
  const now = new Date("2026-11-04T10:00:00Z");
  assert.equal(countdown("2026-11-04T10:00:45Z", now), "还有 45 秒");
  assert.equal(countdown("2026-11-04T10:12:05Z", now), "还有 12 分 05 秒");
  assert.equal(countdown("2026-11-04T12:03:00Z", now), "还有 2 小时 3 分");
  assert.equal(countdown("2026-11-07T14:00:00Z", now), "还有 3 天 4 小时");
  assert.equal(countdown("2026-11-04T09:59:00Z", now), "已到期");
  assert.equal(elapsed("2026-11-04T09:56:48Z", now), "3 分 12 秒");
  assert.equal(elapsed("2026-11-04T08:55:00Z", now), "1 小时 5 分");
});

test("watch events read in Chinese, nothing meaning any", () => {
  assert.equal(watchOn(["done", "failed"]), "完成、失败");
  assert.equal(watchOn([]), "任何变化");
});

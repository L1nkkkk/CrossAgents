import assert from "node:assert/strict";
import { test } from "node:test";
import type { TaskStatus } from "@bindings/TaskStatus";
import { groupTasks, progress, runState, taskState } from "./taskState.ts";

const task = (status: TaskStatus, extra: { attempts?: number; depends_on?: string[] } = {}) => ({
  status,
  attempts: extra.attempts ?? 1,
  depends_on: extra.depends_on ?? [],
});

test("every state is a glyph and a word, and says what it waits for", () => {
  assert.deepEqual([taskState(task("pending")).glyph, taskState(task("pending")).word], ["○", "排队中"]);
  assert.equal(taskState(task("pending", { depends_on: ["db", "api"] })).word, "等 db、api");
  // With the run's tasks it names only what is still open; all merged means it waits for a slot.
  const run = [{ key: "db", status: "merged" as const }, { key: "api", status: "running" as const }];
  assert.equal(taskState(task("pending", { depends_on: ["db", "api"] }), run).word, "等 api");
  assert.equal(taskState(task("pending", { depends_on: ["db"] }), run).word, "排队中");
  assert.equal(taskState(task("running", { attempts: 2 })).word, "第 2 次尝试");
  assert.equal(taskState(task("waiting_answer")).word, "等回答");
  assert.equal(taskState(task("stalled")).word, "停了 · 未上报");
  assert.equal(taskState(task("passed")).word, "待检查");
  assert.deepEqual([taskState(task("merged")).glyph, taskState(task("failed")).glyph], ["✓", "✕"]);
});

test("plans awaiting approval need you; tasks are grouped in bucket order", () => {
  assert.equal(runState("awaiting_approval").bucket, "needs_you");
  const groups = groupTasks([task("merged"), task("running"), task("pending"), task("reviewing")]);
  assert.deepEqual(
    groups.map((g) => [g.bucket, g.tasks.map((t) => t.status)]),
    [
      ["waiting", ["pending"]],
      ["active", ["running", "reviewing"]],
      ["ended", ["merged"]],
    ],
  );
});

test("progress reads as one phrase", () => {
  assert.equal(progress([task("merged"), task("running"), task("failed")]), "1/3 已合并 · 1 进行中 · 1 失败");
  assert.equal(progress([]), "");
});

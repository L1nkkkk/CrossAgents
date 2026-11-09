import assert from "node:assert/strict";
import { test } from "node:test";
import type { Task } from "@bindings/Task";
import type { TaskStatus } from "@bindings/TaskStatus";
import { checksPassed, groupTasks, progress, runState, taskState } from "./taskState.ts";

const check = (passed: boolean) => ({ command: "cargo test", exit_code: passed ? 0 : 1, passed, output: "" });

/** A task's report: the checks and flags the state reads (docs/TODO.md §九 ①). */
const report = (init: { checks?: Task["report"]["checks"]; interrupted?: boolean; unchecked?: boolean } = {}): Task["report"] => ({
  summary: null,
  checks: init.checks ?? [],
  diff_stat: null,
  failure: null,
  question: null,
  question_message: null,
  review: null,
  rework: [],
  verification: null,
  verified_sha: null,
  interrupted: init.interrupted ?? false,
  unchecked: init.unchecked ?? false,
  forced: null,
});

const task = (status: TaskStatus, extra: { attempts?: number; depends_on?: string[]; report?: Task["report"] } = {}) => ({
  status,
  attempts: extra.attempts ?? 1,
  depends_on: extra.depends_on ?? [],
  report: extra.report,
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

test("passed says what was actually checked, never 'done' for unchecked", () => {
  const passed = (r: Task["report"]) => taskState(task("passed", { report: r }));
  assert.equal(passed(report({ checks: [check(true)] })).word, "检查通过");
  assert.equal(passed(report({ checks: [check(true)] })).tone, "ok");
  // Failed checks are not a pass, and are not quiet either.
  const failed = passed(report({ checks: [check(false)] }));
  assert.equal(failed.word, "验收未过");
  assert.equal(failed.tone, "danger");
  // No checks at all: the report is ready, nothing verified it.
  assert.equal(passed(report({ unchecked: true })).word, "未验 · 待接收");
  assert.equal(passed(report({ unchecked: true })).tone, "warn");
  // The hub restarted before the checks finished.
  assert.equal(passed(report({ checks: [check(true)], interrupted: true })).word, "验收中断");
  assert.equal(passed(report({ checks: [check(true)], interrupted: true })).tone, "warn");
  // All three wait for the issuer, whichever way they read.
  for (const r of [report({ checks: [check(true)] }), report({ checks: [check(false)] }), report({ interrupted: true })]) {
    assert.equal(passed(r).bucket, "waiting");
  }
  // Verifying is work in progress, not a verdict.
  assert.deepEqual([taskState(task("verifying")).word, taskState(task("verifying")).bucket], ["验收中", "active"]);
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

test("checks say how many passed, and nothing before any ran", () => {
  // None ran: not "0/0 检查通过" — nothing verified the task yet.
  assert.equal(checksPassed([]), "");
  assert.equal(checksPassed([check(true)]), "1/1 检查通过");
  assert.equal(checksPassed([check(true), check(false), check(true), check(true)]), "3/4 检查通过");
});

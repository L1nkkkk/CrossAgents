import assert from "node:assert/strict";
import { test } from "node:test";
import type { HumanItem } from "@bindings/HumanItem";
import type { Run } from "@bindings/Run";
import type { Task } from "@bindings/Task";
import { DEFAULT_POLICY, presetPolicy } from "./policy.ts";
import { todoCount, todoItems, visibleTodos, type TodoRun } from "./todos.ts";

const now = Date.parse("2026-11-01T12:00:00Z");
const at = (hour: number) => `2026-11-01T${String(hour).padStart(2, "0")}:00:00Z`;
const human = (id: string, extra: Partial<HumanItem> = {}): HumanItem => ({
  id, from: "leo", kind: "question", status: "open", text: `Question ${id}`,
  run_id: null, task_id: null, detail: null, options: [], default: null,
  timeout_at: null, answer: null, answered_at: null, action: null, created_at: at(4), ...extra,
});
// Only the fields the derivation consumes are needed for these legacy snapshots.
const run = (extra: Partial<Run> = {}): Run => ({
  id: "run", planner: "leo", title: "Work", status: "running", created_at: at(1), updated_at: at(10), note: null,
  config: { auto_approve: false, policy: DEFAULT_POLICY }, ...extra,
} as Run);
const task = (extra: Partial<Task> = {}): Task => ({
  id: "task", run_id: "run", title: "Deliver", status: "passed", assignee: "leo", issuer: "lead",
  created_at: at(2), updated_at: at(11), handoffs: [], ...extra,
} as Task);
const derive = (items: HumanItem[] = [], runs: TodoRun[] = []) => todoItems({ items, runs, now });

test("two items from one agent survive deduplication and count as two", () => {
  const first = human("one");
  const second = human("two", { kind: "notice" });
  const items = derive([first, second, first]);
  assert.deepEqual(items.map((item) => item.id), ["one", "two"]);
  assert.equal(todoCount(items), 2);
  assert.deepEqual(items.map((item) => item.action), ["answer", "view"]);
  assert.equal(items[0].href, "#/s/leo/questions/one");
  assert.equal(derive([{ ...first, status: "answered" }, second]).length, 1);
  assert.equal(derive([first, second]).length, 2); // opening a session supplies no dismissal
});

test("completed unread, idle, ordinary work and recent defaults are not human work", () => {
  const finished = { run: run({ status: "completed" }), tasks: [task({ status: "merged" })], unread: { leo: { kind: "done", at: now } } };
  assert.deepEqual(derive([], [finished, { run: run({ id: "active" }), tasks: [] }]), []);
  assert.deepEqual(derive([human("default", { status: "defaulted", answered_at: at(11) }), human("dismissed", { status: "dismissed" })]), []);
});

test("auto approval does not invent approval cards; actual human requests remain", () => {
  for (const preset of ["semi", "full"] as const) {
    const r = run({ status: "awaiting_approval" });
    r.config = { ...r.config, policy: presetPolicy(preset) };
    assert.deepEqual(derive([], [{ run: r, tasks: [] }]), []);
    assert.equal(derive([human("tool", { action: "approval", run_id: r.id })], [{ run: r, tasks: [] }]).length, 1);
  }
  const manual = run({ status: "awaiting_approval" });
  assert.equal(derive([], [{ run: manual, tasks: [] }])[0].href, "#/runs/run");
  const legacy = { ...manual, config: { ...manual.config, auto_approve: true } };
  assert.deepEqual(derive([], [{ run: legacy, tasks: [] }]), []);
  const request = human("approval", { action: "approval", run_id: manual.id });
  assert.deepEqual(derive([request], [{ run: manual, tasks: [] }]).map((item) => item.id), [request.id]);
});

test("oldest wait comes first; activity timestamps and input order do not change it", () => {
  const r = { ...run({ status: "awaiting_approval" }), pending_since: at(8) };
  const t = { ...task(), pending_since: at(3) };
  const inputs = [human("late", { created_at: at(9) }), human("early", { created_at: at(2), timeout_at: at(1) })];
  const details = [{ run: r, tasks: [t] }];
  const result = derive(inputs, details);
  assert.deepEqual(result.map((item) => item.id), ["early", "task", "run", "late"]);
  r.updated_at = at(0);
  t.updated_at = at(0);
  assert.deepEqual(derive(inputs.slice().reverse(), details), result);
  assert.deepEqual(todoItems({ items: inputs, runs: details, now: now + 86400000 }), result);
  // Legacy records without a transition timestamp have a stable creation-time fallback.
  assert.equal(derive([], [{ run: run(), tasks: [task()] }])[0].since, at(2));
});

test("cap defaults to three, reports remaining items, and expands without mutating input", () => {
  const items = derive([1, 2, 3, 4, 5].map((id) => human(String(id))));
  assert.deepEqual(visibleTodos(items), { items: items.slice(0, 3), remaining: 2 });
  assert.deepEqual(visibleTodos(items, { expanded: true }), { items, remaining: 0 });
  assert.deepEqual(visibleTodos(items, { cap: 2, expanded: false }), { items: items.slice(0, 2), remaining: 3 });
  assert.equal(items.length, 5);
  assert.deepEqual(visibleTodos([]), { items: [], remaining: 0 });
});

test("latest handoff determines task waiting order ahead of creation time", () => {
  const handoff = (hour: number) => ({ at: at(hour) } as Task["handoffs"][number]);
  const olderTask = task({ id: "created-first", created_at: at(1), handoffs: [handoff(2), handoff(8)] });
  const newerTask = task({ id: "created-later", created_at: at(3), handoffs: [handoff(5)] });
  const details = [{ run: run(), tasks: [olderTask, newerTask] }];
  const result = derive([], details);
  assert.deepEqual(result.map(({ id, since }) => ({ id, since })), [
    { id: "created-later", since: at(5) },
    { id: "created-first", since: at(8) },
  ]);
  olderTask.updated_at = at(0);
  newerTask.updated_at = at(11);
  assert.deepEqual(derive([], details), result);
  // Explicit transition times override the legacy handoff approximation.
  assert.equal(derive([], [{ run: run(), tasks: [{ ...olderTask, pending_since: at(9) }] }])[0].since, at(9));
  assert.equal(derive([], [{ run: run(), tasks: [task({ handoffs: [] })] }])[0].since, at(2));
});

test("unavailable runs and missing tasks retain the todo and explain the missing target", () => {
  const missingRun = derive([human("lost-run", { action: "approval", run_id: "deleted" })])[0];
  assert.equal(missingRun.href, undefined);
  assert.match(missingRun.reason!, /工作.*删除/);
  const missingTask = derive([human("lost-task", { action: "approval", run_id: "run", task_id: "deleted" })], [{ run: run(), tasks: [] }])[0];
  assert.equal(missingTask.href, undefined);
  assert.match(missingTask.reason!, /任务找不到/);
  assert.equal(missingTask.id, "lost-task");
});

test("task approval and acceptance use persistent task destinations with escaped IDs", () => {
  const r = run({ id: "run / 1" });
  const t = task({ id: "task / 1", run_id: r.id });
  const approval = human("approval", { action: "approval", run_id: r.id, task_id: t.id });
  const items = derive([approval], [{ run: r, tasks: [t, t] }]);
  assert.equal(items.length, 2);
  assert.ok(items.every((item) => item.href === "#/runs/run%20%2F%201/tasks/task%20%2F%201"));
  assert.equal(items.find((item) => item.kind === "acceptance")?.action, "confirm");
});

test("failures managed by a running issuer are quiet; paused or failed work needs intervention", () => {
  assert.deepEqual(derive([], [{ run: run(), tasks: [task({ status: "failed" })] }]), []);
  for (const status of ["paused", "failed"] as const) {
    const items = derive([], [{ run: run({ status, note: "Needs recovery" }), tasks: [task({ status: "failed" })] }]);
    assert.equal(items.length, 1);
    assert.equal(items[0].kind, "failure");
    assert.equal(items[0].summary, "Needs recovery");
    assert.equal(items[0].href, "#/runs/run");
  }
});

import assert from "node:assert/strict";
import { test } from "node:test";
import type { Handoff } from "@bindings/Handoff";
import type { RunTrace } from "@bindings/RunTrace";
import type { TaskTrace } from "@bindings/TaskTrace";
import { buildSwimlane, focusLane, holderAt, waitingOnYou } from "./swimlane.ts";

const at = (m: number) => new Date(Date.UTC(2026, 10, 6, 10, m)).toISOString();
const empty = { agents: [], turns: [], commands: [], spent: { tokens: 0, cost_usd: 0 } };

const card = (kind: Handoff["kind"], to: string, to_name: string, m: number, from: string | null = null): Handoff => ({
  id: `h-${to}-${m}`,
  run_id: "run",
  task_id: "t",
  task_key: "a",
  kind,
  from,
  from_name: from ? from.toUpperCase() : "lead",
  to,
  to_name,
  reason: kind === "assign" ? null : "额度用尽",
  goal: "g",
  files: [],
  summary: null,
  decisions: [],
  open_questions: [],
  context_tokens: null,
  fresh: true,
  outcome: null,
  at: at(m),
});

const taskA: TaskTrace = {
  task_id: "t",
  key: "a",
  title: "A",
  status: "merged",
  failure: null,
  lane: {
    ...empty,
    agents: ["w1", "w2"],
    marks: [
      { at: at(3), agent: "w1", kind: "compacted", text: "170k → 40k" },
      { at: at(4), agent: "w1", kind: "asked", text: "用哪个库？" },
      { at: at(5), agent: "w1", kind: "answered", text: "serde" },
      { at: at(12), agent: "w2", kind: "model_changed", text: "sonnet → opus" },
    ],
  },
  checks: [
    { at: at(7), passed: false, checks: [{ command: "cargo test", exit_code: 101, passed: false, output: "" }] },
    { at: at(14), passed: true, checks: [{ command: "cargo test", exit_code: 0, passed: true, output: "" }] },
  ],
  rerouted: [],
  handoffs: [card("assign", "w1", "Rex", 1), card("reroute", "w2", "Mia", 10, "w1")],
  first_failure: null,
  statuses: [
    { at: at(6), from: "running", to: "verifying", agent: "w1" },
    { at: at(8), from: "verifying", to: "retrying", agent: null },
    { at: at(13), from: "running", to: "verifying", agent: "w2" },
    { at: at(15), from: "passed", to: "merged", agent: "w2" },
  ],
};

const trace: RunTrace = {
  run_id: "run",
  lead: { ...empty, agents: ["lead"], marks: [{ at: at(16), agent: "lead", kind: "asked", text: "落地吗？" }] },
  tasks: [taskA],
  spent: { tokens: 0, cost_usd: 0 },
  landed: { mode: "merge", by: "person", reference: "abc123", at: at(20) },
};

test("one lane per worker, each node on the worker that held the task then", () => {
  const s = buildSwimlane(trace);
  assert.deepEqual(
    s.lanes.map((l) => l.name),
    ["Rex", "Mia", "lead"],
  );
  const [rex, mia] = s.lanes;
  assert.deepEqual(
    rex.nodes.map((n) => n.kind),
    ["handoff", "compact", "ask", "answer", "commit", "test_fail", "retry"],
  );
  assert.deepEqual(
    mia.nodes.map((n) => n.kind),
    ["reroute", "model", "commit", "test_pass", "merge"],
  );
  assert.equal(mia.nodes[0].from, "w1");
  assert.equal(rex.nodes.find((n) => n.kind === "ask")!.open, false);
  assert.equal(s.landed?.kind, "land");
  assert.equal(s.slots, 14);
  // Slots follow time across lanes.
  const slots = s.lanes.flatMap((l) => l.nodes.map((n) => [n.at, n.slot] as const)).sort((a, b) => a[1] - b[1]);
  assert.deepEqual(slots.map((x) => x[0]), [...slots.map((x) => x[0])].sort());
});

test("a question nobody answered waits on you", () => {
  const s = buildSwimlane(trace);
  assert.equal(waitingOnYou(s), 1);
  assert.equal(s.lanes[2].nodes[0].label, "等你");
});

test("focus keeps one lane and packs its axis", () => {
  const s = focusLane(buildSwimlane(trace), "w2");
  assert.equal(s.lanes.length, 1);
  assert.deepEqual(
    s.lanes[0].nodes.map((n) => n.slot),
    [0, 1, 2, 3, 4],
  );
  assert.equal(s.landed?.slot, 5);
  assert.equal(focusLane(buildSwimlane(trace), null).lanes.length, 3);
});

test("a retry without its worker goes to whoever held the task", () => {
  assert.equal(holderAt(taskA, at(8)), "w1");
  assert.equal(holderAt(taskA, at(11)), "w2");
  assert.equal(holderAt({ ...taskA, handoffs: [] }, at(11)), "w1");
});

test("an old trace without the new fields still folds", () => {
  const old: RunTrace = { run_id: "run", lead: empty, tasks: [{ ...taskA, statuses: undefined, lane: { ...empty, agents: ["w1"] }, handoffs: [], checks: [] }], spent: { tokens: 0, cost_usd: 0 } };
  const s = buildSwimlane(old, () => "Rex");
  assert.equal(s.lanes.length, 1);
  assert.equal(s.lanes[0].nodes.length, 0);
  assert.equal(s.landed, null);
});

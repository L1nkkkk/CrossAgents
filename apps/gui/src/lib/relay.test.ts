import assert from "node:assert/strict";
import { test } from "node:test";
import type { Agent } from "@bindings/Agent";
import type { Handoff } from "@bindings/Handoff";
import type { Run } from "@bindings/Run";
import type { Task } from "@bindings/Task";
import { awaitsAnswer, relayOf, relayText } from "./relay.ts";

const agent = (name: string) => ({ id: `id-${name}`, name }) as Agent;

/** One handoff of task #a: who holds it, and how that hand ended (null: still going). */
const hand = (by: string, outcome: Handoff["outcome"]): Handoff => ({
  id: `h-${by}`,
  run_id: "run",
  task_id: "task",
  task_key: "a",
  kind: "assign",
  from: null,
  from_name: "Ada",
  to: `id-${by}`,
  to_name: by,
  reason: null,
  goal: "g",
  files: [],
  summary: null,
  decisions: [],
  open_questions: [],
  context_tokens: null,
  fresh: true,
  outcome,
  at: "2026-01-01T00:00:00Z",
});

const run = { id: "run", planner: "id-Ada", config: { worker: "id-Rex" } } as unknown as Run;

const task = (init: {
  status: Task["status"];
  handoffs?: Handoff[];
  assignee?: string | null;
  worker_template?: string | null;
  question?: string | null;
}): Task =>
  ({
    id: "task",
    run_id: "run",
    key: "a",
    title: "t",
    status: init.status,
    assignee: init.assignee ?? null,
    worker_template: init.worker_template ?? null,
    issuer: "id-Ada",
    report: { question: init.question ?? null },
    handoffs: init.handoffs ?? [],
  }) as unknown as Task;

const team = [agent("Ada"), agent("Rex"), agent("Mia")];

test("a task reads as its hands: the ones done, the one holding it now", () => {
  const relay = relayOf(task({ status: "running", handoffs: [hand("Rex", "delivered"), hand("Mia", null)] }), run, team);
  assert.deepEqual(
    relay.legs.map((l) => `${l.name}:${l.glyph}:${l.state}`),
    ["Rex:✓:done", "Mia:●:current"],
  );
  assert.equal(relay.needsYou, false);
  assert.equal(relayText(relay), "Rex ✓ → Mia");
});

test("an open question puts the person at the end of the chain", () => {
  const relay = relayOf(
    task({ status: "waiting_answer", question: "旧会话要迁吗？", handoffs: [hand("Rex", "delivered"), hand("Mia", null)] }),
    run,
    team,
  );
  assert.deepEqual(relay.legs.map((l) => l.state), ["done", "next", "waiting"]);
  assert.equal(relay.legs[2].name, "你");
  assert.equal(relay.needsYou, true);
  assert.equal(relayText(relay), "Rex ✓ → Mia → 你");
});

test("a delivered task goes back to its issuer for the next hand", () => {
  const relay = relayOf(task({ status: "passed", handoffs: [hand("Rex", null)] }), run, team);
  assert.deepEqual(relay.legs.map((l) => `${l.name}:${l.glyph}:${l.state}`), ["Rex:✓:done", "Ada:○:next"]);
});

test("a task nobody has picked up shows the teammate it is planned for", () => {
  const relay = relayOf(task({ status: "pending", worker_template: "id-Rex" }), run, team);
  assert.deepEqual(relay.legs.map((l) => `${l.name}:${l.state}`), ["Rex:next"]);
});

test("a merged task ends on ✓, a failed one on ✕", () => {
  assert.deepEqual(relayOf(task({ status: "merged", handoffs: [hand("Rex", null)] }), run, team).legs.map((l) => l.state), ["done"]);
  assert.deepEqual(relayOf(task({ status: "failed", handoffs: [hand("Rex", null)] }), run, team).legs.map((l) => l.glyph), ["✕"]);
});

test("a running task asks nothing of the person", () => {
  assert.equal(awaitsAnswer(task({ status: "running" })), false);
  assert.equal(awaitsAnswer(task({ status: "running", question: "?" })), true);
});

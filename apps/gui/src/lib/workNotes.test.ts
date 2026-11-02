import assert from "node:assert/strict";
import { test } from "node:test";
import { workDecisions } from "./workNotes.ts";

const card = (over: Record<string, unknown>) =>
  ({
    id: "c",
    kind: "question",
    from: "chat",
    run_id: null,
    task_id: null,
    text: "Add notes",
    detail: null,
    options: ["开始", "不用"],
    default: null,
    timeout_at: null,
    status: "open",
    answer: null,
    answered_at: null,
    action: "work",
    created_at: "2026-10-31T10:00:00Z",
    ...over,
  }) as never;

test("a started Work links its run; a declined or closed one has none", () => {
  const started = card({ id: "a", status: "answered", answer: "开始", run_id: "r1", answered_at: "2026-10-31T10:05:00Z" });
  const declined = card({ id: "b", status: "answered", answer: "不用", answered_at: "2026-10-31T10:03:00Z" });
  const closed = card({ id: "c", status: "dismissed", answered_at: "2026-10-31T10:04:00Z" });
  assert.deepEqual(
    workDecisions([started, declined, closed], "chat").map((d) => [d.id, d.run_id]),
    [["b", null], ["c", null], ["a", "r1"]],
  );
});

test("open cards, other chats' cards and other kinds of question are left out", () => {
  const items = [
    card({ id: "open" }),
    card({ id: "other", from: "elsewhere", status: "answered", run_id: "r" }),
    card({ id: "ask", action: null, status: "answered" }),
  ];
  assert.deepEqual(workDecisions(items, "chat"), []);
});

test("a card closed before decisions were timed still gets a place in the timeline", () => {
  const old = card({ status: "dismissed", answered_at: null });
  assert.equal(workDecisions([old], "chat")[0].at, "2026-10-31T10:00:00Z");
});

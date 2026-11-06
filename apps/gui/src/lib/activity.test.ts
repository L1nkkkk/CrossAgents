import assert from "node:assert/strict";
import { test } from "node:test";
// activity.ts reads the event stream (hub, react), so the decision it asks for lives in
// lib/delivery.ts, pure; this file keeps the regression about the event-level outcome.
import { turnNeedsAttention } from "./delivery.ts";

// The end of a turn decides whether the person is told about it (docs/tasks/DELIVERY-AUDIT.md
// §5, §8 P2-7). Locking this: a turn the person cancelled is their own doing and must stay
// quiet — no unread mark, and there is no notice path for it at all; a turn the hub lost
// (`interrupted`) still needs someone to decide.

test("a turn the person cancelled does not call for attention", () => {
  assert.equal(turnNeedsAttention("cancelled"), false);
});

test("a turn that failed or was lost still does", () => {
  assert.equal(turnNeedsAttention("error"), true);
  assert.equal(turnNeedsAttention("interrupted"), true);
});

test("a turn that ended normally, or one still running, does not", () => {
  assert.equal(turnNeedsAttention("end_turn"), false);
  assert.equal(turnNeedsAttention("max_tokens"), false);
  assert.equal(turnNeedsAttention("max_turn_requests"), false);
  assert.equal(turnNeedsAttention("refusal"), false);
  assert.equal(turnNeedsAttention(null), false);
  assert.equal(turnNeedsAttention(undefined), false);
});

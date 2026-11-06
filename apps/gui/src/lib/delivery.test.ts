import assert from "node:assert/strict";
import { test } from "node:test";
import type { QueuedReason } from "@bindings/QueuedReason";
import { QUEUED_REASONS, deliveryBusy, outcomeText, queueHeading, reasonText } from "./delivery.ts";

// The hub adds variants, it never renames one (crates/core/src/event.rs `QueuedReason`). This
// list is written out on purpose: a new variant makes the mapping test below fail until it has
// its own sentence, which is the point of the table.
const ALL: QueuedReason[] = [
  "turn_running",
  "owner_queue_ahead",
  "external_agent",
  "runtime_no_steer",
  "no_active_turn",
  "steer_write_failed",
  "outcome_unknown",
  "archived",
  "already_started",
  "attachment_refused",
  "empty",
];

test("every queued reason has a sentence of its own", () => {
  assert.deepEqual([...QUEUED_REASONS].sort(), [...ALL].sort());
  const seen = new Set<string>();
  for (const code of ALL) {
    const text = reasonText(code);
    assert.notEqual(text, code, `${code} has no sentence`);
    assert.ok(text.trim().length > 0, `${code} has an empty sentence`);
    assert.ok(!seen.has(text), `${code} copies another reason's sentence: ${text}`);
    seen.add(text);
  }
});

test("a reason code this build does not know is shown as it came, not as a refusal", () => {
  const raw = "queue_paused_by_the_future";
  assert.equal(reasonText(raw), raw);
  assert.equal(reasonText(""), "");
  // Never pretend an unknown code means the message was refused.
  assert.doesNotMatch(reasonText(raw), /拒绝|拒收/);
});

test("a request with no hub answer says whether it may have arrived", () => {
  assert.match(outcomeText("unreachable"), /没发出去/);
  assert.match(outcomeText("outcome_unknown"), /不确定发没发出去/);
  assert.match(outcomeText("outcome_unknown"), /先别重发/);
  // And it says a retry cannot duplicate the message.
  assert.match(outcomeText("outcome_unknown"), /不会重复/);
});

test("busy is the hub's word for it: anything but idle", () => {
  assert.equal(deliveryBusy("idle"), false);
  assert.equal(deliveryBusy(undefined), false);
  assert.equal(deliveryBusy("turn"), true);
  assert.equal(deliveryBusy("starting"), true);
  assert.equal(deliveryBusy("held"), true);
});

test("the queue heading says what waiting means here", () => {
  assert.match(queueHeading("turn", "Nova"), /本轮结束后/);
  assert.match(queueHeading("starting", "Nova"), /启动好/);
  assert.match(queueHeading("held", "Nova"), /自己来取/);
  // No turn runs: the old "本轮结束后" line would be a lie.
  assert.doesNotMatch(queueHeading(undefined, "Nova"), /本轮结束后/);
});

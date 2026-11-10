import assert from "node:assert/strict";
import { test } from "node:test";
import { AGENT_HOLD_MS, holdAfterRequest, holdLapsed, type Hold } from "./browserControl.ts";

test("an agent's bar stays for 15 seconds from its last request", () => {
  const held = holdAfterRequest(null, "Nova", 1_000);
  assert.deepEqual(held, { kind: "agent", agent: "Nova", until: 1_000 + AGENT_HOLD_MS });
  assert.equal(holdLapsed(held, 1_000), false);
  assert.equal(holdLapsed(held, 1_000 + AGENT_HOLD_MS - 1), false);
  assert.equal(holdLapsed(held, 1_000 + AGENT_HOLD_MS), true);
});

test("a second request from the same agent pushes the deadline out", () => {
  const first = holdAfterRequest(null, "Nova", 0);
  const second = holdAfterRequest(first, "Nova", 10_000);
  assert.deepEqual(second, { kind: "agent", agent: "Nova", until: 10_000 + AGENT_HOLD_MS });
  // The first deadline has come and gone, and the bar is still up.
  assert.equal(holdLapsed(second, 15_000), false);
  assert.equal(holdLapsed(second, AGENT_HOLD_MS * 2), true);
});

test("a person who took the tab over keeps it, and its bar never lapses", () => {
  const human: Hold = { kind: "human" };
  assert.deepEqual(holdAfterRequest(human, "Nova", 5_000), human);
  assert.equal(holdLapsed(human, 10_000_000), false);
  assert.equal(holdLapsed(null, 10_000_000), false);
});

test("another agent may hold the tab after the first one's bar went", () => {
  const nova = holdAfterRequest(null, "Nova", 0);
  const rex = holdAfterRequest(nova, "Rex", AGENT_HOLD_MS);
  assert.deepEqual(rex, { kind: "agent", agent: "Rex", until: AGENT_HOLD_MS * 2 });
});

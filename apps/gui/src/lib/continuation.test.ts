import assert from "node:assert/strict";
import { test } from "node:test";
import type { Run } from "@bindings/Run";
import type { RunPolicy } from "@bindings/RunPolicy";
import { DEFAULT_POLICY } from "./policy.ts";
import { t } from "./i18n.ts";
import {
  DEFAULT_INTERVAL_SECONDS,
  DEFAULT_MAX_ATTEMPTS,
  canReset,
  draftOf,
  draftProblems,
  failureText,
  isExhausted,
  MAX_STUCK_MINUTES,
  maxAttemptsOf,
  policyBody,
  resetBody,
  stuckOf,
  wholeInRange,
} from "./continuation.ts";

/** A run with only what these functions read: its policy and its continuation state. */
function runWith(policy: Partial<RunPolicy>, continuation: Run["continuation"] = null): Run {
  return {
    continuation,
    config: { policy: { ...DEFAULT_POLICY, ...policy } },
  } as unknown as Run;
}

const state = (phase: string, attempts = 1): Run["continuation"] =>
  ({ owner: "lead", phase, next_step: "look at it", attempts, next_at: null, last_attempt_at: null, delivery_error: null }) as Run["continuation"];

test("the form starts from the run's policy, or the hub's defaults when it has none", () => {
  assert.deepEqual(draftOf(runWith({})), { enabled: false, maxAttempts: "3", intervalSeconds: "60", stuckMinutes: "" });
  assert.deepEqual(
    draftOf(runWith({ continuation: { max_attempts: 5, interval_seconds: 120, stuck_minutes: null } })),
    { enabled: true, maxAttempts: "5", intervalSeconds: "120", stuckMinutes: "" },
  );
  assert.equal(DEFAULT_MAX_ATTEMPTS, 3);
  assert.equal(DEFAULT_INTERVAL_SECONDS, 60);
});

test("whole numbers inside the bounds only", () => {
  assert.equal(wholeInRange("3", 1, 100), 3);
  assert.equal(wholeInRange(" 7 ", 1, 100), 7);
  assert.equal(wholeInRange("", 1, 100), null);
  assert.equal(wholeInRange("0", 1, 100), null);
  assert.equal(wholeInRange("101", 1, 100), null);
  assert.equal(wholeInRange("2.5", 1, 100), null);
  assert.equal(wholeInRange("x", 1, 100), null);
  assert.equal(maxAttemptsOf({ enabled: true, maxAttempts: "100", intervalSeconds: "60", stuckMinutes: "" }), 100);
});

test("saving on sends the limits and does not reset the count", () => {
  assert.deepEqual(policyBody({ enabled: true, maxAttempts: "5", intervalSeconds: "120", stuckMinutes: "" }), {
    // An empty stuck field is a choice, so it is stated: the hub keeps what it is not told.
    continuation: { enabled: true, max_attempts: 5, interval_seconds: 120, stuck_minutes: null },
    reset_attempts: false,
  });
  assert.deepEqual(policyBody({ enabled: true, maxAttempts: "5", intervalSeconds: "120", stuckMinutes: "30" }), {
    continuation: { enabled: true, max_attempts: 5, interval_seconds: 120, stuck_minutes: 30 },
    reset_attempts: false,
  });
});

test("the stuck report: empty is off, a bad number is a mistake that is named", () => {
  const draft = (stuckMinutes: string) => ({ enabled: true, maxAttempts: "3", intervalSeconds: "60", stuckMinutes });
  assert.equal(stuckOf(draft("")), null, "empty means no report");
  assert.equal(stuckOf(draft("  ")), null);
  assert.equal(stuckOf(draft("30")), 30);
  assert.equal(stuckOf(draft(String(MAX_STUCK_MINUTES))), MAX_STUCK_MINUTES);
  assert.equal(stuckOf(draft("0")), undefined);
  assert.equal(stuckOf(draft(String(MAX_STUCK_MINUTES + 1))), undefined);
  assert.equal(stuckOf(draft("1.5")), undefined);
  assert.deepEqual(draftProblems(draft("")), [], "off is not a problem");
  assert.deepEqual(draftProblems(draft("x")), ["stuckMinutes"]);
  assert.equal(policyBody(draft("x")), null);
});

test("saving off sends only the switch", () => {
  // The hub drops the policy and the state on `enabled: false`; limits would be meaningless,
  // and unreadable ones must not block turning it off.
  assert.deepEqual(policyBody({ enabled: false, maxAttempts: "", intervalSeconds: "", stuckMinutes: "" }), {
    continuation: { enabled: false },
    reset_attempts: false,
  });
});

test("a limit that is not a whole number in range is refused, and named", () => {
  assert.equal(policyBody({ enabled: true, maxAttempts: "0", intervalSeconds: "120", stuckMinutes: "" }), null);
  assert.equal(policyBody({ enabled: true, maxAttempts: "3", intervalSeconds: "0", stuckMinutes: "" }), null);
  assert.deepEqual(draftProblems({ enabled: true, maxAttempts: "0", intervalSeconds: "x", stuckMinutes: "" }), ["maxAttempts", "intervalSeconds"]);
  assert.deepEqual(draftProblems({ enabled: true, maxAttempts: "3", intervalSeconds: "60", stuckMinutes: "" }), []);
  assert.deepEqual(draftProblems({ enabled: false, maxAttempts: "0", intervalSeconds: "x", stuckMinutes: "" }), []);
});

test("re-arming sends only the reset, leaving the policy as it is", () => {
  const body = resetBody();
  assert.deepEqual(body, { reset_attempts: true });
  assert.equal(body.continuation, undefined);
});

test("a run with no continuation state has no allowance to hand out", () => {
  assert.equal(canReset(runWith({})), false);
  assert.equal(canReset(runWith({ continuation: { max_attempts: 3, interval_seconds: 60, stuck_minutes: null } })), false);
  assert.equal(canReset(runWith({}, state("working"))), true);
  assert.equal(isExhausted(runWith({}, state("exhausted", 3))), true);
  assert.equal(isExhausted(runWith({}, state("working"))), false);
  assert.equal(isExhausted(runWith({})), false);
});

test("a refusal says why, and the hub's words are kept", () => {
  const message = "this credential has the read level; POST /v1/runs/{key}/policy needs write";
  const shown = failureText(403, message);
  assert.ok(shown.startsWith(message));
  assert.ok(shown.includes(t("continuation.forbidden")));
  // Other failures are shown as the hub sent them.
  assert.equal(failureText(400, "say what to change"), "say what to change");
  assert.equal(failureText(0, "unreachable"), "unreachable");
});

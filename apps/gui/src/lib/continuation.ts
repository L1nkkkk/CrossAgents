// The person's hand on how a run that is already going continues by itself (docs/CONTINUATION.md):
// the form's values as typed, and the request bodies they add up to. Pure, so the shapes the
// hub expects are tested without a page (continuation.test.ts).

import type { Run } from "@bindings/Run";
import type { UpdateRunPolicyRequest } from "@bindings/UpdateRunPolicyRequest";
import { t } from "./i18n.ts";

/** What the hub uses when a run has no continuation policy yet (ContinuationUpdate's defaults). */
export const DEFAULT_MAX_ATTEMPTS = 3;
export const DEFAULT_INTERVAL_SECONDS = 60;
/** The same bounds the form on the home page offers. */
export const MAX_ATTEMPTS_LIMIT = 100;
export const MAX_INTERVAL_SECONDS = 86400;

export type ContinuationDraft = {
  /** Whether the hub wakes the lead when nobody is working on the run. */
  enabled: boolean;
  /** A count, as typed; `""` until the person writes one. */
  maxAttempts: string;
  /** Seconds between wakeups, as typed. */
  intervalSeconds: string;
};

export type ContinuationField = "maxAttempts" | "intervalSeconds";

/** The form's starting values: the run's policy, or the hub's defaults when it has none. */
export function draftOf(run: Run): ContinuationDraft {
  const policy = run.config.policy.continuation;
  return {
    enabled: !!policy,
    maxAttempts: String(policy?.max_attempts ?? DEFAULT_MAX_ATTEMPTS),
    intervalSeconds: String(policy?.interval_seconds ?? DEFAULT_INTERVAL_SECONDS),
  };
}

/** A whole number in `text` within [min, max], else null. */
export function wholeInRange(text: string, min: number, max: number): number | null {
  const value = Number(text.trim());
  return Number.isInteger(value) && value >= min && value <= max ? value : null;
}

export const maxAttemptsOf = (draft: ContinuationDraft) => wholeInRange(draft.maxAttempts, 1, MAX_ATTEMPTS_LIMIT);
export const intervalOf = (draft: ContinuationDraft) => wholeInRange(draft.intervalSeconds, 1, MAX_INTERVAL_SECONDS);

/** Which limits need fixing before the draft can be saved; [] when it can (off needs no numbers). */
export function draftProblems(draft: ContinuationDraft): ContinuationField[] {
  if (!draft.enabled) return [];
  const problems: ContinuationField[] = [];
  if (maxAttemptsOf(draft) === null) problems.push("maxAttempts");
  if (intervalOf(draft) === null) problems.push("intervalSeconds");
  return problems;
}

/**
 * The body that saves the draft: on sends both limits (the hub keeps any it is not
 * mentioned with, but both are on screen, so both are meant); off sends only the switch,
 * which also forgets the run's continuation state.
 */
export function policyBody(draft: ContinuationDraft): UpdateRunPolicyRequest | null {
  if (!draft.enabled) return { continuation: { enabled: false }, reset_attempts: false };
  const max_attempts = maxAttemptsOf(draft);
  const interval_seconds = intervalOf(draft);
  if (max_attempts === null || interval_seconds === null) return null;
  return { continuation: { enabled: true, max_attempts, interval_seconds }, reset_attempts: false };
}

/** The body that only hands the allowance out again: attempts to zero, policy untouched. */
export function resetBody(): UpdateRunPolicyRequest {
  return { reset_attempts: true };
}

/** Whether there is an allowance to hand out again: a run whose continuation was turned off has none. */
export const canReset = (run: Run) => !!run.continuation;

/** Whether the run's wakeups ran out — the state the person opens the page for. */
export const isExhausted = (run: Run) => run.continuation?.phase === "exhausted";

/** What to show when a change is refused: the hub's words, and the level it asked for on a 403. */
export function failureText(status: number, message: string): string {
  return status === 403 ? `${message} ${t("continuation.forbidden")}` : message;
}

// 接力链 (docs/research/magpie-lancer.md §2.4): one line per task — the hands it passed
// through, who holds it now, and who takes it next: `Rex ✓ → Ada → 你`. Where it waits on the
// person, the last leg says so. Pure, so it is tested (relay.test.ts); relay.tsx draws it.

import type { Agent } from "@bindings/Agent";
import type { HandoffOutcome } from "@bindings/HandoffOutcome";
import type { Run } from "@bindings/Run";
import type { Task } from "@bindings/Task";

export type LegState = "done" | "failed" | "current" | "next" | "waiting";

export type RelayLeg = {
  /** A teammate's name as its handoff card gave it, or 你 for the person. */
  name: string;
  /** The teammate, when it is still in the workspace: a clone is gone once its task ended. */
  agent?: Agent;
  state: LegState;
  glyph: "✓" | "✕" | "↩" | "●" | "○";
};

export type Relay = {
  legs: RelayLeg[];
  /** The person is the next hand: a worker asked something, or its question is open. */
  needsYou: boolean;
};

/** The statuses in which a task moves with the hand that holds it: no one is next yet. */
const MOVING = new Set(["running", "verifying", "retrying", "reviewing", "escalated"]);

/** How a finished hand reads: delivered ✓, sent back or rerouted ↩, failed ✕. */
const OUTCOME: Record<HandoffOutcome, Pick<RelayLeg, "glyph" | "state">> = {
  delivered: { glyph: "✓", state: "done" },
  reworked: { glyph: "↩", state: "done" },
  reassigned: { glyph: "↩", state: "done" },
  interrupted: { glyph: "↩", state: "done" },
  failed: { glyph: "✕", state: "failed" },
  cancelled: { glyph: "○", state: "done" },
};

/** The person the task waits for: its worker asked something and no answer came yet. */
export const awaitsAnswer = (task: Task): boolean => task.status === "waiting_answer" || !!task.report?.question;

/** A teammate by id, else by the name a card carries (any case); undefined when it is gone. */
function teammate(agents: Agent[] | undefined, id: string | null | undefined, name?: string | null): Agent | undefined {
  if (!agents) return undefined;
  const byId = id ? agents.find((a) => a.id === id) : undefined;
  if (byId) return byId;
  const key = name?.toLowerCase();
  return key ? agents.find((a) => a.name.toLowerCase() === key) : undefined;
}

/**
 * The relay of one task: every hand it went through, the one that holds it now (●, or ○ while
 * nothing has started), and the next hand — 你 when it waits on the person, the issuer once the
 * worker delivered. A merged task ends on ✓; a failed one on ✕.
 */
export function relayOf(task: Task, run: Run, agents?: Agent[]): Relay {
  const legs: RelayLeg[] = [];
  const push = (name: string, agent: Agent | undefined, glyph: RelayLeg["glyph"], state: LegState) =>
    legs.push({ name, state, glyph, ...(agent ? { agent } : {}) });

  for (const h of task.handoffs) {
    // A card names the execution session it went to; the person knows the teammate it was
    // cloned from ("Rex-a" is "Rex" on this line).
    const from = h.template ? teammate(agents, null, h.template) : undefined;
    const agent = teammate(agents, h.to, h.to_name) ?? from;
    const name = from?.name ?? agent?.name ?? h.to_name;
    if (h.outcome) push(name, agent, OUTCOME[h.outcome].glyph, OUTCOME[h.outcome].state);
    else push(name, agent, MOVING.has(task.status) ? "●" : "○", "current");
  }
  if (!task.handoffs.length) {
    // Nothing handed out yet: the teammate it is planned for is the first hand.
    const planned = teammate(agents, task.assignee) ?? teammate(agents, task.worker_template) ?? teammate(agents, run.config.worker);
    push(planned?.name ?? "worker", planned, "○", task.status === "pending" ? "next" : "current");
  }

  const last = legs[legs.length - 1];
  let needsYou = false;
  if (awaitsAnswer(task)) {
    // The worker asked: the baton is with the person now, not with the worker.
    if (last?.state === "current") {
      last.state = "next";
      last.glyph = "○";
    }
    push("你", undefined, "○", "waiting");
    needsYou = true;
  } else if (MOVING.has(task.status) || task.status === "pending") {
    // It moves with the hand that holds it; nobody is next yet.
  } else if (task.status === "passed" || task.status === "stalled") {
    // The worker is done with it: the issuer takes it next, and mostly the person behind it.
    if (last?.state === "current") {
      last.state = "done";
      last.glyph = "✓";
    }
    const issuer = teammate(agents, task.issuer) ?? teammate(agents, run.planner);
    push(issuer?.name ?? "主导", issuer, "○", "next");
  } else if (task.status === "merged") {
    if (last) {
      last.state = "done";
      last.glyph = "✓";
    }
  } else if (task.status === "failed" || task.status === "cancelled") {
    if (last) {
      last.state = "failed";
      last.glyph = task.status === "failed" ? "✕" : "○";
    }
  }
  return { legs, needsYou };
}

/** The chain in words, for a title or an aria-label: `Rex ✓ → Ada → 你`. */
export function relayText(relay: Relay): string {
  return relay.legs.map((leg) => (leg.state === "done" || leg.state === "failed" ? `${leg.name} ${leg.glyph}` : leg.name)).join(" → ");
}

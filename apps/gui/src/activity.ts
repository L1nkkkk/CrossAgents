// What each agent is doing and what the human has not seen yet, from the live event stream.
//
// - Latest activity: the newest tool step (`$ cmd`, `✎ file`) or the start of the newest output,
//   shown on sidebar rows while the agent works. Only known for what happened since the page
//   loaded.
// - The running turn (a `Pulse`): when it started, when it last produced something, and its
//   newest items — the sidebar row says how long it has run and what it does now.
// - Unread: a turn that answered *your* message (or ended in an error) while you were not looking
//   at that agent. Turns started by the orchestrator do not count, or every worker step would.
//   Kept in localStorage; opening the agent clears it.
// - Viewing: the agents open in a pane right now (one, or two side by side).

import { useSyncExternalStore } from "react";
import type { AgentExit } from "@bindings/AgentExit";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import { onHubEvent } from "./hub";
import { stepLine } from "./lib/workGroups";

export type Unread = { kind: "done" | "error"; at: string };

/**
 * The turn an agent is in, as the sidebar can see it without its transcript: when the turn
 * started, when it last produced something, and its newest items (enough for `liveActivity`).
 * A turn that began before this page loaded has no `since` — the sidebar then asks the
 * transcript once (liveTurn.tsx `usePulseSeeds`), or shows only what it saw itself.
 */
export type Pulse = {
  /** Turn start, ms; absent when this page did not see it start. */
  since?: number;
  /** When it last produced something, ms; quiet counts from here (and only this page's time counts). */
  at?: number;
  /** The newest items in order, so the row says what it does now. */
  items: TranscriptItem[];
  /** The turn ended: nothing live to show on its row. */
  over: boolean;
};

const UNREAD_KEY = "crossagents.unread";
const listeners = new Set<() => void>();
const activity = new Map<string, string>();
/** How many items a pulse keeps: `liveActivity` reads the last one, a little history is enough. */
const PULSE_ITEMS = 6;
const pulses = new Map<string, Pulse>();
/** Each agent's latest `agent.error` since the page loaded. */
const errors = new Map<string, string>();
/** How its process ended, while that is its latest error (`agent.exited`). */
const exits = new Map<string, AgentExit & { message: string }>();
const fromHuman = new Set<string>(); // message ids of messages a person sent
let unread: Record<string, Unread> = load();
let viewing: string[] = [];
let started = false;

function load(): Record<string, Unread> {
  try {
    return JSON.parse(localStorage.getItem(UNREAD_KEY) ?? "{}") ?? {};
  } catch {
    return {};
  }
}

function save() {
  try {
    localStorage.setItem(UNREAD_KEY, JSON.stringify(unread));
  } catch {
    // Storage may be unavailable; unread marks then last for this page only.
  }
}

function changed() {
  listeners.forEach((l) => l());
}

function firstLine(text: string): string {
  const line = text.trim().split("\n").find((l) => l.trim()) ?? "";
  return line.length > 80 ? line.slice(0, 80) + "…" : line;
}

/** The pulse so far, now at `ts` and moving again; a new turn keeps only its start. */
function moved(id: string, ts: number): Pulse {
  const p = pulses.get(id);
  return { ...(p?.since === undefined ? {} : { since: p.since }), at: ts, items: p?.items ?? [], over: false };
}

function push(id: string, ts: number, item: TranscriptItem) {
  const p = moved(id, ts);
  pulses.set(id, { ...p, items: [...p.items, item].slice(-PULSE_ITEMS) });
}

type Call = Extract<TranscriptItem, { type: "tool_call" }>;

/** The newest call with `toolCallId`, replaced by `change`; the pulse still moves either way. */
function patchCall(id: string, ts: number, toolCallId: string, change: (call: Call) => Call) {
  const p = moved(id, ts);
  const items = p.items.slice();
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.type === "tool_call" && item.tool_call_id === toolCallId) {
      items[i] = change(item);
      break;
    }
  }
  pulses.set(id, { ...p, items });
}

/** Starts following the event stream; idempotent. */
export function startActivity() {
  if (started) return;
  started = true;
  onHubEvent((e) => {
    switch (e.type) {
      case "agent.input":
        if (!e.data.from || e.data.from.kind === "human") fromHuman.add(e.data.message_id);
        return;
      case "agent.tool_call":
        activity.set(e.data.id, stepLine({ type: "tool_call", ...e.data }));
        push(e.data.id, Date.parse(e.ts), { type: "tool_call", tool_call_id: e.data.tool_call_id, title: e.data.title, kind: e.data.kind, status: e.data.status });
        return changed();
      case "agent.tool_call_updated":
        patchCall(e.data.id, Date.parse(e.ts), e.data.tool_call_id, (call) => ({
          ...call,
          title: e.data.title ?? call.title,
          status: e.data.status ?? call.status,
        }));
        return changed();
      case "agent.tool_output":
        patchCall(e.data.id, Date.parse(e.ts), e.data.tool_call_id, (call) => ({
          ...call,
          output: `${call.output ?? ""}${e.data.text}`.slice(-4000),
        }));
        return changed();
      case "agent.output": {
        const id = e.data.id;
        const ts = Date.parse(e.ts);
        if (e.data.stream === "text" && e.data.text.trim()) activity.set(id, firstLine(e.data.text));
        // Consecutive output of one stream is one item, as the transcript keeps it.
        const p = moved(id, ts);
        const last = p.items[p.items.length - 1];
        const items =
          last?.type === "output" && last.stream === e.data.stream
            ? [...p.items.slice(0, -1), { ...last, text: last.text + e.data.text }]
            : [...p.items, { type: "output" as const, stream: e.data.stream, text: e.data.text }];
        pulses.set(id, { ...p, items: items.slice(-PULSE_ITEMS) });
        return changed();
      }
      case "agent.permission":
        push(e.data.id, Date.parse(e.ts), { type: "permission", kind: e.data.kind, title: e.data.title, decision: e.data.decision });
        return changed();
      case "agent.warning":
        push(e.data.id, Date.parse(e.ts), { type: "warning", message: e.data.message });
        return changed();
      case "agent.turn_started":
        pulses.set(e.data.id, { since: Date.parse(e.ts), at: Date.parse(e.ts), items: [], over: false });
        return changed();
      case "agent.status_changed": {
        const id = e.data.id;
        const p = pulses.get(id);
        if (e.data.to === "running" && (!p || p.over)) {
          pulses.set(id, { since: Date.parse(e.ts), at: Date.parse(e.ts), items: [], over: false });
          return changed();
        }
        // Idle, stopped or in error: the row has nothing live left to say.
        if (e.data.to !== "running" && p && !p.over) {
          pulses.set(id, { ...p, over: true });
          return changed();
        }
        return;
      }
      case "agent.turn_completed": {
        const { id, message_id, stop_reason } = e.data;
        const p = pulses.get(id);
        if (p && !p.over) pulses.set(id, { ...p, over: true });
        const mine = fromHuman.delete(message_id);
        // An interrupted turn needs someone to decide whether to run it again.
        const bad = stop_reason === "error" || stop_reason === "interrupted";
        if (viewing.includes(id) || (!mine && !bad)) return changed();
        unread = { ...unread, [id]: { kind: bad ? "error" : "done", at: e.ts } };
        save();
        return changed();
      }
      case "agent.exited":
        // The `agent.error` that follows carries the same message.
        exits.set(e.data.id, { cause: e.data.cause, code: e.data.code, message: e.data.message });
        return;
      case "agent.error":
        errors.set(e.data.id, e.data.message);
        push(e.data.id, Date.parse(e.ts), { type: "error", message: e.data.message });
        if (exits.get(e.data.id)?.message !== e.data.message) exits.delete(e.data.id);
        return changed();
      case "agent.removed":
        activity.delete(e.data.id);
        pulses.delete(e.data.id);
        if (unread[e.data.id]) markSeen(e.data.id);
        return;
    }
  });
}

export function markSeen(agentId: string) {
  if (!unread[agentId]) return;
  const { [agentId]: _, ...rest } = unread;
  unread = rest;
  save();
  changed();
}

/** Registers an open pane's agent; returns the cleanup. */
export function view(agentId: string): () => void {
  viewing = [...viewing, agentId];
  markSeen(agentId);
  changed();
  return () => {
    const i = viewing.indexOf(agentId);
    if (i >= 0) viewing = [...viewing.slice(0, i), ...viewing.slice(i + 1)];
    changed();
  };
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

export const useActivity = (agentId: string) => useSyncExternalStore(subscribe, () => activity.get(agentId));
/** The agent's running turn as this page has seen it (see `Pulse`). */
export const usePulse = (agentId: string) => useSyncExternalStore(subscribe, () => pulses.get(agentId));
/** Whether this page knows a turn's start, so a row can show its elapsed time. */
export const pulseKnown = (agentId: string) => {
  const p = pulses.get(agentId);
  return !!p && !p.over && p.since !== undefined;
};

/**
 * What an open session's transcript says about its running turn, for the sidebar: the turn's
 * start and the items they share. A pulse the stream has already moved keeps its quiet clock
 * (only time on this page counts); otherwise quiet starts now. A pulse the stream already knows
 * the start of is left alone.
 */
export function noteTurn(agentId: string, since: number, items: TranscriptItem[]) {
  const p = pulses.get(agentId);
  if (p && (p.over || p.since !== undefined)) return;
  pulses.set(agentId, { since, at: p?.at ?? Date.now(), items: items.slice(-PULSE_ITEMS), over: false });
  changed();
}

/** The agent's latest error seen on the stream, if any. */
export const lastError = (agentId: string) => errors.get(agentId);
/** How the agent's process ended, when that is its latest error on the stream. */
export const lastExit = (agentId: string): AgentExit | undefined => exits.get(agentId);
export const useUnread = () => useSyncExternalStore(subscribe, () => unread);
export const useViewing = () => useSyncExternalStore(subscribe, () => viewing);

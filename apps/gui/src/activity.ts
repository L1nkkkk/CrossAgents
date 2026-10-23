// What each agent is doing and what the human has not seen yet, from the live event stream.
//
// - Latest activity: the newest tool step (`$ cmd`, `✎ file`) or the start of the newest output,
//   shown on sidebar rows while the agent works. Only known for what happened since the page
//   loaded.
// - Unread: a turn that answered *your* message (or ended in an error) while you were not looking
//   at that agent. Turns started by the orchestrator do not count, or every worker step would.
//   Kept in localStorage; opening the agent clears it.
// - Viewing: the agents open in a pane right now (one, or two side by side).

import { useSyncExternalStore } from "react";
import { onHubEvent } from "./hub";
import { stepLine } from "./lib/workGroups";

export type Unread = { kind: "done" | "error"; at: string };

const UNREAD_KEY = "crossagents.unread";
const listeners = new Set<() => void>();
const activity = new Map<string, string>();
/** Each agent's latest `agent.error` since the page loaded. */
const errors = new Map<string, string>();
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
        return changed();
      case "agent.output":
        if (e.data.stream === "text" && e.data.text.trim()) {
          activity.set(e.data.id, firstLine(e.data.text));
          changed();
        }
        return;
      case "agent.turn_completed": {
        const { id, message_id, stop_reason } = e.data;
        const mine = fromHuman.delete(message_id);
        // An interrupted turn needs someone to decide whether to run it again.
        const bad = stop_reason === "error" || stop_reason === "interrupted";
        if (viewing.includes(id) || (!mine && !bad)) return;
        unread = { ...unread, [id]: { kind: bad ? "error" : "done", at: e.ts } };
        save();
        return changed();
      }
      case "agent.error":
        errors.set(e.data.id, e.data.message);
        return changed();
      case "agent.removed":
        activity.delete(e.data.id);
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
/** The agent's latest error seen on the stream, if any. */
export const lastError = (agentId: string) => errors.get(agentId);
export const useUnread = () => useSyncExternalStore(subscribe, () => unread);
export const useViewing = () => useSyncExternalStore(subscribe, () => viewing);

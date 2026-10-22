// The raw hub events of each agent since the page loaded, for the 事件流 pane (debugging).
// Kept per agent, newest last, at most `KEEP` each.

import { useSyncExternalStore } from "react";
import { onHubEvent, type HubEvent } from "./hub";

const KEEP = 1000;
const byAgent = new Map<string, HubEvent[]>();
const listeners = new Set<() => void>();
let started = false;

/** The agents an event is about: its own id, a task's assignee, a question's asker. */
export function agentsOf(e: HubEvent): string[] {
  const data = (e as { data?: Record<string, unknown> }).data;
  if (!data) return [];
  if (e.type.startsWith("agent.") && typeof data.id === "string") return [data.id];
  const ids: string[] = [];
  for (const key of ["assignee", "author", "owner", "by"]) if (typeof data[key] === "string") ids.push(data[key] as string);
  const item = data.item as { from?: unknown } | undefined;
  if (item && typeof item.from === "string") ids.push(item.from);
  return ids;
}

export function startEventLog() {
  if (started) return;
  started = true;
  onHubEvent((e) => {
    const ids = agentsOf(e);
    if (!ids.length) return;
    for (const id of ids) {
      const list = byAgent.get(id) ?? [];
      const next = list.length >= KEEP ? [...list.slice(list.length - KEEP + 1), e] : [...list, e];
      byAgent.set(id, next);
    }
    listeners.forEach((l) => l());
  });
}

const EMPTY: HubEvent[] = [];

export const useEventLog = (agentId: string | undefined) =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => (agentId ? (byAgent.get(agentId) ?? EMPTY) : EMPTY),
  );

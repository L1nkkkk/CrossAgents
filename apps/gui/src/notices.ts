// What a session lost on the way back (docs/tasks/S1.md §7 第 15 条): the hub says so with an
// `agent.warning` — the conversation came back but its context did not, this is a new session,
// the code the run left was not restored. Folded into a turn like any warning, it would sit
// under an old message, or nowhere when no turn has started; so these are kept here too and
// shown at the top of the session until the person closes them.
//
// Collected from the moment the page loads, wherever it is: restoring from the run page and
// opening the session afterwards still shows them. Kept in localStorage across restarts.

import { useSyncExternalStore } from "react";
import { onHubEvent } from "./hub";

/**
 * How the hub starts each of them; the words are the hub's own, matched here, never
 * rewritten: crates/hub/src/runtime.rs (`archive` / `start`), crates/orchestrator/src/landing.rs
 * (`lead_returned`).
 */
export const CONTINUITY_WARNINGS = ["对话记录恢复了，但", "这是新会话，没有带上之前的上下文", "对话保留了，但原执行代码没有恢复"];

export function isContinuityWarning(message: string): boolean {
  return CONTINUITY_WARNINGS.some((start) => message.startsWith(start));
}

export type Notice = { id: string; message: string; at: string };

const KEY = "crossagents.notices";
/** Per session, the newest few: an old one the person never closed does not pile up forever. */
const KEEP = 5;
const NONE: Notice[] = [];

let notices: Record<string, Notice[]> = load();
const listeners = new Set<() => void>();

function load(): Record<string, Notice[]> {
  try {
    const raw = typeof localStorage === "undefined" ? null : localStorage.getItem(KEY);
    return raw ? (JSON.parse(raw) as Record<string, Notice[]>) : {};
  } catch {
    return {};
  }
}

function save() {
  try {
    localStorage.setItem(KEY, JSON.stringify(notices));
  } catch {
    // Not kept across restarts, still shown now.
  }
  listeners.forEach((l) => l());
}

onHubEvent((event) => {
  if (event.type !== "agent.warning" || !isContinuityWarning(event.data.message)) return;
  const agent = event.data.id;
  const mine = notices[agent] ?? NONE;
  // A reconnect replays events: the same one only once.
  if (mine.some((n) => n.id === event.id)) return;
  notices = { ...notices, [agent]: [...mine, { id: event.id, message: event.data.message, at: event.ts }].slice(-KEEP) };
  save();
});

/** The session's notices, oldest first. */
export function useNotices(agent: string): Notice[] {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => notices[agent] ?? NONE,
  );
}

export function dismissNotice(agent: string, id: string) {
  const left = (notices[agent] ?? NONE).filter((n) => n.id !== id);
  const next = { ...notices };
  if (left.length) next[agent] = left;
  else delete next[agent];
  notices = next;
  save();
}

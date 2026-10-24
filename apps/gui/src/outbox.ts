// Messages sent from this window, shown at once. The hub answers a message only once its
// agent is up, which takes seconds for one that was stopped; until the hub's own copy
// arrives in the transcript, the conversation shows the message from here.

import { useEffect, useSyncExternalStore } from "react";
import type { TranscriptTurn } from "@bindings/TranscriptTurn";
import { api, errorText } from "./api";

export type Outgoing = {
  key: number;
  agent: string;
  text: string;
  now?: boolean;
  /** The hub's id for it, once it took it. */
  id?: string;
  /** Why it was not sent. */
  error?: string;
};

let items: Outgoing[] = [];
let next = 0;
const listeners = new Set<() => void>();

function set(change: (xs: Outgoing[]) => Outgoing[]) {
  items = change(items);
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

async function deliver(item: Outgoing): Promise<void> {
  try {
    const sent = await api.sendMessage(item.agent, { text: item.text, now: item.now || undefined });
    set((xs) => xs.map((x) => (x.key === item.key ? { ...x, id: sent.message_id, error: undefined } : x)));
  } catch (err) {
    set((xs) => xs.map((x) => (x.key === item.key ? { ...x, error: errorText(err) } : x)));
    throw err;
  }
}

/**
 * Sends `text` to `agent`, showing it in that conversation right away. A failure stays there
 * with a retry, unless `takeBack` (the caller puts the text back where it was typed).
 */
export async function sendMessage(agent: string, text: string, opts: { now?: boolean; takeBack?: boolean } = {}): Promise<void> {
  const item: Outgoing = { key: ++next, agent, text, now: opts.now };
  set((xs) => [...xs, item]);
  try {
    await deliver(item);
  } catch (err) {
    if (opts.takeBack) dismiss(item.key);
    throw err;
  }
}

export function retry(key: number) {
  const item = items.find((x) => x.key === key);
  if (!item) return;
  set((xs) => xs.map((x) => (x.key === key ? { ...x, error: undefined } : x)));
  void deliver(item).catch(() => {});
}

export function dismiss(key: number) {
  set((xs) => xs.filter((x) => x.key !== key));
}

/** What `agent`'s conversation shows from here: messages its transcript does not have yet. */
export function useOutbox(agent: string | undefined, turns: TranscriptTurn[] | undefined): Outgoing[] {
  const all = useSyncExternalStore(subscribe, () => items);
  const arrived = (x: Outgoing) => !!x.id && !!turns?.some((t) => t.message_id === x.id);
  // Once the hub's copy is in, ours goes for good (even if that message is withdrawn later).
  useEffect(() => {
    if (all.some((x) => x.agent === agent && arrived(x))) set((xs) => xs.filter((x) => !(x.agent === agent && arrived(x))));
  });
  return all.filter((x) => x.agent === agent && !arrived(x));
}

// Two conversations side by side: the routed session on the left, one more beside it. Kept on
// this device. Ctrl/⌘-click or middle-click an agent in the sidebar to open it beside.

import { useSyncExternalStore } from "react";

type Panes = { beside: string | null; ratio: number };

const KEY = "crossagents.panes";
const clamp = (r: number) => Math.min(0.75, Math.max(0.25, r));
const listeners = new Set<() => void>();
let state: Panes = load();

function load(): Panes {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (saved && typeof saved.ratio === "number") return { beside: saved.beside ?? null, ratio: clamp(saved.ratio) };
  } catch {
    // Fall through to the default.
  }
  return { beside: null, ratio: 0.5 };
}

function set(next: Panes) {
  state = next;
  try {
    localStorage.setItem(KEY, JSON.stringify(state));
  } catch {
    // The layout then lasts for this page only.
  }
  listeners.forEach((l) => l());
}

/** Opens `name` beside the routed session; without one, just goes to it. */
export function openBeside(name: string) {
  if (!/^#\/s\//.test(window.location.hash)) {
    window.location.hash = `/s/${encodeURIComponent(name)}`;
    return;
  }
  set({ ...state, beside: name });
}

export const closeBeside = () => set({ ...state, beside: null });
export const setRatio = (ratio: number) => set({ ...state, ratio: clamp(ratio) });

export const usePanes = () =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );

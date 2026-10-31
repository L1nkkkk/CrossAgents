// The widths of the docked sidebar and tool column, dragged by the user and remembered per
// browser. The conversation between them keeps at least MIN_MAIN; whatever does not fit is
// taken from the tool column first, and when even its minimum does not fit it floats.

import { useSyncExternalStore, type PointerEvent as ReactPointerEvent } from "react";

const KEY = "crossagents.widths";
export const SIDEBAR = { min: 200, max: 420, initial: 226 };
export const TOOLS = { min: 300, max: 960, initial: 420 };
/** The narrowest the conversation gets beside docked columns, gaps between panels included. */
export const MIN_MAIN = 500;

type Widths = { sidebar: number; tools: number };

const clamp = (v: number, lo: number, hi: number) => Math.round(Math.min(hi, Math.max(lo, v)));

let widths: Widths = (() => {
  try {
    const w = JSON.parse(localStorage.getItem(KEY) ?? "null");
    return {
      sidebar: typeof w?.sidebar === "number" ? clamp(w.sidebar, SIDEBAR.min, SIDEBAR.max) : SIDEBAR.initial,
      tools: typeof w?.tools === "number" ? clamp(w.tools, TOOLS.min, TOOLS.max) : TOOLS.initial,
    };
  } catch {
    return { sidebar: SIDEBAR.initial, tools: TOOLS.initial };
  }
})();
const listeners = new Set<() => void>();

function set(next: Widths) {
  widths = next;
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // Not remembered; still applies for now.
  }
  listeners.forEach((l) => l());
}

export const setSidebarWidth = (w: number) => set({ ...widths, sidebar: clamp(w, SIDEBAR.min, SIDEBAR.max) });
/** Never wider than fits now, so a later wider window does not jump to an overshoot. */
export const setToolsWidth = (w: number, sidebar: boolean) =>
  set({ ...widths, tools: clamp(Math.min(w, room(window.innerWidth, sidebar)), TOOLS.min, TOOLS.max) });

const room = (innerWidth: number, sidebar: boolean) => innerWidth - (sidebar ? widths.sidebar : 0) - MIN_MAIN;

const subscribe = (l: () => void) => {
  listeners.add(l);
  window.addEventListener("resize", l);
  return () => {
    listeners.delete(l);
    window.removeEventListener("resize", l);
  };
};

/**
 * The widths to lay out with in a window `innerWidth` wide. `sidebar`: whether the sidebar is
 * docked. `float`: the tool column cannot dock beside a MIN_MAIN conversation. `tools` is the
 * docked width, shrunk to what fits.
 */
export function layoutWidths(innerWidth: number, sidebar: boolean) {
  const fits = room(innerWidth, sidebar);
  return { sidebar: widths.sidebar, tools: clamp(Math.min(widths.tools, fits), TOOLS.min, TOOLS.max), float: fits < TOOLS.min };
}

export function useWidths(sidebar: boolean) {
  // A string snapshot so equal layouts do not re-render.
  const key = useSyncExternalStore(subscribe, () => JSON.stringify(layoutWidths(window.innerWidth, sidebar)));
  return JSON.parse(key) as ReturnType<typeof layoutWidths>;
}

/**
 * Starts dragging a column edge: `apply` gets the column's new width. `from` is the edge's
 * side of the column, so moving right widens a sidebar ("right") and narrows a tool column
 * ("left").
 */
export function dragEdge(e: ReactPointerEvent, width: number, from: "left" | "right", apply: (w: number) => void) {
  if (e.button !== 0) return;
  e.preventDefault();
  const x0 = e.clientX;
  document.body.classList.add("col-resizing");
  const move = (ev: PointerEvent) => apply(width + (from === "right" ? ev.clientX - x0 : x0 - ev.clientX));
  const up = () => {
    document.body.classList.remove("col-resizing");
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

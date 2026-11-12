// The widths of the docked sidebar and tool column, and the height of the bottom dock, dragged
// by the user and remembered per browser. A drag leaves the conversation between them at least
// MIN_MAIN; whatever does not fit is taken from the tool column first, and when the window leaves
// the conversation less than FLOAT_MAIN beside the column's minimum, the column floats. The bottom
// dock leaves the conversation at least MIN_MAIN_HEIGHT.

import { useSyncExternalStore, type PointerEvent as ReactPointerEvent } from "react";

const KEY = "crossagents.widths";
export const SIDEBAR = { min: 200, max: 420, initial: 226 };
export const TOOLS = { min: 300, max: 2400, initial: 420 };
/** The narrowest a drag leaves the conversation beside docked columns, gaps between panels included. */
export const MIN_MAIN = 340;
/** Below this much room for the conversation, the tool column floats instead of docking. */
export const FLOAT_MAIN = 500;
export const BOTTOM = { min: 120, initial: 280 };
export const MIN_MAIN_HEIGHT = 240;

type Widths = { sidebar: number; tools: number; bottom: number };

const clamp = (v: number, lo: number, hi: number) => Math.round(Math.min(hi, Math.max(lo, v)));

let widths: Widths = (() => {
  try {
    const w = JSON.parse(localStorage.getItem(KEY) ?? "null");
    return {
      sidebar: typeof w?.sidebar === "number" ? clamp(w.sidebar, SIDEBAR.min, SIDEBAR.max) : SIDEBAR.initial,
      tools: typeof w?.tools === "number" ? clamp(w.tools, TOOLS.min, TOOLS.max) : TOOLS.initial,
      bottom: typeof w?.bottom === "number" ? Math.max(BOTTOM.min, Math.round(w.bottom)) : BOTTOM.initial,
    };
  } catch {
    return { sidebar: SIDEBAR.initial, tools: TOOLS.initial, bottom: BOTTOM.initial };
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
export const setBottomHeight = (h: number) => set({ ...widths, bottom: bottomFits(h, window.innerHeight) });

const bottomFits = (h: number, innerHeight: number) => clamp(h, BOTTOM.min, Math.max(BOTTOM.min, innerHeight - MIN_MAIN_HEIGHT));
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
 * docked. `float`: the tool column at its minimum would leave the conversation less than
 * FLOAT_MAIN. `tools` is the
 * docked width, shrunk to what fits; `bottom` the bottom dock's height, likewise.
 */
export function layoutWidths(innerWidth: number, sidebar: boolean, innerHeight: number) {
  const fits = room(innerWidth, sidebar);
  const float = fits - (FLOAT_MAIN - MIN_MAIN) < TOOLS.min;
  return { sidebar: widths.sidebar, tools: clamp(Math.min(widths.tools, fits), TOOLS.min, TOOLS.max), float, bottom: bottomFits(widths.bottom, innerHeight) };
}

export function useWidths(sidebar: boolean) {
  // A string snapshot so equal layouts do not re-render.
  const key = useSyncExternalStore(subscribe, () => JSON.stringify(layoutWidths(window.innerWidth, sidebar, window.innerHeight)));
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

/** Starts dragging the bottom dock's top edge: moving up makes it taller. */
export function dragBottom(e: ReactPointerEvent, height: number) {
  if (e.button !== 0) return;
  e.preventDefault();
  const y0 = e.clientY;
  document.body.classList.add("row-resizing");
  const move = (ev: PointerEvent) => setBottomHeight(height + y0 - ev.clientY);
  const up = () => {
    document.body.classList.remove("row-resizing");
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

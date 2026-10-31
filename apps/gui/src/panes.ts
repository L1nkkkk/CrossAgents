// The layout around conversations, kept on this device in localStorage `crossagents.panes`:
//
// - Two conversations side by side: the routed session on the left, one more beside it.
//   Ctrl/⌘-click or middle-click an agent in the sidebar to open it beside.
// - The tool column on the right (docs/tasks/V6-GUI.md §6): up to two panes stacked, each one
//   of 改动 (per-turn diffs), 任务 (the task's details) or 提交图 (the run's branches on a run
//   page, else the conversation's checkout). Panes follow
//   the focused conversation: the routed one, or the one beside it once it was clicked last.
//   Something opened on purpose (a turn's "查看变更", a task card's turn) is the pane's
//   `target` until the focus moves to another conversation.
//
// 事件流 (raw hub events) is not one of them: it is for debugging, so only a development page or
// the mock preview offers it, and a saved layout that still names it loses just that pane.

import { useSyncExternalStore } from "react";

export type PaneKind = "changes" | "task" | "events" | "graph";
const LABELS: Record<PaneKind, string> = { changes: "改动", task: "任务", events: "事件流", graph: "提交图" };
/** Raw hub events: shown by the dev server and `preview:mock`, not by a packaged build. */
export const DEBUG_PANES = __BUILD__.dev || __MOCK__;
/** What the tool column's menu offers, in order. */
export const PANE_KINDS: { kind: PaneKind; label: string }[] = (Object.keys(LABELS) as PaneKind[])
  .filter((k) => k !== "events" || DEBUG_PANES)
  .map((kind) => ({ kind, label: LABELS[kind] }));

/** What a pane was opened for: an agent's turn (and file), or a task. */
export type Target = { agent?: string; turn?: string; file?: string; run?: string; task?: string };

export type Tools = { open: boolean; panes: PaneKind[]; split: number };
type Panes = { beside: string | null; ratio: number; tools: Tools };

const KEY = "crossagents.panes";
const clamp = (r: number) => Math.min(0.75, Math.max(0.25, r));
const clampSplit = (r: number) => Math.min(0.8, Math.max(0.2, r));
/** Any kind a saved layout may name — including a pane the menu no longer offers, so `load` can recognise it. */
const isKind = (k: unknown): k is PaneKind => typeof k === "string" && k in LABELS;
/** Whether the tool column offers `kind` here (事件流 only where debugging is on). */
export const offered = (k: PaneKind) => PANE_KINDS.some((p) => p.kind === k);
/** What a pane is called in the menu and its header. */
export const paneLabel = (k: PaneKind) => LABELS[k];
const DEFAULT_TOOLS: Tools = { open: false, panes: ["changes"], split: 0.5 };
const listeners = new Set<() => void>();
let state: Panes = load();
/** The conversation clicked last, and what a pane was opened for: not kept across reloads. */
let focus: string | null = null;
let target: Target | null = null;
const sessionTools = new Map<string, { panes: PaneKind[]; target: Target | null }>();
let view: { state: Panes; focus: string | null; target: Target | null } = { state, focus, target };

function load(): Panes {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (saved && typeof saved.ratio === "number") {
      const t = saved.tools;
      const panes: PaneKind[] = Array.isArray(t?.panes) ? t.panes.filter(isKind).filter(offered).slice(0, 2) : [];
      const tools: Tools = {
        open: t?.open === true,
        panes: panes.length ? [...new Set(panes)] : DEFAULT_TOOLS.panes,
        split: typeof t?.split === "number" ? clampSplit(t.split) : DEFAULT_TOOLS.split,
      };
      return { beside: saved.beside ?? null, ratio: clamp(saved.ratio), tools };
    }
  } catch {
    // Fall through to the default.
  }
  return { beside: null, ratio: 0.5, tools: DEFAULT_TOOLS };
}

function changed() {
  view = { state, focus, target };
  listeners.forEach((l) => l());
}

function set(next: Panes) {
  state = next;
  try {
    localStorage.setItem(KEY, JSON.stringify(state));
  } catch {
    // The layout then lasts for this page only.
  }
  changed();
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

/** A conversation was clicked: the tool panes follow it. */
export function focusSession(name: string) {
  if (focus === name) return;
  if (focus) sessionTools.set(focus, { panes: state.tools.panes, target });
  focus = name;
  const saved = sessionTools.get(name);
  target = saved?.target ?? null;
  state = { ...state, tools: { ...state.tools, panes: saved?.panes ?? DEFAULT_TOOLS.panes } };
  changed();
}

/** Opens the tool column showing `kind` (in the first pane unless it is already shown). */
export function openTool(kind: PaneKind, what?: Target) {
  const panes = state.tools.panes.includes(kind) ? state.tools.panes : [kind, ...state.tools.panes.slice(1)];
  target = what ?? target;
  set({ ...state, tools: { ...state.tools, open: true, panes: [...new Set(panes)] } });
}

export const toggleTools = () => set({ ...state, tools: { ...state.tools, open: !state.tools.open } });
export const closeTools = () => set({ ...state, tools: { ...state.tools, open: false } });
export const setSplit = (split: number) => set({ ...state, tools: { ...state.tools, split: clampSplit(split) } });
export const clearTarget = () => {
  target = null;
  changed();
};

/** Shows `kind` in pane `index`; if the other pane showed it, the two swap. */
export function setPane(index: number, kind: PaneKind) {
  const panes = state.tools.panes.slice();
  const other = panes.indexOf(kind);
  if (other >= 0 && other !== index) panes[other] = panes[index];
  panes[index] = kind;
  set({ ...state, tools: { ...state.tools, panes } });
}

/** A second pane, showing a kind the first does not. */
export function addPane() {
  const panes = state.tools.panes;
  if (panes.length >= 2) return;
  const kind = PANE_KINDS.find((p) => !panes.includes(p.kind))!.kind;
  set({ ...state, tools: { ...state.tools, panes: [...panes, kind] } });
}

export function removePane(index: number) {
  const panes = state.tools.panes.filter((_, i) => i !== index);
  set({ ...state, tools: { ...state.tools, open: panes.length > 0 && state.tools.open, panes: panes.length ? panes : DEFAULT_TOOLS.panes } });
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export const usePanes = () => useSyncExternalStore(subscribe, () => view.state);
export const useLayout = () => useSyncExternalStore(subscribe, () => view);

/** Of the open conversations, the one the tool panes follow. */
export function focusedOf(primary: string, beside: string | null, focus: string | null): string {
  return focus && (focus === primary || focus === beside) ? focus : primary;
}

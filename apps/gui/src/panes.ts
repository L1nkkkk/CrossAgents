// The layout around conversations, kept on this device in localStorage `crossagents.panes`:
//
// - Two conversations side by side: the routed session on the left, one more beside it.
//   Ctrl/⌘-click or middle-click an agent in the sidebar to open it beside.
// - Tabs in two docks (lib/docks.ts), one on the right and one under the conversations: 改动
//   (per-turn diffs), 文件 (browsing the workspace), 任务 (the task's details) and 提交图 (the
//   run's branches on a run page, else the conversation's checkout) and 后台 (the agents' reminders,
//   watches and the tasks being verified). Which dock holds what is
//   the same everywhere; what a tab shows follows the focused conversation: the routed one, or
//   the one beside it once it was clicked last. Something opened on purpose (a turn's "查看变更",
//   a task card's turn) is `target` until the focus moves to another conversation. Terminals
//   (terminal.tsx) are tabs too, with their own project instead.
//
// 事件流 (raw hub events) is for debugging, so only a development page or the mock preview offers
// it, and a saved layout that still names it loses just that tab.

import { useSyncExternalStore } from "react";
import { closeTab, dockOf, moveTab, restoreDocks, showTab, type DockId, type Docks } from "./lib/docks";

export type PaneKind = "changes" | "files" | "task" | "events" | "graph" | "background";
/** A terminal's tab, by its id in the hub. */
export type TermTab = `term:${string}`;
export type TabId = PaneKind | TermTab;
export const isTermTab = (tab: TabId): tab is TermTab => tab.startsWith("term:");
export const termTab = (id: string): TermTab => `term:${id}`;
export type DockTarget = DockId;
const LABELS: Record<PaneKind, string> = { changes: "改动", files: "文件", task: "任务", events: "事件流", graph: "提交图", background: "后台" };
/** Raw hub events: shown by the dev server and `preview:mock`, not by a packaged build. */
export const DEBUG_PANES = __BUILD__.dev || __MOCK__;
/** What the tool column's menu offers, in order. */
export const PANE_KINDS: { kind: PaneKind; label: string }[] = (Object.keys(LABELS) as PaneKind[])
  .filter((k) => k !== "events" || DEBUG_PANES)
  .map((kind) => ({ kind, label: LABELS[kind] }));

/** What a pane was opened for: an agent's turn (and file), or a task. */
export type Target = { agent?: string; turn?: string; file?: string; run?: string; task?: string };

type Panes = { beside: string | null; ratio: number; docks: Docks<TabId> };

const KEY = "crossagents.panes";
const clamp = (r: number) => Math.min(0.75, Math.max(0.25, r));
/** Any kind a saved layout may name — including a pane the menu no longer offers, so `load` can recognise it. */
const isKind = (k: unknown): k is PaneKind => typeof k === "string" && k in LABELS;
/** Whether this build offers `kind` (事件流 only where debugging is on). */
export const offered = (k: PaneKind) => PANE_KINDS.some((p) => p.kind === k);
const isTab = (k: unknown): k is TabId => (typeof k === "string" && k.startsWith("term:")) || (isKind(k) && offered(k));
/** What a tab is called. */
export const paneLabel = (k: PaneKind) => LABELS[k];
/** The tabs a first start has, all on the right. */
const FRESH: TabId[] = PANE_KINDS.map((p) => p.kind).filter((k) => k !== "events");
const listeners = new Set<() => void>();
let state: Panes = load();
/** The conversation clicked last, and what a tab was opened for: not kept across reloads. */
let focus: string | null = null;
let target: Target | null = null;
/**
 * In the floating drawer both docks are one row of tabs: the one showing, and whether something
 * opened on purpose asked for the drawer (its tab may be in the bottom dock, which floating hides).
 */
let floatTab: TabId | null = null;
let peek = false;
const sessionTargets = new Map<string, Target | null>();
let view: { state: Panes; focus: string | null; target: Target | null; floatTab: TabId | null; peek: boolean } = { state, focus, target, floatTab, peek };

function load(): Panes {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (saved && typeof saved.ratio === "number") return { beside: saved.beside ?? null, ratio: clamp(saved.ratio), docks: restoreDocks(saved, isTab, FRESH) };
  } catch {
    // Fall through to the default.
  }
  return { beside: null, ratio: 0.5, docks: restoreDocks(null, isTab, FRESH) };
}

function changed() {
  view = { state, focus, target, floatTab, peek };
  listeners.forEach((l) => l());
}

function set(next: Panes) {
  state = next;
  try {
    const { docks, ...rest } = state;
    localStorage.setItem(KEY, JSON.stringify({ ...rest, ...docks }));
  } catch {
    // The layout then lasts for this page only.
  }
  changed();
}

const setDocks = (docks: Docks<TabId>) => set({ ...state, docks });

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

/** A conversation was clicked: the tabs follow it. */
export function focusSession(name: string) {
  if (focus === name) return;
  // The first click since the page loaded moves nothing: the tabs already show this one.
  if (!focus) {
    focus = name;
    changed();
    return;
  }
  sessionTargets.set(focus, target);
  focus = name;
  target = sessionTargets.get(name) ?? null;
  changed();
}

/** Shows `kind` wherever its tab is (on the right if it has none), for `what`. */
export function openTool(kind: PaneKind, what?: Target) {
  target = what ?? target;
  floatTab = kind;
  peek = true;
  setDocks(showTab(state.docks, kind));
}

/** Shows the tab the route opened, without unfolding the right dock for good. */
export function routeTool(kind: PaneKind) {
  floatTab = kind;
  setDocks(showTab(state.docks, kind, false));
}

/** The right dock's switch. Empty, it starts again with a closed tab, or the bottom dock's first. */
export function toggleTools() {
  const { right, bottom } = state.docks;
  if (right.open) return closeTools();
  if (right.tabs.length) return setDocks({ ...state.docks, right: { ...right, open: true } });
  const closed = PANE_KINDS.find((p) => !dockOf(state.docks, p.kind))?.kind ?? bottom.tabs[0];
  if (closed) setDocks(moveTab(state.docks, closed, "right"));
}

export function closeTools() {
  peek = false;
  setDocks({ ...state.docks, right: { ...state.docks.right, open: false } });
}

export const toggleBottom = () => setDocks({ ...state.docks, bottom: { ...state.docks.bottom, open: !state.docks.bottom.open } });
export const closeBottom = () => setDocks({ ...state.docks, bottom: { ...state.docks.bottom, open: false } });
export const clearTarget = () => {
  target = null;
  changed();
};

/** Shows `tab` where it is, and asks for the drawer when docking does not fit. */
export function peekTab(tab: TabId) {
  floatTab = tab;
  peek = true;
  setDocks(showTab(state.docks, tab));
}

/** Puts a new tab in `dock`. `show`: it comes to the front; else it just joins the row. */
export function placeTab(tab: TabId, dock: DockId, show = true) {
  if (show) {
    floatTab = tab;
    peek = true;
    return setDocks(moveTab(state.docks, tab, dock));
  }
  const { tabs, active, open } = state.docks[dock];
  setDocks({ ...state.docks, [dock]: { tabs: [...tabs, tab], active: active ?? tab, open: open || tabs.length === 0 } });
}

export function pickTab(dock: DockId, tab: TabId) {
  floatTab = tab;
  setDocks({ ...state.docks, [dock]: { ...state.docks[dock], active: tab } });
}

export const dropTab = (tab: TabId, dock: DockId, index?: number) => setDocks(moveTab(state.docks, tab, dock, index));

export function shutTab(tab: TabId) {
  if (floatTab === tab) floatTab = null;
  setDocks(closeTab(state.docks, tab));
}

/** Kinds with no tab now, for a dock's + menu. */
export const closedKinds = (docks: Docks<TabId>) => PANE_KINDS.filter((p) => !dockOf(docks, p.kind));

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

// The tabs around conversations: two docks, one on the right and one under the conversations,
// each an ordered row of tabs with one showing. A tab is in at most one dock. Pure; tested in
// docks.test.ts. Where a dragged tab lands is `dropTarget`, from the rectangles on screen.

export type DockId = "right" | "bottom";
export type Dock<T extends string = string> = { tabs: T[]; active: T | null; open: boolean };
export type Docks<T extends string = string> = Record<DockId, Dock<T>>;

export const dockOf = <T extends string>(docks: Docks<T>, tab: T): DockId | null => (docks.right.tabs.includes(tab) ? "right" : docks.bottom.tabs.includes(tab) ? "bottom" : null);

/** The tab a dock shows: its active one if it still holds that, else its first. */
export function shownTab<T extends string>(dock: Dock<T>): T | null {
  return dock.active !== null && dock.tabs.includes(dock.active) ? dock.active : (dock.tabs[0] ?? null);
}

/** Moves `tab` to `index` of `to` (counted without the tab), from wherever it was; it shows there. */
export function moveTab<T extends string>(docks: Docks<T>, tab: T, to: DockId, index?: number): Docks<T> {
  const from = dockOf(docks, tab);
  const next: Docks<T> = { right: { ...docks.right }, bottom: { ...docks.bottom } };
  if (from) {
    next[from].tabs = next[from].tabs.filter((t) => t !== tab);
    // Moving out of a dock leaves the neighbour showing, as closing a browser tab does.
    if (from !== to && docks[from].active === tab) next[from].active = neighbour(docks[from].tabs, tab);
    if (!next[from].tabs.length) next[from].open = false;
  }
  const tabs = next[to].tabs.slice();
  tabs.splice(Math.min(index ?? tabs.length, tabs.length), 0, tab);
  next[to] = { tabs, active: tab, open: true };
  return next;
}

/** Closes `tab`. A dock left with no tabs folds away. */
export function closeTab<T extends string>(docks: Docks<T>, tab: T): Docks<T> {
  const from = dockOf(docks, tab);
  if (!from) return docks;
  const old = docks[from];
  const tabs = old.tabs.filter((t) => t !== tab);
  const active = shownTab(old) === tab ? neighbour(old.tabs, tab) : old.active;
  return { ...docks, [from]: { tabs, active, open: old.open && tabs.length > 0 } };
}

function neighbour<T extends string>(tabs: T[], tab: T): T | null {
  const at = tabs.indexOf(tab);
  return tabs[at + 1] ?? tabs[at - 1] ?? null;
}

/** Shows `tab` where it is, or adds it to the right dock. `open`: also unfolds a folded right dock. */
export function showTab<T extends string>(docks: Docks<T>, tab: T, open = true): Docks<T> {
  const from = dockOf(docks, tab);
  if (!from) {
    const moved = moveTab(docks, tab, "right");
    return open ? moved : { ...moved, right: { ...moved.right, open: docks.right.open } };
  }
  // The bottom dock has no switch of its own to come back from, so showing a tab there unfolds it.
  return { ...docks, [from]: { ...docks[from], active: tab, open: from === "bottom" || open || docks[from].open } };
}

/** A saved layout's docks, keeping only tabs `known` (each once). */
export function restoreDocks<T extends string>(saved: unknown, known: (tab: unknown) => tab is T, fresh: T[]): Docks<T> {
  const raw = saved as { right?: Partial<Dock>; bottom?: Partial<Dock>; tools?: { open?: unknown; panes?: unknown } } | null;
  if (raw?.right && raw.bottom) {
    const seen = new Set<T>();
    const read = (dock: Partial<Dock>): Dock<T> => {
      const tabs = (Array.isArray(dock.tabs) ? dock.tabs : []).filter(known).filter((t) => !seen.has(t) && !!seen.add(t));
      return { tabs, active: known(dock.active) ? dock.active : null, open: dock.open === true && tabs.length > 0 };
    };
    return { right: read(raw.right), bottom: read(raw.bottom) };
  }
  // A layout from before tabs: its stacked panes all become tabs, the first one showing.
  const panes = (Array.isArray(raw?.tools?.panes) ? raw.tools.panes : []).filter(known);
  return {
    right: { tabs: [...new Set([...fresh, ...panes])], active: panes[0] ?? fresh[0] ?? null, open: raw?.tools?.open === true },
    bottom: { tabs: [], active: null, open: false },
  };
}

export type Rect = { left: number; top: number; right: number; bottom: number };
export type Strip = { dock: DockId; rect: Rect; tabs: { id: string; left: number; right: number }[] };
export type Drop = { dock: DockId; index?: number; zone: Rect; marker?: number };

const inside = (r: Rect, x: number, y: number) => x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;

/** The lower part of the conversations a new bottom dock takes; the part on the right a new right dock takes. */
export const NEW_BOTTOM = 0.35;
export const NEW_RIGHT = 1 / 3;

/**
 * Where a tab dragged to (x, y) would land. Over a row of tabs: between them. Over a dock: at its
 * end. Over the edges of the conversations, a dock not showing would open there.
 */
export function dropTarget(x: number, y: number, tab: string, at: { strips: Strip[]; right: Rect | null; bottom: Rect | null; center: Rect }): Drop | null {
  for (const strip of at.strips) {
    if (!inside(strip.rect, x, y)) continue;
    const others = strip.tabs.filter((t) => t.id !== tab);
    const index = others.filter((t) => (t.left + t.right) / 2 < x).length;
    const marker = others.length === 0 ? strip.rect.left + 6 : index < others.length ? others[index].left : others[others.length - 1].right;
    return { dock: strip.dock, index, zone: strip.rect, marker };
  }
  if (at.right && inside(at.right, x, y)) return { dock: "right", zone: at.right };
  if (at.bottom && inside(at.bottom, x, y)) return { dock: "bottom", zone: at.bottom };
  if (!inside(at.center, x, y)) return null;
  const { left, top, right, bottom } = at.center;
  const lower = bottom - (bottom - top) * NEW_BOTTOM;
  if (!at.bottom && y >= lower) return { dock: "bottom", zone: { left, top: lower, right, bottom } };
  const side = right - (right - left) * NEW_RIGHT;
  if (!at.right && x >= side) return { dock: "right", zone: { left: side, top, right, bottom: at.bottom ? bottom : lower } };
  return null;
}

// Extension tabs live in the same docks as the built-in panes, under `ext:<name>/<id>`
// (docs/EXTENSIONS.md §5). Kept here, free of React and localStorage, so the shape can be
// checked by the unit tests and by panes.ts before an extension has loaded.

/** A tab an extension registered: `ext:standup/report`. */
export type ExtTab = `ext:${string}/${string}`;

/** Lowercase extension name, then the tab id it chose; neither part contains `/`. */
const EXT_TAB = /^ext:([a-z][a-z0-9-]*)\/([^/]+)$/;

export const isExtTab = (tab: unknown): tab is ExtTab => typeof tab === "string" && EXT_TAB.test(tab);

export const extTabOf = (name: string, id: string): ExtTab => `ext:${name}/${id}`;

/** The name and id `tab` was built from. Throws for anything that is not an extension tab. */
export function parseExtTab(tab: ExtTab): { name: string; id: string } {
  const match = EXT_TAB.exec(tab);
  if (!match) throw new Error(`not an extension tab: ${tab}`);
  return { name: match[1], id: match[2] };
}

export const extensionOfTab = (tab: ExtTab): string => parseExtTab(tab).name;

/**
 * A dock after `keep` was applied: the remaining tabs, an active tab that still exists (else
 * the first one), and folded away when nothing is left. `open` stays as it was otherwise.
 */
export function filterDockTabs<T extends string>(
  dock: { tabs: T[]; active: T | null; open: boolean },
  keep: (tab: T) => boolean,
): { tabs: T[]; active: T | null; open: boolean } {
  const tabs = dock.tabs.filter(keep);
  if (tabs.length === dock.tabs.length) return dock;
  const active = dock.active !== null && tabs.includes(dock.active) ? dock.active : (tabs[0] ?? null);
  return { tabs, active, open: dock.open && tabs.length > 0 };
}

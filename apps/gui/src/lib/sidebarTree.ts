/** Source relationships are global: a chat and its runs can belong to different groups. */
export function groupRunsByOrigin<T extends { run: { origin_chat_id?: string | null; planner: string } }>(
  runs: T[],
  visibleChats: Set<string>,
): { children: Map<string, T[]>; standalone: T[] } {
  const children = new Map<string, T[]>();
  const standalone: T[] = [];
  for (const detail of runs) {
    const origin = detail.run.origin_chat_id;
    if (!origin || origin === detail.run.planner || !visibleChats.has(origin)) {
      standalone.push(detail);
      continue;
    }
    const siblings = children.get(origin) ?? [];
    siblings.push(detail);
    children.set(origin, siblings);
  }
  return { children, standalone };
}

/**
 * What a chat's child runs mean for its row: the ones shown under it (not pinned elsewhere,
 * matching the search), whether a matching child keeps the chat in a search, and whether a
 * selected child keeps it open and past the "更多" cut.
 */
export function childPath<T>(
  runs: T[],
  is: { matching: (run: T) => boolean; selected: (run: T) => boolean; pinned: (run: T) => boolean },
): { shown: T[]; matched: boolean; selected: boolean } {
  return {
    shown: runs.filter((r) => !is.pinned(r) && is.matching(r)),
    matched: runs.some(is.matching),
    selected: runs.some(is.selected),
  };
}

/** The rows a group shows before "更多": the first `cap`, plus any that must stay (a selected path). */
export function capped<T>(items: T[], keep: (item: T) => boolean, cap: number, all: boolean): T[] {
  return all ? items : items.filter((item, i) => i < cap || keep(item));
}

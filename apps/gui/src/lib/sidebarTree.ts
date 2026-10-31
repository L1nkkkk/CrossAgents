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

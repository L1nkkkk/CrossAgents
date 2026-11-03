/** Keep matching options and their section headers, including disabled options. */
export function filterMenu<T extends { kind: "item" | "header"; key: string; label: unknown; sub?: unknown; searchText?: string }>(items: readonly T[], query: string): T[] {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return [...items];
  let header: T | undefined;
  let added = false;
  const result: T[] = [];
  for (const item of items) {
    if (item.kind === "header") {
      header = item;
      added = false;
      continue;
    }
    const text = [header?.label, item.key, item.label, item.sub, item.searchText]
      .filter((v) => typeof v === "string").join(" ").toLocaleLowerCase();
    if (!terms.every((term) => text.includes(term))) continue;
    if (header && !added) result.push(header);
    added = true;
    result.push(item);
  }
  return result;
}

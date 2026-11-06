// "需要你" rows for projects whose identity waits on a person (docs/tasks/LESSONS.md §4.2,
// L3): a checkout whose directory now points at a remote the project does not know. Binding
// or merging happens on the 排障记录 page, so the row opens it.

import type { RunDetail } from "@bindings/RunDetail";
import { api } from "../api";
import { useHubData } from "../hub";
import type { TodoItem } from "../lib/todos";

/** A path as the hub compares it: separators unified, no trailing slash, lower case. */
const pathKey = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

/** One row per project whose identity is unconfirmed, attributed to the newest run there. */
export function useUnboundProjects(runs: readonly RunDetail[]): TodoItem[] {
  const projects = useHubData(
    () => api.projects(),
    [runs.length],
    (e) => e.type === "run.created",
  );
  const out: TodoItem[] = [];
  for (const p of projects.data ?? []) {
    if (!p.unconfirmed_remote) continue;
    const checkouts = p.paths.map((at) => pathKey(at.path));
    const near = runs
      .filter((d) => checkouts.includes(pathKey(d.run.repo)))
      .sort((a, b) => b.run.updated_at.localeCompare(a.run.updated_at))[0];
    out.push({
      id: `project:${p.id}`,
      agent: near?.run.planner ?? runs[0]?.run.planner ?? "",
      kind: "notice",
      summary: `${p.name}：目录的 remote 变成了 ${p.unconfirmed_remote}，要确认它属于哪个项目`,
      action: "view",
      href: "#/lessons",
      since: near?.run.updated_at ?? "",
    });
  }
  return out;
}

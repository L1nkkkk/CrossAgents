// "需要你" rows for runs whose recorded problems nobody has sorted yet (docs/tasks/LESSONS.md L2).

import type { RunDetail } from "@bindings/RunDetail";
import { api } from "../api";
import { useHubData } from "../hub";
import type { TodoItem } from "../lib/todos";

/** One row per run with unsorted issues, pointing at the run page where 「交给…整理」 is. */
export function useUnsortedIssues(runs: readonly RunDetail[]): TodoItem[] {
  const ids = runs.map((d) => d.run.id);
  const counts = useHubData(
    () => Promise.all(ids.map((id) => api.runIssues(id).then((r) => r.unsorted, () => 0))),
    [ids.join(",")],
    (e) => e.type === "issue.recorded" || e.type === "issue.triaged",
  );
  const out: TodoItem[] = [];
  runs.forEach((d, i) => {
    const n = counts.data?.[i] ?? 0;
    if (n > 0)
      out.push({
        id: `issues:${d.run.id}`,
        agent: d.run.planner,
        kind: "notice",
        summary: `${d.run.title}：${n} 条问题未整理`,
        action: "view",
        href: "#/runs/" + encodeURIComponent(d.run.id),
        since: d.run.updated_at,
      });
  });
  return out;
}

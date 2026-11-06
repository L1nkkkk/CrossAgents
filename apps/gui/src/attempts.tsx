// Runs (attempts) of a task (docs/research/multica.md §2.4–2.5): a run ends when its worker's
// turn does, and that is not the task ending — the issuer still decides. The task page shows
// how many runs there were and how each went, so a stall or a retry is visible rather than
// hidden behind one "attempts" number.

import type { Task } from "@bindings/Task";
import type { TaskRun } from "@bindings/TaskRun";
import type { TaskRunOutcome } from "@bindings/TaskRunOutcome";
import { clock } from "./lib/time";

/** How one outcome reads, and in what tone. */
export const RUN_OUTCOME: Record<TaskRunOutcome, { label: string; tone: string }> = {
  running: { label: "进行中", tone: "accent" },
  reported: { label: "已报告", tone: "ok" },
  asked: { label: "提问", tone: "warn" },
  gave_up: { label: "放弃", tone: "warn" },
  no_report: { label: "没有上报就结束", tone: "" },
  infrastructure: { label: "平台故障", tone: "warn" },
  agent_error: { label: "agent_error", tone: "danger" },
  session_lost: { label: "会话失效", tone: "warn" },
  cancelled: { label: "已取消", tone: "" },
};

/**
 * The task's runs, newest first: each one's number, how it ended, whether the hub retried it,
 * when it began and ended, and what the hub had to say about it.
 *
 * Old records carry no runs at all (`runs` absent): nothing is shown rather than an empty box.
 */
export function TaskRuns({ task }: { task: Task }) {
  const runs = task.runs ?? [];
  if (runs.length === 0) return null;
  const newestFirst = runs.slice().reverse();
  return (
    <section>
      <h2>
        运行 <span className="muted small">共 {runs.length} 次</span>
      </h2>
      <ul className="plain">
        {newestFirst.map((run) => (
          <RunLine key={run.index} run={run} />
        ))}
      </ul>
    </section>
  );
}

function RunLine({ run }: { run: TaskRun }) {
  const outcome = RUN_OUTCOME[run.outcome] ?? { label: run.outcome, tone: "" };
  const ended = run.ended_at ? clock(run.ended_at) : "进行中";
  return (
    <li className="small" style={{ marginBottom: 6 }}>
      <div className="row" style={{ gap: 8 }}>
        <span className="muted">第 {run.index} 次</span>
        <span className={`chip ${outcome.tone}`}>{outcome.label}</span>
        {run.retried && <span className="chip accent">已自动重试</span>}
        <span className="spacer" />
        <span className="muted" title={`${run.started_at} → ${run.ended_at ?? ""}`}>
          {clock(run.started_at)} → {ended}
        </span>
      </div>
      {run.note && (
        <div className="muted" style={{ whiteSpace: "pre-wrap" }}>
          {run.note}
        </div>
      )}
    </li>
  );
}

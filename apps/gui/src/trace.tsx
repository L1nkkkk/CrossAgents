// A run's trace (docs/tasks/DOGFOOD.md §8): by task, the first thing that went wrong, then
// every turn, command and check, and what it cost. Folded by the hub from the run's events,
// so workers removed after their task still show.

import { useState } from "react";
import type { LaneTrace } from "@bindings/LaneTrace";
import type { TaskTrace } from "@bindings/TaskTrace";
import { api } from "./api";
import { ErrorBox, StatusChip, agentName, taskTone, useAgents } from "./components";
import { useHubData } from "./hub";
import { clock } from "./lib/time";

const money = (usd: number) => (usd >= 0.01 ? `$${usd.toFixed(2)}` : usd > 0 ? "<$0.01" : "$0");
const tokens = (n: number) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(n));
const failed = (code: number | null) => code !== null && code !== 0;

export function RunTraceView({ runId }: { runId: string }) {
  const trace = useHubData(
    () => api.runTrace(runId),
    [runId],
    (e) => (e.type === "task.status_changed" || e.type === "task.checked" || e.type === "task.rerouted") && e.data.run_id === runId,
  );
  if (trace.error) return <ErrorBox error={trace.error} />;
  if (!trace.data) return <p className="muted small">加载追溯…</p>;
  const t = trace.data;
  return (
    <div className="trace">
      <div className="small muted">
        共花费 {tokens(t.spent.tokens)} tokens · {money(t.spent.cost_usd)} · Planner {laneLine(t.lead)}
      </div>
      {t.tasks.map((task) => (
        <TaskRow key={task.task_id ?? task.key} task={task} />
      ))}
    </div>
  );
}

function laneLine(lane: LaneTrace): string {
  const bad = lane.commands.filter((c) => failed(c.exit_code)).length;
  return `${lane.turns.length} 轮 · ${lane.commands.length} 条命令${bad ? `（${bad} 条失败）` : ""} · ${money(lane.spent.cost_usd)}`;
}

function TaskRow({ task }: { task: TaskTrace }) {
  const agents = useAgents();
  const [open, setOpen] = useState(!!task.first_failure);
  const lane = task.lane;
  return (
    <div className={`trace-task ${task.first_failure ? "bad" : ""}`}>
      <button className="trace-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        <code>#{task.key}</code>
        <span className="trace-title">{task.title ?? ""}</span>
        {task.status && <StatusChip status={task.status} tone={taskTone(task.status)} />}
        <span className="muted small">{laneLine(lane)}</span>
        <span className="disclosure" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {task.first_failure && <div className="trace-first">首个失败：{task.first_failure}</div>}
      {open && (
        <div className="trace-body">
          {task.failure && <div className="small">任务失败原因：{task.failure}</div>}
          {task.rerouted.map((r, i) => (
            <div key={i} className="small muted">
              改派：{r}
            </div>
          ))}
          {lane.turns.length > 0 && (
            <ul className="trace-list">
              {lane.turns.map((turn) => (
                <li key={turn.message_id} className={turn.error || turn.stop_reason === "error" || turn.stop_reason === "interrupted" ? "bad" : ""}>
                  <span className="muted">{clock(turn.started_at)}</span> 轮次 · {agentName(agents.data, turn.agent)} · {turn.stop_reason ?? "进行中"}
                  {turn.error && <span> · {turn.error}</span>}
                </li>
              ))}
            </ul>
          )}
          {lane.commands.length > 0 && (
            <ul className="trace-list">
              {lane.commands.map((c, i) => (
                <li key={i} className={failed(c.exit_code) ? "bad" : ""}>
                  <span className="muted">{clock(c.at)}</span> <code>{c.command}</code> · exit {c.exit_code ?? "?"}
                  {c.duration_ms !== null && <span className="muted"> · {(c.duration_ms / 1000).toFixed(1)}s</span>}
                  {c.checker && <span className="muted"> · 检查方运行</span>}
                  {c.output_tail && <pre className="trace-output">{c.output_tail}</pre>}
                </li>
              ))}
            </ul>
          )}
          {task.checks.map((check, i) => (
            <div key={i} className={`small ${check.passed ? "" : "bad"}`}>
              <span className="muted">{clock(check.at)}</span> 验收 {check.passed ? "通过" : "未通过"}：
              {check.checks.map((r) => `${r.passed ? "✓" : "✗"} ${r.command}`).join("  ")}
            </div>
          ))}
          <div className="small muted">
            花费 {tokens(lane.spent.tokens)} tokens · {money(lane.spent.cost_usd)}
          </div>
        </div>
      )}
    </div>
  );
}

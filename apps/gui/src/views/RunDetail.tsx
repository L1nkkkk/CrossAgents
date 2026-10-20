import { useState } from "react";
import type { PlannedTask } from "@bindings/PlannedTask";
import type { RunDetail } from "@bindings/RunDetail";
import { api, errorText } from "../api";
import { navigate } from "../App";
import { Badge, ErrorBox, List, agentName, time, useAgents } from "../components";
import { useHubData, type HubEvent } from "../hub";

function concerns(runId: string, e: HubEvent): boolean {
  switch (e.type) {
    case "run.status_changed":
    case "run.plan_submitted":
      return e.data.id === runId;
    case "task.status_changed":
    case "task.checked":
      return e.data.run_id === runId;
    default:
      return false;
  }
}

const ACTIVE = ["planning", "awaiting_approval", "running"];

export function RunDetailView({ runId }: { runId: string }) {
  const detail = useHubData(() => api.getRun(runId), [runId], (e) => concerns(runId, e));
  const agents = useAgents();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function act(action: () => Promise<RunDetail>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      detail.reload();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  if (!detail.data) return detail.error ? <ErrorBox error={detail.error} /> : <p className="muted">加载中…</p>;
  const { run, tasks } = detail.data;

  return (
    <div>
      <p className="crumbs">
        <a href="#/runs">Runs</a> /
      </p>
      <header className="page-header">
        <h1>{run.title}</h1>
        <Badge status={run.status} />
        <div className="spacer" />
        {ACTIVE.includes(run.status) && (
          <button className="danger" disabled={busy} onClick={() => confirm("取消这个 run？") && act(() => api.cancelRun(run.id))}>
            取消 run
          </button>
        )}
      </header>
      <ErrorBox error={error ?? detail.error} />

      <div className="card">
        <dl className="kv">
          <dt>需求</dt>
          <dd className="pre">{run.request}</dd>
          <dt>Planner</dt>
          <dd>
            <a href={`#/agents/${encodeURIComponent(agentName(agents.data, run.planner))}`}>{agentName(agents.data, run.planner)}</a>
          </dd>
          <dt>仓库</dt>
          <dd>
            <code>{run.repo}</code>
          </dd>
          <dt>分支</dt>
          <dd>
            <code>{run.base_branch}</code> → <code>{run.branch}</code>
          </dd>
          <dt>配置</dt>
          <dd>
            Worker {agentName(agents.data, run.config.worker)}
            {run.config.escalate && <> · 升级 {agentName(agents.data, run.config.escalate)}</>}
            {run.config.reviewer && <> · Reviewer {agentName(agents.data, run.config.reviewer)}</>} · 并行{" "}
            {run.config.max_parallel} · 每个 Worker 尝试 {run.config.max_attempts} 次
          </dd>
          <dt>创建</dt>
          <dd>{time(run.created_at)}</dd>
          {run.note && (
            <>
              <dt>备注</dt>
              <dd className="pre">{run.note}</dd>
            </>
          )}
        </dl>
      </div>

      {run.status === "planning" && <p className="muted">Planner 正在拆解任务，提交计划后会显示在这里。</p>}

      {run.status === "awaiting_approval" && (
        <Approval
          plan={run.plan}
          busy={busy}
          onApprove={() => act(() => api.approveRun(run.id))}
          onReject={(reason) => act(() => api.rejectRun(run.id, { reason }))}
        />
      )}

      {tasks.length > 0 && (
        <>
          <h2>任务</h2>
          <table className="table clickable">
            <thead>
              <tr>
                <th>状态</th>
                <th>Key</th>
                <th>标题</th>
                <th>Worker</th>
                <th>尝试</th>
                <th>依赖</th>
              </tr>
            </thead>
            <tbody>
              {tasks.map((task) => (
                <tr key={task.id} onClick={() => navigate("runs", run.id, "tasks", task.id)}>
                  <td>
                    <Badge status={task.status} />
                    {task.escalated && <span className="tag">已升级</span>}
                  </td>
                  <td>
                    <code>{task.key}</code>
                  </td>
                  <td>{task.title}</td>
                  <td>{agentName(agents.data, task.assignee)}</td>
                  <td>{task.attempts}</td>
                  <td>{task.depends_on.join(", ") || "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {run.status !== "awaiting_approval" && tasks.length === 0 && run.plan.length > 0 && (
        <>
          <h2>计划</h2>
          <Plan plan={run.plan} />
        </>
      )}
    </div>
  );
}

function Approval({
  plan,
  busy,
  onApprove,
  onReject,
}: {
  plan: PlannedTask[];
  busy: boolean;
  onApprove: () => void;
  onReject: (reason: string) => void;
}) {
  const [reason, setReason] = useState("");
  return (
    <section>
      <h2>计划待审批（{plan.length} 个任务）</h2>
      <Plan plan={plan} />
      <div className="card approval">
        <button className="primary" disabled={busy} onClick={onApprove}>
          批准并开工
        </button>
        <div className="reject">
          <textarea
            rows={2}
            placeholder="驳回理由（必填），会转给 Planner"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
          />
          <button className="danger" disabled={busy || !reason.trim()} onClick={() => onReject(reason.trim())}>
            驳回
          </button>
        </div>
      </div>
    </section>
  );
}

function Plan({ plan }: { plan: PlannedTask[] }) {
  return (
    <div className="plan">
      {plan.map((task) => (
        <div className="card" key={task.key}>
          <h3>
            <code>{task.key}</code> {task.title}
          </h3>
          {task.depends_on.length > 0 && <p className="muted">依赖：{task.depends_on.join(", ")}</p>}
          <SpecView spec={task.spec} />
        </div>
      ))}
    </div>
  );
}

export function SpecView({ spec }: { spec: PlannedTask["spec"] }) {
  return (
    <dl className="kv">
      <dt>目标</dt>
      <dd className="pre">{spec.goal}</dd>
      <dt>允许修改</dt>
      <dd>
        <List items={spec.scope.allow} empty="不限" />
      </dd>
      {spec.scope.deny.length > 0 && (
        <>
          <dt>禁止修改</dt>
          <dd>
            <List items={spec.scope.deny} />
          </dd>
        </>
      )}
      {spec.interfaces.length > 0 && (
        <>
          <dt>接口</dt>
          <dd>
            <List items={spec.interfaces} />
          </dd>
        </>
      )}
      <dt>验收命令</dt>
      <dd>
        <List items={spec.acceptance} empty="无" />
      </dd>
      {spec.references.length > 0 && (
        <>
          <dt>参考</dt>
          <dd>
            <List items={spec.references} />
          </dd>
        </>
      )}
    </dl>
  );
}

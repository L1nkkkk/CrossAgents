import { useState, type FormEvent } from "react";
import type { Agent } from "@bindings/Agent";
import { api, errorText } from "../api";
import { navigate } from "../App";
import { Badge, ErrorBox, Field, time, useAgents } from "../components";
import { useHubData } from "../hub";

export function RunsView() {
  const runs = useHubData(
    () => api.listRuns().then((r) => r.runs),
    [],
    (e) => e.type.startsWith("run."),
  );
  const [creating, setCreating] = useState(false);
  const sorted = (runs.data ?? []).slice().sort((a, b) => b.created_at.localeCompare(a.created_at));

  return (
    <div>
      <header className="page-header">
        <h1>Runs</h1>
        <button onClick={() => setCreating(!creating)}>{creating ? "收起" : "新建 run"}</button>
      </header>
      {creating && <NewRunForm />}
      <ErrorBox error={runs.error} />
      {runs.data && !sorted.length && <p className="muted">还没有 run。</p>}
      {sorted.length > 0 && (
        <table className="table clickable">
          <thead>
            <tr>
              <th>状态</th>
              <th>标题</th>
              <th>分支</th>
              <th>创建时间</th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((run) => (
              <tr key={run.id} onClick={() => navigate("runs", run.id)}>
                <td>
                  <Badge status={run.status} />
                </td>
                <td>{run.title}</td>
                <td>
                  <code>{run.branch}</code>
                </td>
                <td className="muted">{time(run.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

function AgentSelect({
  agents,
  value,
  onChange,
  optional,
}: {
  agents: Agent[];
  value: string;
  onChange: (v: string) => void;
  optional?: boolean;
}) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      {optional ? <option value="">（不用）</option> : <option value="">选择 agent…</option>}
      {agents.map((a) => (
        <option key={a.id} value={a.name}>
          {a.name} · {a.role} · {a.plugin}
          {a.model ? `/${a.model}` : ""}
        </option>
      ))}
    </select>
  );
}

function NewRunForm() {
  const agents = useAgents();
  const all = agents.data ?? [];
  const byRole = (role: string) => {
    const preferred = all.filter((a) => a.role === role);
    return [...preferred, ...all.filter((a) => a.role !== role)];
  };
  const [request, setRequest] = useState("");
  const [title, setTitle] = useState("");
  const [planner, setPlanner] = useState("");
  const [worker, setWorker] = useState("");
  const [escalate, setEscalate] = useState("");
  const [reviewer, setReviewer] = useState("");
  const [maxParallel, setMaxParallel] = useState("");
  const [autoApprove, setAutoApprove] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  const plannerValue = planner || byRole("planner").find((a) => a.role === "planner")?.name || "";
  const workerValue = worker || byRole("worker").find((a) => a.role === "worker")?.name || "";

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      const run = await api.createRun({
        request,
        title: title.trim() || null,
        planner: plannerValue,
        worker: workerValue,
        escalate: escalate || null,
        reviewer: reviewer || null,
        auto_approve: autoApprove,
        max_parallel: maxParallel ? Number(maxParallel) : null,
        max_attempts: null,
      });
      navigate("runs", run.id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="card form" onSubmit={submit}>
      <Field label="需求">
        <textarea rows={4} value={request} onChange={(e) => setRequest(e.target.value)} placeholder="要做什么" required />
      </Field>
      <Field label="标题（可选）">
        <input value={title} onChange={(e) => setTitle(e.target.value)} />
      </Field>
      {!all.length && agents.data && (
        <p className="muted">
          还没有 agent，先到 <a href="#/agents">Agents</a> 登记 Planner 和 Worker。
        </p>
      )}
      <div className="row">
        <Field label="Planner">
          <AgentSelect agents={byRole("planner")} value={plannerValue} onChange={setPlanner} />
        </Field>
        <Field label="Worker 模板">
          <AgentSelect agents={byRole("worker")} value={workerValue} onChange={setWorker} />
        </Field>
      </div>
      <div className="row">
        <Field label="升级到（可选）">
          <AgentSelect agents={byRole("worker")} value={escalate} onChange={setEscalate} optional />
        </Field>
        <Field label="Reviewer（可选）">
          <AgentSelect agents={byRole("reviewer")} value={reviewer} onChange={setReviewer} optional />
        </Field>
        <Field label="并行数">
          <input type="number" min={1} value={maxParallel} onChange={(e) => setMaxParallel(e.target.value)} placeholder="3" />
        </Field>
      </div>
      <label className="check">
        <input type="checkbox" checked={autoApprove} onChange={(e) => setAutoApprove(e.target.checked)} />
        不等审批，直接开工
      </label>
      <ErrorBox error={error} />
      <div>
        <button type="submit" className="primary" disabled={busy || !request.trim() || !plannerValue || !workerValue}>
          发起
        </button>
      </div>
    </form>
  );
}

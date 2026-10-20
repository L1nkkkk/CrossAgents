import type { ReactNode } from "react";
import { api } from "../api";
import { Badge, ErrorBox, agentName, time, useAgents } from "../components";
import { useHubData } from "../hub";
import { SpecView } from "./RunDetail";

export function TaskDetailView({ runId, taskId }: { runId: string; taskId: string }) {
  const detail = useHubData(
    () => api.getRun(runId),
    [runId],
    (e) => (e.type === "task.status_changed" || e.type === "task.checked") && e.data.id === taskId,
  );
  const agents = useAgents();

  if (!detail.data) return detail.error ? <ErrorBox error={detail.error} /> : <p className="muted">加载中…</p>;
  const { run } = detail.data;
  const task = detail.data.tasks.find((t) => t.id === taskId || t.key === taskId);
  if (!task) return <ErrorBox error={`run ${run.title} 里没有任务 ${taskId}`} />;
  const { report } = task;
  const assignee = agentName(agents.data, task.assignee);

  return (
    <div>
      <p className="crumbs">
        <a href="#/runs">Runs</a> / <a href={`#/runs/${run.id}`}>{run.title}</a> /
      </p>
      <header className="page-header">
        <h1>
          <code>{task.key}</code> {task.title}
        </h1>
        <Badge status={task.status} />
        {task.escalated && <span className="tag">已升级</span>}
      </header>
      <ErrorBox error={detail.error} />

      <div className="card">
        <dl className="kv">
          <dt>Worker</dt>
          <dd>{task.assignee ? <a href={`#/agents/${encodeURIComponent(assignee)}`}>{assignee}</a> : "—"}</dd>
          {task.reviewer && (
            <>
              <dt>Reviewer</dt>
              <dd>{agentName(agents.data, task.reviewer)}</dd>
            </>
          )}
          <dt>尝试</dt>
          <dd>
            {task.attempts}
            {task.nudges > 0 && <span className="muted"> · 提醒 {task.nudges} 次</span>}
          </dd>
          <dt>分支</dt>
          <dd>{task.branch ? <code>{task.branch}</code> : "—"}</dd>
          <dt>工作区</dt>
          <dd>{task.worktree ? <code>{task.worktree}</code> : "—"}</dd>
          <dt>更新</dt>
          <dd>{time(task.updated_at)}</dd>
        </dl>
      </div>

      {report.failure && (
        <Section title="失败原因">
          <div className="error pre">{report.failure}</div>
        </Section>
      )}
      {report.question && (
        <Section title="Worker 的问题">
          <div className="card pre">{report.question}</div>
        </Section>
      )}
      {report.review && (
        <Section title="审查意见">
          <div className="card pre">{report.review}</div>
        </Section>
      )}
      <Section title="Worker 总结">
        {report.summary ? <div className="card pre">{report.summary}</div> : <p className="muted">还没有提交。</p>}
      </Section>
      <Section title="验收">
        {report.checks.length ? (
          report.checks.map((check, i) => (
            <details className="card check-result" key={i} open={!check.passed}>
              <summary>
                <span className={check.passed ? "pass" : "fail"}>{check.passed ? "✓" : "✗"}</span> <code>{check.command}</code>
                <span className="muted"> · exit {check.exit_code ?? "—"}</span>
              </summary>
              <pre>{check.output || "（无输出）"}</pre>
            </details>
          ))
        ) : (
          <p className="muted">还没有运行验收。</p>
        )}
      </Section>
      <Section title="Diff stat">
        {report.diff_stat ? <pre className="card">{report.diff_stat}</pre> : <p className="muted">—</p>}
      </Section>
      <Section title="规格">
        <div className="card">
          <SpecView spec={task.spec} />
        </div>
      </Section>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h2>{title}</h2>
      {children}
    </section>
  );
}

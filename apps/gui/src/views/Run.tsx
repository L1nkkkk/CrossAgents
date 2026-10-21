import { useState, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { Task } from "@bindings/Task";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { TaskSpec } from "@bindings/TaskSpec";
import { api, errorText } from "../api";
import {
  Avatar,
  DiffStat,
  Dot,
  ErrorBox,
  Icon,
  List,
  Prose,
  StatusChip,
  agentName,
  agentTitle,
  ago,
  displayName,
  runTone,
  sessionHref,
  taskTone,
  time,
  useAgents,
} from "../components";
import { useHubData, type HubEvent } from "../hub";
import { BUCKET_LABEL, groupTasks, progress, taskState } from "../lib/taskState";
import { PRESETS, budgetText, presetOf } from "../lib/policy";
import { StateWord, useArmed } from "../parts";

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

export function useRun(runId: string) {
  return useHubData(() => api.getRun(runId), [runId], (e) => concerns(runId, e));
}

const ACTIVE = ["planning", "awaiting_approval", "running", "paused"];

export function RunView({ runId, selectedTask }: { runId: string; selectedTask?: string }) {
  const detail = useRun(runId);
  const agents = useAgents();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const cancelArm = useArmed();

  if (!detail.data) {
    return <div className="page-inner">{detail.error ? <ErrorBox error={detail.error} /> : <p className="muted">加载中…</p>}</div>;
  }
  const { run, tasks } = detail.data;
  const planner = agentName(agents.data, run.planner);
  const plannerAgent = agents.data?.find((a) => a.id === run.planner);
  const done = tasks.filter((t) => t.status === "merged").length;
  const allEnded = tasks.length > 0 && tasks.every((t) => ["merged", "failed", "cancelled"].includes(t.status));

  async function cancel() {
    setBusy(true);
    setError(undefined);
    try {
      await api.cancelRun(run.id);
      detail.reload();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className="topbar">
        <span className="title">{run.title}</span>
        <StatusChip status={run.status} tone={runTone(run.status)} />
        {tasks.length > 0 && (
          <span className="chip">
            {done}/{tasks.length} merged
          </span>
        )}
        <div className="spacer" />
        {plannerAgent ? <Who agent={plannerAgent} label="Planner" /> : <a className="chip" href={sessionHref(planner)}>Planner · {planner}</a>}
        {ACTIVE.includes(run.status) && (
          <button className={`danger ${cancelArm.armed ? "armed" : ""}`} disabled={busy} onClick={() => cancelArm.click(cancel)}>
            {cancelArm.armed ? "再点一次取消" : "取消 run"}
          </button>
        )}
      </div>
      <div className="page">
        <div className="page-inner" style={{ display: "flex", flexDirection: "column", gap: 16 }}>
          <ErrorBox error={error ?? detail.error} />
          <Request text={run.request} />
          <RunMeta run={run} agents={agents.data} />

          {run.status === "planning" && (
            <div className="working">
              <span className="spark">✻</span>
              <span>
                Planner 正在拆解任务 · <a href={sessionHref(planner)}>看它在做什么</a>
              </span>
            </div>
          )}
          {run.status === "awaiting_approval" && <PlanCard run={run} onDone={detail.reload} />}
          {(run.status === "completed" || (run.status === "running" && allEnded)) && (
            <Ending run={run} closing={run.status === "running"} onDone={detail.reload} />
          )}
          {run.status === "paused" && (
            <div className="item-warn">已暂停：{run.note ?? "等你决定"}。在「问你」里选择加预算继续，或取消这个 run。</div>
          )}
          {run.note && run.status !== "awaiting_approval" && run.status !== "paused" && <div className="item-error">{run.note}</div>}

          {tasks.length > 0 && (
            <section>
              <h2>
                任务 <span className="muted small" style={{ fontWeight: 400 }}>{progress(tasks)}</span>
              </h2>
              {groupTasks(tasks).map((group) => (
                <div key={group.bucket} className="board-group">
                  <div className="board-label">
                    {BUCKET_LABEL[group.bucket]} · {group.tasks.length}
                  </div>
                  <div className="board">
                    {group.tasks.map((task) => (
                      <a
                        key={task.id}
                        className={`board-row ${selectedTask === task.id || selectedTask === task.key ? "active" : ""}`}
                        href={`#/runs/${run.id}/tasks/${task.id}`}
                      >
                        <code className="board-key">#{task.key}</code>
                        <div style={{ minWidth: 0 }}>
                          <div className="t">{task.title}</div>
                          <div className="sub row" style={{ gap: 6, flexWrap: "nowrap" }}>
                            <TaskOwner task={task} agents={agents.data} fallback={run.config.worker} />
                            {task.escalated && " · 已升级"}
                          </div>
                        </div>
                        <StateWord state={taskState(task, tasks)} />
                      </a>
                    ))}
                  </div>
                </div>
              ))}
            </section>
          )}

          {run.status !== "awaiting_approval" && tasks.length === 0 && run.plan.length > 0 && (
            <section>
              <h2>计划</h2>
              <PlanList run={run} />
            </section>
          )}
        </div>
      </div>
    </>
  );
}

function RunMeta({ run, agents }: { run: Run; agents?: Parameters<typeof agentName>[0] }) {
  return (
    <div className="row small secondary" style={{ gap: 14 }}>
      <span className="row" style={{ gap: 5 }}>
        <Icon name="branch" size={14} />
        <code>{run.base_branch}</code> → <code>{run.branch}</code>
      </span>
      <span className="row" style={{ gap: 5 }} title={run.repo}>
        <Icon name="folder" size={14} />
        <span style={{ maxWidth: 260, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{run.repo}</span>
      </span>
      <TeamStack ids={run.config.team.length ? run.config.team : [run.config.worker]} agents={agents} />
      <span>
        默认 {agentName(agents, run.config.worker)}
        {run.config.escalate && ` → 升级 ${agentName(agents, run.config.escalate)}`} · 并行 {run.config.max_parallel}
      </span>
      <PolicyChip run={run} />
      <span title={time(run.created_at)}>{ago(run.created_at)}</span>
    </div>
  );
}

/**
 * The end of a run: while the lead wraps up (every task ended), or once it is done. Shows the
 * lead's summary and where the result landed; lands or closes it for the person.
 */
function Ending({ run, closing, onDone }: { run: Run; closing: boolean; onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      onDone();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }
  const landed = run.landed;
  return (
    <div className="done-card">
      <span className="done-check">{closing ? "○" : "✓"}</span>
      <div className="done-text" style={{ flex: 1 }}>
        <strong>{closing ? "收尾中" : "完成"}</strong>
        {closing ? " · 所有任务都已结束，等 Planner 落地和写总结" : ""}
        {landed ? (
          <div className="small">
            已落地到 <code>{run.base_branch}</code>：{landed.mode === "pr" ? <a href={landed.reference} target="_blank" rel="noreferrer">{landed.reference}</a> : <code>{landed.reference}</code>}
            <span className="muted"> · {landed.by === "person" ? "你" : landed.by} · {ago(landed.at)}</span>
          </div>
        ) : (
          <div className="small muted">
            结果在 <code>{run.branch}</code>，还没进 <code>{run.base_branch}</code>
          </div>
        )}
        {run.summary && (
          <div className="run-summary">
            <Prose text={run.summary} />
          </div>
        )}
        <ErrorBox error={error} />
      </div>
      <div className="row" style={{ gap: 6, alignSelf: "flex-start" }}>
        {!landed && (
          <>
            <button className="primary small" disabled={busy} onClick={() => act(() => api.landRun(run.id, "merge"))} title={`在 ${run.base_branch} 上生成一个合并提交`}>
              合并进 {run.base_branch}
            </button>
            <button className="ghost small" disabled={busy} onClick={() => act(() => api.landRun(run.id, "squash"))} title="所有改动压成一个提交">
              压缩合并
            </button>
          </>
        )}
        {closing && (
          <button className="ghost small" disabled={busy} onClick={() => act(() => api.finishRun(run.id))}>
            结束 run
          </button>
        )}
      </div>
    </div>
  );
}

/** "全托管 · 上限 $5 · 2 小时". */
function PolicyChip({ run }: { run: Run }) {
  const policy = run.config.policy;
  const preset = PRESETS.find((p) => p.key === presetOf(policy));
  const budget = budgetText(policy.budget);
  return (
    <span className="chip" title={preset?.hint}>
      {preset?.label ?? "自定义托管"}
      {budget && ` · 上限 ${budget}`}
    </span>
  );
}

/** The planned tasks, each with the teammate it is assigned to; specs folded away. */
export function PlanTasks({ run }: { run: Run }) {
  const agents = useAgents();
  const byName = (name: string | null) => (name ? agents.data?.find((a) => a.name === name) : undefined);
  const fallback = agents.data?.find((a) => a.id === run.config.worker);
  return (
    <div className="tasks">
      {run.plan.map((task, i) => {
        const worker = byName(task.assignee);
        const reviewer = byName(task.reviewer);
        return (
          <div className="plan-task" key={task.key}>
            <div className="head">
              <span className="muted">{i + 1}.</span>
              <code>{task.key}</code>
              <span className="plan-title">{task.title}</span>
              {task.depends_on.length > 0 && <span className="chip">等 {task.depends_on.join(", ")}</span>}
              <span className="spacer" />
              {worker ? (
                <Who agent={worker} />
              ) : (
                <span className="who-chip default" title="未指定，交给默认 Worker">
                  {fallback ? <Avatar agent={fallback} size={18} /> : null}
                  {fallback ? displayName(fallback) : (task.assignee ?? "默认")}
                  <span className="muted">· 默认</span>
                </span>
              )}
              {reviewer && <Who agent={reviewer} label="审" />}
            </div>
            <details>
              <summary>规格</summary>
              <SpecView spec={task.spec} />
            </details>
          </div>
        );
      })}
    </div>
  );
}

/** A teammate as a small chip: avatar, name, title. */
function Who({ agent, label }: { agent: Agent; label?: string }) {
  return (
    <a className="who-chip" href={sessionHref(agent.name)} title={`${displayName(agent)} · ${agentTitle(agent)} · ${agent.model ?? agent.plugin}`}>
      {label && <span className="who-label">{label}</span>}
      <Avatar agent={agent} size={18} />
      {displayName(agent)}
      <span className="muted">{agentTitle(agent)}</span>
    </a>
  );
}

/** Who works on a task: the live worker, else the teammate it was assigned (clones are
 * removed once a task is merged), else the run's default worker. */
function TaskOwner({ task, agents, fallback }: { task: Task; agents?: Agent[]; fallback: string }) {
  const find = (id: string | null) => (id ? agents?.find((a) => a.id === id) : undefined);
  const who = find(task.assignee) ?? find(task.worker_template) ?? find(fallback);
  if (!who) return <span>—</span>;
  return (
    <>
      <Avatar agent={who} size={16} />
      <span>{displayName(who)}</span>
      <span className="muted">{agentTitle(who)}</span>
    </>
  );
}

/** The run's team as overlapping avatars. */
function TeamStack({ ids, agents }: { ids: string[]; agents?: Agent[] }) {
  const team = ids.map((id) => agents?.find((a) => a.id === id)).filter((a): a is Agent => !!a);
  if (!team.length) return null;
  return (
    <span className="row" style={{ gap: 6 }} title={team.map((a) => `${displayName(a)} · ${agentTitle(a)}`).join("\n")}>
      <span className="stack">
        {team.slice(0, 6).map((a) => (
          <Avatar key={a.id} agent={a} size={20} />
        ))}
      </span>
      团队 {team.length} 人
    </span>
  );
}

/** The plan as a list of tasks, specs folded away. */
function PlanList({ run }: { run: Run }) {
  return (
    <div className="plan-card">
      <PlanTasks run={run} />
    </div>
  );
}

/** Plan approval, shown in the run view and in the planner's conversation. */
export function PlanCard({ run, onDone }: { run: Run; onDone?: () => void }) {
  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  async function act(action: () => Promise<RunDetail>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      onDone?.();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="plan-card">
      <header>
        <Dot tone={{ color: "warn" }} />
        Plan · 待审批
        <span className="muted small" style={{ fontWeight: 400 }}>
          {run.plan.length} 个任务
        </span>
        <div className="spacer" />
        <a className="small" href={`#/runs/${run.id}`} style={{ fontWeight: 400 }}>
          {run.title}
        </a>
      </header>
      <PlanTasks run={run} />
      {run.note && (
        <div style={{ padding: "0 16px 10px" }} className="small muted">
          上次驳回：{run.note}
        </div>
      )}
      <footer>
        {rejecting ? (
          <>
            <textarea rows={2} autoFocus placeholder="驳回理由（必填），会转给 Planner" value={reason} onChange={(e) => setReason(e.target.value)} />
            <button className="danger" disabled={busy || !reason.trim()} onClick={() => act(() => api.rejectRun(run.id, { reason: reason.trim() }))}>
              驳回
            </button>
            <button className="ghost" onClick={() => setRejecting(false)}>
              取消
            </button>
          </>
        ) : (
          <>
            <button className="primary" disabled={busy} onClick={() => act(() => api.approveRun(run.id))}>
              Approve · 开工
            </button>
            <button disabled={busy} onClick={() => setRejecting(true)}>
              驳回并说明
            </button>
          </>
        )}
        {error && (
          <div style={{ flexBasis: "100%" }}>
            <ErrorBox error={error} />
          </div>
        )}
      </footer>
    </div>
  );
}

export function SpecView({ spec }: { spec: TaskSpec }) {
  return (
    <dl className="kv" style={{ marginTop: 8 }}>
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

export function TaskDrawer({ runId, taskId, onClose }: { runId: string; taskId: string; onClose: () => void }) {
  const detail = useRun(runId);
  const agents = useAgents();
  const task = detail.data?.tasks.find((t) => t.id === taskId || t.key === taskId);

  return (
    <>
      <div className="topbar">
        {task ? (
          <>
            <code>{task.key}</code>
            <StatusChip status={task.status} tone={taskTone(task.status)} />
          </>
        ) : (
          <span className="muted">任务</span>
        )}
        <div className="spacer" />
        <button className="ghost icon" onClick={onClose} aria-label="关闭">
          <Icon name="close" size={16} />
        </button>
      </div>
      <div className="body">
        {!detail.data && (detail.error ? <ErrorBox error={detail.error} /> : <p className="muted">加载中…</p>)}
        {detail.data && !task && <ErrorBox error={`这个 run 里没有任务 ${taskId}`} />}
        {task && (
          <>
            <h3>{task.title}</h3>
            <dl className="kv small">
              <dt>Worker</dt>
              <dd>
                {task.assignee ? <a href={sessionHref(agentName(agents.data, task.assignee))}>{agentName(agents.data, task.assignee)}</a> : "—"}
                {task.escalated && <span className="chip warn" style={{ marginLeft: 6 }}>已升级</span>}
              </dd>
              {task.reviewer && (
                <>
                  <dt>Reviewer</dt>
                  <dd>
                    <a href={sessionHref(agentName(agents.data, task.reviewer))}>{agentName(agents.data, task.reviewer)}</a>
                  </dd>
                </>
              )}
              <dt>尝试</dt>
              <dd>
                {task.attempts}
                {task.nudges > 0 && <span className="muted"> · 提醒 {task.nudges} 次</span>}
              </dd>
              <dt>分支</dt>
              <dd>{task.branch ? <code>{task.branch}</code> : "—"}</dd>
              <dt>更新</dt>
              <dd>{ago(task.updated_at)}</dd>
            </dl>

            {task.report.failure && <Section title="失败原因">{<div className="item-error">{task.report.failure}</div>}</Section>}
            {task.report.question && (
              <Section title="Worker 的问题">
                <div className="msg-card">
                  <Prose text={task.report.question} />
                </div>
              </Section>
            )}
            {task.report.review && (
              <Section title="审查意见">
                <div className="msg-card">
                  <Prose text={task.report.review} />
                </div>
              </Section>
            )}
            <Section title="Worker 总结">
              {task.report.summary ? (
                <div className="msg-card">
                  <Prose text={task.report.summary} />
                </div>
              ) : (
                <p className="muted small">还没有提交。</p>
              )}
            </Section>
            {task.report.diff_stat && (
              <Section title="改动">
                <DiffStat text={task.report.diff_stat} />
              </Section>
            )}
            <Section title="验收">
              {task.report.checks.length ? (
                task.report.checks.map((check, i) => (
                  <details className="check-result" key={i} open={!check.passed}>
                    <summary>
                      <span className={check.passed ? "pass" : "fail"}>{check.passed ? "✓" : "✗"}</span>
                      <code>{check.command}</code>
                      <span className="muted small">exit {check.exit_code ?? "—"}</span>
                    </summary>
                    <pre>{check.output || "（无输出）"}</pre>
                  </details>
                ))
              ) : (
                <p className="muted small">还没有运行验收。</p>
              )}
            </Section>
            <Section title="规格">
              <SpecView spec={task.spec} />
            </Section>
          </>
        )}
      </div>
    </>
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


/** The human's request; long ones start folded to a few lines. */
function Request({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const long = text.length > 280 || text.split("\n").length > 5;
  return (
    <div className={`request ${long && !open ? "folded" : ""}`}>
      <div className="request-text">{text}</div>
      {long && (
        <button className="tools-more" style={{ padding: 0 }} onClick={() => setOpen(!open)}>
          {open ? "收起" : "展开全部"}
        </button>
      )}
    </div>
  );
}

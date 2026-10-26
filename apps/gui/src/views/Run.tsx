import { useEffect, useState, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { Task } from "@bindings/Task";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { TaskSpec } from "@bindings/TaskSpec";
import type { PlannedTask } from "@bindings/PlannedTask";
import type { RunPolicy } from "@bindings/RunPolicy";
import { navigate } from "../App";
import { api, errorText } from "../api";
import { DeleteRunDialog } from "../deleteRun";
import { HandoffChain } from "../handoff";
import {
  Avatar,
  ErrorBox,
  Icon,
  List,
  Prose,
  StatusChip,
  agentName,
  agentTitle,
  ago,
  displayName,
  isAgent,
  runTone,
  sessionHref,
  taskTone,
  time,
  useAgents,
} from "../components";
import { onHubEvent, useHubData, type HubEvent } from "../hub";
import { BUCKET_LABEL, groupTasks, progress, taskState } from "../lib/taskState";
import { PRESETS, budgetText, presetOf, presetPolicy, type Preset } from "../lib/policy";
import { accountOf, prettyModel } from "../lib/models";
import { StateWord, TaskChanges } from "../parts";
import { RunChanges, TaskReview } from "../review";
import { RunTraceView } from "../trace";
import { wsKey } from "../workspace";
import { SessionView } from "./Session";

function concerns(runId: string, e: HubEvent): boolean {
  switch (e.type) {
    case "run.status_changed":
    case "run.plan_submitted":
      return e.data.id === runId;
    case "task.status_changed":
    case "task.checked":
    case "task.rerouted":
    case "task.handed_off":
      return e.data.run_id === runId;
    default:
      return false;
  }
}

export function useRun(runId: string) {
  return useHubData(() => api.getRun(runId), [runId], (e) => concerns(runId, e));
}

const ACTIVE = ["planning", "awaiting_approval", "running", "paused"];

/**
 * A run is its lead's conversation (docs/tasks/W1.md §8): the run's header, its facts above
 * the messages, how it ended after them, and the task board beside them. The plan waits
 * for approval in the dock above the composer, as in the lead's own conversation.
 */
export function RunView({ runId, selectedTask, confirmDelete }: { runId: string; selectedTask?: string; confirmDelete?: boolean }) {
  const detail = useRun(runId);
  const agents = useAgents();
  const [deleting, setDeleting] = useState(false);

  // `#/runs/<id>/delete` opens the confirmation (a link the person can be sent; also how the
  // mock previews show the dialog).
  useEffect(() => {
    if (confirmDelete) setDeleting(true);
  }, [confirmDelete]);
  // The run this page shows was deleted (here or from another window): there is nothing left
  // to look at.
  useEffect(() => {
    const off = onHubEvent((e) => {
      if (e.type === "run.deleted" && e.data.run_id === runId) navigate("");
    });
    return off;
  }, [runId]);

  if (!detail.data) {
    return <div className="page-inner">{detail.error ? <ErrorBox error={detail.error} /> : <p className="muted">加载中…</p>}</div>;
  }
  const { run, tasks } = detail.data;
  const planner = agentName(agents.data, run.planner);
  const plannerAgent = agents.data?.find((a) => a.id === run.planner);
  const done = tasks.filter((t) => t.status === "merged").length;
  const allEnded = tasks.length > 0 && tasks.every((t) => ["merged", "failed", "cancelled"].includes(t.status));

  const header = (
    <div className="topbar">
      <span className="title">{run.title}</span>
      <StatusChip status={run.status} tone={runTone(run.status)} />
      {tasks.length > 0 && (
        <span className="chip">
          {done}/{tasks.length} merged
        </span>
      )}
      <div className="spacer" />
      {plannerAgent ? <Who agent={plannerAgent} label="主导" /> : <a className="chip" href={sessionHref(run.planner)}>主导 · {planner}</a>}
      <button
        className="danger"
        title={ACTIVE.includes(run.status) ? "先取消，再删除它的一切" : "删除这个 run、它的任务、agent 和工作区"}
        onClick={() => setDeleting(true)}
      >
        {ACTIVE.includes(run.status) ? "取消并删除" : "删除"}
      </button>
    </div>
  );

  const top = (
    <div className="run-head">
      <RunMeta run={run} agents={agents.data} />
      <ErrorBox error={detail.error} />
      {run.status === "paused" && <div className="item-warn">已暂停：{run.note ?? "等你决定"}。在「问你」里选择加预算继续，或取消这个 run。</div>}
      {run.note && !["awaiting_approval", "paused", "planning"].includes(run.status) && <div className="item-error">{run.note}</div>}
    </div>
  );

  const bottom = (
    <div className="run-foot">
      {run.status !== "awaiting_approval" && tasks.length === 0 && run.plan.length > 0 && <PlanList run={run} />}
      {(run.status === "completed" || (run.status === "running" && allEnded)) && <Ending run={run} closing={run.status === "running"} onDone={detail.reload} />}
      <RunNotes runId={run.id} />
      {tasks.length > 0 && (
        <details className="notes-panel">
          <summary>追溯</summary>
          <RunTraceView runId={run.id} />
        </details>
      )}
    </div>
  );

  const side = tasks.length ? <TaskBoard run={run} tasks={tasks} agents={agents.data} selectedTask={selectedTask} /> : undefined;

  return (
    <>
      <SessionView key={run.planner} name={run.planner} header={header} top={top} bottom={bottom} side={side} />
      {deleting && (
        <DeleteRunDialog
          detail={detail.data}
          onClose={() => {
            setDeleting(false);
            if (confirmDelete) navigate("runs", runId);
          }}
        />
      )}
    </>
  );
}

/** The run's tasks by what they need, each opening its details beside. */
function TaskBoard({ run, tasks, agents, selectedTask }: { run: Run; tasks: Task[]; agents?: Agent[]; selectedTask?: string }) {
  return (
    <section className="task-board">
      <div className="task-board-head">
        任务 <span className="muted small">{progress(tasks)}</span>
      </div>
      {groupTasks(tasks).map((group) => (
        <div key={group.bucket} className="board-group">
          <div className="board-label">
            {BUCKET_LABEL[group.bucket]} · {group.tasks.length}
          </div>
          <div className="board">
            {group.tasks.map((task) => (
              <div key={task.id} className="board-item">
              <a className={`board-row ${selectedTask === task.id || selectedTask === task.key ? "active" : ""}`} href={`#/runs/${run.id}/tasks/${task.id}`}>
                <code className="board-key">#{task.key}</code>
                <div style={{ minWidth: 0 }}>
                  <div className="t">{task.title}</div>
                  <div className="sub row" style={{ gap: 6, flexWrap: "nowrap" }}>
                    <TaskOwner task={task} agents={agents} fallback={run.config.worker} />
                    {task.escalated && " · 已升级"}
                    {task.progress && !task.status.match(/merged|failed|cancelled/) && (
                      <span className="board-progress" title={task.progress}>
                        · {task.progress}
                      </span>
                    )}
                  </div>
                </div>
                <StateWord state={taskState(task, tasks)} />
              </a>
              <HandoffChain handoffs={task.handoffs} agents={agents} />
              </div>
            ))}
          </div>
        </div>
      ))}
    </section>
  );
}

function RunMeta({ run, agents }: { run: Run; agents?: Parameters<typeof agentName>[0] }) {
  return (
    <div className="row small secondary" style={{ gap: 14 }}>
      {run.in_place ? (
        <InPlaceNote run={run} />
      ) : (
        <span className="row" style={{ gap: 5 }}>
          <Icon name="branch" size={14} />
          <code>{run.base_branch}</code> → <code>{run.branch}</code>
        </span>
      )}
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
  // A run that only answered has nothing to land (docs/tasks/W1.md §4).
  const diff = useHubData(() => (landed || run.in_place ? Promise.resolve(undefined) : api.runDiff(run.id)), [run.id, !!landed], () => false);
  const empty = !!diff.data && !diff.data.diff.trim();
  const landable = !landed && !run.in_place && !empty;
  return (
    <div className="done-card">
      <span className="done-check">{closing ? "○" : "✓"}</span>
      <div className="done-text" style={{ flex: 1 }}>
        <strong>{closing ? "收尾中" : "完成"}</strong>
        {closing ? " · 所有任务都已结束，等主导落地和写总结" : ""}
        {run.in_place ? (
          <div className="small muted">
            改动已直接写在 <code>{run.repo}</code>
          </div>
        ) : empty ? (
          <div className="small muted">没有改动，不用落地</div>
        ) : landed ? (
          <div className="small">
            已落地到 <code>{run.base_branch}</code>：{landed.mode === "pr" ? <a href={landed.reference} target="_blank" rel="noreferrer">{landed.reference}</a> : <code>{landed.reference}</code>}
            <span className="muted"> · {landed.by === "person" ? "你" : landed.by} · {ago(landed.at)}</span>
          </div>
        ) : (
          <div className="small muted">
            结果在 <code>{run.branch}</code>，还没进 <code>{run.base_branch}</code>
          </div>
        )}
        {landable && <RunChanges run={run} />}
        {run.summary && (
          <div className="run-summary">
            <Prose text={run.summary} />
          </div>
        )}
        <ErrorBox error={error} />
      </div>
      <div className="row" style={{ gap: 6, alignSelf: "flex-start" }}>
        {landable && (
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

/** Notes the run's agents keep for each other (note_write, scope "run"). */
function RunNotes({ runId }: { runId: string }) {
  const notes = useHubData(() => api.notes("run", runId), [runId], (e) => e.type === "note.written");
  const list = notes.data?.notes ?? [];
  if (!list.length) return null;
  return (
    <details className="notes-panel">
      <summary>这个 run 的笔记 · {list.length}</summary>
      {list.map((n) => (
        <div key={n.key} className="note">
          <code>{n.key}</code>
          <Prose text={n.text} />
        </div>
      ))}
    </details>
  );
}

/** "全托管 · 上限 $5 · 2 小时". */
/** A run outside git works in the folder itself; git can be set up for the runs after it. */
function InPlaceNote({ run }: { run: Run }) {
  const [state, setState] = useState<"idle" | "busy" | "done">("idle");
  const [error, setError] = useState<string>();
  async function init() {
    setState("busy");
    setError(undefined);
    try {
      await api.initRepo(run.repo);
      setState("done");
    } catch (err) {
      setError(errorText(err));
      setState("idle");
    }
  }
  return (
    <span className="row" style={{ gap: 6 }}>
      <span className="chip warn" title="这个文件夹没有 git：任务一次一个、直接改文件，没有分支、对比和合并，改坏了也无法自动撤销。">
        无版本控制 · 直接改文件
      </span>
      {state === "done" ? (
        <span>已初始化 git，之后的 Work 会用独立工作树</span>
      ) : (
        <button className="ghost small" disabled={state === "busy"} onClick={init} title={`在 ${run.repo} 里 git init，并把现有文件提交为第一个版本`}>
          初始化 git
        </button>
      )}
      {error && <span className="chip danger" title={error}>初始化失败</span>}
    </span>
  );
}

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
export function PlanTasks({ run, pick }: { run: Run; pick?: (task: PlannedTask) => ReactNode }) {
  const agents = useAgents();
  const byName = (name: string | null) => (name ? agents.data?.find((a) => isAgent(a, name)) : undefined);
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
              {pick ? (
                pick(task)
              ) : worker ? (
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
    <a className="who-chip" href={sessionHref(agent)} title={`${displayName(agent)} · ${agentTitle(agent)} · ${agent.model ?? agent.plugin}`}>
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

/** A number typed into a cap field; empty means no cap. */
const capOf = (text: string) => (text.trim() ? Number(text) : null);

/**
 * What the person settles when approving a plan: how much the run does without them, who
 * does each task, and its caps (docs/tasks/W1.md §5.2). Shared by the run page's card and the
 * dock in the lead's conversation. Nothing chosen here is remembered for the next run.
 */
export function usePlanApproval(run: Run, busy: boolean) {
  const agents = useAgents();
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const policy = run.config.policy;
  const [preset, setPreset] = useState<Preset>(presetOf(policy) ?? "manual");
  // Per task: "" is the run's default worker, `agent:<id>` a teammate, `model:<m>` a new
  // worker on the lead's login.
  const [picks, setPicks] = useState<Record<string, string>>({});
  const [maxUsd, setMaxUsd] = useState(policy.budget.max_usd?.toString() ?? "");
  const [maxMinutes, setMaxMinutes] = useState(policy.budget.max_minutes?.toString() ?? "");
  const [maxTokens, setMaxTokens] = useState(policy.budget.max_tokens != null ? String(policy.budget.max_tokens / 1000) : "");
  const [maxParallel, setMaxParallel] = useState(String(run.config.max_parallel));

  const everyone = agents.data ?? [];
  const lead = everyone.find((a) => a.id === run.planner);
  const fallback = everyone.find((a) => a.id === run.config.worker);
  const teammates = everyone.filter((a) => a.role === "worker" && run.config.team.includes(a.id));
  const models = plugins.data?.find((p) => p.name === lead?.plugin)?.models ?? [];
  const planned = (task: PlannedTask) => {
    const agent = task.assignee ? everyone.find((a) => isAgent(a, task.assignee!)) : undefined;
    return agent ? `agent:${agent.id}` : "";
  };
  const pickOf = (task: PlannedTask) => picks[task.key] ?? planned(task);

  /** The worker a pick stands for, made on the lead's login when it is a model. */
  async function workerOf(pick: string, made: Map<string, string>): Promise<string | null> {
    if (!pick) return null;
    if (pick.startsWith("agent:")) return pick.slice("agent:".length);
    const model = pick.slice("model:".length);
    const known = made.get(model);
    if (known) return known;
    const account = lead && accountOf(lead);
    const found = everyone.find(
      (a) => a.role === "worker" && a.plugin === lead?.plugin && a.model === model && accountOf(a) === account && wsKey(a.cwd) === wsKey(run.repo),
    );
    const id =
      found?.id ??
      (
        await api.createAgent({
          plugin: lead!.plugin,
          driver: null,
          model,
          role: "worker",
          cwd: run.repo,
          config: account ? { account } : {},
          profile: { display_name: null, title: "Worker", avatar: null, color: null, persona: null },
        })
      ).id;
    made.set(model, id);
    return id;
  }

  async function approve(): Promise<RunDetail> {
    const made = new Map<string, string>();
    const assignees: Record<string, string | null> = {};
    for (const task of run.plan) assignees[task.key] = await workerOf(pickOf(task), made);
    const settled: RunPolicy = {
      ...presetPolicy(preset, policy),
      budget: {
        max_usd: capOf(maxUsd),
        max_minutes: capOf(maxMinutes),
        max_tokens: maxTokens.trim() ? Math.round(Number(maxTokens) * 1000) : null,
      },
    };
    return api.approveRun(run.id, {
      policy: settled,
      max_parallel: run.in_place ? null : capOf(maxParallel),
      assignees,
    });
  }

  const pick = (task: PlannedTask) => (
    <select className="who-pick" value={pickOf(task)} disabled={busy} onChange={(e) => setPicks({ ...picks, [task.key]: e.target.value })}>
      <option value="">自动{fallback ? ` · ${displayName(fallback)}` : ""}</option>
      {teammates.length > 0 && (
        <optgroup label="队友">
          {teammates.map((a) => (
            <option key={a.id} value={`agent:${a.id}`}>
              {displayName(a)} · {prettyModel(a.model ?? a.plugin)}
            </option>
          ))}
        </optgroup>
      )}
      {models.length > 0 && (
        <optgroup label="新建（用主导的登录）">
          {models.map((m) => (
            <option key={m} value={`model:${m}`}>
              {prettyModel(m)}
            </option>
          ))}
        </optgroup>
      )}
    </select>
  );
  const capsText = budgetText({ max_usd: capOf(maxUsd), max_minutes: capOf(maxMinutes), max_tokens: maxTokens.trim() ? Number(maxTokens) * 1000 : null });

  const settings = (
      <div className="plan-settings">
        <div className="row">
          <span className="muted small">托管</span>
          <div className="segmented">
            {PRESETS.map((p) => (
              <button key={p.key} className={preset === p.key ? "active" : ""} title={p.hint} disabled={busy} onClick={() => setPreset(p.key)}>
                {p.label}
              </button>
            ))}
          </div>
        </div>
        <div className="small muted">
          {PRESETS.find((p) => p.key === preset)?.hint}
          {preset === "full" && "。只对这一次生效"}
          {preset === "full" && !capsText && "，没有设上限"}
        </div>
        <details>
          <summary className="small muted">更多{capsText ? ` · ${capsText}` : ""}</summary>
          <div className="caps">
            <label>
              <span className="small muted">预算 $</span>
              <input type="number" min={0} step="0.5" value={maxUsd} onChange={(e) => setMaxUsd(e.target.value)} placeholder="不限" />
            </label>
            <label>
              <span className="small muted">时间（分钟）</span>
              <input type="number" min={1} value={maxMinutes} onChange={(e) => setMaxMinutes(e.target.value)} placeholder="不限" />
            </label>
            <label>
              <span className="small muted">Token（千）</span>
              <input type="number" min={1} value={maxTokens} onChange={(e) => setMaxTokens(e.target.value)} placeholder="不限" />
            </label>
            {!run.in_place && (
              <label>
                <span className="small muted">并行数</span>
                <input type="number" min={1} value={maxParallel} onChange={(e) => setMaxParallel(e.target.value)} />
              </label>
            )}
          </div>
        </details>
      </div>
  );

  return { pick, settings, approve, ready: !!agents.data };
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

/** A task's details: who, attempts, failure, question, review, summary, changes, checks, spec. */
export function TaskDetail({ runId, taskId }: { runId: string; taskId: string }) {
  const detail = useRun(runId);
  const agents = useAgents();
  const task = detail.data?.tasks.find((t) => t.id === taskId || t.key === taskId);

  return (
    <div className="task-detail">
      {!detail.data && (detail.error ? <ErrorBox error={detail.error} /> : <p className="muted">加载中…</p>)}
      {detail.data && !task && <ErrorBox error={`这个 run 里没有任务 ${taskId}`} />}
      {task && (
        <>
          <div className="row" style={{ gap: 8 }}>
            <code>#{task.key}</code>
            <StatusChip status={task.status} tone={taskTone(task.status)} />
            <span className="spacer" />
            <a className="small" href={`#/runs/${detail.data!.run.id}/tasks/${task.id}`}>
              {detail.data!.run.title}
            </a>
          </div>
          <h3 style={{ marginTop: 10 }}>{task.title}</h3>
          <dl className="kv small">
            <dt>Worker</dt>
            <dd>
              {task.assignee ? <a href={sessionHref(task.assignee)}>{agentName(agents.data, task.assignee)}</a> : "—"}
              {task.escalated && <span className="chip warn" style={{ marginLeft: 6 }}>已升级</span>}
            </dd>
            {task.reviewer && (
              <>
                <dt>Reviewer</dt>
                <dd>
                  <a href={sessionHref(task.reviewer)}>{agentName(agents.data, task.reviewer)}</a>
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
            <dd title={time(task.updated_at)}>{ago(task.updated_at)}</dd>
          </dl>

          {task.handoffs.length > 0 && (
            <Section title="交接">
              <HandoffChain handoffs={task.handoffs} agents={agents.data} open={task.handoffs[task.handoffs.length - 1].id} />
            </Section>
          )}
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
          <TaskChanges task={task} agents={agents.data ?? []} title={(children) => <Section title="改动">{children}</Section>} />
          {task.worktree && !detail.data!.run.in_place && (
            <Section title="审查">
              <TaskReview run={detail.data!.run} task={task} />
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

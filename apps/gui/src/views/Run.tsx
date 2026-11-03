import { useEffect, useState, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { Task } from "@bindings/Task";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { TaskSpec } from "@bindings/TaskSpec";
import type { PlannedTask } from "@bindings/PlannedTask";
import type { RunPolicy } from "@bindings/RunPolicy";
import type { UpdateRunPolicyRequest } from "@bindings/UpdateRunPolicyRequest";
import { navigate } from "../App";
import { ApiError, api, errorText } from "../api";
import { DeleteRunDialog } from "../deleteRun";
import { HandoffChain, OutcomeChip, freshLabel, isEscalated } from "../handoff";
import {
  Avatar,
  ErrorBox,
  FileScopeProvider,
  Icon,
  List,
  OneLine,
  Prose,
  StatusChip,
  agentName,
  agentTitle,
  ago,
  beyondFirstLine,
  displayName,
  firstSentence,
  isAgent,
  runTone,
  sessionHref,
  taskTone,
  time,
  useAgents,
  VendorMark,
} from "../components";
import { ChipMenu, type MenuItem } from "../menu";
import { sessionStateKey } from "../lib/sessionChoice";
import { onHubEvent, useHubData, type HubEvent } from "../hub";
import { BUCKET_LABEL, groupTasks, progress, taskState } from "../lib/taskState";
import { PRESETS, budgetText, presetOf, presetPolicy, type Preset } from "../lib/policy";
import {
  MAX_ATTEMPTS_LIMIT,
  MAX_INTERVAL_SECONDS,
  MAX_STUCK_MINUTES,
  canReset,
  draftOf,
  draftProblems,
  failureText,
  isExhausted,
  policyBody,
  resetBody,
  type ContinuationField,
} from "../lib/continuation";
import { accountOf, effortOf, prettyModel } from "../lib/models";
import { modelPickValue, parseModelPick, reasoningWorkerRequest, workerAgentRequest, workerModelGroups, workerModelLabel, workerTarget, type ReasoningChoice } from "../lib/workerModels";
import { ReasoningPicks } from "../reasoningPicks";
import { cursorMode, reasoningModels } from "../lib/reasoning";
import { StateWord, TaskChanges } from "../parts";
import { RunChanges, TaskReview } from "../review";
import { RunTraceView } from "../trace";
import { RailCard } from "../rail";
import { t, type Key } from "../lib/i18n";
import { wsKey } from "../workspace";
import { openTool } from "../panes";
import { useQuota } from "./Quota";
import { SessionView, useSideSlot } from "./Session";

function concerns(runId: string, e: HubEvent): boolean {
  switch (e.type) {
    case "run.continuation_changed":
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
/** A continuation phase's words: the dictionary key for it. */
const CONTINUATION_LABELS: Record<string, Key> = {
  ended: "continuation.phase.ended", paused: "continuation.phase.paused", waiting_approval: "continuation.phase.waiting_approval",
  unavailable: "continuation.phase.unavailable", waiting_human: "continuation.phase.waiting_human", working: "continuation.phase.working",
  waiting_tasks: "continuation.phase.waiting_tasks", waiting_checks: "continuation.phase.waiting_checks", finalize: "continuation.phase.finalize",
  continue: "continuation.phase.continue", recover: "continuation.phase.recover", exhausted: "continuation.phase.exhausted",
};

/** What a continuation's phase says, in words; a phase the table lacks is shown as the hub sent it. */
const continuationLabel = (phase: string) => (CONTINUATION_LABELS[phase] ? t(CONTINUATION_LABELS[phase]) : phase);

/**
 * A run is its lead's conversation (docs/tasks/W1.md §8). The conversation keeps only what the
 * person has to answer — the questions above the composer, the plan waiting for approval, the
 * landing buttons, a run that paused or failed. The run's structure — tasks and their hands, the
 * plan before it is split, who takes part, the notes, the trace, the policy — is in the rail
 * beside it (a drawer when the window is too narrow for one).
 */
export function RunView({
  runId,
  selectedTask,
  confirmDelete,
}: {
  runId: string;
  selectedTask?: string;
  confirmDelete?: boolean;
}) {
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
  const allEnded = tasks.length > 0 && tasks.every((t) => ["merged", "failed", "cancelled"].includes(t.status));

  const header = (
    <div className="topbar">
      <span className="title">{run.title}</span>
      <StatusChip status={run.status} tone={runTone(run.status)} />
      {/* Narrow, the header's panel button already carries the progress: one copy. */}
      <ProgressChip tasks={tasks} />
      <div className="spacer" />
      <SidePanelToggle tasks={tasks} />
      {!run.in_place && (
        <button className="ghost small graph-toggle" onClick={() => openTool("graph")} title="在右侧工具列看这个 run 的分支和提交">
          <span className="row" style={{ gap: 5 }}>
            <Icon name="branch" size={13} />
            提交图
          </span>
        </button>
      )}
      <LeadChip agent={plannerAgent} name={planner} href={sessionHref(run.planner)} />
      <button
        className="danger"
        title={ACTIVE.includes(run.status) ? "先取消，再删除它的一切" : "删除这个 run、它的任务、agent 和工作区"}
        onClick={() => setDeleting(true)}
      >
        {ACTIVE.includes(run.status) ? "取消并删除" : "删除"}
      </button>
    </div>
  );

  // Only what needs the person: a run that paused, one that ended badly. Both read as one line
  // and open for the whole note; everything else about the run is in the rail.
  const top = (
    <div className="run-head">
      {run.status === "paused" && (
        <OneLine tone="warn" icon={<span aria-hidden="true">❚❚</span>} title="已暂停" summary={firstSentence(run.note ?? "等你决定")}>
          {`${run.note ?? "等你决定"}。在「问你」里选择加预算继续，或取消这个 run。`}
        </OneLine>
      )}
      {run.note && !["awaiting_approval", "paused", "planning"].includes(run.status) && (
        <OneLine tone="danger" icon={<Icon name="close" size={13} />} title="run 的说明" summary={firstSentence(run.note)}>
          {beyondFirstLine(run.note)}
        </OneLine>
      )}
      <ErrorBox error={detail.error} />
    </div>
  );

  const bottom =
    run.status === "completed" || (run.status === "running" && allEnded) ? <Ending run={run} closing={run.status === "running"} onDone={detail.reload} /> : undefined;

  const side = <RunRail run={run} tasks={tasks} agents={agents.data} selectedTask={selectedTask} onPolicyChange={detail.reload} />;

  return (
    <FileScopeProvider value={{ kind: "run", run: run.id }}>
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
    </FileScopeProvider>
  );
}

/**
 * Who took part (docs/tasks/S1.md §3.5): each task's executions in turn, from its handoff
 * cards — the session (linked; an archived execution session opens too, and can be restored),
 * whether it was started for the task or worked on it itself, and how it went. The run owns
 * none of them: they belong to the workspace.
 */
function Participants({ tasks, agents }: { tasks: Task[]; agents?: Agent[] }) {
  const [error, setError] = useState<string>();
  const worked = tasks.filter((t) => t.handoffs.length > 0);
  if (!worked.length) return null;
  async function restore(id: string) {
    setError(undefined);
    try {
      await api.archiveAgent(id, false);
    } catch (err) {
      setError(errorText(err));
    }
  }
  return (
    <div className="participants">
      <ErrorBox error={error} />
      {worked.map((task) => (
        <div key={task.id} className="part-task">
          <div className="part-head">
            <code className="board-key">#{task.key}</code> <span>{task.title}</span>
          </div>
          <ul className="plain part-list">
            {task.handoffs.map((h, i) => {
              const who = agents?.find((a) => a.id === h.to);
              return (
                <li key={h.id} className="part-row">
                  <span className="muted small">执行 {i + 1}</span>
                  <a className="hand-who" href={sessionHref(h.to)} title="打开这个会话">
                    {who && <Avatar agent={who} size={16} />}
                    {who ? displayName(who) : h.to_name || "?"}
                  </a>
                  <span className="chip">{freshLabel(h)}</span>
                  <OutcomeChip handoff={h} />
                  {who?.archived_at && (
                    <>
                      <span className="chip">已归档</span>
                      <button className="ghost small" onClick={() => void restore(h.to)} title="恢复后可以再被指定用本人（fresh: false）">
                        恢复
                      </button>
                    </>
                  )}
                  {!who && <span className="muted small">会话已删除</span>}
                  <span className="spacer" />
                  <span className="muted small" title={time(h.at)}>
                    {ago(h.at)}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </div>
  );
}

/** The run's tasks by what they need, each opening its details beside. */
function TaskBoard({ run, tasks, agents, selectedTask }: { run: Run; tasks: Task[]; agents?: Agent[]; selectedTask?: string }) {
  return (
    <section className="task-board">
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
                    {isEscalated(task) && " · 已升级"}
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

/** "2/4 merged", while the rail beside the conversation shows the tasks anyway. */
function ProgressChip({ tasks }: { tasks: Task[] }) {
  const { narrow } = useSideSlot();
  if (narrow || !tasks.length) return null;
  const done = tasks.filter((t) => t.status === "merged").length;
  return (
    <span className="chip">
      {done}/{tasks.length} merged
    </span>
  );
}

/** The lead, when the header has room for it: the conversation is the lead's anyway. */
function LeadChip({ agent, name, href }: { agent?: Agent; name: string; href: string }) {
  const { narrow } = useSideSlot();
  if (narrow) return null;
  return agent ? <Who agent={agent} label="主导" /> : <a className="chip" href={href}>主导 · {name}</a>;
}

/**
 * The header's way into the run's panels: when the window is too narrow (or the rail is turned
 * off) to keep them beside the conversation, this opens them over it — with the task progress, so
 * the header still says how the run is doing without them.
 */
function SidePanelToggle({ tasks }: { tasks: Task[] }) {
  const { narrow, open, toggle } = useSideSlot();
  if (!narrow) return null;
  const done = tasks.filter((t) => t.status === "merged").length;
  return (
    <button
      className={`ghost small side-toggle ${open ? "on" : ""}`}
      onClick={toggle}
      aria-label={open ? "收起 run 的面板" : "打开 run 的面板"}
      aria-expanded={open}
      title="任务、计划、参与者、笔记、追溯和策略"
    >
      <Icon name="panel" size={14} />
      {tasks.length > 0 ? `任务 ${done}/${tasks.length}` : "面板"}
    </button>
  );
}

/**
 * The run's structure, in the rail beside its conversation: what the tasks are doing (and whose
 * hands they passed through), the plan while it is not split into tasks yet, where the run works
 * and on what policy, who took part, what the agents noted, and the trace. Each section is a
 * panel that folds by its heading; the trace and the participants start folded.
 */
function RunRail({
  run,
  tasks,
  agents,
  selectedTask,
  onPolicyChange,
}: {
  run: Run;
  tasks: Task[];
  agents?: Agent[];
  selectedTask?: string;
  onPolicyChange: () => void;
}) {
  return (
    <>
      {tasks.length > 0 ? (
        <RailCard id="run-tasks" title="任务" aside={progress(tasks)}>
          <TaskBoard run={run} tasks={tasks} agents={agents} selectedTask={selectedTask} />
        </RailCard>
      ) : (
        run.plan.length > 0 && (
          <RailCard id="run-plan" title="计划" aside={`${run.plan.length} 个任务`}>
            <PlanList run={run} />
          </RailCard>
        )
      )}
      <RailCard id="run-facts" title="run">
        <RunFacts run={run} agents={agents} />
      </RailCard>
      <RailCard id="run-policy" title="策略与预算">
        <PolicyChip run={run} />
        <ContinuationMeta key={run.id} run={run} agents={agents} onDone={onPolicyChange} />
      </RailCard>
      {tasks.some((t) => t.handoffs.length > 0) && (
        <RailCard id="run-participants" title="参与者" defaultOpen={false}>
          <Participants tasks={tasks} agents={agents} />
        </RailCard>
      )}
      <RunNotes runId={run.id} />
      {tasks.length > 0 && (
        <RailCard id="run-trace" title="追溯" defaultOpen={false}>
          <RunTraceView runId={run.id} />
        </RailCard>
      )}
    </>
  );
}

/** Where the run works and who works in it, one line each. */
function RunFacts({ run, agents }: { run: Run; agents?: Agent[] }) {
  return (
    <div className="rail-facts">
      {run.in_place ? (
        <InPlaceNote run={run} />
      ) : (
        <div className="rail-line" title={`${run.base_branch} → ${run.branch}`}>
          <Icon name="branch" size={14} />
          <span className="rail-main">
            <code>{run.base_branch}</code> → <code>{run.branch}</code>
          </span>
        </div>
      )}
      <div className="rail-line" title={run.repo}>
        <Icon name="folder" size={14} />
        <span className="rail-main">{run.repo}</span>
      </div>
      <div className="rail-line">
        <Icon name="team" size={14} />
        <span className="rail-main">
          默认 {agentName(agents, run.config.worker)}
          {run.config.escalate && ` → 升级 ${agentName(agents, run.config.escalate)}`} · 并行 {run.config.max_parallel}
        </span>
      </div>
      <div className="rail-line">
        <TeamStack ids={run.config.team.length ? run.config.team : [run.config.worker]} agents={agents} />
      </div>
      <div className="rail-line" title={time(run.created_at)}>
        <span className="rail-main muted">创建于 {ago(run.created_at)}</span>
      </div>
    </div>
  );
}

/** What to say about a limit the person has to fix before the policy can be saved. */
const INVALID_FIELD: Record<ContinuationField, Key> = {
  maxAttempts: "continuation.invalidMax",
  intervalSeconds: "continuation.invalidInterval",
  stuckMinutes: "continuation.invalidStuck",
};

/**
 * How the run goes on by itself (docs/CONTINUATION.md): the hub's facts about it, and the
 * person's hand on the policy — on or off, its limits, and, once the wakeups ran out, its
 * allowance handed out again. Nothing here happens by itself; a refusal is shown, not
 * swallowed.
 */
function ContinuationMeta({ run, agents, onDone }: { run: Run; agents?: Agent[]; onDone: () => void }) {
  const state = run.continuation;
  const exhausted = isExhausted(run);
  const [open, setOpen] = useState(exhausted);
  const [draft, setDraft] = useState(() => draftOf(run));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [note, setNote] = useState<Key>();
  const problems = draftProblems(draft);

  async function act(body: UpdateRunPolicyRequest, done: Key, failed: Key) {
    setBusy(true);
    setError(undefined);
    setNote(undefined);
    try {
      await api.updateRunPolicy(run.id, body);
      setNote(done);
      onDone();
    } catch (err) {
      setError(t(failed, { error: failureText(err instanceof ApiError ? err.status : 0, errorText(err)) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <details
      className={exhausted ? "warn-text" : "muted"}
      open={open}
      onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}
    >
      <summary>
        {state
          ? t("continuation.summary", {
              phase: continuationLabel(state.phase),
              attempts: state.attempts,
              max: run.config.policy.continuation?.max_attempts ?? 0,
            })
          : t("continuation.off")}
      </summary>
      {state && (
        <>
          <p>{state.next_step}</p>
          <p>{t("continuation.owner", { name: agentName(agents, state.owner) })}</p>
          {state.next_at && <p>{t("continuation.nextCheck", { time: time(state.next_at) })}</p>}
          {state.delivery_error && <p>{t("continuation.deliveryError", { error: state.delivery_error })}</p>}
        </>
      )}
      <div className="row row-top">
        <div className="field">
          <span>{t("continuation.title")}</span>
          <label className="field-check">
            <input
              type="checkbox"
              checked={draft.enabled}
              disabled={busy}
              onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}
            />
            {t("continuation.enable")}
          </label>
          <span className="muted small">{t("continuation.hint")}</span>
        </div>
        <label className="field">
          <span>{t("continuation.maxAttempts")}</span>
          <input
            type="number"
            min={1}
            max={MAX_ATTEMPTS_LIMIT}
            value={draft.maxAttempts}
            disabled={busy || !draft.enabled}
            onChange={(e) => setDraft({ ...draft, maxAttempts: e.target.value })}
          />
        </label>
        <label className="field">
          <span>{t("continuation.interval")}</span>
          <input
            type="number"
            min={1}
            max={MAX_INTERVAL_SECONDS}
            value={draft.intervalSeconds}
            disabled={busy || !draft.enabled}
            onChange={(e) => setDraft({ ...draft, intervalSeconds: e.target.value })}
          />
        </label>
        <label className="field">
          <span>{t("continuation.stuck")}</span>
          <input
            type="number"
            min={1}
            max={MAX_STUCK_MINUTES}
            placeholder={t("continuation.stuckOff")}
            value={draft.stuckMinutes}
            disabled={busy || !draft.enabled}
            onChange={(e) => setDraft({ ...draft, stuckMinutes: e.target.value })}
          />
          <span className="muted small">{t("continuation.stuckHint")}</span>
        </label>
      </div>
      {problems.length > 0 && (
        <p className="warn-text small">
          {problems.map((field) => t(INVALID_FIELD[field])).join(" ")}
        </p>
      )}
      <div className="row">
        <button
          className={exhausted ? "ghost small" : "primary small"}
          disabled={busy || problems.length > 0}
          title={problems.length > 0 ? t("continuation.saveDisabled") : t("continuation.hint")}
          onClick={() => {
            const body = policyBody(draft);
            if (body) void act(body, "continuation.saved", "continuation.saveFailed");
          }}
        >
          {t("continuation.save")}
        </button>
        <button
          className={exhausted ? "primary small" : "ghost small"}
          disabled={busy || !canReset(run)}
          title={canReset(run) ? t("continuation.resetHint") : t("continuation.resetDisabled")}
          onClick={() => void act(resetBody(), "continuation.resetDone", "continuation.resetFailed")}
        >
          {t("continuation.reset")}
        </button>
        {note && <span className="small muted">{t(note)}</span>}
      </div>
      {!canReset(run) && <p className="small muted">{t("continuation.resetDisabled")}</p>}
      <ErrorBox error={error} />
    </details>
  );
}

/**
 * The end of a run: while the lead wraps up (every task ended), or once it is done. One line says
 * how it went and carries the buttons the person has to press — land it, close it; the summary,
 * where the result landed and the whole diff open under it.
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
  // The line's gist: what the lead concluded, else where it ended.
  const gist = run.summary
    ? firstSentence(run.summary)
    : closing
      ? "所有任务都已结束，等主导落地和写总结"
      : landed
        ? `已落地到 ${run.base_branch}`
        : run.in_place
          ? "改动已直接写在文件夹里"
          : empty
            ? "没有改动，不用落地"
            : `结果在 ${run.branch}，还没进 ${run.base_branch}`;
  return (
    <div className="done-line">
      <OneLine tone={closing ? "warn" : "ok"} icon={<span className="done-glyph" aria-hidden="true">{closing ? "○" : "✓"}</span>} title={closing ? "收尾中" : "完成"} summary={gist}>
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
        {run.summary && <Prose text={run.summary} />}
        <ErrorBox error={error} />
      </OneLine>
      <div className="done-actions">
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
    <RailCard id="run-notes" title="笔记" aside={String(list.length)}>
      {list.map((n) => (
        <div key={n.key} className="note">
          <code>{n.key}</code>
          <Prose text={n.text} />
        </div>
      ))}
    </RailCard>
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
  const quota = useQuota();
  // What each workspace session is doing and did last, so the person can pick by it.
  const candidates = useHubData(
    () => api.sessionCandidates(run.repo).then((r) => r.sessions),
    [run.repo],
    (e) => e.type.startsWith("agent.") || e.type === "run.status_changed" || e.type === "task.handed_off",
  );
  const policy = run.config.policy;
  const [preset, setPreset] = useState<Preset>(presetOf(policy) ?? "manual");
  // Per task: "" is the run's default worker, `agent:<id>` a teammate, `model:<plugin>/<model>`
  // a new worker on the model's own plugin and a login of it.
  const [picks, setPicks] = useState<Record<string, string>>({});
  const [reasoning, setReasoning] = useState<Record<string, ReasoningChoice>>({});
  const [maxUsd, setMaxUsd] = useState(policy.budget.max_usd?.toString() ?? "");
  const [maxMinutes, setMaxMinutes] = useState(policy.budget.max_minutes?.toString() ?? "");
  const [maxTokens, setMaxTokens] = useState(policy.budget.max_tokens != null ? String(policy.budget.max_tokens / 1000) : "");
  const [maxParallel, setMaxParallel] = useState(String(run.config.max_parallel));

  const everyone = agents.data ?? [];
  const lead = everyone.find((a) => a.id === run.planner);
  const fallback = everyone.find((a) => a.id === run.config.worker);
  // The run's team and the workspace's other sessions, chats included: any session may take a
  // task (docs/AGENT-WORKSPACE.md §2.1). Not its lead, nor the conversation that started it.
  const teammates = everyone.filter(
    (a) => a.id !== run.planner && a.id !== run.origin_chat_id && !a.archived_at && (run.config.team.includes(a.id) || wsKey(a.cwd) === wsKey(run.repo)),
  );
  const accounts = quota.data?.accounts ?? [];
  const leadRef = lead ? { plugin: lead.plugin, account: accountOf(lead) } : undefined;
  // Every installed plugin's models, by family; a plugin with no signed-in login stays listed
  // but disabled, with the reason.
  const workerGroups = workerModelGroups(plugins.data ?? [], accounts, leadRef);
  const workerOptions = workerGroups.flatMap((g) => g.options);
  const planned = (task: PlannedTask) => {
    const agent = task.assignee ? everyone.find((a) => isAgent(a, task.assignee!)) : undefined;
    return agent ? `agent:${agent.id}` : "";
  };
  const pickOf = (task: PlannedTask) => picks[task.key] ?? planned(task);

  /** The worker a pick stands for: a teammate, or a new agent on the model's own plugin. */
  async function workerOf(task: PlannedTask, made: Map<string, string>): Promise<string | null> {
    const pick = pickOf(task);
    const override = task.fresh || parseModelPick(pick) ? reasoning[task.key] : undefined;
    if (override) {
      const parsed = parseModelPick(pick);
      const template = pick.startsWith("agent:") ? everyone.find((a) => a.id === pick.slice(6)) : !pick ? fallback : undefined;
      const base = template ?? (parsed ? workerAgentRequest(workerTarget(parsed.plugin, parsed.model, accounts, leadRef), run.repo) : undefined);
      if (!base) throw new Error("无法确定任务执行者，请重新选择");
      const body = reasoningWorkerRequest(base, run.repo, override);
      const key = JSON.stringify([pick, body]);
      const known = made.get(key);
      if (known) return known;
      const id = (await api.createAgent(body)).id;
      made.set(key, id);
      return id;
    }
    if (!pick) return null;
    if (pick.startsWith("agent:")) return pick.slice("agent:".length);
    const parsed = parseModelPick(pick);
    if (!parsed) throw new Error(t("worker.unknownPick", { pick }));
    const known = made.get(pick);
    if (known) return known;
    // The model's plugin decides, not the lead's: a DeepSeek model belongs to the plugin that
    // offers it, and only a same-plugin worker may use the lead's login.
    const target = workerTarget(parsed.plugin, parsed.model, accounts, leadRef);
    const found = everyone.find(
      (a) => a.id !== run.planner && a.plugin === target.plugin && a.model === target.model && accountOf(a) === target.account && wsKey(a.cwd) === wsKey(run.repo),
    );
    const id = found?.id ?? (await api.createAgent(workerAgentRequest(target, run.repo))).id;
    made.set(pick, id);
    return id;
  }

  async function approve(): Promise<RunDetail> {
    const made = new Map<string, string>();
    const assignees: Record<string, string | null> = {};
    for (const task of run.plan) assignees[task.key] = await workerOf(task, made);
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

  /** "Glm 5.3 Flash · 空闲 · run "…" #a (done)"; only the model when the hub says nothing. */
  const teammateSub = (a: Agent) => {
    const parts = [prettyModel(a.model ?? a.plugin)];
    const c = candidates.data?.find((s) => s.agent?.id === a.id);
    if (c) {
      parts.push(t(sessionStateKey(c.state)));
      if (c.last_execution) parts.push(c.last_execution);
    }
    return parts.join(" · ");
  };
  const autoLabel = `自动${fallback ? ` · ${displayName(fallback)}` : ""}`;

  const pick = (task: PlannedTask) => {
    const value = pickOf(task);
    const selectedModel = parseModelPick(value);
    const offered = new Set((plugins.data ?? []).flatMap((p) => reasoningModels(p.name, p.models, selectedModel?.plugin === p.name ? selectedModel.model : undefined).map((m) => modelPickValue(p.name, m))));
    const set = (v: string) => {
      setPicks({ ...picks, [task.key]: v });
      setReasoning((prev) => { const next = { ...prev }; delete next[task.key]; return next; });
    };
    const items: MenuItem[] = [
      { kind: "item", key: "auto", label: autoLabel, sub: "默认 worker", subDir: "ltr", icon: fallback ? <Avatar agent={fallback} size={18} /> : undefined, on: value === "", onPick: () => set("") },
      ...(teammates.length ? [{ kind: "header", key: "h-team", label: "队友" } satisfies MenuItem] : []),
      ...teammates.map(
        (a): MenuItem => ({
          kind: "item",
          key: `agent:${a.id}`,
          label: displayName(a),
          sub: teammateSub(a),
          subDir: "ltr",
          icon: <Avatar agent={a} size={18} />,
          on: value === `agent:${a.id}`,
          onPick: () => set(`agent:${a.id}`),
        }),
      ),
      ...workerGroups.flatMap((group): MenuItem[] => [
        { kind: "header", key: `h-${group.key}`, label: t("plan.newWorkerFamily", { family: group.label }) },
        ...group.options.filter((o) => offered.has(modelPickValue(o.plugin, o.model))).map((option): MenuItem => {
          const v = modelPickValue(option.plugin, option.model);
          return {
            kind: "item",
            key: v,
            label: option.plugin === "cursor" ? `${prettyModel(cursorMode(option.model).base)} · Cursor` : workerModelLabel(option, workerOptions, accounts),
            sub: option.blocked ? t(option.blocked.key) : option.model,
            ...(option.blocked ? { subDir: "ltr" as const } : {}),
            icon: <VendorMark plugin={option.plugin} size={14} />,
            on: value === v,
            disabled: !!option.blocked,
            onPick: () => set(v),
          };
        }),
      ]),
    ];
    // The trigger shows the pick: a teammate's face and name, a new model's vendor and model.
    const chosen = value.startsWith("agent:") ? everyone.find((a) => a.id === value.slice("agent:".length)) : undefined;
    const model = parseModelPick(value);
    const option = model && workerOptions.find((o) => o.plugin === model.plugin && o.model === model.model);
    const template = chosen ?? (!value ? fallback : undefined);
    const plugin = plugins.data?.find((p) => p.name === (model?.plugin ?? template?.plugin));
    const setting = reasoning[task.key] ?? { model: model?.model ?? template?.model ?? null, effort: template ? effortOf(template.config) ?? "" : "" };
    const icon = chosen ? (
      <Avatar agent={chosen} size={16} />
    ) : model ? (
      <VendorMark plugin={model.plugin} size={14} />
    ) : fallback ? (
      <Avatar agent={fallback} size={16} />
    ) : undefined;
    const label = chosen ? displayName(chosen) : model ? model.plugin === "cursor" ? `${prettyModel(cursorMode(model.model).base)} · Cursor` : (option ? workerModelLabel(option, workerOptions, accounts) : prettyModel(model.model)) : autoLabel;
    if (busy)
      return (
        <span className="who-pick">
          <button type="button" className="chip-pick" disabled>
            {icon}
            <span className="chip-label">{label}</span>
          </button>
        </span>
      );
    return (
      <span className="who-pick" data-task={task.key}>
        <ChipMenu icon={icon} label={label} title="执行者" items={items} align="right" searchable />
        {task.fresh || model ? <ReasoningPicks plugin={plugin} model={setting.model} effort={setting.effort} onChange={(model, effort) => setReasoning({ ...reasoning, [task.key]: { model, effort } })} />
          : <span className="small muted" title="该任务复用原会话及其推理设置，可在原会话的输入框旁调整">沿用会话推理设置</span>}
      </span>
    );
  };
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
        // Paths in the task's words are its worktree's; once that is gone, the run's branch has them.
        <FileScopeProvider value={task.worktree ? { kind: "task", run: detail.data!.run.id, task: task.id } : { kind: "run", run: detail.data!.run.id }}>
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
              {isEscalated(task) && <span className="chip warn" style={{ marginLeft: 6 }}>已升级</span>}
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
        </FileScopeProvider>
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

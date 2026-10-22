// Pieces shared by the conversation, the run view and the strips above the composer: state
// words, task cards, compact orchestration / agent message rows, two-click confirm.

import { useEffect, useRef, useState, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import type { Run } from "@bindings/Run";
import type { Task } from "@bindings/Task";
import type { TranscriptTurn } from "@bindings/TranscriptTurn";
import { useActivity } from "./activity";
import { api } from "./api";
import { Avatar, DiffStat, Prose, agentTitle, ago, displayName, sessionHref } from "./components";
import { ChangeStat } from "./diffview";
import { useHubData } from "./hub";
import { sumStats } from "./lib/diff";
import { taskState, type State } from "./lib/taskState";
import { clock } from "./lib/time";
import { duration } from "./lib/workGroups";
import { openBeside, openTool } from "./panes";

/** A state as glyph + word; a moving one breathes. */
export function StateWord({ state, children }: { state: State; children?: ReactNode }) {
  return (
    <span className={`state ${state.tone}`}>
      <span className={`glyph ${state.live ? "live" : ""}`} aria-hidden="true">
        {state.glyph}
      </span>
      {state.word}
      {children}
    </span>
  );
}

/**
 * Danger in two clicks instead of a dialog: the first arms the control (it turns red and says
 * so), the second runs the action. Disarms by itself after a few seconds.
 */
export function useArmed(ms = 3000): { armed: boolean; click: (action: () => void) => void } {
  const [armed, setArmed] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return {
    armed,
    click(action) {
      clearTimeout(timer.current);
      if (armed) {
        setArmed(false);
        action();
      } else {
        setArmed(true);
        timer.current = setTimeout(() => setArmed(false), ms);
      }
    },
  };
}

/** Who does a task: the live worker, else the teammate it went to, else the run's default. */
export function taskOwner(task: Task, run: Run, agents: Agent[]): Agent | undefined {
  const find = (id: string | null) => (id ? agents.find((a) => a.id === id) : undefined);
  return find(task.assignee) ?? find(task.worker_template) ?? find(run.config.worker);
}

/** Finds a task (and its run) among the loaded runs. */
export function findTask(tree: RunDetail[] | undefined, taskId: string | undefined): { run: Run; task: Task; tasks: Task[] } | undefined {
  if (!tree || !taskId) return undefined;
  for (const d of tree) {
    const task = d.tasks.find((t) => t.id === taskId);
    if (task) return { run: d.run, task, tasks: d.tasks };
  }
  return undefined;
}

/** Running time while it moves, else when it last changed. */
function taskTime(task: Task, live: boolean, now: number): string {
  return live ? duration(now - new Date(task.updated_at).getTime()) : ago(task.updated_at);
}

function useNow(active: boolean, every = 1000): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => setNow(Date.now()), every);
    return () => clearInterval(timer);
  }, [active, every]);
  return now;
}

/**
 * A task as one live line — `[avatar] Rex  #a  title ……… ● 运行中 · 3m ▸` — that opens to its
 * brief, question, review, summary, changes, checks and actions. While it runs, the title gives
 * way to what the worker is doing.
 */
export function TaskCard({ task, run, agents, siblings, open: initial = false }: { task: Task; run: Run; agents: Agent[]; siblings?: Task[]; open?: boolean }) {
  const [open, setOpen] = useState(initial);
  const state = taskState(task, siblings);
  const owner = taskOwner(task, run, agents);
  const doing = useActivity(task.assignee ?? "");
  const now = useNow(state.live);
  // What the worker does right now, else its own progress line, else the title.
  const line = (state.live && doing) || task.progress || task.title;

  return (
    <div className={`task-card ${open ? "open" : ""}`}>
      <button className="tc-line" onClick={() => setOpen(!open)} aria-expanded={open} title={task.title}>
        {owner ? <Avatar agent={owner} size={18} /> : <span className="tc-noone" />}
        <span className="tc-who">{owner ? displayName(owner) : "—"}</span>
        <code className="tc-key">#{task.key}</code>
        {task.parent && siblings && <span className="muted small">↳ #{siblings.find((t) => t.id === task.parent)?.key ?? "?"}</span>}
        <span className={`tc-title ${line !== task.title ? "doing" : ""}`}>{line}</span>
        <StateWord state={state} />
        <span className="tc-time">{taskTime(task, state.live, now)}</span>
        <span className="disclosure" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && <TaskBody task={task} run={run} owner={owner} agents={agents} />}
    </div>
  );
}

function TaskBody({ task, run, owner, agents }: { task: Task; run: Run; owner?: Agent; agents: Agent[] }) {
  const report = task.report;
  const failedChecks = report.checks.filter((c) => !c.passed);
  const reviewer = task.reviewer ? agents.find((a) => a.id === task.reviewer) : undefined;
  return (
    <div className="tc-body">
      <TcSection title="要做什么">
        <div className="tc-goal">
          <Prose text={task.spec.goal} />
        </div>
      </TcSection>
      {report.question && (
        <TcSection title="提问" tone="warn">
          <Prose text={report.question} />
        </TcSection>
      )}
      {report.failure && (
        <TcSection title={task.status === "stalled" ? "做不下去的原因" : "失败原因"} tone="danger">
          <div className="pre small">{report.failure}</div>
        </TcSection>
      )}
      {report.review && (
        <TcSection title={reviewer ? `${displayName(reviewer)} 的审查意见` : "审查意见"}>
          <Prose text={report.review} />
        </TcSection>
      )}
      {report.summary && (
        <TcSection title="总结">
          <Prose text={report.summary} />
        </TcSection>
      )}
      <TaskChanges task={task} agents={agents} title={(children) => <TcSection title="改动">{children}</TcSection>} />
      {report.checks.length > 0 && (
        <TcSection title="验收">
          {failedChecks.length ? (
            failedChecks.map((c, i) => (
              <details key={i} className="check-result" open={i === 0}>
                <summary>
                  <span className="fail">✕</span> <code>{c.command}</code> <span className="muted small">exit {c.exit_code ?? "—"}</span>
                </summary>
                <pre>{c.output || "（无输出）"}</pre>
              </details>
            ))
          ) : (
            <span className="small">
              <span className="pass">✓</span> {report.checks.length} 条验收命令都通过了
            </span>
          )}
        </TcSection>
      )}
      <div className="tc-actions">
        {owner && (
          <>
            <a href={sessionHref(owner.name)}>打开 {displayName(owner)}</a>
            <button className="link" onClick={() => openBeside(owner.name)}>
              在旁边打开
            </button>
          </>
        )}
        <a href={`#/runs/${run.id}/tasks/${task.id}`}>任务详情</a>
        {task.attempts > 1 && <span className="muted">第 {task.attempts} 次尝试</span>}
        {task.escalated && <span className="muted">已升级给更强的模型</span>}
      </div>
    </div>
  );
}

function TcSection({ title, tone, children }: { title: string; tone?: "warn" | "danger"; children: ReactNode }) {
  return (
    <section className={`tc-section ${tone ?? ""}`}>
      <h4>{title}</h4>
      {children}
    </section>
  );
}

/**
 * What a task changed: every turn of its worker that changed files, summed, each one opening
 * its diff in the tool column; then the task's diff stat against the integration branch.
 * Worker clones are removed once a task ends, and with them their turns: only the stat stays.
 */
export function TaskChanges({ task, agents, title }: { task: Task; agents: Agent[]; title: (children: ReactNode) => ReactNode }) {
  const worker = task.assignee ? agents.find((a) => a.id === task.assignee) : undefined;
  const transcript = useHubData(
    () => (worker ? api.transcript(worker.name) : Promise.resolve(undefined)),
    [worker?.name],
    (e) => !!worker && (e.type === "agent.turn_diff" || e.type === "agent.turn_completed") && e.data.id === worker.id,
  );
  const turns = (transcript.data?.turns ?? []).filter((t) => !!t.started_at).map((turn, i) => ({ turn, n: i + 1 }));
  const changed = turns.filter((x) => x.turn.diff);
  const diffStat = task.report.diff_stat;
  if (!changed.length && !diffStat) return null;
  const total = sumStats(changed.map((x) => x.turn.diff!));
  return title(
    <div className="task-turns">
      {worker && changed.length > 0 && (
        <>
          <div className="tt-total small">
            ✎ {changed.length} 轮改了 {total.files.length} 个文件 <ChangeStat files={[total]} />
          </div>
          {changed.map(({ turn, n }) => (
            <button key={turn.message_id} className="tt-row" onClick={() => openTool("changes", { agent: worker.name, turn: turn.message_id })} title="在右侧查看这一轮的改动">
              <span className="tt-n">第 {n} 轮</span>
              <span className="muted">{clock(turn.completed_at ?? turn.started_at!)}</span>
              <span className="tt-files">{turn.diff!.files.map((f) => f.path.split("/").pop()).join(" · ")}</span>
              <ChangeStat files={[turn.diff!]} />
              <span className="disclosure" aria-hidden="true">
                ▸
              </span>
            </button>
          ))}
        </>
      )}
      {diffStat && (
        <details className="tt-stat" open={!changed.length}>
          <summary className="small muted">对集成分支的 diff stat</summary>
          <DiffStat text={diffStat} />
        </details>
      )}
    </div>,
  );
}

function preview(text: string): string {
  // Skip the "Message from alice:" header agent_send adds; the sender is shown already.
  const line =
    text
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l && !/^Message from .*:$/.test(l)) ?? "";
  return line.length > 120 ? line.slice(0, 120) + "…" : line;
}

/**
 * A message from the orchestrator or another agent, as one collapsible line:
 * `↙ 编排 · #a 已合并  first line ›`, or `↙ 来自 [avatar] Rex  first line ›`. Only a person's own
 * messages are bubbles on the right.
 */
export function PeerMessage({ turn, agents, tree }: { turn: TranscriptTurn; agents: Agent[]; tree?: RunDetail[] }) {
  const [open, setOpen] = useState(false);
  const from = turn.from!;
  const sender = from.kind === "agent" ? agents.find((a) => a.id === from.agent_id) : undefined;
  const found = from.kind === "orchestrator" ? findTask(tree, from.task_id) : undefined;
  const run = from.kind === "orchestrator" ? (found?.run ?? tree?.find((d) => d.run.id === from.run_id)?.run) : undefined;

  return (
    <div className={`peer ${open ? "open" : ""}`}>
      <button className="peer-line" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="peer-arrow" aria-hidden="true">
          ↙
        </span>
        {from.kind === "agent" ? (
          <span className="peer-from">
            来自 {sender && <Avatar agent={sender} size={16} />}
            {sender ? displayName(sender) : "另一个 agent"}
          </span>
        ) : from.kind === "notice" ? (
          <span className="peer-from">通知</span>
        ) : (
          <span className="peer-from">编排</span>
        )}
        {turn.reply_excerpt && (
          <span className="peer-reply" title="回复的消息">
            ↩ 回复「{turn.reply_excerpt}」
          </span>
        )}
        {found && (
          <span className="peer-task">
            <code>#{found.task.key}</code>
            <StateWord state={taskState(found.task, found.tasks)} />
          </span>
        )}
        {!found && run && <span className="peer-run">{run.title}</span>}
        <span className="peer-preview">{preview(turn.input)}</span>
        <span className="disclosure" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div className="peer-body">
          {found && <TaskCard task={found.task} run={found.run} agents={agents} siblings={found.tasks} />}
          <div className="peer-text">{turn.input}</div>
          {sender && (
            <div className="small muted">
              {displayName(sender)} · {agentTitle(sender)} · <a href={sessionHref(sender.name)}>打开</a>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

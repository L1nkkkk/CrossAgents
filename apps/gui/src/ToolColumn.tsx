// The tabs around conversations (panes.ts): a browser-like row of them in each dock, on the right
// and under the conversations — 改动 (what each turn changed), 文件 (the workspace's files), 任务
// (a task's details), and 提交图 (commits). 事件流 (raw hub events, for debugging) is offered only
// where debugging is on (panes.ts `PANE_KINDS`). Tabs show the focused conversation, or what was
// opened on purpose (panes.ts `target`); nothing from another conversation mixes in. When docking
// would squeeze the conversation, both docks are one row in a drawer over the page.

import { useEffect, useMemo, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { TurnDiffResponse } from "@bindings/TurnDiffResponse";
import { api, errorText } from "./api";
import { Avatar, ErrorBox, Icon, displayName, isAgent, useAgents } from "./components";
import { CommitGraphView } from "./commitgraph";
import { ChangeStat, DiffView } from "./diffview";
import { useEventLog } from "./eventlog";
import { FilesPane } from "./FileViewer";
import type { HubEvent } from "./hub";
import { parseDiff } from "./lib/diff";
import { clock, full } from "./lib/time";
import { clearTarget, closeBottom, paneLabel, useLayout, type PaneKind, type Target } from "./panes";
import { shownTab, type Dock, type DockId } from "./lib/docks";
import { TabRow } from "./tabs";
import { useWidths } from "./lib/widths";
import { useSession, type TranscriptTurn } from "./session";
import { useSidebar } from "./titlebar";
import { TaskDetail, PlanTasks } from "./views/Run";
import { useHubData } from "./hub";

/**
 * Whether the tool column floats over the page as a drawer: when docking it would leave the
 * conversation narrower than MIN_MAIN, which depends on the sidebar and the dragged widths.
 */
export function useToolsFloat() {
  const { docked } = useSidebar();
  return useWidths(docked).float;
}

/**
 * `dock`: which dock this is. `follow`: the conversation the tabs follow (session routes).
 * `task`: the task the route opened (run routes). `run`: the routed run, whose branches 提交图
 * shows. `onClose` folds the right dock; `onCloseTask` is for closing the route's task's tab.
 */
export function ToolColumn({ dock, follow, task, run, onClose, onCloseTask }: { dock: DockId; follow?: string; task?: { run: string; task: string }; run?: string; onClose: () => void; onCloseTask?: () => void }) {
  const { state, target, floatTab } = useLayout();
  const narrow = useToolsFloat();
  const { right, bottom } = state.docks;
  // Floating, there is one row: the bottom dock's tabs follow the right dock's.
  const shown: Dock<PaneKind> = narrow ? { tabs: [...right.tabs, ...bottom.tabs], active: floatTab ?? right.active, open: true } : state.docks[dock];
  const kind = shownTab(shown);
  return (
    <div className={`tool-column ${narrow ? "narrow" : ""}`} data-tabdock={dock}>
      <section className="tool-pane" aria-label={kind ? paneLabel(kind) : undefined}>
        <header className="tool-head">
          <TabRow dock={dock} tabs={shown.tabs} active={kind} narrow={narrow} onCloseTask={task ? onCloseTask : undefined} />
          {kind && <FollowButton kind={kind} follow={follow} target={target} />}
          {dock === "right" ? (
            <button className="ghost icon" onClick={onClose} title="收起工具列" aria-label="收起工具列">
              <Icon name="panel" size={15} />
            </button>
          ) : (
            <button className="ghost icon" onClick={closeBottom} title="收起下方面板" aria-label="收起下方面板">
              <Icon name="chevron" size={15} />
            </button>
          )}
        </header>
        {kind && <PaneBody kind={kind} follow={follow} target={target} routeTask={task} routeRun={run} />}
      </section>
    </div>
  );
}

/** The target if it may show here: an agent opened on purpose wins over the focused conversation. */
function useSafeTarget(follow: string | undefined, target: Target | null) {
  const agents = useAgents();
  const sameAgent = follow && target?.agent && agents.data?.some((a) => isAgent(a, follow) && isAgent(a, target.agent!));
  return follow && target?.agent !== follow && !sameAgent ? null : target;
}

function FollowButton({ kind, follow, target }: { kind: PaneKind; follow?: string; target: Target | null }) {
  const safeTarget = useSafeTarget(follow, target);
  if (kind === "task" || !safeTarget?.agent || safeTarget.agent === follow) return null;
  return (
    <button className="ghost small" onClick={clearTarget} title="回到当前对话">
      跟随对话
    </button>
  );
}

function PaneBody({ kind, follow, target, routeTask, routeRun }: { kind: PaneKind; follow?: string; target: Target | null; routeTask?: { run: string; task: string }; routeRun?: string }) {
  const safeTarget = useSafeTarget(follow, target);
  const agent = safeTarget?.agent ?? follow;
  return (
    <div className={`tool-body ${kind === "graph" ? "tool-body-graph" : kind === "files" ? "tool-body-files" : ""}`}>
      {kind === "changes" && (agent ? <ChangesPane key={agent} agent={agent} target={target?.agent === agent ? target : null} /> : <Blank text="打开一个会话，这里显示它每一轮改了什么。" />)}
      {kind === "files" &&
        (routeRun ? (
          <FilesPane key={routeRun} scope={{ kind: "run", run: routeRun }} />
        ) : agent ? (
          <FilesPane key={agent} scope={{ kind: "agent", agent }} />
        ) : (
          <Blank text="打开一个会话或 run，这里显示它工作目录里的文件。" />
        ))}
      {kind === "task" && <TaskPane key={follow} follow={follow} target={safeTarget} routeTask={routeTask} />}
      {kind === "events" && (agent ? <EventsPane key={agent} agent={agent} /> : <Blank text="打开一个会话，这里显示它的原始 hub 事件。" />)}
      {kind === "graph" &&
        (routeRun ? (
          <CommitGraphView key={routeRun} scope={{ kind: "run", run: routeRun }} />
        ) : agent ? (
          <CommitGraphView key={agent} scope={{ kind: "agent", agent }} />
        ) : (
          <Blank text="打开一个会话或 run，这里显示它的提交图。" />
        ))}
    </div>
  );
}

function Blank({ text }: { text: string }) {
  return <p className="muted small tool-blank">{text}</p>;
}

/** Who the pane shows, when it is not the conversation in front. */
function Whose({ agent }: { agent?: Agent }) {
  if (!agent) return null;
  return (
    <span className="tool-whose">
      <Avatar agent={agent} size={16} />
      {displayName(agent)}
    </span>
  );
}

type Numbered = { turn: TranscriptTurn; n: number };

/** One turn's diff at a time; the picker lists the turns that changed files. */
function ChangesPane({ agent, target }: { agent: string; target: Target | null }) {
  const { transcript, error } = useSession(agent);
  const [picked, setPicked] = useState<string>();
  const numbered: Numbered[] = (transcript?.turns ?? []).filter((t) => !!t.started_at).map((turn, i) => ({ turn, n: i + 1 }));
  const changed = numbered.filter((x) => x.turn.diff);
  // A turn opened on purpose, else the one picked here, else the latest.
  useEffect(() => setPicked(undefined), [target?.turn]);
  const chosen = changed.find((x) => x.turn.message_id === (picked ?? target?.turn)) ?? changed[changed.length - 1];
  const at = chosen ? changed.indexOf(chosen) : -1;

  if (!transcript) return error ? <ErrorBox error={error} /> : <Blank text="加载中…" />;
  if (!chosen) return <Blank text={`${displayName(transcript.agent)} 还没有改过文件。只读的轮次不显示在这里。`} />;
  const stat = chosen.turn.diff!;
  return (
    <div className="changes-pane">
      <div className="turn-picker">
        <Whose agent={transcript.agent} />
        <button className="ghost icon" disabled={at <= 0} onClick={() => setPicked(changed[at - 1].turn.message_id)} aria-label="上一轮">
          ‹
        </button>
        <select value={chosen.turn.message_id} onChange={(e) => setPicked(e.target.value)} aria-label="选择轮次">
          {changed.map((x) => (
            <option key={x.turn.message_id} value={x.turn.message_id}>
              第 {x.n} 轮 · {clock(x.turn.completed_at ?? x.turn.started_at!)} · {x.turn.diff!.files.length} 个文件 +{x.turn.diff!.added} −{x.turn.diff!.removed}
            </option>
          ))}
        </select>
        <button className="ghost icon" disabled={at >= changed.length - 1} onClick={() => setPicked(changed[at + 1].turn.message_id)} aria-label="下一轮">
          ›
        </button>
      </div>
      <div className="turn-files small muted">
        {stat.files.length} 个文件 <ChangeStat files={[stat]} />
        {stat.source === "hub" && <span title="驱动没有报告改动，由 hub 比较本轮前后的工作区得出"> · hub 算出</span>}
      </div>
      <TurnDiff agent={agent} turn={chosen.turn} file={target?.turn === chosen.turn.message_id ? target.file : undefined} />
    </div>
  );
}

/** Fetches a turn's diff; again when its stat changes (the turn is still running). */
function TurnDiff({ agent, turn, file }: { agent: string; turn: TranscriptTurn; file?: string }) {
  const [state, setState] = useState<{ key: string; data?: TurnDiffResponse; error?: string }>();
  const stat = turn.diff!;
  const key = `${turn.message_id}:${stat.files.length}:${stat.added}:${stat.removed}`;
  useEffect(() => {
    let live = true;
    api.turnDiff(agent, turn.message_id).then(
      (data) => live && setState({ key, data }),
      (err) => live && setState({ key, error: errorText(err) }),
    );
    return () => {
      live = false;
    };
  }, [agent, key]);
  const files = useMemo(() => (state?.data ? parseDiff(state.data.diff) : []), [state?.data]);
  if (state?.error) return <ErrorBox error={state.error} />;
  if (!state?.data) return <Blank text="加载改动…" />;
  return <DiffView files={files} focus={file} />;
}

/** The routed or explicitly selected work, otherwise the execution reported by the hub. */
function TaskPane({ follow, target, routeTask }: { follow?: string; target: Target | null; routeTask?: { run: string; task: string } }) {
  const agents = useAgents();
  const agent = agents.data?.find((a) => follow && isAgent(a, follow));
  const execution = useHubData(() => agent ? api.agentExecution(agent.id) : Promise.resolve(null), [agent?.id], (e) => ["agent.updated", "agent.status_changed", "task.status_changed", "task.handed_off", "run.status_changed"].includes(e.type));
  const x = execution.data?.execution;
  const run = routeTask?.run ?? target?.run ?? x?.run_id;
  const task = routeTask?.task ?? target?.task ?? (target?.run ? undefined : x?.task_id);
  const detail = useHubData(() => run ? api.getRun(run) : Promise.resolve(null), [run], (e) => e.type.startsWith("run.") || e.type === "task.status_changed");
  if (run && task) return <TaskDetail key={task} runId={run} taskId={task} />;
  if (detail.data && detail.data.run.id === run) return <div className="session-plan"><a href={`#/runs/${encodeURIComponent(detail.data.run.id)}`}>{detail.data.run.title}</a><PlanTasks run={detail.data.run} /></div>;
  if (detail.error) return <ErrorBox error={detail.error} />;
  if (run) return <Blank text="正在读取关联工作。" />;
  return <Blank text="当前没有执行中的任务。请从会话的关联工作中选择计划或任务。" />;
}

/** The agent's raw events since the page loaded, newest first, with a filter. */
function EventsPane({ agent }: { agent: string }) {
  const agents = useAgents();
  const who = agents.data?.find((a) => isAgent(a, agent));
  const events = useEventLog(who?.id);
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<number>();
  const f = filter.trim().toLowerCase();
  const shown = events.filter((e) => !f || e.type.includes(f) || JSON.stringify(e).toLowerCase().includes(f)).slice(-300).reverse();
  return (
    <div className="events-pane">
      <div className="events-filter">
        <Whose agent={who} />
        <input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="过滤：类型或内容，如 tool_call" aria-label="过滤事件" />
      </div>
      {!events.length && <Blank text="打开页面以来还没有这个会话的事件。" />}
      <div className="events-list">
        {shown.map((e) => (
          <div key={e.seq} className={`ev ${open === e.seq ? "open" : ""}`}>
            <button className="ev-line" onClick={() => setOpen(open === e.seq ? undefined : e.seq)} title={full(e.ts)}>
              <span className="ev-seq">{e.seq}</span>
              <span className="ev-type">{e.type}</span>
              <span className="ev-sum">{summary(e)}</span>
            </button>
            {open === e.seq && <pre className="ev-json">{JSON.stringify(e, null, 2)}</pre>}
          </div>
        ))}
      </div>
    </div>
  );
}

function summary(e: HubEvent): string {
  const d = (e as { data?: Record<string, unknown> }).data ?? {};
  const pick = ["title", "text", "status", "to", "message", "stop_reason", "stream"].map((k) => d[k]).find((v) => typeof v === "string") as string | undefined;
  return (pick ?? "").replace(/\s+/g, " ").slice(0, 80);
}

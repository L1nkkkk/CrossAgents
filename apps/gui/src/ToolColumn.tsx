// The tabs around conversations (panes.ts): a browser-like row of them in each dock, on the right
// and under the conversations — 改动 (what each turn changed), 文件 (the workspace's files), 任务
// (a task's details), 提交图 (commits), and 后台 (the agents' reminders and watches, the tasks being
// verified). 事件流 (raw hub events, for debugging) is offered only
// where debugging is on (panes.ts `PANE_KINDS`). Tabs show the focused conversation, or what was
// opened on purpose (panes.ts `target`); nothing from another conversation mixes in. When docking
// would squeeze the conversation, both docks are one row in a drawer over the page.

import { useEffect, useMemo, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { TurnDiffResponse } from "@bindings/TurnDiffResponse";
import type { BackgroundReminder } from "@bindings/BackgroundReminder";
import type { BackgroundResponse } from "@bindings/BackgroundResponse";
import type { BackgroundWatch } from "@bindings/BackgroundWatch";
import type { VerifyingTask } from "@bindings/VerifyingTask";
import { api, errorText } from "./api";
import { ActivityFeed } from "./ActivityFeed";
import { Avatar, ErrorBox, Icon, displayName, isAgent, sessionHref, useAgents } from "./components";
import { board, countdown, elapsed, watchOn, withoutReminder, type Split } from "./lib/background";
import { CommitGraphView } from "./commitgraph";
import { ChangeStat, DiffView } from "./diffview";
import { useEventLog } from "./eventlog";
import { FilesPane } from "./FileViewer";
import type { HubEvent } from "./hub";
import { parseDiff } from "./lib/diff";
import { clock, full } from "./lib/time";
import { clearTarget, closeBottom, isExtTab, isPaneKind, isTermTab, paneLabel, useLayout, type PaneKind, type TabId, type Target } from "./panes";
import { ExtensionTabBody } from "./extensions/ui";
import { shownTab, type Dock, type DockId } from "./lib/docks";
import { TabRow } from "./tabs";
import { TerminalBody, useTabShows } from "./terminal";
import { useWidths } from "./lib/widths";
import { useSession, type TranscriptTurn } from "./session";
import { useSidebar } from "./titlebar";
import { TaskDetail, PlanTasks } from "./views/Run";
import { onHubEvent, useHubData } from "./hub";

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
  const all: Dock<TabId> = narrow ? { tabs: [...right.tabs, ...bottom.tabs], active: floatTab ?? right.active, open: true } : state.docks[dock];
  // Another project's terminals keep their place in the row, out of sight.
  const shows = useTabShows();
  const shown = { ...all, tabs: all.tabs.filter(shows) };
  const kind = shownTab(shown);
  return (
    <div className={`tool-column ${narrow ? "narrow" : ""}`} data-tabdock={dock}>
      <section className="tool-pane" aria-label={kind && !isTermTab(kind) ? paneLabel(kind) : undefined}>
        <header className="tool-head">
          <TabRow dock={dock} tabs={shown.tabs} active={kind} narrow={narrow} onCloseTask={task ? onCloseTask : undefined} />
          {kind && isPaneKind(kind) && <FollowButton kind={kind} follow={follow} target={target} />}
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
        {kind &&
          (isTermTab(kind) ? (
            <TerminalBody tab={kind} dock={dock} />
          ) : isExtTab(kind) ? (
            <ExtensionTabBody tab={kind} follow={follow} />
          ) : (
            <PaneBody kind={kind} follow={follow} target={target} routeTask={task} routeRun={run} />
          ))}
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
  if (kind === "task" || kind === "activity" || !safeTarget?.agent || safeTarget.agent === follow) return null;
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
      {kind === "background" && <BackgroundPane follow={agent} routeRun={routeRun ?? routeTask?.run} />}
      {kind === "activity" && <ActivityFeed />}
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

/** Events after which the board is read again: what may add, end or rename one of its items. */
const backgroundRelevant = (e: HubEvent) =>
  e.type === "reminder.fired" ||
  e.type === "watch.triggered" ||
  e.type === "task.status_changed" ||
  e.type === "task.checked" ||
  e.type === "run.status_changed" ||
  e.type === "run.deleted" ||
  e.type === "agent.created" ||
  e.type === "agent.updated" ||
  e.type === "agent.removed";

/** A clock that ticks every second while the tab shows, for the countdowns. */
function useNow(): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, []);
  return now;
}

const runHref = (run: string) => `#/runs/${encodeURIComponent(run)}`;

/**
 * What the hub keeps going behind the conversations: the agents' reminders, their watches and
 * the tasks being verified. What concerns the followed conversation or the routed run comes first;
 * the rest is folded. Agents set these with their own tools; here a reminder can only be cancelled.
 */
function BackgroundPane({ follow, routeRun }: { follow?: string; routeRun?: string }) {
  const agents = useAgents();
  const agentId = agents.data?.find((a) => follow && isAgent(a, follow))?.id;
  const loaded = useHubData(() => api.background(), [], backgroundRelevant);
  // Reminders gone here before the board is read again: fired (the event) or cancelled from here.
  const [gone, setGone] = useState<Set<string>>(() => new Set());
  const [failed, setFailed] = useState<string>();
  const now = useNow();
  useEffect(
    () =>
      onHubEvent((e) => {
        if (e.type === "reminder.fired") setGone((g) => new Set(g).add(e.data.reminder_id));
      }),
    [],
  );
  const bg = useMemo<BackgroundResponse | undefined>(() => {
    if (!loaded.data) return undefined;
    let out = loaded.data;
    for (const id of gone) out = withoutReminder(out, id);
    return out;
  }, [loaded.data, gone]);
  const split = useMemo(() => (bg ? board(bg, { agent: agentId, run: routeRun }) : undefined), [bg, agentId, routeRun]);

  const cancel = (r: BackgroundReminder) => {
    setFailed(undefined);
    setGone((g) => new Set(g).add(r.reminder.id));
    api.cancelReminder(r.reminder.id).then(
      () => loaded.reload(),
      (err) => {
        // Already gone in the hub (fired or cancelled meanwhile): that is what was wanted.
        if ((err as { status?: number }).status === 404) return loaded.reload();
        setGone((g) => {
          const next = new Set(g);
          next.delete(r.reminder.id);
          return next;
        });
        setFailed(`没能取消提醒：${errorText(err)}`);
      },
    );
  };

  if (!bg || !split) return loaded.error ? <ErrorBox error={loaded.error} /> : <Blank text="加载中…" />;
  const total = bg.reminders.length + bg.watches.length + bg.verifying.length;
  if (!total) return <Blank text="后台现在没有东西：没有 agent 设的提醒或关注，也没有正在验收的任务。" />;
  const others = split.reminders.other.length + split.watches.other.length + split.verifying.other.length;
  return (
    <div className="bg-pane">
      {failed && <ErrorBox error={failed} />}
      {others === total ? <p className="muted small">当前对话和 run 没有后台条目。</p> : <BgSections board={split} pick="near" now={now} onCancel={cancel} />}
      {others > 0 && (
        <details className="bg-others">
          <summary className="small muted">其他会话和 run 的 {others} 项</summary>
          <BgSections board={split} pick="other" now={now} onCancel={cancel} />
        </details>
      )}
    </div>
  );
}

type Picked = { reminders: Split<BackgroundReminder>; watches: Split<BackgroundWatch>; verifying: Split<VerifyingTask> };

function BgSections({ board, pick, now, onCancel }: { board: Picked; pick: "near" | "other"; now: Date; onCancel: (r: BackgroundReminder) => void }) {
  const reminders = board.reminders[pick];
  const watches = board.watches[pick];
  const verifying = board.verifying[pick];
  return (
    <>
      {reminders.length > 0 && (
        <section className="bg-section">
          <h4>提醒 · {reminders.length}</h4>
          {reminders.map((r) => {
            const task = r.task_key ?? r.reminder.task;
            return (
              <div key={r.reminder.id} className="bg-item">
                <div className="bg-line">
                  <a href={sessionHref(r.reminder.owner)} className="bg-who">
                    {r.owner_name}
                  </a>
                  <span className="bg-when" title={full(r.reminder.due_at)}>
                    {clock(r.reminder.due_at, now)} · {countdown(r.reminder.due_at, now)}
                  </span>
                  <button className="ghost small bg-cancel" onClick={() => onCancel(r)} title="取消这个提醒，它不会再叫醒 agent">
                    取消
                  </button>
                </div>
                {r.reminder.note && <div className="bg-note">{r.reminder.note}</div>}
                {task && (
                  <div className="bg-meta small muted">
                    关联任务 {r.run_id ? <a href={runHref(r.run_id)}>#{task}</a> : <span>#{task}</span>}
                    {r.reminder.only_if_open && " · 任务结束就不再提醒"}
                  </div>
                )}
              </div>
            );
          })}
        </section>
      )}
      {watches.length > 0 && (
        <section className="bg-section">
          <h4>关注 · {watches.length}</h4>
          {watches.map((w) => (
            <div key={w.watch.id} className="bg-item">
              <div className="bg-line">
                <a href={sessionHref(w.watch.owner)} className="bg-who">
                  {w.owner_name}
                </a>
                <span className="muted">在看</span>
                {w.watch.target.kind === "agent" ? (
                  <a href={sessionHref(w.watch.target.agent_id)}>{w.target_name}</a>
                ) : w.run_id ? (
                  <a href={runHref(w.run_id)}>{w.target_name}</a>
                ) : (
                  <span>{w.target_name}</span>
                )}
              </div>
              <div className="bg-meta small muted" title={full(w.watch.created_at)}>
                关注：{watchOn(w.watch.on)} · {clock(w.watch.created_at, now)} 起
              </div>
            </div>
          ))}
        </section>
      )}
      {verifying.length > 0 && (
        <section className="bg-section">
          <h4>验收中 · {verifying.length}</h4>
          {verifying.map((v) => (
            <div key={v.task_id} className="bg-item">
              <div className="bg-line">
                <a href={runHref(v.run_id)} className="bg-who">
                  #{v.key}
                </a>
                <span className="bg-title">{v.title}</span>
                <span className="bg-when" title={full(v.since)}>
                  已跑 {elapsed(v.since, now)}
                </span>
              </div>
              <div className="bg-meta small muted">
                {v.run_title}
                {v.assignee_name && " · "}
                {v.assignee_name && (v.assignee ? <a href={sessionHref(v.assignee)}>{v.assignee_name}</a> : v.assignee_name)}
                {` · ${v.checks} 项检查`}
              </div>
            </div>
          ))}
        </section>
      )}
    </>
  );
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

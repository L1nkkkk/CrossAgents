// The tool column (docs/tasks/V6-GUI.md §6): up to two panes stacked on the right — 改动 (what
// each turn changed), 任务 (a task's details), and 提交图 (commits). 事件流 (raw hub events, for
// debugging) is offered only where debugging is on (panes.ts `PANE_KINDS`). Panes show the
// focused conversation, or what was opened on purpose (panes.ts `target`); nothing from another
// conversation mixes in. When docking it would squeeze the conversation it is a one-pane drawer
// over the page.

import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { TurnDiffResponse } from "@bindings/TurnDiffResponse";
import { api, errorText } from "./api";
import { useRunTree } from "./App";
import { Avatar, ErrorBox, Icon, displayName, isAgent, useAgents } from "./components";
import { CommitGraphView } from "./commitgraph";
import { ChangeStat, DiffView } from "./diffview";
import { useEventLog } from "./eventlog";
import type { HubEvent } from "./hub";
import { parseDiff } from "./lib/diff";
import { clock, full } from "./lib/time";
import { PANE_KINDS, addPane, clearTarget, offered, paneLabel, removePane, setPane, setSplit, useLayout, type PaneKind, type Target } from "./panes";
import { useWidths } from "./lib/widths";
import { useSession, type TranscriptTurn } from "./session";
import { useSidebar } from "./titlebar";
import { TaskDetail } from "./views/Run";

/**
 * Whether the tool column floats over the page as a drawer: when docking it would leave the
 * conversation narrower than MIN_MAIN, which depends on the sidebar and the dragged widths.
 */
export function useToolsFloat() {
  const { docked } = useSidebar();
  return useWidths(docked).float;
}

/**
 * `follow`: the conversation the panes follow (session routes). `task`: the task the route
 * opened (run routes), shown in the first pane. `run`: the routed run, whose branches 提交图
 * shows. `onClose` closes the whole column.
 */
export function ToolColumn({ follow, task, run, onClose }: { follow?: string; task?: { run: string; task: string }; run?: string; onClose: () => void }) {
  const { state, target } = useLayout();
  const narrow = useToolsFloat();
  const ref = useRef<HTMLDivElement>(null);
  const tools = state.tools;
  let panes: PaneKind[] = task ? [...new Set<PaneKind>(["task", ...(tools.open ? tools.panes : [])])] : tools.panes;
  // A saved layout may name a pane this build does not offer (事件流): drop it, and never leave
  // the column with nothing to show.
  panes = panes.filter(offered);
  if (!panes.length) panes = ["changes"];
  panes = panes.slice(0, narrow ? 1 : 2);

  const drag = (e: ReactPointerEvent<HTMLDivElement>) => {
    const box = ref.current?.getBoundingClientRect();
    if (!box) return;
    e.preventDefault();
    const move = (ev: PointerEvent) => setSplit((ev.clientY - box.top) / box.height);
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  const rows = panes.length === 2 ? `minmax(0, ${tools.split}fr) 6px minmax(0, ${1 - tools.split}fr)` : "minmax(0, 1fr)";
  return (
    <div className={`tool-column ${narrow ? "narrow" : ""}`} ref={ref} style={{ gridTemplateRows: rows }}>
      {panes.map((kind, i) => (
        <PaneSlot
          key={i}
          index={i}
          kind={kind}
          count={panes.length}
          narrow={narrow}
          follow={follow}
          target={target}
          routeTask={task}
          routeRun={run}
          onCloseColumn={onClose}
          divider={i === 1 ? <div className="divider horizontal" onPointerDown={drag} onDoubleClick={() => setSplit(0.5)} role="separator" aria-orientation="horizontal" /> : null}
        />
      ))}
    </div>
  );
}

function PaneSlot({
  index,
  kind,
  count,
  narrow,
  follow,
  target,
  routeTask,
  routeRun,
  onCloseColumn,
  divider,
}: {
  index: number;
  kind: PaneKind;
  count: number;
  narrow: boolean;
  follow?: string;
  target: Target | null;
  routeTask?: { run: string; task: string };
  routeRun?: string;
  onCloseColumn: () => void;
  divider: React.ReactNode;
}) {
  // An agent opened on purpose wins over the focused conversation.
  const agent = target?.agent ?? follow;
  const pinned = !!target?.agent && target.agent !== follow;
  const head = (
    <>
      <div className="segmented small" role="tablist">
        {PANE_KINDS.map((p) => (
          <button key={p.kind} role="tab" aria-selected={p.kind === kind} className={p.kind === kind ? "active" : ""} onClick={() => setPane(index, p.kind)}>
            {p.label}
          </button>
        ))}
      </div>
      <span className="spacer" />
      {pinned && kind !== "task" && (
        <button className="ghost small" onClick={clearTarget} title="回到当前对话">
          跟随对话
        </button>
      )}
      {!narrow && count === 1 && (
        <button className="ghost icon" onClick={addPane} title="再加一个窗格" aria-label="再加一个窗格">
          <Icon name="plus" size={15} />
        </button>
      )}
      {count === 2 && (
        <button className="ghost icon" onClick={() => removePane(index)} title="关闭这个窗格" aria-label="关闭这个窗格">
          <Icon name="close" size={15} />
        </button>
      )}
      {index === 0 && (
        <button className="ghost icon" onClick={onCloseColumn} title="收起工具列" aria-label="收起工具列">
          <Icon name="panel" size={15} />
        </button>
      )}
    </>
  );
  return (
    <>
      {divider}
      <section className="tool-pane" aria-label={paneLabel(kind)}>
        <header className="tool-head">{head}</header>
        <div className={`tool-body ${kind === "graph" ? "tool-body-graph" : ""}`}>
          {kind === "changes" && (agent ? <ChangesPane key={agent} agent={agent} target={target?.agent === agent ? target : null} /> : <Blank text="打开一个会话，这里显示它每一轮改了什么。" />)}
          {kind === "task" && <TaskPane follow={follow} target={target} routeTask={routeTask} />}
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
      </section>
    </>
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

/** The routed task, else the one opened on purpose, else the focused agent's latest task. */
function TaskPane({ follow, target, routeTask }: { follow?: string; target: Target | null; routeTask?: { run: string; task: string } }) {
  const tree = useRunTree();
  const agents = useAgents();
  let found = routeTask ?? (target?.run && target.task ? { run: target.run, task: target.task } : undefined);
  if (!found && follow) {
    const agent = agents.data?.find((a) => isAgent(a, follow));
    const tasks = (tree.data ?? []).flatMap((d) => d.tasks.filter((t) => agent && (t.assignee === agent.id || t.reviewer === agent.id)));
    const latest = tasks.sort((a, b) => b.updated_at.localeCompare(a.updated_at))[0];
    if (latest) found = { run: latest.run_id, task: latest.id };
  }
  if (!found) return <Blank text="这个会话没有任务。Worker 和 Reviewer 的会话会在这里显示它们的任务。" />;
  return <TaskDetail key={found.task} runId={found.run} taskId={found.task} />;
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

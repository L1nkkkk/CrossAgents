// The commit graph of a run's branches or a session's checkout (lib/graphlayout.ts lays it
// out): one row per commit with its lanes drawn in SVG, branch labels, and, for the commit
// picked, what it changed (against its first parent) below.

import { useEffect, useMemo, useRef, useState } from "react";
import type { CommitGraph as Graph } from "@bindings/CommitGraph";
import type { GraphRef } from "@bindings/GraphRef";
import { api } from "./api";
import { ErrorBox, ago, time } from "./components";
import { ChangeStat, DiffView } from "./diffview";
import { openFile } from "./FileViewer";
import { useHubData, type HubEvent } from "./hub";
import { parseDiff } from "./lib/diff";
import { layoutGraph, type GraphEdge, type GraphRow } from "./lib/graphlayout";
import "./commitgraph.css";

/** Whose commits: a run's branches, or a session's checkout. */
export type GraphScope = { kind: "run"; run: string } | { kind: "agent"; agent: string };

const ROW = 28;
const LANE = 14;
const PAD = 9;
const MID = ROW / 2;
const x = (lane: number) => PAD + lane * LANE;

function relevant(scope: GraphScope, e: HubEvent): boolean {
  if (scope.kind === "run") return (e.type === "task.status_changed" && e.data.run_id === scope.run) || (e.type === "run.status_changed" && e.data.id === scope.run);
  return e.type === "agent.turn_completed" && e.data.id === scope.agent;
}

export function CommitGraphView({ scope, limit = 200 }: { scope: GraphScope; limit?: number }) {
  const key = scope.kind === "run" ? scope.run : scope.agent;
  const graph = useHubData<Graph>(
    () => (scope.kind === "run" ? api.runGraph(scope.run, limit) : api.agentGraph(scope.agent, limit)),
    [scope.kind, key, limit],
    (e) => relevant(scope, e),
  );
  const layout = useMemo(() => (graph.data ? layoutGraph(graph.data) : undefined), [graph.data]);
  const [picked, setPicked] = useState<string>();
  const listRef = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState({ start: 0, end: 0 });
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const update = () => setVisible({ start: Math.floor(el.scrollTop / ROW), end: Math.ceil((el.scrollTop + el.clientHeight) / ROW) });
    update();
    const observer = new ResizeObserver(update);
    observer.observe(el);
    el.addEventListener("scroll", update);
    return () => { observer.disconnect(); el.removeEventListener("scroll", update); };
  }, [layout, picked]);

  if (graph.error && !graph.data) return <ErrorBox error={graph.error} />;
  if (!graph.data || !layout) return <p className="muted small">加载提交图…</p>;
  if (!layout.rows.length) return <p className="muted small">还没有提交。</p>;
  const width = x(Math.max(layout.lanes, 1) - 1) + PAD;

  return (
    <div className="cgraph">
      <Legend refs={graph.data.refs} colors={layout.colors} rows={layout.rows.slice(visible.start, visible.end)} base={graph.data.base_branch} />
      <div className="cgraph-list" ref={listRef} role="listbox" aria-label="提交">
        {layout.rows.map((row) => (
          <Row key={row.commit.sha} row={row} width={width} active={picked === row.commit.sha} onPick={() => setPicked(picked === row.commit.sha ? undefined : row.commit.sha)} />
        ))}
        {graph.data.truncated && <div className="cgraph-more muted small">只显示了最近 {layout.rows.length} 个提交，更早的没有列出</div>}
      </div>
      {picked && <CommitDiff scope={scope} sha={picked} subject={layout.rows.find((r) => r.commit.sha === picked)?.commit.subject} />}
    </div>
  );
}

/** Which colour is which branch. */
function Legend({ refs, colors, rows, base }: { refs: GraphRef[]; colors: Map<string, number>; rows: GraphRow[]; base?: string | null }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const visible = new Set(rows.flatMap((row) => row.refs.map((ref) => ref.name)));
  const head = refs.find((ref) => ref.kind === "head");
  const primary = refs.filter((ref) => ref.kind !== "other" || ref.name === base || ref.name === "main" || ref.name === "master" || (!!head && ref.sha === head.sha) || visible.has(ref.name));
  const hidden = refs.filter((ref) => !primary.includes(ref));
  const matches = hidden.filter((ref) => `${ref.name} ${ref.task_title ?? ""}`.toLowerCase().includes(query.toLowerCase()));
  const item = (ref: GraphRef) => (
    <span key={ref.name} className={`cgraph-legend-item g-c${colors.get(ref.name) ?? 0}`} title={ref.task_title ? `${ref.name} ? ${ref.task_title}` : ref.name}>
      <span className="cgraph-dot" />{refLabel(ref)} <span className="muted">{KIND_WORD[ref.kind]}</span>
    </span>
  );
  if (!refs.length) return null;
  return <div className="cgraph-legend-wrap">
    <div className="cgraph-legend small">{primary.map(item)}</div>
    {hidden.length > 0 && <div className="cgraph-legend-more">
      <button className="ghost small" aria-expanded={open} onClick={() => setOpen(!open)}>+{hidden.length} 个分支</button>
      {open && <div className="cgraph-legend-popover">
        <input autoFocus aria-label="搜索分支" placeholder="搜索分支" value={query} onChange={(e) => setQuery(e.target.value)} />
        <div className="cgraph-legend-results">{matches.map(item)}{!matches.length && <span className="muted small">没有匹配的分支</span>}</div>
      </div>}
    </div>}
  </div>;
}

const KIND_WORD: Record<GraphRef["kind"], string> = { base: "基线", run: "run 分支", task: "任务", head: "当前", other: "分支" };

function refLabel(ref: GraphRef): string {
  if (ref.kind === "task" && ref.task_key) return `#${ref.task_key}`;
  return ref.name;
}

function Row({ row, width, active, onPick }: { row: GraphRow; width: number; active: boolean; onPick: () => void }) {
  const { commit } = row;
  const cx = x(row.lane);
  return (
    <button className={`cgraph-row ${active ? "active" : ""} ${row.merge ? "merge" : ""}`} onClick={onPick} role="option" aria-selected={active} title={`${commit.sha}\n${commit.subject}`}>
      <svg className="cgraph-lanes" width={width} height={ROW} viewBox={`0 0 ${width} ${ROW}`} aria-hidden>
        {row.top.map((e, i) => (
          <Edge key={`t${i}`} edge={e} y0={0} y1={MID} />
        ))}
        {row.bottom.map((e, i) => (
          <Edge key={`b${i}`} edge={e} y0={MID} y1={ROW} />
        ))}
        {row.merge ? (
          <circle className={`cgraph-node merge g-c${color(row.color)}`} cx={cx} cy={MID} r={4.5} />
        ) : (
          <circle className={`cgraph-node g-c${color(row.color)}`} cx={cx} cy={MID} r={3.5} />
        )}
      </svg>
      <span className="cgraph-text">
        {row.refs.map((ref) => (
          <span key={ref.name} className={`cgraph-ref g-c${color(row.color)} k-${ref.kind}`} title={ref.task_title ? `${ref.name} · ${ref.task_title}` : ref.name}>
            {refLabel(ref)}
          </span>
        ))}
        {row.merge && <span className="cgraph-merge-tag">合并</span>}
        <span className="cgraph-subject">{commit.subject || "（无标题）"}</span>
      </span>
      <span className="cgraph-author muted small">{commit.author}</span>
      <span className="cgraph-time muted small" title={time(commit.time)}>
        {ago(commit.time)}
      </span>
      <code className="cgraph-sha">{commit.sha.slice(0, 7)}</code>
    </button>
  );
}

const color = (c: number) => (c < 0 ? "x" : String(c));

function Edge({ edge, y0, y1 }: { edge: GraphEdge; y0: number; y1: number }) {
  const x0 = x(edge.from);
  const x1 = x(edge.to);
  const d = x0 === x1 ? `M${x0} ${y0}V${y1}` : `M${x0} ${y0}C${x0} ${(y0 + y1) / 2} ${x1} ${(y0 + y1) / 2} ${x1} ${y1}`;
  return <path className={`cgraph-edge g-c${color(edge.color)}`} d={d} />;
}

/** One commit's changes against its first parent; file names open the file. */
function CommitDiff({ scope, sha, subject }: { scope: GraphScope; sha: string; subject?: string }) {
  const key = scope.kind === "run" ? scope.run : scope.agent;
  const changes = useHubData(
    () => (scope.kind === "run" ? api.runCommitDiff(scope.run, sha) : api.agentCommitDiff(scope.agent, sha)),
    [scope.kind, key, sha],
    () => false,
  );
  const files = useMemo(() => (changes.data ? parseDiff(changes.data.diff) : []), [changes.data]);
  const fileScope = scope.kind === "run" ? ({ kind: "run", run: scope.run } as const) : ({ kind: "agent", agent: scope.agent } as const);
  return (
    <div className="cgraph-diff">
      <div className="cgraph-diff-head small">
        <code>{sha.slice(0, 7)}</code>
        <span className="cgraph-subject">{subject}</span>
        {changes.data && (
          <span className="muted">
            {files.length} 个文件 <ChangeStat files={files} />
          </span>
        )}
        {changes.data?.truncated && <span className="chip warn">diff 太长，已截断</span>}
      </div>
      {changes.error && <ErrorBox error={changes.error} />}
      {!changes.data && !changes.error && <p className="muted small">加载改动…</p>}
      {changes.data && (files.length ? <DiffView files={files} onOpenFile={(path) => openFile({ scope: fileScope, path })} /> : <p className="muted small">这个提交没有改文件。</p>)}
    </div>
  );
}

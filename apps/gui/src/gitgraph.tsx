// The whole-repository commit graph of the Git page (`#/git`): lanes laid out by
// lib/graphlayout.ts and drawn the way commitgraph.tsx draws them (same sizes, same SVG, same
// `cgraph-*` classes), with the repository's refs — HEAD, local and remote branches, tags —
// labelled on their commits.

import { useEffect, useMemo, useRef } from "react";
import type { GitGraph } from "@bindings/GitGraph";
import type { GitRefKind } from "@bindings/GitRefKind";
import type { GraphRef } from "@bindings/GraphRef";
import { ago, time } from "./components";
import { layoutGraph, type GraphEdge, type GraphRow } from "./lib/graphlayout";
import "./commitgraph.css";

// As in commitgraph.tsx.
const ROW = 28;
const LANE = 14;
const PAD = 9;
const MID = ROW / 2;
const x = (lane: number) => PAD + lane * LANE;
const color = (c: number) => (c < 0 ? "x" : String(c));

const KIND_WORD: Record<GitRefKind, string> = { head: "HEAD", local: "本地分支", remote: "远程分支", tag: "标签" };
/** Labels on a commit: HEAD first, then local branches, remote ones, tags. */
const KIND_ORDER: Record<GitRefKind, number> = { head: 0, local: 1, remote: 2, tag: 3 };

/**
 * The graph's refs as the layout knows them: HEAD gets the accent lane, every other ref is a
 * plain branch; `kinds` keeps what each really is, for its label.
 */
export function graphRefs(graph: GitGraph): { refs: GraphRef[]; kinds: Map<string, GitRefKind> } {
  const kinds = new Map(graph.refs.map((r) => [r.name, r.kind]));
  // Tags last, so a branch rather than a tag names (and colours) the line it is on.
  const ordered = [...graph.refs].sort((a, b) => Number(a.kind === "tag") - Number(b.kind === "tag"));
  return { refs: ordered.map((r) => ({ name: r.name, kind: r.kind === "head" ? "head" : "other", sha: r.sha })), kinds };
}

export function GitGraphView({ graph, picked, onPick }: { graph: GitGraph; picked?: string; onPick: (sha: string) => void }) {
  const { layout, kinds } = useMemo(() => {
    const { refs, kinds } = graphRefs(graph);
    return { layout: layoutGraph({ refs, commits: graph.commits }), kinds };
  }, [graph]);
  const listRef = useRef<HTMLDivElement>(null);

  // A commit picked elsewhere (a branch on the left, a parent on the right) scrolls into view.
  useEffect(() => {
    if (!picked) return;
    const el = listRef.current?.querySelector<HTMLElement>(`[data-sha="${picked}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [picked, layout]);

  if (!layout.rows.length) return <p className="muted small">还没有提交。</p>;
  const width = x(Math.max(layout.lanes, 1) - 1) + PAD;
  return (
    <div className="cgraph git-graph">
      <div className="cgraph-list" ref={listRef} role="listbox" aria-label="提交">
        {layout.rows.map((row) => (
          <Row key={row.commit.sha} row={row} kinds={kinds} width={width} active={picked === row.commit.sha} onPick={() => onPick(row.commit.sha)} />
        ))}
        {graph.truncated && <div className="cgraph-more muted small">只显示了最近 {layout.rows.length} 个提交，更早的没有列出</div>}
      </div>
    </div>
  );
}

function Row({ row, kinds, width, active, onPick }: { row: GraphRow; kinds: Map<string, GitRefKind>; width: number; active: boolean; onPick: () => void }) {
  const { commit } = row;
  const cx = x(row.lane);
  const refs = [...row.refs].sort((a, b) => KIND_ORDER[kinds.get(a.name) ?? "local"] - KIND_ORDER[kinds.get(b.name) ?? "local"]);
  const head = refs.some((ref) => kinds.get(ref.name) === "head");
  return (
    <button
      className={`cgraph-row ${active ? "active" : ""} ${row.merge ? "merge" : ""} ${head ? "git-head-row" : ""}`}
      data-sha={commit.sha}
      onClick={onPick}
      role="option"
      aria-selected={active}
      title={`${commit.subject}\n${commit.author} \u00b7 ${time(commit.time)}\n${commit.sha}`}
    >
      <svg className="cgraph-lanes" width={width} height={ROW} viewBox={`0 0 ${width} ${ROW}`} aria-hidden>
        {row.top.map((e, i) => <Edge key={`t${i}`} edge={e} y0={0} y1={MID} />)}
        {row.bottom.map((e, i) => <Edge key={`b${i}`} edge={e} y0={MID} y1={ROW} />)}
        {head && <circle className={`git-head-ring g-c${color(row.color)}`} cx={cx} cy={MID} r={7} />}
        <circle className={`cgraph-node ${row.merge ? "merge" : ""} g-c${color(row.color)}`} cx={cx} cy={MID} r={row.merge ? 4.5 : 3.5} />
      </svg>
      <span className="cgraph-text">
        {refs.map((ref) => {
          const kind = kinds.get(ref.name) ?? "local";
          return (
            <span key={ref.name} className={`cgraph-ref git-ref g-c${color(row.color)} k-${kind}`} title={`${ref.name} · ${KIND_WORD[kind]}`}>
              {ref.name}
            </span>
          );
        })}
        {row.merge && <span className="cgraph-merge-tag">合并</span>}
        <span className="cgraph-subject">{commit.subject || "（无标题）"}</span>
      </span>
      <span className="cgraph-author muted small">{commit.author}</span>
      <span className="cgraph-time muted small" title={time(commit.time)}>
        <span className="cgraph-time-full">{ago(commit.time)}</span><span className="cgraph-time-short">{shortTime(commit.time)}</span>
      </span>
      <code className="cgraph-sha">{commit.sha.slice(0, 7)}</code>
    </button>
  );
}

// As in commitgraph.tsx: the time where the column is narrow.
function shortTime(value: string): string {
  const date = new Date(value);
  const minutes = Math.max(0, Math.floor((Date.now() - date.getTime()) / 60000));
  if (!Number.isFinite(minutes)) return value;
  if (minutes < 60) return `${minutes}m`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)}h`;
  if (minutes < 10080) return `${Math.floor(minutes / 1440)}d`;
  return `${date.getMonth() + 1}/${date.getDate()}`;
}

function Edge({ edge, y0, y1 }: { edge: GraphEdge; y0: number; y1: number }) {
  const x0 = x(edge.from);
  const x1 = x(edge.to);
  const d = x0 === x1 ? `M${x0} ${y0}V${y1}` : `M${x0} ${y0}C${x0} ${(y0 + y1) / 2} ${x1} ${(y0 + y1) / 2} ${x1} ${y1}`;
  return <path className={`cgraph-edge g-c${color(edge.color)}`} d={d} />;
}

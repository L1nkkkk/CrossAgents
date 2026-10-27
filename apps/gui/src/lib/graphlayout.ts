// Lays out a commit graph for drawing: each commit (newest first, topological order) gets a
// lane, and each row says which lines cross it. Lanes never shift sideways: a line keeps its
// column from the child to the parent, so rows can be drawn on their own.
//
// Colours follow branches: walking each ref's first-parent chain (base, then run, then the
// tasks and the rest), a commit belongs to the first ref that reaches it. Lines take the
// colour of the lane they run in, which is the colour of the child that opened it.

import type { CommitGraph } from "@bindings/CommitGraph";
import type { GraphCommit } from "@bindings/GraphCommit";
import type { GraphRef } from "@bindings/GraphRef";

/** A line in one half of a row: from lane `from` at the half's top to lane `to` at its bottom. */
export type GraphEdge = { from: number; to: number; color: number };

export type GraphRow = {
  commit: GraphCommit;
  lane: number;
  /** Palette index of the commit's branch; -1 for none known. */
  color: number;
  /** The refs pointing at this commit, as the graph lists them. */
  refs: GraphRef[];
  merge: boolean;
  /** Lines from the row's top to the commit's centre (`to` is the commit's lane for lines ending in it). */
  top: GraphEdge[];
  /** Lines from the commit's centre to the row's bottom. */
  bottom: GraphEdge[];
};

export type GraphLayout = {
  rows: GraphRow[];
  /** How many lanes the widest row uses. */
  lanes: number;
  /** Palette index by ref name. */
  colors: Map<string, number>;
};

const KIND_ORDER: Record<GraphRef["kind"], number> = { base: 0, run: 1, head: 1, task: 2, other: 3 };

/** Palette indices: 0 the base, 1 the run branch (or HEAD), then a cycle of six for the rest. */
export const PALETTE_SIZE = 8;

/** The palette index of each ref: base 0, run/HEAD 1, every other ref the next of 2..7 in turn. */
export function refColors(refs: GraphRef[]): Map<string, number> {
  const colors = new Map<string, number>();
  let next = 0;
  for (const ref of sortRefs(refs)) {
    if (ref.kind === "base") colors.set(ref.name, 0);
    else if (ref.kind === "run" || ref.kind === "head") colors.set(ref.name, 1);
    else colors.set(ref.name, 2 + (next++ % (PALETTE_SIZE - 2)));
  }
  return colors;
}

function sortRefs(refs: GraphRef[]): GraphRef[] {
  return refs
    .map((ref, i) => ({ ref, i }))
    .sort((a, b) => KIND_ORDER[a.ref.kind] - KIND_ORDER[b.ref.kind] || a.i - b.i)
    .map((x) => x.ref);
}

/** Which ref each commit belongs to (by sha): the first, in ref order, whose first-parent chain reaches it. */
export function owners(graph: Pick<CommitGraph, "refs" | "commits">): Map<string, string> {
  const bySha = new Map(graph.commits.map((c) => [c.sha, c]));
  const owner = new Map<string, string>();
  for (const ref of sortRefs(graph.refs)) {
    let sha: string | undefined = ref.sha;
    while (sha && !owner.has(sha)) {
      const commit = bySha.get(sha);
      if (!commit) break;
      owner.set(sha, ref.name);
      sha = commit.parents[0];
    }
  }
  return owner;
}

export function layoutGraph(graph: Pick<CommitGraph, "refs" | "commits">): GraphLayout {
  const colors = refColors(graph.refs);
  const owner = owners(graph);
  const refByName = new Map(graph.refs.map((r) => [r.name, r]));
  const colorOf = (sha: string): number | undefined => {
    const name = owner.get(sha);
    return name === undefined ? undefined : colors.get(name);
  };

  // What each lane waits for (a parent sha), and the colour of the line in it.
  const lanes: (string | null)[] = [];
  const laneColor: number[] = [];
  const rows: GraphRow[] = [];
  let width = 0;

  const freeLane = () => {
    const i = lanes.indexOf(null);
    return i < 0 ? lanes.length : i;
  };

  for (const commit of graph.commits) {
    const { sha, parents } = commit;
    const before = lanes.slice();
    const beforeColor = laneColor.slice();

    let lane = lanes.indexOf(sha);
    if (lane < 0) lane = freeLane();
    const color = colorOf(sha) ?? (before[lane] === sha ? beforeColor[lane] : undefined) ?? -1;

    // Lines into this row: lanes waiting for this commit end in it; the rest pass through.
    const top: GraphEdge[] = [];
    before.forEach((want, j) => {
      if (want === null) return;
      top.push({ from: j, to: want === sha ? lane : j, color: beforeColor[j] });
    });

    // Lanes that waited for this commit are free now; its own lane goes on to its first parent.
    before.forEach((want, j) => {
      if (want === sha) lanes[j] = null;
    });
    if (parents.length) {
      lanes[lane] = parents[0];
      laneColor[lane] = color;
    } else {
      lanes[lane] = null;
    }
    // Further parents (a merge): join a lane already waiting for that parent, or open one.
    const targets: { lane: number; color: number }[] = [];
    for (const parent of parents.slice(1)) {
      let k = lanes.indexOf(parent);
      if (k < 0) {
        k = freeLane();
        lanes[k] = parent;
        laneColor[k] = colorOf(parent) ?? color;
      }
      targets.push({ lane: k, color: laneColor[k] });
    }

    const bottom: GraphEdge[] = [];
    lanes.forEach((want, j) => {
      if (want === null) return;
      if (j === lane) bottom.push({ from: lane, to: lane, color });
      else if (before[j] !== null && before[j] !== sha && before[j] === want) bottom.push({ from: j, to: j, color: laneColor[j] });
    });
    for (const t of targets) bottom.push({ from: lane, to: t.lane, color: t.color });

    while (lanes.length && lanes[lanes.length - 1] === null) {
      lanes.pop();
      laneColor.pop();
    }
    width = Math.max(width, lane + 1, before.length, lanes.length);

    rows.push({
      commit,
      lane,
      color,
      refs: commit.refs.map((name) => refByName.get(name)).filter((r): r is GraphRef => !!r),
      merge: parents.length > 1,
      top,
      bottom,
    });
  }

  const used = rows.reduce((m, r) => Math.max(m, r.lane + 1, ...r.top.map((e) => Math.max(e.from, e.to) + 1), ...r.bottom.map((e) => Math.max(e.from, e.to) + 1)), 0);
  return { rows, lanes: Math.min(used, width), colors };
}

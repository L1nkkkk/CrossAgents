// The run's swimlane (§十三): one line per worker, and on it what happened to its tasks and
// its session, in order — 提交, 交接, 测试 ✓/✗, 重试, 改派 (a fork from the worker before),
// 合入/落地, 上下文压缩, 模型切换, 等你. Folded from the run trace (`GET /v1/runs/{id}/trace`)
// and the handoff cards it carries.
//
// Everything here is pure; swimlane.test.ts checks it.

import type { Handoff } from "@bindings/Handoff";
import type { RunTrace } from "@bindings/RunTrace";
import type { TaskTrace } from "@bindings/TaskTrace";

export type NodeKind =
  | "handoff"
  | "reroute"
  | "commit"
  | "test_pass"
  | "test_fail"
  | "retry"
  | "merge"
  | "land"
  | "compact"
  | "model"
  | "ask"
  | "answer";

export type Tone = "ok" | "bad" | "warn" | "accent" | "muted";

export interface LaneNode {
  id: string;
  at: string;
  kind: NodeKind;
  /** The task's key, when the node is about a task. */
  task: string | null;
  label: string;
  detail: string | null;
  tone: Tone;
  /** Position on the shared time axis: every node of the run, in order. */
  slot: number;
  /** For 交接 and 改派: the card. */
  handoff?: Handoff;
  /** For 改派: the lane it forked from. */
  from?: string;
  /** For 等你: a question nobody has answered yet. */
  open?: boolean;
}

export interface Lane {
  agent: string;
  name: string;
  tasks: string[];
  nodes: LaneNode[];
}

export interface Swimlane {
  lanes: Lane[];
  /** Number of slots on the time axis (the landing included). */
  slots: number;
  landed: LaneNode | null;
}

export const GLYPH: Record<NodeKind, string> = {
  handoff: "⇢",
  reroute: "⑂",
  commit: "↑",
  test_pass: "✓",
  test_fail: "✗",
  retry: "↻",
  merge: "⤓",
  land: "⚑",
  compact: "⇊",
  model: "⇄",
  ask: "?",
  answer: "↩",
};

export const KIND_TEXT: Record<NodeKind, string> = {
  handoff: "交接",
  reroute: "改派",
  commit: "提交",
  test_pass: "测试通过",
  test_fail: "测试失败",
  retry: "重试",
  merge: "合入",
  land: "落地",
  compact: "上下文压缩",
  model: "模型切换",
  ask: "等你",
  answer: "已回答",
};

type Draft = Omit<LaneNode, "slot" | "id"> & { agent: string };

const time = (iso: string) => {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? 0 : t;
};

/** Who held the task at `at`: the last card given by then, else the first agent on its lane. */
export function holderAt(task: TaskTrace, at: string): string | null {
  let who: string | null = null;
  for (const h of [...task.handoffs].sort((a, b) => time(a.at) - time(b.at))) {
    if (time(h.at) <= time(at)) who = h.to;
  }
  return who ?? task.handoffs[0]?.to ?? task.lane.agents[0] ?? null;
}

function taskDrafts(task: TaskTrace): Draft[] {
  const out: Draft[] = [];
  const key = task.key;
  for (const h of task.handoffs) {
    const fork = h.kind !== "assign" && !!h.from;
    out.push({
      agent: h.to,
      at: h.at,
      kind: fork ? "reroute" : "handoff",
      task: key,
      label: fork ? `改派 #${key}` : `接手 #${key}`,
      detail: h.reason ?? h.summary ?? null,
      tone: fork ? "warn" : "accent",
      handoff: h,
      from: fork ? (h.from ?? undefined) : undefined,
    });
  }
  for (const s of task.statuses ?? []) {
    const agent = s.agent ?? holderAt(task, s.at);
    if (!agent) continue;
    if (s.to === "verifying") out.push({ agent, at: s.at, kind: "commit", task: key, label: `提交 #${key}`, detail: null, tone: "accent" });
    else if (s.to === "retrying") out.push({ agent, at: s.at, kind: "retry", task: key, label: `重试 #${key}`, detail: task.failure, tone: "warn" });
    else if (s.to === "merged") out.push({ agent, at: s.at, kind: "merge", task: key, label: `合入 #${key}`, detail: null, tone: "ok" });
  }
  for (const c of task.checks) {
    const agent = holderAt(task, c.at);
    if (!agent) continue;
    const bad = c.checks.filter((r) => !r.passed);
    out.push({
      agent,
      at: c.at,
      kind: c.passed ? "test_pass" : "test_fail",
      task: key,
      label: `${c.passed ? "测试 ✓" : "测试 ✗"} #${key}`,
      detail: c.passed ? `${c.checks.length} 项检查通过` : bad.map((r) => `${r.command} → ${r.exit_code ?? "?"}`).join("\n") || "检查未通过",
      tone: c.passed ? "ok" : "bad",
    });
  }
  out.push(...markDrafts(task.lane.marks ?? [], key, (at) => holderAt(task, at)));
  return out;
}

function markDrafts(marks: NonNullable<TaskTrace["lane"]["marks"]>, key: string | null, holder: (at: string) => string | null): Draft[] {
  const out: Draft[] = [];
  marks.forEach((m, i) => {
    const agent = m.agent || holder(m.at);
    if (!agent) return;
    const text = m.text ?? null;
    switch (m.kind) {
      case "compacted":
        out.push({ agent, at: m.at, kind: "compact", task: key, label: text ? `压缩 ${text}` : "上下文压缩", detail: text, tone: "muted" });
        break;
      case "model_changed":
        out.push({ agent, at: m.at, kind: "model", task: key, label: text ? `模型 ${text}` : "模型切换", detail: text, tone: "muted" });
        break;
      case "asked": {
        const answered = marks.slice(i + 1).some((n) => n.kind === "answered" && n.agent === m.agent);
        out.push({ agent, at: m.at, kind: "ask", task: key, label: answered ? "问你" : "等你", detail: text, tone: answered ? "muted" : "warn", open: !answered });
        break;
      }
      case "answered":
        out.push({ agent, at: m.at, kind: "answer", task: key, label: "你已回答", detail: text, tone: "ok" });
        break;
    }
  });
  return out;
}

/** Folds a run trace into lanes. `name` gives an agent's name when no card named it. */
export function buildSwimlane(trace: RunTrace, name: (id: string) => string = (id) => id.slice(0, 8)): Swimlane {
  const drafts: Draft[] = [];
  const names = new Map<string, string>();
  const tasksOf = new Map<string, string[]>();
  const order: string[] = [];
  const see = (agent: string, task?: string) => {
    if (!tasksOf.has(agent)) {
      tasksOf.set(agent, []);
      order.push(agent);
    }
    if (task && !tasksOf.get(agent)!.includes(task)) tasksOf.get(agent)!.push(task);
  };
  for (const task of trace.tasks) {
    for (const h of task.handoffs) {
      names.set(h.to, h.to_name);
      if (h.from && h.from_name && !names.has(h.from)) names.set(h.from, h.from_name);
    }
    const mine = taskDrafts(task);
    for (const a of [...task.handoffs.map((h) => h.to), ...task.lane.agents]) see(a, task.key);
    for (const d of mine) see(d.agent, task.key);
    drafts.push(...mine);
  }
  // A fork needs the lane it came from, even if that worker left nothing else.
  for (const d of drafts) if (d.from && !tasksOf.has(d.from)) see(d.from);
  // The lead's own questions and switches (those of no task) get the lead's lane.
  const lead = markDrafts(trace.lead.marks ?? [], null, () => trace.lead.agents[0] ?? null);
  for (const d of lead) see(d.agent);
  drafts.push(...lead);

  const sorted = drafts
    .map((d, i) => ({ d, i }))
    .sort((a, b) => time(a.d.at) - time(b.d.at) || a.i - b.i)
    .map(({ d }) => d);
  const lanes = new Map<string, Lane>(order.map((a) => [a, { agent: a, name: names.get(a) ?? name(a), tasks: tasksOf.get(a)!, nodes: [] }]));
  sorted.forEach((d, slot) => {
    const { agent, ...node } = d;
    lanes.get(agent)!.nodes.push({ ...node, id: `${agent}:${slot}`, slot });
  });
  let slots = sorted.length;
  let landed: LaneNode | null = null;
  if (trace.landed) {
    const l = trace.landed;
    landed = { id: "landed", at: l.at, kind: "land", task: null, label: "落地", detail: `${l.mode} · ${l.by} · ${l.reference}`, tone: "ok", slot: slots };
    slots += 1;
  }
  // First seen first: the order work was given out.
  const list = [...lanes.values()].sort((a, b) => first(a) - first(b));
  return { lanes: list, slots, landed };
}

const first = (l: Lane) => (l.nodes.length ? l.nodes[0].slot : Number.MAX_SAFE_INTEGER);

/** Focus mode: only one lane, the axis packed to its own nodes. */
export function focusLane(s: Swimlane, agent: string | null): Swimlane {
  if (!agent) return s;
  const lane = s.lanes.find((l) => l.agent === agent);
  if (!lane) return s;
  const nodes = lane.nodes.map((n, i) => ({ ...n, slot: i }));
  const landed = s.landed ? { ...s.landed, slot: nodes.length } : null;
  return { lanes: [{ ...lane, nodes }], slots: nodes.length + (landed ? 1 : 0), landed };
}

/** Questions still waiting for the person, across lanes. */
export const waitingOnYou = (s: Swimlane) => s.lanes.reduce((n, l) => n + l.nodes.filter((x) => x.open).length, 0);

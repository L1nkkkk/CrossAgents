// The run's swimlane (§十三): a column per worker, time running down, and on each column what
// happened to it — 交接, 提交, 测试 ✓/✗, 重试, 改派 (forking from the worker before), 合入,
// 上下文压缩, 模型切换, 等你 — ending in 落地. Click a column's head to see only that worker;
// click a node for its detail, and on a 交接/改派 for the handoff card.

import { useMemo, useState } from "react";
import { api } from "./api";
import { ErrorBox, agentName, useAgents } from "./components";
import { HandoffCard } from "./handoff";
import { useHubData } from "./hub";
import { GLYPH, KIND_TEXT, buildSwimlane, focusLane, waitingOnYou, type LaneNode, type Swimlane } from "./lib/swimlane";
import { clock } from "./lib/time";
import "./swimlane.css";

export function RunSwimlane({ runId }: { runId: string }) {
  const agents = useAgents();
  const trace = useHubData(
    () => api.runTrace(runId),
    [runId],
    (e) =>
      ((e.type === "task.status_changed" || e.type === "task.checked" || e.type === "task.rerouted" || e.type === "task.handed_off") && e.data.run_id === runId) ||
      (e.type === "run.landed" && e.data.id === runId) ||
      ((e.type === "human.asked" || e.type === "human.answered") && e.data.item.run_id === runId),
  );
  const [focus, setFocus] = useState<string | null>(null);
  const [picked, setPicked] = useState<string | null>(null);
  const all = useMemo(() => (trace.data ? buildSwimlane(trace.data, (id) => agentName(agents.data, id)) : null), [trace.data, agents.data]);
  if (trace.error) return <ErrorBox error={trace.error} />;
  if (!all) return <p className="muted small">加载泳道…</p>;
  if (all.lanes.length === 0) return <p className="muted small">还没有 Worker 开工。</p>;
  const shown = focusLane(all, focus);
  const node = picked ? (shown.lanes.flatMap((l) => l.nodes).find((n) => n.id === picked) ?? (shown.landed?.id === picked ? shown.landed : null)) : null;
  const waiting = waitingOnYou(all);
  return (
    <div className="swim">
      <div className="swim-bar small">
        {focus ? (
          <button className="swim-back" onClick={() => setFocus(null)}>
            ← 全部 {all.lanes.length} 根线
          </button>
        ) : (
          <span className="muted">{all.lanes.length} 个 Worker · 点名字只看一根线</span>
        )}
        {waiting > 0 && <span className="chip warn">等你 {waiting}</span>}
      </div>
      <Grid swim={shown} focused={!!focus} picked={picked} onPick={(id) => setPicked(id === picked ? null : id)} onFocus={(a) => setFocus(a === focus ? null : a)} />
      {node && <Detail node={node} agents={agents.data} name={(id) => all.lanes.find((l) => l.agent === id)?.name ?? agentName(agents.data, id)} />}
    </div>
  );
}

const ROW = 30;
const FOCUS_X = "18px";
// The line runs near a column's left edge, so a node's label has the rest of the column.
const laneX = (i: number, cols: number, focused: boolean) => (focused ? FOCUS_X : `calc(${i / cols} * 100% + 16px)`);

function Grid({ swim, focused, picked, onPick, onFocus }: { swim: Swimlane; focused: boolean; picked: string | null; onPick: (id: string) => void; onFocus: (agent: string) => void }) {
  const col = new Map(swim.lanes.map((l, i) => [l.agent, i]));
  const cols = swim.lanes.length;
  const forks = swim.lanes.flatMap((l) => l.nodes.filter((n) => n.from && col.has(n.from)).map((n) => ({ n, a: col.get(n.from!)!, b: col.get(l.agent)! })));
  const height = Math.max(swim.slots, 1) * ROW;
  return (
    <div className={`swim-grid ${focused ? "focused" : ""}`} style={{ ["--cols" as string]: cols }}>
      {swim.lanes.map((l, i) => (
        <button key={l.agent} className="swim-head" style={{ gridColumn: i + 1 }} onClick={() => onFocus(l.agent)} title={`#${l.tasks.join(" #")}${focused ? " · 点此回到全部" : " · 只看这根线"}`}>
          <span className="swim-name">{l.name}</span>
          <span className="swim-tasks muted">{l.tasks.map((t) => `#${t}`).join(" ")}</span>
        </button>
      ))}
      <div className="swim-body" style={{ height }}>
        {swim.lanes.map((l, i) => {
          const firstSlot = l.nodes[0]?.slot ?? 0;
          const lastSlot = l.nodes[l.nodes.length - 1]?.slot ?? 0;
          return <div key={l.agent} className="swim-line" style={{ left: laneX(i, cols, focused), top: firstSlot * ROW + ROW / 2, height: Math.max(0, lastSlot - firstSlot) * ROW }} />;
        })}
        {forks.map(({ n, a, b }) => (
          <div
            key={`fork-${n.id}`}
            className="swim-fork"
            style={{
              left: laneX(Math.min(a, b), cols, false),
              width: `calc(${Math.abs(b - a) / cols} * 100%)`,
              top: n.slot * ROW + ROW / 2,
            }}
          />
        ))}
        {swim.lanes.map((l, i) =>
          l.nodes.map((n) => <Node key={n.id} node={n} left={laneX(i, cols, focused)} wide={focused} picked={picked === n.id} onPick={onPick} />),
        )}
        {swim.landed && (
          <button className={`swim-land ${picked === "landed" ? "picked" : ""}`} style={{ top: swim.landed.slot * ROW + 4 }} onClick={() => onPick("landed")} title={swim.landed.detail ?? ""}>
            {GLYPH.land} 落地 · {clock(swim.landed.at)}
          </button>
        )}
      </div>
    </div>
  );
}

function Node({ node: n, left, wide, picked, onPick }: { node: LaneNode; left: string; wide: boolean; picked: boolean; onPick: (id: string) => void }) {
  return (
    <button
      className={`swim-node ${n.tone} ${n.open ? "open" : ""} ${picked ? "picked" : ""}`}
      style={{ left, top: n.slot * ROW + ROW / 2 }}
      onClick={() => onPick(n.id)}
      title={`${KIND_TEXT[n.kind]}${n.task ? ` #${n.task}` : ""} · ${clock(n.at)}${n.detail ? `\n${n.detail}` : ""}`}
      data-kind={n.kind}
    >
      <span className="swim-dot" aria-hidden="true">
        {GLYPH[n.kind]}
      </span>
      <span className="swim-label">
        {n.label}
        {wide && <span className="muted"> · {clock(n.at)}</span>}
      </span>
    </button>
  );
}

function Detail({ node: n, agents, name }: { node: LaneNode; agents: Parameters<typeof HandoffCard>[0]["agents"]; name: (id: string) => string }) {
  return (
    <div className="swim-detail">
      <div className="small">
        <strong>{KIND_TEXT[n.kind]}</strong>
        {n.task && <code> #{n.task}</code>}
        <span className="muted"> · {clock(n.at)}</span>
        {n.from && <span className="muted"> · 从 {name(n.from)} 分出</span>}
      </div>
      {n.detail && <pre className="swim-text small">{n.detail}</pre>}
      {n.handoff && <HandoffCard handoff={n.handoff} agents={agents} />}
    </div>
  );
}

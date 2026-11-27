// QQ-style Mini Floating Window (Gaming HUD):
// Always-on-top compact status widget with OS-level opacity control, auto-fade on blur,
// collapsible 1-line status pill (~286x52) or compact live feed (~312x248) showing running
// agents, pending approvals/questions, and newly completed sessions at a glance.

import { useMemo, type MouseEvent as ReactMouseEvent } from "react";
import type { Agent } from "@bindings/Agent";
import { ChevronDown, ChevronUp, Eye, Maximize2, PanelRight, Pin } from "lucide-react";
import { deliveryOf, lastError, useActivity, useDelivery, useUnread } from "../activity";
import { navigate, useRunTree } from "../App";
import { Avatar, BrandMark, displayName, useAgents } from "../components";
import { summaryText } from "../lib/sidebarTree";
import { todoItems } from "../lib/todos";
import { stepLine } from "../lib/workGroups";
import { RowElapsed, useRowLive } from "../liveTurn";
import { useQuestions } from "../questions";
import { ScopedNameLabel } from "../sessionRef";
import { inTauri } from "../titlebar";
import {
  cycleWindowOpacity,
  setMiniCollapsed,
  setWindowAlwaysOnTop,
  setWindowAutoFade,
  setWindowMode,
  useWindowMode,
} from "../windowMode";

export function MiniWindow() {
  const wm = useWindowMode();
  const agentsData = useAgents();
  const tree = useRunTree();
  const questions = useQuestions();
  const unread = useUnread();

  const agents = useMemo(() => (agentsData.data ?? []).filter((a) => !a.archived_at), [agentsData.data]);
  const runs = tree.data ?? [];
  const todos = useMemo(
    () => todoItems({ items: questions.data?.items ?? [], runs, now: Date.now() }),
    [questions.data, runs],
  );

  const isRunning = (a: Agent) => {
    const d = deliveryOf(a.id)?.state;
    return d ? d !== "idle" : a.status === "running";
  };

  const running = useMemo(() => agents.filter(isRunning), [agents, unread]);
  const doneUnread = useMemo(
    () => agents.filter((a) => !isRunning(a) && (unread[a.id]?.kind === "done" || unread[a.id]?.kind === "error")),
    [agents, unread],
  );

  const onDrag = (event: ReactMouseEvent<HTMLElement>) => {
    if (!inTauri || event.button !== 0) return;
    if ((event.target as Element).closest("button, a, input")) return;
    window.crossagents?.command?.("drag");
  };

  const openInClassic = (agentId?: string) => {
    if (agentId) navigate("s", agentId);
    void setWindowMode("classic");
  };

  const openInEfficiency = (agentId?: string) => {
    if (agentId) navigate("s", agentId);
    void setWindowMode("efficiency");
  };

  const headline = todos.length > 0
    ? `${todos.length} 项等你处理`
    : running.length > 0
      ? `${running.length} 个正在运行`
      : doneUnread.length > 0
        ? `${doneUnread.length} 个已完成`
        : "全部空闲";

  if (wm.miniCollapsed) {
    return (
      <div className="mini-shell collapsed" onMouseDown={onDrag}>
        <span className={`mini-pulse ${todos.length > 0 ? "warn" : running.length > 0 ? "live" : doneUnread.length > 0 ? "ok" : ""}`} />
        <BrandMark size={15} />
        <span className="mini-pill-text" title={headline}>
          {headline}
          {running[0] ? ` · ${displayName(running[0])}` : ""}
        </span>
        <div className="spacer" />
        <button
          type="button"
          className="mini-btn"
          onClick={() => void setMiniCollapsed(false)}
          title="展开迷你监控卡片"
          aria-label="展开"
        >
          <ChevronDown size={13} />
        </button>
        <button
          type="button"
          className="mini-btn"
          onClick={() => openInClassic()}
          title="切换到经典窄面板（Alt+Shift+2）"
          aria-label="经典模式"
        >
          <PanelRight size={12} />
        </button>
        <button
          type="button"
          className="mini-btn"
          onClick={() => openInEfficiency()}
          title="切换到完整效率窗口（Alt+Shift+1）"
          aria-label="效率模式"
        >
          <Maximize2 size={12} />
        </button>
      </div>
    );
  }

  return (
    <div className="mini-shell">
      <header className="mini-bar" onMouseDown={onDrag}>
        <BrandMark size={14} />
        <span className="mini-title">{headline}</span>
        <div className="spacer" />
        <button
          type="button"
          className={`mini-btn ${wm.autoFade ? "on" : ""}`}
          onClick={() => setWindowAutoFade(!wm.autoFade)}
          title={wm.autoFade ? "失焦半透明已开启（点击常亮）" : "常亮（点击开启失焦半透明）"}
        >
          {Math.round(wm.opacity * 100)}%
        </button>
        <button
          type="button"
          className="mini-btn"
          onClick={cycleWindowOpacity}
          title="切换透明度（100% / 85% / 65% / 45%）"
          aria-label="切换透明度"
        >
          <Eye size={12} />
        </button>
        <button
          type="button"
          className={`mini-btn ${wm.alwaysOnTop ? "on" : ""}`}
          onClick={() => void setWindowAlwaysOnTop(!wm.alwaysOnTop)}
          title={wm.alwaysOnTop ? "取消置顶" : "窗口置顶"}
          aria-pressed={wm.alwaysOnTop}
        >
          <Pin size={12} />
        </button>
        <button
          type="button"
          className="mini-btn"
          onClick={() => void setMiniCollapsed(true)}
          title="收起为一行状态条"
          aria-label="收起"
        >
          <ChevronUp size={13} />
        </button>
        <button
          type="button"
          className="mini-btn"
          onClick={() => openInClassic()}
          title="切换到经典窄面板（Alt+Shift+2）"
          aria-label="经典模式"
        >
          <PanelRight size={12} />
        </button>
        <button
          type="button"
          className="mini-btn"
          onClick={() => openInEfficiency()}
          title="切换到完整效率窗口（Alt+Shift+1）"
          aria-label="效率模式"
        >
          <Maximize2 size={12} />
        </button>
      </header>

      <div className="mini-feed">
        {todos.slice(0, 3).map((item) => {
          const target = agents.find((a) => a.id === item.agent);
          return (
            <button
              key={item.kind + item.id}
              type="button"
              className="mini-item warn"
              onClick={() => openInClassic(target?.id)}
            >
              <span className="mini-badge warn">等你</span>
              <span className="mini-item-body">
                <strong>{target ? <ScopedNameLabel name={target.name} /> : "待办"}</strong>
                <span className="mini-item-sub">{item.summary}</span>
              </span>
            </button>
          );
        })}

        {running.map((agent) => (
          <MiniAgentRow key={agent.id} agent={agent} kind="running" onOpen={() => openInClassic(agent.id)} />
        ))}

        {doneUnread.slice(0, 3).map((agent) => (
          <MiniAgentRow key={agent.id} agent={agent} kind="done" onOpen={() => openInClassic(agent.id)} />
        ))}

        {todos.length === 0 && running.length === 0 && doneUnread.length === 0 && (
          <div className="mini-idle">
            <div>当前 {agents.length} 个会话全部空闲</div>
            <div className="mini-idle-sub">有新进度、完成或待审批时会实时浮在这里</div>
          </div>
        )}
      </div>
    </div>
  );
}

function MiniAgentRow({ agent, kind, onOpen }: { agent: Agent; kind: "running" | "done"; onOpen: () => void }) {
  const delivery = useDelivery(agent.id);
  const doing = useActivity(agent.id);
  const turn = useRowLive(agent);
  const newest = turn?.items.at(-1);
  const step = kind === "running" && newest?.type === "tool_call" ? stepLine(newest) : "";
  const isError = agent.status === "error";
  const line = isError
    ? lastError(agent.id) || "运行出错"
    : kind === "running"
      ? step || summaryText(doing) || summaryText(agent.topic) || "正在运行…"
      : summaryText(agent.topic) || "本轮已完成";

  return (
    <button type="button" className={`mini-item ${kind} ${isError ? "danger" : ""}`} onClick={onOpen}>
      <span className={`face ${kind === "running" ? "running" : ""}`}>
        <Avatar agent={agent} size={22} />
      </span>
      <span className="mini-item-body">
        <span className="mini-item-head">
          <strong>
            <ScopedNameLabel name={agent.name} />
          </strong>
          {kind === "running" && delivery?.state === "turn" ? (
            <RowElapsed agentId={agent.id} since={turn?.since} at={turn?.at} items={turn?.items ?? []} />
          ) : (
            <span className={`mini-tag ${isError ? "danger" : "ok"}`}>{isError ? "出错" : "完成"}</span>
          )}
        </span>
        <span className="mini-item-sub">
          {kind === "running" && <span className="step-mark">⎿ </span>}
          {line}
        </span>
      </span>
    </button>
  );
}

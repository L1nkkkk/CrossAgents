// QQ-style Mini Floating Window (Gaming HUD):
// Always-on-top compact status overlay with OS-level opacity control, auto-fade on blur,
// live status glow strip, multi-node OS badges, collapsible 46px pill (~300x46), or rich
// compact live monitor card (~332x268) showing pending actions, active runs, unread completions,
// and quick-jump recent sessions when idle.

import { useMemo, type MouseEvent as ReactMouseEvent } from "react";
import type { Agent } from "@bindings/Agent";
import { Check, ChevronDown, ChevronUp, Eye, Maximize2, PanelRight, Pin } from "lucide-react";
import { deliveryOf, lastError, markSeen, useActivity, useDelivery, useUnread } from "../activity";
import { navigate, useRunTree } from "../App";
import { Avatar, BrandMark, ModelMark, displayName, useAgents } from "../components";
import { CENTRE_MACHINE, agentMachine, shortSince, summaryText } from "../lib/sidebarTree";
import { todoItems } from "../lib/todos";
import { stepLine } from "../lib/workGroups";
import { RowElapsed, useRowLive } from "../liveTurn";
import { normalizeNodeOs, useNodes } from "../nodes";
import { OsIcon } from "../osIcon";
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
import { homeOf, isChat, wsName } from "../workspace";

type NodeMeta = { label: string; os?: string; remote: boolean; online: boolean };

export function MiniWindow() {
  const wm = useWindowMode();
  const agentsData = useAgents();
  const nodesData = useNodes();
  const tree = useRunTree();
  const questions = useQuestions();
  const unread = useUnread();

  const agents = useMemo(() => (agentsData.data ?? []).filter((a) => !a.archived_at), [agentsData.data]);
  const remoteNodes = nodesData.data ?? [];
  const nodeMetaMap = useMemo(() => {
    const map = new Map<string, NodeMeta>();
    for (const n of remoteNodes) {
      map.set(n.id, { label: n.name || n.id, os: normalizeNodeOs(n.os), remote: true, online: n.online });
    }
    return map;
  }, [remoteNodes]);
  const nodeOf = (agent: Agent): NodeMeta | undefined => {
    const k = agentMachine(agent);
    if (k === CENTRE_MACHINE) return undefined;
    return nodeMetaMap.get(k) ?? { label: k, remote: true, online: false };
  };

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
  const recentIdle = useMemo(
    () =>
      agents
        .slice()
        .sort((a, b) => b.created_at.localeCompare(a.created_at))
        .slice(0, 3),
    [agents],
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

  const tone =
    todos.length > 0 ? "warn" : running.length > 0 ? "live" : doneUnread.length > 0 ? "ok" : "idle";

  const headline = todos.length > 0
    ? `${todos.length} 项等你`
    : running.length > 0
      ? `${running.length} 运行中`
      : doneUnread.length > 0
        ? `${doneUnread.length} 已完成`
        : "全部空闲";

  const leadAgent = running[0] ?? doneUnread[0];
  const leadNode = leadAgent ? nodeOf(leadAgent) : undefined;

  if (wm.miniCollapsed) {
    return (
      <div className={`mini-shell collapsed tone-${tone}`} onMouseDown={onDrag}>
        <span className={`mini-pulse ${tone}`} />
        <BrandMark size={14} />
        <span className="mini-pill-text" title={headline}>
          <span className="mini-pill-state">{headline}</span>
          {leadAgent && (
            <span className="mini-pill-who">
              {" · "}
              {displayName(leadAgent)}
            </span>
          )}
        </span>
        {leadNode && (
          <span className={`mini-node-chip ${leadNode.online ? "" : "offline"}`} title={leadNode.label}>
            <OsIcon os={leadNode.os} size={10} />
          </span>
        )}
        <div className="spacer" />
        <div className="mini-pill-actions">
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
      </div>
    );
  }

  const hasLiveFeed = todos.length > 0 || running.length > 0 || doneUnread.length > 0;

  return (
    <div className={`mini-shell tone-${tone}`}>
      <div className={`mini-accent-bar ${tone}`} aria-hidden="true" />
      <header className="mini-bar" onMouseDown={onDrag}>
        <span className={`mini-pulse ${tone}`} />
        <BrandMark size={13} />
        <span className="mini-title">{headline}</span>
        {running.length > 0 && todos.length > 0 && (
          <span className="mini-sub-pill">{running.length} 运行</span>
        )}
        <div className="spacer" />
        <button
          type="button"
          className={`mini-btn mini-pct ${wm.autoFade ? "on" : ""}`}
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
          const nm = target ? nodeOf(target) : undefined;
          return (
            <button
              key={item.kind + item.id}
              type="button"
              className="mini-item warn"
              onClick={() => openInClassic(target?.id)}
            >
              <span className="mini-badge warn">等你</span>
              <span className="mini-item-body">
                <span className="mini-item-head">
                  <strong className="mini-item-name">
                    {target ? <ScopedNameLabel name={target.name} /> : "待办"}
                  </strong>
                  {nm && (
                    <span className={`mini-node-chip ${nm.online ? "" : "offline"}`}>
                      <OsIcon os={nm.os} size={10} />
                      <span>{nm.label}</span>
                    </span>
                  )}
                </span>
                <span className="mini-item-sub">{item.summary}</span>
              </span>
            </button>
          );
        })}

        {running.map((agent) => (
          <MiniAgentRow
            key={agent.id}
            agent={agent}
            node={nodeOf(agent)}
            kind="running"
            onOpen={() => openInClassic(agent.id)}
          />
        ))}

        {doneUnread.slice(0, 3).map((agent) => (
          <MiniAgentRow
            key={agent.id}
            agent={agent}
            node={nodeOf(agent)}
            kind="done"
            onOpen={() => openInClassic(agent.id)}
            onDismiss={() => markSeen(agent.id)}
          />
        ))}

        {!hasLiveFeed && (
          <div className="mini-idle-wrap">
            {recentIdle.length > 0 ? (
              <>
                <div className="mini-idle-label">最近会话 · 点开直接进入经典窗</div>
                {recentIdle.map((agent) => {
                  const nm = nodeOf(agent);
                  return (
                    <button
                      key={agent.id}
                      type="button"
                      className="mini-item idle"
                      onClick={() => openInClassic(agent.id)}
                    >
                      <Avatar agent={agent} size={22} />
                      <span className="mini-item-body">
                        <span className="mini-item-head">
                          <strong className="mini-item-name">
                            <ScopedNameLabel name={agent.name} />
                          </strong>
                          <ModelMark plugin={agent.plugin} model={agent.model} />
                          <span className="spacer" />
                          <span className="mini-time">{shortSince(agent.created_at, Date.now())}</span>
                        </span>
                        <span className="mini-item-sub-row">
                          <span className="mini-item-sub">
                            {summaryText(agent.topic) || (isChat(agent) ? "独立对话" : wsName(homeOf(agent)))}
                          </span>
                          {nm && (
                            <span className={`mini-node-chip ${nm.online ? "" : "offline"}`}>
                              <OsIcon os={nm.os} size={9} />
                              <span>{nm.label}</span>
                            </span>
                          )}
                        </span>
                      </span>
                    </button>
                  );
                })}
              </>
            ) : (
              <div className="mini-idle">
                <div>当前暂无活跃会话</div>
                <div className="mini-idle-sub">有新进度、完成或待审批时会实时浮在这里</div>
              </div>
            )}
          </div>
        )}
      </div>

      <footer className="mini-footer">
        <span>{agents.length} 会话</span>
        {remoteNodes.length > 0 && (
          <span> · {remoteNodes.filter((n) => n.online).length}/{remoteNodes.length} 远端在线</span>
        )}
        <span className="spacer" />
        <span className="mini-kbd">Alt+Shift+1/2/3</span>
      </footer>
    </div>
  );
}

function MiniAgentRow({
  agent,
  node,
  kind,
  onOpen,
  onDismiss,
}: {
  agent: Agent;
  node?: NodeMeta;
  kind: "running" | "done";
  onOpen: () => void;
  onDismiss?: () => void;
}) {
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
    <div className={`mini-item-wrap ${kind} ${isError ? "danger" : ""}`}>
      <button type="button" className={`mini-item ${kind} ${isError ? "danger" : ""}`} onClick={onOpen}>
        <span className={`face ${kind === "running" ? "running" : ""}`}>
          <Avatar agent={agent} size={24} />
        </span>
        <span className="mini-item-body">
          <span className="mini-item-head">
            <strong className="mini-item-name">
              <ScopedNameLabel name={agent.name} />
            </strong>
            <ModelMark plugin={agent.plugin} model={agent.model} />
            <span className="spacer" />
            {kind === "running" && delivery?.state === "turn" ? (
              <RowElapsed agentId={agent.id} since={turn?.since} at={turn?.at} items={turn?.items ?? []} />
            ) : (
              <span className={`mini-tag ${isError ? "danger" : "ok"}`}>{isError ? "出错" : "完成"}</span>
            )}
          </span>
          <span className="mini-item-sub-row">
            <span className="mini-item-sub">
              {kind === "running" && <span className="step-mark">⎿ </span>}
              {line}
            </span>
            {node && (
              <span className={`mini-node-chip ${node.online ? "" : "offline"}`} title={`节点：${node.label}`}>
                <OsIcon os={node.os} size={9} />
                <span>{node.label}</span>
              </span>
            )}
          </span>
        </span>
      </button>
      {onDismiss && (
        <button
          type="button"
          className="mini-dismiss"
          onClick={(e) => {
            e.stopPropagation();
            onDismiss();
          }}
          title="标为已读"
          aria-label="标为已读"
        >
          <Check size={12} />
        </button>
      )}
    </div>
  );
}

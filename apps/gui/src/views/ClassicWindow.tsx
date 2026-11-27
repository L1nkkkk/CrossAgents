// QQ-style Classic Narrow Panel mode:
// - Compact vertical floating panel (~368x780) with classic QQ-style left rail + live session list
// - Full multi-node distinction: local machine + remote nodes (OS icon, online status, node filter
//   pills, collapsible node sections, per-row remote node badges, node-aware quick launch)
// - Native Windows screen-edge magnetic snapping & smooth auto-hide drawer (with glowing 6px edge
//   status peek strip that slides open on hover without stealing game focus)

import { useEffect, useMemo, useState, type MouseEvent as ReactMouseEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { HubInfo } from "@bindings/HubInfo";
import type { RemoteNodeInfo } from "@bindings/RemoteNodeInfo";
import {
  ArrowLeft,
  Bell,
  CheckCheck,
  ChevronDown,
  ChevronRight,
  Eye,
  FolderKanban,
  Magnet,
  Maximize2,
  MessageSquare,
  PanelRightClose,
  Pin,
  Plus,
  Search,
  Settings,
  Zap,
} from "lucide-react";
import { deliveryOf, lastError, markAllSeen, useActivity, useDelivery, useUnread } from "../activity";
import { navigate, useRunTree } from "../App";
import { api, errorText } from "../api";
import { ChatAvatar } from "../ChatAvatar";
import { Avatar, BrandMark, Icon, ModelMark, displayName, useAgents } from "../components";
import { useConnectionStatus } from "../hub";
import { useChatProfile } from "../lib/chatProfile";
import { newChatRequest } from "../lib/sessionChoice";
import { isMac } from "../lib/shortcuts";
import { CENTRE_MACHINE, agentMachine, shortSince, summaryText } from "../lib/sidebarTree";
import { todoItems } from "../lib/todos";
import { stepLine } from "../lib/workGroups";
import { RowElapsed, useRowLive } from "../liveTurn";
import { normalizeNodeOs, useNodePlugins, useNodes } from "../nodes";
import { OsIcon } from "../osIcon";
import { sendMessage } from "../outbox";
import { useQuestions } from "../questions";
import { ScopedNameLabel, encodeSessionDrag } from "../sessionRef";
import { inTauri } from "../titlebar";
import {
  cycleWindowOpacity,
  dockWindowNow,
  setWindowAlwaysOnTop,
  setWindowAutoFade,
  setWindowEdgeDock,
  setWindowMode,
  useWindowMode,
} from "../windowMode";
import { homeOf, isChat, wsKey, wsName } from "../workspace";
import { SessionView } from "./Session";

type ClassicTab = "all" | "needs" | "running" | "projects";

type MachineSummary = {
  key: string;
  label: string;
  os?: string;
  remote: boolean;
  online: boolean;
  count: number;
  busy: number;
  needs: number;
  node?: RemoteNodeInfo;
};

export function ClassicWindow({ route }: { route: string[] }) {
  const wm = useWindowMode();
  const agentsData = useAgents();
  const tree = useRunTree();
  const questions = useQuestions();
  const nodesData = useNodes();
  const unread = useUnread();
  const status = useConnectionStatus();
  const profile = useChatProfile();

  const [hubInfo, setHubInfo] = useState<HubInfo>();
  useEffect(() => {
    if (status === "connected") api.info().then(setHubInfo, () => {});
  }, [status]);

  const [tab, setTab] = useState<ClassicTab>("all");
  const [selectedNode, setSelectedNode] = useState<string>("all");
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [foldedNodes, setFoldedNodes] = useState<Set<string>>(() => new Set());
  const [todosFolded, setTodosFolded] = useState(false);
  const [query, setQuery] = useState("");
  const [openSessionId, setOpenSessionId] = useState<string | null>(() => (route[0] === "s" && route[1] ? route[1] : null));
  const [quickNew, setQuickNew] = useState(false);
  const [quickNode, setQuickNode] = useState<string>("");
  const [quickPrompt, setQuickPrompt] = useState("");
  const [quickBusy, setQuickBusy] = useState(false);
  const [quickError, setQuickError] = useState<string>();

  const quickPlugins = useNodePlugins(quickNode);
  const agents = useMemo(() => (agentsData.data ?? []).filter((a) => !a.archived_at), [agentsData.data]);
  const remoteNodes = nodesData.data ?? [];
  const remoteNodeMap = useMemo(() => new Map(remoteNodes.map((n) => [n.id, n])), [remoteNodes]);
  const runs = tree.data ?? [];
  const todos = useMemo(
    () => todoItems({ items: questions.data?.items ?? [], runs, now: Date.now() }),
    [questions.data, runs],
  );
  const todoIdSet = useMemo(() => new Set(todos.map((t) => t.agent)), [todos]);

  const isRunning = (a: Agent) => {
    const d = deliveryOf(a.id)?.state;
    return d ? d !== "idle" : a.status === "running";
  };

  const runningAgents = useMemo(() => agents.filter(isRunning), [agents, unread]);
  const unreadCount = useMemo(() => Object.values(unread).filter((u) => u.kind === "done" || u.kind === "error").length, [unread]);

  // All execution machines: centre first, then known remote nodes, then any historical node ids.
  const machines = useMemo<MachineSummary[]>(() => {
    const order: string[] = [CENTRE_MACHINE];
    const seen = new Set<string>(order);
    for (const n of remoteNodes) {
      if (!seen.has(n.id)) {
        seen.add(n.id);
        order.push(n.id);
      }
    }
    for (const a of agents) {
      const k = agentMachine(a);
      if (!seen.has(k)) {
        seen.add(k);
        order.push(k);
      }
    }
    return order.map((key) => {
      const list = agents.filter((a) => agentMachine(a) === key);
      const node = key === CENTRE_MACHINE ? undefined : remoteNodeMap.get(key);
      const remote = key !== CENTRE_MACHINE;
      const busy = list.filter(isRunning).length;
      const needs = list.filter((a) => todoIdSet.has(a.id) || a.status === "error").length;
      return {
        key,
        label: remote ? node?.name || key : hubInfo?.machine?.hostname || "本机",
        os: remote ? normalizeNodeOs(node?.os) : normalizeNodeOs(hubInfo?.machine?.os),
        remote,
        online: remote ? Boolean(node?.online) : status === "connected",
        count: list.length,
        busy,
        needs,
        node,
      };
    });
  }, [agents, remoteNodes, remoteNodeMap, todoIdSet, hubInfo?.machine?.hostname, hubInfo?.machine?.os, status, unread]);

  const machineMap = useMemo(() => new Map(machines.map((m) => [m.key, m])), [machines]);
  const hasMultipleNodes = machines.length > 1;
  const onlineRemoteCount = remoteNodes.filter((n) => n.online).length;

  // Projects scoped by (machineKey, wsKey) so identical folder names across OSes never collide.
  const projects = useMemo(() => {
    const map = new Map<string, { key: string; machineKey: string; name: string; path: string; count: number; busy: number }>();
    for (const a of agents) {
      const mk = agentMachine(a);
      if (selectedNode !== "all" && mk !== selectedNode) continue;
      const chat = isChat(a);
      const path = chat ? "" : homeOf(a);
      const pk = chat ? "__chats" : wsKey(path);
      const key = `${mk}::${pk}`;
      const name = chat ? "独立对话" : wsName(path);
      const cur = map.get(key) ?? { key, machineKey: mk, name, path, count: 0, busy: 0 };
      cur.count += 1;
      if (isRunning(a)) cur.busy += 1;
      map.set(key, cur);
    }
    return [...map.values()].sort((a, b) => b.busy - a.busy || b.count - a.count || a.name.localeCompare(b.name));
  }, [agents, selectedNode, unread]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    let list = agents;
    if (selectedNode !== "all") {
      list = list.filter((a) => agentMachine(a) === selectedNode);
    }
    if (tab === "needs") list = list.filter((a) => todoIdSet.has(a.id) || a.status === "error");
    else if (tab === "running") list = list.filter(isRunning);
    else if (tab === "projects" && selectedProject) {
      list = list.filter((a) => {
        const mk = agentMachine(a);
        const pk = isChat(a) ? "__chats" : wsKey(homeOf(a));
        return `${mk}::${pk}` === selectedProject;
      });
    }
    if (q) {
      list = list.filter((a) => {
        const m = machineMap.get(agentMachine(a));
        return [a.name, a.topic, a.model, m?.label, isChat(a) ? "独立对话" : wsName(homeOf(a))]
          .filter(Boolean)
          .some((s) => s!.toLowerCase().includes(q));
      });
    }
    return list.slice().sort((a, b) => {
      const aNeed = todoIdSet.has(a.id) || a.status === "error" ? 1 : 0;
      const bNeed = todoIdSet.has(b.id) || b.status === "error" ? 1 : 0;
      if (aNeed !== bNeed) return bNeed - aNeed;
      const aRun = isRunning(a) ? 1 : 0;
      const bRun = isRunning(b) ? 1 : 0;
      if (aRun !== bRun) return bRun - aRun;
      const aUnread = unread[a.id] ? 1 : 0;
      const bUnread = unread[b.id] ? 1 : 0;
      if (aUnread !== bUnread) return bUnread - aUnread;
      return b.created_at.localeCompare(a.created_at);
    });
  }, [agents, todoIdSet, tab, selectedNode, selectedProject, query, unread, machineMap]);

  // Group filtered list by node whenever multiple nodes exist and user hasn't isolated one node.
  const groupedByNode = useMemo(() => {
    if (!hasMultipleNodes || selectedNode !== "all" || tab === "needs" || tab === "running") return null;
    const by = new Map<string, Agent[]>();
    for (const m of machines) by.set(m.key, []);
    for (const a of filtered) {
      const k = agentMachine(a);
      const list = by.get(k) ?? [];
      list.push(a);
      by.set(k, list);
    }
    return machines
      .map((m) => ({ machine: m, agents: by.get(m.key) ?? [] }))
      .filter((g) => g.agents.length > 0 || (!query.trim() && g.machine.remote && g.machine.online));
  }, [hasMultipleNodes, selectedNode, tab, machines, filtered, query]);

  const toggleNodeFold = (key: string) => {
    setFoldedNodes((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const onDragTitle = (event: ReactMouseEvent<HTMLElement>) => {
    if (!inTauri || event.button !== 0) return;
    if ((event.target as Element).closest("button, a, input, select")) return;
    window.crossagents?.command?.("drag");
  };

  async function startQuickChat() {
    const text = quickPrompt.trim();
    if (!text || quickBusy) return;
    const offered = (quickPlugins.data ?? []).filter((p) => p.models.length > 0);
    if (!offered.length) {
      setQuickError(quickNode ? "该远程节点暂无可用模型插件" : "没有可用的模型插件");
      return;
    }
    setQuickBusy(true);
    setQuickError(undefined);
    try {
      const targetMachine = quickNode || CENTRE_MACHINE;
      const matchingProj = projects.find(
        (p) => p.machineKey === targetMachine && (!selectedProject || p.key === selectedProject) && p.path,
      );
      const remoteDefault = quickNode ? remoteNodeMap.get(quickNode)?.default_cwd ?? "" : "";
      const cwd = matchingProj?.path || remoteDefault;
      const body = newChatRequest({ plugin: offered[0].name, model: offered[0].models[0] }, text, cwd, quickNode || undefined);
      const created = await api.createAgent(body);
      void sendMessage(created.id, text).catch(() => {});
      setQuickPrompt("");
      setQuickNew(false);
      setOpenSessionId(created.id);
      navigate("s", created.id);
    } catch (err) {
      setQuickError(errorText(err));
    } finally {
      setQuickBusy(false);
    }
  }

  const activeSession = openSessionId ? agents.find((a) => a.id === openSessionId) : undefined;
  const activeMachine = activeSession ? machineMap.get(agentMachine(activeSession)) : undefined;

  // Global status color for the 6px tucked-in edge peek strip.
  const peekTone =
    todos.length > 0 ? "warn" : runningAgents.length > 0 ? "live" : unreadCount > 0 ? "ok" : "idle";

  return (
    <div className={`classic-shell ${wm.dockedEdge !== "none" ? `docked-${wm.dockedEdge}` : ""} ${wm.dockHidden ? "dock-hidden" : ""}`}>
      {/* Sleek glowing peek handle shown on the exposed 6px sliver when tucked into screen edge */}
      {wm.dockHidden && wm.dockedEdge !== "none" && (
        <div
          className={`classic-peek-strip edge-${wm.dockedEdge} tone-${peekTone}`}
          aria-label="CrossAgents 已贴边收起，悬停展开"
          title={`CrossAgents（${todos.length > 0 ? `${todos.length} 项等你` : runningAgents.length > 0 ? `${runningAgents.length} 运行中` : "悬停展开"}）`}
        >
          <span className="classic-peek-pill" />
        </div>
      )}

      {/* Top draggable QQ-style classic bar */}
      <header className="classic-titlebar" onMouseDown={onDragTitle}>
        <div className="classic-brand">
          <BrandMark size={15} />
          <span>CrossAgents</span>
          {wm.dockedEdge !== "none" && <span className="classic-dock-dot" title="已吸附屏幕边缘（移开鼠标自动收起）" />}
        </div>
        <div className="spacer" />
        <div className="classic-tb-group">
          <button
            type="button"
            className={`classic-tb-btn ${wm.edgeDock ? "on" : ""}`}
            onClick={() => void setWindowEdgeDock(!wm.edgeDock)}
            title={
              wm.edgeDock
                ? "贴边抽屉已开启（拖到屏幕左/右/上边缘自动吸附隐藏，移入边缘滑出）"
                : "开启 QQ 式贴边自动隐藏"
            }
            aria-pressed={wm.edgeDock}
          >
            <Magnet size={13} />
          </button>
          <button
            type="button"
            className={`classic-tb-btn ${wm.alwaysOnTop ? "on" : ""}`}
            onClick={() => void setWindowAlwaysOnTop(!wm.alwaysOnTop)}
            title={wm.alwaysOnTop ? "取消窗口置顶" : "窗口置顶（浮在其他应用上方）"}
            aria-pressed={wm.alwaysOnTop}
          >
            <Pin size={13} />
          </button>
          <button
            type="button"
            className={`classic-tb-btn ${wm.opacity < 0.99 ? "on" : ""}`}
            onClick={cycleWindowOpacity}
            title={`调整窗口透明度（当前 ${Math.round(wm.opacity * 100)}%）`}
          >
            <Eye size={13} />
          </button>
          <button
            type="button"
            className="classic-tb-btn"
            onClick={() => void setWindowMode("mini")}
            title="切换到迷你悬浮窗（Alt+Shift+3）"
            aria-label="切换到迷你窗"
          >
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <rect x="2" y="3" width="12" height="10" rx="2.2" stroke="currentColor" strokeWidth="1.4" />
              <rect x="7.6" y="7.4" width="4.4" height="3.6" rx="0.9" fill="currentColor" />
            </svg>
          </button>
          <button
            type="button"
            className="classic-tb-btn"
            onClick={() => void setWindowMode("efficiency")}
            title="展开为完整效率模式（Alt+Shift+1）"
            aria-label="展开为效率模式"
          >
            <Maximize2 size={13} />
          </button>
        </div>
        {inTauri && !isMac && (
          <>
            <span className="classic-tb-sep" aria-hidden="true" />
            <div className="classic-tb-group">
              <button type="button" className="classic-tb-btn" onClick={() => window.crossagents?.command?.("minimize")} title="最小化">
                <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1 5h8" stroke="currentColor" strokeWidth="1.2" /></svg>
              </button>
              <button type="button" className="classic-tb-btn close" onClick={() => window.crossagents?.command?.("close")} title="收起到系统托盘">
                <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1.5 1.5l7 7M8.5 1.5l-7 7" stroke="currentColor" strokeWidth="1.2" /></svg>
              </button>
            </div>
          </>
        )}
      </header>

      {/* Profile / live status header (echoing QQ's top identity strip) */}
      {!openSessionId && (
        <div className="classic-hero">
          <div className="classic-profile">
            <div className="classic-profile-avatar">
              <ChatAvatar image={profile.avatar} />
              <span className={`classic-status-dot ${status === "connected" ? "ok" : "warn"}`} />
            </div>
            <div className="classic-profile-meta">
              <div className="classic-profile-row">
                <span className="classic-profile-name">{profile.name.trim() || "CrossAgents"}</span>
                {hasMultipleNodes && (
                  <span className="classic-nodes-pill" title={`本机 + ${onlineRemoteCount}/${remoteNodes.length} 个远程节点在线`}>
                    {1 + onlineRemoteCount}/{machines.length} 节点
                  </span>
                )}
              </div>
              <div className="classic-profile-sub">
                {runningAgents.length > 0 ? `${runningAgents.length} 运行中` : "全部空闲"}
                {todos.length > 0 ? ` · ${todos.length} 项等你` : ""}
                {unreadCount > 0 ? ` · ${unreadCount} 未读` : ""}
              </div>
            </div>
            <button
              type="button"
              className={`classic-pill-toggle ${wm.autoFade ? "on" : ""}`}
              onClick={() => setWindowAutoFade(!wm.autoFade)}
              title="失焦自动微透明，鼠标移入恢复清晰"
            >
              {wm.autoFade ? "失焦淡化" : "常亮"}
            </button>
          </div>

          <div className="classic-search-row">
            <label className="classic-search">
              <Search size={13} />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="搜索会话、代号、节点或项目"
                aria-label="搜索会话"
              />
              {query && (
                <button type="button" className="icon-btn" onClick={() => setQuery("")} aria-label="清空">
                  <Icon name="close" size={11} />
                </button>
              )}
            </label>
            <button
              type="button"
              className={`classic-add-btn ${quickNew ? "on" : ""}`}
              onClick={() => {
                setQuickNew((v) => !v);
                if (!quickNew && selectedNode !== "all" && selectedNode !== CENTRE_MACHINE) {
                  setQuickNode(selectedNode);
                }
              }}
              title="快速发起一个新会话"
              aria-label="新建会话"
            >
              <Plus size={15} />
            </button>
          </div>

          {/* Node selector pills whenever multiple execution machines exist */}
          {hasMultipleNodes && (
            <div className="classic-node-bar" role="tablist" aria-label="按节点筛选">
              <button
                type="button"
                role="tab"
                aria-selected={selectedNode === "all"}
                className={`classic-node-pill ${selectedNode === "all" ? "active" : ""}`}
                onClick={() => {
                  setSelectedNode("all");
                  setSelectedProject(null);
                }}
              >
                <span>全部</span>
                <span className="classic-node-count">{agents.length}</span>
              </button>
              {machines.map((m) => (
                <button
                  key={m.key}
                  type="button"
                  role="tab"
                  aria-selected={selectedNode === m.key}
                  className={`classic-node-pill ${selectedNode === m.key ? "active" : ""} ${m.remote && !m.online ? "offline" : ""}`}
                  onClick={() => {
                    setSelectedNode(m.key);
                    setSelectedProject(null);
                  }}
                  title={`${m.label} · ${m.online ? "在线" : "离线"} · ${m.count} 个会话`}
                >
                  <OsIcon os={m.os} size={11} />
                  <span className="classic-node-label">{m.label}</span>
                  {m.remote && <span className={`classic-node-dot ${m.online ? "online" : "offline"}`} />}
                  {m.busy > 0 && <span className="classic-node-busy">{m.busy}</span>}
                </button>
              ))}
            </div>
          )}

          {quickNew && (
            <div className="classic-quick-new">
              {hasMultipleNodes && (
                <div className="classic-quick-node-row">
                  <span className="muted tiny">执行节点</span>
                  <select
                    className="classic-quick-select"
                    value={quickNode}
                    onChange={(e) => setQuickNode(e.target.value)}
                    aria-label="选择执行节点"
                  >
                    <option value="">本机</option>
                    {remoteNodes.map((n) => (
                      <option key={n.id} value={n.id} disabled={!n.online}>
                        {n.name}
                        {n.online ? "" : "（离线）"}
                      </option>
                    ))}
                  </select>
                </div>
              )}
              <textarea
                rows={2}
                value={quickPrompt}
                onChange={(e) => setQuickPrompt(e.target.value)}
                placeholder="输入任务直接启动新会话（Enter 发送）"
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    void startQuickChat();
                  } else if (e.key === "Escape") {
                    setQuickNew(false);
                  }
                }}
              />
              {quickError && <div className="small danger">{quickError}</div>}
              <div className="row" style={{ justifyContent: "flex-end", gap: 6 }}>
                <button type="button" className="ghost small" onClick={() => setQuickNew(false)}>
                  取消
                </button>
                <button type="button" className="primary small" disabled={quickBusy || !quickPrompt.trim()} onClick={() => void startQuickChat()}>
                  {quickBusy ? "启动中…" : "发送"}
                </button>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Main body: either QQ-style rail + list, or in-pane compact session view */}
      {openSessionId ? (
        <div className="classic-chat-wrap">
          <div className="classic-chat-bar">
            <button type="button" className="classic-back-btn" onClick={() => setOpenSessionId(null)} title="返回会话列表">
              <ArrowLeft size={14} />
              <span>返回</span>
            </button>
            <div className="classic-chat-title">
              <div className="classic-chat-name">
                {activeSession ? <ScopedNameLabel name={activeSession.name} /> : openSessionId.slice(0, 8)}
              </div>
              {activeSession && (
                <div className="classic-chat-meta">
                  {activeMachine?.remote && (
                    <span className={`classic-row-node ${activeMachine.online ? "" : "offline"}`}>
                      <OsIcon os={activeMachine.os} size={10} />
                      <span>{activeMachine.label}</span>
                    </span>
                  )}
                  {!isChat(activeSession) && <span className="classic-chat-proj">{wsName(homeOf(activeSession))}</span>}
                </div>
              )}
            </div>
            <button
              type="button"
              className="classic-back-btn"
              onClick={() => {
                navigate("s", openSessionId);
                void setWindowMode("efficiency");
              }}
              title="在完整效率窗口打开此会话"
            >
              <Maximize2 size={12} />
              <span>大窗</span>
            </button>
          </div>
          <div className="classic-chat-body">
            <SessionView key={openSessionId} name={openSessionId} />
          </div>
        </div>
      ) : (
        <div className="classic-body">
          <aside className="classic-rail" aria-label="视图筛选">
            <button
              type="button"
              className={`classic-rail-btn ${tab === "all" ? "active" : ""}`}
              onClick={() => {
                setTab("all");
                setSelectedProject(null);
              }}
              title="全部会话"
            >
              <MessageSquare size={17} />
              {unreadCount > 0 && <span className="classic-rail-badge">{unreadCount > 99 ? "99+" : unreadCount}</span>}
            </button>
            <button
              type="button"
              className={`classic-rail-btn ${tab === "needs" ? "active" : ""}`}
              onClick={() => setTab("needs")}
              title={`需要你处理（${todos.length}）`}
            >
              <Bell size={17} />
              {todos.length > 0 && <span className="classic-rail-badge warn">{todos.length}</span>}
            </button>
            <button
              type="button"
              className={`classic-rail-btn ${tab === "running" ? "active" : ""}`}
              onClick={() => setTab("running")}
              title={`正在运行（${runningAgents.length}）`}
            >
              <Zap size={17} />
              {runningAgents.length > 0 && <span className="classic-rail-badge live">{runningAgents.length}</span>}
            </button>
            <button
              type="button"
              className={`classic-rail-btn ${tab === "projects" ? "active" : ""}`}
              onClick={() => {
                setTab("projects");
                if (!selectedProject && projects.length > 0) setSelectedProject(projects[0].key);
              }}
              title="按项目筛选"
            >
              <FolderKanban size={17} />
            </button>

            <div className="spacer" />

            {unreadCount > 0 && (
              <button
                type="button"
                className="classic-rail-btn"
                onClick={markAllSeen}
                title="全部标为已读"
                aria-label="全部标为已读"
              >
                <CheckCheck size={16} />
              </button>
            )}
            {inTauri && (
              <button
                type="button"
                className="classic-rail-btn"
                onClick={() => void dockWindowNow(wm.dockedEdge === "left" ? "left" : "right")}
                title="立刻贴边收起（鼠标滑到屏幕边缘自动弹出）"
                aria-label="立刻贴边收起"
              >
                <PanelRightClose size={16} />
              </button>
            )}
            <button
              type="button"
              className="classic-rail-btn"
              onClick={() => {
                navigate("settings");
                void setWindowMode("efficiency");
              }}
              title="打开完整设置"
              aria-label="设置"
            >
              <Settings size={16} />
            </button>
          </aside>

          <div className="classic-main-col">
            <div className="classic-list">
              {tab === "projects" && (
                <div className="classic-proj-chips">
                  <button
                    type="button"
                    className={`classic-proj-chip ${!selectedProject ? "active" : ""}`}
                    onClick={() => setSelectedProject(null)}
                  >
                    全部项目
                  </button>
                  {projects.map((p) => {
                    const m = machineMap.get(p.machineKey);
                    return (
                      <button
                        key={p.key}
                        type="button"
                        className={`classic-proj-chip ${selectedProject === p.key ? "active" : ""}`}
                        onClick={() => setSelectedProject(p.key)}
                        title={m?.remote ? `${m.label} · ${p.name}` : p.name}
                      >
                        {m?.remote && <OsIcon os={m.os} size={10} />}
                        <span>{p.name}</span>
                        {p.busy > 0 && <i className="working-dot" />}
                      </button>
                    );
                  })}
                </div>
              )}

              {tab !== "running" && todos.length > 0 && !query && (
                <div className="classic-todos">
                  <button
                    type="button"
                    className="classic-section-bar warn"
                    onClick={() => setTodosFolded((v) => !v)}
                    aria-expanded={!todosFolded}
                  >
                    {todosFolded ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
                    <span>等你处理（{todos.length}）</span>
                  </button>
                  {!todosFolded &&
                    todos.slice(0, 4).map((item) => {
                      const target = agents.find((a) => a.id === item.agent);
                      const tm = target ? machineMap.get(agentMachine(target)) : undefined;
                      return (
                        <button
                          key={item.kind + item.id}
                          type="button"
                          className="classic-todo-row"
                          onClick={() => {
                            if (target) {
                              setOpenSessionId(target.id);
                              navigate("s", target.id);
                            } else if (item.href) {
                              window.location.hash = item.href.replace(/^#/, "");
                              void setWindowMode("efficiency");
                            }
                          }}
                        >
                          <span className="classic-todo-tag">等你</span>
                          <span className="classic-todo-text">
                            <strong>{target ? displayName(target) : "待办"}</strong>
                            {tm?.remote && <span className="classic-todo-node">@{tm.label}</span>}
                            {" · "}
                            {item.summary}
                          </span>
                        </button>
                      );
                    })}
                </div>
              )}

              {filtered.length === 0 ? (
                <div className="classic-empty">
                  {tab === "needs"
                    ? "当前没有等你处理的事项"
                    : tab === "running"
                      ? "当前没有正在运行的会话"
                      : "没有匹配的会话"}
                </div>
              ) : groupedByNode ? (
                groupedByNode.map(({ machine, agents: nodeAgents }) => {
                  const folded = !query.trim() && foldedNodes.has(machine.key);
                  return (
                    <section key={machine.key} className={`classic-node-section ${machine.remote && !machine.online ? "offline" : ""}`}>
                      <button
                        type="button"
                        className="classic-node-head"
                        onClick={() => toggleNodeFold(machine.key)}
                        aria-expanded={!folded}
                      >
                        {folded ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
                        <OsIcon os={machine.os} size={12} />
                        <span className="classic-node-head-name">{machine.label}</span>
                        {machine.remote && (
                          <span className={`classic-node-state ${machine.online ? "online" : "offline"}`}>
                            {machine.online ? "在线" : "离线"}
                          </span>
                        )}
                        <span className="spacer" />
                        {machine.busy > 0 && <span className="classic-node-head-busy">{machine.busy} 运行</span>}
                        <span className="classic-node-head-count">{nodeAgents.length}</span>
                      </button>
                      {!folded &&
                        (nodeAgents.length === 0 ? (
                          <div className="classic-node-empty">暂无活跃会话</div>
                        ) : (
                          nodeAgents.map((agent) => (
                            <ClassicRow
                              key={agent.id}
                              agent={agent}
                              machine={machine}
                              showNodeBadge={false}
                              needCount={todos.filter((t) => t.agent === agent.id).length}
                              onOpen={() => {
                                setOpenSessionId(agent.id);
                                navigate("s", agent.id);
                              }}
                            />
                          ))
                        ))}
                    </section>
                  );
                })
              ) : (
                filtered.map((agent) => (
                  <ClassicRow
                    key={agent.id}
                    agent={agent}
                    machine={machineMap.get(agentMachine(agent))}
                    showNodeBadge={hasMultipleNodes}
                    needCount={todos.filter((t) => t.agent === agent.id).length}
                    onOpen={() => {
                      setOpenSessionId(agent.id);
                      navigate("s", agent.id);
                    }}
                  />
                ))
              )}
            </div>

            {/* Classic QQ-style bottom footer bar */}
            <footer className="classic-footer">
              <span className="classic-footer-hint">
                {wm.dockedEdge !== "none"
                  ? `已贴${wm.dockedEdge === "left" ? "左" : wm.dockedEdge === "right" ? "右" : "上"}边 · 移出自动收起`
                  : wm.edgeDock
                    ? "拖至屏幕左/右/上边缘可贴边收起"
                    : "贴边抽屉已关闭"}
              </span>
              <span className="spacer" />
              <span className="classic-footer-stat">{filtered.length} 会话</span>
            </footer>
          </div>
        </div>
      )}
    </div>
  );
}

function ClassicRow({
  agent,
  machine,
  showNodeBadge,
  needCount,
  onOpen,
}: {
  agent: Agent;
  machine?: MachineSummary;
  showNodeBadge: boolean;
  needCount: number;
  onOpen: () => void;
}) {
  const delivery = useDelivery(agent.id);
  const doing = useActivity(agent.id);
  const unread = useUnread()[agent.id];
  const turn = useRowLive(agent);

  const running = delivery ? delivery.state === "turn" : agent.status === "running";
  const error = agent.status === "error";
  const offline = Boolean(machine?.remote && !machine.online);
  const newest = turn?.items.at(-1);
  const step = running && newest?.type === "tool_call" ? stepLine(newest) : "";
  const sub = error
    ? lastError(agent.id) || "运行出错"
    : running
      ? step || summaryText(doing) || summaryText(agent.topic) || "正在处理…"
      : offline
        ? summaryText(agent.topic) || "节点离线"
        : summaryText(agent.topic) || (isChat(agent) ? "独立对话" : wsName(homeOf(agent)));

  return (
    <button
      type="button"
      className={`classic-row ${running ? "running" : ""} ${needCount > 0 ? "needs" : ""} ${offline ? "offline" : ""}`}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "copy";
        encodeSessionDrag(e.dataTransfer, agent);
        e.dataTransfer.setData("text/plain", agent.name);
      }}
      onClick={onOpen}
    >
      <span className={`classic-row-face ${running ? "running" : ""}`}>
        <Avatar agent={agent} size={36} />
        {needCount > 0 && <span className="face-badge warn">{needCount}</span>}
        {!needCount && error && <span className="face-badge danger">!</span>}
        {!needCount && !error && unread?.kind === "done" && <span className="face-badge fresh" />}
      </span>
      <span className="classic-row-main">
        <span className="classic-row-top">
          <span className="classic-row-name">
            <ScopedNameLabel name={agent.name} />
          </span>
          <ModelMark plugin={agent.plugin} model={agent.model} />
          <span className="spacer" />
          {running ? (
            <RowElapsed agentId={agent.id} since={turn?.since} at={turn?.at} items={turn?.items ?? []} />
          ) : (
            <span className="classic-row-time">{shortSince(agent.created_at, Date.now())}</span>
          )}
        </span>
        <span className="classic-row-sub">
          <span className="classic-row-sub-text">
            {running && <span className="step-mark">⎿ </span>}
            {sub}
          </span>
          <span className="classic-row-tags">
            {showNodeBadge && machine?.remote && (
              <span className={`classic-row-node ${machine.online ? "" : "offline"}`} title={`节点：${machine.label}`}>
                <OsIcon os={machine.os} size={10} />
                <span>{machine.label}</span>
              </span>
            )}
            {!isChat(agent) && <span className="classic-row-proj">{wsName(homeOf(agent))}</span>}
          </span>
        </span>
      </span>
    </button>
  );
}

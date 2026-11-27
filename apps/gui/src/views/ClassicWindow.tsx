// QQ-style Classic Narrow Panel mode: a compact vertical sidebar window (~350px wide) that
// stays neatly on the side of your screen while gaming or multitasking, with quick search,
// narrow left filter rail (All / Needs You / Running / Projects), high-density live session
// list, and in-pane quick conversation & approval view.

import { useMemo, useState, type MouseEvent as ReactMouseEvent } from "react";
import type { Agent } from "@bindings/Agent";
import { ArrowLeft, Bell, Eye, FolderKanban, Maximize2, MessageSquare, Pin, Plus, Search, Zap } from "lucide-react";
import { deliveryOf, lastError, useActivity, useDelivery, useUnread } from "../activity";
import { navigate, useRunTree } from "../App";
import { api, errorText } from "../api";
import { ChatAvatar } from "../ChatAvatar";
import { Avatar, BrandMark, Icon, ModelMark, displayName, useAgents } from "../components";
import { useConnectionStatus, usePlugins } from "../hub";
import { useChatProfile } from "../lib/chatProfile";
import { newChatRequest } from "../lib/sessionChoice";
import { shortSince, summaryText } from "../lib/sidebarTree";
import { todoItems } from "../lib/todos";
import { stepLine } from "../lib/workGroups";
import { RowElapsed, useRowLive } from "../liveTurn";
import { sendMessage } from "../outbox";
import { useQuestions } from "../questions";
import { ScopedNameLabel, encodeSessionDrag } from "../sessionRef";
import { inTauri } from "../titlebar";
import { isMac } from "../lib/shortcuts";
import {
  cycleWindowOpacity,
  setWindowAlwaysOnTop,
  setWindowAutoFade,
  setWindowMode,
  useWindowMode,
} from "../windowMode";
import { homeOf, isChat, wsKey, wsName } from "../workspace";
import { SessionView } from "./Session";

type ClassicTab = "all" | "needs" | "running" | "projects";

export function ClassicWindow({ route }: { route: string[] }) {
  const wm = useWindowMode();
  const agentsData = useAgents();
  const tree = useRunTree();
  const questions = useQuestions();
  const plugins = usePlugins();
  const unread = useUnread();
  const status = useConnectionStatus();
  const profile = useChatProfile();

  const [tab, setTab] = useState<ClassicTab>("all");
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [openSessionId, setOpenSessionId] = useState<string | null>(() => (route[0] === "s" && route[1] ? route[1] : null));
  const [quickNew, setQuickNew] = useState(false);
  const [quickPrompt, setQuickPrompt] = useState("");
  const [quickBusy, setQuickBusy] = useState(false);
  const [quickError, setQuickError] = useState<string>();

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

  const runningAgents = useMemo(() => agents.filter(isRunning), [agents, unread]);
  const unreadCount = useMemo(() => Object.values(unread).filter((u) => u.kind === "done" || u.kind === "error").length, [unread]);

  const projects = useMemo(() => {
    const map = new Map<string, { key: string; name: string; path: string; count: number; busy: number }>();
    for (const a of agents) {
      const chat = isChat(a);
      const path = chat ? "" : homeOf(a);
      const key = chat ? "__chats" : wsKey(path);
      const name = chat ? "独立对话" : wsName(path);
      const cur = map.get(key) ?? { key, name, path, count: 0, busy: 0 };
      cur.count += 1;
      if (isRunning(a)) cur.busy += 1;
      map.set(key, cur);
    }
    return [...map.values()].sort((a, b) => b.busy - a.busy || b.count - a.count || a.name.localeCompare(b.name));
  }, [agents]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const todoIds = new Set(todos.map((t) => t.agent));
    let list = agents;
    if (tab === "needs") list = list.filter((a) => todoIds.has(a.id) || a.status === "error");
    else if (tab === "running") list = list.filter(isRunning);
    else if (tab === "projects" && selectedProject) {
      list = list.filter((a) => (isChat(a) ? "__chats" : wsKey(homeOf(a))) === selectedProject);
    }
    if (q) {
      list = list.filter((a) =>
        [a.name, a.topic, a.model, isChat(a) ? "独立对话" : wsName(homeOf(a))]
          .filter(Boolean)
          .some((s) => s!.toLowerCase().includes(q)),
      );
    }
    return list.slice().sort((a, b) => {
      const aNeed = todoIds.has(a.id) || a.status === "error" ? 1 : 0;
      const bNeed = todoIds.has(b.id) || b.status === "error" ? 1 : 0;
      if (aNeed !== bNeed) return bNeed - aNeed;
      const aRun = isRunning(a) ? 1 : 0;
      const bRun = isRunning(b) ? 1 : 0;
      if (aRun !== bRun) return bRun - aRun;
      const aUnread = unread[a.id] ? 1 : 0;
      const bUnread = unread[b.id] ? 1 : 0;
      if (aUnread !== bUnread) return bUnread - aUnread;
      return b.created_at.localeCompare(a.created_at);
    });
  }, [agents, todos, tab, selectedProject, query, unread]);

  const onDragTitle = (event: ReactMouseEvent<HTMLElement>) => {
    if (!inTauri || event.button !== 0) return;
    if ((event.target as Element).closest("button, a, input")) return;
    window.crossagents?.command?.("drag");
  };

  async function startQuickChat() {
    const text = quickPrompt.trim();
    if (!text || quickBusy) return;
    const offered = (plugins.data ?? []).filter((p) => p.models.length > 0);
    if (!offered.length) {
      setQuickError("没有可用的模型插件");
      return;
    }
    setQuickBusy(true);
    setQuickError(undefined);
    try {
      const proj = selectedProject && selectedProject !== "__chats"
        ? projects.find((p) => p.key === selectedProject)?.path ?? ""
        : projects.find((p) => p.key !== "__chats")?.path ?? "";
      const body = newChatRequest({ plugin: offered[0].name, model: offered[0].models[0] }, text, proj);
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

  return (
    <div className="classic-shell">
      {/* Top draggable QQ-style classic bar */}
      <header className="classic-titlebar" onMouseDown={onDragTitle}>
        <div className="classic-brand">
          <BrandMark size={16} />
          <span>CrossAgents</span>
        </div>
        <div className="spacer" />
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
            <rect x="2.2" y="3.2" width="11.6" height="9.6" rx="2.2" stroke="currentColor" strokeWidth="1.4" />
            <rect x="7.5" y="7.5" width="4.4" height="3.4" rx="0.9" fill="currentColor" />
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
        {inTauri && !isMac && (
          <>
            <button type="button" className="classic-tb-btn" onClick={() => window.crossagents?.command?.("minimize")} title="最小化">
              <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1 5h8" stroke="currentColor" strokeWidth="1.2" /></svg>
            </button>
            <button type="button" className="classic-tb-btn close" onClick={() => window.crossagents?.command?.("close")} title="收起到系统托盘">
              <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1.5 1.5l7 7M8.5 1.5l-7 7" stroke="currentColor" strokeWidth="1.2" /></svg>
            </button>
          </>
        )}
      </header>

      {/* Profile / live status header (echoing QQ's top identity strip) */}
      {!openSessionId && (
        <>
          <div className="classic-profile">
            <div className="classic-profile-avatar">
              <ChatAvatar image={profile.avatar} />
              <span className={`classic-status-dot ${status === "connected" ? "ok" : "warn"}`} />
            </div>
            <div className="classic-profile-meta">
              <div className="classic-profile-name">{profile.name.trim() || "CrossAgents"}</div>
              <div className="classic-profile-sub">
                {runningAgents.length > 0 ? `${runningAgents.length} 运行中` : "全部空闲"}
                {todos.length > 0 ? ` · ${todos.length} 项等你处理` : ""}
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
                placeholder="搜索会话、代号或项目"
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
              onClick={() => setQuickNew((v) => !v)}
              title="快速发起一个新会话"
              aria-label="新建会话"
            >
              <Plus size={15} />
            </button>
          </div>

          {quickNew && (
            <div className="classic-quick-new">
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
        </>
      )}

      {/* Main body: either QQ-style rail + list, or in-pane compact session view */}
      {openSessionId ? (
        <div className="classic-chat-wrap">
          <div className="classic-chat-bar">
            <button type="button" className="ghost small" onClick={() => setOpenSessionId(null)}>
              <ArrowLeft size={14} />
              <span>会话列表</span>
            </button>
            <div className="classic-chat-title">
              {activeSession ? <ScopedNameLabel name={activeSession.name} /> : openSessionId.slice(0, 8)}
            </div>
            <button
              type="button"
              className="ghost small"
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
          </aside>

          <div className="classic-list">
            {tab === "projects" && (
              <div className="classic-proj-chips">
                <button
                  type="button"
                  className={`classic-proj-chip ${!selectedProject ? "active" : ""}`}
                  onClick={() => setSelectedProject(null)}
                >
                  全部
                </button>
                {projects.map((p) => (
                  <button
                    key={p.key}
                    type="button"
                    className={`classic-proj-chip ${selectedProject === p.key ? "active" : ""}`}
                    onClick={() => setSelectedProject(p.key)}
                  >
                    {p.name}
                    {p.busy > 0 && <i className="working-dot" />}
                  </button>
                ))}
              </div>
            )}

            {tab !== "running" && todos.length > 0 && !query && (
              <div className="classic-todos">
                {todos.slice(0, 3).map((item) => {
                  const target = agents.find((a) => a.id === item.agent);
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
                        <strong>{target ? displayName(target) : "待办"}</strong> · {item.summary}
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
            ) : (
              filtered.map((agent) => (
                <ClassicRow
                  key={agent.id}
                  agent={agent}
                  needCount={todos.filter((t) => t.agent === agent.id).length}
                  onOpen={() => {
                    setOpenSessionId(agent.id);
                    navigate("s", agent.id);
                  }}
                />
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function ClassicRow({ agent, needCount, onOpen }: { agent: Agent; needCount: number; onOpen: () => void }) {
  const delivery = useDelivery(agent.id);
  const doing = useActivity(agent.id);
  const unread = useUnread()[agent.id];
  const turn = useRowLive(agent);

  const running = delivery ? delivery.state === "turn" : agent.status === "running";
  const error = agent.status === "error";
  const newest = turn?.items.at(-1);
  const step = running && newest?.type === "tool_call" ? stepLine(newest) : "";
  const sub = error
    ? lastError(agent.id) || "运行出错"
    : running
      ? step || summaryText(doing) || summaryText(agent.topic) || "正在处理…"
      : summaryText(agent.topic) || (isChat(agent) ? "独立对话" : wsName(homeOf(agent)));

  return (
    <button
      type="button"
      className={`classic-row ${running ? "running" : ""} ${needCount > 0 ? "needs" : ""}`}
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
          {!isChat(agent) && <span className="classic-row-proj">{wsName(homeOf(agent))}</span>}
        </span>
      </span>
    </button>
  );
}

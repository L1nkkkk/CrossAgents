// The sidebar: live runs with their team hanging off a rail, standalone sessions, then
// history. Agents appear as people (avatar, display name, title); runs as ◯ nodes.

import { createContext, useContext, useEffect, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { useActivity, useUnread } from "./activity";
import { api } from "./api";
import { ACTIVE_RUN, runAgents } from "./App";
import { Avatar, BrandMark, Icon, Mark, ago, agentTitle, displayName, runTone, sessionHref, taskTone } from "./components";
import { useConnectionStatus } from "./hub";
import { level, tightest, windowLabel } from "./lib/quota";
import { openBeside } from "./panes";
import { useQuota } from "./views/Quota";
import { liveRuns, selectWorkspace, useSelectedWorkspace, useWorkspaces, wsHref, type Workspace } from "./workspace";

export function Sidebar({ route }: { route: string[] }) {
  const { list: workspaces, tree, agents } = useWorkspaces();
  const scope = useSelectedWorkspace();
  const current = workspaces.find((w) => w.key === scope);
  const [query, setQuery] = useState("");
  const [section, selected] = route;
  const everyone = agents.data ?? [];
  const q = query.trim().toLowerCase();
  const matchesAgent = (a: Agent) => !q || a.name.toLowerCase().includes(q) || displayName(a).toLowerCase().includes(q);
  const matchesRun = (d: RunDetail) => !q || d.run.title.toLowerCase().includes(q) || runAgents(d, everyone).some(matchesAgent);

  const runs = (current ? current.runs : (tree.data ?? [])).filter(matchesRun);
  const pool = current ? current.agents : everyone;
  const inRuns = new Set((tree.data ?? []).flatMap((d) => runAgents(d, everyone).map((a) => a.id)));
  const standalone = pool.filter((a) => !inRuns.has(a.id) && matchesAgent(a));
  const active = runs.filter((d) => ACTIVE_RUN.includes(d.run.status));
  const ended = runs.filter((d) => !ACTIVE_RUN.includes(d.run.status));
  const isRun = (d: RunDetail) => section === "runs" && selected === d.run.id;
  const isSession = (a: Agent) => section === "s" && selected === a.name;
  const pendingPlans = new Set((tree.data ?? []).filter((d) => d.run.status === "awaiting_approval").map((d) => d.run.planner));

  return (
    <PendingPlans.Provider value={pendingPlans}>
    <nav className="sidebar">
      <div className="brand">
        <BrandMark size={20} />
        <WorkspaceSwitcher workspaces={workspaces} current={current} onPage={section === "w"} />
      </div>

      <label className="search">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
          <circle cx="11" cy="11" r="7" />
          <path d="m20 20-3.5-3.5" />
        </svg>
        <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search" aria-label="搜索 run、会话和队友" />
        {query && (
          <button className="icon-btn" onClick={() => setQuery("")} aria-label="清除">
            <Icon name="close" size={12} />
          </button>
        )}
      </label>

      <a className={`new-run ${!section ? "active" : ""}`} href="#/">
        <Icon name="plus" size={14} />
        <span>新建 run</span>
        <kbd>Ctrl N</kbd>
      </a>

      <div className="scroll">
        {active.length > 0 && <Label text="进行中" count={active.length} />}
        {active.map((d) => (
          <RunGroup key={d.run.id} detail={d} agents={everyone} selected={isRun(d)} isSession={isSession} />
        ))}

        {standalone.length > 0 && <Label text="Sessions" count={standalone.length} />}
        <Capped items={standalone} keep={isSession} searching={!!q}>
          {(agent) => <SessionLink key={agent.id} agent={agent} active={isSession(agent)} />}
        </Capped>

        {ended.length > 0 && <Label text="最近" count={ended.length} />}
        <Capped items={ended} keep={isRun} searching={!!q}>
          {(d, i, shown) => (
            <div key={d.run.id}>
              {(i === 0 || dayOf(shown[i - 1].run.updated_at) !== dayOf(d.run.updated_at)) && (
                <div className="day-label">{dayOf(d.run.updated_at)}</div>
              )}
              <HistoryItem detail={d} agents={everyone} selected={isRun(d)} isSession={isSession} />
            </div>
          )}
        </Capped>

        {q && !runs.length && !standalone.length && <p className="sidebar-empty">没有匹配「{query}」的 run、会话或队友。</p>}
        {!q && tree.data && agents.data && !runs.length && !pool.length && (
          <p className="sidebar-empty">
            {current ? `${current.name} 里还没有 run 和会话。` : "还没有 run 和会话。"}先到「团队」添加一个 Planner 和一个 Worker。
          </p>
        )}
      </div>

      <div className="foot">
        <div className="foot-nav">
          <a className={section === "agents" ? "active" : ""} href="#/agents">
            <Icon name="cpu" size={15} />
            团队
          </a>
          <a className={section === "usage" ? "active" : ""} href="#/usage">
            <Icon name="chart" size={15} />
            用量
          </a>
        </div>
        <QuotaMini />
        <Connection />
      </div>
    </nav>
    </PendingPlans.Provider>
  );
}

function Label({ text, count }: { text: string; count: number }) {
  return (
    <div className="nav-label">
      <span>{text}</span>
      <span className="count">{count}</span>
    </div>
  );
}

/** Rows shown before "更多": long lists stay short until asked. */
const CAP = 5;

/** The first `CAP` items (plus any `keep` wants visible, e.g. the selected one), then "更多 (n)". */
function Capped<T>({
  items,
  keep,
  searching,
  children,
}: {
  items: T[];
  keep: (item: T) => boolean;
  searching: boolean;
  children: (item: T, index: number, shown: T[]) => ReactNode;
}) {
  const [all, setAll] = useState(false);
  if (!items.length) return null;
  const shown = all || searching ? items : items.filter((item, i) => i < CAP || keep(item));
  const hidden = items.length - shown.length;
  return (
    <div className="list">
      {shown.map((item, i) => children(item, i, shown))}
      {hidden > 0 && (
        <button className="more" onClick={() => setAll(true)}>
          更多 ({hidden})
        </button>
      )}
      {all && items.length > CAP && !searching && (
        <button className="more" onClick={() => setAll(false)}>
          收起
        </button>
      )}
    </div>
  );
}

/** A live run: its node, title and task segments, and its team on a rail. */
function RunGroup({
  detail,
  agents,
  selected,
  isSession,
}: {
  detail: RunDetail;
  agents: Agent[];
  selected: boolean;
  isSession: (agent: Agent) => boolean;
}) {
  const { run, tasks } = detail;
  const members = runAgents(detail, agents);
  const merged = tasks.filter((t) => t.status === "merged").length;

  return (
    <div className="run-group live open">
      <a className={`run-row ${selected ? "active" : ""}`} href={`#/runs/${run.id}`} title={run.request}>
        <Mark shape="run" tone={runTone(run.status)} size={12} title={run.status} />
        <span className="run-main">
          <span className="name">{run.title}</span>
          {tasks.length > 0 && (
            <span className="segments" aria-label={`${merged}/${tasks.length} merged`}>
              {tasks.map((t) => (
                <span key={t.id} className={`seg ${taskTone(t.status).color} ${taskTone(t.status).live ? "live" : ""}`} title={`${t.key} · ${t.status}`} />
              ))}
            </span>
          )}
        </span>
        {run.status === "awaiting_approval" ? (
          <span className="flag">待审批</span>
        ) : tasks.length > 0 ? (
          <span className="meta">
            {merged}/{tasks.length}
          </span>
        ) : null}
      </a>
      <Team members={members} isSession={isSession} />
    </div>
  );
}

function Team({ members, isSession }: { members: Agent[]; isSession: (agent: Agent) => boolean }) {
  if (!members.length) return null;
  return (
    <div className="rail">
      {members.map((agent) => (
        <div className="rail-item" key={agent.id}>
          <SessionLink agent={agent} active={isSession(agent)} />
        </div>
      ))}
    </div>
  );
}

/** Planners whose plan waits for approval; provided once by the sidebar for every row. */
const PendingPlans = createContext<Set<string>>(new Set());

/**
 * A teammate: avatar with presence, display name, and a status tail in three tiers —
 * attention (出错 / 待审批 as a tinted pill), activity (✻ working, with the latest step on a
 * second line), passive (完成 when you have not looked yet, else the title). Ctrl/⌘-click or
 * middle-click opens it beside the current conversation.
 */
function SessionLink({ agent, active }: { agent: Agent; active: boolean }) {
  const doing = useActivity(agent.id);
  const unread = useUnread()[agent.id];
  const planWaiting = useContext(PendingPlans).has(agent.id);
  const running = agent.status === "running";
  const beside = (e: ReactMouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    openBeside(agent.name);
  };

  let tail: ReactNode;
  if (agent.status === "error" || unread?.kind === "error") tail = <span className="pill danger">出错</span>;
  else if (planWaiting) tail = <span className="pill warn">待审批</span>;
  else if (running)
    tail = (
      <span className="working-tag">
        <span className="spark">✻</span> working
      </span>
    );
  else if (unread) tail = <span className="tail-done">完成</span>;
  else tail = <span className="meta">{agent.profile?.title || agent.model || agent.plugin}</span>;

  return (
    <div className="nav-row">
      <a
        className={`nav-item ${active ? "active" : ""} ${agent.status === "stopped" ? "dim" : ""} ${running && doing ? "two" : ""}`}
        href={sessionHref(agent.name)}
        title={`${displayName(agent)} · ${agentTitle(agent)} · ${agent.plugin}${agent.model ? " · " + agent.model : ""} · ${agent.status}\nCtrl+单击或中键：在旁边打开`}
        onClick={(e) => (e.ctrlKey || e.metaKey) && beside(e)}
        onMouseDown={(e) => e.button === 1 && e.preventDefault()}
        onAuxClick={(e) => e.button === 1 && beside(e)}
      >
        <Avatar agent={agent} size={20} status />
        <span className="nav-main">
          <span className="name-line">
            <span className="name">{displayName(agent)}</span>
            {tail}
          </span>
          {running && doing && <span className="activity">{doing}</span>}
        </span>
      </a>
      {!active && (
        <button className="beside-btn" onClick={beside} title="在旁边打开" aria-label={`在旁边打开 ${displayName(agent)}`}>
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="M12 4v16" />
          </svg>
        </button>
      )}
    </div>
  );
}

/** "今天", "昨天", else the date. */
function dayOf(iso: string): string {
  const d = new Date(iso);
  const days = Math.round((new Date(new Date().toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 86_400_000);
  return days <= 0 ? "今天" : days === 1 ? "昨天" : d.toLocaleDateString();
}

const OUTCOME: Record<string, string> = { completed: "完成", failed: "失败", cancelled: "已取消" };

/** An ended run: title and time, then how it ended on a `⎿` result line. */
function HistoryItem({
  detail,
  agents,
  selected,
  isSession,
}: {
  detail: RunDetail;
  agents: Agent[];
  selected: boolean;
  isSession: (agent: Agent) => boolean;
}) {
  const { run, tasks } = detail;
  const members = runAgents(detail, agents);
  // A planner leads many runs; only its workers' sessions pull an ended run open.
  const open = selected || members.some((m) => m.id !== run.planner && isSession(m));
  const merged = tasks.filter((t) => t.status === "merged").length;
  const tone = runTone(run.status);
  const result = tasks.length > 0 ? `${merged}/${tasks.length} merged` : (OUTCOME[run.status] ?? run.status);

  return (
    <div className="history">
      <a className={`history-row ${selected ? "active" : ""}`} href={`#/runs/${run.id}`} title={run.request}>
        <Mark shape="run" tone={tone} size={11} title={run.status} />
        <span className="run-main">
          <span className="line">
            <span className="name">{run.title}</span>
            <span className="when">{ago(run.updated_at)}</span>
          </span>
          <span className={`result ${tone.color}`}>⎿ {result}</span>
        </span>
      </a>
      {open && <Team members={members} isSession={isSession} />}
    </div>
  );
}

/** The repository the sidebar shows, in place of the product name. */
function WorkspaceSwitcher({ workspaces, current, onPage }: { workspaces: Workspace[]; current?: Workspace; onPage: boolean }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !ref.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", close);
    };
  }, [open]);

  const pick = (key: string) => {
    selectWorkspace(key);
    setOpen(false);
    window.location.hash = key ? wsHref(key).slice(1) : "/";
  };
  const total = workspaces.reduce((n, w) => n + liveRuns(w), 0);

  return (
    <div className="ws" ref={ref}>
      <button className={`ws-button ${open || onPage ? "open" : ""}`} onClick={() => setOpen(!open)} title={current?.path ?? "全部工作区"} aria-expanded={open}>
        <span className="ws-name">{current?.name ?? "CrossAgents"}</span>
        <span className="ws-scope">{current ? "" : "全部工作区"}</span>
        <svg className="chev" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M7 10l5 5 5-5" />
        </svg>
      </button>
      {open && (
        <div className="ws-menu" role="menu">
          <button className={`ws-item ${!current ? "on" : ""}`} onClick={() => pick("")} role="menuitem">
            <span className="ws-item-main">
              <span className="ws-item-name">全部工作区</span>
              <span className="ws-item-sub">{workspaces.length} 个仓库</span>
            </span>
            {total > 0 && <span className="ws-live">{total}</span>}
          </button>
          <div className="ws-sep" />
          {workspaces.map((w) => (
            <button key={w.key} className={`ws-item ${current?.key === w.key ? "on" : ""}`} onClick={() => pick(w.key)} role="menuitem" title={w.path}>
              <span className="ws-item-main">
                <span className="ws-item-name">{w.name}</span>
                <span className="ws-item-path">{w.path}</span>
              </span>
              {liveRuns(w) > 0 && <span className="ws-live">{liveRuns(w)}</span>}
            </button>
          ))}
          {!workspaces.length && <div className="ws-empty">添加队友时填写的工作目录会出现在这里。</div>}
        </div>
      )}
    </div>
  );
}

/** The most used quota window of all accounts, e.g. "额度 8% · 30 天". */
function QuotaMini() {
  const quota = useQuota();
  const found = tightest((quota.data?.accounts ?? []).map((a) => ({ ...a, windows: a.status.rate_limits })));
  if (!found) return null;
  const pct = Math.round(found.window.used_percent);
  const name = found.account.account.label || found.account.account.id;
  return (
    <a className="quota-mini" href="#/usage" title={`${name} · ${windowLabel(found.window)}已用 ${pct}%`}>
      <span className={`quota-bar ${level(pct, found.account.status.limit_reached)}`}>
        <span style={{ width: `${Math.min(100, pct)}%` }} />
      </span>
      额度 {pct}% · {windowLabel(found.window).replace(/限额$/, "")}
    </a>
  );
}

function Connection() {
  const status = useConnectionStatus();
  const [info, setInfo] = useState<{ home: string; version: string }>();
  useEffect(() => {
    if (status === "connected") api.info().then(setInfo, () => {});
  }, [status]);
  const color = status === "connected" ? "ok" : status === "connecting" ? "warn" : "danger";
  const text = status === "connected" ? "hub 已连接" : status === "connecting" ? "连接中…" : status.replace(/^offline: /, "未连接：");
  return (
    <div className="connection" title={info?.home}>
      <span className={`dot ${color} ${status === "connecting" ? "pulse" : ""}`} />
      <span className="text">{text}</span>
      {info && status === "connected" && <span className="ver">v{info.version}</span>}
    </div>
  );
}

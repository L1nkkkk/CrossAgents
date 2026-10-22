// The sidebar: live runs with their team hanging off a rail, standalone sessions, then
// history. Agents appear as people (avatar, display name, title); runs as ◯ nodes.

import { createContext, useContext, useEffect, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { useActivity, useUnread } from "./activity";
import { api } from "./api";
import { ACTIVE_RUN, runAgents } from "./App";
import { Avatar, BrandMark, Icon, Mark, VendorMark, ago, agentTitle, displayName, runTone, sessionHref, taskTone, vendorName } from "./components";
import { useConnectionStatus } from "./hub";
import { t } from "./lib/i18n";
import { level, tightest, windowLabel } from "./lib/quota";
import { day } from "./lib/time";
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
        <Icon name="search" size={14} />
        <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("sidebar.search")} aria-label={t("sidebar.searchLabel")} />
        {query && (
          <button className="icon-btn" onClick={() => setQuery("")} aria-label={t("common.clear")}>
            <Icon name="close" size={12} />
          </button>
        )}
      </label>

      <a className={`new-run ${!section ? "active" : ""}`} href="#/">
        <Icon name="plus" size={16} />
        <span>{t("sidebar.newRun")}</span>
        <kbd>Ctrl N</kbd>
      </a>

      <div className="scroll">
        {active.length > 0 && <Label text={t("sidebar.live")} count={active.length} />}
        {active.map((d) => (
          <RunGroup key={d.run.id} detail={d} agents={everyone} selected={isRun(d)} isSession={isSession} />
        ))}

        {standalone.length > 0 && <Label text={t("sidebar.sessions")} count={standalone.length} />}
        <Capped items={standalone} keep={isSession} searching={!!q}>
          {(agent) => <SessionLink key={agent.id} agent={agent} active={isSession(agent)} />}
        </Capped>

        {ended.length > 0 && <Label text={t("sidebar.recent")} count={ended.length} />}
        <Capped items={ended} keep={isRun} searching={!!q}>
          {(d, i, shown) => (
            <div key={d.run.id}>
              {(i === 0 || day(shown[i - 1].run.updated_at) !== day(d.run.updated_at)) && (
                <div className="day-label">{day(d.run.updated_at)}</div>
              )}
              <HistoryItem detail={d} agents={everyone} selected={isRun(d)} isSession={isSession} />
            </div>
          )}
        </Capped>

        {q && !runs.length && !standalone.length && <p className="sidebar-empty">{t("sidebar.noMatch", { query })}</p>}
        {!q && tree.data && agents.data && !runs.length && !pool.length && (
          <div className="sidebar-empty">
            <p>{current ? t("sidebar.emptyIn", { name: current.name }) : t("sidebar.empty")}</p>
            <a className="button small" href="#/agents">
              <Icon name="addMate" size={14} />
              {t("sidebar.addMate")}
            </a>
          </div>
        )}
      </div>

      <div className="foot">
        <div className="foot-nav">
          <a className={section === "agents" ? "active" : ""} href="#/agents">
            <Icon name="team" size={15} />
            {t("nav.team")}
          </a>
          <a className={section === "usage" ? "active" : ""} href="#/usage">
            <Icon name="chart" size={15} />
            {t("nav.usage")}
          </a>
          <a className={section === "settings" ? "active" : ""} href="#/settings" title={t("nav.settings")} aria-label={t("nav.settings")}>
            <Icon name="settings" size={15} />
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
          {t("common.more", { n: hidden })}
        </button>
      )}
      {all && items.length > CAP && !searching && (
        <button className="more" onClick={() => setAll(false)}>
          {t("common.less")}
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
            <span className="segments" aria-label={t("sidebar.merged", { merged, total: tasks.length })}>
              {tasks.map((task) => (
                <span key={task.id} className={`seg ${taskTone(task.status).color} ${taskTone(task.status).live ? "live" : ""}`} title={`${task.key} · ${task.status}`} />
              ))}
            </span>
          )}
        </span>
        {run.status === "awaiting_approval" ? (
          <span className="flag">{t("state.awaitingApproval")}</span>
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
  if (agent.status === "error" || unread?.kind === "error") tail = <span className="pill danger">{t("state.error")}</span>;
  else if (planWaiting) tail = <span className="pill warn">{t("state.awaitingApproval")}</span>;
  else if (running)
    tail = (
      <span className="working-tag">
        <span className="spark">✻</span> {t("sidebar.working")}
      </span>
    );
  else if (unread) tail = <span className="tail-done">{t("state.done")}</span>;
  else
    tail = (
      <span className="meta">
        <VendorMark plugin={agent.plugin} size={10} /> {agent.profile?.title || agent.model || vendorName(agent.plugin)}
      </span>
    );

  return (
    <div className="nav-row">
      <a
        className={`nav-item ${active ? "active" : ""} ${agent.status === "stopped" ? "dim" : ""} ${running && doing ? "two" : ""}`}
        href={sessionHref(agent.name)}
        title={`${displayName(agent)} · ${agentTitle(agent)} · ${agent.plugin}${agent.model ? " · " + agent.model : ""} · ${agent.status}\n${t("sidebar.besideHint")}`}
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
        <button className="beside-btn" onClick={beside} title={t("sidebar.openBeside")} aria-label={t("sidebar.openBesideName", { name: displayName(agent) })}>
          <Icon name="beside" size={14} />
        </button>
      )}
    </div>
  );
}

const OUTCOME: Record<string, Parameters<typeof t>[0]> = {
  completed: "outcome.completed",
  failed: "outcome.failed",
  cancelled: "outcome.cancelled",
};

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
  const outcome = OUTCOME[run.status];
  const result = tasks.length > 0 ? t("sidebar.merged", { merged, total: tasks.length }) : outcome ? t(outcome) : run.status;

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
      <button className={`ws-button ${open || onPage ? "open" : ""}`} onClick={() => setOpen(!open)} title={current?.path ?? t("ws.all")} aria-expanded={open}>
        <span className="ws-name">{current?.name ?? "CrossAgents"}</span>
        <span className="ws-scope">{current ? "" : t("ws.all")}</span>
        <span className="chev">
          <Icon name="chevron" size={14} />
        </span>
      </button>
      {open && (
        <div className="ws-menu" role="menu">
          <button className={`ws-item ${!current ? "on" : ""}`} onClick={() => pick("")} role="menuitem">
            <span className="ws-item-main">
              <span className="ws-item-name">{t("ws.all")}</span>
              <span className="ws-item-sub">{t("ws.repos", { n: workspaces.length })}</span>
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
          {!workspaces.length && <div className="ws-empty">{t("ws.empty")}</div>}
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
    <a className="quota-mini" href="#/usage" title={t("quota.miniTitle", { name, window: windowLabel(found.window), pct })}>
      <span className={`quota-bar ${level(pct, found.account.status.limit_reached)}`}>
        <span style={{ width: `${Math.min(100, pct)}%` }} />
      </span>
      {t("quota.mini", { pct, window: windowLabel(found.window).replace(/限额$/, "") })}
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
  const text =
    status === "connected" ? t("hub.connected") : status === "connecting" ? t("hub.connecting") : t("hub.offline", { reason: status.replace(/^offline: /, "") });
  return (
    <div className="connection" title={info?.home}>
      <span className={`dot ${color} ${status === "connecting" ? "pulse" : ""}`} />
      <span className="text">{text}</span>
      {info && status === "connected" && <span className="ver">v{info.version}</span>}
    </div>
  );
}

// The sidebar, managed like Claude Code's: search, a short nav, pinned rows, then one group per
// workspace. In a group, runs and standalone sessions are plain rows, newest first; a run's
// teammates (the ones it started with and the ones its lead recruited) nest under it.

import { createContext, useContext, useEffect, useState, useSyncExternalStore, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { useActivity, useUnread } from "./activity";
import { api } from "./api";
import { ACTIVE_RUN, runAgents } from "./App";
import { Avatar, BrandMark, Icon, Mark, agentTitle, displayName, hueOf, runTone, sessionHref } from "./components";
import { useConnectionStatus } from "./hub";
import { t } from "./lib/i18n";
import { level, tightest, windowLabel } from "./lib/quota";
import { openBeside } from "./panes";
import { useQuota } from "./views/Quota";
import { selectWorkspace, useWorkspaces, type Workspace } from "./workspace";

/** A row in a group: a run (with its team) or a session outside any run. */
type Item = { kind: "run"; key: string; at: string; detail: RunDetail } | { kind: "session"; key: string; at: string; agent: Agent };

const runKey = (d: RunDetail) => `run:${d.run.id}`;
const sessionKey = (a: Agent) => `s:${a.name}`;

export function Sidebar({ route }: { route: string[] }) {
  const { list: workspaces, tree, agents } = useWorkspaces();
  const pinned = usePinned();
  const [query, setQuery] = useState("");
  const [section, selected] = route;
  const everyone = agents.data ?? [];
  const q = query.trim().toLowerCase();
  const matchesAgent = (a: Agent) => !q || a.name.toLowerCase().includes(q) || displayName(a).toLowerCase().includes(q);
  const matchesRun = (d: RunDetail) => !q || d.run.title.toLowerCase().includes(q) || runAgents(d, everyone).some(matchesAgent);

  const inRuns = new Set((tree.data ?? []).flatMap((d) => runAgents(d, everyone).map((a) => a.id)));
  const itemsOf = (w: Workspace): Item[] =>
    [
      ...w.runs.filter(matchesRun).map((detail): Item => ({ kind: "run", key: runKey(detail), at: detail.run.updated_at, detail })),
      ...w.agents
        .filter((a) => !inRuns.has(a.id) && matchesAgent(a))
        .map((agent): Item => ({ kind: "session", key: sessionKey(agent), at: agent.created_at, agent })),
    ].sort((a, b) => b.at.localeCompare(a.at));

  const groups = workspaces.map((w) => ({ w, items: itemsOf(w) }));
  const pins = groups.flatMap((g) => g.items).filter((i) => pinned.has(i.key));
  const shown = groups.map((g) => ({ ...g, items: g.items.filter((i) => !pinned.has(i.key)) })).filter((g) => g.items.length || (!q && g.w.agents.length));
  const isRun = (d: RunDetail) => section === "runs" && selected === d.run.id;
  const isSession = (a: Agent) => section === "s" && selected === a.name;
  const pendingPlans = new Set((tree.data ?? []).filter((d) => d.run.status === "awaiting_approval").map((d) => d.run.planner));
  const loaded = !!tree.data && !!agents.data;

  const row = (item: Item) =>
    item.kind === "run" ? (
      <RunRow key={item.key} detail={item.detail} agents={everyone} selected={isRun(item.detail)} isSession={isSession} searching={!!q} />
    ) : (
      <SessionRow key={item.key} agent={item.agent} active={isSession(item.agent)} pinKey={item.key} />
    );

  return (
    <PendingPlans.Provider value={pendingPlans}>
      <nav className="sidebar">
        <div className="brand">
          <BrandMark size={18} />
          <span className="brand-name">CrossAgents</span>
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

        <div className="side-nav">
          <a className={`nav-new ${!section ? "active" : ""}`} href="#/" onClick={() => selectWorkspace("")}>
            <Icon name="plus" size={16} />
            <span>{t("sidebar.new")}</span>
            <kbd>Ctrl N</kbd>
          </a>
          <a className={`nav-team ${section === "agents" ? "active" : ""}`} href="#/agents">
            <Icon name="team" size={16} />
            <span>{t("nav.team")}</span>
          </a>
          <a className={`nav-usage ${section === "usage" ? "active" : ""}`} href="#/usage">
            <Icon name="chart" size={16} />
            <span>{t("nav.usage")}</span>
          </a>
        </div>

        <div className="scroll">
          {pins.length > 0 && (
            <section className="group">
              <div className="group-head static">
                <span className="group-name">{t("sidebar.pinned")}</span>
              </div>
              {pins.map(row)}
            </section>
          )}

          {shown.map(({ w, items }) => (
            <Group key={w.key} workspace={w} searching={!!q}>
              <Capped items={items} keep={(i) => (i.kind === "run" ? isRun(i.detail) || runAgents(i.detail, everyone).some(isSession) : isSession(i.agent))} searching={!!q}>
                {row}
              </Capped>
            </Group>
          ))}

          {q && !pins.length && !shown.length && <p className="sidebar-empty">{t("sidebar.noMatch", { query })}</p>}
          {!q && loaded && !workspaces.length && <p className="sidebar-empty">{t("sidebar.empty")}</p>}
        </div>

        <div className="foot">
          <QuotaMini />
          <div className="foot-row">
            <Connection />
            <a className={`icon-btn ${section === "settings" ? "on" : ""}`} href="#/settings" title={t("nav.settings")} aria-label={t("nav.settings")}>
              <Icon name="settings" size={15} />
            </a>
          </div>
        </div>
      </nav>
    </PendingPlans.Provider>
  );
}

/** A workspace: its name, collapsible, with + to start something new in it. */
function Group({ workspace, searching, children }: { workspace: Workspace; searching: boolean; children: ReactNode }) {
  const collapsed = useCollapsed();
  const closed = collapsed.has(workspace.key) && !searching;
  return (
    <section className="group">
      <div className="group-head">
        <button className="group-name" onClick={() => toggleCollapsed(workspace.key)} title={workspace.path} aria-expanded={!closed}>
          <span className={`ws-dot av-${hueOf(workspace.key)}`} />
          {workspace.name}
          {closed && <Icon name="chevronRight" size={13} />}
        </button>
        <a
          className="icon-btn group-add"
          href="#/"
          onClick={() => selectWorkspace(workspace.key)}
          title={t("sidebar.newIn", { name: workspace.name })}
          aria-label={t("sidebar.newIn", { name: workspace.name })}
        >
          <Icon name="plus" size={14} />
        </a>
      </div>
      {!closed && children}
    </section>
  );
}

/** Rows shown before "更多": long groups stay short until asked. */
const CAP = 5;

/** The first `CAP` items (plus any `keep` wants visible, e.g. the selected one), then "更多 (n)". */
function Capped<T>({ items, keep, searching, children }: { items: T[]; keep: (item: T) => boolean; searching: boolean; children: (item: T) => ReactNode }) {
  const [all, setAll] = useState(false);
  if (!items.length) return null;
  const shown = all || searching ? items : items.filter((item, i) => i < CAP || keep(item));
  const hidden = items.length - shown.length;
  return (
    <>
      {shown.map(children)}
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
    </>
  );
}

/** Planners whose plan waits for approval; provided once by the sidebar for every row. */
const PendingPlans = createContext<Set<string>>(new Set());

type Dot = "live" | "warn" | "danger" | "unread" | undefined;

/** A run: its title, a chevron when it has a team, a status dot; the team nests under it. */
function RunRow({ detail, agents, selected, isSession, searching }: { detail: RunDetail; agents: Agent[]; selected: boolean; isSession: (a: Agent) => boolean; searching: boolean }) {
  const { run } = detail;
  const members = runAgents(detail, agents);
  const unread = useUnread();
  const live = ACTIVE_RUN.includes(run.status);
  const memberActive = members.some(isSession);
  // Live runs and the one you are in start open; your click wins after that.
  const [toggled, setToggled] = useState<boolean>();
  const open = searching || (toggled ?? (live || selected || memberActive));
  useEffect(() => {
    if (memberActive) setToggled(undefined);
  }, [memberActive]);

  const dot: Dot =
    run.status === "awaiting_approval" || run.status === "paused"
      ? "warn"
      : run.status === "failed" || members.some((m) => m.status === "error" || unread[m.id]?.kind === "error")
        ? "danger"
        : live
          ? "live"
          : members.some((m) => unread[m.id])
            ? "unread"
            : undefined;

  return (
    <div className="tree">
      <Row
        href={`#/runs/${run.id}`}
        active={selected}
        dim={!live}
        title={`${run.title}\n${run.request}`}
        dot={dot}
        pinKey={runKey(detail)}
        lead={<Mark shape="run" tone={runTone(run.status)} size={11} title={run.status} />}
        label={run.title}
        after={
          members.length > 0 && (
            <button
              className={`disclose ${open ? "open" : ""}`}
              onClick={(e) => {
                e.preventDefault();
                setToggled(!open);
              }}
              aria-label={t(open ? "sidebar.hideTeam" : "sidebar.showTeam")}
              aria-expanded={open}
            >
              <Icon name="chevron" size={13} />
            </button>
          )
        }
      />
      {open && members.length > 0 && (
        <div className="children">
          {members.map((agent) => (
            <SessionRow key={agent.id} agent={agent} active={isSession(agent)} note={workingOn(detail, agent)} />
          ))}
        </div>
      )}
    </div>
  );
}

/** What a teammate does in the run: the task it works on or reviews, else its title (the lead's). */
function workingOn(detail: RunDetail, agent: Agent): string {
  const task = detail.tasks.find((t) => t.assignee === agent.id) ?? detail.tasks.find((t) => t.reviewer === agent.id);
  return task ? `${task.key} · ${task.title}` : agent.profile?.title || agentTitle(agent);
}

/**
 * A conversation: the agent's name, with a dot when it needs you (出错, 待审批), is working, or
 * finished while you were away. Ctrl/⌘-click or middle-click opens it beside the current one.
 */
function SessionRow({ agent, active, note, pinKey }: { agent: Agent; active: boolean; note?: string; pinKey?: string }) {
  const doing = useActivity(agent.id);
  const unread = useUnread()[agent.id];
  const planWaiting = useContext(PendingPlans).has(agent.id);
  const dot: Dot =
    agent.status === "error" || unread?.kind === "error" ? "danger" : planWaiting ? "warn" : agent.status === "running" ? "live" : unread ? "unread" : undefined;
  const role = agent.profile?.title || agentTitle(agent);
  return (
    <Row
      href={sessionHref(agent.name)}
      active={active}
      dim={agent.status === "stopped"}
      title={`${displayName(agent)} · ${role} · ${agent.plugin}${agent.model ? " · " + agent.model : ""} · ${agent.status}${doing ? "\n" + doing : ""}\n${t("sidebar.besideHint")}`}
      dot={dot}
      pinKey={pinKey}
      beside={agent.name}
      lead={<Avatar agent={agent} size={note ? 16 : 18} />}
      label={
        <>
          {displayName(agent)}
          {note && <span className="role">{note}</span>}
        </>
      }
    />
  );
}

/** One line: a link with a label, a status dot at the end, and pin / open-beside on hover. */
function Row({
  href,
  active,
  dim,
  title,
  lead,
  label,
  after,
  dot,
  pinKey,
  beside,
}: {
  href: string;
  active: boolean;
  dim?: boolean;
  title: string;
  lead?: ReactNode;
  label: ReactNode;
  after?: ReactNode;
  dot: Dot;
  pinKey?: string;
  beside?: string;
}) {
  const pinned = usePinned();
  const isPinned = !!pinKey && pinned.has(pinKey);
  const openAside = (e: ReactMouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (beside) openBeside(beside);
  };
  return (
    <div className={`row ${active ? "active" : ""} ${dim ? "dim" : ""}`}>
      <a
        className="row-link"
        href={href}
        title={title}
        onClick={(e) => beside && (e.ctrlKey || e.metaKey) && openAside(e)}
        onMouseDown={(e) => e.button === 1 && beside && e.preventDefault()}
        onAuxClick={(e) => e.button === 1 && beside && openAside(e)}
      >
        {lead && <span className="row-lead">{lead}</span>}
        <span className="row-label">{label}</span>
        {after}
      </a>
      {dot && <span className={`row-dot ${dot}`} aria-hidden="true" />}
      <span className="row-actions">
        {beside && !active && (
          <button className="icon-btn" onClick={openAside} title={t("sidebar.openBeside")} aria-label={t("sidebar.openBeside")}>
            <Icon name="beside" size={13} />
          </button>
        )}
        {pinKey && (
          <button className={`icon-btn ${isPinned ? "on" : ""}`} onClick={() => togglePinned(pinKey)} title={t(isPinned ? "sidebar.unpin" : "sidebar.pin")} aria-label={t(isPinned ? "sidebar.unpin" : "sidebar.pin")}>
            <Icon name="pin" size={13} />
          </button>
        )}
      </span>
    </div>
  );
}

// ---- Pinned rows and collapsed groups, remembered per browser. ----------------------------

function storedSet(key: string) {
  const listeners = new Set<() => void>();
  let value = new Set<string>();
  try {
    value = new Set(JSON.parse(localStorage.getItem(key) ?? "[]") as string[]);
  } catch {
    // Unreadable or unavailable storage: start empty.
  }
  return {
    use: () =>
      useSyncExternalStore(
        (l) => {
          listeners.add(l);
          return () => listeners.delete(l);
        },
        () => value,
      ),
    toggle(item: string) {
      value = new Set(value);
      if (value.has(item)) value.delete(item);
      else value.add(item);
      try {
        localStorage.setItem(key, JSON.stringify([...value]));
      } catch {
        // The choice then lasts for this page only.
      }
      listeners.forEach((l) => l());
    },
  };
}

const pins = storedSet("crossagents.pinned");
const collapsedGroups = storedSet("crossagents.collapsed");
const usePinned = pins.use;
const togglePinned = pins.toggle;
const useCollapsed = collapsedGroups.use;
const toggleCollapsed = collapsedGroups.toggle;

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

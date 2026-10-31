// The sidebar, managed like Claude Code's: search, a short nav, pinned rows, then 对话 (every
// conversation, in a project or not, tagged with its project) and one group per project holding
// its standalone Work. Runs proposed by a conversation nest under it, with their execution
// members one level below; direct recruits stay under their conversation too. Archived or
// missing sources leave their runs visible in the project. Archived chats can be restored.

import { createContext, useContext, useEffect, useState, useSyncExternalStore, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { useActivity, useUnread } from "./activity";
import { api, errorText } from "./api";
import type { DevHub } from "./bridge";
import type { HubInfo } from "@bindings/HubInfo";
import { buildTag, compareBuilds } from "./lib/build";
import { useUpdateState } from "./lib/updates";
import { ACTIVE_RUN, runAgents } from "./App";
import { Avatar, BrandMark, Icon, Mark, agentTitle, displayName, hueOf, isAgent, runTone, sessionHref } from "./components";
import { DeleteRunDialog } from "./deleteRun";
import { useConnectionStatus } from "./hub";
import { t } from "./lib/i18n";
import { prettyModel } from "./lib/models";
import { capped, childPath, groupRunsByOrigin } from "./lib/sidebarTree";
import { startNew } from "./menubar";
import { level, tightest, windowLabel } from "./lib/quota";
import { openBeside } from "./panes";
import { useQuota } from "./views/Quota";
import { CHATS, projectOf, selectWorkspace, useWorkspaces, type Workspace } from "./workspace";

/** A row in a group: a run (with its team) or a session outside any run. */
type Item = { kind: "run"; key: string; at: string; detail: RunDetail } | { kind: "session"; key: string; at: string; agent: Agent };

const runKey = (d: RunDetail) => `run:${d.run.id}`;
const sessionKey = (a: Agent) => `s:${a.id}`;

/** The conversation that recruited `agent` (chat mode), if any. */
const recruiterOf = (agent: Agent) => {
  const by = (agent.config as Record<string, unknown> | null)?.spawned_by_agent;
  return typeof by === "string" ? by : undefined;
};

/** What a conversation is listed by: its title, else its agent's name. */
const labelOf = (agent: Agent) => agent.topic || displayName(agent);

export function Sidebar({ route }: { route: string[] }) {
  const { list: workspaces, tree, agents } = useWorkspaces();
  const pinned = usePinned();
  const [query, setQuery] = useState("");
  const [section, selected] = route;
  // A right-click on a run: its menu, and the delete confirmation it opens.
  const [menu, setMenu] = useState<{ detail: RunDetail; x: number; y: number }>();
  const [deleting, setDeleting] = useState<RunDetail>();
  const everyone = agents.data ?? [];
  const q = query.trim().toLowerCase();
  const matchesAgent = (a: Agent) =>
    !q || a.name.toLowerCase().includes(q) || (a.topic ?? "").toLowerCase().includes(q) || (projectOf(a) ?? "").toLowerCase().includes(q);
  const live = everyone.filter((a) => !a.archived_at);
  const recruitsOf = (a: Agent) => live.filter((r) => recruiterOf(r) === a.id);
  const archived = everyone.filter((a) => a.archived_at && !recruiterOf(a));
  const matchesRun = (d: RunDetail) => !q || d.run.title.toLowerCase().includes(q) || runAgents(d, everyone).some(matchesAgent);

  const inRuns = new Set((tree.data ?? []).flatMap((d) => runAgents(d, everyone).map((a) => a.id)));
  const { children: childRuns, standalone } = groupRunsByOrigin(
    tree.data ?? [],
    new Set(live.filter((a) => !inRuns.has(a.id) && !recruiterOf(a)).map((a) => a.id)),
  );
  const runsOf = (a: Agent) => childRuns.get(a.id) ?? [];
  const isRun = (d: RunDetail) => section === "runs" && selected === d.run.id;
  const isSession = (a: Agent) => section === "s" && isAgent(a, selected);
  const pathOf = (a: Agent) =>
    childPath(runsOf(a), {
      matching: matchesRun,
      selected: (d) => isRun(d) || runAgents(d, everyone).some(isSession),
      pinned: (d) => pinned.has(runKey(d)),
    });
  const standaloneIds = new Set(standalone.map((d) => d.run.id));
  const itemsOf = (w: Workspace): Item[] =>
    [
      ...w.runs.filter((d) => standaloneIds.has(d.run.id) && matchesRun(d)).map((detail): Item => ({ kind: "run", key: runKey(detail), at: detail.run.updated_at, detail })),
      ...w.agents
        .filter((a) => !inRuns.has(a.id) && !a.archived_at && !recruiterOf(a) && (matchesAgent(a) || recruitsOf(a).some(matchesAgent) || pathOf(a).matched))
        .map((agent): Item => ({ kind: "session", key: sessionKey(agent), at: agent.created_at, agent })),
    ].sort((a, b) => b.at.localeCompare(a.at));

  const groups = workspaces.map((w) => ({ w, items: itemsOf(w) }));
  const nestedItems: Item[] = [...childRuns.values()].flat().filter(matchesRun).map((detail) => ({ kind: "run", key: runKey(detail), at: detail.run.updated_at, detail }));
  const pins = [...groups.flatMap((g) => g.items), ...nestedItems].filter((i) => pinned.has(i.key));
  const shown = groups.map((g) => ({ ...g, items: g.items.filter((i) => !pinned.has(i.key)) }))
    .filter((g) => g.items.length || (!q && g.w.agents.some((a) => !a.archived_at && !inRuns.has(a.id) && !recruiterOf(a))));
  const pendingPlans = new Set((tree.data ?? []).filter((d) => d.run.status === "awaiting_approval").map((d) => d.run.planner));
  const loaded = !!tree.data && !!agents.data;

  const runRow = (detail: RunDetail) => (
    <RunRow
      key={runKey(detail)}
      detail={detail}
      agents={everyone}
      selected={isRun(detail)}
      isSession={isSession}
      searching={!!q}
      onMenu={(e) => {
        e.preventDefault();
        setMenu({ detail, x: e.clientX, y: e.clientY });
      }}
    />
  );
  const row = (item: Item) =>
    item.kind === "run" ? runRow(item.detail) : (
      <SessionRow
        key={item.key} agent={item.agent} active={isSession(item.agent)} pinKey={item.key}
        recruits={recruitsOf(item.agent)} isSession={isSession} searching={!!q}
        runs={pathOf(item.agent).shown}
        renderRun={runRow} childActive={pathOf(item.agent).selected}
      />
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
              <InChats.Provider value={true}>{pins.map(row)}</InChats.Provider>
            </section>
          )}

          {shown.map(({ w, items }) => (
            <Group key={w.key} workspace={w} searching={!!q}>
              <InChats.Provider value={w.key === CHATS}>
                <Capped items={items} keep={(i) => (i.kind === "run" ? isRun(i.detail) || runAgents(i.detail, everyone).some(isSession) : isSession(i.agent) || recruitsOf(i.agent).some(isSession) || pathOf(i.agent).selected)} searching={!!q}>
                  {row}
                </Capped>
              </InChats.Provider>
            </Group>
          ))}

          {!q && archived.length > 0 && <Archived agents={archived} isSession={isSession} />}

          {q && !pins.length && !shown.length && <p className="sidebar-empty">{t("sidebar.noMatch", { query })}</p>}
          {!q && loaded && !workspaces.length && <p className="sidebar-empty">{t("sidebar.empty")}</p>}
        </div>

        <div className="foot">
          <UpdateNotice />
          <DevHubNotice />
          <QuotaMini />
          <div className="foot-row">
            <Connection />
            <a className={`icon-btn ${section === "settings" ? "on" : ""}`} href="#/settings" title={t("nav.settings")} aria-label={t("nav.settings")}>
              <Icon name="settings" size={15} />
            </a>
          </div>
        </div>
      </nav>
      {menu && (
        <RunMenu
          at={menu}
          onDelete={() => {
            setDeleting(menu.detail);
            setMenu(undefined);
          }}
          onClose={() => setMenu(undefined)}
        />
      )}
      {deleting && <DeleteRunDialog detail={deleting} onClose={() => setDeleting(undefined)} />}
    </PendingPlans.Provider>
  );
}

/** The right-click menu on a run: delete it, and everything it owns (docs/tasks/R1.md §3). */
function RunMenu({ at, onDelete, onClose }: { at: { x: number; y: number }; onDelete: () => void; onClose: () => void }) {
  return (
    <>
      <div className="menu-scrim" onMouseDown={onClose} onContextMenu={(e) => e.preventDefault()} />
      <div className="context-menu" style={{ left: at.x, top: at.y }} role="menu">
        <button role="menuitem" onClick={onDelete}>
          删除
        </button>
      </div>
    </>
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
          {workspace.key === CHATS ? <Icon name="chat" size={12} /> : <span className={`ws-dot av-${hueOf(workspace.key)}`} />}
          {workspace.name}
          {closed && <Icon name="chevronRight" size={13} />}
        </button>
        <a
          className="icon-btn group-add"
          href="#/"
          onClick={() => {
            // + on the chats group starts a chat outside any project; on a project, Work in it.
            selectWorkspace(workspace.key === CHATS ? "" : workspace.key);
            startNew(workspace.key === CHATS ? "chat" : "run");
          }}
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
  const shown = capped(items, keep, CAP, all || searching);
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
/** Rows in the 对话 group (or pinned): conversations from any project, so each names its own. */
const InChats = createContext(false);

type Dot = "live" | "warn" | "danger" | "unread" | undefined;

/** A run: its title, a chevron when it has a team, a status dot; the team nests under it. */
function RunRow({
  detail,
  agents,
  selected,
  isSession,
  searching,
  onMenu,
}: {
  detail: RunDetail;
  agents: Agent[];
  selected: boolean;
  isSession: (a: Agent) => boolean;
  searching: boolean;
  onMenu?: (e: ReactMouseEvent) => void;
}) {
  const { run } = detail;
  const members = runAgents(detail, agents);
  const unread = useUnread();
  const live = ACTIVE_RUN.includes(run.status);
  // A lead sits in every run it led: only a live one opens for its session (workers belong to one run).
  const memberActive = members.some((m) => isSession(m) && (live || m.id !== run.planner));
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
        onMenu={onMenu}
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
            <SessionRow key={agent.id} agent={agent} active={isSession(agent)} note={workingOn(detail, agent)} ended={!live} />
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
 * A conversation: its title (else its agent's name), with a dot when it needs you (出错, 待审批),
 * is working, or finished while you were away; the teammates it recruited nest under it.
 * Ctrl/⌘-click or middle-click opens it beside the current one.
 */
function SessionRow({
  agent,
  active,
  note,
  pinKey,
  recruits = [],
  isSession,
  searching,
  ended,
  runs = [],
  renderRun,
  childActive = false,
}: {
  agent: Agent;
  active: boolean;
  note?: string;
  pinKey?: string;
  recruits?: Agent[];
  isSession?: (a: Agent) => boolean;
  searching?: boolean;
  /** A member row of a run that is over: the agent may be busy elsewhere (a lead runs many), not here. */
  ended?: boolean;
  runs?: RunDetail[];
  renderRun?: (detail: RunDetail) => ReactNode;
  childActive?: boolean;
}) {
  const [toggled, setToggled] = useState<boolean>();
  const recruitActive = !!isSession && recruits.some(isSession);
  const open = !!searching || (toggled ?? (active || recruitActive || childActive || recruits.some((r) => r.status === "running")));
  useEffect(() => {
    if (childActive) setToggled(undefined);
  }, [childActive]);
  const doing = useActivity(agent.id);
  const unread = useUnread()[agent.id];
  const planWaiting = useContext(PendingPlans).has(agent.id);
  const dot: Dot =
    agent.status === "error" || unread?.kind === "error"
      ? "danger"
      : ended
        ? unread
          ? "unread"
          : undefined
        : planWaiting
          ? "warn"
          : agent.status === "running"
            ? "live"
            : unread
              ? "unread"
              : undefined;
  const role = agent.profile?.title || agentTitle(agent);
  // Conversations from every project share one list: each says which project it works in.
  const project = useContext(InChats) ? projectOf(agent) : undefined;
  const row = (
    <Row
      href={sessionHref(agent)}
      active={active}
      dim={agent.status === "stopped"}
      title={`${agent.topic ? agent.topic + "\n" : ""}${displayName(agent)} · ${role} · ${agent.plugin}${agent.model ? " · " + agent.model : ""} · ${agent.status}${doing ? "\n" + doing : ""}\n${t("sidebar.besideHint")}`}
      dot={dot}
      pinKey={pinKey}
      beside={agent.id}
      archive={note ? undefined : agent}
      lead={<Avatar agent={agent} size={note ? 16 : 18} />}
      label={
        <>
          {note ? displayName(agent) : labelOf(agent)}
          {note && <span className="role">{note}</span>}
        </>
      }
      tag={!note && project ? <span className="row-tag" title={agent.cwd}>{project}</span> : undefined}
      after={
        (recruits.length > 0 || runs.length > 0) && (
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
  );
  if (!recruits.length && !runs.length) return row;
  return (
    <div className="tree">
      {row}
      {open && (
        <div className="children">
          {runs.map((d) => renderRun?.(d))}
          {recruits.map((r) => (
            <SessionRow key={r.id} agent={r} active={!!isSession?.(r)} note={r.profile?.title || (r.model ? prettyModel(r.model) : r.plugin)} />
          ))}
        </div>
      )}
    </div>
  );
}

/** Archived conversations, folded at the bottom; each can be restored (with its recruits). */
function Archived({ agents, isSession }: { agents: Agent[]; isSession: (a: Agent) => boolean }) {
  const [open, setOpen] = useState(false);
  const sorted = agents.slice().sort((a, b) => (b.archived_at ?? "").localeCompare(a.archived_at ?? ""));
  return (
    <section className="group archived-group">
      <div className="group-head">
        <button className="group-name" onClick={() => setOpen(!open)} aria-expanded={open}>
          <Icon name="archive" size={12} />
          {t("sidebar.archived", { n: agents.length })}
          <Icon name={open ? "chevron" : "chevronRight"} size={13} />
        </button>
      </div>
      {open &&
        sorted.map((a) => (
          <div key={a.id} className={`nav-row dim ${isSession(a) ? "active" : ""}`}>
            <a className="row-link" href={sessionHref(a)} title={`${labelOf(a)} · ${displayName(a)}`}>
              <span className="row-lead">
                <Avatar agent={a} size={16} />
              </span>
              <span className="row-label">{labelOf(a)}</span>
            </a>
            <span className="row-actions">
              <button className="icon-btn" onClick={() => void api.archiveAgent(a.id, false)} title={t("sidebar.restore")} aria-label={t("sidebar.restore")}>
                <Icon name="restore" size={13} />
              </button>
            </span>
          </div>
        ))}
    </section>
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
  tag,
  after,
  dot,
  pinKey,
  beside,
  archive,
  onMenu,
}: {
  href: string;
  active: boolean;
  dim?: boolean;
  title: string;
  lead?: ReactNode;
  label: ReactNode;
  /** A short label kept visible after `label`, which truncates first (a conversation's project). */
  tag?: ReactNode;
  after?: ReactNode;
  dot: Dot;
  pinKey?: string;
  beside?: string;
  /** The conversation this row archives from its hover actions. */
  archive?: Agent;
  /** The right-click menu of this row. */
  onMenu?: (e: ReactMouseEvent) => void;
}) {
  const pinned = usePinned();
  const isPinned = !!pinKey && pinned.has(pinKey);
  const openAside = (e: ReactMouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (beside) openBeside(beside);
  };
  return (
    <div className={`nav-row ${active ? "active" : ""} ${dim ? "dim" : ""}`}>
      <a
        className="row-link"
        href={href}
        title={title}
        onClick={(e) => beside && (e.ctrlKey || e.metaKey) && openAside(e)}
        onMouseDown={(e) => e.button === 1 && beside && e.preventDefault()}
        onAuxClick={(e) => e.button === 1 && beside && openAside(e)}
        onContextMenu={onMenu}
      >
        {lead && <span className="row-lead">{lead}</span>}
        <span className="row-label">{label}</span>
        {tag}
        {after}
      </a>
      {dot && <span className={`row-dot ${dot}`} aria-hidden="true" />}
      <span className="row-actions">
        {beside && !active && (
          <button className="icon-btn" onClick={openAside} title={t("sidebar.openBeside")} aria-label={t("sidebar.openBeside")}>
            <Icon name="beside" size={13} />
          </button>
        )}
        {archive && (
          <button
            className="icon-btn"
            onClick={(e) => {
              e.preventDefault();
              void api.archiveAgent(archive.id, true);
            }}
            title={t("sidebar.archive")}
            aria-label={t("sidebar.archive")}
          >
            <Icon name="archive" size={13} />
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

/**
 * The installed GUI found a development hub (`hub start` from `target/`) where its own should
 * run: it leaves it alone, so say so, or the page and the backend quietly disagree on version.
 */
function DevHubNotice() {
  const status = useConnectionStatus();
  const [dev, setDev] = useState<DevHub | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (status === "connected") window.crossagents?.devHub?.().then(setDev, () => {});
  }, [status]);
  if (!dev) return null;
  const switchHub = async () => {
    setBusy(true);
    setError(undefined);
    try {
      await window.crossagents?.useShippedHub?.();
      setDev((await window.crossagents?.devHub?.()) ?? null);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="dev-hub" title={t("hub.devTitle", { exe: dev.exe })}>
      <span>{t("hub.dev", { shipped: dev.shipped })}</span>
      {error && <span className="err">{error}</span>}
      <button className="link" disabled={busy} onClick={switchHub}>
        {busy ? t("hub.switching") : t("hub.useShipped")}
      </button>
    </div>
  );
}

function Connection() {
  const status = useConnectionStatus();
  const [info, setInfo] = useState<HubInfo>();
  useEffect(() => {
    if (status === "connected") api.info().then(setInfo, () => {});
  }, [status]);
  const color = status === "connected" ? "ok" : status === "connecting" ? "warn" : "danger";
  const text =
    status === "connected" ? t("hub.connected") : status === "connecting" ? t("hub.connecting") : t("hub.offline", { reason: status.replace(/^offline: /, "") });
  return (
    <div className="connection" title={info?.home}>
      <span className={`dot ${color} ${status === "connecting" ? "pulse" : ""}`} />
      <span className="text" title={text}>{text}</span>
      {info && status === "connected" && <Versions hub={info} />}
    </div>
  );
}

/**
 * One version when page and hub come from the same build; both, in warning colour, when not
 * (an installed page on a hub built from another commit). The tooltip has the details.
 */
function Versions({ hub }: { hub: HubInfo }) {
  const page = __BUILD__;
  const cmp = compareBuilds(page, hub);
  const dev = page.dev || hub.debug;
  const title = [
    t("build.page", { tag: buildTag(page), dev: page.dev ? t("build.dev") : "" }),
    t("build.hub", { tag: buildTag(hub), dev: hub.debug ? t("build.dev") : "" }),
    hub.home,
  ].join("\n");
  return (
    <span className={`ver ${cmp.same ? "" : "differs"}`} title={title}>
      {cmp.same ? `v${hub.version}` : t("build.differs", { page: cmp.page, hub: cmp.hub })}
      {dev && <span className="dev">dev</span>}
    </span>
  );
}

/**
 * A downloaded update, offered where the person looks anyway: it installs by itself when the
 * GUI quits, but the tray keeps the GUI running for days. 稍后 hides it until the next version.
 */
function UpdateNotice() {
  const [state] = useUpdateState();
  const [later, setLater] = useState<string>();
  if (state?.state !== "ready" || later === state.version) return null;
  return (
    <div className="update-ready" title="重启只换界面：后台的 hub 和 agent 不会中断，重新打开后自动换成新版本">
      <Icon name="info" size={14} />
      <span className="update-text">新版本 {state.version} 已下载</span>
      <button className="link" onClick={() => setLater(state.version)}>
        稍后
      </button>
      <button className="primary small" onClick={() => window.crossagents?.updates?.install()}>
        重启更新
      </button>
    </div>
  );
}

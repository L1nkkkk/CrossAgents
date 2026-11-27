// Flat project/member navigation. Domain grouping and human work live in pure views.
import { createContext, useContext, useEffect, useMemo, useRef, useState, useSyncExternalStore, type CSSProperties, type DragEvent as ReactDragEvent, type FocusEvent as ReactFocusEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { HubInfo } from "@bindings/HubInfo";
import type { RemoteNodeInfo } from "@bindings/RemoteNodeInfo";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { SessionSearchHit } from "@bindings/SessionSearchHit";
import type { Work } from "@bindings/Work";
import type { WorkDetail } from "@bindings/WorkDetail";
import { deliveryOf, lastError, markAllSeen, useActivity, useDelivery, useUnread } from "./activity";
import { api, errorText } from "./api";
import type { DevHub } from "./bridge";
import { buildTag, compareBuilds } from "./lib/build";
import { useUpdateState } from "./lib/updates";
import { Avatar, BrandMark, Icon, ModelMark, VendorMark, ago, displayName, isAgent, relationText, sessionHref, sessionLabel } from "./components";
import { useConnectionStatus, onHubEvent, useHubData } from "./hub";
import { ChatAvatar } from "./ChatAvatar";
import { useChatProfile } from "./lib/chatProfile";
import { vendorLabel } from "./lib/models";
import { RunModeMark, RunModeTag, leadsInPlace, modeWords } from "./runmode";
import { runState } from "./lib/taskState";
import { RowElapsed, usePulseSeeds, useRowLive } from "./liveTurn";
import { t } from "./lib/i18n";
import { sessionWork } from "./lib/projectView";
import { stepLine } from "./lib/workGroups";
import { WorkDetails } from "./lib/workDetails";
import { SessionPeek, TaskSegments, type PeekFacts } from "./SessionPeek";
import { CENTRE_MACHINE, buildMachineGroups, machineFoldKey, reorderIds, searchMachineGroups, type SidebarMachine, capped, groupClosed, shortSince, summaryText, type SidebarGroup, type SidebarMember } from "./lib/sidebarTree";
import { agentMachineKey, normalizeNodeOs, selectNode, useNodes } from "./nodes";
import { todoItems, visibleTodos, type TodoItem } from "./lib/todos";
import { useUnsortedIssues } from "./lessons/unsorted";
import { useUnboundProjects } from "./lessons/projects";
import { FEATURE_LABELS, hiddenByHub, missingText, useFeatureSupport, useHubFeatures } from "./lib/features";
import { useQuestions } from "./questions";
import { startNew } from "./menubar";
import { level, resetText, tightest } from "./lib/quota";
import { openBeside } from "./panes";
import { useExtensions } from "./extensions/defaults";
import { useQuota } from "./views/Quota";
import { markWords } from "./lib/connection";
import { useConnectionState } from "./lib/useConnection";
import { OsIcon } from "./osIcon";
import "./sidebar.css";
import { CHATS, selectWorkspace, useWorkspaces, wsHref, wsKey } from "./workspace";

const Confirmed = createContext(true);
const Needs = createContext<readonly TodoItem[]>([]);
const Runs = createContext<readonly RunDetail[]>([]);
/** Persisted per-group session drag-and-drop order. */
const SESSION_ORDER_KEY = "crossagents.sessionOrder";
const sessionOrderListeners = new Set<() => void>();
let sessionOrderRaw: string | null | undefined;
let sessionOrderCache: Record<string, string[]> = {};
function readSessionOrder(): Record<string, string[]> {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(SESSION_ORDER_KEY);
  } catch {
    raw = null;
  }
  if (raw === sessionOrderRaw) return sessionOrderCache;
  sessionOrderRaw = raw;
  try {
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    sessionOrderCache = parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, string[]>) : {};
  } catch {
    sessionOrderCache = {};
  }
  return sessionOrderCache;
}
function saveSessionOrder(group: string, ids: string[]) {
  const next = { ...readSessionOrder(), [group]: ids };
  const raw = JSON.stringify(next);
  try {
    localStorage.setItem(SESSION_ORDER_KEY, raw);
  } catch {
    // Ignore storage failures.
  }
  sessionOrderRaw = raw;
  sessionOrderCache = next;
  for (const fn of sessionOrderListeners) fn();
}
function useSessionOrder(): Record<string, string[]> {
  return useSyncExternalStore(
    (cb) => {
      sessionOrderListeners.add(cb);
      return () => sessionOrderListeners.delete(cb);
    },
    readSessionOrder,
    readSessionOrder,
  );
}
let draggingSession: { group: string; id: string } | null = null;
/** How long a row stays lit after its turn ends. */
const DONE_FLASH = 1800;
/** An entry the hub refused (403): still readable, so its reason shows, but not clickable. */
const REFUSED: CSSProperties = { opacity: 0.5, cursor: "default" };
/** A pointer passing through the list shows no card. */
const PEEK_DELAY = 350;
const CRAWL_DELAY = 200;
const CRAWL_PX_PER_MS = 0.04;
/** Clipped by less than this, moving reads as jitter, not as showing more. */
const CRAWL_MIN_PX = 8;
/** Minimalist 44px collapsed icon rail: expand, new, search, live/waiting session avatars, tools popover, settings. */
export function SidebarRail({ route, onExpand }: { route: string[]; onExpand: () => void }) {
  const agents = useHubData(() => api.listAgents().then((r) => r.agents), [], (e) => e.type.startsWith("agent."));
  const questions = useQuestions();
  const unread = useUnread();
  const [toolsOpen, setToolsOpen] = useState(false);
  const inToolRoute = ["usage", "expertise", "git", "lessons", "extensions", "ext"].includes(route[0] ?? "");
  const waitingIds = new Set((questions.data?.items ?? []).filter((q) => q.status === "open").map((q) => q.from));

  const liveAgents = (agents.data ?? [])
    .filter((a) => !a.archived_at)
    .map((a) => {
      const state = deliveryOf(a.id)?.state;
      const running = state ? state !== "idle" : a.status === "running";
      const waiting = waitingIds.has(a.id);
      const err = a.status === "error" || unread[a.id]?.kind === "error";
      const done = unread[a.id]?.kind === "done";
      const priority = err ? 4 : waiting ? 3 : running ? 2 : done ? 1 : 0;
      return { agent: a, running, waiting, err, done, priority };
    })
    .filter((x) => x.priority > 0)
    .sort((a, b) => b.priority - a.priority)
    .slice(0, 6);

  return (
    <nav className="sidebar-rail" aria-label="极简导航栏">
      <button type="button" className="rail-btn brand-btn" onClick={onExpand} title="展开侧栏 (Ctrl+B)" aria-label="展开侧栏">
        <BrandMark size={18} />
      </button>
      <a className={`rail-btn ${!route[0] ? "active" : ""}`} href="#/" onClick={() => selectWorkspace("")} title="新建会话 (Ctrl+N)" aria-label="新建会话">
        <Icon name="plus" size={16} />
      </a>
      <button
        type="button"
        className="rail-btn"
        title="搜索会话 (Ctrl+K)"
        aria-label="搜索会话"
        onClick={() => {
          onExpand();
          requestAnimationFrame(() => document.querySelector<HTMLInputElement>(".sidebar .search input")?.focus());
        }}
      >
        <Icon name="search" size={15} />
      </button>
      {liveAgents.length > 0 && <div className="rail-sep" />}
      <div className="rail-live">
        {liveAgents.map(({ agent, running, waiting, err }) => {
          const active = route[0] === "s" && isAgent(agent, route[1]);
          const dotClass = err ? "danger" : waiting ? "warn" : running ? "live" : "";
          const hint = `${sessionLabel(agent)} · ${err ? "出错" : waiting ? "等你处理" : running ? "运行中" : "有新回复"}`;
          return (
            <a key={agent.id} className={`rail-btn ${active ? "active" : ""}`} href={sessionHref(agent)} title={hint} aria-label={hint}>
              <Avatar agent={agent} size={22} />
              <span className={`rail-dot ${dotClass}`} aria-hidden="true" />
            </a>
          );
        })}
      </div>
      <div className="rail-sep" />
      <button
        type="button"
        className={`rail-btn ${toolsOpen || inToolRoute ? "active" : ""}`}
        title="更多工具（用量、专长、Git、排障、扩展）"
        aria-label="更多工具"
        aria-expanded={toolsOpen}
        onClick={() => setToolsOpen((v) => !v)}
      >
        <Icon name="sliders" size={15} />
      </button>
      <a className={`rail-btn ${route[0] === "settings" ? "active" : ""}`} href="#/settings" title="设置" aria-label="设置">
        <Icon name="settings" size={16} />
      </a>
      {toolsOpen && (
        <>
          <div className="menu-scrim" onClick={() => setToolsOpen(false)} />
          <div className="context-menu rail-tools-pop" role="menu">
            <a className="menu-item" href="#/usage" onClick={() => setToolsOpen(false)}>
              <Icon name="chart" size={14} />
              <span className="menu-label">{t("nav.usage")}</span>
            </a>
            <a className="menu-item" href="#/expertise" onClick={() => setToolsOpen(false)}>
              <Icon name="brain" size={14} />
              <span className="menu-label">{t("nav.expertise")}</span>
            </a>
            <a className="menu-item" href="#/git" onClick={() => setToolsOpen(false)}>
              <Icon name="branch" size={14} />
              <span className="menu-label">Git</span>
            </a>
            <a className="menu-item" href="#/lessons" onClick={() => setToolsOpen(false)}>
              <Icon name="tasks" size={14} />
              <span className="menu-label">{t("nav.lessons")}</span>
            </a>
            <a className="menu-item" href="#/extensions" onClick={() => setToolsOpen(false)}>
              <Icon name="panel" size={14} />
              <span className="menu-label">扩展</span>
            </a>
          </div>
        </>
      )}
    </nav>
  );
}

/** How long a row's archive can be taken back from the sidebar. */
const UNDO_WINDOW = 8000;
const Archiving = createContext<(agent: Agent) => void>(() => {});
const actionWord = (action: TodoItem["action"]) => t(action === "answer" ? "sidebar.answer" : action === "confirm" ? "sidebar.confirm" : "sidebar.view");

/**
 * Moves a row's clipped line to its end while the row is hovered, past the hover actions that sit
 * over its right side; the returned function puts it back in one step.
 */
function crawl(row: HTMLElement): () => void {
  const line = row.querySelector<HTMLElement>("[data-crawl]");
  if (!line || matchMedia("(prefers-reduced-motion: reduce)").matches) return () => {};
  const actions = row.querySelector(".row-actions");
  const covered = actions ? Math.max(0, line.getBoundingClientRect().right - actions.getBoundingClientRect().left) : 0;
  line.style.textOverflow = "clip";
  line.style.paddingRight = `${covered}px`;
  const range = line.scrollWidth - line.clientWidth;
  let frame = 0;
  if (range > CRAWL_MIN_PX) {
    const start = performance.now();
    const move = (now: number) => {
      line.scrollLeft = Math.min(range, (now - start) * CRAWL_PX_PER_MS);
      if (line.scrollLeft < range) frame = requestAnimationFrame(move);
    };
    frame = requestAnimationFrame(move);
  }
  return () => {
    cancelAnimationFrame(frame);
    line.scrollLeft = 0;
    line.style.textOverflow = "";
    line.style.paddingRight = "";
  };
}

/** Archives from a row, then offers it back for a moment; a failure stays until it is closed. */
function useArchiveNote() {
  const [note, setNote] = useState<{ agent: Agent; error?: string } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const show = (next: { agent: Agent; error?: string } | null) => {
    clearTimeout(timer.current);
    setNote(next);
    if (next && !next.error) timer.current = setTimeout(() => setNote(null), UNDO_WINDOW);
  };
  const archive = (agent: Agent) => {
    api.archiveAgent(agent.id, true).then(
      () => show({ agent }),
      (err) => show({ agent, error: t("sidebar.archiveFailed", { name: displayName(agent), reason: errorText(err) }) }),
    );
  };
  const undo = (agent: Agent) => {
    show(null);
    api.archiveAgent(agent.id, false).catch((err) => show({ agent, error: t("sidebar.undoFailed", { name: displayName(agent), reason: errorText(err) }) }));
  };
  return { note, archive, undo, close: () => show(null) };
}
const sessionKey = (a: Agent) => "s:" + a.id;
const labelOf = (a: Agent) => sessionLabel(a);
/** How many of a project's work groups show before 「更多」 reveals the rest. */
const WORKS_PER_PROJECT = 3;
/** The fold of the work groups no project claims. */
const OTHER_WORKS = "__other_works";

/**
 * The work groups (docs/tasks/G1.md §3.5): one row each, however many rounds they have had.
 * This is the list only — one request — so every group can be placed under its project without
 * reading each group's rounds. `WorkDetails` below reads those, and only for a row that opens.
 * `enabled` is false on a hub that does not have the page (an older hub, or one whose token may
 * not read it), which this then does not even ask.
 */
function useWorks(enabled: boolean) {
  return useHubData(
    async () => {
      if (!enabled) return [];
      const { works } = await api.listWorks();
      return works.slice().sort((a, b) => b.updated_at.localeCompare(a.updated_at));
    },
    [enabled],
    (e) => e.type.startsWith("work.") || e.type.startsWith("run."),
  );
}

/** Work details are shared by the sidebar: read on a row's fold, refreshed while it stays open. */
const workDetails = new WorkDetails();

function useWorkDetail(workId: string, open: boolean): WorkDetail | undefined {
  const [detail, setDetail] = useState(() => workDetails.get(workId));
  useEffect(() => {
    if (!open) return;
    // A row read before shows its rounds at once; expand asks again only if an event has made
    // them stale. One opened for the first time asks for them now.
    setDetail(workDetails.get(workId));
    void workDetails.expand(workId, api.getWork);
    const off = workDetails.subscribe(() => setDetail(workDetails.get(workId)));
    return () => {
      off();
      workDetails.collapse(workId);
    };
  }, [workId, open]);
  return detail;
}

/** A work group's project: the same path normalisation the sidebar already groups by. */
const workProject = (work: Work) => wsKey(work.repo);

/**
 * One work group: its title, the latest loaded round's state and mode; its rounds fold out
 * below. The rounds behind the fold are read on opening it, not for every row on the list.
 */
function WorkRow({ work, runs, lead, active, selected }: { work: Work; runs: Run[]; lead?: Agent; active: boolean; selected?: string }) {
  const [open, setOpen] = useState(false);
  const detail = useWorkDetail(work.id, open);
  // What the sidebar already has: the recent runs. A group with none of them loaded is just its
  // title — no state, no mode.
  const loaded = runs.slice().sort((a, b) => a.created_at.localeCompare(b.created_at));
  const current = loaded[loaded.length - 1];
  const state = current ? runState(current.status) : undefined;
  const dot: Dot = !state ? undefined : state.bucket === "active" ? "live" : state.bucket === "needs_you" ? "warn" : state.tone === "danger" ? "danger" : undefined;
  // Its rounds are read when the fold opens; until they arrive the fold shows nothing, so a
  // partial set of loaded rounds is never numbered as if it were all of them.
  const rounds = detail ? detail.runs.slice().sort((a, b) => a.created_at.localeCompare(b.created_at)) : [];
  const count = detail?.runs.length ?? loaded.length;
  const href = `#/work/${encodeURIComponent(work.id)}`;
  // Where it works is only worth a tag when it is the surprising case (原地); a worktree run is
  // the usual one, and its word stays in the tooltip so a narrow row keeps the title readable.
  const mode = current ? modeWords(current) : undefined;
  const label = current ? t("sidebar.workTitle", { title: work.title, n: count }) : work.title;
  const who = lead ? displayName(lead) : undefined;
  return (
    <div className="work-row-wrap">
      <div className={`nav-row work-row ${active ? "active" : ""}`}>
        <button
          className="icon-btn work-fold"
          aria-expanded={open}
          aria-label={t(open ? "sidebar.collapseRounds" : "sidebar.expandRounds", { name: work.title })}
          onClick={() => setOpen(!open)}
        >
          <Icon name={open ? "chevron" : "chevronRight"} size={12} />
        </button>
        <a
          className="row-link"
          href={href}
          title={label + (who ? " · " + who : "") + (mode ? " · " + mode.label : "")}
          aria-current={active ? "page" : undefined}
        >
          {lead && (
            <span className="work-lead-face" aria-hidden="true">
              <Avatar agent={lead} size={16} />
            </span>
          )}
          <span className="row-main">
            <span className="row-label">{work.title}</span>
          </span>
          {current && leadsInPlace(current) && <RunModeTag run={current} compact className="row-mode" />}
          {count > 0 && <span className="work-round-badge">{t("sidebar.roundsCount", { n: count })}</span>}
        </a>
        {dot && <span className={`row-dot ${dot}`} aria-hidden="true" />}
      </div>
      {open && (
        <ul className="work-round-list">
          {rounds.map((round, i) => {
            const state = runState(round.status);
            return (
              <li key={round.id}>
                <a
                  className={`work-round-link ${selected === round.id ? "active" : ""}`}
                  href={`${href}/${encodeURIComponent(round.id)}`}
                  title={round.title}
                  aria-current={selected === round.id ? "true" : undefined}
                >
                  <span className="work-round-line">
                    <span className="work-round-n">{t("sidebar.roundN", { n: i + 1 })}</span>
                    <span className={`work-round-state ${state.tone}`}>{state.word}</span>
                  </span>
                  <span className="work-round-title">{round.title}</span>
                </a>
              </li>
            );
          })}
          {!rounds.length && detail && <li className="muted small work-round-empty">{t("sidebar.noRounds")}</li>}
        </ul>
      )}
    </div>
  );
}

/** A project's work groups, newest first: its three newest, and 「更多」 for the rest. */
function WorkList({ works, runs, agents, route, headed = true }: { works: Work[]; runs: RunDetail[]; agents: Agent[]; route: string[]; headed?: boolean }) {
  const [all, setAll] = useState(false);
  if (!works.length) return null;
  const shown = all ? works : works.slice(0, WORKS_PER_PROJECT);
  const hidden = works.length - WORKS_PER_PROJECT;
  return (
    <div className="project-works">
      {headed && (
        <div className="project-works-head">
          <span className="project-works-label">{t("sidebar.works")}</span>
          <span className="project-works-count">{works.length}</span>
        </div>
      )}
      {shown.map((work) => (
        <WorkRow
          key={work.id}
          work={work}
          lead={agents.find((a) => a.id === work.lead)}
          // What the sidebar has already loaded for this group; a row opens its own fold for the rest.
          runs={runs.filter((d) => d.run.work_id === work.id).map((d) => d.run)}
          active={route[0] === "work" && route[1] === work.id}
          selected={route[0] === "work" && route[1] === work.id ? route[2] : undefined}
        />
      ))}
      {/* One toggle, in place: it keeps keyboard focus where it was (see Capped). */}
      {hidden > 0 && (
        <button className="more" aria-expanded={all} onClick={() => setAll(!all)}>
          {all ? t("common.less") : t("sidebar.moreWorks", { n: hidden })}
        </button>
      )}
    </div>
  );
}

/** The work groups no project claims — folded at the end, so they are never dropped. */
function OtherWorks({ works, runs, agents, route }: { works: Work[]; runs: RunDetail[]; agents: Agent[]; route: string[] }) {
  const collapsed = useCollapsed();
  const closed = collapsed.has(OTHER_WORKS);
  if (!works.length) return null;
  return (
    <section className="group other-works">
      <div className="group-head">
        <button
          className="group-name"
          aria-expanded={!closed}
          aria-label={t(closed ? "sidebar.expandOther" : "sidebar.collapseOther")}
          onClick={() => toggleCollapsed(OTHER_WORKS)}
        >
          <Icon name={closed ? "chevronRight" : "chevron"} size={13} />
          {t("sidebar.other")}
        </button>
      </div>
      {!closed && <WorkList works={works} runs={runs} agents={agents} route={route} headed={false} />}
    </section>
  );
}

export function Sidebar({ route }: { route: string[] }) {
  const { list, tree, agents, loaded } = useWorkspaces();
  const nodes = useNodes();
  const questions = useQuestions();
  const pinned = usePinned();
  const order = projectOrder.use();
  const unread = useUnread();
  // What this hub has, and what it refused (docs/TODO.md §二): entries it did not list are
  // hidden, and one whose request came back 403 stays, greyed, with the hub's message.
  const git = useFeatureSupport("git");
  const lessons = useFeatureSupport("lessons");
  const work = useFeatureSupport("work");
  const [query, setQuery] = useState("");
  const [contentHits, setContentHits] = useState<SessionSearchHit[]>([]);
  const [contentSearching, setContentSearching] = useState(false);
  const everyone = agents.data ?? [];
  usePulseSeeds(everyone);
  const archiving = useArchiveNote();
  const runs = tree.data ?? [];
  const works = useWorks(work.show && !work.disabled);
  const workList = works.data ?? [];
  const searching = !!query.trim();

  // Cross-session full-text search across past messages & topics (debounced).
  useEffect(() => {
    const q = query.trim();
    if (q.length < 2) {
      setContentHits([]);
      setContentSearching(false);
      return;
    }
    let live = true;
    setContentSearching(true);
    const timer = window.setTimeout(() => {
      void api.searchSessions(q, { archived: true, limit: 15 }).then(
        (res) => {
          if (!live) return;
          setContentHits(res.hits);
          setContentSearching(false);
        },
        () => {
          if (!live) return;
          setContentHits([]);
          setContentSearching(false);
        },
      );
    }, 220);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [query]);
  // A run or work event can change a round a row shows: mark what was read stale, and reread the
  // details whose rounds are open — only those. The list itself is refreshed by `useWorks`.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const off = onHubEvent((e) => {
      if (!e.type.startsWith("run.") && !e.type.startsWith("work.")) return;
      clearTimeout(timer);
      timer = setTimeout(() => {
        workDetails.invalidate();
        void workDetails.refresh(api.getWork);
      }, 250);
    });
    return () => {
      clearTimeout(timer);
      off();
    };
  }, []);
  const todos = todoItems({
    items: questions.data?.items ?? [],
    runs,
    now: Date.now(),
  });
  const unsortedIssues = useUnsortedIssues(runs);
  const unboundProjects = useUnboundProjects(runs);
  // Pages an extension registered (`ca.pages`): each is one row here, and only while it loaded.
  const extensionPages = [...useExtensions().pages.values()];
  const [onlyActive, setOnlyActive] = useState(false);
  const inToolRoute = ["usage", "expertise", "git", "lessons", "extensions", "ext"].includes(route[0] ?? "");
  const [toolsOpen, setToolsOpen] = useState(false);
  const showTools = toolsOpen || inToolRoute;
  const sessionOrder = useSessionOrder();
  const [ctxMenu, setCtxMenu] = useState<{ agent: Agent; x: number; y: number } | null>(null);
  const [renameTarget, setRenameTarget] = useState<{ agent: Agent; value: string } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const flashToast = (msg: string) => {
    clearTimeout(toastTimer.current);
    setToast(msg);
    toastTimer.current = setTimeout(() => setToast(null), 3200);
  };
  useEffect(() => () => clearTimeout(toastTimer.current), []);
  const copyResume = async (agent: Agent) => {
    try {
      const res = await api.resumeCommand(agent.id);
      if (res.command) {
        await navigator.clipboard.writeText(res.command);
        flashToast(`已复制 ${displayName(agent)} 的终端恢复命令`);
      } else {
        flashToast(res.unavailable ?? "这个会话暂时没有终端恢复命令");
      }
    } catch (err) {
      flashToast(errorText(err));
    }
  };
  const submitRename = async () => {
    if (!renameTarget) return;
    const { agent, value } = renameTarget;
    setRenameTarget(null);
    try {
      await api.setTopic(agent.id, value.trim() || null);
      flashToast("已更新会话标题");
    } catch (err) {
      flashToast(errorText(err));
    }
  };
  const batchArchiveIdle = async (cwd: string) => {
    const key = wsKey(cwd);
    const targets = everyone.filter(
      (a) =>
        !a.archived_at &&
        wsKey(a.cwd) === key &&
        !isSession(a) &&
        !pinned.has(sessionKey(a)) &&
        a.status !== "running" &&
        deliveryOf(a.id)?.state !== "turn",
    );
    if (!targets.length) {
      flashToast("没有可归档的空闲会话");
      return;
    }
    await Promise.allSettled(targets.map((a) => api.archiveAgent(a.id, true)));
    flashToast(`已归档 ${targets.length} 个空闲会话`);
  };
  const remoteNodes = (nodes.data ?? []).filter((n) => !n.local);
  const remoteNodeMap = new Map(remoteNodes.map((n) => [n.id, n]));
  const allMachines = buildMachineGroups(
    everyone,
    runs,
    remoteNodes.map((n) => n.id),
    pinned,
    [...order],
    sessionOrder,
  );
  const groups = allMachines.flatMap((m) => m.groups);
  const isActiveMember = (m: SidebarMember) => {
    const state = deliveryOf(m.agent.id)?.state;
    const running = state ? state !== "idle" : m.agent.status === "running";
    return running || m.agent.status === "error" || !!unread[m.agent.id] || todos.some((item) => item.agent === m.agent.id);
  };
  const filteredMachines = onlyActive
    ? allMachines.map((m) => ({
        ...m,
        groups: m.groups.map((g) => ({ ...g, members: g.members.filter(isActiveMember) })).filter((g) => g.members.length > 0),
      }))
    : allMachines;
  const shownMachines = searchMachineGroups(filteredMachines, query);
  const shown = shownMachines.flatMap((m) => m.groups);
  // A work group belongs to the project its `repo` names on the centre machine; one no project claims goes last.
  const centreGroups = allMachines.find((m) => m.key === CENTRE_MACHINE)?.groups ?? [];
  const projectKeys = new Set(centreGroups.map((g) => g.key));
  const otherWorks = workList.filter((w) => !projectKeys.has(workProject(w)));
  const archived = everyone.filter((a) => a.archived_at);
  const isSession = (a: Agent) => route[0] === "s" && isAgent(a, route[1]);
  const reorderInGroup = (groupKey: string, currentIds: string[], fromId: string, toId: string, after: boolean) => {
    saveSessionOrder(groupKey, reorderIds(currentIds, fromId, toId, after));
  };
  const openRowMenu = (agent: Agent) => (e: ReactMouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu({ agent, x: Math.min(e.clientX, window.innerWidth - 200), y: Math.min(e.clientY, window.innerHeight - 220) });
  };
  const row = (member: SidebarMember, groupKey?: string, groupIds?: string[], nodeOffline?: boolean) => (
    <SessionRow
      key={member.agent.id}
      member={member}
      active={isSession(member.agent)}
      nodeOffline={nodeOffline}
      dragKey={groupKey ? { group: groupKey, id: member.agent.id } : undefined}
      onReorder={groupKey && groupIds ? (fromId, toId, after) => reorderInGroup(groupKey, groupIds, fromId, toId, after) : undefined}
      onMenu={openRowMenu(member.agent)}
    />
  );
  const pinnedMembers = [
    ...new Map(
      groups
        .flatMap((g) => g.members)
        .filter((m) => pinned.has(sessionKey(m.agent)))
        .map((m) => [m.agent.id, m]),
    ).values(),
  ];
  const pinnedRuns = runs.filter((d) => pinned.has("run:" + d.run.id));
  const done = Object.values(unread).filter((u) => u.kind === "done").length;
  return (
    <Confirmed.Provider value={loaded && !!questions.data && !agents.error && !tree.error && !questions.error}>
      <Needs.Provider value={todos}>
        <Runs.Provider value={runs}>
        <Archiving.Provider value={archiving.archive}>
        <nav className="sidebar flat-sidebar">
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
            <button
              type="button"
              className={`search-filter-btn ${onlyActive ? "on" : ""}`}
              onClick={(e) => {
                e.preventDefault();
                setOnlyActive((v) => !v);
              }}
              title={onlyActive ? "显示全部会话" : "仅看运行中 / 待处理 / 未读会话"}
              aria-pressed={onlyActive}
              aria-label="仅看活跃会话"
            >
              <Icon name="info" size={13} />
            </button>
          </label>
          <div className="side-nav-primary">
            <a className={"nav-new-full " + (!route[0] ? "active" : "")} href="#/" onClick={() => selectWorkspace("")}>
              <Icon name="plus" size={16} />
              <span>{t("sidebar.new")}</span>
              <kbd>Ctrl N</kbd>
            </a>
            <button
              type="button"
              className={`nav-tools-toggle ${inToolRoute ? "on" : ""}`}
              aria-expanded={showTools}
              onClick={() => setToolsOpen((v) => !v)}
            >
              <Icon name="sliders" size={14} />
              <span>更多工具</span>
              <span className="tools-chev">
                <Icon name={showTools ? "chevron" : "chevronRight"} size={12} />
              </span>
            </button>
            {showTools && (
              <div className="nav-tools-list">
                <a className={route[0] === "usage" ? "active" : ""} href="#/usage">
                  <Icon name="chart" size={14} />
                  <span>{t("nav.usage")}</span>
                </a>
                <a className={route[0] === "expertise" ? "active" : ""} href="#/expertise">
                  <Icon name="brain" size={14} />
                  <span>{t("nav.expertise")}</span>
                </a>
                {git.show && (
                  <a
                    className={route[0] === "git" ? "active" : ""}
                    href="#/git"
                    title={git.disabled ? git.reason : "一个项目的分支、提交与未提交的改动"}
                    aria-disabled={git.disabled || undefined}
                    style={git.disabled ? REFUSED : undefined}
                    onClick={git.disabled ? (e) => e.preventDefault() : undefined}
                  >
                    <Icon name="branch" size={14} />
                    <span>Git</span>
                  </a>
                )}
                {lessons.show && (
                  <a
                    className={route[0] === "lessons" ? "active" : ""}
                    href="#/lessons"
                    title={lessons.disabled ? lessons.reason : undefined}
                    aria-disabled={lessons.disabled || undefined}
                    style={lessons.disabled ? REFUSED : undefined}
                    onClick={lessons.disabled ? (e) => e.preventDefault() : undefined}
                  >
                    <Icon name="tasks" size={14} />
                    <span>{t("nav.lessons")}</span>
                  </a>
                )}
                <a className={route[0] === "extensions" || route[0] === "ext" ? "active" : ""} href="#/extensions">
                  <Icon name="panel" size={14} />
                  <span>扩展</span>
                </a>
              </div>
            )}
          </div>
          <div className="scroll">
            {extensionPages.length > 0 && (
              <section className="group ext-nav-pages">
                <div className="group-head static">
                  <span className="group-name">扩展页面</span>
                </div>
                {extensionPages.map((page) => {
                  const active = route[0] === "ext" && route[1] === page.extension && route[2] === page.id;
                  return (
                    <div key={`${page.extension}/${page.id}`} className={"nav-row " + (active ? "active" : "")}>
                      <a className="row-link" href={`#/ext/${encodeURIComponent(page.extension)}/${encodeURIComponent(page.id)}`} title={`${page.label} · 扩展 ${page.extension}`}>
                        <span className="row-lead">
                          <Icon name="panel" size={14} />
                        </span>
                        <span className="row-label">{page.label}</span>
                      </a>
                      <span className="row-tag">{page.extension}</span>
                    </div>
                  );
                })}
              </section>
            )}
            <NeedsYou items={[...todos, ...unsortedIssues, ...unboundProjects]} agents={everyone} />
            {done > 0 && (
              <div className="sidebar-unread-bar">
                <span>{t("sidebar.doneUnread", { n: done })}</span>
                <button type="button" className="link" onClick={markAllSeen}>
                  全部已读
                </button>
              </div>
            )}
            {!searching && work.show && work.disabled && (
              <div className="works-refused nav-row" aria-disabled="true" title={work.reason} style={REFUSED}>
                <span className="row-link">
                  <span className="row-label">{work.reason ?? "hub 不允许读取工作组"}</span>
                </span>
              </div>
            )}
            {!searching && (pinnedMembers.length > 0 || pinnedRuns.length > 0) && (
              <section className="group">
                <div className="group-head static">
                  <span className="group-name">{t("sidebar.pinned")}</span>
                </div>
                {pinnedMembers.map((m) => row(m))}
                {pinnedRuns.map((d) => (
                  <Row
                    key={d.run.id}
                    href={"#/runs/" + d.run.id}
                    active={route[0] === "runs" && route[1] === d.run.id}
                    title={d.run.title}
                    label={d.run.title}
                    dot={undefined}
                    pinKey={"run:" + d.run.id}
                  />
                ))}
              </section>
            )}
            {shownMachines.map((machine) => {
              const isRemote = machine.key !== CENTRE_MACHINE;
              const remoteNode = isRemote ? remoteNodeMap.get(machine.key) : undefined;
              const nodeOffline = isRemote && !(remoteNode?.online ?? false);
              return (
                <Machine key={machine.key} machine={machine} node={remoteNode} searching={searching}>
                  {machine.groups.map((g) => {
                    const isCentre = !isRemote;
                    const foldKey = isCentre ? g.key : `${machine.key}:${g.key}`;
                    const groupWorks = isCentre ? workList.filter((w) => workProject(w) === g.key) : [];
                    const workLeads = new Set(groupWorks.map((w) => w.lead));
                    const sessionMembers = searching
                      ? g.members
                      : g.members.filter((m) => !workLeads.has(m.agent.id) || isSession(m.agent) || pinned.has(sessionKey(m.agent)));
                    return (
                      <Group
                        key={foldKey}
                        group={g}
                        foldKey={foldKey}
                        machineKey={machine.key}
                        remote={isRemote}
                        searching={searching}
                        active={isCentre && route[0] === "w" && route[1] === g.key}
                        onMove={
                          g.key === CHATS || isRemote
                            ? undefined
                            : () => {
                                const keys = centreGroups.filter((x) => x.key !== CHATS).map((x) => x.key);
                                const i = keys.indexOf(g.key);
                                if (i > 0) [keys[i - 1], keys[i]] = [keys[i], keys[i - 1]];
                                projectOrder.replace(keys);
                              }
                        }
                      >
                        <Capped
                          items={sessionMembers}
                          keep={(m) => isSession(m.agent) || m.agent.status === "running" || m.agent.status === "error" || !!unread[m.agent.id] || todos.some((item) => item.agent === m.agent.id)}
                          searching={searching}
                        >
                          {(m) => row(m, foldKey, sessionMembers.map((x) => x.agent.id), nodeOffline)}
                        </Capped>
                        {!searching && isCentre && <WorkList works={groupWorks} runs={runs} agents={everyone} route={route} />}
                      </Group>
                    );
                  })}
                  {!searching && isRemote && machine.groups.length === 0 && (
                    <p className="machine-empty">{nodeOffline ? "节点离线" : "暂无会话 · 点右侧 + 新建"}</p>
                  )}
                  {!searching && !isRemote && <OtherWorks works={otherWorks} runs={runs} agents={everyone} route={route} />}
                </Machine>
              );
            })}
            {archived.length > 0 && <div hidden={searching}><Archived agents={archived} isSession={isSession} /></div>}
            {searching && query.trim().length >= 2 && (contentSearching || contentHits.length > 0) && (
              <section className="sidebar-search-hits" aria-label="跨会话消息搜索结果">
                <div className="sidebar-hits-head">
                  <span>消息内容命中</span>
                  <span className="muted tiny">
                    {contentSearching ? "检索中…" : `${contentHits.length} 条`}
                  </span>
                </div>
                {contentHits.map((hit, idx) => {
                  const href = hit.message_id
                    ? `#/s/${encodeURIComponent(hit.agent_id)}/m/${encodeURIComponent(hit.message_id)}`
                    : `#/s/${encodeURIComponent(hit.agent_id)}`;
                  const roleTag = hit.role === "user" ? "你" : hit.role === "topic" ? "主题" : "AI";
                  return (
                    <a
                      key={`${hit.agent_id}-${hit.seq}-${idx}`}
                      href={href}
                      className="sidebar-hit-row"
                      title={`${hit.agent_name} · ${hit.cwd}`}
                    >
                      <div className="sidebar-hit-top">
                        <span className="sidebar-hit-name">{hit.agent_name}</span>
                        <span className={`sidebar-hit-role ${hit.role}`}>{roleTag}</span>
                        {hit.archived && <span className="sidebar-hit-role archived">归档</span>}
                        <span className="spacer" />
                        <span className="muted tiny">{ago(hit.ts)}</span>
                      </div>
                      <div className="sidebar-hit-snippet">{hit.snippet}</div>
                    </a>
                  );
                })}
              </section>
            )}
            {searching && !shown.length && !contentSearching && !contentHits.length && <p className="sidebar-empty">{t("sidebar.noMatch", { query })}</p>}
            {!query && loaded && !groups.length && <p className="sidebar-empty">{t("sidebar.empty")}</p>}
          </div>
          {renameTarget && (
            <form
              className="rename-row"
              onSubmit={(e) => {
                e.preventDefault();
                void submitRename();
              }}
            >
              <input
                value={renameTarget.value}
                onChange={(e) => setRenameTarget({ ...renameTarget, value: e.target.value })}
                onKeyDown={(e) => e.key === "Escape" && setRenameTarget(null)}
                placeholder={`给 ${displayName(renameTarget.agent)} 起个标题…`}
                autoFocus
              />
              <button type="submit" className="primary small">
                保存
              </button>
              <button type="button" className="ghost small" onClick={() => setRenameTarget(null)}>
                取消
              </button>
            </form>
          )}
          {ctxMenu && (
            <>
              <div className="menu-scrim" onClick={() => setCtxMenu(null)} />
              <div className="context-menu" style={{ left: ctxMenu.x, top: ctxMenu.y }} role="menu">
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    setRenameTarget({ agent: ctxMenu.agent, value: ctxMenu.agent.topic ?? "" });
                    setCtxMenu(null);
                  }}
                >
                  <span className="menu-text">
                    <span className="menu-label">重命名标题…</span>
                  </span>
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    togglePinned(sessionKey(ctxMenu.agent));
                    setCtxMenu(null);
                  }}
                >
                  <span className="menu-text">
                    <span className="menu-label">{pinned.has(sessionKey(ctxMenu.agent)) ? "取消置顶" : "置顶会话"}</span>
                  </span>
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    openBeside(ctxMenu.agent.id);
                    setCtxMenu(null);
                  }}
                >
                  <span className="menu-text">
                    <span className="menu-label">{t("sidebar.openBeside")}</span>
                  </span>
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    void copyResume(ctxMenu.agent);
                    setCtxMenu(null);
                  }}
                >
                  <span className="menu-text">
                    <span className="menu-label">复制终端恢复命令</span>
                  </span>
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    archiving.archive(ctxMenu.agent);
                    setCtxMenu(null);
                  }}
                >
                  <span className="menu-text">
                    <span className="menu-label">{t("sidebar.archive")}</span>
                  </span>
                </button>
                <button
                  type="button"
                  className="menu-item"
                  onClick={() => {
                    void batchArchiveIdle(ctxMenu.agent.cwd);
                    setCtxMenu(null);
                  }}
                >
                  <span className="menu-text">
                    <span className="menu-label">归档本项目空闲会话</span>
                  </span>
                </button>
                {Object.keys(unread).length > 0 && (
                  <button
                    type="button"
                    className="menu-item"
                    onClick={() => {
                      markAllSeen();
                      setCtxMenu(null);
                    }}
                  >
                    <span className="menu-text">
                      <span className="menu-label">全部标为已读</span>
                    </span>
                  </button>
                )}
                {list.filter((w) => wsKey(w.path) !== wsKey(ctxMenu.agent.cwd) && w.agents.every((a) => agentMachineKey(a) === agentMachineKey(ctxMenu.agent))).length > 0 && (
                  <>
                    <div className="menu-head">移入项目</div>
                    {list
                      .filter((w) => wsKey(w.path) !== wsKey(ctxMenu.agent.cwd) && w.agents.every((a) => agentMachineKey(a) === agentMachineKey(ctxMenu.agent)))
                      .map((w) => (
                        <button
                          key={w.key}
                          type="button"
                          className="menu-item"
                          onClick={() => {
                            void api.setAgentCwd(ctxMenu.agent.id, w.path);
                            setCtxMenu(null);
                          }}
                        >
                          <span className="menu-text">
                            <span className="menu-label">{w.name}</span>
                            <span className="menu-sub">{w.path}</span>
                          </span>
                        </button>
                      ))}
                  </>
                )}
              </div>
            </>
          )}
          {toast && !archiving.note && (
            <div className="archive-note" role="status">
              <span className="archive-note-text">{toast}</span>
              <button className="icon-btn" onClick={() => setToast(null)} aria-label="关闭">
                <Icon name="close" size={12} />
              </button>
            </div>
          )}
          {archiving.note && (
            <div className={"archive-note " + (archiving.note.error ? "failed" : "")} role="status">
              <span className="archive-note-text">{archiving.note.error ?? t("sidebar.archivedOne", { name: displayName(archiving.note.agent) })}</span>
              {!archiving.note.error && (
                <button className="link" onClick={() => archiving.undo(archiving.note!.agent)}>
                  {t("sidebar.undo")}
                </button>
              )}
              <button className="icon-btn" onClick={archiving.close} aria-label={t("common.clear")}>
                <Icon name="close" size={12} />
              </button>
            </div>
          )}
          <div className="foot">
            <UpdateNotice />
            <DevHubNotice />
            <OldHubNotice />
            <SidebarFooter route={route} />
          </div>
        </nav>
        </Archiving.Provider>
        </Runs.Provider>
      </Needs.Provider>
    </Confirmed.Provider>
  );
}

function NeedsYou({ items, agents }: { items: TodoItem[]; agents: Agent[] }) {
  const collapsed = useCollapsed();
  const closed = collapsed.has("__needs_you");
  const [all, setAll] = useState(false);
  const visible = visibleTodos(items, { expanded: all });
  if (!items.length) return null;
  return (
    <section className="group needs-you-group">
      <div className="group-head">
        <button className="group-name" aria-expanded={!closed} onClick={() => toggleCollapsed("__needs_you")}>
          <Icon name={closed ? "chevronRight" : "chevron"} size={13} />
          {t("sidebar.needsYou", { n: items.length })}
        </button>
      </div>
      {!closed && (
        <>
          {visible.items.map((item) => {
            const agent = agents.find((a) => a.id === item.agent);
            const reason = item.reason || (!agent && item.href?.startsWith("#/s/") ? t("sidebar.missingAgent") : undefined);
            const who = agent ? displayName(agent) : t("sidebar.unknownAgent");
            const body = (
              <>
                {agent ? <Avatar agent={agent} size={20} /> : <strong className="todo-who">{who}</strong>}
                <span className="todo-summary">{item.summary}</span>
                <span className="todo-action">{actionWord(item.action)}</span>
              </>
            );
            return item.href && !reason ? (
              <a key={item.kind + item.id} className="todo-row" href={item.href} title={who + "：" + item.summary}>
                {body}
              </a>
            ) : (
              <div key={item.kind + item.id} className="todo-row unavailable" title={who + "：" + item.summary}>
                {body}
                <span className="todo-reason">{reason || t("sidebar.unavailable")}</span>
              </div>
            );
          })}
          {/* One toggle, in place: splitting it into 展开/收起 unmounts the focused button and
              drops keyboard focus to the page top when it changes. */}
          {items.length > 3 && (
            <button className="more" onClick={() => setAll(!all)} aria-expanded={all}>
              {all ? t("common.less") : t("sidebar.moreTodos", { n: visible.remaining })}
            </button>
          )}
        </>
      )}
    </section>
  );
}

function Group({
  group,
  foldKey,
  machineKey = CENTRE_MACHINE,
  remote = false,
  searching,
  active,
  onMove,
  children,
}: {
  group: SidebarGroup;
  foldKey?: string;
  machineKey?: string;
  remote?: boolean;
  searching: boolean;
  active: boolean;
  onMove?: () => void;
  children: ReactNode;
}) {
  const collapsed = useCollapsed();
  const key = foldKey ?? group.key;
  const closed = groupClosed(collapsed, key, searching);
  const name = group.key === CHATS ? t("sidebar.standalone") : group.name;
  const needs = useContext(Needs);
  const confirmed = useContext(Confirmed);
  const waits = (m: SidebarMember) => m.agent.status === "error" || needs.some((item) => item.agent === m.agent.id);
  const waiting = group.members.filter(waits);
  const busy = group.members.filter((m) => {
    // "忙" is the hub's delivery state (P1-4); before it has said anything about this session,
    // its process status is the only hint there is.
    const state = deliveryOf(m.agent.id)?.state;
    return state ? state !== "idle" : m.agent.status === "running";
  });
  // Folded, the faces say who is inside: whoever wants attention first.
  const faces = [...waiting, ...busy, ...group.members].filter((m, i, all) => all.indexOf(m) === i).slice(0, 3);
  return (
    <section className="group">
      <div className={"group-head project-head " + (active ? "active" : "")}>
        <button
          className="icon-btn"
          aria-label={t(closed ? "sidebar.expandProject" : "sidebar.collapseProject", { name })}
          aria-expanded={!closed}
          disabled={searching}
          onClick={() => toggleCollapsed(key)}
        >
          <span className="project-glyph">
            <Icon name={group.key === CHATS ? "chat" : "folder"} size={14} />
          </span>
          <span className="project-fold">
            <Icon name={closed ? "chevronRight" : "chevron"} size={13} />
          </span>
        </button>
        {group.key === CHATS || remote ? (
          <button type="button" className="group-name project-name" title={name} onClick={() => !searching && toggleCollapsed(key)}>
            {name}
          </button>
        ) : (
          <a className="group-name project-name" title={name} href={wsHref(group.key)} aria-current={active ? "page" : undefined}>
            {name}
          </a>
        )}
        <span className="project-stat">
          {closed && (
            <span className="face-pile" aria-hidden="true">
              {faces.map((m) => (
                <Avatar key={m.agent.id} agent={m.agent} size={16} />
              ))}
            </span>
          )}
          {confirmed && waiting.length > 0 && (
            <span className="stat-waiting" title={t("sidebar.needsAction", { n: waiting.length })}>
              <Icon name="info" size={11} />
              {waiting.length}
            </span>
          )}
          {confirmed && busy.length > 0 && (
            <span className="stat-busy" title={`${busy.length} ${t("sidebar.working")}`}>
              <i className="working-dot" />
              {busy.length}
            </span>
          )}
        </span>
        <span className="project-actions">
          {onMove && (
            <button className="icon-btn group-order" onClick={onMove} title={t("sidebar.moveUp")} aria-label={t("sidebar.moveUp")}>
              ↑
            </button>
          )}
          <a
            className="icon-btn group-add"
            href="#/"
            onClick={() => {
              selectNode(remote ? machineKey : "");
              selectWorkspace(group.key === CHATS ? "" : group.key);
              startNew(remote || group.key === CHATS ? "chat" : "run");
            }}
            title={t("sidebar.newIn", { name })}
            aria-label={t("sidebar.newIn", { name })}
          >
            <Icon name="plus" size={14} />
          </a>
        </span>
      </div>
      {!closed && children}
    </section>
  );
}

const CAP = 5;
function Capped<T>({
  items,
  keep,
  searching,
  children,
}: {
  items: T[];
  keep: (item: T) => boolean;
  searching: boolean;
  children: (item: T) => ReactNode;
}) {
  const [all, setAll] = useState(false);
  const shown = capped(items, keep, CAP, all || searching);
  // Whether folding to CAP actually hides any row; a list whose rows are all kept shows none.
  const collapsible = capped(items, keep, CAP, false).length < items.length;
  return (
    <>
      {shown.map(children)}
      {/* One toggle, in place: splitting it into 更多/收起 unmounts the focused button and
          drops keyboard focus to the page top when it changes. */}
      {!searching && collapsible && (
        <button className="more" onClick={() => setAll(!all)} aria-expanded={all}>
          {all ? t("common.less") : t("common.more", { n: items.length - shown.length })}
        </button>
      )}
    </>
  );
}

type Dot = "live" | "warn" | "danger" | "unread" | undefined;
function SessionRow({
  member,
  active,
  nodeOffline,
  dragKey,
  onReorder,
  onMenu,
}: {
  member: SidebarMember;
  active: boolean;
  nodeOffline?: boolean;
  dragKey?: { group: string; id: string };
  onReorder?: (fromId: string, toId: string, after: boolean) => void;
  onMenu?: (e: ReactMouseEvent) => void;
}) {
  const { agent } = member;
  const doing = useActivity(agent.id);
  const unread = useUnread()[agent.id];
  const connection = useConnectionStatus();
  const waitingOn = useContext(Needs).filter((item) => item.agent === agent.id);
  const needs = waitingOn.length;
  const confirmed = useContext(Confirmed);
  const uncertain = connection !== "connected" || !confirmed || !agent.status;
  // The hub's delivery state is the one word for "busy" (P1-4); `status` is the process's.
  const delivery = useDelivery(agent.id);
  const running = !uncertain && (delivery ? delivery.state === "turn" : agent.status === "running");
  const error = !uncertain && agent.status === "error";
  const status = uncertain
    ? t(connection === "connecting" ? "sidebar.connecting" : "sidebar.unknownStatus")
    : needs
      ? t("sidebar.needsAction", { n: needs })
      : error
        ? t("state.error")
        : delivery?.state === "starting"
          ? "启动中"
          : delivery?.state === "held"
            ? "等它来取"
            : running
              ? t("sidebar.working")
              : nodeOffline
                ? "节点离线"
                : agent.status === "stopped"
                  ? t("sidebar.stopped")
                  : t("sidebar.idle");
  const turn = useRowLive(agent);
  const summary = summaryText(member.summary) || summaryText(agent.topic);
  const fresh = unread?.kind === "done";
  const failed = unread?.kind === "error";
  const flagged = !uncertain && (needs > 0 || error || fresh || failed);
  // Nothing to do here: the row stays quiet, the tooltip still says which state it is.
  const quiet = !uncertain && !needs && !error && !running;
  // Only news earns a second line: the rest is one small row, so what is alive stands out by size.
  const compact = quiet && !fresh && !failed;
  const newest = turn?.items.at(-1);
  const step = running && newest?.type === "tool_call" ? stepLine(newest) : "";
  // The second line is the row's latest news: what it asks, what went wrong, what it does now.
  const line = !uncertain && needs ? waitingOn[0].summary : error ? lastError(agent.id) || summary || status : running ? step || summaryText(doing) || summary || status : summary || status;
  const tone = uncertain ? "" : needs ? "attention" : error || failed ? "danger" : "";
  const runs = useContext(Runs);
  const work = useMemo(() => sessionWork(agent.id, runs), [agent.id, runs]);
  const plan = work.lead?.tasks ?? [];
  // Where the run it leads works: one small glyph, since the name has to stay readable here.
  const mode = work.lead ? modeWords(work.lead.run) : undefined;
  // A turn that just ended lights its row once: finishing is worth noticing, not worth keeping.
  const wasRunning = useRef(running);
  const [justDone, setJustDone] = useState(false);
  useEffect(() => {
    const ended = wasRunning.current && !running && !uncertain && !error;
    wasRunning.current = running;
    if (!ended) return;
    setJustDone(true);
    const timer = setTimeout(() => setJustDone(false), DONE_FLASH);
    return () => clearTimeout(timer);
  }, [running, uncertain, error]);
  const peek: PeekFacts = {
    agent,
    status: uncertain ? status : fresh || failed ? status + " · " + t("sidebar.unread") : status,
    summary,
    waiting: uncertain ? [] : waitingOn,
    error: !uncertain && error ? lastError(agent.id) : undefined,
    turn: running ? turn : undefined,
    work,
  };
  return (
    <Row
      href={sessionHref(agent)}
      active={active}
      dim={compact && (agent.status === "stopped" || !!nodeOffline)}
      compact={compact}
      flash={justDone}
      peek={peek}
      title={
        sessionLabel(agent) +
        " · " +
        status +
        (relationText(agent) ? " · " + relationText(agent) : "") +
        (fresh || failed ? " · " + t("sidebar.unread") : "") +
        (agent.model ? " · " + agent.model : "") +
        (mode ? " · " + mode.label : "") +
        "\n" +
        (summary || t("sidebar.noActivity")) +
        (mode ? "\n" + mode.title : "") +
        "\n" +
        t("sidebar.besideHint")
      }
      dot={undefined}
      pinKey={sessionKey(agent)}
      beside={agent.id}
      archive={agent}
      dragKey={dragKey}
      onReorder={onReorder}
      onMenu={onMenu}
      lead={
        <span className={"face " + (running ? "running" : "")}>
          <Avatar agent={agent} size={compact ? 18 : 24} />
          {!uncertain && needs > 0 && <span className="face-badge warn">{needs}</span>}
          {!uncertain && !needs && (error || failed) && <span className="face-badge danger">!</span>}
          {!uncertain && !needs && !error && fresh && <span className="face-badge fresh" />}
        </span>
      }
      label={
        <span className="member-top">
          <span className={"member-name " + (flagged ? "strong" : "")}>{sessionLabel(agent)}</span>
          <ModelMark plugin={agent.plugin} model={agent.model} />
          {typeof (agent.config as Record<string, unknown> | null)?.worktree_branch === "string" && (
            <span className="member-branch" title={`Worktree 分支：${String((agent.config as Record<string, unknown>).worktree_branch)}`}>
              {String((agent.config as Record<string, unknown>).worktree_branch).replace(/^ca\//, "")}
            </span>
          )}
          {work.lead && <RunModeMark run={work.lead.run} className="row-mode" />}
          {compact ? <span className="member-inline" data-crawl="">{summary}</span> : <span className="member-gap" />}
          {plan.length > 0 && !running && !(!uncertain && needs > 0) && <span className="member-count">{plan.filter((task) => task.status === "merged").length}/{plan.length}</span>}
          {quiet && !plan.length && member.lastAt && <span className="member-time" title={ago(member.lastAt)}>{shortSince(member.lastAt, Date.now())}</span>}
          {!uncertain && needs > 0 && <time className="member-time attention" title={ago(waitingOn[0].since)}>{[actionWord(waitingOn[0].action), shortSince(waitingOn[0].since, Date.now())].filter(Boolean).join(" · ")}</time>}
          {running && <RowElapsed agentId={agent.id} since={turn?.since} at={turn?.at} items={turn?.items ?? []} />}
          {uncertain && <span className="member-status quiet">{status}</span>}
        </span>
      }
      live={
        <>
          {!compact && (
            <span className={"member-summary " + tone + (step ? " step" : "") + (running ? " running" : "")}>
              {running && <span className="step-mark">⎿</span>}
              <span className="member-summary-text" data-crawl="">{line}</span>
            </span>
          )}
          {plan.length > 0 && <TaskSegments tasks={plan} />}
        </>
      }
    />
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
              <button
                className="icon-btn"
                onClick={() => void api.archiveAgent(a.id, false)}
                title={t("sidebar.restore")}
                aria-label={t("sidebar.restore")}
              >
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
  compact,
  flash,
  peek,
  title,
  lead,
  label,
  live,
  tag,
  after,
  dot,
  pinKey,
  beside,
  archive,
  dragKey,
  onReorder,
  onMenu,
}: {
  href: string;
  active: boolean;
  dim?: boolean;
  compact?: boolean;
  /** Its turn just ended. */
  flash?: boolean;
  /** What the hover card says; the card then stands in for the tooltip. */
  peek?: PeekFacts;
  title: string;
  lead?: ReactNode;
  label: ReactNode;
  /** What a running turn does now, kept visible after the title truncates (its elapsed time). */
  live?: ReactNode;
  /** A short label kept visible after `label`, which truncates first (a conversation's project). */
  tag?: ReactNode;
  after?: ReactNode;
  dot: Dot;
  pinKey?: string;
  beside?: string;
  /** The conversation this row archives from its hover actions. */
  archive?: Agent;
  /** Drag-and-drop identity within a sidebar group. */
  dragKey?: { group: string; id: string };
  onReorder?: (fromId: string, toId: string, after: boolean) => void;
  /** The right-click menu of this row. */
  onMenu?: (e: ReactMouseEvent) => void;
}) {
  const pinned = usePinned();
  const archiveRow = useContext(Archiving);
  const isPinned = !!pinKey && pinned.has(pinKey);
  const openAside = (e: ReactMouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (beside) openBeside(beside);
  };
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const hidePeek = () => {
    clearTimeout(timer.current);
    setAnchor(null);
  };
  const showPeek = (el: HTMLElement, delay: number) => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setAnchor(el.getBoundingClientRect()), delay);
  };
  const crawlTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const uncrawl = useRef<() => void>(undefined);
  const stopCrawl = () => {
    clearTimeout(crawlTimer.current);
    uncrawl.current?.();
    uncrawl.current = undefined;
  };
  useEffect(
    () => () => {
      clearTimeout(timer.current);
      stopCrawl();
    },
    [],
  );
  const hover = peek && {
    onMouseEnter: (e: ReactMouseEvent<HTMLDivElement>) => {
      const row = e.currentTarget;
      showPeek(row, PEEK_DELAY);
      stopCrawl();
      crawlTimer.current = setTimeout(() => (uncrawl.current = crawl(row)), CRAWL_DELAY);
    },
    onMouseLeave: () => {
      hidePeek();
      stopCrawl();
    },
    onMouseDown: hidePeek,
    onFocus: (e: ReactFocusEvent<HTMLDivElement>) => e.target.matches(":focus-visible") && showPeek(e.currentTarget, 0),
    onBlur: hidePeek,
  };
  const [dragging, setDragging] = useState(false);
  const [dropPos, setDropPos] = useState<"before" | "after" | null>(null);
  const dnd = dragKey
    ? {
        draggable: true,
        onDragStart: (e: ReactDragEvent<HTMLDivElement>) => {
          hidePeek();
          draggingSession = dragKey;
          setDragging(true);
          e.dataTransfer.effectAllowed = "move";
          e.dataTransfer.setData("text/plain", dragKey.id);
        },
        onDragEnd: () => {
          draggingSession = null;
          setDragging(false);
          setDropPos(null);
        },
        onDragOver: (e: ReactDragEvent<HTMLDivElement>) => {
          if (!draggingSession || draggingSession.group !== dragKey.group || draggingSession.id === dragKey.id) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          const rect = e.currentTarget.getBoundingClientRect();
          setDropPos(e.clientY > rect.top + rect.height / 2 ? "after" : "before");
        },
        onDragLeave: () => setDropPos(null),
        onDrop: (e: ReactDragEvent<HTMLDivElement>) => {
          if (!draggingSession || draggingSession.group !== dragKey.group || draggingSession.id === dragKey.id) return;
          e.preventDefault();
          const rect = e.currentTarget.getBoundingClientRect();
          const after = e.clientY > rect.top + rect.height / 2;
          onReorder?.(draggingSession.id, dragKey.id, after);
          draggingSession = null;
          setDropPos(null);
        },
      }
    : undefined;
  return (
    <div
      className={`nav-row ${active ? "active" : ""} ${dim ? "dim" : ""} ${compact ? "compact" : ""} ${flash ? "just-done" : ""} ${dragging ? "dragging" : ""} ${dropPos ? `drag-over-${dropPos}` : ""}`}
      {...hover}
      {...dnd}
    >
      {peek && anchor && <SessionPeek anchor={anchor} facts={peek} />}
      <a
        className="row-link"
        href={href}
        title={peek ? undefined : title}
        aria-label={peek ? title : undefined}
        onClick={(e) => beside && (e.ctrlKey || e.metaKey) && openAside(e)}
        onMouseDown={(e) => e.button === 1 && beside && e.preventDefault()}
        onAuxClick={(e) => e.button === 1 && beside && openAside(e)}
        onContextMenu={onMenu}
      >
        {lead && <span className="row-lead">{lead}</span>}
        <span className="row-main">
          <span className="row-label">{label}</span>
          {live}
        </span>
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
              archiveRow(archive);
            }}
            title={t("sidebar.archive")}
            aria-label={t("sidebar.archive")}
          >
            <Icon name="archive" size={13} />
          </button>
        )}
        {pinKey && (
          <button
            className={`icon-btn ${isPinned ? "on" : ""}`}
            onClick={() => togglePinned(pinKey)}
            title={t(isPinned ? "sidebar.unpin" : "sidebar.pin")}
            aria-label={t(isPinned ? "sidebar.unpin" : "sidebar.pin")}
          >
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
    replace(items: string[]) {
      value = new Set(items);
      try {
        localStorage.setItem(key, JSON.stringify(items));
      } catch {}
      listeners.forEach((l) => l());
    },
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

const projectOrder = storedSet("crossagents.projectOrder");
const pins = storedSet("crossagents.pinned");
const collapsedGroups = storedSet("crossagents.collapsed");
const usePinned = pins.use;
const togglePinned = pins.toggle;
const useCollapsed = collapsedGroups.use;
const toggleCollapsed = collapsedGroups.toggle;

/** Compact user profile footer card + grouped quota popover. */
function SidebarFooter({ route }: { route: string[] }) {
  const profile = useChatProfile();
  const status = useConnectionStatus();
  const conn = useConnectionState();
  const [info, setInfo] = useState<HubInfo>();
  useEffect(() => {
    if (status === "connected") api.info().then(setInfo, () => {});
  }, [status, conn?.mode]);

  const statusTone = status === "connected" ? "ok" : status === "connecting" ? "warn" : "danger";
  const statusText =
    status === "connected"
      ? t("hub.connected")
      : status === "connecting"
        ? t("hub.connecting")
        : t("hub.offline", { reason: status.replace(/^offline: /, "") });

  const host = info?.machine?.hostname;
  const remote = conn?.mode === "remote";
  const customName = profile.name.trim();
  const displayTitle = customName || host || "本机";
  const where = remote ? "远程" : customName ? host || "本机" : undefined;

  const cardTitle = [
    displayTitle,
    statusText,
    info?.version ? `v${info.version}` : undefined,
    info?.home,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <div className="foot-user-wrap">
      <div className="foot-user">
        <a className="foot-profile" href="#/settings" title={cardTitle}>
          <span className="foot-avatar-wrap">
            <ChatAvatar image={profile.avatar} size={30} />
            <span
              className={`foot-status-dot ${statusTone} ${status === "connecting" ? "pulse" : ""}`}
              role="img"
              aria-label={statusText}
              title={statusText}
            />
          </span>
          <span className="foot-identity">
            <span className="foot-name">{displayTitle}</span>
            <span className="foot-meta">
              {status !== "connected" ? (
                <span className={`foot-meta-state ${statusTone}`}>{statusText}</span>
              ) : (
                <>
                  {where && <span className="foot-meta-where">{where}</span>}
                  {info && <Versions hub={info} />}
                </>
              )}
            </span>
          </span>
        </a>
        <div className="foot-actions">
          <QuotaPill />
          <a
            className={"icon-btn foot-settings-btn " + (route[0] === "settings" ? "on" : "")}
            href="#/settings"
            title={t("nav.settings")}
            aria-label={t("nav.settings")}
          >
            <Icon name="settings" size={15} />
          </a>
        </div>
      </div>
    </div>
  );
}

/** Compact quota pill (`4/8`) with a hover/focus popover grouped by vendor. */
function QuotaPill() {
  const quota = useQuota();
  const accounts = quota.data?.accounts ?? [];
  if (!accounts.length) return null;

  const cells = accounts.map((a) => {
    const top = tightest([{ ...a, windows: a.status.rate_limits }])?.window;
    const used = top ? Math.max(0, Math.min(100, Math.round(top.used_percent))) : 0;
    const unavailable = !a.status.logged_in || a.at_risk || a.egress_block || a.status.limit_reached || used >= 100;
    const tone = unavailable ? "danger" : top ? level(top.used_percent, a.status.limit_reached) : "ok";
    const rank = tone === "ok" ? 0 : tone === "warn" ? 1 : 2;
    return { a, top, used, tone, unavailable, rank };
  });

  const okCount = cells.filter((c) => !c.unavailable).length;
  const pillTone =
    okCount === 0
      ? "danger"
      : okCount < accounts.length || cells.some((c) => c.tone === "warn")
        ? "warn"
        : "ok";

  // Group by provider in stable order, sorting healthy accounts before exhausted/unavailable ones.
  const byVendor = new Map<string, typeof cells>();
  for (const c of cells) {
    const list = byVendor.get(c.a.account.provider) ?? [];
    list.push(c);
    byVendor.set(c.a.account.provider, list);
  }
  const vendorGroups = [...byVendor.entries()].map(([provider, list]) => {
    const sorted = [...list].sort((x, y) => x.rank - y.rank || x.used - y.used);
    const ok = sorted.filter((c) => !c.unavailable).length;
    return { provider, label: vendorLabel(provider), items: sorted, ok, total: sorted.length };
  });

  return (
    <div className="foot-quota-wrap">
      <a
        className={`foot-quota-pill ${pillTone}`}
        href="#/usage"
        title={t("quota.fleet", { ok: okCount, total: accounts.length })}
        aria-label={t("quota.fleet", { ok: okCount, total: accounts.length })}
      >
        <span className={`dot ${pillTone}`} aria-hidden="true" />
        <span className="foot-quota-count">
          {okCount}/{accounts.length}
        </span>
      </a>
      <div className="quota-peek" role="region" aria-label={t("quota.peekTitle")}>
        <div className="quota-peek-head">
          <span>账号号池</span>
          <span className="quota-peek-head-right">
            <span className="quota-peek-summary">
              {okCount}/{accounts.length} 可用
            </span>
            <a className="quota-peek-manage" href="#/usage">
              管理
            </a>
          </span>
        </div>
        <div className="quota-peek-groups">
          {vendorGroups.map((g) => (
            <div key={g.provider} className="quota-peek-group">
              <div className="quota-peek-vendor">
                <VendorMark plugin={g.provider} size={12} />
                <span className="quota-peek-vendor-name">{g.label}</span>
                <span className={`quota-peek-vendor-count ${g.ok === 0 ? "danger" : g.ok < g.total ? "warn" : "ok"}`}>
                  {g.ok}/{g.total}
                </span>
              </div>
              <div className="quota-peek-list">
                {g.items.map(({ a, top, used, tone, unavailable }) => {
                  const statusLabel = a.at_risk
                    ? t("quota.atRisk")
                    : a.egress_block
                      ? "出口异常"
                      : !a.status.logged_in
                        ? t("quota.unlogged")
                        : a.status.limit_reached || used >= 100
                          ? "已耗尽"
                          : top
                            ? `已用 ${used}%`
                            : "充足";
                  const reset = top?.resets_at ? resetText(top.resets_at) : undefined;
                  return (
                    <div key={a.account.id} className={"quota-peek-item" + (unavailable ? " is-down" : "")}>
                      <div className="quota-peek-row">
                        <span className={`dot ${tone}`} aria-hidden="true" />
                        <span className="quota-peek-name">{a.account.label || a.account.id}</span>
                        <span className={`quota-peek-val ${tone}`}>{statusLabel}</span>
                      </div>
                      {(a.agents.length > 0 || reset) && (
                        <div className="quota-peek-sub">
                          {a.agents.length > 0 && (
                            <span className="quota-peek-agents">{a.agents.map((ag) => `@${ag}`).join(" ")}</span>
                          )}
                          {reset && <span className="quota-peek-reset">{reset}</span>}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

/**
 * The installed GUI found a development hub (`hub start` from `target/`) where its own should
 * run: it leaves it alone, so say so, or the page and the backend quietly disagree on version.
 * In remote mode none of this applies: the hub is elsewhere and the GUI never manages it.
 */
function DevHubNotice() {
  const status = useConnectionStatus();
  const connection = useConnectionState();
  const remote = connection?.mode === "remote";
  const [dev, setDev] = useState<DevHub | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (status === "connected" && !remote) window.crossagents?.devHub?.().then(setDev, () => {});
  }, [status, remote]);
  if (!dev || remote) return null;
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

/**
 * A hub built before `HubInfo.features`: its entries would just be missing, so say which — and
 * that the version is what says so, since 0.2.6 already has some of them — and how to get the
 * rest. Reloading reconnects and asks `/v1/info` again, which is also how a refused entry gets
 * another try.
 */
function OldHubNotice() {
  const features = useHubFeatures();
  const hidden = hiddenByHub(features);
  if (features.listed !== null || !hidden.length) return null;
  const names = hidden.map((f) => FEATURE_LABELS[f]).join("、");
  const version = features.version ?? "未知";
  return (
    <div
      className="dev-hub"
      title={`旧版 hub 的 /v1/info 不带功能清单，按版本 ${version} 推断这些入口不可用：${names}。换成同一版本的新 hub 后会自己出现。`}
    >
      <span>旧版 hub：按版本推断 {names} 不可用</span>
      <button className="link" onClick={() => window.location.reload()}>
        重新连接
      </button>
    </div>
  );
}

/**
 * A machine and the workspaces on it: the centre hub machine plus any connected (or known
 * offline) remote execution nodes. Folding is remembered per machine; hovering offers `+`
 * to start a new Chat directly on that machine.
 */
function Machine({
  machine,
  node,
  searching,
  children,
}: {
  machine: SidebarMachine;
  node?: RemoteNodeInfo;
  searching: boolean;
  children: ReactNode;
}) {
  const isCentre = machine.key === CENTRE_MACHINE;
  const state = useConnectionState();
  const status = useConnectionStatus();
  const [info, setInfo] = useState<HubInfo>();
  useEffect(() => {
    if (isCentre && status === "connected") api.info().then(setInfo, () => {});
  }, [isCentre, status, state?.mode]);
  const collapsed = useCollapsed();
  const foldKey = machineFoldKey(machine.key);
  const closed = groupClosed(collapsed, foldKey, searching);

  if (!isCentre) {
    const online = node?.online ?? false;
    const os = normalizeNodeOs(node?.os);
    const name = node?.name || machine.key;
    const installed = (node?.plugins ?? []).filter((p) => p.installed).map((p) => p.name);
    const title = [
      node?.hostname && node.hostname !== name ? `${name} (${node.hostname})` : name,
      online ? "在线节点" : "离线节点",
      node?.os && node.os !== "unknown" ? (node.arch ? `${node.os}/${node.arch}` : node.os) : undefined,
      installed.length ? `可用 CLI: ${installed.join(", ")}` : undefined,
      node?.default_cwd || undefined,
    ]
      .filter(Boolean)
      .join(" · ");
    return (
      <section className="group machine">
        <div className="group-head machine-head">
          <button
            className="icon-btn"
            aria-label={`${closed ? "展开" : "折叠"}机器 ${name}`}
            aria-expanded={!closed}
            disabled={searching}
            onClick={() => toggleCollapsed(foldKey)}
          >
            <Icon name={closed ? "chevronRight" : "chevron"} size={13} />
          </button>
          <a className={`machine-name conn-where ${online ? "ok" : "muted"}`} href="#/settings/devices" title={title}>
            <OsIcon os={os} size={14} />
            <span className="text">{name}</span>
            {!online && <span className="machine-via">离线</span>}
            <span className={`dot ${online ? "ok" : "off"}`} />
          </a>
          {online && (
            <a
              className="icon-btn machine-add"
              href="#/"
              onClick={() => {
                selectNode(machine.key);
                selectWorkspace("");
                startNew("chat");
              }}
              title={`在 ${name} 上新建会话`}
              aria-label={`在 ${name} 上新建会话`}
            >
              <Icon name="plus" size={13} />
            </a>
          )}
        </div>
        {!closed && <div className="machine-body">{children}</div>}
      </section>
    );
  }

  // A plain browser tab has no connection bridge: the mark then follows the hub connection alone.
  const words = state ? markWords(state) : undefined;
  const tone = words?.tone ?? (status === "connected" ? "ok" : status === "connecting" ? "warn" : "danger");
  const host = info?.machine?.hostname;
  const remote = state?.mode === "remote";
  const name = host || (remote ? words?.text : undefined) || "本机";
  const title = [host, words?.title ?? (status === "connected" ? "已连接" : "未连接")].filter(Boolean).join(" · ");
  return (
    <section className="group machine">
      <div className="group-head machine-head">
        <button
          className="icon-btn"
          aria-label={`${closed ? "展开" : "折叠"}机器 ${name}`}
          aria-expanded={!closed}
          disabled={searching}
          onClick={() => toggleCollapsed(foldKey)}
        >
          <Icon name={closed ? "chevronRight" : "chevron"} size={13} />
        </button>
        <a className={`machine-name conn-where ${tone}`} href="#/settings/connection" title={title}>
          <OsIcon os={info?.machine?.os} size={14} />
          <span className="text">{name}</span>
          {remote && host && <span className="machine-via">中心</span>}
          <span className={`dot ${tone === "muted" ? "off" : tone}`} />
        </a>
        <a
          className="icon-btn machine-add"
          href="#/"
          onClick={() => {
            selectNode("");
            selectWorkspace("");
            startNew("chat");
          }}
          title={`在 ${name} 上新建会话`}
          aria-label={`在 ${name} 上新建会话`}
        >
          <Icon name="plus" size={13} />
        </a>
      </div>
      {!closed && <div className="machine-body">{children}</div>}
    </section>
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
  const features = useHubFeatures();
  const why = missingText(features);
  const title = [
    t("build.page", {
      tag: buildTag(page),
      dev: page.dev ? t("build.dev") : "",
    }),
    t("build.hub", {
      tag: buildTag(hub),
      dev: hub.debug ? t("build.dev") : "",
    }),
    ...(why ? [why] : []),
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

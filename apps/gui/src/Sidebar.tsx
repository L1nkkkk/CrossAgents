// Flat project/member navigation. Domain grouping and human work live in pure views.
import { createContext, useContext, useEffect, useMemo, useRef, useState, useSyncExternalStore, type FocusEvent as ReactFocusEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { HubInfo } from "@bindings/HubInfo";
import type { RunDetail } from "@bindings/RunDetail";
import { deliveryOf, lastError, useActivity, useDelivery, useUnread } from "./activity";
import { api, errorText } from "./api";
import type { DevHub } from "./bridge";
import { buildTag, compareBuilds } from "./lib/build";
import { useUpdateState } from "./lib/updates";
import { Avatar, BrandMark, Icon, ModelMark, ago, displayName, isAgent, sessionHref } from "./components";
import { useConnectionStatus } from "./hub";
import { RowElapsed, usePulseSeeds, useRowLive } from "./liveTurn";
import { t } from "./lib/i18n";
import { sessionWork } from "./lib/projectView";
import { stepLine } from "./lib/workGroups";
import { SessionPeek, TaskSegments, type PeekFacts } from "./SessionPeek";
import { capped, sidebarGroups, searchGroups, groupClosed, shortSince, summaryText, type SidebarGroup, type SidebarMember } from "./lib/sidebarTree";
import { todoItems, visibleTodos, type TodoItem } from "./lib/todos";
import { useUnsortedIssues } from "./lessons/unsorted";
import { useQuestions } from "./questions";
import { startNew } from "./menubar";
import { level, tightest, windowLabel } from "./lib/quota";
import { openBeside } from "./panes";
import { useExtensions } from "./extensions/defaults";
import { useQuota } from "./views/Quota";
import { CHATS, selectWorkspace, useWorkspaces, wsHref } from "./workspace";

const Confirmed = createContext(true);
const Needs = createContext<readonly TodoItem[]>([]);
const Runs = createContext<readonly RunDetail[]>([]);
/** How long a row stays lit after its turn ends. */
const DONE_FLASH = 1800;
/** A pointer passing through the list shows no card. */
const PEEK_DELAY = 350;
const CRAWL_DELAY = 200;
const CRAWL_PX_PER_MS = 0.04;
/** Clipped by less than this, moving reads as jitter, not as showing more. */
const CRAWL_MIN_PX = 8;
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
const labelOf = (a: Agent) => displayName(a);

export function Sidebar({ route }: { route: string[] }) {
  const { list, tree, agents, loaded } = useWorkspaces();
  const questions = useQuestions();
  const pinned = usePinned();
  const order = projectOrder.use();
  const unread = useUnread();
  const [query, setQuery] = useState("");
  const everyone = agents.data ?? [];
  usePulseSeeds(everyone);
  const archiving = useArchiveNote();
  const runs = tree.data ?? [];
  const todos = todoItems({
    items: questions.data?.items ?? [],
    runs,
    now: Date.now(),
  });
  const unsortedIssues = useUnsortedIssues(runs);
  // Pages an extension registered (`ca.pages`): each is one row here, and only while it loaded.
  const extensionPages = [...useExtensions().pages.values()];
  const groups = sidebarGroups(everyone, runs, list, pinned, [...order]);
  const shown = searchGroups(groups, query);
  const archived = everyone.filter((a) => a.archived_at);
  const isSession = (a: Agent) => route[0] === "s" && isAgent(a, route[1]);
  const row = (member: SidebarMember) => <SessionRow key={member.agent.id} member={member} active={isSession(member.agent)} />;
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
          </label>
          <div className="side-nav">
            <a className={"nav-new " + (!route[0] ? "active" : "")} href="#/" onClick={() => selectWorkspace("")}>
              <Icon name="plus" size={16} />
              <span>{t("sidebar.new")}</span>
              <kbd>Ctrl N</kbd>
            </a>
            <a className={"nav-usage " + (route[0] === "usage" ? "active" : "")} href="#/usage">
              <Icon name="chart" size={16} />
              <span>{t("nav.usage")}</span>
            </a>
            <a className={"nav-expertise " + (route[0] === "expertise" ? "active" : "")} href="#/expertise">
              <Icon name="brain" size={16} />
              <span>{t("nav.expertise")}</span>
            </a>
            <a className={"nav-lessons " + (route[0] === "lessons" ? "active" : "")} href="#/lessons">
              <Icon name="tasks" size={16} />
              <span>{t("nav.lessons")}</span>
            </a>
            <a className={"nav-extensions " + (route[0] === "extensions" || route[0] === "ext" ? "active" : "")} href="#/extensions">
              <Icon name="panel" size={16} />
              <span>扩展</span>
            </a>
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
            <NeedsYou items={[...todos, ...unsortedIssues]} agents={everyone} />
            {done > 0 && <p className="sidebar-unread">{t("sidebar.doneUnread", { n: done })}</p>}
            {!query.trim() && (pinnedMembers.length > 0 || pinnedRuns.length > 0) && (
              <section className="group">
                <div className="group-head static">
                  <span className="group-name">{t("sidebar.pinned")}</span>
                </div>
                {pinnedMembers.map(row)}
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
            {shown.map((g) => (
              <Group
                key={g.key}
                group={g}
                searching={!!query.trim()}
                active={route[0] === "w" && route[1] === g.key}
                onMove={
                  g.key === CHATS
                    ? undefined
                    : () => {
                        const keys = groups.filter((x) => x.key !== CHATS).map((x) => x.key);
                        const i = keys.indexOf(g.key);
                        if (i > 0) [keys[i - 1], keys[i]] = [keys[i], keys[i - 1]];
                        projectOrder.replace(keys);
                      }
                }
              >
                <Capped
                  items={g.members}
                  keep={(m) => isSession(m.agent) || m.agent.status === "running" || m.agent.status === "error" || !!unread[m.agent.id] || todos.some((item) => item.agent === m.agent.id)}
                  searching={!!query.trim()}
                >
                  {row}
                </Capped>
              </Group>
            ))}
            {archived.length > 0 && <div hidden={!!query.trim()}><Archived agents={archived} isSession={isSession} /></div>}
            {query.trim() && !shown.length && <p className="sidebar-empty">{t("sidebar.noMatch", { query })}</p>}
            {!query && loaded && !groups.length && <p className="sidebar-empty">{t("sidebar.empty")}</p>}
          </div>
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
            <QuotaMini />
            <div className="foot-row">
              <Connection />
              <a className={"icon-btn " + (route[0] === "settings" ? "on" : "")} href="#/settings" title={t("nav.settings")} aria-label={t("nav.settings")}>
                <Icon name="settings" size={15} />
              </a>
            </div>
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
          {visible.remaining > 0 && (
            <button className="more" onClick={() => setAll(true)}>
              {t("sidebar.moreTodos", { n: visible.remaining })}
            </button>
          )}
          {all && items.length > 3 && (
            <button className="more" onClick={() => setAll(false)}>
              {t("common.less")}
            </button>
          )}
        </>
      )}
    </section>
  );
}

function Group({
  group,
  searching,
  active,
  onMove,
  children,
}: {
  group: SidebarGroup;
  searching: boolean;
  active: boolean;
  onMove?: () => void;
  children: ReactNode;
}) {
  const collapsed = useCollapsed();
  const closed = groupClosed(collapsed, group.key, searching);
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
          onClick={() => toggleCollapsed(group.key)}
        >
          <span className="project-glyph">
            <Icon name={group.key === CHATS ? "chat" : "folder"} size={14} />
          </span>
          <span className="project-fold">
            <Icon name={closed ? "chevronRight" : "chevron"} size={13} />
          </span>
        </button>
        {group.key === CHATS ? (
          <span className="group-name project-name">{name}</span>
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
              selectWorkspace(group.key === CHATS ? "" : group.key);
              startNew(group.key === CHATS ? "chat" : "run");
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
  return (
    <>
      {shown.map(children)}
      {items.length > shown.length && (
        <button className="more" onClick={() => setAll(true)}>
          {t("common.more", { n: items.length - shown.length })}
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

type Dot = "live" | "warn" | "danger" | "unread" | undefined;
function SessionRow({ member, active }: { member: SidebarMember; active: boolean }) {
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
      dim={compact && agent.status === "stopped"}
      compact={compact}
      flash={justDone}
      peek={peek}
      title={displayName(agent) + " · " + status + (fresh || failed ? " · " + t("sidebar.unread") : "") + (agent.model ? " · " + agent.model : "") + "\n" + (summary || t("sidebar.noActivity")) + "\n" + t("sidebar.besideHint")}
      dot={undefined}
      pinKey={sessionKey(agent)}
      beside={agent.id}
      archive={agent}
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
          <span className={"member-name " + (flagged ? "strong" : "")}>{displayName(agent)}</span>
          <ModelMark plugin={agent.plugin} model={agent.model} />
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
  return (
    <div className={`nav-row ${active ? "active" : ""} ${dim ? "dim" : ""} ${compact ? "compact" : ""} ${flash ? "just-done" : ""}`} {...hover}>
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

/** The most used quota window of all accounts, e.g. "额度 8% · 30 天". */
function QuotaMini() {
  const quota = useQuota();
  const found = tightest(
    (quota.data?.accounts ?? []).map((a) => ({
      ...a,
      windows: a.status.rate_limits,
    })),
  );
  if (!found) return null;
  const pct = Math.round(found.window.used_percent);
  const name = found.account.account.label || found.account.account.id;
  return (
    <a
      className="quota-mini"
      href="#/usage"
      title={t("quota.miniTitle", {
        name,
        window: windowLabel(found.window),
        pct,
      })}
    >
      <span className={`quota-bar ${level(pct, found.account.status.limit_reached)}`}>
        <span style={{ width: `${Math.min(100, pct)}%` }} />
      </span>
      {t("quota.mini", {
        pct,
        window: windowLabel(found.window).replace(/限额$/, ""),
      })}
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
    status === "connected"
      ? t("hub.connected")
      : status === "connecting"
        ? t("hub.connecting")
        : t("hub.offline", { reason: status.replace(/^offline: /, "") });
  return (
    <div className="connection" title={info?.home}>
      <span className={`dot ${color} ${status === "connecting" ? "pulse" : ""}`} />
      <span className="text" title={text}>
        {text}
      </span>
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
    t("build.page", {
      tag: buildTag(page),
      dev: page.dev ? t("build.dev") : "",
    }),
    t("build.hub", {
      tag: buildTag(hub),
      dev: hub.debug ? t("build.dev") : "",
    }),
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

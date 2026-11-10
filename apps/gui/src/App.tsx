import { useEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { api, getBridge } from "./api";
import { useHubData, useVersionMismatch, type VersionMismatch } from "./hub";
import { closeBeside, closeTools, focusSession, focusedOf, isTermTab, openTool, routeTool, setRatio, useLayout, usePanes } from "./panes";
import { shownTab } from "./lib/docks";
import { FileViewerHost } from "./FileViewer";
import { TabDragLayer } from "./tabs";
import { TerminalHydrate, TerminalProblem, TerminalScope, toggleTerminal, useTabShows } from "./terminal";
import { BrowserHost } from "./browser";
import { startNew } from "./menubar";
import { toggleRail } from "./rail";
import { Sidebar } from "./Sidebar";
import { Titlebar, closeSidebarPeek, hoverSidebar, inDesktop, toggleSidebar, unhoverSidebar, useSidebar } from "./titlebar";
import { ToolColumn, useToolsFloat } from "./ToolColumn";
import { BOTTOM, SIDEBAR, TOOLS, dragBottom, dragEdge, setBottomHeight, setSidebarWidth, setToolsWidth, useWidths } from "./lib/widths";
import { HomeView } from "./views/Home";
import { RunView } from "./views/Run";
import { SessionView } from "./views/Session";
import { SettingsView } from "./views/Settings";
import { UsageView } from "./views/Usage";
import { ExpertiseView } from "./views/Expertise";
import { LessonsView } from "./views/Lessons/Lessons";
import { WorkspaceView } from "./views/Workspace";
import { WorkView } from "./views/Work";
import { GitView } from "./views/Git";
import { ExtensionDetail } from "./extensions/page/Detail";
import { ExtensionsView } from "./extensions/page/Extensions";
import { ExtensionPageView } from "./extensions/page/Host";
import { FEATURE_LABELS, useFeatureSupport, type FeatureSupport } from "./lib/features";

/** `#/runs/<id>` → `["runs", "<id>"]`. */
function useRoute(): string[] {
  const parse = () =>
    window.location.hash
      .replace(/^#\/?/, "")
      .split("/")
      .filter(Boolean)
      .map(decodeURIComponent);
  const [route, setRoute] = useState(parse);
  useEffect(() => {
    const onChange = () => setRoute(parse());
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}

export function navigate(...parts: string[]) {
  window.location.hash = "/" + parts.map(encodeURIComponent).join("/");
}

/** Runs shown in the sidebar, newest first. */
const SIDEBAR_RUNS = 12;
/** Runs whose sessions stay listed under them; finished runs fold theirs away. */
export const ACTIVE_RUN = ["planning", "awaiting_approval", "running", "paused"];

/** Recent runs with their tasks, for grouping sessions by run. */
export function useRunTree() {
  return useHubData(
    async () => {
      const { runs } = await api.listRuns();
      const recent = runs.slice().sort((a, b) => b.created_at.localeCompare(a.created_at)).slice(0, SIDEBAR_RUNS);
      return Promise.all(recent.map((r) => api.getRun(r.id)));
    },
    [],
    (e) => e.type.startsWith("run.") || e.type === "task.status_changed" || e.type === "task.handed_off",
  );
}

/**
 * Every run `agent` leads, with its tasks, however old: a lead's conversation shows their
 * handoffs, which must not depend on how many runs the sidebar lists.
 */
export function useRunsLedBy(agent: string | undefined) {
  return useHubData(
    async () => {
      if (!agent) return [];
      const { runs } = await api.listRuns();
      return Promise.all(runs.filter((r) => r.planner === agent).map((r) => api.getRun(r.id)));
    },
    [agent],
    (e) => e.type.startsWith("run.") || e.type === "task.handed_off",
  );
}

/** The agents working in a run: its planner, then each task's worker and reviewer. */
export function runAgents(detail: RunDetail, agents: Agent[]): Agent[] {
  const ids = [detail.run.planner, ...detail.tasks.flatMap((t) => [t.assignee, t.reviewer])];
  const seen = new Set<string>();
  const out: Agent[] = [];
  for (const id of ids) {
    if (!id || seen.has(id)) continue;
    seen.add(id);
    const agent = agents.find((a) => a.id === id);
    if (agent) out.push(agent);
  }
  return out;
}

/** The routed session, and a second one beside it when one is open. */
function Sessions({ primary, question }: { primary: string; question?: string }) {
  const { beside, ratio } = usePanes();
  const { focus } = useLayout();
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { if (question) focusSession(primary); }, [primary, question]);
  if (!beside || beside === primary) return <SessionView key={primary} name={primary} questionId={question} />;

  const drag = (e: ReactPointerEvent<HTMLDivElement>) => {
    const box = ref.current?.getBoundingClientRect();
    if (!box) return;
    e.preventDefault();
    const move = (ev: PointerEvent) => setRatio((ev.clientX - box.left) / box.width);
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  return (
    <div className={`split ${focusedOf(primary, beside, focus) === beside ? "split-focus-beside" : "split-focus-primary"}`} ref={ref} style={{ gridTemplateColumns: `minmax(0, ${ratio}fr) 6px minmax(0, ${1 - ratio}fr)` }}>
      <div className="split-session split-primary"><SessionView key={primary} name={primary} questionId={question} /><button className="split-switch" onClick={() => focusSession(beside)}>切换到另一会话</button></div>
      <div className="divider" onPointerDown={drag} onDoubleClick={() => setRatio(0.5)} title="拖动调整宽度，双击平分" role="separator" aria-orientation="vertical" />
      <div className="split-session split-beside"><SessionView key={beside} name={beside} onClose={closeBeside} /><button className="split-switch" onClick={() => focusSession(primary)}>切换到另一会话</button></div>
    </div>
  );
}

/** In place of every page while the hub speaks another API version: nothing else is safe to show. */
function VersionMismatchView({ hub, gui }: VersionMismatch) {
  return (
    <div className="version-mismatch" role="alert">
      <h2>hub 与 GUI 版本不匹配</h2>
      <p>
        hub 的接口版本是 <code>{hub}</code>，这个 GUI 用的是 <code>{gui}</code>。为免读错或写坏数据，GUI 不会连接这个 hub。
      </p>
      <p>请把 hub 和 GUI 更新到同一版本，再重启 hub（<code>crossagents hub start</code>）。hub 重启后这里会自动重新连接。</p>
    </div>
  );
}

/**
 * A page this hub does not have: an older hub that listed no features, or one that refused the
 * page's request (403) and said why (`support.reason`). The entry is hidden or greyed in the
 * sidebar; a link that still leads here gets this in place of a view that would only error.
 */
function UnavailablePage({ label, support }: { label: string; support: FeatureSupport }) {
  const why = support.reason ?? "这个 hub 是较早的版本，没有这个功能。把 hub 换成同一版本后重新连接即可。";
  // A refusal is about this credential and the hub has just said why; an old hub's reason is
  // in the version tooltip, which is where a person compares versions anyway.
  const where = support.reason
    ? "侧栏里这个入口已经变灰，悬停可以再看一次这条消息。"
    : "侧栏版本号的提示里写着原因。";
  return (
    <div className="version-mismatch" role="alert">
      <h2>{label}{support.reason ? "现在不可用" : "在这个 hub 上不可用"}</h2>
      <p>{why}</p>
      <p>
        {where}
        <a href="#/">回到首页</a>。
      </p>
    </div>
  );
}

export function App() {
  const route = useRoute();
  const [section, a, b, c] = route;
  const { state, focus, peek } = useLayout();

  const sidebar = useSidebar();
  const floatTools = useToolsFloat();
  const widths = useWidths(sidebar.docked);
  // `#/runs/<id>/graph` (a link the person can be sent; the mock previews too) opens 提交图.
  const graphRoute = section === "runs" && b === "graph" ? a : undefined;
  useEffect(() => {
    if (graphRoute) openTool("graph");
  }, [graphRoute]);
  // `#/runs/<id>/tasks/<task>` shows the 任务 tab; leaving the route puts the right dock back.
  const routeTask = section === "runs" && a && b === "tasks" ? c : undefined;
  useEffect(() => {
    if (routeTask) routeTool("task");
  }, [routeTask]);
  const mismatch = useVersionMismatch();
  // Pages an older hub does not have, and pages whose request it refused (docs/TODO.md §二).
  const git = useFeatureSupport("git");
  const lessons = useFeatureSupport("lessons");
  const work = useFeatureSupport("work");
  /** The page itself when the hub has it; the explanation when it does not or refused it. */
  const orOff = (support: FeatureSupport, label: string, page: ReactNode) =>
    support.known && (!support.show || support.disabled) ? <UnavailablePage label={label} support={support} /> : page;

  // Picking something in the floating sidebar puts it away.
  const where = window.location.hash;
  useEffect(closeSidebarPeek, [where]);

  const shows = useTabShows();
  const docks = state.docks;
  const bottomShows = docks.bottom.open && docks.bottom.tabs.some(shows);
  const bottomTab = shownTab({ ...docks.bottom, tabs: docks.bottom.tabs.filter(shows) });
  const terminalKey = useRef(() => {});
  terminalKey.current = () => toggleTerminal(bottomTab && isTermTab(bottomTab) ? bottomTab : null, bottomShows);

  // Ctrl+N (Cmd+N): new, as the sidebar advertises; Ctrl+B: the sidebar; Ctrl+I: the info rail;
  // Ctrl+`: the terminal; Alt+←/→: history.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = (e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey;
      if (mod && e.key === "`") {
        e.preventDefault();
        terminalKey.current();
      } else if (mod && e.key.toLowerCase() === "n") {
        e.preventDefault();
        startNew("chat");
      } else if (mod && e.key === ",") {
        e.preventDefault();
        window.location.hash = "/settings";
      } else if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        const search = document.querySelector<HTMLInputElement>(".sidebar .search input");
        search?.focus();
        search?.select();
      } else if (e.key === "F11") {
        e.preventDefault();
        getBridge()?.command?.("fullscreen");
      } else if (mod && e.key.toLowerCase() === "b") {
        e.preventDefault();
        toggleSidebar();
      } else if (mod && e.key.toLowerCase() === "i") {
        e.preventDefault();
        toggleRail();
      } else if (e.altKey && !e.ctrlKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        e.preventDefault();
        if (e.key === "ArrowLeft") history.back();
        else history.forward();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  let main;
  /** What the tabs follow here; pages with neither conversation nor run have no docks. */
  let tools: Pick<Parameters<typeof ToolColumn>[0], "follow" | "task" | "run" | "onClose" | "onCloseTask"> | null = null;
  switch (section) {
    case "runs":
      main = a ? <RunView runId={a} selectedTask={b === "tasks" ? c : undefined} confirmDelete={b === "delete"} /> : <HomeView />;
      if (a && routeTask)
        tools = {
          task: { run: a, task: routeTask },
          run: a,
          onClose: () => {
            closeTools();
            navigate("runs", a);
          },
          onCloseTask: () => navigate("runs", a),
        };
      else if (a) tools = { run: a, onClose: closeTools };
      break;
    case "s":
      main = a ? <Sessions primary={a} question={b === "questions" ? c : undefined} /> : <HomeView />;
      if (a) tools = { follow: focusedOf(a, state.beside, focus), onClose: closeTools };
      break;
    case "w":
      main = a ? <WorkspaceView key={a} wsKey={a} /> : <HomeView />;
      break;
    // 一个工作组：主导的对话连续不断，各轮是其中的分隔（docs/tasks/G1.md §3.5）。
    case "work":
      main = orOff(work, FEATURE_LABELS.work, a ? <WorkView key={a} workId={a} roundId={b} /> : <HomeView />);
      break;
    case "agents":
      // The old 队友 page lives on as 设置 → 智能体.
      main = <SettingsView page="agents" />;
      break;
    case "usage":
      main = <UsageView />;
      break;
    case "expertise":
      main = <ExpertiseView />;
      break;
    case "lessons":
      main = orOff(lessons, FEATURE_LABELS.lessons, <LessonsView />);
      break;
    case "settings":
      main = <SettingsView page={a} />;
      break;
    // 扩展: the list, one extension's approval screen, and pages an extension registered.
    case "extensions":
      main = a ? <ExtensionDetail name={a} /> : <ExtensionsView />;
      break;
    case "ext":
      main = a && b ? <ExtensionPageView extension={a} id={b} /> : <ExtensionsView />;
      break;
    case "git":
      main = orOff(git, FEATURE_LABELS.git, <GitView path={a} />);
      break;
    default:
      main = <HomeView />;
  }

  const { right, bottom } = docks;
  // The route's task opens the right dock while the route lasts, unless its tab went below.
  const taskOnRight = !!routeTask && right.tabs.includes("task");
  const drawer = !!tools && (floatTools ? right.open || peek || taskOnRight : (right.open || taskOnRight) && right.tabs.some(shows));
  const below = !!tools && !floatTools && bottom.open && bottom.tabs.some(shows);
  const app = (
    <div
      className={`app ${drawer ? (floatTools ? "drawer-float" : "with-drawer") : ""} ${sidebar.docked ? "" : "no-sidebar"} ${widths.sidebar < 215 ? "sidebar-tight" : ""}`}
      style={{ "--sidebar-width": `${widths.sidebar}px`, "--tools-width": `${widths.tools}px`, "--bottom-height": `${widths.bottom}px` } as CSSProperties}
    >
      {sidebar.docked && <Sidebar route={route} />}
      {sidebar.docked && (
        <div
          className="col-edge sidebar-edge"
          role="separator"
          aria-orientation="vertical"
          title="拖动调整宽度，双击还原"
          onPointerDown={(e) => dragEdge(e, widths.sidebar, "right", setSidebarWidth)}
          onDoubleClick={() => setSidebarWidth(SIDEBAR.initial)}
        />
      )}
      {sidebar.floating && (
        <>
          {/* Opened by a click: a click anywhere else puts it away. */}
          {sidebar.pinned && <div className="scrim" onClick={closeSidebarPeek} />}
          <div className="sidebar-float" onMouseEnter={hoverSidebar} onMouseLeave={unhoverSidebar}>
            <Sidebar route={route} />
          </div>
        </>
      )}
      <div className="center-col">
        <main className="main">{mismatch ? <VersionMismatchView {...mismatch} /> : main}</main>
        {below && tools && (
          <>
            <div
              className="row-edge"
              role="separator"
              aria-orientation="horizontal"
              title="拖动调整高度，双击还原"
              onPointerDown={(e) => dragBottom(e, widths.bottom)}
              onDoubleClick={() => setBottomHeight(BOTTOM.initial)}
            />
            <section className="bottom-dock">
              <ToolColumn dock="bottom" {...tools} />
            </section>
          </>
        )}
      </div>
      {drawer && tools && (
        <aside className="drawer">
          <ToolColumn dock="right" {...tools} />
        </aside>
      )}
      {drawer && !floatTools && (
        <div
          className="col-edge tools-edge"
          role="separator"
          aria-orientation="vertical"
          title="拖动调整宽度，双击还原"
          onPointerDown={(e) => dragEdge(e, widths.tools, "left", (w) => setToolsWidth(w, sidebar.docked))}
          onDoubleClick={() => setToolsWidth(TOOLS.initial, sidebar.docked)}
        />
      )}
      <FileViewerHost />
      <TabDragLayer />
      <TerminalHydrate />
      <BrowserHost />
      {tools && <TerminalScope follow={tools.follow} run={tools.run} />}
      <TerminalProblem />
    </div>
  );
  if (!inDesktop) return app;
  return (
    <div className="window">
      <Titlebar />
      {app}
    </div>
  );
}


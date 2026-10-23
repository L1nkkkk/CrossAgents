import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { api, getBridge } from "./api";
import { useHubData } from "./hub";
import { closeBeside, closeTools, focusedOf, setRatio, useLayout, usePanes } from "./panes";
import { startNew } from "./menubar";
import { toggleRail } from "./rail";
import { Sidebar } from "./Sidebar";
import { Titlebar, closeSidebarPeek, hoverSidebar, inDesktop, toggleSidebar, unhoverSidebar, useSidebar } from "./titlebar";
import { ToolColumn, useToolsFloat } from "./ToolColumn";
import { HomeView } from "./views/Home";
import { RunView } from "./views/Run";
import { SessionView } from "./views/Session";
import { SettingsView } from "./views/Settings";
import { UsageView } from "./views/Usage";
import { WorkspaceView } from "./views/Workspace";

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
    (e) => e.type.startsWith("run.") || e.type === "task.status_changed",
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
function Sessions({ primary }: { primary: string }) {
  const { beside, ratio } = usePanes();
  const ref = useRef<HTMLDivElement>(null);
  if (!beside || beside === primary) return <SessionView key={primary} name={primary} />;

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
    <div className="split" ref={ref} style={{ gridTemplateColumns: `minmax(0, ${ratio}fr) 6px minmax(0, ${1 - ratio}fr)` }}>
      <SessionView key={primary} name={primary} />
      <div className="divider" onPointerDown={drag} onDoubleClick={() => setRatio(0.5)} title="拖动调整宽度，双击平分" role="separator" aria-orientation="vertical" />
      <SessionView key={beside} name={beside} onClose={closeBeside} />
    </div>
  );
}

export function App() {
  const route = useRoute();
  const [section, a, b, c] = route;
  const { state, focus } = useLayout();

  const sidebar = useSidebar();
  const floatTools = useToolsFloat();

  // Picking something in the floating sidebar puts it away.
  const where = window.location.hash;
  useEffect(closeSidebarPeek, [where]);

  // Ctrl+N (Cmd+N): new, as the sidebar advertises; Ctrl+B: the sidebar; Ctrl+I: the info rail;
  // Alt+←/→: history.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = (e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey;
      if (mod && e.key.toLowerCase() === "n") {
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
  let drawer = null;
  switch (section) {
    case "runs":
      main = a ? <RunView runId={a} selectedTask={b === "tasks" ? c : undefined} /> : <HomeView />;
      if (a && b === "tasks" && c)
        drawer = (
          <ToolColumn
            task={{ run: a, task: c }}
            onClose={() => {
              closeTools();
              navigate("runs", a);
            }}
          />
        );
      else if (a && state.tools.open) drawer = <ToolColumn onClose={closeTools} />;
      break;
    case "s":
      main = a ? <Sessions primary={a} /> : <HomeView />;
      if (a && state.tools.open) drawer = <ToolColumn follow={focusedOf(a, state.beside, focus)} onClose={closeTools} />;
      break;
    case "w":
      main = a ? <WorkspaceView key={a} wsKey={a} /> : <HomeView />;
      break;
    case "agents":
      // The old 队友 page lives on as 设置 → 智能体.
      main = <SettingsView page="agents" />;
      break;
    case "usage":
      main = <UsageView />;
      break;
    case "settings":
      main = <SettingsView page={a} />;
      break;
    default:
      main = <HomeView />;
  }

  const app = (
    <div className={`app ${drawer ? (floatTools ? "drawer-float" : "with-drawer") : ""} ${sidebar.docked ? "" : "no-sidebar"}`}>
      {sidebar.docked && <Sidebar route={route} />}
      {sidebar.floating && (
        <>
          {/* Opened by a click: a click anywhere else puts it away. */}
          {sidebar.pinned && <div className="scrim" onClick={closeSidebarPeek} />}
          <div className="sidebar-float" onMouseEnter={hoverSidebar} onMouseLeave={unhoverSidebar}>
            <Sidebar route={route} />
          </div>
        </>
      )}
      <main className="main">{main}</main>
      {drawer && (
        <aside className="drawer">
          {drawer}
        </aside>
      )}
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


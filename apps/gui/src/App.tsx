import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { api } from "./api";

import { useHubData } from "./hub";
import { closeBeside, setRatio, usePanes } from "./panes";
import { Sidebar } from "./Sidebar";
import { AgentsView } from "./views/Agents";
import { HomeView } from "./views/Home";
import { RunView, TaskDrawer } from "./views/Run";
import { SessionView } from "./views/Session";
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

  // Ctrl+N (Cmd+N): new run, as the sidebar button advertises.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "n") {
        e.preventDefault();
        window.location.hash = "/";
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
      if (a && b === "tasks" && c) drawer = <TaskDrawer runId={a} taskId={c} onClose={() => navigate("runs", a)} />;
      break;
    case "s":
      main = a ? <Sessions primary={a} /> : <HomeView />;
      break;
    case "w":
      main = a ? <WorkspaceView key={a} wsKey={a} /> : <HomeView />;
      break;
    case "agents":
      main = <AgentsView />;
      break;
    case "usage":
      main = <UsageView />;
      break;
    default:
      main = <HomeView />;
  }

  return (
    <div className={`app ${drawer ? "with-drawer" : ""}`}>
      <Sidebar route={route} />
      <main className="main">{main}</main>
      {drawer && <aside className="drawer">{drawer}</aside>}
    </div>
  );
}

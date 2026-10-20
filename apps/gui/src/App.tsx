import { useEffect, useState } from "react";
import { api } from "./api";
import { useConnectionStatus } from "./hub";
import { AgentsView } from "./views/Agents";
import { RunDetailView } from "./views/RunDetail";
import { RunsView } from "./views/Runs";
import { TaskDetailView } from "./views/TaskDetail";
import { UsageView } from "./views/Usage";

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

function Page({ route }: { route: string[] }) {
  const [section, a, b, c] = route;
  switch (section) {
    case "runs":
      if (a && b === "tasks" && c) return <TaskDetailView runId={a} taskId={c} />;
      if (a) return <RunDetailView runId={a} />;
      return <RunsView />;
    case "agents":
      return <AgentsView selected={a} />;
    case "usage":
      return <UsageView />;
    default:
      return <RunsView />;
  }
}

function Connection() {
  const status = useConnectionStatus();
  const [home, setHome] = useState<string>();
  useEffect(() => {
    if (status === "connected") api.info().then((info) => setHome(`${info.home} · v${info.version}`), () => {});
  }, [status]);
  const kind = status === "connected" ? "ok" : status === "connecting" ? "warn" : "bad";
  return (
    <div className="connection" title={home}>
      <span className={`dot ${kind}`} />
      <span>{status === "connected" ? "hub 已连接" : status === "connecting" ? "连接中…" : status.replace(/^offline: /, "未连接：")}</span>
    </div>
  );
}

export function App() {
  const route = useRoute();
  const section = route[0] ?? "runs";
  const link = (to: string, label: string) => (
    <a href={`#/${to}`} className={section === to ? "active" : ""}>
      {label}
    </a>
  );
  return (
    <div className="app">
      <nav className="sidebar">
        <div className="brand">CrossAgents</div>
        {link("runs", "Runs")}
        {link("agents", "Agents")}
        {link("usage", "用量")}
        <div className="spacer" />
        <Connection />
      </nav>
      <main className="content">
        <Page route={route} />
      </main>
    </div>
  );
}

import type { ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import { api } from "./api";
import { useHubData } from "./hub";

export function Badge({ status }: { status: string }) {
  return <span className={`badge s-${status}`}>{status}</span>;
}

export function ErrorBox({ error }: { error?: string }) {
  return error ? <div className="error">{error}</div> : null;
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="field">
      <span>{label}</span>
      {children}
    </label>
  );
}

export function List({ items, empty = "—" }: { items: string[]; empty?: string }) {
  if (!items.length) return <span className="muted">{empty}</span>;
  return (
    <ul className="plain">
      {items.map((item, i) => (
        <li key={i}>
          <code>{item}</code>
        </li>
      ))}
    </ul>
  );
}

export function time(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

/** All agents, kept fresh by agent events; for showing names instead of ids. */
export function useAgents() {
  return useHubData(
    () => api.listAgents().then((r) => r.agents),
    [],
    (e) => e.type === "agent.created" || e.type === "agent.removed" || e.type === "agent.status_changed",
  );
}

export function agentName(agents: Agent[] | undefined, id: string | null): string {
  if (!id) return "—";
  return agents?.find((a) => a.id === id)?.name ?? id.slice(0, 8);
}
